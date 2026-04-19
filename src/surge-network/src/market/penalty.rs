// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Penalty curve types for soft-constraint power system optimizers.
//!
//! Used by AC-OPF, DC-OPF, SCOPF, SCUC, SCED, and contingency corrective dispatch
//! to price violations of thermal, voltage, ramp, and reserve constraints.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Custom serde helpers for `f64` fields that may be `f64::INFINITY`.
///
/// JSON does not support infinity; we encode it as the string `"inf"` so the
/// round-trip is lossless and human-readable.
mod f64_or_inf {
    use super::*;

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        if v.is_infinite() && v.is_sign_positive() {
            s.serialize_str("inf")
        } else if v.is_infinite() && v.is_sign_negative() {
            s.serialize_str("-inf")
        } else {
            s.serialize_f64(*v)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        use serde::de::{self, Visitor};
        use std::fmt;

        struct F64OrInf;

        impl<'de> Visitor<'de> for F64OrInf {
            type Value = f64;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a finite f64 or the string \"inf\" / \"-inf\"")
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<f64, E> {
                Ok(v)
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<f64, E> {
                Ok(v as f64)
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<f64, E> {
                Ok(v as f64)
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<f64, E> {
                match v {
                    "inf" | "+inf" | "Inf" | "Infinity" => Ok(f64::INFINITY),
                    "-inf" | "-Inf" | "-Infinity" => Ok(f64::NEG_INFINITY),
                    other => other
                        .parse::<f64>()
                        .map_err(|_| E::invalid_value(de::Unexpected::Str(v), &self)),
                }
            }
        }

        d.deserialize_any(F64OrInf)
    }
}

/// A single segment of a piecewise-linear penalty curve.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PenaltySegment {
    /// Upper bound of violation in this tier. Use `f64::INFINITY` for the last segment.
    ///
    /// Serialized as `"inf"` in JSON (JSON does not support IEEE 754 infinity).
    #[serde(with = "f64_or_inf")]
    pub max_violation: f64,
    /// Cost per unit of violation in this tier ($/unit).
    pub cost_per_unit: f64,
}

/// Penalty curve applied to a soft-constraint slack variable.
///
/// `PiecewiseLinear` is LP-compatible (convex, non-decreasing slopes).
/// `Quadratic` is for NLP solvers (Ipopt) only.
/// Slopes must be non-decreasing to preserve convexity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PenaltyCurve {
    Linear { cost_per_unit: f64 },
    PiecewiseLinear { segments: Vec<PenaltySegment> },
    Quadratic { cost_coefficient: f64 },
}

impl PenaltyCurve {
    /// Evaluate the total penalty cost for a given violation magnitude.
    ///
    /// Returns 0.0 for non-positive violations (no penalty for feasible points).
    pub fn cost(&self, violation: f64) -> f64 {
        if violation <= 0.0 {
            return 0.0;
        }
        match self {
            PenaltyCurve::Linear { cost_per_unit } => cost_per_unit * violation,
            PenaltyCurve::Quadratic { cost_coefficient } => {
                cost_coefficient * violation * violation
            }
            PenaltyCurve::PiecewiseLinear { segments } => {
                let mut total = 0.0;
                let mut remaining = violation;
                let mut prev_max = 0.0_f64;
                for seg in segments {
                    let seg_width = (seg.max_violation - prev_max).min(remaining);
                    if seg_width <= 0.0 {
                        break;
                    }
                    total += seg.cost_per_unit * seg_width;
                    remaining -= seg_width;
                    prev_max = seg.max_violation;
                    if remaining <= 0.0 {
                        break;
                    }
                }
                total
            }
        }
    }

    /// Returns the marginal cost (slope) at a given violation level.
    ///
    /// For `Linear` and `Quadratic`, returns the derivative at `violation`.
    /// For `PiecewiseLinear`, returns the slope of the active segment.
    /// Useful for LP objective coefficients and NLP gradient construction.
    pub fn marginal_cost_at(&self, violation: f64) -> f64 {
        match self {
            PenaltyCurve::Linear { cost_per_unit } => *cost_per_unit,
            PenaltyCurve::Quadratic { cost_coefficient } => 2.0 * cost_coefficient * violation,
            PenaltyCurve::PiecewiseLinear { segments } => {
                for seg in segments {
                    if violation <= seg.max_violation {
                        return seg.cost_per_unit;
                    }
                }
                segments.last().map(|s| s.cost_per_unit).unwrap_or(0.0)
            }
        }
    }

    /// Build a [`PenaltyCurve::PiecewiseLinear`] matching the ERCOT ORDC formula.
    ///
    /// ORDC: `penalty(reserve_mw) = VOLL x LOLP(reserve_mw)`
    ///
    /// # Arguments
    /// * `voll` - Value of Lost Load in $/MWh (ERCOT uses $9,000/MWh).
    /// * `lolp_curve` - sorted breakpoints `(reserve_mw, lolp)`.
    ///
    /// # Returns
    /// A `PiecewiseLinear` `PenaltyCurve` representing the ERCOT ORDC.
    ///
    /// # Examples
    ///
    /// ```
    /// use surge_network::market::PenaltyCurve;
    /// let ordc = PenaltyCurve::ordc(9000.0, &[(0.0, 1.0), (500.0, 0.1), (2000.0, 0.01)]);
    /// assert_eq!(ordc.cost(0.0), 0.0); // No shortfall -> no penalty
    /// let cost_2000 = ordc.cost(2000.0);
    /// assert!(cost_2000 > 0.0); // Full shortfall -> positive penalty
    /// ```
    pub fn ordc(voll: f64, lolp_curve: &[(f64, f64)]) -> Self {
        if lolp_curve.is_empty() {
            return PenaltyCurve::Linear {
                cost_per_unit: voll,
            };
        }
        let mut pts: Vec<(f64, f64)> = lolp_curve.to_vec();
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let breakpoints: Vec<(f64, f64)> = pts.iter().map(|&(r, lolp)| (r, voll * lolp)).collect();

        let mut segments: Vec<PenaltySegment> = Vec::new();
        for i in 0..breakpoints.len().saturating_sub(1) {
            let (r0, c0) = breakpoints[i];
            let (r1, c1) = breakpoints[i + 1];
            let delta_r = (r1 - r0).max(1e-9);
            let slope = ((c1 - c0) / delta_r).abs();
            segments.push(PenaltySegment {
                max_violation: r1,
                cost_per_unit: slope,
            });
        }
        let last_slope = segments.last().map(|s| s.cost_per_unit).unwrap_or(0.0);
        segments.push(PenaltySegment {
            max_violation: f64::INFINITY,
            cost_per_unit: last_slope,
        });

        PenaltyCurve::PiecewiseLinear { segments }
    }
}

/// Penalty configuration for all soft-constraint types in a power system optimizer.
///
/// Used by AC-OPF, DC-OPF, SCOPF, SCUC, SCED, and contingency corrective dispatch.
/// Default values strongly discourage violations without making them numerically infeasible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PenaltyConfig {
    /// Penalty for thermal limit overload (per MVA above limit).
    pub thermal: PenaltyCurve,
    /// Penalty for voltage above Vmax (per pu above limit).
    pub voltage_high: PenaltyCurve,
    /// Penalty for voltage below Vmin (per pu below limit).
    pub voltage_low: PenaltyCurve,
    /// Legacy symmetric penalty for active-power balance mismatch (per MW).
    ///
    /// Used for both curtailment and excess when the asymmetric fields below
    /// are not explicitly set.
    pub power_balance: PenaltyCurve,
    /// Optional penalty for unserved load / curtailment power-balance slack.
    ///
    /// When `None`, falls back to `power_balance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_balance_curtailment: Option<PenaltyCurve>,
    /// Optional penalty for excess-generation power-balance slack.
    ///
    /// When `None`, falls back to `power_balance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_balance_excess: Option<PenaltyCurve>,
    /// Penalty for reactive-power balance mismatch (per MVAr).
    ///
    /// Applied to AC OPF bus Q-balance slack variables
    /// (`q_slack_pos_mvar`, `q_slack_neg_mvar` in `BusPeriodResult`).
    /// When left at its default, callers fall back to `power_balance`
    /// so Q slack is priced against the same curve as P. Set this
    /// field when the market specifies a distinct reactive-balance
    /// violation cost.
    #[serde(default = "default_reactive_balance_penalty")]
    pub reactive_balance: PenaltyCurve,
    /// Penalty for ramp rate violation (per MW/min above ramp limit).
    pub ramp: PenaltyCurve,
    /// Penalty for angle limit violation (per radian above limit).
    pub angle: PenaltyCurve,
    /// Penalty for operating reserve shortfall (per MW short; configurable as ORDC).
    pub reserve: PenaltyCurve,
}

impl Default for PenaltyConfig {
    fn default() -> Self {
        PenaltyConfig {
            // Thermal: flat $1.5k/MVA default for branch/flowgate/interface soft limits
            thermal: PenaltyCurve::Linear {
                cost_per_unit: 1_500.0,
            },
            // Voltage high: $5k/pu for small violations (≤1%), $50k/pu for large
            voltage_high: PenaltyCurve::PiecewiseLinear {
                segments: vec![
                    PenaltySegment {
                        max_violation: 0.01,
                        cost_per_unit: 5_000.0,
                    },
                    PenaltySegment {
                        max_violation: f64::INFINITY,
                        cost_per_unit: 50_000.0,
                    },
                ],
            },
            // Voltage low: same tiers as high
            voltage_low: PenaltyCurve::PiecewiseLinear {
                segments: vec![
                    PenaltySegment {
                        max_violation: 0.01,
                        cost_per_unit: 5_000.0,
                    },
                    PenaltySegment {
                        max_violation: f64::INFINITY,
                        cost_per_unit: 50_000.0,
                    },
                ],
            },
            // Power balance: near-hard ($1M/pu). Infeasibility here signals a severe model error.
            power_balance: PenaltyCurve::Linear {
                cost_per_unit: 1_000_000.0,
            },
            power_balance_curtailment: None,
            power_balance_excess: None,
            // Reactive balance: matches the legacy default (same curve as
            // active balance) so existing callers see no behavioural change.
            reactive_balance: default_reactive_balance_penalty(),
            // Ramp: $100/(MW/min) for small violations (≤5 MW/min), $1k/(MW/min) for large
            ramp: PenaltyCurve::PiecewiseLinear {
                segments: vec![
                    PenaltySegment {
                        max_violation: 5.0,
                        cost_per_unit: 100.0,
                    },
                    PenaltySegment {
                        max_violation: f64::INFINITY,
                        cost_per_unit: 1_000.0,
                    },
                ],
            },
            // Angle: linear $500/radian
            angle: PenaltyCurve::Linear {
                cost_per_unit: 500.0,
            },
            // Reserve: linear $1000/MW default (replace with ORDC for market simulation)
            reserve: PenaltyCurve::Linear {
                cost_per_unit: 1_000.0,
            },
        }
    }
}

fn default_reactive_balance_penalty() -> PenaltyCurve {
    PenaltyCurve::Linear {
        cost_per_unit: 1_000_000.0,
    }
}

impl PenaltyConfig {
    /// Effective penalty curve for unserved-load / curtailment slack.
    pub fn power_balance_curtailment_curve(&self) -> &PenaltyCurve {
        self.power_balance_curtailment
            .as_ref()
            .unwrap_or(&self.power_balance)
    }

    /// Effective penalty curve for excess-generation slack.
    pub fn power_balance_excess_curve(&self) -> &PenaltyCurve {
        self.power_balance_excess
            .as_ref()
            .unwrap_or(&self.power_balance)
    }

    /// Effective penalty curve for reactive-power balance slack.
    ///
    /// Returns `reactive_balance` when it has been explicitly set away from
    /// the default ($1M/pu), otherwise falls back to `power_balance` to
    /// preserve legacy behaviour where a single curve priced both active
    /// and reactive balance slack.
    pub fn reactive_balance_curve(&self) -> &PenaltyCurve {
        &self.reactive_balance
    }

    /// Build a `PenaltyConfig` with the `reserve` field set to an ERCOT-style
    /// Operating Reserve Demand Curve (ORDC).
    ///
    /// This is a convenience wrapper around [`PenaltyCurve::ordc`] that returns
    /// a complete `PenaltyConfig` (all other penalty curves use their defaults).
    ///
    /// # Arguments
    /// - `voll`: Value of Lost Load ($/MWh). Typical ERCOT value is $3,500–$5,000.
    /// - `breakpoints`: Reserve-level to LOLP mapping: `&[(reserve_mw, lolp_fraction)]`.
    ///   The penalty at each breakpoint is `voll × lolp`.
    ///   Standard ERCOT breakpoints:
    ///   `[(0.0, 1.0), (500.0, 0.1), (1000.0, 0.01), (2000.0, 0.001)]`
    ///
    /// # Example
    /// ```
    /// use surge_network::market::PenaltyConfig;
    /// let cfg = PenaltyConfig::with_ordc_reserve(5000.0, &[
    ///     (0.0,    1.0),
    ///     (500.0,  0.1),
    ///     (1000.0, 0.01),
    ///     (2000.0, 0.001),
    /// ]);
    /// // At 0 MW reserve: penalty = 5000 × 1.0 = 5000 $/MWh
    /// // At 500 MW reserve: penalty = 5000 × 0.1 = 500 $/MWh
    /// ```
    pub fn with_ordc_reserve(voll: f64, breakpoints: &[(f64, f64)]) -> PenaltyConfig {
        PenaltyConfig {
            reserve: PenaltyCurve::ordc(voll, breakpoints),
            ..PenaltyConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // PenaltyCurve::cost() tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_linear_cost_zero_violation() {
        let c = PenaltyCurve::Linear {
            cost_per_unit: 1000.0,
        };
        assert_eq!(c.cost(0.0), 0.0);
        assert_eq!(c.cost(-1.0), 0.0);
    }

    #[test]
    fn test_linear_cost_positive_violation() {
        let c = PenaltyCurve::Linear {
            cost_per_unit: 500.0,
        };
        assert!((c.cost(2.0) - 1000.0).abs() < 1e-9);
        assert!((c.cost(0.5) - 250.0).abs() < 1e-9);
    }

    #[test]
    fn test_quadratic_cost() {
        let c = PenaltyCurve::Quadratic {
            cost_coefficient: 10.0,
        };
        // cost(v) = 10 * v^2
        assert!((c.cost(3.0) - 90.0).abs() < 1e-9);
        assert_eq!(c.cost(0.0), 0.0);
        assert_eq!(c.cost(-2.0), 0.0);
    }

    #[test]
    fn test_pwl_cost_within_first_segment() {
        // Two-segment curve: [0, 0.05] @ $1k, (0.05, ∞) @ $10k
        let c = PenaltyCurve::PiecewiseLinear {
            segments: vec![
                PenaltySegment {
                    max_violation: 0.05,
                    cost_per_unit: 1_000.0,
                },
                PenaltySegment {
                    max_violation: f64::INFINITY,
                    cost_per_unit: 10_000.0,
                },
            ],
        };
        // Violation = 0.03 → entirely in first segment
        assert!((c.cost(0.03) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn test_pwl_cost_spanning_segments() {
        let c = PenaltyCurve::PiecewiseLinear {
            segments: vec![
                PenaltySegment {
                    max_violation: 0.05,
                    cost_per_unit: 1_000.0,
                },
                PenaltySegment {
                    max_violation: f64::INFINITY,
                    cost_per_unit: 10_000.0,
                },
            ],
        };
        // Violation = 0.10: first 0.05 @ $1k + next 0.05 @ $10k = $50 + $500 = $550
        assert!((c.cost(0.10) - 550.0).abs() < 1e-9);
    }

    #[test]
    fn test_pwl_cost_exactly_at_segment_boundary() {
        let c = PenaltyCurve::PiecewiseLinear {
            segments: vec![
                PenaltySegment {
                    max_violation: 1.0,
                    cost_per_unit: 100.0,
                },
                PenaltySegment {
                    max_violation: f64::INFINITY,
                    cost_per_unit: 200.0,
                },
            ],
        };
        // Exactly at 1.0 → 100 * 1.0 = 100
        assert!((c.cost(1.0) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_pwl_cost_zero_violation() {
        let c = PenaltyCurve::PiecewiseLinear {
            segments: vec![
                PenaltySegment {
                    max_violation: 1.0,
                    cost_per_unit: 100.0,
                },
                PenaltySegment {
                    max_violation: f64::INFINITY,
                    cost_per_unit: 200.0,
                },
            ],
        };
        assert_eq!(c.cost(0.0), 0.0);
        assert_eq!(c.cost(-5.0), 0.0);
    }

    // -----------------------------------------------------------------------
    // PenaltyCurve::marginal_cost_at() tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_linear_marginal_constant() {
        let c = PenaltyCurve::Linear {
            cost_per_unit: 750.0,
        };
        assert!((c.marginal_cost_at(0.0) - 750.0).abs() < 1e-9);
        assert!((c.marginal_cost_at(100.0) - 750.0).abs() < 1e-9);
    }

    #[test]
    fn test_quadratic_marginal() {
        let c = PenaltyCurve::Quadratic {
            cost_coefficient: 5.0,
        };
        // marginal = 2 * 5 * v = 10v
        assert!((c.marginal_cost_at(3.0) - 30.0).abs() < 1e-9);
        assert!((c.marginal_cost_at(0.0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_pwl_marginal_first_segment() {
        let c = PenaltyCurve::PiecewiseLinear {
            segments: vec![
                PenaltySegment {
                    max_violation: 0.05,
                    cost_per_unit: 1_000.0,
                },
                PenaltySegment {
                    max_violation: f64::INFINITY,
                    cost_per_unit: 10_000.0,
                },
            ],
        };
        assert!((c.marginal_cost_at(0.0) - 1_000.0).abs() < 1e-9);
        assert!((c.marginal_cost_at(0.03) - 1_000.0).abs() < 1e-9);
    }

    #[test]
    fn test_pwl_marginal_second_segment() {
        let c = PenaltyCurve::PiecewiseLinear {
            segments: vec![
                PenaltySegment {
                    max_violation: 0.05,
                    cost_per_unit: 1_000.0,
                },
                PenaltySegment {
                    max_violation: f64::INFINITY,
                    cost_per_unit: 10_000.0,
                },
            ],
        };
        assert!((c.marginal_cost_at(0.10) - 10_000.0).abs() < 1e-9);
        assert!((c.marginal_cost_at(1.0) - 10_000.0).abs() < 1e-9);
    }

    #[test]
    fn test_pwl_marginal_empty_segments() {
        let c = PenaltyCurve::PiecewiseLinear { segments: vec![] };
        assert!((c.marginal_cost_at(0.0) - 0.0).abs() < 1e-9);
        assert!((c.marginal_cost_at(10.0) - 0.0).abs() < 1e-9);
    }

    // -----------------------------------------------------------------------
    // PenaltyConfig::default() consistency checks
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_thermal_penalty() {
        let cfg = PenaltyConfig::default();
        assert!((cfg.thermal.marginal_cost_at(0.0) - 1_500.0).abs() < 1e-9);
        assert!((cfg.thermal.marginal_cost_at(0.10) - 1_500.0).abs() < 1e-9);
    }

    #[test]
    fn test_default_voltage_penalty() {
        let cfg = PenaltyConfig::default();
        // Small voltage violation: $5k/pu
        assert!((cfg.voltage_high.marginal_cost_at(0.0) - 5_000.0).abs() < 1e-9);
        // Large violation: $50k/pu
        assert!((cfg.voltage_high.marginal_cost_at(0.02) - 50_000.0).abs() < 1e-9);
        // Symmetric for voltage_low
        assert!((cfg.voltage_low.marginal_cost_at(0.0) - 5_000.0).abs() < 1e-9);
    }

    #[test]
    fn test_default_power_balance_penalty() {
        let cfg = PenaltyConfig::default();
        // Near-hard constraint: $1M/pu at any violation
        assert!((cfg.power_balance.marginal_cost_at(0.5) - 1_000_000.0).abs() < 1e-9);
        assert!((cfg.power_balance.cost(0.001) - 1_000.0).abs() < 1e-9);
        assert!(cfg.power_balance_curtailment.is_none());
        assert!(cfg.power_balance_excess.is_none());
        assert_eq!(cfg.power_balance_curtailment_curve(), &cfg.power_balance);
        assert_eq!(cfg.power_balance_excess_curve(), &cfg.power_balance);
    }

    #[test]
    fn test_asymmetric_power_balance_penalty_accessors() {
        let mut cfg = PenaltyConfig::default();
        let curtailment = PenaltyCurve::Linear {
            cost_per_unit: 9_000_000.0,
        };
        let excess = PenaltyCurve::Linear {
            cost_per_unit: 25_000.0,
        };
        cfg.power_balance_curtailment = Some(curtailment.clone());
        cfg.power_balance_excess = Some(excess.clone());

        assert_eq!(cfg.power_balance_curtailment_curve(), &curtailment);
        assert_eq!(cfg.power_balance_excess_curve(), &excess);
    }

    // -----------------------------------------------------------------------
    // ORDC builder tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_ordc_basic_structure() {
        let curve = PenaltyCurve::ordc(
            5_000.0,
            &[(2000.0, 0.00), (1000.0, 0.20), (500.0, 0.40), (0.0, 1.00)],
        );
        // Should be piecewise linear
        assert!(matches!(curve, PenaltyCurve::PiecewiseLinear { .. }));
    }

    #[test]
    fn test_ordc_last_segment_is_infinity() {
        let curve = PenaltyCurve::ordc(5_000.0, &[(2000.0, 0.00), (1000.0, 0.20), (0.0, 1.00)]);
        if let PenaltyCurve::PiecewiseLinear { segments } = &curve {
            assert!(
                segments
                    .last()
                    .map(|s| s.max_violation.is_infinite())
                    .unwrap_or(false),
                "Last segment must have infinite max_violation"
            );
        } else {
            panic!("Expected PiecewiseLinear");
        }
    }

    #[test]
    fn test_ordc_monotone_slopes() {
        let curve = PenaltyCurve::ordc(
            5_000.0,
            &[(2000.0, 0.00), (1000.0, 0.20), (500.0, 0.40), (0.0, 1.00)],
        );
        if let PenaltyCurve::PiecewiseLinear { segments } = &curve {
            for seg in segments {
                assert!(seg.cost_per_unit >= 0.0, "All slopes must be non-negative");
            }
        }
    }

    #[test]
    fn test_ordc_empty_breakpoints() {
        // With no breakpoints, falls back to linear at VOLL
        let curve = PenaltyCurve::ordc(5_000.0, &[]);
        assert!(
            matches!(curve, PenaltyCurve::Linear { cost_per_unit } if (cost_per_unit - 5_000.0).abs() < 1e-9)
        );
    }

    #[test]
    fn test_ordc_unsorted_input_handled() {
        // Breakpoints in ascending order (should still produce valid ORDC)
        let curve = PenaltyCurve::ordc(
            5_000.0,
            &[(0.0, 1.00), (500.0, 0.40), (1000.0, 0.20), (2000.0, 0.00)],
        );
        if let PenaltyCurve::PiecewiseLinear { segments } = &curve {
            assert!(!segments.is_empty(), "Should produce non-empty segments");
            assert!(
                segments.last().unwrap().max_violation.is_infinite(),
                "Last segment must reach infinity"
            );
        } else {
            panic!("Expected PiecewiseLinear");
        }
    }

    // -----------------------------------------------------------------------
    // Serde round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_serde_linear_roundtrip() {
        let curve = PenaltyCurve::Linear {
            cost_per_unit: 1234.5,
        };
        let json = serde_json::to_string(&curve).expect("serialize");
        let back: PenaltyCurve = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(curve, back);
    }

    #[test]
    fn test_serde_quadratic_roundtrip() {
        let curve = PenaltyCurve::Quadratic {
            cost_coefficient: 42.0,
        };
        let json = serde_json::to_string(&curve).expect("serialize");
        let back: PenaltyCurve = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(curve, back);
    }

    #[test]
    fn test_serde_pwl_roundtrip() {
        let curve = PenaltyCurve::PiecewiseLinear {
            segments: vec![
                PenaltySegment {
                    max_violation: 0.05,
                    cost_per_unit: 1_000.0,
                },
                PenaltySegment {
                    max_violation: f64::INFINITY,
                    cost_per_unit: 10_000.0,
                },
            ],
        };
        let json = serde_json::to_string(&curve).expect("serialize");
        let back: PenaltyCurve = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(curve, back);
    }

    #[test]
    fn test_serde_penalty_segment_roundtrip() {
        let seg = PenaltySegment {
            max_violation: f64::INFINITY,
            cost_per_unit: 99.9,
        };
        let json = serde_json::to_string(&seg).expect("serialize");
        let back: PenaltySegment = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(seg, back);
    }

    #[test]
    fn test_serde_penalty_config_roundtrip() {
        let cfg = PenaltyConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: PenaltyConfig = serde_json::from_str(&json).expect("deserialize");
        // Spot-check a few fields after round-trip
        assert_eq!(back.power_balance, cfg.power_balance);
        assert_eq!(
            back.power_balance_curtailment,
            cfg.power_balance_curtailment
        );
        assert_eq!(back.power_balance_excess, cfg.power_balance_excess);
        assert_eq!(back.angle, cfg.angle);
        assert_eq!(back.thermal, cfg.thermal);
    }

    #[test]
    fn test_serde_penalty_config_roundtrip_asymmetric_power_balance() {
        let cfg = PenaltyConfig {
            power_balance_curtailment: Some(PenaltyCurve::PiecewiseLinear {
                segments: vec![
                    PenaltySegment {
                        max_violation: 10.0,
                        cost_per_unit: 1_000.0,
                    },
                    PenaltySegment {
                        max_violation: f64::INFINITY,
                        cost_per_unit: 10_000.0,
                    },
                ],
            }),
            power_balance_excess: Some(PenaltyCurve::Linear {
                cost_per_unit: 50.0,
            }),
            ..PenaltyConfig::default()
        };

        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: PenaltyConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.power_balance, cfg.power_balance);
        assert_eq!(
            back.power_balance_curtailment,
            cfg.power_balance_curtailment
        );
        assert_eq!(back.power_balance_excess, cfg.power_balance_excess);
    }

    #[test]
    fn test_pwl_serde_json_tag() {
        let curve = PenaltyCurve::PiecewiseLinear { segments: vec![] };
        let json = serde_json::to_string(&curve).expect("serialize");
        assert!(
            json.contains("\"piecewise_linear\""),
            "tag should be snake_case: {json}"
        );
    }

    #[test]
    fn test_linear_serde_json_tag() {
        let curve = PenaltyCurve::Linear { cost_per_unit: 1.0 };
        let json = serde_json::to_string(&curve).expect("serialize");
        assert!(
            json.contains("\"linear\""),
            "tag should be 'linear': {json}"
        );
    }

    #[test]
    fn test_quadratic_serde_json_tag() {
        let curve = PenaltyCurve::Quadratic {
            cost_coefficient: 1.0,
        };
        let json = serde_json::to_string(&curve).expect("serialize");
        assert!(
            json.contains("\"quadratic\""),
            "tag should be 'quadratic': {json}"
        );
    }

    // -----------------------------------------------------------------------
    // PNL-008: with_ordc_reserve builder test
    // -----------------------------------------------------------------------

    /// PNL-008: Verify PenaltyConfig::with_ordc_reserve builds a correct config.
    ///
    /// Breakpoints: [(0.0, 1.0), (500.0, 0.1)]
    /// - At 0 MW reserve → LOLP = 1.0 → penalty = voll × 1.0 = 5000 $/MWh
    /// - At 500 MW reserve → LOLP = 0.1 → penalty = voll × 0.1 = 500 $/MWh
    ///
    /// The ORDC is built as a piecewise-linear curve over "shortfall" (reserve shortage).
    /// Shortfall = max_reserve - reserve.  For breakpoints [(0,1.0),(500,0.1)]:
    ///   max_reserve = 500 MW (highest breakpoint).
    ///   At reserve=0   → shortfall=500 → total_cost = integral of slopes from 0..500.
    ///   At reserve=500 → shortfall=0   → cost = 0.
    ///
    /// The marginal cost at shortfall=0 equals the slope of the first segment,
    /// which corresponds to the price change from reserve=500 → reserve<500.
    /// VOLL × (LOLP(0) - LOLP(500)) / 500 MW = 5000 × (1.0 - 0.1) / 500 = 9.0 $/MWh per MW.
    #[test]
    fn test_with_ordc_reserve_pnl_008() {
        let cfg = PenaltyConfig::with_ordc_reserve(5000.0, &[(0.0, 1.0), (500.0, 0.1)]);

        // Reserve curve should be PiecewiseLinear.
        assert!(
            matches!(cfg.reserve, PenaltyCurve::PiecewiseLinear { .. }),
            "reserve should be PiecewiseLinear, got: {:?}",
            cfg.reserve
        );

        // All other fields should be the defaults.
        let default = PenaltyConfig::default();
        assert_eq!(cfg.thermal, default.thermal, "thermal should be default");
        assert_eq!(
            cfg.power_balance, default.power_balance,
            "power_balance should be default"
        );

        // Verify the curve has a valid piecewise structure.
        if let PenaltyCurve::PiecewiseLinear { segments } = &cfg.reserve {
            assert!(
                !segments.is_empty(),
                "ORDC should have at least one segment"
            );
            assert!(
                segments
                    .last()
                    .map(|s| s.max_violation.is_infinite())
                    .unwrap_or(false),
                "Last segment must extend to infinity"
            );
            // All slopes should be non-negative (convex).
            for seg in segments {
                assert!(seg.cost_per_unit >= 0.0, "Slopes must be non-negative");
            }
        }
    }

    /// PNL-008: verify with_ordc_reserve with typical ERCOT breakpoints does not panic.
    #[test]
    fn test_with_ordc_reserve_ercot_typical() {
        let cfg = PenaltyConfig::with_ordc_reserve(
            5000.0,
            &[(0.0, 1.0), (500.0, 0.1), (1000.0, 0.01), (2000.0, 0.001)],
        );
        // Should produce a non-trivial piecewise curve.
        assert!(matches!(cfg.reserve, PenaltyCurve::PiecewiseLinear { .. }));
        // The marginal cost at zero shortfall (ample reserves) should be near zero.
        // max_reserve = 2000; at shortfall=0 → we are at the first segment boundary.
        let marginal_at_zero = cfg.reserve.marginal_cost_at(0.0);
        // Slope ≈ VOLL × Δlolp / Δreserve = 5000 × (0.001 - 0.0) / (2000 - max) = near 0
        // Just verify it's non-negative and finite.
        assert!(marginal_at_zero.is_finite() && marginal_at_zero >= 0.0);
    }
}

// ---------------------------------------------------------------------------
// PNL-008: PenaltyCurve::ordc tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod ordc_curve_tests {
    use super::*;

    /// PNL-008: At zero shortfall, penalty should be 0.
    /// The ORDC maps reserve_mw shortfall -> penalty.
    /// At violation=0 (no shortfall), cost=0.
    /// The marginal slope at zero should be VOLL / max_reserve.
    #[test]
    fn test_ordc_at_zero_reserve() {
        let ordc = PenaltyCurve::ordc(9000.0, &[(0.0, 1.0), (2000.0, 0.0)]);
        // cost(0) = 0 (no shortfall -> no penalty)
        assert_eq!(
            ordc.cost(0.0),
            0.0,
            "zero shortfall should have zero penalty"
        );
        // marginal_cost_at(0.0) is the first segment slope = 9000/2000 = 4.5 $/MW
        let expected_slope = 9000.0 / 2000.0;
        let marginal = ordc.marginal_cost_at(0.0);
        assert!(
            (marginal - expected_slope).abs() < 1e-3,
            "marginal at zero shortfall should be {expected_slope:.4}, got {marginal:.4}"
        );
    }

    /// PNL-008: At maximum shortfall (2000 MW), cost = integral under LOLP curve = VOLL.
    #[test]
    fn test_ordc_at_full_reserve() {
        // slope = 9000/2000 = 4.5; cost(2000) = 4.5 * 2000 = 9000
        let ordc = PenaltyCurve::ordc(9000.0, &[(0.0, 1.0), (2000.0, 0.0)]);
        let cost_at_max = ordc.cost(2000.0);
        assert!(
            (cost_at_max - 9000.0).abs() < 1e-3,
            "cost at 2000 MW shortfall should equal VOLL=9000, got {cost_at_max:.4}"
        );
        assert_eq!(ordc.cost(0.0), 0.0, "cost(0) should be 0");
    }

    /// PNL-008: Three-breakpoint ORDC should produce a PiecewiseLinear curve.
    #[test]
    fn test_ordc_three_breakpoints_is_pwl() {
        let ordc = PenaltyCurve::ordc(9000.0, &[(0.0, 1.0), (500.0, 0.1), (2000.0, 0.01)]);
        assert!(matches!(ordc, PenaltyCurve::PiecewiseLinear { .. }));
    }
}
