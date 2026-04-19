// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCED sparse-problem solve helpers.

use surge_network::Network;
use surge_opf::backends::{LpOptions, LpResult, LpSolveStatus};
use surge_sparse::Triplet;
use tracing::{debug, warn};

use super::rows::{ScedRowPlan, ScedRowsInput, build_rows, plan_rows};
use crate::common::builders;
use crate::common::dc::{
    DcNomogramTighteningInput, DcSolveSession, DcSparseProblemInput, build_sparse_problem,
    solve_sparse_problem, tighten_nomograms,
};
use crate::common::reserves::ReserveLpLayout;
use crate::common::runtime::DispatchPeriodContext;
use crate::error::ScedError;
use crate::sced::layout::ScedLayout;
use crate::sced::plan::ScedProblemPlan;

const SCED_PRICING_SOFT_SLACK_TOL_PU: f64 = 1e-7;
const SCED_LMP_ANCHOR_EPS_MW: f64 = 1.0;

pub(super) struct ScedProblemBuildInput<'a> {
    pub network: &'a Network,
    pub context: DispatchPeriodContext<'a>,
    pub solve: &'a DcSolveSession<'a>,
    pub problem_plan: &'a ScedProblemPlan<'a>,
}

pub(super) struct ScedProblemBuildState {
    pub row_plan: ScedRowPlan,
    pub n_row: usize,
    pub n_flow: usize,
    pub n_branch_flow: usize,
    pub n_fg_rows: usize,
    pub fg_limits: Vec<f64>,
    pub fg_shift_offsets: Vec<f64>,
    pub row_lower: Vec<f64>,
    pub row_upper: Vec<f64>,
    pub a_start: Vec<i32>,
    pub a_index: Vec<i32>,
    pub a_value: Vec<f64>,
}

pub(super) fn build_problem(input: ScedProblemBuildInput<'_>) -> ScedProblemBuildState {
    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let bus_map = &input.solve.bus_map;
    let base = input.solve.base_mva;
    let model_plan = &input.problem_plan.model_plan;
    let active = &model_plan.active;
    let layout = &model_plan.layout;
    let network_plan = &model_plan.network_plan;
    let network_rows = &input.problem_plan.network_rows;
    let n_bus = input.network.n_buses();
    let n_gen = setup.n_gen;
    let n_hvdc_links = setup.n_hvdc_links;
    let n_var = layout.dispatch.n_vars;
    let n_branch_flow = network_plan.constrained_branches.len();
    let n_fg_rows = network_plan.fg_rows.len();
    let n_iface_rows = network_plan.iface_rows.len();
    let n_flow = n_branch_flow + n_fg_rows + n_iface_rows;
    let n_system_policy_rows = builders::system_policy_rows(&setup.tie_line_pairs, spec, 1);
    let row_plan = plan_rows(
        n_flow,
        n_bus,
        n_system_policy_rows,
        setup,
        &active.reserve_layout,
        spec,
        input.context.has_prev_dispatch(),
        input.context.period,
        network_plan.angle_constrained_branches.len(),
    );
    let n_explicit_ctg_rows = crate::common::contingency::explicit_contingency_objective_rows(
        model_plan.explicit_contingency.as_ref(),
    );
    let n_row = row_plan.n_row + n_explicit_ctg_rows;

    let est_nnz = 6 * n_bus + n_gen + 2 * n_flow;
    let mut triplets: Vec<Triplet<f64>> = Vec::with_capacity(est_nnz);
    let mut row_lower: Vec<f64> = Vec::with_capacity(n_row);
    let mut row_upper: Vec<f64> = Vec::with_capacity(n_row);

    builders::build_dc_network_rows(builders::DcNetworkRowsInput {
        flow_network: input.network,
        dispatch_network: input.network,
        constrained_branches: &network_plan.constrained_branches,
        fg_rows: &network_plan.fg_rows,
        resolved_flowgates: &setup.resolved_flowgates,
        iface_rows: &network_plan.iface_rows,
        resolved_interfaces: &setup.resolved_interfaces,
        setup,
        gen_indices: &setup.gen_indices,
        gen_bus_idx: &setup.gen_bus_idx,
        spec,
        bus_map,
        pbusinj: &network_rows.pbusinj,
        hvdc_loss_a_bus: &network_rows.hvdc_loss_a_bus,
        hvdc_from_idx: &network_plan.hvdc_from_idx,
        hvdc_to_idx: &network_plan.hvdc_to_idx,
        hvdc_band_offsets: &setup.hvdc_band_offsets_rel,
        dl_list: &active.dl_list,
        active_vbids: &active.active_vbids,
        par_branch_set: Some(&setup.par_branch_set),
        extra_terms: &network_rows.power_balance_extra_terms,
        col_base: 0,
        row_base: 0,
        theta_off: layout.dispatch.theta,
        pg_off: layout.dispatch.pg,
        sto_ch_off: layout.dispatch.sto_ch,
        sto_dis_off: layout.dispatch.sto_dis,
        hvdc_off: layout.dispatch.hvdc,
        branch_slack: Some(builders::SoftLimitSlackLayout {
            lower_off: layout.branch_lower_slack,
            upper_off: layout.branch_upper_slack,
        }),
        flowgate_slack: Some(builders::SoftLimitSlackLayout {
            lower_off: layout.flowgate_lower_slack,
            upper_off: layout.flowgate_upper_slack,
        }),
        interface_slack: Some(builders::SoftLimitSlackLayout {
            lower_off: layout.interface_lower_slack,
            upper_off: layout.interface_upper_slack,
        }),
        dl_off: layout.dispatch.dl,
        vbid_off: layout.dispatch.vbid,
        n_hvdc_links,
        storage_in_pu: true,
        base,
        hour: 0,
        // SCED never enters switchable-branch mode: it reconciles a
        // fixed commitment the upstream SCUC has already decided.
        switching_pf_l_cols: None,
    })
    .extend_into(&mut triplets, &mut row_lower, &mut row_upper);

    build_rows(
        ScedRowsInput {
            network: input.network,
            spec,
            context: input.context,
            setup,
            reserve_layout: &active.reserve_layout,
            reserve_ctx: &active.reserve_ctx,
            plan: &row_plan,
            layout,
            n_pb_curt_segs: model_plan.n_pb_curt_segs,
            n_pb_excess_segs: model_plan.n_pb_excess_segs,
            pg_offset: layout.dispatch.pg,
            sto_ch_offset: layout.dispatch.sto_ch,
            sto_dis_offset: layout.dispatch.sto_dis,
            sto_soc_offset: layout.dispatch.sto_soc,
            sto_epi_dis_offset: layout.dispatch.sto_epi_dis,
            sto_epi_ch_offset: layout.dispatch.sto_epi_ch,
            e_g_offset: layout.dispatch.e_g,
            block_offset: layout.dispatch.block,
            blk_res_offset: layout.dispatch.block_reserve,
            base,
        },
        &mut triplets,
        &mut row_lower,
        &mut row_upper,
    );

    // Angle difference constraint rows: for each branch with finite angle
    // limits, add two inequality rows:
    //   upper: θ_from - θ_to - σ_upper ≤ angmax
    //   lower: -θ_from + θ_to - σ_lower ≤ -angmin
    for (row_idx, acb) in network_plan.angle_constrained_branches.iter().enumerate() {
        let upper_row = row_plan.angle_diff_base_row + 2 * row_idx;
        let lower_row = upper_row + 1;
        // Upper: θ_from - θ_to - σ_upper ≤ angmax
        triplets.push(Triplet {
            row: upper_row,
            col: layout.dispatch.theta + acb.from_bus_idx,
            val: 1.0,
        });
        triplets.push(Triplet {
            row: upper_row,
            col: layout.dispatch.theta + acb.to_bus_idx,
            val: -1.0,
        });
        triplets.push(Triplet {
            row: upper_row,
            col: layout.angle_diff_upper_slack_col(row_idx),
            val: -1.0,
        });
        row_lower.push(f64::NEG_INFINITY);
        row_upper.push(acb.angmax_rad);
        // Lower: -θ_from + θ_to - σ_lower ≤ -angmin
        triplets.push(Triplet {
            row: lower_row,
            col: layout.dispatch.theta + acb.from_bus_idx,
            val: -1.0,
        });
        triplets.push(Triplet {
            row: lower_row,
            col: layout.dispatch.theta + acb.to_bus_idx,
            val: 1.0,
        });
        triplets.push(Triplet {
            row: lower_row,
            col: layout.angle_diff_lower_slack_col(row_idx),
            val: -1.0,
        });
        row_lower.push(f64::NEG_INFINITY);
        row_upper.push(-acb.angmin_rad);
    }

    // Append explicit contingency objective rows when the solve path
    // expanded contingencies up front.
    if let Some(explicit_ctg) = model_plan.explicit_contingency.as_ref() {
        let ctg_row_base = row_lower.len();
        crate::common::contingency::build_explicit_contingency_objective_rows(
            crate::common::contingency::ExplicitContingencyObjectiveRowsInput {
                plan: explicit_ctg,
                thermal_penalty_curve: spec.thermal_penalty_curve,
                period_hours: &|period| spec.period_hours(period),
                row_base: ctg_row_base,
                base,
            },
        )
        .extend_into(&mut triplets, &mut row_lower, &mut row_upper);
    }

    debug_assert_eq!(
        row_lower.len(),
        n_row,
        "row count mismatch: built {} rows, expected {}",
        row_lower.len(),
        n_row
    );

    let (a_start, a_index, a_value) = surge_opf::advanced::triplets_to_csc(&triplets, n_row, n_var);
    ScedProblemBuildState {
        row_plan,
        n_row,
        n_flow,
        n_branch_flow,
        n_fg_rows,
        fg_limits: network_rows.fg_limits.clone(),
        fg_shift_offsets: network_rows.fg_shift_offsets.clone(),
        row_lower,
        row_upper,
        a_start,
        a_index,
        a_value,
    }
}

pub(super) struct ScedProblemInput<'a> {
    pub network: &'a Network,
    pub solve: &'a DcSolveSession<'a>,
    pub problem: ScedProblemBuildState,
    pub problem_plan: ScedProblemPlan<'a>,
}

pub(super) struct ScedProblemState<'a> {
    pub problem_plan: ScedProblemPlan<'a>,
    pub problem: ScedProblemBuildState,
    pub solution: LpResult,
    pub dloss_dp_final: Vec<f64>,
    pub objective_col_cost: Vec<f64>,
}

fn maybe_mark_inactive_soft_slack(
    cols_to_fix: &mut Vec<usize>,
    solution: &LpResult,
    col: usize,
    tol_pu: f64,
) -> bool {
    let value = solution.x.get(col).copied().unwrap_or(0.0);
    if value.abs() <= tol_pu {
        cols_to_fix.push(col);
        true
    } else {
        false
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_near_zero_soft_slack_cols(
    solution: &LpResult,
    layout: &ScedLayout,
    reserve_layout: &ReserveLpLayout,
    n_bus: usize,
    n_pb_curt_segs: usize,
    n_pb_excess_segs: usize,
    n_branch_flow: usize,
    n_fg_rows: usize,
    n_iface_rows: usize,
    n_gen: usize,
    tol_pu: f64,
) -> Vec<usize> {
    let mut cols_to_fix = Vec::new();
    let mut found_inactive_soft_slack = false;

    for bus_idx in 0..n_bus {
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.pb_curtailment_bus_col(bus_idx),
            tol_pu,
        );
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.pb_excess_bus_col(bus_idx),
            tol_pu,
        );
    }

    for seg_idx in 0..n_pb_curt_segs {
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.pb_curtailment_seg_col(seg_idx),
            tol_pu,
        );
    }
    for seg_idx in 0..n_pb_excess_segs {
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.pb_excess_seg_col(seg_idx),
            tol_pu,
        );
    }

    for row_idx in 0..n_branch_flow {
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.branch_lower_slack_col(row_idx),
            tol_pu,
        );
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.branch_upper_slack_col(row_idx),
            tol_pu,
        );
    }
    for row_idx in 0..n_fg_rows {
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.flowgate_lower_slack_col(row_idx),
            tol_pu,
        );
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.flowgate_upper_slack_col(row_idx),
            tol_pu,
        );
    }
    for row_idx in 0..n_iface_rows {
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.interface_lower_slack_col(row_idx),
            tol_pu,
        );
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.interface_upper_slack_col(row_idx),
            tol_pu,
        );
    }
    for gen_idx in 0..n_gen {
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.ramp_up_slack_col(gen_idx),
            tol_pu,
        );
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.ramp_down_slack_col(gen_idx),
            tol_pu,
        );
    }
    for row_idx in 0..layout.n_angle_diff_rows {
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.angle_diff_lower_slack_col(row_idx),
            tol_pu,
        );
        found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
            &mut cols_to_fix,
            solution,
            layout.angle_diff_upper_slack_col(row_idx),
            tol_pu,
        );
    }

    for product in &reserve_layout.products {
        for slack_idx in 0..product.n_penalty_slacks {
            found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
                &mut cols_to_fix,
                solution,
                product.slack_offset + slack_idx,
                tol_pu,
            );
        }
        for zonal_idx in 0..product.n_zonal {
            found_inactive_soft_slack |= maybe_mark_inactive_soft_slack(
                &mut cols_to_fix,
                solution,
                product.zonal_slack_offset + zonal_idx,
                tol_pu,
            );
        }
    }

    if !found_inactive_soft_slack {
        return Vec::new();
    }

    cols_to_fix.sort_unstable();
    cols_to_fix.dedup();
    cols_to_fix
}

#[allow(clippy::too_many_arguments)]
fn maybe_run_pricing_cleanup(
    solver: &dyn surge_opf::backends::LpSolver,
    spec: &crate::common::spec::DispatchProblemSpec<'_>,
    network: &Network,
    gen_indices: &[usize],
    solution: &LpResult,
    prob: &mut surge_opf::backends::SparseProblem,
    layout: &ScedLayout,
    reserve_layout: &ReserveLpLayout,
    n_bus: usize,
    n_pb_curt_segs: usize,
    n_pb_excess_segs: usize,
    n_branch_flow: usize,
    n_fg_rows: usize,
    n_iface_rows: usize,
    n_gen: usize,
) -> Result<Option<LpResult>, ScedError> {
    let cols_to_fix = collect_near_zero_soft_slack_cols(
        solution,
        layout,
        reserve_layout,
        n_bus,
        n_pb_curt_segs,
        n_pb_excess_segs,
        n_branch_flow,
        n_fg_rows,
        n_iface_rows,
        n_gen,
        SCED_PRICING_SOFT_SLACK_TOL_PU,
    );
    if cols_to_fix.is_empty() {
        return Ok(None);
    }

    for (j, &gi) in gen_indices.iter().enumerate() {
        if network.generators[gi].is_storage() {
            continue;
        }
        let pg_col = layout.dispatch.pg + j;
        let lower = prob.col_lower.get(pg_col).copied().unwrap_or(0.0);
        let upper = prob.col_upper.get(pg_col).copied().unwrap_or(0.0);
        if lower <= 1e-9 {
            continue;
        }
        let col_start = prob.a_start[pg_col] as usize;
        let col_end = prob.a_start[pg_col + 1] as usize;
        for nz in col_start..col_end {
            let row = prob.a_index[nz] as usize;
            let coeff = prob.a_value[nz];
            if prob.row_lower[row].is_finite() {
                prob.row_lower[row] -= coeff * lower;
            }
            if prob.row_upper[row].is_finite() {
                prob.row_upper[row] -= coeff * lower;
            }
        }
        prob.col_lower[pg_col] = 0.0;
        prob.col_upper[pg_col] = (upper - lower).max(0.0);
    }

    for col in &cols_to_fix {
        prob.col_lower[*col] = 0.0;
        prob.col_upper[*col] = 0.0;
    }

    let repriced = solve_sparse_problem(solver, prob, spec.tolerance, None)?;
    if matches!(
        repriced.status,
        LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
    ) {
        debug!(
            n_fixed_soft_slacks = cols_to_fix.len(),
            tolerance_pu = SCED_PRICING_SOFT_SLACK_TOL_PU,
            "SCED pricing cleanup: fixed inactive soft slacks before extracting dual prices"
        );
        Ok(Some(repriced))
    } else {
        warn!(
            status = ?repriced.status,
            n_fixed_soft_slacks = cols_to_fix.len(),
            "SCED pricing cleanup did not converge; keeping primary pricing solution"
        );
        Ok(None)
    }
}

fn anchor_balance_duals(
    solver: &dyn surge_opf::backends::LpSolver,
    prob: &surge_opf::backends::SparseProblem,
    solution: &mut LpResult,
    objective_value: f64,
    n_flow: usize,
    island_refs: &surge_opf::advanced::IslandRefs,
    base: f64,
    tolerance: f64,
) -> Result<(), ScedError> {
    if island_refs.n_islands == 0 || island_refs.island_ref_bus.is_empty() {
        return Ok(());
    }

    let eps_pu = SCED_LMP_ANCHOR_EPS_MW / base;
    for (island_id, &ref_bus) in island_refs.island_ref_bus.iter().enumerate() {
        let row = n_flow + ref_bus;
        if row >= prob.row_lower.len() || row >= solution.row_dual.len() {
            continue;
        }

        let mut perturbed = prob.clone();
        perturbed.row_lower[row] -= eps_pu;
        perturbed.row_upper[row] -= eps_pu;

        let anchored = solve_sparse_problem(solver, &perturbed, tolerance, None)?;
        if !matches!(
            anchored.status,
            LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
        ) {
            warn!(
                island = island_id,
                ref_bus,
                status = ?anchored.status,
                "SCED LMP anchoring failed for island reference bus"
            );
            continue;
        }

        let true_ref_lmp = (anchored.objective - objective_value) / SCED_LMP_ANCHOR_EPS_MW;
        let raw_ref_lmp = -solution.row_dual[row] / base;
        let delta = true_ref_lmp - raw_ref_lmp;
        if delta.abs() <= 1e-8 {
            continue;
        }

        for (bus_idx, &bus_island) in island_refs.bus_island.iter().enumerate() {
            if bus_island == island_id {
                solution.row_dual[n_flow + bus_idx] -= delta * base;
            }
        }

        debug!(
            island = island_id,
            ref_bus, raw_ref_lmp, true_ref_lmp, delta, "SCED LMP anchoring applied"
        );
    }

    Ok(())
}

pub(super) fn solve_problem(
    input: ScedProblemInput<'_>,
) -> Result<ScedProblemState<'_>, ScedError> {
    let ScedProblemInput {
        network,
        solve,
        problem,
        mut problem_plan,
    } = input;
    let spec = &solve.spec;
    let solver = solve.solver.as_ref();
    let setup = &solve.setup;
    let bus_map = &solve.bus_map;
    let base = solve.base_mva;
    let model_plan = &problem_plan.model_plan;
    let layout = &model_plan.layout;
    let fg_rows = &model_plan.network_plan.fg_rows;
    let gen_bus_idx = &setup.gen_bus_idx;
    let n_bus = network.n_buses();
    let n_gen = setup.n_gen;
    let n_var = layout.dispatch.n_vars;
    let pg_offset = layout.dispatch.pg;
    let mut prob = build_sparse_problem(DcSparseProblemInput {
        n_col: n_var,
        n_row: problem.n_row,
        col_cost: std::mem::take(&mut problem_plan.columns.col_cost),
        col_lower: std::mem::take(&mut problem_plan.columns.col_lower),
        col_upper: std::mem::take(&mut problem_plan.columns.col_upper),
        row_lower: problem.row_lower.clone(),
        row_upper: problem.row_upper.clone(),
        a_start: problem.a_start.clone(),
        a_index: problem.a_index.clone(),
        a_value: problem.a_value.clone(),
        q_start: problem_plan.columns.q_start.take(),
        q_index: problem_plan.columns.q_index.take(),
        q_value: problem_plan.columns.q_value.take(),
        col_names: None,
        row_names: None,
        integrality: None,
    });
    let objective_col_cost = prob.col_cost.clone();
    let lp_opts = LpOptions {
        tolerance: spec.tolerance,
        ..Default::default()
    };
    let mut solution = solve_sparse_problem(solver, &prob, spec.tolerance, None)?;
    let anchor_prob = prob.clone();
    let anchor_objective = solution.objective;

    let mut fg_limits = problem.fg_limits.clone();
    let fg_shift_offsets = problem.fg_shift_offsets.clone();

    tighten_nomograms(DcNomogramTighteningInput {
        network,
        spec,
        solver,
        lp_opts: &lp_opts,
        lp_sol: &mut solution,
        lp_prob: &mut prob,
        fg_rows,
        fg_limits: &mut fg_limits,
        compute_flow_mw: |_, fgi, lp_sol| {
            let fg = &network.flowgates[fgi];
            let mut flow_pu = 0.0;
            for wbr in &fg.monitored {
                let fb = wbr.branch.from_bus;
                let tb = wbr.branch.to_bus;
                let ckt = &wbr.branch.circuit;
                let coeff = wbr.coefficient;
                if let (Some(&fi), Some(&ti)) = (bus_map.get(&fb), bus_map.get(&tb))
                    && let Some(br) = setup.branch_lookup.dc_branch(network, fb, tb, ckt.as_str())
                {
                    flow_pu += coeff * br.b_dc() * (lp_sol.x[fi] - lp_sol.x[ti]);
                }
            }
            flow_pu * base
        },
        apply_limit: |lp_prob, ri, new_limit| {
            let row = problem.n_branch_flow + ri;
            lp_prob.row_lower[row] = -new_limit / base - fg_shift_offsets[ri];
            lp_prob.row_upper[row] = new_limit / base - fg_shift_offsets[ri];
        },
    })?;

    let mut dloss_dp_final = vec![0.0_f64; n_bus];
    if spec.use_loss_factors
        && n_bus > 1
        && matches!(
            solution.status,
            LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
        )
    {
        let mut gen_csc_pos: Vec<Option<usize>> = Vec::with_capacity(n_gen);
        for (j, &gbi) in gen_bus_idx.iter().enumerate().take(n_gen) {
            let target_row = (problem.n_flow + gbi) as i32;
            let col = pg_offset + j;
            let col_start = prob.a_start[col] as usize;
            let col_end = prob.a_start[col + 1] as usize;
            let mut found = None;
            for pos in col_start..col_end {
                if prob.a_index[pos] == target_row {
                    found = Some(pos);
                    break;
                }
            }
            gen_csc_pos.push(found);
        }

        let pb_start = problem.n_flow;
        let orig_rhs: Vec<f64> = (0..n_bus).map(|i| prob.row_lower[pb_start + i]).collect();
        let monitored_branches: Vec<usize> = (0..network.n_branches()).collect();
        let loss_ptdf = surge_dc::compute_ptdf(
            network,
            &surge_dc::PtdfRequest::for_branches(&monitored_branches),
        )
        .map_err(|e| ScedError::SolverError(format!("PTDF for loss factors: {e}")))?;
        let mut prev_dloss = vec![0.0_f64; n_bus];
        for loss_iter in 0..spec.max_loss_factor_iters {
            let theta: Vec<f64> = solution.x[0..n_bus].to_vec();
            dloss_dp_final = surge_opf::advanced::compute_dc_loss_sensitivities(
                network, &theta, bus_map, &loss_ptdf,
            );
            let total_loss = surge_opf::compute_total_dc_losses(network, &theta, bus_map);

            let max_delta = dloss_dp_final
                .iter()
                .zip(prev_dloss.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f64, f64::max);
            if loss_iter > 0 && max_delta < spec.loss_factor_tol {
                debug!(iter = loss_iter, max_delta, "SCED loss factors converged");
                break;
            }
            prev_dloss.clone_from(&dloss_dp_final);

            for j in 0..n_gen {
                if let Some(pos) = gen_csc_pos[j] {
                    let pf = (1.0 - dloss_dp_final[gen_bus_idx[j]]).clamp(0.5, 1.5);
                    prob.a_value[pos] = -pf;
                }
            }

            // Build per-bus load array from network.loads.
            let mut bus_load_mw = vec![0.0_f64; n_bus];
            for load in &network.loads {
                if load.in_service {
                    if let Some(&bi) = bus_map.get(&load.bus) {
                        bus_load_mw[bi] += load.active_power_demand_mw;
                    }
                }
            }
            let total_load: f64 = bus_load_mw.iter().map(|v| v.max(0.0)).sum();
            for (i, &orig_rhs_i) in orig_rhs.iter().enumerate().take(n_bus) {
                let loss_share = if total_load > 1e-6 {
                    total_loss * (bus_load_mw[i].max(0.0) / total_load)
                } else {
                    total_loss / n_bus as f64
                };
                let rhs = orig_rhs_i - loss_share;
                prob.row_lower[pb_start + i] = rhs;
                prob.row_upper[pb_start + i] = rhs;
            }

            solution = solver
                .solve(&prob, &lp_opts)
                .map_err(ScedError::SolverError)?;
            if !matches!(
                solution.status,
                LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
            ) {
                warn!(
                    iter = loss_iter,
                    "SCED loss factor iteration: solver did not converge"
                );
                break;
            }
        }
    }

    if !matches!(
        solution.status,
        LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
    ) {
        warn!(
            status = ?solution.status,
            iterations = solution.iterations,
            buses = n_bus,
            generators = n_gen,
            "SCED: solver did not converge"
        );
        return Err(ScedError::NotConverged {
            iterations: solution.iterations,
        });
    }

    let skip_pricing_cleanup = std::env::var("SURGE_SKIP_SCED_PRICING_CLEANUP")
        .map(|value| value != "0")
        .unwrap_or(false);
    if !skip_pricing_cleanup {
        if let Some(repriced) = maybe_run_pricing_cleanup(
            solver,
            spec,
            network,
            &setup.gen_indices,
            &solution,
            &mut prob,
            layout,
            &model_plan.active.reserve_layout,
            n_bus,
            model_plan.n_pb_curt_segs,
            model_plan.n_pb_excess_segs,
            problem.n_branch_flow,
            problem.n_fg_rows,
            model_plan.network_plan.iface_rows.len(),
            n_gen,
        )? {
            solution.row_dual = repriced.row_dual;
            solution.col_dual = repriced.col_dual;
        }
    }

    anchor_balance_duals(
        solver,
        &anchor_prob,
        &mut solution,
        anchor_objective,
        problem.n_flow,
        &solve.island_refs,
        base,
        spec.tolerance,
    )?;

    Ok(ScedProblemState {
        problem_plan,
        problem,
        solution,
        dloss_dp_final,
        objective_col_cost,
    })
}
