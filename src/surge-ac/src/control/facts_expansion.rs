// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! FACTS device expansion for AC power flow.
//!
//! Converts `network.facts_devices` into concrete Branch modifications and
//! Generator additions before the Newton-Raphson solve. The original `Network`
//! is not mutated; a cloned, expanded copy is returned.
//!
//! | Mode | Device | Effect |
//! |------|--------|--------|
//! | 0 | Out of service | Skipped |
//! | 1 | TCSC (series impedance) | Subtract `linx` from branch.x between bus_from and bus_to |
//! | 2 | SVC / STATCOM (shunt) | Add PV Generator at bus_from with Q ∈ [−q_max, q_max] |
//! | 3 | UPFC | Both mode-2 shunt + mode-1 series |
//! | 4 | Series power control | Same as mode 1 (P_des used as soft target — not enforced here) |
//! | 5 | Impedance modulation | Same as mode 1 (direct reactance modification) |

use std::borrow::Cow;

use surge_network::Network;
use surge_network::network::FactsMode;
use surge_network::network::Generator;

/// Expand `network.facts_devices` into a modified `Network`.
///
/// Returns a [`Cow::Borrowed`] reference when no FACTS devices are present
/// (avoiding a clone on the common path), or a [`Cow::Owned`] modified network
/// with:
/// - SVC / STATCOM devices converted to PV generators (with Q limits only, Pg = 0).
/// - TCSC / series devices applied as direct reactance modifications to the
///   first matching branch between `bus_from` and `bus_to`.
///
/// Devices with `mode = OutOfService` are silently skipped.
/// If a series FACTS device references a bus pair with no matching in-service
/// branch, a warning is emitted and the device is skipped.
pub fn expand_facts(network: &Network) -> Cow<'_, Network> {
    if network.facts_devices.is_empty() {
        return Cow::Borrowed(network);
    }

    let mut expanded = network.clone();

    for facts in &network.facts_devices {
        match facts.mode {
            FactsMode::OutOfService => {
                // Silently skip out-of-service devices.
            }

            FactsMode::ShuntOnly => {
                // SVC or STATCOM: add a PV generator at bus_from.
                // Pg = 0 (no active power injection), Qmin/Qmax from SHMX.
                // Voltage setpoint from VSET.
                let shunt_gen =
                    make_shunt_generator(facts.bus_from, facts.voltage_setpoint_pu, facts.q_max);
                expanded.generators.push(shunt_gen);
            }

            FactsMode::SeriesOnly
            | FactsMode::SeriesPowerControl
            | FactsMode::ImpedanceModulation => {
                // TCSC or series impedance: subtract linx from the matching branch.
                apply_series_reactance(
                    &mut expanded,
                    facts.bus_from,
                    facts.bus_to,
                    facts.series_reactance_pu,
                    &facts.name,
                );
            }

            FactsMode::ShuntSeries => {
                // UPFC: both shunt (PV gen) and series (reactance mod).
                let shunt_gen =
                    make_shunt_generator(facts.bus_from, facts.voltage_setpoint_pu, facts.q_max);
                expanded.generators.push(shunt_gen);
                apply_series_reactance(
                    &mut expanded,
                    facts.bus_from,
                    facts.bus_to,
                    facts.series_reactance_pu,
                    &facts.name,
                );
            }
        }
    }

    // Clear FACTS devices from the expanded network so that downstream solvers
    // (e.g. solve_ac_pf_kernel's outer-loop FACTS control) don't double-count them.
    expanded.facts_devices.clear();

    Cow::Owned(expanded)
}

/// Build a PV generator for a shunt FACTS device (SVC / STATCOM).
fn make_shunt_generator(bus: u32, voltage_setpoint_pu: f64, q_max: f64) -> Generator {
    let mut shunt_gen = Generator::new(bus, 0.0, voltage_setpoint_pu);
    shunt_gen.qmax = q_max;
    shunt_gen.qmin = -q_max;
    shunt_gen.pmax = 0.0; // no active power injection
    shunt_gen.pmin = 0.0;
    shunt_gen
}

/// Subtract `linx` from the reactance of the first in-service branch between
/// `bus_from` and `bus_to` (in either direction).
///
/// Emits a `tracing::warn` if no matching branch is found and returns without
/// modifying the network.
fn apply_series_reactance(
    network: &mut Network,
    bus_from: u32,
    bus_to: u32,
    linx: f64,
    name: &str,
) {
    if bus_to == 0 {
        // No remote bus — series device with no target branch; skip.
        tracing::warn!(
            device = name,
            bus_from,
            "FACTS series device has bus_to = 0 (no remote bus); skipping reactance modification"
        );
        return;
    }

    for branch in network.branches.iter_mut() {
        if !branch.in_service {
            continue;
        }
        let endpoints_match = (branch.from_bus == bus_from && branch.to_bus == bus_to)
            || (branch.from_bus == bus_to && branch.to_bus == bus_from);
        if endpoints_match {
            branch.x -= linx;
            tracing::debug!(
                device = name,
                bus_from,
                bus_to,
                linx,
                new_x = branch.x,
                "FACTS series device applied: reactance modified"
            );
            return;
        }
    }

    tracing::warn!(
        device = name,
        bus_from,
        bus_to,
        linx,
        "FACTS series device: no in-service branch found between bus_from and bus_to; skipping"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::Branch;
    use surge_network::network::{Bus, BusType};
    use surge_network::network::{FactsDevice, FactsMode};

    fn make_3bus() -> Network {
        let mut net = Network::new("test");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses.push(Bus::new(3, BusType::PV, 138.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.02, 0.2, 0.0));
        net.generators.push(Generator::new(1, 100.0, 1.0));
        net
    }

    #[test]
    fn test_no_facts_devices_returns_clone() {
        let net = make_3bus();
        let expanded = expand_facts(&net);
        assert_eq!(expanded.generators.len(), net.generators.len());
        assert_eq!(expanded.branches.len(), net.branches.len());
    }

    #[test]
    fn test_svc_shunt_expansion_adds_generator() {
        let mut net = make_3bus();
        net.facts_devices.push(FactsDevice {
            name: "SVC1".into(),
            bus_from: 2,
            bus_to: 0,
            mode: FactsMode::ShuntOnly,
            p_setpoint_mw: 0.0,
            q_setpoint_mvar: 0.0,
            voltage_setpoint_pu: 1.02,
            q_max: 150.0,
            series_reactance_pu: 0.0,
            in_service: true,
            ..FactsDevice::default()
        });

        let expanded = expand_facts(&net);
        // One extra generator should have been added at bus 2
        assert_eq!(expanded.generators.len(), 2);
        let svc_gen = &expanded.generators[1];
        assert_eq!(svc_gen.bus, 2);
        assert!((svc_gen.voltage_setpoint_pu - 1.02).abs() < 1e-10);
        assert!((svc_gen.qmax - 150.0).abs() < 1e-10);
        assert!((svc_gen.qmin - (-150.0)).abs() < 1e-10);
        assert!((svc_gen.p).abs() < 1e-10);
    }

    #[test]
    fn test_tcsc_series_expansion_modifies_branch_x() {
        let mut net = make_3bus();
        net.facts_devices.push(FactsDevice {
            name: "TCSC1".into(),
            bus_from: 1,
            bus_to: 2,
            mode: FactsMode::SeriesOnly,
            p_setpoint_mw: 0.0,
            q_setpoint_mvar: 0.0,
            voltage_setpoint_pu: 1.0,
            q_max: 0.0,
            series_reactance_pu: 0.03, // reduce reactance by 0.03 pu
            in_service: true,
            ..FactsDevice::default()
        });

        let original_x = net.branches[0].x; // 0.1
        let expanded = expand_facts(&net);
        // Branch 0 (bus 1→2) reactance should be reduced by linx
        assert!((expanded.branches[0].x - (original_x - 0.03)).abs() < 1e-10);
        // Branch 1 (bus 2→3) should be unchanged
        assert!((expanded.branches[1].x - 0.2).abs() < 1e-10);
    }

    #[test]
    fn test_out_of_service_facts_skipped() {
        let mut net = make_3bus();
        net.facts_devices.push(FactsDevice {
            name: "OOS".into(),
            bus_from: 2,
            bus_to: 0,
            mode: FactsMode::OutOfService,
            p_setpoint_mw: 0.0,
            q_setpoint_mvar: 0.0,
            voltage_setpoint_pu: 1.0,
            q_max: 100.0,
            series_reactance_pu: 0.0,
            in_service: false,
            ..FactsDevice::default()
        });

        let expanded = expand_facts(&net);
        // No new generators, no branch changes
        assert_eq!(expanded.generators.len(), net.generators.len());
    }

    #[test]
    fn test_upfc_adds_generator_and_modifies_branch() {
        let mut net = make_3bus();
        net.facts_devices.push(FactsDevice {
            name: "UPFC1".into(),
            bus_from: 2,
            bus_to: 3,
            mode: FactsMode::ShuntSeries,
            p_setpoint_mw: 0.0,
            q_setpoint_mvar: 0.0,
            voltage_setpoint_pu: 1.01,
            q_max: 80.0,
            series_reactance_pu: 0.05,
            in_service: true,
            ..FactsDevice::default()
        });

        let expanded = expand_facts(&net);
        assert_eq!(expanded.generators.len(), 2); // one shunt generator added
        assert!((expanded.branches[1].x - (0.2 - 0.05)).abs() < 1e-10); // branch 2→3 modified
    }
}
