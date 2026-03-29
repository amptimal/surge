// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared DC-SCOPF model assembly helpers.

use std::collections::HashMap;

use surge_dc::PtdfRows;
use surge_network::Network;

use crate::common::context::OpfNetworkContext;
use crate::dc::costs::{
    GeneratorCostBuffers, apply_generator_costs, build_hessian_csc, build_pwl_gen_info,
    quadratic_pwl_local_indices,
};
use crate::dc::opf::DcOpfError;
use surge_sparse::Triplet;

use super::types::ScopfOptions;

pub(crate) struct PreventiveBaseModel {
    pub constrained_branches: Vec<usize>,
    pub active_interface_indices: Vec<usize>,
    pub active_base_flowgate_indices: Vec<usize>,
    pub active_contingency_flowgate_indices: Vec<usize>,
    pub gen_bus_idx: Vec<usize>,
    pub hvdc_injections: Vec<(usize, usize, f64, f64)>,
    pub ptdf: PtdfRows,
    pub n_flow: usize,
    pub n_ang: usize,
    pub n_ifg: usize,
    pub n_base_rows: usize,
    pub n_var_base: usize,
    pub col_cost: Vec<f64>,
    pub c0_total: f64,
    pub hessian: Option<(Vec<i32>, Vec<i32>, Vec<f64>)>,
    pub col_lower: Vec<f64>,
    pub col_upper: Vec<f64>,
    pub base_triplets: Vec<Triplet<f64>>,
    pub base_row_lower: Vec<f64>,
    pub base_row_upper: Vec<f64>,
}

pub(crate) fn build_preventive_base_model(
    network: &Network,
    options: &ScopfOptions,
    ctx: &OpfNetworkContext,
) -> Result<PreventiveBaseModel, DcOpfError> {
    let n_bus = ctx.n_bus;
    let n_br = ctx.n_branches;
    let bus_map = &ctx.bus_map;
    let base = ctx.base_mva;
    let gen_indices = &ctx.gen_indices;
    let n_gen = gen_indices.len();
    let bus_pd_mw = network.bus_load_p_mw();

    let constrained_branches = if options.dc_opf.enforce_thermal_limits {
        ctx.constrained_branch_indices(options.min_rate_a)
    } else {
        Vec::new()
    };
    let n_flow = constrained_branches.len();

    let angle_constrained_branches = collect_angle_constrained_branches(network);
    let n_ang = angle_constrained_branches.len();

    let (
        n_ifg,
        active_interface_indices,
        active_base_flowgate_indices,
        active_contingency_flowgate_indices,
    ) = collect_active_flowgate_indices(network, options.enforce_flowgates);

    let pwl_gen_info = build_pwl_gen_info(
        network,
        gen_indices,
        base,
        false,
        options.dc_opf.pwl_cost_breakpoints,
    );
    let n_pwl_gen = pwl_gen_info.len();
    let n_pwl_rows: usize = pwl_gen_info.iter().map(|entry| entry.segments.len()).sum();

    let n_base_rows = n_flow + n_ang + n_ifg + n_bus + n_pwl_rows;
    let s_upper_base_offset = n_bus + n_gen;
    let s_lower_base_offset = n_bus + n_gen + n_flow;
    let e_g_offset = n_bus + n_gen + 2 * n_flow;
    let n_var_base = n_bus + n_gen + 2 * n_flow + n_pwl_gen;

    let thermal_penalty_per_pu = options.penalty_config.thermal.marginal_cost_at(0.0) * base;

    let mut col_cost = vec![0.0; n_var_base];
    let mut q_diag = vec![0.0; n_gen];
    let mut c0_total = 0.0;
    let poly_quad_local_indices = quadratic_pwl_local_indices(network, gen_indices, false);
    apply_generator_costs(
        network,
        gen_indices,
        base,
        n_bus,
        GeneratorCostBuffers {
            col_cost: &mut col_cost,
            q_diag: &mut q_diag,
            c0_total: &mut c0_total,
        },
        &poly_quad_local_indices,
    )?;

    for k in 0..n_pwl_gen {
        col_cost[e_g_offset + k] = 1.0;
    }
    for ci in 0..n_flow {
        col_cost[s_upper_base_offset + ci] = thermal_penalty_per_pu;
        col_cost[s_lower_base_offset + ci] = thermal_penalty_per_pu;
    }

    let hessian = build_hessian_csc(n_bus, &q_diag, 2 * n_flow + n_pwl_gen);

    let mut col_lower = vec![0.0; n_var_base];
    let mut col_upper = vec![0.0; n_var_base];
    crate::dc::island_lmp::fix_island_theta_bounds(
        &mut col_lower,
        &mut col_upper,
        0,
        n_bus,
        &ctx.island_refs,
    );
    for (j, &gi) in gen_indices.iter().enumerate() {
        col_lower[n_bus + j] = network.generators[gi].pmin / base;
        col_upper[n_bus + j] = network.generators[gi].pmax / base;
    }
    for ci in 0..n_flow {
        col_lower[s_upper_base_offset + ci] = 0.0;
        col_upper[s_upper_base_offset + ci] = f64::INFINITY;
        col_lower[s_lower_base_offset + ci] = 0.0;
        col_upper[s_lower_base_offset + ci] = f64::INFINITY;
    }
    for k in 0..n_pwl_gen {
        col_lower[e_g_offset + k] = f64::NEG_INFINITY;
        col_upper[e_g_offset + k] = f64::INFINITY;
    }

    let all_branches: Vec<usize> = (0..n_br).collect();
    let ptdf = surge_dc::compute_ptdf(network, &surge_dc::PtdfRequest::for_branches(&all_branches))
        .map_err(|e| DcOpfError::SolverError(e.to_string()))?;

    let gen_bus_idx: Vec<usize> = gen_indices
        .iter()
        .map(|&gi| bus_map[&network.generators[gi].bus])
        .collect();

    let hvdc_links = surge_hvdc::interop::links_from_network(network);
    let hvdc_injections = hvdc_links
        .iter()
        .filter_map(|link| {
            let from_idx = *bus_map.get(&link.from_bus())?;
            let to_idx = *bus_map.get(&link.to_bus())?;
            let p_dc_pu = link.p_dc_mw() / base;
            Some((from_idx, to_idx, -p_dc_pu, p_dc_pu))
        })
        .collect();

    let mut base_triplets: Vec<Triplet<f64>> = Vec::with_capacity(6 * n_bus + n_gen + 4 * n_flow);
    for (ci, &branch_idx) in constrained_branches.iter().enumerate() {
        let br = &network.branches[branch_idx];
        if br.x.abs() < 1e-20 {
            continue;
        }
        let b_val = br.b_dc();
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];
        base_triplets.push(Triplet {
            row: ci,
            col: from,
            val: b_val,
        });
        base_triplets.push(Triplet {
            row: ci,
            col: to,
            val: -b_val,
        });
        base_triplets.push(Triplet {
            row: ci,
            col: s_upper_base_offset + ci,
            val: -1.0,
        });
        base_triplets.push(Triplet {
            row: ci,
            col: s_lower_base_offset + ci,
            val: 1.0,
        });
    }

    for (ai, &branch_idx) in angle_constrained_branches.iter().enumerate() {
        let br = &network.branches[branch_idx];
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];
        let row = n_flow + ai;
        base_triplets.push(Triplet {
            row,
            col: from,
            val: 1.0,
        });
        base_triplets.push(Triplet {
            row,
            col: to,
            val: -1.0,
        });
    }

    if n_ifg > 0 {
        let mut ifg_row = n_flow + n_ang;
        for &interface_idx in &active_interface_indices {
            let iface = &network.interfaces[interface_idx];
            append_monitored_branch_terms(
                network,
                bus_map,
                ifg_row,
                &iface.members,
                &mut base_triplets,
            );
            ifg_row += 1;
        }
        for &flowgate_idx in &active_base_flowgate_indices {
            let fg = &network.flowgates[flowgate_idx];
            append_monitored_branch_terms(
                network,
                bus_map,
                ifg_row,
                &fg.monitored,
                &mut base_triplets,
            );
            ifg_row += 1;
        }
    }

    for branch in &network.branches {
        if !branch.in_service || branch.x.abs() < 1e-20 {
            continue;
        }
        let from = bus_map[&branch.from_bus];
        let to = bus_map[&branch.to_bus];
        let b = branch.b_dc();
        let eq_from = n_flow + n_ang + n_ifg + from;
        let eq_to = n_flow + n_ang + n_ifg + to;
        base_triplets.push(Triplet {
            row: eq_from,
            col: to,
            val: -b,
        });
        base_triplets.push(Triplet {
            row: eq_to,
            col: from,
            val: -b,
        });
        base_triplets.push(Triplet {
            row: eq_from,
            col: from,
            val: b,
        });
        base_triplets.push(Triplet {
            row: eq_to,
            col: to,
            val: b,
        });
    }
    for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
        base_triplets.push(Triplet {
            row: n_flow + n_ang + n_ifg + bus_idx,
            col: n_bus + j,
            val: -1.0,
        });
    }
    {
        let mut pwl_row = n_flow + n_ang + n_ifg + n_bus;
        for (k, entry) in pwl_gen_info.iter().enumerate() {
            for &(slope_pu, _intercept) in &entry.segments {
                base_triplets.push(Triplet {
                    row: pwl_row,
                    col: n_bus + entry.local_gen_index,
                    val: -slope_pu,
                });
                base_triplets.push(Triplet {
                    row: pwl_row,
                    col: e_g_offset + k,
                    val: 1.0,
                });
                pwl_row += 1;
            }
        }
    }

    let mut base_row_lower = Vec::with_capacity(n_base_rows);
    let mut base_row_upper = Vec::with_capacity(n_base_rows);
    for &branch_idx in &constrained_branches {
        let br = &network.branches[branch_idx];
        let fmax = br.rating_a_mva / base;
        let pfinj = if br.phase_shift_rad.abs() < 1e-12 {
            0.0
        } else {
            br.b_dc() * br.phase_shift_rad
        };
        base_row_lower.push(-fmax - pfinj);
        base_row_upper.push(fmax - pfinj);
    }
    for &branch_idx in &angle_constrained_branches {
        let br = &network.branches[branch_idx];
        base_row_lower.push(br.angle_diff_min_rad.unwrap_or(f64::NEG_INFINITY));
        base_row_upper.push(br.angle_diff_max_rad.unwrap_or(f64::INFINITY));
    }
    for &interface_idx in &active_interface_indices {
        let iface = &network.interfaces[interface_idx];
        base_row_lower.push(-iface.limit_reverse_mw.abs() / base);
        base_row_upper.push(iface.limit_forward_mw / base);
    }
    for &flowgate_idx in &active_base_flowgate_indices {
        let fg = &network.flowgates[flowgate_idx];
        let rev = fg.effective_reverse_or_forward(0);
        base_row_lower.push(-rev / base);
        base_row_upper.push(fg.limit_mw / base);
    }

    let pbusinj = build_phase_shift_bus_injections(network, bus_map, n_bus);
    for (bus_idx, p_inj) in pbusinj.iter().enumerate().take(n_bus) {
        let pd_pu = bus_pd_mw[bus_idx] / base;
        let gs_pu = network.buses[bus_idx].shunt_conductance_mw / base;
        let rhs = -pd_pu - gs_pu - p_inj;
        base_row_lower.push(rhs);
        base_row_upper.push(rhs);
    }
    for entry in &pwl_gen_info {
        for &(_slope_pu, intercept) in &entry.segments {
            base_row_lower.push(intercept);
            base_row_upper.push(f64::INFINITY);
        }
    }

    Ok(PreventiveBaseModel {
        constrained_branches,
        active_interface_indices,
        active_base_flowgate_indices,
        active_contingency_flowgate_indices,
        gen_bus_idx,
        hvdc_injections,
        ptdf,
        n_flow,
        n_ang,
        n_ifg,
        n_base_rows,
        n_var_base,
        col_cost,
        c0_total,
        hessian,
        col_lower,
        col_upper,
        base_triplets,
        base_row_lower,
        base_row_upper,
    })
}

fn collect_angle_constrained_branches(network: &Network) -> Vec<usize> {
    const ANG_UNCONSTRAINED_LO: f64 = -std::f64::consts::PI;
    const ANG_UNCONSTRAINED_HI: f64 = std::f64::consts::PI;
    let mut branches = Vec::new();
    for (branch_idx, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        let lo = br.angle_diff_min_rad.unwrap_or(f64::NEG_INFINITY);
        let hi = br.angle_diff_max_rad.unwrap_or(f64::INFINITY);
        if lo > ANG_UNCONSTRAINED_LO + 1e-12 || hi < ANG_UNCONSTRAINED_HI - 1e-12 {
            branches.push(branch_idx);
        }
    }
    branches
}

fn collect_active_flowgate_indices(
    network: &Network,
    enforce_flowgates: bool,
) -> (usize, Vec<usize>, Vec<usize>, Vec<usize>) {
    if !enforce_flowgates {
        return (0, vec![], vec![], vec![]);
    }

    let active_interface_indices: Vec<usize> = network
        .interfaces
        .iter()
        .enumerate()
        .filter(|(_, iface)| iface.in_service && iface.limit_forward_mw > 0.0)
        .map(|(i, _)| i)
        .collect();
    let active_base_flowgate_indices: Vec<usize> = network
        .flowgates
        .iter()
        .enumerate()
        .filter(|(_, fg)| fg.in_service && fg.contingency_branch.is_none())
        .map(|(i, _)| i)
        .collect();
    let active_contingency_flowgate_indices: Vec<usize> = network
        .flowgates
        .iter()
        .enumerate()
        .filter(|(_, fg)| fg.in_service && fg.contingency_branch.is_some())
        .map(|(i, _)| i)
        .collect();
    (
        active_interface_indices.len() + active_base_flowgate_indices.len(),
        active_interface_indices,
        active_base_flowgate_indices,
        active_contingency_flowgate_indices,
    )
}

fn build_phase_shift_bus_injections(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    n_bus: usize,
) -> Vec<f64> {
    let mut pbusinj = vec![0.0_f64; n_bus];
    for branch in &network.branches {
        if !branch.in_service || branch.x.abs() < 1e-20 || branch.phase_shift_rad.abs() < 1e-12 {
            continue;
        }
        let pf = branch.b_dc() * branch.phase_shift_rad;
        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];
        pbusinj[from_idx] += pf;
        pbusinj[to_idx] -= pf;
    }
    pbusinj
}

fn append_monitored_branch_terms(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    row: usize,
    monitored_branches: &[surge_network::network::WeightedBranchRef],
    triplets: &mut Vec<Triplet<f64>>,
) {
    for member in monitored_branches {
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
                row,
                col: from,
                val: coeff * b_val,
            });
            triplets.push(Triplet {
                row,
                col: to,
                val: -coeff * b_val,
            });
        }
    }
}
