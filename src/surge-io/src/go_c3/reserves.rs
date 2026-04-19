// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO C3 reserve product and offer wiring.
//!
//! Responsibilities split between this module (network-build time) and
//! `surge-dispatch::go_c3` (request-build time):
//!
//! * **This module (`surge-io::go_c3::reserves`)** determines which GO C3
//!   reserve products are "active" for the scenario (i.e. have a nonzero
//!   violation cost in at least one zone) and records their IDs on
//!   [`GoC3Context::reserve_product_ids`]. It then writes per-producer
//!   [`ReserveOffer`] rows onto each generator's
//!   `MarketParams::reserve_offers` based on the device's per-product
//!   capability fields (`p_reg_res_up_ub`, `p_syn_res_ub`, etc.) and the
//!   corresponding per-period cost time series.
//!
//! * **`surge-dispatch::go_c3`** builds the [`ReserveProduct`] definitions
//!   and [`ZonalReserveRequirement`] rows that go into the dispatch request
//!   itself. Those depend on types that live in surge-dispatch and cannot
//!   be referenced from surge-io due to the dependency direction.
//!
//! Mirrors Python `markets/go_c3/adapter.py::_build_reserve_products` (the
//! active-ID selection half) and `_apply_generator_reserve_offers`.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::market::ReserveOffer;
use surge_network::network::MarketParams;

use super::Error;
use super::context::GoC3Context;
use super::types::*;

/// Canonical GO C3 → Surge reserve product definitions, ordered to match
/// the Python adapter's `_GO_RESERVE_PRODUCT_MAP`. The ordering matters
/// because [`GoC3Context::reserve_product_ids`] preserves it for
/// downstream consumers.
struct GoReserveProductSpec {
    /// Surge product ID (`"reg_up"`, `"syn"`, …).
    id: &'static str,
    /// Kind (`Real` or `Reactive`) — matches the surge-network kind enum.
    kind: GoReserveKind,
    /// GO C3 violation-cost field on `active_zonal_reserve` /
    /// `reactive_zonal_reserve` (e.g. `"REG_UP_vio_cost"`).
    go_vio_cost_key: &'static str,
    /// Getter that returns the device's per-unit capability for this product
    /// (e.g. `p_reg_res_up_ub`).
    device_cap: fn(&GoC3Device) -> f64,
    /// Getter that returns a reference to the per-period cost time series
    /// for this product on a device (e.g. `p_reg_res_up_cost`). For
    /// reactive products this is `q_res_{up,down}_cost`; for the synthetic
    /// `q_headroom` product the adapter never writes an offer.
    device_cost_ts: fn(&GoC3DeviceTimeSeries) -> &[f64],
    /// True when producers may not offer this product. GO C3 §4.6 eq (106)
    /// sets `p^rrd,off = 0` for producers.
    skip_for_producers: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoReserveKind {
    Real,
    Reactive,
}

fn go_reserve_product_specs() -> &'static [GoReserveProductSpec] {
    &[
        GoReserveProductSpec {
            id: "reg_up",
            kind: GoReserveKind::Real,
            go_vio_cost_key: "REG_UP_vio_cost",
            device_cap: |d| d.p_reg_res_up_ub,
            device_cost_ts: |ts| ts.p_reg_res_up_cost.as_slice(),
            skip_for_producers: false,
        },
        GoReserveProductSpec {
            id: "reg_down",
            kind: GoReserveKind::Real,
            go_vio_cost_key: "REG_DOWN_vio_cost",
            device_cap: |d| d.p_reg_res_down_ub,
            device_cost_ts: |ts| ts.p_reg_res_down_cost.as_slice(),
            skip_for_producers: false,
        },
        GoReserveProductSpec {
            id: "syn",
            kind: GoReserveKind::Real,
            go_vio_cost_key: "SYN_vio_cost",
            device_cap: |d| d.p_syn_res_ub,
            device_cost_ts: |ts| ts.p_syn_res_cost.as_slice(),
            skip_for_producers: false,
        },
        GoReserveProductSpec {
            id: "nsyn",
            kind: GoReserveKind::Real,
            go_vio_cost_key: "NSYN_vio_cost",
            device_cap: |d| d.p_nsyn_res_ub,
            device_cost_ts: |ts| ts.p_nsyn_res_cost.as_slice(),
            skip_for_producers: false,
        },
        GoReserveProductSpec {
            id: "ramp_up_on",
            kind: GoReserveKind::Real,
            go_vio_cost_key: "RAMPING_RESERVE_UP_vio_cost",
            device_cap: |d| d.p_ramp_res_up_online_ub,
            device_cost_ts: |ts| ts.p_ramp_res_up_online_cost.as_slice(),
            skip_for_producers: false,
        },
        GoReserveProductSpec {
            id: "ramp_up_off",
            kind: GoReserveKind::Real,
            go_vio_cost_key: "RAMPING_RESERVE_UP_vio_cost",
            device_cap: |d| d.p_ramp_res_up_offline_ub,
            device_cost_ts: |ts| ts.p_ramp_res_up_offline_cost.as_slice(),
            skip_for_producers: false,
        },
        GoReserveProductSpec {
            id: "ramp_down_on",
            kind: GoReserveKind::Real,
            go_vio_cost_key: "RAMPING_RESERVE_DOWN_vio_cost",
            device_cap: |d| d.p_ramp_res_down_online_ub,
            device_cost_ts: |ts| ts.p_ramp_res_down_online_cost.as_slice(),
            skip_for_producers: false,
        },
        GoReserveProductSpec {
            id: "ramp_down_off",
            kind: GoReserveKind::Real,
            go_vio_cost_key: "RAMPING_RESERVE_DOWN_vio_cost",
            device_cap: |_d| 0.0, // producers never offer this; see §4.6 eq (106)
            device_cost_ts: |ts| ts.p_ramp_res_down_offline_cost.as_slice(),
            skip_for_producers: true,
        },
        // Reactive reserves: no per-device cap field in GO C3 (the AC OPF
        // derives headroom from q_min/q_max), so producers don't write
        // offers for these. We still need them in the active-id list so
        // the reactive products show up in downstream pipelines.
        GoReserveProductSpec {
            id: "q_res_up",
            kind: GoReserveKind::Reactive,
            go_vio_cost_key: "REACT_UP_vio_cost",
            device_cap: |_d| 0.0,
            device_cost_ts: |ts| ts.q_res_up_cost.as_slice(),
            skip_for_producers: true,
        },
        GoReserveProductSpec {
            id: "q_res_down",
            kind: GoReserveKind::Reactive,
            go_vio_cost_key: "REACT_DOWN_vio_cost",
            device_cap: |_d| 0.0,
            device_cost_ts: |ts| ts.q_res_down_cost.as_slice(),
            skip_for_producers: true,
        },
    ]
}

/// Determine which reserve products are active and wire per-producer
/// offers onto the network's generators.
///
/// Idempotent: calling this twice with the same inputs yields the same
/// result. Safe to call after the enrichment pass has populated generator
/// operational metadata.
pub fn apply_reserves(
    network: &mut Network,
    context: &mut GoC3Context,
    problem: &GoC3Problem,
) -> Result<(), Error> {
    context.reserve_product_ids.clear();

    let active_ids = determine_active_product_ids(problem);
    if active_ids.is_empty() {
        return Ok(());
    }

    context.reserve_product_ids = active_ids.clone();

    // Note: the synthetic `q_headroom` product is conditionally added by
    // the dispatch-request builder (surge-dispatch::go_c3) when the
    // reactive zonal reserve fields carry a nonzero violation cost. It
    // never gets a per-device offer, so it is not represented in the
    // spec table above.
    maybe_append_q_headroom(&mut context.reserve_product_ids, problem);

    apply_generator_reserve_offers(network, problem, &active_ids)?;

    Ok(())
}

/// Compute the set of active reserve product IDs based on which products
/// have a nonzero violation cost in at least one zone. Mirrors
/// `_active_reserve_product_ids` in the Python adapter.
fn determine_active_product_ids(problem: &GoC3Problem) -> Vec<String> {
    let has_active = !problem.network.active_zonal_reserve.is_empty();
    let has_reactive = !problem.network.reactive_zonal_reserve.is_empty();
    if !has_active && !has_reactive {
        return Vec::new();
    }

    let mut active_ids = Vec::new();
    for spec in go_reserve_product_specs() {
        let is_active = match spec.kind {
            GoReserveKind::Real => problem
                .network
                .active_zonal_reserve
                .iter()
                .any(|zone| zone_vio_cost(zone, spec.go_vio_cost_key) > 0.0),
            GoReserveKind::Reactive => problem
                .network
                .reactive_zonal_reserve
                .iter()
                .any(|zone| reactive_zone_vio_cost(zone, spec.go_vio_cost_key) > 0.0),
        };
        if is_active {
            active_ids.push(spec.id.to_string());
        }
    }
    active_ids
}

/// Add the synthetic `q_headroom` product ID when reactive zonal reserves
/// carry a nonzero combined violation cost. The product itself is emitted
/// by the dispatch request builder; we only add it to the context's ID
/// list so downstream consumers know to expect it.
fn maybe_append_q_headroom(ids: &mut Vec<String>, problem: &GoC3Problem) {
    if problem.network.reactive_zonal_reserve.is_empty() {
        return;
    }
    let has_q_headroom = problem
        .network
        .reactive_zonal_reserve
        .iter()
        .any(|zone| (zone.REACT_UP_vio_cost + zone.REACT_DOWN_vio_cost) > 0.0);
    if has_q_headroom && !ids.iter().any(|id| id == "q_headroom") {
        ids.push("q_headroom".to_string());
    }
}

fn zone_vio_cost(zone: &GoC3ActiveZonalReserve, key: &str) -> f64 {
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

fn reactive_zone_vio_cost(zone: &GoC3ReactiveZonalReserve, key: &str) -> f64 {
    match key {
        "REACT_UP_vio_cost" => zone.REACT_UP_vio_cost,
        "REACT_DOWN_vio_cost" => zone.REACT_DOWN_vio_cost,
        _ => 0.0,
    }
}

/// Write per-producer [`ReserveOffer`] rows onto `Generator::market.reserve_offers`.
///
/// Only producers with nonzero active-power capability contribute. The
/// static fallback cost stored on the network uses the period-0 value of
/// the per-period cost time series; per-period costs flow through the
/// dispatch request builder's `generator_reserve_offer_schedules` path.
fn apply_generator_reserve_offers(
    network: &mut Network,
    problem: &GoC3Problem,
    active_ids: &[String],
) -> Result<(), Error> {
    if active_ids.is_empty() {
        return Ok(());
    }

    let base_mva = problem.network.general.base_norm_mva;

    let device_ts_by_uid: HashMap<&str, &GoC3DeviceTimeSeries> = problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();

    // Pre-index raw devices by UID for O(1) lookup.
    let devices_by_uid: HashMap<&str, &GoC3Device> = problem
        .network
        .simple_dispatchable_device
        .iter()
        .map(|d| (d.uid.as_str(), d))
        .collect();

    let active_id_set: std::collections::HashSet<&str> =
        active_ids.iter().map(|s| s.as_str()).collect();

    for generator in network.generators.iter_mut() {
        let Some(device) = devices_by_uid.get(generator.id.as_str()) else {
            continue;
        };
        if device.device_type != GoC3DeviceType::Producer {
            continue;
        }
        let Some(ts) = device_ts_by_uid.get(generator.id.as_str()) else {
            continue;
        };
        if is_zero_mw_producer(ts) {
            continue;
        }

        let mut offers: Vec<ReserveOffer> = Vec::new();
        for spec in go_reserve_product_specs() {
            if !active_id_set.contains(spec.id) {
                continue;
            }
            if spec.skip_for_producers {
                continue;
            }
            let cap_pu = (spec.device_cap)(device);
            if cap_pu <= 1e-12 {
                continue;
            }
            let capacity_mw = cap_pu * base_mva;
            let cost_ts = (spec.device_cost_ts)(ts);
            let cost_per_mwh = cost_ts
                .first()
                .copied()
                .map(|v| go_cost_to_mwh(v, base_mva))
                .unwrap_or(0.0);
            offers.push(ReserveOffer {
                product_id: spec.id.to_string(),
                capacity_mw,
                cost_per_mwh,
            });
        }

        if !offers.is_empty() {
            let market = generator.market.get_or_insert_with(MarketParams::default);
            market.reserve_offers = offers;
        }
    }

    Ok(())
}

/// Convert a GO C3 per-unit cost (`$/pu-hour`) to `$/MWh`. Matches
/// `adapter.py::_go_cost_to_mwh`.
fn go_cost_to_mwh(cost_pu: f64, base_mva: f64) -> f64 {
    if base_mva.abs() <= 1e-12 {
        cost_pu
    } else {
        cost_pu / base_mva
    }
}

fn is_zero_mw_producer(ts: &GoC3DeviceTimeSeries) -> bool {
    ts.p_ub.iter().all(|v| v.abs() <= 1e-9) && ts.p_lb.iter().all(|v| v.abs() <= 1e-9)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::go_c3;
    use std::path::PathBuf;

    fn canonical_73bus_d2_911() -> Option<PathBuf> {
        let candidates = [
            std::env::var("SURGE_TEST_DATA").ok().map(|root| {
                PathBuf::from(root)
                    .join("go-c3/datasets/event4_73/D2/C3E4N00073D2/scenario_911.json")
            }),
            Some(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/benchmarks/go-c3/datasets/event4_73/D2/C3E4N00073D2/scenario_911.json"),
            ),
        ];
        candidates.into_iter().flatten().find(|path| path.exists())
    }

    #[test]
    fn reserves_active_ids_match_nonzero_vio_costs() {
        let Some(path) = canonical_73bus_d2_911() else {
            eprintln!("skipping reserves_active_ids_match_nonzero_vio_costs: fixture absent");
            return;
        };
        let (_, context) = go_c3::load_enriched_network(&path, &go_c3::GoC3Policy::default())
            .expect("load_enriched_network");

        // Scenario 911 has nonzero violation costs for every real product
        // plus REACT_UP/REACT_DOWN, so the active list should include all
        // 10 real/reactive products plus the synthetic q_headroom.
        let ids: Vec<&str> = context
            .reserve_product_ids
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert!(ids.contains(&"reg_up"), "missing reg_up: {:?}", ids);
        assert!(ids.contains(&"reg_down"));
        assert!(ids.contains(&"syn"));
        assert!(ids.contains(&"nsyn"));
        assert!(ids.contains(&"ramp_up_on"));
        assert!(ids.contains(&"ramp_up_off"));
        assert!(ids.contains(&"ramp_down_on"));
        assert!(ids.contains(&"ramp_down_off"));
        assert!(ids.contains(&"q_res_up"));
        assert!(ids.contains(&"q_res_down"));
        assert!(ids.contains(&"q_headroom"));
    }

    #[test]
    fn reserves_generator_offers_mirror_python_adapter() {
        let Some(path) = canonical_73bus_d2_911() else {
            eprintln!("skipping reserves_generator_offers_mirror_python_adapter: fixture absent");
            return;
        };
        let (network, _) = go_c3::load_enriched_network(&path, &go_c3::GoC3Policy::default())
            .expect("load_enriched_network");

        let unit = network
            .generators
            .iter()
            .find(|g| g.id == "sd_051")
            .expect("sd_051 missing");
        let market = unit.market.as_ref().expect("sd_051 market");
        let offers_by_product: HashMap<&str, &ReserveOffer> = market
            .reserve_offers
            .iter()
            .map(|o| (o.product_id.as_str(), o))
            .collect();

        // sd_051 caps from problem.json:
        //   reg_up = 0.185 pu → 18.5 MW at $6/MWh (600/100)
        //   reg_down = 0.185 pu → 18.5 MW at $6/MWh
        //   syn = 0 pu (skipped)
        //   nsyn = 0.37 pu → 37 MW at $0/MWh
        //   ramp_up_on/off = 0.37 pu → 37 MW at $0/MWh each
        //   ramp_down_on = 0.37 pu → 37 MW at $0/MWh
        //   ramp_down_off = SKIPPED (§4.6 eq 106: producers can't offer)
        let reg_up = offers_by_product.get("reg_up").expect("reg_up offer");
        assert!((reg_up.capacity_mw - 18.5).abs() < 1e-6);
        assert!((reg_up.cost_per_mwh - 6.0).abs() < 1e-6);

        let reg_down = offers_by_product.get("reg_down").expect("reg_down offer");
        assert!((reg_down.capacity_mw - 18.5).abs() < 1e-6);
        assert!((reg_down.cost_per_mwh - 6.0).abs() < 1e-6);

        assert!(
            !offers_by_product.contains_key("syn"),
            "sd_051 has p_syn_res_ub=0 so it should not offer syn"
        );

        let nsyn = offers_by_product.get("nsyn").expect("nsyn offer");
        assert!((nsyn.capacity_mw - 37.0).abs() < 1e-6);
        assert!(nsyn.cost_per_mwh.abs() < 1e-9);

        for pid in ["ramp_up_on", "ramp_up_off", "ramp_down_on"] {
            let o = offers_by_product
                .get(pid)
                .unwrap_or_else(|| panic!("{} offer missing", pid));
            assert!(
                (o.capacity_mw - 37.0).abs() < 1e-6,
                "{} capacity {} != 37",
                pid,
                o.capacity_mw
            );
        }

        assert!(
            !offers_by_product.contains_key("ramp_down_off"),
            "producers never offer ramp_down_off (§4.6 eq 106)"
        );
    }
}
