// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! VSC (Voltage Source Converter) DC line data structures.
//!
//! A VSC-HVDC link uses self-commutated converters (IGBTs) which can
//! independently control both active and reactive power at each terminal.
//! PSS/E RAW section: "VSC DC LINE DATA".

use serde::{Deserialize, Serialize};

/// AC-side control mode for a VSC converter terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VscConverterAcControlMode {
    /// Controls AC voltage magnitude at its bus (PV bus behaviour).
    AcVoltage = 1,
    /// Controls reactive power injection at its bus (PQ behaviour).
    #[default]
    ReactivePower = 2,
}

impl VscConverterAcControlMode {
    /// Convert a PSS/E MODE integer. Values other than 1 map to `ReactivePower`.
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::AcVoltage,
            _ => Self::ReactivePower,
        }
    }
}

/// Control mode for the overall VSC-HVDC link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VscHvdcControlMode {
    /// Link blocked (out of service).
    Blocked = 0,
    /// Active power flow controlled.
    #[default]
    PowerControl = 1,
    /// DC voltage controlled.
    VdcControl = 2,
}

impl VscHvdcControlMode {
    /// Convert a PSS/E MDC integer. Out-of-range values map to `Blocked`.
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::PowerControl,
            2 => Self::VdcControl,
            _ => Self::Blocked,
        }
    }
}

/// One converter terminal of a VSC-HVDC link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VscConverterTerminal {
    /// AC bus number this converter is connected to.
    pub bus: u32,
    /// AC-side control mode (PV = voltage, PQ = reactive).
    pub control_mode: VscConverterAcControlMode,
    /// DC power setpoint (MW) or DC voltage setpoint (kV), depending on link mode.
    pub dc_setpoint: f64,
    /// AC voltage setpoint (pu) when `control_mode = AcVoltage`,
    /// or reactive power setpoint (MVAr) when `control_mode = ReactivePower`.
    pub ac_setpoint: f64,
    /// Constant converter loss in MW (ALOSS).
    pub loss_constant_mw: f64,
    /// Linear converter loss in MW per MW transferred (BLOSS).
    pub loss_linear: f64,
    /// Minimum reactive power injection in MVAr (SMN), typically ≤ 0.
    pub q_min_mvar: f64,
    /// Maximum reactive power injection in MVAr (SMX), typically ≥ 0.
    pub q_max_mvar: f64,
    /// Lower AC voltage limit in pu (GMN).
    pub voltage_min_pu: f64,
    /// Upper AC voltage limit in pu (GMX).
    pub voltage_max_pu: f64,
    /// Converter in-service flag.
    pub in_service: bool,
}

impl Default for VscConverterTerminal {
    fn default() -> Self {
        Self {
            bus: 0,
            control_mode: VscConverterAcControlMode::ReactivePower,
            dc_setpoint: 0.0,
            ac_setpoint: 1.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            q_min_mvar: -9999.0,
            q_max_mvar: 9999.0,
            voltage_min_pu: 0.9,
            voltage_max_pu: 1.1,
            in_service: true,
        }
    }
}

/// VSC-HVDC two-terminal link (PSS/E VSC DC LINE DATA section).
///
/// Models a point-to-point voltage source converter HVDC link.
/// Each converter can independently control both P and Q (or AC voltage),
/// unlike LCC converters which are constrained to absorb reactive power.
///
/// For `FixedSchedule` power flow the DC power setpoint is used directly;
/// converter losses reduce the power at the sending end. For `SequentialAcDc`
/// both converter AC power/voltage interactions are iterated to convergence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VscHvdcLink {
    /// Name / identifier of the VSC link.
    pub name: String,
    /// Link control mode (0 = blocked, 1 = power, 2 = Vdc).
    pub mode: VscHvdcControlMode,
    /// DC cable resistance in ohms (RDC).
    pub resistance_ohm: f64,
    /// Converter at the "sending" (or rectifier) end.
    pub converter1: VscConverterTerminal,
    /// Converter at the "receiving" (or inverter) end.
    pub converter2: VscConverterTerminal,
}

impl Default for VscHvdcLink {
    fn default() -> Self {
        Self {
            name: String::new(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.0,
            converter1: VscConverterTerminal::default(),
            converter2: VscConverterTerminal::default(),
        }
    }
}
