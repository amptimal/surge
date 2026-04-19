// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 consumer → dispatchable-load translation.
//!
//! Every GO C3 consumer has a `p_lb` / `p_ub` per-period envelope and
//! a per-period waterfall of piecewise value blocks (`cost[t] =
//! [[willingness_to_pay_$/pu, block_size_pu], ...]`). The canonical
//! market models each consumer as:
//!
//! * a fixed bus-load of `p_lb[t]` (handled in [`super::bus_profiles`]), and
//! * a dispatchable range `[p_lb[t], p_ub[t]]` decomposed into one or
//!   more [`DispatchableLoad`] blocks — each block is one tranche of
//!   the willingness-to-pay waterfall, paid at its block marginal
//!   value when served, curtailed at that same marginal value when
//!   not.
//!
//! Two paths diverge based on whether the consumer carries GO C3
//! reactive PQ linking:
//!
//! * **Non-linked path** (the common case): produce `N` dispatchable
//!   loads where `N` is the maximum block count across all periods,
//!   each with a `LinearCurtailment` cost model keyed to the block's
//!   willingness-to-pay. Shared `ramp_group` on all blocks so the
//!   SCUC row builder enforces the aggregate ramp across them.
//! * **Linked path**: a single dispatchable load covering the full
//!   flexible range, with a per-period [`LoadCostModel::PiecewiseLinear`]
//!   cost model and per-period `q_sched`/`q_min`/`q_max` overrides
//!   that honor the `(q_0, beta)` linear link.
//!
//! The reserve-offer assignment follows GO C3 §4.6 producer/consumer
//! asymmetries: consumers skip `nsyn` and `ramp_up_off` per eqs 107-108.

use std::collections::HashMap;

use surge_dispatch::ReserveOfferSchedule;
use surge_dispatch::request::{
    DispatchableLoadOfferSchedule, DispatchableLoadReserveOfferSchedule,
};
use surge_io::go_c3::types::GoC3DeviceType;
use surge_io::go_c3::{GoC3Context, GoC3Device, GoC3DeviceTimeSeries, GoC3Problem};
use surge_network::market::{
    DispatchableLoad, DlOfferSchedule, DlPeriodParams, LoadArchetype, LoadCostModel, ReserveOffer,
};
use surge_network::network::generator::PqLinearLink;

use super::reserves::catalog as reserve_catalog;

const DL_BLOCK_TIEBREAK_EPSILON: f64 = 0.0;

/// Result of a single consumer's dispatchable-load translation.
pub(super) struct ConsumerPieces {
    pub loads: Vec<DispatchableLoad>,
    pub offer_schedules: Vec<DispatchableLoadOfferSchedule>,
    pub reserve_offer_schedules: Vec<DispatchableLoadReserveOfferSchedule>,
}

impl ConsumerPieces {
    fn new() -> Self {
        Self {
            loads: Vec::new(),
            offer_schedules: Vec::new(),
            reserve_offer_schedules: Vec::new(),
        }
    }
}

/// Build all consumer dispatchable-load pieces for the request.
pub(super) fn build_consumer_pieces(
    problem: &GoC3Problem,
    context: &mut GoC3Context,
    device_ts_by_uid: &HashMap<&str, &GoC3DeviceTimeSeries>,
) -> ConsumerPieces {
    let mut out = ConsumerPieces::new();
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Consumer {
            continue;
        }
        let Some(ts) = device_ts_by_uid.get(device.uid.as_str()) else {
            continue;
        };
        let Some(&bus_number) = context.bus_uid_to_number.get(&device.bus) else {
            continue;
        };

        let pieces = if consumer_has_pq_linking(device) {
            build_linked_consumer(problem, context, device, ts, bus_number)
        } else {
            build_block_consumer(problem, context, device, ts, bus_number)
        };
        out.loads.extend(pieces.loads);
        out.offer_schedules.extend(pieces.offer_schedules);
        out.reserve_offer_schedules
            .extend(pieces.reserve_offer_schedules);
    }
    out
}

fn consumer_has_pq_linking(device: &GoC3Device) -> bool {
    (device.q_linear_cap as i32) == 1 || (device.q_bound_cap as i32) == 1
}

/// Normalize `cost[period]` into `(willingness_to_pay_$/MWh, block_size_pu)`
/// sorted descending by cost, matching the GO C3 validator's
/// highest-value-first convention.
fn consumer_cost_blocks(
    device_ts: &GoC3DeviceTimeSeries,
    period_idx: usize,
    base_mva: f64,
) -> Vec<(f64, f64)> {
    let Some(blocks) = device_ts.cost.get(period_idx) else {
        return Vec::new();
    };
    let mut out: Vec<(f64, f64)> = blocks
        .iter()
        .filter(|b| b.len() == 2)
        .filter_map(|b| {
            let marginal = (b[0] / base_mva - DL_BLOCK_TIEBREAK_EPSILON).max(0.0);
            let size_pu = b[1];
            if size_pu <= 1e-12 {
                None
            } else {
                Some((marginal, size_pu))
            }
        })
        .collect();
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Non-PQ-linked consumer path: decompose the flex range into N cost
/// blocks (N = max across periods), emit N DispatchableLoads, one
/// per block, sharing the same ramp group.
fn build_block_consumer(
    problem: &GoC3Problem,
    context: &mut GoC3Context,
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
    bus_number: u32,
) -> ConsumerPieces {
    let uid = device.uid.clone();
    let periods = problem.time_series_input.general.time_periods;
    let base_mva = problem.network.general.base_norm_mva;

    let p_lb: Vec<f64> = resize_series(&device_ts.p_lb, periods, 0.0);
    let p_ub: Vec<f64> = resize_series(&device_ts.p_ub, periods, 0.0);
    let q_lb: Vec<f64> = resize_series(&device_ts.q_lb, periods, 0.0);
    let q_ub: Vec<f64> = resize_series(&device_ts.q_ub, periods, 0.0);

    let flex_by_period: Vec<f64> = p_lb
        .iter()
        .zip(p_ub.iter())
        .map(|(lb, ub)| (ub - lb).max(0.0))
        .collect();

    let mut block_specs_by_period: Vec<Vec<(f64, f64)>> = Vec::with_capacity(periods);
    let mut max_blocks: usize = 0;
    for period_idx in 0..periods {
        let flex_total = flex_by_period[period_idx];
        let raw_blocks = consumer_cost_blocks(device_ts, period_idx, base_mva);
        let p_lb_period = p_lb[period_idx].max(0.0);

        let mut blocks: Vec<(f64, f64)> = Vec::new();
        if flex_total > 1e-12 && !raw_blocks.is_empty() {
            let mut skip_remaining = p_lb_period;
            let mut flex_remaining = flex_total;
            for (cost_per_mwh, block_size_pu) in &raw_blocks {
                if flex_remaining <= 1e-12 {
                    break;
                }
                let mut size_pu = *block_size_pu;
                if skip_remaining > 1e-12 {
                    let skip_here = size_pu.min(skip_remaining);
                    size_pu -= skip_here;
                    skip_remaining -= skip_here;
                }
                if size_pu <= 1e-12 {
                    continue;
                }
                let usable = size_pu.min(flex_remaining);
                if usable > 1e-12 {
                    blocks.push((*cost_per_mwh, usable));
                    flex_remaining -= usable;
                }
            }
            if flex_remaining > 1e-9 {
                blocks.push((0.0, flex_remaining));
            }
            if blocks.is_empty() {
                blocks = vec![(0.0, flex_total)];
            }
        } else if flex_total > 1e-12 {
            blocks = vec![(0.0, flex_total)];
        }
        max_blocks = max_blocks.max(blocks.len());
        block_specs_by_period.push(blocks);
    }

    // Context mutation: record resource IDs so solution export can
    // pair block outputs back with the originating consumer.
    let resource_ids: Vec<String> = (0..max_blocks)
        .map(|block_idx| format!("{uid}::blk:{block_idx:02}"))
        .collect();
    context
        .consumer_dispatchable_resource_ids_by_uid
        .insert(uid.clone(), resource_ids.clone());

    // Block max-size sum drives reserve share allocation across blocks.
    let mut block_max_size_pu = vec![0.0; max_blocks];
    for period_blocks in &block_specs_by_period {
        for (block_idx, (_, block_size_pu)) in period_blocks.iter().enumerate() {
            if block_idx < max_blocks && *block_size_pu > block_max_size_pu[block_idx] {
                block_max_size_pu[block_idx] = *block_size_pu;
            }
        }
    }
    let total_block_capacity_pu: f64 = block_max_size_pu.iter().sum();

    // Reserve offer allocation: one entry per active product per
    // block (skipping `nsyn`, `ramp_up_off`, and reactive).
    let reserve_offers_by_block = build_block_reserve_offers(
        problem,
        context,
        device,
        device_ts,
        &block_max_size_pu,
        total_block_capacity_pu,
    );

    // Ramp group = consumer UID when any ramp rate is set.
    let ramp_up_pu_per_hr = option_nonzero(device.p_ramp_up_ub);
    let ramp_down_pu_per_hr = option_nonzero(device.p_ramp_down_ub);
    let initial_served_pu = device.initial_status.p;
    let ramp_group_opt = if ramp_up_pu_per_hr.is_some() || ramp_down_pu_per_hr.is_some() {
        Some(uid.clone())
    } else {
        None
    };
    let initial_q_total_pu = device.initial_status.q;

    let mut pieces = ConsumerPieces::new();
    for (block_idx, resource_id) in resource_ids.iter().enumerate() {
        let (initial_cost_per_mwh, initial_size_pu) = block_specs_by_period
            .first()
            .and_then(|period_blocks| period_blocks.get(block_idx))
            .copied()
            .unwrap_or((0.0, 0.0));
        let (initial_q_sched_pu, initial_q_min_pu, initial_q_max_pu) = block_q_params(
            0,
            initial_size_pu,
            &flex_by_period,
            &q_lb,
            &q_ub,
            initial_q_total_pu,
        );

        let mut load = DispatchableLoad::curtailable(
            bus_number,
            // p_sched in MW, p_min in MW for the `curtailable` ctor.
            // We immediately override the fields to match the block model.
            0.0, 0.0, 0.0, 0.0, base_mva,
        );
        load.resource_id = resource_id.clone();
        load.bus = bus_number;
        load.p_sched_pu = initial_size_pu;
        load.q_sched_pu = initial_q_sched_pu;
        load.p_min_pu = 0.0;
        load.p_max_pu = initial_size_pu;
        load.q_min_pu = initial_q_min_pu;
        load.q_max_pu = initial_q_max_pu;
        load.archetype = LoadArchetype::IndependentPQ;
        load.cost_model = LoadCostModel::LinearCurtailment {
            cost_per_mw: initial_cost_per_mwh,
        };
        load.fixed_power_factor = false;
        load.in_service = true;
        load.reserve_group = Some(uid.clone());
        if let Some(r) = ramp_group_opt.clone() {
            load.ramp_group = Some(r);
            load.initial_p_pu = Some(initial_served_pu);
        }
        if let Some(r) = ramp_up_pu_per_hr {
            load.ramp_up_pu_per_hr = Some(r);
        }
        if let Some(r) = ramp_down_pu_per_hr {
            load.ramp_down_pu_per_hr = Some(r);
        }
        if !reserve_offers_by_block[block_idx].is_empty() {
            load.reserve_offers = reserve_offers_by_block[block_idx].to_vec();
        }
        pieces.loads.push(load);

        // Per-period schedule overrides.
        let schedule_periods: Vec<Option<DlPeriodParams>> = (0..periods)
            .map(|period_idx| {
                if block_idx < block_specs_by_period[period_idx].len() {
                    let (cost_per_mwh, block_size_pu) =
                        block_specs_by_period[period_idx][block_idx];
                    let (q_sched_pu, q_min_pu, q_max_pu) = block_q_params(
                        period_idx,
                        block_size_pu,
                        &flex_by_period,
                        &q_lb,
                        &q_ub,
                        initial_q_total_pu,
                    );
                    Some(DlPeriodParams {
                        p_sched_pu: block_size_pu,
                        p_max_pu: block_size_pu,
                        q_sched_pu: Some(q_sched_pu),
                        q_min_pu: Some(q_min_pu),
                        q_max_pu: Some(q_max_pu),
                        pq_linear_equality: None,
                        pq_linear_upper: None,
                        pq_linear_lower: None,
                        cost_model: LoadCostModel::LinearCurtailment {
                            cost_per_mw: cost_per_mwh,
                        },
                    })
                } else {
                    Some(DlPeriodParams {
                        p_sched_pu: 0.0,
                        p_max_pu: 0.0,
                        q_sched_pu: Some(0.0),
                        q_min_pu: Some(0.0),
                        q_max_pu: Some(0.0),
                        pq_linear_equality: None,
                        pq_linear_upper: None,
                        pq_linear_lower: None,
                        cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 0.0 },
                    })
                }
            })
            .collect();
        pieces.offer_schedules.push(DispatchableLoadOfferSchedule {
            resource_id: resource_id.clone(),
            schedule: DlOfferSchedule {
                periods: schedule_periods,
            },
        });

        // Per-block reserve offer schedule (per-period, shares same
        // capacity as base).
        let reserve_offer_schedule_periods: Vec<Vec<ReserveOffer>> = (0..periods)
            .map(|period_idx| {
                build_block_reserve_offers_for_period(
                    problem,
                    context,
                    device,
                    device_ts,
                    &block_max_size_pu,
                    total_block_capacity_pu,
                    block_idx,
                    period_idx,
                )
            })
            .collect();
        if reserve_offer_schedule_periods.iter().any(|p| !p.is_empty()) {
            pieces
                .reserve_offer_schedules
                .push(DispatchableLoadReserveOfferSchedule {
                    resource_id: resource_id.clone(),
                    schedule: ReserveOfferSchedule {
                        periods: reserve_offer_schedule_periods,
                    },
                });
        }
    }
    pieces
}

fn block_q_params(
    period_idx: usize,
    block_size_pu: f64,
    flex_by_period: &[f64],
    q_lb: &[f64],
    q_ub: &[f64],
    initial_q_total_pu: f64,
) -> (f64, f64, f64) {
    let flex_total = flex_by_period[period_idx];
    let share = if flex_total > 1e-12 {
        block_size_pu / flex_total
    } else {
        0.0
    };
    let q_min_pu = q_lb.get(period_idx).copied().unwrap_or(0.0) * share;
    let q_max_pu = q_ub.get(period_idx).copied().unwrap_or(0.0) * share;
    let q_sched_pu = clamp(initial_q_total_pu * share, q_min_pu, q_max_pu);
    (q_sched_pu, q_min_pu, q_max_pu)
}

fn clamp(value: f64, lo: f64, hi: f64) -> f64 {
    let (lo, hi) = if lo > hi { (hi, lo) } else { (lo, hi) };
    value.max(lo).min(hi)
}

fn option_nonzero(value: f64) -> Option<f64> {
    if value.abs() > 0.0 { Some(value) } else { None }
}

fn resize_series(src: &[f64], periods: usize, fill: f64) -> Vec<f64> {
    let mut out = src.to_vec();
    if out.len() < periods {
        out.resize(periods, fill);
    }
    out
}

/// Per-block reserve offer catalog (base entries — period 0 copy used
/// as the block's base `reserve_offers` list).
fn build_block_reserve_offers(
    problem: &GoC3Problem,
    context: &GoC3Context,
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
    block_max_size_pu: &[f64],
    total_block_capacity_pu: f64,
) -> Vec<Vec<ReserveOffer>> {
    let active_ids: std::collections::HashSet<&str> = context
        .reserve_product_ids
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut out: Vec<Vec<ReserveOffer>> = vec![Vec::new(); block_max_size_pu.len()];
    if active_ids.is_empty() || total_block_capacity_pu <= 1e-12 {
        return out;
    }
    let base_mva = problem.network.general.base_norm_mva;
    for entry in reserve_catalog() {
        let pid = entry.canonical.id.as_str();
        if !active_ids.contains(pid) {
            continue;
        }
        if pid == "nsyn" || pid == "ramp_up_off" {
            continue;
        }
        if matches!(
            entry.canonical.kind,
            surge_network::market::ReserveKind::Reactive
                | surge_network::market::ReserveKind::ReactiveHeadroom
        ) {
            continue;
        }
        let cap_pu = (entry.device_cap_getter)(device);
        if cap_pu <= 1e-12 {
            continue;
        }
        let total_capacity_mw = cap_pu * base_mva;
        let cost_series = (entry.device_cost_ts_getter)(device_ts);
        let initial_cost = cost_series.first().copied().unwrap_or(0.0) / base_mva.max(1.0);
        for (block_idx, &block_capacity_pu) in block_max_size_pu.iter().enumerate() {
            if block_capacity_pu <= 1e-12 {
                continue;
            }
            let share = block_capacity_pu / total_block_capacity_pu;
            out[block_idx].push(ReserveOffer {
                product_id: entry.canonical.id.clone(),
                capacity_mw: total_capacity_mw * share,
                cost_per_mwh: initial_cost,
            });
        }
    }
    out
}

fn build_block_reserve_offers_for_period(
    problem: &GoC3Problem,
    context: &GoC3Context,
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
    block_max_size_pu: &[f64],
    total_block_capacity_pu: f64,
    block_idx: usize,
    period_idx: usize,
) -> Vec<ReserveOffer> {
    let active_ids: std::collections::HashSet<&str> = context
        .reserve_product_ids
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut offers: Vec<ReserveOffer> = Vec::new();
    if active_ids.is_empty() || total_block_capacity_pu <= 1e-12 {
        return offers;
    }
    let base_mva = problem.network.general.base_norm_mva;
    let block_capacity_pu = block_max_size_pu.get(block_idx).copied().unwrap_or(0.0);
    if block_capacity_pu <= 1e-12 {
        return offers;
    }
    let share = block_capacity_pu / total_block_capacity_pu;
    for entry in reserve_catalog() {
        let pid = entry.canonical.id.as_str();
        if !active_ids.contains(pid) {
            continue;
        }
        if pid == "nsyn" || pid == "ramp_up_off" {
            continue;
        }
        if matches!(
            entry.canonical.kind,
            surge_network::market::ReserveKind::Reactive
                | surge_network::market::ReserveKind::ReactiveHeadroom
        ) {
            continue;
        }
        let cap_pu = (entry.device_cap_getter)(device);
        if cap_pu <= 1e-12 {
            continue;
        }
        let total_capacity_mw = cap_pu * base_mva;
        let cost_series = (entry.device_cost_ts_getter)(device_ts);
        let cost_pu = cost_series
            .get(period_idx)
            .copied()
            .or_else(|| cost_series.last().copied())
            .unwrap_or(0.0);
        offers.push(ReserveOffer {
            product_id: entry.canonical.id.clone(),
            capacity_mw: total_capacity_mw * share,
            cost_per_mwh: cost_pu / base_mva.max(1.0),
        });
    }
    offers
}

/// PQ-linked consumer path: one dispatchable load covering the full
/// variable range, with a per-period `PiecewiseLinear` cost model.
fn build_linked_consumer(
    problem: &GoC3Problem,
    context: &mut GoC3Context,
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
    bus_number: u32,
) -> ConsumerPieces {
    let uid = device.uid.clone();
    let periods = problem.time_series_input.general.time_periods;
    let base_mva = problem.network.general.base_norm_mva;

    let floor_series = resize_series(&device_ts.p_lb, periods, 0.0);
    let ceiling_series = resize_series(&device_ts.p_ub, periods, 0.0);
    let q_floor_series = resize_series(&device_ts.q_lb, periods, 0.0);
    let q_ceiling_series = resize_series(&device_ts.q_ub, periods, 0.0);
    let base_floor_pu = floor_series.iter().copied().fold(f64::INFINITY, f64::min);
    let base_floor_pu = if base_floor_pu.is_finite() {
        base_floor_pu
    } else {
        0.0
    };
    let fixed_bus_series_pu: Vec<f64> = floor_series
        .iter()
        .map(|&f| (f - base_floor_pu).max(0.0))
        .collect();
    let variable_sched_series_pu: Vec<f64> = ceiling_series
        .iter()
        .zip(fixed_bus_series_pu.iter())
        .map(|(ceil, fixed)| (*ceil - *fixed).max(base_floor_pu))
        .collect();
    let max_variable_sched_pu = variable_sched_series_pu
        .iter()
        .copied()
        .fold(base_floor_pu, f64::max);
    let resource_id = format!("{uid}::blk:00");
    context
        .consumer_dispatchable_resource_ids_by_uid
        .insert(uid.clone(), vec![resource_id.clone()]);

    let ramp_up = option_nonzero(device.p_ramp_up_ub);
    let ramp_down = option_nonzero(device.p_ramp_down_ub);
    let initial_served = device.initial_status.p;
    let ramp_group_opt = if ramp_up.is_some() || ramp_down.is_some() {
        Some(uid.clone())
    } else {
        None
    };
    let initial_q_total_pu = device.initial_status.q;

    // Per-period reserve offer allocation (full-range; no sharing).
    let reserve_offers_base: Vec<ReserveOffer> = build_linked_reserve_offers_period(
        problem,
        context,
        device,
        device_ts,
        0,
        max_variable_sched_pu,
    );
    let reserve_offer_schedule_periods: Vec<Vec<ReserveOffer>> = (0..periods)
        .map(|period_idx| {
            build_linked_reserve_offers_period(
                problem,
                context,
                device,
                device_ts,
                period_idx,
                max_variable_sched_pu,
            )
        })
        .collect();

    // Period 0 baseline values.
    let initial_period = build_linked_period(
        0,
        device,
        device_ts,
        &fixed_bus_series_pu,
        &variable_sched_series_pu,
        &q_floor_series,
        &q_ceiling_series,
        initial_q_total_pu,
        base_mva,
    );

    let mut load = DispatchableLoad::curtailable(bus_number, 0.0, 0.0, 0.0, 0.0, base_mva);
    load.resource_id = resource_id.clone();
    load.bus = bus_number;
    load.p_sched_pu = initial_period.p_sched_pu;
    load.q_sched_pu = initial_period.q_sched_pu.unwrap_or(0.0);
    load.p_min_pu = base_floor_pu;
    load.p_max_pu = initial_period.p_max_pu;
    load.q_min_pu = initial_period.q_min_pu.unwrap_or(0.0);
    load.q_max_pu = initial_period.q_max_pu.unwrap_or(0.0);
    load.archetype = LoadArchetype::IndependentPQ;
    load.cost_model = initial_period.cost_model.clone();
    load.fixed_power_factor = false;
    load.in_service = true;
    load.reserve_group = Some(uid.clone());
    if let Some(r) = ramp_group_opt.clone() {
        load.ramp_group = Some(r);
        load.initial_p_pu = Some(initial_served);
    }
    if let Some(r) = ramp_up {
        load.ramp_up_pu_per_hr = Some(r);
    }
    if let Some(r) = ramp_down {
        load.ramp_down_pu_per_hr = Some(r);
    }
    // Apply PQ-link payloads onto base load.
    apply_pq_link_payloads(&mut load, device, fixed_bus_series_pu[0]);

    if !reserve_offers_base.is_empty() {
        load.reserve_offers = reserve_offers_base.clone();
    }

    let schedule_periods: Vec<Option<DlPeriodParams>> = (0..periods)
        .map(|period_idx| {
            Some(build_linked_period(
                period_idx,
                device,
                device_ts,
                &fixed_bus_series_pu,
                &variable_sched_series_pu,
                &q_floor_series,
                &q_ceiling_series,
                initial_q_total_pu,
                base_mva,
            ))
        })
        .collect();

    let mut pieces = ConsumerPieces::new();
    pieces.loads.push(load);
    pieces.offer_schedules.push(DispatchableLoadOfferSchedule {
        resource_id: resource_id.clone(),
        schedule: DlOfferSchedule {
            periods: schedule_periods,
        },
    });
    if reserve_offer_schedule_periods.iter().any(|p| !p.is_empty()) {
        pieces
            .reserve_offer_schedules
            .push(DispatchableLoadReserveOfferSchedule {
                resource_id,
                schedule: ReserveOfferSchedule {
                    periods: reserve_offer_schedule_periods,
                },
            });
    }
    pieces
}

#[allow(clippy::too_many_arguments)]
fn build_linked_period(
    period_idx: usize,
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
    fixed_bus_series_pu: &[f64],
    variable_sched_series_pu: &[f64],
    q_floor_series: &[f64],
    q_ceiling_series: &[f64],
    initial_q_total_pu: f64,
    base_mva: f64,
) -> DlPeriodParams {
    let fixed_bus_pu = fixed_bus_series_pu[period_idx];
    let var_sched_pu = variable_sched_series_pu[period_idx];
    let q_lo = q_floor_series[period_idx];
    let q_hi = q_ceiling_series[period_idx];
    let q_sched_pu = clamp(initial_q_total_pu, q_lo, q_hi);
    let raw_blocks = consumer_cost_blocks(device_ts, period_idx, base_mva);
    let cost_model = if raw_blocks.is_empty() {
        LoadCostModel::LinearCurtailment { cost_per_mw: 0.0 }
    } else {
        LoadCostModel::PiecewiseLinear {
            points: go_cost_blocks_to_piecewise_linear_points(
                &raw_blocks,
                fixed_bus_pu,
                var_sched_pu,
                base_mva,
            ),
        }
    };
    let (pq_equality, pq_upper, pq_lower) = compute_pq_links(device, fixed_bus_pu);
    DlPeriodParams {
        p_sched_pu: var_sched_pu,
        p_max_pu: var_sched_pu,
        q_sched_pu: Some(q_sched_pu),
        q_min_pu: Some(q_lo),
        q_max_pu: Some(q_hi),
        pq_linear_equality: pq_equality,
        pq_linear_upper: pq_upper,
        pq_linear_lower: pq_lower,
        cost_model,
    }
}

fn build_linked_reserve_offers_period(
    problem: &GoC3Problem,
    context: &GoC3Context,
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
    period_idx: usize,
    max_variable_sched_pu: f64,
) -> Vec<ReserveOffer> {
    if max_variable_sched_pu <= 1e-12 {
        return Vec::new();
    }
    let base_mva = problem.network.general.base_norm_mva;
    let active_ids: std::collections::HashSet<&str> = context
        .reserve_product_ids
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut offers = Vec::new();
    for entry in reserve_catalog() {
        let pid = entry.canonical.id.as_str();
        if !active_ids.contains(pid) {
            continue;
        }
        if pid == "nsyn" || pid == "ramp_up_off" {
            continue;
        }
        if matches!(
            entry.canonical.kind,
            surge_network::market::ReserveKind::Reactive
                | surge_network::market::ReserveKind::ReactiveHeadroom
        ) {
            continue;
        }
        let cap_pu = (entry.device_cap_getter)(device);
        if cap_pu <= 1e-12 {
            continue;
        }
        let capacity_mw = cap_pu * base_mva;
        let cost_series = (entry.device_cost_ts_getter)(device_ts);
        let cost_pu = cost_series
            .get(period_idx)
            .copied()
            .or_else(|| cost_series.last().copied())
            .unwrap_or(0.0);
        offers.push(ReserveOffer {
            product_id: entry.canonical.id.clone(),
            capacity_mw,
            cost_per_mwh: cost_pu / base_mva.max(1.0),
        });
    }
    offers
}

fn compute_pq_links(
    device: &GoC3Device,
    _p_shift_pu: f64,
) -> (
    Option<PqLinearLink>,
    Option<PqLinearLink>,
    Option<PqLinearLink>,
) {
    // GO C3 PQ-linking coefficients (`q_0`, `beta`, `q_0_ub`, etc.)
    // are not yet exposed on the `surge_io::go_c3::GoC3Device` struct.
    // All public 73-bus / 617-bus scenarios we've inspected have
    // `q_linear_cap = 0` and `q_bound_cap = 0`, so this path is
    // currently dormant. When larger scenarios with PQ linking come
    // online, extend `GoC3Device` with the coefficient fields and
    // populate the link structures here.
    let _ = device;
    (None, None, None)
}

fn apply_pq_link_payloads(load: &mut DispatchableLoad, device: &GoC3Device, p_shift_pu: f64) {
    let (eq, up, lo) = compute_pq_links(device, p_shift_pu);
    load.pq_linear_equality = eq;
    load.pq_linear_upper = up;
    load.pq_linear_lower = lo;
}

/// Convert GO C3 consumer cost blocks into the (P_MW, marginal_utility_$/MWh)
/// point list the `PiecewiseLinear` cost model expects.
fn go_cost_blocks_to_piecewise_linear_points(
    raw_blocks: &[(f64, f64)],
    p_skip_pu: f64,
    required_span_pu: f64,
    base_mva: f64,
) -> Vec<(f64, f64)> {
    let mut skip_remaining = p_skip_pu.max(0.0);
    let mut span_remaining = required_span_pu.max(0.0);
    let mut cursor_pu = 0.0;
    let mut last_mu: Option<f64> = None;
    let mut points: Vec<(f64, f64)> = Vec::new();

    for (marginal_utility, block_size_pu) in raw_blocks {
        if span_remaining <= 1e-12 {
            break;
        }
        let mut usable = *block_size_pu;
        if skip_remaining > 1e-12 {
            let skipped_here = usable.min(skip_remaining);
            usable -= skipped_here;
            skip_remaining -= skipped_here;
        }
        if usable <= 1e-12 {
            continue;
        }
        let span_here = usable.min(span_remaining);
        if points.is_empty() {
            points.push((0.0, *marginal_utility));
        } else if let Some(prev) = last_mu {
            if (marginal_utility - prev).abs() > 1e-12 {
                points.push((cursor_pu * base_mva, *marginal_utility));
            }
        }
        cursor_pu += span_here;
        points.push((cursor_pu * base_mva, *marginal_utility));
        last_mu = Some(*marginal_utility);
        span_remaining -= span_here;
    }

    if points.is_empty() {
        let terminal_mw = required_span_pu.max(0.0) * base_mva;
        return vec![(0.0, 0.0), (terminal_mw, 0.0)];
    }

    if span_remaining > 1e-12 {
        cursor_pu += span_remaining;
        points.push((cursor_pu * base_mva, last_mu.unwrap_or(0.0)));
    } else if points.len() == 1 {
        points.push((cursor_pu * base_mva, last_mu.unwrap_or(0.0)));
    }
    points
}
