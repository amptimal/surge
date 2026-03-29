// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! LCC HVDC power flow equations and automatic tap control.
//!
//! Models a bipolar LCC link with four control modes (ConstantPower,
//! ConstantCurrent, ConstantAlpha, and VDCOL), including commutation
//! reactance voltage drop and iterative DC voltage/current coupling.
//!
//! # DC Voltage Equations
//!
//! The DC voltage at each converter end is governed by the standard
//! 6-pulse bridge equation (see Kimbark, "Direct Current Transmission",
//! Vol. 1; Arrillaga, "High Voltage Direct Current Transmission", 2nd ed.):
//!
//! ```text
//!   Vd_R = (3√2/π) × a_R × V_ac_R × cos(α) − (3/π) × X_c_R × I_d
//!   Vd_I = (3√2/π) × a_I × V_ac_I × cos(γ) − (3/π) × X_c_I × I_d
//! ```
//!
//! where `(3/π) × X_c × I_d` is the commutation reactance voltage drop.
//! During each commutation interval, two thyristor valves conduct
//! simultaneously, and the transformer leakage reactance limits the rate
//! of current transfer. The overlap angle μ increases with DC current,
//! reducing the average DC output voltage.
//!
//! # Voltage-Current Coupling
//!
//! The DC voltage depends on the DC current through the commutation
//! reactance drop, and the DC current depends on the DC voltage through
//! the power balance equation. This nonlinear coupling is resolved by
//! iterating in `compute_lcc_operating_point`:
//!
//!   1. Compute no-load DC voltages (Vd0_R, Vd0_I) ignoring commutation drop
//!   2. Estimate I_d from the no-load voltage
//!   3. Update Vd_R, Vd_I with the commutation drop term
//!   4. Recompute I_d from the power balance: I_d = P / (Vd_R × base_mva)
//!   5. Repeat until |ΔI_d| < 1e-9
//!
//! With `x_c_r = x_c_i = 0.0` (the default), the commutation reactance
//! drop vanishes and the model reduces to the simplified constant-power
//! formulation.
//!
//! # Tap Control (FPQ-41)
//!
//! `TapControl` implements automatic converter transformer tap selection to
//! maintain a target DC voltage at the rectifier.  The continuous optimal tap is
//!
//! ```text
//!   a_R = (Vd_nom × π) / (3√2 × V_ac × cos(α))
//! ```
//!
//! which is then rounded to the nearest discrete tap position and clamped to
//! `[a_min, a_max]`.

use tracing::debug;

use crate::model::control::LccHvdcControlMode;
use crate::model::link::LccHvdcLink;
use crate::result::{HvdcLccDetail, HvdcStationSolution, HvdcTechnology};

/// Automatic converter transformer tap controller for an LCC rectifier.
///
/// Given a target DC voltage, a range of available tap positions, and the
/// number of discrete steps, `select_tap` returns the optimal tap ratio
/// `a_R` that best achieves the target while honouring the tap limits.
///
/// # Example
///
/// Set `target_vd_r_pu` to the nominal DC voltage produced by a unit tap at
/// the operating point (`k × v_ac × cos(α) ≈ 1.3045` for α = 15°, v_ac = 1.0)
/// so that `select_tap` returns a tap close to 1.0:
///
/// ```rust
/// use surge_hvdc::TapControl;
///
/// // k × cos(15°) ≈ 1.3045 — the DC voltage a unit tap would produce.
/// let k_cos_alpha: f64 = 3.0 * std::f64::consts::SQRT_2 / std::f64::consts::PI
///     * 15_f64.to_radians().cos();
/// let ctrl = TapControl {
///     a_min: 0.9,
///     a_max: 1.1,
///     n_taps: 33,
///     // target_vd_r_pu: desired DC voltage in per-unit on the DC base
///     target_vd_r_pu: k_cos_alpha, // target that maps to a_r ≈ 1.0
/// };
/// let alpha_rad: f64 = 15_f64.to_radians();
/// let tap = ctrl.select_tap(1.0, alpha_rad.cos());
/// assert!((tap - 1.0).abs() < 0.01);
/// ```
#[derive(Debug, Clone)]
pub struct TapControl {
    /// Minimum tap ratio (e.g. 0.9 for ±10 % range).
    pub a_min: f64,
    /// Maximum tap ratio (e.g. 1.1 for ±10 % range).
    pub a_max: f64,
    /// Number of discrete tap positions (including both endpoints).
    /// A value of 1 means a single fixed tap at `a_min`.  A value of 33
    /// gives a step size of (a_max−a_min)/32.
    pub n_taps: u32,
    /// Target DC voltage at the rectifier in per-unit.
    pub target_vd_r_pu: f64,
}

impl TapControl {
    /// Select the optimal discrete tap ratio to achieve `target_vd_r_pu`.
    ///
    /// The ideal continuous tap that satisfies the DC voltage equation at
    /// constant angle α is:
    ///
    ///   a_R = (Vd_nom × π) / (3√2 × V_ac × cos(α))
    ///
    /// This is rounded to the nearest tap position in `[a_min, a_max]`.
    /// If the target is unreachable (i.e. the ideal tap lies outside the
    /// range) the nearest limit is returned.
    ///
    /// # Arguments
    /// * `v_ac`     — AC bus voltage magnitude at the rectifier (pu).
    /// * `cos_alpha` — Cosine of the firing angle α (dimensionless).
    ///
    pub fn select_tap(&self, v_ac: f64, cos_alpha: f64) -> f64 {
        let k = 3.0 * std::f64::consts::SQRT_2 / std::f64::consts::PI;
        let denominator = k * v_ac * cos_alpha;

        // Compute ideal continuous tap; guard against non-positive denominator.
        // When cos_alpha <= 0 (firing angle >= 90°, commutation failure region),
        // denominator <= 0 and a_ideal would be negative — return a_max as fallback.
        let a_ideal = if denominator <= 0.0 {
            // cos_alpha <= 0: firing angle >= 90°, commutation failure region.
            // Return a_max (maximum step-up) as the least-bad fallback.
            debug!(
                v_ac = v_ac,
                cos_alpha = cos_alpha,
                a_max = self.a_max,
                "TapControl: non-positive denominator (cos_alpha <= 0), returning a_max"
            );
            self.a_max
        } else {
            self.target_vd_r_pu / denominator
        };

        // Clamp to physically achievable range.
        let a_clamped = a_ideal.clamp(self.a_min, self.a_max);

        // Round to nearest discrete step.
        // Guard: n_taps = 0 would underflow (u32), n_taps = 1 yields n_steps = 0
        // causing divide-by-zero (+inf step_size). Both map to a single fixed tap.
        if self.n_taps < 2 {
            debug!(
                a_min = self.a_min,
                "TapControl: n_taps < 2, returning fixed tap a_min"
            );
            return self.a_min;
        }
        let n_steps = (self.n_taps - 1) as f64;
        let step_size = (self.a_max - self.a_min) / n_steps;
        let step_index = ((a_clamped - self.a_min) / step_size).round();
        let tap = self.a_min + step_index * step_size;
        debug!(
            v_ac = v_ac,
            cos_alpha = cos_alpha,
            a_ideal = a_ideal,
            a_clamped = a_clamped,
            tap = tap,
            "TapControl: selected discrete tap"
        );
        tap
    }
}

/// Operating point computed from LCC equations.
#[derive(Debug, Clone)]
pub struct LccOperatingPoint {
    /// DC voltage at rectifier in pu (system base kV / nominal DC kV).
    pub vd_r_pu: f64,
    /// DC voltage at inverter in pu.
    pub vd_i_pu: f64,
    /// DC current in per-unit on system base.
    pub i_dc_pu: f64,
    /// DC power delivered to inverter in MW (= Vd_I × I_d × system_base).
    /// The rectifier draws `p_dc_mw + p_loss_mw` from the AC network.
    pub p_dc_mw: f64,
    /// DC line losses in MW.
    pub p_loss_mw: f64,
    /// Rectifier firing angle α in degrees (solved from control mode).
    pub alpha_deg: f64,
}

/// Compute the LCC operating point given AC bus voltage magnitudes.
///
/// Implements the standard LCC DC voltage equations with commutation reactance
/// voltage drop (see CIGRE B4 / Kimbark / Arrillaga formulations):
///
/// ```text
///   Vd_R = (3√2/π) × a_r × V_ac_R × cos(α) − (3/π) × X_c_R × I_d
///   Vd_I = (3√2/π) × a_i × V_ac_I × cos(γ) − (3/π) × X_c_I × I_d
/// ```
///
/// where the `(3/π) × X_c × I_d` term represents the DC voltage drop due to
/// commutation overlap. During commutation (typically 20-30° overlap), two
/// thyristor valves conduct simultaneously and the commutation reactance
/// effectively short-circuits a portion of the AC voltage, reducing the
/// average DC output voltage.
///
/// # Voltage-Current Coupling
///
/// When `x_c_r > 0` or `x_c_i > 0`, the DC voltages depend on the DC current,
/// which in turn depends on the DC voltages. This creates a nonlinear coupling
/// that is resolved iteratively:
///
/// 1. Compute no-load DC voltages: `Vd0_R = k × a_r × V_ac_R × cos(α)`
/// 2. Estimate initial `I_d` from the no-load voltage
/// 3. Iterate: update `Vd_R`, `Vd_I` accounting for commutation drop, then
///    recompute `I_d` from `P_dc / Vd_R` plus line losses
/// 4. Converge when `|I_d_new - I_d| < 1e-9`
///
/// With `x_c_r = x_c_i = 0.0` (the default), the commutation reactance drop
/// vanishes and the result is identical to the simplified constant-power model.
///
/// # Arguments
/// * `params`    -- LCC link parameters (including `a_r`, `a_i`, `x_c_r`, `x_c_i`)
/// * `v_ac_r`   -- AC voltage magnitude at rectifier bus (pu)
/// * `v_ac_i`   -- AC voltage magnitude at inverter bus (pu)
/// * `base_mva` -- System base MVA
///
/// Returns the computed operating point.
pub fn compute_lcc_operating_point(
    params: &LccHvdcLink,
    v_ac_r: f64,
    v_ac_i: f64,
    base_mva: f64,
) -> LccOperatingPoint {
    debug!(
        from_bus = params.from_bus,
        to_bus = params.to_bus,
        p_dc_mw = params.p_dc_mw,
        v_ac_r = v_ac_r,
        v_ac_i = v_ac_i,
        firing_angle_deg = params.firing_angle_deg,
        extinction_angle_deg = params.extinction_angle_deg,
        alpha_min_deg = params.alpha_min_deg,
        a_r = params.a_r,
        a_i = params.a_i,
        x_c_r = params.x_c_r,
        x_c_i = params.x_c_i,
        "LCC operating point computation starting"
    );

    // Bridge constant: k = 3√2/π ≈ 1.3505 for a 6-pulse bridge.
    let k = 3.0 * std::f64::consts::SQRT_2 / std::f64::consts::PI;
    // Commutation reactance voltage drop constant: k_xc = 3/π ≈ 0.9549.
    let k_xc = 3.0 / std::f64::consts::PI;

    let gamma_rad = params.extinction_angle_deg.to_radians();
    // Firing angle for use in ConstantPower and ConstantCurrent modes —
    // bounded below by alpha_min_deg to prevent commutation failure.
    let alpha_rad = params
        .firing_angle_deg
        .max(params.alpha_min_deg)
        .to_radians();

    let k_r = k * params.a_r;
    let k_i = k * params.a_i;

    // No-load DC voltages at current AC bus voltages.
    let vd0_r = k_r * v_ac_r * alpha_rad.cos();
    let vd0_i = k_i * v_ac_i * gamma_rad.cos();

    let r_dc = params.r_dc_pu;
    let xc_r = params.x_c_r;
    let xc_i = params.x_c_i;

    let op = match &params.control_mode {
        LccHvdcControlMode::ConstantPower => {
            // Iteratively solve for I_d such that P_rect = P_dc + P_loss.
            let p_dc = params.p_dc_mw;
            let mut id = p_dc / (vd0_r.max(0.01) * base_mva);
            for _ in 0..30 {
                let vd_r = vd0_r - k_xc * xc_r * id;
                let p_loss_line = id * id * r_dc * base_mva;
                let id_new = (p_dc + p_loss_line) / (vd_r.max(0.01) * base_mva);
                if (id_new - id).abs() < 1e-9 {
                    id = id_new;
                    break;
                }
                id = id_new;
            }
            let vd_r_pu = vd0_r - k_xc * xc_r * id;
            let vd_i_pu = vd0_i - k_xc * xc_i * id;
            let p_loss_mw = id * id * r_dc * base_mva;
            LccOperatingPoint {
                vd_r_pu,
                vd_i_pu,
                i_dc_pu: id,
                p_dc_mw: p_dc,
                p_loss_mw,
                alpha_deg: params.firing_angle_deg.max(params.alpha_min_deg),
            }
        }

        LccHvdcControlMode::ConstantCurrent { i_d_pu } => {
            // Fixed DC current; power varies with AC voltage.
            let id = *i_d_pu;
            let vd_r_pu = (vd0_r - k_xc * xc_r * id).max(0.0);
            let vd_i_pu = (vd0_i - k_xc * xc_i * id).max(0.0);
            let p_loss_mw = id * id * r_dc * base_mva;
            let p_dc_mw = vd_i_pu * id * base_mva;
            LccOperatingPoint {
                vd_r_pu,
                vd_i_pu,
                i_dc_pu: id,
                p_dc_mw,
                p_loss_mw,
                alpha_deg: params.firing_angle_deg.max(params.alpha_min_deg),
            }
        }

        LccHvdcControlMode::ConstantAlpha { alpha_deg } => {
            // Fixed firing angle; solve for I_d from the Thevenin equivalent.
            // Vd_R - Vd_I = I_d × R_dc  (with commutation drops on both sides)
            // (Vd0_R_α - k_xc×xc_r×I_d) - (Vd0_I - k_xc×xc_i×I_d) = I_d×R_dc
            // → I_d = (Vd0_R_α - Vd0_I) / (R_dc + k_xc×(xc_r - xc_i))
            let alpha = alpha_deg.max(params.alpha_min_deg).to_radians();
            let vd0_r_alpha = k_r * v_ac_r * alpha.cos();
            // denom = R_dc + k_xc*(xc_r - xc_i).  Normally positive because
            // R_dc ≥ 0 and xc_r ≥ xc_i in typical LCC designs.  If xc_i >
            // xc_r + R_dc/k_xc (inverter has much larger commutation reactance),
            // denom goes negative; clamping I_d to 0.0 is the safe fallback
            // (no power transfer at this operating point).
            let denom = r_dc + k_xc * (xc_r - xc_i);
            let id = if denom.abs() > 1e-12 {
                ((vd0_r_alpha - vd0_i) / denom).max(0.0)
            } else {
                0.0
            };
            let vd_r_pu = (vd0_r_alpha - k_xc * xc_r * id).max(0.0);
            let vd_i_pu = (vd0_i - k_xc * xc_i * id).max(0.0);
            let p_loss_mw = id * id * r_dc * base_mva;
            let p_dc_mw = vd_i_pu * id * base_mva;
            LccOperatingPoint {
                vd_r_pu,
                vd_i_pu,
                i_dc_pu: id,
                p_dc_mw,
                p_loss_mw,
                alpha_deg: alpha_deg.max(params.alpha_min_deg),
            }
        }

        LccHvdcControlMode::Vdcol {
            i_order_pu,
            v_high_pu,
            v_low_pu,
            i_min_pu,
        } => {
            // Iteratively apply VDCOL characteristic.
            // V_dc estimate is the rectifier-side DC voltage Vd_R.
            let mut id = *i_order_pu;
            for _ in 0..30 {
                let vd_r = (vd0_r - k_xc * xc_r * id).max(0.0);
                let i_cmd = if vd_r >= *v_high_pu {
                    *i_order_pu
                } else if vd_r <= *v_low_pu {
                    *i_min_pu
                } else {
                    let frac = (vd_r - v_low_pu) / (v_high_pu - v_low_pu);
                    i_min_pu + frac * (i_order_pu - i_min_pu)
                };
                if (i_cmd - id).abs() < 1e-9 {
                    id = i_cmd;
                    break;
                }
                id = i_cmd;
            }
            let vd_r_pu = (vd0_r - k_xc * xc_r * id).max(0.0);
            let vd_i_pu = (vd0_i - k_xc * xc_i * id).max(0.0);
            let p_loss_mw = id * id * r_dc * base_mva;
            let p_dc_mw = vd_i_pu * id * base_mva;
            LccOperatingPoint {
                vd_r_pu,
                vd_i_pu,
                i_dc_pu: id,
                p_dc_mw,
                p_loss_mw,
                alpha_deg: params.firing_angle_deg.max(params.alpha_min_deg),
            }
        }
    };

    debug!(
        vd_r_pu = op.vd_r_pu,
        vd_i_pu = op.vd_i_pu,
        i_dc_pu = op.i_dc_pu,
        p_dc_mw = op.p_dc_mw,
        p_loss_mw = op.p_loss_mw,
        "LCC operating point computed"
    );

    op
}

/// Build an `HvdcStationSolution` from an LCC operating point.
///
/// The rectifier draws (P_dc + P_loss) from the AC network and absorbs
/// reactive power Q_R. The inverter injects P_dc into the AC network and
/// absorbs Q_I.
/// Build two [`HvdcStationSolution`]s for an LCC link (rectifier + inverter).
///
/// Returns `[rectifier, inverter]`.
pub fn lcc_converter_results(
    params: &LccHvdcLink,
    op: &LccOperatingPoint,
) -> [HvdcStationSolution; 2] {
    let p_rect = op.p_dc_mw + op.p_loss_mw;
    let q_r = params.q_rectifier_mvar(op.p_dc_mw);
    let q_i = params.q_inverter_mvar(op.p_dc_mw);
    let name = (!params.name.is_empty()).then(|| params.name.clone());

    debug!(
        from_bus = params.from_bus,
        to_bus = params.to_bus,
        p_dc_mw = op.p_dc_mw,
        p_loss_mw = op.p_loss_mw,
        dc_current_ka = op.i_dc_pu,
        dc_voltage_pu = op.vd_r_pu,
        "LCC converter results built"
    );

    let rectifier = HvdcStationSolution {
        name: name.clone(),
        technology: HvdcTechnology::Lcc,
        ac_bus: params.from_bus,
        dc_bus: None,
        // Rectifier: draws from AC (negative injection convention)
        p_ac_mw: -p_rect,
        // LCC always absorbs reactive power (negative = absorbed)
        q_ac_mvar: -q_r,
        p_dc_mw: op.p_dc_mw,
        v_dc_pu: op.vd_r_pu,
        converter_loss_mw: op.p_loss_mw,
        lcc_detail: Some(HvdcLccDetail {
            alpha_deg: op.alpha_deg,
            gamma_deg: params.extinction_angle_deg,
            i_dc_pu: op.i_dc_pu,
            power_factor: params.power_factor_r,
        }),
        converged: true,
    };

    let inverter = HvdcStationSolution {
        name,
        technology: HvdcTechnology::Lcc,
        ac_bus: params.to_bus,
        dc_bus: None,
        // Inverter: injects into AC (positive injection convention)
        p_ac_mw: op.p_dc_mw,
        // LCC inverter also absorbs reactive power
        q_ac_mvar: -q_i,
        p_dc_mw: -op.p_dc_mw,
        v_dc_pu: op.vd_i_pu,
        converter_loss_mw: 0.0, // DC line losses are attributed to the rectifier-side terminal
        lcc_detail: Some(HvdcLccDetail {
            alpha_deg: 0.0, // inverter uses extinction angle, not firing angle
            gamma_deg: params.extinction_angle_deg,
            i_dc_pu: op.i_dc_pu,
            power_factor: params.power_factor_i,
        }),
        converged: true,
    };

    [rectifier, inverter]
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const SQRT_2: f64 = std::f64::consts::SQRT_2;
    const BASE_MVA: f64 = 100.0;

    // Helper: build baseline LCC params (a_r = a_i = 1.0).
    fn base_params() -> LccHvdcLink {
        LccHvdcLink::new(1, 2, 200.0)
    }

    // ── FPQ-41-A: a_r = 1.0, a_i = 1.0 gives same result as pre-FPQ-41 ──────

    #[test]
    fn fpq41_unity_taps() {
        let params = base_params();
        assert_eq!(params.a_r, 1.0, "default a_r must be 1.0");
        assert_eq!(params.a_i, 1.0, "default a_i must be 1.0");

        // With unity taps the k_r / k_i factors equal k = 3√2/π.
        let k = 3.0 * SQRT_2 / PI;
        let v_ac = 1.0;
        let alpha_rad = params.firing_angle_deg.to_radians();
        let gamma_rad = params.extinction_angle_deg.to_radians();

        let op = compute_lcc_operating_point(&params, v_ac, v_ac, BASE_MVA);

        let expected_vd_r = k * v_ac * alpha_rad.cos();
        let expected_vd_i = k * v_ac * gamma_rad.cos();

        assert!(
            (op.vd_r_pu - expected_vd_r).abs() < 1e-10,
            "vd_r_pu mismatch: got {}, expected {}",
            op.vd_r_pu,
            expected_vd_r
        );
        assert!(
            (op.vd_i_pu - expected_vd_i).abs() < 1e-10,
            "vd_i_pu mismatch: got {}, expected {}",
            op.vd_i_pu,
            expected_vd_i
        );
        assert_eq!(op.p_dc_mw, params.p_dc_mw);
    }

    // ── FPQ-41-B: higher a_r increases rectifier DC voltage ──────────────────

    #[test]
    fn fpq41_b_higher_a_r_increases_vd_r() {
        let mut params_base = base_params();
        params_base.a_r = 1.0;

        let mut params_high = base_params();
        params_high.a_r = 1.05;

        let v_ac = 1.0;
        let op_base = compute_lcc_operating_point(&params_base, v_ac, v_ac, BASE_MVA);
        let op_high = compute_lcc_operating_point(&params_high, v_ac, v_ac, BASE_MVA);

        assert!(
            op_high.vd_r_pu > op_base.vd_r_pu,
            "higher a_r must produce higher Vd_R: base={}, high={}",
            op_base.vd_r_pu,
            op_high.vd_r_pu
        );

        // The increase should be proportional to the tap ratio change.
        let ratio = op_high.vd_r_pu / op_base.vd_r_pu;
        assert!(
            (ratio - 1.05).abs() < 1e-10,
            "Vd_R should scale linearly with a_r: expected ratio 1.05, got {}",
            ratio
        );
    }

    // ── FPQ-41-C: TapControl::select_tap returns tap within [a_min, a_max] ──

    #[test]
    fn fpq41_c_select_tap_within_bounds() {
        let ctrl = TapControl {
            a_min: 0.9,
            a_max: 1.1,
            n_taps: 33,
            target_vd_r_pu: 1.0,
        };
        let alpha_rad: f64 = 15_f64.to_radians();
        // Test across a range of AC voltages.
        for v_scale in [0.85, 0.90, 0.95, 1.00, 1.05, 1.10, 1.15] {
            let tap = ctrl.select_tap(v_scale, alpha_rad.cos());
            assert!(
                tap >= ctrl.a_min && tap <= ctrl.a_max,
                "tap {tap} out of bounds [{}, {}] at v_ac={v_scale}",
                ctrl.a_min,
                ctrl.a_max
            );
        }
    }

    // ── FPQ-41-D: TapControl::select_tap rounds to nearest discrete step ────

    #[test]
    fn fpq41_d_select_tap_discrete_steps() {
        let ctrl = TapControl {
            a_min: 0.9,
            a_max: 1.1,
            n_taps: 3, // steps: 0.9, 1.0, 1.1
            target_vd_r_pu: 1.0,
        };
        let alpha_rad: f64 = 15_f64.to_radians();
        let cos_alpha = alpha_rad.cos();

        // With n_taps=3 the only valid taps are {0.9, 1.0, 1.1}.
        // For target 1.0, v_ac 1.0, the ideal a = target / (k × v_ac × cos_α).
        let k = 3.0 * SQRT_2 / PI;
        let a_ideal = ctrl.target_vd_r_pu / (k * 1.0 * cos_alpha);

        let tap = ctrl.select_tap(1.0, cos_alpha);

        // Must snap to one of the three positions.
        let valid = [0.9_f64, 1.0, 1.1];
        let on_grid = valid.iter().any(|&v| (tap - v).abs() < 1e-10);
        assert!(
            on_grid,
            "tap {tap} not on grid {{0.9, 1.0, 1.1}} (ideal a={a_ideal:.4})"
        );
    }

    // ── FPQ-41-F: TapControl::select_tap returns a_min for n_taps = 1 (no NaN) ──

    #[test]
    fn fpq41_f_select_tap_n_taps_1_returns_amin_not_nan() {
        let ctrl = TapControl {
            a_min: 0.9,
            a_max: 1.1,
            n_taps: 1, // single position — step_size would be ∞ without guard
            target_vd_r_pu: 1.0,
        };
        let tap = ctrl.select_tap(1.0, 15_f64.to_radians().cos());
        assert!(
            tap.is_finite(),
            "select_tap with n_taps=1 must not return NaN or Inf, got {tap}"
        );
        assert_eq!(tap, ctrl.a_min, "n_taps=1 should return a_min");
    }

    // ── FPQ-41-E: TapControl::select_tap clamps when target unreachable ─────

    #[test]
    fn fpq41_e_select_tap_clamps_to_limits() {
        let ctrl = TapControl {
            a_min: 0.9,
            a_max: 1.1,
            n_taps: 33,
            target_vd_r_pu: 1.0,
        };
        let alpha_rad: f64 = 15_f64.to_radians();
        let cos_alpha = alpha_rad.cos();

        // Very low AC voltage -> ideal tap would be > a_max -> clamp to a_max.
        let tap_low_v = ctrl.select_tap(0.1, cos_alpha);
        assert_eq!(
            tap_low_v, ctrl.a_max,
            "expected a_max clamp for unreachable high tap, got {tap_low_v}"
        );

        // Very high AC voltage -> ideal tap would be < a_min -> clamp to a_min.
        let tap_high_v = ctrl.select_tap(10.0, cos_alpha);
        assert_eq!(
            tap_high_v, ctrl.a_min,
            "expected a_min clamp for unreachable low tap, got {tap_high_v}"
        );
    }

    // ── Commutation reactance: default x_c = 0 matches the simplified model ──

    #[test]
    fn commutation_reactance_zero_matches_simplified_model() {
        // With x_c_r = x_c_i = 0.0 (default), the result must exactly match
        // the no-load DC voltage (no commutation drop).
        let params = base_params();
        assert_eq!(params.x_c_r, 0.0, "default x_c_r must be 0.0");
        assert_eq!(params.x_c_i, 0.0, "default x_c_i must be 0.0");

        let k = 3.0 * SQRT_2 / PI;
        let v_ac = 1.0;
        let alpha_rad = params.firing_angle_deg.to_radians();
        let gamma_rad = params.extinction_angle_deg.to_radians();

        let op = compute_lcc_operating_point(&params, v_ac, v_ac, BASE_MVA);

        let expected_vd_r = k * v_ac * alpha_rad.cos();
        let expected_vd_i = k * v_ac * gamma_rad.cos();

        assert!(
            (op.vd_r_pu - expected_vd_r).abs() < 1e-10,
            "x_c=0: vd_r_pu should equal no-load voltage: got {}, expected {}",
            op.vd_r_pu,
            expected_vd_r
        );
        assert!(
            (op.vd_i_pu - expected_vd_i).abs() < 1e-10,
            "x_c=0: vd_i_pu should equal no-load voltage: got {}, expected {}",
            op.vd_i_pu,
            expected_vd_i
        );
    }

    // ── Commutation reactance: nonzero X_c reduces DC voltage ──

    #[test]
    fn commutation_reactance_reduces_dc_voltage() {
        // With nonzero commutation reactance, Vd_R should be lower than the
        // no-load voltage Vd0_R because the commutation overlap consumes part
        // of the AC voltage: Vd_R = Vd0_R - (3/pi) * X_c_R * I_d.
        let mut params = base_params();
        params.x_c_r = 0.15; // typical value: 0.10-0.20 pu
        params.x_c_i = 0.15;
        params.r_dc_pu = 0.01; // small DC resistance

        let k = 3.0 * SQRT_2 / PI;
        let v_ac = 1.0;
        let alpha_rad = params.firing_angle_deg.to_radians();
        let gamma_rad = params.extinction_angle_deg.to_radians();
        let vd0_r = k * v_ac * alpha_rad.cos();
        let vd0_i = k * v_ac * gamma_rad.cos();

        let op = compute_lcc_operating_point(&params, v_ac, v_ac, BASE_MVA);

        assert!(
            op.vd_r_pu < vd0_r,
            "Commutation reactance must reduce Vd_R below no-load: got {:.6}, no-load {:.6}",
            op.vd_r_pu,
            vd0_r
        );
        assert!(
            op.vd_i_pu < vd0_i,
            "Commutation reactance must reduce Vd_I below no-load: got {:.6}, no-load {:.6}",
            op.vd_i_pu,
            vd0_i
        );

        // Verify the drop magnitude: Vd = Vd0 - (3/pi) * X_c * I_d
        let k_xc = 3.0 / PI;
        let expected_drop_r = k_xc * params.x_c_r * op.i_dc_pu;
        let actual_drop_r = vd0_r - op.vd_r_pu;
        assert!(
            (actual_drop_r - expected_drop_r).abs() < 1e-8,
            "Rectifier voltage drop should be (3/pi)*Xc*Id = {:.6}, got {:.6}",
            expected_drop_r,
            actual_drop_r
        );
    }

    // ── Commutation reactance: higher X_c gives larger voltage drop ──

    #[test]
    fn commutation_reactance_higher_xc_larger_drop() {
        let mut params_low = base_params();
        params_low.x_c_r = 0.10;
        params_low.x_c_i = 0.10;
        params_low.r_dc_pu = 0.01;

        let mut params_high = base_params();
        params_high.x_c_r = 0.20;
        params_high.x_c_i = 0.20;
        params_high.r_dc_pu = 0.01;

        let v_ac = 1.0;
        let op_low = compute_lcc_operating_point(&params_low, v_ac, v_ac, BASE_MVA);
        let op_high = compute_lcc_operating_point(&params_high, v_ac, v_ac, BASE_MVA);

        assert!(
            op_high.vd_r_pu < op_low.vd_r_pu,
            "Higher X_c should produce lower Vd_R: x_c=0.10 gives {:.6}, x_c=0.20 gives {:.6}",
            op_low.vd_r_pu,
            op_high.vd_r_pu
        );
    }

    // ── Commutation reactance: with DC resistance, losses increase ──

    #[test]
    fn commutation_reactance_with_resistance_increases_current() {
        // With commutation reactance drop, Vd_R is lower, so to deliver the
        // same P_dc the DC current must increase, producing higher I^2*R losses.
        let mut params_no_xc = base_params();
        params_no_xc.r_dc_pu = 0.05;
        params_no_xc.x_c_r = 0.0;
        params_no_xc.x_c_i = 0.0;

        let mut params_with_xc = base_params();
        params_with_xc.r_dc_pu = 0.05;
        params_with_xc.x_c_r = 0.15;
        params_with_xc.x_c_i = 0.15;

        let v_ac = 1.0;
        let op_no_xc = compute_lcc_operating_point(&params_no_xc, v_ac, v_ac, BASE_MVA);
        let op_with_xc = compute_lcc_operating_point(&params_with_xc, v_ac, v_ac, BASE_MVA);

        assert!(
            op_with_xc.i_dc_pu > op_no_xc.i_dc_pu,
            "Commutation reactance should increase DC current: without={:.6}, with={:.6}",
            op_no_xc.i_dc_pu,
            op_with_xc.i_dc_pu
        );
        assert!(
            op_with_xc.p_loss_mw > op_no_xc.p_loss_mw,
            "Commutation reactance should increase line losses: without={:.4} MW, with={:.4} MW",
            op_no_xc.p_loss_mw,
            op_with_xc.p_loss_mw
        );
    }

    // ── LccHvdcControlMode::ConstantCurrent — fixed I_d, power floats with V_ac ──

    #[test]
    fn constant_current_power_scales_with_vac() {
        let i_d_pu = 2.0; // 2.0 pu → 200 MW at Vd = 1.0 pu
        let mut params = base_params();
        params.control_mode = LccHvdcControlMode::ConstantCurrent { i_d_pu };

        // Nominal AC voltage.
        let op_nom = compute_lcc_operating_point(&params, 1.0, 1.0, BASE_MVA);
        // Depressed AC voltage — power should decrease, I_d unchanged.
        let op_low = compute_lcc_operating_point(&params, 0.9, 0.9, BASE_MVA);

        assert!(
            (op_nom.i_dc_pu - i_d_pu).abs() < 1e-10,
            "I_d must equal setpoint: got {}, expected {}",
            op_nom.i_dc_pu,
            i_d_pu
        );
        assert!(
            (op_low.i_dc_pu - i_d_pu).abs() < 1e-10,
            "I_d must remain at setpoint under low voltage: got {}, expected {}",
            op_low.i_dc_pu,
            i_d_pu
        );
        assert!(
            op_low.p_dc_mw < op_nom.p_dc_mw,
            "power must decrease when V_ac drops: nom={:.2} MW, low={:.2} MW",
            op_nom.p_dc_mw,
            op_low.p_dc_mw
        );
    }

    // ── LccHvdcControlMode::ConstantAlpha — fixed α, I_d and P float ─────────────

    #[test]
    fn constant_alpha_current_determined_by_circuit() {
        let alpha_deg = 20.0_f64;
        let mut params = base_params();
        params.r_dc_pu = 0.1; // non-zero resistance so I_d > 0
        params.control_mode = LccHvdcControlMode::ConstantAlpha { alpha_deg };

        let op = compute_lcc_operating_point(&params, 1.0, 1.0, BASE_MVA);

        // With xc_r = xc_i = 0 and r_dc > 0:
        //   I_d = (Vd0_R(α=20°) - Vd0_I(γ=15°)) / R_dc
        let k = 3.0 * SQRT_2 / PI;
        let vd0_r = k * 1.0 * alpha_deg.to_radians().cos();
        let vd0_i = k * 1.0 * 15.0_f64.to_radians().cos();
        let expected_id = ((vd0_r - vd0_i) / 0.1).max(0.0);

        assert!(
            (op.i_dc_pu - expected_id).abs() < 1e-8,
            "I_d must come from Thevenin solution: got {:.6}, expected {:.6}",
            op.i_dc_pu,
            expected_id
        );
        assert!(op.p_dc_mw >= 0.0, "power must be non-negative");
    }

    // ── LccHvdcControlMode::Vdcol — current order limited by DC voltage ───────────

    #[test]
    fn vdcol_reduces_current_at_low_voltage() {
        let mut params = base_params();
        // VDCOL: full order 2.0 pu above 0.9 pu, min 0.3 pu below 0.5 pu.
        params.control_mode = LccHvdcControlMode::Vdcol {
            i_order_pu: 2.0,
            v_high_pu: 0.9,
            v_low_pu: 0.5,
            i_min_pu: 0.3,
        };

        // At nominal V_ac the DC voltage is well above v_high; full current order.
        let op_nom = compute_lcc_operating_point(&params, 1.0, 1.0, BASE_MVA);
        assert!(
            (op_nom.i_dc_pu - 2.0).abs() < 1e-6,
            "full current order expected at nominal voltage, got {:.4}",
            op_nom.i_dc_pu
        );

        // At severely depressed V_ac the DC voltage drops below v_low; minimum order.
        let op_fault = compute_lcc_operating_point(&params, 0.3, 0.3, BASE_MVA);
        assert!(
            op_fault.i_dc_pu <= 0.3 + 1e-6,
            "current must be clamped to i_min_pu at severe undervoltage, got {:.4}",
            op_fault.i_dc_pu
        );
    }

    // ── alpha_min_deg guards against sub-limit firing angles ──────────────────

    #[test]
    fn alpha_min_clamps_constant_power_firing_angle() {
        let mut params = base_params();
        // Set firing angle below alpha_min — the solver must honour the limit.
        params.firing_angle_deg = 2.0;
        params.alpha_min_deg = 5.0;
        // ConstantPower uses max(firing_angle_deg, alpha_min_deg) internally.
        let op = compute_lcc_operating_point(&params, 1.0, 1.0, BASE_MVA);

        // Vd_R must reflect cos(5°) not cos(2°) since 2° < alpha_min.
        let k = 3.0 * SQRT_2 / PI;
        let vd_r_if_clamped = k * 5.0_f64.to_radians().cos();
        // The actual Vd_R with commutation loss will be slightly lower; just check ordering.
        assert!(
            op.vd_r_pu <= vd_r_if_clamped + 1e-6,
            "Vd_R must not exceed the alpha_min clamped value"
        );
    }
}
