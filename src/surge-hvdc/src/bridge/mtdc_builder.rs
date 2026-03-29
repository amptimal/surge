// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Build VSC MTDC solver inputs from a `surge_network::Network`.
//!
//! This adapter centralizes the conversion from core-network HVDC records to
//! the explicit `DcNetwork` + `VscStation` inputs used by the block-coupled and
//! simultaneous MTDC solvers.

use std::collections::HashMap;

use surge_network::Network;

use crate::bridge::psse::hvdc_links_from_network;
use crate::dc_network::topology::{DcCable, DcNetwork, dc_branch_z_base, dc_bus_z_base};
use crate::error::HvdcError;
use crate::model::control::VscHvdcControlMode;
use crate::model::link::HvdcLink;
use crate::single_explicit_dc_grid;
use crate::solver::block_coupled::VscStation;

/// Build a VSC MTDC network and station list from a core `Network`.
///
/// Supported inputs:
/// - Explicit DC-grid data via one canonical `network.hvdc.dc_grids[*]`
/// - A single point-to-point `vsc_dc_line` fallback when explicit DC-network
///   data is not present
///
/// The fallback path is intentionally narrow: without explicit DC topology,
/// multiple VSC links cannot be inferred into one MTDC network unambiguously.
pub fn build_vsc_mtdc_system(network: &Network) -> Result<(DcNetwork, Vec<VscStation>), HvdcError> {
    if network.hvdc.has_explicit_dc_topology() {
        return build_from_explicit_dc_network(network);
    }
    build_from_single_vsc_link(network)
}

fn build_from_explicit_dc_network(
    network: &Network,
) -> Result<(DcNetwork, Vec<VscStation>), HvdcError> {
    let dc_grid = single_explicit_dc_grid(network)?;
    if dc_grid.buses.is_empty() {
        return Err(HvdcError::UnsupportedConfiguration(
            "explicit VSC MTDC topology requires at least one DC bus".to_string(),
        ));
    }

    let dc_bus_map: HashMap<u32, usize> = dc_grid
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.bus_id, i))
        .collect();
    let ac_bus_map = network.bus_index_map();

    let slack_converters: Vec<_> = dc_grid
        .converters
        .iter()
        .filter_map(|c| c.as_vsc())
        .filter(|c| c.status && c.control_type_dc == 2)
        .collect();
    if slack_converters.is_empty() {
        return Err(HvdcError::UnsupportedConfiguration(
            "explicit VSC MTDC topology requires exactly one DC-voltage slack converter"
                .to_string(),
        ));
    }
    if slack_converters.len() > 1 {
        return Err(HvdcError::UnsupportedConfiguration(
            "explicit VSC MTDC topology has multiple DC-voltage slack converters".to_string(),
        ));
    }
    let slack_converter = slack_converters[0];
    let slack_dc_bus = dc_bus_map
        .get(&slack_converter.dc_bus)
        .copied()
        .ok_or_else(|| {
            HvdcError::InvalidLink(format!(
                "DC-voltage slack converter references unknown dc_bus {}",
                slack_converter.dc_bus
            ))
        })?;

    let mut dc_net = DcNetwork::new(dc_grid.buses.len(), slack_dc_bus);
    dc_net.v_dc_slack = slack_converter.voltage_dc_setpoint_pu.max(0.05);
    dc_net.v_dc[slack_dc_bus] = dc_net.v_dc_slack;

    for dc_bus in &dc_grid.buses {
        let _ = dc_bus_z_base(network.base_mva, dc_bus.base_kv_dc)?;
    }

    for br in &dc_grid.branches {
        if !br.status {
            continue;
        }
        let from_dc_bus = dc_bus_map.get(&br.from_bus).copied().ok_or_else(|| {
            HvdcError::InvalidLink(format!("DC branch from_bus {} not found", br.from_bus))
        })?;
        let to_dc_bus = dc_bus_map.get(&br.to_bus).copied().ok_or_else(|| {
            HvdcError::InvalidLink(format!("DC branch to_bus {} not found", br.to_bus))
        })?;
        let z_base = dc_branch_z_base(
            network.base_mva,
            dc_grid.buses[from_dc_bus].base_kv_dc,
            dc_grid.buses[to_dc_bus].base_kv_dc,
        )?;
        dc_net.add_cable(DcCable {
            from_dc_bus,
            to_dc_bus,
            r_dc_pu: (br.r_ohm / z_base).max(1e-6),
            i_max_pu: 0.0,
        });
    }

    // Wire per-bus shunt conductance and ground return resistance from core DcBus.
    for (i, dc_bus) in dc_grid.buses.iter().enumerate() {
        let z_base = dc_bus_z_base(network.base_mva, dc_bus.base_kv_dc)?;
        // G_shunt in siemens → per-unit: G_pu = G_siemens * z_base
        if dc_bus.g_shunt_siemens > 0.0 {
            dc_net.g_shunt_pu[i] = dc_bus.g_shunt_siemens * z_base;
        }
        // R_ground in ohms → G_ground in per-unit: G_pu = z_base / R_ground
        if dc_bus.r_ground_ohm > 0.0 {
            dc_net.g_ground_pu[i] = z_base / dc_bus.r_ground_ohm;
        }
    }

    let stations: Vec<VscStation> = dc_grid
        .converters
        .iter()
        .filter_map(|c| c.as_vsc())
        .filter(|c| c.status)
        .map(|c| {
            if !ac_bus_map.contains_key(&c.ac_bus) {
                return Err(HvdcError::BusNotFound(c.ac_bus));
            }
            let dc_bus_idx = dc_bus_map.get(&c.dc_bus).copied().ok_or_else(|| {
                HvdcError::InvalidLink(format!("converter dc_bus {} not found", c.dc_bus))
            })?;
            Ok(VscStation {
                ac_bus: c.ac_bus,
                dc_bus_idx,
                control_mode: match c.control_type_dc {
                    2 => VscHvdcControlMode::ConstantVdc {
                        v_dc_target: c.voltage_dc_setpoint_pu.max(0.05),
                        q_set: c.reactive_power_mvar,
                    },
                    3 => VscHvdcControlMode::PVdcDroop {
                        p_set: c.power_dc_setpoint_mw,
                        voltage_dc_setpoint_pu: c.voltage_dc_setpoint_pu.max(0.05),
                        k_droop: c.droop,
                        p_min: c.active_power_ac_min_mw,
                        p_max: c.active_power_ac_max_mw,
                    },
                    _ => VscHvdcControlMode::ConstantPQ {
                        p_set: c.power_dc_setpoint_mw,
                        q_set: c.reactive_power_mvar,
                    },
                },
                q_max_mvar: c.reactive_power_ac_max_mvar,
                q_min_mvar: c.reactive_power_ac_min_mvar,
                loss_constant_mw: c.loss_constant_mw / network.base_mva,
                loss_linear: c.loss_linear,
                loss_c_rectifier: c.loss_quadratic_rectifier,
                loss_c_inverter: c.loss_quadratic_inverter,
            })
        })
        .collect::<Result<_, _>>()?;

    if stations.is_empty() {
        return Err(HvdcError::UnsupportedConfiguration(
            "network has no in-service VSC DC converters for the MTDC solver".to_string(),
        ));
    }

    Ok((dc_net, stations))
}

fn build_from_single_vsc_link(
    network: &Network,
) -> Result<(DcNetwork, Vec<VscStation>), HvdcError> {
    let vsc_links: Vec<_> = hvdc_links_from_network(network)
        .into_iter()
        .filter_map(|link| match link {
            HvdcLink::Vsc(vsc) => Some(vsc),
            HvdcLink::Lcc(_) => None,
        })
        .collect();

    if vsc_links.is_empty() {
        return Err(HvdcError::UnsupportedConfiguration(
            "network has no VSC HVDC data for the MTDC solver".to_string(),
        ));
    }
    if vsc_links.len() > 1 {
        return Err(HvdcError::UnsupportedConfiguration(
            "multiple VSC links without explicit DC network data cannot be inferred into one MTDC topology; populate a canonical dc_grid instead".to_string(),
        ));
    }

    let vsc = &vsc_links[0];
    let mut dc_net = DcNetwork::new(2, 1);
    dc_net.v_dc_slack = 1.0;
    dc_net.v_dc[1] = 1.0;
    dc_net.add_cable(DcCable {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 0.001,
        i_max_pu: 0.0,
    });

    let stations = vec![
        VscStation {
            ac_bus: vsc.from_bus,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: vsc.p_dc_mw,
                q_set: vsc.q_from_mvar,
            },
            q_max_mvar: vsc.q_max_from_mvar,
            q_min_mvar: vsc.q_min_from_mvar,
            loss_constant_mw: vsc.loss_coeff_a_mw,
            loss_linear: vsc.loss_coeff_b_pu,
            loss_c_rectifier: vsc.loss_c_pu,
            loss_c_inverter: vsc.loss_c_pu,
        },
        VscStation {
            ac_bus: vsc.to_bus,
            dc_bus_idx: 1,
            control_mode: VscHvdcControlMode::ConstantVdc {
                v_dc_target: 1.0,
                q_set: vsc.q_to_mvar,
            },
            q_max_mvar: vsc.q_max_to_mvar,
            q_min_mvar: vsc.q_min_to_mvar,
            loss_constant_mw: vsc.loss_coeff_a_mw,
            loss_linear: vsc.loss_coeff_b_pu,
            loss_c_rectifier: vsc.loss_c_pu,
            loss_c_inverter: vsc.loss_c_pu,
        },
    ];

    Ok((dc_net, stations))
}

#[cfg(test)]
mod tests {
    use super::build_vsc_mtdc_system;
    use crate::HvdcError;
    use surge_network::Network;
    use surge_network::network::{Bus, BusType, DcBranch, DcBus, DcConverter, DcConverterStation};

    fn build_explicit_vsc_network() -> Network {
        let mut network = Network::new("explicit-vsc-mtdc");
        network.base_mva = 100.0;
        network.buses.push(Bus::new(1, BusType::PQ, 230.0));
        network.buses.push(Bus::new(2, BusType::PQ, 230.0));

        let grid = network.hvdc.ensure_dc_grid(1, None);
        grid.buses.push(DcBus {
            bus_id: 101,
            p_dc_mw: 0.0,
            v_dc_pu: 1.02,
            base_kv_dc: 320.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.01,
            r_ground_ohm: 400.0,
        });
        grid.buses.push(DcBus {
            bus_id: 202,
            p_dc_mw: 0.0,
            v_dc_pu: 0.98,
            base_kv_dc: 400.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.branches.push(DcBranch {
            id: String::new(),
            from_bus: 101,
            to_bus: 202,
            r_ohm: 12.0,
            l_mh: 0.0,
            c_uf: 0.0,
            rating_a_mva: 0.0,
            rating_b_mva: 0.0,
            rating_c_mva: 0.0,
            status: true,
        });

        let converter_template = DcConverterStation {
            id: String::new(),
            dc_bus: 101,
            ac_bus: 1,
            control_type_dc: 1,
            control_type_ac: 1,
            active_power_mw: 999.0,
            reactive_power_mvar: 0.0,
            is_lcc: false,
            voltage_setpoint_pu: 1.0,
            transformer_r_pu: 0.0,
            transformer_x_pu: 0.0,
            transformer: false,
            tap_ratio: 1.0,
            filter_susceptance_pu: 0.0,
            filter: false,
            reactor_r_pu: 0.0,
            reactor_x_pu: 0.0,
            reactor: false,
            base_kv_ac: 230.0,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            current_max_pu: 10.0,
            status: true,
            loss_constant_mw: 1.0,
            loss_linear: 0.1,
            loss_quadratic_rectifier: 0.02,
            loss_quadratic_inverter: 0.03,
            droop: 0.0,
            power_dc_setpoint_mw: 50.0,
            voltage_dc_setpoint_pu: 1.0,
            active_power_ac_max_mw: 100.0,
            active_power_ac_min_mw: -100.0,
            reactive_power_ac_max_mvar: 50.0,
            reactive_power_ac_min_mvar: -50.0,
        };
        let grid = network.hvdc.find_dc_grid_mut(1).expect("grid exists");
        grid.converters
            .push(DcConverter::Vsc(converter_template.clone()));
        grid.converters.push(DcConverter::Vsc(DcConverterStation {
            dc_bus: 202,
            ac_bus: 2,
            control_type_dc: 2,
            power_dc_setpoint_mw: 0.0,
            voltage_dc_setpoint_pu: 1.01,
            ..converter_template
        }));

        network
    }

    #[test]
    fn explicit_builder_uses_per_bus_base_and_bus_conductances() {
        let network = build_explicit_vsc_network();

        let (dc_net, stations) = build_vsc_mtdc_system(&network).expect("builder should succeed");

        let z_branch = 360.0_f64 * 360.0 / 100.0;
        assert!((dc_net.branches[0].r_dc_pu - 12.0 / z_branch).abs() < 1e-12);

        let z_bus0 = 320.0_f64 * 320.0 / 100.0;
        assert!((dc_net.g_shunt_pu[0] - 0.01 * z_bus0).abs() < 1e-12);
        assert!((dc_net.g_ground_pu[0] - z_bus0 / 400.0).abs() < 1e-12);
        assert_eq!(dc_net.v_dc_slack, 1.01);
        assert_eq!(stations[0].control_mode.p_set_mw(), 50.0);
        assert!((stations[0].loss_c_rectifier - 0.02).abs() < 1e-12);
        assert!((stations[0].loss_c_inverter - 0.03).abs() < 1e-12);
    }

    #[test]
    fn explicit_builder_requires_exactly_one_slack_converter() {
        let mut network = build_explicit_vsc_network();
        network.hvdc.dc_grids[0].converters[1]
            .as_vsc_mut()
            .expect("vsc converter")
            .control_type_dc = 1;

        assert!(matches!(
            build_vsc_mtdc_system(&network),
            Err(HvdcError::UnsupportedConfiguration(message))
                if message.contains("exactly one DC-voltage slack converter")
        ));
    }

    #[test]
    fn explicit_builder_rejects_multiple_slack_converters() {
        let mut network = build_explicit_vsc_network();
        network.hvdc.dc_grids[0].converters[0]
            .as_vsc_mut()
            .expect("vsc converter")
            .control_type_dc = 2;

        assert!(matches!(
            build_vsc_mtdc_system(&network),
            Err(HvdcError::UnsupportedConfiguration(message))
                if message.contains("multiple DC-voltage slack converters")
        ));
    }

    #[test]
    fn explicit_builder_rejects_unknown_converter_dc_bus() {
        let mut network = build_explicit_vsc_network();
        *network.hvdc.dc_grids[0].converters[0].dc_bus_mut() = 999;

        assert!(matches!(
            build_vsc_mtdc_system(&network),
            Err(HvdcError::InvalidLink(message)) if message.contains("dc_bus 999")
        ));
    }

    #[test]
    fn explicit_builder_rejects_nonpositive_dc_base_kv() {
        let mut network = build_explicit_vsc_network();
        network.hvdc.dc_grids[0].buses[0].base_kv_dc = 0.0;

        assert!(matches!(
            build_vsc_mtdc_system(&network),
            Err(HvdcError::UnsupportedConfiguration(message))
                if message.contains("base_kv_dc")
        ));
    }
}
