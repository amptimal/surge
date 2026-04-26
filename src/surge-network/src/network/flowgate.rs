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
#[serde(try_from = "InterfaceSerde")]
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

#[derive(Debug, Deserialize)]
struct InterfaceSerde {
    pub name: String,
    #[serde(default)]
    pub members: Vec<WeightedBranchRef>,
    #[serde(default)]
    pub branches: Vec<(u32, u32, String)>,
    #[serde(default)]
    pub coefficients: Vec<f64>,
    pub limit_forward_mw: f64,
    pub limit_reverse_mw: f64,
    pub in_service: bool,
    #[serde(default)]
    pub limit_forward_mw_schedule: Vec<f64>,
    #[serde(default)]
    pub limit_reverse_mw_schedule: Vec<f64>,
}

impl TryFrom<InterfaceSerde> for Interface {
    type Error = String;

    fn try_from(value: InterfaceSerde) -> Result<Self, Self::Error> {
        let members = if !value.members.is_empty() {
            value.members
        } else if value.branches.is_empty() && value.coefficients.is_empty() {
            Vec::new()
        } else {
            if value.branches.len() != value.coefficients.len() {
                return Err(format!(
                    "interface '{}' has {} legacy branches but {} coefficients",
                    value.name,
                    value.branches.len(),
                    value.coefficients.len()
                ));
            }
            value
                .branches
                .into_iter()
                .zip(value.coefficients)
                .map(|(branch, coefficient)| WeightedBranchRef {
                    branch: branch.into(),
                    coefficient,
                })
                .collect()
        };

        Ok(Self {
            name: value.name,
            members,
            limit_forward_mw: value.limit_forward_mw,
            limit_reverse_mw: value.limit_reverse_mw,
            in_service: value.in_service,
            limit_forward_mw_schedule: value.limit_forward_mw_schedule,
            limit_reverse_mw_schedule: value.limit_reverse_mw_schedule,
        })
    }
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

/// Sentinel forward-limit value returned by [`Flowgate::effective_limit_mw`]
/// on periods where a single-period flowgate is inactive. Downstream LP
/// builders translate this into a row whose bounds are so wide the
/// constraint is trivially satisfied (Gurobi's GRB_INFINITY convention).
pub const INACTIVE_FLOWGATE_LIMIT_MW: f64 = 1e30;

/// A flowgate: a monitored element under a specific contingency.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "FlowgateSerde")]
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
    /// Per-bus effective PTDF coefficients for the monitored aggregate.
    ///
    /// When non-empty, the LP row builder switches from the theta-form
    /// constraint `Σ coeff·b_dc·Δθ ≤ limit` to the PTDF/injection form
    /// `Σ_i ptdf_eff_i · p_net_inj_i ≤ limit`. The latter directly
    /// constrains generator/load/storage/HVDC variables and is the
    /// only form that binds dispatch when the SCUC LP runs in
    /// `scuc_disable_bus_power_balance` mode (where theta is decoupled
    /// from `pg`). Each entry is `(bus_number, eff_ptdf_pu)` where
    /// `eff_ptdf_pu = Σ_term coefficient_term · ptdf_l_term[bus_idx]`,
    /// summed over the flowgate's `monitored` terms. Only buses with
    /// non-trivial PTDF contribution are stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ptdf_per_bus: Vec<(u32, f64)>,
    /// Per-band HVDC coefficients for banded N-1 HVDC contingency constraints.
    /// Each entry: `(hvdc_link_index, band_index, coefficient_pu)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hvdc_band_coefficients: Vec<(usize, usize, f64)>,
    /// Compact single-active-period marker. When `Some(p)`,
    /// [`Flowgate::effective_limit_mw`] returns `limit_mw` at timestep
    /// `p` and the [`INACTIVE_FLOWGATE_LIMIT_MW`] sentinel for all
    /// other timesteps — producing the same LP behaviour as a 18-slot
    /// `limit_mw_schedule` with 17 sentinel entries but without the
    /// per-flowgate `Vec<f64>` allocation (~1.2 GB savings on
    /// 617-bus explicit N-1 SCUC, where this is populated by
    /// `build_branch_security_flowgate`). When `None`, the legacy
    /// `limit_mw_schedule` / `limit_mw` lookup is used unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_mw_active_period: Option<u32>,
    /// Which side(s) of the flowgate limit can bind. Defaults to
    /// [`FlowgateBreachSides::Both`] for user-supplied or preseeded
    /// cuts where the screener doesn't know the direction. The
    /// iterative security-SCUC screener emits
    /// [`FlowgateBreachSides::Upper`] or [`FlowgateBreachSides::Lower`]
    /// after observing which side of `±limit_mw` the monitored-branch
    /// flow actually crossed, so the bounds layer can pin the
    /// non-breached side's slack column to zero and let the surge
    /// lp-reduce presolve drop it. Saves a factor-of-two on slack
    /// columns per (flowgate, period) pair when set.
    #[serde(default, skip_serializing_if = "FlowgateBreachSides::is_both")]
    pub breach_sides: FlowgateBreachSides,
}

/// Which side(s) of a [`Flowgate`]'s limit the LP allocates slack
/// columns for. `Both` (default) keeps the symmetric encoding; `Upper`
/// and `Lower` restrict slack allocation to the matching side,
/// collapsing the other side's slack column to zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowgateBreachSides {
    /// The symmetric default: slack on both sides of the limit band.
    #[default]
    Both,
    /// Only the upper side (`monitored_flow ≤ +limit`) can bind. The
    /// lower-side slack column is pinned to zero.
    Upper,
    /// Only the lower side (`-limit ≤ monitored_flow`) can bind. The
    /// upper-side slack column is pinned to zero.
    Lower,
}

impl FlowgateBreachSides {
    /// Helper for `#[serde(skip_serializing_if)]`: treats the default
    /// `Both` variant as elidable in JSON to preserve on-disk format
    /// compatibility for all flowgates where no direction is known.
    pub fn is_both(&self) -> bool {
        matches!(self, FlowgateBreachSides::Both)
    }

    /// Whether an upper-side slack column should be allocated for
    /// this flowgate.
    pub fn allocates_upper_slack(&self) -> bool {
        matches!(self, FlowgateBreachSides::Both | FlowgateBreachSides::Upper)
    }

    /// Whether a lower-side slack column should be allocated for
    /// this flowgate.
    pub fn allocates_lower_slack(&self) -> bool {
        matches!(self, FlowgateBreachSides::Both | FlowgateBreachSides::Lower)
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BranchRefSerde {
    Structured(BranchRef),
    Legacy((u32, u32, String)),
}

impl From<BranchRefSerde> for BranchRef {
    fn from(value: BranchRefSerde) -> Self {
        match value {
            BranchRefSerde::Structured(branch) => branch,
            BranchRefSerde::Legacy(branch) => branch.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct FlowgateSerde {
    pub name: String,
    #[serde(default)]
    pub monitored: Vec<WeightedBranchRef>,
    #[serde(default)]
    pub monitored_branches: Vec<(u32, u32, String)>,
    #[serde(default)]
    pub monitored_coefficients: Vec<f64>,
    pub contingency_branch: Option<BranchRefSerde>,
    pub limit_mw: f64,
    #[serde(default)]
    pub limit_reverse_mw: f64,
    pub in_service: bool,
    #[serde(default)]
    pub limit_mw_schedule: Vec<f64>,
    #[serde(default)]
    pub limit_reverse_mw_schedule: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hvdc_coefficients: Vec<(usize, f64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hvdc_band_coefficients: Vec<(usize, usize, f64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ptdf_per_bus: Vec<(u32, f64)>,
    #[serde(default)]
    pub limit_mw_active_period: Option<u32>,
    #[serde(default)]
    pub breach_sides: FlowgateBreachSides,
}

impl TryFrom<FlowgateSerde> for Flowgate {
    type Error = String;

    fn try_from(value: FlowgateSerde) -> Result<Self, Self::Error> {
        let monitored = if !value.monitored.is_empty() {
            value.monitored
        } else if value.monitored_branches.is_empty() && value.monitored_coefficients.is_empty() {
            Vec::new()
        } else {
            if value.monitored_branches.len() != value.monitored_coefficients.len() {
                return Err(format!(
                    "flowgate '{}' has {} legacy monitored branches but {} coefficients",
                    value.name,
                    value.monitored_branches.len(),
                    value.monitored_coefficients.len()
                ));
            }
            value
                .monitored_branches
                .into_iter()
                .zip(value.monitored_coefficients)
                .map(|(branch, coefficient)| WeightedBranchRef {
                    branch: branch.into(),
                    coefficient,
                })
                .collect()
        };

        Ok(Self {
            name: value.name,
            monitored,
            contingency_branch: value.contingency_branch.map(Into::into),
            limit_mw: value.limit_mw,
            limit_reverse_mw: value.limit_reverse_mw,
            in_service: value.in_service,
            limit_mw_schedule: value.limit_mw_schedule,
            limit_reverse_mw_schedule: value.limit_reverse_mw_schedule,
            hvdc_coefficients: value.hvdc_coefficients,
            hvdc_band_coefficients: value.hvdc_band_coefficients,
            ptdf_per_bus: value.ptdf_per_bus,
            limit_mw_active_period: value.limit_mw_active_period,
            breach_sides: value.breach_sides,
        })
    }
}

impl Flowgate {
    /// Forward MW limit at timestep `t`.
    ///
    /// Resolution order:
    /// 1. If `limit_mw_active_period` is `Some(p)`: return `limit_mw` at
    ///    `t == p`, and [`INACTIVE_FLOWGATE_LIMIT_MW`] otherwise. This is
    ///    the compact encoding used by explicit N-1 security flowgates
    ///    (one period active, all others disabled). Avoids allocating an
    ///    `n_periods`-length `Vec<f64>` per flowgate.
    /// 2. Else if `limit_mw_schedule[t]` exists: return it.
    /// 3. Else: fall back to `limit_mw`.
    pub fn effective_limit_mw(&self, t: usize) -> f64 {
        if let Some(active) = self.limit_mw_active_period {
            return if t == active as usize {
                self.limit_mw
            } else {
                INACTIVE_FLOWGATE_LIMIT_MW
            };
        }
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
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: vec![],
            limit_mw_active_period: None,
            breach_sides: FlowgateBreachSides::Both,
        };
        assert_eq!(fg.effective_limit_mw(0), 90.0);
        assert_eq!(fg.effective_limit_mw(2), 70.0);
        // Beyond schedule: fall back to limit_mw.
        assert_eq!(fg.effective_limit_mw(5), 100.0);
        // Reverse limit: 0 → falls back to forward.
        assert_eq!(fg.effective_reverse_or_forward(0), 90.0);
    }
}
