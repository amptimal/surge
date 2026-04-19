// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO C3 enrichment pass.
//!
//! `to_network` in `network.rs` is intentionally lossless and policy-agnostic:
//! it builds the structural network (buses, branches, generators, loads,
//! shunts, DC lines) directly from the `GoC3Problem` without consulting the
//! time-series data, commitment schedules, or reserve products.
//!
//! This module layers additional, time-series-informed enrichments on top of
//! that bare network. Each enrichment is driven by a flag on
//! [`GoC3EnrichOptions`] so callers can opt in incrementally and reproduce
//! the behaviour of Python's `markets/go_c3/adapter.py::build_surge_network`
//! piece by piece.
//!
//! Enrichments currently implemented:
//!
//! * **Generator operational data** — populates `pmin`/`pmax`/`qmin`/`qmax`
//!   from the static envelope of the per-period time series, wires
//!   `CommitmentParams` (min up/down, startup/shutdown ramps, quick-start),
//!   the ramp curve, and startup cost tiers (via `MarketParams::energy_offer`).
//! * **Slack bus inference** — replaces the naïve "first PV bus or bus 1"
//!   rule with the reactive-capability heuristic used by the Python adapter
//!   (largest single reactive-capable producer).
//!
//! Policy-dependent enrichments (reserves, voltage regulation, dispatchable
//! consumer mode, DC line reactive terminals) live in sibling modules.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::market::{EnergyOffer, OfferCurve, StartupTier};
use surge_network::network::{BusType, CommitmentParams, Generator, MarketParams, RampingParams};

use super::Error;
use super::context::{GoC3Context, GoC3DeviceKind};
use super::policy::{GoC3Policy, GoC3SlackInferenceMode};
use super::types::*;

/// Apply enrichments to a network built by [`super::network::to_network`].
///
/// This pass reads the `GoC3Policy` but only consults the subset of policy
/// fields that affect operational envelope and slack-bus selection. Other
/// policy-sensitive enrichments (reserves, voltage, consumers, HVDC q)
/// live in dedicated sibling modules and must be called separately.
pub fn enrich_network(
    network: &mut Network,
    context: &mut GoC3Context,
    problem: &GoC3Problem,
    policy: &GoC3Policy,
) -> Result<(), Error> {
    if policy.slack_mode == GoC3SlackInferenceMode::ReactiveCapability {
        apply_reactive_capability_slack(network, problem, context);
    }

    apply_generator_enrichments(network, problem)?;
    reclassify_zero_mw_producers(context, problem);

    Ok(())
}

/// Reclassify zero-MW producers (every-period `p_ub <= 1e-9` and `p_lb <= 1e-9`)
/// as `ProducerStatic` in the context's device-kind map. Mirrors Python's
/// `_is_zero_mw_producer` check; the AC reconcile's pin helper keys on this
/// to hard-pin their P output at zero rather than banding.
fn reclassify_zero_mw_producers(context: &mut GoC3Context, problem: &GoC3Problem) {
    let ts_by_uid: HashMap<&str, &GoC3DeviceTimeSeries> = problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Producer {
            continue;
        }
        let Some(ts) = ts_by_uid.get(device.uid.as_str()) else {
            continue;
        };
        let is_zero_mw =
            ts.p_ub.iter().all(|v| v.abs() <= 1e-9) && ts.p_lb.iter().all(|v| v.abs() <= 1e-9);
        if is_zero_mw {
            context
                .device_kind_by_uid
                .insert(device.uid.clone(), GoC3DeviceKind::ProducerStatic);
        }
    }
}

// ─── Slack bus inference ─────────────────────────────────────────────────────

fn apply_reactive_capability_slack(
    network: &mut Network,
    problem: &GoC3Problem,
    context: &mut GoC3Context,
) {
    // If the GO C3 input explicitly labels any bus "Slack", honour that
    // and leave the network as-is. We cannot rely on checking `network.buses`
    // for an existing Slack because `network.rs::convert_buses` promotes
    // the first PV/first bus as a fallback when the GO C3 input has no
    // explicit Slack — the fallback is exactly what we want to override
    // here with the reactive-capability heuristic.
    let explicit_slack = problem
        .network
        .bus
        .iter()
        .any(|b| b.bus_type.as_deref() == Some("Slack"));
    if explicit_slack {
        return;
    }

    // Index the time series by device UID for quick q-range lookup.
    let device_ts_by_uid: HashMap<&str, &GoC3DeviceTimeSeries> = problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();

    // Score each bus by (peak p_ub_mw, q_range_mvar) of the single largest
    // reactive-capable producer it hosts. Mirrors the Python heuristic.
    let base_mva = problem.network.general.base_norm_mva;
    let mut best: Option<(String, (f64, f64))> = None;
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Producer {
            continue;
        }
        let Some(ts) = device_ts_by_uid.get(device.uid.as_str()) else {
            continue;
        };
        if !has_reactive_regulation_range(ts) {
            continue;
        }
        let peak_p_mw = ts
            .p_ub
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max)
            .max(0.0)
            * base_mva;
        let q_min = ts.q_lb.iter().copied().fold(f64::INFINITY, f64::min);
        let q_max = ts.q_ub.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let q_range_mvar = if q_min.is_finite() && q_max.is_finite() {
            (q_max - q_min) * base_mva
        } else {
            0.0
        };
        let score = (peak_p_mw, q_range_mvar);
        match &best {
            None => best = Some((device.bus.clone(), score)),
            Some((_, existing)) if score > *existing => {
                best = Some((device.bus.clone(), score));
            }
            _ => {}
        }
    }

    let chosen_bus_uid = match best {
        Some((uid, _)) => Some(uid),
        None => problem.network.bus.first().map(|b| b.uid.clone()),
    };

    let Some(target_uid) = chosen_bus_uid else {
        return;
    };
    let Some(&target_bus_number) = context.bus_uid_to_number.get(&target_uid) else {
        return;
    };

    // Demote any bus that `network.rs` promoted as a Slack fallback back to
    // its original GO C3 type (PV/PQ/Isolated). Without this step we could
    // end up with two Slack buses when the heuristic picks a different bus
    // than the fallback's first-PV choice.
    let original_type_by_uid: HashMap<&str, BusType> = problem
        .network
        .bus
        .iter()
        .map(|b| {
            let bt = match b.bus_type.as_deref() {
                Some("Slack") => BusType::Slack,
                Some("PV") => BusType::PV,
                Some("PQ") => BusType::PQ,
                Some("Notused") => BusType::Isolated,
                _ => BusType::PQ,
            };
            (b.uid.as_str(), bt)
        })
        .collect();
    for bus in network.buses.iter_mut() {
        if bus.bus_type != BusType::Slack {
            continue;
        }
        if bus.number == target_bus_number {
            continue;
        }
        if let Some(&original) = original_type_by_uid.get(bus.name.as_str()) {
            bus.bus_type = original;
        }
    }

    if let Some(bus) = network
        .buses
        .iter_mut()
        .find(|b| b.number == target_bus_number)
    {
        bus.bus_type = BusType::Slack;
    }

    // Record the final Slack bus number(s) in the context.
    context.slack_bus_numbers = network
        .buses
        .iter()
        .filter(|b| b.bus_type == BusType::Slack)
        .map(|b| b.number)
        .collect();
}

fn has_reactive_regulation_range(ts: &GoC3DeviceTimeSeries) -> bool {
    if ts.q_lb.is_empty() && ts.q_ub.is_empty() {
        return false;
    }
    let q_min = ts.q_lb.iter().copied().fold(f64::INFINITY, f64::min);
    let q_max = ts.q_ub.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    q_max > q_min + 1e-9
}

// ─── Generator enrichment ───────────────────────────────────────────────────

fn apply_generator_enrichments(network: &mut Network, problem: &GoC3Problem) -> Result<(), Error> {
    let base_mva = problem.network.general.base_norm_mva;

    let device_ts_by_uid: HashMap<&str, &GoC3DeviceTimeSeries> = problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();

    // Build a { uid → (pmax_mw, pmin_mw, qmax_mvar, qmin_mvar) } map once
    // so we don't rescan the time series per generator during mutation.
    let mut envelopes: HashMap<String, (f64, f64, f64, f64)> = HashMap::new();
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Producer {
            continue;
        }
        let Some(ts) = device_ts_by_uid.get(device.uid.as_str()) else {
            continue;
        };
        let pmax_pu = ts.p_ub.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let pmin_pu = ts.p_lb.iter().copied().fold(f64::INFINITY, f64::min);
        let qmax_pu = ts.q_ub.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let qmin_pu = ts.q_lb.iter().copied().fold(f64::INFINITY, f64::min);
        let pmax_mw = if pmax_pu.is_finite() {
            pmax_pu * base_mva
        } else {
            0.0
        };
        let pmin_mw = if pmin_pu.is_finite() {
            pmin_pu * base_mva
        } else {
            0.0
        };
        let qmax_mvar = if qmax_pu.is_finite() {
            qmax_pu * base_mva
        } else {
            9999.0
        };
        let qmin_mvar = if qmin_pu.is_finite() {
            qmin_pu * base_mva
        } else {
            -9999.0
        };
        envelopes.insert(device.uid.clone(), (pmax_mw, pmin_mw, qmax_mvar, qmin_mvar));
    }

    // Index raw devices for the commitment-data pass.
    let devices_by_uid: HashMap<&str, &GoC3Device> = problem
        .network
        .simple_dispatchable_device
        .iter()
        .map(|d| (d.uid.as_str(), d))
        .collect();

    for generator in network.generators.iter_mut() {
        if let Some(&(pmax_mw, pmin_mw, qmax_mvar, qmin_mvar)) = envelopes.get(&generator.id) {
            generator.pmax = pmax_mw;
            generator.pmin = pmin_mw;
            generator.qmax = qmax_mvar;
            generator.qmin = qmin_mvar;
        }

        let Some(device) = devices_by_uid.get(generator.id.as_str()) else {
            continue;
        };

        // Python's `network.add_generator(...)` path populates empty
        // defaults for `commitment`, `ramping`, and `market` even for
        // zero-MW producers that aren't subsequently annotated with
        // commitment metadata. Replicate that so serialized networks
        // match (serde diffs matter at the reconcile boundary).
        generator
            .commitment
            .get_or_insert_with(CommitmentParams::default);
        generator.ramping.get_or_insert_with(RampingParams::default);
        generator.market.get_or_insert_with(MarketParams::default);

        // Skip richer commitment metadata for zero-MW producers — Python
        // adapter.py classifies these as "producer_static" and leaves
        // commitment/ramp/startup fields at their Default values.
        let is_zero_mw = device_ts_by_uid
            .get(generator.id.as_str())
            .map(|ts| {
                ts.p_ub.iter().all(|v| v.abs() <= 1e-9) && ts.p_lb.iter().all(|v| v.abs() <= 1e-9)
            })
            .unwrap_or(false);
        if is_zero_mw {
            continue;
        }
        apply_commitment_for_generator(generator, device, base_mva);
    }

    Ok(())
}

fn apply_commitment_for_generator(generator: &mut Generator, device: &GoC3Device, base_mva: f64) {
    // Commitment data (min up/down, startup/shutdown ramps, quick-start).
    // Mirrors Python `_apply_generator_commitment_metadata`, which writes
    // these fields unconditionally (including 0.0 when absent from the
    // input) so that the Python rich-object view matches the network
    // state exactly.
    let commitment = generator
        .commitment
        .get_or_insert_with(CommitmentParams::default);
    commitment.min_up_time_hr = Some(device.in_service_time_lb);
    commitment.min_down_time_hr = Some(device.down_time_lb);
    let startup_ramp_mw_per_min = device.p_startup_ramp_ub * base_mva / 60.0;
    let shutdown_ramp_mw_per_min = device.p_shutdown_ramp_ub * base_mva / 60.0;
    if startup_ramp_mw_per_min > 0.0 {
        commitment.startup_ramp_mw_per_min = Some(startup_ramp_mw_per_min);
    }
    if shutdown_ramp_mw_per_min > 0.0 {
        commitment.shutdown_ramp_mw_per_min = Some(shutdown_ramp_mw_per_min);
    }

    // Quick-start = any offline reserve capability. Mirrors the Python rule
    // in `_apply_generator_commitment_metadata` (adapter.py:1768-1774).
    generator.quick_start = device.p_nsyn_res_ub.abs() > 1e-9
        || device.p_ramp_res_up_offline_ub.abs() > 1e-9
        || device.p_ramp_res_down_offline_ub.abs() > 1e-9;

    // Ramp curves (normal up/down only — GO C3 has a single scalar rate).
    let ramp_up_mw_per_min = device.p_ramp_up_ub * base_mva / 60.0;
    let ramp_down_mw_per_min = device.p_ramp_down_ub * base_mva / 60.0;
    if ramp_up_mw_per_min > 0.0 || ramp_down_mw_per_min > 0.0 {
        let ramping = generator.ramping.get_or_insert_with(RampingParams::default);
        if ramp_up_mw_per_min > 0.0 {
            ramping.ramp_up_curve = vec![(0.0, ramp_up_mw_per_min)];
        }
        if ramp_down_mw_per_min > 0.0 {
            ramping.ramp_down_curve = vec![(0.0, ramp_down_mw_per_min)];
        }
    }

    // Startup cost tiers → market.energy_offer.submitted.startup_tiers.
    let tiers = collect_startup_tiers(device);
    if !tiers.is_empty() {
        let market = generator.market.get_or_insert_with(MarketParams::default);
        match &mut market.energy_offer {
            Some(eo) => eo.submitted.startup_tiers = tiers,
            None => {
                market.energy_offer = Some(EnergyOffer {
                    submitted: OfferCurve {
                        segments: Vec::new(),
                        no_load_cost: 0.0,
                        startup_tiers: tiers,
                    },
                    mitigated: None,
                    mitigation_active: false,
                });
            }
        }
    }
}

fn collect_startup_tiers(device: &GoC3Device) -> Vec<StartupTier> {
    // GO C3 `startup_states` is [[cost_adjustment, max_offline_hours], ...]
    // with no sync_time. The Python adapter turns this into a list of
    // (max_offline_hours, startup_cost + cost_adjustment, 0.0) tuples sorted
    // ascending by max_offline_hours. Mirrors adapter.py::_startup_tiers.
    if device.startup_states.is_empty() {
        return Vec::new();
    }
    let mut tiers: Vec<StartupTier> = device
        .startup_states
        .iter()
        .filter_map(|pair| {
            if pair.len() < 2 {
                return None;
            }
            let cost_adjustment = pair[0];
            let max_offline_hours = pair[1];
            Some(StartupTier {
                max_offline_hours,
                cost: device.startup_cost + cost_adjustment,
                sync_time_min: 0.0,
            })
        })
        .collect();
    tiers.sort_by(|a, b| {
        a.max_offline_hours
            .partial_cmp(&b.max_offline_hours)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    tiers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::go_c3;
    use std::path::PathBuf;

    /// Path to the canonical 73-bus D2 scenario 911 used for enrich parity
    /// checks. The file is ~1.8 MB and lives in the external `surge-bench`
    /// data tree, so tests skip gracefully when it is not available.
    fn canonical_73bus_d2_911() -> Option<PathBuf> {
        // Honour SURGE_TEST_DATA override first (matches CLAUDE.md convention).
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
    fn enrich_populates_generator_envelope_and_commitment() {
        let Some(path) = canonical_73bus_d2_911() else {
            eprintln!(
                "skipping enrich_populates_generator_envelope_and_commitment: \
                 73-bus D2/911 scenario fixture not present"
            );
            return;
        };
        let problem = go_c3::load_problem(&path).expect("load problem");
        let (mut network, mut context) = go_c3::to_network(&problem).expect("to_network");
        go_c3::enrich_network(&mut network, &mut context, &problem, &GoC3Policy::default())
            .expect("enrich");

        // Producer sd_051 on bus_02:
        //   p_ub static envelope = 0.55 pu → pmax = 55 MW
        //   p_lb static envelope = 0.22 pu → pmin = 22 MW
        //   q_ub peak             = 0.19 pu → qmax = 19 MVAr
        //   q_lb floor            = -0.15 pu → qmin = -15 MVAr
        //   in_service_time_lb = down_time_lb = 2.2 hr
        //   p_ramp_up_ub = p_ramp_down_ub = 0.55 pu/hr → 0.55 × 100 / 60 ≈ 0.9167 MW/min
        //   startup_states = [[0, 24]] + startup_cost ≈ 5665.23
        //   p_nsyn_res_ub > 0 → quick_start flag
        let unit = network
            .generators
            .iter()
            .find(|g| g.id == "sd_051")
            .expect("sd_051 generator missing");
        assert!(
            (unit.pmax - 55.0).abs() < 1e-6,
            "pmax={} expected 55",
            unit.pmax
        );
        assert!(
            (unit.pmin - 22.0).abs() < 1e-6,
            "pmin={} expected 22",
            unit.pmin
        );
        assert!(
            (unit.qmax - 19.0).abs() < 1e-6,
            "qmax={} expected 19",
            unit.qmax
        );
        assert!(
            (unit.qmin + 15.0).abs() < 1e-6,
            "qmin={} expected -15",
            unit.qmin
        );
        assert!(
            unit.quick_start,
            "sd_051 should be quick_start (p_nsyn_res_ub > 0)"
        );

        let commitment = unit
            .commitment
            .as_ref()
            .expect("sd_051 commitment not populated");
        assert_eq!(commitment.min_up_time_hr, Some(2.2));
        assert_eq!(commitment.min_down_time_hr, Some(2.2));
        let expected_ramp = 0.55 * 100.0 / 60.0;
        assert!(
            commitment
                .startup_ramp_mw_per_min
                .map(|v| (v - expected_ramp).abs() < 1e-9)
                .unwrap_or(false),
            "startup_ramp_mw_per_min={:?} expected ~{expected_ramp}",
            commitment.startup_ramp_mw_per_min
        );
        assert!(
            commitment
                .shutdown_ramp_mw_per_min
                .map(|v| (v - expected_ramp).abs() < 1e-9)
                .unwrap_or(false),
        );

        let ramping = unit.ramping.as_ref().expect("sd_051 ramping not populated");
        assert_eq!(ramping.ramp_up_curve.len(), 1);
        assert!((ramping.ramp_up_curve[0].1 - expected_ramp).abs() < 1e-9);
        assert_eq!(ramping.ramp_down_curve.len(), 1);
        assert!((ramping.ramp_down_curve[0].1 - expected_ramp).abs() < 1e-9);

        let market = unit.market.as_ref().expect("sd_051 market not populated");
        let offer = market
            .energy_offer
            .as_ref()
            .expect("sd_051 energy_offer not populated");
        let tiers = &offer.submitted.startup_tiers;
        assert_eq!(tiers.len(), 1);
        assert!((tiers[0].max_offline_hours - 24.0).abs() < 1e-9);
        assert!((tiers[0].cost - 5665.234428).abs() < 1e-6);
        assert!(tiers[0].sync_time_min.abs() < 1e-12);
    }

    #[test]
    fn enrich_infers_slack_bus_by_reactive_capability() {
        let Some(path) = canonical_73bus_d2_911() else {
            eprintln!(
                "skipping enrich_infers_slack_bus_by_reactive_capability: \
                 73-bus D2/911 scenario fixture not present"
            );
            return;
        };
        let problem = go_c3::load_problem(&path).expect("load problem");
        let (mut network, mut context) = go_c3::to_network(&problem).expect("to_network");
        go_c3::enrich_network(&mut network, &mut context, &problem, &GoC3Policy::default())
            .expect("enrich");

        // Scenario 911 has no explicit Slack bus. The Python heuristic
        // selects bus_06 (hosts sd_110: peak p_ub=4.0 pu=400 MW with
        // q_range 2.5 pu=250 MVAr) which wins on (peak_p, q_range).
        let slack_buses: Vec<&str> = network
            .buses
            .iter()
            .filter(|b| b.bus_type == BusType::Slack)
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(slack_buses, vec!["bus_06"]);
    }

    #[test]
    fn enrich_tracks_shunt_initial_steps() {
        let Some(path) = canonical_73bus_d2_911() else {
            eprintln!(
                "skipping enrich_tracks_shunt_initial_steps: \
                 73-bus D2/911 scenario fixture not present"
            );
            return;
        };
        let problem = go_c3::load_problem(&path).expect("load problem");
        let (mut network, mut context) = go_c3::to_network(&problem).expect("to_network");
        go_c3::enrich_network(&mut network, &mut context, &problem, &GoC3Policy::default())
            .expect("enrich");

        assert_eq!(
            context.shunt_initial_steps.len(),
            problem.network.shunt.len(),
            "shunt_initial_steps should have one entry per shunt"
        );
        for shunt in &problem.network.shunt {
            let tracked = context
                .shunt_initial_steps
                .get(&shunt.uid)
                .copied()
                .unwrap_or(-999);
            assert_eq!(
                tracked, shunt.initial_status.step,
                "shunt {} initial step mismatch",
                shunt.uid
            );
        }
    }

    #[test]
    fn enrich_respects_explicit_slack_bus_labels() {
        // Build a synthetic mini-problem with one bus explicitly marked Slack.
        let json = r#"{
            "network": {
                "general": {"base_norm_mva": 100.0},
                "bus": [
                    {"uid": "b1", "base_nom_volt": 230.0, "vm_lb": 0.95, "vm_ub": 1.05,
                     "initial_status": {"vm": 1.0, "va": 0.0}, "type": "PQ"},
                    {"uid": "b2", "base_nom_volt": 230.0, "vm_lb": 0.95, "vm_ub": 1.05,
                     "initial_status": {"vm": 1.0, "va": 0.0}, "type": "Slack"}
                ],
                "simple_dispatchable_device": [],
                "ac_line": [],
                "two_winding_transformer": [],
                "dc_line": [],
                "shunt": []
            },
            "time_series_input": {
                "general": {"time_periods": 1, "interval_duration": [1.0]},
                "simple_dispatchable_device": []
            },
            "reliability": {"contingency": []}
        }"#;
        let problem = go_c3::load_problem_str(json).expect("parse");
        let (mut network, mut context) = go_c3::to_network(&problem).expect("to_network");
        go_c3::enrich_network(&mut network, &mut context, &problem, &GoC3Policy::default())
            .expect("enrich");
        // Only b2 should be Slack even though ReactiveCapability mode is the default.
        let slack_names: Vec<&str> = network
            .buses
            .iter()
            .filter(|b| b.bus_type == BusType::Slack)
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(slack_names, vec!["b2"]);
    }
}
