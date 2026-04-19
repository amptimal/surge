// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical [`PenaltyConfig`] construction from pre-scaled primitives.
//!
//! Market data formats typically store soft-constraint violation
//! costs as per-pu-hour prices. The dispatch kernel's
//! [`PenaltyConfig`], by contrast, consumes `$/MW`, `$/MVAr`,
//! `$/MVA`, etc. Adapters do the per-pu → per-MW conversion (and any
//! format-specific ramp/voltage scaling) once, then hand the result
//! to [`build_penalty_config`] here.
//!
//! The canonical shape applied by this module:
//!
//! * `voltage_high` and `voltage_low` share the same curve (symmetric
//!   over/under voltage penalty).
//! * `power_balance_curtailment` and `power_balance_excess` are left
//!   at `None` — the symmetric `power_balance` curve covers both.
//!
//! Formulations that need asymmetric curtailment / excess penalties
//! should build [`PenaltyConfig`] directly rather than going through
//! this helper.

use surge_network::market::{PenaltyConfig, PenaltyCurve, PenaltySegment};

/// Pre-scaled penalty inputs. All costs are in the dispatch kernel's
/// native units — adapters convert from their source units (e.g. GO
/// C3's per-pu-hour) before calling [`build_penalty_config`].
#[derive(Debug, Clone)]
pub struct PenaltyInputs {
    /// Branch thermal slack cost in $/MVA-h.
    pub thermal_per_mva: f64,
    /// Active bus balance slack cost in $/MW-h.
    pub p_balance_per_mw: f64,
    /// Reactive bus balance slack cost in $/MVAr-h.
    pub q_balance_per_mvar: f64,
    /// Voltage violation curve, applied symmetrically to high and low.
    pub voltage_curve: PenaltyCurve,
    /// Angle deviation slack cost in $/rad-h.
    pub angle_per_rad: f64,
    /// Reserve shortfall cost in $/MW-h.
    pub reserve_per_mw: f64,
    /// Ramp slack cost in $/MW.
    pub ramp_per_mw: f64,
}

/// Build the canonical [`PenaltyConfig`] from pre-scaled inputs.
pub fn build_penalty_config(inputs: &PenaltyInputs) -> PenaltyConfig {
    PenaltyConfig {
        thermal: PenaltyCurve::Linear {
            cost_per_unit: inputs.thermal_per_mva,
        },
        voltage_high: inputs.voltage_curve.clone(),
        voltage_low: inputs.voltage_curve.clone(),
        power_balance: PenaltyCurve::Linear {
            cost_per_unit: inputs.p_balance_per_mw,
        },
        power_balance_curtailment: None,
        power_balance_excess: None,
        reactive_balance: PenaltyCurve::Linear {
            cost_per_unit: inputs.q_balance_per_mvar,
        },
        ramp: PenaltyCurve::Linear {
            cost_per_unit: inputs.ramp_per_mw,
        },
        angle: PenaltyCurve::Linear {
            cost_per_unit: inputs.angle_per_rad,
        },
        reserve: PenaltyCurve::Linear {
            cost_per_unit: inputs.reserve_per_mw,
        },
    }
}

/// Build a two-segment piecewise-linear voltage penalty curve with a
/// knee. Below the knee the cost-per-pu is [`below_knee_per_pu`]; above
/// it switches to [`above_knee_per_pu`], held to `1e30` as the implicit
/// upper bound. This shape matches the canonical "small deviations
/// cheap, large deviations expensive" pattern used by most market
/// formulations.
pub fn voltage_piecewise_curve(
    knee_pu: f64,
    below_knee_per_pu: f64,
    above_knee_per_pu: f64,
) -> PenaltyCurve {
    PenaltyCurve::PiecewiseLinear {
        segments: vec![
            PenaltySegment {
                max_violation: knee_pu,
                cost_per_unit: below_knee_per_pu,
            },
            PenaltySegment {
                max_violation: 1.0e30,
                cost_per_unit: above_knee_per_pu,
            },
        ],
    }
}
