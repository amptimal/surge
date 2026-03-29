// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Transmission interfaces and flowgates.
//!
//! An **Interface** is a set of transmission branches defining a flow boundary
//! between two areas.  Interface flow = sum of (coefficient * branch MW flow).
//!
//! A **Flowgate** is a monitored element (or set of elements) under a specific
//! N-1 contingency.  All in-service flowgates are enforced in DC-OPF/SCED/SCUC
//! as linear constraints on base-case monitored-element flow; for contingency
//! flowgates the OTDF-adjusted limit is pre-computed offline and stored in
//! `limit_mw`.  Contingency flowgates (contingency_branch = Some(...)) are enforced dynamically
//! in SCOPF via OTDF-based cuts; see surge_opf::scopf::solve_scopf.

use serde::{Deserialize, Serialize};

use crate::network::{BranchRef, WeightedBranchRef};

/// A transmission interface: a set of branches defining a flow boundary.
///
/// Interface flow = sum of (coefficient * branch MW flow).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interface {
    /// Human-readable name (e.g. "Houston Import").
    pub name: String,
    /// Weighted branch members defining the interface flow boundary.
    pub members: Vec<WeightedBranchRef>,
    /// MW limit (forward direction).
    pub limit_forward_mw: f64,
    /// MW limit (reverse direction, typically a positive value representing the
    /// magnitude of allowable reverse flow).
    pub limit_reverse_mw: f64,
    /// Whether this interface is actively monitored.
    pub in_service: bool,
    /// Per-timestep forward MW limit schedule (optional).
    ///
    /// When non-empty, `effective_limit_forward_mw(t)` returns `schedule[t]`
    /// for timesteps within range, falling back to `limit_forward_mw` otherwise.
    /// Enables dynamic interface limits (e.g., ambient-adjusted thermal limits).
    #[serde(default)]
    pub limit_forward_mw_schedule: Vec<f64>,
    /// Per-timestep reverse MW limit schedule (optional).
    ///
    /// When non-empty, `effective_limit_reverse_mw(t)` returns `schedule[t]`
    /// for timesteps within range, falling back to `limit_reverse_mw` otherwise.
    #[serde(default)]
    pub limit_reverse_mw_schedule: Vec<f64>,
}

impl Interface {
    /// Forward MW limit at timestep `t`.
    ///
    /// Returns `limit_forward_mw_schedule[t]` when available, else `limit_forward_mw`.
    pub fn effective_limit_forward_mw(&self, t: usize) -> f64 {
        self.limit_forward_mw_schedule
            .get(t)
            .copied()
            .unwrap_or(self.limit_forward_mw)
    }

    /// Reverse MW limit at timestep `t`.
    ///
    /// Returns `limit_reverse_mw_schedule[t]` when available, else `limit_reverse_mw`.
    pub fn effective_limit_reverse_mw(&self, t: usize) -> f64 {
        self.limit_reverse_mw_schedule
            .get(t)
            .copied()
            .unwrap_or(self.limit_reverse_mw)
    }
}

/// A flowgate: a monitored element under a specific contingency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flowgate {
    /// Human-readable name (e.g. "FG_123").
    pub name: String,
    /// The monitored element(s) with signed coefficients.
    pub monitored: Vec<WeightedBranchRef>,
    /// The contingency element (branch that trips). `None` = base-case-only flowgate.
    pub contingency_branch: Option<BranchRef>,
    /// Forward MW limit (positive direction defined by monitored_coefficients).
    pub limit_mw: f64,
    /// Reverse MW limit (magnitude of allowable reverse flow).
    /// When zero (default), the forward limit is applied symmetrically.
    #[serde(default)]
    pub limit_reverse_mw: f64,
    /// Whether this flowgate is actively monitored.
    pub in_service: bool,
    /// Per-timestep forward MW limit schedule (optional).
    ///
    /// When non-empty, `effective_limit_mw(t)` returns `schedule[t]`
    /// for timesteps within range, falling back to `limit_mw` otherwise.
    /// Enables dynamic flowgate limits (ambient ratings, planned outage windows).
    #[serde(default)]
    pub limit_mw_schedule: Vec<f64>,
    /// Per-timestep reverse MW limit schedule (optional).
    ///
    /// When non-empty, `effective_limit_reverse_mw(t)` returns `schedule[t]`
    /// for timesteps within range, falling back to `limit_reverse_mw` otherwise.
    #[serde(default)]
    pub limit_reverse_mw_schedule: Vec<f64>,
    /// HVDC link coefficients for N-1 HVDC contingency constraints.
    /// Each entry: `(hvdc_link_index, coefficient_pu)`.
    /// When non-empty, the flowgate constraint includes HVDC dispatch variable terms:
    ///   `Σ coeff_i·b_dc_i·(θ_from_i − θ_to_i) + Σ hvdc_coeff_k·P_hvdc[k] ∈ [-limit, limit]`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hvdc_coefficients: Vec<(usize, f64)>,
}

impl Flowgate {
    /// Forward MW limit at timestep `t`.
    ///
    /// Returns `limit_mw_schedule[t]` when available, else `limit_mw`.
    pub fn effective_limit_mw(&self, t: usize) -> f64 {
        self.limit_mw_schedule
            .get(t)
            .copied()
            .unwrap_or(self.limit_mw)
    }

    /// Reverse MW limit at timestep `t`.
    ///
    /// Returns `limit_reverse_mw_schedule[t]` when available, else
    /// `limit_reverse_mw`.  When the result is zero (or negative), callers
    /// should fall back to the forward limit for symmetric enforcement.
    pub fn effective_limit_reverse_mw(&self, t: usize) -> f64 {
        self.limit_reverse_mw_schedule
            .get(t)
            .copied()
            .unwrap_or(self.limit_reverse_mw)
    }

    /// Effective reverse limit, falling back to forward limit when zero.
    ///
    /// This is the convenience method for constraint generation: returns
    /// the reverse limit if explicitly set (> 0), otherwise the forward limit
    /// for symmetric enforcement.
    pub fn effective_reverse_or_forward(&self, t: usize) -> f64 {
        let rev = self.effective_limit_reverse_mw(t);
        if rev > 0.0 {
            rev
        } else {
            self.effective_limit_mw(t)
        }
    }
}

/// A piecewise-linear operating nomogram: restricts one flowgate's MW limit
/// based on the real-time MW flow measured on a second "index" flowgate.
///
/// The `points` vector is a sorted list of `(index_flow_mw, constrained_limit_mw)`
/// pairs defining the nomogram curve.  `evaluate(index_flow_mw)` performs
/// piecewise-linear interpolation with flat extrapolation at the endpoints.
///
/// # Example
/// ```
/// use surge_network::network::OperatingNomogram;
/// let nom = OperatingNomogram {
///     name: "NomA".into(),
///     index_flowgate: "FG_North".into(),
///     constrained_flowgate: "FG_South".into(),
///     points: vec![(-500.0, 1000.0), (0.0, 800.0), (500.0, 500.0)],
///     in_service: true,
/// };
/// assert!((nom.evaluate(250.0) - 650.0).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatingNomogram {
    /// Human-readable name.
    pub name: String,
    /// Name of the flowgate whose flow is used as the x-axis input.
    pub index_flowgate: String,
    /// Name of the flowgate whose MW limit is tightened by this nomogram.
    pub constrained_flowgate: String,
    /// Sorted `(index_flow_mw, constrained_limit_mw)` breakpoints.
    ///
    /// Must have at least one point.  Need not cover the full operating range;
    /// out-of-range inputs are clamped to the nearest endpoint (flat extrapolation).
    pub points: Vec<(f64, f64)>,
    /// Whether this nomogram is actively enforced.
    pub in_service: bool,
}

impl OperatingNomogram {
    /// Evaluate the nomogram: return the constrained flowgate's MW limit
    /// given `index_flow_mw` on the index flowgate.
    ///
    /// Uses piecewise-linear interpolation between breakpoints, with flat
    /// extrapolation outside the defined range.  Returns `f64::INFINITY` if
    /// `points` is empty (no constraint).
    pub fn evaluate(&self, index_flow_mw: f64) -> f64 {
        if self.points.is_empty() {
            return f64::INFINITY;
        }
        // Flat extrapolation at left endpoint.
        if index_flow_mw <= self.points[0].0 {
            return self.points[0].1;
        }
        // Flat extrapolation at right endpoint.
        let last = self.points[self.points.len() - 1];
        if index_flow_mw >= last.0 {
            return last.1;
        }
        // Linear interpolation between adjacent breakpoints.
        for w in self.points.windows(2) {
            let (x0, y0) = w[0];
            let (x1, y1) = w[1];
            if index_flow_mw < x1 {
                let t = (index_flow_mw - x0) / (x1 - x0);
                return y0 + t * (y1 - y0);
            }
        }
        last.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nomogram_evaluate() {
        let nom = OperatingNomogram {
            name: "N1".into(),
            index_flowgate: "FG_A".into(),
            constrained_flowgate: "FG_B".into(),
            points: vec![(-500.0, 1000.0), (0.0, 800.0), (500.0, 500.0)],
            in_service: true,
        };
        // Left flat extrapolation.
        assert!((nom.evaluate(-600.0) - 1000.0).abs() < 1e-9);
        // Exact breakpoint.
        assert!((nom.evaluate(0.0) - 800.0).abs() < 1e-9);
        // Midpoint interpolation.
        assert!((nom.evaluate(250.0) - 650.0).abs() < 1e-9);
        // Right flat extrapolation.
        assert!((nom.evaluate(600.0) - 500.0).abs() < 1e-9);
    }

    #[test]
    fn test_effective_limit_mw_schedule() {
        let fg = Flowgate {
            name: "FG".into(),
            monitored: vec![],
            contingency_branch: None,
            limit_mw: 100.0,
            limit_reverse_mw: 0.0,
            in_service: true,
            limit_mw_schedule: vec![90.0, 80.0, 70.0],
            limit_reverse_mw_schedule: vec![],
            hvdc_coefficients: vec![],
        };
        assert_eq!(fg.effective_limit_mw(0), 90.0);
        assert_eq!(fg.effective_limit_mw(2), 70.0);
        // Beyond schedule: fall back to limit_mw.
        assert_eq!(fg.effective_limit_mw(5), 100.0);
        // Reverse limit: 0 → falls back to forward.
        assert_eq!(fg.effective_reverse_or_forward(0), 90.0);
    }
}
