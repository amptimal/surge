// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Post-solve SCUC result extraction helpers.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::market::{CostCurve, DispatchableLoad};
use surge_opf::advanced::IslandRefs;
use surge_opf::backends::LpResult;
use tracing::warn;

use super::layout::ScucLayout;
use super::plan::{ExplicitContingencyObjectivePlan, ScucCcPlantInfo};
use super::pricing::{PricingRunState, PricingSummary};
use super::rows::ScucStartupTierInfo;
use crate::common::costs::uses_convex_polynomial_pwl;
use crate::common::dc::DcSolveSession;
use crate::common::network::AngleConstrainedBranch;
use crate::common::reserves::{
    ReserveResults, dispatchable_load_reserve_offer_for_period, generator_reserve_offer_for_period,
};
use crate::common::spec::DispatchProblemSpec;
use crate::dispatch::{CommitmentMode, RawDispatchSolution};
use crate::economics::{SYSTEM_SUBJECT_ID, make_term, push_term, sum_terms};
use crate::report_ids::{
    branch_subject_id, combined_cycle_plant_id, dispatchable_load_resource_id, flowgate_subject_id,
    generator_resource_id, hvdc_link_id, interface_subject_id, reserve_requirement_subject_id,
};
use crate::request::Formulation;
use crate::request::{CommitmentPolicyKind, IntervalCoupling};
use crate::result::DispatchStudy;
use crate::result::{ConstraintKind, ConstraintScope};
use crate::solution::{RawConstraintPeriodResult, RawDispatchPeriodResult};
use surge_solution::{
    ObjectiveBucket, ObjectiveQuantityUnit, ObjectiveSubjectKind, ObjectiveTerm, ObjectiveTermKind,
};

pub(super) struct CommitmentExtraction {
    pub pg_mw_all: Vec<Vec<f64>>,
    pub commitment: Vec<Vec<bool>>,
    pub startup_events: Vec<Vec<bool>>,
    pub shutdown_events: Vec<Vec<bool>>,
    pub regulation_all: Vec<Vec<bool>>,
    pub total_startup_cost: f64,
    pub storage_soc_out: HashMap<usize, Vec<f64>>,
    pub storage_ch_all: Vec<Vec<f64>>,
    pub storage_dis_all: Vec<Vec<f64>>,
}

pub(super) struct CommitmentExtractionInput<'a> {
    pub sol: &'a LpResult,
    pub layout: &'a ScucLayout,
    pub gen_indices: &'a [usize],
    pub gen_tier_info_by_hour: &'a [Vec<Vec<ScucStartupTierInfo>>],
    pub delta_gen_off: &'a [usize],
    pub storage_gen_local: &'a [(usize, usize, usize)],
    pub n_storage: usize,
    pub has_reg_products: bool,
    pub regulation_offset: usize,
    pub n_hours: usize,
    pub base: f64,
}

pub(super) fn extract_commitment_dispatch(
    input: CommitmentExtractionInput<'_>,
) -> CommitmentExtraction {
    let n_gen = input.gen_indices.len();
    let mut pg_mw_all = Vec::with_capacity(input.n_hours);
    let mut commitment = Vec::with_capacity(input.n_hours);
    let mut startup_events = Vec::with_capacity(input.n_hours);
    let mut shutdown_events = Vec::with_capacity(input.n_hours);
    let mut regulation_all = Vec::with_capacity(input.n_hours);
    let mut total_startup_cost = 0.0;
    let mut storage_soc_out: HashMap<usize, Vec<f64>> = HashMap::new();
    let mut storage_ch_all = Vec::with_capacity(input.n_hours);
    let mut storage_dis_all = Vec::with_capacity(input.n_hours);

    for t in 0..input.n_hours {
        let mut pg_t = Vec::with_capacity(n_gen);
        let mut u_t = Vec::with_capacity(n_gen);
        let mut v_t = Vec::with_capacity(n_gen);
        let mut w_t = Vec::with_capacity(n_gen);
        for j in 0..n_gen {
            let pg_val = input.sol.x[input.layout.pg_col(t, j)] * input.base;
            let u_val = input.sol.x[input.layout.commitment_col(t, j)];
            let v_val = input.sol.x[input.layout.startup_col(t, j)];
            let w_val = input.sol.x[input.layout.shutdown_col(t, j)];

            pg_t.push(pg_val);
            u_t.push(u_val > 0.5);
            v_t.push(v_val > 0.5);
            w_t.push(w_val > 0.5);

            if v_val > 0.5 {
                for (k, tier) in input.gen_tier_info_by_hour[j][t].iter().enumerate() {
                    let d_val = input.sol.x[input
                        .layout
                        .col(t, input.layout.startup_delta + input.delta_gen_off[j] + k)];
                    if d_val > 0.5 {
                        total_startup_cost += tier.cost;
                        break;
                    }
                }
            }
        }

        for &(s, _j, gi) in input.storage_gen_local {
            let soc_val = input.sol.x[input.layout.storage_soc_col(t, s)];
            storage_soc_out.entry(gi).or_default().push(soc_val);
        }

        let sto_ch_t: Vec<f64> = (0..input.n_storage)
            .map(|s| input.sol.x[input.layout.storage_charge_col(t, s)])
            .collect();
        let sto_dis_t: Vec<f64> = (0..input.n_storage)
            .map(|s| input.sol.x[input.layout.storage_discharge_col(t, s)])
            .collect();
        let r_t: Vec<bool> = if input.has_reg_products {
            (0..n_gen)
                .map(|j| input.sol.x[input.layout.col(t, input.regulation_offset + j)] > 0.5)
                .collect()
        } else {
            vec![]
        };

        pg_mw_all.push(pg_t);
        commitment.push(u_t);
        startup_events.push(v_t);
        shutdown_events.push(w_t);
        regulation_all.push(r_t);
        storage_ch_all.push(sto_ch_t);
        storage_dis_all.push(sto_dis_t);
    }

    CommitmentExtraction {
        pg_mw_all,
        commitment,
        startup_events,
        shutdown_events,
        regulation_all,
        total_startup_cost,
        storage_soc_out,
        storage_ch_all,
        storage_dis_all,
    }
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

fn scuc_block_fixed_cost(cost: &CostCurve, committed: bool, pmin_mw: f64) -> f64 {
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

fn build_period_objective_terms(
    input: &PeriodAssemblyInput<'_>,
    t: usize,
    period: &RawDispatchPeriodResult,
) -> Vec<ObjectiveTerm> {
    let mut terms = Vec::new();
    let dt_h = input.spec.period_hours(t);
    let base = input.base;
    let sol = input.sol;
    let pwl_gen_local_by_j: HashMap<usize, usize> = input
        .pwl_gen_j
        .iter()
        .enumerate()
        .map(|(k, &j)| (j, k))
        .collect();
    let cc_member_j: HashSet<usize> = input
        .cc_infos
        .iter()
        .flat_map(|info| info.member_gen_j.iter().copied())
        .collect();

    for (j, &gi) in input.gen_indices.iter().enumerate() {
        let generator = &input.network.generators[gi];
        let resource_id = generator_resource_id(generator);
        let is_cc_member = cc_member_j.contains(&j);
        if generator.is_storage() || is_cc_member {
            continue;
        }
        let g_hourly = &input.hourly_networks[t].generators[gi];
        let mut offer_cost_buf = None;
        let cost = crate::common::costs::resolve_cost_for_period_from_spec(
            gi,
            t,
            generator,
            input.spec,
            &mut offer_cost_buf,
            Some(g_hourly.pmax),
        );
        let no_load_rate = resolved_no_load_cost(cost);
        let is_committed = input.commitment[t][j];
        let no_load_dollars = if !is_committed {
            0.0
        } else if input.is_block_mode {
            let fixed_cost = scuc_block_fixed_cost(cost, is_committed, generator.pmin);
            no_load_rate.min(fixed_cost) * dt_h
        } else {
            no_load_rate * dt_h
        };
        let energy_dollars = if input.is_block_mode {
            let block_start = input.gen_block_start[j];
            let block_energy: f64 = input.gen_blocks[j]
                .iter()
                .enumerate()
                .map(|(block_idx, block)| {
                    let col = input
                        .layout
                        .col(t, input.layout.dispatch.block + block_start + block_idx);
                    sol.x[col] * base * block.marginal_cost * dt_h
                })
                .sum();
            let fixed_cost = scuc_block_fixed_cost(cost, input.commitment[t][j], generator.pmin);
            (fixed_cost * dt_h - no_load_dollars).max(0.0) + block_energy
        } else if input.use_plc && uses_convex_polynomial_pwl(cost) {
            let total: f64 = (0..input.n_bp)
                .map(|bp_idx| {
                    let col = input
                        .layout
                        .col(t, input.layout.plc_lambda + j * input.n_bp + bp_idx);
                    sol.x[col] * input.col_cost[col]
                })
                .sum();
            (total - no_load_dollars).max(0.0)
        } else if let Some(&pwl_idx) = pwl_gen_local_by_j.get(&j) {
            let col = input.layout.col(t, input.layout.dispatch.e_g + pwl_idx);
            if input.col_cost[col].abs() > 1e-12 {
                (sol.x[col] * input.col_cost[col] - no_load_dollars).max(0.0)
            } else {
                0.0
            }
        } else {
            match &cost {
                CostCurve::Polynomial { coeffs, .. } => {
                    let pg_mw = period.pg_mw.get(j).copied().unwrap_or(0.0).max(0.0);
                    match coeffs.len() {
                        0 => 0.0,
                        1 => 0.0,
                        2 => coeffs[0] * pg_mw * dt_h,
                        _ => coeffs[1] * pg_mw * dt_h,
                    }
                }
                CostCurve::PiecewiseLinear { .. } => 0.0,
            }
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
                Some(period.pg_mw.get(j).copied().unwrap_or(0.0).max(0.0) * dt_h),
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
        for tier_idx in 0..input.startup_tier_capacity[j] {
            let idx = input.layout.col(
                t,
                input.layout.startup_delta + input.delta_gen_off[j] + tier_idx,
            );
            push_term(
                &mut terms,
                make_term(
                    format!("startup_tier_{tier_idx}"),
                    ObjectiveBucket::Startup,
                    ObjectiveTermKind::GeneratorStartup,
                    ObjectiveSubjectKind::Resource,
                    resource_id.clone(),
                    sol.x[idx] * input.col_cost[idx],
                    Some(sol.x[idx]),
                    Some(ObjectiveQuantityUnit::Event),
                    Some(input.col_cost[idx]),
                ),
            );
        }
        let shutdown_idx = input.layout.shutdown_col(t, j);
        push_term(
            &mut terms,
            make_term(
                "shutdown",
                ObjectiveBucket::Shutdown,
                ObjectiveTermKind::GeneratorShutdown,
                ObjectiveSubjectKind::Resource,
                resource_id.clone(),
                sol.x[shutdown_idx] * input.col_cost[shutdown_idx],
                Some(sol.x[shutdown_idx]),
                Some(ObjectiveQuantityUnit::Event),
                Some(input.col_cost[shutdown_idx]),
            ),
        );
        if input.effective_co2_price > 0.0 {
            let pg_mw = period.pg_mw.get(j).copied().unwrap_or(0.0).max(0.0);
            let carbon_dollars =
                pg_mw * input.effective_co2_rate[j] * input.effective_co2_price * dt_h;
            push_term(
                &mut terms,
                make_term(
                    "carbon",
                    ObjectiveBucket::Adder,
                    ObjectiveTermKind::CarbonAdder,
                    ObjectiveSubjectKind::Resource,
                    resource_id,
                    carbon_dollars,
                    Some(pg_mw * dt_h),
                    Some(ObjectiveQuantityUnit::Mwh),
                    Some(input.effective_co2_rate[j] * input.effective_co2_price),
                ),
            );
        }
    }

    for &(s, _, gi) in input.storage_gen_local {
        let generator = &input.network.generators[gi];
        let resource_id = generator_resource_id(generator);
        let storage = generator
            .storage
            .as_ref()
            .expect("storage_gen_local only contains generators with storage");
        let charge_mw = input.storage_ch_all[t].get(s).copied().unwrap_or(0.0);
        let discharge_mw = input.storage_dis_all[t].get(s).copied().unwrap_or(0.0);
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
    for (k, (storage_local, _)) in input.sto_dis_offer_info.iter().enumerate() {
        if let Some(&(_, _, gi)) = input.storage_gen_local.get(*storage_local) {
            let resource_id = generator_resource_id(&input.network.generators[gi]);
            let col = input.layout.col(t, input.layout.dispatch.sto_epi_dis + k);
            push_term(
                &mut terms,
                make_term(
                    "discharge_offer_epigraph",
                    ObjectiveBucket::Energy,
                    ObjectiveTermKind::StorageOfferEpigraph,
                    ObjectiveSubjectKind::Resource,
                    resource_id,
                    sol.x[col] * input.col_cost[col],
                    None,
                    None,
                    None,
                ),
            );
        }
    }
    for (k, (storage_local, _)) in input.sto_ch_bid_info.iter().enumerate() {
        if let Some(&(_, _, gi)) = input.storage_gen_local.get(*storage_local) {
            let resource_id = generator_resource_id(&input.network.generators[gi]);
            let col = input.layout.col(t, input.layout.dispatch.sto_epi_ch + k);
            push_term(
                &mut terms,
                make_term(
                    "charge_bid_epigraph",
                    ObjectiveBucket::Energy,
                    ObjectiveTermKind::StorageOfferEpigraph,
                    ObjectiveSubjectKind::Resource,
                    resource_id,
                    sol.x[col] * input.col_cost[col],
                    None,
                    None,
                    None,
                ),
            );
        }
    }

    for (k, dl) in input.dl_list.iter().enumerate() {
        let (_, _, _, _, _, cost_model) = crate::common::costs::resolve_dl_for_period_from_spec(
            input.dl_orig_idx.get(k).copied().unwrap_or(k),
            t,
            dl,
            input.spec,
        );
        let resource_id =
            dispatchable_load_resource_id(dl, input.dl_orig_idx.get(k).copied().unwrap_or(k));
        if let Some(load_result) = period.dr_results.loads.get(k) {
            let col = input.layout.col(t, input.layout.dispatch.dl + k);
            let exact_dollars = crate::common::costs::exact_dispatchable_load_objective_dollars(
                cost_model,
                sol.x[col],
                input.col_cost[col],
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

    if let Some(reserve_results) = input.hourly_reserve_results.get(t).and_then(Option::as_ref) {
        for (product_id, awards) in &reserve_results.awards {
            for (j, &award_mw) in awards.iter().enumerate() {
                if award_mw.abs() <= 1e-9 {
                    continue;
                }
                let gi = input.gen_indices[j];
                let resource_id = generator_resource_id(&input.network.generators[gi]);
                let rate = generator_reserve_offer_for_period(
                    input.spec,
                    gi,
                    &input.network.generators[gi],
                    product_id,
                    t,
                )
                .map(|offer| offer.cost_per_mwh)
                .unwrap_or(0.0);
                push_term(
                    &mut terms,
                    make_term(
                        format!("reserve:{product_id}"),
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
        for (product_id, awards) in &reserve_results.dl_awards {
            for (k, &award_mw) in awards.iter().enumerate() {
                if award_mw.abs() <= 1e-9 {
                    continue;
                }
                let dl = input.dl_list[k];
                let resource_id = dispatchable_load_resource_id(
                    dl,
                    input.dl_orig_idx.get(k).copied().unwrap_or(k),
                );
                let rate = dispatchable_load_reserve_offer_for_period(
                    input.spec,
                    input.dl_orig_idx.get(k).copied().unwrap_or(k),
                    dl,
                    product_id,
                    t,
                )
                .map(|offer| offer.cost_per_mwh)
                .unwrap_or(0.0);
                push_term(
                    &mut terms,
                    make_term(
                        format!("reserve:{product_id}"),
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
    }
    for ap in &input.reserve_layout.products {
        let reserve_subject_id = reserve_requirement_subject_id(&ap.product.id, None);
        for slack_idx in 0..ap.n_penalty_slacks {
            let col = input.layout.col(t, ap.slack_offset + slack_idx);
            let slack_mw = sol.x[col] * base;
            push_term(
                &mut terms,
                make_term(
                    format!("shortfall_segment_{slack_idx}"),
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::ReserveShortfall,
                    ObjectiveSubjectKind::ReserveRequirement,
                    reserve_subject_id.clone(),
                    sol.x[col] * input.col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
                ),
            );
        }
        for (zi, zonal_req) in ap.zonal_reqs.iter().enumerate() {
            let col = input.layout.col(t, ap.zonal_slack_offset + zi);
            let slack_mw = sol.x[col] * base;
            push_term(
                &mut terms,
                make_term(
                    "zonal_shortfall",
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::ReserveShortfall,
                    ObjectiveSubjectKind::ReserveRequirement,
                    reserve_requirement_subject_id(&ap.product.id, Some(zonal_req.zone_id)),
                    sol.x[col] * input.col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }

    for (k, hvdc) in input.spec.hvdc_links.iter().enumerate() {
        let subject_id = hvdc_link_id(hvdc, k);
        if hvdc.is_banded() {
            for (band_idx, band) in hvdc.bands.iter().enumerate() {
                let col = input.layout.col(
                    t,
                    input.layout.dispatch.hvdc + input.hvdc_band_offsets[k] + band_idx,
                );
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
                        sol.x[col] * input.col_cost[col],
                        Some(mw * dt_h),
                        Some(ObjectiveQuantityUnit::Mwh),
                        (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
                    ),
                );
            }
        } else {
            let col = input
                .layout
                .col(t, input.layout.dispatch.hvdc + input.hvdc_band_offsets[k]);
            let mw = sol.x[col] * base;
            push_term(
                &mut terms,
                make_term(
                    "dispatch",
                    ObjectiveBucket::Energy,
                    ObjectiveTermKind::HvdcEnergy,
                    ObjectiveSubjectKind::HvdcLink,
                    subject_id,
                    sol.x[col] * input.col_cost[col],
                    Some(mw * dt_h),
                    Some(ObjectiveQuantityUnit::Mwh),
                    (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }

    for (k, &bi) in input.active_vbids.iter().enumerate() {
        let vb = &input.spec.virtual_bids[bi];
        let subject_id = if vb.position_id.is_empty() {
            format!("virtual_bid:{bi}")
        } else {
            vb.position_id.clone()
        };
        let col = input.layout.col(t, input.layout.dispatch.vbid + k);
        let cleared_mw = sol.x[col] * base;
        push_term(
            &mut terms,
            make_term(
                "virtual_bid",
                ObjectiveBucket::Adder,
                ObjectiveTermKind::VirtualBid,
                ObjectiveSubjectKind::VirtualBid,
                subject_id,
                sol.x[col] * input.col_cost[col],
                Some(cleared_mw * dt_h),
                Some(ObjectiveQuantityUnit::Mwh),
                Some(vb.price_per_mwh),
            ),
        );
    }

    for (seg_idx, _) in input
        .spec
        .power_balance_penalty
        .curtailment
        .iter()
        .enumerate()
    {
        let col = input.layout.pb_curtailment_seg_col(t, seg_idx);
        if col >= sol.x.len() || col >= input.col_cost.len() {
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
                sol.x[col] * input.col_cost[col],
                Some(mw),
                Some(ObjectiveQuantityUnit::Mw),
                (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
            ),
        );
    }
    for (seg_idx, _) in input.spec.power_balance_penalty.excess.iter().enumerate() {
        let col = input.layout.pb_excess_seg_col(t, seg_idx);
        if col >= sol.x.len() || col >= input.col_cost.len() {
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
                sol.x[col] * input.col_cost[col],
                Some(mw),
                Some(ObjectiveQuantityUnit::Mw),
                (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
            ),
        );
    }

    for (row_idx, &branch_idx) in input.constrained_branches.iter().enumerate() {
        let branch = &input.network.branches[branch_idx];
        for (component_id, col) in [
            ("reverse", input.layout.branch_lower_slack_col(t, row_idx)),
            ("forward", input.layout.branch_upper_slack_col(t, row_idx)),
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
                    sol.x[col] * input.col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }
    for (row_idx, &fg_idx) in input.fg_rows.iter().enumerate() {
        let flowgate = &input.network.flowgates[fg_idx];
        for (component_id, col) in [
            ("reverse", input.layout.flowgate_lower_slack_col(t, row_idx)),
            ("forward", input.layout.flowgate_upper_slack_col(t, row_idx)),
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
                    sol.x[col] * input.col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }
    for (row_idx, &iface_idx) in input.iface_rows.iter().enumerate() {
        let interface = &input.network.interfaces[iface_idx];
        for (component_id, col) in [
            (
                "reverse",
                input.layout.interface_lower_slack_col(t, row_idx),
            ),
            (
                "forward",
                input.layout.interface_upper_slack_col(t, row_idx),
            ),
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
                    sol.x[col] * input.col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }

    for (j, &gi) in input.gen_indices.iter().enumerate() {
        let resource_id = generator_resource_id(&input.network.generators[gi]);
        for (component_id, col) in [
            ("headroom", input.layout.headroom_slack_col(t, j)),
            ("footroom", input.layout.footroom_slack_col(t, j)),
        ] {
            let slack_mw = sol.x[col] * base;
            push_term(
                &mut terms,
                make_term(
                    component_id,
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::CommitmentCapacityPenalty,
                    ObjectiveSubjectKind::Resource,
                    resource_id.clone(),
                    sol.x[col] * input.col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
                ),
            );
        }
        for (component_id, col) in [
            ("up", input.layout.ramp_up_slack_col(t, j)),
            ("down", input.layout.ramp_down_slack_col(t, j)),
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
                    sol.x[col] * input.col_cost[col],
                    Some(slack_mw),
                    Some(ObjectiveQuantityUnit::Mw),
                    (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[col] / (base * dt_h)),
                ),
            );
        }
    }
    for (row_idx, acb) in input.angle_constrained_branches.iter().enumerate() {
        let branch = &input.network.branches[acb.branch_idx];
        for (component_id, col) in [
            ("lower", input.layout.angle_diff_lower_slack_col(t, row_idx)),
            ("upper", input.layout.angle_diff_upper_slack_col(t, row_idx)),
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
                    sol.x[col] * input.col_cost[col],
                    Some(slack_rad),
                    Some(ObjectiveQuantityUnit::Rad),
                    (dt_h > 0.0).then_some(input.col_cost[col] / dt_h),
                ),
            );
        }
    }

    for branch_local_idx in 0..input.network.branches.len() {
        let branch = &input.network.branches[branch_local_idx];
        let startup_col = input.layout.branch_startup_col(t, branch_local_idx);
        let shutdown_col = input.layout.branch_shutdown_col(t, branch_local_idx);
        push_term(
            &mut terms,
            make_term(
                "branch_startup",
                ObjectiveBucket::Other,
                ObjectiveTermKind::BranchSwitchingStartup,
                ObjectiveSubjectKind::Branch,
                branch_subject_id(branch),
                sol.x[startup_col] * input.col_cost[startup_col],
                Some(sol.x[startup_col]),
                Some(ObjectiveQuantityUnit::Event),
                Some(input.col_cost[startup_col]),
            ),
        );
        push_term(
            &mut terms,
            make_term(
                "branch_shutdown",
                ObjectiveBucket::Other,
                ObjectiveTermKind::BranchSwitchingShutdown,
                ObjectiveSubjectKind::Branch,
                branch_subject_id(branch),
                sol.x[shutdown_col] * input.col_cost[shutdown_col],
                Some(sol.x[shutdown_col]),
                Some(ObjectiveQuantityUnit::Event),
                Some(input.col_cost[shutdown_col]),
            ),
        );
    }

    if t == 0 {
        for (slack_idx, slack_kind) in input.energy_window_slack_kinds.iter().enumerate() {
            let col = input.energy_window_slack_base + slack_idx;
            let quantity_mwh = sol.x[col] * base;
            let direction = match slack_kind.direction {
                super::plan::EnergyWindowSlackDirection::Min => "min",
                super::plan::EnergyWindowSlackDirection::Max => "max",
            };
            push_term(
                &mut terms,
                make_term(
                    format!("energy_window:{}:{direction}", slack_kind.limit_idx),
                    ObjectiveBucket::Penalty,
                    ObjectiveTermKind::EnergyWindowPenalty,
                    ObjectiveSubjectKind::System,
                    SYSTEM_SUBJECT_ID,
                    sol.x[col] * input.col_cost[col],
                    Some(quantity_mwh),
                    Some(ObjectiveQuantityUnit::Mwh),
                    (base > 0.0).then_some(input.col_cost[col] / base),
                ),
            );
        }
    }

    if let Some(explicit_ctg) = input.explicit_contingency
        && let Some(period_plan) = explicit_ctg.periods.get(t)
    {
        push_term(
            &mut terms,
            make_term(
                "explicit_ctg_worst_case",
                ObjectiveBucket::Adder,
                ObjectiveTermKind::ExplicitContingencyWorstCase,
                ObjectiveSubjectKind::System,
                SYSTEM_SUBJECT_ID,
                sol.x[period_plan.worst_case_col] * input.col_cost[period_plan.worst_case_col],
                None,
                None,
                Some(input.col_cost[period_plan.worst_case_col]),
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
                sol.x[period_plan.avg_case_col] * input.col_cost[period_plan.avg_case_col],
                None,
                None,
                Some(input.col_cost[period_plan.avg_case_col]),
            ),
        );
    }

    for (plant_index, info) in input.cc_infos.iter().enumerate() {
        let plant_id = combined_cycle_plant_id(
            input
                .network
                .market_data
                .combined_cycle_plants
                .get(plant_index),
            plant_index,
        );
        for config_idx in 0..info.n_configs {
            let z_idx = input.cc_var_base + info.z_block_off + config_idx * input.n_hours + t;
            push_term(
                &mut terms,
                make_term(
                    format!("config:{config_idx}"),
                    ObjectiveBucket::NoLoad,
                    ObjectiveTermKind::CombinedCycleNoLoad,
                    ObjectiveSubjectKind::CombinedCyclePlant,
                    plant_id.clone(),
                    sol.x[z_idx] * input.col_cost[z_idx],
                    Some(sol.x[z_idx]),
                    Some(ObjectiveQuantityUnit::Event),
                    Some(input.col_cost[z_idx]),
                ),
            );
        }
        for transition_idx in 0..info.transition_pairs.len() {
            let idx =
                input.cc_var_base + info.ytrans_block_off + transition_idx * input.n_hours + t;
            push_term(
                &mut terms,
                make_term(
                    format!("transition:{transition_idx}"),
                    ObjectiveBucket::Other,
                    ObjectiveTermKind::CombinedCycleTransition,
                    ObjectiveSubjectKind::CombinedCyclePlant,
                    plant_id.clone(),
                    sol.x[idx] * input.col_cost[idx],
                    Some(sol.x[idx]),
                    Some(ObjectiveQuantityUnit::Event),
                    Some(input.col_cost[idx]),
                ),
            );
        }
        for (entry_idx, _) in info.pgcc_entries.iter().enumerate() {
            let idx = input.cc_var_base + info.pgcc_block_off + entry_idx * input.n_hours + t;
            let mw = sol.x[idx] * base;
            push_term(
                &mut terms,
                make_term(
                    format!("dispatch:{entry_idx}"),
                    ObjectiveBucket::Energy,
                    ObjectiveTermKind::CombinedCycleDispatch,
                    ObjectiveSubjectKind::CombinedCyclePlant,
                    plant_id.clone(),
                    sol.x[idx] * input.col_cost[idx],
                    Some(mw * dt_h),
                    Some(ObjectiveQuantityUnit::Mwh),
                    (dt_h > 0.0 && base > 0.0).then_some(input.col_cost[idx] / (base * dt_h)),
                ),
            );
        }
    }

    let objective_gap = period.total_cost - sum_terms(&terms);
    if objective_gap.abs() > 1e-6 {
        warn!(
            hour = t,
            period_total = period.total_cost,
            ledger_total = sum_terms(&terms),
            objective_gap,
            "SCUC: objective ledger does not fully reconcile with period total; emitting residual term"
        );
        push_term(
            &mut terms,
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

    terms
}

pub(super) struct PeriodAssemblyInput<'a> {
    pub network: &'a Network,
    pub hourly_networks: &'a [Network],
    pub sol: &'a LpResult,
    pub col_cost: &'a [f64],
    pub layout: &'a ScucLayout,
    pub cc_var_base: usize,
    pub cc_infos: &'a [ScucCcPlantInfo],
    pub reserve_layout: &'a crate::common::reserves::ReserveLpLayout,
    pub dl_act_var_base: usize,
    pub n_dl_activation: usize,
    pub dl_rebound_var_base: usize,
    pub n_dl_rebound: usize,
    pub energy_window_slack_base: usize,
    pub energy_window_slack_kinds: &'a [super::plan::EnergyWindowSlackKind],
    pub explicit_contingency: Option<&'a ExplicitContingencyObjectivePlan>,
    pub n_hours: usize,
    pub n_bus: usize,
    pub n_gen: usize,
    pub n_storage: usize,
    pub storage_gen_local: &'a [(usize, usize, usize)],
    pub sto_dis_offer_info: &'a [(usize, Vec<(f64, f64)>)],
    pub sto_ch_bid_info: &'a [(usize, Vec<(f64, f64)>)],
    pub constrained_branches: &'a [usize],
    pub fg_rows: &'a [usize],
    pub iface_rows: &'a [usize],
    pub gen_indices: &'a [usize],
    pub commitment: &'a [Vec<bool>],
    pub delta_gen_off: &'a [usize],
    pub startup_tier_capacity: &'a [usize],
    pub is_block_mode: bool,
    pub use_plc: bool,
    pub n_bp: usize,
    pub gen_blocks: &'a [Vec<crate::common::blocks::DispatchBlock>],
    pub gen_block_start: &'a [usize],
    pub pwl_gen_j: &'a [usize],
    pub lmp_out: &'a [Vec<f64>],
    pub pg_mw_all: &'a [Vec<f64>],
    pub storage_ch_all: &'a [Vec<f64>],
    pub storage_dis_all: &'a [Vec<f64>],
    pub hourly_reserve_results: &'a [Option<ReserveResults>],
    pub branch_shadow_prices: &'a [Vec<f64>],
    pub fg_shadow_prices: &'a [Vec<f64>],
    pub iface_shadow_prices: &'a [Vec<f64>],
    pub hvdc_dispatch_mw_out: &'a [Vec<f64>],
    pub hvdc_band_dispatch_mw_out: &'a [Vec<Vec<f64>>],
    pub hvdc_band_offsets: &'a [usize],
    pub dloss_dp_out: &'a [Vec<f64>],
    pub effective_co2_price: f64,
    pub effective_co2_rate: &'a [f64],
    pub step_h: f64,
    pub dl_list: &'a [&'a DispatchableLoad],
    pub dl_orig_idx: &'a [usize],
    pub dl_off: usize,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub active_vbids: &'a [usize],
    pub vbid_off: usize,
    pub bus_map: &'a HashMap<u32, usize>,
    pub island_refs: &'a IslandRefs,
    pub base: f64,
    pub angle_constrained_branches: &'a [AngleConstrainedBranch],
}

pub(super) struct PeriodAssemblyOutput {
    pub periods: Vec<RawDispatchPeriodResult>,
    pub total_co2_t: f64,
}

fn period_costs_from_objective(input: &PeriodAssemblyInput<'_>) -> Vec<f64> {
    let mut period_costs = vec![0.0; input.n_hours];
    let vars_per_hour = input.layout.vars_per_hour();

    for (t, period_total_cost) in period_costs.iter_mut().enumerate() {
        let hour_base = input.layout.hour_col_base(t);
        for col in hour_base..hour_base + vars_per_hour {
            *period_total_cost += input.sol.x[col] * input.col_cost[col];
        }
    }

    for info in input.cc_infos {
        for config_idx in 0..info.n_configs {
            for (t, period_total_cost) in period_costs.iter_mut().enumerate() {
                let z_idx = input.cc_var_base + info.z_block_off + config_idx * input.n_hours + t;
                let yup_idx = input.cc_var_base
                    + info.z_block_off
                    + info.n_configs * input.n_hours
                    + config_idx * input.n_hours
                    + t;
                let ydn_idx = input.cc_var_base
                    + info.z_block_off
                    + 2 * info.n_configs * input.n_hours
                    + config_idx * input.n_hours
                    + t;
                *period_total_cost += input.sol.x[z_idx] * input.col_cost[z_idx];
                *period_total_cost += input.sol.x[yup_idx] * input.col_cost[yup_idx];
                *period_total_cost += input.sol.x[ydn_idx] * input.col_cost[ydn_idx];
            }
        }
        for transition_idx in 0..info.transition_pairs.len() {
            for (t, period_total_cost) in period_costs.iter_mut().enumerate() {
                let idx =
                    input.cc_var_base + info.ytrans_block_off + transition_idx * input.n_hours + t;
                *period_total_cost += input.sol.x[idx] * input.col_cost[idx];
            }
        }
        for entry_idx in 0..info.pgcc_entries.len() {
            for (t, period_total_cost) in period_costs.iter_mut().enumerate() {
                let idx = input.cc_var_base + info.pgcc_block_off + entry_idx * input.n_hours + t;
                *period_total_cost += input.sol.x[idx] * input.col_cost[idx];
            }
        }
    }

    for block_idx in 0..input.n_dl_activation {
        for (t, period_total_cost) in period_costs.iter_mut().enumerate() {
            let idx = input.dl_act_var_base + block_idx * input.n_hours + t;
            *period_total_cost += input.sol.x[idx] * input.col_cost[idx];
        }
    }

    for block_idx in 0..input.n_dl_rebound {
        for (t, period_total_cost) in period_costs.iter_mut().enumerate() {
            let idx = input.dl_rebound_var_base + block_idx * input.n_hours + t;
            *period_total_cost += input.sol.x[idx] * input.col_cost[idx];
        }
    }

    if let Some(explicit_ctg) = input.explicit_contingency {
        for (period_idx, period) in explicit_ctg.periods.iter().enumerate() {
            if period_idx >= period_costs.len() {
                break;
            }
            period_costs[period_idx] +=
                input.sol.x[period.worst_case_col] * input.col_cost[period.worst_case_col];
            period_costs[period_idx] +=
                input.sol.x[period.avg_case_col] * input.col_cost[period.avg_case_col];
        }
    }

    let allocated_cost: f64 = period_costs.iter().sum();
    let residue = input.sol.objective - allocated_cost;
    if residue.abs() > 1e-6
        && let Some(first_period_cost) = period_costs.first_mut()
    {
        *first_period_cost += residue;
    }

    period_costs
}

#[allow(clippy::needless_range_loop)]
pub(super) fn build_period_results(input: PeriodAssemblyInput<'_>) -> PeriodAssemblyOutput {
    let mut periods = Vec::with_capacity(input.n_hours);
    let mut total_co2_t = 0.0;
    let period_costs = period_costs_from_objective(&input);

    for t in 0..input.n_hours {
        let pg_t = &input.pg_mw_all[t];
        let co2_t: f64 = pg_t
            .iter()
            .zip(input.effective_co2_rate.iter())
            .map(|(pg, &rate)| pg * rate * input.step_h)
            .sum();
        total_co2_t += co2_t;

        let lmp_t = &input.lmp_out[t];
        let (lmp_energy, lmp_congestion, lmp_loss) = if input.spec.use_loss_factors
            && input
                .dloss_dp_out
                .get(t)
                .is_some_and(|dloss| dloss.len() == lmp_t.len())
        {
            surge_opf::advanced::decompose_lmp_with_losses(
                lmp_t,
                &input.dloss_dp_out[t],
                input.island_refs,
            )
        } else {
            surge_opf::advanced::decompose_lmp_lossless(lmp_t, input.island_refs)
        };

        let sto_soc_t: Vec<f64> = (0..input.n_storage)
            .map(|s| input.sol.x[input.layout.storage_soc_col(t, s)])
            .collect();

        let dr_results_t = crate::common::extraction::extract_dr_results(
            &input.sol.x,
            input.dl_list,
            input.dl_off,
            lmp_t,
            input.bus_map,
            input.base,
            |off| input.layout.col(t, off),
        );
        let virtual_bid_results_t = crate::common::extraction::extract_virtual_bid_results(
            &input.sol.x,
            input.spec,
            input.active_vbids,
            input.vbid_off,
            lmp_t,
            input.bus_map,
            input.base,
            |off| input.layout.col(t, off),
        );

        let pb_curtailment_by_bus_mw: Vec<f64> = (0..input.n_bus)
            .map(|bus_idx| {
                input.sol.x[input.layout.pb_curtailment_bus_col(t, bus_idx)] * input.base
            })
            .collect();
        let pb_excess_by_bus_mw: Vec<f64> = (0..input.n_bus)
            .map(|bus_idx| input.sol.x[input.layout.pb_excess_bus_col(t, bus_idx)] * input.base)
            .collect();
        let pb_curtailment_mw: f64 = pb_curtailment_by_bus_mw.iter().sum();
        let pb_excess_mw: f64 = pb_excess_by_bus_mw.iter().sum();
        if pb_curtailment_mw > 1e-4 {
            tracing::warn!(
                hour = t,
                curtailment_mw = pb_curtailment_mw,
                "SCUC: load curtailment — insufficient generation"
            );
        }
        if pb_excess_mw > 1e-4 {
            tracing::warn!(
                hour = t,
                excess_mw = pb_excess_mw,
                "SCUC: excess generation — minimum generation exceeds load"
            );
        }

        let headroom_slack_total: f64 = (0..input.n_gen)
            .map(|j| input.sol.x[input.layout.headroom_slack_col(t, j)] * input.base)
            .sum();
        let footroom_slack_total: f64 = (0..input.n_gen)
            .map(|j| input.sol.x[input.layout.footroom_slack_col(t, j)] * input.base)
            .sum();
        if headroom_slack_total > 1e-4 {
            tracing::warn!(
                hour = t,
                headroom_slack_mw = headroom_slack_total,
                "SCUC: headroom capacity slack active — commitment coupling relaxed"
            );
        }
        if footroom_slack_total > 1e-4 {
            tracing::warn!(
                hour = t,
                footroom_slack_mw = footroom_slack_total,
                "SCUC: footroom capacity slack active — commitment coupling relaxed"
            );
        }

        let thermal_penalty = input.spec.thermal_penalty_curve.marginal_cost_at(0.0);
        let ramp_penalty = input.spec.ramp_penalty_curve.marginal_cost_at(0.0);
        let commitment_capacity_penalty = 1e6;
        let dt_h = input.step_h;
        let pb_curtailment_rate = input
            .spec
            .power_balance_penalty
            .curtailment
            .first()
            .map(|(_, price)| *price);
        let pb_excess_rate = input
            .spec
            .power_balance_penalty
            .excess
            .first()
            .map(|(_, price)| *price);
        let mut constraint_results = Vec::new();
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
        for (row_idx, &branch_idx) in input.constrained_branches.iter().enumerate() {
            let branch = &input.network.branches[branch_idx];
            let reverse_slack_mw =
                input.sol.x[input.layout.branch_lower_slack_col(t, row_idx)] * input.base;
            let forward_slack_mw =
                input.sol.x[input.layout.branch_upper_slack_col(t, row_idx)] * input.base;
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
        for (row_idx, &fg_idx) in input.fg_rows.iter().enumerate() {
            let flowgate = &input.network.flowgates[fg_idx];
            let reverse_slack_mw =
                input.sol.x[input.layout.flowgate_lower_slack_col(t, row_idx)] * input.base;
            let forward_slack_mw =
                input.sol.x[input.layout.flowgate_upper_slack_col(t, row_idx)] * input.base;
            let is_explicit_ctg_flowgate = input
                .explicit_contingency
                .and_then(|plan| plan.flowgate_row_cases.get(row_idx))
                .copied()
                .flatten()
                .is_some();
            let penalty_cost = (!is_explicit_ctg_flowgate).then_some(thermal_penalty);
            if reverse_slack_mw > 1e-4 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("flowgate:{}:reverse", flowgate.name),
                    kind: ConstraintKind::Flowgate,
                    scope: ConstraintScope::Flowgate,
                    slack_mw: Some(reverse_slack_mw),
                    penalty_cost,
                    penalty_dollars: penalty_cost.map(|r| reverse_slack_mw * r * dt_h),
                    ..Default::default()
                });
            }
            if forward_slack_mw > 1e-4 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("flowgate:{}:forward", flowgate.name),
                    kind: ConstraintKind::Flowgate,
                    scope: ConstraintScope::Flowgate,
                    slack_mw: Some(forward_slack_mw),
                    penalty_cost,
                    penalty_dollars: penalty_cost.map(|r| forward_slack_mw * r * dt_h),
                    ..Default::default()
                });
            }
        }
        for (row_idx, &iface_idx) in input.iface_rows.iter().enumerate() {
            let iface = &input.network.interfaces[iface_idx];
            let reverse_slack_mw =
                input.sol.x[input.layout.interface_lower_slack_col(t, row_idx)] * input.base;
            let forward_slack_mw =
                input.sol.x[input.layout.interface_upper_slack_col(t, row_idx)] * input.base;
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
        for (j, &gi) in input.gen_indices.iter().enumerate() {
            let resource_id = input.network.generators[gi].id.clone();
            let headroom_slack_mw = input.sol.x[input.layout.headroom_slack_col(t, j)] * input.base;
            let footroom_slack_mw = input.sol.x[input.layout.footroom_slack_col(t, j)] * input.base;
            let ramp_up_slack_mw = input.sol.x[input.layout.ramp_up_slack_col(t, j)] * input.base;
            let ramp_down_slack_mw =
                input.sol.x[input.layout.ramp_down_slack_col(t, j)] * input.base;
            if headroom_slack_mw > 1e-4 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("headroom:{resource_id}"),
                    kind: ConstraintKind::CommitmentCapacity,
                    scope: ConstraintScope::Resource,
                    slack_mw: Some(headroom_slack_mw),
                    penalty_cost: Some(commitment_capacity_penalty),
                    penalty_dollars: Some(headroom_slack_mw * commitment_capacity_penalty * dt_h),
                    ..Default::default()
                });
            }
            if footroom_slack_mw > 1e-4 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("footroom:{resource_id}"),
                    kind: ConstraintKind::CommitmentCapacity,
                    scope: ConstraintScope::Resource,
                    slack_mw: Some(footroom_slack_mw),
                    penalty_cost: Some(commitment_capacity_penalty),
                    penalty_dollars: Some(footroom_slack_mw * commitment_capacity_penalty * dt_h),
                    ..Default::default()
                });
            }
            if ramp_up_slack_mw > 1e-4 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("ramp_up:{resource_id}"),
                    kind: ConstraintKind::Ramp,
                    scope: ConstraintScope::Resource,
                    slack_mw: Some(ramp_up_slack_mw),
                    penalty_cost: Some(ramp_penalty),
                    penalty_dollars: Some(ramp_up_slack_mw * ramp_penalty * dt_h),
                    ..Default::default()
                });
            }
            if ramp_down_slack_mw > 1e-4 {
                constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("ramp_down:{resource_id}"),
                    kind: ConstraintKind::Ramp,
                    scope: ConstraintScope::Resource,
                    slack_mw: Some(ramp_down_slack_mw),
                    penalty_cost: Some(ramp_penalty),
                    penalty_dollars: Some(ramp_down_slack_mw * ramp_penalty * dt_h),
                    ..Default::default()
                });
            }
            // Pg column bound shadows (reduced cost of the Pg variable).
            // These are only populated when the LP backend returned
            // col_dual data (pure LP solve, non-MIP). For fixed-
            // commitment SCUC the solve reduces to an LP and the reduced
            // cost tells us exactly how "pinned" each generator was
            // economically. Consumers (e.g. the AC target tracking
            // builder) read these shadows via
            // `pg_upper:<resource_id>` / `pg_lower:<resource_id>`
            // constraint_ids.
            if !input.sol.col_dual.is_empty() {
                let pg_col = input.layout.pg_col(t, j);
                if pg_col < input.sol.col_dual.len() {
                    let raw = input.sol.col_dual[pg_col];
                    // Gurobi returns the reduced cost directly. For a
                    // minimization LP, reduced cost > 0 ⇒ at lower
                    // bound; reduced cost < 0 ⇒ at upper bound.
                    // The canonical shadow representation is the
                    // magnitude, with the direction recorded in the
                    // constraint_id prefix.
                    let pg_lower_shadow = raw.max(0.0) / input.base;
                    let pg_upper_shadow = (-raw).max(0.0) / input.base;
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
                }
            }
        }
        // Angle difference slacks.
        let angle_penalty_rate = input.spec.angle_penalty_curve.marginal_cost_at(0.0);
        for (row_idx, acb) in input.angle_constrained_branches.iter().enumerate() {
            let branch = &input.network.branches[acb.branch_idx];
            let upper_slack_rad = input.sol.x[input.layout.angle_diff_upper_slack_col(t, row_idx)];
            let lower_slack_rad = input.sol.x[input.layout.angle_diff_lower_slack_col(t, row_idx)];
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

        let mut raw_period = RawDispatchPeriodResult {
            pg_mw: pg_t.clone(),
            lmp: lmp_t.clone(),
            lmp_energy,
            lmp_congestion,
            total_cost: period_costs[t],
            co2_t,
            hvdc_dispatch_mw: input.hvdc_dispatch_mw_out[t].clone(),
            hvdc_band_dispatch_mw: input.hvdc_band_dispatch_mw_out[t].clone(),
            storage_charge_mw: input.storage_ch_all[t].clone(),
            storage_discharge_mw: input.storage_dis_all[t].clone(),
            storage_soc_mwh: sto_soc_t,
            lmp_loss,
            branch_shadow_prices: input
                .branch_shadow_prices
                .get(t)
                .cloned()
                .unwrap_or_default(),
            flowgate_shadow_prices: input.fg_shadow_prices.get(t).cloned().unwrap_or_default(),
            interface_shadow_prices: input
                .iface_shadow_prices
                .get(t)
                .cloned()
                .unwrap_or_default(),
            par_results: vec![],
            dr_results: dr_results_t,
            virtual_bid_results: virtual_bid_results_t,
            reserve_awards: input.hourly_reserve_results[t]
                .as_ref()
                .map(|rr| rr.awards.clone())
                .unwrap_or_default(),
            reserve_prices: input.hourly_reserve_results[t]
                .as_ref()
                .map(|rr| rr.prices.clone())
                .unwrap_or_default(),
            reserve_provided: input.hourly_reserve_results[t]
                .as_ref()
                .map(|rr| rr.provided.clone())
                .unwrap_or_default(),
            reserve_shortfall: input.hourly_reserve_results[t]
                .as_ref()
                .map(|rr| rr.shortfall.clone())
                .unwrap_or_default(),
            zonal_reserve_prices: input.hourly_reserve_results[t]
                .as_ref()
                .map(|rr| rr.zonal_prices.clone())
                .unwrap_or_default(),
            zonal_reserve_shortfall: input.hourly_reserve_results[t]
                .as_ref()
                .map(|rr| rr.zonal_shortfall.clone())
                .unwrap_or_default(),
            dr_reserve_awards: input.hourly_reserve_results[t]
                .as_ref()
                .map(|rr| rr.dl_awards.clone())
                .unwrap_or_default(),
            power_balance_violation: surge_network::market::PowerBalanceViolation {
                curtailment_mw: pb_curtailment_mw,
                excess_mw: pb_excess_mw,
                curtailment_cost: pb_curtailment_rate
                    .map(|r| pb_curtailment_mw * r * dt_h)
                    .unwrap_or(0.0),
                excess_cost: pb_excess_rate
                    .map(|r| pb_excess_mw * r * dt_h)
                    .unwrap_or(0.0),
            },
            objective_terms: Vec::new(),
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
            // SCED-AC Benders is SCED-only; SCUC does not produce eta values.
            sced_ac_benders_eta_dollars_per_hour: None,
        };
        raw_period.objective_terms = build_period_objective_terms(&input, t, &raw_period);
        periods.push(raw_period);
    }

    PeriodAssemblyOutput {
        periods,
        total_co2_t,
    }
}

pub(super) fn extract_bus_angles(
    sol: &LpResult,
    layout: &ScucLayout,
    n_hours: usize,
    n_bus: usize,
) -> Vec<Vec<f64>> {
    (0..n_hours)
        .map(|t| (0..n_bus).map(|i| sol.x[layout.theta_col(t, i)]).collect())
        .collect()
}

pub(super) fn extract_penalty_slacks(
    sol: &LpResult,
    penalty_slack_base: usize,
    n_penalty_slacks: usize,
) -> Vec<f64> {
    (0..n_penalty_slacks)
        .map(|i| sol.x[penalty_slack_base + i])
        .collect()
}

pub(super) struct SolutionAssemblyInput<'a> {
    pub network: &'a Network,
    pub hourly_networks: &'a [Network],
    pub spec: &'a DispatchProblemSpec<'a>,
    pub sol: &'a LpResult,
    /// Post-repricing LP solution. `None` when pricing was skipped —
    /// callers must gate lookups on `pricing_summary.pricing_converged`.
    pub lp_sol: Option<&'a LpResult>,
    pub layout: &'a ScucLayout,
    pub col_cost: &'a [f64],
    pub cc_var_base: usize,
    pub cc_infos: &'a [ScucCcPlantInfo],
    pub reserve_layout: &'a crate::common::reserves::ReserveLpLayout,
    pub dl_act_var_base: usize,
    pub angle_constrained_branches: &'a [AngleConstrainedBranch],
    pub n_dl_activation: usize,
    pub dl_rebound_var_base: usize,
    pub n_dl_rebound: usize,
    pub explicit_contingency: Option<&'a ExplicitContingencyObjectivePlan>,
    /// Multi-interval energy window slack columns. Empty when there
    /// are no energy windows.
    pub energy_window_slack_base: usize,
    pub energy_window_slack_kinds: &'a [super::plan::EnergyWindowSlackKind],
    pub extracted: CommitmentExtraction,
    pub pricing_summary: PricingSummary,
    pub effective_co2_price: f64,
    pub effective_co2_rate: &'a [f64],
    pub hvdc_band_offsets: &'a [usize],
    pub n_hours: usize,
    pub n_gen: usize,
    pub n_bus: usize,
    pub n_storage: usize,
    pub storage_gen_local: &'a [(usize, usize, usize)],
    pub sto_dis_offer_info: &'a [(usize, Vec<(f64, f64)>)],
    pub sto_ch_bid_info: &'a [(usize, Vec<(f64, f64)>)],
    pub constrained_branches: &'a [usize],
    pub fg_rows: &'a [usize],
    pub iface_rows: &'a [usize],
    pub gen_indices: &'a [usize],
    pub delta_gen_off: &'a [usize],
    pub startup_tier_capacity: &'a [usize],
    pub is_block_mode: bool,
    pub use_plc: bool,
    pub n_bp: usize,
    pub gen_blocks: &'a [Vec<crate::common::blocks::DispatchBlock>],
    pub gen_block_start: &'a [usize],
    pub pwl_gen_j: &'a [usize],
    pub solve_time_secs: f64,
    pub penalty_slack_base: usize,
    pub n_penalty_slacks: usize,
    pub dl_list: &'a [&'a DispatchableLoad],
    pub dl_orig_idx: &'a [usize],
    pub dl_off: usize,
    pub active_vbids: &'a [usize],
    pub vbid_off: usize,
    pub bus_map: &'a HashMap<u32, usize>,
    pub island_refs: &'a IslandRefs,
    pub base: f64,
    pub bus_loss_allocation_mw: Vec<Vec<f64>>,
}

pub(super) struct ScucExtractionInput<'a> {
    pub network: &'a Network,
    pub solve: &'a DcSolveSession<'a>,
    pub pricing_state: PricingRunState<'a>,
    pub solve_time_secs: f64,
}

pub(super) fn extract_solution(input: ScucExtractionInput<'_>) -> RawDispatchSolution {
    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let bus_map = &input.solve.bus_map;
    let island_refs = &input.solve.island_refs;
    let base = input.solve.base_mva;
    let PricingRunState {
        primary_state,
        lp_sol,
        summary: pricing_summary,
    } = input.pricing_state;
    let captured_model_diagnostic = primary_state.model_diagnostic;
    let captured_bus_loss_allocation_mw = primary_state.bus_loss_allocation_mw;
    let captured_commitment_mip_trace = primary_state.commitment_mip_trace.clone();
    let model_plan = primary_state.problem_plan.model_plan;
    let sol = &primary_state.solution;
    let layout_plan = &model_plan.layout;
    let layout = &layout_plan.layout;
    let active_inputs = &layout_plan.active;
    let startup_plan = &model_plan.startup;
    let variable_plan = &model_plan.variable;
    let n_hours = spec.n_periods;
    let n_gen = setup.n_gen;
    let n_bus = input.network.n_buses();
    let n_storage = setup.n_storage;
    let extracted = extract_commitment_dispatch(CommitmentExtractionInput {
        sol,
        layout,
        gen_indices: &setup.gen_indices,
        gen_tier_info_by_hour: &startup_plan.gen_tier_info_by_hour,
        delta_gen_off: &startup_plan.delta_gen_off,
        storage_gen_local: &setup.storage_gen_local,
        n_storage,
        has_reg_products: active_inputs.has_reg_products,
        regulation_offset: layout.regulation_mode,
        n_hours,
        base,
    });

    let mut result = assemble_solution(SolutionAssemblyInput {
        network: input.network,
        hourly_networks: &model_plan.hourly_networks,
        spec,
        sol,
        lp_sol: lp_sol.as_ref(),
        layout,
        col_cost: &primary_state.problem_plan.columns.col_cost,
        cc_var_base: variable_plan.cc_var_base,
        cc_infos: &variable_plan.cc_infos,
        reserve_layout: &active_inputs.reserve_layout,
        dl_act_var_base: variable_plan.dl_act_var_base,
        n_dl_activation: model_plan.layout.dl_activation_infos.len(),
        dl_rebound_var_base: variable_plan.dl_rebound_var_base,
        n_dl_rebound: variable_plan.n_dl_rebound,
        explicit_contingency: variable_plan.explicit_contingency.as_ref(),
        energy_window_slack_base: variable_plan.energy_window_slack_base,
        energy_window_slack_kinds: &variable_plan.energy_window_slack_kinds,
        extracted,
        pricing_summary,
        effective_co2_price: setup.effective_co2_price,
        effective_co2_rate: &setup.effective_co2_rate,
        hvdc_band_offsets: &setup.hvdc_band_offsets_rel,
        n_hours,
        n_gen,
        n_bus,
        n_storage,
        storage_gen_local: &setup.storage_gen_local,
        sto_dis_offer_info: &setup.sto_dis_offer_info,
        sto_ch_bid_info: &setup.sto_ch_bid_info,
        constrained_branches: &model_plan.network_plan.constrained_branches,
        fg_rows: &model_plan.network_plan.fg_rows,
        iface_rows: &model_plan.network_plan.iface_rows,
        gen_indices: &setup.gen_indices,
        delta_gen_off: &startup_plan.delta_gen_off,
        startup_tier_capacity: &startup_plan.startup_tier_capacity,
        is_block_mode: setup.is_block_mode,
        use_plc: model_plan.use_plc,
        n_bp: model_plan.n_bp,
        gen_blocks: &setup.gen_blocks,
        gen_block_start: &setup.gen_block_start,
        pwl_gen_j: &model_plan.pwl.gen_j,
        solve_time_secs: input.solve_time_secs,
        penalty_slack_base: variable_plan.penalty_slack_base,
        n_penalty_slacks: variable_plan.n_penalty_slacks,
        dl_list: &active_inputs.dl_list,
        dl_orig_idx: &active_inputs.dl_orig_idx,
        dl_off: layout.dispatch.dl,
        active_vbids: &active_inputs.active_vbids,
        vbid_off: layout.dispatch.vbid,
        bus_map,
        island_refs,
        base,
        angle_constrained_branches: &model_plan.network_plan.angle_constrained_branches,
        bus_loss_allocation_mw: captured_bus_loss_allocation_mw,
    });
    result.model_diagnostics = captured_model_diagnostic.into_iter().collect();
    result.diagnostics.commitment_mip_trace = captured_commitment_mip_trace;
    result
}

pub(super) fn assemble_solution(input: SolutionAssemblyInput<'_>) -> RawDispatchSolution {
    let SolutionAssemblyInput {
        network,
        hourly_networks,
        spec,
        sol,
        lp_sol,
        layout,
        col_cost,
        cc_var_base,
        cc_infos,
        reserve_layout,
        dl_act_var_base,
        n_dl_activation,
        dl_rebound_var_base,
        n_dl_rebound,
        explicit_contingency,
        mut extracted,
        pricing_summary,
        effective_co2_price,
        effective_co2_rate,
        hvdc_band_offsets,
        n_hours,
        n_gen,
        n_bus,
        n_storage,
        storage_gen_local,
        sto_dis_offer_info,
        sto_ch_bid_info,
        constrained_branches,
        fg_rows,
        iface_rows,
        gen_indices,
        delta_gen_off,
        startup_tier_capacity,
        is_block_mode,
        use_plc,
        n_bp,
        gen_blocks,
        gen_block_start,
        pwl_gen_j,
        solve_time_secs,
        penalty_slack_base,
        n_penalty_slacks,
        dl_list,
        dl_orig_idx,
        dl_off,
        active_vbids,
        vbid_off,
        bus_map,
        island_refs,
        energy_window_slack_base,
        energy_window_slack_kinds,
        base,
        angle_constrained_branches,
        bus_loss_allocation_mw,
    } = input;
    let cc_plants = &network.market_data.combined_cycle_plants;

    // Warn when any energy-window slack is active so operators can see
    // where the dispatch is paying the soft penalty. The cost itself
    // is already accounted for in `sol.objective`, which is what
    // `total_cost` reads below. Also create ConstraintPeriodResult
    // entries for PenaltySummary aggregation.
    let mut energy_window_constraint_results: Vec<RawConstraintPeriodResult> = Vec::new();
    if !energy_window_slack_kinds.is_empty() {
        let ew_penalty_per_puh = spec.energy_window_violation_per_puh;
        for (slack_offset, kind) in energy_window_slack_kinds.iter().enumerate() {
            let col = energy_window_slack_base + slack_offset;
            let slack_pu_h = sol.x.get(col).copied().unwrap_or(0.0);
            if slack_pu_h > 1e-9 {
                let slack_mwh = slack_pu_h * base;
                tracing::warn!(
                    limit_idx = kind.limit_idx,
                    direction = ?kind.direction,
                    slack_mwh,
                    "SCUC: multi-interval energy window slack active — \
                     soft constraint violation"
                );
                let direction_str = match kind.direction {
                    super::plan::EnergyWindowSlackDirection::Min => "min",
                    super::plan::EnergyWindowSlackDirection::Max => "max",
                };
                let penalty_dollars = if ew_penalty_per_puh > 0.0 {
                    // The slack variable is in pu·h, and the penalty is
                    // $/pu·h, so the cost is slack_pu_h × rate × base_mva.
                    Some(slack_pu_h * ew_penalty_per_puh * base)
                } else {
                    None
                };
                energy_window_constraint_results.push(RawConstraintPeriodResult {
                    constraint_id: format!("energy_window:{}:{direction_str}", kind.limit_idx),
                    kind: ConstraintKind::EnergyWindow,
                    scope: ConstraintScope::System,
                    slack_mw: Some(slack_mwh),
                    penalty_cost: if ew_penalty_per_puh > 0.0 {
                        Some(ew_penalty_per_puh * base)
                    } else {
                        None
                    },
                    penalty_dollars,
                    ..Default::default()
                });
            }
        }
    }
    let has_hvdc = !spec.hvdc_links.is_empty();
    let step_h = spec.dt_hours;
    let iterations = sol.iterations;

    if spec.enforce_flowgates
        && spec.max_nomogram_iter > 0
        && !network.nomograms.is_empty()
        && pricing_summary.pricing_converged
    {
        // pricing_converged true → lp_sol must be Some; defensive match.
        if let Some(lp) = lp_sol {
            for (hour, pg_t) in extracted.pg_mw_all.iter_mut().enumerate().take(n_hours) {
                for (gen_idx, pg) in pg_t.iter_mut().enumerate().take(n_gen) {
                    *pg = lp.x[layout.pg_col(hour, gen_idx)] * base;
                }
            }
        }
    }

    let (hvdc_dispatch_mw_out, hvdc_band_dispatch_mw_out): (Vec<Vec<f64>>, Vec<Vec<Vec<f64>>>) =
        if has_hvdc {
            (0..n_hours)
                .map(|hour| {
                    crate::common::extraction::extract_hvdc_dispatch(
                        &sol.x,
                        spec,
                        hvdc_band_offsets,
                        base,
                        |off| layout.col(hour, layout.dispatch.hvdc + off),
                    )
                })
                .unzip()
        } else {
            (vec![vec![]; n_hours], vec![vec![]; n_hours])
        };

    let mut period_output = build_period_results(PeriodAssemblyInput {
        network,
        hourly_networks,
        sol,
        col_cost,
        layout,
        cc_var_base,
        cc_infos,
        reserve_layout,
        dl_act_var_base,
        n_dl_activation,
        dl_rebound_var_base,
        n_dl_rebound,
        energy_window_slack_base,
        energy_window_slack_kinds,
        explicit_contingency,
        n_hours,
        n_bus: network.n_buses(),
        n_gen,
        n_storage,
        storage_gen_local,
        sto_dis_offer_info,
        sto_ch_bid_info,
        constrained_branches,
        fg_rows,
        iface_rows,
        gen_indices,
        commitment: &extracted.commitment,
        delta_gen_off,
        startup_tier_capacity,
        is_block_mode,
        use_plc,
        n_bp,
        gen_blocks,
        gen_block_start,
        pwl_gen_j,
        lmp_out: &pricing_summary.lmp_out,
        pg_mw_all: &extracted.pg_mw_all,
        storage_ch_all: &extracted.storage_ch_all,
        storage_dis_all: &extracted.storage_dis_all,
        hourly_reserve_results: &pricing_summary.hourly_reserve_results,
        branch_shadow_prices: &pricing_summary.branch_shadow_prices,
        fg_shadow_prices: &pricing_summary.fg_shadow_prices,
        iface_shadow_prices: &pricing_summary.iface_shadow_prices,
        hvdc_dispatch_mw_out: &hvdc_dispatch_mw_out,
        hvdc_band_dispatch_mw_out: &hvdc_band_dispatch_mw_out,
        hvdc_band_offsets,
        dloss_dp_out: &pricing_summary.dloss_dp_out,
        effective_co2_price,
        effective_co2_rate,
        step_h,
        dl_list,
        dl_orig_idx,
        dl_off,
        spec,
        active_vbids,
        vbid_off,
        bus_map,
        island_refs,
        base,
        angle_constrained_branches,
    });

    // Attach energy window constraint results to period 0 (they span
    // the full horizon and can't be meaningfully split by period).
    if !energy_window_constraint_results.is_empty() {
        if let Some(first_period) = period_output.periods.first_mut() {
            first_period
                .constraint_results
                .extend(energy_window_constraint_results);
        }
    }

    let mut cc_config_schedule = Vec::new();
    let mut cc_transition_cost = 0.0f64;
    let mut cc_transition_costs = vec![0.0; cc_infos.len()];
    if !cc_infos.is_empty() {
        for hour in 0..n_hours {
            let mut configs_t = Vec::with_capacity(cc_infos.len());
            for (plant_idx, info) in cc_infos.iter().enumerate() {
                let plant = &cc_plants[plant_idx];
                let mut active = None;
                for (config_idx, config) in plant.configs.iter().enumerate() {
                    let z_idx = info.z_block_off + config_idx * n_hours + hour;
                    if sol.x[layout.penalty_slack_base(n_hours) + n_penalty_slacks + z_idx] > 0.5 {
                        active = Some(config.name.clone());
                        break;
                    }
                }
                configs_t.push(active);

                for (transition_idx, &(from_config, to_config)) in
                    info.transition_pairs.iter().enumerate()
                {
                    let ytrans_idx = layout.penalty_slack_base(n_hours)
                        + n_penalty_slacks
                        + info.ytrans_block_off
                        + transition_idx * n_hours
                        + hour;
                    if sol.x[ytrans_idx] > 0.5
                        && let Some(&(cost, _)) =
                            info.allowed_transitions.get(&(from_config, to_config))
                    {
                        cc_transition_cost += cost;
                        cc_transition_costs[plant_idx] += cost;
                    }
                }
            }
            cc_config_schedule.push(configs_t);
        }
    }

    let total_cost = sol.objective;
    let operating_cost = total_cost - extracted.total_startup_cost;
    tracing::info!(
        solve_time_secs,
        hours = n_hours,
        generators = n_gen,
        total_cost,
        mip_iterations = iterations,
        "SCUC solved"
    );
    let co2_shadow_price = if effective_co2_price > 0.0 {
        effective_co2_rate
            .iter()
            .map(|&rate| effective_co2_price * rate)
            .collect()
    } else {
        vec![0.0; n_gen]
    };

    let commitment_kind = match spec.commitment {
        CommitmentMode::AllCommitted => CommitmentPolicyKind::AllCommitted,
        CommitmentMode::Fixed { .. } => CommitmentPolicyKind::Fixed,
        CommitmentMode::Optimize(_) => CommitmentPolicyKind::Optimize,
        CommitmentMode::Additional { .. } => CommitmentPolicyKind::Additional,
    };
    // The orchestrator in dispatch.rs overwrites study via
    // attach_public_catalogs_and_solve_metadata for production paths.
    // These defaults are for direct-call paths (tests, internal use).
    // SCUC is always time-coupled (monolithic MILP over all periods).
    RawDispatchSolution {
        study: DispatchStudy {
            formulation: Formulation::Dc,
            coupling: IntervalCoupling::TimeCoupled,
            commitment: commitment_kind,
            periods: n_hours,
            security_enabled: false,
            stage: None,
        },
        resources: Vec::new(),
        buses: Vec::new(),
        summary: crate::DispatchSummary {
            total_cost,
            total_co2_t: period_output.total_co2_t,
            ..Default::default()
        },
        diagnostics: crate::DispatchDiagnostics {
            iterations,
            solve_time_secs,
            // phase_timings is populated by solve_scuc_with_problem_spec
            // after extract_solution returns so its own wall counts too.
            phase_timings: None,
            pricing_converged: Some(pricing_summary.pricing_converged),
            penalty_slack_values: extract_penalty_slacks(sol, penalty_slack_base, n_penalty_slacks),
            security: None,
            sced_ac_benders: None,
            ac_sced_period_timings: Vec::new(),
            // Filled in by the caller (extract_solution) after assembly.
            commitment_mip_trace: None,
        },
        periods: period_output.periods,
        commitment: Some(extracted.commitment),
        startup: Some(extracted.startup_events),
        shutdown: Some(extracted.shutdown_events),
        operating_cost: Some(operating_cost),
        startup_cost_total: Some(extracted.total_startup_cost),
        startup_costs: None,
        system_inertia_s: None,
        estimated_rocof_hz_per_s: None,
        frequency_secure: None,
        co2_shadow_price,
        storage_soc: extracted.storage_soc_out,
        bus_loss_allocation_mw,
        bus_angles_rad: extract_bus_angles(sol, layout, n_hours, n_bus),
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
        regulation: extracted.regulation_all,
        branch_commitment_state: extract_branch_commitment_state(
            sol,
            layout,
            n_hours,
            input.network,
            input.spec.allow_branch_switching,
        ),
        cc_config_schedule,
        cc_transition_cost,
        cc_transition_costs,
        model_diagnostics: Vec::new(),
    }
}

/// Extract the cleared branch commitment `[t][branch_local_idx]` from
/// the LP solution when `allow_branch_switching = true`. Returns an
/// empty vec otherwise so the `branch_commitment_state` field only
/// carries meaningful data in the switching-enabled mode.
pub(super) fn extract_branch_commitment_state(
    sol: &LpResult,
    layout: &ScucLayout,
    n_hours: usize,
    network: &Network,
    allow_branch_switching: bool,
) -> Vec<Vec<bool>> {
    if !allow_branch_switching {
        return Vec::new();
    }
    let n_branches = network.branches.len();
    (0..n_hours)
        .map(|t| {
            (0..n_branches)
                .map(|j| sol.x[layout.branch_commitment_col(t, j)] > 0.5)
                .collect()
        })
        .collect()
}
