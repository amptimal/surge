// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Voltage regulation and slack-bus fallback for GO C3 networks.
//!
//! This pass replicates the Python pipeline from `markets/go_c3/adapter.py`:
//! `_apply_ac_reactive_support_qualification`, per-producer voltage regulation
//! decisions, and `_ensure_voltage_control_fallback`.
//!
//! Responsibilities:
//!
//! 1. **Per-producer AC reactive support qualification** — devices with
//!    zero p_lb, nonzero reactive range, and zero on/startup cost are
//!    flagged with `ac_reactive_support_flexible = true` in the
//!    generator's `MarketParams::qualifications` map so the AC OPF can
//!    use them as reactive-only support resources.
//!
//! 2. **Per-producer voltage regulation flag** — when the policy preserves
//!    AC voltage controls and the device has reactive regulation range
//!    plus either an explicit `vm_setpoint` or a PV/Slack bus type, the
//!    generator is marked `voltage_regulated = true` and its resource ID
//!    is recorded in
//!    [`GoC3Context::explicit_voltage_regulating_resource_ids`].
//!
//! 3. **Connected-components voltage fallback** — Python's
//!    `_ensure_voltage_control_fallback`. Walks the in-service bus-branch
//!    graph, and for each connected component picks a single Slack bus
//!    (preferring buses that host already-preferred generators, else the
//!    candidate bus with the largest pmax). Marks candidate generators in
//!    that component as voltage-regulating. This guarantees that
//!    `Network::validate_for_solve()` passes even when the policy
//!    otherwise disables voltage controls.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::network::{BusType, Generator};

use super::Error;
use super::context::GoC3Context;
use super::policy::GoC3Policy;
use super::types::*;

// (The AC reactive-support qualification flag would normally be set here,
// but Python's rich-object write doesn't persist, so we silently drop it
// for parity — see `apply_per_producer_voltage_flags` below.)
/// Qualification flag name for generators that must be excluded from the
/// voltage-regulation fallback sweep (e.g. DC line reactive-support
/// producers added by `hvdc_q.rs`).
const AC_VOLTAGE_REGULATION_EXCLUDED: &str = "ac_voltage_regulation_excluded";

/// Apply the voltage-regulation policy to the network.
///
/// Runs the three passes described in the module docstring in order. Idempotent.
pub fn apply_voltage_regulation(
    network: &mut Network,
    context: &mut GoC3Context,
    problem: &GoC3Problem,
    policy: &GoC3Policy,
) -> Result<(), Error> {
    apply_per_producer_voltage_flags(network, context, problem, policy)?;
    apply_voltage_control_fallback(network, context, policy)?;
    Ok(())
}

// ─── Per-producer pass ──────────────────────────────────────────────────────

fn apply_per_producer_voltage_flags(
    network: &mut Network,
    context: &mut GoC3Context,
    problem: &GoC3Problem,
    policy: &GoC3Policy,
) -> Result<(), Error> {
    let preserve = policy.preserve_ac_voltage_controls();

    let device_ts_by_uid: HashMap<&str, &GoC3DeviceTimeSeries> = problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();
    let devices_by_uid: HashMap<&str, &GoC3Device> = problem
        .network
        .simple_dispatchable_device
        .iter()
        .map(|d| (d.uid.as_str(), d))
        .collect();
    let buses_by_uid: HashMap<&str, &GoC3Bus> = problem
        .network
        .bus
        .iter()
        .map(|b| (b.uid.as_str(), b))
        .collect();

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

        // 1. AC reactive support qualification.
        //
        // Python's `_apply_ac_reactive_support_qualification` SETS this
        // flag on the pyo3 rich-object `generator.qualifications` dict,
        // but that dict is a serde COPY — writes don't round-trip back
        // to the underlying Rust Generator. The Python-built network
        // therefore ships with empty qualifications on every producer.
        // We reproduce that silent no-op to keep the Rust build-path
        // output byte-identical to Python's.
        //
        // If the Python adapter ever fixes its rich-object writeback,
        // restore the explicit flag here.
        let _ = (qualifies_as_flexible_reactive_support(device, ts), ts);

        // 2. Per-producer voltage regulation decision.
        let has_q_range = q_range_mvar(ts) > 1e-9;
        let is_zero_mw_producer =
            ts.p_ub.iter().all(|v| v.abs() <= 1e-9) && ts.p_lb.iter().all(|v| v.abs() <= 1e-9);
        let bus = buses_by_uid.get(device.bus.as_str());
        let bus_type = bus.and_then(|b| b.bus_type.as_deref());
        let explicit_vm_setpoint = device.vm_setpoint;
        let default_vm_setpoint = bus.map(|b| b.initial_status.vm).unwrap_or(1.0);
        let bus_voltage_control_explicit = matches!(bus_type, Some("PV") | Some("Slack"));

        let can_regulate_voltage = preserve
            && has_q_range
            && (explicit_vm_setpoint.is_some()
                || bus_voltage_control_explicit
                || is_zero_mw_producer);

        let target_vm_setpoint = if preserve && has_q_range {
            explicit_vm_setpoint.unwrap_or(default_vm_setpoint)
        } else {
            generator.voltage_setpoint_pu
        };

        if can_regulate_voltage {
            context
                .explicit_voltage_regulating_resource_ids
                .insert(generator.id.clone());
            if explicit_vm_setpoint.is_some() {
                context
                    .go_explicit_voltage_regulating_resource_ids
                    .insert(generator.id.clone());
            }
        }

        generator.voltage_setpoint_pu = target_vm_setpoint;
        generator.voltage_regulated = can_regulate_voltage;
        generator.reg_bus = if can_regulate_voltage {
            Some(generator.bus)
        } else {
            None
        };
    }

    Ok(())
}

/// Python `_apply_ac_reactive_support_qualification`. A device qualifies
/// when its minimum `p_lb` is <= 0, it has nonzero reactive capability,
/// and it has zero fixed on/startup cost.
fn qualifies_as_flexible_reactive_support(device: &GoC3Device, ts: &GoC3DeviceTimeSeries) -> bool {
    let min_p_lb = ts.p_lb.iter().copied().fold(f64::INFINITY, f64::min);
    if min_p_lb.is_infinite() {
        return false;
    }
    if min_p_lb > 1e-9 {
        return false;
    }
    let reactive_capability = ts
        .q_lb
        .iter()
        .chain(ts.q_ub.iter())
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);
    if reactive_capability <= 1e-9 {
        return false;
    }
    device.on_cost.abs() <= 1e-9 && device.startup_cost.abs() <= 1e-9
}

fn q_range_mvar(ts: &GoC3DeviceTimeSeries) -> f64 {
    if ts.q_lb.is_empty() && ts.q_ub.is_empty() {
        return 0.0;
    }
    let q_min = ts.q_lb.iter().copied().fold(f64::INFINITY, f64::min);
    let q_max = ts.q_ub.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if q_min.is_finite() && q_max.is_finite() {
        q_max - q_min
    } else {
        0.0
    }
}

// ─── Connected-components fallback ──────────────────────────────────────────

/// Python `_ensure_voltage_control_fallback` — guarantees at least one
/// voltage-regulating generator per connected component of the
/// in-service bus-branch graph.
fn apply_voltage_control_fallback(
    network: &mut Network,
    context: &mut GoC3Context,
    policy: &GoC3Policy,
) -> Result<(), Error> {
    if network.buses.is_empty() || network.generators.is_empty() {
        return Ok(());
    }

    let bus_numbers: HashSet<u32> = network.buses.iter().map(|b| b.number).collect();
    let is_ac_formulation = policy.formulation == super::policy::GoC3Formulation::Ac;
    let debug_slack = std::env::var("SURGE_GO_C3_DEBUG_SLACK").ok().as_deref() == Some("1");
    if debug_slack {
        eprintln!(
            "go_c3_voltage_fallback start slack={:?}",
            context.slack_bus_numbers
        );
    }

    // Build adjacency from in-service branches.
    let mut adjacency: HashMap<u32, HashSet<u32>> = bus_numbers
        .iter()
        .copied()
        .map(|n| (n, HashSet::new()))
        .collect();
    for branch in &network.branches {
        if !branch.in_service {
            continue;
        }
        let from = branch.from_bus;
        let to = branch.to_bus;
        if from == to {
            continue;
        }
        if bus_numbers.contains(&from) && bus_numbers.contains(&to) {
            adjacency.get_mut(&from).unwrap().insert(to);
            adjacency.get_mut(&to).unwrap().insert(from);
        }
    }

    // Classify generators: candidates (have q range, not excluded), plus
    // preferred subset (already in explicit_voltage_regulating_resource_ids).
    let excluded: HashSet<String> = collect_excluded_resource_ids(network);

    // Index generators by bus number.
    let mut candidates_by_bus: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut preferred_by_bus: HashMap<u32, Vec<usize>> = HashMap::new();
    for (idx, generator) in network.generators.iter().enumerate() {
        if generator.id.trim().is_empty() || !generator.in_service {
            continue;
        }
        if excluded.contains(&generator.id) {
            continue;
        }
        if !bus_numbers.contains(&generator.bus) {
            continue;
        }
        if !generator_has_reactive_regulation_range(generator) {
            continue;
        }
        // In DC formulations, skip zero-MW reactive-only devices.
        if !is_ac_formulation && generator.pmax <= 0.0 {
            continue;
        }
        candidates_by_bus
            .entry(generator.bus)
            .or_default()
            .push(idx);
        if context
            .explicit_voltage_regulating_resource_ids
            .contains(&generator.id)
        {
            preferred_by_bus.entry(generator.bus).or_default().push(idx);
        }
    }

    if candidates_by_bus.is_empty() {
        return Ok(());
    }

    // Walk connected components.
    let mut updated_slack_buses: HashSet<u32> = context.slack_bus_numbers.iter().copied().collect();
    let mut visited: HashSet<u32> = HashSet::new();
    let mut sorted_buses: Vec<u32> = bus_numbers.iter().copied().collect();
    sorted_buses.sort_unstable();

    // Map from generator index → bus_number override to apply after the
    // mutable loop (so we don't mutate while holding refs into the vec).
    let mut bus_type_updates: Vec<(u32, BusType)> = Vec::new();
    let mut generator_updates: Vec<GeneratorUpdate> = Vec::new();

    for start_bus in sorted_buses {
        if visited.contains(&start_bus) {
            continue;
        }
        let mut component = Vec::new();
        let mut stack = vec![start_bus];
        while let Some(current) = stack.pop() {
            if !visited.insert(current) {
                continue;
            }
            component.push(current);
            if let Some(neighbours) = adjacency.get(&current) {
                for &n in neighbours {
                    if !visited.contains(&n) {
                        stack.push(n);
                    }
                }
            }
        }

        let component_candidate_buses: Vec<u32> = component
            .iter()
            .copied()
            .filter(|n| candidates_by_bus.contains_key(n))
            .collect();
        if component_candidate_buses.is_empty() {
            continue;
        }

        let component_preferred_buses: Vec<u32> = component
            .iter()
            .copied()
            .filter(|n| preferred_by_bus.contains_key(n))
            .collect();

        if !component_preferred_buses.is_empty() {
            // Preserve any existing slack bus in the component as long as it
            // still hosts a reactive-capable candidate. Explicit support
            // regulators should add PV buses, not silently steal the slack
            // role from the original large-machine reference.
            let slack_candidates: Vec<u32> = component_candidate_buses
                .iter()
                .copied()
                .filter(|n| updated_slack_buses.contains(n))
                .collect();
            let chosen_bus = if !slack_candidates.is_empty() {
                pick_max_pmax_bus(&slack_candidates, &candidates_by_bus, &network.generators)
            } else {
                pick_max_pmax_bus(
                    &component_preferred_buses,
                    &candidates_by_bus,
                    &network.generators,
                )
            };

            for &bus_number in &component {
                let bt = if bus_number == chosen_bus {
                    BusType::Slack
                } else if component_preferred_buses.contains(&bus_number) {
                    BusType::PV
                } else {
                    BusType::PQ
                };
                bus_type_updates.push((bus_number, bt));
            }
            for &bus_number in &component {
                updated_slack_buses.remove(&bus_number);
            }
            updated_slack_buses.insert(chosen_bus);
            if debug_slack {
                eprintln!(
                    "go_c3_voltage_fallback preferred component={:?} chosen={} preferred={:?}",
                    component, chosen_bus, component_preferred_buses
                );
            }

            for &bus_number in &component_preferred_buses {
                if let Some(indices) = preferred_by_bus.get(&bus_number) {
                    for &idx in indices {
                        generator_updates.push(GeneratorUpdate {
                            index: idx,
                            voltage_regulated: true,
                            reg_bus: Some(bus_number),
                            setpoint_pu: Some(network_bus_voltage_target_pu(
                                &network.buses,
                                bus_number,
                                1.0,
                            )),
                        });
                    }
                }
            }
            if let Some(indices) = candidates_by_bus.get(&chosen_bus) {
                let target_vm = network_bus_voltage_target_pu(&network.buses, chosen_bus, 1.0);
                for &idx in indices {
                    generator_updates.push(GeneratorUpdate {
                        index: idx,
                        voltage_regulated: true,
                        reg_bus: Some(chosen_bus),
                        setpoint_pu: Some(target_vm),
                    });
                }
            }
            continue;
        }

        // No preferred generators in this component — fall back to any
        // candidate bus.
        let slack_candidates: Vec<u32> = component_candidate_buses
            .iter()
            .copied()
            .filter(|n| updated_slack_buses.contains(n))
            .collect();
        let chosen_bus = if !slack_candidates.is_empty() {
            pick_max_pmax_bus(&slack_candidates, &candidates_by_bus, &network.generators)
        } else {
            pick_max_pmax_bus(
                &component_candidate_buses,
                &candidates_by_bus,
                &network.generators,
            )
        };

        for &bus_number in &component {
            let bt = if bus_number == chosen_bus {
                BusType::Slack
            } else if component_candidate_buses.contains(&bus_number) {
                BusType::PV
            } else {
                BusType::PQ
            };
            bus_type_updates.push((bus_number, bt));
        }
        for &bus_number in &component {
            updated_slack_buses.remove(&bus_number);
        }
        updated_slack_buses.insert(chosen_bus);
        if debug_slack {
            eprintln!(
                "go_c3_voltage_fallback generic component={:?} chosen={} candidates={:?}",
                component, chosen_bus, component_candidate_buses
            );
        }

        for &bus_number in &component_candidate_buses {
            let target_vm = network_bus_voltage_target_pu(&network.buses, bus_number, 1.0);
            if let Some(indices) = candidates_by_bus.get(&bus_number) {
                for &idx in indices {
                    generator_updates.push(GeneratorUpdate {
                        index: idx,
                        voltage_regulated: true,
                        reg_bus: Some(bus_number),
                        setpoint_pu: Some(target_vm),
                    });
                }
            }
        }
    }

    // Apply all accumulated updates.
    for (bus_number, bt) in bus_type_updates {
        if let Some(bus) = network.buses.iter_mut().find(|b| b.number == bus_number) {
            bus.bus_type = bt;
        }
    }
    for upd in generator_updates {
        if let Some(generator) = network.generators.get_mut(upd.index) {
            generator.voltage_regulated = upd.voltage_regulated;
            generator.reg_bus = upd.reg_bus;
            if let Some(vm) = upd.setpoint_pu {
                generator.voltage_setpoint_pu = vm;
            }
        }
    }

    let mut slack_list: Vec<u32> = updated_slack_buses.into_iter().collect();
    slack_list.sort_unstable();
    context.slack_bus_numbers = slack_list;
    if debug_slack {
        eprintln!(
            "go_c3_voltage_fallback final slack={:?}",
            context.slack_bus_numbers
        );
    }

    Ok(())
}

struct GeneratorUpdate {
    index: usize,
    voltage_regulated: bool,
    reg_bus: Option<u32>,
    setpoint_pu: Option<f64>,
}

fn collect_excluded_resource_ids(network: &Network) -> HashSet<String> {
    let mut excluded = HashSet::new();
    for generator in &network.generators {
        let Some(market) = generator.market.as_ref() else {
            continue;
        };
        if market
            .qualifications
            .get(AC_VOLTAGE_REGULATION_EXCLUDED)
            .copied()
            .unwrap_or(false)
        {
            excluded.insert(generator.id.clone());
        }
    }
    excluded
}

fn generator_has_reactive_regulation_range(generator: &Generator) -> bool {
    generator.qmax > generator.qmin + 1e-9
}

fn pick_max_pmax_bus(
    buses: &[u32],
    candidates_by_bus: &HashMap<u32, Vec<usize>>,
    generators: &[Generator],
) -> u32 {
    *buses
        .iter()
        .max_by(|a, b| {
            let a_max = candidates_by_bus
                .get(a)
                .map(|ids| max_pmax(ids, generators))
                .unwrap_or(0.0);
            let b_max = candidates_by_bus
                .get(b)
                .map(|ids| max_pmax(ids, generators))
                .unwrap_or(0.0);
            a_max
                .partial_cmp(&b_max)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.cmp(b))
        })
        .expect("pick_max_pmax_bus called with empty slice")
}

fn max_pmax(indices: &[usize], generators: &[Generator]) -> f64 {
    indices
        .iter()
        .filter_map(|&idx| generators.get(idx))
        .map(|g| g.pmax)
        .fold(f64::NEG_INFINITY, f64::max)
}

fn network_bus_voltage_target_pu(
    buses: &[surge_network::network::Bus],
    bus_number: u32,
    default: f64,
) -> f64 {
    buses
        .iter()
        .find(|b| b.number == bus_number)
        .map(|b| {
            if b.voltage_magnitude_pu > 1e-9 {
                b.voltage_magnitude_pu
            } else {
                default
            }
        })
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::go_c3;
    use crate::go_c3::GoC3Formulation;

    #[test]
    fn zero_mw_producer_is_marked_as_explicit_voltage_regulator() {
        let json = r#"{
            "network": {
                "general": {"base_norm_mva": 100.0},
                "bus": [
                    {
                        "uid": "b1",
                        "base_nom_volt": 230.0,
                        "vm_lb": 0.95,
                        "vm_ub": 1.05,
                        "initial_status": {"vm": 1.01, "va": 0.0},
                        "type": "PQ"
                    }
                ],
                "simple_dispatchable_device": [
                    {
                        "uid": "gq",
                        "bus": "b1",
                        "device_type": "producer",
                        "on_cost": 0.0,
                        "startup_cost": 0.0,
                        "startup_states": [],
                        "shutdown_cost": 0.0,
                        "startups_ub": [],
                        "energy_req_ub": [],
                        "energy_req_lb": [],
                        "in_service_time_lb": 0.0,
                        "down_time_lb": 0.0,
                        "p_ramp_up_ub": 1.0,
                        "p_ramp_down_ub": 1.0,
                        "p_startup_ramp_ub": 1.0,
                        "p_shutdown_ramp_ub": 1.0,
                        "p_reg_res_up_ub": 0.0,
                        "p_reg_res_down_ub": 0.0,
                        "p_syn_res_ub": 0.0,
                        "p_nsyn_res_ub": 0.0,
                        "p_ramp_res_up_online_ub": 0.0,
                        "p_ramp_res_down_online_ub": 0.0,
                        "p_ramp_res_up_offline_ub": 0.0,
                        "p_ramp_res_down_offline_ub": 0.0,
                        "q_linear_cap": 0.0,
                        "q_bound_cap": 0.0,
                        "initial_status": {
                            "on_status": 0,
                            "p": 0.0,
                            "q": 0.1,
                            "accu_down_time": 5.0,
                            "accu_up_time": 0.0
                        }
                    }
                ],
                "ac_line": [],
                "two_winding_transformer": [],
                "dc_line": [],
                "shunt": []
            },
            "time_series_input": {
                "general": {"time_periods": 1, "interval_duration": [1.0]},
                "simple_dispatchable_device": [
                    {
                        "uid": "gq",
                        "on_status_ub": [1],
                        "on_status_lb": [1],
                        "p_ub": [0.0],
                        "p_lb": [0.0],
                        "q_ub": [0.3],
                        "q_lb": [-0.3],
                        "cost": [[[0.0, 0.0]]]
                    }
                ]
            },
            "reliability": {"contingency": []}
        }"#;
        let problem = go_c3::load_problem_str(json).expect("parse");
        let policy = GoC3Policy {
            formulation: GoC3Formulation::Ac,
            ..GoC3Policy::default()
        };
        let (mut network, mut context) =
            go_c3::to_network_with_policy(&problem, &policy).expect("to_network");
        go_c3::enrich_network(&mut network, &mut context, &problem, &policy).expect("enrich");
        apply_voltage_regulation(&mut network, &mut context, &problem, &policy)
            .expect("apply_voltage_regulation");

        assert!(
            context
                .explicit_voltage_regulating_resource_ids
                .contains("gq"),
            "zero-MW producer should be marked as an explicit voltage regulator"
        );
        let generator = network
            .generators
            .iter()
            .find(|generator| generator.id == "gq")
            .expect("generator gq present");
        assert!(generator.voltage_regulated);
        assert_eq!(generator.reg_bus, Some(generator.bus));
        assert!((generator.voltage_setpoint_pu - 1.01).abs() < 1e-9);
    }

    #[test]
    fn preferred_support_buses_do_not_steal_existing_slack_bus() {
        let json = r#"{
            "network": {
                "general": {"base_norm_mva": 100.0},
                "bus": [
                    {
                        "uid": "b1",
                        "base_nom_volt": 230.0,
                        "vm_lb": 0.95,
                        "vm_ub": 1.05,
                        "initial_status": {"vm": 1.01, "va": 0.0},
                        "type": "PQ"
                    },
                    {
                        "uid": "b2",
                        "base_nom_volt": 230.0,
                        "vm_lb": 0.95,
                        "vm_ub": 1.05,
                        "initial_status": {"vm": 1.03, "va": 0.0},
                        "type": "PQ"
                    }
                ],
                "simple_dispatchable_device": [
                    {
                        "uid": "g_big",
                        "bus": "b1",
                        "device_type": "producer",
                        "on_cost": 0.0,
                        "startup_cost": 0.0,
                        "startup_states": [],
                        "shutdown_cost": 0.0,
                        "startups_ub": [],
                        "energy_req_ub": [],
                        "energy_req_lb": [],
                        "in_service_time_lb": 0.0,
                        "down_time_lb": 0.0,
                        "p_ramp_up_ub": 4.0,
                        "p_ramp_down_ub": 4.0,
                        "p_startup_ramp_ub": 4.0,
                        "p_shutdown_ramp_ub": 4.0,
                        "p_reg_res_up_ub": 0.0,
                        "p_reg_res_down_ub": 0.0,
                        "p_syn_res_ub": 0.0,
                        "p_nsyn_res_ub": 0.0,
                        "p_ramp_res_up_online_ub": 0.0,
                        "p_ramp_res_down_online_ub": 0.0,
                        "p_ramp_res_up_offline_ub": 0.0,
                        "p_ramp_res_down_offline_ub": 0.0,
                        "q_linear_cap": 0.0,
                        "q_bound_cap": 0.0,
                        "initial_status": {
                            "on_status": 0,
                            "p": 0.0,
                            "q": 0.0,
                            "accu_down_time": 5.0,
                            "accu_up_time": 0.0
                        }
                    },
                    {
                        "uid": "g_support",
                        "bus": "b2",
                        "device_type": "producer",
                        "on_cost": 0.0,
                        "startup_cost": 0.0,
                        "startup_states": [],
                        "shutdown_cost": 0.0,
                        "startups_ub": [],
                        "energy_req_ub": [],
                        "energy_req_lb": [],
                        "in_service_time_lb": 0.0,
                        "down_time_lb": 0.0,
                        "p_ramp_up_ub": 1.0,
                        "p_ramp_down_ub": 1.0,
                        "p_startup_ramp_ub": 1.0,
                        "p_shutdown_ramp_ub": 1.0,
                        "p_reg_res_up_ub": 0.0,
                        "p_reg_res_down_ub": 0.0,
                        "p_syn_res_ub": 0.0,
                        "p_nsyn_res_ub": 0.0,
                        "p_ramp_res_up_online_ub": 0.0,
                        "p_ramp_res_down_online_ub": 0.0,
                        "p_ramp_res_up_offline_ub": 0.0,
                        "p_ramp_res_down_offline_ub": 0.0,
                        "q_linear_cap": 0.0,
                        "q_bound_cap": 0.0,
                        "initial_status": {
                            "on_status": 0,
                            "p": 0.0,
                            "q": 0.08,
                            "accu_down_time": 5.0,
                            "accu_up_time": 0.0
                        }
                    }
                ],
                "ac_line": [
                    {
                        "uid": "l12",
                        "fr_bus": "b1",
                        "to_bus": "b2",
                        "r": 0.01,
                        "x": 0.05,
                        "b": 0.001,
                        "mva_ub_nom": 1.0,
                        "mva_ub_sht": 1.0,
                        "mva_ub_em": 1.0,
                        "connection_cost": 0.0,
                        "disconnection_cost": 0.0,
                        "initial_status": {"on_status": 1},
                        "additional_shunt": 0
                    }
                ],
                "two_winding_transformer": [],
                "dc_line": [],
                "shunt": []
            },
            "time_series_input": {
                "general": {"time_periods": 1, "interval_duration": [1.0]},
                "simple_dispatchable_device": [
                    {
                        "uid": "g_big",
                        "on_status_ub": [1],
                        "on_status_lb": [1],
                        "p_ub": [4.0],
                        "p_lb": [0.0],
                        "q_ub": [1.25],
                        "q_lb": [-1.25],
                        "cost": [[[0.0, 4.0]]]
                    },
                    {
                        "uid": "g_support",
                        "on_status_ub": [1],
                        "on_status_lb": [1],
                        "p_ub": [0.0],
                        "p_lb": [0.0],
                        "q_ub": [0.3],
                        "q_lb": [-0.3],
                        "cost": [[[0.0, 0.0]]]
                    }
                ]
            },
            "reliability": {"contingency": []}
        }"#;
        let problem = go_c3::load_problem_str(json).expect("parse");
        let policy = GoC3Policy::default();
        let (mut network, mut context) =
            go_c3::to_network_with_policy(&problem, &policy).expect("to_network");
        go_c3::enrich_network(&mut network, &mut context, &problem, &policy).expect("enrich");
        apply_voltage_regulation(&mut network, &mut context, &problem, &policy)
            .expect("apply_voltage_regulation");

        assert_eq!(context.slack_bus_numbers, vec![1]);
        assert_eq!(network.buses[0].bus_type, BusType::Slack);
        assert_ne!(network.buses[1].bus_type, BusType::Slack);

        let big = network
            .generators
            .iter()
            .find(|generator| generator.id == "g_big")
            .expect("g_big present");
        assert!(big.voltage_regulated);
        assert_eq!(big.reg_bus, Some(1));
    }

    #[test]
    fn pick_max_pmax_bus_breaks_ties_deterministically() {
        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmax = 100.0;
        let mut g2 = Generator::new(2, 0.0, 1.0);
        g2.pmax = 100.0;
        let generators = vec![g1, g2];
        let candidates_by_bus = HashMap::from([(10_u32, vec![0_usize]), (20_u32, vec![1_usize])]);

        assert_eq!(
            pick_max_pmax_bus(&[10, 20], &candidates_by_bus, &generators),
            20
        );
    }
}
