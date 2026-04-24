// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCUC objective assembly helpers.

use std::collections::HashSet;

use surge_network::Network;
use surge_network::market::{CostCurve, DispatchableLoad, PenaltyCurve, VirtualBidDirection};
use surge_network::network::StorageDispatchMode;

use super::layout::ScucLayout;
use super::plan::ExplicitContingencyObjectivePlan;
use super::rows::ScucStartupTierInfo;
use crate::common::blocks::DispatchBlock;
use crate::common::costs::uses_convex_polynomial_pwl;
use crate::common::reserves::{
    ReserveLpLayout, dispatchable_load_reserve_offer_for_period, generator_reserve_offer_for_period,
};
use crate::common::spec::DispatchProblemSpec;

type PwlSegments = Vec<(f64, f64)>;
type HourlyPwlSegments = Vec<Option<PwlSegments>>;

pub(super) struct ScucObjectiveCcPlant {
    pub plant_index: usize,
    pub z_block_off: usize,
    pub ytrans_block_off: usize,
    pub pgcc_block_off: usize,
    pub member_gen_j: HashSet<usize>,
    pub transition_costs: Vec<f64>,
    pub pgcc_entries: Vec<(usize, usize)>,
}

pub(super) struct ScucObjectiveInput<'a> {
    pub network: &'a Network,
    pub hourly_networks: &'a [Network],
    pub spec: &'a DispatchProblemSpec<'a>,
    pub layout: &'a ScucLayout,
    pub n_var: usize,
    pub n_hours: usize,
    pub n_gen: usize,
    pub n_bp: usize,
    pub n_branch_flow: usize,
    pub n_fg_rows: usize,
    pub n_iface_rows: usize,
    pub is_block_mode: bool,
    pub use_plc: bool,
    pub gen_indices: &'a [usize],
    pub gen_blocks: &'a [Vec<DispatchBlock>],
    pub gen_block_start: &'a [usize],
    pub gen_tier_info_by_hour: &'a [Vec<Vec<ScucStartupTierInfo>>],
    pub startup_tier_capacity: &'a [usize],
    pub delta_gen_off: &'a [usize],
    pub reserve_layout: &'a ReserveLpLayout,
    pub storage_gen_local: &'a [(usize, usize, usize)],
    pub n_sto_dis_epi: usize,
    pub n_sto_ch_epi: usize,
    pub hvdc_band_offsets: &'a [usize],
    pub pwl_gen_j: &'a [usize],
    pub pwl_gen_segments_by_hour: &'a [HourlyPwlSegments],
    pub dl_list: &'a [&'a DispatchableLoad],
    pub dl_orig_idx: &'a [usize],
    pub active_vbids: &'a [usize],
    pub effective_co2_price: f64,
    pub effective_co2_rate: &'a [f64],
    pub cc_var_base: usize,
    pub cc_plants: &'a [ScucObjectiveCcPlant],
    pub explicit_contingency: Option<&'a ExplicitContingencyObjectivePlan>,
    pub base: f64,
}

fn cc_z_idx(
    cc_var_base: usize,
    plant: &ScucObjectiveCcPlant,
    c: usize,
    t: usize,
    n_hours: usize,
) -> usize {
    cc_var_base + plant.z_block_off + c * n_hours + t
}

fn cc_ytrans_idx(
    cc_var_base: usize,
    plant: &ScucObjectiveCcPlant,
    tr_idx: usize,
    t: usize,
    n_hours: usize,
) -> usize {
    cc_var_base + plant.ytrans_block_off + tr_idx * n_hours + t
}

fn cc_pgcc_idx(
    cc_var_base: usize,
    plant: &ScucObjectiveCcPlant,
    entry_idx: usize,
    t: usize,
    n_hours: usize,
) -> usize {
    cc_var_base + plant.pgcc_block_off + entry_idx * n_hours + t
}

/// Convert a `$/MWh` rate (or any per-MWh rate) into the dollar cost per
/// unit of an LP column whose underlying physical quantity is `pu × hours`.
///
/// Surge LP power columns are in pu of `base_mva`, and one period covers
/// `dt_h` hours, so the contribution of one unit of the column to the
/// objective is `rate_per_mwh × base_mva × dt_h`. Use this helper for every
/// `$/MWh` cost (energy block, reserve procurement, slack penalty in
/// `$/pu·h`, virtual bid, HVDC band, storage variable/degradation, etc.).
///
/// All of these terms are formally `dt × c × variable`; dropping the
/// `dt` factor on the cost side biases dispatch on non-uniform-period
/// horizons.
#[inline]
fn pu_h_cost(rate_per_mwh: f64, base_mva: f64, dt_h: f64) -> f64 {
    rate_per_mwh * base_mva * dt_h
}

/// Convert a `$/h` fixed-while-online rate into the dollar cost per
/// period for a binary commitment column (no `base_mva` because the
/// column is dimensionless): `z^on = dt × c^on × u^on`.
#[inline]
fn h_cost(rate_per_hour: f64, dt_h: f64) -> f64 {
    rate_per_hour * dt_h
}

pub(super) fn build_objective(input: ScucObjectiveInput<'_>) -> Vec<f64> {
    let mut col_cost = vec![0.0; input.n_var];
    let cc_member_j: HashSet<usize> = input
        .cc_plants
        .iter()
        .flat_map(|ci| ci.member_gen_j.iter().copied())
        .collect();

    for t in 0..input.n_hours {
        let dt_h = input.spec.period_hours(t);
        for (j, &gi) in input.gen_indices.iter().enumerate() {
            let g = &input.network.generators[gi];
            let g_hourly = &input.hourly_networks[t].generators[gi];
            let is_storage = g.is_storage();
            let mut offer_cost_buf: Option<CostCurve> = None;
            let cost = crate::common::costs::resolve_cost_for_period_from_spec(
                gi,
                t,
                g,
                input.spec,
                &mut offer_cost_buf,
                Some(g_hourly.pmax),
            );

            let suppress_u_noload = cc_member_j.contains(&j) || is_storage;
            let suppress_pg_cost = cc_member_j.contains(&j) || is_storage;
            let sd = match cost {
                CostCurve::Polynomial { shutdown, .. }
                | CostCurve::PiecewiseLinear { shutdown, .. } => *shutdown,
            };

            if input.is_block_mode {
                let cost_at_pmin = match cost {
                    CostCurve::Polynomial { coeffs, .. } => {
                        let mut val = 0.0;
                        for c in coeffs {
                            val = val * g.pmin + c;
                        }
                        val
                    }
                    CostCurve::PiecewiseLinear { points, .. } => {
                        points.first().map(|&(_, c)| c).unwrap_or(0.0)
                    }
                };
                if !suppress_u_noload {
                    // Eq (61): z^on = dt × c^on × u^on. `cost_at_pmin` is a
                    // $/h fixed-while-online rate; scale by the period.
                    col_cost[input.layout.commitment_col(t, j)] += h_cost(cost_at_pmin, dt_h);
                }
                if !suppress_pg_cost {
                    for (i, block) in input.gen_blocks[j].iter().enumerate() {
                        let idx = input.layout.col(
                            t,
                            input.layout.dispatch.block + input.gen_block_start[j] + i,
                        );
                        // Eq (131): z^en = dt × Σ c^en × p_jtm.
                        col_cost[idx] = pu_h_cost(block.marginal_cost, input.base, dt_h);
                    }
                }
            } else {
                let uses_plc_cost = input.use_plc && uses_convex_polynomial_pwl(cost);
                match cost {
                    CostCurve::Polynomial { coeffs, .. } => match coeffs.len() {
                        0 => {}
                        1 => {
                            if !suppress_u_noload {
                                // Single coefficient = constant $/h fixed cost.
                                col_cost[input.layout.commitment_col(t, j)] +=
                                    h_cost(coeffs[0], dt_h);
                            }
                        }
                        2 => {
                            if !suppress_pg_cost {
                                // coeffs[0] is the linear ($/MWh) coefficient
                                // (surge polynomial convention: degree-ascending
                                // for the 2-coeff case skips the constant term).
                                col_cost[input.layout.pg_col(t, j)] =
                                    pu_h_cost(coeffs[0], input.base, dt_h);
                            }
                            if !suppress_u_noload {
                                col_cost[input.layout.commitment_col(t, j)] +=
                                    h_cost(coeffs[1], dt_h);
                            }
                        }
                        _ => {
                            if uses_plc_cost {
                                if !suppress_u_noload {
                                    col_cost[input.layout.commitment_col(t, j)] +=
                                        h_cost(coeffs[2], dt_h);
                                }
                                if !suppress_pg_cost {
                                    let pmin = g.pmin;
                                    let pmax = g.pmax;
                                    let a = coeffs[0];
                                    let b = coeffs[1];
                                    for k in 0..input.n_bp {
                                        let pk = if input.n_bp > 1 {
                                            pmin + k as f64 * (pmax - pmin)
                                                / (input.n_bp - 1) as f64
                                        } else {
                                            pmin
                                        };
                                        // cost_k is in $/h (quadratic poly
                                        // evaluated at MW); scale by period
                                        // hours. PLC λ columns are convex
                                        // multipliers in [0, 1] so no `× base`.
                                        let cost_k = a * pk * pk + b * pk;
                                        let lam_idx = input
                                            .layout
                                            .col(t, input.layout.plc_lambda + j * input.n_bp + k);
                                        col_cost[lam_idx] = h_cost(cost_k, dt_h);
                                    }
                                }
                            } else {
                                if !suppress_pg_cost {
                                    col_cost[input.layout.pg_col(t, j)] =
                                        pu_h_cost(coeffs[1], input.base, dt_h);
                                }
                                if !suppress_u_noload {
                                    col_cost[input.layout.commitment_col(t, j)] +=
                                        h_cost(coeffs[2], dt_h);
                                }
                            }
                        }
                    },
                    CostCurve::PiecewiseLinear { .. } => {}
                }
            }

            for (k, tier) in input.gen_tier_info_by_hour[j][t].iter().enumerate() {
                let idx = input
                    .layout
                    .col(t, input.layout.startup_delta + input.delta_gen_off[j] + k);
                col_cost[idx] = tier.cost;
            }
            for k in input.gen_tier_info_by_hour[j][t].len()..input.startup_tier_capacity[j] {
                let idx = input
                    .layout
                    .col(t, input.layout.startup_delta + input.delta_gen_off[j] + k);
                col_cost[idx] = 0.0;
            }
            col_cost[input.layout.shutdown_col(t, j)] += sd;
        }

        for ap in &input.reserve_layout.products {
            for (j, &gi) in input.gen_indices.iter().enumerate() {
                // Non-participants have no reserve col; nothing to cost.
                let Some(intra_period_col) = ap.gen_reserve_col(j) else {
                    continue;
                };
                let g = &input.network.generators[gi];
                let cost = generator_reserve_offer_for_period(input.spec, gi, g, &ap.product.id, t)
                    .map(|offer| offer.cost_per_mwh)
                    .unwrap_or(0.0);
                if cost > 0.0 {
                    // Eqs (90)-(97): z^[kind]_jt = dt × c × p^[kind]_jt.
                    col_cost[input.layout.col(t, intra_period_col)] =
                        pu_h_cost(cost, input.base, dt_h);
                }
            }
            // DL reserve cost is now group-level. All members of a
            // group share the same `cost_per_mwh` for a product, so
            // use the first member's offer as the group's rate.
            for (gi, group) in input.reserve_layout.dl_consumer_groups.iter().enumerate() {
                let Some(intra_period_col) = ap.dl_group_reserve_col(gi) else {
                    continue;
                };
                let cost = group
                    .member_dl_indices
                    .iter()
                    .find_map(|&k| {
                        let dl = input.dl_list[k];
                        dispatchable_load_reserve_offer_for_period(
                            input.spec,
                            input.dl_orig_idx.get(k).copied().unwrap_or(k),
                            dl,
                            &ap.product.id,
                            t,
                        )
                        .map(|offer| offer.cost_per_mwh)
                        .filter(|c| *c > 0.0)
                    })
                    .unwrap_or(0.0);
                if cost > 0.0 {
                    col_cost[input.layout.col(t, intra_period_col)] =
                        pu_h_cost(cost, input.base, dt_h);
                }
            }
            // Eqs (28)-(35): z^[kind]_n = dt × c^[kind]_n × shortfall.
            let penalty = pu_h_cost(
                ap.product.demand_curve.marginal_cost_at(0.0).max(0.0),
                input.base,
                dt_h,
            );
            col_cost[input.layout.col(t, ap.slack_offset)] = penalty;
            let default_zonal_penalty_per_mwh = match &ap.product.demand_curve {
                PenaltyCurve::PiecewiseLinear { segments } => segments
                    .last()
                    .map(|segment| segment.cost_per_unit)
                    .unwrap_or(0.0)
                    .max(0.0),
                _ => ap.product.demand_curve.marginal_cost_at(0.0).max(0.0),
            };
            for (zi, req) in ap.zonal_reqs.iter().enumerate() {
                let rate_per_mwh = req
                    .shortfall_cost_per_unit
                    .unwrap_or(default_zonal_penalty_per_mwh)
                    .max(0.0);
                col_cost[input.layout.col(t, ap.zonal_slack_offset + zi)] =
                    pu_h_cost(rate_per_mwh, input.base, dt_h);
            }
        }

        for &(s, _, gi) in input.storage_gen_local {
            let sto = input.network.generators[gi]
                .storage
                .as_ref()
                .expect("storage_gen_local only contains generators with storage");
            match sto.dispatch_mode {
                StorageDispatchMode::CostMinimization => {
                    // SCUC stores storage `ch`/`dis` columns in **MW** (see
                    // `common/builders.rs` unit-conventions doc — only SCED
                    // uses pu storage variables). The cost rate is $/MWh, so
                    // the per-period coefficient is `rate × dt_h`. Do NOT
                    // multiply by `base_mva` here — the variable is already
                    // physical MW, not normalized pu.
                    col_cost[input.layout.storage_discharge_col(t, s)] += h_cost(
                        sto.variable_cost_per_mwh + sto.degradation_cost_per_mwh,
                        dt_h,
                    );
                    col_cost[input.layout.storage_charge_col(t, s)] +=
                        h_cost(sto.degradation_cost_per_mwh, dt_h);
                }
                StorageDispatchMode::OfferCurve | StorageDispatchMode::SelfSchedule => {}
            }
        }
        for k in 0..input.n_sto_dis_epi {
            col_cost[input.layout.col(t, input.layout.dispatch.sto_epi_dis + k)] = 1.0;
        }
        for k in 0..input.n_sto_ch_epi {
            col_cost[input.layout.col(t, input.layout.dispatch.sto_epi_ch + k)] = 1.0;
        }

        for (k, hvdc) in input.spec.hvdc_links.iter().enumerate() {
            if hvdc.is_banded() {
                for (b, band) in hvdc.bands.iter().enumerate() {
                    if band.cost_per_mwh.abs() > 1e-12 {
                        col_cost[input.layout.col(
                            t,
                            input.layout.dispatch.hvdc + input.hvdc_band_offsets[k] + b,
                        )] = pu_h_cost(band.cost_per_mwh, input.base, dt_h);
                    }
                }
            } else if hvdc.cost_per_mwh > 0.0 {
                col_cost[input
                    .layout
                    .col(t, input.layout.dispatch.hvdc + input.hvdc_band_offsets[k])] =
                    pu_h_cost(hvdc.cost_per_mwh, input.base, dt_h);
            }
        }

        for (k, &j) in input.pwl_gen_j.iter().enumerate() {
            if input.pwl_gen_segments_by_hour[t][k].is_some() && !cc_member_j.contains(&j) {
                // Epigraph rows constrain `e_g` in $/h (slope_pu and intercept
                // carry $/h units; see `common/costs::pwl_curve_segments`).
                // Scale by `dt_h` so the LP objective contribution is $ for
                // the period, matching `pu_h_cost` convention elsewhere.
                col_cost[input.layout.col(t, input.layout.dispatch.e_g + k)] = dt_h;
            }
        }

        for (k, dl) in input.dl_list.iter().enumerate() {
            let (_, _, _, _, _, cost_model) = crate::common::costs::resolve_dl_for_period_from_spec(
                input.dl_orig_idx[k],
                t,
                dl,
                input.spec,
            );
            // Load value is integrated over the interval:
            //   z_value += price_pu × p_on_pu × dt_h
            // All cost models scale by dt_h so the dispatch optimum is
            // invariant to period decomposition.
            col_cost[input.layout.col(t, input.layout.dispatch.dl + k)] =
                cost_model.dc_linear_obj_coeff(input.base) * dt_h;
        }

        for (k, &bi) in input.active_vbids.iter().enumerate() {
            let vb = &input.spec.virtual_bids[bi];
            col_cost[input.layout.col(t, input.layout.dispatch.vbid + k)] = match vb.direction {
                VirtualBidDirection::Inc => pu_h_cost(vb.price_per_mwh, input.base, dt_h),
                VirtualBidDirection::Dec => -pu_h_cost(vb.price_per_mwh, input.base, dt_h),
            };
        }

        if input.effective_co2_price > 0.0 {
            for j in 0..input.n_gen {
                col_cost[input.layout.pg_col(t, j)] += pu_h_cost(
                    input.effective_co2_price * input.effective_co2_rate[j],
                    input.base,
                    dt_h,
                );
            }
        }

        if !input.spec.scuc_disable_bus_power_balance {
            if let Some(penalty) = input.spec.power_balance_penalty.curtailment.first() {
                let _ = penalty;
                for seg_idx in 0..input.spec.power_balance_penalty.curtailment.len() {
                    let seg_rate = input.spec.power_balance_penalty.curtailment[seg_idx].1;
                    col_cost[input.layout.pb_curtailment_seg_col(t, seg_idx)] =
                        pu_h_cost(seg_rate, input.base, dt_h);
                }
            }
            if let Some(penalty) = input.spec.power_balance_penalty.excess.first() {
                let _ = penalty;
                for seg_idx in 0..input.spec.power_balance_penalty.excess.len() {
                    let seg_rate = input.spec.power_balance_penalty.excess[seg_idx].1;
                    col_cost[input.layout.pb_excess_seg_col(t, seg_idx)] =
                        pu_h_cost(seg_rate, input.base, dt_h);
                }
            }
        }

        // Eqs (138)-(141), (158): branch overload penalty in $/(pu·h).
        let thermal_penalty = pu_h_cost(
            input.spec.thermal_penalty_curve.marginal_cost_at(0.0),
            input.base,
            dt_h,
        );
        for row_idx in 0..input.n_branch_flow {
            col_cost[input.layout.branch_lower_slack_col(t, row_idx)] = thermal_penalty;
            col_cost[input.layout.branch_upper_slack_col(t, row_idx)] = thermal_penalty;
        }
        for row_idx in 0..input.n_fg_rows {
            let is_explicit_ctg_flowgate = input
                .explicit_contingency
                .and_then(|plan| plan.flowgate_row_cases.get(row_idx))
                .copied()
                .flatten()
                .is_some();
            let slack_penalty = if is_explicit_ctg_flowgate {
                0.0
            } else {
                thermal_penalty
            };
            col_cost[input.layout.flowgate_lower_slack_col(t, row_idx)] = slack_penalty;
            col_cost[input.layout.flowgate_upper_slack_col(t, row_idx)] = slack_penalty;
        }
        for row_idx in 0..input.n_iface_rows {
            col_cost[input.layout.interface_lower_slack_col(t, row_idx)] = thermal_penalty;
            col_cost[input.layout.interface_upper_slack_col(t, row_idx)] = thermal_penalty;
        }

        // When `ramp_constraints_hard` is set, the ramp slack columns
        // are pinned to zero in `bounds.rs` and we skip pricing them
        // here so the LP cannot ever look at the slack penalty value
        // (which becomes meaningless under hard mode).
        let ramp_penalty = if input.spec.ramp_constraints_hard {
            0.0
        } else {
            pu_h_cost(
                input.spec.ramp_penalty_curve.marginal_cost_at(0.0),
                input.base,
                dt_h,
            )
        };
        for j in 0..input.n_gen {
            col_cost[input.layout.ramp_up_slack_col(t, j)] = ramp_penalty;
            col_cost[input.layout.ramp_down_slack_col(t, j)] = ramp_penalty;
        }

        // Angle difference slack penalty. The penalty cost is $/rad, so
        // no base_mva scaling is needed — the slack variables are in radians.
        let angle_penalty_per_rad = input.spec.angle_penalty_curve.marginal_cost_at(0.0) * dt_h;
        for row_idx in 0..input.layout.n_angle_diff_rows {
            col_cost[input.layout.angle_diff_lower_slack_col(t, row_idx)] = angle_penalty_per_rad;
            col_cost[input.layout.angle_diff_upper_slack_col(t, row_idx)] = angle_penalty_per_rad;
        }
    }

    if let Some(explicit_ctg) = input.explicit_contingency {
        for period in &explicit_ctg.periods {
            if period.case_indices.is_empty() {
                continue;
            }
            col_cost[period.worst_case_col] = 1.0;
            col_cost[period.avg_case_col] = 1.0;
        }
    }

    for plant in input.cc_plants {
        let cc_plant = &input.network.market_data.combined_cycle_plants[plant.plant_index];
        for (tr_idx, cost) in plant.transition_costs.iter().copied().enumerate() {
            // Combined-cycle config transitions are one-time $ events
            // analogous to startup costs (Group C, no dt scaling).
            if cost > 0.0 {
                for t in 0..input.n_hours {
                    col_cost[cc_ytrans_idx(input.cc_var_base, plant, tr_idx, t, input.n_hours)] +=
                        cost;
                }
            }
        }
        for (c, config) in cc_plant.configs.iter().enumerate() {
            // CC no-load cost is $/h fixed-while-in-config, eq (61)-style.
            if config.no_load_cost > 0.0 {
                for t in 0..input.n_hours {
                    let dt_h = input.spec.period_hours(t);
                    col_cost[cc_z_idx(input.cc_var_base, plant, c, t, input.n_hours)] +=
                        h_cost(config.no_load_cost, dt_h);
                }
            }
        }
    }

    for t in 0..input.n_hours {
        let dt_h = input.spec.period_hours(t);
        for plant in input.cc_plants {
            for (entry_idx, &(gen_j, config_c)) in plant.pgcc_entries.iter().enumerate() {
                let gi = input.gen_indices[gen_j];
                let config_cost = if let Some(config_offers) =
                    input.spec.cc_config_offers.get(plant.plant_index)
                    && let Some(schedule) = config_offers.get(config_c)
                    && let Some(Some(offer_curve)) = schedule.periods.get(t)
                {
                    let cost = crate::common::costs::offer_curve_to_cost_curve(offer_curve, None);
                    match &cost {
                        CostCurve::Polynomial { coeffs, .. } if coeffs.len() >= 2 => {
                            pu_h_cost(coeffs[0], input.base, dt_h)
                        }
                        CostCurve::Polynomial { coeffs, .. } if !coeffs.is_empty() => {
                            pu_h_cost(coeffs[0], input.base, dt_h)
                        }
                        _ => 0.0,
                    }
                } else {
                    let g = &input.network.generators[gi];
                    match g.cost.as_ref() {
                        Some(CostCurve::Polynomial { coeffs, .. }) if coeffs.len() >= 2 => {
                            pu_h_cost(coeffs[0], input.base, dt_h)
                        }
                        _ => 0.0,
                    }
                };
                col_cost[cc_pgcc_idx(input.cc_var_base, plant, entry_idx, t, input.n_hours)] +=
                    config_cost;
            }
        }
    }

    col_cost
}
