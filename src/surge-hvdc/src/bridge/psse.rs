// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Bridge module: convert canonical `surge-network` HVDC link types to surge-hvdc solver types.
//!
//! `surge-network` stores point-to-point HVDC assets as `network.hvdc.links`, preserving the
//! source-native LCC/VSC records in canonical storage.
//!
//! This module converts them into the `LccHvdcLink`, `VscHvdcLink`, and `HvdcLink`
//! types used by the surge-hvdc solver, performing the necessary per-unit
//! conversions and mapping control-mode semantics.

use surge_network::Network;
use surge_network::network::{
    LccHvdcControlMode as NetworkLccControlMode, LccHvdcLink as NetworkLccLink,
    VscConverterAcControlMode, VscHvdcControlMode as NetworkVscControlMode,
    VscHvdcLink as NetworkVscLink,
};

use crate::bridge::lcc_commutation_reactance_pu;
use crate::model::control::LccHvdcControlMode;
use crate::model::link::{HvdcLink as SolverHvdcLink, LccHvdcLink, VscHvdcLink};

/// Convert a PSS/E-parsed `LccHvdcLink` (LCC two-terminal DC) into surge-hvdc `LccHvdcLink`.
///
/// Key mappings:
/// - `dc.scheduled_setpoint` -> `p_dc_mw` (when mode = PowerControl; for CurrentControl,
///   P = I_dc * V_dc where I_dc = setvl in kA and V_dc = vschd in kV)
/// - `dc.resistance_ohm` (ohms) -> `r_dc_pu`: `r_dc_pu = resistance_ohm / (vschd^2 / base_mva)`
/// - `dc.rectifier.alpha_min` (PSS/E field `ANMN`) -> `firing_angle_deg`
///   Note: PSS/E does not store the solved operating firing angle in the raw
///   data — `ANMN` is the minimum firing angle limit, which is also used here
///   as the initial steady-state operating point estimate.
/// - `dc.inverter.alpha_min` (PSS/E field `ANMN`) -> `extinction_angle_deg`
/// - `dc.rectifier.commutation_reactance_ohm * n_bridges / (e_base^2 / base_mva)` -> `x_c_r` (pu)
/// - `dc.rectifier.tap` -> `a_r` (transformer turns ratio)
///
/// `ConstantAlpha` and `Vdcol` control modes are not reachable from PSS/E
/// raw data; set `LccHvdcLink.control_mode` directly after construction to use them.
///
/// Power factor is estimated from the converter firing/extinction angles using the
/// standard approximation: `pf = cos(angle)` where angle is the operating angle.
///
/// Returns `None` if the DC line mode is `Blocked` (out of service).
pub fn lcc_from_dc_line(dc: &NetworkLccLink, base_mva: f64) -> Option<LccHvdcLink> {
    if dc.mode == NetworkLccControlMode::Blocked {
        return None;
    }

    // DC power setpoint in MW (reference value; actual mode carries the setpoint)
    let p_dc_mw = match dc.mode {
        NetworkLccControlMode::PowerControl => dc.scheduled_setpoint,
        NetworkLccControlMode::CurrentControl => {
            // setvl is in kA, vschd in kV => P = I * V in MW
            dc.scheduled_setpoint * dc.scheduled_voltage_kv
        }
        NetworkLccControlMode::Blocked => unreachable!(),
    };

    // LCC control mode derived from PSS/E IMODE field.
    // CurrentControl: i_d_pu = setvl [kA] × vschd [kV] / base_mva [MVA]
    //   (= p_dc_mw / base_mva, i.e. per-unit on system base at rated DC voltage)
    let control_mode = match dc.mode {
        NetworkLccControlMode::PowerControl => LccHvdcControlMode::ConstantPower,
        NetworkLccControlMode::CurrentControl => LccHvdcControlMode::ConstantCurrent {
            i_d_pu: dc.scheduled_setpoint * dc.scheduled_voltage_kv / base_mva,
        },
        NetworkLccControlMode::Blocked => unreachable!(),
    };

    // DC resistance: convert from ohms to per-unit
    // Z_dc_base = Vdc_kV^2 / base_mva
    let z_dc_base = if dc.scheduled_voltage_kv > 0.0 {
        dc.scheduled_voltage_kv * dc.scheduled_voltage_kv / base_mva
    } else {
        // Avoid division by zero; use 1.0 as fallback (r_dc_pu = resistance_ohm)
        1.0
    };
    let r_dc_pu = dc.resistance_ohm / z_dc_base;

    // Commutation reactance: convert from ohms-per-bridge to per-unit (total)
    // x_c_pu = x_comm * n_bridges / (e_base^2 / base_mva)
    let x_c_r = lcc_commutation_reactance_pu(
        dc.rectifier.commutation_reactance_ohm,
        dc.rectifier.n_bridges,
        dc.rectifier.base_voltage_kv,
        base_mva,
    )
    .unwrap_or(0.0);

    let x_c_i = lcc_commutation_reactance_pu(
        dc.inverter.commutation_reactance_ohm,
        dc.inverter.n_bridges,
        dc.inverter.base_voltage_kv,
        base_mva,
    )
    .unwrap_or(0.0);

    // Firing/extinction angles: use alpha_min as the steady-state operating point
    let firing_angle_deg = dc.rectifier.alpha_min;
    let extinction_angle_deg = dc.inverter.alpha_min;

    // Power factor from operating angles: pf = cos(angle)
    let power_factor_r = firing_angle_deg.to_radians().cos();
    let power_factor_i = extinction_angle_deg.to_radians().cos();

    // Transformer turns ratios
    let a_r = dc.rectifier.tap;
    let a_i = dc.inverter.tap;

    Some(LccHvdcLink {
        from_bus: dc.rectifier.bus,
        to_bus: dc.inverter.bus,
        p_dc_mw,
        r_dc_pu,
        firing_angle_deg,
        extinction_angle_deg,
        // PSS/E two-terminal DC stores alpha_min (ANMN) as the minimum firing angle
        // limit, which is also what we use as the steady-state operating angle.
        // Use 5.0° as the physical lower bound for commutation safety (typical range
        // is 3–7° for modern thyristor valves). The 1.0° floor guards degenerate cases.
        alpha_min_deg: 5.0_f64.max(firing_angle_deg * 0.33).max(1.0),
        power_factor_r,
        power_factor_i,
        a_r,
        a_i,
        x_c_r,
        x_c_i,
        control_mode,
        name: dc.name.clone(),
    })
}

/// Convert a PSS/E-parsed `VscHvdcLink` into surge-hvdc `VscHvdcLink`.
///
/// Key mappings:
/// - `vsc.converter1.dc_setpoint` -> `p_dc_mw` (MW, when mode = PowerControl)
/// - `vsc.converter1.loss_constant_mw` -> `loss_coeff_a_mw` (constant loss in pu: loss_a / base_mva)
/// - `vsc.converter1.loss_linear` -> `loss_coeff_b_pu` (linear loss coefficient, dimensionless)
/// - `vsc.converter1.q_min_mvar/q_max` -> `q_min_from_mvar/q_max_from_mvar`
/// - `vsc.converter2.q_min_mvar/q_max` -> `q_min_to_mvar/q_max_to_mvar`
/// - Reactive power setpoints depend on the converter control mode:
///   - `ReactivePower`: `ac_setpoint` is in MVAr -> use directly
///   - `AcVoltage`: `ac_setpoint` is in pu -> set Q to 0.0 (PV bus behaviour)
///
/// Returns `None` if the VSC line mode is `Blocked` (out of service).
pub fn vsc_from_vsc_dc_line(vsc: &NetworkVscLink, base_mva: f64) -> Option<VscHvdcLink> {
    if vsc.mode == NetworkVscControlMode::Blocked {
        return None;
    }

    // DC power setpoint
    let p_dc_mw = match vsc.mode {
        NetworkVscControlMode::PowerControl => vsc.converter1.dc_setpoint,
        NetworkVscControlMode::VdcControl => {
            // In Vdc control mode, converter1.dc_setpoint is the DC voltage (kV).
            // Use converter2's dc_setpoint as power if available, otherwise 0.
            vsc.converter2.dc_setpoint
        }
        NetworkVscControlMode::Blocked => unreachable!(),
    };

    // Reactive power at from-bus (converter1)
    let q_from_mvar = match vsc.converter1.control_mode {
        VscConverterAcControlMode::ReactivePower => vsc.converter1.ac_setpoint,
        VscConverterAcControlMode::AcVoltage => 0.0, // PV bus: Q determined by AC solver
    };

    // Reactive power at to-bus (converter2)
    let q_to_mvar = match vsc.converter2.control_mode {
        VscConverterAcControlMode::ReactivePower => vsc.converter2.ac_setpoint,
        VscConverterAcControlMode::AcVoltage => 0.0, // PV bus: Q determined by AC solver
    };

    // Loss coefficients: convert constant loss from MW to per-unit
    let loss_coeff_a_mw = if base_mva > 0.0 {
        vsc.converter1.loss_constant_mw / base_mva
    } else {
        0.0
    };
    let loss_coeff_b_pu = vsc.converter1.loss_linear;

    Some(VscHvdcLink {
        from_bus: vsc.converter1.bus,
        to_bus: vsc.converter2.bus,
        p_dc_mw,
        q_from_mvar,
        q_to_mvar,
        loss_coeff_a_mw,
        loss_coeff_b_pu,
        loss_c_pu: 0.0, // PSS/E only provides linear loss model (a + b*I)
        q_max_from_mvar: vsc.converter1.q_max_mvar,
        q_min_from_mvar: vsc.converter1.q_min_mvar,
        q_max_to_mvar: vsc.converter2.q_max_mvar,
        q_min_to_mvar: vsc.converter2.q_min_mvar,
        p_dc_min_mw: 0.0, // PSS/E does not specify OPF bounds
        p_dc_max_mw: 0.0,
        name: vsc.name.clone(),
    })
}

/// Extract all point-to-point HVDC links from a network's canonical `hvdc` namespace.
///
/// Converts all in-service (non-blocked) DC lines and VSC DC lines to `HvdcLink`
/// enum variants. Blocked links are excluded.
pub fn hvdc_links_from_network(network: &Network) -> Vec<SolverHvdcLink> {
    let base_mva = network.base_mva;
    network
        .hvdc
        .links
        .iter()
        .filter_map(|link| match link {
            surge_network::network::HvdcLink::Lcc(dc) => {
                lcc_from_dc_line(dc, base_mva).map(SolverHvdcLink::Lcc)
            }
            surge_network::network::HvdcLink::Vsc(vsc) => {
                vsc_from_vsc_dc_line(vsc, base_mva).map(SolverHvdcLink::Vsc)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::{
        LccConverterTerminal, LccHvdcControlMode, LccHvdcLink, VscConverterTerminal,
        VscHvdcControlMode, VscHvdcLink,
    };

    #[test]
    fn test_lcc_bridge_basic() {
        let base_mva = 100.0;
        let dc = LccHvdcLink {
            name: "HVDC-1".to_string(),
            mode: LccHvdcControlMode::PowerControl,
            resistance_ohm: 5.0,         // 5 ohms
            scheduled_setpoint: 500.0,   // 500 MW
            scheduled_voltage_kv: 500.0, // 500 kV
            rectifier: LccConverterTerminal {
                bus: 1,
                n_bridges: 2,
                alpha_max: 90.0,
                alpha_min: 15.0, // operating firing angle
                commutation_resistance_ohm: 0.0,
                commutation_reactance_ohm: 10.0, // 10 ohms per bridge
                base_voltage_kv: 345.0,          // 345 kV rated AC voltage
                turns_ratio: 1.0,
                tap: 1.05,
                tap_max: 1.1,
                tap_min: 0.9,
                tap_step: 0.00625,
                in_service: true,
            },
            inverter: LccConverterTerminal {
                bus: 2,
                n_bridges: 2,
                alpha_max: 90.0,
                alpha_min: 18.0, // operating extinction angle
                commutation_resistance_ohm: 0.0,
                commutation_reactance_ohm: 8.0, // 8 ohms per bridge
                base_voltage_kv: 230.0,         // 230 kV rated AC voltage
                turns_ratio: 1.0,
                tap: 0.98,
                tap_max: 1.1,
                tap_min: 0.9,
                tap_step: 0.00625,
                in_service: true,
            },
            ..LccHvdcLink::default()
        };

        let lcc = lcc_from_dc_line(&dc, base_mva).expect("should convert non-blocked LCC");

        // Bus mapping
        assert_eq!(lcc.from_bus, 1);
        assert_eq!(lcc.to_bus, 2);
        assert_eq!(lcc.name, "HVDC-1");

        // Power setpoint
        assert!((lcc.p_dc_mw - 500.0).abs() < 1e-10);

        // DC resistance: Z_dc_base = 500^2 / 100 = 2500 ohms; r_dc_pu = 5 / 2500 = 0.002
        assert!((lcc.r_dc_pu - 0.002).abs() < 1e-10);

        // Firing / extinction angles
        assert!((lcc.firing_angle_deg - 15.0).abs() < 1e-10);
        assert!((lcc.extinction_angle_deg - 18.0).abs() < 1e-10);

        // Power factors: cos(15 deg), cos(18 deg)
        assert!((lcc.power_factor_r - 15.0_f64.to_radians().cos()).abs() < 1e-10);
        assert!((lcc.power_factor_i - 18.0_f64.to_radians().cos()).abs() < 1e-10);

        // Transformer turns ratios
        assert!((lcc.a_r - 1.05).abs() < 1e-10);
        assert!((lcc.a_i - 0.98).abs() < 1e-10);

        // Commutation reactance (rectifier):
        // z_base_r = 345^2 / 100 = 1190.25; x_c_r = 10 * 2 / 1190.25 = 0.01680672...
        let expected_x_c_r = 10.0 * 2.0 / (345.0 * 345.0 / 100.0);
        assert!((lcc.x_c_r - expected_x_c_r).abs() < 1e-10);

        // Commutation reactance (inverter):
        // z_base_i = 230^2 / 100 = 529; x_c_i = 8 * 2 / 529 = 0.03023...
        let expected_x_c_i = 8.0 * 2.0 / (230.0 * 230.0 / 100.0);
        assert!((lcc.x_c_i - expected_x_c_i).abs() < 1e-10);
    }

    #[test]
    fn test_vsc_bridge_basic() {
        let base_mva = 100.0;
        let vsc = VscHvdcLink {
            name: "VSC-1".to_string(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 2.5,
            converter1: VscConverterTerminal {
                bus: 10,
                control_mode: VscConverterAcControlMode::ReactivePower,
                dc_setpoint: 200.0,    // 200 MW
                ac_setpoint: 50.0,     // 50 MVAr (reactive power mode)
                loss_constant_mw: 1.5, // 1.5 MW constant loss
                loss_linear: 0.02,     // linear loss coefficient
                q_min_mvar: -100.0,
                q_max_mvar: 100.0,
                voltage_min_pu: 0.9,
                voltage_max_pu: 1.1,
                in_service: true,
            },
            converter2: VscConverterTerminal {
                bus: 20,
                control_mode: VscConverterAcControlMode::AcVoltage,
                dc_setpoint: 0.0,
                ac_setpoint: 1.02, // 1.02 pu voltage (AcVoltage mode)
                loss_constant_mw: 1.0,
                loss_linear: 0.01,
                q_min_mvar: -80.0,
                q_max_mvar: 80.0,
                voltage_min_pu: 0.9,
                voltage_max_pu: 1.1,
                in_service: true,
            },
        };

        let params = vsc_from_vsc_dc_line(&vsc, base_mva).expect("should convert non-blocked VSC");

        // Bus mapping
        assert_eq!(params.from_bus, 10);
        assert_eq!(params.to_bus, 20);
        assert_eq!(params.name, "VSC-1");

        // Power setpoint (PowerControl mode: converter1.dc_setpoint)
        assert!((params.p_dc_mw - 200.0).abs() < 1e-10);

        // Reactive power: converter1 in ReactivePower mode => use ac_setpoint directly
        assert!((params.q_from_mvar - 50.0).abs() < 1e-10);
        // converter2 in AcVoltage mode => Q = 0.0
        assert!((params.q_to_mvar - 0.0).abs() < 1e-10);

        // Loss coefficients: loss_a_mw / base_mva = 1.5 / 100 = 0.015
        assert!((params.loss_coeff_a_mw - 0.015).abs() < 1e-10);
        assert!((params.loss_coeff_b_pu - 0.02).abs() < 1e-10);
        assert!((params.loss_c_pu - 0.0).abs() < 1e-10);

        // Q limits
        assert!((params.q_max_from_mvar - 100.0).abs() < 1e-10);
        assert!((params.q_min_from_mvar - (-100.0)).abs() < 1e-10);
        assert!((params.q_max_to_mvar - 80.0).abs() < 1e-10);
        assert!((params.q_min_to_mvar - (-80.0)).abs() < 1e-10);
    }

    #[test]
    fn test_bridge_from_network() {
        let mut network = Network::new("test");
        network.base_mva = 100.0;

        // Add one LCC DC line
        network.hvdc.push_lcc_link(LccHvdcLink {
            name: "LCC-1".to_string(),
            mode: LccHvdcControlMode::PowerControl,
            scheduled_setpoint: 300.0,
            scheduled_voltage_kv: 500.0,
            resistance_ohm: 5.0,
            rectifier: LccConverterTerminal {
                bus: 1,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 2,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        });

        // Add one VSC DC line
        network.hvdc.push_vsc_link(VscHvdcLink {
            name: "VSC-1".to_string(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 2.0,
            converter1: VscConverterTerminal {
                bus: 3,
                dc_setpoint: 150.0,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 4,
                ..VscConverterTerminal::default()
            },
        });

        let links = hvdc_links_from_network(&network);
        assert_eq!(links.len(), 2);

        // First should be LCC
        assert!(matches!(&links[0], SolverHvdcLink::Lcc(lcc) if lcc.name == "LCC-1"));
        // Second should be VSC
        assert!(matches!(&links[1], SolverHvdcLink::Vsc(vsc) if vsc.name == "VSC-1"));
    }

    #[test]
    fn test_bridge_blocked_excluded() {
        let mut network = Network::new("test");
        network.base_mva = 100.0;

        // Add a blocked LCC DC line
        network.hvdc.push_lcc_link(LccHvdcLink {
            name: "LCC-blocked".to_string(),
            mode: LccHvdcControlMode::Blocked,
            scheduled_setpoint: 300.0,
            scheduled_voltage_kv: 500.0,
            rectifier: LccConverterTerminal {
                bus: 1,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 2,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        });

        // Add a blocked VSC DC line
        network.hvdc.push_vsc_link(VscHvdcLink {
            name: "VSC-blocked".to_string(),
            mode: VscHvdcControlMode::Blocked,
            resistance_ohm: 2.0,
            converter1: VscConverterTerminal {
                bus: 3,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 4,
                ..VscConverterTerminal::default()
            },
        });

        // Add one in-service VSC DC line
        network.hvdc.push_vsc_link(VscHvdcLink {
            name: "VSC-active".to_string(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 1.0,
            converter1: VscConverterTerminal {
                bus: 5,
                dc_setpoint: 100.0,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 6,
                ..VscConverterTerminal::default()
            },
        });

        let links = hvdc_links_from_network(&network);

        // Only the active VSC should be included; both blocked links excluded
        assert_eq!(links.len(), 1);
        assert!(matches!(&links[0], SolverHvdcLink::Vsc(vsc) if vsc.name == "VSC-active"));
    }
}
