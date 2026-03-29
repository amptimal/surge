// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Transformer core saturation, core loss, and core type models.
//!
//! Provides data structures for nonlinear harmonic power flow:
//!
//! - [`SaturationCurve`]: Piecewise-linear Phi-I_m characteristic
//! - [`CoreLossModel`]: Frequency-dependent Steinmetz core loss decomposition
//! - [`CoreType`]: Transformer core construction type (affects GIC K-factor)
//! - [`TransformerSaturation`]: Unified saturation model (PWL, two-slope, polynomial)
//! - [`ConverterCommutationModel`]: 6/12/18/24-pulse voltage-dependent overlap

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// CoreType (moved from surge-gic/src/types.rs)
// ---------------------------------------------------------------------------

/// Transformer core construction type.
///
/// Determines the K-factor for GIC reactive power absorption and affects
/// harmonic saturation behaviour.
///
/// K-factor values from EPRI 3002002985 and Overbye et al. (2012).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum CoreType {
    /// 3-limb core-form: most resistant to GIC (K = 0.33).
    ThreeLimbCore,
    /// 5-limb core-form: moderate vulnerability (K = 1.18).
    #[default]
    FiveLimbCore,
    /// 5-limb shell-form: most vulnerable to GIC (K = 2.0).
    FiveLimbShell,
    /// Bank of single-phase core-form units (K = 1.18).
    SinglePhaseBank,
    /// Single-phase shell-form (K = 0.80).
    SinglePhaseShell,
    /// User-specified custom K-factor.
    Custom(f64),
}

impl CoreType {
    /// Return the K-factor for this core type (dimensionless).
    pub fn k_factor(&self) -> f64 {
        match self {
            CoreType::ThreeLimbCore => 0.33,
            CoreType::FiveLimbCore => 1.18,
            CoreType::FiveLimbShell => 2.0,
            CoreType::SinglePhaseBank => 1.18,
            CoreType::SinglePhaseShell => 0.80,
            CoreType::Custom(k) => {
                if k.is_finite() && *k >= 0.0 {
                    *k
                } else {
                    0.0
                }
            }
        }
    }
}

impl fmt::Display for CoreType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreType::ThreeLimbCore => write!(f, "3-limb"),
            CoreType::FiveLimbCore => write!(f, "5-limb"),
            CoreType::FiveLimbShell => write!(f, "5-limb-shell"),
            CoreType::SinglePhaseBank => write!(f, "1ph-bank"),
            CoreType::SinglePhaseShell => write!(f, "1ph-shell"),
            CoreType::Custom(k) => write!(f, "custom({k})"),
        }
    }
}

impl FromStr for CoreType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace(' ', "-").as_str() {
            "3-limb" | "3limb" | "three-limb" | "three-limb-core" => Ok(CoreType::ThreeLimbCore),
            "5-limb" | "5limb" | "five-limb" | "five-limb-core" => Ok(CoreType::FiveLimbCore),
            "5-limb-shell" | "5limb-shell" | "five-limb-shell" => Ok(CoreType::FiveLimbShell),
            "1ph-bank" | "single-phase-bank" | "1ph-core" => Ok(CoreType::SinglePhaseBank),
            "1ph-shell" | "single-phase-shell" => Ok(CoreType::SinglePhaseShell),
            other => {
                if let Some(rest) = other
                    .strip_prefix("custom(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    rest.parse::<f64>()
                        .map(CoreType::Custom)
                        .map_err(|e| format!("invalid custom K-factor: {e}"))
                } else {
                    Err(format!("unknown core type: '{other}'"))
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SaturationCurve
// ---------------------------------------------------------------------------

/// A single point on the Phi-I_m saturation curve.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SaturationPoint {
    /// Magnetizing current in pu of rated transformer current.
    pub i_m_pu: f64,
    /// Flux linkage in pu of rated flux.
    pub phi_pu: f64,
}

/// Piecewise-linear transformer core saturation characteristic.
///
/// Represents the single-valued (anhysteretic) Phi-I_m curve of the transformer
/// core material. Points must be monotonically increasing in both Phi and I_m.
///
/// Points are stored for Phi >= 0 only. [`evaluate()`](Self::evaluate) applies
/// odd symmetry for negative flux: `f_BH(-Phi) = -f_BH(Phi)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaturationCurve {
    /// Ordered (I_m, Phi) points. Sorted ascending by i_m_pu (and phi_pu).
    /// Minimum 2 points required. First point should be (0, 0).
    pub points: Vec<SaturationPoint>,
}

impl SaturationCurve {
    /// Evaluate magnetizing current I_m at a given flux Phi using PWL interpolation.
    ///
    /// Applies odd symmetry: `f(-Phi) = -f(Phi)`. For Phi beyond the last point,
    /// linear extrapolation from the last two points is used.
    pub fn evaluate(&self, phi_pu: f64) -> f64 {
        if phi_pu < 0.0 {
            return -self.evaluate_positive(-phi_pu);
        }
        self.evaluate_positive(phi_pu)
    }

    /// Evaluate for phi >= 0 (no symmetry applied).
    fn evaluate_positive(&self, phi: f64) -> f64 {
        let pts = &self.points;
        if pts.is_empty() {
            return 0.0;
        }
        if pts.len() == 1 {
            // Single point: assume linear from origin
            if pts[0].phi_pu.abs() < 1e-30 {
                return 0.0;
            }
            return phi * pts[0].i_m_pu / pts[0].phi_pu;
        }

        // Below first point: interpolate from origin (0,0) to first point
        if phi <= pts[0].phi_pu {
            if pts[0].phi_pu.abs() < 1e-30 {
                return 0.0;
            }
            return phi * pts[0].i_m_pu / pts[0].phi_pu;
        }

        // Interior: find the segment
        for i in 1..pts.len() {
            if phi <= pts[i].phi_pu {
                let dp = pts[i].phi_pu - pts[i - 1].phi_pu;
                if dp.abs() < 1e-30 {
                    return pts[i].i_m_pu;
                }
                let t = (phi - pts[i - 1].phi_pu) / dp;
                return pts[i - 1].i_m_pu + t * (pts[i].i_m_pu - pts[i - 1].i_m_pu);
            }
        }

        // Beyond last point: linear extrapolation from last two points
        let n = pts.len();
        let dp = pts[n - 1].phi_pu - pts[n - 2].phi_pu;
        if dp.abs() < 1e-30 {
            return pts[n - 1].i_m_pu;
        }
        let slope = (pts[n - 1].i_m_pu - pts[n - 2].i_m_pu) / dp;
        pts[n - 1].i_m_pu + slope * (phi - pts[n - 1].phi_pu)
    }

    /// Inverse lookup: find Phi for a given I_m (for initialization).
    ///
    /// Applies odd symmetry for negative I_m.
    pub fn evaluate_inverse(&self, i_m_pu: f64) -> f64 {
        if i_m_pu < 0.0 {
            return -self.evaluate_inverse_positive(-i_m_pu);
        }
        self.evaluate_inverse_positive(i_m_pu)
    }

    fn evaluate_inverse_positive(&self, i_m: f64) -> f64 {
        let pts = &self.points;
        if pts.is_empty() {
            return 0.0;
        }
        if pts.len() == 1 {
            if pts[0].i_m_pu.abs() < 1e-30 {
                return 0.0;
            }
            return i_m * pts[0].phi_pu / pts[0].i_m_pu;
        }

        if i_m <= pts[0].i_m_pu {
            if pts[0].i_m_pu.abs() < 1e-30 {
                return 0.0;
            }
            return i_m * pts[0].phi_pu / pts[0].i_m_pu;
        }

        for i in 1..pts.len() {
            if i_m <= pts[i].i_m_pu {
                let di = pts[i].i_m_pu - pts[i - 1].i_m_pu;
                if di.abs() < 1e-30 {
                    return pts[i].phi_pu;
                }
                let t = (i_m - pts[i - 1].i_m_pu) / di;
                return pts[i - 1].phi_pu + t * (pts[i].phi_pu - pts[i - 1].phi_pu);
            }
        }

        // Extrapolate
        let n = pts.len();
        let di = pts[n - 1].i_m_pu - pts[n - 2].i_m_pu;
        if di.abs() < 1e-30 {
            return pts[n - 1].phi_pu;
        }
        let slope = (pts[n - 1].phi_pu - pts[n - 2].phi_pu) / di;
        pts[n - 1].phi_pu + slope * (i_m - pts[n - 1].i_m_pu)
    }

    /// Validate the saturation curve data.
    pub fn validate(&self) -> Result<(), String> {
        if self.points.len() < 2 {
            return Err(format!(
                "saturation curve requires at least 2 points, got {}",
                self.points.len()
            ));
        }
        for i in 1..self.points.len() {
            if self.points[i].phi_pu <= self.points[i - 1].phi_pu {
                return Err(format!(
                    "saturation curve phi_pu not monotonically increasing at index {}: {:.6} <= {:.6}",
                    i,
                    self.points[i].phi_pu,
                    self.points[i - 1].phi_pu
                ));
            }
            if self.points[i].i_m_pu <= self.points[i - 1].i_m_pu {
                return Err(format!(
                    "saturation curve i_m_pu not monotonically increasing at index {}: {:.6} <= {:.6}",
                    i,
                    self.points[i].i_m_pu,
                    self.points[i - 1].i_m_pu
                ));
            }
        }
        if self.points[0].phi_pu < 0.0 || self.points[0].i_m_pu < 0.0 {
            return Err("saturation curve points must have non-negative values".to_string());
        }
        Ok(())
    }

    /// Typical 500 MVA power transformer saturation curve.
    ///
    /// Knee point at ~1.15 pu flux, air-core slope above 1.25 pu.
    pub fn typical_power_transformer() -> Self {
        Self {
            points: vec![
                SaturationPoint {
                    i_m_pu: 0.0,
                    phi_pu: 0.0,
                },
                SaturationPoint {
                    i_m_pu: 0.001,
                    phi_pu: 0.5,
                },
                SaturationPoint {
                    i_m_pu: 0.003,
                    phi_pu: 0.8,
                },
                SaturationPoint {
                    i_m_pu: 0.01,
                    phi_pu: 1.0,
                },
                SaturationPoint {
                    i_m_pu: 0.03,
                    phi_pu: 1.1,
                },
                SaturationPoint {
                    i_m_pu: 0.10,
                    phi_pu: 1.15,
                },
                SaturationPoint {
                    i_m_pu: 0.50,
                    phi_pu: 1.20,
                },
                SaturationPoint {
                    i_m_pu: 2.0,
                    phi_pu: 1.25,
                },
                SaturationPoint {
                    i_m_pu: 10.0,
                    phi_pu: 1.30,
                },
            ],
        }
    }

    /// Typical 25 MVA distribution transformer saturation curve.
    ///
    /// Lower knee point (~1.10 pu) and steeper saturation than power transformers.
    pub fn typical_distribution_transformer() -> Self {
        Self {
            points: vec![
                SaturationPoint {
                    i_m_pu: 0.0,
                    phi_pu: 0.0,
                },
                SaturationPoint {
                    i_m_pu: 0.002,
                    phi_pu: 0.5,
                },
                SaturationPoint {
                    i_m_pu: 0.005,
                    phi_pu: 0.8,
                },
                SaturationPoint {
                    i_m_pu: 0.015,
                    phi_pu: 1.0,
                },
                SaturationPoint {
                    i_m_pu: 0.05,
                    phi_pu: 1.05,
                },
                SaturationPoint {
                    i_m_pu: 0.15,
                    phi_pu: 1.10,
                },
                SaturationPoint {
                    i_m_pu: 0.80,
                    phi_pu: 1.15,
                },
                SaturationPoint {
                    i_m_pu: 5.0,
                    phi_pu: 1.20,
                },
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// TwoSlopeSaturation
// ---------------------------------------------------------------------------

/// Two-slope saturation model (PSS/E convention).
///
/// Below the knee: linear with slope 1/b_mag_unsat (unsaturated magnetizing
/// admittance). Above the knee: air-core reactance (much steeper I vs Phi).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TwoSlopeSaturation {
    /// Knee-point flux in pu (typically 1.1-1.2 pu).
    pub phi_knee_pu: f64,
    /// Unsaturated magnetizing susceptance (pu, = Branch.b_mag).
    pub b_mag_unsat: f64,
    /// Air-core reactance above knee (pu). Typical: 0.2-0.4 pu.
    pub x_air_pu: f64,
}

impl TwoSlopeSaturation {
    /// Convert to a piecewise-linear curve with 10 points.
    pub fn to_piecewise_linear(&self) -> SaturationCurve {
        let mut points = Vec::with_capacity(10);
        points.push(SaturationPoint {
            i_m_pu: 0.0,
            phi_pu: 0.0,
        });

        // Unsaturated region: I_m = Phi * b_mag_unsat
        let b = self.b_mag_unsat.abs().max(1e-6);
        let n_unsat = 4;
        for k in 1..=n_unsat {
            let phi = self.phi_knee_pu * k as f64 / n_unsat as f64;
            let i_m = phi * b;
            points.push(SaturationPoint {
                i_m_pu: i_m,
                phi_pu: phi,
            });
        }

        // Saturated region: I_m = I_knee + (Phi - Phi_knee) / x_air
        let i_knee = self.phi_knee_pu * b;
        let x_air = self.x_air_pu.abs().max(1e-6);
        let n_sat = 5;
        for k in 1..=n_sat {
            let dphi = 0.05 * k as f64; // 0.05, 0.10, 0.15, 0.20, 0.25 above knee
            let phi = self.phi_knee_pu + dphi;
            let i_m = i_knee + dphi / x_air;
            points.push(SaturationPoint {
                i_m_pu: i_m,
                phi_pu: phi,
            });
        }

        SaturationCurve { points }
    }
}

// ---------------------------------------------------------------------------
// PolynomialSaturation
// ---------------------------------------------------------------------------

/// Polynomial saturation model (EMTP/Dommel-type).
///
/// `I_m = a_1 * Phi + a_3 * Phi^3 + a_5 * Phi^5 + ...`
/// (odd powers only for symmetric saturation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolynomialSaturation {
    /// Coefficients [a_1, a_3, a_5, ...] for odd-power terms.
    pub coefficients: Vec<f64>,
}

impl PolynomialSaturation {
    /// Evaluate I_m from Phi using the polynomial.
    pub fn evaluate(&self, phi: f64) -> f64 {
        let mut result = 0.0;
        let mut power = 1u32;
        for &coeff in &self.coefficients {
            result += coeff * phi.powi(power as i32);
            power += 2;
        }
        result
    }

    /// Sample the polynomial to produce a piecewise-linear curve.
    pub fn to_piecewise_linear(&self, n_points: usize) -> SaturationCurve {
        let n = n_points.max(3);
        let phi_max = 1.5; // sample up to 1.5 pu flux
        let mut points = Vec::with_capacity(n);
        for k in 0..n {
            let phi = phi_max * k as f64 / (n - 1) as f64;
            let i_m = self.evaluate(phi);
            points.push(SaturationPoint {
                i_m_pu: i_m.max(0.0),
                phi_pu: phi,
            });
        }
        SaturationCurve { points }
    }
}

// ---------------------------------------------------------------------------
// TransformerSaturation (unified enum)
// ---------------------------------------------------------------------------

/// Unified transformer saturation model supporting multiple representations.
///
/// All variants can be converted to the canonical [`SaturationCurve`] (PWL)
/// for computation via [`as_pwl()`](Self::as_pwl).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransformerSaturation {
    /// Full piecewise-linear Phi-I_m curve.
    PiecewiseLinear(SaturationCurve),
    /// Two-slope model (knee point + air-core reactance).
    TwoSlope(TwoSlopeSaturation),
    /// Polynomial (Dommel-type) model.
    Polynomial(PolynomialSaturation),
}

impl TransformerSaturation {
    /// Convert any variant to the canonical PWL representation.
    pub fn as_pwl(&self) -> SaturationCurve {
        match self {
            TransformerSaturation::PiecewiseLinear(c) => c.clone(),
            TransformerSaturation::TwoSlope(ts) => ts.to_piecewise_linear(),
            TransformerSaturation::Polynomial(p) => p.to_piecewise_linear(20),
        }
    }
}

// ---------------------------------------------------------------------------
// CoreLossModel
// ---------------------------------------------------------------------------

/// Frequency-dependent core loss decomposition (Steinmetz model).
///
/// At harmonic order h, the core loss conductance is:
///
/// ```text
/// g_core(h) = g_mag * (f_eddy * h^2 + f_hyst * h^1.6 + f_excess * h^1.5)
/// ```
///
/// where `f_eddy + f_hyst + f_excess = 1.0` and `g_mag` is the
/// fundamental-frequency core loss conductance from `Branch::g_mag`.
///
/// Default decomposition (IEEE C57.110): f_eddy = 0.5, f_hyst = 0.5.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CoreLossModel {
    /// Fraction of fundamental core loss due to eddy currents (scales as h^2).
    pub f_eddy: f64,
    /// Fraction due to hysteresis (scales as h^1.6).
    pub f_hyst: f64,
    /// Fraction due to excess/anomalous losses (scales as h^1.5).
    pub f_excess: f64,
}

impl Default for CoreLossModel {
    fn default() -> Self {
        Self {
            f_eddy: 0.5,
            f_hyst: 0.5,
            f_excess: 0.0,
        }
    }
}

impl CoreLossModel {
    /// Validate that fractions sum to 1.0 and are non-negative.
    pub fn validate(&self) -> Result<(), String> {
        if self.f_eddy < 0.0 || self.f_hyst < 0.0 || self.f_excess < 0.0 {
            return Err("core loss fractions must be non-negative".to_string());
        }
        let sum = self.f_eddy + self.f_hyst + self.f_excess;
        if (sum - 1.0).abs() > 1e-6 {
            return Err(format!("core loss fractions must sum to 1.0, got {sum:.6}"));
        }
        Ok(())
    }

    /// Compute the core loss conductance scaling factor at harmonic order h.
    ///
    /// Returns the multiplier to apply to `g_mag`:
    /// `g_core(h) = g_mag * self.scale(h)`
    ///
    /// At h=1, returns 1.0 (by construction, since fractions sum to 1.0).
    pub fn scale(&self, h: f64) -> f64 {
        self.f_eddy * h * h + self.f_hyst * h.powf(1.6) + self.f_excess * h.powf(1.5)
    }
}

// ---------------------------------------------------------------------------
// ConverterCommutationModel
// ---------------------------------------------------------------------------

/// 6-pulse converter with voltage-dependent commutation overlap.
///
/// During commutation, two thyristors conduct simultaneously, shorting two
/// phases through the commutation reactance. The overlap angle mu depends on
/// the AC terminal voltage — lower voltage -> larger mu -> more harmonic
/// distortion.
///
/// Reference: Arrillaga & Watson (2003), Chapter 4; Kimbark (1971).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConverterCommutationModel {
    /// Bus number where the converter is connected.
    pub bus: u32,
    /// Number of pulses (6, 12, 18, 24). Default: 6.
    pub pulse_number: u32,
    /// Firing angle alpha in degrees. Default: 15 (rectifier).
    pub firing_angle_deg: f64,
    /// Commutation reactance X_c in per-unit (on converter MVA base).
    pub x_commutation_pu: f64,
    /// DC load current in per-unit (on converter MVA base).
    pub i_dc_pu: f64,
    /// Converter transformer turns ratio (AC:DC side).
    pub transformer_ratio: f64,
    /// Rated power (MVA) for per-unit base conversion to system base.
    pub rated_mva: f64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn typical_curve() -> SaturationCurve {
        SaturationCurve::typical_power_transformer()
    }

    #[test]
    fn pwl_interpolation_interior() {
        let c = typical_curve();
        // At phi = 1.05 (between 1.0 and 1.1), should interpolate
        let i_m = c.evaluate(1.05);
        assert!(i_m > 0.01, "i_m at 1.05 pu should be > 0.01, got {i_m}");
        assert!(i_m < 0.03, "i_m at 1.05 pu should be < 0.03, got {i_m}");
        // Check it's between the two bounding values
        let i_at_1_0 = c.evaluate(1.0);
        let i_at_1_1 = c.evaluate(1.1);
        assert!(i_m > i_at_1_0 && i_m < i_at_1_1);
    }

    #[test]
    fn pwl_interpolation_extrapolation() {
        let c = typical_curve();
        // Beyond last point (phi=1.30), should extrapolate linearly
        let i_at_1_3 = c.evaluate(1.30);
        let i_at_1_5 = c.evaluate(1.50);
        assert!(i_at_1_5 > i_at_1_3, "extrapolation should be increasing");
        // Check the slope matches the last segment
        let last = c.points.last().unwrap();
        let prev = &c.points[c.points.len() - 2];
        let expected_slope = (last.i_m_pu - prev.i_m_pu) / (last.phi_pu - prev.phi_pu);
        let actual_slope = (i_at_1_5 - i_at_1_3) / 0.2;
        assert!(
            (actual_slope - expected_slope).abs() < 1e-10,
            "extrapolation slope {actual_slope:.6} != expected {expected_slope:.6}"
        );
    }

    #[test]
    fn pwl_odd_symmetry() {
        let c = typical_curve();
        for phi in [0.5, 1.0, 1.1, 1.2, 1.3] {
            let pos = c.evaluate(phi);
            let neg = c.evaluate(-phi);
            assert!(
                (pos + neg).abs() < 1e-15,
                "odd symmetry violated at phi={phi}: f({phi})={pos}, f(-{phi})={neg}"
            );
        }
    }

    #[test]
    fn two_slope_to_pwl() {
        let ts = TwoSlopeSaturation {
            phi_knee_pu: 1.15,
            b_mag_unsat: 0.01,
            x_air_pu: 0.3,
        };
        let pwl = ts.to_piecewise_linear();
        assert!(pwl.validate().is_ok());
        // At the knee, i_m should be phi_knee * b_mag
        let i_at_knee = pwl.evaluate(ts.phi_knee_pu);
        let expected = ts.phi_knee_pu * ts.b_mag_unsat;
        assert!(
            (i_at_knee - expected).abs() < 1e-6,
            "at knee: i_m={i_at_knee:.6}, expected {expected:.6}"
        );
        // Above knee, slope should be steeper (1/x_air)
        let i_above = pwl.evaluate(1.25);
        assert!(i_above > i_at_knee);
    }

    #[test]
    fn polynomial_to_pwl() {
        let poly = PolynomialSaturation {
            coefficients: vec![1.0, 0.5], // I = Phi + 0.5*Phi^3
        };
        let pwl = poly.to_piecewise_linear(20);
        assert!(pwl.points.len() == 20);
        // Spot-check at phi=1.0: I = 1.0 + 0.5 = 1.5
        let i_m = poly.evaluate(1.0);
        assert!((i_m - 1.5).abs() < 1e-10);
    }

    #[test]
    fn validate_rejects_nonmonotonic() {
        let c = SaturationCurve {
            points: vec![
                SaturationPoint {
                    i_m_pu: 0.0,
                    phi_pu: 0.0,
                },
                SaturationPoint {
                    i_m_pu: 0.1,
                    phi_pu: 1.0,
                },
                SaturationPoint {
                    i_m_pu: 0.05,
                    phi_pu: 1.1,
                }, // non-monotonic i_m
            ],
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_single_point() {
        let c = SaturationCurve {
            points: vec![SaturationPoint {
                i_m_pu: 0.01,
                phi_pu: 1.0,
            }],
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn core_loss_scale_at_h1() {
        let m = CoreLossModel::default();
        let s = m.scale(1.0);
        assert!(
            (s - 1.0).abs() < 1e-10,
            "scale at h=1 should be 1.0, got {s}"
        );
    }

    #[test]
    fn core_loss_scale_at_h5() {
        let m = CoreLossModel::default(); // 50/50 eddy/hysteresis
        let s = m.scale(5.0);
        // 0.5 * 25 + 0.5 * 5^1.6
        let expected = 0.5 * 25.0 + 0.5 * 5.0_f64.powf(1.6);
        assert!(
            (s - expected).abs() < 1e-10,
            "scale at h=5: got {s:.6}, expected {expected:.6}"
        );
        assert!(
            s > 10.0,
            "core loss at 5th harmonic should be >10x fundamental"
        );
    }

    #[test]
    fn core_loss_all_eddy() {
        let m = CoreLossModel {
            f_eddy: 1.0,
            f_hyst: 0.0,
            f_excess: 0.0,
        };
        assert!(m.validate().is_ok());
        let s = m.scale(7.0);
        assert!((s - 49.0).abs() < 1e-10, "all-eddy scale at h=7 = h^2 = 49");
    }

    #[test]
    fn core_loss_validate_rejects_bad_sum() {
        let m = CoreLossModel {
            f_eddy: 0.5,
            f_hyst: 0.3,
            f_excess: 0.0,
        };
        assert!(m.validate().is_err());
    }

    #[test]
    fn core_type_k_factor() {
        assert!((CoreType::ThreeLimbCore.k_factor() - 0.33).abs() < 1e-10);
        assert!((CoreType::FiveLimbCore.k_factor() - 1.18).abs() < 1e-10);
        assert!((CoreType::Custom(2.5).k_factor() - 2.5).abs() < 1e-10);
    }

    #[test]
    fn core_type_from_str() {
        assert_eq!(
            "3-limb".parse::<CoreType>().unwrap(),
            CoreType::ThreeLimbCore
        );
        assert_eq!(
            "5-limb-shell".parse::<CoreType>().unwrap(),
            CoreType::FiveLimbShell
        );
        assert!("invalid".parse::<CoreType>().is_err());
    }

    #[test]
    fn inverse_lookup() {
        let c = typical_curve();
        // Round-trip: evaluate at some phi, then inverse should give back phi
        for phi in [0.5, 1.0, 1.1, 1.15, 1.2] {
            let i_m = c.evaluate(phi);
            let phi_back = c.evaluate_inverse(i_m);
            assert!(
                (phi_back - phi).abs() < 1e-10,
                "round-trip failed at phi={phi}: i_m={i_m}, phi_back={phi_back}"
            );
        }
    }
}
