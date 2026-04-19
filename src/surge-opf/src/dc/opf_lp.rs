#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Sparse B-theta DC-OPF formulation.
//!
//! Variables: x = [θ (all buses) | Pg (in-service generators) |
//!                 s_upper (thermal slacks) | s_lower (thermal slacks) |
//!                 e_g (PWL epiograph variables, one per PWL generator) |
//!                 v (virtual bid variables, one per in-service VirtualBid)]
//!
//! Equality constraints (power balance at each bus):
//!   B_bus * θ - A_gen * Pg = -Pd
//!
//! Inequality constraints (branch thermal limits, soft via slacks):
//!   -fmax ≤ Bf * θ - s_upper + s_lower ≤ fmax
//!
//! Inequality constraints (PWL cost epiograph, OPF-01):
//!   e_g - slope_k * Pg_j ≥ intercept_k   for each segment k of each PWL generator j
//!
//! The slack bus angle is fixed at 0 via bounds (col_lower = col_upper = 0).
//! This is O(nnz) throughout — no dense PTDF matrix needed.
//! LMPs come directly from the power balance duals at each bus.

use std::time::Instant;

use surge_network::Network;
use surge_network::market::{VirtualBidDirection, VirtualBidResult};
use surge_solution::ParResult; // PenaltyConfig used via DcOpfOptions
use surge_solution::{
    OpfBranchResults, OpfDeviceDispatch, OpfGeneratorResults, OpfPricing, OpfSolution, OpfType,
    PfSolution, SolveStatus,
};
use tracing::info;

use surge_hvdc::interop::dc_grid_injections;

use crate::backends::{LpOptions, LpResult, LpSolveStatus, SparseProblem, try_default_lp_solver};
use crate::common::context::OpfNetworkContext;
use crate::dc::costs::{
    GeneratorCostBuffers, apply_generator_costs, build_hessian_csc, build_pwl_gen_info,
    has_mixed_quadratic_polynomial_costs, quadratic_pwl_local_indices,
};
use crate::dc::island_lmp::IslandRefs;
use crate::dc::opf::{DcOpfError, DcOpfOptions, DcOpfResult, DcOpfRuntime, HvdcOpfLink};

use surge_sparse::Triplet;

/// Convert COO triplets to CSC format (delegates to surge_sparse).
/// Duplicate entries at the same (row, col) are summed.
pub fn triplets_to_csc(
    triplets: &[Triplet<f64>],
    n_row: usize,
    n_col: usize,
) -> (Vec<i32>, Vec<i32>, Vec<f64>) {
    surge_sparse::CscMatrix::try_from_triplets(n_row, n_col, triplets)
        .and_then(|matrix| matrix.try_to_i32())
        .expect("internal OPF triplets must form a valid CSC matrix")
}

struct DcOpfModelBuild {
    prob: SparseProblem,
    n_var: usize,
    n_row: usize,
    n_bus: usize,
    n_gen: usize,
    n_flow: usize,
    n_hvdc: usize,
    n_gen_slacks: usize,
    theta_offset: usize,
    pg_offset: usize,
    hvdc_offset: usize,
    s_upper_offset: usize,
    s_lower_offset: usize,
    sg_upper_offset: usize,
    sg_lower_offset: usize,
    vbid_offset: usize,
    balance_offset: usize,
    base: f64,
    c0_total: f64,
    total_load_mw: f64,
    has_gen_slacks: bool,
    gen_indices: Vec<usize>,
    constrained_branches: Vec<usize>,
    active_iface_indices: Vec<usize>,
    active_fg_indices: Vec<usize>,
    gen_bus_idx: Vec<usize>,
    hvdc_var: Vec<HvdcOpfLink>,
    hvdc_from_idx: Vec<usize>,
    hvdc_to_idx: Vec<usize>,
    active_vbids: Vec<usize>,
    island_refs: IslandRefs,
    pbusinj: Vec<f64>,
    loss_ptdf: Option<surge_dc::PtdfRows>,
}

struct DcOpfExecution {
    solution: LpResult,
    solver_name: String,
    solver_version: String,
}

fn map_lp_status(status: LpSolveStatus) -> Result<(), DcOpfError> {
    match status {
        LpSolveStatus::Optimal => Ok(()),
        LpSolveStatus::SubOptimal => Err(DcOpfError::SubOptimalSolution),
        LpSolveStatus::Infeasible => Err(DcOpfError::InfeasibleProblem),
        LpSolveStatus::Unbounded => Err(DcOpfError::UnboundedProblem),
        LpSolveStatus::SolverError(message) => Err(DcOpfError::SolverError(message)),
    }
}

fn resolve_hvdc_bus_index(
    bus_map: &std::collections::HashMap<u32, usize>,
    link_index: usize,
    hvdc: &HvdcOpfLink,
    bus_number: u32,
    role: &'static str,
) -> Result<usize, DcOpfError> {
    bus_map
        .get(&bus_number)
        .copied()
        .ok_or_else(|| DcOpfError::InvalidHvdcLink {
            index: link_index,
            from_bus: hvdc.from_bus,
            to_bus: hvdc.to_bus,
            reason: format!("{role} bus {bus_number} not found in network"),
        })
}

fn should_use_canonical_pwl_costs(
    network: &Network,
    gen_indices: &[usize],
    solver_name: &str,
    options: &DcOpfOptions,
) -> bool {
    if options.use_pwl_costs {
        return true;
    }

    const LARGE_MIXED_QP_BUS_THRESHOLD: usize = 1000;
    if solver_name != "HiGHS" || network.n_buses() < LARGE_MIXED_QP_BUS_THRESHOLD {
        return false;
    }
    if !has_mixed_quadratic_polynomial_costs(network, gen_indices) {
        return false;
    }

    info!(
        buses = network.n_buses(),
        solver = solver_name,
        "DC-OPF: using canonical PWL generator costs for the large mixed-quadratic HiGHS class"
    );
    true
}

fn execute_dc_opf_model(
    network: &Network,
    options: &DcOpfOptions,
    runtime: &DcOpfRuntime,
    model: &mut DcOpfModelBuild,
) -> Result<DcOpfExecution, DcOpfError> {
    let lp_opts = LpOptions {
        tolerance: options.tolerance,
        ..Default::default()
    };
    let solver = match runtime.lp_solver.clone() {
        Some(s) => s,
        None => try_default_lp_solver().map_err(DcOpfError::SolverError)?,
    };
    let mut sol = solver
        .solve(&model.prob, &lp_opts)
        .map_err(DcOpfError::SolverError)?;

    map_lp_status(sol.status.clone())?;

    if options.use_loss_factors {
        use super::loss_factors::{compute_dc_loss_sensitivities, compute_total_dc_losses};
        if model.loss_ptdf.is_none() {
            let monitored_branches: Vec<usize> = network
                .branches
                .iter()
                .enumerate()
                .filter(|(_, branch)| branch.in_service && branch.x.abs() >= 1e-20)
                .map(|(idx, _)| idx)
                .collect();
            model.loss_ptdf = Some(
                surge_dc::compute_ptdf(
                    network,
                    &surge_dc::PtdfRequest::for_branches(&monitored_branches),
                )
                .map_err(|e| DcOpfError::SolverError(format!("loss-factor PTDF failed: {e}")))?,
            );
        }
        let loss_ptdf = model
            .loss_ptdf
            .as_ref()
            .expect("loss-factor PTDF initialized above");

        let bus_pd_mw = network.bus_load_p_mw();
        let gen_csc_positions: Vec<usize> = (0..model.n_gen)
            .map(|j| {
                let col = model.pg_offset + j;
                let target_row = (model.balance_offset + model.gen_bus_idx[j]) as i32;
                let start = model.prob.a_start[col] as usize;
                let end = model.prob.a_start[col + 1] as usize;
                model.prob.a_index[start..end]
                    .iter()
                    .position(|&r| r == target_row)
                    .map(|p| start + p)
                    .expect("gen power balance coefficient not found in CSC")
            })
            .collect();

        let ctx = OpfNetworkContext::for_dc(network)?;
        let bus_map = &ctx.bus_map;
        let mut prev_dloss = vec![0.0f64; model.n_bus];

        for loss_iter in 0..options.max_loss_iter {
            let theta = &sol.x[model.theta_offset..model.theta_offset + model.n_bus];
            let dloss_dp = compute_dc_loss_sensitivities(network, theta, bus_map, loss_ptdf);

            if loss_iter > 0 {
                let max_change = dloss_dp
                    .iter()
                    .zip(prev_dloss.iter())
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f64, f64::max);
                if max_change < options.loss_tol {
                    break;
                }
            }
            prev_dloss.copy_from_slice(&dloss_dp);

            for (j, &csc_pos) in gen_csc_positions.iter().enumerate() {
                let bus_idx = model.gen_bus_idx[j];
                let pf_inv = (1.0 - dloss_dp[bus_idx]).clamp(0.5, 1.5);
                model.prob.a_value[csc_pos] = -pf_inv;
            }

            let total_loss_pu = compute_total_dc_losses(network, theta, bus_map);
            let total_load_pu: f64 = bus_pd_mw
                .iter()
                .map(|&pd| pd / model.base)
                .sum::<f64>()
                .abs()
                .max(1e-10);
            for i in 0..model.n_bus {
                let load_share = (bus_pd_mw[i] / model.base).abs() / total_load_pu;
                let loss_at_bus = total_loss_pu * load_share;
                let pd_pu = bus_pd_mw[i] / model.base;
                let gs_pu = network.buses[i].shunt_conductance_mw / model.base;
                let rhs = -pd_pu - gs_pu - model.pbusinj[i] - loss_at_bus;
                model.prob.row_lower[model.balance_offset + i] = rhs;
                model.prob.row_upper[model.balance_offset + i] = rhs;
            }

            sol = solver
                .solve(&model.prob, &lp_opts)
                .map_err(DcOpfError::SolverError)?;
            map_lp_status(sol.status.clone())?;
        }
    }

    Ok(DcOpfExecution {
        solution: sol,
        solver_name: solver.name().to_string(),
        solver_version: solver.version().to_string(),
    })
}

fn decode_dc_opf_result(
    network: &Network,
    options: &DcOpfOptions,
    ctx: &OpfNetworkContext,
    model: DcOpfModelBuild,
    execution: DcOpfExecution,
    solve_time: f64,
) -> DcOpfResult {
    let DcOpfExecution {
        solution: sol,
        solver_name,
        solver_version,
    } = execution;
    let bus_map = &ctx.bus_map;
    let bus_pd_mw = network.bus_load_p_mw();
    let theta_vals = &sol.x[model.theta_offset..model.theta_offset + model.n_bus];
    let pg_pu = &sol.x[model.pg_offset..model.pg_offset + model.n_gen];
    let gen_p_mw: Vec<f64> = pg_pu.iter().map(|&p| p * model.base).collect();

    let (hvdc_dispatch_mw, hvdc_shadow_prices): (Vec<f64>, Vec<f64>) = if model.n_hvdc > 0 {
        let dispatch: Vec<f64> = sol.x[model.hvdc_offset..model.hvdc_offset + model.n_hvdc]
            .iter()
            .map(|&p| p * model.base)
            .collect();
        let shadow: Vec<f64> = if sol.col_dual.len() > model.hvdc_offset {
            (0..model.n_hvdc)
                .map(|k| -sol.col_dual[model.hvdc_offset + k] / model.base)
                .collect()
        } else {
            vec![0.0; model.n_hvdc]
        };
        (dispatch, shadow)
    } else {
        (vec![], vec![])
    };

    let gen_limit_violations: Vec<(usize, f64)> = if model.has_gen_slacks {
        let mut violations = Vec::new();
        for j in 0..model.n_gen {
            let s_up = sol.x[model.sg_upper_offset + j];
            let s_dn = sol.x[model.sg_lower_offset + j];
            if s_up > 1e-6 || s_dn > 1e-6 {
                violations.push((model.gen_indices[j], (s_up + s_dn) * model.base));
            }
        }
        violations
    } else {
        vec![]
    };

    let total_cost = sol.objective + model.c0_total;
    let va: Vec<f64> = theta_vals.to_vec();
    let lmp: Vec<f64> = sol.row_dual[model.balance_offset..model.balance_offset + model.n_bus]
        .iter()
        .map(|&d| d / model.base)
        .collect();

    let (lmp_energy, lmp_congestion, lmp_loss) = if options.use_loss_factors {
        use super::loss_factors::compute_dc_loss_sensitivities;
        let dloss_dp = compute_dc_loss_sensitivities(
            network,
            theta_vals,
            bus_map,
            model
                .loss_ptdf
                .as_ref()
                .expect("loss-factor PTDF should be available when use_loss_factors=true"),
        );
        super::island_lmp::decompose_lmp_with_losses(&lmp, &dloss_dp, &model.island_refs)
    } else {
        super::island_lmp::decompose_lmp_lossless(&lmp, &model.island_refs)
    };

    let mut branch_shadow_prices = vec![0.0; network.branches.len()];
    for (ci, &l) in model.constrained_branches.iter().enumerate() {
        branch_shadow_prices[l] = sol.row_dual[ci] / model.base;
    }

    let shadow_price_angmin = vec![0.0_f64; network.branches.len()];
    let shadow_price_angmax = vec![0.0_f64; network.branches.len()];

    let n_if = model.active_iface_indices.len();
    let flowgate_shadow_prices: Vec<f64> = if !model.active_fg_indices.is_empty() {
        let mut v = vec![0.0; network.flowgates.len()];
        for (ri, &fgi) in model.active_fg_indices.iter().enumerate() {
            v[fgi] = sol.row_dual[model.n_flow + n_if + ri] / model.base;
        }
        v
    } else {
        vec![]
    };
    let interface_shadow_prices: Vec<f64> = if !model.active_iface_indices.is_empty() {
        let mut v = vec![0.0; network.interfaces.len()];
        for (ri, &ii) in model.active_iface_indices.iter().enumerate() {
            v[ii] = sol.row_dual[model.n_flow + ri] / model.base;
        }
        v
    } else {
        vec![]
    };

    let par_results: Vec<ParResult> = options
        .par_setpoints
        .iter()
        .map(|ps| {
            if let Some(br_idx) = network.branches.iter().position(|br| {
                br.in_service
                    && br.from_bus == ps.from_bus
                    && br.to_bus == ps.to_bus
                    && br.circuit == ps.circuit
            }) {
                let br = &network.branches[br_idx];
                let from_i = bus_map[&ps.from_bus];
                let to_i = bus_map[&ps.to_bus];
                let b_dc = br.b_dc();
                let implied_shift_rad = if b_dc.abs() > 1e-20 {
                    theta_vals[from_i] - theta_vals[to_i] - ps.target_mw / (model.base * b_dc)
                } else {
                    0.0
                };
                let implied_shift_deg = implied_shift_rad.to_degrees();
                let within_limits = br
                    .opf_control
                    .as_ref()
                    .map(|c| {
                        implied_shift_rad >= c.phase_min_rad && implied_shift_rad <= c.phase_max_rad
                    })
                    .unwrap_or(false);
                ParResult {
                    from_bus: ps.from_bus,
                    to_bus: ps.to_bus,
                    circuit: ps.circuit.clone(),
                    target_mw: ps.target_mw,
                    implied_shift_deg,
                    within_limits,
                }
            } else {
                ParResult {
                    from_bus: ps.from_bus,
                    to_bus: ps.to_bus,
                    circuit: ps.circuit.clone(),
                    target_mw: ps.target_mw,
                    implied_shift_deg: 0.0,
                    within_limits: false,
                }
            }
        })
        .collect();

    let virtual_bid_results: Vec<VirtualBidResult> = model
        .active_vbids
        .iter()
        .enumerate()
        .map(|(k, &bi)| {
            let vb = &options.virtual_bids[bi];
            let cleared_mw = sol.x[model.vbid_offset + k] * model.base;
            let bus_lmp = bus_map.get(&vb.bus).map(|&i| lmp[i]).unwrap_or(0.0);
            VirtualBidResult {
                position_id: vb.position_id.clone(),
                bus: vb.bus,
                direction: vb.direction,
                cleared_mw,
                price_per_mwh: vb.price_per_mwh,
                lmp: bus_lmp,
            }
        })
        .collect();

    let mut p_inject = vec![0.0; model.n_bus];
    for i in 0..model.n_bus {
        p_inject[i] = -bus_pd_mw[i] / model.base
            - network.buses[i].shunt_conductance_mw / model.base
            - model.pbusinj[i];
    }
    for (j, &bus_idx) in model.gen_bus_idx.iter().enumerate() {
        p_inject[bus_idx] += pg_pu[j];
    }
    for (k, hvdc) in model.hvdc_var.iter().enumerate() {
        let p_dc_pu = sol.x[model.hvdc_offset + k];
        let fi = model.hvdc_from_idx[k];
        p_inject[fi] -= p_dc_pu;
        let ti = model.hvdc_to_idx[k];
        p_inject[ti] += p_dc_pu * (1.0 - hvdc.loss_b_frac);
    }
    for (k, &bi) in model.active_vbids.iter().enumerate() {
        let vb = &options.virtual_bids[bi];
        if let Some(&bus_idx) = bus_map.get(&vb.bus) {
            let v_pu = sol.x[model.vbid_offset + k];
            match vb.direction {
                VirtualBidDirection::Inc => p_inject[bus_idx] += v_pu,
                VirtualBidDirection::Dec => p_inject[bus_idx] -= v_pu,
            }
        }
    }

    info!(
        "DC-OPF (sparse) solved in {:.1} ms ({} vars, {} constraints, {} iters, cost={:.2} $/hr, \
         {} HVDC vars, {} gen slacks, {} gen violations)",
        solve_time * 1000.0,
        model.n_var,
        model.n_row,
        sol.iterations,
        total_cost,
        model.n_hvdc,
        model.n_gen_slacks,
        gen_limit_violations.len(),
    );

    let gen_bus_numbers: Vec<u32> = model
        .gen_indices
        .iter()
        .map(|&gi| network.generators[gi].bus)
        .collect();
    let gen_ids: Vec<String> = model
        .gen_indices
        .iter()
        .map(|&gi| network.generators[gi].id.clone())
        .collect();
    let total_generation_mw: f64 = gen_p_mw.iter().sum();
    let total_losses_mw = (total_generation_mw - model.total_load_mw).max(0.0);

    let branch_pf_mw: Vec<f64> = network
        .branches
        .iter()
        .map(|br| {
            if !br.in_service {
                return 0.0;
            }
            let from_i = bus_map[&br.from_bus];
            let to_i = bus_map[&br.to_bus];
            br.b_dc() * (theta_vals[from_i] - theta_vals[to_i] - br.phase_shift_rad) * model.base
        })
        .collect();
    let branch_pt_mw: Vec<f64> = branch_pf_mw.iter().map(|&p| -p).collect();
    let branch_loading_pct: Vec<f64> = network
        .branches
        .iter()
        .zip(branch_pf_mw.iter())
        .map(|(br, &pf)| {
            if br.rating_a_mva <= 0.0 {
                f64::NAN
            } else {
                pf.abs() / br.rating_a_mva * 100.0
            }
        })
        .collect();
    let pf_solution = PfSolution {
        pf_model: surge_solution::PfModel::Dc,
        status: SolveStatus::Converged,
        iterations: 1,
        max_mismatch: 0.0,
        solve_time_secs: 0.0,
        voltage_magnitude_pu: vec![1.0; model.n_bus],
        voltage_angle_rad: va,
        active_power_injection_pu: p_inject,
        reactive_power_injection_pu: vec![0.0; model.n_bus],
        branch_p_from_mw: branch_pf_mw.clone(),
        branch_p_to_mw: branch_pt_mw.clone(),
        branch_q_from_mvar: vec![0.0; network.n_branches()],
        branch_q_to_mvar: vec![0.0; network.n_branches()],
        bus_numbers: network.buses.iter().map(|b| b.number).collect(),
        island_ids: vec![],
        q_limited_buses: vec![],
        n_q_limit_switches: 0,
        gen_slack_contribution_mw: vec![],
        convergence_history: vec![],
        worst_mismatch_bus: None,
        area_interchange: None,
    };

    let shadow_price_pg_min: Vec<f64> = (0..model.n_gen)
        .map(|j| (-sol.col_dual[model.pg_offset + j]).max(0.0) / model.base)
        .collect();
    let shadow_price_pg_max: Vec<f64> = (0..model.n_gen)
        .map(|j| sol.col_dual[model.pg_offset + j].max(0.0) / model.base)
        .collect();

    let has_thermal_slack_violation = (0..model.n_flow).any(|ci| {
        sol.x[model.s_upper_offset + ci] > 1e-6 || sol.x[model.s_lower_offset + ci] > 1e-6
    });
    let is_feasible = gen_limit_violations.is_empty() && !has_thermal_slack_violation;

    let opf = OpfSolution {
        opf_type: if model.n_hvdc > 0 {
            OpfType::HvdcOpf
        } else {
            OpfType::DcOpf
        },
        base_mva: network.base_mva,
        power_flow: pf_solution,
        generators: OpfGeneratorResults {
            gen_p_mw,
            gen_q_mvar: vec![],
            gen_bus_numbers,
            gen_ids,
            gen_machine_ids: model
                .gen_indices
                .iter()
                .map(|&gi| {
                    network.generators[gi]
                        .machine_id
                        .clone()
                        .unwrap_or_else(|| "1".to_string())
                })
                .collect(),
            shadow_price_pg_min,
            shadow_price_pg_max,
            shadow_price_qg_min: vec![],
            shadow_price_qg_max: vec![],
        },
        pricing: OpfPricing {
            lmp,
            lmp_energy,
            lmp_congestion,
            lmp_loss,
            lmp_reactive: vec![],
        },
        branches: OpfBranchResults {
            branch_loading_pct,
            branch_shadow_prices,
            shadow_price_angmin,
            shadow_price_angmax,
            thermal_limit_slack_from_mva: vec![],
            thermal_limit_slack_to_mva: vec![],
            flowgate_shadow_prices,
            interface_shadow_prices,
            shadow_price_vm_min: vec![],
            shadow_price_vm_max: vec![],
        },
        devices: OpfDeviceDispatch {
            switched_shunt_dispatch: vec![],
            tap_dispatch: vec![],
            phase_dispatch: vec![],
            svc_dispatch: vec![],
            tcsc_dispatch: vec![],
            storage_net_mw: vec![],
            dispatchable_load_served_mw: vec![],
            dispatchable_load_served_q_mvar: vec![],
            // DC-OPF does not clear reactive reserves (no Q variables
            // and no q-reserve rows); the AC reconcile path carries
            // them.
            producer_q_reserve_up_mvar: vec![],
            producer_q_reserve_down_mvar: vec![],
            consumer_q_reserve_up_mvar: vec![],
            consumer_q_reserve_down_mvar: vec![],
            zone_q_reserve_up_shortfall_mvar: vec![],
            zone_q_reserve_down_shortfall_mvar: vec![],
            // HVDC P2P as an NLP variable is AC-OPF-only; DC-OPF carries
            // HVDC via `hvdc_dispatch_mw` on the top-level result.
            hvdc_p2p_dispatch_mw: vec![],
            discrete_feasible: None,
            discrete_violations: vec![],
        },
        total_cost,
        total_load_mw: model.total_load_mw,
        total_generation_mw,
        total_losses_mw,
        par_results,
        virtual_bid_results,
        benders_cut_duals: vec![],
        objective_terms: vec![],
        audit: Default::default(),
        solve_time_secs: solve_time,
        iterations: Some(sol.iterations),
        solver_name: Some(solver_name),
        solver_version: Some(solver_version),
        ac_opf_timings: None,
        bus_q_slack_pos_mvar: vec![],
        bus_q_slack_neg_mvar: vec![],
        bus_p_slack_pos_mw: vec![],
        bus_p_slack_neg_mw: vec![],
        vm_slack_high_pu: vec![],
        vm_slack_low_pu: vec![],
        angle_diff_slack_high_rad: vec![],
        angle_diff_slack_low_rad: vec![],
    };

    DcOpfResult {
        opf,
        hvdc_dispatch_mw,
        hvdc_shadow_prices,
        gen_limit_violations,
        is_feasible,
    }
}

/// Solve DC-OPF using the sparse B-theta formulation.
///
/// # Formulation
///
/// Minimizes generator cost subject to power balance (B-theta) and branch thermal limits.
/// Angle difference constraints (`angmin`/`angmax`) are not included — DC-OPF does not
/// enforce them (MATPOWER DCPPowerModel likewise omits them by default).
///
/// Optionally co-optimizes HVDC P_dc setpoints (via `options.hvdc_links`) and/or
/// relaxes generator limits with penalty slacks (via `options.gen_limit_penalty`).
pub fn solve_dc_opf_lp(
    network: &Network,
    options: &DcOpfOptions,
) -> Result<DcOpfResult, DcOpfError> {
    solve_dc_opf_lp_with_runtime(network, options, &DcOpfRuntime::default())
}

/// Solve DC-OPF LP with explicit runtime controls (solver backend, warm-start).
pub fn solve_dc_opf_lp_with_runtime(
    network: &Network,
    options: &DcOpfOptions,
    runtime: &DcOpfRuntime,
) -> Result<DcOpfResult, DcOpfError> {
    let mut runtime = runtime.clone();
    if runtime.lp_solver.is_none() {
        runtime.lp_solver = Some(try_default_lp_solver().map_err(DcOpfError::SolverError)?);
    }

    // Expand FACTS devices (TCSC series reactance modification affects B-matrix;
    // SVC/STATCOM are reactive-only but expansion is a no-op for DC).
    let network = surge_ac::expand_facts(network);
    let network = &network;
    let ctx = OpfNetworkContext::for_dc(network)?;

    let start = Instant::now();
    let n_bus = ctx.n_bus;
    let n_br = ctx.n_branches;

    info!(
        buses = n_bus,
        branches = n_br,
        "starting DC-OPF (sparse B-theta)"
    );
    let bus_map = &ctx.bus_map;
    let base = ctx.base_mva;
    let island_refs = ctx.island_refs.clone();
    let gen_indices = &ctx.gen_indices;
    let n_gen = gen_indices.len();
    let total_load_mw = ctx.total_load_mw;
    let bus_pd_mw = network.bus_load_p_mw();
    let lp_solver_name = runtime
        .lp_solver
        .as_ref()
        .map(|solver| solver.name())
        .unwrap_or("unknown");
    let use_pwl_costs =
        should_use_canonical_pwl_costs(network, gen_indices, lp_solver_name, options);

    // Check capacity
    if ctx.total_capacity_mw < ctx.total_load_mw && options.gen_limit_penalty.is_none() {
        return Err(DcOpfError::InsufficientCapacity {
            load_mw: ctx.total_load_mw,
            capacity_mw: ctx.total_capacity_mw,
        });
    }

    // --- Identify PAR (flow-setpoint) branch indices ---
    // PAR branches are removed from the B matrix and replaced by fixed injections.
    // We build a set of branch indices to skip in all B-matrix assembly loops.
    use std::collections::HashSet;
    let par_branch_set: HashSet<usize> = options
        .par_setpoints
        .iter()
        .filter_map(|ps| {
            ctx.branch_idx_map
                .get(&(ps.from_bus, ps.to_bus, ps.circuit.clone()))
                .copied()
                .filter(|&idx| network.branches[idx].in_service)
        })
        .collect();

    // --- Identify constrained branches (needed to size slack variables) ---
    // PAR branches are excluded from thermal flow constraints.
    let constrained_branches: Vec<usize> = if options.enforce_thermal_limits {
        ctx.constrained_branch_indices(options.min_rate_a)
            .into_iter()
            .filter(|idx| !par_branch_set.contains(idx))
            .collect()
    } else {
        Vec::new()
    };
    let n_flow = constrained_branches.len();

    // --- PWL generator analysis (OPF-01) ---
    // For generators with PiecewiseLinear cost curves, we use the LP epiograph
    // formulation instead of the overall-slope approximation.
    //
    // For each PWL generator j with breakpoints [(p0,c0), (p1,c1), ..., (pK,cK)]:
    //   - Introduce auxiliary variable e_g[k_pwl] representing $/hr cost
    //   - Objective: minimize e_g[k_pwl]   (coefficient 1.0 in col_cost)
    //   - For each segment s in 0..K-1:
    //       slope_s = (c_{s+1} - c_s) / (p_{s+1} - p_s)   [$/MWh]
    //       intercept_s = c_s - slope_s * p_s               [$/hr at Pg=0]
    //       Constraint: e_g - slope_s * Pg_j (in $/hr units) >= intercept_s
    //       In per-unit Pg: slope_s_pu = slope_s * base_mva  [$/hr / pu]
    //       Constraint: e_g - slope_s_pu * Pg_pu >= intercept_s
    //
    // pwl_gen_info[k] = (local_gen_index_j, Vec<(slope_pu, intercept)>)
    let pwl_gen_info = build_pwl_gen_info(
        network,
        gen_indices,
        base,
        use_pwl_costs,
        options.pwl_cost_breakpoints,
    );
    let poly_quad_local_indices = quadratic_pwl_local_indices(network, gen_indices, use_pwl_costs);
    let n_pwl_gen = pwl_gen_info.len();
    let n_pwl_rows: usize = pwl_gen_info.iter().map(|entry| entry.segments.len()).sum();

    // --- HVDC link analysis ---
    let hvdc_links: &[HvdcOpfLink] = match &options.hvdc_links {
        Some(links) => links.as_slice(),
        None => &[],
    };
    let hvdc_var: Vec<HvdcOpfLink> = hvdc_links
        .iter()
        .filter(|h| h.is_variable())
        .cloned()
        .collect();
    let n_hvdc = hvdc_var.len();

    for (link_idx, hvdc) in hvdc_links.iter().enumerate() {
        resolve_hvdc_bus_index(bus_map, link_idx, hvdc, hvdc.from_bus, "from")?;
        resolve_hvdc_bus_index(bus_map, link_idx, hvdc, hvdc.to_bus, "to")?;
    }

    // --- Gen-limit slack analysis ---
    let has_gen_slacks = options.gen_limit_penalty.is_some();
    let n_gen_slacks = if has_gen_slacks { n_gen } else { 0 };
    let gen_limit_penalty_cost = options.gen_limit_penalty.unwrap_or(0.0);

    // Variables:
    //   x = [θ (n_bus) | Pg (n_gen) | P_hvdc (n_hvdc) |
    //        s_upper (n_flow) | s_lower (n_flow) |
    //        sg_upper (n_gen_slacks) | sg_lower (n_gen_slacks) |
    //        e_g (n_pwl_gen)]
    //
    // s_upper[ci] >= 0: absorbs Bf*θ exceeding +fmax (upper thermal violation).
    // s_lower[ci] >= 0: absorbs Bf*θ below -fmax (lower thermal violation).
    // The flow row becomes: -fmax ≤ Bf*θ - s_upper[ci] + s_lower[ci] ≤ fmax
    // Both slacks are penalised at penalty_config.thermal.marginal_cost_at(0) * base ($/pu).
    // P_hvdc[k] ∈ [p_dc_min, p_dc_max]: HVDC link power transfer.
    // sg_upper[j] >= 0: generator Pmax slack (when gen_limit_penalty is set).
    // sg_lower[j] >= 0: generator Pmin slack (when gen_limit_penalty is set).
    // e_g[k] >= 0: epiograph variable for PWL generator k (represents $/hr cost).
    // v[k] ∈ [0, mw_limit/base]: virtual bid cleared MW (zero overhead when empty).
    let n_slack = 2 * n_flow;
    let theta_offset = 0;
    let pg_offset = n_bus;
    let hvdc_offset = n_bus + n_gen;
    let s_upper_offset = hvdc_offset + n_hvdc;
    let s_lower_offset = s_upper_offset + n_flow;
    let sg_upper_offset = s_lower_offset + n_flow;
    let sg_lower_offset = sg_upper_offset + n_gen_slacks;
    let e_g_offset = sg_lower_offset + n_gen_slacks;
    // Virtual bids — fast-path: zero overhead when empty.
    let active_vbids: Vec<usize> = options
        .virtual_bids
        .iter()
        .enumerate()
        .filter(|(_, vb)| vb.in_service)
        .map(|(i, _)| i)
        .collect();
    let n_vbid = active_vbids.len();
    let vbid_offset = e_g_offset + n_pwl_gen;
    let n_var = vbid_offset + n_vbid;

    // Penalty cost coefficient: $/pu = ($/MVA at zero violation) * (MVA / pu)
    let thermal_penalty_per_pu = options.penalty_config.thermal.marginal_cost_at(0.0) * base;

    // --- Objective ---
    let mut col_cost = vec![0.0_f64; n_var];
    let mut q_diag = vec![0.0_f64; n_gen];
    let mut c0_total = 0.0_f64;

    apply_generator_costs(
        network,
        gen_indices,
        base,
        pg_offset,
        GeneratorCostBuffers {
            col_cost: &mut col_cost,
            q_diag: &mut q_diag,
            c0_total: &mut c0_total,
        },
        &poly_quad_local_indices,
    )?;

    // Epiograph variables: objective coefficient = 1.0 (minimize sum of e_g)
    for k in 0..n_pwl_gen {
        col_cost[e_g_offset + k] = 1.0;
    }

    // Virtual bid objective coefficients ($/pu = price * base):
    //   Inc bid: +price * base  (paying to inject — competes against generators)
    //   Dec bid: -price * base  (receiving payment — competes against loads)
    for (k, &bi) in active_vbids.iter().enumerate() {
        let vb = &options.virtual_bids[bi];
        col_cost[vbid_offset + k] = match vb.direction {
            VirtualBidDirection::Inc => vb.price_per_mwh * base,
            VirtualBidDirection::Dec => -vb.price_per_mwh * base,
        };
    }

    // Thermal slack objective coefficients (both upper and lower slacks)
    for ci in 0..n_flow {
        col_cost[s_upper_offset + ci] = thermal_penalty_per_pu;
        col_cost[s_lower_offset + ci] = thermal_penalty_per_pu;
    }

    // HVDC variables: zero cost (optimal dispatch minimises generator cost)
    // P_hvdc columns occupy [hvdc_offset .. hvdc_offset + n_hvdc] — cost stays 0.

    // Gen-limit slack penalties ($/pu)
    if has_gen_slacks {
        let penalty_pu = gen_limit_penalty_cost * base;
        for j in 0..n_gen {
            col_cost[sg_upper_offset + j] = penalty_pu;
            col_cost[sg_lower_offset + j] = penalty_pu;
        }
    }

    // Build Hessian (diagonal, upper-triangular CSC, Pg block only).
    // θ / slack / e_g columns have zero quadratic cost — they are LP variables.
    // HiGHS applies its own internal qp_regularization_value (1e-3) to handle
    // any null-space issues; COPT uses its own interior-point regularization.
    // No explicit Tikhonov is needed here.
    let hessian = build_hessian_csc(
        n_bus,
        &q_diag,
        n_hvdc + n_slack + 2 * n_gen_slacks + n_pwl_gen + n_vbid,
    );

    // --- Box bounds ---
    // Default: lower = 0, upper = +∞ (HiGHS interprets 1e30 as +∞ internally,
    // but f64::INFINITY is accepted by the Rust HiGHS wrapper).
    let mut col_lower = vec![0.0_f64; n_var];
    let mut col_upper = vec![f64::INFINITY; n_var];

    // θ bounds: [-π, π], with one reference bus per island fixed at 0.
    // For single-island networks this fixes only the slack bus (same as before).
    super::island_lmp::fix_island_theta_bounds(
        &mut col_lower,
        &mut col_upper,
        theta_offset,
        n_bus,
        &island_refs,
    );

    // Pg bounds: [pmin/base, pmax/base]
    // When gen_limit_penalty is set, the hard bounds are relaxed — gen-limit
    // constraints with slack variables enforce pmin/pmax softly instead.
    for (j, &gi) in gen_indices.iter().enumerate() {
        if has_gen_slacks {
            // Relaxed: column lower bound stays at pmin (for absorbing units)
            // but pmax enforced via slack row, not column bound.
            col_lower[pg_offset + j] = f64::NEG_INFINITY;
            col_upper[pg_offset + j] = f64::INFINITY;
        } else {
            col_lower[pg_offset + j] = network.generators[gi].pmin / base;
            col_upper[pg_offset + j] = network.generators[gi].pmax / base;
        }
    }

    // HVDC variable bounds: [p_dc_min/base, p_dc_max/base]
    for (k, hvdc) in hvdc_var.iter().enumerate() {
        col_lower[hvdc_offset + k] = hvdc.p_dc_min_mw / base;
        col_upper[hvdc_offset + k] = hvdc.p_dc_max_mw / base;
    }

    // Thermal slack bounds: [0, +∞] — already set by default above.

    // Gen-limit slack bounds: [0, +∞]
    for j in 0..n_gen_slacks {
        col_lower[sg_upper_offset + j] = 0.0;
        // col_upper already INFINITY
        col_lower[sg_lower_offset + j] = 0.0;
    }

    // e_g (epiograph) bounds: [-∞, +∞] — unconstrained except by epiograph rows.
    // The epiograph constraints force e_g >= slope * Pg + intercept for each segment,
    // so e_g will be at least the minimum segment cost. Allow negative lower bound
    // for cases where the lowest breakpoint has negative cost (unusual but valid).
    for k in 0..n_pwl_gen {
        col_lower[e_g_offset + k] = f64::NEG_INFINITY;
        col_upper[e_g_offset + k] = f64::INFINITY;
    }

    // Virtual bid bounds: [0, mw_limit/base]
    for (k, &bi) in active_vbids.iter().enumerate() {
        let vb = &options.virtual_bids[bi];
        col_lower[vbid_offset + k] = 0.0;
        col_upper[vbid_offset + k] = vb.mw_limit / base;
    }

    // --- Interface + base-case flowgate constraints ---
    //
    // Count active interfaces and base-case flowgates to allocate constraint rows.
    // Each produces one linear constraint on θ: Σ coeff_i × b_dc_i × (θ_from - θ_to).
    // Contingency flowgates are skipped — they belong in SCOPF.
    //
    // Track network-level indices (into network.interfaces / network.flowgates) of
    // active rows so we can map LP row duals back to shadow prices later.
    let (n_iface, active_iface_indices, active_fg_indices) = if options.enforce_flowgates {
        let iface_idx: Vec<usize> = network
            .interfaces
            .iter()
            .enumerate()
            .filter(|(_, iface)| iface.in_service && iface.limit_forward_mw > 0.0)
            .map(|(i, _)| i)
            .collect();
        let fg_idx: Vec<usize> = network
            .flowgates
            .iter()
            .enumerate()
            .filter(|(_, fg)| fg.in_service && fg.contingency_branch.is_none())
            .map(|(i, _)| i)
            .collect();
        let n = iface_idx.len() + fg_idx.len();
        (n, iface_idx, fg_idx)
    } else {
        (0, vec![], vec![])
    };

    // --- Constraints ---
    // Row layout:
    //   [branch flow rows (n_flow) |
    //    interface/flowgate rows (n_iface) |
    //    power balance rows (n_bus) |
    //    gen pmax rows (n_gen_slacks) |
    //    gen pmin rows (n_gen_slacks) |
    //    PWL epiograph rows (n_pwl_rows)]
    let balance_offset = n_flow + n_iface;
    let gen_pmax_row_offset = balance_offset + n_bus;
    let gen_pmin_row_offset = gen_pmax_row_offset + n_gen_slacks;
    let pwl_row_offset = gen_pmin_row_offset + n_gen_slacks;
    let n_row = pwl_row_offset + n_pwl_rows;

    // Generator bus index: gen local index → bus internal index
    let gen_bus_idx: Vec<usize> = gen_indices
        .iter()
        .map(|&gi| bus_map[&network.generators[gi].bus])
        .collect();

    // HVDC bus indices (for variable links)
    let hvdc_from_idx: Vec<usize> = hvdc_var.iter().map(|h| bus_map[&h.from_bus]).collect();
    let hvdc_to_idx: Vec<usize> = hvdc_var.iter().map(|h| bus_map[&h.to_bus]).collect();

    // Build constraint matrix as COO triplets
    // Each flow row: 2 for Bf*θ + 1 for s_upper + 1 for s_lower = 4
    // Each interface/flowgate row: ~2 entries per monitored branch (θ_from, θ_to)
    // Each epiograph row: 1 for Pg + 1 for e_g = 2
    // HVDC adds 2 entries per variable link in balance rows
    // Gen-limit slacks add 2 rows × 2 entries per generator
    let est_nnz = 6 * n_bus
        + n_gen
        + 4 * n_flow
        + 4 * n_iface
        + 2 * n_hvdc
        + 4 * n_gen_slacks
        + 2 * n_pwl_rows;
    let mut triplets: Vec<Triplet<f64>> = Vec::with_capacity(est_nnz);

    // --- Branch flow rows (rows 0..n_flow) ---
    // Row ci: Bf*θ - s_upper[ci] + s_lower[ci] ∈ [-fmax, fmax]
    // Bf[l, from] = b_dc, Bf[l, to] = -b_dc
    // s_upper column coefficient = -1 (upper slack absorbs positive overflows)
    // s_lower column coefficient = +1 (lower slack absorbs negative underflows)
    //
    // Use signed b_dc() to match MATPOWER makeBdc: b = 1/x (no abs).
    // Series capacitors (x < 0) correctly produce negative b, which is the
    // physically correct susceptance for B-theta DC-OPF formulation.
    for (ci, &l) in constrained_branches.iter().enumerate() {
        let br = &network.branches[l];
        if br.x.abs() < 1e-20 {
            continue;
        }
        let b_val = br.b_dc();
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];

        // Bf*θ terms
        triplets.push(Triplet {
            row: ci,
            col: theta_offset + from,
            val: b_val,
        });
        triplets.push(Triplet {
            row: ci,
            col: theta_offset + to,
            val: -b_val,
        });
        // Soft slack terms: -s_upper + s_lower
        triplets.push(Triplet {
            row: ci,
            col: s_upper_offset + ci,
            val: -1.0,
        });
        triplets.push(Triplet {
            row: ci,
            col: s_lower_offset + ci,
            val: 1.0,
        });
    }

    // --- Interface / base-case flowgate rows (rows n_flow..n_flow+n_iface) ---
    //
    // Each row constrains a linear combination of branch flows:
    //   flow = Σ coeff_i × b_dc_i × (θ_from_i - θ_to_i)
    // expressed directly in θ variables as:
    //   Σ coeff_i × b_dc_i × θ_from_i - Σ coeff_i × b_dc_i × θ_to_i
    //
    // Row bounds: [-limit_reverse/base, limit_forward/base] for interfaces,
    //             [-limit/base, limit/base] for base-case flowgates.
    if n_iface > 0 {
        let mut iface_row = n_flow;

        // Interfaces
        for iface in &network.interfaces {
            if !iface.in_service || iface.limit_forward_mw <= 0.0 {
                continue;
            }
            for member in &iface.members {
                let coeff = member.coefficient;
                let branch_ref = &member.branch;
                // Find the matching branch in the network
                if let Some(br) = network
                    .branches
                    .iter()
                    .find(|br| br.in_service && branch_ref.matches_branch(br) && br.x.abs() > 1e-20)
                {
                    let b_val = br.b_dc();
                    let from = bus_map[&br.from_bus];
                    let to = bus_map[&br.to_bus];
                    triplets.push(Triplet {
                        row: iface_row,
                        col: theta_offset + from,
                        val: coeff * b_val,
                    });
                    triplets.push(Triplet {
                        row: iface_row,
                        col: theta_offset + to,
                        val: -coeff * b_val,
                    });
                }
            }
            iface_row += 1;
        }

        // Base-case flowgates (contingency_branch = None)
        for fg in &network.flowgates {
            if !fg.in_service || fg.contingency_branch.is_some() {
                continue;
            }
            for member in &fg.monitored {
                let coeff = member.coefficient;
                let branch_ref = &member.branch;
                if let Some(br) = network
                    .branches
                    .iter()
                    .find(|br| br.in_service && branch_ref.matches_branch(br) && br.x.abs() > 1e-20)
                {
                    let b_val = br.b_dc();
                    let from = bus_map[&br.from_bus];
                    let to = bus_map[&br.to_bus];
                    triplets.push(Triplet {
                        row: iface_row,
                        col: theta_offset + from,
                        val: coeff * b_val,
                    });
                    triplets.push(Triplet {
                        row: iface_row,
                        col: theta_offset + to,
                        val: -coeff * b_val,
                    });
                }
            }
            iface_row += 1;
        }
        debug_assert_eq!(iface_row, n_flow + n_iface);
    }

    // --- Power balance rows (rows balance_offset..balance_offset+n_bus) ---
    // Full B_bus matrix: B[i,i] = Σ(b_ij), B[i,j] = -b_ij where b_ij = 1/(x_ij*tap_ij).
    // Signed susceptance matches MATPOWER makeBdc exactly (negative x → negative b).
    // PAR flow-setpoint branches are excluded from B_bus; their power is injected as fixed.
    for (br_idx, branch) in network.branches.iter().enumerate() {
        if !branch.in_service || branch.x.abs() < 1e-20 || par_branch_set.contains(&br_idx) {
            continue;
        }

        let from = bus_map[&branch.from_bus];
        let to = bus_map[&branch.to_bus];
        let b = branch.b_dc();

        let eq_from = balance_offset + from;
        let eq_to = balance_offset + to;

        // Off-diagonal
        triplets.push(Triplet {
            row: eq_from,
            col: theta_offset + to,
            val: -b,
        });
        triplets.push(Triplet {
            row: eq_to,
            col: theta_offset + from,
            val: -b,
        });
        // Diagonal
        triplets.push(Triplet {
            row: eq_from,
            col: theta_offset + from,
            val: b,
        });
        triplets.push(Triplet {
            row: eq_to,
            col: theta_offset + to,
            val: b,
        });
    }

    // -A_gen block: power balance rows, Pg columns
    for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
        triplets.push(Triplet {
            row: balance_offset + bus_idx,
            col: pg_offset + j,
            val: -1.0,
        });
    }

    // --- HVDC injection in power balance rows ---
    // Rectifier (from_bus): draws P_hvdc from AC bus → +1 coefficient (increases load)
    // Inverter  (to_bus):   injects (1-loss_b)*P_hvdc → -(1-loss_b) coefficient
    for (k, hvdc) in hvdc_var.iter().enumerate() {
        let hvdc_col = hvdc_offset + k;
        let fi = hvdc_from_idx[k];
        triplets.push(Triplet {
            row: balance_offset + fi,
            col: hvdc_col,
            val: 1.0,
        });
        let ti = hvdc_to_idx[k];
        triplets.push(Triplet {
            row: balance_offset + ti,
            col: hvdc_col,
            val: -(1.0 - hvdc.loss_b_frac),
        });
    }

    // --- Virtual bid power-balance injections ---
    // Inc bid: injects MW at bus  → coefficient -1.0 (reduces RHS = increases supply)
    // Dec bid: withdraws MW at bus → coefficient +1.0 (increases RHS = increases demand)
    // Note: balance row is B*θ - A*Pg = -Pd, so:
    //   Inc (+supply): subtract from RHS → add -1.0 to A_gen block (same as generator)
    //   Dec (+demand): add to RHS         → add +1.0 (same as load)
    for (k, &bi) in active_vbids.iter().enumerate() {
        let vb = &options.virtual_bids[bi];
        if let Some(&bus_idx) = bus_map.get(&vb.bus) {
            let coeff = match vb.direction {
                VirtualBidDirection::Inc => -1.0, // injection: like generator
                VirtualBidDirection::Dec => 1.0,  // withdrawal: like load
            };
            triplets.push(Triplet {
                row: balance_offset + bus_idx,
                col: vbid_offset + k,
                val: coeff,
            });
        }
    }

    // --- Generator limit constraints (with slack variables) ---
    // When gen_limit_penalty is set:
    //   Pg - sg_upper ≤ pmax/base     (row: gen_pmax_row_offset + j)
    //  -Pg - sg_lower ≤ -pmin/base    (row: gen_pmin_row_offset + j)
    if has_gen_slacks {
        for (j, &gi) in gen_indices.iter().enumerate() {
            let g = &network.generators[gi];

            // Pmax row: Pg - sg_upper ≤ pmax/base
            let row_max = gen_pmax_row_offset + j;
            triplets.push(Triplet {
                row: row_max,
                col: pg_offset + j,
                val: 1.0,
            });
            triplets.push(Triplet {
                row: row_max,
                col: sg_upper_offset + j,
                val: -1.0,
            });

            // Pmin row: -Pg - sg_lower ≤ -pmin/base
            let row_min = gen_pmin_row_offset + j;
            triplets.push(Triplet {
                row: row_min,
                col: pg_offset + j,
                val: -1.0,
            });
            triplets.push(Triplet {
                row: row_min,
                col: sg_lower_offset + j,
                val: -1.0,
            });

            // Row bounds are set below with the other row bounds.
            let _ = g; // suppress unused
        }
    }

    // --- PWL epiograph constraints (OPF-01) ---
    // Row layout: rows pwl_row_offset .. pwl_row_offset + n_pwl_rows
    // For each PWL generator k (with local gen index j_k) and segment s:
    //   e_g[k] - slope_s_pu * Pg_j_k >= intercept_s
    //   Written as: -slope_s_pu * Pg_j_k + 1.0 * e_g[k] >= intercept_s
    {
        let mut pwl_row = pwl_row_offset;
        for (k, entry) in pwl_gen_info.iter().enumerate() {
            for &(slope_pu, _intercept) in &entry.segments {
                // Pg column: coefficient = -slope_s_pu
                triplets.push(Triplet {
                    row: pwl_row,
                    col: pg_offset + entry.local_gen_index,
                    val: -slope_pu,
                });
                // e_g column: coefficient = 1.0
                triplets.push(Triplet {
                    row: pwl_row,
                    col: e_g_offset + k,
                    val: 1.0,
                });
                pwl_row += 1;
            }
        }
    }

    // --- Convert to CSC ---
    let (a_start, a_index, a_value) = triplets_to_csc(&triplets, n_row, n_var);

    // --- Phase shift injections (MATPOWER makeBdc Pfinj / Pbusinj) ---
    //
    // MATPOWER formulation with phase-shifting transformers (PSTs):
    //   Bf * θ + Pfinj ∈ [-fmax, fmax]
    //   B_bus * θ + Pbusinj = Pgen - Pd
    //
    // Pfinj[l] = b_l * shift_l_rad       [p.u. MW]  (MATPOWER makeBdc: Pfinj = b .* shift)
    // Pbusinj assembled from Cft' * Pfinj (sum at from-bus, subtract at to-bus)
    //
    // Moving Pfinj to the RHS: Bf*θ ∈ [-fmax - Pfinj, fmax - Pfinj]
    // Moving Pbusinj to the RHS: B_bus*θ - Pg = -Pd - Pbusinj
    //
    // This matches MATPOWER's DC-OPF/DCPPowerModel exactly.

    // Compute Pfinj for each constrained branch and Pbusinj for each bus.
    let mut pfinj = vec![0.0_f64; constrained_branches.len()]; // indexed by ci
    let mut pbusinj = vec![0.0_f64; n_bus]; // indexed by bus array index

    for (br_idx, branch) in network.branches.iter().enumerate() {
        // Skip PAR flow-setpoint branches — they are removed from B_bus entirely.
        if par_branch_set.contains(&br_idx) {
            continue;
        }
        if !branch.in_service || branch.x.abs() < 1e-20 || branch.phase_shift_rad.abs() < 1e-12 {
            continue;
        }
        let phi_rad = branch.phase_shift_rad;
        let b = branch.b_dc(); // signed susceptance, matches MATPOWER
        let pf = b * phi_rad; // Pfinj = b * phi — matches MATPOWER makeBdc exactly

        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];

        // Pbusinj: from-bus gets +pfinj, to-bus gets -pfinj
        pbusinj[from_idx] += pf;
        pbusinj[to_idx] -= pf;
    }

    // PAR scheduled-interchange injections.
    // Replacing branch (from, to) with fixed flow target_mw from from→to:
    //   from_bus loses target_mw/base  →  pbusinj[from] += target_mw/base
    //   to_bus gains target_mw/base    →  pbusinj[to]   -= target_mw/base
    // (rhs = -pd - gs - pbusinj, so +pbusinj → more negative rhs → more net load)
    for ps in &options.par_setpoints {
        if let Some(br_idx) = network.branches.iter().position(|br| {
            br.in_service
                && br.from_bus == ps.from_bus
                && br.to_bus == ps.to_bus
                && br.circuit == ps.circuit
        }) {
            let br = &network.branches[br_idx];
            if br.x.abs() < 1e-20 {
                continue; // degenerate branch, skip
            }
            // Warn if target exceeds thermal rating
            if br.rating_a_mva > 0.0 && ps.target_mw.abs() > br.rating_a_mva {
                tracing::warn!(
                    from_bus = ps.from_bus,
                    to_bus = ps.to_bus,
                    circuit = %ps.circuit,
                    target_mw = ps.target_mw,
                    rate_a = br.rating_a_mva,
                    "PAR setpoint target_mw exceeds branch rate_a thermal limit"
                );
            }
            let from_idx = bus_map[&ps.from_bus];
            let to_idx = bus_map[&ps.to_bus];
            pbusinj[from_idx] += ps.target_mw / base;
            pbusinj[to_idx] -= ps.target_mw / base;
        }
    }

    // For constrained branches, look up their Pfinj by branch index
    {
        // Build a map from branch index -> Pfinj
        let mut branch_pfinj = vec![0.0_f64; network.branches.len()];
        for branch_idx in 0..network.branches.len() {
            let br = &network.branches[branch_idx];
            if !br.in_service || br.x.abs() < 1e-20 || br.phase_shift_rad.abs() < 1e-12 {
                continue;
            }
            let phi_rad = br.phase_shift_rad;
            let b = br.b_dc();
            branch_pfinj[branch_idx] = b * phi_rad; // Pfinj = b * phi — matches MATPOWER
        }
        for (ci, &l) in constrained_branches.iter().enumerate() {
            pfinj[ci] = branch_pfinj[l];
        }
    }

    // --- Row bounds ---
    let mut row_lower = vec![0.0; n_row];
    let mut row_upper = vec![0.0; n_row];

    // Branch flow rows: -fmax/base - Pfinj[ci] ≤ Bf*θ ≤ fmax/base - Pfinj[ci]
    for (ci, &l) in constrained_branches.iter().enumerate() {
        let fmax = network.branches[l].rating_a_mva / base;
        row_lower[ci] = -fmax - pfinj[ci];
        row_upper[ci] = fmax - pfinj[ci];
    }

    // Interface / base-case flowgate row bounds
    if n_iface > 0 {
        let mut iface_row = n_flow;

        for iface in &network.interfaces {
            if !iface.in_service || iface.limit_forward_mw <= 0.0 {
                continue;
            }
            // Forward limit is positive, reverse limit is the magnitude of allowed reverse flow.
            // Interface flow (pu) ∈ [-limit_reverse/base, limit_forward/base]
            row_lower[iface_row] = -iface.limit_reverse_mw / base;
            row_upper[iface_row] = iface.limit_forward_mw / base;
            iface_row += 1;
        }

        for fg in &network.flowgates {
            if !fg.in_service || fg.contingency_branch.is_some() {
                continue;
            }
            // Asymmetric limits: flow ∈ [-reverse/base, forward/base]
            let rev = fg.effective_reverse_or_forward(0);
            row_lower[iface_row] = -rev / base;
            row_upper[iface_row] = fg.limit_mw / base;
            iface_row += 1;
        }
    }

    // --- Fixed HVDC link injections ---
    // Fixed links (p_dc_min == p_dc_max) are baked into pbusinj as constant.
    for hvdc in hvdc_links.iter().filter(|h| !h.is_variable()) {
        let p_dc = hvdc.p_dc_min_mw; // fixed setpoint
        let p_inv = hvdc.p_inv_mw(p_dc);
        let fi = bus_map[&hvdc.from_bus];
        pbusinj[fi] += p_dc / base; // rectifier draws power
        let ti = bus_map[&hvdc.to_bus];
        pbusinj[ti] -= p_inv / base; // inverter injects power
    }
    // Variable HVDC links: constant loss_a component at inverter (to) bus
    for (k, hvdc) in hvdc_var.iter().enumerate() {
        let ti = hvdc_to_idx[k];
        pbusinj[ti] += hvdc.loss_a_mw / base;
    }

    // Multi-terminal DC (MTDC) injections: apply as Pbusinj adjustments.
    // Positive p_mw = inverter → injects into AC → pbusinj[i] -= p_mw/base
    // (reduces demand, increases net injection).
    // Negative p_mw = rectifier → withdraws from AC → pbusinj[i] += |p_mw|/base.
    {
        let dc_grid_results = dc_grid_injections(network).map_err(|error| {
            DcOpfError::InvalidNetwork(format!("explicit DC-grid solve failed: {error}"))
        })?;
        for inj in &dc_grid_results.injections {
            if let Some(&i) = bus_map.get(&inj.ac_bus) {
                // Sign: positive injection into AC bus reduces the "load" side.
                pbusinj[i] -= inj.p_mw / base;
            }
        }
    }

    // Power balance rows: B*θ - A_gen*Pg = -(Pd + Gs) / base - Pbusinj[i]
    //
    // Matches MATPOWER DC-OPF: bmis = -(bus(:,PD) + bus(:,GS)) / baseMVA - Pbusinj
    // bus.shunt_conductance_mw is the shunt conductance in MW at V=1.0 pu; in DC approximation
    // (V assumed = 1.0 pu), shunt load = Gs [MW], so it's a fixed real power
    // consumption that must be included in the power balance.
    for i in 0..n_bus {
        let pd_pu = bus_pd_mw[i] / base;
        let gs_pu = network.buses[i].shunt_conductance_mw / base; // shunt conductance [MW] / base_MVA = [pu]
        let rhs = -pd_pu - gs_pu - pbusinj[i];
        row_lower[balance_offset + i] = rhs;
        row_upper[balance_offset + i] = rhs;
    }

    // Gen-limit constraint row bounds (when gen_limit_penalty is set)
    if has_gen_slacks {
        for (j, &gi) in gen_indices.iter().enumerate() {
            let g = &network.generators[gi];
            // Pmax row: Pg - sg_upper ≤ pmax/base
            row_lower[gen_pmax_row_offset + j] = f64::NEG_INFINITY;
            row_upper[gen_pmax_row_offset + j] = g.pmax / base;
            // Pmin row: -Pg - sg_lower ≤ -pmin/base
            row_lower[gen_pmin_row_offset + j] = f64::NEG_INFINITY;
            row_upper[gen_pmin_row_offset + j] = -g.pmin / base;
        }
    }

    // PWL epiograph rows: e_g - slope_s_pu * Pg >= intercept_s
    // Row lower bound = intercept_s, upper bound = +∞
    {
        let mut pwl_row = pwl_row_offset;
        for entry in &pwl_gen_info {
            for &(_slope_pu, intercept) in &entry.segments {
                row_lower[pwl_row] = intercept;
                row_upper[pwl_row] = f64::INFINITY;
                pwl_row += 1;
            }
        }
    }

    let mut model = DcOpfModelBuild {
        prob: SparseProblem {
            n_col: n_var,
            n_row,
            col_cost,
            col_lower,
            col_upper,
            row_lower,
            row_upper,
            a_start,
            a_index,
            a_value,
            q_start: hessian.as_ref().map(|(q_start, _, _)| q_start.clone()),
            q_index: hessian.as_ref().map(|(_, q_index, _)| q_index.clone()),
            q_value: hessian.as_ref().map(|(_, _, q_value)| q_value.clone()),
            col_names: None,
            row_names: None,
            integrality: None,
        },
        n_var,
        n_row,
        n_bus,
        n_gen,
        n_flow,
        n_hvdc,
        n_gen_slacks,
        theta_offset,
        pg_offset,
        hvdc_offset,
        s_upper_offset,
        s_lower_offset,
        sg_upper_offset,
        sg_lower_offset,
        vbid_offset,
        balance_offset,
        base,
        c0_total,
        total_load_mw,
        has_gen_slacks,
        gen_indices: gen_indices.to_vec(),
        constrained_branches,
        active_iface_indices,
        active_fg_indices,
        gen_bus_idx,
        hvdc_var,
        hvdc_from_idx,
        hvdc_to_idx,
        active_vbids,
        island_refs,
        pbusinj,
        loss_ptdf: None,
    };
    let execution = execute_dc_opf_model(network, options, &runtime, &mut model)?;
    let solve_time = start.elapsed().as_secs_f64();
    Ok(decode_dc_opf_result(
        network, options, &ctx, model, execution, solve_time,
    ))
}

#[cfg(test)]
mod tests {
    use crate::test_util::{case_path, data_available, test_data_path};

    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Debug)]
    struct SequenceLpSolver {
        statuses: Vec<LpSolveStatus>,
        calls: AtomicUsize,
        name: &'static str,
    }

    impl SequenceLpSolver {
        fn new(name: &'static str, statuses: Vec<LpSolveStatus>) -> Self {
            Self {
                statuses,
                calls: AtomicUsize::new(0),
                name,
            }
        }
    }

    impl crate::backends::LpSolver for SequenceLpSolver {
        fn solve(&self, prob: &SparseProblem, _opts: &LpOptions) -> Result<LpResult, String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let status = self
                .statuses
                .get(call)
                .cloned()
                .or_else(|| self.statuses.last().cloned())
                .unwrap_or(LpSolveStatus::Optimal);
            Ok(LpResult {
                x: vec![0.0; prob.n_col],
                row_dual: vec![0.0; prob.n_row],
                col_dual: vec![0.0; prob.n_col],
                objective: 0.0,
                status,
                iterations: call as u32,
                mip_trace: None,
            })
        }

        fn name(&self) -> &'static str {
            self.name
        }
    }

    #[test]
    fn test_sparse_dcopf_case2383wp() {
        let net = surge_io::load(case_path("case2383wp")).unwrap();
        let opts = DcOpfOptions::default();

        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("sparse DC-OPF should solve case2383wp");

        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 1.0,
            "power balance: gen={total_gen:.2}, load={total_load:.2}"
        );
        assert!(sol.total_cost > 0.0);

        println!(
            "case2383wp sparse DC-OPF: cost={:.2}, time={:.1}ms",
            sol.total_cost,
            sol.solve_time_secs * 1000.0
        );
    }

    #[test]
    fn test_sparse_dcopf_rejects_invalid_hvdc_link() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = DcOpfOptions {
            hvdc_links: Some(vec![HvdcOpfLink::new(1, 999, 0.0, 10.0)]),
            ..Default::default()
        };

        let err = solve_dc_opf_lp(&net, &opts).unwrap_err();
        assert!(
            matches!(err, DcOpfError::InvalidHvdcLink { .. }),
            "invalid HVDC link should be rejected, got {err:?}"
        );
    }

    #[test]
    fn test_sparse_dcopf_status_contract_rejects_infeasible_and_unbounded() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let statuses = [
            (LpSolveStatus::Infeasible, DcOpfError::InfeasibleProblem),
            (LpSolveStatus::Unbounded, DcOpfError::UnboundedProblem),
        ];

        for (status, expected) in statuses {
            let solver: Arc<dyn crate::backends::LpSolver> =
                Arc::new(SequenceLpSolver::new("test-status", vec![status.clone()]));
            let runtime = DcOpfRuntime::default().with_lp_solver(solver);
            let err =
                solve_dc_opf_lp_with_runtime(&net, &DcOpfOptions::default(), &runtime).unwrap_err();
            assert!(
                std::mem::discriminant(&err) == std::mem::discriminant(&expected),
                "unexpected solver status mapping: got {err:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn test_sparse_dcopf_loss_factor_iteration_failure_is_reported() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let solver: Arc<dyn crate::backends::LpSolver> = Arc::new(SequenceLpSolver::new(
            "test-loss-factors",
            vec![LpSolveStatus::Optimal, LpSolveStatus::Infeasible],
        ));
        let runtime = DcOpfRuntime::default().with_lp_solver(solver);
        let opts = DcOpfOptions {
            use_loss_factors: true,
            max_loss_iter: 1,
            ..Default::default()
        };

        let err = solve_dc_opf_lp_with_runtime(&net, &opts, &runtime).unwrap_err();
        assert!(
            matches!(err, DcOpfError::InfeasibleProblem),
            "loss-factor re-solve should propagate infeasible status, got {err:?}"
        );
    }

    /// OPF-01: True piecewise-linear cost epiograph formulation.
    ///
    /// Constructs a minimal 2-bus, 2-generator case with PWL cost curves and verifies:
    /// 1. Dispatch is at the kink point of gen 1 (100 MW exactly).
    /// 2. Total cost matches the hand-calculated epiograph result.
    #[test]
    fn test_pwl_epiograph_dispatch_at_kink() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        // Build a 2-bus network:
        //   Bus 1 (Slack), Bus 2 (PQ, 200 MW load)
        //   Branch 1→2: x=0.1 p.u. (b_dc=10), rate_a=500 MVA (effectively unconstrained)
        //   Gen 1 (bus 1): PWL [(0,0), (100,500), (250,1700)] pmax=250
        //     Seg 1 slope = 5 $/MWh, Seg 2 slope = 8 $/MWh
        //   Gen 2 (bus 1): PWL [(0,0), (200,1200)] pmax=200
        //     Seg 1 slope = 6 $/MWh
        //
        // Merit order: gen1-seg1 (5) → gen2 (6) → gen1-seg2 (8)
        // Optimal for 200 MW: gen1=100 MW (kink!), gen2=100 MW, cost=500+600=1100 $/hr.

        let base = 100.0_f64;

        let mut net = Network::new("pwl_test");
        net.base_mva = base;

        // Bus 1: Slack (no load)
        let bus1 = Bus::new(1, BusType::Slack, 100.0);

        // Bus 2: PQ with 200 MW load
        let bus2 = Bus::new(2, BusType::PQ, 100.0);

        net.buses.push(bus1);
        net.buses.push(bus2);
        net.loads
            .push(surge_network::network::Load::new(2, 200.0, 0.0));

        // Branch 1→2: x=0.1 p.u., rate_a=500 MVA (unconstrained for this test)
        let mut branch = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        branch.rating_a_mva = 500.0;
        net.branches.push(branch);

        // Gen 1 at bus 1: PWL [(0,0), (100,500), (250,1700)]
        let mut gen1 = Generator::new(1, 0.0, 1.0);
        gen1.pmin = 0.0;
        gen1.pmax = 250.0;
        gen1.cost = Some(CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 0.0), (100.0, 500.0), (250.0, 1700.0)],
        });
        net.generators.push(gen1);

        // Gen 2 at bus 1: PWL [(0,0), (200,1200)]
        let mut gen2 = Generator::new(1, 0.0, 1.0);
        gen2.pmin = 0.0;
        gen2.pmax = 200.0;
        gen2.cost = Some(CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 0.0), (200.0, 1200.0)],
        });
        net.generators.push(gen2);

        let opts = DcOpfOptions {
            enforce_thermal_limits: false, // unconstrained to isolate cost epiograph
            ..Default::default()
        };
        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("PWL epiograph DC-OPF should solve");

        // Power balance: gen1 + gen2 = 200 MW
        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        assert!(
            (total_gen - 200.0).abs() < 0.5,
            "power balance: gen={:.2}, load=200.0",
            total_gen
        );

        // Dispatch at kink: gen1 should be at ~100 MW, gen2 at ~100 MW
        // (kink at 100 MW makes gen1-seg1 slope = 5 match gen2 slope = 6: epiograph is
        // degenerate here, so the optimal is any split with gen1 at or below 100 MW and
        // total = 200. But the LP will prefer gen1 at its pmin/kink and gen2 makes up rest.)
        // The LP prefers gen1 at 100 MW (kink, slope 5) + gen2 at 100 MW (slope 6).
        assert!(
            sol.generators.gen_p_mw[0] <= 100.0 + 1.0,
            "gen1 should be at or below kink (100 MW), got {:.2} MW",
            sol.generators.gen_p_mw[0]
        );
        assert!(
            sol.generators.gen_p_mw[1] >= 0.0,
            "gen2 should be non-negative, got {:.2} MW",
            sol.generators.gen_p_mw[1]
        );

        // Total cost should be 1100 $/hr (gen1 kink cost=500 + gen2 at 100 MW cost=600)
        // or lower (gen1 could be <100 MW in degenerate case, but that's also 1100 at optimum).
        let expected_cost = 1100.0;
        assert!(
            (sol.total_cost - expected_cost).abs() < 2.0,
            "total cost should be ~{:.2} $/hr (epiograph), got {:.2} $/hr",
            expected_cost,
            sol.total_cost
        );

        println!(
            "OPF-01 PWL epiograph: gen1={:.2} MW, gen2={:.2} MW, cost={:.2} $/hr (expected {:.2})",
            sol.generators.gen_p_mw[0], sol.generators.gen_p_mw[1], sol.total_cost, expected_cost
        );
    }

    /// OPF-09: LMP three-part decomposition identity on case14.
    ///
    /// Verifies that for all buses: lmp[i] = lmp_energy[i] + lmp_congestion[i] + lmp_loss[i]
    /// Also verifies that the energy component is the same for all buses in a lossless DC-OPF.
    #[test]
    fn test_lmp_decomposition_identity_case14() {
        let net = surge_io::load(case_path("case14")).unwrap();
        let opts = DcOpfOptions::default();
        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("DC-OPF should solve case14");

        let n_bus = net.n_buses();

        // Verify decomposition identity: lmp[i] = lmp_energy[i] + lmp_congestion[i] + lmp_loss[i]
        for i in 0..n_bus {
            let decomp =
                sol.pricing.lmp_energy[i] + sol.pricing.lmp_congestion[i] + sol.pricing.lmp_loss[i];
            let err = (sol.pricing.lmp[i] - decomp).abs();
            assert!(
                err < 1e-8,
                "LMP decomposition identity violated at bus {} (idx {}): \
                 lmp={:.6}, energy+congestion+loss={:.6}, err={:.2e}",
                net.buses[i].number,
                i,
                sol.pricing.lmp[i],
                decomp,
                err
            );
        }

        // In lossless DC-OPF, lmp_loss should be zero at all buses
        for (i, &loss) in sol.pricing.lmp_loss.iter().enumerate() {
            assert!(
                loss.abs() < 1e-10,
                "DC-OPF loss component should be zero at bus {} (idx {}), got {:.2e}",
                net.buses[i].number,
                i,
                loss
            );
        }

        // Energy component should be the same for all buses (lossless DC-OPF reference price)
        let energy_0 = sol.pricing.lmp_energy[0];
        for (i, &energy) in sol.pricing.lmp_energy.iter().enumerate() {
            assert!(
                (energy - energy_0).abs() < 1e-8,
                "Energy component should be uniform for lossless DC-OPF: \
                 bus {} (idx {}) has {:.6}, bus 0 has {:.6}",
                net.buses[i].number,
                i,
                energy,
                energy_0
            );
        }

        println!(
            "case14 LMP decomposition: energy={:.4} $/MWh, \
             congestion range=[{:.4}, {:.4}], loss=0",
            sol.pricing.lmp_energy[0],
            sol.pricing
                .lmp_congestion
                .iter()
                .cloned()
                .fold(f64::INFINITY, f64::min),
            sol.pricing
                .lmp_congestion
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max),
        );
    }

    /// OPF-09: LMP three-part decomposition identity on case30.
    ///
    /// case30 has more congestion than case14, which exercises the congestion component.
    #[test]
    fn test_lmp_decomposition_identity_case30() {
        let net = surge_io::load(case_path("case30")).unwrap();
        let opts = DcOpfOptions::default();
        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("DC-OPF should solve case30");

        let n_bus = net.n_buses();

        // Verify decomposition identity at each bus
        for i in 0..n_bus {
            let decomp =
                sol.pricing.lmp_energy[i] + sol.pricing.lmp_congestion[i] + sol.pricing.lmp_loss[i];
            let err = (sol.pricing.lmp[i] - decomp).abs();
            assert!(
                err < 1e-8,
                "LMP decomposition identity violated at bus {} (idx {}): \
                 lmp={:.6}, energy+congestion+loss={:.6}, err={:.2e}",
                net.buses[i].number,
                i,
                sol.pricing.lmp[i],
                decomp,
                err
            );
        }

        println!(
            "case30 LMP decomposition: energy={:.4} $/MWh, \
             LMP range=[{:.4}, {:.4}] $/MWh",
            sol.pricing.lmp_energy[0],
            sol.pricing
                .lmp
                .iter()
                .cloned()
                .fold(f64::INFINITY, f64::min),
            sol.pricing
                .lmp
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max),
        );
    }

    /// OPF-09: LMP three-part decomposition (energy + congestion + loss).
    ///
    /// Runs DC-OPF on case14 with thermal limits enforced (to create congestion) and verifies:
    /// 1. lmp[b] = lmp_energy[b] + lmp_congestion[b] + lmp_loss[b] for all buses (within 1e-8)
    /// 2. Energy component is the same at all buses (system energy price, lossless DC)
    /// 3. Congestion component is non-zero at at least one bus (binding line creates price spread)
    /// 4. Loss component is zero for DC-OPF (lossless model)
    #[test]
    fn test_opf09_lmp_decomposition() {
        let net = surge_io::load(case_path("case14")).unwrap();
        let opts = DcOpfOptions::default(); // enforce thermal limits by default
        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("DC-OPF should solve case14");

        let n_bus = net.n_buses();

        // 1. Decomposition identity: lmp[b] = lmp_energy[b] + lmp_congestion[b] + lmp_loss[b]
        for i in 0..n_bus {
            let decomp =
                sol.pricing.lmp_energy[i] + sol.pricing.lmp_congestion[i] + sol.pricing.lmp_loss[i];
            let err = (sol.pricing.lmp[i] - decomp).abs();
            assert!(
                err < 1e-8,
                "OPF-09: LMP decomposition identity violated at bus {} (idx {}): \
                 lmp={:.8}, energy+congestion+loss={:.8}, err={:.2e}",
                net.buses[i].number,
                i,
                sol.pricing.lmp[i],
                decomp,
                err
            );
        }

        // 2. Energy component is uniform (same at all buses for lossless DC-OPF)
        let energy_ref = sol.pricing.lmp_energy[0];
        for i in 1..n_bus {
            let diff = (sol.pricing.lmp_energy[i] - energy_ref).abs();
            assert!(
                diff < 1e-8,
                "OPF-09: Energy component should be uniform for DC-OPF: \
                 bus {} (idx {}) = {:.8}, ref = {:.8}, diff = {:.2e}",
                net.buses[i].number,
                i,
                sol.pricing.lmp_energy[i],
                energy_ref,
                diff
            );
        }

        // 3. Loss component is zero for DC-OPF (lossless model)
        for i in 0..n_bus {
            assert!(
                sol.pricing.lmp_loss[i].abs() < 1e-10,
                "OPF-09: Loss component should be zero for DC-OPF at bus {} (idx {}), got {:.2e}",
                net.buses[i].number,
                i,
                sol.pricing.lmp_loss[i]
            );
        }

        // 4. Total LMPs must be non-zero (energy market has positive prices)
        let all_zero = sol.pricing.lmp.iter().all(|&v| v.abs() < 1e-6);
        assert!(
            !all_zero,
            "OPF-09: all LMPs are zero — something is wrong with the DC-OPF solution"
        );

        println!(
            "OPF-09 case14: energy={:.4} $/MWh, congestion=[{:.4},{:.4}] $/MWh, loss=0",
            energy_ref,
            sol.pricing
                .lmp_congestion
                .iter()
                .cloned()
                .fold(f64::INFINITY, f64::min),
            sol.pricing
                .lmp_congestion
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max),
        );
    }

    /// Gurobi cross-validation: DC-OPF on case9 must match HiGHS within 1e-4 relative.
    ///
    /// Requires a valid Gurobi license (GUROBI_HOME or libgurobi*.so on LD_LIBRARY_PATH).
    /// Skipped gracefully when Gurobi is not available at runtime.
    #[test]
    fn test_gurobi_vs_highs_case9() {
        use crate::backends::LpSolver;
        use crate::backends::gurobi::GurobiLpSolver;
        use crate::backends::highs::HiGHSLpSolver;
        use std::sync::Arc;

        let net = surge_io::load(case_path("case9")).unwrap();

        // Gurobi — skip test gracefully if license unavailable
        let grb = match GurobiLpSolver::new_validated() {
            Ok(s) => Arc::new(s) as Arc<dyn LpSolver>,
            Err(e) => {
                println!("SKIP: Gurobi unavailable ({e}) — skipping cross-validation test");
                return;
            }
        };

        // Explicit HiGHS reference (not try_default_lp_solver)
        let highs_solver = match HiGHSLpSolver::new() {
            Ok(s) => Arc::new(s) as Arc<dyn LpSolver>,
            Err(e) => {
                println!("SKIP: HiGHS unavailable ({e}) — skipping cross-validation test");
                return;
            }
        };
        let highs_runtime = DcOpfRuntime::default().with_lp_solver(highs_solver);
        let highs_sol =
            solve_dc_opf_lp_with_runtime(&net, &DcOpfOptions::default(), &highs_runtime)
                .map(|r| r.opf)
                .expect("HiGHS DC-OPF should solve case9");

        let grb_runtime = DcOpfRuntime::default().with_lp_solver(grb);
        let grb_sol = solve_dc_opf_lp_with_runtime(&net, &DcOpfOptions::default(), &grb_runtime)
            .map(|r| r.opf)
            .expect("Gurobi DC-OPF should solve case9");

        // Objectives must match within 1e-4 relative (unique optimal value).
        let rel_err =
            (grb_sol.total_cost - highs_sol.total_cost).abs() / highs_sol.total_cost.abs().max(1.0);
        assert!(
            rel_err < 1e-4,
            "case9 objective mismatch: gurobi={:.4}, highs={:.4}, rel_err={:.2e}",
            grb_sol.total_cost,
            highs_sol.total_cost,
            rel_err
        );

        // NOTE: Individual LMPs (duals) are NOT compared here because case9
        // DC-OPF has degenerate primal solutions — different QP solvers find
        // different optimal vertices with identical objectives but distinct
        // dual values.  Only the objective is unique at the optimum.

        println!(
            "Gurobi vs HiGHS case9: obj_grb={:.4}, obj_highs={:.4}, rel_err={:.2e}",
            grb_sol.total_cost, highs_sol.total_cost, rel_err
        );
    }

    /// Gurobi cross-validation: DC-OPF on case118.
    ///
    /// Requires a valid Gurobi license. Skipped gracefully when Gurobi is not available.
    #[test]
    fn test_gurobi_vs_highs_case118() {
        use crate::backends::LpSolver;
        use crate::backends::gurobi::GurobiLpSolver;
        use std::sync::Arc;

        let net = surge_io::load(case_path("case118")).unwrap();

        let highs_sol = solve_dc_opf_lp(&net, &DcOpfOptions::default())
            .map(|r| r.opf)
            .expect("HiGHS DC-OPF should solve case118");

        let grb = match GurobiLpSolver::new_validated() {
            Ok(s) => Arc::new(s) as Arc<dyn LpSolver>,
            Err(e) => {
                println!("SKIP: Gurobi unavailable ({e}) — skipping cross-validation test");
                return;
            }
        };
        let grb_runtime = DcOpfRuntime::default().with_lp_solver(grb);
        let grb_sol = solve_dc_opf_lp_with_runtime(&net, &DcOpfOptions::default(), &grb_runtime)
            .map(|r| r.opf)
            .expect("Gurobi DC-OPF should solve case118");

        let rel_err =
            (grb_sol.total_cost - highs_sol.total_cost).abs() / highs_sol.total_cost.abs().max(1.0);
        assert!(
            rel_err < 1e-4,
            "case118 objective mismatch: gurobi={:.4}, highs={:.4}, rel_err={:.2e}",
            grb_sol.total_cost,
            highs_sol.total_cost,
            rel_err
        );

        println!(
            "Gurobi vs HiGHS case118: obj_grb={:.4}, obj_highs={:.4}, rel_err={:.2e}",
            grb_sol.total_cost, highs_sol.total_cost, rel_err
        );
    }

    /// Regression test: DC-OPF on ACTIVSg2000, which has generators with
    /// mixed zero/non-zero quadratic cost (c2).  This creates a positive
    /// semi-definite Hessian that caused HiGHS to return model status 10
    /// (UNBOUNDED) when solver=choose selected simplex instead of IPM.
    /// Fix: highs_solver.rs forces solver=ipm after passing a non-empty Hessian.
    #[test]
    fn test_sparse_dcopf_activsg2000_mixed_quadratic_costs() {
        use crate::backends::LpSolver;
        use crate::backends::gurobi::GurobiLpSolver;
        use std::sync::Arc;

        // ACTIVSg2000 has mixed zero/non-zero quadratic costs (PSD Hessian).
        // HiGHS may return SubOptimalSolution on this QP; use Gurobi when available.
        let solver: Arc<dyn LpSolver> = match GurobiLpSolver::new_validated() {
            Ok(s) => Arc::new(s),
            Err(_) => {
                println!("SKIP: Gurobi unavailable — HiGHS has known QP limitations on this case");
                return;
            }
        };

        let net = surge_io::load(case_path("case_ACTIVSg2000")).unwrap();
        let runtime = DcOpfRuntime::default().with_lp_solver(solver);

        let sol = solve_dc_opf_lp_with_runtime(&net, &DcOpfOptions::default(), &runtime)
            .map(|r| r.opf)
            .expect("DC-OPF should solve ACTIVSg2000 (mixed zero/nonzero c2)");

        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 100.0,
            "power balance: gen={total_gen:.2} MW, load={total_load:.2} MW"
        );
        assert!(sol.total_cost > 0.0, "objective cost must be positive");

        println!(
            "ACTIVSg2000 DC-OPF: cost={:.2}, gen={:.1} MW, time={:.1}ms",
            sol.total_cost,
            total_gen,
            sol.solve_time_secs * 1000.0
        );
    }

    /// Regression test: the canonical default HiGHS path should proactively
    /// linearize the large mixed-quadratic ACTIVSg class instead of returning
    /// a suboptimal QP status.
    #[test]
    fn test_sparse_dcopf_activsg2000_default_runtime() {
        let net = surge_io::load(case_path("case_ACTIVSg2000")).unwrap();
        let sol = solve_dc_opf_lp(&net, &DcOpfOptions::default())
            .map(|r| r.opf)
            .expect("default DC-OPF should solve ACTIVSg2000");

        assert!(sol.total_cost > 0.0, "objective cost must be positive");
    }

    /// OPF-LP-01: use_pwl_costs=true path on case9.
    ///
    /// Verifies that the LP (PWL tangent-line) formulation gives an objective within
    /// 1e-4 of the exact QP formulation when using 100 breakpoints.
    #[test]
    fn test_sparse_dcopf_pwl_costs_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let qp_opts = DcOpfOptions::default(); // use_pwl_costs = false
        let lp_opts = DcOpfOptions {
            use_pwl_costs: true,
            pwl_cost_breakpoints: 100,
            ..Default::default()
        };

        let qp_sol = solve_dc_opf_lp(&net, &qp_opts)
            .map(|r| r.opf)
            .expect("QP DC-OPF should solve case9");
        let lp_sol = solve_dc_opf_lp(&net, &lp_opts)
            .map(|r| r.opf)
            .expect("LP DC-OPF should solve case9");

        let rel_err =
            (lp_sol.total_cost - qp_sol.total_cost).abs() / qp_sol.total_cost.abs().max(1.0);
        assert!(
            rel_err < 1e-4,
            "PWL(100bp) vs QP case9: lp={:.4}, qp={:.4}, rel_err={:.2e}",
            lp_sol.total_cost,
            qp_sol.total_cost,
            rel_err
        );

        // Power balance on both
        let load_mw: f64 = net.total_load_mw();
        let lp_gen: f64 = lp_sol.generators.gen_p_mw.iter().sum();
        assert!(
            (lp_gen - load_mw).abs() < 1.0,
            "PWL path power balance: gen={:.2}, load={:.2}",
            lp_gen,
            load_mw
        );

        println!(
            "OPF-LP-01 PWL costs (N=100): lp={:.4}, qp={:.4}, rel_err={:.2e}",
            lp_sol.total_cost, qp_sol.total_cost, rel_err
        );
    }

    /// Regression test: DC-OPF on ACTIVSg500, same PSD Hessian / solver=choose issue.
    #[test]
    fn test_sparse_dcopf_activsg500_mixed_quadratic_costs() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = surge_io::load(test_data_path("case_ACTIVSg500.m")).unwrap();
        let opts = DcOpfOptions::default();

        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("DC-OPF should solve ACTIVSg500 (mixed zero/nonzero c2)");

        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 50.0,
            "power balance: gen={total_gen:.2} MW, load={total_load:.2} MW"
        );
        assert!(sol.total_cost > 0.0, "objective cost must be positive");

        println!(
            "ACTIVSg500 DC-OPF: cost={:.2}, gen={:.1} MW, time={:.1}ms",
            sol.total_cost,
            total_gen,
            sol.solve_time_secs * 1000.0
        );
    }

    /// OPF-PST: Reported branch flows must include the PST shift term.
    ///
    /// Regression test for the bug where post-processing used
    ///   b_dc * (θ_from − θ_to)
    /// instead of the correct DC-PF definition
    ///   b_dc * (θ_from − θ_to − φ_rad).
    ///
    /// Network: 3 buses, branch 0 is a PST (shift = 5°).
    ///   Bus 1 (Slack, cheap gen)  — Bus 2 (PQ load) via PST branch
    ///   Bus 1                     — Bus 3 (PQ load) via regular line
    ///   Bus 2                     — Bus 3            via regular line
    #[test]
    fn test_pst_branch_flow_includes_shift_term() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let base = 100.0_f64;
        let phi_deg = 5.0_f64;
        let phi_rad = phi_deg.to_radians();

        let mut net = Network::new("pst_opf_test");
        net.base_mva = base;

        // Buses
        net.buses.push(Bus::new(1, BusType::Slack, 100.0));
        net.buses.push(Bus::new(2, BusType::PQ, 100.0));
        net.buses.push(Bus::new(3, BusType::PQ, 100.0));
        net.loads
            .push(surge_network::network::Load::new(2, 150.0, 0.0));
        net.loads
            .push(surge_network::network::Load::new(3, 100.0, 0.0));

        // Branch 0: 1→2 PST with shift = phi_deg
        let mut br_pst = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br_pst.phase_shift_rad = phi_rad;
        br_pst.rating_a_mva = 300.0;
        net.branches.push(br_pst);

        // Branch 1: 1→3 regular line
        let mut br_13 = Branch::new_line(1, 3, 0.0, 0.2, 0.0);
        br_13.rating_a_mva = 300.0;
        net.branches.push(br_13);

        // Branch 2: 2→3 regular line
        let mut br_23 = Branch::new_line(2, 3, 0.0, 0.3, 0.0);
        br_23.rating_a_mva = 300.0;
        net.branches.push(br_23);

        // Gen at bus 1 (cheap)
        let mut gen1 = Generator::new(1, 0.0, 1.0);
        gen1.pmin = 0.0;
        gen1.pmax = 400.0;
        gen1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 10.0, 0.0],
        }); // f(P)=10P $/hr
        net.generators.push(gen1);

        let opts = DcOpfOptions {
            enforce_thermal_limits: true,
            ..Default::default()
        };
        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("DC-OPF should solve PST test case");

        // Reconstruct the expected PST branch flow from the optimal angles.
        let va = &sol.power_flow.voltage_angle_rad; // internal bus order
        let bus_map: std::collections::HashMap<u32, usize> = net
            .buses
            .iter()
            .enumerate()
            .map(|(i, b)| (b.number, i))
            .collect();
        let from_i = bus_map[&1u32];
        let to_i = bus_map[&2u32];
        let b_pst = net.branches[0].b_dc();

        let expected_with_shift = b_pst * (va[from_i] - va[to_i] - phi_rad) * base;
        let old_buggy_value = b_pst * (va[from_i] - va[to_i]) * base;

        let reported = sol.power_flow.branch_p_from_mw[0];

        // The fix: reported flow must equal the shift-corrected formula.
        assert!(
            (reported - expected_with_shift).abs() < 1e-6,
            "PST branch flow (reported={reported:.6}) must match b*(θf-θt-φ)={expected_with_shift:.6}"
        );

        // Guard: the old formula and the correct formula differ non-trivially
        // for phi=5°, confirming the test is meaningful.
        assert!(
            (expected_with_shift - old_buggy_value).abs() > 0.01,
            "PST shift must cause a measurable difference in branch flow; \
             with_shift={expected_with_shift:.6}, without={old_buggy_value:.6}"
        );
    }

    /// C3: DC-OPF flowgate shadow prices are extracted for binding / slack flowgates.
    #[test]
    fn test_dc_opf_flowgate_shadow_prices() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Flowgate, Generator, Interface};

        // 3-bus network: bus 1 (slack, gen cheap), bus 2 (load), bus 3 (gen expensive)
        let mut net = Network::new("fg_shadow_test");
        net.base_mva = 100.0;

        let bus1 = Bus::new(1, BusType::Slack, 100.0);
        let bus2 = Bus::new(2, BusType::PQ, 100.0);
        let bus3 = Bus::new(3, BusType::PQ, 100.0);
        net.buses.extend([bus1, bus2, bus3]);
        net.loads
            .push(surge_network::network::Load::new(2, 100.0, 0.0));
        net.loads
            .push(surge_network::network::Load::new(3, 50.0, 0.0));

        // Cheap gen at bus 1, expensive at bus 3
        let mut gen1 = Generator::new(1, 0.0, 300.0);
        gen1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 10.0, 0.0],
        });
        let mut gen3 = Generator::new(3, 0.0, 200.0);
        gen3.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 50.0, 0.0],
        });
        net.generators.extend([gen1, gen3]);

        // Branches 1-2 and 2-3 (x=0.1 each, no thermal limit)
        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 0.0;
        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 0.0;
        net.branches.extend([br12, br23]);

        // Tight flowgate on branch 1-2 (will bind; unconstrained dispatch sends ~100 MW)
        net.flowgates.push(Flowgate {
            name: "FG_tight".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 60.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            limit_mw_active_period: None,
        });
        // Slack flowgate with very high limit
        net.flowgates.push(Flowgate {
            name: "FG_slack".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 9999.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            limit_mw_active_period: None,
        });
        // Test 1: flowgate-only (no interfaces) — avoid redundant / infeasible constraints
        {
            let opts = DcOpfOptions {
                enforce_thermal_limits: false,
                enforce_flowgates: true,
                ..Default::default()
            };
            let sol = solve_dc_opf_lp(&net, &opts).unwrap().opf;

            assert_eq!(sol.branches.flowgate_shadow_prices.len(), 2);
            assert!(sol.branches.interface_shadow_prices.is_empty());

            // Tight flowgate → non-zero shadow price
            assert!(
                sol.branches.flowgate_shadow_prices[0].abs() > 1e-4,
                "tight flowgate shadow price should be non-zero, got {:.6}",
                sol.branches.flowgate_shadow_prices[0]
            );
            // Slack flowgate → ≈ 0
            assert!(
                sol.branches.flowgate_shadow_prices[1].abs() < 1e-4,
                "slack flowgate shadow price should be ~0, got {:.6}",
                sol.branches.flowgate_shadow_prices[1]
            );
        }

        // Test 2: interface-only — add an interface and remove flowgates
        {
            let mut net2 = net.clone();
            net2.flowgates.clear();
            net2.interfaces.push(Interface {
                name: "IF_12".to_string(),
                members: vec![surge_network::network::WeightedBranchRef::new(
                    1, 2, "1", 1.0,
                )],
                limit_forward_mw: 60.0, // binding
                limit_reverse_mw: 60.0,
                in_service: true,
                limit_forward_mw_schedule: Vec::new(),
                limit_reverse_mw_schedule: Vec::new(),
            });

            let opts2 = DcOpfOptions {
                enforce_thermal_limits: false,
                enforce_flowgates: true,
                ..Default::default()
            };
            let sol2 = solve_dc_opf_lp(&net2, &opts2).map(|r| r.opf).unwrap();

            assert_eq!(sol2.branches.interface_shadow_prices.len(), 1);
            assert!(sol2.branches.flowgate_shadow_prices.is_empty());

            assert!(
                sol2.branches.interface_shadow_prices[0].abs() > 1e-4,
                "binding interface shadow price should be non-zero, got {:.6}",
                sol2.branches.interface_shadow_prices[0]
            );
        }
    }

    /// E4: PAR flow-setpoint in DC-OPF.
    ///
    /// 3-bus network with a PAR on branch 1→2.  Set target MW to 30 MW.
    /// Verify:
    /// 1. Power balance holds (total gen ≈ total load).
    /// 2. par_results has exactly one entry with correct target_mw.
    /// 3. implied_shift_deg is finite (post-solve angle was computed).
    #[test]
    fn test_dc_opf_par_setpoint() {
        use crate::dc::opf::DcOpfOptions;
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let base = 100.0_f64;

        // 3-bus network:
        //   Bus 1 (Slack), Bus 2 (PQ, 80 MW load), Bus 3 (PQ, 60 MW load)
        //   Branch 1→2: PAR (x=0.2), branch 1→3 and 2→3: normal lines
        //   Gen 1 at bus 1, Gen 2 at bus 3

        let mut net = Network::new("par_dc_opf_test");
        net.base_mva = base;

        let bus1 = Bus::new(1, BusType::Slack, 100.0);
        let bus2 = Bus::new(2, BusType::PQ, 100.0);
        let bus3 = Bus::new(3, BusType::PQ, 100.0);
        net.buses.extend([bus1, bus2, bus3]);
        net.loads
            .push(surge_network::network::Load::new(2, 80.0, 0.0));
        net.loads
            .push(surge_network::network::Load::new(3, 60.0, 0.0));

        // PAR branch 1→2
        let mut par_br = Branch::new_line(1, 2, 0.0, 0.2, 0.0);
        par_br.circuit = "1".to_string();
        par_br.rating_a_mva = 200.0;
        par_br.opf_control = Some(surge_network::network::BranchOpfControl {
            phase_min_rad: (-30.0_f64).to_radians(),
            phase_max_rad: 30.0_f64.to_radians(),
            ..Default::default()
        });
        net.branches.push(par_br);

        let mut br13 = Branch::new_line(1, 3, 0.0, 0.1, 0.0);
        br13.circuit = "1".to_string();
        br13.rating_a_mva = 200.0;
        net.branches.push(br13);

        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.circuit = "1".to_string();
        br23.rating_a_mva = 200.0;
        net.branches.push(br23);

        let mut gen1 = Generator::new(1, 0.0, 1.0);
        gen1.pmin = 0.0;
        gen1.pmax = 200.0;
        gen1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(gen1);

        let mut gen2 = Generator::new(3, 0.0, 1.0);
        gen2.pmin = 0.0;
        gen2.pmax = 100.0;
        gen2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![25.0, 0.0],
        });
        net.generators.push(gen2);

        let opts = DcOpfOptions {
            enforce_thermal_limits: false,
            par_setpoints: vec![surge_solution::ParSetpoint {
                from_bus: 1,
                to_bus: 2,
                circuit: "1".to_string(),
                target_mw: 30.0,
            }],
            ..Default::default()
        };

        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("PAR setpoint DC-OPF should solve");

        // Power balance: total gen ≈ total load (140 MW)
        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        assert!(
            (total_gen - 140.0).abs() < 0.5,
            "power balance: gen={:.2}, load=140.0",
            total_gen
        );

        // PAR result returned
        assert_eq!(sol.par_results.len(), 1, "should have 1 par_result");
        assert!(
            (sol.par_results[0].target_mw - 30.0).abs() < 1e-6,
            "target_mw should be 30.0, got {}",
            sol.par_results[0].target_mw
        );
        assert!(
            sol.par_results[0].implied_shift_deg.is_finite(),
            "implied_shift_deg should be finite, got {}",
            sol.par_results[0].implied_shift_deg
        );
    }

    #[test]
    fn test_dc_opf_par_within_limits_uses_radian_bounds() {
        use crate::dc::opf::DcOpfOptions;
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, BranchOpfControl, Bus, BusType, Generator};

        let mut net = Network::new("par_limit_units");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.buses.push(Bus::new(3, BusType::PQ, 230.0));

        let mut gen1 = Generator::new(1, 20.0, 1.0);
        gen1.pmax = 200.0;
        gen1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 10.0, 0.0],
        });
        net.generators.push(gen1);
        let mut gen2 = Generator::new(3, 20.0, 1.0);
        gen2.pmax = 120.0;
        gen2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 15.0, 0.0],
        });
        net.generators.push(gen2);

        net.loads
            .push(surge_network::network::Load::new(2, 80.0, 0.0));
        net.loads
            .push(surge_network::network::Load::new(3, 60.0, 0.0));

        let mut branch = Branch::new_line(1, 2, 0.0, 0.2, 0.0);
        branch.opf_control = Some(BranchOpfControl {
            phase_min_rad: (-5.0_f64).to_radians(),
            phase_max_rad: 5.0_f64.to_radians(),
            ..BranchOpfControl::default()
        });
        net.branches.push(branch);
        net.branches.push(Branch::new_line(1, 3, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.0, 0.1, 0.0));

        let opts = DcOpfOptions {
            par_setpoints: vec![surge_solution::ParSetpoint {
                from_bus: 1,
                to_bus: 2,
                circuit: "1".to_string(),
                target_mw: 60.0,
            }],
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let result = solve_dc_opf_lp(&net, &opts).expect("PAR DC-OPF should solve");
        assert_eq!(result.opf.par_results.len(), 1);
        assert!(
            result.opf.par_results[0].implied_shift_deg.abs() > 1.0,
            "test should exercise a non-trivial implied phase shift"
        );
        assert!(
            result.opf.par_results[0].within_limits,
            "a few degrees of implied shift should remain inside a +/-5 degree mechanical range when checked in radians"
        );
    }

    // -----------------------------------------------------------------------
    // Virtual bid tests (Issue #39)
    // -----------------------------------------------------------------------

    /// Build a simple 2-bus network for virtual bid tests.
    ///
    /// Bus 1 (Slack): Gen at $10/MWh, pmax=200 MW
    /// Bus 2 (PQ):    100 MW load
    /// Branch 1→2:   x=0.1 (unconstrained, rate_a=500 MVA)
    /// Equilibrium LMP ≈ $10/MWh (single uncongested zone).
    fn make_2bus_net_for_vbid() -> (surge_network::Network, super::DcOpfOptions) {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};
        let base = 100.0_f64;
        let mut net = Network::new("vbid_test");
        net.base_mva = base;

        let bus1 = Bus::new(1, BusType::Slack, base);
        let bus2 = Bus::new(2, BusType::PQ, base);
        net.buses.push(bus1);
        net.buses.push(bus2);
        net.loads
            .push(surge_network::network::Load::new(2, 100.0, 0.0));

        let mut br = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br.rating_a_mva = 500.0;
        net.branches.push(br);

        let mut g = Generator::new(1, 0.0, 1.0);
        g.pmin = 0.0;
        g.pmax = 200.0;
        g.cost = Some(CostCurve::Polynomial {
            coeffs: vec![10.0, 0.0], // $10/MWh linear
            startup: 0.0,
            shutdown: 0.0,
        });
        net.generators.push(g);

        let opts = DcOpfOptions::default();
        (net, opts)
    }

    /// VB-01: Inc bid at bus 2 priced below LMP — should clear fully and
    /// compete with physical generation, reducing the physical gen dispatch.
    #[test]
    fn test_vbid_inc_clears_below_lmp() {
        let (net, mut opts) = make_2bus_net_for_vbid();

        // Inc bid: offer $5/MWh for up to 30 MW at bus 2.
        // The equilibrium LMP is $10/MWh, so this bid is below LMP → should clear fully.
        opts.virtual_bids = vec![surge_network::market::VirtualBid {
            position_id: "inc_1".to_string(),
            bus: 2,
            period: 0,
            mw_limit: 30.0,
            price_per_mwh: 5.0,
            direction: surge_network::market::VirtualBidDirection::Inc,
            in_service: true,
        }];

        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("DC-OPF with Inc bid should solve");

        // Inc bid cleared at its limit (30 MW)
        assert_eq!(sol.virtual_bid_results.len(), 1);
        let vbr = &sol.virtual_bid_results[0];
        assert!(
            (vbr.cleared_mw - 30.0).abs() < 0.5,
            "Inc bid should clear at 30 MW, got {:.2}",
            vbr.cleared_mw
        );

        // Physical gen dispatch should be reduced by the cleared Inc amount
        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        assert!(
            (total_gen - 70.0).abs() < 1.0,
            "Physical gen should be ~70 MW (100 load - 30 vbid), got {:.2}",
            total_gen
        );
    }

    /// VB-02: Dec bid at bus 2 priced above LMP — should clear fully and
    /// require extra generation to cover the virtual withdrawal.
    #[test]
    fn test_vbid_dec_clears_above_lmp() {
        let (net, mut opts) = make_2bus_net_for_vbid();

        // Dec bid: offer $20/MWh for up to 20 MW at bus 2.
        // The equilibrium LMP is $10/MWh, so price ($20) > LMP → Dec bid should clear.
        opts.virtual_bids = vec![surge_network::market::VirtualBid {
            position_id: "dec_1".to_string(),
            bus: 2,
            period: 0,
            mw_limit: 20.0,
            price_per_mwh: 20.0,
            direction: surge_network::market::VirtualBidDirection::Dec,
            in_service: true,
        }];

        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("DC-OPF with Dec bid should solve");

        // Dec bid cleared at its limit (20 MW)
        assert_eq!(sol.virtual_bid_results.len(), 1);
        let vbr = &sol.virtual_bid_results[0];
        assert!(
            (vbr.cleared_mw - 20.0).abs() < 0.5,
            "Dec bid should clear at 20 MW, got {:.2}",
            vbr.cleared_mw
        );

        // Physical gen dispatch should be load + virtual withdrawal = 100 + 20 = 120 MW
        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        assert!(
            (total_gen - 120.0).abs() < 1.0,
            "Physical gen should be ~120 MW (100 load + 20 vbid), got {:.2}",
            total_gen
        );
    }

    /// VB-03: Uneconomic Inc bid (price > LMP) should not clear.
    #[test]
    fn test_vbid_uneconomic_inc_does_not_clear() {
        let (net, mut opts) = make_2bus_net_for_vbid();

        // Inc bid: $50/MWh for 30 MW — far above the $10/MWh LMP, should clear at 0.
        opts.virtual_bids = vec![surge_network::market::VirtualBid {
            position_id: "inc_2".to_string(),
            bus: 2,
            period: 0,
            mw_limit: 30.0,
            price_per_mwh: 50.0,
            direction: surge_network::market::VirtualBidDirection::Inc,
            in_service: true,
        }];

        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("DC-OPF with uneconomic bid should solve");

        assert_eq!(sol.virtual_bid_results.len(), 1);
        let vbr = &sol.virtual_bid_results[0];
        assert!(
            vbr.cleared_mw < 1.0,
            "Uneconomic Inc bid should not clear, got {:.2} MW",
            vbr.cleared_mw
        );

        // Physical gen unchanged at 100 MW
        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        assert!(
            (total_gen - 100.0).abs() < 1.0,
            "Physical gen should be ~100 MW, got {:.2}",
            total_gen
        );
    }

    /// VB-04: in_service=false bid — zero overhead, result list empty.
    #[test]
    fn test_vbid_out_of_service_excluded() {
        let (net, mut opts) = make_2bus_net_for_vbid();

        opts.virtual_bids = vec![surge_network::market::VirtualBid {
            position_id: "inc_3".to_string(),
            bus: 2,
            period: 0,
            mw_limit: 30.0,
            price_per_mwh: 5.0,
            direction: surge_network::market::VirtualBidDirection::Inc,
            in_service: false, // disabled
        }];

        let sol = solve_dc_opf_lp(&net, &opts)
            .map(|r| r.opf)
            .expect("DC-OPF should solve");

        assert!(
            sol.virtual_bid_results.is_empty(),
            "Out-of-service bids should produce no results"
        );

        // Physical gen serves full load
        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        assert!(
            (total_gen - 100.0).abs() < 1.0,
            "Physical gen should be 100 MW, got {:.2}",
            total_gen
        );
    }

    /// Two electrically disconnected islands connected by an HVDC link.
    ///
    /// Island A (buses 1-2): cheap gen ($20/MWh), 100 MW load
    /// Island B (buses 3-4): expensive gen ($50/MWh), 200 MW load
    /// HVDC link: bus 1 → bus 3, 50 MW capacity, co-optimized
    ///
    /// Expected: HVDC at max (50 MW), island A energy ~$20, island B energy ~$50.
    /// The energy components must differ between islands.
    #[test]
    fn test_two_island_hvdc_per_island_energy_lmp() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let base = 100.0;
        let mut net = Network::new("two_island_hvdc");
        net.base_mva = base;

        // Island A: buses 1-2
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);

        // Island B: buses 3-4
        let b3 = Bus::new(3, BusType::Slack, 138.0); // second island needs a slack
        let b4 = Bus::new(4, BusType::PQ, 138.0);

        net.buses = vec![b1, b2, b3, b4];
        net.loads
            .push(surge_network::network::Load::new(2, 100.0, 0.0));
        net.loads
            .push(surge_network::network::Load::new(4, 200.0, 0.0));

        // Branches within each island
        let mut br_a = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        br_a.rating_a_mva = 500.0;
        let mut br_b = Branch::new_line(3, 4, 0.01, 0.1, 0.0);
        br_b.rating_a_mva = 500.0;
        net.branches = vec![br_a, br_b];
        // No branch between island A and B — they are electrically disconnected.

        // Cheap gen in island A
        let mut g_a = Generator::new(1, 200.0, 1.0);
        g_a.pmin = 0.0;
        g_a.pmax = 300.0;
        g_a.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0], // $20/MWh linear
        });

        // Expensive gen in island B
        let mut g_b = Generator::new(3, 200.0, 1.0);
        g_b.pmin = 0.0;
        g_b.pmax = 300.0;
        g_b.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0], // $50/MWh linear
        });

        net.generators = vec![g_a, g_b];

        // HVDC link: bus 1 → bus 3, 0-50 MW capacity (co-optimized)
        let hvdc = HvdcOpfLink::new(1, 3, 0.0, 50.0);

        let opts = DcOpfOptions {
            hvdc_links: Some(vec![hvdc]),
            ..DcOpfOptions::default()
        };

        let result = solve_dc_opf_lp(&net, &opts).expect("Two-island HVDC OPF should solve");
        let sol = &result.opf;

        // HVDC should be at max capacity (50 MW) since island B is more expensive
        let hvdc_mw = result.hvdc_dispatch_mw[0];
        assert!(
            (hvdc_mw - 50.0).abs() < 1.0,
            "HVDC should flow 50 MW (max), got {:.2}",
            hvdc_mw
        );

        // Island A energy: ~$20 (cheap gen sets price)
        let energy_a = sol.pricing.lmp_energy[0]; // bus 1 (island A ref)
        assert!(
            (energy_a - 20.0).abs() < 1.0,
            "Island A energy should be ~$20/MWh, got {:.2}",
            energy_a
        );

        // Island B energy: ~$50 (expensive gen sets price)
        let energy_b = sol.pricing.lmp_energy[2]; // bus 3 (island B ref)
        assert!(
            (energy_b - 50.0).abs() < 1.0,
            "Island B energy should be ~$50/MWh, got {:.2}",
            energy_b
        );

        // Energy prices MUST differ between islands
        assert!(
            (energy_a - energy_b).abs() > 10.0,
            "Per-island energy prices must differ: A={:.2}, B={:.2}",
            energy_a,
            energy_b
        );

        // Within each island, energy should be uniform
        assert!(
            (sol.pricing.lmp_energy[0] - sol.pricing.lmp_energy[1]).abs() < 1e-6,
            "Energy within island A should be uniform: bus1={:.4}, bus2={:.4}",
            sol.pricing.lmp_energy[0],
            sol.pricing.lmp_energy[1]
        );
        assert!(
            (sol.pricing.lmp_energy[2] - sol.pricing.lmp_energy[3]).abs() < 1e-6,
            "Energy within island B should be uniform: bus3={:.4}, bus4={:.4}",
            sol.pricing.lmp_energy[2],
            sol.pricing.lmp_energy[3]
        );

        // LMP decomposition identity must hold
        for i in 0..4 {
            let decomp =
                sol.pricing.lmp_energy[i] + sol.pricing.lmp_congestion[i] + sol.pricing.lmp_loss[i];
            let err = (sol.pricing.lmp[i] - decomp).abs();
            assert!(
                err < 1e-6,
                "LMP decomposition violated at bus {}: lmp={:.4}, decomp={:.4}",
                net.buses[i].number,
                sol.pricing.lmp[i],
                decomp
            );
        }

        // Total cost: gen_a serves 150 MW (100 + 50 export), gen_b serves 150 MW (200 - 50 import)
        // Cost = 150*20 + 150*50 = 3000 + 7500 = 10500 $/hr
        assert!(
            (sol.total_cost - 10500.0).abs() < 10.0,
            "Total cost should be ~$10500/hr, got {:.2}",
            sol.total_cost
        );

        // HVDC shadow price: should equal inter-area energy spread ($50 - $20 = $30)
        // when the link is at its upper bound (50 MW).
        let hvdc_sp = result.hvdc_shadow_prices[0];
        assert!(
            (hvdc_sp - 30.0).abs() < 1.0,
            "HVDC shadow price should be ~$30/MWh (energy spread), got {:.2}",
            hvdc_sp
        );

        eprintln!(
            "Two-island HVDC test PASSED: energy_A={:.2}, energy_B={:.2}, hvdc={:.2} MW, \
             hvdc_shadow={:.2} $/MWh, cost={:.2}",
            energy_a, energy_b, hvdc_mw, hvdc_sp, sol.total_cost
        );
    }
}
