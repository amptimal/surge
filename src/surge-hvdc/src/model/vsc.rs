// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! VSC HVDC power flow equations.
//!
//! Models a Voltage-Source Converter HVDC link.  The loss model is the full
//! quadratic IGBT converter model (FPQ-43 / P5-022):
//!
//!   P_loss = (a + b × |I_ac| + c × I_ac²) × base_mva
//!
//! where |I_ac| is the per-unit apparent current at the AC terminal:
//!   |I_ac| = |S_ac| / V_ac = sqrt(P² + Q²) / (V_ac × base_mva)
//!
//! Setting `c = 0` (and `a = 0`) recovers the earlier linear-only model.
//!
//! Control modes (PLAN-098 / FPQ-55 / P5-064):
//!   Each station carries a `VscHvdcControlMode` that governs how P and Q are
//!   determined at each sequential AC-DC iteration step.
//!
//! Droop control (PLAN-099 / FPQ-56 / P5-065):
//!   `VscHvdcControlMode::PVdcDroop` couples the station's active power to the DC
//!   bus voltage, distributing imbalances across MTDC stations.

use tracing::debug;

use crate::model::control::{VscHvdcControlMode, VscStationState};
use crate::model::link::VscHvdcLink;
use crate::result::{HvdcStationSolution, HvdcTechnology};

/// Compute VSC converter losses in MW from loss coefficients and AC terminal conditions.
///
/// # Model
///
/// ```text
/// I_ac = sqrt(P² + Q²) / (V_ac × base_mva)   [per-unit]
/// P_loss = (a + b × I_ac + c × I_ac²) × base_mva   [MW]
/// ```
///
/// With `a = 0, c = 0` this reduces to the original linear model.
///
/// This is the single canonical implementation of the quadratic IGBT loss model
/// used by all VSC converter types in the crate.
pub fn compute_vsc_losses_mw(
    loss_a: f64,
    loss_b: f64,
    loss_c: f64,
    p_mw: f64,
    q_mvar: f64,
    v_ac_pu: f64,
    base_mva: f64,
) -> f64 {
    let s_mva = (p_mw * p_mw + q_mvar * q_mvar).sqrt();
    let i_ac_pu = if v_ac_pu > 1e-6 {
        s_mva / (v_ac_pu * base_mva)
    } else {
        0.0
    };
    (loss_a + loss_b * i_ac_pu + loss_c * i_ac_pu * i_ac_pu) * base_mva
}

/// Compute VSC losses in MW for a point-to-point [`VscHvdcLink`].
///
/// Delegates to [`compute_vsc_losses_mw`] using the link's loss coefficients.
pub fn vsc_losses_mw(
    params: &VscHvdcLink,
    p_mw: f64,
    q_mvar: f64,
    v_pu: f64,
    base_mva: f64,
) -> f64 {
    compute_vsc_losses_mw(
        params.loss_coeff_a_mw,
        params.loss_coeff_b_pu,
        params.loss_c_pu,
        p_mw,
        q_mvar,
        v_pu,
        base_mva,
    )
}

/// P/Q injections that represent a VSC link in the AC network.
///
/// Rectifier bus: negative P injection (draws power), Q per setpoint.
/// Inverter bus:  positive P injection (sources power minus losses), Q per setpoint.
#[derive(Debug, Clone, Copy)]
pub struct VscInjections {
    /// P injection at rectifier bus in pu (negative = load).
    pub p_from_pu: f64,
    /// Q injection at rectifier bus in pu.
    pub q_from_pu: f64,
    /// P injection at inverter bus in pu (positive = source).
    pub p_to_pu: f64,
    /// Q injection at inverter bus in pu.
    pub q_to_pu: f64,
}

/// Compute VSC P/Q injections for the AC network given losses in MW.
pub fn vsc_injections(params: &VscHvdcLink, p_loss_mw: f64, base_mva: f64) -> VscInjections {
    let p_from_pu = -params.p_dc_mw / base_mva;
    let q_from_pu = params.clamp_q_from(params.q_from_mvar) / base_mva;
    let p_to_pu = (params.p_dc_mw - p_loss_mw) / base_mva;
    let q_to_pu = params.clamp_q_to(params.q_to_mvar) / base_mva;
    VscInjections {
        p_from_pu,
        q_from_pu,
        p_to_pu,
        q_to_pu,
    }
}

/// Build two [`HvdcStationSolution`]s for a VSC link (rectifier + inverter).
///
/// Uses the standard `ConstantPQ` logic: P and Q are taken from `VscHvdcLink`
/// setpoints, and losses are computed with the quadratic model.
///
/// Returns `[rectifier, inverter]`.
pub fn vsc_converter_results(
    params: &VscHvdcLink,
    v_from: f64,
    v_to: f64,
    base_mva: f64,
) -> [HvdcStationSolution; 2] {
    let q_from = params.clamp_q_from(params.q_from_mvar);
    let q_to = params.clamp_q_to(params.q_to_mvar);
    let name = (!params.name.is_empty()).then(|| params.name.clone());

    // Compute losses based on rectifier current magnitude (full quadratic model).
    let p_loss = vsc_losses_mw(params, params.p_dc_mw, q_from.abs(), v_from, base_mva);

    let p_to = params.p_dc_mw - p_loss;
    let _ = v_to; // available for future voltage-dependent model extensions

    debug!(
        from_bus = params.from_bus,
        to_bus = params.to_bus,
        p_dc_mw = params.p_dc_mw,
        p_loss_mw = p_loss,
        p_to_mw = p_to,
        q_from_mvar = q_from,
        q_to_mvar = q_to,
        v_from = v_from,
        "VSC converter result (ConstantPQ)"
    );

    let rectifier = HvdcStationSolution {
        name: name.clone(),
        technology: HvdcTechnology::Vsc,
        ac_bus: params.from_bus,
        dc_bus: None,
        p_ac_mw: -params.p_dc_mw,
        q_ac_mvar: q_from,
        p_dc_mw: params.p_dc_mw,
        v_dc_pu: 1.0,
        converter_loss_mw: 0.0,
        lcc_detail: None,
        converged: true,
    };

    let inverter = HvdcStationSolution {
        name,
        technology: HvdcTechnology::Vsc,
        ac_bus: params.to_bus,
        dc_bus: None,
        p_ac_mw: p_to,
        q_ac_mvar: q_to,
        p_dc_mw: -params.p_dc_mw,
        v_dc_pu: 1.0,
        converter_loss_mw: p_loss,
        lcc_detail: None,
        converged: true,
    };

    [rectifier, inverter]
}

/// Build two [`HvdcStationSolution`]s for a VSC link operating under an explicit
/// `VscHvdcControlMode`, using the current station state from the AC-DC iteration.
///
/// Returns `[rectifier, inverter]`.
pub fn vsc_converter_results_with_mode(
    params: &VscHvdcLink,
    mode: &VscHvdcControlMode,
    state: &VscStationState,
    v_from: f64,
    v_to: f64,
    base_mva: f64,
) -> [HvdcStationSolution; 2] {
    // Determine effective P from control mode and current DC bus voltage.
    let p_dc_mw = mode.effective_p_mw(state.v_dc_pu);

    // Determine effective Q at the rectifier terminal.
    let q_from_raw = mode.effective_q_mvar(v_from, state.q_mvar, params.q_from_mvar);
    let q_from = q_from_raw.clamp(params.q_min_from_mvar, params.q_max_from_mvar);

    // Inverter-side Q: taken from params setpoint for all modes.
    let q_to = params.clamp_q_to(params.q_to_mvar);

    // Compute losses using the quadratic model.
    let p_loss = vsc_losses_mw(params, p_dc_mw, q_from.abs(), v_from, base_mva);

    let p_to = p_dc_mw - p_loss;
    let _ = v_to;
    let name = (!params.name.is_empty()).then(|| params.name.clone());

    debug!(
        from_bus = params.from_bus,
        to_bus = params.to_bus,
        p_dc_mw = p_dc_mw,
        p_loss_mw = p_loss,
        p_to_mw = p_to,
        q_from_mvar = q_from,
        q_to_mvar = q_to,
        v_dc_pu = state.v_dc_pu,
        v_from = v_from,
        "VSC converter result (with control mode)"
    );

    let rectifier = HvdcStationSolution {
        name: name.clone(),
        technology: HvdcTechnology::Vsc,
        ac_bus: params.from_bus,
        dc_bus: None,
        p_ac_mw: -p_dc_mw,
        q_ac_mvar: q_from,
        p_dc_mw,
        v_dc_pu: state.v_dc_pu,
        converter_loss_mw: 0.0,
        lcc_detail: None,
        converged: true,
    };

    let inverter = HvdcStationSolution {
        name,
        technology: HvdcTechnology::Vsc,
        ac_bus: params.to_bus,
        dc_bus: None,
        p_ac_mw: p_to,
        q_ac_mvar: q_to,
        p_dc_mw: -p_dc_mw,
        v_dc_pu: state.v_dc_pu,
        converter_loss_mw: p_loss,
        lcc_detail: None,
        converged: true,
    };

    [rectifier, inverter]
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_MVA: f64 = 100.0;

    // ── FPQ-43 / P5-022: Quadratic loss model ────────────────────────────────

    /// Helper to build a VscHvdcLink with explicit a/b/c coefficients.
    fn make_params(a: f64, b: f64, c: f64) -> VscHvdcLink {
        let mut p = VscHvdcLink::new(1, 2, 200.0);
        p.loss_coeff_a_mw = a;
        p.loss_coeff_b_pu = b;
        p.loss_c_pu = c;
        p
    }

    #[test]
    fn fpq43_quadratic_loss_no_load_losses() {
        // a = 0.003 pu, b = 0, c = 0 → P_loss = a × base_mva = 0.3 MW
        let params = make_params(0.003, 0.0, 0.0);
        let loss = vsc_losses_mw(&params, 0.0, 0.0, 1.0, BASE_MVA);
        assert!(
            (loss - 0.3).abs() < 1e-9,
            "No-load loss should be 0.3 MW, got {loss}"
        );
    }

    #[test]
    fn fpq43_quadratic_loss_linear_only() {
        // a = 0, b = 0.01 pu, c = 0 → same as old model
        // I = S / (V × base) = 200 / (1.0 × 100) = 2.0 pu
        // P_loss = b × I × base_mva = 0.01 × 2.0 × 100 = 2.0 MW
        let params = make_params(0.0, 0.01, 0.0);
        let loss = vsc_losses_mw(&params, 200.0, 0.0, 1.0, BASE_MVA);
        assert!(
            (loss - 2.0).abs() < 1e-9,
            "Linear loss should be 2.0 MW, got {loss}"
        );
    }

    #[test]
    fn fpq43_quadratic_loss_full_model() {
        // a = 0.003, b = 0.01, c = 0.02 pu
        // I = 200 / (1.0 × 100) = 2.0 pu
        // P_loss = (0.003 + 0.01 × 2.0 + 0.02 × 4.0) × 100
        //        = (0.003 + 0.020 + 0.080) × 100
        //        = 0.103 × 100 = 10.3 MW
        let params = make_params(0.003, 0.01, 0.02);
        let loss = vsc_losses_mw(&params, 200.0, 0.0, 1.0, BASE_MVA);
        let expected = (0.003 + 0.01 * 2.0 + 0.02 * 4.0) * BASE_MVA;
        assert!(
            (loss - expected).abs() < 1e-9,
            "Full quadratic loss: expected {expected:.4} MW, got {loss:.4}"
        );
    }

    #[test]
    fn fpq43_loss_increases_with_current() {
        // With all three coefficients non-zero, loss must be strictly
        // monotonically increasing with current (i.e., with P_dc at fixed V).
        let params = make_params(0.003, 0.01, 0.02);

        let loss_100 = vsc_losses_mw(&params, 100.0, 0.0, 1.0, BASE_MVA);
        let loss_200 = vsc_losses_mw(&params, 200.0, 0.0, 1.0, BASE_MVA);
        let loss_300 = vsc_losses_mw(&params, 300.0, 0.0, 1.0, BASE_MVA);

        assert!(
            loss_100 < loss_200 && loss_200 < loss_300,
            "Loss must increase with current: {loss_100:.3} < {loss_200:.3} < {loss_300:.3}"
        );
    }

    #[test]
    fn fpq43_zero_c_equals_linear_model() {
        // Setting c = 0 must reproduce the old linear result exactly.
        let mut params_linear = VscHvdcLink::new(1, 2, 150.0);
        params_linear.loss_coeff_a_mw = 0.002;
        params_linear.loss_coeff_b_pu = 0.015;
        // loss_c_pu defaults to 0.0

        let mut params_quad = params_linear.clone();
        params_quad.loss_c_pu = 0.0;

        let loss_l = vsc_losses_mw(&params_linear, 150.0, 0.0, 1.0, BASE_MVA);
        let loss_q = vsc_losses_mw(&params_quad, 150.0, 0.0, 1.0, BASE_MVA);
        assert!(
            (loss_l - loss_q).abs() < 1e-12,
            "c=0 must match the linear model: linear={loss_l}, quad={loss_q}"
        );
    }

    // ── PLAN-098 / FPQ-55 / P5-064: VSC control mode integration ─────────────

    #[test]
    fn control_mode_constant_pq_matches_base_result() {
        let mut params = VscHvdcLink::new(1, 2, 100.0);
        params.q_from_mvar = 20.0;

        let mode = VscHvdcControlMode::ConstantPQ {
            p_set: 100.0,
            q_set: 20.0,
        };
        let state = VscStationState::new(100.0, 20.0);

        let [rect_base, _inv_base] = vsc_converter_results(&params, 1.0, 1.0, BASE_MVA);
        let [rect_mode, _inv_mode] =
            vsc_converter_results_with_mode(&params, &mode, &state, 1.0, 1.0, BASE_MVA);

        assert!(
            (rect_base.p_ac_mw - rect_mode.p_ac_mw).abs() < 1e-9,
            "p_ac must match between base and ConstantPQ mode"
        );
        assert!(
            (rect_base.q_ac_mvar - rect_mode.q_ac_mvar).abs() < 1e-9,
            "q_ac must match between base and ConstantPQ mode"
        );
    }

    #[test]
    fn control_mode_droop_changes_p_with_vdc() {
        let params = VscHvdcLink::new(1, 2, 100.0);
        let mode = VscHvdcControlMode::PVdcDroop {
            p_set: 100.0,
            voltage_dc_setpoint_pu: 1.0,
            k_droop: 50.0,
            p_min: 50.0,
            p_max: 150.0,
        };
        // v_dc = 1.02 → P = 100 + 50 * 0.02 = 101 MW
        let state = VscStationState {
            p_mw: 100.0,
            q_mvar: 0.0,
            v_dc_pu: 1.02,
        };
        let [rect, _inv] =
            vsc_converter_results_with_mode(&params, &mode, &state, 1.0, 1.0, BASE_MVA);
        assert!(
            (rect.p_dc_mw - 101.0).abs() < 1e-9,
            "Droop mode: expected p_dc=101.0 MW, got {}",
            rect.p_dc_mw
        );
    }
}
