// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCED pre-solve validation and network-row planning helpers.

use super::bounds::{ScedBoundsInput, build_variable_bounds};
use super::objective::{ScedObjectiveInput, build_objective};
use crate::common::builders::{self, PowerBalanceExtraTerm};
use crate::common::contingency::ExplicitContingencyObjectivePlan;
use crate::common::dc::{DcModelContext, DcSolveSession};
use crate::common::network::DcNetworkPlan;
use crate::common::reserves::{ReserveLpCtx, ReserveLpLayout};
use crate::common::runtime::DispatchPeriodContext;
use crate::common::setup::{DispatchBranchLookup, ResolvedMonitoredElement};
use crate::common::spec::DispatchProblemSpec;
use crate::error::ScedError;
use crate::sced::layout::ScedLayout;
use surge_hvdc::interop::{apply_dc_grid_injections, dc_grid_injections};
use surge_network::Network;
use surge_network::market::DispatchableLoad;
use surge_opf::advanced::IslandRefs;
use tracing::warn;

pub(super) fn prepare_network(base_network: &Network) -> Network {
    // TCSC series reactance changes the DC B-matrix; reactive FACTS devices are
    // no-ops in the DC formulation.
    let mut network = surge_ac::expand_facts(base_network).into_owned();

    // Apply MTDC injections as fixed bus demand adjustments using the flat-start
    // DC-side solution of the converter network.
    {
        if let Ok(dc_grid) = dc_grid_injections(&network) {
            apply_dc_grid_injections(&mut network, &dc_grid.injections, true);
        }
    }

    // In SCED, only quick-start units can provide non-spin reserve.
    for generator in &mut network.generators {
        if !generator.quick_start {
            if let Some(ref mut market) = generator.market {
                market
                    .reserve_offers
                    .retain(|offer| offer.product_id != "nspin");
            }
        }
    }

    network
}

pub(super) struct ScedValidationInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub gen_indices: &'a [usize],
    pub n_gen: usize,
}

pub(super) fn validate_problem_inputs(input: ScedValidationInput<'_>) -> Result<(), ScedError> {
    if let Some(rm) = input.spec.regulation_eligible
        && rm.len() != input.n_gen
    {
        warn!(
            provided = rm.len(),
            expected = input.n_gen,
            "regulation_eligible length mismatch; trailing generators default to regulation-enabled"
        );
    }

    let total_load_mw: f64 = input
        .network
        .loads
        .iter()
        .filter(|l| l.in_service)
        .map(|l| l.active_power_demand_mw)
        .sum();
    let total_capacity: f64 = input
        .gen_indices
        .iter()
        .map(|&gi| input.network.generators[gi].pmax)
        .sum();
    if total_capacity < total_load_mw {
        warn!(
            load_mw = total_load_mw,
            capacity_mw = total_capacity,
            deficit_mw = total_load_mw - total_capacity,
            "SCED: insufficient generation capacity to serve load; continuing with penalized balance slack"
        );
    }

    Ok(())
}

pub(super) struct ScedModelPlan<'a> {
    pub layout: super::layout::ScedLayout,
    pub active: super::layout::ScedActiveInputs<'a>,
    pub network_plan: DcNetworkPlan,
    pub n_blk_res_vars: usize,
    pub n_pb_curt_segs: usize,
    pub n_pb_excess_segs: usize,
    pub explicit_contingency: Option<ExplicitContingencyObjectivePlan>,
}

pub(super) struct ScedModelPlanInput<'a> {
    pub network: &'a Network,
    pub context: crate::common::runtime::DispatchPeriodContext<'a>,
    pub solve: &'a DcSolveSession<'a>,
}

pub(super) fn build_model_plan(
    input: ScedModelPlanInput<'_>,
) -> Result<ScedModelPlan<'_>, ScedError> {
    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let bus_map = &input.solve.bus_map;
    validate_problem_inputs(ScedValidationInput {
        network: input.network,
        spec,
        gen_indices: &setup.gen_indices,
        n_gen: setup.n_gen,
    })?;

    let network_plan = DcModelContext::build_network_plan(
        input.network,
        spec,
        bus_map,
        Some(&setup.par_branch_set),
    );
    let layout_plan = super::layout::build_layout_plan(super::layout::ScedLayoutPlanInput {
        network: input.network,
        spec,
        context: input.context,
        gen_indices: &setup.gen_indices,
        reserve_products: &setup.r_products,
        system_reserve_requirements: &setup.r_sys_reqs,
        zonal_reserve_requirements: &setup.r_zonal_reqs,
        hvdc_band_offsets_rel: &setup.hvdc_band_offsets_rel,
        n_bus: input.network.n_buses(),
        n_gen: setup.n_gen,
        n_storage: setup.n_storage,
        n_sto_dis_epi: setup.n_sto_dis_epi,
        n_sto_ch_epi: setup.n_sto_ch_epi,
        n_hvdc_vars: setup.n_hvdc_vars,
        n_pwl_gen: setup.n_pwl_gen,
        n_block_vars: setup.n_block_vars,
        has_per_block_reserves: setup.has_per_block_reserves,
        n_branch_flow: network_plan.constrained_branches.len(),
        n_fg_rows: network_plan.fg_rows.len(),
        n_iface_rows: network_plan.iface_rows.len(),
        n_angle_diff_rows: network_plan.angle_constrained_branches.len(),
        n_explicit_ctg_vars: 0,
    });

    Ok(ScedModelPlan {
        layout: layout_plan.layout,
        active: layout_plan.active,
        network_plan,
        n_blk_res_vars: layout_plan.n_blk_res_vars,
        n_pb_curt_segs: layout_plan.n_pb_curt_segs,
        n_pb_excess_segs: layout_plan.n_pb_excess_segs,
        explicit_contingency: None,
    })
}

/// Build an SCED model plan with explicit contingency columns allocated.
///
/// This is the analogue of the SCUC's explicit-security path: each contingency
/// case gets a penalty variable, plus one worst-case and one avg-case column for
/// the (single) period.
#[allow(dead_code)]
pub(super) fn build_explicit_model_plan<'a>(
    input: ScedModelPlanInput<'a>,
    cases: &[crate::common::spec::ExplicitContingencyCase],
    case_flowgates: &[crate::common::spec::ExplicitContingencyFlowgate],
) -> Result<ScedModelPlan<'a>, ScedError> {
    use crate::common::contingency::{ExplicitContingencyCasePlan, ExplicitContingencyPeriodPlan};
    use std::collections::HashMap;

    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let bus_map = &input.solve.bus_map;
    validate_problem_inputs(ScedValidationInput {
        network: input.network,
        spec,
        gen_indices: &setup.gen_indices,
        n_gen: setup.n_gen,
    })?;

    let network_plan = DcModelContext::build_network_plan(
        input.network,
        spec,
        bus_map,
        Some(&setup.par_branch_set),
    );
    let n_fg_rows = network_plan.fg_rows.len();

    // Compute explicit contingency variable count: n_cases penalty + 1 worst + 1 avg
    let n_cases = cases.len();
    let n_explicit_ctg_vars = if n_cases > 0 { n_cases + 2 } else { 0 };

    let layout_plan = super::layout::build_layout_plan(super::layout::ScedLayoutPlanInput {
        network: input.network,
        spec,
        context: input.context,
        gen_indices: &setup.gen_indices,
        reserve_products: &setup.r_products,
        system_reserve_requirements: &setup.r_sys_reqs,
        zonal_reserve_requirements: &setup.r_zonal_reqs,
        hvdc_band_offsets_rel: &setup.hvdc_band_offsets_rel,
        n_bus: input.network.n_buses(),
        n_gen: setup.n_gen,
        n_storage: setup.n_storage,
        n_sto_dis_epi: setup.n_sto_dis_epi,
        n_sto_ch_epi: setup.n_sto_ch_epi,
        n_hvdc_vars: setup.n_hvdc_vars,
        n_pwl_gen: setup.n_pwl_gen,
        n_block_vars: setup.n_block_vars,
        has_per_block_reserves: setup.has_per_block_reserves,
        n_branch_flow: network_plan.constrained_branches.len(),
        n_fg_rows,
        n_iface_rows: network_plan.iface_rows.len(),
        n_angle_diff_rows: network_plan.angle_constrained_branches.len(),
        n_explicit_ctg_vars,
    });

    let explicit_contingency = if n_cases > 0 {
        let ctg_base = layout_plan
            .layout
            .explicit_ctg_base
            .expect("explicit_ctg_base must be set when n_explicit_ctg_vars > 0");

        // Build flowgate row index map (same logic as SCUC plan.rs)
        let flowgate_row_by_index: HashMap<usize, usize> = network_plan
            .fg_rows
            .iter()
            .enumerate()
            .map(|(row_idx, &fg_idx)| (fg_idx, row_idx))
            .collect();
        let mut flowgate_row_cases = vec![None; n_fg_rows];
        let mut case_flowgate_rows = vec![Vec::<usize>::new(); n_cases];
        for mapping in case_flowgates {
            let Some(&flowgate_row) = flowgate_row_by_index.get(&mapping.flowgate_idx) else {
                continue;
            };
            if let Some(slot) = flowgate_row_cases.get_mut(flowgate_row) {
                *slot = Some(mapping.case_index);
            }
            if let Some(rows) = case_flowgate_rows.get_mut(mapping.case_index) {
                rows.push(flowgate_row);
            }
        }
        for rows in &mut case_flowgate_rows {
            rows.sort_unstable();
        }

        let case_penalty_base = ctg_base;
        let worst_case_base = ctg_base + n_cases;
        let avg_case_base = worst_case_base + 1;

        let layout = &layout_plan.layout;
        let plan_cases = cases
            .iter()
            .enumerate()
            .map(|(case_index, case)| {
                let rows = std::mem::take(&mut case_flowgate_rows[case_index]);
                let slack_cols = rows
                    .iter()
                    .map(|&fg_row| {
                        (
                            layout.flowgate_lower_slack_col(fg_row),
                            layout.flowgate_upper_slack_col(fg_row),
                        )
                    })
                    .collect();
                ExplicitContingencyCasePlan {
                    case_index,
                    period: case.period,
                    penalty_col: case_penalty_base + case_index,
                    flowgate_slack_cols: slack_cols,
                }
            })
            .collect::<Vec<_>>();

        // SCED is single-period: only one period entry
        let period_case_indices: Vec<usize> = plan_cases.iter().map(|c| c.case_index).collect();
        let periods = vec![ExplicitContingencyPeriodPlan {
            case_indices: period_case_indices,
            worst_case_col: worst_case_base,
            avg_case_col: avg_case_base,
        }];

        Some(ExplicitContingencyObjectivePlan {
            case_penalty_base,
            worst_case_base,
            avg_case_base,
            cases: plan_cases,
            periods,
            flowgate_row_cases,
        })
    } else {
        None
    };

    Ok(ScedModelPlan {
        layout: layout_plan.layout,
        active: layout_plan.active,
        network_plan,
        n_blk_res_vars: layout_plan.n_blk_res_vars,
        n_pb_curt_segs: layout_plan.n_pb_curt_segs,
        n_pb_excess_segs: layout_plan.n_pb_excess_segs,
        explicit_contingency,
    })
}

pub(super) struct ScedColumnBuildInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub context: crate::common::runtime::DispatchPeriodContext<'a>,
    pub island_refs: &'a IslandRefs,
    pub reserve_layout: &'a ReserveLpLayout,
    pub reserve_ctx: &'a ReserveLpCtx<'a>,
    pub gen_indices: &'a [usize],
    pub gen_blocks: &'a [Vec<crate::common::blocks::DispatchBlock>],
    pub gen_block_start: &'a [usize],
    pub storage_gen_local: &'a [(usize, usize, usize)],
    pub hvdc_band_offsets_abs: &'a [usize],
    pub pwl_gen_info: &'a [(usize, Vec<(f64, f64)>)],
    pub dl_list: &'a [&'a DispatchableLoad],
    pub dl_orig_idx: &'a [usize],
    pub active_vbids: &'a [usize],
    pub effective_co2_price: f64,
    pub effective_co2_rate: &'a [f64],
    pub layout: &'a ScedLayout,
    pub n_var: usize,
    pub n_bus: usize,
    pub n_gen: usize,
    pub n_storage: usize,
    pub n_hvdc_vars: usize,
    pub n_pwl_gen: usize,
    pub n_vbid: usize,
    pub n_block_vars: usize,
    pub n_blk_res_vars: usize,
    pub n_sto_dis_epi: usize,
    pub n_sto_ch_epi: usize,
    pub n_pb_curt_segs: usize,
    pub n_pb_excess_segs: usize,
    pub n_branch_flow: usize,
    pub n_fg_rows: usize,
    pub n_iface_rows: usize,
    pub is_block_mode: bool,
    pub has_per_block_reserves: bool,
    pub explicit_contingency: Option<&'a ExplicitContingencyObjectivePlan>,
    pub base: f64,
}

pub(super) struct ScedColumnBuildState {
    pub col_cost: Vec<f64>,
    pub c0_total: f64,
    pub q_start: Option<Vec<i32>>,
    pub q_index: Option<Vec<i32>>,
    pub q_value: Option<Vec<f64>>,
    pub col_lower: Vec<f64>,
    pub col_upper: Vec<f64>,
}

pub(super) fn build_column_state(input: ScedColumnBuildInput<'_>) -> ScedColumnBuildState {
    let mut objective = build_objective(ScedObjectiveInput {
        network: input.network,
        spec: input.spec,
        reserve_ctx: input.reserve_ctx,
        reserve_layout: input.reserve_layout,
        gen_indices: input.gen_indices,
        gen_blocks: input.gen_blocks,
        storage_gen_local: input.storage_gen_local,
        hvdc_band_offsets_abs: input.hvdc_band_offsets_abs,
        pwl_gen_info: input.pwl_gen_info,
        dl_list: input.dl_list,
        dl_orig_idx: input.dl_orig_idx,
        active_vbids: input.active_vbids,
        effective_co2_price: input.effective_co2_price,
        effective_co2_rate: input.effective_co2_rate,
        period: input.context.period,
        layout: input.layout,
        n_var: input.n_var,
        n_bus: input.n_bus,
        n_gen: input.n_gen,
        n_storage: input.n_storage,
        n_hvdc_vars: input.n_hvdc_vars,
        n_pwl_gen: input.n_pwl_gen,
        n_vbid: input.n_vbid,
        n_block_vars: input.n_block_vars,
        n_blk_res_vars: input.n_blk_res_vars,
        n_sto_dis_epi: input.n_sto_dis_epi,
        n_sto_ch_epi: input.n_sto_ch_epi,
        n_pb_curt_segs: input.n_pb_curt_segs,
        n_pb_excess_segs: input.n_pb_excess_segs,
        n_branch_flow: input.n_branch_flow,
        n_fg_rows: input.n_fg_rows,
        n_iface_rows: input.n_iface_rows,
        is_block_mode: input.is_block_mode,
        pg_offset: input.layout.dispatch.pg,
        sto_ch_offset: input.layout.dispatch.sto_ch,
        sto_dis_offset: input.layout.dispatch.sto_dis,
        sto_epi_dis_offset: input.layout.dispatch.sto_epi_dis,
        sto_epi_ch_offset: input.layout.dispatch.sto_epi_ch,
        e_g_offset: input.layout.dispatch.e_g,
        dl_offset: input.layout.dispatch.dl,
        vbid_offset: input.layout.dispatch.vbid,
        block_offset: input.layout.dispatch.block,
        base: input.base,
    });

    let bounds = build_variable_bounds(ScedBoundsInput {
        network: input.network,
        spec: input.spec,
        context: input.context,
        island_refs: input.island_refs,
        reserve_layout: input.reserve_layout,
        reserve_ctx: input.reserve_ctx,
        gen_indices: input.gen_indices,
        gen_blocks: input.gen_blocks,
        gen_block_start: input.gen_block_start,
        storage_gen_local: input.storage_gen_local,
        hvdc_band_offsets_abs: input.hvdc_band_offsets_abs,
        dl_list: input.dl_list,
        dl_orig_idx: input.dl_orig_idx,
        active_vbids: input.active_vbids,
        n_var: input.n_var,
        n_bus: input.n_bus,
        n_pwl_gen: input.n_pwl_gen,
        n_sto_dis_epi: input.n_sto_dis_epi,
        n_sto_ch_epi: input.n_sto_ch_epi,
        n_block_vars: input.n_block_vars,
        n_blk_res_vars: input.n_blk_res_vars,
        n_branch_flow: input.n_branch_flow,
        n_fg_rows: input.n_fg_rows,
        n_iface_rows: input.n_iface_rows,
        is_block_mode: input.is_block_mode,
        has_per_block_reserves: input.has_per_block_reserves,
        layout: input.layout,
        theta_offset: input.layout.dispatch.theta,
        pg_offset: input.layout.dispatch.pg,
        sto_ch_offset: input.layout.dispatch.sto_ch,
        sto_dis_offset: input.layout.dispatch.sto_dis,
        sto_soc_offset: input.layout.dispatch.sto_soc,
        sto_epi_dis_offset: input.layout.dispatch.sto_epi_dis,
        sto_epi_ch_offset: input.layout.dispatch.sto_epi_ch,
        e_g_offset: input.layout.dispatch.e_g,
        dl_offset: input.layout.dispatch.dl,
        vbid_offset: input.layout.dispatch.vbid,
        block_offset: input.layout.dispatch.block,
        blk_res_offset: input.layout.dispatch.block_reserve,
        base: input.base,
        col_cost: &mut objective.col_cost,
    });

    let mut col_lower = bounds.col_lower;
    let mut col_upper = bounds.col_upper;

    // Wire up explicit contingency columns: bounds and objective.
    if let Some(explicit_ctg) = input.explicit_contingency {
        let dt_h = input.spec.period_hours(input.context.period);
        let _thermal_penalty =
            input.spec.thermal_penalty_curve.marginal_cost_at(0.0) * input.base * dt_h;

        // Zero out flowgate slack costs for flowgates owned by
        // explicit contingency cases — their penalty is captured
        // through the per-case penalty variable instead.
        for row_idx in 0..input.n_fg_rows {
            let is_explicit_ctg_flowgate = explicit_ctg
                .flowgate_row_cases
                .get(row_idx)
                .copied()
                .flatten()
                .is_some();
            if is_explicit_ctg_flowgate {
                objective.col_cost[input.layout.flowgate_lower_slack_col(row_idx)] = 0.0;
                objective.col_cost[input.layout.flowgate_upper_slack_col(row_idx)] = 0.0;
            }
        }

        // Case penalty: [0, +inf) cost 0
        for case in &explicit_ctg.cases {
            col_lower[case.penalty_col] = 0.0;
            col_upper[case.penalty_col] = f64::INFINITY;
            objective.col_cost[case.penalty_col] = 0.0;
        }

        // Worst/avg: [0, +inf) cost 1.0; empty periods pinned to zero
        for period in &explicit_ctg.periods {
            if period.case_indices.is_empty() {
                col_lower[period.worst_case_col] = 0.0;
                col_upper[period.worst_case_col] = 0.0;
                col_lower[period.avg_case_col] = 0.0;
                col_upper[period.avg_case_col] = 0.0;
                objective.col_cost[period.worst_case_col] = 0.0;
                objective.col_cost[period.avg_case_col] = 0.0;
            } else {
                col_lower[period.worst_case_col] = 0.0;
                col_upper[period.worst_case_col] = f64::INFINITY;
                col_lower[period.avg_case_col] = 0.0;
                col_upper[period.avg_case_col] = f64::INFINITY;
                objective.col_cost[period.worst_case_col] = 1.0;
                objective.col_cost[period.avg_case_col] = 1.0;
            }
        }
    }

    ScedColumnBuildState {
        col_cost: objective.col_cost,
        c0_total: objective.c0_total,
        q_start: objective.q_start,
        q_index: objective.q_index,
        q_value: objective.q_value,
        col_lower,
        col_upper,
    }
}

pub(super) struct ScedNetworkRowsPlan {
    pub fg_limits: Vec<f64>,
    pub fg_shift_offsets: Vec<f64>,
    pub pbusinj: Vec<f64>,
    pub hvdc_loss_a_bus: Vec<f64>,
    pub power_balance_extra_terms: Vec<PowerBalanceExtraTerm>,
}

pub(super) struct ScedNetworkRowsPlanInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub layout: &'a ScedLayout,
    pub branch_lookup: &'a DispatchBranchLookup,
    pub resolved_flowgates: &'a [ResolvedMonitoredElement],
    pub par_branch_set: &'a std::collections::HashSet<usize>,
    pub fg_rows: &'a [usize],
    pub hvdc_from_idx: &'a [Option<usize>],
    pub bus_map: &'a std::collections::HashMap<u32, usize>,
    pub n_bus: usize,
    pub base: f64,
}

pub(super) fn build_network_rows_plan(input: ScedNetworkRowsPlanInput<'_>) -> ScedNetworkRowsPlan {
    for ps in input.spec.par_setpoints {
        if let Some(br) = input.branch_lookup.in_service_branch(
            input.network,
            ps.from_bus,
            ps.to_bus,
            ps.circuit.as_str(),
        ) && br.rating_a_mva > 0.0
            && ps.target_mw.abs() > br.rating_a_mva
        {
            warn!(
                from_bus = ps.from_bus,
                to_bus = ps.to_bus,
                circuit = %ps.circuit,
                target_mw = ps.target_mw,
                rate_a = br.rating_a_mva,
                "PAR setpoint target_mw exceeds branch rate_a thermal limit"
            );
        }
    }

    let pbusinj = builders::compute_par_injection(
        input.network,
        input.spec,
        input.bus_map,
        input.n_bus,
        input.par_branch_set,
        input.branch_lookup,
        input.base,
    );
    let hvdc_loss_a_bus = builders::compute_hvdc_loss_injection(
        input.spec,
        input.hvdc_from_idx,
        input.n_bus,
        input.base,
    );
    let (fg_limits, fg_shift_offsets) = builders::init_flowgate_nomogram_data(
        input.network,
        input.fg_rows,
        input.resolved_flowgates,
    );

    let mut power_balance_extra_terms = Vec::with_capacity(2 * input.n_bus);
    for bus_idx in 0..input.n_bus {
        power_balance_extra_terms.push(PowerBalanceExtraTerm {
            bus_idx,
            col: input.layout.pb_curtailment_bus_col(bus_idx),
            coeff: -1.0,
        });
        power_balance_extra_terms.push(PowerBalanceExtraTerm {
            bus_idx,
            col: input.layout.pb_excess_bus_col(bus_idx),
            coeff: 1.0,
        });
    }

    ScedNetworkRowsPlan {
        fg_limits,
        fg_shift_offsets,
        pbusinj,
        hvdc_loss_a_bus,
        power_balance_extra_terms,
    }
}

pub(super) struct ScedProblemPlan<'a> {
    pub model_plan: &'a ScedModelPlan<'a>,
    pub columns: ScedColumnBuildState,
    pub network_rows: ScedNetworkRowsPlan,
}

pub(super) struct ScedProblemPlanInput<'a> {
    pub network: &'a Network,
    pub context: DispatchPeriodContext<'a>,
    pub solve: &'a DcSolveSession<'a>,
    pub model_plan: &'a ScedModelPlan<'a>,
}

pub(super) fn build_problem_plan(input: ScedProblemPlanInput<'_>) -> ScedProblemPlan<'_> {
    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let bus_map = &input.solve.bus_map;
    let island_refs = &input.solve.island_refs;
    let base = input.solve.base_mva;
    let model_plan = input.model_plan;
    let layout = &model_plan.layout;
    let active = &model_plan.active;
    let columns = build_column_state(ScedColumnBuildInput {
        network: input.network,
        spec,
        context: input.context,
        island_refs,
        reserve_layout: &active.reserve_layout,
        reserve_ctx: &active.reserve_ctx,
        gen_indices: &setup.gen_indices,
        gen_blocks: &setup.gen_blocks,
        gen_block_start: &setup.gen_block_start,
        storage_gen_local: &setup.storage_gen_local,
        hvdc_band_offsets_abs: &layout.hvdc_band_offsets_abs,
        pwl_gen_info: &setup.pwl_gen_info,
        dl_list: &active.dl_list,
        dl_orig_idx: &active.dl_orig_idx,
        active_vbids: &active.active_vbids,
        effective_co2_price: setup.effective_co2_price,
        effective_co2_rate: &setup.effective_co2_rate,
        layout,
        n_var: layout.dispatch.n_vars,
        n_bus: input.network.n_buses(),
        n_gen: setup.n_gen,
        n_storage: setup.n_storage,
        n_hvdc_vars: setup.n_hvdc_vars,
        n_pwl_gen: setup.n_pwl_gen,
        n_vbid: active.active_vbids.len(),
        n_block_vars: setup.n_block_vars,
        n_blk_res_vars: model_plan.n_blk_res_vars,
        n_sto_dis_epi: setup.n_sto_dis_epi,
        n_sto_ch_epi: setup.n_sto_ch_epi,
        n_pb_curt_segs: model_plan.n_pb_curt_segs,
        n_pb_excess_segs: model_plan.n_pb_excess_segs,
        n_branch_flow: model_plan.network_plan.constrained_branches.len(),
        n_fg_rows: model_plan.network_plan.fg_rows.len(),
        n_iface_rows: model_plan.network_plan.iface_rows.len(),
        is_block_mode: setup.is_block_mode,
        has_per_block_reserves: setup.has_per_block_reserves,
        explicit_contingency: model_plan.explicit_contingency.as_ref(),
        base,
    });
    let network_rows = build_network_rows_plan(ScedNetworkRowsPlanInput {
        network: input.network,
        spec,
        layout,
        branch_lookup: &setup.branch_lookup,
        resolved_flowgates: &setup.resolved_flowgates,
        par_branch_set: &setup.par_branch_set,
        fg_rows: &model_plan.network_plan.fg_rows,
        hvdc_from_idx: &model_plan.network_plan.hvdc_from_idx,
        bus_map,
        n_bus: input.network.n_buses(),
        base,
    });

    ScedProblemPlan {
        model_plan,
        columns,
        network_rows,
    }
}
