// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Apply OPF/SCED dispatch solutions back into a Network.
//!
//! These functions close the gap between the dispatch/OPF solvers and the
//! power flow / N-1 contingency analysis solvers. After solving OPF or SCED,
//! call one of these to stamp the optimal dispatch onto the network so that
//! a subsequent `solve_ac_pf`, `compute_n1`, or any other network analysis
//! operates at the dispatched operating point.

use std::collections::HashMap;

use thiserror::Error;

use surge_network::Network;

use crate::OpfSolution;

/// Error returned when applying solver outputs back into a [`Network`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApplySolutionError {
    #[error(
        "generator dispatch length mismatch: expected {expected} in-service generators, got {actual}"
    )]
    GeneratorDispatchLengthMismatch { expected: usize, actual: usize },
    #[error(
        "generator reactive dispatch length mismatch: expected {expected} in-service generators, got {actual}"
    )]
    GeneratorReactiveDispatchLengthMismatch { expected: usize, actual: usize },
    #[error(
        "generator bus mapping length mismatch: expected {expected} in-service generators, got {actual}"
    )]
    GeneratorBusMappingLengthMismatch { expected: usize, actual: usize },
    #[error(
        "generator machine-id mapping length mismatch: expected {expected} in-service generators, got {actual}"
    )]
    GeneratorMachineIdMappingLengthMismatch { expected: usize, actual: usize },
    #[error(
        "generator ID mapping length mismatch: expected {expected} in-service generators, got {actual}"
    )]
    GeneratorIdMappingLengthMismatch { expected: usize, actual: usize },
    #[error(
        "generator identity mismatch at OPF position {position}: expected ({expected_bus}, {expected_machine_id}), got ({actual_bus}, {actual_machine_id})"
    )]
    GeneratorIdentityMismatch {
        position: usize,
        expected_bus: u32,
        expected_machine_id: String,
        actual_bus: u32,
        actual_machine_id: String,
    },
    #[error(
        "ambiguous generator identity ({bus}, {machine_id}) in OPF replay; generator identities must be unique among in-service units"
    )]
    AmbiguousGeneratorIdentity { bus: u32, machine_id: String },
    #[error(
        "duplicate generator ID {generator_id} in OPF replay mapping; solution generator IDs must be unique"
    )]
    DuplicateGeneratorIdInSolution { generator_id: String },
    #[error(
        "generator ID mismatch at OPF position {position}: expected {expected_id}, got {actual_id}"
    )]
    GeneratorIdMismatch {
        position: usize,
        expected_id: String,
        actual_id: String,
    },
    #[error(
        "ambiguous generator ID {generator_id} in target network; generator IDs must be unique"
    )]
    AmbiguousGeneratorIdInNetwork { generator_id: String },
    #[error("bus voltage magnitude length mismatch: expected {expected} buses, got {actual}")]
    VoltageMagnitudeLengthMismatch { expected: usize, actual: usize },
    #[error("bus voltage angle length mismatch: expected {expected} buses, got {actual}")]
    VoltageAngleLengthMismatch { expected: usize, actual: usize },
    #[error(
        "bus voltage bus-number mapping length mismatch: expected {expected} buses, got {actual}"
    )]
    VoltageBusNumberLengthMismatch { expected: usize, actual: usize },
    #[error("bus number {bus_number} from the solution was not found in the target network")]
    VoltageBusNumberNotFound { bus_number: u32 },
    #[error("duplicate bus number {bus_number} in target network")]
    DuplicateBusNumberInNetwork { bus_number: u32 },
    #[error("duplicate bus number {bus_number} in solution bus-voltage mapping")]
    DuplicateBusNumberInSolution { bus_number: u32 },
}

fn in_service_generator_count(network: &Network) -> usize {
    network.generators.iter().filter(|g| g.in_service).count()
}

fn normalized_machine_id(machine_id: Option<&str>) -> &str {
    machine_id.unwrap_or("1")
}

fn canonical_in_service_generator_id_map(
    network: &Network,
) -> Result<HashMap<String, usize>, ApplySolutionError> {
    let mut canonical_network = Network::new("opf_replay_ids");
    let mut source_indices = Vec::new();

    for (idx, generator) in network.generators.iter().enumerate() {
        if !generator.in_service {
            continue;
        }
        canonical_network.generators.push(generator.clone());
        source_indices.push(idx);
    }
    canonical_network.canonicalize_generator_ids();

    let mut id_map = HashMap::new();
    for (canonical_generator, &source_idx) in canonical_network
        .generators
        .iter()
        .zip(source_indices.iter())
    {
        if id_map
            .insert(canonical_generator.id.clone(), source_idx)
            .is_some()
        {
            return Err(ApplySolutionError::AmbiguousGeneratorIdInNetwork {
                generator_id: canonical_generator.id.clone(),
            });
        }
    }
    Ok(id_map)
}

/// Apply an OPF solution's generator dispatch and (optionally) bus voltages
/// back into a [`Network`].
///
/// Only in-service generators are updated. When generator identity metadata is
/// present in the solution, dispatch is matched by `(bus, machine_id)` rather
/// than by the current generator array order. Bus voltages are stamped by
/// external bus number when either voltage vector is populated.
pub fn apply_opf_dispatch(
    network: &mut Network,
    sol: &OpfSolution,
) -> Result<(), ApplySolutionError> {
    let expected_generators = in_service_generator_count(network);
    if sol.generators.gen_p_mw.len() != expected_generators {
        return Err(ApplySolutionError::GeneratorDispatchLengthMismatch {
            expected: expected_generators,
            actual: sol.generators.gen_p_mw.len(),
        });
    }
    if !sol.generators.gen_q_mvar.is_empty()
        && sol.generators.gen_q_mvar.len() != expected_generators
    {
        return Err(
            ApplySolutionError::GeneratorReactiveDispatchLengthMismatch {
                expected: expected_generators,
                actual: sol.generators.gen_q_mvar.len(),
            },
        );
    }
    if !sol.generators.gen_bus_numbers.is_empty()
        && sol.generators.gen_bus_numbers.len() != expected_generators
    {
        return Err(ApplySolutionError::GeneratorBusMappingLengthMismatch {
            expected: expected_generators,
            actual: sol.generators.gen_bus_numbers.len(),
        });
    }
    if !sol.generators.gen_machine_ids.is_empty()
        && sol.generators.gen_machine_ids.len() != expected_generators
    {
        return Err(
            ApplySolutionError::GeneratorMachineIdMappingLengthMismatch {
                expected: expected_generators,
                actual: sol.generators.gen_machine_ids.len(),
            },
        );
    }
    if !sol.generators.gen_ids.is_empty() && sol.generators.gen_ids.len() != expected_generators {
        return Err(ApplySolutionError::GeneratorIdMappingLengthMismatch {
            expected: expected_generators,
            actual: sol.generators.gen_ids.len(),
        });
    }

    let mut generator_dispatch_applied = false;
    if !sol.generators.gen_ids.is_empty() {
        let network_indices = canonical_in_service_generator_id_map(network)?;
        let mut seen_solution_ids = HashMap::new();
        let mut pending_updates = Vec::with_capacity(sol.generators.gen_ids.len());
        for (j, generator_id) in sol.generators.gen_ids.iter().enumerate() {
            if seen_solution_ids.insert(generator_id.clone(), j).is_some() {
                return Err(ApplySolutionError::DuplicateGeneratorIdInSolution {
                    generator_id: generator_id.clone(),
                });
            }
            let Some(&generator_index) = network_indices.get(generator_id) else {
                if !sol.generators.gen_bus_numbers.is_empty()
                    && !sol.generators.gen_machine_ids.is_empty()
                {
                    pending_updates.clear();
                    break;
                }
                let actual_id = network
                    .generators
                    .iter()
                    .filter(|g| g.in_service)
                    .nth(j)
                    .map(|g| g.id.clone())
                    .unwrap_or_else(|| "<missing>".to_string());
                return Err(ApplySolutionError::GeneratorIdMismatch {
                    position: j,
                    expected_id: generator_id.clone(),
                    actual_id,
                });
            };
            pending_updates.push((generator_index, j));
        }

        if !pending_updates.is_empty() {
            for (generator_index, j) in pending_updates {
                let generator = &mut network.generators[generator_index];
                generator.p = sol.generators.gen_p_mw[j];
                if !sol.generators.gen_q_mvar.is_empty() {
                    generator.q = sol.generators.gen_q_mvar[j];
                }
            }
            generator_dispatch_applied = true;
        }
    }

    if !generator_dispatch_applied && !sol.generators.gen_bus_numbers.is_empty() {
        if sol.generators.gen_machine_ids.len() != expected_generators {
            return Err(
                ApplySolutionError::GeneratorMachineIdMappingLengthMismatch {
                    expected: expected_generators,
                    actual: sol.generators.gen_machine_ids.len(),
                },
            );
        }

        let mut generator_indices = HashMap::new();
        for (idx, generator) in network.generators.iter().enumerate() {
            if !generator.in_service {
                continue;
            }
            let key = (
                generator.bus,
                normalized_machine_id(generator.machine_id.as_deref()).to_string(),
            );
            if generator_indices.insert(key.clone(), idx).is_some() {
                return Err(ApplySolutionError::AmbiguousGeneratorIdentity {
                    bus: key.0,
                    machine_id: key.1,
                });
            }
        }

        for (j, (&expected_bus, expected_machine_id)) in sol
            .generators
            .gen_bus_numbers
            .iter()
            .zip(sol.generators.gen_machine_ids.iter())
            .enumerate()
        {
            let Some(&generator_index) =
                generator_indices.get(&(expected_bus, expected_machine_id.clone()))
            else {
                let actual = network
                    .generators
                    .iter()
                    .filter(|g| g.in_service)
                    .nth(j)
                    .map(|g| {
                        (
                            g.bus,
                            normalized_machine_id(g.machine_id.as_deref()).to_string(),
                        )
                    })
                    .unwrap_or((0, "<missing>".to_string()));
                return Err(ApplySolutionError::GeneratorIdentityMismatch {
                    position: j,
                    expected_bus,
                    expected_machine_id: expected_machine_id.clone(),
                    actual_bus: actual.0,
                    actual_machine_id: actual.1,
                });
            };

            let generator = &mut network.generators[generator_index];
            generator.p = sol.generators.gen_p_mw[j];
            if !sol.generators.gen_q_mvar.is_empty() {
                generator.q = sol.generators.gen_q_mvar[j];
            }
        }
        generator_dispatch_applied = true;
    }

    if !generator_dispatch_applied {
        let mut j = 0usize;
        for g in network.generators.iter_mut() {
            if !g.in_service {
                continue;
            }
            g.p = sol.generators.gen_p_mw[j];
            if !sol.generators.gen_q_mvar.is_empty() {
                g.q = sol.generators.gen_q_mvar[j];
            }
            j += 1;
        }
    }

    if !sol.power_flow.voltage_magnitude_pu.is_empty()
        || !sol.power_flow.voltage_angle_rad.is_empty()
    {
        apply_bus_voltages_by_bus_number(
            network,
            &sol.power_flow.bus_numbers,
            &sol.power_flow.voltage_magnitude_pu,
            &sol.power_flow.voltage_angle_rad,
        )?;
    }

    Ok(())
}

/// Apply a generator dispatch vector (MW, one per in-service generator) to
/// the network.
///
/// Use this with SCED/SCUC period results:
/// ```ignore
/// apply_dispatch_mw(network, &period.gen_p_mw)?;
/// ```
///
/// Only in-service generators are updated. Out-of-service generators are
/// skipped and do not consume an index from `gen_p_mw`.
pub fn apply_dispatch_mw(
    network: &mut Network,
    gen_p_mw: &[f64],
) -> Result<(), ApplySolutionError> {
    let expected_generators = in_service_generator_count(network);
    if gen_p_mw.len() != expected_generators {
        return Err(ApplySolutionError::GeneratorDispatchLengthMismatch {
            expected: expected_generators,
            actual: gen_p_mw.len(),
        });
    }

    let mut j = 0usize;
    for g in network.generators.iter_mut() {
        if !g.in_service {
            continue;
        }
        g.p = gen_p_mw[j];
        j += 1;
    }

    Ok(())
}

/// Stamp bus voltage magnitudes and angles onto the network by external bus number.
///
/// `bus_numbers`, `vm`, and `va` must all have one entry per bus in the solution.
/// Each `bus_numbers[i]` is matched against the target network and the
/// corresponding voltage values are stamped onto that bus.
pub fn apply_bus_voltages_by_bus_number(
    network: &mut Network,
    bus_numbers: &[u32],
    vm: &[f64],
    va: &[f64],
) -> Result<(), ApplySolutionError> {
    if bus_numbers.len() != network.buses.len() {
        return Err(ApplySolutionError::VoltageBusNumberLengthMismatch {
            expected: network.buses.len(),
            actual: bus_numbers.len(),
        });
    }
    if vm.len() != network.buses.len() {
        return Err(ApplySolutionError::VoltageMagnitudeLengthMismatch {
            expected: network.buses.len(),
            actual: vm.len(),
        });
    }
    if va.len() != network.buses.len() {
        return Err(ApplySolutionError::VoltageAngleLengthMismatch {
            expected: network.buses.len(),
            actual: va.len(),
        });
    }

    let mut bus_indices = HashMap::new();
    for (idx, bus) in network.buses.iter().enumerate() {
        if bus_indices.insert(bus.number, idx).is_some() {
            return Err(ApplySolutionError::DuplicateBusNumberInNetwork {
                bus_number: bus.number,
            });
        }
    }

    let mut seen_solution_bus_numbers = HashMap::new();
    for (i, &bus_number) in bus_numbers.iter().enumerate() {
        if seen_solution_bus_numbers.insert(bus_number, i).is_some() {
            return Err(ApplySolutionError::DuplicateBusNumberInSolution { bus_number });
        }
        let Some(&bus_idx) = bus_indices.get(&bus_number) else {
            return Err(ApplySolutionError::VoltageBusNumberNotFound { bus_number });
        };
        let bus = &mut network.buses[bus_idx];
        bus.voltage_magnitude_pu = vm[i];
        bus.voltage_angle_rad = va[i];
    }

    Ok(())
}

/// Stamp bus voltage magnitudes and angles onto the network in current bus order.
///
/// This low-level helper is positional: `vm` and `va` must be aligned with the
/// current `network.buses` array. Prefer [`apply_bus_voltages_by_bus_number`]
/// when replaying solved voltages back into a possibly re-ordered network.
pub fn apply_bus_voltages(
    network: &mut Network,
    vm: &[f64],
    va: &[f64],
) -> Result<(), ApplySolutionError> {
    if vm.len() != network.buses.len() {
        return Err(ApplySolutionError::VoltageMagnitudeLengthMismatch {
            expected: network.buses.len(),
            actual: vm.len(),
        });
    }
    if va.len() != network.buses.len() {
        return Err(ApplySolutionError::VoltageAngleLengthMismatch {
            expected: network.buses.len(),
            actual: va.len(),
        });
    }

    for (i, bus) in network.buses.iter_mut().enumerate() {
        bus.voltage_magnitude_pu = vm[i];
        bus.voltage_angle_rad = va[i];
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OpfGeneratorResults, PfSolution};
    use surge_network::network::{Bus, BusType, Generator};
    fn make_generator(bus: u32, in_service: bool) -> Generator {
        let mut g = Generator::new(bus, 0.0, 1.0);
        g.in_service = in_service;
        g
    }

    fn make_opf_solution(
        gen_p_mw: Vec<f64>,
        gen_q_mvar: Vec<f64>,
        voltage_magnitude_pu: Vec<f64>,
        voltage_angle_rad: Vec<f64>,
    ) -> OpfSolution {
        OpfSolution {
            power_flow: PfSolution {
                voltage_magnitude_pu,
                voltage_angle_rad,
                ..Default::default()
            },
            generators: OpfGeneratorResults {
                gen_p_mw,
                gen_q_mvar,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_apply_opf_dispatch_pg_qg() {
        let mut net = Network::new("test");
        let mut g0 = make_generator(1, true);
        g0.machine_id = Some("A".to_string());
        let mut g1 = make_generator(2, false);
        g1.machine_id = Some("B".to_string());
        let mut g2 = make_generator(3, true);
        g2.machine_id = Some("C".to_string());
        net.generators.push(g0);
        net.generators.push(g1);
        net.generators.push(g2);
        net.canonicalize_generator_ids();

        let mut sol = make_opf_solution(vec![100.0, 200.0], vec![30.0, 60.0], vec![], vec![]);
        sol.generators.gen_ids = vec![net.generators[0].id.clone(), net.generators[2].id.clone()];

        apply_opf_dispatch(&mut net, &sol).unwrap();

        assert_eq!(net.generators[0].p, 100.0);
        assert_eq!(net.generators[0].q, 30.0);
        assert_eq!(net.generators[1].p, 0.0);
        assert_eq!(net.generators[1].q, 0.0);
        assert_eq!(net.generators[2].p, 200.0);
        assert_eq!(net.generators[2].q, 60.0);
    }

    #[test]
    fn test_apply_dispatch_mw_skips_out_of_service() {
        let mut net = Network::new("test");
        net.generators.push(make_generator(1, true));
        net.generators.push(make_generator(2, false));
        net.generators.push(make_generator(3, true));

        apply_dispatch_mw(&mut net, &[150.0, 250.0]).unwrap();

        assert_eq!(net.generators[0].p, 150.0);
        assert_eq!(net.generators[1].p, 0.0);
        assert_eq!(net.generators[2].p, 250.0);
    }

    #[test]
    fn test_apply_dispatch_rejects_short_generator_vector() {
        let mut net = Network::new("test");
        net.generators.push(make_generator(1, true));
        net.generators.push(make_generator(2, true));

        let err = apply_dispatch_mw(&mut net, &[150.0]).unwrap_err();
        assert_eq!(
            err,
            ApplySolutionError::GeneratorDispatchLengthMismatch {
                expected: 2,
                actual: 1,
            }
        );
    }

    #[test]
    fn test_apply_bus_voltages_requires_full_vectors() {
        let mut net = Network::new("test");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));

        let err = apply_bus_voltages(&mut net, &[1.0], &[0.0, 0.1]).unwrap_err();
        assert_eq!(
            err,
            ApplySolutionError::VoltageMagnitudeLengthMismatch {
                expected: 2,
                actual: 1,
            }
        );
    }

    #[test]
    fn test_apply_bus_voltages_by_bus_number_matches_identity() {
        let mut net = Network::new("test");
        net.buses.push(Bus::new(20, BusType::Slack, 138.0));
        net.buses.push(Bus::new(10, BusType::PQ, 138.0));

        apply_bus_voltages_by_bus_number(&mut net, &[10, 20], &[1.02, 1.05], &[0.1, -0.2]).unwrap();

        assert_eq!(net.buses[0].number, 20);
        assert!((net.buses[0].voltage_magnitude_pu - 1.05).abs() < 1e-12);
        assert!((net.buses[0].voltage_angle_rad + 0.2).abs() < 1e-12);
        assert_eq!(net.buses[1].number, 10);
        assert!((net.buses[1].voltage_magnitude_pu - 1.02).abs() < 1e-12);
        assert!((net.buses[1].voltage_angle_rad - 0.1).abs() < 1e-12);
    }

    #[test]
    fn test_apply_opf_dispatch_requires_voltage_bus_numbers() {
        let mut net = Network::new("test");
        net.buses.push(Bus::new(10, BusType::Slack, 138.0));
        net.buses.push(Bus::new(20, BusType::PQ, 138.0));
        net.generators.push(make_generator(10, true));

        let sol = make_opf_solution(vec![100.0], vec![], vec![1.0, 0.98], vec![0.0, -0.1]);
        let err = apply_opf_dispatch(&mut net, &sol).unwrap_err();
        assert_eq!(
            err,
            ApplySolutionError::VoltageBusNumberLengthMismatch {
                expected: 2,
                actual: 0,
            }
        );
    }

    #[test]
    fn test_apply_opf_dispatch_matches_generators_by_identity() {
        let mut net = Network::new("test");
        let mut gen_a = make_generator(10, true);
        gen_a.machine_id = Some("A".to_string());
        let mut gen_b = make_generator(20, true);
        gen_b.machine_id = Some("B".to_string());
        net.generators.push(gen_b);
        net.generators.push(gen_a);
        net.canonicalize_generator_ids();

        let mut sol = make_opf_solution(vec![75.0, 125.0], vec![15.0, 25.0], vec![], vec![]);
        sol.generators.gen_ids = vec![net.generators[1].id.clone(), net.generators[0].id.clone()];

        apply_opf_dispatch(&mut net, &sol).unwrap();

        assert_eq!(net.generators[0].p, 125.0);
        assert_eq!(net.generators[0].q, 25.0);
        assert_eq!(net.generators[1].p, 75.0);
        assert_eq!(net.generators[1].q, 15.0);
    }

    #[test]
    fn test_apply_opf_dispatch_matches_bus_voltages_by_identity() {
        let mut net = Network::new("test");
        net.buses.push(Bus::new(20, BusType::Slack, 138.0));
        net.buses.push(Bus::new(10, BusType::PQ, 138.0));

        let mut sol = make_opf_solution(vec![], vec![], vec![1.02, 1.05], vec![0.1, -0.2]);
        sol.power_flow.bus_numbers = vec![10, 20];

        apply_opf_dispatch(&mut net, &sol).unwrap();

        assert!((net.buses[0].voltage_magnitude_pu - 1.05).abs() < 1e-12);
        assert!((net.buses[0].voltage_angle_rad + 0.2).abs() < 1e-12);
        assert!((net.buses[1].voltage_magnitude_pu - 1.02).abs() < 1e-12);
        assert!((net.buses[1].voltage_angle_rad - 0.1).abs() < 1e-12);
    }

    #[test]
    fn test_apply_opf_dispatch_rejects_identity_mismatch() {
        let mut net = Network::new("test");
        let mut generator = make_generator(10, true);
        generator.machine_id = Some("A".to_string());
        net.generators.push(generator);

        let mut sol = make_opf_solution(vec![75.0], vec![], vec![], vec![]);
        sol.generators.gen_ids = vec!["missing-generator".to_string()];

        let err = apply_opf_dispatch(&mut net, &sol).unwrap_err();
        assert_eq!(
            err,
            ApplySolutionError::GeneratorIdMismatch {
                position: 0,
                expected_id: "missing-generator".to_string(),
                actual_id: net.generators[0].id.clone(),
            }
        );
    }

    #[test]
    fn test_apply_opf_dispatch_rejects_duplicate_solution_generator_ids() {
        let mut net = Network::new("test");
        net.generators.push(make_generator(10, true));
        net.generators.push(make_generator(10, true));
        net.canonicalize_generator_ids();

        let mut sol = make_opf_solution(vec![75.0, 125.0], vec![], vec![], vec![]);
        sol.generators.gen_ids = vec![net.generators[0].id.clone(), net.generators[0].id.clone()];

        let err = apply_opf_dispatch(&mut net, &sol).unwrap_err();
        assert_eq!(
            err,
            ApplySolutionError::DuplicateGeneratorIdInSolution {
                generator_id: net.generators[0].id.clone(),
            }
        );
    }

    #[test]
    fn test_apply_opf_dispatch_rejects_partial_voltage_vectors() {
        let mut net = Network::new("test");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));

        let sol = make_opf_solution(vec![], vec![], vec![], vec![0.0, 0.1]);

        let err = apply_opf_dispatch(&mut net, &sol).unwrap_err();
        assert_eq!(
            err,
            ApplySolutionError::VoltageBusNumberLengthMismatch {
                expected: 2,
                actual: 0,
            }
        );
    }

    #[test]
    fn test_apply_opf_dispatch_matches_canonicalized_generator_ids() {
        let mut net = Network::new("test");
        let mut gen_a = make_generator(10, true);
        gen_a.machine_id = Some("A".to_string());
        gen_a.id = "  ".to_string();
        let mut gen_b = make_generator(20, true);
        gen_b.machine_id = Some("B".to_string());
        gen_b.id = "\t".to_string();
        net.generators.push(gen_b);
        net.generators.push(gen_a);

        let mut canonical = net.clone();
        canonical.canonicalize_generator_ids();

        let mut sol = make_opf_solution(vec![75.0, 125.0], vec![15.0, 25.0], vec![], vec![]);
        sol.generators.gen_ids = vec![
            canonical.generators[1].id.clone(),
            canonical.generators[0].id.clone(),
        ];

        apply_opf_dispatch(&mut net, &sol).unwrap();

        assert_eq!(net.generators[0].p, 125.0);
        assert_eq!(net.generators[0].q, 25.0);
        assert_eq!(net.generators[1].p, 75.0);
        assert_eq!(net.generators[1].q, 15.0);
    }
}
