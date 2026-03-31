// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared DC-SCOPF model assembly helpers.

use std::collections::{HashMap, HashSet};

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
    /// Maps variable HVDC link index k → position in `hvdc_injections`.
    pub hvdc_var_to_inj_idx: Vec<usize>,
    pub ptdf: PtdfRows,
    pub n_flow: usize,
    pub n_ang: usize,
    pub n_ifg: usize,
    pub hvdc_offset: usize,
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
    /// Indices into `base_triplets` for generator power-balance coefficients (-1.0).
    /// Used by loss factor iteration to update coefficients in-place.
    pub gen_balance_triplet_indices: Vec<usize>,
    /// Row offset where power balance rows begin (n_flow + n_ang + n_ifg).
    pub balance_row_offset: usize,
    /// Per-bus injection vector (pbusinj) used to construct power balance RHS.
    /// Stored so loss factor iteration can recompute RHS with loss allocation.
    pub pbusinj: Vec<f64>,
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

    // PAR branches are removed from B_bus and replaced by fixed injections.
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

    let constrained_branches = if options.dc_opf.enforce_thermal_limits {
        ctx.constrained_branch_indices(options.min_rate_a)
            .into_iter()
            .filter(|idx| !par_branch_set.contains(idx))
            .collect()
    } else {
        Vec::new()
    };
    let n_flow = constrained_branches.len();

    let angle_constrained_branches = if options.enforce_angle_limits {
        collect_angle_constrained_branches(network)
    } else {
        Vec::new()
    };
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
        options.dc_opf.use_pwl_costs,
        options.dc_opf.pwl_cost_breakpoints,
    );
    let n_pwl_gen = pwl_gen_info.len();
    let n_pwl_rows: usize = pwl_gen_info.iter().map(|entry| entry.segments.len()).sum();

    // Variable HVDC links from DcOpfOptions.
    let all_hvdc_opf_links = options.dc_opf.hvdc_links.as_deref().unwrap_or(&[]);
    let hvdc_var: Vec<&crate::dc::opf::HvdcOpfLink> = all_hvdc_opf_links
        .iter()
        .filter(|h| h.is_variable())
        .collect();
    let n_hvdc = hvdc_var.len();

    let has_gen_slacks = options.dc_opf.gen_limit_penalty.is_some();
    let n_gen_slacks = if has_gen_slacks { n_gen } else { 0 };
    let n_base_rows = n_flow + n_ang + n_ifg + n_bus + n_pwl_rows + 2 * n_gen_slacks;
    // Variable layout:
    //   [θ(n_bus) | Pg(n_gen) | P_hvdc(n_hvdc) | s_therm_up(n_flow)
    //    | s_therm_lo(n_flow) | s_ang_up(n_ang) | s_ang_lo(n_ang)
    //    | sg_upper(n_gen_slacks) | sg_lower(n_gen_slacks) | e_g(n_pwl_gen)]
    let hvdc_offset = n_bus + n_gen;
    let s_upper_base_offset = hvdc_offset + n_hvdc;
    let s_lower_base_offset = s_upper_base_offset + n_flow;
    let sa_upper_offset = s_lower_base_offset + n_flow;
    let sa_lower_offset = sa_upper_offset + n_ang;
    let sg_upper_offset = sa_lower_offset + n_ang;
    let sg_lower_offset = sg_upper_offset + n_gen_slacks;
    let e_g_offset = sg_lower_offset + n_gen_slacks;
    let n_var_base = e_g_offset + n_pwl_gen;

    let thermal_penalty_per_pu = options.penalty_config.thermal.marginal_cost_at(0.0) * base;
    let angle_penalty_per_rad = options.penalty_config.angle.marginal_cost_at(0.0);

    let mut col_cost = vec![0.0; n_var_base];
    let mut q_diag = vec![0.0; n_gen];
    let mut c0_total = 0.0;
    let poly_quad_local_indices =
        quadratic_pwl_local_indices(network, gen_indices, options.dc_opf.use_pwl_costs);
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
    for ai in 0..n_ang {
        col_cost[sa_upper_offset + ai] = angle_penalty_per_rad;
        col_cost[sa_lower_offset + ai] = angle_penalty_per_rad;
    }
    if let Some(penalty) = options.dc_opf.gen_limit_penalty {
        let penalty_pu = penalty * base;
        for j in 0..n_gen {
            col_cost[sg_upper_offset + j] = penalty_pu;
            col_cost[sg_lower_offset + j] = penalty_pu;
        }
    }

    let hessian = build_hessian_csc(
        n_bus,
        &q_diag,
        n_hvdc + 2 * n_flow + 2 * n_ang + 2 * n_gen_slacks + n_pwl_gen,
    );

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
        if has_gen_slacks {
            col_lower[n_bus + j] = f64::NEG_INFINITY;
            col_upper[n_bus + j] = f64::INFINITY;
        } else {
            col_lower[n_bus + j] = network.generators[gi].pmin / base;
            col_upper[n_bus + j] = network.generators[gi].pmax / base;
        }
    }
    // HVDC variable bounds: [p_dc_min/base, p_dc_max/base], zero cost.
    for (k, hvdc) in hvdc_var.iter().enumerate() {
        col_lower[hvdc_offset + k] = hvdc.p_dc_min_mw / base;
        col_upper[hvdc_offset + k] = hvdc.p_dc_max_mw / base;
        // col_cost[hvdc_offset + k] remains 0.0 — co-optimized with gen costs
    }
    for ci in 0..n_flow {
        col_lower[s_upper_base_offset + ci] = 0.0;
        col_upper[s_upper_base_offset + ci] = f64::INFINITY;
        col_lower[s_lower_base_offset + ci] = 0.0;
        col_upper[s_lower_base_offset + ci] = f64::INFINITY;
    }
    for ai in 0..n_ang {
        col_lower[sa_upper_offset + ai] = 0.0;
        col_upper[sa_upper_offset + ai] = f64::INFINITY;
        col_lower[sa_lower_offset + ai] = 0.0;
        col_upper[sa_lower_offset + ai] = f64::INFINITY;
    }
    for j in 0..n_gen_slacks {
        col_lower[sg_upper_offset + j] = 0.0;
        col_upper[sg_upper_offset + j] = f64::INFINITY;
        col_lower[sg_lower_offset + j] = 0.0;
        col_upper[sg_lower_offset + j] = f64::INFINITY;
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

    // Build HVDC injection table for contingency evaluation.
    // When DcOpfOptions::hvdc_links is set, use those (with proper loss model
    // and variable dispatch support). Otherwise fall back to interop links.
    // Each entry: (from_bus_idx, to_bus_idx, from_inj_pu, to_inj_pu).
    // For variable links, initial injections use the midpoint of bounds;
    // the solve loop updates these from the LP solution each iteration.
    //
    // `hvdc_var_to_inj_idx[k]` maps variable link k to its index in
    // `hvdc_injections`, so the solve loop can update the right entry.
    let hvdc_links = surge_hvdc::interop::links_from_network(network);
    let mut hvdc_var_to_inj_idx: Vec<usize> = Vec::with_capacity(n_hvdc);
    let hvdc_injections: Vec<(usize, usize, f64, f64)> = if !all_hvdc_opf_links.is_empty() {
        let mut injs = Vec::new();
        for hvdc in all_hvdc_opf_links {
            let fi = bus_map[&hvdc.from_bus];
            let ti = bus_map[&hvdc.to_bus];
            if hvdc.is_variable() {
                hvdc_var_to_inj_idx.push(injs.len());
                let p_mid = (hvdc.p_dc_min_mw + hvdc.p_dc_max_mw) * 0.5 / base;
                injs.push((fi, ti, -p_mid, p_mid));
            } else {
                let p_dc_pu = hvdc.p_dc_min_mw / base;
                let p_inv_pu = hvdc.p_inv_mw(hvdc.p_dc_min_mw) / base;
                injs.push((fi, ti, -p_dc_pu, p_inv_pu));
            }
        }
        injs
    } else {
        hvdc_links
            .iter()
            .filter_map(|link| {
                let from_idx = *bus_map.get(&link.from_bus())?;
                let to_idx = *bus_map.get(&link.to_bus())?;
                let p_dc_pu = link.p_dc_mw() / base;
                Some((from_idx, to_idx, -p_dc_pu, p_dc_pu))
            })
            .collect()
    };

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
        base_triplets.push(Triplet {
            row,
            col: sa_upper_offset + ai,
            val: -1.0,
        });
        base_triplets.push(Triplet {
            row,
            col: sa_lower_offset + ai,
            val: 1.0,
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

    for (br_idx, branch) in network.branches.iter().enumerate() {
        if !branch.in_service || branch.x.abs() < 1e-20 || par_branch_set.contains(&br_idx) {
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
    let mut gen_balance_triplet_indices = Vec::with_capacity(gen_bus_idx.len());
    for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
        gen_balance_triplet_indices.push(base_triplets.len());
        base_triplets.push(Triplet {
            row: n_flow + n_ang + n_ifg + bus_idx,
            col: n_bus + j,
            val: -1.0,
        });
    }
    // HVDC variable link power balance coefficients.
    // Rectifier (from_bus): +1.0 (draws power from AC).
    // Inverter (to_bus): -(1 - loss_b_frac) (injects net of linear losses).
    let hvdc_from_idx: Vec<usize> = hvdc_var.iter().map(|h| bus_map[&h.from_bus]).collect();
    let hvdc_to_idx: Vec<usize> = hvdc_var.iter().map(|h| bus_map[&h.to_bus]).collect();
    for (k, hvdc) in hvdc_var.iter().enumerate() {
        let fi = hvdc_from_idx[k];
        let ti = hvdc_to_idx[k];
        base_triplets.push(Triplet {
            row: n_flow + n_ang + n_ifg + fi,
            col: hvdc_offset + k,
            val: 1.0,
        });
        base_triplets.push(Triplet {
            row: n_flow + n_ang + n_ifg + ti,
            col: hvdc_offset + k,
            val: -(1.0 - hvdc.loss_b_frac),
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

    // Gen-limit soft constraint rows: Pg_j - sg_upper_j ≤ pmax/base,
    //                                 -Pg_j - sg_lower_j ≤ -pmin/base
    if has_gen_slacks {
        let gen_pmax_row = n_flow + n_ang + n_ifg + n_bus + n_pwl_rows;
        let gen_pmin_row = gen_pmax_row + n_gen;
        for j in 0..n_gen {
            // Pmax row: Pg_j - sg_upper_j ≤ pmax/base
            base_triplets.push(Triplet {
                row: gen_pmax_row + j,
                col: n_bus + j,
                val: 1.0,
            });
            base_triplets.push(Triplet {
                row: gen_pmax_row + j,
                col: sg_upper_offset + j,
                val: -1.0,
            });
            // Pmin row: -Pg_j - sg_lower_j ≤ -pmin/base
            base_triplets.push(Triplet {
                row: gen_pmin_row + j,
                col: n_bus + j,
                val: -1.0,
            });
            base_triplets.push(Triplet {
                row: gen_pmin_row + j,
                col: sg_lower_offset + j,
                val: -1.0,
            });
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

    let mut pbusinj = build_phase_shift_bus_injections(network, bus_map, n_bus);

    // Undo phase-shift injection for PAR branches (they are removed from B_bus)
    // and replace with scheduled-interchange target_mw injection.
    for ps in &options.dc_opf.par_setpoints {
        if let Some(&br_idx) = ctx
            .branch_idx_map
            .get(&(ps.from_bus, ps.to_bus, ps.circuit.clone()))
        {
            let br = &network.branches[br_idx];
            if !br.in_service || br.x.abs() < 1e-20 {
                continue;
            }
            // Remove the PST contribution that build_phase_shift_bus_injections added
            if br.phase_shift_rad.abs() >= 1e-12 {
                let pf = br.b_dc() * br.phase_shift_rad;
                let fi = bus_map[&br.from_bus];
                let ti = bus_map[&br.to_bus];
                pbusinj[fi] -= pf;
                pbusinj[ti] += pf;
            }
            // Add scheduled-interchange injection
            let fi = bus_map[&ps.from_bus];
            let ti = bus_map[&ps.to_bus];
            pbusinj[fi] += ps.target_mw / base;
            pbusinj[ti] -= ps.target_mw / base;
        }
    }

    // HVDC link injections — matches DC-OPF (opf_lp.rs).
    if !all_hvdc_opf_links.is_empty() {
        // When DcOpfOptions::hvdc_links is set, use those with proper loss model.
        // Fixed links: bake full injection (with losses) into pbusinj.
        // Variable links: only constant loss_a at inverter bus.
        for hvdc in all_hvdc_opf_links.iter().filter(|h| !h.is_variable()) {
            let p_dc = hvdc.p_dc_min_mw;
            let p_inv = hvdc.p_inv_mw(p_dc);
            let fi = bus_map[&hvdc.from_bus];
            pbusinj[fi] += p_dc / base;
            let ti = bus_map[&hvdc.to_bus];
            pbusinj[ti] -= p_inv / base;
        }
        for (k, hvdc) in hvdc_var.iter().enumerate() {
            let ti = hvdc_to_idx[k];
            pbusinj[ti] += hvdc.loss_a_mw / base;
        }
    } else {
        // Fallback: use interop links (lossless approximation).
        for link in &hvdc_links {
            if let Some(&fi) = bus_map.get(&link.from_bus()) {
                pbusinj[fi] += link.p_dc_mw() / base;
            }
            if let Some(&ti) = bus_map.get(&link.to_bus()) {
                pbusinj[ti] -= link.p_dc_mw() / base;
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
                pbusinj[i] -= inj.p_mw / base;
            }
        }
    }

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
    if has_gen_slacks {
        for &gi in gen_indices {
            let g = &network.generators[gi];
            // Pmax row: Pg - sg_upper ≤ pmax/base
            base_row_lower.push(f64::NEG_INFINITY);
            base_row_upper.push(g.pmax / base);
            // Pmin row: -Pg - sg_lower ≤ -pmin/base
        }
        for &gi in gen_indices {
            let g = &network.generators[gi];
            base_row_lower.push(f64::NEG_INFINITY);
            base_row_upper.push(-g.pmin / base);
        }
    }

    let balance_row_offset = n_flow + n_ang + n_ifg;

    Ok(PreventiveBaseModel {
        constrained_branches,
        active_interface_indices,
        active_base_flowgate_indices,
        active_contingency_flowgate_indices,
        gen_bus_idx,
        hvdc_injections,
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
        base_triplets,
        base_row_lower,
        base_row_upper,
        gen_balance_triplet_indices,
        balance_row_offset,
        pbusinj,
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
