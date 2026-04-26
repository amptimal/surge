// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Post-solve SCED result extraction helpers.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::market::{CostCurve, VirtualBidResult};
use surge_solution::ParResult;
use tracing::{debug, info, warn};

use super::frequency::{check_frequency_security, compute_estimated_rocof, compute_system_inertia};
use super::problem::ScedProblemState;
use crate::common::dc::DcSolveSession;
use crate::common::reserves::{
    dispatchable_load_reserve_offer_for_period, generator_reserve_offer_for_period,
};
use crate::common::runtime::DispatchPeriodContext;
use crate::dispatch::{CommitmentMode, RawDispatchSolution};
use crate::economics::{SYSTEM_SUBJECT_ID, make_term, push_term, sum_terms};
use crate::report_ids::{
    branch_subject_id, dispatchable_load_resource_id, flowgate_subject_id, generator_resource_id,
    hvdc_link_id, interface_subject_id, reserve_requirement_subject_id,
};
use crate::request::Formulation;
use crate::request::{CommitmentPolicyKind, IntervalCoupling};
use crate::result::DispatchStudy;
use crate::result::{ConstraintKind, ConstraintScope};
#[cfg(test)]
use crate::solution::RawScedSolution;
use crate::solution::{RawConstraintPeriodResult, RawDispatchPeriodResult};
use surge_solution::{
    ObjectiveBucket, ObjectiveQuantityUnit, ObjectiveSubjectKind, ObjectiveTerm, ObjectiveTermKind,
};

pub(super) struct ScedExtractionInput<'a> {
    pub network: &'a Network,
    pub context: DispatchPeriodContext<'a>,
    pub solve: &'a DcSolveSession<'a>,
    pub problem_state: ScedProblemState<'a>,
    pub solve_time_secs: f64,
}

fn resolved_no_load_cost(cost: &CostCurve) -> f64 {
    match cost {
        CostCurve::Polynomial { coeffs, .. } => coeffs.last().copied().unwrap_or(0.0),
        CostCurve::PiecewiseLinear { points, .. } => {
            points.first().map(|(_, total)| *total).unwrap_or(0.0)
        }
    }
}

fn evaluate_polynomial_at(coeffs: &[f64], mw: f64) -> f64 {
    coeffs.iter().fold(0.0, |value, coeff| value * mw + coeff)
}

fn sced_block_fixed_cost(cost: &CostCurve, committed: bool, pmin_mw: f64) -> f64 {
    if committed {
        match cost {
            CostCurve::Polynomial { coeffs, .. } => evaluate_polynomial_at(coeffs, pmin_mw),
            CostCurve::PiecewiseLinear { points, .. } => {
                points.first().map(|(_, total)| *total).unwrap_or(0.0)
            }
        }
    } else {
        match cost {
            CostCurve::Polynomial { coeffs, .. } => match coeffs.len() {
                0 => 0.0,
                1 => coeffs[0],
                2 => coeffs[1],
                _ => coeffs[2],
            },
            CostCurve::PiecewiseLinear { .. } => 0.0,
        }
    }
}

fn build_objective_terms(input: &ScedExtractionInput<'_>) -> Vec<ObjectiveTerm> {
    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let problem_state = &input.problem_state;
    let model_plan = &problem_state.problem_plan.model_plan;
    let active_inputs = &model_plan.active;
    let layout = &model_plan.layout;
    let sol = &problem_state.solution;
    let base = input.solve.base_mva;
    let dt_h = spec.period_hours(input.context.period);
    let period_spec = spec.period(input.context.period);
    let col_cost = &problem_state.objective_col_cost;
    let mut terms = Vec::new();
    let pwl_gen_local_by_j: HashMap<usize, usize> = setup
        .pwl_gen_info
        .iter()
        .enumerate()
        .map(|(k, (j, _))| (*j, k))
        .collect();

    for (j, &gi) in setup.gen_indices.iter().enumerate() {
        let generator = &input.network.generators[gi];
        let resource_id = generator_resource_id(generator);
        if generator.is_storage() {
            continue;
        }
        let mut offer_cost_buf = None;
        let cost = crate::common::costs::resolve_cost_for_period_from_spec(
            gi,
            input.context.period,
            generator,
            spec,
            &mut offer_cost_buf,
            Some(generator.pmax),
        );
        let no_load_rate = resolved_no_load_cost(cost);
        let no_load_dollars = if setup.is_block_mode {
            let fixed_cost =
                sced_block_fixed_cost(cost, period_spec.is_committed(j), generator.pmin);
            no_load_rate.min(fixed_cost) * dt_h
        } else {
            no_load_rate * dt_h
        };
        let energy_dollars = if setup.is_block_mode {
            let block_start = setup.gen_block_start[j];
            let block_energy: f64 = setup.gen_blocks[j]
                .iter()
                .enumerate()
                .map(|(block_idx, block)| {
                    let col = layout.dispatch.block + block_start + block_idx;
                    sol.x[col] * base * block.marginal_cost * dt_h
                })
                .sum();
            let fixed_cost =
                sced_block_fixed_cost(cost, period_spec.is_committed(j), generator.pmin);
            (fixed_cost * dt_h - no_load_dollars).max(0.0) + block_energy
        } else if let Some(&pwl_idx) = pwl_gen_local_by_j.get(&j) {
            // `sol.x[e_g]` is the per-hour cost rate in $/h (epigraph rows
            // carry $/h units). Multiply by `dt_h` to match the LP objective
            // contribution (col_cost = dt_h on the e_g column).
            (sol.x[layout.dispatch.e_g + pwl_idx] * dt_h - no_load_dollars).max(0.0)
        } else {
            let pg_mw = sol.x[layout.dispatch.pg + j] * base;
            let total = cost.evaluate(pg_mw.max(0.0)) * dt_h;
            (total - no_load_dollars).max(0.0)
        };
        push_term(
            &mut terms,
            make_term(
                "energy",
                ObjectiveBucket::Energy,
                ObjectiveTermKind::GeneratorEnergy,
                ObjectiveSubjectKind::Resource,
                resource_id.clone(),
                energy_dollars,
                Some(sol.x[layout.dispatch.pg + j] * base * dt_h),
                Some(ObjectiveQuantityUnit::Mwh),
                None,
            ),
        );
        push_term(
            &mut terms,
            make_term(
                "no_load",
                ObjectiveBucket::NoLoad,
                ObjectiveTermKind::GeneratorNoLoad,
                ObjectiveSubjectKind::Resource,
                resource_id.clone(),
                no_load_dollars,
                None,
                None,
                None,
            ),
        );
        if setup.effective_co2_price > 0.0 {
            let pg_mw = sol.x[layout.dispatch.pg + j] * base;
            let carbon_dollars =
                pg_mw.max(0.0) * setup.effective_co2_rate[j] * setup.effective_co2_price * dt_h;
            push_term(
                &mut terms,
                make_term(
                    "carbon",
                    ObjectiveBucket::Adder,
                    ObjectiveTermKind::CarbonAdder,
                    ObjectiveSubjectKind::Resource,
                    resource_id,
                    carbon_dollars,
                    Some(pg_mw.max(0.0) * dt_h),
                    Some(ObjectiveQuantityUnit::Mwh),
                    Some(setup.effective_co2_rate[j] * setup.effective_co2_price),
                ),
            );
        }
    }

    for &(s, _, gi) in &setup.storage_gen_local {
        let generator = &input.network.generators[gi];
        let resource_id = generator_resource_id(generator);
        let storage = generator
            .storage
            .as_ref()
            .expect("storage_gen_local only contains generators with storage");
        let charge_mw = sol.x[layout.dispatch.sto_ch + s] * base;
        let discharge_mw = sol.x[layout.dispatch.sto_dis + s] * base;
        match storage.dispatch_mode {
            surge_network::network::StorageDispatchMode::CostMinimization => {
                let discharge_rate =
                    storage.variable_cost_per_mwh + storage.degradation_cost_per_mwh;
                push_term(
                    &mut terms,
                    make_term(
                        "discharge",
                        ObjectiveBucket::Energy,
                        ObjectiveTermKind::StorageEnergy,
                        ObjectiveSubjectKind::Resource,
                        resource_id.clone(),
                        discharge_mw * discharge_rate * dt_h,
                        Some(discharge_mw * dt_h),
                        Some(ObjectiveQuantityUnit::Mwh),
                        Some(discharge_rate),
                    ),
                );
                push_term(
                    &mut terms,
                    make_term(
                        "charge",
                        ObjectiveBucket::Energy,
                        ObjectiveTermKind::StorageEnergy,
                        ObjectiveSubjectKind::Resource,
                        resource_id,
                        charge_mw * storage.degradation_cost_per_mwh * dt_h,
                        Some(charge_mw * dt_h),
                        Some(ObjectiveQuantityUnit::Mwh),
                        Some(storage.degradation_cost_per_mwh),
                    ),
                );
            }
            surge_network::network::StorageDispatchMode::OfferCurve
            | surge_network::network::StorageDispatchMode::SelfSchedule => {}
        }
    }
    for (k, (storage_local, _)) in setup.sto_dis_offer_info.iter().enumerate() {
        if let Some(&(_, _, gi)) = setup.storage_gen_local.get(*storage_local) {
            let resource_id = generator_resource_id(&input.network.generators[gi]);
            push_term(
                &mut terms,
                make_term(
                    "discharge_offer_epigraph",
                    ObjectiveBucket::Energy,
                    ObjectiveTermKind::StorageOfferEpigraph,
                    ObjectiveSubjectKind::Resource,
                    resource_id,
                    sol.x[layout.dispatch.sto_epi_dis + k],
                    None,
                    None,
                    None,
                ),
            );
        }
    }
    for (k, (storage_local, _)) in setup.sto_ch_bid_info.iter().enumerate() {
        if let Some(&(_, _, gi)) = setup.storage_gen_local.get(*storage_local) {
            let resource_id = generator_resource_id(&input.network.generators[gi]);
            push_term(
                &mut terms,
                make_term(
                    "charge_bid_epigraph",
                    ObjectiveBucket::Energy,
                    ObjectiveTermKind::StorageOfferEpigraph,
                    ObjectiveSubjectKind::Resource,
                    resource_id,
                    sol.x[layout.dispatch.sto_epi_ch + k],
                    None,
                    None,
                    None,
                ),
            );
        }
    }

    for (k, dl) in active_inputs.dl_list.iter().enumerate() {
        let (_, _, _, _, _, cost_model) = crate::common::costs::resolve_dl_for_period_from_spec(
            active_inputs.dl_orig_idx.get(k).copied().unwrap_or(k),
            input.context.period,
            dl,
            spec,
        );
        let resource_id = dispatchable_load_resource_id(
            dl,
            active_inputs.dl_orig_idx.get(k).copied().unwrap_or(k),
        );
        let result = crate::common::extraction::extract_dr_results(
            &sol.x,
            &active_inputs.dl_list,
            layout.dispatch.dl,
            &vec![0.0; input.network.n_buses()],
            &input.solve.bus_map,
            base,
            |off| off,
        );
        if let Some(load_result) = result.loads.get(k) {
            let col = layout.dispatch.dl + k;
            let exact_dollars = crate::common::costs::exact_dispatchable_load_objective_dollars(
                cost_model,
                sol.x[col],
                col_cost[col],
                base,
                dt_h,
            );
            push_term(
                &mut terms,
                make_term(
                    "energy",
                    ObjectiveBucket::Energy,
                    ObjectiveTermKind::DispatchableLoadEnergy,
                    ObjectiveSubjectKind::Resource,
                    resource_id,
                    exact_dollars,
                    Some(load_result.p_served_pu * base * dt_h),
                    Some(ObjectiveQuantityUnit::Mwh),
                    None,
                ),
            );
        }
    }

    for ap in &active_inputs.reserve_layout.products {
        for (j, &gi) in setup.gen_indices.iter().enumerate() {
            // Non-participants have no column — no award to report.
            let Some(col) = ap.gen_reserve_col(j) else {
                continue;
            };
            let award_mw = sol.x[col] * base;
            if award_mw.abs() <= 1e-9 {
                continue;
            }
            let resource_id = generator_resource_id(&input.network.generators[gi]);
            let rate = generator_reserve_offer_for_period(
                spec,
                gi,
                &input.network.generators[gi],
                &ap.product.id,
                input.context.period,
            )
            .map(|offer| offer.cost_per_mwh)
            .unwrap_or(0.0);
            push_term(
                &mut terms,
                make_term(
                    format!("reserve:{}", ap.product.id),
                    ObjectiveBucket::Reserve,
                    ObjectiveTermKind::ReserveProcurement,
                    ObjectiveSubjectKind::Resource,
                    resource_id,
                    award_mw * rate * dt_h,
                    Some(award_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    Some(rate * dt_h),
                ),
            );
        }
        // DL reserve awards are published per-BLOCK via uniform
        // pro-rata split of the consumer group's award. All members of
        // a group share the same $/MWh rate, so Σ (share × rate) ×
        // dt_h = group_award × rate × dt_h exactly — the total
        // reserve procurement cost matches the LP objective to the
        // bit.
        for (gi, group) in active_inputs
            .reserve_layout
            .dl_consumer_groups
            .iter()
            .enumerate()
        {
            let Some(col) = ap.dl_group_reserve_col(gi) else {
                continue;
            };
            let group_award_mw = sol.x[col] * base;
            if group_award_mw.abs() <= 1e-9 {
                continue;
            }
            let shares =
                crate::common::reserves::prorata_group_award_uniform(group, group_award_mw);
            for (offset, &k) in group.member_dl_indices.iter().enumerate() {
                let award_mw = shares[offset];
                if award_mw.abs() <= 1e-9 {
                    continue;
                }
                let dl = active_inputs.dl_list[k];
                let resource_id = dispatchable_load_resource_id(
                    dl,
                    active_inputs.dl_orig_idx.get(k).copied().unwrap_or(k),
                );
                let rate = dispatchable_load_reserve_offer_for_period(
                    spec,
                    active_inputs.dl_orig_idx.get(k).copied().unwrap_or(k),
                    dl,
                    &ap.product.id,
                    input.context.period,
                )
                .map(|offer| offer.cost_per_mwh)
                .unwrap_or(0.0);
                push_term(
                    &mut terms,
                    make_term(
                        format!("reserve:{}", ap.product.id),
                        ObjectiveBucket::Reserve,
                        ObjectiveTermKind::ReserveProcurement,
                        ObjectiveSubjectKind::Resource,
                        resource_id,
                        award_mw * rate * dt_h,
                        Some(award_mw),
                        Some(ObjectiveQuantityUnit::Mw),
                        Some(rate * dt_h),
                    ),
                );
            }
        }
        let reserve_subject_id = reserve_requirement_subject_id(&ap.product.id, None);
        for slack_idx in 0..ap.n_penalty_slacks {
            let col = ap.slack_offset + slack_idx;
            let slack_mw = sol.x[col] * base;
            let dollars = sol.x[col] * col_cost[col];
            let unit_rate = (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h));
            push_term(
                &mut terms,
                make_term(
                    format!("shortfall_segment_{slack_idx}"),
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::ReserveShortfall,
                    ObjectiveSubjectKind::ReserveRequirement,
                    reserve_subject_id.clone(),
                    dollars,
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    unit_rate,
                ),
            );
        }
        for (zi, zonal_req) in ap.zonal_reqs.iter().enumerate() {
            let col = ap.zonal_slack_offset + zi;
            let slack_mw = sol.x[col] * base;
            let dollars = sol.x[col] * col_cost[col];
            let unit_rate = (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h));
            push_term(
                &mut terms,
                make_term(
                    "zonal_shortfall",
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::ReserveShortfall,
                    ObjectiveSubjectKind::ReserveRequirement,
                    reserve_requirement_subject_id(&ap.product.id, Some(zonal_req.zone_id)),
                    dollars,
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    unit_rate,
                ),
            );
        }
    }

    for (k, hvdc) in spec.hvdc_links.iter().enumerate() {
        let subject_id = hvdc_link_id(hvdc, k);
        if hvdc.is_banded() {
            for (band_idx, band) in hvdc.bands.iter().enumerate() {
                let col = layout.hvdc_band_offsets_abs[k] + band_idx;
                let mw = sol.x[col] * base;
                push_term(
                    &mut terms,
                    make_term(
                        if band.id.is_empty() {
                            format!("band:{band_idx}")
                        } else {
                            format!("band:{}", band.id)
                        },
                        ObjectiveBucket::Energy,
                        ObjectiveTermKind::HvdcEnergy,
                        ObjectiveSubjectKind::HvdcLink,
                        subject_id.clone(),
                        sol.x[col] * col_cost[col],
                        Some(mw * dt_h),
                        Some(ObjectiveQuantityUnit::Mwh),
                        (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h)),
                    ),
                );
            }
        } else {
            let col = layout.hvdc_band_offsets_abs[k];
            let mw = sol.x[col] * base;
            push_term(
                &mut terms,
                make_term(
                    "dispatch",
                    ObjectiveBucket::Energy,
                    ObjectiveTermKind::HvdcEnergy,
                    ObjectiveSubjectKind::HvdcLink,
                    subject_id,
                    sol.x[col] * col_cost[col],
                    Some(mw * dt_h),
                    Some(ObjectiveQuantityUnit::Mwh),
                    (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }

    for (k, &bi) in active_inputs.active_vbids.iter().enumerate() {
        let vb = &spec.virtual_bids[bi];
        let col = layout.dispatch.vbid + k;
        let cleared_mw = sol.x[col] * base;
        push_term(
            &mut terms,
            make_term(
                "virtual_bid",
                ObjectiveBucket::Adder,
                ObjectiveTermKind::VirtualBid,
                ObjectiveSubjectKind::VirtualBid,
                format!("virtual_bid:{k}"),
                sol.x[col] * col_cost[col],
                Some(cleared_mw * dt_h),
                Some(ObjectiveQuantityUnit::Mwh),
                Some(vb.price_per_mwh),
            ),
        );
    }

    for (seg_idx, _) in spec.power_balance_penalty.curtailment.iter().enumerate() {
        let col = layout.pb_curtailment_seg_col(seg_idx);
        if col >= sol.x.len() || col >= col_cost.len() {
            continue;
        }
        let mw = sol.x[col] * base;
        push_term(
            &mut terms,
            make_term(
                format!("curtailment_segment_{seg_idx}"),
                ObjectiveBucket::Penalty,
                ObjectiveTermKind::PowerBalancePenalty,
                ObjectiveSubjectKind::System,
                SYSTEM_SUBJECT_ID,
                sol.x[col] * col_cost[col],
                Some(mw),
                Some(ObjectiveQuantityUnit::Mw),
                (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h)),
            ),
        );
    }
    for (seg_idx, _) in spec.power_balance_penalty.excess.iter().enumerate() {
        let col = layout.pb_excess_seg_col(seg_idx);
        if col >= sol.x.len() || col >= col_cost.len() {
            continue;
        }
        let mw = sol.x[col] * base;
        push_term(
            &mut terms,
            make_term(
                format!("excess_segment_{seg_idx}"),
                ObjectiveBucket::Penalty,
                ObjectiveTermKind::PowerBalancePenalty,
                ObjectiveSubjectKind::System,
                SYSTEM_SUBJECT_ID,
                sol.x[col] * col_cost[col],
                Some(mw),
                Some(ObjectiveQuantityUnit::Mw),
                (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h)),
            ),
        );
    }
    for (row_idx, &branch_idx) in model_plan
        .network_plan
        .constrained_branches
        .iter()
        .enumerate()
    {
        let branch = &input.network.branches[branch_idx];
        for (component_id, col) in [
            ("reverse", layout.branch_lower_slack_col(row_idx)),
            ("forward", layout.branch_upper_slack_col(row_idx)),
        ] {
            let slack_mw = sol.x[col] * base;
            push_term(
                &mut terms,
                make_term(
                    format!("thermal:{component_id}"),
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::ThermalLimitPenalty,
                    ObjectiveSubjectKind::Branch,
                    branch_subject_id(branch),
                    sol.x[col] * col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }
    for (row_idx, &fg_idx) in model_plan.network_plan.fg_rows.iter().enumerate() {
        let flowgate = &input.network.flowgates[fg_idx];
        for (component_id, col) in [
            ("reverse", layout.flowgate_lower_slack_col(row_idx)),
            ("forward", layout.flowgate_upper_slack_col(row_idx)),
        ] {
            let slack_mw = sol.x[col] * base;
            push_term(
                &mut terms,
                make_term(
                    format!("flowgate:{component_id}"),
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::FlowgatePenalty,
                    ObjectiveSubjectKind::Flowgate,
                    flowgate_subject_id(&flowgate.name, fg_idx),
                    sol.x[col] * col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }
    for (row_idx, &iface_idx) in model_plan.network_plan.iface_rows.iter().enumerate() {
        let interface = &input.network.interfaces[iface_idx];
        for (component_id, col) in [
            ("reverse", layout.interface_lower_slack_col(row_idx)),
            ("forward", layout.interface_upper_slack_col(row_idx)),
        ] {
            let slack_mw = sol.x[col] * base;
            push_term(
                &mut terms,
                make_term(
                    format!("interface:{component_id}"),
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::InterfacePenalty,
                    ObjectiveSubjectKind::Interface,
                    interface_subject_id(&interface.name, iface_idx),
                    sol.x[col] * col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }
    for (j, &gi) in setup.gen_indices.iter().enumerate() {
        let resource_id = generator_resource_id(&input.network.generators[gi]);
        for (component_id, col) in [
            ("up", layout.ramp_up_slack_col(j)),
            ("down", layout.ramp_down_slack_col(j)),
        ] {
            let slack_mw = sol.x[col] * base;
            push_term(
                &mut terms,
                make_term(
                    format!("ramp:{component_id}"),
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::RampPenalty,
                    ObjectiveSubjectKind::Resource,
                    resource_id.clone(),
                    sol.x[col] * col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }
    for (row_idx, acb) in model_plan
        .network_plan
        .angle_constrained_branches
        .iter()
        .enumerate()
    {
        let branch = &input.network.branches[acb.branch_idx];
        for (component_id, col) in [
            ("lower", layout.angle_diff_lower_slack_col(row_idx)),
            ("upper", layout.angle_diff_upper_slack_col(row_idx)),
        ] {
            let slack_rad = sol.x[col];
            push_term(
                &mut terms,
                make_term(
                    format!("angle:{component_id}"),
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::AngleDifferencePenalty,
                    ObjectiveSubjectKind::Branch,
                    branch_subject_id(branch),
                    sol.x[col] * col_cost[col],
                    Some(slack_rad),
                    Some(ObjectiveQuantityUnit::Rad),
                    (dt_h > 0.0).then_some(col_cost[col] / dt_h),
                ),
            );
        }
    }

    if let Some(eta_col) = layout.benders_eta_col() {
        push_term(
            &mut terms,
            make_term(
                "eta",
                ObjectiveBucket::Adder,
                ObjectiveTermKind::BendersEta,
                ObjectiveSubjectKind::System,
                SYSTEM_SUBJECT_ID,
                sol.x[eta_col] * col_cost[eta_col],
                None,
                None,
                Some(col_cost[eta_col]),
            ),
        );
    }

    if let Some(explicit_ctg) = model_plan.explicit_contingency.as_ref() {
        for period in &explicit_ctg.periods {
            push_term(
                &mut terms,
                make_term(
                    "explicit_ctg_worst_case",
                    ObjectiveBucket::Adder,
                    ObjectiveTermKind::ExplicitContingencyWorstCase,
                    ObjectiveSubjectKind::System,
                    SYSTEM_SUBJECT_ID,
                    sol.x[period.worst_case_col] * col_cost[period.worst_case_col],
                    None,
                    None,
                    Some(col_cost[period.worst_case_col]),
                ),
            );
            push_term(
                &mut terms,
                make_term(
                    "explicit_ctg_average_case",
                    ObjectiveBucket::Adder,
                    ObjectiveTermKind::ExplicitContingencyAverageCase,
                    ObjectiveSubjectKind::System,
                    SYSTEM_SUBJECT_ID,
                    sol.x[period.avg_case_col] * col_cost[period.avg_case_col],
                    None,
                    None,
                    Some(col_cost[period.avg_case_col]),
                ),
            );
        }
    }

    terms
}

pub(super) fn extract_solution(input: ScedExtractionInput<'_>) -> RawDispatchSolution {
    let mut objective_terms = build_objective_terms(&input);
    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let bus_map = &input.solve.bus_map;
    let island_refs = &input.solve.island_refs;
    let base = input.solve.base_mva;
    let problem_state = input.problem_state;
    let model_plan = &problem_state.problem_plan.model_plan;
    let layout = &model_plan.layout;
    let active_inputs = &model_plan.active;
    let network_plan = &model_plan.network_plan;
    let n_bus = input.network.n_buses();
    let n_gen = setup.gen_indices.len();
    let n_storage = setup.storage_gen_local.len();
    let n_dl = active_inputs.dl_list.len();
    let has_hvdc = !spec.hvdc_links.is_empty();

    let dt_h = spec.period_hours(input.context.period);
    let pg_offset = layout.dispatch.pg;
    let sto_ch_offset = layout.dispatch.sto_ch;
    let sto_dis_offset = layout.dispatch.sto_dis;
    let sto_soc_offset = layout.dispatch.sto_soc;
    let dl_offset = layout.dispatch.dl;
    let vbid_offset = layout.dispatch.vbid;
    let sol = &problem_state.solution;
    let pg_pu = &sol.x[pg_offset..pg_offset + n_gen];
    let pg_mw: Vec<f64> = pg_pu.iter().map(|&p| p * base).collect();
    let total_cost = sol.objective + problem_state.problem_plan.columns.c0_total;

    let rr = crate::common::reserves::extract_results(
        &active_inputs.reserve_layout,
        &sol.x,
        Some(&sol.row_dual),
        problem_state.problem.row_plan.reserve_row_base,
        n_gen,
        n_storage,
        n_dl,
        base,
    );

    let (hvdc_dispatch_mw, hvdc_band_dispatch_mw) = if has_hvdc {
        let (dispatch, bands) = crate::common::extraction::extract_hvdc_dispatch(
            &sol.x,
            spec,
            &layout.hvdc_band_offsets_abs,
            base,
            |off| off,
        );
        debug!(
            n_hvdc_links = spec.hvdc_links.len(),
            hvdc_dispatch_mw = ?dispatch,
            "SCED: HVDC dispatch"
        );
        (dispatch, bands)
    } else {
        (vec![], vec![])
    };

    for ap in &active_inputs.reserve_layout.products {
        let shortfall_mw = sol.x[ap.slack_offset] * base;
        if shortfall_mw > 1e-4 {
            let provided = rr.provided.get(&ap.product.id).copied().unwrap_or(0.0);
            warn!(
                product = %ap.product.id,
                shortfall_mw,
                requirement_mw = ap.system_req_cap_mw,
                provided_mw = provided,
                "SCED: reserve requirement not fully met — shortfall penalised"
            );
        }
    }

    let lmp: Vec<f64> = (0..n_bus)
        .map(|i| -sol.row_dual[problem_state.problem.n_flow + i] / base)
        .collect();
    let mut lmp = lmp;
    crate::common::extraction::correct_dc_lmp_orientation(&mut lmp, island_refs);

    let branch_shadow_prices =
        if spec.enforce_thermal_limits && !network_plan.constrained_branches.is_empty() {
            crate::common::extraction::extract_branch_shadow_prices_single(
                &sol.row_dual,
                problem_state.problem.n_branch_flow,
                base,
            )
        } else {
            vec![]
        };
    let flowgate_shadow_prices = if spec.enforce_flowgates && !network_plan.fg_rows.is_empty() {
        crate::common::extraction::extract_flowgate_shadow_prices_single(
            &sol.row_dual,
            &network_plan.fg_rows,
            input.network.flowgates.len(),
            problem_state.problem.n_branch_flow,
            base,
        )
    } else {
        vec![]
    };
    let interface_shadow_prices = if spec.enforce_flowgates && !network_plan.iface_rows.is_empty() {
        crate::common::extraction::extract_interface_shadow_prices_single(
            &sol.row_dual,
            &network_plan.iface_rows,
            input.network.interfaces.len(),
            problem_state.problem.n_branch_flow,
            problem_state.problem.n_fg_rows,
            base,
        )
    } else {
        vec![]
    };

    let par_results: Vec<ParResult> = spec
        .par_setpoints
        .iter()
        .map(|ps| {
            if let Some(br) = setup.branch_lookup.in_service_branch(
                input.network,
                ps.from_bus,
                ps.to_bus,
                ps.circuit.as_str(),
            ) {
                let from_i = bus_map[&ps.from_bus];
                let to_i = bus_map[&ps.to_bus];
                let b_dc = br.b_dc();
                let implied_shift_rad = if b_dc.abs() > 1e-20 {
                    sol.x[from_i] - sol.x[to_i] - ps.target_mw / (base * b_dc)
                } else {
                    0.0
                };
                let implied_shift_deg = implied_shift_rad.to_degrees();
                let (phase_min_rad, phase_max_rad) = br
                    .opf_control
                    .as_ref()
                    .map(|c| (c.phase_min_rad, c.phase_max_rad))
                    .unwrap_or((-std::f64::consts::FRAC_PI_6, std::f64::consts::FRAC_PI_6));
                let within_limits =
                    implied_shift_rad >= phase_min_rad && implied_shift_rad <= phase_max_rad;
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

    info!(
        solve_time_ms = input.solve_time_secs * 1000.0,
        vars = layout.dispatch.n_vars,
        constraints = problem_state.problem.n_row,
        total_cost,
        "SCED solved"
    );

    let total_co2_t: f64 = pg_mw
        .iter()
        .zip(setup.effective_co2_rate.iter())
        .map(|(pg, &rate)| pg * rate)
        .sum();

    let (system_inertia_s, estimated_rocof_hz_per_s, frequency_secure) =
        if !spec.frequency_security.generator_h_values.is_empty() {
            (
                compute_system_inertia(&pg_mw, &setup.gen_indices, input.network, spec),
                compute_estimated_rocof(&pg_mw, &setup.gen_indices, input.network, spec),
                check_frequency_security(&pg_mw, &setup.gen_indices, input.network, spec),
            )
        } else {
            (0.0, 0.0, true)
        };

    let (lmp_energy, lmp_congestion, lmp_loss) = if spec.use_loss_factors && n_bus > 1 {
        surge_opf::advanced::decompose_lmp_with_losses(
            &lmp,
            &problem_state.dloss_dp_final,
            island_refs,
        )
    } else {
        surge_opf::advanced::decompose_lmp_lossless(&lmp, island_refs)
    };

    let virtual_bid_results: Vec<VirtualBidResult> =
        crate::common::extraction::extract_virtual_bid_results(
            &sol.x,
            spec,
            &active_inputs.active_vbids,
            vbid_offset,
            &lmp,
            bus_map,
            base,
            |off| off,
        );
    let dr_results = crate::common::extraction::extract_dr_results(
        &sol.x,
        &active_inputs.dl_list,
        dl_offset,
        &lmp,
        bus_map,
        base,
        |off| off,
    );
    let objective_gap = total_cost - sum_terms(&objective_terms);
    if objective_gap.abs() > 1e-6 {
        warn!(
            total_cost,
            ledger_total = sum_terms(&objective_terms),
            objective_gap,
            "SCED objective ledger did not reconcile exactly; emitting residual term"
        );
        push_term(
            &mut objective_terms,
            make_term(
                "residual",
                ObjectiveBucket::Other,
                ObjectiveTermKind::Other,
                ObjectiveSubjectKind::System,
                SYSTEM_SUBJECT_ID,
                objective_gap,
                None,
                None,
                None,
            ),
        );
    }

    let (storage_charge_mw, storage_discharge_mw, storage_soc_mwh) = if n_storage > 0 {
        let charge: Vec<f64> = (0..n_storage)
            .map(|s| sol.x[sto_ch_offset + s] * base)
            .collect();
        let discharge: Vec<f64> = (0..n_storage)
            .map(|s| sol.x[sto_dis_offset + s] * base)
            .collect();
        let soc: Vec<f64> = (0..n_storage).map(|s| sol.x[sto_soc_offset + s]).collect();
        (charge, discharge, soc)
    } else {
        (vec![], vec![], vec![])
    };

    if n_storage > 0 {
        debug!(
            n_storage,
            storage_charge_mw = ?storage_charge_mw,
            storage_discharge_mw = ?storage_discharge_mw,
            storage_soc_mwh = ?storage_soc_mwh,
            "SCED: storage dispatch"
        );
    }

    let pb_curtailment_by_bus_mw: Vec<f64> = (0..n_bus)
        .map(|bus_idx| sol.x[layout.pb_curtailment_bus_col(bus_idx)] * base)
        .collect();
    let pb_excess_by_bus_mw: Vec<f64> = (0..n_bus)
        .map(|bus_idx| sol.x[layout.pb_excess_bus_col(bus_idx)] * base)
        .collect();
    let pb_curtailment_mw: f64 = pb_curtailment_by_bus_mw.iter().sum();
    let pb_excess_mw: f64 = pb_excess_by_bus_mw.iter().sum();
    let pb_curtailment_cost: f64 = spec
        .power_balance_penalty
        .curtailment
        .iter()
        .enumerate()
        .map(|(seg_idx, &(_, rate))| {
            sol.x[layout.pb_curtailment_seg_col(seg_idx)] * base * rate * dt_h
        })
        .sum();
    let pb_excess_cost: f64 = spec
        .power_balance_penalty
        .excess
        .iter()
        .enumerate()
        .map(|(seg_idx, &(_, rate))| sol.x[layout.pb_excess_seg_col(seg_idx)] * base * rate * dt_h)
        .sum();
    if pb_curtailment_mw > 1e-4 {
        warn!(
            curtailment_mw = pb_curtailment_mw,
            "SCED: load curtailment — insufficient generation"
        );
    }
    if pb_excess_mw > 1e-4 {
        warn!(
            excess_mw = pb_excess_mw,
            "SCED: excess generation — minimum generation exceeds load"
        );
    }
    let thermal_penalty = spec.thermal_penalty_curve.marginal_cost_at(0.0);
    let ramp_penalty = spec.ramp_penalty_curve.marginal_cost_at(0.0);
    let mut constraint_results = Vec::new();
    let pb_curtailment_rate = spec
        .power_balance_penalty
        .curtailment
        .first()
        .map(|(_, price)| *price);
    let pb_excess_rate = spec
        .power_balance_penalty
        .excess
        .first()
        .map(|(_, price)| *price);
    for (bus_idx, &slack_mw) in pb_curtailment_by_bus_mw.iter().enumerate() {
        if slack_mw > 1e-4 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!(
                    "power_balance:bus:{}:curtailment",
                    input.network.buses[bus_idx].number
                ),
                kind: ConstraintKind::PowerBalance,
                scope: ConstraintScope::Bus,
                slack_mw: Some(slack_mw),
                penalty_cost: pb_curtailment_rate,
                penalty_dollars: pb_curtailment_rate.map(|r| slack_mw * r * dt_h),
                ..Default::default()
            });
        }
    }
    for (bus_idx, &slack_mw) in pb_excess_by_bus_mw.iter().enumerate() {
        if slack_mw > 1e-4 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!(
                    "power_balance:bus:{}:excess",
                    input.network.buses[bus_idx].number
                ),
                kind: ConstraintKind::PowerBalance,
                scope: ConstraintScope::Bus,
                slack_mw: Some(slack_mw),
                penalty_cost: pb_excess_rate,
                penalty_dollars: pb_excess_rate.map(|r| slack_mw * r * dt_h),
                ..Default::default()
            });
        }
    }
    for (row_idx, &branch_idx) in network_plan.constrained_branches.iter().enumerate() {
        let branch = &input.network.branches[branch_idx];
        let reverse_slack_mw = sol.x[layout.branch_lower_slack_col(row_idx)] * base;
        let forward_slack_mw = sol.x[layout.branch_upper_slack_col(row_idx)] * base;
        if reverse_slack_mw > 1e-4 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!(
                    "branch:{}:{}:{}:reverse",
                    branch.from_bus, branch.to_bus, branch.circuit
                ),
                kind: ConstraintKind::BranchThermal,
                scope: ConstraintScope::Branch,
                slack_mw: Some(reverse_slack_mw),
                penalty_cost: Some(thermal_penalty),
                penalty_dollars: Some(reverse_slack_mw * thermal_penalty * dt_h),
                ..Default::default()
            });
        }
        if forward_slack_mw > 1e-4 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!(
                    "branch:{}:{}:{}:forward",
                    branch.from_bus, branch.to_bus, branch.circuit
                ),
                kind: ConstraintKind::BranchThermal,
                scope: ConstraintScope::Branch,
                slack_mw: Some(forward_slack_mw),
                penalty_cost: Some(thermal_penalty),
                penalty_dollars: Some(forward_slack_mw * thermal_penalty * dt_h),
                ..Default::default()
            });
        }
    }
    for (row_idx, &fg_idx) in network_plan.fg_rows.iter().enumerate() {
        let flowgate = &input.network.flowgates[fg_idx];
        let reverse_slack_mw = sol.x[layout.flowgate_lower_slack_col(row_idx)] * base;
        let forward_slack_mw = sol.x[layout.flowgate_upper_slack_col(row_idx)] * base;
        if reverse_slack_mw > 1e-4 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("flowgate:{}:reverse", flowgate.name),
                kind: ConstraintKind::Flowgate,
                scope: ConstraintScope::Flowgate,
                slack_mw: Some(reverse_slack_mw),
                penalty_cost: Some(thermal_penalty),
                penalty_dollars: Some(reverse_slack_mw * thermal_penalty * dt_h),
                ..Default::default()
            });
        }
        if forward_slack_mw > 1e-4 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("flowgate:{}:forward", flowgate.name),
                kind: ConstraintKind::Flowgate,
                scope: ConstraintScope::Flowgate,
                slack_mw: Some(forward_slack_mw),
                penalty_cost: Some(thermal_penalty),
                penalty_dollars: Some(forward_slack_mw * thermal_penalty * dt_h),
                ..Default::default()
            });
        }
    }
    for (row_idx, &iface_idx) in network_plan.iface_rows.iter().enumerate() {
        let iface = &input.network.interfaces[iface_idx];
        let reverse_slack_mw = sol.x[layout.interface_lower_slack_col(row_idx)] * base;
        let forward_slack_mw = sol.x[layout.interface_upper_slack_col(row_idx)] * base;
        if reverse_slack_mw > 1e-4 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("interface:{}:reverse", iface.name),
                kind: ConstraintKind::Interface,
                scope: ConstraintScope::Interface,
                slack_mw: Some(reverse_slack_mw),
                penalty_cost: Some(thermal_penalty),
                penalty_dollars: Some(reverse_slack_mw * thermal_penalty * dt_h),
                ..Default::default()
            });
        }
        if forward_slack_mw > 1e-4 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("interface:{}:forward", iface.name),
                kind: ConstraintKind::Interface,
                scope: ConstraintScope::Interface,
                slack_mw: Some(forward_slack_mw),
                penalty_cost: Some(thermal_penalty),
                penalty_dollars: Some(forward_slack_mw * thermal_penalty * dt_h),
                ..Default::default()
            });
        }
    }
    for (j, &gi) in setup.gen_indices.iter().enumerate() {
        let resource_id = input.network.generators[gi].id.clone();
        let ramp_up_slack_mw = sol.x[layout.ramp_up_slack_col(j)] * base;
        let ramp_down_slack_mw = sol.x[layout.ramp_down_slack_col(j)] * base;
        let (ramp_up_shadow, ramp_down_shadow) = if input.context.has_prev_dispatch() {
            let ramp_up_row = problem_state.problem.row_plan.ramp_base_row + 2 * j;
            let ramp_down_row = ramp_up_row + 1;
            (
                -sol.row_dual[ramp_up_row] / base,
                sol.row_dual[ramp_down_row] / base,
            )
        } else {
            (0.0, 0.0)
        };
        if ramp_up_slack_mw > 1e-4 || ramp_up_shadow.abs() > 1e-6 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("ramp_up:{resource_id}"),
                kind: ConstraintKind::Ramp,
                scope: ConstraintScope::Resource,
                shadow_price: Some(ramp_up_shadow),
                slack_mw: Some(ramp_up_slack_mw),
                penalty_cost: Some(ramp_penalty),
                penalty_dollars: if ramp_up_slack_mw > 1e-4 {
                    Some(ramp_up_slack_mw * ramp_penalty * dt_h)
                } else {
                    None
                },
            });
        }
        if ramp_down_slack_mw > 1e-4 || ramp_down_shadow.abs() > 1e-6 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("ramp_down:{resource_id}"),
                kind: ConstraintKind::Ramp,
                scope: ConstraintScope::Resource,
                shadow_price: Some(ramp_down_shadow),
                slack_mw: Some(ramp_down_slack_mw),
                penalty_cost: Some(ramp_penalty),
                penalty_dollars: if ramp_down_slack_mw > 1e-4 {
                    Some(ramp_down_slack_mw * ramp_penalty * dt_h)
                } else {
                    None
                },
            });
        }

        let pg_col = pg_offset + j;
        let pg_lower_shadow = (-sol.col_dual[pg_col]).max(0.0) / base;
        let pg_upper_shadow = sol.col_dual[pg_col].max(0.0) / base;
        if pg_lower_shadow > 1e-6 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("pg_lower:{resource_id}"),
                kind: ConstraintKind::GeneratorBound,
                scope: ConstraintScope::Resource,
                shadow_price: Some(pg_lower_shadow),
                ..Default::default()
            });
        }
        if pg_upper_shadow > 1e-6 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("pg_upper:{resource_id}"),
                kind: ConstraintKind::GeneratorBound,
                scope: ConstraintScope::Resource,
                shadow_price: Some(pg_upper_shadow),
                ..Default::default()
            });
        }

        if setup.is_block_mode {
            let block_start = setup.gen_block_start[j];
            for (block_idx, block) in setup.gen_blocks[j].iter().enumerate() {
                let col = layout.dispatch.block + block_start + block_idx;
                let block_lower_shadow = (-sol.col_dual[col]).max(0.0) / base;
                let block_upper_shadow = sol.col_dual[col].max(0.0) / base;
                if block_lower_shadow > 1e-6 {
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!(
                            "dispatch_block_lower:{resource_id}:{block_idx}:{:.3}:{:.3}",
                            block.mw_lo, block.mw_hi
                        ),
                        kind: ConstraintKind::DispatchBlockBound,
                        scope: ConstraintScope::Resource,
                        shadow_price: Some(block_lower_shadow),
                        ..Default::default()
                    });
                }
                if block_upper_shadow > 1e-6 {
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!(
                            "dispatch_block_upper:{resource_id}:{block_idx}:{:.3}:{:.3}",
                            block.mw_lo, block.mw_hi
                        ),
                        kind: ConstraintKind::DispatchBlockBound,
                        scope: ConstraintScope::Resource,
                        shadow_price: Some(block_upper_shadow),
                        ..Default::default()
                    });
                }
            }
        }
    }
    for (k, dl) in active_inputs.dl_list.iter().enumerate() {
        let resource_id = if dl.resource_id.is_empty() {
            format!("dispatchable_load_{}", k)
        } else {
            dl.resource_id.clone()
        };
        let dl_col = dl_offset + k;
        let dl_lower_shadow = (-sol.col_dual[dl_col]).max(0.0) / base;
        let dl_upper_shadow = sol.col_dual[dl_col].max(0.0) / base;
        if dl_lower_shadow > 1e-6 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("dispatchable_load_lower:{resource_id}"),
                kind: ConstraintKind::DispatchableLoadBound,
                scope: ConstraintScope::Resource,
                shadow_price: Some(dl_lower_shadow),
                ..Default::default()
            });
        }
        if dl_upper_shadow > 1e-6 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!("dispatchable_load_upper:{resource_id}"),
                kind: ConstraintKind::DispatchableLoadBound,
                scope: ConstraintScope::Resource,
                shadow_price: Some(dl_upper_shadow),
                ..Default::default()
            });
        }
    }
    if n_storage > 0 {
        let soc_row_base = problem_state.problem.row_plan.sto_base_row;
        let dis_as_row_base = soc_row_base + n_storage;
        let ch_as_row_base = soc_row_base + 2 * n_storage;
        let pg_link_row_base =
            soc_row_base + 3 * n_storage + setup.n_sto_dis_offer_rows + setup.n_sto_ch_bid_rows;

        for &(s, _, gi) in &setup.storage_gen_local {
            let resource_id = input.network.generators[gi].id.clone();
            let soc_col = sto_soc_offset + s;
            let soc_lower_shadow = (-sol.col_dual[soc_col]).max(0.0);
            let soc_upper_shadow = sol.col_dual[soc_col].max(0.0);

            if soc_lower_shadow > 1e-6 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("storage_soc_lower:{resource_id}"),
                    kind: ConstraintKind::StorageSocBound,
                    scope: ConstraintScope::Resource,
                    shadow_price: Some(soc_lower_shadow),
                    ..Default::default()
                });
            }
            if soc_upper_shadow > 1e-6 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("storage_soc_upper:{resource_id}"),
                    kind: ConstraintKind::StorageSocBound,
                    scope: ConstraintScope::Resource,
                    shadow_price: Some(soc_upper_shadow),
                    ..Default::default()
                });
            }

            let soc_shadow = -sol.row_dual[soc_row_base + s];
            if soc_shadow.abs() > 1e-6 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("storage_soc:{resource_id}"),
                    kind: ConstraintKind::StorageSoc,
                    scope: ConstraintScope::Resource,
                    shadow_price: Some(soc_shadow),
                    ..Default::default()
                });
            }

            let discharge_as_shadow = -sol.row_dual[dis_as_row_base + s] / base;
            if discharge_as_shadow.abs() > 1e-6 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("storage_reserve:up:{resource_id}"),
                    kind: ConstraintKind::StorageReserveCoupling,
                    scope: ConstraintScope::Resource,
                    shadow_price: Some(discharge_as_shadow),
                    ..Default::default()
                });
            }

            let charge_as_shadow = -sol.row_dual[ch_as_row_base + s] / base;
            if charge_as_shadow.abs() > 1e-6 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("storage_reserve:down:{resource_id}"),
                    kind: ConstraintKind::StorageReserveCoupling,
                    scope: ConstraintScope::Resource,
                    shadow_price: Some(charge_as_shadow),
                    ..Default::default()
                });
            }

            let pg_link_shadow = -sol.row_dual[pg_link_row_base + s] / base;
            if pg_link_shadow.abs() > 1e-6 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("storage_pg_link:{resource_id}"),
                    kind: ConstraintKind::StorageDispatchLink,
                    scope: ConstraintScope::Resource,
                    shadow_price: Some(pg_link_shadow),
                    ..Default::default()
                });
            }
        }
    }
    // Angle difference slacks.
    let angle_penalty_rate = spec.angle_penalty_curve.marginal_cost_at(0.0);
    for (row_idx, acb) in model_plan
        .network_plan
        .angle_constrained_branches
        .iter()
        .enumerate()
    {
        let branch = &input.network.branches[acb.branch_idx];
        let upper_slack_rad = sol.x[layout.angle_diff_upper_slack_col(row_idx)];
        let lower_slack_rad = sol.x[layout.angle_diff_lower_slack_col(row_idx)];
        if upper_slack_rad > 1e-6 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!(
                    "angle_diff:{}:{}:{}:upper",
                    branch.from_bus, branch.to_bus, branch.circuit
                ),
                kind: ConstraintKind::AngleDifference,
                scope: ConstraintScope::Branch,
                slack_mw: Some(upper_slack_rad),
                penalty_cost: Some(angle_penalty_rate),
                penalty_dollars: Some(upper_slack_rad * angle_penalty_rate * dt_h),
                ..Default::default()
            });
        }
        if lower_slack_rad > 1e-6 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: format!(
                    "angle_diff:{}:{}:{}:lower",
                    branch.from_bus, branch.to_bus, branch.circuit
                ),
                kind: ConstraintKind::AngleDifference,
                scope: ConstraintScope::Branch,
                slack_mw: Some(lower_slack_rad),
                penalty_cost: Some(angle_penalty_rate),
                penalty_dollars: Some(lower_slack_rad * angle_penalty_rate * dt_h),
                ..Default::default()
            });
        }
    }

    constraint_results.extend(crate::common::reserves::extract_constraint_results(
        &active_inputs.reserve_layout,
        &sol.row_dual,
        problem_state.problem.row_plan.reserve_row_base,
        &active_inputs.reserve_ctx,
        1e-6,
    ));

    // SCED-AC Benders epigraph value: when the layout has an eta column, read
    // its primal directly out of the LP solution. The eta variable carries
    // dollars/hour units (no per-unit scaling) since its cost coefficient is
    // 1.0 and the cut row constants are also in dollars/hour.
    let sced_ac_benders_eta_dollars_per_hour = layout.benders_eta_col().map(|col| sol.x[col]);

    let dispatch = RawDispatchPeriodResult {
        pg_mw,
        lmp,
        // DC SCED has no per-bus Q-balance dual — Q-LMP is AC-only.
        q_lmp: Vec::new(),
        lmp_energy,
        lmp_congestion,
        total_cost,
        co2_t: total_co2_t,
        hvdc_dispatch_mw,
        hvdc_band_dispatch_mw,
        storage_charge_mw,
        storage_discharge_mw,
        storage_soc_mwh,
        lmp_loss,
        branch_shadow_prices,
        flowgate_shadow_prices,
        interface_shadow_prices,
        par_results,
        dr_results,
        virtual_bid_results,
        objective_terms,
        reserve_awards: rr.awards.clone(),
        reserve_prices: rr.prices.clone(),
        reserve_provided: rr.provided.clone(),
        reserve_shortfall: rr.shortfall.clone(),
        zonal_reserve_prices: rr.zonal_prices.clone(),
        zonal_reserve_shortfall: rr.zonal_shortfall.clone(),
        dr_reserve_awards: rr.dl_awards.clone(),
        power_balance_violation: surge_network::market::PowerBalanceViolation {
            curtailment_mw: pb_curtailment_mw,
            excess_mw: pb_excess_mw,
            curtailment_cost: pb_curtailment_cost,
            excess_cost: pb_excess_cost,
        },
        resource_results: Vec::new(),
        bus_results: Vec::new(),
        reserve_results: Vec::new(),
        constraint_results,
        hvdc_results: Vec::new(),
        tap_dispatch: Vec::new(),
        phase_dispatch: Vec::new(),
        switched_shunt_dispatch: Vec::new(),
        emissions_results: None,
        frequency_results: None,
        sced_ac_benders_eta_dollars_per_hour,
    };
    let mut storage_soc = HashMap::new();
    if !dispatch.storage_soc_mwh.is_empty() {
        for (s, &(.., gi)) in setup.storage_gen_local.iter().enumerate() {
            if let Some(&soc) = dispatch.storage_soc_mwh.get(s) {
                storage_soc.insert(gi, vec![soc]);
            }
        }
    }
    let commitment_kind = match spec.commitment {
        CommitmentMode::AllCommitted => CommitmentPolicyKind::AllCommitted,
        CommitmentMode::Fixed { .. } => CommitmentPolicyKind::Fixed,
        CommitmentMode::Optimize(_) => CommitmentPolicyKind::Optimize,
        CommitmentMode::Additional { .. } => CommitmentPolicyKind::Additional,
    };
    let periods = vec![dispatch];
    // The orchestrator in dispatch.rs overwrites study via
    // attach_public_catalogs_and_solve_metadata for production paths.
    // These defaults are for direct-call paths (tests, internal use).
    RawDispatchSolution {
        study: DispatchStudy {
            formulation: Formulation::Dc,
            coupling: IntervalCoupling::PeriodByPeriod,
            commitment: commitment_kind,
            periods: periods.len(),
            security_enabled: false,
            stage: None,
        },
        resources: Vec::new(),
        buses: Vec::new(),
        summary: crate::DispatchSummary {
            total_cost,
            total_co2_t,
            ..Default::default()
        },
        diagnostics: crate::DispatchDiagnostics {
            iterations: sol.iterations,
            solve_time_secs: input.solve_time_secs,
            ..Default::default()
        },
        periods,
        commitment: None,
        startup: None,
        shutdown: None,
        startup_costs: None,
        operating_cost: None,
        startup_cost_total: None,
        storage_soc,
        bus_angles_rad: vec![sol.x[layout.dispatch.theta..layout.dispatch.theta + n_bus].to_vec()],
        bus_voltage_pu: Vec::new(),
        generator_q_mvar: Vec::new(),
        bus_q_slack_pos_mvar: Vec::new(),
        bus_q_slack_neg_mvar: Vec::new(),
        bus_p_slack_pos_mw: Vec::new(),
        bus_p_slack_neg_mw: Vec::new(),
        thermal_limit_slack_from_mva: Vec::new(),
        thermal_limit_slack_to_mva: Vec::new(),
        bus_vm_slack_high_pu: Vec::new(),
        bus_vm_slack_low_pu: Vec::new(),
        angle_diff_slack_high_rad: Vec::new(),
        angle_diff_slack_low_rad: Vec::new(),
        ac_p_balance_penalty_per_mw: None,
        ac_q_balance_penalty_per_mvar: None,
        ac_thermal_penalty_per_mva: None,
        ac_voltage_penalty_per_pu: None,
        ac_angle_penalty_per_rad: None,
        system_inertia_s: Some(system_inertia_s),
        estimated_rocof_hz_per_s: Some(estimated_rocof_hz_per_s),
        frequency_secure: Some(frequency_secure),
        co2_shadow_price: Vec::new(),
        regulation: Vec::new(),
        branch_commitment_state: Vec::new(),
        cc_config_schedule: Vec::new(),
        cc_transition_cost: 0.0,
        cc_transition_costs: Vec::new(),
        model_diagnostics: Vec::new(),
        aux_flowgate_names: Vec::new(),
        bus_loss_allocation_mw: Vec::new(),
        scuc_final_loss_warm_start: None,
    }
}

#[cfg(test)]
pub(super) fn dispatch_result_to_sced_solution(result: RawDispatchSolution) -> RawScedSolution {
    let dispatch = result
        .periods
        .into_iter()
        .next()
        .expect("single-period SCED should return exactly one dispatch period");
    RawScedSolution {
        dispatch,
        solve_time_secs: result.diagnostics.solve_time_secs,
        iterations: result.diagnostics.iterations,
        system_inertia_s: result.system_inertia_s.unwrap_or(0.0),
        estimated_rocof_hz_per_s: result.estimated_rocof_hz_per_s.unwrap_or(0.0),
        frequency_secure: result.frequency_secure.unwrap_or(true),
    }
}
