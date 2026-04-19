// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 reserve-product catalog and field
//! accessors.
//!
//! This module is the **format-specific** half of reserve construction
//! for GO C3. It pairs the canonical reserve-product shapes (defined
//! in [`crate::reserves`]) with the GO-C3-specific glue that reads
//! zonal violation costs, fraction-based requirement coefficients,
//! time-series requirements, and per-device capability fields from
//! the GO C3 problem data structures.
//!
//! The catalog covers the nine products GO C3 specifies:
//!
//! * `reg_up`, `reg_down` — regulation (Committed; load-fraction sized)
//! * `syn`, `nsyn` — synchronous / non-synchronous (largest-unit sized)
//! * `ramp_up_on`, `ramp_up_off`, `ramp_down_on`, `ramp_down_off` — ramping
//!   reserves (exogenous time series sized)
//! * `q_res_up`, `q_res_down` — reactive reserves (paid via synthetic
//!   q_headroom product in the canonical SCUC kernel)

use std::collections::{HashMap, HashSet};

use surge_dispatch::{GeneratorReserveOfferSchedule, ReserveOfferSchedule};
use surge_io::go_c3::types::{
    GoC3ActiveReserveTimeSeries, GoC3ActiveZonalReserve, GoC3DeviceType, GoC3ReactiveZonalReserve,
};
use surge_io::go_c3::{GoC3Context, GoC3Device, GoC3DeviceTimeSeries, GoC3Problem};
use surge_network::market::{ReserveKind, ReserveOffer, ReserveProduct, ZonalReserveRequirement};

use crate::reserves::{
    ReserveProductSpec, reactive_headroom_product, reserve_product_from_catalog,
    zonal_requirement_from_largest_unit, zonal_requirement_from_load_fraction,
    zonal_requirement_from_series,
};

/// Pairing of a canonical reserve product spec with GO-C3-specific
/// glue for looking up violation costs, requirement fractions, and
/// per-device capability.
pub(super) struct GoC3ReserveEntry {
    pub canonical: ReserveProductSpec,
    /// Field name on [`GoC3ActiveZonalReserve`] or
    /// [`GoC3ReactiveZonalReserve`] that holds the zone's $/pu-h
    /// shortfall cost for this product.
    pub go_vio_cost_key: &'static str,
    /// Optional field name on [`GoC3ActiveZonalReserve`] that holds
    /// the zone's fraction coefficient (e.g. `REG_UP`, `SYN`). If
    /// `Some`, the zonal requirement uses the fraction rule.
    pub go_fraction_key: Option<&'static str>,
    /// Optional time-series key on [`GoC3ActiveReserveTimeSeries`] /
    /// [`GoC3ReactiveReserveTimeSeries`] that holds the zone's
    /// exogenous per-period requirement (pu). If `Some`, the zonal
    /// requirement uses the time-series rule.
    pub go_ts_key: Option<&'static str>,
    /// Field on [`GoC3Device`] that holds the producer-side capability
    /// cap in pu. Return 0 to suppress offers for this product.
    pub device_cap_getter: fn(&GoC3Device) -> f64,
    /// Field on [`GoC3DeviceTimeSeries`] that holds the per-period
    /// offer cost time series in `$/pu-h`.
    pub device_cost_ts_getter: fn(&GoC3DeviceTimeSeries) -> &[f64],
    /// True when this product should be skipped when iterating over
    /// producer reserve offer schedules (e.g. reactive products, or
    /// the offline sibling of a ramping-down pair).
    pub skip_for_producers: bool,
}

/// The canonical GO C3 reserve catalog.
///
/// The entries are populated from the standard canonical spec
/// shorthands plus GO-C3-specific field accessors.
///
/// GO C3 uses a *cascaded* reserve-shortfall model (C3DataUtilities
/// `evaluation.eval_prz_t_z_{rgu,scr,nsc}`): the residual unmet
/// requirement from REG_UP carries into the SCR balance, and then into
/// the NSC balance. Only direct SCR awards reduce SCR demand — REG_UP
/// awards only help SCR if they over-provision REG_UP first (producing
/// a negative carry-over). The generic catalog's `balance_products`
/// substitution mechanism ("sum across listed products meets demand")
/// doesn't model the cascade faithfully: the LP sees `SYN + REG_UP`
/// awards as directly interchangeable at any level, so it allocates
/// just enough REG_UP to meet REG_UP demand plus "enough SYN" to
/// substitute into SCR — validator then scores a shortfall because
/// from its perspective, REG_UP was just-met (no leftover) and SCR
/// awards alone fall short. Strip `balance_products` for SYN and NSYN
/// to force the LP to cover their demand directly, matching the
/// validator's accounting. RAMPING_RESERVE_* products keep their
/// balance_products because the validator's RRU/RRD evaluators add
/// on/off awards together (not a cascade — see
/// `evaluation.eval_prz_t_z_{rru,rrd}`), so the substitution pattern
/// matches.
fn goc3_cascade_synchronized() -> ReserveProductSpec {
    let mut spec = ReserveProductSpec::synchronized();
    spec.balance_products.clear();
    spec
}

fn goc3_cascade_non_synchronized() -> ReserveProductSpec {
    let mut spec = ReserveProductSpec::non_synchronized();
    spec.balance_products.clear();
    spec
}

pub(super) fn catalog() -> Vec<GoC3ReserveEntry> {
    vec![
        GoC3ReserveEntry {
            canonical: ReserveProductSpec::regulation_up(),
            go_vio_cost_key: "REG_UP_vio_cost",
            go_fraction_key: Some("REG_UP"),
            go_ts_key: None,
            device_cap_getter: |d| d.p_reg_res_up_ub,
            device_cost_ts_getter: |ts| ts.p_reg_res_up_cost.as_slice(),
            skip_for_producers: false,
        },
        GoC3ReserveEntry {
            canonical: ReserveProductSpec::regulation_down(),
            go_vio_cost_key: "REG_DOWN_vio_cost",
            go_fraction_key: Some("REG_DOWN"),
            go_ts_key: None,
            device_cap_getter: |d| d.p_reg_res_down_ub,
            device_cost_ts_getter: |ts| ts.p_reg_res_down_cost.as_slice(),
            skip_for_producers: false,
        },
        GoC3ReserveEntry {
            canonical: goc3_cascade_synchronized(),
            go_vio_cost_key: "SYN_vio_cost",
            go_fraction_key: Some("SYN"),
            go_ts_key: None,
            device_cap_getter: |d| d.p_syn_res_ub,
            device_cost_ts_getter: |ts| ts.p_syn_res_cost.as_slice(),
            skip_for_producers: false,
        },
        GoC3ReserveEntry {
            canonical: goc3_cascade_non_synchronized(),
            go_vio_cost_key: "NSYN_vio_cost",
            go_fraction_key: Some("NSYN"),
            go_ts_key: None,
            device_cap_getter: |d| d.p_nsyn_res_ub,
            device_cost_ts_getter: |ts| ts.p_nsyn_res_cost.as_slice(),
            skip_for_producers: false,
        },
        GoC3ReserveEntry {
            canonical: ReserveProductSpec::ramping_up_online(),
            go_vio_cost_key: "RAMPING_RESERVE_UP_vio_cost",
            go_fraction_key: None,
            go_ts_key: Some("RAMPING_RESERVE_UP"),
            device_cap_getter: |d| d.p_ramp_res_up_online_ub,
            device_cost_ts_getter: |ts| ts.p_ramp_res_up_online_cost.as_slice(),
            skip_for_producers: false,
        },
        GoC3ReserveEntry {
            canonical: ReserveProductSpec::ramping_up_offline(),
            go_vio_cost_key: "RAMPING_RESERVE_UP_vio_cost",
            go_fraction_key: None,
            go_ts_key: Some("RAMPING_RESERVE_UP"),
            device_cap_getter: |d| d.p_ramp_res_up_offline_ub,
            device_cost_ts_getter: |ts| ts.p_ramp_res_up_offline_cost.as_slice(),
            skip_for_producers: false,
        },
        GoC3ReserveEntry {
            canonical: ReserveProductSpec::ramping_down_online(),
            go_vio_cost_key: "RAMPING_RESERVE_DOWN_vio_cost",
            go_fraction_key: None,
            go_ts_key: Some("RAMPING_RESERVE_DOWN"),
            device_cap_getter: |d| d.p_ramp_res_down_online_ub,
            device_cost_ts_getter: |ts| ts.p_ramp_res_down_online_cost.as_slice(),
            skip_for_producers: false,
        },
        GoC3ReserveEntry {
            canonical: ReserveProductSpec::ramping_down_offline(),
            go_vio_cost_key: "RAMPING_RESERVE_DOWN_vio_cost",
            go_fraction_key: None,
            go_ts_key: Some("RAMPING_RESERVE_DOWN"),
            device_cap_getter: |_d| 0.0,
            device_cost_ts_getter: |ts| ts.p_ramp_res_down_offline_cost.as_slice(),
            skip_for_producers: true,
        },
        GoC3ReserveEntry {
            canonical: ReserveProductSpec::reactive_up(),
            go_vio_cost_key: "REACT_UP_vio_cost",
            go_fraction_key: None,
            go_ts_key: Some("REACT_UP"),
            device_cap_getter: |_d| 0.0,
            device_cost_ts_getter: |ts| ts.q_res_up_cost.as_slice(),
            skip_for_producers: true,
        },
        GoC3ReserveEntry {
            canonical: ReserveProductSpec::reactive_down(),
            go_vio_cost_key: "REACT_DOWN_vio_cost",
            go_fraction_key: None,
            go_ts_key: Some("REACT_DOWN"),
            device_cap_getter: |_d| 0.0,
            device_cost_ts_getter: |ts| ts.q_res_down_cost.as_slice(),
            skip_for_producers: true,
        },
    ]
}

/// Build reserve products and zonal requirement rows from a GO C3
/// problem.
///
/// Delegates the actual product/requirement construction to the
/// canonical helpers in [`crate::reserves`]; this routine is
/// responsible only for reading GO C3 fields and feeding them into
/// those helpers.
pub(super) fn build_reserves(
    problem: &GoC3Problem,
    context: &GoC3Context,
    base_mva: f64,
    periods: usize,
    penalty_multiplier: f64,
) -> (Vec<ReserveProduct>, Vec<ZonalReserveRequirement>) {
    let active_product_ids: HashSet<&str> = context
        .reserve_product_ids
        .iter()
        .map(|s| s.as_str())
        .collect();
    if active_product_ids.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let entries = catalog();
    let mut products = Vec::new();
    let mut zonal_requirements = Vec::new();

    // Zone UID → area ID (1-indexed). Active zones first, then reactive.
    let active_zone_uid_to_area: HashMap<&str, usize> = problem
        .network
        .active_zonal_reserve
        .iter()
        .enumerate()
        .map(|(i, z)| (z.uid.as_str(), i + 1))
        .collect();
    let reactive_zone_uid_to_area: HashMap<&str, usize> = problem
        .network
        .reactive_zonal_reserve
        .iter()
        .enumerate()
        .map(|(i, z)| (z.uid.as_str(), i + 1 + active_zone_uid_to_area.len()))
        .collect();

    // Bus-to-zone membership for participant_bus_numbers.
    let mut active_zone_buses: HashMap<&str, Vec<u32>> = HashMap::new();
    let mut reactive_zone_buses: HashMap<&str, Vec<u32>> = HashMap::new();
    for bus in &problem.network.bus {
        if let Some(&bus_number) = context.bus_uid_to_number.get(&bus.uid) {
            for zone_uid in &bus.active_reserve_uids {
                if active_zone_uid_to_area.contains_key(zone_uid.as_str()) {
                    active_zone_buses
                        .entry(zone_uid.as_str())
                        .or_default()
                        .push(bus_number);
                }
            }
            for zone_uid in &bus.reactive_reserve_uids {
                if reactive_zone_uid_to_area.contains_key(zone_uid.as_str()) {
                    reactive_zone_buses
                        .entry(zone_uid.as_str())
                        .or_default()
                        .push(bus_number);
                }
            }
        }
    }

    for entry in &entries {
        if !active_product_ids.contains(entry.canonical.id.as_str()) {
            continue;
        }
        let (vio_cost, has_any_zone) = match entry.canonical.kind {
            ReserveKind::Real | ReserveKind::ReactiveHeadroom => {
                let cost = problem
                    .network
                    .active_zonal_reserve
                    .iter()
                    .map(|z| active_zone_vio_cost(z, entry.go_vio_cost_key))
                    .fold(0.0_f64, f64::max);
                (cost, !problem.network.active_zonal_reserve.is_empty())
            }
            ReserveKind::Reactive => {
                let cost = problem
                    .network
                    .reactive_zonal_reserve
                    .iter()
                    .map(|z| reactive_zone_vio_cost(z, entry.go_vio_cost_key))
                    .fold(0.0_f64, f64::max);
                (cost, !problem.network.reactive_zonal_reserve.is_empty())
            }
        };
        if !has_any_zone || vio_cost <= 0.0 {
            continue;
        }

        products.push(reserve_product_from_catalog(
            &entry.canonical,
            vio_cost * penalty_multiplier / base_mva.max(1.0),
        ));

        // Zonal requirements — only real products. Reactive requirements
        // flow through the synthetic q_headroom product below.
        if matches!(entry.canonical.kind, ReserveKind::Real) {
            build_real_zone_requirements(
                entry,
                problem,
                &active_zone_uid_to_area,
                &active_zone_buses,
                base_mva,
                periods,
                penalty_multiplier,
                &mut zonal_requirements,
            );
        }
    }

    // Synthetic q_headroom product for reactive zones.
    if active_product_ids.contains("q_headroom") {
        build_q_headroom_product(
            problem,
            &reactive_zone_uid_to_area,
            &reactive_zone_buses,
            base_mva,
            periods,
            penalty_multiplier,
            &mut products,
            &mut zonal_requirements,
        );
    }

    (products, zonal_requirements)
}

fn build_real_zone_requirements(
    entry: &GoC3ReserveEntry,
    problem: &GoC3Problem,
    zone_uid_to_area: &HashMap<&str, usize>,
    zone_buses: &HashMap<&str, Vec<u32>>,
    base_mva: f64,
    periods: usize,
    penalty_multiplier: f64,
    out: &mut Vec<ZonalReserveRequirement>,
) {
    let zone_ts_by_uid: HashMap<&str, &_> = problem
        .time_series_input
        .active_zonal_reserve
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();

    for zone in &problem.network.active_zonal_reserve {
        let Some(&area_id) = zone_uid_to_area.get(zone.uid.as_str()) else {
            continue;
        };
        let shortfall = active_zone_vio_cost(zone, entry.go_vio_cost_key) * penalty_multiplier
            / base_mva.max(1.0);
        let participant_buses = zone_buses.get(zone.uid.as_str()).cloned();

        let requirement = if let Some(fraction_key) = entry.go_fraction_key {
            let sigma = zone_fraction(zone, fraction_key);
            if sigma <= 0.0 {
                continue;
            }
            match entry.canonical.id.as_str() {
                "reg_up" | "reg_down" => Some(zonal_requirement_from_load_fraction(
                    area_id,
                    entry.canonical.id.clone(),
                    sigma,
                    periods,
                    participant_buses,
                    shortfall,
                )),
                "syn" | "nsyn" => Some(zonal_requirement_from_largest_unit(
                    area_id,
                    entry.canonical.id.clone(),
                    sigma,
                    participant_buses,
                    shortfall,
                )),
                _ => None,
            }
        } else if let Some(ts_key) = entry.go_ts_key {
            let zone_ts = zone_ts_by_uid.get(zone.uid.as_str()).copied();
            let mut vals = vec![0.0; periods];
            if let Some(series) = zone_ts.map(|ts| ts_values_for_key(ts, ts_key)) {
                for (i, &v) in series.iter().enumerate().take(periods) {
                    vals[i] = v * base_mva;
                }
            }
            if vals.iter().any(|x| x.abs() > 1e-12) {
                Some(zonal_requirement_from_series(
                    area_id,
                    entry.canonical.id.clone(),
                    vals,
                    participant_buses,
                    shortfall,
                ))
            } else {
                None
            }
        } else {
            None
        };

        if let Some(req) = requirement {
            out.push(req);
        }
    }
}

fn build_q_headroom_product(
    problem: &GoC3Problem,
    zone_uid_to_area: &HashMap<&str, usize>,
    zone_buses: &HashMap<&str, Vec<u32>>,
    base_mva: f64,
    periods: usize,
    penalty_multiplier: f64,
    products: &mut Vec<ReserveProduct>,
    requirements: &mut Vec<ZonalReserveRequirement>,
) {
    let reactive_ts_by_uid: HashMap<&str, &_> = problem
        .time_series_input
        .reactive_zonal_reserve
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();

    let mut emitted_product = false;
    for zone in &problem.network.reactive_zonal_reserve {
        let combined_cost = zone.REACT_UP_vio_cost + zone.REACT_DOWN_vio_cost;
        if combined_cost <= 0.0 {
            continue;
        }
        let Some(&area_id) = zone_uid_to_area.get(zone.uid.as_str()) else {
            continue;
        };
        let ts = reactive_ts_by_uid.get(zone.uid.as_str()).copied();
        let mut combined_mw = vec![0.0; periods];
        if let Some(ts) = ts {
            for (i, slot) in combined_mw.iter_mut().enumerate() {
                let up = ts.REACT_UP.get(i).copied().unwrap_or(0.0);
                let down = ts.REACT_DOWN.get(i).copied().unwrap_or(0.0);
                *slot = (up + down) * base_mva;
            }
        }
        if !combined_mw.iter().any(|v| *v > 1e-9) {
            continue;
        }

        if !emitted_product {
            products.push(reactive_headroom_product(
                combined_cost * penalty_multiplier / base_mva.max(1.0),
            ));
            emitted_product = true;
        }

        let participants = zone_buses.get(zone.uid.as_str()).cloned();
        requirements.push(zonal_requirement_from_series(
            area_id,
            "q_headroom",
            combined_mw,
            participants,
            combined_cost * penalty_multiplier / base_mva.max(1.0),
        ));
    }
}

/// Per-period generator reserve offer curves for each active product.
pub(super) fn build_generator_reserve_offer_schedules(
    problem: &GoC3Problem,
    context: &GoC3Context,
    device_ts_by_uid: &HashMap<&str, &GoC3DeviceTimeSeries>,
    base_mva: f64,
    periods: usize,
) -> Vec<GeneratorReserveOfferSchedule> {
    let active_ids: HashSet<&str> = context
        .reserve_product_ids
        .iter()
        .map(|s| s.as_str())
        .collect();
    if active_ids.is_empty() {
        return Vec::new();
    }

    let entries = catalog();
    let mut out = Vec::new();

    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Producer {
            continue;
        }
        let Some(ts) = device_ts_by_uid.get(device.uid.as_str()) else {
            continue;
        };

        let mut per_period: Vec<Vec<ReserveOffer>> = vec![Vec::new(); periods];
        for entry in &entries {
            if !active_ids.contains(entry.canonical.id.as_str()) || entry.skip_for_producers {
                continue;
            }
            if matches!(
                entry.canonical.kind,
                ReserveKind::Reactive | ReserveKind::ReactiveHeadroom
            ) {
                continue; // reactive and headroom have no per-device cap field
            }
            let cap_pu = (entry.device_cap_getter)(device);
            if cap_pu <= 1e-12 {
                continue;
            }
            let capacity_mw = cap_pu * base_mva;
            let cost_ts = (entry.device_cost_ts_getter)(ts);
            for (period_idx, period_offers) in per_period.iter_mut().enumerate().take(periods) {
                let cost_pu = cost_ts
                    .get(period_idx)
                    .copied()
                    .unwrap_or_else(|| cost_ts.last().copied().unwrap_or(0.0));
                period_offers.push(ReserveOffer {
                    product_id: entry.canonical.id.clone(),
                    capacity_mw,
                    cost_per_mwh: cost_pu / base_mva.max(1.0),
                });
            }
        }
        if per_period.iter().any(|p| !p.is_empty()) {
            out.push(GeneratorReserveOfferSchedule {
                resource_id: device.uid.clone(),
                schedule: ReserveOfferSchedule {
                    periods: per_period,
                },
            });
        }
    }
    out
}

// ── GO C3 field accessors ─────────────────────────────────────────────────

#[allow(non_snake_case)]
fn active_zone_vio_cost(zone: &GoC3ActiveZonalReserve, key: &str) -> f64 {
    match key {
        "REG_UP_vio_cost" => zone.REG_UP_vio_cost,
        "REG_DOWN_vio_cost" => zone.REG_DOWN_vio_cost,
        "SYN_vio_cost" => zone.SYN_vio_cost,
        "NSYN_vio_cost" => zone.NSYN_vio_cost,
        "RAMPING_RESERVE_UP_vio_cost" => zone.RAMPING_RESERVE_UP_vio_cost,
        "RAMPING_RESERVE_DOWN_vio_cost" => zone.RAMPING_RESERVE_DOWN_vio_cost,
        _ => 0.0,
    }
}

#[allow(non_snake_case)]
fn reactive_zone_vio_cost(zone: &GoC3ReactiveZonalReserve, key: &str) -> f64 {
    match key {
        "REACT_UP_vio_cost" => zone.REACT_UP_vio_cost,
        "REACT_DOWN_vio_cost" => zone.REACT_DOWN_vio_cost,
        _ => 0.0,
    }
}

#[allow(non_snake_case)]
fn zone_fraction(zone: &GoC3ActiveZonalReserve, key: &str) -> f64 {
    match key {
        "REG_UP" => zone.REG_UP,
        "REG_DOWN" => zone.REG_DOWN,
        "SYN" => zone.SYN,
        "NSYN" => zone.NSYN,
        _ => 0.0,
    }
}

#[allow(non_snake_case)]
fn ts_values_for_key(ts: &GoC3ActiveReserveTimeSeries, key: &str) -> Vec<f64> {
    match key {
        "RAMPING_RESERVE_UP" => ts.RAMPING_RESERVE_UP.clone(),
        "RAMPING_RESERVE_DOWN" => ts.RAMPING_RESERVE_DOWN.clone(),
        "SYN" => ts.SYN.clone(),
        "NSYN" => ts.NSYN.clone(),
        "REG_UP" => ts.REG_UP.clone(),
        "REG_DOWN" => ts.REG_DOWN.clone(),
        _ => Vec::new(),
    }
}
