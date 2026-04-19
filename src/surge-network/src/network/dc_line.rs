// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Two-terminal LCC-HVDC line data structures.
//!
//! A two-terminal DC line connects a rectifier bus (AC → DC) and an
//! inverter bus (DC → AC) through a DC circuit with resistance `resistance_ohm`.
//! PSS/E RAW section: "TWO-TERMINAL DC DATA".

use serde::{Deserialize, Serialize};

/// Control mode for a two-terminal LCC-HVDC link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LccHvdcControlMode {
    /// Line is blocked (out of service).
    Blocked = 0,
    /// Power control: SETVL is scheduled MW.
    #[default]
    PowerControl = 1,
    /// Current control: SETVL is scheduled kA.
    CurrentControl = 2,
}

impl LccHvdcControlMode {
    /// Convert a PSS/E MDC integer to a `LccHvdcControlMode`. Out-of-range values map to `Blocked`.
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::PowerControl,
            2 => Self::CurrentControl,
            _ => Self::Blocked,
        }
    }
}

/// One converter terminal (rectifier or inverter) of a two-terminal LCC-HVDC link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LccConverterTerminal {
    /// AC bus number this converter is connected to.
    pub bus: u32,
    /// Number of 6-pulse converter bridges (NBR / NBI).
    #[serde(alias = "num_bridges")]
    pub n_bridges: u32,
    /// Maximum firing/extinction angle in degrees (ALFMX / GAMMX).
    pub alpha_max: f64,
    /// Minimum firing/extinction angle in degrees (ALFMN / GAMMN).
    pub alpha_min: f64,
    /// Commutating resistance per bridge in ohms (RCR / RCI).
    pub commutation_resistance_ohm: f64,
    /// Commutating reactance per bridge in ohms (XCR / XCI).
    pub commutation_reactance_ohm: f64,
    /// Converter transformer rated AC voltage (line-to-line kV) on the network side (EBASR / EBASI).
    pub base_voltage_kv: f64,
    /// Transformer turns ratio (EBASR/EBASI side to converter side) (TRR / TRI).
    pub turns_ratio: f64,
    /// Off-nominal tap ratio (TAPR / TAPI), default 1.0.
    pub tap: f64,
    /// Maximum tap ratio (TMXR / TMXI).
    pub tap_max: f64,
    /// Minimum tap ratio (TMNR / TMNI).
    pub tap_min: f64,
    /// Tap step size (STPR / STPI), 0 = continuous.
    pub tap_step: f64,
    /// Converter in-service flag (ICR / ICI == 1).
    pub in_service: bool,
}

impl Default for LccConverterTerminal {
    fn default() -> Self {
        Self {
            bus: 0,
            n_bridges: 1,
            alpha_max: 90.0,
            alpha_min: 5.0,
            commutation_resistance_ohm: 0.0,
            commutation_reactance_ohm: 0.0,
            base_voltage_kv: 0.0,
            turns_ratio: 1.0,
            tap: 1.0,
            tap_max: 1.1,
            tap_min: 0.9,
            tap_step: 0.00625,
            in_service: true,
        }
    }
}

/// Two-terminal LCC-HVDC link (PSS/E TWO-TERMINAL DC DATA section).
///
/// Models a point-to-point high-voltage DC link with line-commutated
/// converters (thyristor bridges). The rectifier converts AC power to DC;
/// the inverter converts DC power back to AC.
///
/// For power flow with `FixedSchedule` DC model the converter power is
/// computed from `setvl`/`vschd` and injected as constant PQ at the
/// converter buses. With `SequentialAcDc`, the AC/DC operating point is
/// iterated to convergence.
///
/// When `p_dc_min_mw < p_dc_max_mw` the link exposes an optimization
/// variable rather than a fixed setpoint — used by the joint AC-DC OPF
/// path (`build_hvdc_p2p_nlp_data`) to put HVDC P into the NLP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LccHvdcLink {
    /// Name / identifier of the DC link.
    pub name: String,
    /// Control mode (0 = blocked, 1 = power control, 2 = current control).
    pub mode: LccHvdcControlMode,
    /// DC circuit resistance in ohms.
    pub resistance_ohm: f64,
    /// Scheduled DC power (MW) for MDC=1, or scheduled current (kA) for MDC=2.
    pub scheduled_setpoint: f64,
    /// Scheduled DC voltage in kV.
    pub scheduled_voltage_kv: f64,
    /// Max DC voltage for mode switching in kV (VCMOD).
    pub voltage_mode_switch_kv: f64,
    /// Compounding resistance in ohms (RCOMP).
    pub compounding_resistance_ohm: f64,
    /// Current margin in kA (DELTI).
    pub current_margin_ka: f64,
    /// Metering end: `'R'` = rectifier-metered, `'I'` = inverter-metered.
    pub meter: char,
    /// Minimum DC voltage in kV (DCVMIN).
    pub voltage_min_kv: f64,
    /// Maximum AC/DC outer-loop iterations (CCCITMX).
    pub ac_dc_iteration_max: u32,
    /// Acceleration factor for AC/DC iteration (CCCACC).
    pub ac_dc_iteration_acceleration: f64,
    /// Rectifier terminal (AC bus → DC).
    pub rectifier: LccConverterTerminal,
    /// Inverter terminal (DC → AC bus).
    pub inverter: LccConverterTerminal,
    /// Minimum DC power setpoint in MW for joint AC-DC OPF.
    ///
    /// When `p_dc_min_mw < p_dc_max_mw`, the joint AC-DC OPF will treat
    /// this link's DC power as a decision variable bounded in
    /// `[p_dc_min_mw, p_dc_max_mw]` rather than using the fixed
    /// `scheduled_setpoint`. Default `0.0` (no variable range → fall back
    /// to the sequential-iteration path with a fixed setpoint).
    #[serde(default)]
    pub p_dc_min_mw: f64,
    /// Maximum DC power setpoint in MW for joint AC-DC OPF.
    ///
    /// When `p_dc_min_mw < p_dc_max_mw`, the joint AC-DC OPF will treat
    /// this link's DC power as a decision variable bounded in
    /// `[p_dc_min_mw, p_dc_max_mw]`. Default `0.0`.
    #[serde(default)]
    pub p_dc_max_mw: f64,
}

impl Default for LccHvdcLink {
    fn default() -> Self {
        Self {
            name: String::new(),
            mode: LccHvdcControlMode::PowerControl,
            resistance_ohm: 0.0,
            scheduled_setpoint: 0.0,
            scheduled_voltage_kv: 500.0,
            voltage_mode_switch_kv: 0.0,
            compounding_resistance_ohm: 0.0,
            current_margin_ka: 0.0,
            meter: 'I',
            voltage_min_kv: 0.0,
            ac_dc_iteration_max: 20,
            ac_dc_iteration_acceleration: 1.0,
            rectifier: LccConverterTerminal::default(),
            inverter: LccConverterTerminal::default(),
            p_dc_min_mw: 0.0,
            p_dc_max_mw: 0.0,
        }
    }
}

impl LccHvdcLink {
    /// Returns `true` when the joint AC-DC OPF should treat this link's
    /// DC power as an NLP decision variable (i.e. `[p_dc_min_mw,
    /// p_dc_max_mw]` is a non-degenerate interval).
    pub fn has_variable_p_dc(&self) -> bool {
        self.p_dc_min_mw < self.p_dc_max_mw
    }
}
