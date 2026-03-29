// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! HVDC link model types: LCC and VSC parameters.

use serde::{Deserialize, Serialize};

use crate::model::control::LccHvdcControlMode;

/// Parameters for a Line-Commutated Converter (LCC, thyristor) HVDC link.
///
/// Models a bipolar LCC link with a rectifier end (from_bus) and an inverter
/// end (to_bus). The control mode determines how the DC operating point is
/// computed; see [`LccHvdcControlMode`] for the available options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LccHvdcLink {
    /// AC bus number at the rectifier end.
    pub from_bus: u32,
    /// AC bus number at the inverter end.
    pub to_bus: u32,
    /// DC power setpoint in MW (positive = power flows from_bus → to_bus).
    /// Used directly in `ConstantPower` mode; serves as a reference value
    /// in other modes.
    pub p_dc_mw: f64,
    /// DC line resistance in per-unit on system base.
    /// Used to compute DC line losses: P_loss = I_d² × R_dc.
    pub r_dc_pu: f64,
    /// Rectifier firing angle α in degrees (default: 15°).
    /// In `ConstantPower` and `ConstantCurrent` modes this is the steady-state
    /// operating angle used to compute converter reactive absorption.
    /// In `ConstantAlpha` mode this field is ignored (the mode carries its own α).
    pub firing_angle_deg: f64,
    /// Inverter extinction angle γ in degrees (default: 15°).
    pub extinction_angle_deg: f64,
    /// Minimum rectifier firing angle α_min in degrees (default: 5°).
    ///
    /// Guards against commutation failure: the solver will not allow α to drop
    /// below this value in any control mode.  Typical values are 3–7° for
    /// modern thyristor valves.
    pub alpha_min_deg: f64,
    /// Rectifier power factor (default: 0.9).
    /// Used to compute reactive power absorbed by the rectifier: Q = P × tan(arccos(PF)).
    pub power_factor_r: f64,
    /// Inverter power factor (default: 0.9).
    pub power_factor_i: f64,
    /// Rectifier converter transformer turns ratio (per-unit).
    ///
    /// Represents the ratio of the transformer secondary voltage to the AC bus
    /// voltage. Used in the DC voltage equation:
    ///   `Vd_R = (3√2/π) × a_r × V_ac_R × cos(α) − (3/π) × X_c_R × I_d`
    ///
    /// Default = 1.0 (absorbed into per-unit base). FPQ-41.
    pub a_r: f64,
    /// Inverter converter transformer turns ratio (per-unit).
    ///
    /// Represents the ratio of the transformer secondary voltage to the AC bus
    /// voltage. Used in the DC voltage equation:
    ///   `Vd_I = (3√2/π) × a_i × V_ac_I × cos(γ) + (3/π) × X_c_I × I_d`
    ///
    /// Default = 1.0 (absorbed into per-unit base). FPQ-41.
    pub a_i: f64,
    /// Rectifier commutation reactance in per-unit on system base.
    ///
    /// This is the leakage reactance of the converter transformer at the
    /// rectifier end. It produces a DC voltage drop proportional to the DC
    /// current (commutation overlap):
    ///   `Vd_R = (3√2/π) × a_r × V_ac_R × cos(α) − (3/π) × X_c_R × I_d`
    ///
    /// Default = 0.0 (no commutation reactance drop, matching the simplified
    /// constant-power model).
    pub x_c_r: f64,
    /// Inverter commutation reactance in per-unit on system base.
    ///
    /// Produces a DC voltage drop at the inverter end:
    ///   `Vd_I = (3√2/π) × a_i × V_ac_I × cos(γ) − (3/π) × X_c_I × I_d`
    ///
    /// Default = 0.0 (no commutation reactance drop).
    pub x_c_i: f64,
    /// Active control mode for this LCC link (default: `ConstantPower`).
    pub control_mode: LccHvdcControlMode,
    /// Optional link name for reporting.
    pub name: String,
}

impl LccHvdcLink {
    /// Create an LCC link with default angle and power factor settings.
    pub fn new(from_bus: u32, to_bus: u32, p_dc_mw: f64) -> Self {
        Self {
            from_bus,
            to_bus,
            p_dc_mw,
            r_dc_pu: 0.0,
            firing_angle_deg: 15.0,
            extinction_angle_deg: 15.0,
            alpha_min_deg: 5.0,
            power_factor_r: 0.9,
            power_factor_i: 0.9,
            a_r: 1.0,
            a_i: 1.0,
            x_c_r: 0.0,
            x_c_i: 0.0,
            control_mode: LccHvdcControlMode::ConstantPower,
            name: String::new(),
        }
    }

    /// Compute reactive power absorbed at the rectifier (positive = absorbed from AC).
    ///
    /// Uses the Uhlmann approximation: the effective power factor angle phi is
    /// approximated as `phi = (alpha + mu) / 2`, where alpha is the firing angle
    /// and mu is the commutation overlap angle. When commutation reactance is
    /// zero (mu ~ 0), this simplifies to `phi = alpha / 2`, and the reactive
    /// power is `Q = P * tan(alpha)` from the basic rectifier equation.
    ///
    /// For non-zero commutation reactance, the overlap angle mu depends on the
    /// DC current (not available here), so we use `Q = P * tan(alpha)` which is
    /// the standard approximation for the rectifier end. This is more physically
    /// accurate than a fixed power factor because it tracks the actual firing
    /// angle setting.
    pub fn q_rectifier_mvar(&self, p_dc_mw: f64) -> f64 {
        // Clamp to alpha_min_deg so Q tracks the same angle used in the power flow.
        let alpha_rad = self.firing_angle_deg.max(self.alpha_min_deg).to_radians();
        // Q = P * tan(alpha) from the basic 6-pulse bridge rectifier equation:
        // The converter absorbs reactive power proportional to tan of the firing angle.
        p_dc_mw * alpha_rad.tan()
    }

    /// Compute reactive power absorbed at the inverter (positive = absorbed from AC).
    ///
    /// Uses the extinction angle gamma analogously to the rectifier firing angle.
    /// Q_I = P * tan(gamma).
    pub fn q_inverter_mvar(&self, p_dc_mw: f64) -> f64 {
        let gamma_rad = self.extinction_angle_deg.to_radians();
        p_dc_mw * gamma_rad.tan()
    }
}

/// Parameters for a Voltage-Source Converter (VSC, IGBT) HVDC link.
///
/// VSC HVDC operates under independent P and Q control at each converter.
/// The rectifier draws P_dc from the AC network; the inverter injects
/// (P_dc − P_loss) into the AC network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VscHvdcLink {
    /// AC bus number at the rectifier end.
    pub from_bus: u32,
    /// AC bus number at the inverter end.
    pub to_bus: u32,
    /// DC power setpoint in MW (positive = power flows from_bus → to_bus).
    pub p_dc_mw: f64,
    /// Reactive power setpoint at the rectifier in MVAR (+ = injection into AC).
    pub q_from_mvar: f64,
    /// Reactive power setpoint at the inverter in MVAR (+ = injection into AC).
    pub q_to_mvar: f64,
    /// Constant loss coefficient in pu (independent of current).
    pub loss_coeff_a_mw: f64,
    /// Linear loss coefficient (scales with AC current magnitude).
    pub loss_coeff_b_pu: f64,
    /// Quadratic loss coefficient in per-unit (FPQ-43 / P5-022).
    ///
    /// Implements the full IGBT converter loss model:
    ///   `P_loss = (a + b × |I_ac| + c × I_ac²) × base_mva`
    ///
    /// where `I_ac` is the per-unit AC apparent current:
    ///   `I_ac = sqrt(P² + Q²) / (V_ac × base_mva)`
    ///
    /// Typical CIGRE B4 TB 492 values:
    ///   a = 0.003 pu (no-load / transformer core losses)
    ///   b = 0.010 pu (switching / linear term)
    ///   c = 0.020 pu (conduction / quadratic term)
    ///
    /// Setting `c = 0` (the default) reduces to the linear model.
    pub loss_c_pu: f64,
    /// Maximum reactive injection at rectifier (MVAR).
    pub q_max_from_mvar: f64,
    /// Minimum reactive injection at rectifier (MVAR).
    pub q_min_from_mvar: f64,
    /// Maximum reactive injection at inverter (MVAR).
    pub q_max_to_mvar: f64,
    /// Minimum reactive injection at inverter (MVAR).
    pub q_min_to_mvar: f64,
    /// Minimum DC power setpoint for OPF variable bounds (MW).
    ///
    /// When `p_dc_min_mw < p_dc_max_mw`, the OPF treats P_dc as a variable
    /// bounded in `[p_dc_min_mw, p_dc_max_mw]` rather than using the fixed
    /// `p_dc_mw` setpoint.
    pub p_dc_min_mw: f64,
    /// Maximum DC power setpoint for OPF variable bounds (MW).
    ///
    /// When `p_dc_min_mw < p_dc_max_mw`, the OPF treats P_dc as a variable
    /// bounded in `[p_dc_min_mw, p_dc_max_mw]`.
    pub p_dc_max_mw: f64,
    /// Optional link name for reporting.
    pub name: String,
}

impl VscHvdcLink {
    /// Create a VSC link with zero losses and zero reactive setpoints.
    pub fn new(from_bus: u32, to_bus: u32, p_dc_mw: f64) -> Self {
        Self {
            from_bus,
            to_bus,
            p_dc_mw,
            q_from_mvar: 0.0,
            q_to_mvar: 0.0,
            loss_coeff_a_mw: 0.0,
            loss_coeff_b_pu: 0.0,
            loss_c_pu: 0.0,
            q_max_from_mvar: 9999.0,
            q_min_from_mvar: -9999.0,
            q_max_to_mvar: 9999.0,
            q_min_to_mvar: -9999.0,
            p_dc_min_mw: 0.0,
            p_dc_max_mw: 0.0,
            name: String::new(),
        }
    }

    /// Returns `true` if this VSC link has variable P_dc bounds for OPF.
    ///
    /// A link is considered variable when `p_dc_min_mw < p_dc_max_mw`.
    pub fn has_variable_p_dc(&self) -> bool {
        self.p_dc_min_mw < self.p_dc_max_mw
    }

    /// Compute VSC losses in MW given the AC current magnitude in per-unit.
    ///
    /// Full quadratic IGBT loss model (FPQ-43 / P5-022):
    ///   `P_loss = (a + b × I_ac + c × I_ac²) × base_mva`
    ///
    /// With `c = 0` (default) this reduces to the original linear model.
    pub fn losses_mw(&self, i_ac_pu: f64, base_mva: f64) -> f64 {
        (self.loss_coeff_a_mw + self.loss_coeff_b_pu * i_ac_pu + self.loss_c_pu * i_ac_pu * i_ac_pu)
            * base_mva
    }

    /// Clamp Q setpoints to converter capability limits.
    pub fn clamp_q_from(&self, q: f64) -> f64 {
        q.clamp(self.q_min_from_mvar, self.q_max_from_mvar)
    }

    /// Clamp Q setpoints to converter capability limits.
    pub fn clamp_q_to(&self, q: f64) -> f64 {
        q.clamp(self.q_min_to_mvar, self.q_max_to_mvar)
    }
}

/// A single HVDC link — either LCC or VSC technology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HvdcLink {
    /// Line-Commutated Converter (classic thyristor HVDC).
    Lcc(LccHvdcLink),
    /// Voltage-Source Converter (modern IGBT HVDC).
    Vsc(VscHvdcLink),
}

impl HvdcLink {
    /// Bus number at the rectifier (power source) end.
    pub fn from_bus(&self) -> u32 {
        match self {
            HvdcLink::Lcc(p) => p.from_bus,
            HvdcLink::Vsc(p) => p.from_bus,
        }
    }

    /// Bus number at the inverter (power sink) end.
    pub fn to_bus(&self) -> u32 {
        match self {
            HvdcLink::Lcc(p) => p.to_bus,
            HvdcLink::Vsc(p) => p.to_bus,
        }
    }

    /// DC power setpoint in MW.
    pub fn p_dc_mw(&self) -> f64 {
        match self {
            HvdcLink::Lcc(p) => p.p_dc_mw,
            HvdcLink::Vsc(p) => p.p_dc_mw,
        }
    }

    /// Link name.
    pub fn name(&self) -> &str {
        match self {
            HvdcLink::Lcc(p) => &p.name,
            HvdcLink::Vsc(p) => &p.name,
        }
    }
}
