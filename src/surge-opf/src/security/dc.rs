#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DC-SCOPF solver — preventive and corrective modes.
//!
//! Preventive: iterative cutting-plane (LODF-based θ-space cuts).
//! Corrective: extensive-form LP with per-contingency redispatch blocks.
//!
//! Both modes share the sparse B-theta formulation with variables `[θ | Pg]`.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use surge_network::Network;
use surge_network::market::CostCurve;
use surge_network::network::generate_n1_branch_contingencies;
use surge_solution::{
    OpfBranchResults, OpfDeviceDispatch, OpfGeneratorResults, OpfPricing, OpfSolution, PfSolution,
    SolveStatus,
};
use tracing::{debug, info, warn};

use super::dc_contingencies::prepare_preventive_contingencies;
use super::dc_model::build_preventive_base_model;
use super::dc_support::{
    ContingencyData, CorrectiveCtgBlock, CutInfo, CutType, ViolationInfo, get_ptdf,
};
use super::types::*;
use crate::backends::{LpOptions, SparseProblem, try_default_lp_solver};
use crate::common::context::OpfNetworkContext;
use crate::dc::opf::DcOpfError;
use surge_sparse::Triplet;

use crate::dc::opf_lp::triplets_to_csc;

// All public types (ScopfOptions, ScopfResult, ScopfError, etc.) are in scopf_types.rs.

fn branch_loading_pct_from_flows(network: &Network, branch_p_from_mw: &[f64]) -> Vec<f64> {
    network
        .branches
        .iter()
        .zip(branch_p_from_mw.iter())
        .map(|(branch, &pf_mw)| {
            if branch.rating_a_mva <= 0.0 {
                f64::NAN
            } else {
                pf_mw.abs() / branch.rating_a_mva * 100.0
            }
        })
        .collect()
}

/// Solve SCOPF using sparse B-theta formulation with HiGHS.
///
/// Variables: x = [θ (n_bus) | Pg (n_gen)]
///
/// Row layout (grows with cuts):
///   Rows 0..n_flow                      — Base branch flow: Bf*θ  (double-bounded)
///   Rows n_flow..n_flow+n_ang           — Angle difference constraints
///   Rows n_flow+n_ang..+n_ifg           — Interface/flowgate constraints
///   Rows ..+n_bus                        — Power balance: B_bus*θ - A_gen*Pg = -Pd
///   Rows ..+n_pwl_rows                  — PWL epiograph constraints
///   Rows n_base_rows..                  — Contingency cuts (branch/gen/flowgate)
pub(crate) fn solve_dc_preventive_with_context(
    network: &Network,
    options: &ScopfOptions,
    context: &ScopfRunContext,
) -> Result<ScopfResult, DcOpfError> {
    // MUMPS (Ipopt's default linear solver) is NOT thread-safe.  Hold the
    // crate-level mutex so concurrent test threads don't SIGSEGV.
    #[cfg(test)]
    let _ipopt_guard = crate::test_util::IPOPT_MUTEX
        .lock()
        .expect("IPOPT_MUTEX should not be poisoned");

    let start = Instant::now();
    let ctx = OpfNetworkContext::for_dc(network)?;
    let n_bus = ctx.n_bus;
    let n_br = ctx.n_branches;

    info!(
        buses = n_bus,
        branches = n_br,
        max_iterations = options.max_iterations,
        max_cuts_per_iter = options.max_cuts_per_iteration,
        "starting SCOPF"
    );
    let bus_map = &ctx.bus_map;
    let base = ctx.base_mva;
    let island_refs = ctx.island_refs.clone();
    let gen_indices = &ctx.gen_indices;
    let n_gen = gen_indices.len();
    let total_load_mw = ctx.total_load_mw;
    let bus_pd_mw = network.bus_load_p_mw();

    if ctx.total_capacity_mw < ctx.total_load_mw {
        return Err(DcOpfError::InsufficientCapacity {
            load_mw: ctx.total_load_mw,
            capacity_mw: ctx.total_capacity_mw,
        });
    }

    let theta_offset = 0;
    let pg_offset = n_bus;
    let thermal_penalty_per_pu = options.penalty_config.thermal.marginal_cost_at(0.0) * base;
    let base_model = build_preventive_base_model(network, options, &ctx)?;
    let ctg_fg_indices = base_model.active_contingency_flowgate_indices.clone();
    let super::dc_model::PreventiveBaseModel {
        constrained_branches,
        active_interface_indices: active_iface_indices,
        active_base_flowgate_indices: active_fg_indices,
        active_contingency_flowgate_indices: _,
        gen_bus_idx,
        mut hvdc_injections,
        hvdc_var_to_inj_idx,
        ptdf,
        n_flow,
        n_ang,
        n_ifg,
        hvdc_offset,
        n_base_rows,
        n_var_base,
        col_cost,
        c0_total,
        hessian,
        col_lower,
        col_upper,
        mut base_triplets,
        mut base_row_lower,
        mut base_row_upper,
        gen_balance_triplet_indices,
        balance_row_offset,
        pbusinj: model_pbusinj,
    } = base_model;

    if !hvdc_injections.is_empty() {
        debug!(
            n_hvdc_links = hvdc_injections.len(),
            "SCOPF: pre-computed HVDC injections for contingency analysis"
        );
    }

    let prepared_contingencies = prepare_preventive_contingencies(
        network,
        options,
        context,
        super::dc_contingencies::PreventiveContingencyInputs {
            bus_map,
            gen_indices,
            gen_bus_idx: &gen_bus_idx,
            ptdf: &ptdf,
            n_bus,
            n_branches: n_br,
            n_base_rows,
            n_var_base,
            theta_offset,
            thermal_penalty_per_pu,
            base,
        },
    );
    let screening_threshold_fraction = prepared_contingencies
        .initial_cuts
        .screening_threshold_fraction;
    let pairs_evaluated = prepared_contingencies.initial_cuts.pairs_evaluated;
    let pre_screened_count = prepared_contingencies.initial_cuts.pre_screened_count;
    let super::dc_contingencies::PreparedPreventiveContingencies {
        contingencies,
        monitored_branches,
        ctg_data,
        gen_ctg_data,
        n2_ctg_data,
        mixed_gen_shifts,
        initial_cuts,
    } = prepared_contingencies;
    let super::dc_contingencies::PreventiveInitialCuts {
        mut cut_triplets,
        mut cut_row_lower,
        mut cut_row_upper,
        mut cut_metadata,
        mut constrained_pairs,
        mut cut_slack_cost,
        mut cut_slack_lower,
        mut cut_slack_upper,
        pre_screened_count: _,
        pairs_evaluated: _,
        screening_threshold_fraction: _,
    } = initial_cuts;

    let mut scopf_iter = 0u32;
    let use_loss_factors = options.dc_opf.use_loss_factors;
    let max_loss_iter = options.dc_opf.max_loss_iter;
    let loss_tol = options.dc_opf.loss_tol;
    let mut prev_dloss = vec![0.0f64; n_bus];
    let mut loss_iter_count = 0usize;

    // Resolve LP solver once before the loop (shared across iterations).
    let lp_solver = context
        .runtime
        .lp_solver
        .clone()
        .map_or_else(|| try_default_lp_solver(), Ok)
        .map_err(DcOpfError::SolverError)?;

    loop {
        let n_cuts = cut_metadata.len();
        let n_row = n_base_rows + n_cuts;
        let n_var = n_var_base + 2 * n_cuts; // base vars + 2 slack cols per cut

        // Extend col_cost / col_lower / col_upper with cut slack columns
        let mut full_col_cost = col_cost.clone();
        let mut full_col_lower = col_lower.clone();
        let mut full_col_upper = col_upper.clone();
        full_col_cost.extend_from_slice(&cut_slack_cost);
        full_col_lower.extend_from_slice(&cut_slack_lower);
        full_col_upper.extend_from_slice(&cut_slack_upper);

        // Combine base + cut triplets → CSC
        let mut all_triplets = Vec::with_capacity(base_triplets.len() + cut_triplets.len());
        for t in &base_triplets {
            all_triplets.push(Triplet {
                row: t.row,
                col: t.col,
                val: t.val,
            });
        }
        for t in &cut_triplets {
            all_triplets.push(Triplet {
                row: t.row,
                col: t.col,
                val: t.val,
            });
        }
        let (a_start, a_index, a_value) = triplets_to_csc(&all_triplets, n_row, n_var);

        // Row bounds: base + cuts
        let mut row_lower: Vec<f64> = Vec::with_capacity(n_row);
        let mut row_upper: Vec<f64> = Vec::with_capacity(n_row);
        row_lower.extend_from_slice(&base_row_lower);
        row_upper.extend_from_slice(&base_row_upper);
        row_lower.extend_from_slice(&cut_row_lower);
        row_upper.extend_from_slice(&cut_row_upper);

        // Extend Hessian for cut slack columns (zero diagonal — no quadratic cost)
        // q_start_vec already covers [θ | Pg | base thermal slacks | e_g] via n_var_base+1 entries.
        let (q_start_ext, q_index_ext, q_value_ext) =
            if let Some((q_start_vec, q_index_vec, q_value_vec)) = &hessian {
                let mut qs = q_start_vec.clone();
                // Remove trailing sentinel, add empty columns for cut slacks, re-add sentinel
                qs.pop();
                let nnz = q_index_vec.len() as i32;
                for _ in 0..(2 * n_cuts) {
                    qs.push(nnz);
                }
                qs.push(nnz);
                (
                    Some(qs),
                    Some(q_index_vec.clone()),
                    Some(q_value_vec.clone()),
                )
            } else {
                (None, None, None)
            };

        // Solve
        let prob = SparseProblem {
            n_col: n_var,
            n_row,
            col_cost: full_col_cost,
            col_lower: full_col_lower,
            col_upper: full_col_upper,
            row_lower,
            row_upper,
            a_start,
            a_index,
            a_value,
            q_start: q_start_ext,
            q_index: q_index_ext,
            q_value: q_value_ext,
            integrality: None,
        };
        let lp_opts = LpOptions {
            tolerance: options.dc_opf.tolerance,
            ..Default::default()
        };
        let sol = lp_solver
            .solve(&prob, &lp_opts)
            .map_err(DcOpfError::SolverError)?;

        match sol.status {
            crate::backends::LpSolveStatus::Optimal => {}
            crate::backends::LpSolveStatus::SubOptimal => {
                return Err(DcOpfError::SubOptimalSolution);
            }
            _ => {
                return Err(DcOpfError::NotConverged {
                    iterations: sol.iterations,
                });
            }
        }

        // Extract θ and Pg
        let theta = &sol.x[theta_offset..theta_offset + n_bus];
        let pg_pu = &sol.x[pg_offset..pg_offset + n_gen];

        // Update HVDC injections from the LP solution for variable links.
        // Contingency evaluation uses these to compute post-trip flow shifts.
        for (k, &inj_idx) in hvdc_var_to_inj_idx.iter().enumerate() {
            let p_dc_pu = sol.x[hvdc_offset + k];
            hvdc_injections[inj_idx].2 = -p_dc_pu; // rectifier draws
            hvdc_injections[inj_idx].3 = p_dc_pu; // inverter injects
        }

        // Compute base branch flows from θ: flow[l] = b_dc_l * (θ_from - θ_to - shift_rad_l)
        let mut base_flow = vec![0.0; n_br];
        for (l, flow) in base_flow.iter_mut().enumerate() {
            let br = &network.branches[l];
            if !br.in_service || br.x.abs() < 1e-20 {
                continue;
            }
            let from = bus_map[&br.from_bus];
            let to = bus_map[&br.to_bus];
            let shift_rad = br.phase_shift_rad;
            *flow = br.b_dc() * (theta[from] - theta[to] - shift_rad);
        }

        // --- Nomogram tightening ---
        // Evaluate operating nomograms and tighten flowgate bounds if needed.
        if options.enforce_flowgates
            && !network.nomograms.is_empty()
            && !active_fg_indices.is_empty()
        {
            // Compute MW flow on each active flowgate from LP solution
            let fg_name_to_ri: HashMap<&str, usize> = active_fg_indices
                .iter()
                .enumerate()
                .map(|(ri, &fgi)| (network.flowgates[fgi].name.as_str(), ri))
                .collect();

            let flowgate_flows_mw: Vec<f64> = active_fg_indices
                .iter()
                .map(|&fgi| {
                    let fg = &network.flowgates[fgi];
                    let mut flow_pu = 0.0;
                    for member in &fg.monitored {
                        let coeff = member.coefficient;
                        let branch_ref = &member.branch;
                        if let (Some(&fi), Some(&ti)) = (
                            bus_map.get(&branch_ref.from_bus),
                            bus_map.get(&branch_ref.to_bus),
                        ) && let Some(br) = network.branches.iter().find(|br| {
                            br.in_service && branch_ref.matches_branch(br) && br.x.abs() > 1e-20
                        }) {
                            flow_pu += coeff * br.b_dc() * (theta[fi] - theta[ti]);
                        }
                    }
                    flow_pu * base
                })
                .collect();

            let flow_by_name: HashMap<&str, f64> = active_fg_indices
                .iter()
                .enumerate()
                .map(|(ri, &fgi)| (network.flowgates[fgi].name.as_str(), flowgate_flows_mw[ri]))
                .collect();

            for nom in network.nomograms.iter().filter(|n| n.in_service) {
                let Some(&index_flow) = flow_by_name.get(nom.index_flowgate.as_str()) else {
                    continue;
                };
                let Some(&ri) = fg_name_to_ri.get(nom.constrained_flowgate.as_str()) else {
                    continue;
                };
                let new_limit = nom.evaluate(index_flow);
                let fg_ref = &network.flowgates[active_fg_indices[ri]];
                let current_limit = fg_ref.limit_mw;
                if new_limit < current_limit - 1e-3 {
                    // Tighten the base row bounds — forward limit tightened by nomogram,
                    // reverse limit uses the flowgate's own reverse (or forward) limit.
                    let row_offset = n_flow + n_ang + active_iface_indices.len() + ri;
                    let rev = fg_ref.effective_reverse_or_forward(0).min(new_limit);
                    base_row_lower[row_offset] = -rev / base;
                    base_row_upper[row_offset] = new_limit / base;
                    debug!(
                        fg = network.flowgates[active_fg_indices[ri]].name,
                        old_limit = current_limit,
                        new_limit,
                        "SCOPF nomogram tightened flowgate limit"
                    );
                }
            }
        }

        // Check contingencies for violations
        let mut violations: Vec<ViolationInfo> = Vec::new();

        for cd in &ctg_data {
            let flow_k = base_flow[cd.outaged_br];

            for &l in &monitored_branches {
                if l == cd.outaged_br {
                    continue;
                }
                if constrained_pairs.contains(&(cd.ctg_idx, l)) {
                    continue;
                }

                let ptdf_diff_l =
                    get_ptdf(&ptdf, l, cd.from_bus_idx) - get_ptdf(&ptdf, l, cd.to_bus_idx);
                let lodf_lk = ptdf_diff_l / cd.denom;

                if !lodf_lk.is_finite() {
                    continue;
                }

                let post_flow = base_flow[l] + lodf_lk * flow_k;
                let f_max = options.contingency_rating.of(&network.branches[l]) / base;
                let excess = post_flow.abs() - f_max;

                if excess > options.violation_tolerance_pu {
                    violations.push(ViolationInfo {
                        contingency_idx: cd.ctg_idx,
                        monitored_branch_idx: l,
                        severity: excess,
                        lodf_lk,
                    });
                }
            }
        }

        // --- HVDC contingency violation check ---
        // For each contingency with hvdc_converter_indices or hvdc_cable_indices,
        // compute the post-contingency flow change at monitored branches via PTDF.
        // Tripping an HVDC link removes its P injection at both converter buses.
        for (ci, ctg) in contingencies.iter().enumerate() {
            // Skip branch-only contingencies (already handled above).
            if ctg.hvdc_converter_indices.is_empty() && ctg.hvdc_cable_indices.is_empty() {
                continue;
            }

            // Combine converter and cable indices into one set of tripped links.
            let tripped: Vec<usize> = ctg
                .hvdc_converter_indices
                .iter()
                .chain(ctg.hvdc_cable_indices.iter())
                .copied()
                .collect();

            for &l in &monitored_branches {
                if constrained_pairs.contains(&(ci, l)) {
                    continue;
                }

                // Compute total flow shift on branch l from all tripped HVDC links.
                let mut delta_flow = 0.0_f64;
                for &hvdc_idx in &tripped {
                    if hvdc_idx >= hvdc_injections.len() {
                        continue;
                    }
                    let (from_idx, to_idx, p_from_pu, p_to_pu) = hvdc_injections[hvdc_idx];
                    // Removing the injection means ΔP = -injection.
                    // Post-contingency flow shift = PTDF[l, from] * (-p_from) + PTDF[l, to] * (-p_to)
                    delta_flow += get_ptdf(&ptdf, l, from_idx) * (-p_from_pu)
                        + get_ptdf(&ptdf, l, to_idx) * (-p_to_pu);
                }

                let post_flow = base_flow[l] + delta_flow;
                let f_max = options.contingency_rating.of(&network.branches[l]) / base;
                let excess = post_flow.abs() - f_max;

                if excess > options.violation_tolerance_pu {
                    // Use lodf_lk = 0.0 for HVDC contingencies (no branch LODF).
                    // The cut for HVDC contingencies is simply a tightened base flow bound.
                    violations.push(ViolationInfo {
                        contingency_idx: ci,
                        monitored_branch_idx: l,
                        severity: excess,
                        lodf_lk: 0.0,
                    });
                }
            }
        }

        // --- Generator contingency violation check ---
        // For each single-generator contingency, compute post-contingency flow shift
        // via PTDF: ΔF_l = PTDF[l, bus_g] × (-Pg_g).
        for gcd in &gen_ctg_data {
            let pg_tripped_pu = sol.x[pg_offset + gcd.gen_local];

            for &l in &monitored_branches {
                if constrained_pairs.contains(&(gcd.ctg_idx, l)) {
                    continue;
                }

                let ptdf_l_g = get_ptdf(&ptdf, l, gcd.bus_idx);
                let post_flow = base_flow[l] + ptdf_l_g * (-pg_tripped_pu);
                let f_max = options.contingency_rating.of(&network.branches[l]) / base;
                let excess = post_flow.abs() - f_max;

                if excess > options.violation_tolerance_pu {
                    violations.push(ViolationInfo {
                        contingency_idx: gcd.ctg_idx,
                        monitored_branch_idx: l,
                        severity: excess,
                        lodf_lk: 0.0, // not LODF-based — gen trip uses PTDF directly
                    });
                }
            }
        }

        // --- N-2 multi-branch contingency violation check ---
        // For each 2-branch contingency, compute post-contingency flow using
        // compound LODF from Woodbury rank-2 update.
        for n2d in &n2_ctg_data {
            let flow_k1 = base_flow[n2d.k1];
            let flow_k2 = base_flow[n2d.k2];

            // Also add gen-trip PTDF shift for mixed contingencies
            let mixed_delta = |l: usize| -> f64 {
                if let Some(gen_locals) = mixed_gen_shifts.get(&n2d.ctg_idx) {
                    let mut delta = 0.0;
                    for &gl in gen_locals {
                        let gi = gen_indices[gl];
                        let bus_idx = bus_map[&network.generators[gi].bus];
                        let pg_pu = sol.x[pg_offset + gl];
                        delta += get_ptdf(&ptdf, l, bus_idx) * (-pg_pu);
                    }
                    delta
                } else {
                    0.0
                }
            };

            for &l in &monitored_branches {
                if l == n2d.k1 || l == n2d.k2 {
                    continue;
                }
                if constrained_pairs.contains(&(n2d.ctg_idx, l)) {
                    continue;
                }

                let (d_m1, d_m2) = n2d.compound_lodf(&ptdf, l);
                if !d_m1.is_finite() || !d_m2.is_finite() {
                    continue;
                }

                let post_flow = base_flow[l] + d_m1 * flow_k1 + d_m2 * flow_k2 + mixed_delta(l);
                let f_max = options.contingency_rating.of(&network.branches[l]) / base;
                let excess = post_flow.abs() - f_max;

                if excess > options.violation_tolerance_pu {
                    violations.push(ViolationInfo {
                        contingency_idx: n2d.ctg_idx,
                        monitored_branch_idx: l,
                        severity: excess,
                        lodf_lk: d_m1, // store first LODF component for reference
                    });
                }
            }
        }

        // --- Mixed branch+gen contingency violation check ---
        // For single-branch contingencies that also trip generators,
        // add the gen-trip PTDF shift to the LODF-based flow.
        for cd in &ctg_data {
            if let Some(gen_locals) = mixed_gen_shifts.get(&cd.ctg_idx) {
                let flow_k = base_flow[cd.outaged_br];

                for &l in &monitored_branches {
                    if l == cd.outaged_br {
                        continue;
                    }
                    if constrained_pairs.contains(&(cd.ctg_idx, l)) {
                        continue;
                    }

                    let ptdf_diff_l =
                        get_ptdf(&ptdf, l, cd.from_bus_idx) - get_ptdf(&ptdf, l, cd.to_bus_idx);
                    let lodf_lk = ptdf_diff_l / cd.denom;
                    if !lodf_lk.is_finite() {
                        continue;
                    }

                    // Base branch-outage flow + gen-trip PTDF shift
                    let mut post_flow = base_flow[l] + lodf_lk * flow_k;
                    for &gl in gen_locals {
                        let gi = gen_indices[gl];
                        let bus_idx = bus_map[&network.generators[gi].bus];
                        let pg_pu = sol.x[pg_offset + gl];
                        post_flow += get_ptdf(&ptdf, l, bus_idx) * (-pg_pu);
                    }

                    let f_max = options.contingency_rating.of(&network.branches[l]) / base;
                    let excess = post_flow.abs() - f_max;

                    if excess > options.violation_tolerance_pu {
                        violations.push(ViolationInfo {
                            contingency_idx: cd.ctg_idx,
                            monitored_branch_idx: l,
                            severity: excess,
                            lodf_lk,
                        });
                    }
                }
            }
        }

        // --- Contingency flowgate OTDF-based violation check ---
        // For each contingency flowgate (contingency_branch = Some), compute
        // post-contingency flowgate flow and check against limit.
        if !ctg_fg_indices.is_empty() {
            for &fg_idx in &ctg_fg_indices {
                let fg = &network.flowgates[fg_idx];
                let contingency_branch = fg
                    .contingency_branch
                    .as_ref()
                    .expect("ctg_fg_indices filtered to contingency_branch.is_some()");

                // Find the outaged branch index
                let outaged_br = match network
                    .branches
                    .iter()
                    .position(|br| br.in_service && contingency_branch.matches_branch(br))
                {
                    Some(idx) => idx,
                    None => continue,
                };

                // Find the matching ContingencyData for this outage
                let cd = match ctg_data.iter().find(|c| c.outaged_br == outaged_br) {
                    Some(cd) => cd,
                    None => continue, // bridge line or not a single-branch contingency
                };

                // Compute post-contingency flowgate flow
                let flow_k = base_flow[outaged_br];
                let mut fg_flow = 0.0_f64;
                for member in &fg.monitored {
                    let coeff = member.coefficient;
                    let branch_ref = &member.branch;
                    if let Some(br_idx) = network
                        .branches
                        .iter()
                        .position(|br| br.in_service && branch_ref.matches_branch(br))
                    {
                        // Post-contingency flow = base flow + LODF * flow_k
                        let ptdf_diff = get_ptdf(&ptdf, br_idx, cd.from_bus_idx)
                            - get_ptdf(&ptdf, br_idx, cd.to_bus_idx);
                        let lodf = ptdf_diff / cd.denom;
                        if lodf.is_finite() {
                            fg_flow += coeff * (base_flow[br_idx] + lodf * flow_k);
                        }
                    }
                }

                let fg_fwd = fg.limit_mw / base;
                let fg_rev = fg.effective_reverse_or_forward(0) / base;
                let excess = if fg_flow > 0.0 {
                    fg_flow - fg_fwd
                } else {
                    -fg_flow - fg_rev
                };
                if excess > options.violation_tolerance_pu {
                    // Use the first monitored branch for the violation record
                    if let Some(br_idx) = fg.monitored.first().and_then(|member| {
                        network
                            .branches
                            .iter()
                            .position(|br| br.in_service && member.branch.matches_branch(br))
                    }) {
                        violations.push(ViolationInfo {
                            contingency_idx: cd.ctg_idx,
                            monitored_branch_idx: br_idx,
                            severity: excess,
                            lodf_lk: 0.0, // flowgate cut has its own structure
                        });
                    }
                }
            }
        }

        // --- Interface N-1 violation check ---
        // For each single-branch contingency, compute post-contingency interface
        // flow (sum of branch flows after LODF redistribution) and check limits.
        if options.enforce_flowgates && !active_iface_indices.is_empty() {
            for cd in &ctg_data {
                let flow_k = base_flow[cd.outaged_br];

                for &ii in &active_iface_indices {
                    let iface = &network.interfaces[ii];
                    if !iface.in_service {
                        continue;
                    }

                    // Compute post-contingency interface flow
                    let mut iface_flow_pu = 0.0_f64;
                    for member in &iface.members {
                        let coeff = member.coefficient;
                        let branch_ref = &member.branch;
                        if let Some(br_idx) = network.branches.iter().position(|br| {
                            br.in_service && branch_ref.matches_branch(br) && br.x.abs() > 1e-20
                        }) {
                            if br_idx == cd.outaged_br {
                                continue; // outaged branch has zero flow
                            }
                            let ptdf_diff = get_ptdf(&ptdf, br_idx, cd.from_bus_idx)
                                - get_ptdf(&ptdf, br_idx, cd.to_bus_idx);
                            let lodf = ptdf_diff / cd.denom;
                            let post_br_flow = base_flow[br_idx] + lodf * flow_k;
                            iface_flow_pu += coeff * post_br_flow;
                        }
                    }

                    let fwd_limit = iface.limit_forward_mw / base;
                    let rev_limit = iface.limit_reverse_mw / base;

                    let fwd_excess = iface_flow_pu - fwd_limit;
                    let rev_excess = -iface_flow_pu - rev_limit;
                    let excess = fwd_excess.max(rev_excess);

                    if excess > options.violation_tolerance_pu {
                        // Use first interface branch as the monitored branch for the cut
                        if let Some(br_idx) = iface.members.first().and_then(|member| {
                            network
                                .branches
                                .iter()
                                .position(|br| br.in_service && member.branch.matches_branch(br))
                        }) {
                            violations.push(ViolationInfo {
                                contingency_idx: cd.ctg_idx,
                                monitored_branch_idx: br_idx,
                                severity: excess,
                                lodf_lk: 0.0,
                            });
                        }
                    }
                }
            }
        }

        if violations.is_empty() {
            // Cutting-plane converged. Check if loss iteration is needed.
            if use_loss_factors && loss_iter_count < max_loss_iter {
                use crate::dc::loss_factors::{
                    compute_dc_loss_sensitivities, compute_total_dc_losses,
                };
                let theta = &sol.x[theta_offset..theta_offset + n_bus];
                let dloss_dp = compute_dc_loss_sensitivities(network, theta, bus_map, &ptdf);

                if loss_iter_count > 0 {
                    let max_change = dloss_dp
                        .iter()
                        .zip(prev_dloss.iter())
                        .map(|(a, b)| (a - b).abs())
                        .fold(0.0f64, f64::max);
                    if max_change < loss_tol {
                        // Loss factors converged — proceed to result.
                    } else {
                        prev_dloss.copy_from_slice(&dloss_dp);

                        // Update gen coefficients in base triplets.
                        for (j, &ti) in gen_balance_triplet_indices.iter().enumerate() {
                            let bus_idx = gen_bus_idx[j];
                            let pf_inv = (1.0 - dloss_dp[bus_idx]).clamp(0.5, 1.5);
                            base_triplets[ti].val = -pf_inv;
                        }

                        // Update power balance RHS with loss allocation.
                        let total_loss_pu = compute_total_dc_losses(network, theta, bus_map);
                        let total_load_pu: f64 = bus_pd_mw
                            .iter()
                            .map(|&pd| pd / base)
                            .sum::<f64>()
                            .abs()
                            .max(1e-10);
                        for i in 0..n_bus {
                            let load_share = (bus_pd_mw[i] / base).abs() / total_load_pu;
                            let loss_at_bus = total_loss_pu * load_share;
                            let pd_pu = bus_pd_mw[i] / base;
                            let gs_pu = network.buses[i].shunt_conductance_mw / base;
                            let rhs = -pd_pu - gs_pu - model_pbusinj[i] - loss_at_bus;
                            base_row_lower[balance_row_offset + i] = rhs;
                            base_row_upper[balance_row_offset + i] = rhs;
                        }

                        loss_iter_count += 1;
                        info!(
                            loss_iter = loss_iter_count,
                            max_change, "loss factor iteration — re-solving SCOPF"
                        );
                        continue; // re-enter cutting-plane loop
                    }
                } else {
                    prev_dloss.copy_from_slice(&dloss_dp);

                    // First loss iteration: update coefficients.
                    for (j, &ti) in gen_balance_triplet_indices.iter().enumerate() {
                        let bus_idx = gen_bus_idx[j];
                        let pf_inv = (1.0 - dloss_dp[bus_idx]).clamp(0.5, 1.5);
                        base_triplets[ti].val = -pf_inv;
                    }

                    let total_loss_pu = compute_total_dc_losses(network, theta, bus_map);
                    let total_load_pu: f64 = bus_pd_mw
                        .iter()
                        .map(|&pd| pd / base)
                        .sum::<f64>()
                        .abs()
                        .max(1e-10);
                    for i in 0..n_bus {
                        let load_share = (bus_pd_mw[i] / base).abs() / total_load_pu;
                        let loss_at_bus = total_loss_pu * load_share;
                        let pd_pu = bus_pd_mw[i] / base;
                        let gs_pu = network.buses[i].shunt_conductance_mw / base;
                        let rhs = -pd_pu - gs_pu - model_pbusinj[i] - loss_at_bus;
                        base_row_lower[balance_row_offset + i] = rhs;
                        base_row_upper[balance_row_offset + i] = rhs;
                    }

                    loss_iter_count += 1;
                    info!(
                        loss_iter = loss_iter_count,
                        "loss factor iteration — re-solving SCOPF"
                    );
                    continue; // re-enter cutting-plane loop
                }
            }

            // Fully converged — extract solution
            let n_ctg_cuts = cut_metadata.len();

            info!(
                "SCOPF converged in {} iterations ({} contingency constraints)",
                scopf_iter + 1,
                n_ctg_cuts
            );

            // --- Extract LMPs ---
            // Total LMP from power balance duals (rows n_flow+n_ang..n_flow+n_ang+n_bus)
            // HiGHS row_dual convention: negate for standard Lagrange multiplier
            let lmp: Vec<f64> = (0..n_bus)
                .map(|i| sol.row_dual[n_flow + n_ang + n_ifg + i] / base)
                .collect();

            // Per-island energy decomposition (with loss component when active)
            let (lmp_energy, _, lmp_loss) = if use_loss_factors && loss_iter_count > 0 {
                use crate::dc::loss_factors::compute_dc_loss_sensitivities;
                let dloss_dp = compute_dc_loss_sensitivities(network, theta, bus_map, &ptdf);
                crate::dc::island_lmp::decompose_lmp_with_losses(&lmp, &dloss_dp, &island_refs)
            } else {
                crate::dc::island_lmp::decompose_lmp_lossless(&lmp, &island_refs)
            };

            // Base congestion from branch flow duals (rows 0..n_flow)
            let mut lmp_base_congestion = vec![0.0; n_bus];
            for (ci, &l) in constrained_branches.iter().enumerate() {
                let mu = sol.row_dual[ci]; // standard Lagrange convention
                if mu.abs() < 1e-12 {
                    continue;
                }
                for i in 0..n_bus {
                    lmp_base_congestion[i] += mu * get_ptdf(&ptdf, l, i) / base;
                }
            }

            // Contingency congestion from cut duals
            let mut lmp_ctg_congestion = vec![0.0; n_bus];
            for (cut_idx, cut) in cut_metadata.iter().enumerate() {
                let row_idx = n_base_rows + cut_idx;
                let mu = sol.row_dual[row_idx]; // standard Lagrange convention
                if mu.abs() < 1e-12 {
                    continue;
                }

                let l = cut.monitored_branch_idx;
                match cut.cut_type {
                    CutType::BranchThermal
                        if cut.outaged_branch_indices.len() == 1
                            && cut.outaged_branch_indices[0] < n_br =>
                    {
                        let k = cut.outaged_branch_indices[0];
                        let lodf_lk = cut.lodf_lk;
                        for i in 0..n_bus {
                            let eff_ptdf = get_ptdf(&ptdf, l, i) + lodf_lk * get_ptdf(&ptdf, k, i);
                            lmp_ctg_congestion[i] += mu * eff_ptdf / base;
                        }
                    }
                    CutType::GeneratorTrip => {
                        // Gen-trip cut: sensitivity is PTDF[l, bus] for monitored branch
                        for i in 0..n_bus {
                            lmp_ctg_congestion[i] += mu * get_ptdf(&ptdf, l, i) / base;
                        }
                    }
                    CutType::MultiBranchN2 => {
                        // N-2 cut: effective PTDF = PTDF[l,i] + D1*PTDF[k1,i] + D2*PTDF[k2,i]
                        if let Some(n2d) = n2_ctg_data.iter().find(|n| n.ctg_idx == cut.ctg_idx) {
                            let (d1, d2) = n2d.compound_lodf(&ptdf, l);
                            for i in 0..n_bus {
                                let eff_ptdf = get_ptdf(&ptdf, l, i)
                                    + d1 * get_ptdf(&ptdf, n2d.k1, i)
                                    + d2 * get_ptdf(&ptdf, n2d.k2, i);
                                lmp_ctg_congestion[i] += mu * eff_ptdf / base;
                            }
                        }
                    }
                    _ => {
                        // HVDC or unknown: use monitored branch PTDF only
                        for i in 0..n_bus {
                            lmp_ctg_congestion[i] += mu * get_ptdf(&ptdf, l, i) / base;
                        }
                    }
                }
            }

            // Total congestion = LMP - energy - loss
            let lmp_congestion: Vec<f64> = lmp
                .iter()
                .zip(lmp_energy.iter())
                .zip(lmp_loss.iter())
                .map(|((&l, &e), &lo)| l - e - lo)
                .collect();

            // Branch shadow prices (base-case)
            let mut branch_shadow_prices = vec![0.0; n_br];
            for (ci, &l) in constrained_branches.iter().enumerate() {
                branch_shadow_prices[l] = sol.row_dual[ci] / base;
            }

            // Build PF solution from optimal dispatch
            let gen_p_mw: Vec<f64> = pg_pu.iter().map(|&p| p * base).collect();
            let total_cost = sol.objective + c0_total;

            let va: Vec<f64> = theta.to_vec();

            let mut p_inject = vec![0.0; n_bus];
            for i in 0..n_bus {
                p_inject[i] = -bus_pd_mw[i] / base;
            }
            for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
                p_inject[bus_idx] += pg_pu[j];
            }

            let branch_pf_pu: Vec<f64> = network
                .branches
                .iter()
                .map(|br| {
                    if !br.in_service {
                        return 0.0;
                    }
                    let from_i = bus_map[&br.from_bus];
                    let to_i = bus_map[&br.to_bus];
                    br.b_dc() * (theta[from_i] - theta[to_i] - br.phase_shift_rad)
                })
                .collect();
            let branch_pf_mw: Vec<f64> = branch_pf_pu.iter().map(|&p| p * base).collect();
            let branch_pt_mw: Vec<f64> = branch_pf_mw.iter().map(|&p| -p).collect();
            let branch_loading_pct = branch_loading_pct_from_flows(network, &branch_pf_mw);

            let pf_solution = PfSolution {
                pf_model: surge_solution::PfModel::Dc,
                status: SolveStatus::Converged,
                iterations: 1,
                max_mismatch: 0.0,
                solve_time_secs: 0.0,
                voltage_magnitude_pu: vec![1.0; n_bus],
                voltage_angle_rad: va,
                active_power_injection_pu: p_inject,
                reactive_power_injection_pu: vec![0.0; n_bus],
                branch_p_from_mw: branch_pf_mw,
                branch_p_to_mw: branch_pt_mw,
                branch_q_from_mvar: vec![0.0; n_br],
                branch_q_to_mvar: vec![0.0; n_br],
                bus_numbers: network.buses.iter().map(|b| b.number).collect(),
                island_ids: vec![],
                q_limited_buses: vec![],
                n_q_limit_switches: 0,
                gen_slack_contribution_mw: vec![],
                convergence_history: vec![],
                worst_mismatch_bus: None,
                area_interchange: None,
            };

            let solve_time = start.elapsed().as_secs_f64();

            // Collect binding contingencies
            let mut binding = Vec::new();
            for (cut_idx, cut) in cut_metadata.iter().enumerate() {
                let row_idx = n_base_rows + cut_idx;
                let shadow = sol.row_dual[row_idx] / base;

                if shadow.abs() < 1e-6 {
                    continue;
                }

                let l = cut.monitored_branch_idx;
                let post_flow = match cut.cut_type {
                    CutType::BranchThermal
                        if cut.outaged_branch_indices.len() == 1
                            && cut.outaged_branch_indices[0] < n_br =>
                    {
                        let k = cut.outaged_branch_indices[0];
                        base_flow[l] + cut.lodf_lk * base_flow[k]
                    }
                    CutType::GeneratorTrip => {
                        let pg_tripped = cut
                            .gen_local_idx
                            .map(|gl| sol.x[pg_offset + gl])
                            .unwrap_or(0.0);
                        let bus_g = gen_ctg_data
                            .iter()
                            .find(|g| g.ctg_idx == cut.ctg_idx)
                            .map(|g| g.bus_idx)
                            .unwrap_or(0);
                        base_flow[l] + get_ptdf(&ptdf, l, bus_g) * (-pg_tripped)
                    }
                    CutType::MultiBranchN2 => {
                        if let Some(n2d) = n2_ctg_data.iter().find(|n| n.ctg_idx == cut.ctg_idx) {
                            let (d1, d2) = n2d.compound_lodf(&ptdf, l);
                            base_flow[l] + d1 * base_flow[n2d.k1] + d2 * base_flow[n2d.k2]
                        } else {
                            base_flow[l]
                        }
                    }
                    _ => base_flow[l],
                };
                let f_max = options.contingency_rating.of(&network.branches[l]) / base;
                let loading_pct = if f_max > 0.0 {
                    post_flow.abs() / f_max * 100.0
                } else {
                    0.0
                };

                // Find label from branch or gen contingency data
                let label = ctg_data
                    .iter()
                    .find(|c| c.ctg_idx == cut.ctg_idx)
                    .map(|c| c.label.clone())
                    .or_else(|| {
                        gen_ctg_data
                            .iter()
                            .find(|c| c.ctg_idx == cut.ctg_idx)
                            .map(|c| c.label.clone())
                    })
                    .or_else(|| contingencies.get(cut.ctg_idx).map(|c| c.label.clone()))
                    .unwrap_or_default();

                binding.push(BindingContingency {
                    contingency_label: label,
                    cut_kind: match cut.cut_type {
                        CutType::BranchThermal => ScopfCutKind::BranchThermal,
                        CutType::GeneratorTrip => ScopfCutKind::GeneratorTrip,
                        CutType::MultiBranchN2 => ScopfCutKind::MultiBranchN2,
                    },
                    outaged_branch_indices: cut.outaged_branch_indices.clone(),
                    outaged_generator_indices: cut
                        .gen_local_idx
                        .map(|gen_local| vec![gen_indices[gen_local]])
                        .unwrap_or_default(),
                    monitored_branch_idx: l,
                    loading_pct,
                    shadow_price: shadow,
                });
            }

            info!(
                "SCOPF solved in {:.1} ms ({} generators, {} base constraints, {} contingency cuts, cost={:.2} $/hr)",
                solve_time * 1000.0,
                n_gen,
                constrained_branches.len(),
                n_ctg_cuts,
                total_cost
            );

            let cutting_plane_constraints = cut_metadata.len().saturating_sub(pre_screened_count);
            let screening_stats = ScopfScreeningStats {
                pairs_evaluated,
                pre_screened_constraints: pre_screened_count,
                cutting_plane_constraints,
                threshold_fraction: screening_threshold_fraction,
            };

            let gen_bus_numbers_scopf: Vec<u32> = gen_indices
                .iter()
                .map(|&gi| network.generators[gi].bus)
                .collect();
            let gen_ids_scopf: Vec<String> = gen_indices
                .iter()
                .map(|&gi| network.generators[gi].id.clone())
                .collect();
            let gen_machine_ids_scopf: Vec<String> = gen_indices
                .iter()
                .map(|&gi| {
                    network.generators[gi]
                        .machine_id
                        .clone()
                        .unwrap_or_else(|| "1".to_string())
                })
                .collect();
            let total_generation_mw_scopf: f64 = gen_p_mw.iter().sum();
            let total_losses_mw_scopf = if use_loss_factors && loss_iter_count > 0 {
                use crate::dc::loss_factors::compute_total_dc_losses;
                compute_total_dc_losses(network, theta, bus_map) * base
            } else {
                0.0
            };

            // Extract generator bound duals from LP column duals.
            // HiGHS col_dual: negative at lower bound, positive at upper bound.
            // Divide by base to get $/MWh (matches AC-OPF convention).
            let mu_pg_min_scopf: Vec<f64> = (0..n_gen)
                .map(|j| (-sol.col_dual[pg_offset + j]).max(0.0) / base)
                .collect();
            let mu_pg_max_scopf: Vec<f64> = (0..n_gen)
                .map(|j| sol.col_dual[pg_offset + j].max(0.0) / base)
                .collect();

            return Ok(ScopfResult {
                base_opf: OpfSolution {
                    opf_type: surge_solution::OpfType::DcScopf,
                    base_mva: network.base_mva,
                    power_flow: pf_solution,
                    generators: OpfGeneratorResults {
                        gen_p_mw,
                        gen_q_mvar: vec![],
                        gen_bus_numbers: gen_bus_numbers_scopf,
                        gen_ids: gen_ids_scopf,
                        gen_machine_ids: gen_machine_ids_scopf,
                        shadow_price_pg_min: mu_pg_min_scopf,
                        shadow_price_pg_max: mu_pg_max_scopf,
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
                        shadow_price_angmin: vec![],
                        shadow_price_angmax: vec![],
                        flowgate_shadow_prices: {
                            let mut v = vec![0.0; network.flowgates.len()];
                            for (ri, &fi) in active_fg_indices.iter().enumerate() {
                                let row = n_flow + n_ang + active_iface_indices.len() + ri;
                                v[fi] = sol.row_dual[row] / base;
                            }
                            v
                        },
                        interface_shadow_prices: {
                            let mut v = vec![0.0; network.interfaces.len()];
                            for (ri, &ii) in active_iface_indices.iter().enumerate() {
                                let row = n_flow + n_ang + ri;
                                v[ii] = sol.row_dual[row] / base;
                            }
                            v
                        },
                        shadow_price_vm_min: vec![],
                        shadow_price_vm_max: vec![],
                    },
                    devices: OpfDeviceDispatch::default(),
                    total_cost,
                    total_load_mw,
                    total_generation_mw: total_generation_mw_scopf,
                    total_losses_mw: total_losses_mw_scopf,
                    par_results: vec![],
                    virtual_bid_results: vec![],
                    benders_cut_duals: vec![],
                    solve_time_secs: solve_time,
                    iterations: Some(sol.iterations),
                    solver_name: Some(lp_solver.name().to_string()),
                    solver_version: Some(lp_solver.version().to_string()),
                },
                formulation: ScopfFormulation::Dc,
                mode: ScopfMode::Preventive,
                iterations: scopf_iter + 1,
                converged: true,
                total_contingencies_evaluated: contingencies.len(),
                total_contingency_constraints: n_ctg_cuts,
                binding_contingencies: binding,
                lmp_contingency_congestion: lmp_ctg_congestion,
                remaining_violations: vec![],
                failed_contingencies: vec![],
                screening_stats,
                solve_time_secs: solve_time,
            });
        }

        // Sort violations by severity and add top N as cuts
        violations.sort_by(|a, b| {
            b.severity
                .partial_cmp(&a.severity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let n_add = violations.len().min(options.max_cuts_per_iteration);

        for v in violations.iter().take(n_add) {
            constrained_pairs.insert((v.contingency_idx, v.monitored_branch_idx));

            let cut_idx = cut_metadata.len();
            let cut_row = n_base_rows + cut_idx;
            let s_up_col = n_var_base + 2 * cut_idx;
            let s_lo_col = n_var_base + 2 * cut_idx + 1;

            let l = v.monitored_branch_idx;

            // Classify contingency type
            let ctg = &contingencies[v.contingency_idx];
            let is_hvdc_ctg =
                !ctg.hvdc_converter_indices.is_empty() || !ctg.hvdc_cable_indices.is_empty();
            let is_gen_ctg =
                ctg.branch_indices.is_empty() && !ctg.generator_indices.is_empty() && !is_hvdc_ctg;
            let is_n2_ctg = ctg.branch_indices.len() == 2
                && n2_ctg_data.iter().any(|n| n.ctg_idx == v.contingency_idx);
            let has_mixed_gens = mixed_gen_shifts.contains_key(&v.contingency_idx);

            // Monitored branch l: b_dc_l*(θ_from_l - θ_to_l)
            let br_l = &network.branches[l];
            let b_l = br_l.b_dc();
            let from_l = bus_map[&br_l.from_bus];
            let to_l = bus_map[&br_l.to_bus];

            if is_gen_ctg {
                // Generator-trip cut:
                //   b_l × (θ_from_l - θ_to_l) + PTDF[l, bus_g] × (-Pg_g) ∈ [-fmax, fmax]
                // Rewrite: b_l × (θ_from_l - θ_to_l) - PTDF[l, bus_g] × Pg_g ∈ [-fmax, fmax]
                let gcd = match gen_ctg_data.iter().find(|g| g.ctg_idx == v.contingency_idx) {
                    Some(g) => g,
                    None => continue,
                };

                let ptdf_l_g = get_ptdf(&ptdf, l, gcd.bus_idx);

                // θ coefficients for monitored branch l
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + from_l,
                    val: b_l,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + to_l,
                    val: -b_l,
                });
                // Pg coefficient: trip generator means Pg → 0, so flow shift = PTDF × (-Pg)
                // In the constraint: base_flow_l + PTDF_lg × (-Pg_g) ≤ fmax
                //   → b_l×(θf-θt) - PTDF_lg × Pg_g ≤ fmax
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: pg_offset + gcd.gen_local,
                    val: -ptdf_l_g,
                });
                // Penalty slacks
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: s_up_col,
                    val: -1.0,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: s_lo_col,
                    val: 1.0,
                });

                let pfinj_l = b_l * br_l.phase_shift_rad;
                let fmax_l = options.contingency_rating.of(&network.branches[l]) / base;
                cut_row_lower.push(-fmax_l - pfinj_l);
                cut_row_upper.push(fmax_l - pfinj_l);

                cut_slack_cost.extend_from_slice(&[thermal_penalty_per_pu, thermal_penalty_per_pu]);
                cut_slack_lower.extend_from_slice(&[0.0, 0.0]);
                cut_slack_upper.extend_from_slice(&[f64::INFINITY, f64::INFINITY]);

                cut_metadata.push(CutInfo {
                    ctg_idx: v.contingency_idx,
                    monitored_branch_idx: l,
                    outaged_branch_indices: vec![],
                    lodf_lk: 0.0,
                    cut_type: CutType::GeneratorTrip,
                    gen_local_idx: Some(gcd.gen_local),
                });
            } else if is_hvdc_ctg {
                // HVDC contingency cut: the post-contingency flow is
                //   f_l_post = Bf[l,:]*θ + delta_flow_hvdc
                // Constraint: Bf[l,:]*θ ∈ [-fmax - delta, fmax - delta]
                // where delta = PTDF-based flow shift from removing HVDC injection.
                let tripped: Vec<usize> = ctg
                    .hvdc_converter_indices
                    .iter()
                    .chain(ctg.hvdc_cable_indices.iter())
                    .copied()
                    .collect();
                let mut delta_flow = 0.0_f64;
                for &hvdc_idx in &tripped {
                    if hvdc_idx >= hvdc_injections.len() {
                        continue;
                    }
                    let (fi, ti, p_from, p_to) = hvdc_injections[hvdc_idx];
                    delta_flow +=
                        get_ptdf(&ptdf, l, fi) * (-p_from) + get_ptdf(&ptdf, l, ti) * (-p_to);
                }

                // 2 θ-triplets + 2 slack triplets
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + from_l,
                    val: b_l,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + to_l,
                    val: -b_l,
                });
                // Penalty slacks
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: s_up_col,
                    val: -1.0,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: s_lo_col,
                    val: 1.0,
                });

                let pfinj_l = b_l * br_l.phase_shift_rad;
                let fmax_l = options.contingency_rating.of(&network.branches[l]) / base;
                cut_row_lower.push(-fmax_l - pfinj_l - delta_flow);
                cut_row_upper.push(fmax_l - pfinj_l - delta_flow);

                cut_slack_cost.extend_from_slice(&[thermal_penalty_per_pu, thermal_penalty_per_pu]);
                cut_slack_lower.extend_from_slice(&[0.0, 0.0]);
                cut_slack_upper.extend_from_slice(&[f64::INFINITY, f64::INFINITY]);

                cut_metadata.push(CutInfo {
                    ctg_idx: v.contingency_idx,
                    monitored_branch_idx: l,
                    outaged_branch_indices: vec![], // HVDC contingency, no outaged branch
                    lodf_lk: 0.0,
                    cut_type: CutType::BranchThermal,
                    gen_local_idx: None,
                });
            } else if is_n2_ctg {
                // N-2 multi-branch cut: compound LODF from Woodbury rank-2 update.
                // f_l_post = b_l*(θfl - θtl) + D_l1*b_k1*(θfk1 - θtk1) + D_l2*b_k2*(θfk2 - θtk2)
                let n2d = n2_ctg_data
                    .iter()
                    .find(|n| n.ctg_idx == v.contingency_idx)
                    .expect("N-2 contingency data must exist for n2 violation");
                let (d_l1, d_l2) = n2d.compound_lodf(&ptdf, l);

                let br_k1 = &network.branches[n2d.k1];
                let br_k2 = &network.branches[n2d.k2];
                let b_k1 = br_k1.b_dc();
                let b_k2 = br_k2.b_dc();

                // 6 θ-triplets: monitored + 2 outaged branches
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + from_l,
                    val: b_l,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + to_l,
                    val: -b_l,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + n2d.k1_from,
                    val: d_l1 * b_k1,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + n2d.k1_to,
                    val: -d_l1 * b_k1,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + n2d.k2_from,
                    val: d_l2 * b_k2,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + n2d.k2_to,
                    val: -d_l2 * b_k2,
                });

                // Gen-trip Pg coefficients for mixed N-2+gen contingencies
                if let Some(gen_locals) = mixed_gen_shifts.get(&v.contingency_idx) {
                    for &gl in gen_locals {
                        let gi = gen_indices[gl];
                        let bus_idx = bus_map[&network.generators[gi].bus];
                        let ptdf_l_g = get_ptdf(&ptdf, l, bus_idx);
                        cut_triplets.push(Triplet {
                            row: cut_row,
                            col: pg_offset + gl,
                            val: -ptdf_l_g,
                        });
                    }
                }

                // Penalty slacks
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: s_up_col,
                    val: -1.0,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: s_lo_col,
                    val: 1.0,
                });

                let pfinj_l = b_l * br_l.phase_shift_rad;
                let pfinj_k1 = b_k1 * br_k1.phase_shift_rad;
                let pfinj_k2 = b_k2 * br_k2.phase_shift_rad;
                let fmax_l = options.contingency_rating.of(&network.branches[l]) / base;
                cut_row_lower.push(-fmax_l - pfinj_l - d_l1 * pfinj_k1 - d_l2 * pfinj_k2);
                cut_row_upper.push(fmax_l - pfinj_l - d_l1 * pfinj_k1 - d_l2 * pfinj_k2);

                cut_slack_cost.extend_from_slice(&[thermal_penalty_per_pu, thermal_penalty_per_pu]);
                cut_slack_lower.extend_from_slice(&[0.0, 0.0]);
                cut_slack_upper.extend_from_slice(&[f64::INFINITY, f64::INFINITY]);

                cut_metadata.push(CutInfo {
                    ctg_idx: v.contingency_idx,
                    monitored_branch_idx: l,
                    outaged_branch_indices: vec![n2d.k1, n2d.k2],
                    lodf_lk: d_l1,
                    cut_type: CutType::MultiBranchN2,
                    gen_local_idx: None,
                });
            } else {
                // Branch contingency cut (LODF-based approach).
                let cd = ctg_data
                    .iter()
                    .find(|c| c.ctg_idx == v.contingency_idx)
                    .expect("contingency_idx from violation must exist in ctg_data");
                let lodf_lk = v.lodf_lk;

                // LODF * outaged branch k: LODF[l,k] * b_dc_k*(θ_from_k - θ_to_k)
                let br_k = &network.branches[cd.outaged_br];
                let b_k = br_k.b_dc();

                // 4 θ-triplets + 2 slack triplets
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + from_l,
                    val: b_l,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + to_l,
                    val: -b_l,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + cd.from_bus_idx,
                    val: lodf_lk * b_k,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: theta_offset + cd.to_bus_idx,
                    val: -lodf_lk * b_k,
                });

                // Gen-trip Pg coefficients for mixed branch+gen contingencies
                if has_mixed_gens && let Some(gen_locals) = mixed_gen_shifts.get(&v.contingency_idx)
                {
                    for &gl in gen_locals {
                        let gi = gen_indices[gl];
                        let bus_idx = bus_map[&network.generators[gi].bus];
                        let ptdf_l_g = get_ptdf(&ptdf, l, bus_idx);
                        cut_triplets.push(Triplet {
                            row: cut_row,
                            col: pg_offset + gl,
                            val: -ptdf_l_g,
                        });
                    }
                }

                // Penalty slacks for this contingency cut
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: s_up_col,
                    val: -1.0,
                });
                cut_triplets.push(Triplet {
                    row: cut_row,
                    col: s_lo_col,
                    val: 1.0,
                });

                // PST phase-shift offset: Bf*θ ∈ [-fmax - pfinj, fmax - pfinj]
                let pfinj_l = b_l * br_l.phase_shift_rad;
                let pfinj_k = b_k * br_k.phase_shift_rad;
                let fmax_l = options.contingency_rating.of(&network.branches[l]) / base;
                cut_row_lower.push(-fmax_l - pfinj_l - lodf_lk * pfinj_k);
                cut_row_upper.push(fmax_l - pfinj_l - lodf_lk * pfinj_k);

                cut_slack_cost.extend_from_slice(&[thermal_penalty_per_pu, thermal_penalty_per_pu]);
                cut_slack_lower.extend_from_slice(&[0.0, 0.0]);
                cut_slack_upper.extend_from_slice(&[f64::INFINITY, f64::INFINITY]);

                cut_metadata.push(CutInfo {
                    ctg_idx: v.contingency_idx,
                    monitored_branch_idx: l,
                    outaged_branch_indices: vec![cd.outaged_br],
                    lodf_lk,
                    cut_type: CutType::BranchThermal,
                    gen_local_idx: None,
                });
            }
        }

        scopf_iter += 1;
        info!(
            "SCOPF iteration {}: added {} cuts ({} total), {} violations found",
            scopf_iter,
            n_add,
            cut_metadata.len(),
            violations.len()
        );

        if scopf_iter >= options.max_iterations {
            return Err(DcOpfError::NotConverged {
                iterations: scopf_iter,
            });
        }
    }
}

// =============================================================================
// Corrective SCOPF (N-1 with post-contingency redispatch)
// =============================================================================

/// Solve corrective SCOPF using sparse B-theta formulation with HiGHS.
///
/// # Formulation
///
/// **Variables:**
/// ```text
/// x = [θ⁰ (n_bus) | Pg⁰ (n_gen)
///      | (per contingency k added): ΔPg_k (n_gen) | θ_k (n_bus)]
/// ```
///
/// **Objective:** minimize sum_g cost(Pg⁰[g])  ← pre-contingency cost only
///
/// **Constraints (base, identical to preventive SCOPF):**
/// - Power balance: B_bus * θ⁰ - A_gen * Pg⁰ = -Pd
/// - Pre-contingency thermal limits: |Bf * θ⁰| ≤ F_max
/// - Generator bounds: Pmin/base ≤ Pg⁰[g] ≤ Pmax/base
///
/// **Constraints (added per violated contingency k):**
/// - Energy balance: sum_g ΔPg_k[g] = 0
/// - Post-contingency generator bounds: Pmin/base - Pg⁰[g] ≤ ΔPg_k[g] ≤ Pmax/base - Pg⁰[g]
///   (encoded as: Pmin/base ≤ Pg⁰[g] + ΔPg_k[g] ≤ Pmax/base)
/// - Post-contingency power balance: B_k * θ_k = (Pg⁰ + ΔPg_k) - Pd
///   where B_k = B_bus minus outaged branch k
/// - Post-contingency thermal limits: |Bf_k * θ_k| ≤ F_max for all l ≠ k
///
/// This extensive-form formulation is built incrementally (contingencies added
/// only when a violation is detected), keeping LP size manageable.
///
/// # Arguments
/// * `network` — The power network.
/// * `options` — SCOPF options (the `corrective` flag is ignored; always corrective).
///
/// # Returns
/// A [`ScopfResult`] with the corrective SCOPF result.
pub(crate) fn solve_dc_corrective_with_context(
    network: &Network,
    options: &ScopfOptions,
    context: &ScopfRunContext,
) -> Result<ScopfResult, DcOpfError> {
    #[cfg(test)]
    let _ipopt_guard = crate::test_util::IPOPT_MUTEX
        .lock()
        .expect("IPOPT_MUTEX should not be poisoned");

    let start = Instant::now();
    let ctx = OpfNetworkContext::for_dc(network)?;
    let n_bus = ctx.n_bus;
    let n_br = ctx.n_branches;

    info!(
        buses = n_bus,
        branches = n_br,
        max_iterations = options.max_iterations,
        "starting corrective SCOPF"
    );
    let bus_map = &ctx.bus_map;
    let base = ctx.base_mva;
    let island_refs = ctx.island_refs.clone();
    let gen_indices = &ctx.gen_indices;
    let n_gen = gen_indices.len();
    let total_load_mw = ctx.total_load_mw;
    let bus_pd_mw = network.bus_load_p_mw();

    if ctx.total_capacity_mw < ctx.total_load_mw {
        return Err(DcOpfError::InsufficientCapacity {
            load_mw: ctx.total_load_mw,
            capacity_mw: ctx.total_capacity_mw,
        });
    }

    // Generator bus indices
    let gen_bus_idx: Vec<usize> = gen_indices
        .iter()
        .map(|&gi| bus_map[&network.generators[gi].bus])
        .collect();

    // --- PAR branch exclusion ---
    let par_branch_set: HashSet<usize> = options
        .dc_opf
        .par_setpoints
        .iter()
        .filter_map(|ps| {
            ctx.branch_idx_map
                .get(&(ps.from_bus, ps.to_bus, ps.circuit.clone()))
                .copied()
                .filter(|&idx| network.branches[idx].in_service)
        })
        .collect();

    // --- Constrained branches ---
    let constrained_branches: Vec<usize> = if options.dc_opf.enforce_thermal_limits {
        ctx.constrained_branch_indices(options.min_rate_a)
            .into_iter()
            .filter(|idx| !par_branch_set.contains(idx))
            .collect()
    } else {
        Vec::new()
    };
    let n_flow = constrained_branches.len();

    // Map from branch index to constrained-branch slot
    let mut br_to_ci: Vec<Option<usize>> = vec![None; n_br];
    for (ci, &l) in constrained_branches.iter().enumerate() {
        br_to_ci[l] = Some(ci);
    }

    // --- Identify angle-constrained branches ---
    // Only enforce when angmin > -π or angmax < +π (tighter than unconstrained default).
    // angmin/angmax are stored in radians by the IO parsers.
    // Gated by options.enforce_angle_limits.
    let angle_constrained_branches_c: Vec<usize> = if options.enforce_angle_limits {
        const ANG_UNCONSTRAINED_LO_C: f64 = -std::f64::consts::PI;
        const ANG_UNCONSTRAINED_HI_C: f64 = std::f64::consts::PI;
        (0..n_br)
            .filter(|&l| {
                let br = &network.branches[l];
                if !br.in_service {
                    return false;
                }
                let lo = br.angle_diff_min_rad.unwrap_or(f64::NEG_INFINITY);
                let hi = br.angle_diff_max_rad.unwrap_or(f64::INFINITY);
                lo > ANG_UNCONSTRAINED_LO_C || hi < ANG_UNCONSTRAINED_HI_C
            })
            .collect()
    } else {
        Vec::new()
    };
    let n_ang_c = angle_constrained_branches_c.len();

    // Variable HVDC links from DcOpfOptions.
    let all_hvdc_opf_links = options.dc_opf.hvdc_links.as_deref().unwrap_or(&[]);
    let hvdc_var_c: Vec<&crate::dc::opf::HvdcOpfLink> = all_hvdc_opf_links
        .iter()
        .filter(|h| h.is_variable())
        .collect();
    let n_hvdc_c = hvdc_var_c.len();

    let has_gen_slacks_c = options.dc_opf.gen_limit_penalty.is_some();
    let n_gen_slacks_c = if has_gen_slacks_c { n_gen } else { 0 };

    // --- Pre-contingency variable layout ---
    // x_base = [θ⁰ | Pg⁰ | P_hvdc⁰ | s_upper_base | s_lower_base
    //           | sa_upper | sa_lower | sg_upper | sg_lower]
    let theta0_off = 0usize;
    let pg0_off = n_bus;
    let hvdc0_off = n_bus + n_gen;
    let s_upper_base_off = hvdc0_off + n_hvdc_c;
    let s_lower_base_off = s_upper_base_off + n_flow;
    let sa_upper_base_off = s_lower_base_off + n_flow;
    let sa_lower_base_off = sa_upper_base_off + n_ang_c;
    let sg_upper_base_off = sa_lower_base_off + n_ang_c;
    let sg_lower_base_off = sg_upper_base_off + n_gen_slacks_c;
    let base_n_var = sg_lower_base_off + n_gen_slacks_c;

    // Penalty cost per per-unit thermal violation (matches dc_opf_lp.rs)
    let thermal_penalty_per_pu = options.penalty_config.thermal.marginal_cost_at(0.0) * base;
    let angle_penalty_per_rad = options.penalty_config.angle.marginal_cost_at(0.0);

    // --- PTDF for LODF calculation ---
    let all_branches_ptdf: Vec<usize> = (0..n_br).collect();
    let ptdf = surge_dc::compute_ptdf(
        network,
        &surge_dc::PtdfRequest::for_branches(&all_branches_ptdf),
    )
    .map_err(|e| DcOpfError::SolverError(e.to_string()))?;

    // --- Generate contingencies ---
    let contingencies = options
        .contingencies
        .clone()
        .unwrap_or_else(|| generate_n1_branch_contingencies(network));

    let monitored_branches: Vec<usize> = (0..n_br)
        .filter(|&l| {
            let br = &network.branches[l];
            br.in_service && br.rating_a_mva >= options.min_rate_a
        })
        .collect();

    // Pre-compute contingency LODF denominators
    let ctg_data: Vec<ContingencyData> = {
        let mut v = Vec::with_capacity(contingencies.len());
        for (ci, ctg) in contingencies.iter().enumerate() {
            if ctg.branch_indices.len() != 1 || !ctg.generator_indices.is_empty() {
                warn!(
                    contingency = %ctg.label,
                    "corrective SCOPF: skipping contingency (only single-branch N-1 supported)"
                );
                continue;
            }
            let outaged_br = ctg.branch_indices[0];
            if outaged_br >= n_br || !network.branches[outaged_br].in_service {
                continue;
            }
            let br = &network.branches[outaged_br];
            let from_idx = bus_map[&br.from_bus];
            let to_idx = bus_map[&br.to_bus];
            let ptdf_diff_k =
                get_ptdf(&ptdf, outaged_br, from_idx) - get_ptdf(&ptdf, outaged_br, to_idx);
            let denom = 1.0 - ptdf_diff_k;
            if denom.abs() < 1e-10 {
                continue;
            }
            v.push(ContingencyData {
                ctg_idx: ci,
                outaged_br,
                from_bus_idx: from_idx,
                to_bus_idx: to_idx,
                denom,
                label: ctg.label.clone(),
            });
        }
        v
    };

    // --- Objective (linear costs on Pg⁰ only) ---
    // We will grow col_cost as corrective blocks are added (new columns have 0 cost)
    let mut obj_coeffs_base = vec![0.0f64; base_n_var];
    let mut q_diag = vec![0.0f64; n_gen];
    let mut c0_total = 0.0f64;

    for (j, &gi) in gen_indices.iter().enumerate() {
        let g = &network.generators[gi];
        match g.cost.as_ref().ok_or(DcOpfError::MissingCost {
            gen_idx: gi,
            bus: g.bus,
        })? {
            CostCurve::Polynomial { coeffs, .. } => match coeffs.len() {
                0 => {}
                1 => c0_total += coeffs[0],
                2 => {
                    obj_coeffs_base[pg0_off + j] = coeffs[0] * base;
                    c0_total += coeffs[1];
                }
                _ => {
                    q_diag[j] = 2.0 * coeffs[0] * base * base;
                    obj_coeffs_base[pg0_off + j] = coeffs[1] * base;
                    c0_total += coeffs[2];
                }
            },
            CostCurve::PiecewiseLinear { points, .. } => {
                if points.len() >= 2 {
                    let (x0, y0) = points[0];
                    let (x1, y1) = points[points.len() - 1];
                    let dx = x1 - x0;
                    if dx > 1e-10 {
                        obj_coeffs_base[pg0_off + j] = (y1 - y0) / dx * base;
                    }
                    c0_total += y0;
                }
            }
        }
    }

    // Base thermal slack penalty costs
    for ci in 0..n_flow {
        obj_coeffs_base[s_upper_base_off + ci] = thermal_penalty_per_pu;
        obj_coeffs_base[s_lower_base_off + ci] = thermal_penalty_per_pu;
    }
    // Angle slack penalty costs.
    for ai in 0..n_ang_c {
        obj_coeffs_base[sa_upper_base_off + ai] = angle_penalty_per_rad;
        obj_coeffs_base[sa_lower_base_off + ai] = angle_penalty_per_rad;
    }
    // Gen-limit slack penalty costs.
    if let Some(penalty) = options.dc_opf.gen_limit_penalty {
        let penalty_pu = penalty * base;
        for j in 0..n_gen {
            obj_coeffs_base[sg_upper_base_off + j] = penalty_pu;
            obj_coeffs_base[sg_lower_base_off + j] = penalty_pu;
        }
    }

    let has_quadratic = q_diag.iter().any(|&v| v.abs() > 1e-20);

    // --- Base column bounds (pre-contingency) ---
    let mut col_lower_base = vec![0.0f64; base_n_var];
    let mut col_upper_base = vec![0.0f64; base_n_var];

    crate::dc::island_lmp::fix_island_theta_bounds(
        &mut col_lower_base,
        &mut col_upper_base,
        theta0_off,
        n_bus,
        &island_refs,
    );
    for (j, &gi) in gen_indices.iter().enumerate() {
        if has_gen_slacks_c {
            col_lower_base[pg0_off + j] = f64::NEG_INFINITY;
            col_upper_base[pg0_off + j] = f64::INFINITY;
        } else {
            col_lower_base[pg0_off + j] = network.generators[gi].pmin / base;
            col_upper_base[pg0_off + j] = network.generators[gi].pmax / base;
        }
    }
    // HVDC variable bounds.
    for (k, hvdc) in hvdc_var_c.iter().enumerate() {
        col_lower_base[hvdc0_off + k] = hvdc.p_dc_min_mw / base;
        col_upper_base[hvdc0_off + k] = hvdc.p_dc_max_mw / base;
    }
    // Base thermal slack bounds: [0, +∞)
    for ci in 0..n_flow {
        col_lower_base[s_upper_base_off + ci] = 0.0;
        col_upper_base[s_upper_base_off + ci] = f64::INFINITY;
        col_lower_base[s_lower_base_off + ci] = 0.0;
        col_upper_base[s_lower_base_off + ci] = f64::INFINITY;
    }
    // Angle slack bounds: [0, +∞)
    for ai in 0..n_ang_c {
        col_lower_base[sa_upper_base_off + ai] = 0.0;
        col_upper_base[sa_upper_base_off + ai] = f64::INFINITY;
        col_lower_base[sa_lower_base_off + ai] = 0.0;
        col_upper_base[sa_lower_base_off + ai] = f64::INFINITY;
    }
    // Gen-limit slack bounds.
    for j in 0..n_gen_slacks_c {
        col_lower_base[sg_upper_base_off + j] = 0.0;
        col_upper_base[sg_upper_base_off + j] = f64::INFINITY;
        col_lower_base[sg_lower_base_off + j] = 0.0;
        col_upper_base[sg_lower_base_off + j] = f64::INFINITY;
    }

    // --- Base constraint triplets (pre-contingency, fixed) ---
    // Row layout (base):
    //   0..n_flow                      : branch thermal limits Bf*θ⁰ ∈ [-fmax, fmax]
    //   n_flow..n_flow+n_ang_c         : angle diff limits θ_from - θ_to ∈ [angmin, angmax]
    //   n_flow+n_ang_c..+n_bus         : power balance B*θ⁰ - A_gen*Pg⁰ = -Pd
    //   ..+2*n_gen_slacks              : gen-limit soft constraints (if enabled)
    //
    // These rows reference only base-variable columns (0..base_n_var).
    let base_n_row = n_flow + n_ang_c + n_bus + 2 * n_gen_slacks_c;

    let mut base_triplets: Vec<Triplet<f64>> = Vec::new();

    // Branch flow rows (soft via penalty slacks)
    for (ci, &l) in constrained_branches.iter().enumerate() {
        let br = &network.branches[l];
        if br.x.abs() < 1e-20 {
            continue;
        }
        let b_val = br.b_dc();
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];
        base_triplets.push(Triplet {
            row: ci,
            col: theta0_off + from,
            val: b_val,
        });
        base_triplets.push(Triplet {
            row: ci,
            col: theta0_off + to,
            val: -b_val,
        });
        // Penalty slacks: -s_upper + s_lower
        base_triplets.push(Triplet {
            row: ci,
            col: s_upper_base_off + ci,
            val: -1.0,
        });
        base_triplets.push(Triplet {
            row: ci,
            col: s_lower_base_off + ci,
            val: 1.0,
        });
    }

    // Angle difference rows (n_flow..n_flow+n_ang_c): soft via penalty slacks
    for (ai, &l) in angle_constrained_branches_c.iter().enumerate() {
        let br = &network.branches[l];
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];
        let ang_row = n_flow + ai;
        base_triplets.push(Triplet {
            row: ang_row,
            col: theta0_off + from,
            val: 1.0,
        });
        base_triplets.push(Triplet {
            row: ang_row,
            col: theta0_off + to,
            val: -1.0,
        });
        base_triplets.push(Triplet {
            row: ang_row,
            col: sa_upper_base_off + ai,
            val: -1.0,
        });
        base_triplets.push(Triplet {
            row: ang_row,
            col: sa_lower_base_off + ai,
            val: 1.0,
        });
    }

    // Power balance rows (B_bus * θ⁰): rows n_flow+n_ang_c..n_flow+n_ang_c+n_bus
    for (br_idx, branch) in network.branches.iter().enumerate() {
        if !branch.in_service || branch.x.abs() < 1e-20 || par_branch_set.contains(&br_idx) {
            continue;
        }
        let from = bus_map[&branch.from_bus];
        let to = bus_map[&branch.to_bus];
        let b = branch.b_dc();
        let eq_from = n_flow + n_ang_c + from;
        let eq_to = n_flow + n_ang_c + to;
        base_triplets.push(Triplet {
            row: eq_from,
            col: theta0_off + to,
            val: -b,
        });
        base_triplets.push(Triplet {
            row: eq_to,
            col: theta0_off + from,
            val: -b,
        });
        base_triplets.push(Triplet {
            row: eq_from,
            col: theta0_off + from,
            val: b,
        });
        base_triplets.push(Triplet {
            row: eq_to,
            col: theta0_off + to,
            val: b,
        });
    }
    // -A_gen block
    let mut gen_balance_triplet_indices_c = Vec::with_capacity(gen_bus_idx.len());
    for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
        gen_balance_triplet_indices_c.push(base_triplets.len());
        base_triplets.push(Triplet {
            row: n_flow + n_ang_c + bus_idx,
            col: pg0_off + j,
            val: -1.0,
        });
    }
    let balance_row_offset_c = n_flow + n_ang_c;
    // HVDC variable link power balance coefficients.
    let hvdc_from_idx_c: Vec<usize> = hvdc_var_c.iter().map(|h| bus_map[&h.from_bus]).collect();
    let hvdc_to_idx_c: Vec<usize> = hvdc_var_c.iter().map(|h| bus_map[&h.to_bus]).collect();
    for (k, hvdc) in hvdc_var_c.iter().enumerate() {
        base_triplets.push(Triplet {
            row: n_flow + n_ang_c + hvdc_from_idx_c[k],
            col: hvdc0_off + k,
            val: 1.0,
        });
        base_triplets.push(Triplet {
            row: n_flow + n_ang_c + hvdc_to_idx_c[k],
            col: hvdc0_off + k,
            val: -(1.0 - hvdc.loss_b_frac),
        });
    }
    // Gen-limit soft constraint rows.
    if has_gen_slacks_c {
        let gen_pmax_row_c = n_flow + n_ang_c + n_bus;
        let gen_pmin_row_c = gen_pmax_row_c + n_gen;
        for j in 0..n_gen {
            base_triplets.push(Triplet {
                row: gen_pmax_row_c + j,
                col: pg0_off + j,
                val: 1.0,
            });
            base_triplets.push(Triplet {
                row: gen_pmax_row_c + j,
                col: sg_upper_base_off + j,
                val: -1.0,
            });
            base_triplets.push(Triplet {
                row: gen_pmin_row_c + j,
                col: pg0_off + j,
                val: -1.0,
            });
            base_triplets.push(Triplet {
                row: gen_pmin_row_c + j,
                col: sg_lower_base_off + j,
                val: -1.0,
            });
        }
    }

    // --- Base row bounds ---
    let mut base_row_lower: Vec<f64> = Vec::with_capacity(base_n_row);
    let mut base_row_upper: Vec<f64> = Vec::with_capacity(base_n_row);

    for &l in &constrained_branches {
        let fmax = network.branches[l].rating_a_mva / base;
        base_row_lower.push(-fmax);
        base_row_upper.push(fmax);
    }
    // Angle difference bounds
    for &l in &angle_constrained_branches_c {
        let br = &network.branches[l];
        base_row_lower.push(br.angle_diff_min_rad.unwrap_or(f64::NEG_INFINITY));
        base_row_upper.push(br.angle_diff_max_rad.unwrap_or(f64::INFINITY));
    }
    // Power balance: B*θ⁰ - A_gen*Pg⁰ = -(Pd + Gs)/base - Pbusinj
    //
    // Same correction as preventive SCOPF and dc_opf_lp: include shunt conductance Gs
    // and PST bus injection offsets Pbusinj so the power balance matches MATPOWER exactly.
    let mut pbusinj_corrective = vec![0.0_f64; n_bus];
    for branch in &network.branches {
        if !branch.in_service || branch.x.abs() < 1e-20 || branch.phase_shift_rad.abs() < 1e-12 {
            continue;
        }
        let pf = branch.b_dc() * branch.phase_shift_rad;
        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];
        pbusinj_corrective[from_idx] += pf;
        pbusinj_corrective[to_idx] -= pf;
    }

    // HVDC link injections.
    if !all_hvdc_opf_links.is_empty() {
        for hvdc in all_hvdc_opf_links.iter().filter(|h| !h.is_variable()) {
            let p_dc = hvdc.p_dc_min_mw;
            let p_inv = hvdc.p_inv_mw(p_dc);
            let fi = bus_map[&hvdc.from_bus];
            pbusinj_corrective[fi] += p_dc / base;
            let ti = bus_map[&hvdc.to_bus];
            pbusinj_corrective[ti] -= p_inv / base;
        }
        for (k, hvdc) in hvdc_var_c.iter().enumerate() {
            let ti = hvdc_to_idx_c[k];
            pbusinj_corrective[ti] += hvdc.loss_a_mw / base;
        }
    } else {
        let hvdc_links_corrective = surge_hvdc::interop::links_from_network(network);
        for link in &hvdc_links_corrective {
            if let Some(&fi) = bus_map.get(&link.from_bus()) {
                pbusinj_corrective[fi] += link.p_dc_mw() / base;
            }
            if let Some(&ti) = bus_map.get(&link.to_bus()) {
                pbusinj_corrective[ti] -= link.p_dc_mw() / base;
            }
        }
    }

    // Multi-terminal DC (MTDC) grid injections.
    {
        let dc_grid_results =
            surge_hvdc::interop::dc_grid_injections(network).map_err(|error| {
                DcOpfError::SolverError(format!("explicit DC-grid solve failed: {error}"))
            })?;
        for inj in &dc_grid_results.injections {
            if let Some(&i) = bus_map.get(&inj.ac_bus) {
                pbusinj_corrective[i] -= inj.p_mw / base;
            }
        }
    }

    // PAR scheduled-interchange injections.
    for ps in &options.dc_opf.par_setpoints {
        if let Some(&br_idx) = ctx
            .branch_idx_map
            .get(&(ps.from_bus, ps.to_bus, ps.circuit.clone()))
        {
            let br = &network.branches[br_idx];
            if !br.in_service || br.x.abs() < 1e-20 {
                continue;
            }
            // Undo PST phase-shift contribution
            if br.phase_shift_rad.abs() >= 1e-12 {
                let pf = br.b_dc() * br.phase_shift_rad;
                let fi = bus_map[&br.from_bus];
                let ti = bus_map[&br.to_bus];
                pbusinj_corrective[fi] -= pf;
                pbusinj_corrective[ti] += pf;
            }
            // Add scheduled-interchange injection
            let fi = bus_map[&ps.from_bus];
            let ti = bus_map[&ps.to_bus];
            pbusinj_corrective[fi] += ps.target_mw / base;
            pbusinj_corrective[ti] -= ps.target_mw / base;
        }
    }

    for i in 0..n_bus {
        let pd_pu = bus_pd_mw[i] / base;
        let gs_pu = network.buses[i].shunt_conductance_mw / base;
        let rhs = -pd_pu - gs_pu - pbusinj_corrective[i];
        base_row_lower.push(rhs);
        base_row_upper.push(rhs);
    }
    // Gen-limit soft constraint row bounds.
    if has_gen_slacks_c {
        for &gi in gen_indices {
            let g = &network.generators[gi];
            base_row_lower.push(f64::NEG_INFINITY);
            base_row_upper.push(g.pmax / base);
        }
        for &gi in gen_indices {
            let g = &network.generators[gi];
            base_row_lower.push(f64::NEG_INFINITY);
            base_row_upper.push(-g.pmin / base);
        }
    }

    // --- Iterative corrective constraint generation ---
    // Growing extended variable / constraint state:
    //   extra_triplets     : constraint entries for corrective blocks
    //   extra_row_lower/upper : bounds for corrective rows
    //   extra_col_lower/upper/cost : bounds/costs for corrective variable columns
    //   ctg_blocks         : list of activated contingency blocks
    //   constrained_pairs  : (ctg_idx, monitored_br) already added as constraints
    let mut extra_triplets: Vec<Triplet<f64>> = Vec::new();
    let mut extra_row_lower: Vec<f64> = Vec::new();
    let mut extra_row_upper: Vec<f64> = Vec::new();
    let mut extra_col_lower: Vec<f64> = Vec::new();
    let mut extra_col_upper: Vec<f64> = Vec::new();
    let mut extra_col_cost: Vec<f64> = Vec::new();
    let mut ctg_blocks: Vec<CorrectiveCtgBlock> = Vec::new();
    // Pairs for which we've already added a post-contingency thermal row
    let mut thermal_pairs: HashSet<(usize, usize)> = HashSet::new();
    // Contingency indices for which a corrective block has been activated
    let mut activated_ctg: HashSet<usize> = HashSet::new();
    // Cap on corrective contingency blocks
    let max_corrective_ctg: usize = 200;

    let lp_solver = context
        .runtime
        .lp_solver
        .clone()
        .map_or_else(|| try_default_lp_solver(), Ok)
        .map_err(DcOpfError::SolverError)?;

    let mut scopf_iter = 0u32;
    let use_loss_factors_c = options.dc_opf.use_loss_factors;
    let max_loss_iter_c = options.dc_opf.max_loss_iter;
    let loss_tol_c = options.dc_opf.loss_tol;
    let mut prev_dloss_c = vec![0.0f64; n_bus];
    let mut loss_iter_count_c = 0usize;

    loop {
        // --- Build current LP dimensions ---
        let n_extra_col = extra_col_lower.len();
        let n_var = base_n_var + n_extra_col;
        let n_extra_row = extra_row_lower.len();
        let n_row = base_n_row + n_extra_row;

        // --- Build col_cost ---
        let mut col_cost: Vec<f64> = obj_coeffs_base.clone();
        col_cost.extend_from_slice(&extra_col_cost);

        // --- Build col_lower / col_upper ---
        let mut col_lower: Vec<f64> = col_lower_base.clone();
        col_lower.extend_from_slice(&extra_col_lower);
        let mut col_upper: Vec<f64> = col_upper_base.clone();
        col_upper.extend_from_slice(&extra_col_upper);

        // --- Build Hessian (diagonal, upper-triangular CSC) ---
        // The Hessian only touches Pg⁰ columns (pg0_off..pg0_off+n_gen).
        // Extra corrective columns have zero quadratic cost.
        // Tikhonov regularization: see dc_opf_lp.rs for rationale.
        let mut q_start_vec: Vec<i32> = Vec::new();
        let mut q_index_vec: Vec<i32> = Vec::new();
        let mut q_value_vec: Vec<f64> = Vec::new();

        if has_quadratic {
            q_start_vec = Vec::with_capacity(n_var + 1);
            // θ⁰ columns: no Hessian
            for _ in 0..n_bus {
                q_start_vec.push(q_index_vec.len() as i32);
            }
            // Pg⁰ columns: only nonzero-c2 generators enter the Hessian.
            for (j, &qd) in q_diag.iter().enumerate() {
                q_start_vec.push(q_index_vec.len() as i32);
                if qd.abs() > 1e-20 {
                    q_index_vec.push((pg0_off + j) as i32);
                    q_value_vec.push(qd);
                }
            }
            // HVDC + thermal slacks + angle slacks + gen-limit slacks + extra corrective columns: no Hessian
            for _ in 0..(n_hvdc_c + 2 * n_flow + 2 * n_ang_c + 2 * n_gen_slacks_c + n_extra_col) {
                q_start_vec.push(q_index_vec.len() as i32);
            }
            q_start_vec.push(q_index_vec.len() as i32);
        }

        // --- Assemble full triplet list ---
        let mut all_triplets: Vec<Triplet<f64>> =
            Vec::with_capacity(base_triplets.len() + extra_triplets.len());
        for t in &base_triplets {
            all_triplets.push(Triplet {
                row: t.row,
                col: t.col,
                val: t.val,
            });
        }
        for t in &extra_triplets {
            all_triplets.push(Triplet {
                row: t.row,
                col: t.col,
                val: t.val,
            });
        }
        let (a_start, a_index, a_value) = triplets_to_csc(&all_triplets, n_row, n_var);

        // --- Row bounds ---
        let mut row_lower: Vec<f64> = Vec::with_capacity(n_row);
        let mut row_upper: Vec<f64> = Vec::with_capacity(n_row);
        row_lower.extend_from_slice(&base_row_lower);
        row_upper.extend_from_slice(&base_row_upper);
        row_lower.extend_from_slice(&extra_row_lower);
        row_upper.extend_from_slice(&extra_row_upper);

        // --- Solve LP ---
        let prob = SparseProblem {
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
            q_start: if has_quadratic {
                Some(q_start_vec)
            } else {
                None
            },
            q_index: if has_quadratic {
                Some(q_index_vec)
            } else {
                None
            },
            q_value: if has_quadratic {
                Some(q_value_vec)
            } else {
                None
            },
            integrality: None,
        };
        let lp_opts = LpOptions {
            tolerance: options.dc_opf.tolerance,
            ..Default::default()
        };
        let sol = lp_solver
            .solve(&prob, &lp_opts)
            .map_err(DcOpfError::SolverError)?;

        match sol.status {
            crate::backends::LpSolveStatus::Optimal => {}
            crate::backends::LpSolveStatus::SubOptimal => {
                return Err(DcOpfError::SubOptimalSolution);
            }
            _ => {
                return Err(DcOpfError::NotConverged {
                    iterations: sol.iterations,
                });
            }
        }

        // --- Extract base-case solution ---
        let theta0 = &sol.x[theta0_off..theta0_off + n_bus];
        let pg0_pu = &sol.x[pg0_off..pg0_off + n_gen];

        // Base branch flows from θ⁰
        // Use b_dc()*(θ_from - θ_to - shift_rad) — matches preventive path and DC PF definition.
        // Plain (θ_from - θ_to)/x would ignore tap ratio and PST shift, producing wrong LODF
        // screening and false/missed security violations for transformer and PST branches.
        let mut base_flow = vec![0.0f64; n_br];
        for l in 0..n_br {
            let br = &network.branches[l];
            if !br.in_service || br.x.abs() < 1e-20 {
                continue;
            }
            let from = bus_map[&br.from_bus];
            let to = bus_map[&br.to_bus];
            let shift_rad = br.phase_shift_rad;
            base_flow[l] = br.b_dc() * (theta0[from] - theta0[to] - shift_rad);
        }

        // --- Check contingency violations via LODF ---
        let mut violations: Vec<ViolationInfo> = Vec::new();

        for cd in &ctg_data {
            let flow_k = base_flow[cd.outaged_br];

            for &l in &monitored_branches {
                if l == cd.outaged_br {
                    continue;
                }
                if thermal_pairs.contains(&(cd.ctg_idx, l)) {
                    continue;
                }

                let ptdf_diff_l =
                    get_ptdf(&ptdf, l, cd.from_bus_idx) - get_ptdf(&ptdf, l, cd.to_bus_idx);
                let lodf_lk = ptdf_diff_l / cd.denom;
                if !lodf_lk.is_finite() {
                    continue;
                }

                let post_flow = base_flow[l] + lodf_lk * flow_k;
                let f_max = options.contingency_rating.of(&network.branches[l]) / base;
                let excess = post_flow.abs() - f_max;

                if excess > options.violation_tolerance_pu {
                    violations.push(ViolationInfo {
                        contingency_idx: cd.ctg_idx,
                        monitored_branch_idx: l,
                        severity: excess,
                        lodf_lk,
                    });
                }
            }
        }

        if violations.is_empty() {
            // Converged
            let n_ctg_constraints = thermal_pairs.len();

            info!(
                "Corrective SCOPF converged in {} iterations ({} contingency blocks, {} thermal rows)",
                scopf_iter + 1,
                ctg_blocks.len(),
                n_ctg_constraints
            );

            // Cutting-plane converged. Check if loss iteration is needed.
            if use_loss_factors_c && loss_iter_count_c < max_loss_iter_c {
                use crate::dc::loss_factors::{
                    compute_dc_loss_sensitivities, compute_total_dc_losses,
                };
                let dloss_dp = compute_dc_loss_sensitivities(network, theta0, bus_map, &ptdf);

                let need_resolve = if loss_iter_count_c > 0 {
                    let max_change = dloss_dp
                        .iter()
                        .zip(prev_dloss_c.iter())
                        .map(|(a, b)| (a - b).abs())
                        .fold(0.0f64, f64::max);
                    max_change >= loss_tol_c
                } else {
                    true
                };

                if need_resolve {
                    prev_dloss_c.copy_from_slice(&dloss_dp);

                    for (j, &ti) in gen_balance_triplet_indices_c.iter().enumerate() {
                        let bus_idx = gen_bus_idx[j];
                        let pf_inv = (1.0 - dloss_dp[bus_idx]).clamp(0.5, 1.5);
                        base_triplets[ti].val = -pf_inv;
                    }

                    let total_loss_pu = compute_total_dc_losses(network, theta0, bus_map);
                    let total_load_pu: f64 = bus_pd_mw
                        .iter()
                        .map(|&pd| pd / base)
                        .sum::<f64>()
                        .abs()
                        .max(1e-10);
                    for i in 0..n_bus {
                        let load_share = (bus_pd_mw[i] / base).abs() / total_load_pu;
                        let loss_at_bus = total_loss_pu * load_share;
                        let pd_pu = bus_pd_mw[i] / base;
                        let gs_pu = network.buses[i].shunt_conductance_mw / base;
                        let rhs = -pd_pu - gs_pu - pbusinj_corrective[i] - loss_at_bus;
                        base_row_lower[balance_row_offset_c + i] = rhs;
                        base_row_upper[balance_row_offset_c + i] = rhs;
                    }

                    loss_iter_count_c += 1;
                    info!(
                        loss_iter = loss_iter_count_c,
                        "corrective loss factor iteration — re-solving"
                    );
                    continue;
                }
            }

            // --- Extract results ---
            let gen_p_mw: Vec<f64> = pg0_pu.iter().map(|&p| p * base).collect();
            let total_cost = sol.objective + c0_total;

            // LMPs from power balance duals
            let lmp: Vec<f64> = (0..n_bus)
                .map(|i| sol.row_dual[n_flow + n_ang_c + i] / base)
                .collect();
            let (lmp_energy, lmp_congestion, lmp_loss) =
                if use_loss_factors_c && loss_iter_count_c > 0 {
                    use crate::dc::loss_factors::compute_dc_loss_sensitivities;
                    let dloss_dp = compute_dc_loss_sensitivities(network, theta0, bus_map, &ptdf);
                    crate::dc::island_lmp::decompose_lmp_with_losses(&lmp, &dloss_dp, &island_refs)
                } else {
                    crate::dc::island_lmp::decompose_lmp_lossless(&lmp, &island_refs)
                };

            // Branch shadow prices
            let mut branch_shadow_prices = vec![0.0f64; n_br];
            for (ci, &l) in constrained_branches.iter().enumerate() {
                branch_shadow_prices[l] = sol.row_dual[ci] / base;
            }

            let va: Vec<f64> = theta0.to_vec();
            let mut p_inject = vec![0.0f64; n_bus];
            for i in 0..n_bus {
                p_inject[i] = -bus_pd_mw[i] / base;
            }
            for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
                p_inject[bus_idx] += pg0_pu[j];
            }

            let branch_pf_pu: Vec<f64> = network
                .branches
                .iter()
                .map(|br| {
                    if !br.in_service {
                        return 0.0;
                    }
                    let from_i = bus_map[&br.from_bus];
                    let to_i = bus_map[&br.to_bus];
                    br.b_dc() * (theta0[from_i] - theta0[to_i] - br.phase_shift_rad)
                })
                .collect();
            let branch_pf_mw: Vec<f64> = branch_pf_pu.iter().map(|&p| p * base).collect();
            let branch_pt_mw: Vec<f64> = branch_pf_mw.iter().map(|&p| -p).collect();
            let branch_loading_pct = branch_loading_pct_from_flows(network, &branch_pf_mw);

            let pf_solution = PfSolution {
                pf_model: surge_solution::PfModel::Dc,
                status: SolveStatus::Converged,
                iterations: 1,
                max_mismatch: 0.0,
                solve_time_secs: 0.0,
                voltage_magnitude_pu: vec![1.0; n_bus],
                voltage_angle_rad: va,
                active_power_injection_pu: p_inject,
                reactive_power_injection_pu: vec![0.0; n_bus],
                branch_p_from_mw: branch_pf_mw,
                branch_p_to_mw: branch_pt_mw,
                branch_q_from_mvar: vec![0.0; n_br],
                branch_q_to_mvar: vec![0.0; n_br],
                bus_numbers: network.buses.iter().map(|b| b.number).collect(),
                island_ids: vec![],
                q_limited_buses: vec![],
                n_q_limit_switches: 0,
                gen_slack_contribution_mw: vec![],
                convergence_history: vec![],
                worst_mismatch_bus: None,
                area_interchange: None,
            };

            let solve_time = start.elapsed().as_secs_f64();

            info!(
                "Corrective SCOPF solved in {:.1} ms ({} generators, {} base constraints, {} ctg blocks, cost={:.2} $/hr)",
                solve_time * 1000.0,
                n_gen,
                constrained_branches.len(),
                ctg_blocks.len(),
                total_cost
            );

            let gen_bus_numbers_cscopf: Vec<u32> = gen_indices
                .iter()
                .map(|&gi| network.generators[gi].bus)
                .collect();
            let gen_ids_cscopf: Vec<String> = gen_indices
                .iter()
                .map(|&gi| network.generators[gi].id.clone())
                .collect();
            let gen_machine_ids_cscopf: Vec<String> = gen_indices
                .iter()
                .map(|&gi| {
                    network.generators[gi]
                        .machine_id
                        .clone()
                        .unwrap_or_else(|| "1".to_string())
                })
                .collect();
            let total_generation_mw_cscopf: f64 = gen_p_mw.iter().sum();
            let total_losses_mw_cscopf = if use_loss_factors_c && loss_iter_count_c > 0 {
                use crate::dc::loss_factors::compute_total_dc_losses;
                compute_total_dc_losses(network, theta0, bus_map) * base
            } else {
                0.0
            };

            // Extract generator bound duals from LP column duals.
            // Pre-contingency Pg⁰ variables are at columns pg0_off + j.
            // HiGHS col_dual: negative at lower bound, positive at upper bound.
            let mu_pg_min_cscopf: Vec<f64> = (0..n_gen)
                .map(|j| (-sol.col_dual[pg0_off + j]).max(0.0) / base)
                .collect();
            let mu_pg_max_cscopf: Vec<f64> = (0..n_gen)
                .map(|j| sol.col_dual[pg0_off + j].max(0.0) / base)
                .collect();

            return Ok(ScopfResult {
                base_opf: OpfSolution {
                    opf_type: surge_solution::OpfType::DcScopf,
                    base_mva: network.base_mva,
                    power_flow: pf_solution,
                    generators: OpfGeneratorResults {
                        gen_p_mw,
                        gen_q_mvar: vec![],
                        gen_bus_numbers: gen_bus_numbers_cscopf,
                        gen_ids: gen_ids_cscopf,
                        gen_machine_ids: gen_machine_ids_cscopf,
                        shadow_price_pg_min: mu_pg_min_cscopf,
                        shadow_price_pg_max: mu_pg_max_cscopf,
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
                        shadow_price_angmin: vec![],
                        shadow_price_angmax: vec![],
                        flowgate_shadow_prices: vec![],
                        interface_shadow_prices: vec![],
                        shadow_price_vm_min: vec![],
                        shadow_price_vm_max: vec![],
                    },
                    devices: OpfDeviceDispatch::default(),
                    total_cost,
                    total_load_mw,
                    total_generation_mw: total_generation_mw_cscopf,
                    total_losses_mw: total_losses_mw_cscopf,
                    par_results: vec![],
                    virtual_bid_results: vec![],
                    benders_cut_duals: vec![],
                    solve_time_secs: solve_time,
                    iterations: Some(sol.iterations),
                    solver_name: Some(lp_solver.name().to_string()),
                    solver_version: Some(lp_solver.version().to_string()),
                },
                formulation: ScopfFormulation::Dc,
                mode: ScopfMode::Corrective,
                iterations: scopf_iter + 1,
                converged: true,
                total_contingencies_evaluated: ctg_data.len(),
                total_contingency_constraints: n_ctg_constraints,
                binding_contingencies: vec![],
                lmp_contingency_congestion: vec![0.0; n_bus],
                remaining_violations: vec![],
                failed_contingencies: vec![],
                screening_stats: ScopfScreeningStats::default(),
                solve_time_secs: solve_time,
            });
        }

        // --- Add corrective constraints for violated pairs ---
        // Sort by severity
        violations.sort_by(|a, b| {
            b.severity
                .partial_cmp(&a.severity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let n_add = violations.len().min(options.max_cuts_per_iteration);

        for v in violations.iter().take(n_add) {
            let ctg_idx = v.contingency_idx;
            let l = v.monitored_branch_idx;

            // Mark this (ctg, branch) pair as handled
            thermal_pairs.insert((ctg_idx, l));

            let cd = ctg_data
                .iter()
                .find(|c| c.ctg_idx == ctg_idx)
                .expect("ctg_idx from violation must exist in ctg_data");
            let outaged_br = cd.outaged_br;

            // --- Activate corrective block for contingency k if not yet done ---
            if !activated_ctg.contains(&ctg_idx) {
                if ctg_blocks.len() >= max_corrective_ctg {
                    // Cap reached — skip adding a new corrective block.
                    // The violation will persist; accept it rather than growing indefinitely.
                    continue;
                }

                // New corrective block layout (appended to extra columns):
                //
                //   ΔPg_k[0..n_gen]   : redispatch deltas, per-unit
                //   θ_k[0..n_bus]     : post-contingency angles
                //
                // Column indices within the full LP:
                let dpg_col_offset = base_n_var + extra_col_lower.len();
                let theta_k_col_offset = dpg_col_offset + n_gen;

                // ΔPg_k bounds: ramp-rate-limited corrective redispatch per generator.
                // Ramp rate priority: ramp_agc > ramp_up > capacity swing fallback.
                // Also applies marginal cost signal for corrective upward redispatch.
                for j in 0..n_gen {
                    let g = &network.generators[gen_indices[j]];
                    let window = options.corrective.ramp_window_min;

                    // Ramp budget: ramp_agc > ramp_up > capacity swing fallback
                    let ramp_up_mw = g
                        .ramp_agc_mw_per_min()
                        .or(g.ramp_up_mw_per_min())
                        .map(|rate_mw_per_min| rate_mw_per_min * window)
                        .unwrap_or(g.pmax - g.pmin);

                    let ramp_dn_mw = g
                        .ramp_down_mw_per_min()
                        .or(g.ramp_agc_mw_per_min())
                        .or(g.ramp_up_mw_per_min())
                        .map(|rate_mw_per_min| rate_mw_per_min * window)
                        .unwrap_or(g.pmax - g.pmin);

                    // Clamp by capacity swing too
                    let delta_max = (ramp_up_mw / base).min((g.pmax - g.pmin) / base);
                    let delta_min = -((ramp_dn_mw / base).min((g.pmax - g.pmin) / base));

                    extra_col_lower.push(delta_min);
                    extra_col_upper.push(delta_max);

                    // Marginal cost for corrective redispatch (upward = positive cost)
                    let cost_per_pu =
                        g.cost.as_ref().map(|c| c.linear_coeff()).unwrap_or(0.0) * base;
                    extra_col_cost.push(cost_per_pu);
                }

                // θ_k bounds: [-π, π], with per-island ref buses fixed at 0.
                // If the outaged branch is a bridge (its removal splits an island),
                // we need an additional ref bus in the new sub-island.  Detect this
                // with a quick BFS from each endpoint of the outaged branch,
                // excluding that branch.
                let br_out = &network.branches[outaged_br];
                let from_out = bus_map[&br_out.from_bus];
                let to_out = bus_map[&br_out.to_bus];
                let extra_ref =
                    if island_refs.bus_island[from_out] == island_refs.bus_island[to_out] {
                        // Both endpoints in the same base-case island — check if outage splits it
                        crate::dc::island_lmp::find_split_ref_bus(
                            network,
                            bus_map,
                            &island_refs,
                            from_out,
                            to_out,
                            outaged_br,
                        )
                    } else {
                        None // Endpoints already in different islands — no split possible
                    };
                if let Some(new_ref) = extra_ref {
                    warn!(
                        "Corrective SCOPF: branch outage {} splits island — \
                         fixing additional ref bus {} for new sub-island",
                        outaged_br, new_ref
                    );
                }

                for i in 0..n_bus {
                    let is_ref = island_refs.island_ref_bus.contains(&i) || extra_ref == Some(i);
                    if is_ref {
                        extra_col_lower.push(0.0);
                        extra_col_upper.push(0.0);
                    } else {
                        extra_col_lower.push(-std::f64::consts::PI);
                        extra_col_upper.push(std::f64::consts::PI);
                    }
                    extra_col_cost.push(0.0);
                }

                // ----- Post-contingency constraint rows -----
                // The row offset for the first new row of this block:
                let row_offset_base = base_n_row + extra_row_lower.len();

                // Row A: energy balance  sum_g ΔPg_k[g] = 0
                //   (Equality enforced as lb=ub=0)
                {
                    let row = row_offset_base;
                    for j in 0..n_gen {
                        extra_triplets.push(Triplet {
                            row,
                            col: dpg_col_offset + j,
                            val: 1.0,
                        });
                    }
                    extra_row_lower.push(0.0);
                    extra_row_upper.push(0.0);
                }

                // Row B: post-contingency generator bounds
                //   Pmin/base ≤ Pg⁰[g] + ΔPg_k[g] ≤ Pmax/base
                //   Written as: Pg⁰[g] + ΔPg_k[g] ∈ [pmin/base, pmax/base]
                for j in 0..n_gen {
                    let gi = gen_indices[j];
                    let g = &network.generators[gi];
                    let row = row_offset_base + 1 + j;
                    // Pg⁰[j] + ΔPg_k[j] ∈ [pmin, pmax]
                    extra_triplets.push(Triplet {
                        row,
                        col: pg0_off + j,
                        val: 1.0,
                    });
                    extra_triplets.push(Triplet {
                        row,
                        col: dpg_col_offset + j,
                        val: 1.0,
                    });
                    extra_row_lower.push(g.pmin / base);
                    extra_row_upper.push(g.pmax / base);
                }

                // Row C: post-contingency power balance
                //   B_k * θ_k - A_gen * (Pg⁰ + ΔPg_k) = -Pd
                //   B_k = B_bus minus outaged branch k
                //
                // We expand:
                //   B_k * θ_k - A_gen * Pg⁰ - A_gen * ΔPg_k = -Pd
                //
                // The -A_gen * Pg⁰ coupling terms reference base Pg⁰ columns.
                // The B_k * θ_k terms reference θ_k columns.
                // The -A_gen * ΔPg_k terms reference ΔPg_k columns.
                //
                // B_k = B_bus excluding outaged branch k.
                // For the outaged branch (from_k, to_k) with susceptance b_k:
                //   B_k[from_k, from_k] = B_bus[from_k, from_k] - b_k
                //   B_k[to_k, to_k]     = B_bus[to_k, to_k]     - b_k
                //   B_k[from_k, to_k]   = B_bus[from_k, to_k]   + b_k
                //   B_k[to_k, from_k]   = B_bus[to_k, from_k]   + b_k
                {
                    let row_pb_start = row_offset_base + 1 + n_gen;
                    let br_k = &network.branches[outaged_br];
                    let b_k = br_k.b_dc();
                    let from_k = bus_map[&br_k.from_bus];
                    let to_k = bus_map[&br_k.to_bus];

                    // Build B_k = B_bus minus outaged branch
                    // B_bus contribution (same as base power balance)
                    for branch in &network.branches {
                        if !branch.in_service || branch.x.abs() < 1e-20 {
                            continue;
                        }
                        let from = bus_map[&branch.from_bus];
                        let to = bus_map[&branch.to_bus];
                        let b = branch.b_dc();
                        let row_f = row_pb_start + from;
                        let row_t = row_pb_start + to;
                        extra_triplets.push(Triplet {
                            row: row_f,
                            col: theta_k_col_offset + to,
                            val: -b,
                        });
                        extra_triplets.push(Triplet {
                            row: row_t,
                            col: theta_k_col_offset + from,
                            val: -b,
                        });
                        extra_triplets.push(Triplet {
                            row: row_f,
                            col: theta_k_col_offset + from,
                            val: b,
                        });
                        extra_triplets.push(Triplet {
                            row: row_t,
                            col: theta_k_col_offset + to,
                            val: b,
                        });
                    }
                    // Subtract outaged branch k from B_k (reverse the contributions)
                    if br_k.in_service && br_k.x.abs() >= 1e-20 {
                        let row_f = row_pb_start + from_k;
                        let row_t = row_pb_start + to_k;
                        // B_bus contribution to remove: +b_k at diag, -b_k off-diag
                        extra_triplets.push(Triplet {
                            row: row_f,
                            col: theta_k_col_offset + to_k,
                            val: b_k,
                        });
                        extra_triplets.push(Triplet {
                            row: row_t,
                            col: theta_k_col_offset + from_k,
                            val: b_k,
                        });
                        extra_triplets.push(Triplet {
                            row: row_f,
                            col: theta_k_col_offset + from_k,
                            val: -b_k,
                        });
                        extra_triplets.push(Triplet {
                            row: row_t,
                            col: theta_k_col_offset + to_k,
                            val: -b_k,
                        });
                    }

                    // -A_gen * Pg⁰ coupling (Pg⁰ columns in base variables)
                    for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
                        extra_triplets.push(Triplet {
                            row: row_pb_start + bus_idx,
                            col: pg0_off + j,
                            val: -1.0,
                        });
                    }

                    // -A_gen * ΔPg_k coupling (ΔPg columns in corrective block)
                    for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
                        extra_triplets.push(Triplet {
                            row: row_pb_start + bus_idx,
                            col: dpg_col_offset + j,
                            val: -1.0,
                        });
                    }

                    // Row bounds: B_k*θ_k - A_gen*(Pg⁰+ΔPg_k) = -Pd  (equality)
                    for i in 0..n_bus {
                        let pd_pu = bus_pd_mw[i] / base;
                        extra_row_lower.push(-pd_pu);
                        extra_row_upper.push(-pd_pu);
                    }
                }

                activated_ctg.insert(ctg_idx);
                ctg_blocks.push(CorrectiveCtgBlock {
                    ctg_idx,
                    theta_k_col_offset,
                });

                info!(
                    "Corrective SCOPF: activated block for contingency {} (outage=br{}), total blocks={}",
                    cd.label,
                    outaged_br,
                    ctg_blocks.len()
                );
            }

            // --- Add post-contingency thermal row for monitored branch l ---
            // Find the block for this contingency
            let blk = ctg_blocks
                .iter()
                .find(|b| b.ctg_idx == ctg_idx)
                .expect("ctg block must exist after activation above");

            let br_l = &network.branches[l];
            if br_l.x.abs() < 1e-20 {
                continue;
            }
            let b_l = br_l.b_dc();
            let from_l = bus_map[&br_l.from_bus];
            let to_l = bus_map[&br_l.to_bus];
            let fmax_l = options.contingency_rating.of(&network.branches[l]) / base;

            // Row: Bf_k[l,:] * θ_k - s_up + s_lo ∈ [-fmax_l, fmax_l]
            // Soft constraint via penalty slacks (same pattern as base thermal rows).
            let thermal_row = base_n_row + extra_row_lower.len();
            let s_up_col = base_n_var + extra_col_lower.len();
            let s_lo_col = s_up_col + 1;
            extra_triplets.push(Triplet {
                row: thermal_row,
                col: blk.theta_k_col_offset + from_l,
                val: b_l,
            });
            extra_triplets.push(Triplet {
                row: thermal_row,
                col: blk.theta_k_col_offset + to_l,
                val: -b_l,
            });
            extra_triplets.push(Triplet {
                row: thermal_row,
                col: s_up_col,
                val: -1.0,
            });
            extra_triplets.push(Triplet {
                row: thermal_row,
                col: s_lo_col,
                val: 1.0,
            });
            // Slack variable column data
            extra_col_lower.extend_from_slice(&[0.0, 0.0]);
            extra_col_upper.extend_from_slice(&[f64::INFINITY, f64::INFINITY]);
            extra_col_cost.extend_from_slice(&[thermal_penalty_per_pu, thermal_penalty_per_pu]);
            extra_row_lower.push(-fmax_l);
            extra_row_upper.push(fmax_l);
        }

        scopf_iter += 1;
        info!(
            "Corrective SCOPF iteration {}: added {} violations ({} blocks, {} thermal rows)",
            scopf_iter,
            n_add,
            ctg_blocks.len(),
            thermal_pairs.len()
        );

        if scopf_iter >= options.max_iterations {
            return Err(DcOpfError::NotConverged {
                iterations: scopf_iter,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::test_util::case_path;

    use super::*;
    use crate::dc::opf::DcOpfOptions;
    use surge_network::network::Contingency;
    use surge_solution::OpfSolution;

    /// Convenience wrapper: calls `solve_dc_preventive_with_context` with a default context.
    fn solve_dc_preventive(
        network: &Network,
        options: &ScopfOptions,
    ) -> Result<ScopfResult, DcOpfError> {
        solve_dc_preventive_with_context(network, options, &ScopfRunContext::default())
    }

    fn exact_cost_scopf_options() -> ScopfOptions {
        ScopfOptions {
            dc_opf: DcOpfOptions {
                use_pwl_costs: false,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn assert_lmp_decomposition(opf: &OpfSolution) {
        for i in 0..opf.pricing.lmp.len() {
            let recomposed =
                opf.pricing.lmp_energy[i] + opf.pricing.lmp_congestion[i] + opf.pricing.lmp_loss[i];
            assert!(
                (opf.pricing.lmp[i] - recomposed).abs() < 1e-8,
                "bus {} LMP decomposition mismatch: {:.8} != {:.8} + {:.8} + {:.8}",
                i,
                opf.pricing.lmp[i],
                opf.pricing.lmp_energy[i],
                opf.pricing.lmp_congestion[i],
                opf.pricing.lmp_loss[i]
            );
        }
    }

    #[test]
    fn test_scopf_cost_geq_dcopf_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let dcopf_opts = DcOpfOptions {
            use_pwl_costs: false,
            ..Default::default()
        };
        let dcopf_sol = crate::dc::opf::solve_dc_opf(&net, &dcopf_opts).unwrap().opf;

        let scopf_opts = exact_cost_scopf_options();
        let scopf_sol = solve_dc_preventive(&net, &scopf_opts).unwrap();

        assert!(
            scopf_sol.base_opf.total_cost >= dcopf_sol.total_cost - 0.01,
            "SCOPF cost ({:.2}) should be >= DC-OPF cost ({:.2})",
            scopf_sol.base_opf.total_cost,
            dcopf_sol.total_cost
        );

        println!(
            "case9: DC-OPF cost={:.2}, SCOPF cost={:.2}, iters={}",
            dcopf_sol.total_cost, scopf_sol.base_opf.total_cost, scopf_sol.iterations
        );
    }

    #[test]
    fn test_scopf_no_violations_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let scopf_opts = ScopfOptions::default();
        let scopf_sol = solve_dc_preventive(&net, &scopf_opts).unwrap();

        // Verify no post-contingency violations in final dispatch
        let all_br: Vec<usize> = (0..net.n_branches()).collect();
        let ptdf =
            surge_dc::compute_ptdf(&net, &surge_dc::PtdfRequest::for_branches(&all_br)).unwrap();
        let base = net.base_mva;
        let bus_map = net.bus_index_map();
        let n_bus = net.n_buses();
        let n_br = net.n_branches();

        let bus_pd_mw = net.bus_load_p_mw();
        let mut net_inject = vec![0.0; n_bus];
        for i in 0..n_bus {
            net_inject[i] = -bus_pd_mw[i] / base;
        }
        let gen_indices: Vec<usize> = net
            .generators
            .iter()
            .enumerate()
            .filter(|(_, g)| g.in_service)
            .map(|(i, _)| i)
            .collect();
        for (j, &gi) in gen_indices.iter().enumerate() {
            let bus_idx = bus_map[&net.generators[gi].bus];
            net_inject[bus_idx] += scopf_sol.base_opf.generators.gen_p_mw[j] / base;
        }

        let mut base_flow = vec![0.0; n_br];
        for l in 0..n_br {
            for k in 0..n_bus {
                base_flow[l] += get_ptdf(&ptdf, l, k) * net_inject[k];
            }
        }

        let contingencies = generate_n1_branch_contingencies(&net);
        for ctg in &contingencies {
            if ctg.branch_indices.len() != 1 {
                continue;
            }
            let outaged_br = ctg.branch_indices[0];
            let br_k = &net.branches[outaged_br];
            if !br_k.in_service {
                continue;
            }

            let from_idx = bus_map[&br_k.from_bus];
            let to_idx = bus_map[&br_k.to_bus];
            let ptdf_diff_k =
                get_ptdf(&ptdf, outaged_br, from_idx) - get_ptdf(&ptdf, outaged_br, to_idx);
            let denom = 1.0 - ptdf_diff_k;
            if denom.abs() < 1e-10 {
                continue;
            }

            for l in 0..n_br {
                if l == outaged_br
                    || !net.branches[l].in_service
                    || net.branches[l].rating_a_mva < 1.0
                {
                    continue;
                }
                let ptdf_diff_l = get_ptdf(&ptdf, l, from_idx) - get_ptdf(&ptdf, l, to_idx);
                let lodf_lk = ptdf_diff_l / denom;
                if !lodf_lk.is_finite() {
                    continue;
                }

                let post_flow = base_flow[l] + lodf_lk * base_flow[outaged_br];
                let f_max = net.branches[l].rating_a_mva / base;

                assert!(
                    post_flow.abs() <= f_max + 0.02,
                    "Violation: ctg={}, branch {}, flow={:.4} > limit={:.4}",
                    ctg.label,
                    l,
                    post_flow.abs(),
                    f_max
                );
            }
        }
    }

    #[test]
    fn test_scopf_lmp_decomposition_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let scopf_sol = solve_dc_preventive(&net, &ScopfOptions::default()).unwrap();

        let n_bus = net.n_buses();
        let lmp = &scopf_sol.base_opf.pricing.lmp;
        let congestion = &scopf_sol.base_opf.pricing.lmp_congestion;
        let loss = &scopf_sol.base_opf.pricing.lmp_loss;

        // Energy component should be uniform across all buses
        let energy_0 = lmp[0] - congestion[0] - loss[0];
        for i in 0..n_bus {
            let energy = lmp[i] - congestion[i] - loss[i];
            assert!(
                (energy - energy_0).abs() < 0.01,
                "Energy should be uniform: bus {} has {:.4}, bus 0 has {:.4}",
                i,
                energy,
                energy_0
            );
        }

        // LMPs should be positive
        for (i, &l) in lmp.iter().enumerate() {
            assert!(l > 0.0, "LMP at bus {} should be positive: {:.4}", i, l);
        }
    }

    #[test]
    fn test_scopf_no_limits_equals_dcopf() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let dcopf_opts = DcOpfOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let dcopf_sol = crate::dc::opf::solve_dc_opf(&net, &dcopf_opts).unwrap().opf;

        let scopf_opts = ScopfOptions {
            dc_opf: DcOpfOptions {
                enforce_thermal_limits: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let scopf_sol = solve_dc_preventive(&net, &scopf_opts).unwrap();

        assert!(
            (scopf_sol.base_opf.total_cost - dcopf_sol.total_cost).abs() < 0.1,
            "Without limits: SCOPF={:.2} should equal DC-OPF={:.2}",
            scopf_sol.base_opf.total_cost,
            dcopf_sol.total_cost
        );

        assert_eq!(scopf_sol.iterations, 1);
        assert_eq!(scopf_sol.total_contingency_constraints, 0);
    }

    #[test]
    fn test_scopf_custom_contingencies() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let custom_ctgs: Vec<Contingency> = net
            .branches
            .iter()
            .enumerate()
            .take(3)
            .filter(|(_, br)| br.in_service)
            .map(|(i, br)| Contingency {
                id: format!("custom_{i}"),
                label: format!("Custom {}->{}(ckt {})", br.from_bus, br.to_bus, br.circuit),
                branch_indices: vec![i],
                ..Default::default()
            })
            .collect();

        let scopf_opts = ScopfOptions {
            contingencies: Some(custom_ctgs),
            ..Default::default()
        };
        let scopf_sol = solve_dc_preventive(&net, &scopf_opts).unwrap();

        assert!(scopf_sol.base_opf.total_cost > 0.0);
        let total_gen: f64 = scopf_sol.base_opf.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 0.1,
            "power balance violated"
        );
    }

    // =========================================================================
    // PLAN-095 / P5-061 — ScopfScreeningPolicy unit tests
    // =========================================================================

    /// PLAN-095-T1: Pre-screener with an artificially stressed 6-bus network.
    ///
    /// We construct a 6-bus ring with one heavily pre-loaded branch and verify
    /// that the screener correctly identifies its outage as likely-binding.
    ///
    /// Topology:
    ///   Bus 1 (slack) — Bus 2 — Bus 3
    ///   Bus 1 — Bus 4 — Bus 5 — Bus 3
    ///   Bus 2 — Bus 6 — Bus 3   (the heavily loaded branch is 2→6)
    ///
    /// We give branch 2→6 a very tight thermal limit (1 MVA) so that after any
    /// neighbouring line is outaged, its post-contingency loading estimate easily
    /// exceeds 90 % of rating, triggering pre-screening.
    #[test]
    fn test_screener_identifies_loaded_branch_6bus() {
        // PLAN-095-T1: Verify pre-screener flags a tight branch in a 6-bus network.
        //
        // Topology (two parallel paths from bus 1 to bus 3):
        //   Path A: 1 -> 2 -> 3  (generous 200 MVA limits)
        //   Path B: 1 -> 3       (tight  10 MVA limit, branch index 2)
        //   Load:   bus 3 = 15 MW (small enough that N-1 is feasible via path A)
        //
        // The tight branch (1->3, 10 MVA) will be loaded at:
        //   base-case flow ≈ 50 % of its own limit (via parallel path sharing)
        //   post-contingency when 1->2 is outaged: all 15 MW must go via 1->3
        //   => post loading = 15 MW >> 10 MVA limit => screener flags it at any threshold
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let base_mva = 100.0;

        // Buses
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);

        // Branches
        // Index 0: 1->2 (generous limit, low impedance)
        let mut br_1_2 = Branch::new_line(1, 2, 0.0, 0.05, 0.0);
        br_1_2.rating_a_mva = 200.0;
        // Index 1: 2->3 (generous limit)
        let mut br_2_3 = Branch::new_line(2, 3, 0.0, 0.05, 0.0);
        br_2_3.rating_a_mva = 200.0;
        // Index 2: 1->3 (tight limit — higher impedance parallel path)
        let mut br_1_3 = Branch::new_line(1, 3, 0.0, 0.20, 0.0);
        br_1_3.rating_a_mva = 10.0; // tight: 10 MVA

        // Generator at slack bus 1
        let mut generator = Generator::new(1, 8.0, 1.0);
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let network = Network {
            name: "test_6bus_screener".to_string(),
            base_mva,
            freq_hz: 60.0,
            buses: vec![b1, b2, b3],
            branches: vec![br_1_2, br_2_3, br_1_3],
            generators: vec![generator],
            loads: vec![surge_network::network::Load::new(3, 8.0, 0.0)],
            controls: Default::default(),
            market_data: Default::default(),
            ..Default::default()
        };

        // Run SCOPF with pre-screening at 50% threshold
        let opts = ScopfOptions {
            screener: Some(ScopfScreeningPolicy {
                threshold_fraction: 0.5, // 50% -- catches anything > 5 MW on the 10 MVA branch
                max_initial_contingencies: 500,
            }),
            ..Default::default()
        };

        let sol = solve_dc_preventive(&network, &opts).unwrap();

        // Screener must have evaluated pairs
        assert!(
            sol.screening_stats.pairs_evaluated > 0,
            "screener should have evaluated at least one (monitored, contingency) pair"
        );

        // With 8 MW load and a 10 MVA limit on branch 1->3 (index 2):
        // - base-case load sharing: ~1.6 MW through 1->3 (~16% of limit)
        // - when branch 1->2 is outaged: all 8 MW flows through 1->3 (80% > 50% threshold)
        // So the screener must flag the (1->2 outage, monitor 1->3) pair.
        // The screener must pre-load at least one constraint.
        assert!(
            sol.screening_stats.pre_screened_constraints > 0,
            "screener should have pre-loaded at least one constraint for the tight branch              (br_1_3, rate_a=10 MVA); pre_screened={}",
            sol.screening_stats.pre_screened_constraints
        );

        // Solution must be feasible and power-balanced
        let total_gen: f64 = sol.base_opf.generators.gen_p_mw.iter().sum();
        let total_load: f64 = network.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 1.0,
            "power balance violated: gen={total_gen:.2}, load={total_load:.2}"
        );

        println!(
            "6-bus screener test: {} pairs evaluated, {} pre-screened,              {} cutting-plane constraints, {} iterations",
            sol.screening_stats.pairs_evaluated,
            sol.screening_stats.pre_screened_constraints,
            sol.screening_stats.cutting_plane_constraints,
            sol.iterations
        );
    }

    #[test]
    fn test_screener_does_not_change_optimal_cost_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let opts_no_screen = ScopfOptions {
            screener: None,
            ..Default::default()
        };
        let opts_screened = ScopfOptions {
            screener: Some(ScopfScreeningPolicy {
                threshold_fraction: 0.8,
                max_initial_contingencies: 200,
            }),
            ..Default::default()
        };

        let sol_no_screen = solve_dc_preventive(&net, &opts_no_screen).unwrap();
        let sol_screened = solve_dc_preventive(&net, &opts_screened).unwrap();

        // Optimal costs must be equal (within 0.1 $/hr rounding)
        assert!(
            (sol_screened.base_opf.total_cost - sol_no_screen.base_opf.total_cost).abs() < 0.5,
            "Screened SCOPF cost ({:.2}) should match unscreened ({:.2})",
            sol_screened.base_opf.total_cost,
            sol_no_screen.base_opf.total_cost
        );

        // Screened run should use at most as many iterations as unscreened
        assert!(
            sol_screened.iterations <= sol_no_screen.iterations + 1,
            "Screened SCOPF should not need more iterations than unscreened: \
             screened={}, unscreened={}",
            sol_screened.iterations,
            sol_no_screen.iterations
        );

        // Screened stats must be populated
        assert_eq!(
            sol_screened.screening_stats.threshold_fraction, 0.8,
            "threshold_fraction should match what was set"
        );
        // No-screening stats should show zero pairs evaluated
        assert_eq!(
            sol_no_screen.screening_stats.pairs_evaluated, 0,
            "disabled screener should report 0 pairs evaluated"
        );

        println!(
            "case9 screener comparison: \
             screened={} iters ({} pre-screened + {} cutting-plane) vs \
             unscreened={} iters ({} cutting-plane)",
            sol_screened.iterations,
            sol_screened.screening_stats.pre_screened_constraints,
            sol_screened.screening_stats.cutting_plane_constraints,
            sol_no_screen.iterations,
            sol_no_screen.screening_stats.cutting_plane_constraints
        );
    }

    /// PLAN-095-T3: Pre-screening on case118 — verify screening_stats are consistent.
    ///
    /// total_contingency_constraints == pre_screened_constraints + cutting_plane_constraints
    #[test]
    fn test_screener_stats_consistency_case118() {
        let net = surge_io::load(case_path("case118")).unwrap();

        let opts = ScopfOptions {
            screener: Some(ScopfScreeningPolicy {
                threshold_fraction: 0.9,
                max_initial_contingencies: 500,
            }),
            ..Default::default()
        };
        let sol =
            solve_dc_preventive_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();

        let stats = &sol.screening_stats;
        assert_eq!(
            sol.total_contingency_constraints,
            stats.pre_screened_constraints + stats.cutting_plane_constraints,
            "total_contingency_constraints ({}) must equal pre_screened ({}) + cutting_plane ({})",
            sol.total_contingency_constraints,
            stats.pre_screened_constraints,
            stats.cutting_plane_constraints
        );

        assert_eq!(
            stats.threshold_fraction, 0.9,
            "threshold_fraction must match configuration"
        );

        println!(
            "case118 screening: {} pairs eval, {} pre-screened, {} cutting-plane, \
             {} total cuts, {} iterations",
            stats.pairs_evaluated,
            stats.pre_screened_constraints,
            stats.cutting_plane_constraints,
            sol.total_contingency_constraints,
            sol.iterations
        );
    }
    /// P5-B10: Warm-start SCOPF with active cuts from a prior solve.
    ///
    /// Verifies that:
    /// 1. A cold SCOPF solve produces a valid solution.
    /// 2. A warm-start solve with active_cuts from the cold solve also converges.
    /// 3. The warm-start objective matches the cold solve (same network).
    /// 4. Warm-start does not require more iterations than cold.
    #[test]
    fn test_scopf_warm_start_active_cuts() {
        let net = surge_io::load(case_path("case9")).unwrap();

        // Cold solve (no warm-start, no pre-screening for determinism).
        let cold_opts = ScopfOptions {
            screener: None,
            ..Default::default()
        };
        let cold_sol = solve_dc_preventive(&net, &cold_opts).unwrap();

        // Build warm-start from cold solve.
        let base_pg = cold_sol.base_opf.generators.gen_p_mw.clone();
        let base_vm = cold_sol.base_opf.power_flow.voltage_magnitude_pu.clone();
        let active_cuts: Vec<ScopfWarmStartCut> = cold_sol
            .binding_contingencies
            .iter()
            .map(|binding| ScopfWarmStartCut {
                cut_kind: binding.cut_kind,
                outaged_branch_indices: binding.outaged_branch_indices.clone(),
                outaged_generator_indices: binding.outaged_generator_indices.clone(),
                monitored_branch_idx: binding.monitored_branch_idx,
            })
            .collect();

        let warm_opts = ScopfOptions {
            screener: None,
            ..Default::default()
        };
        let warm_runtime = ScopfRuntime::default().with_warm_start(ScopfWarmStart {
            base_pg: base_pg.clone(),
            base_vm: base_vm.clone(),
            active_cuts,
        });
        let warm_sol =
            crate::security::solve_scopf_with_runtime(&net, &warm_opts, &warm_runtime).unwrap();

        let cold_cost = cold_sol.base_opf.total_cost;
        let warm_cost = warm_sol.base_opf.total_cost;
        let rel_diff = (warm_cost - cold_cost).abs() / cold_cost.abs().max(1.0);
        assert!(
            rel_diff < 0.01,
            "warm cost {warm_cost:.2} vs cold {cold_cost:.2} (rel {rel_diff:.4})"
        );
        assert!(
            warm_sol.iterations <= cold_sol.iterations + 1,
            "warm {} iters > cold {} iters + 1",
            warm_sol.iterations,
            cold_sol.iterations
        );
    }

    // =========================================================================
    // Corrective SCOPF tests
    // =========================================================================

    /// Corrective SCOPF cost must be <= preventive SCOPF cost (case5_pjm).
    ///
    /// Corrective SCOPF gives generators post-contingency flexibility, so the
    /// pre-contingency dispatch can be cheaper. The corrective cost must be
    /// at or below the preventive cost.
    #[test]
    fn test_corrective_scopf_cost_leq_preventive_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let opts = exact_cost_scopf_options();
        let preventive =
            solve_dc_preventive_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();
        let corrective =
            solve_dc_corrective_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();

        assert!(
            corrective.base_opf.total_cost <= preventive.base_opf.total_cost + 0.5,
            "Corrective SCOPF cost ({:.2}) should be <= preventive ({:.2})",
            corrective.base_opf.total_cost,
            preventive.base_opf.total_cost
        );

        println!(
            "case9: preventive={:.2}, corrective={:.2}",
            preventive.base_opf.total_cost, corrective.base_opf.total_cost
        );
    }

    /// Corrective SCOPF power balance must hold (case9).
    #[test]
    fn test_corrective_scopf_power_balance_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = ScopfOptions::default();
        let sol =
            solve_dc_corrective_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();

        let total_gen: f64 = sol.base_opf.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 0.5,
            "power balance: gen={total_gen:.2}, load={total_load:.2}"
        );
        assert!(sol.base_opf.total_cost > 0.0);
    }

    /// Corrective SCOPF cost must be >= DC-OPF cost (relaxation lower bound).
    #[test]
    fn test_corrective_scopf_cost_geq_dcopf_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let dcopf_sol = crate::dc::opf::solve_dc_opf(
            &net,
            &DcOpfOptions {
                use_pwl_costs: false,
                ..Default::default()
            },
        )
        .unwrap()
        .opf;
        let corr_sol = solve_dc_corrective_with_context(
            &net,
            &exact_cost_scopf_options(),
            &ScopfRunContext::default(),
        )
        .unwrap();

        assert!(
            corr_sol.base_opf.total_cost >= dcopf_sol.total_cost - 0.5,
            "Corrective SCOPF cost ({:.2}) should be >= DC-OPF ({:.2})",
            corr_sol.base_opf.total_cost,
            dcopf_sol.total_cost
        );
    }

    /// Regression test for C-02: base-case branch flow must use b_dc() * (θ_from - θ_to - shift_rad)
    /// rather than the incorrect (θ_from - θ_to) / x.
    ///
    /// We build a small 3-bus network (identical topology to case9 bus 1-2-3 triangle)
    /// but replace one branch with a transformer that has tap=0.95 and shift=5.0 degrees.
    /// Before the fix, the SCOPF violation-check loop would compute an incorrect base-case
    /// flow for that branch, potentially triggering spurious cuts or missing real violations.
    /// The test just verifies that the solver converges to Optimal without panicking.
    #[test]
    fn test_scopf_with_tap_transformer() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let base_mva = 100.0;

        // 3-bus triangle: bus 1 (slack) — bus 2 — bus 3, with 1->3 as a transformer
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);

        // Branch 0: 1->2 plain line
        let mut br_1_2 = Branch::new_line(1, 2, 0.01, 0.085, 0.176);
        br_1_2.rating_a_mva = 250.0;

        // Branch 1: 2->3 plain line
        let mut br_2_3 = Branch::new_line(2, 3, 0.017, 0.092, 0.158);
        br_2_3.rating_a_mva = 250.0;

        // Branch 2: 1->3 transformer with off-nominal tap and phase shift
        let mut br_1_3 = Branch::new_line(1, 3, 0.0085, 0.072, 0.149);
        br_1_3.tap = 0.95;
        br_1_3.phase_shift_rad = 5.0_f64.to_radians();
        br_1_3.rating_a_mva = 150.0;

        // Generator at bus 1: covers all 100 MW load
        let mut g1 = Generator::new(1, 100.0, 1.05);
        g1.pmin = 0.0;
        g1.pmax = 250.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 20.0, 0.01],
        });

        // Second generator at bus 2 for redundancy
        let mut g2 = Generator::new(2, 50.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 150.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 25.0, 0.015],
        });

        let network = Network {
            name: "test_scopf_tap_transformer".to_string(),
            base_mva,
            freq_hz: 60.0,
            buses: vec![b1, b2, b3],
            branches: vec![br_1_2, br_2_3, br_1_3],
            generators: vec![g1, g2],
            loads: vec![
                surge_network::network::Load::new(2, 50.0, 0.0),
                surge_network::network::Load::new(3, 50.0, 0.0),
            ],
            controls: Default::default(),
            market_data: Default::default(),
            ..Default::default()
        };

        let opts = ScopfOptions::default();
        let sol = solve_dc_preventive(&network, &opts)
            .expect("SCOPF with tap transformer should converge to Optimal");

        assert!(
            sol.base_opf.total_cost > 0.0,
            "SCOPF cost must be positive: {}",
            sol.base_opf.total_cost
        );

        let total_gen: f64 = sol.base_opf.generators.gen_p_mw.iter().sum();
        let total_load: f64 = network.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 0.5,
            "power balance violated: gen={total_gen:.2}, load={total_load:.2}"
        );

        println!(
            "tap-transformer SCOPF: cost={:.2}, iters={}, cuts={}",
            sol.base_opf.total_cost, sol.iterations, sol.total_contingency_constraints
        );
    }

    /// Corrective SCOPF cost must be <= preventive SCOPF cost (case118).
    #[test]
    fn test_corrective_scopf_leq_preventive_case118() {
        let net = surge_io::load(case_path("case118")).unwrap();

        let opts = exact_cost_scopf_options();
        let preventive =
            solve_dc_preventive_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();
        let corrective =
            solve_dc_corrective_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();

        assert!(
            corrective.base_opf.total_cost <= preventive.base_opf.total_cost + 1.0,
            "Corrective cost ({:.2}) should be <= preventive ({:.2}) for case118",
            corrective.base_opf.total_cost,
            preventive.base_opf.total_cost
        );

        println!(
            "case118: preventive={:.2}, corrective={:.2}, iters={}",
            preventive.base_opf.total_cost, corrective.base_opf.total_cost, corrective.iterations
        );
    }

    #[test]
    fn test_scopf_hvdc_converter_trip() {
        // Build a simple 3-bus network with a branch that has a thermal limit,
        // and inject an HVDC contingency.  When the HVDC converter is tripped,
        // the injection at converter buses is removed, potentially causing
        // overloads on monitored branches.
        use surge_network::market::CostCurve;
        use surge_network::network::Branch;
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("scopf-hvdc-test");
        net.base_mva = 100.0;

        // Bus 1: Slack
        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.0;
        net.buses.push(b1);

        // Bus 2: PQ load
        let b2 = Bus::new(2, BusType::PQ, 230.0);
        net.buses.push(b2);

        // Bus 3: PQ (HVDC converter bus)
        let b3 = Bus::new(3, BusType::PQ, 230.0);
        net.buses.push(b3);

        // Branches with thermal limits
        let mut br12 = Branch::new_line(1, 2, 0.01, 0.05, 0.02);
        br12.rating_a_mva = 100.0;
        net.branches.push(br12);

        let mut br13 = Branch::new_line(1, 3, 0.02, 0.06, 0.03);
        br13.rating_a_mva = 50.0;
        net.branches.push(br13);

        let mut br23 = Branch::new_line(2, 3, 0.015, 0.04, 0.02);
        br23.rating_a_mva = 60.0;
        net.branches.push(br23);

        // Generator at bus 1
        let mut g1 = Generator::new(1, 100.0, 1.0);
        g1.pmax = 200.0;
        g1.pmin = 0.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.02, 20.0, 0.0],
        });
        net.generators.push(g1);

        // Load at bus 2
        net.loads.push(Load::new(2, 80.0, 0.0));

        // Add a VSC HVDC link injecting 30 MW at bus 3 (from external bus 1).
        // This simulates an HVDC import at bus 3.
        use surge_network::network::{VscConverterTerminal, VscHvdcControlMode, VscHvdcLink};
        net.hvdc.push_vsc_link(VscHvdcLink {
            name: "VSC-scopf-test".to_string(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.5,
            converter1: VscConverterTerminal {
                bus: 1,
                dc_setpoint: 30.0,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 3,
                ..VscConverterTerminal::default()
            },
        });

        // Create an HVDC converter trip contingency.
        let hvdc_ctg = Contingency {
            id: "hvdc_conv_0".into(),
            label: "Trip HVDC converter 0".into(),
            hvdc_converter_indices: vec![0],
            ..Default::default()
        };

        let scopf_opts = ScopfOptions {
            contingencies: Some(vec![hvdc_ctg]),
            ..Default::default()
        };

        // The SCOPF should succeed (the HVDC contingency may or may not produce
        // violations depending on flow patterns, but the solver should handle it).
        let result = solve_dc_preventive(&net, &scopf_opts);
        assert!(
            result.is_ok(),
            "SCOPF with HVDC contingency failed: {result:?}"
        );

        let sol = result.unwrap();
        assert!(sol.base_opf.total_cost > 0.0, "cost should be positive");
    }

    // ---------------------------------------------------------------------------
    // Corrective SCOPF ramp-rate and cost tests (no external data required)
    // ---------------------------------------------------------------------------

    /// Build a small 3-bus, 2-generator network for ramp-rate / cost tests.
    ///
    /// Bus 1 (slack, 100 MVA base) — Bus 2 — Bus 3
    ///    Gen 1 @ bus 1: pmin=0, pmax=300 MW
    ///    Gen 2 @ bus 2: pmin=0, pmax=200 MW
    ///    Load: 100 MW at bus 3
    fn make_ramp_test_network() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};
        let base_mva = 100.0;

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PV, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);

        let mut br_1_2 = Branch::new_line(1, 2, 0.01, 0.085, 0.0);
        br_1_2.rating_a_mva = 250.0;
        let mut br_2_3 = Branch::new_line(2, 3, 0.017, 0.092, 0.0);
        br_2_3.rating_a_mva = 250.0;
        let mut br_1_3 = Branch::new_line(1, 3, 0.0085, 0.072, 0.0);
        br_1_3.rating_a_mva = 250.0;

        let mut g1 = Generator::new(1, 80.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 300.0;
        // f(P) = 0.01*P^2 + 20*P  → linear coeff = 20 $/MWh
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.01, 20.0, 0.0],
        });

        let mut g2 = Generator::new(2, 20.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 200.0;
        // f(P) = 0.015*P^2 + 30*P  → linear coeff = 30 $/MWh (more expensive)
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.015, 30.0, 0.0],
        });

        Network {
            name: "ramp_test_network".to_string(),
            base_mva,
            freq_hz: 60.0,
            buses: vec![b1, b2, b3],
            branches: vec![br_1_2, br_2_3, br_1_3],
            generators: vec![g1, g2],
            loads: vec![surge_network::network::Load::new(3, 100.0, 0.0)],
            controls: Default::default(),
            market_data: Default::default(),
            ..Default::default()
        }
    }

    /// Verify that setting ramp_up_curve limits corrective ΔPg column bounds.
    ///
    /// Generator 0 has ramp_up = 5.0 MW/min.  With a 10-minute window the
    /// maximum corrective redispatch is 50 MW = 0.50 p.u. (base_mva = 100).
    /// Without ramp limits the bound would be (pmax-pmin)/base = 3.0 p.u.
    #[test]
    fn test_corrective_scopf_respects_ramp_limits() {
        let mut net = make_ramp_test_network();
        // Set ramp_up = 5.0 MW/min for generator 0
        net.generators[0]
            .ramping
            .get_or_insert_with(Default::default)
            .ramp_up_curve = vec![(0.0, 5.0)]; // 5 MW/min × 10 min = 50 MW budget

        let base = net.base_mva;
        let g = &net.generators[0];
        let window = 10.0_f64;

        // Compute what the corrective bounds should be
        let ramp_up_mw = g
            .ramp_agc_mw_per_min()
            .or(g.ramp_up_mw_per_min())
            .map(|rate| rate * window)
            .unwrap_or(g.pmax - g.pmin);
        let delta_max = (ramp_up_mw / base).min((g.pmax - g.pmin) / base);

        // 50 MW / 100 MVA = 0.50 p.u. — well below (300-0)/100 = 3.0 p.u.
        assert!(
            (delta_max - 0.50).abs() < 1e-10,
            "expected delta_max = 0.50 p.u., got {delta_max}"
        );
        assert!(
            delta_max < (g.pmax - g.pmin) / base,
            "ramp-limited bound should be tighter than capacity swing"
        );

        // Also verify the solver runs successfully with ramp limits applied
        let opts = ScopfOptions {
            corrective: ScopfCorrectiveSettings {
                ramp_window_min: 10.0,
            },
            screener: None,
            ..Default::default()
        };
        let result = solve_dc_corrective_with_context(&net, &opts, &ScopfRunContext::default());
        assert!(
            result.is_ok(),
            "corrective SCOPF with ramp limits failed: {result:?}"
        );
    }

    /// Verify that the no-ramp-data fallback uses capacity-swing bounds.
    ///
    /// When all ramp curves are empty the corrective redispatch
    /// bound must equal (pmax - pmin) / base (full capacity swing).
    #[test]
    fn test_corrective_scopf_no_ramp_data_fallback() {
        let net = make_ramp_test_network();
        let base = net.base_mva;
        let g = &net.generators[0]; // all ramp curves empty

        assert!(g.ramp_agc_mw_per_min().is_none());
        assert!(g.ramp_up_mw_per_min().is_none());
        assert!(g.ramp_down_mw_per_min().is_none());

        let window = 10.0_f64;
        let ramp_up_mw = g
            .ramp_agc_mw_per_min()
            .or(g.ramp_up_mw_per_min())
            .map(|rate| rate * window)
            .unwrap_or(g.pmax - g.pmin);
        let delta_max = (ramp_up_mw / base).min((g.pmax - g.pmin) / base);

        let expected = (g.pmax - g.pmin) / base;
        assert!(
            (delta_max - expected).abs() < 1e-10,
            "fallback delta_max should equal (pmax-pmin)/base = {expected}, got {delta_max}"
        );

        // Solver should also succeed
        let opts = ScopfOptions {
            screener: None,
            ..Default::default()
        };
        let result = solve_dc_corrective_with_context(&net, &opts, &ScopfRunContext::default());
        assert!(
            result.is_ok(),
            "corrective SCOPF fallback failed: {result:?}"
        );
    }

    /// Verify that marginal cost signal is correctly assigned to corrective ΔPg columns.
    ///
    /// Two generators with the same ramp rate but different linear marginal costs.
    /// We verify that linear_coeff() returns the correct per-unit cost coefficient
    /// and that the LP objective for the cheaper generator's column cost is lower.
    #[test]
    fn test_corrective_scopf_ramp_cost_orders_redispatch() {
        use surge_network::market::CostCurve;
        let net = make_ramp_test_network();
        let base = net.base_mva;

        // g1: linear coefficient = 20 $/MWh → cost per p.u. = 20 * 100 = 2000 $/pu/hr
        // g2: linear coefficient = 30 $/MWh → cost per p.u. = 30 * 100 = 3000 $/pu/hr
        let g1 = &net.generators[0];
        let g2 = &net.generators[1];

        let cost1 = g1
            .cost
            .as_ref()
            .map(|c| c.linear_coeff() * base)
            .unwrap_or(0.0);
        let cost2 = g2
            .cost
            .as_ref()
            .map(|c| c.linear_coeff() * base)
            .unwrap_or(0.0);

        assert!(
            (cost1 - 20.0 * base).abs() < 1e-6,
            "g1 corrective cost should be 20 * {base} = {}, got {cost1}",
            20.0 * base
        );
        assert!(
            (cost2 - 30.0 * base).abs() < 1e-6,
            "g2 corrective cost should be 30 * {base} = {}, got {cost2}",
            30.0 * base
        );
        assert!(
            cost1 < cost2,
            "cheaper generator g1 ({cost1}) should have lower corrective cost than g2 ({cost2})"
        );

        // Also verify with PiecewiseLinear cost
        let pwl_cost = CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            // First segment slope = (500-0)/(50-0) = 10 $/MWh
            points: vec![(0.0, 0.0), (50.0, 500.0), (200.0, 4500.0)],
        };
        assert!(
            (pwl_cost.linear_coeff() - 10.0).abs() < 1e-10,
            "PWL linear_coeff should return first-segment slope = 10.0"
        );

        // Solver should succeed
        let opts = ScopfOptions {
            screener: None,
            ..Default::default()
        };
        let result = solve_dc_corrective_with_context(&net, &opts, &ScopfRunContext::default());
        assert!(
            result.is_ok(),
            "corrective SCOPF cost test failed: {result:?}"
        );
    }

    /// ThermalRating::RateB uses branch.rating_b_mva for contingency limits,
    /// producing fewer violations when rate_b > rate_a.
    #[test]
    fn test_scopf_contingency_rate_b() {
        let mut net = surge_io::load(case_path("case9")).unwrap();
        // Set rate_b > rate_a on all branches so contingencies are less constrained
        for br in &mut net.branches {
            br.rating_b_mva = br.rating_a_mva * 1.5;
        }

        let opts_a = ScopfOptions {
            contingency_rating: ThermalRating::RateA,
            screener: None,
            ..Default::default()
        };
        let opts_b = ScopfOptions {
            contingency_rating: ThermalRating::RateB,
            screener: None,
            ..Default::default()
        };

        let result_a = solve_dc_preventive(&net, &opts_a).unwrap();
        let result_b = solve_dc_preventive(&net, &opts_b).unwrap();

        // RateB is more relaxed, so cost should be <= RateA cost
        assert!(
            result_b.base_opf.total_cost <= result_a.base_opf.total_cost + 1e-2,
            "RateB cost ({}) should be <= RateA cost ({})",
            result_b.base_opf.total_cost,
            result_a.base_opf.total_cost
        );
    }

    /// Generator contingency creates PTDF-based cuts in DC-SCOPF.
    #[test]
    fn test_scopf_gen_contingency() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PV, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 200.0;
        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 200.0;
        let mut br13 = Branch::new_line(1, 3, 0.0, 0.1, 0.0);
        br13.rating_a_mva = 200.0;

        let mut g1 = Generator::new(1, 60.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            coeffs: vec![20.0, 0.0],
            startup: 0.0,
            shutdown: 0.0,
        });

        let mut g2 = Generator::new(2, 40.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 200.0;
        g2.cost = Some(CostCurve::Polynomial {
            coeffs: vec![40.0, 0.0],
            startup: 0.0,
            shutdown: 0.0,
        });

        let net = Network {
            name: "test_gen_ctg".into(),
            base_mva: 100.0,
            freq_hz: 60.0,
            buses: vec![b1, b2, b3],
            branches: vec![br12, br23, br13],
            generators: vec![g1, g2],
            loads: vec![
                surge_network::network::Load::new(2, 50.0, 0.0),
                surge_network::network::Load::new(3, 50.0, 0.0),
            ],
            ..Default::default()
        };

        // Add a generator contingency (trip gen at bus 2)
        let gen_ctg = Contingency {
            id: "gen_trip_1".into(),
            label: "Trip gen at bus 2".into(),
            generator_indices: vec![1], // gen index 1 = bus 2 gen
            ..Default::default()
        };

        let opts = ScopfOptions {
            contingencies: Some(vec![gen_ctg]),
            screener: None,
            max_iterations: 5,
            ..Default::default()
        };

        let result =
            solve_dc_preventive_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();
        assert!(result.base_opf.total_cost > 0.0, "cost should be positive");
        // SCOPF should have evaluated at least the gen contingency
        assert!(result.total_contingencies_evaluated > 0);
    }

    /// SCOPF rejects flowgate constraints until aggregate corridor security cuts
    /// are modeled as first-class constraints.
    #[test]
    fn test_scopf_flowgate_constraints_are_rejected() {
        let mut net = surge_io::load(case_path("case9")).unwrap();

        // Add a tight base-case flowgate on the first branch
        let br = &net.branches[0];
        net.flowgates.push(surge_network::network::Flowgate {
            name: "FG1".into(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                br.from_bus,
                br.to_bus,
                br.circuit.clone(),
                1.0,
            )],
            contingency_branch: None, // base-case only
            limit_mw: 50.0,           // tight limit
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: vec![],
            limit_reverse_mw_schedule: vec![],
            hvdc_coefficients: vec![],
        });

        let opts = ScopfOptions {
            enforce_flowgates: true,
            screener: None,
            ..Default::default()
        };
        let err =
            crate::security::solve_scopf(&net, &opts).expect_err("SCOPF should reject flowgates");
        assert!(
            matches!(err, ScopfError::UnsupportedSecurityConstraint { .. }),
            "unexpected error: {err}"
        );
    }

    /// SCOPF rejects interface constraints until aggregate corridor security cuts
    /// are modeled as first-class constraints.
    #[test]
    fn test_scopf_interface_constraints_are_rejected() {
        let mut net = surge_io::load(case_path("case9")).unwrap();

        // Add a tight interface on branch 0
        let br = &net.branches[0];
        net.interfaces.push(surge_network::network::Interface {
            name: "IF1".into(),
            members: vec![surge_network::network::WeightedBranchRef::new(
                br.from_bus,
                br.to_bus,
                br.circuit.clone(),
                1.0,
            )],
            limit_forward_mw: 50.0,
            limit_reverse_mw: 50.0,
            in_service: true,
            limit_forward_mw_schedule: vec![],
            limit_reverse_mw_schedule: vec![],
        });

        let opts = ScopfOptions {
            enforce_flowgates: true,
            screener: None,
            ..Default::default()
        };
        let err =
            crate::security::solve_scopf(&net, &opts).expect_err("SCOPF should reject interfaces");
        assert!(
            matches!(err, ScopfError::UnsupportedSecurityConstraint { .. }),
            "unexpected error: {err}"
        );
    }

    /// N-2 double-branch contingency in DC-SCOPF.
    #[test]
    fn test_scopf_n2_contingency() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        // 4-bus diamond: 1—2, 1—3, 2—4, 3—4, plus a direct 1—4 path.
        // Outaging branches 1—2 and 3—4 simultaneously forces all flow through 1—3—4 and 1—4.
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);
        let b4 = Bus::new(4, BusType::PQ, 138.0);

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 200.0;
        let mut br13 = Branch::new_line(1, 3, 0.0, 0.1, 0.0);
        br13.rating_a_mva = 200.0;
        let mut br24 = Branch::new_line(2, 4, 0.0, 0.1, 0.0);
        br24.rating_a_mva = 200.0;
        let mut br34 = Branch::new_line(3, 4, 0.0, 0.1, 0.0);
        br34.rating_a_mva = 200.0;
        let mut br14 = Branch::new_line(1, 4, 0.0, 0.1, 0.0);
        br14.rating_a_mva = 200.0;

        let mut g1 = Generator::new(1, 100.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 300.0;
        g1.cost = Some(CostCurve::Polynomial {
            coeffs: vec![20.0, 0.0],
            startup: 0.0,
            shutdown: 0.0,
        });

        let net = Network {
            name: "test_n2".into(),
            base_mva: 100.0,
            freq_hz: 60.0,
            buses: vec![b1, b2, b3, b4],
            branches: vec![br12, br13, br24, br34, br14],
            generators: vec![g1],
            loads: vec![
                surge_network::network::Load::new(2, 20.0, 0.0),
                surge_network::network::Load::new(3, 20.0, 0.0),
                surge_network::network::Load::new(4, 60.0, 0.0),
            ],
            ..Default::default()
        };

        // N-2: simultaneously trip branches 0 (1—2) and 3 (3—4)
        let n2_ctg = Contingency {
            id: "n2_br0_br3".into(),
            label: "Trip br 1-2 and br 3-4".into(),
            branch_indices: vec![0, 3],
            ..Default::default()
        };

        let opts = ScopfOptions {
            contingencies: Some(vec![n2_ctg]),
            screener: None,
            max_iterations: 10,
            ..Default::default()
        };

        let result =
            solve_dc_preventive_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();
        assert!(result.base_opf.total_cost > 0.0);
        assert!(
            result.total_contingencies_evaluated > 0,
            "should evaluate the N-2 contingency"
        );
    }

    /// Mixed branch+gen contingency in DC-SCOPF.
    #[test]
    fn test_scopf_mixed_branch_gen_contingency() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PV, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 200.0;
        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 200.0;
        let mut br13 = Branch::new_line(1, 3, 0.0, 0.1, 0.0);
        br13.rating_a_mva = 200.0;

        let mut g1 = Generator::new(1, 60.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            coeffs: vec![20.0, 0.0],
            startup: 0.0,
            shutdown: 0.0,
        });

        let mut g2 = Generator::new(2, 40.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 200.0;
        g2.cost = Some(CostCurve::Polynomial {
            coeffs: vec![40.0, 0.0],
            startup: 0.0,
            shutdown: 0.0,
        });

        let net = Network {
            name: "test_mixed".into(),
            base_mva: 100.0,
            freq_hz: 60.0,
            buses: vec![b1, b2, b3],
            branches: vec![br12, br23, br13],
            generators: vec![g1, g2],
            loads: vec![
                surge_network::network::Load::new(2, 50.0, 0.0),
                surge_network::network::Load::new(3, 50.0, 0.0),
            ],
            ..Default::default()
        };

        // Mixed: trip branch 0 (1—2) AND generator 1 (bus 2) simultaneously
        let mixed_ctg = Contingency {
            id: "mixed_br0_gen1".into(),
            label: "Trip br 1-2 and gen at bus 2".into(),
            branch_indices: vec![0],
            generator_indices: vec![1],
            ..Default::default()
        };

        let opts = ScopfOptions {
            contingencies: Some(vec![mixed_ctg]),
            screener: None,
            max_iterations: 5,
            ..Default::default()
        };

        let result =
            solve_dc_preventive_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();
        assert!(result.base_opf.total_cost > 0.0);
        // The mixed contingency has 1 branch, so it goes into ctg_data
        // with the gen shift tracked in mixed_gen_shifts
        assert!(result.total_contingencies_evaluated > 0);
    }

    #[test]
    fn test_scopf_gen_limit_slacks_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = ScopfOptions {
            dc_opf: DcOpfOptions {
                gen_limit_penalty: Some(1000.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let result = solve_dc_preventive(&net, &opts).unwrap();
        assert!(result.converged);
        assert!(result.base_opf.total_cost > 0.0);
    }

    #[test]
    fn test_scopf_loss_factors_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts_no_loss = ScopfOptions::default();
        let opts_loss = ScopfOptions {
            dc_opf: DcOpfOptions {
                use_loss_factors: true,
                max_loss_iter: 3,
                loss_tol: 1e-3,
                ..Default::default()
            },
            ..Default::default()
        };
        let result_no = solve_dc_preventive(&net, &opts_no_loss).unwrap();
        let result_yes = solve_dc_preventive(&net, &opts_loss).unwrap();
        assert!(result_yes.converged);
        // With loss compensation, cost should be >= lossless (generators
        // must produce more to cover losses).
        assert!(
            result_yes.base_opf.total_cost >= result_no.base_opf.total_cost - 0.01,
            "loss-compensated cost ({:.2}) should be >= lossless ({:.2})",
            result_yes.base_opf.total_cost,
            result_no.base_opf.total_cost,
        );
        assert!(
            result_yes.base_opf.total_generation_mw
                >= result_no.base_opf.total_generation_mw - 0.01,
            "loss-compensated generation ({:.3}) should be >= lossless ({:.3})",
            result_yes.base_opf.total_generation_mw,
            result_no.base_opf.total_generation_mw,
        );
        assert!(
            result_yes.base_opf.total_losses_mw > 0.0,
            "loss-compensated preventive SCOPF should report positive losses"
        );
        assert!(
            result_yes
                .base_opf
                .pricing
                .lmp_loss
                .iter()
                .any(|loss| loss.abs() > 1e-8),
            "loss-compensated preventive SCOPF should expose a non-zero loss LMP component"
        );
        assert_lmp_decomposition(&result_yes.base_opf);
    }

    #[test]
    fn test_scopf_corrective_loss_factors_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts_no_loss = ScopfOptions {
            mode: ScopfMode::Corrective,
            ..Default::default()
        };
        let opts_loss = ScopfOptions {
            mode: ScopfMode::Corrective,
            dc_opf: DcOpfOptions {
                use_loss_factors: true,
                max_loss_iter: 3,
                loss_tol: 1e-3,
                ..Default::default()
            },
            ..Default::default()
        };
        let result_no =
            solve_dc_corrective_with_context(&net, &opts_no_loss, &ScopfRunContext::default())
                .unwrap();
        let result_yes =
            solve_dc_corrective_with_context(&net, &opts_loss, &ScopfRunContext::default())
                .unwrap();

        assert!(result_yes.converged);
        assert!(
            result_yes.base_opf.total_cost >= result_no.base_opf.total_cost - 0.01,
            "loss-compensated corrective cost ({:.2}) should be >= lossless ({:.2})",
            result_yes.base_opf.total_cost,
            result_no.base_opf.total_cost,
        );
        assert!(
            result_yes.base_opf.total_generation_mw
                >= result_no.base_opf.total_generation_mw - 0.01,
            "loss-compensated corrective generation ({:.3}) should be >= lossless ({:.3})",
            result_yes.base_opf.total_generation_mw,
            result_no.base_opf.total_generation_mw,
        );
        assert!(
            result_yes.base_opf.total_losses_mw > 0.0,
            "loss-compensated corrective SCOPF should report positive losses"
        );
        assert!(
            result_yes
                .base_opf
                .pricing
                .lmp_loss
                .iter()
                .any(|loss| loss.abs() > 1e-8),
            "loss-compensated corrective SCOPF should expose a non-zero loss LMP component"
        );
        assert_lmp_decomposition(&result_yes.base_opf);
    }

    #[test]
    fn test_scopf_no_angle_limits_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts_on = ScopfOptions {
            enforce_angle_limits: true,
            ..Default::default()
        };
        let opts_off = ScopfOptions {
            enforce_angle_limits: false,
            ..Default::default()
        };
        let result_on = solve_dc_preventive(&net, &opts_on).unwrap();
        let result_off = solve_dc_preventive(&net, &opts_off).unwrap();
        assert!(result_on.converged);
        assert!(result_off.converged);
        // With angle limits off, cost should be <= cost with limits on
        // (less constrained problem).
        assert!(
            result_off.base_opf.total_cost <= result_on.base_opf.total_cost + 0.01,
            "no-angle cost ({:.2}) should be <= with-angle cost ({:.2})",
            result_off.base_opf.total_cost,
            result_on.base_opf.total_cost,
        );
    }

    #[test]
    fn test_scopf_corrective_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = ScopfOptions {
            mode: ScopfMode::Corrective,
            ..Default::default()
        };
        let result =
            solve_dc_corrective_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();
        assert!(result.converged);
        assert!(result.base_opf.total_cost > 0.0);
    }

    #[test]
    fn test_scopf_corrective_gen_slacks_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = ScopfOptions {
            mode: ScopfMode::Corrective,
            dc_opf: DcOpfOptions {
                gen_limit_penalty: Some(1000.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let result =
            solve_dc_corrective_with_context(&net, &opts, &ScopfRunContext::default()).unwrap();
        assert!(result.converged);
        assert!(result.base_opf.total_cost > 0.0);
    }
}
