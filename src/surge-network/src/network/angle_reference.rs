// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared output-angle reference conventions for power-flow solutions.

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::network::model::Network;

/// Reference angle convention for reported bus voltage angles.
///
/// This is an output/reporting convention, not a physical network property.
/// Changing the angle reference shifts all bus angles in an island uniformly
/// and therefore does not change branch flows or bus injections derived from
/// angle differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AngleReference {
    /// Preserve the initialized angle of the original reference bus.
    #[default]
    PreserveInitial,

    /// Force the original reference bus angle to zero.
    Zero,

    /// Shift all angles so a weighted-average reference angle is zero.
    Distributed(DistributedAngleWeight),
}

/// Weighting scheme for [`AngleReference::Distributed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DistributedAngleWeight {
    /// Weight each bus by its total active load (MW).
    #[default]
    LoadWeighted,
    /// Weight each bus by its total online generation (MW).
    GenerationWeighted,
    /// Weight each bus by connected generator inertia (`H * MBASE`).
    InertiaWeighted,
}

/// Apply an output angle reference convention to all buses in the network.
pub fn apply_angle_reference(
    angles: &mut [f64],
    network: &Network,
    reference_bus_idx: usize,
    reference_angle0_rad: f64,
    mode: AngleReference,
) {
    let bus_indices: Vec<usize> = (0..angles.len()).collect();
    apply_angle_reference_subset(
        angles,
        network,
        &bus_indices,
        reference_bus_idx,
        reference_angle0_rad,
        mode,
    );
}

/// Apply an output angle reference convention to one island/subset of buses.
///
/// The transformation is a uniform shift over `bus_indices`, so downstream
/// branch flows within that connected component are unchanged.
pub fn apply_angle_reference_subset(
    angles: &mut [f64],
    network: &Network,
    bus_indices: &[usize],
    reference_bus_idx: usize,
    reference_angle0_rad: f64,
    mode: AngleReference,
) {
    if bus_indices.is_empty() || reference_bus_idx >= angles.len() {
        return;
    }

    let shift = match mode {
        AngleReference::PreserveInitial => reference_angle0_rad - angles[reference_bus_idx],
        AngleReference::Zero => -angles[reference_bus_idx],
        AngleReference::Distributed(weight_mode) => {
            match distributed_reference_angle(network, angles, bus_indices, weight_mode) {
                Some(theta_ref) => -theta_ref,
                None => {
                    warn!(
                        ?weight_mode,
                        "distributed angle reference: all weights are zero, falling back to PreserveInitial"
                    );
                    reference_angle0_rad - angles[reference_bus_idx]
                }
            }
        }
    };

    if shift.abs() <= 1e-12 {
        return;
    }
    for &bus_idx in bus_indices {
        if let Some(angle) = angles.get_mut(bus_idx) {
            *angle += shift;
        }
    }
}

fn distributed_reference_angle(
    network: &Network,
    angles: &[f64],
    bus_indices: &[usize],
    weight_mode: DistributedAngleWeight,
) -> Option<f64> {
    let mut in_subset = vec![false; network.buses.len()];
    for &bus_idx in bus_indices {
        if let Some(flag) = in_subset.get_mut(bus_idx) {
            *flag = true;
        }
    }

    let bus_map = network.bus_index_map();
    let mut weighted_angle_sum = 0.0;
    let mut total_weight = 0.0;

    match weight_mode {
        DistributedAngleWeight::LoadWeighted => {
            for load in &network.loads {
                if !load.in_service {
                    continue;
                }
                let Some(&bus_idx) = bus_map.get(&load.bus) else {
                    continue;
                };
                if !in_subset[bus_idx] {
                    continue;
                }
                let weight = load.active_power_demand_mw.abs();
                if weight > 0.0 {
                    weighted_angle_sum += weight * angles[bus_idx];
                    total_weight += weight;
                }
            }
        }
        DistributedAngleWeight::GenerationWeighted => {
            for generator in &network.generators {
                if !generator.in_service {
                    continue;
                }
                let Some(&bus_idx) = bus_map.get(&generator.bus) else {
                    continue;
                };
                if !in_subset[bus_idx] {
                    continue;
                }
                let weight = generator.p.abs();
                if weight > 0.0 {
                    weighted_angle_sum += weight * angles[bus_idx];
                    total_weight += weight;
                }
            }
        }
        DistributedAngleWeight::InertiaWeighted => {
            for generator in &network.generators {
                if !generator.in_service {
                    continue;
                }
                let Some(&bus_idx) = bus_map.get(&generator.bus) else {
                    continue;
                };
                if !in_subset[bus_idx] {
                    continue;
                }
                let weight = generator.h_inertia_s.unwrap_or(0.0) * generator.machine_base_mva;
                if weight > 0.0 {
                    weighted_angle_sum += weight * angles[bus_idx];
                    total_weight += weight;
                }
            }
        }
    }

    (total_weight > 1e-12).then_some(weighted_angle_sum / total_weight)
}

#[cfg(test)]
mod tests {
    use super::{AngleReference, DistributedAngleWeight, apply_angle_reference_subset};
    use crate::network::{Bus, BusType, Generator, Load, Network};

    fn base_network() -> Network {
        Network {
            buses: vec![
                Bus {
                    number: 1,
                    bus_type: BusType::Slack,
                    ..Bus::default()
                },
                Bus {
                    number: 2,
                    bus_type: BusType::PQ,
                    ..Bus::default()
                },
                Bus {
                    number: 3,
                    bus_type: BusType::PV,
                    ..Bus::default()
                },
            ],
            loads: vec![
                Load {
                    bus: 1,
                    active_power_demand_mw: 10.0,
                    ..Load::default()
                },
                Load {
                    bus: 2,
                    active_power_demand_mw: 30.0,
                    ..Load::default()
                },
            ],
            generators: vec![
                Generator {
                    bus: 1,
                    p: 50.0,
                    ..Generator::default()
                },
                Generator {
                    bus: 3,
                    p: 150.0,
                    h_inertia_s: Some(4.0),
                    machine_base_mva: 100.0,
                    ..Generator::default()
                },
            ],
            ..Network::default()
        }
    }

    #[test]
    fn distributed_load_reference_zeroes_subset_weighted_mean() {
        let network = base_network();
        let mut angles = vec![0.3, -0.1, 0.8];
        apply_angle_reference_subset(
            &mut angles,
            &network,
            &[0, 1],
            0,
            0.0,
            AngleReference::Distributed(DistributedAngleWeight::LoadWeighted),
        );
        let weighted_mean = (10.0 * angles[0] + 30.0 * angles[1]) / 40.0;
        assert!(weighted_mean.abs() < 1e-12);
        assert!((angles[2] - 0.8).abs() < 1e-12);
    }

    #[test]
    fn distributed_generation_reference_uses_generation_weights() {
        let network = base_network();
        let mut angles = vec![0.2, 0.1, -0.4];
        apply_angle_reference_subset(
            &mut angles,
            &network,
            &[0, 2],
            0,
            0.0,
            AngleReference::Distributed(DistributedAngleWeight::GenerationWeighted),
        );
        let weighted_mean = (50.0 * angles[0] + 150.0 * angles[2]) / 200.0;
        assert!(weighted_mean.abs() < 1e-12);
        assert!((angles[1] - 0.1).abs() < 1e-12);
    }

    #[test]
    fn distributed_inertia_reference_uses_h_times_mbase() {
        let network = base_network();
        let mut angles = vec![0.1, 0.0, 0.7];
        apply_angle_reference_subset(
            &mut angles,
            &network,
            &[0, 2],
            0,
            0.0,
            AngleReference::Distributed(DistributedAngleWeight::InertiaWeighted),
        );
        let weighted_mean = (4.0 * 100.0 * angles[2]) / (4.0 * 100.0);
        assert!(weighted_mean.abs() < 1e-12);
    }

    #[test]
    fn zero_weight_distributed_reference_falls_back_to_preserve_initial() {
        let network = Network {
            buses: vec![
                Bus {
                    number: 1,
                    bus_type: BusType::Slack,
                    ..Bus::default()
                },
                Bus {
                    number: 2,
                    bus_type: BusType::PQ,
                    ..Bus::default()
                },
            ],
            ..Network::default()
        };
        let mut angles = vec![0.6, -0.2];
        apply_angle_reference_subset(
            &mut angles,
            &network,
            &[0, 1],
            0,
            0.25,
            AngleReference::Distributed(DistributedAngleWeight::LoadWeighted),
        );
        assert!((angles[0] - 0.25).abs() < 1e-12);
    }
}
