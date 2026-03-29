// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! LCC commutation failure detection (FPQ-40).
//!
//! LCC (Line-Commutated Converter) commutation failure occurs when the AC voltage
//! dips such that the extinction angle gamma falls below gamma_min (typically
//! 15-20 degrees). During commutation failure, the outgoing thyristor valve
//! fails to turn off before the incoming valve fires, causing a temporary DC
//! short-circuit that collapses DC voltage and power transfer.
//!
//! # Physics
//!
//! For an LCC inverter, the extinction angle gamma is found from:
//!
//!   cos(gamma) = cos(alpha + mu)
//!
//! where alpha is the firing advance angle and mu is the overlap angle.
//! As DC current I_d increases at constant V_ac, the commutation overlap mu
//! increases, which causes gamma to decrease. Commutation failure occurs when
//! gamma < gamma_min.
//!
//! # Detection Tiers
//!
//! This module provides two levels of commutation failure screening:
//!
//! ## Tier 1: Voltage Threshold Check (Simplified Screening)
//!
//! A fast conservative check: if `V_ac < 0.85 pu` (15% voltage dip), flag
//! commutation failure immediately. This threshold is derived from typical
//! CIGRE/NERC guidelines for **strong AC systems** (SCR > 3.0).
//!
//! **Limitations of the voltage threshold approach:**
//! - The 0.85 pu threshold is a single-valued approximation of a phenomenon
//!   that depends on multiple parameters (SCR, X_c, I_d, firing angle, AC
//!   system impedance angle).
//! - For weak AC systems (SCR < 3), commutation failure can occur at voltages
//!   as high as 0.90-0.92 pu because the higher source impedance reduces the
//!   available commutation voltage.
//! - The threshold does not account for rate-of-change of voltage (dV/dt),
//!   which affects whether the thyristor control system can adjust the firing
//!   angle fast enough to prevent commutation failure.
//! - Phase angle jumps (common during nearby faults) can cause commutation
//!   failure even without significant voltage magnitude reduction.
//!
//! ## Tier 2: Analytical Extinction Angle Model (Intermediate Accuracy)
//!
//! The `check_commutation_failure` function computes an analytical extinction
//! angle from the operating point:
//!
//!   cos(gamma) = cos(gamma_rated) + (X_c * I_d_pu / V_d0_pu - X_c)
//!
//! This captures the steady-state dependence of gamma on DC current and AC
//! voltage but has the following simplifications:
//! - Assumes sinusoidal AC voltage (no harmonics or distortion)
//! - Uses a linearized model around the rated operating point
//! - Does not model the transient dynamics of the commutation process
//! - Does not account for control system response (e.g., VDCOL)
//!
//! ## Tier 3: EMT Simulation (Full Accuracy -- External Tools Required)
//!
//! For precise commutation failure analysis, electromagnetic transient (EMT)
//! simulation with cycle-by-cycle gamma monitoring is required. EMT tools
//! model the full thyristor switching dynamics, AC voltage waveform distortion,
//! and control system response. Recommended EMT tools:
//!
//! - **PSCAD/EMTDC**: Industry-standard EMT simulation with detailed thyristor
//!   valve models and firing pulse control
//! - **EMTP-RV**: Alternative EMT tool with similar capabilities
//! - **PowerFactory (EMT mode)**: DIgSILENT PowerFactory's EMT simulation engine
//!
//! EMT simulation is essential for:
//! - Detailed HVDC planning studies near weak AC systems
//! - Multi-infeed HVDC interaction studies (multiple HVDC links feeding
//!   into electrically close AC buses)
//! - Fault ride-through capability assessment
//! - HVDC control system tuning and validation
//!
//! # Extinction Angle Model Details
//!
//! The extinction angle varies with operating conditions:
//!
//!   cos(gamma) = cos(gamma_rated) + (u - u_rated)
//!
//! where u = X_c * I_d_pu / V_d0_pu is the commutation overlap parameter.
//! Higher DC current or lower AC voltage increases u, which increases
//! cos(gamma), pushing gamma toward zero (commutation failure).
//!
//! When the AC voltage drops, V_d0 drops proportionally, so the effective
//! normalized commutation term (X_c * I_d / V_d0) increases even at constant
//! I_d, further reducing gamma.

use tracing::{debug, warn};

/// Result of a commutation failure check for an LCC converter.
#[derive(Debug, Clone)]
pub struct CommutationCheck {
    /// Computed extinction angle γ (degrees) at the current operating point.
    pub extinction_angle_gamma_deg: f64,
    /// Minimum acceptable extinction angle (degrees). Default: 15°.
    pub gamma_min_deg: f64,
    /// `true` if γ < γ_min (commutation failure detected).
    pub commutation_failure: bool,
    /// AC voltage dip threshold below which commutation failure is likely.
    /// Expressed as a fraction of rated voltage (e.g., 0.85 = 85% of rated).
    pub ac_voltage_dip_threshold_pu: f64,
}

/// Check if LCC commutation failure is likely given the current AC voltage and
/// DC operating point.
///
/// Uses a two-tier check:
///
/// 1. **Voltage dip check** (Tier 1): If `v_ac_pu < 0.85` (15% dip), flag
///    commutation failure immediately. This is a conservative fast-screening
///    criterion based on CIGRE/NERC guidelines for strong AC systems.
///
/// 2. **Extinction angle check** (Tier 2): Compute gamma from the analytical
///    commutation model and compare against `gamma_min_deg` (default 15 degrees).
///
/// # Extinction Angle Model
///
/// At rated conditions (V_ac = 1.0 pu, rated I_d), gamma is approximately
/// 18 degrees. As DC current increases or AC voltage drops, the overlap
/// angle mu grows:
///
///   cos(gamma) = cos(gamma_rated) + (X_c * I_d_pu / V_d0_pu - X_c)
///
/// Higher I_d or lower V_d0 increases the commutation overlap parameter u,
/// pushing cos(gamma) closer to 1 and gamma closer to zero. When gamma falls
/// below gamma_min, commutation failure is flagged.
///
/// # Limitations -- Simplified Screening Model
///
/// **Both Tier 1 and Tier 2 checks are simplified steady-state approximations.
/// They do not replace EMT simulation for detailed HVDC studies.**
///
/// ## Voltage Threshold Limitations (Tier 1)
///
/// The 0.85 pu threshold is a single-valued approximation valid for strong
/// AC systems (SCR > 3.0). For weak AC systems (SCR < 3), commutation failure
/// can occur at voltages as high as 0.90-0.92 pu. The actual critical voltage
/// depends on:
///
/// - **Short Circuit Ratio (SCR)**: Lower SCR = weaker system = higher risk.
/// - **Effective SCR (ESCR)**: Accounts for reactive compensation. ESCR < 2.0
///   is considered very weak.
/// - **AC system impedance angle**: Affects the commutation voltage waveform.
/// - **Rate of voltage change (dV/dt)**: Fast voltage drops give the firing
///   control less time to compensate.
/// - **Phase angle jumps**: Can cause commutation failure without significant
///   voltage magnitude reduction.
///
/// ## Analytical Model Limitations (Tier 2)
///
/// The extinction angle model assumes:
/// - Sinusoidal AC voltage (no harmonic distortion)
/// - Steady-state operating point (no transient dynamics)
/// - Linearization around the rated operating point
/// - No control system response (VDCOL, firing angle compensation)
///
/// ## When to Use EMT Simulation (Tier 3)
///
/// For precise commutation failure analysis, electromagnetic transient (EMT)
/// simulation with cycle-by-cycle gamma monitoring is required. Use EMT tools
/// (PSCAD/EMTDC, EMTP-RV, PowerFactory EMT mode) for:
/// - Critical HVDC planning studies on weak AC systems (SCR < 3)
/// - Multi-infeed HVDC interaction analysis
/// - Fault ride-through assessment
/// - Control system tuning and validation
/// - Any study where commutation failure probability is a binding constraint
///
/// # Arguments
/// * `v_ac_pu`                — AC bus voltage magnitude at the inverter (per-unit).
/// * `i_dc_ka`                — DC current (kA). Used for extinction angle computation.
/// * `v_dc_kv`                — DC voltage (kV) at the inverter. Used for normalization.
/// * `transformer_reactance_pu` — Converter transformer leakage reactance (pu on system base).
/// * `gamma_min_deg`          — Minimum extinction angle (degrees). Default: 15°.
///
/// # Returns
/// A `CommutationCheck` struct with all computed quantities.
///
/// # Example
/// ```rust
/// use surge_hvdc::check_commutation_failure;
/// let check = check_commutation_failure(0.80, 1.0, 500.0, 0.12, 15.0);
/// assert!(check.commutation_failure, "0.80 pu voltage should trigger commutation failure");
/// let check_ok = check_commutation_failure(0.95, 0.1, 500.0, 0.12, 15.0);
/// assert!(!check_ok.commutation_failure, "0.95 pu voltage should not trigger failure");
/// ```
pub fn check_commutation_failure(
    v_ac_pu: f64,
    i_dc_ka: f64,
    v_dc_kv: f64,
    transformer_reactance_pu: f64,
    gamma_min_deg: f64,
) -> CommutationCheck {
    debug!(
        v_ac_pu = v_ac_pu,
        i_dc_ka = i_dc_ka,
        v_dc_kv = v_dc_kv,
        transformer_reactance_pu = transformer_reactance_pu,
        gamma_min_deg = gamma_min_deg,
        "Commutation failure check starting"
    );

    // Voltage dip threshold: 15% below rated → 0.85 pu.
    let ac_voltage_dip_threshold_pu = 0.85;

    // Warn in the marginal zone (0.80-0.90 pu) where SCR strongly affects the
    // actual commutation failure boundary. For weak AC systems (SCR < 3), the
    // true failure threshold may be significantly higher than 0.85 pu.
    if (0.80..=0.90).contains(&v_ac_pu) {
        warn!(
            v_ac_pu = v_ac_pu,
            "LCC commutation failure: V_ac is in the marginal zone (0.80-0.90 pu) \
             where the simplified 0.85 pu threshold may be inaccurate. For weak AC \
             systems (SCR < 3), commutation failure may occur at higher voltages. \
             Validate against EMT simulation for critical HVDC planning studies."
        );
    }

    // Fast voltage dip check (conservative: immediate failure flag below threshold).
    let voltage_dip_failure = v_ac_pu < ac_voltage_dip_threshold_pu;

    // Analytical extinction angle computation.
    //
    // Normalize DC current using rated DC power as a reference.
    // Rated DC current ≈ rated DC power / V_dc_kv.
    // We use a simple per-unit normalization:
    //   I_d_pu = I_d_kA / I_d_rated_kA
    // where I_d_rated_kA = P_rated_MW / V_dc_kV.
    // Since we don't know P_rated, we use V_dc as the denominator reference.
    // A 500 kV, 1000 MW link has I_d_rated ≈ 2 kA.
    // Use V_dc_kV / 500 as normalized current scaling.
    let v_dc_ref_kv = v_dc_kv.max(1.0);
    let i_d_rated_ka = v_dc_ref_kv / 500.0_f64.max(1.0); // nominal rated DC current (kA)
    let i_d_pu = i_dc_ka / i_d_rated_ka.max(1e-6);

    // No-load DC voltage (pu), proportional to AC voltage.
    let v_d0_pu = v_ac_pu.max(1e-6);

    // Commutation voltage drop parameter (dimensionless):
    //   u = X_c * I_d_pu / V_d0_pu
    // This represents the fraction of the no-load voltage consumed by commutation.
    let u = (transformer_reactance_pu * i_d_pu / v_d0_pu).clamp(0.0, 2.0);

    // Extinction angle deviation from rated operating point:
    //   cos(γ) = cos(γ_rated) + (u − u_rated)
    //
    // At rated conditions (I_d_pu=1, V_d0_pu=1): u_rated = X_c.
    // When u > u_rated (overcurrent or low voltage): cos(γ) > cos(γ_rated) → γ < γ_rated.
    // When u < u_rated (light load): cos(γ) < cos(γ_rated) → γ > γ_rated (safe margin).
    // γ_rated ≈ 18° at the nominal design point.
    let gamma_rated_rad = 18.0_f64.to_radians();
    let u_rated = transformer_reactance_pu; // overlap parameter at nominal (I_d_pu=1, V_d0_pu=1)
    let cos_gamma = (gamma_rated_rad.cos() + (u - u_rated)).clamp(-1.0, 1.0);
    let gamma_deg = cos_gamma.acos().to_degrees();

    let extinction_angle_failure = gamma_deg < gamma_min_deg;
    let commutation_failure = voltage_dip_failure || extinction_angle_failure;

    if commutation_failure {
        warn!(
            v_ac_pu = v_ac_pu,
            extinction_angle_gamma_deg = gamma_deg,
            gamma_min_deg = gamma_min_deg,
            voltage_dip_failure = voltage_dip_failure,
            extinction_angle_failure = extinction_angle_failure,
            "LCC commutation failure detected"
        );
    } else {
        debug!(
            extinction_angle_gamma_deg = gamma_deg,
            gamma_min_deg = gamma_min_deg,
            "Commutation check passed"
        );
    }

    CommutationCheck {
        extinction_angle_gamma_deg: gamma_deg,
        gamma_min_deg,
        commutation_failure,
        ac_voltage_dip_threshold_pu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commutation_failure_low_voltage() {
        // V_ac = 0.80 pu → well below 0.85 threshold → failure
        let check = check_commutation_failure(0.80, 1.0, 500.0, 0.12, 15.0);
        assert!(
            check.commutation_failure,
            "V_ac=0.80 pu should trigger commutation failure, γ={:.2}°",
            check.extinction_angle_gamma_deg
        );
        assert_eq!(check.gamma_min_deg, 15.0);
        assert_eq!(check.ac_voltage_dip_threshold_pu, 0.85);
    }

    #[test]
    fn test_commutation_failure_nominal_voltage() {
        // V_ac = 0.95 pu → above threshold; at low DC current, no failure expected.
        let check = check_commutation_failure(0.95, 0.1, 500.0, 0.12, 15.0);
        assert!(
            !check.commutation_failure,
            "V_ac=0.95 pu with low DC current should NOT trigger failure, γ={:.2}°",
            check.extinction_angle_gamma_deg
        );
    }

    #[test]
    fn test_commutation_failure_rated_voltage() {
        // V_ac = 1.0 pu, low DC current → γ near rated (≈18°), well above 15°
        let check = check_commutation_failure(1.0, 0.1, 500.0, 0.10, 15.0);
        assert!(
            !check.commutation_failure,
            "V_ac=1.0 pu should not fail, γ={:.2}°",
            check.extinction_angle_gamma_deg
        );
        assert!(
            check.extinction_angle_gamma_deg >= check.gamma_min_deg,
            "γ at rated={:.2}° should be ≥ γ_min={:.2}°",
            check.extinction_angle_gamma_deg,
            check.gamma_min_deg
        );
    }

    #[test]
    fn test_commutation_failure_custom_gamma_min() {
        // With γ_min = 20° (more conservative), some cases may fail.
        let check = check_commutation_failure(0.90, 1.0, 500.0, 0.12, 20.0);
        assert_eq!(check.gamma_min_deg, 20.0);
        // Just verify the struct is populated consistently.
        if check.commutation_failure {
            let voltage_triggered = check.ac_voltage_dip_threshold_pu > 0.90;
            let angle_triggered = check.extinction_angle_gamma_deg < check.gamma_min_deg;
            assert!(
                voltage_triggered || angle_triggered,
                "Failure should be due to voltage or angle criterion"
            );
        }
    }

    #[test]
    fn test_extinction_angle_decreases_with_dc_current() {
        // Higher DC current → larger commutation overlap → lower γ.
        // Use realistic parameters: V_dc = 500 kV, V_ac = 1.0 pu.
        let check_low_i = check_commutation_failure(1.0, 0.5, 500.0, 0.12, 15.0);
        let check_high_i = check_commutation_failure(1.0, 3.0, 500.0, 0.12, 15.0);
        assert!(
            check_high_i.extinction_angle_gamma_deg <= check_low_i.extinction_angle_gamma_deg,
            "Higher DC current should give lower or equal γ: \
             low_I γ={:.2}°, high_I γ={:.2}°",
            check_low_i.extinction_angle_gamma_deg,
            check_high_i.extinction_angle_gamma_deg
        );
    }

    #[test]
    fn test_voltage_threshold_boundary() {
        // Below 0.85 pu → voltage failure triggered
        let below_threshold = check_commutation_failure(0.849, 0.1, 500.0, 0.05, 15.0);
        assert!(
            below_threshold.commutation_failure,
            "0.849 pu should trigger failure (below 0.85 threshold)"
        );
        // At exactly 0.85: 0.85 < 0.85 is false → no voltage failure;
        // whether failure occurs depends on extinction angle only.
        let at_threshold = check_commutation_failure(0.85, 0.1, 500.0, 0.05, 15.0);
        // Not necessarily a failure — just verify it's computed without panic.
        let _ = at_threshold;
    }

    #[test]
    fn test_commutation_struct_fields() {
        let check = check_commutation_failure(1.0, 1.0, 500.0, 0.12, 15.0);
        assert!(check.extinction_angle_gamma_deg >= 0.0);
        assert!(check.extinction_angle_gamma_deg <= 180.0);
        assert_eq!(check.gamma_min_deg, 15.0);
        assert_eq!(check.ac_voltage_dip_threshold_pu, 0.85);
    }

    /// Test that voltages in the marginal zone (0.80-0.90 pu) still produce
    /// valid results. The marginal zone warning is a log message; here we verify
    /// that the function returns a consistent CommutationCheck for all voltages
    /// in the marginal range.
    #[test]
    fn test_marginal_zone_produces_valid_results() {
        // Test several points in the marginal zone (0.80 - 0.90 pu)
        for v_ac_hundredths in 80..=90 {
            let v_ac = v_ac_hundredths as f64 / 100.0;
            let check = check_commutation_failure(v_ac, 1.0, 500.0, 0.12, 15.0);
            assert!(
                check.extinction_angle_gamma_deg >= 0.0
                    && check.extinction_angle_gamma_deg <= 180.0,
                "V_ac={v_ac}: extinction angle out of physical range: {:.2}",
                check.extinction_angle_gamma_deg
            );
            // Below 0.85 the voltage dip check should trigger
            if v_ac < 0.85 {
                assert!(
                    check.commutation_failure,
                    "V_ac={v_ac}: expected failure below 0.85 threshold"
                );
            }
        }
    }
}
