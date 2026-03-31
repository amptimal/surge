#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Unified SCOPF types — options, results, errors, and supporting structures.

use std::sync::Arc;

use crate::ac::types::AcOpfOptions;
use crate::backends::{LpSolver, NlpSolver};
use crate::dc::opf::DcOpfOptions;
use serde::{Deserialize, Serialize};
use surge_network::market::PenaltyConfig;
use surge_network::network::Contingency;

// =============================================================================
// Thermal rating selection
// =============================================================================

/// Which thermal rating to use for post-contingency limits.
///
/// RTOs typically enforce `RateA` for base-case and `RateB` (short-term emergency)
/// for post-contingency constraints. When a branch has `rate_b = 0`, falls back to `rate_a`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThermalRating {
    /// Long-term rating (`branch.rating_a_mva`). Default.
    #[default]
    RateA,
    /// Short-term emergency rating (`branch.rating_b_mva`). Falls back to `rate_a` if zero.
    RateB,
    /// Extreme emergency rating (`branch.rating_c_mva`). Falls back to `rate_a` if zero.
    RateC,
}

impl ThermalRating {
    /// Return the selected rating for a branch, falling back to `rate_a` when the
    /// chosen rating is zero or negative.
    pub fn of(self, branch: &surge_network::network::Branch) -> f64 {
        match self {
            Self::RateA => branch.rating_a_mva,
            Self::RateB => {
                if branch.rating_b_mva > 0.0 {
                    branch.rating_b_mva
                } else {
                    branch.rating_a_mva
                }
            }
            Self::RateC => {
                if branch.rating_c_mva > 0.0 {
                    branch.rating_c_mva
                } else {
                    branch.rating_a_mva
                }
            }
        }
    }
}

// =============================================================================
// Formulation & mode enums
// =============================================================================

/// Power flow formulation for SCOPF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScopfFormulation {
    /// DC B-theta LP (HiGHS/Gurobi). Fast, linear, no voltage/reactive.
    #[default]
    Dc,
    /// Full AC NLP with Benders decomposition. Handles voltage and reactive power.
    Ac,
}

/// Security enforcement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScopfMode {
    /// Base-case dispatch must satisfy all N-1 constraints. No post-contingency redispatch.
    #[default]
    Preventive,
    /// Post-contingency corrective redispatch allowed, limited by ramp rates.
    Corrective,
}

// =============================================================================
// Options
// =============================================================================

/// Unified SCOPF solver options.
///
/// A single entry point for all SCOPF variants. Select the problem via
/// [`formulation`](ScopfOptions::formulation) (DC or AC) and
/// [`mode`](ScopfOptions::mode) (Preventive or Corrective).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScopfOptions {
    // ── Problem selection ─────────────────────────────────────────────────
    /// DC (default) or AC formulation.
    pub formulation: ScopfFormulation,
    /// Preventive (default) or Corrective mode.
    pub mode: ScopfMode,

    // ── Shared iterative settings ─────────────────────────────────────────
    /// Maximum constraint-generation iterations (cutting-plane or Benders cycles).
    pub max_iterations: u32,
    /// Post-contingency flow violation threshold in per-unit.
    /// Default: 0.01 (= 1 MW at 100 MVA base).
    pub violation_tolerance_pu: f64,
    /// Maximum violated constraints to add per iteration.
    pub max_cuts_per_iteration: usize,

    // ── Contingency specification ─────────────────────────────────────────
    /// Custom contingency list. `None` = auto-generate N-1 branch contingencies.
    pub contingencies: Option<Vec<Contingency>>,
    /// Maximum contingencies to evaluate (0 = all). Useful for AC-SCOPF.
    pub max_contingencies: usize,
    /// Minimum branch `rate_a` (MVA) to treat as thermally limited.
    pub min_rate_a: f64,
    /// Thermal rating used for post-contingency limits (default: `RateA`).
    /// RTO practice: base case uses `rate_a`, contingencies use `rate_b` (short-term emergency).
    pub contingency_rating: ThermalRating,
    /// Enforce flowgate and interface constraints.
    ///
    /// SCOPF currently rejects these corridor constraints entirely instead of
    /// silently approximating them as single-branch cuts.
    pub enforce_flowgates: bool,
    /// Enforce branch angle-difference limits as soft constraints.
    /// Default: `true`.  Set to `false` to skip angle constraints entirely,
    /// which is appropriate when the network data carries placeholder ±π
    /// limits that add rows without tightening the feasible region.
    pub enforce_angle_limits: bool,

    // ── Pre-screening ─────────────────────────────────────────────────────
    /// N-1 contingency pre-screener configuration.
    /// `Some` = pre-populate initial constraint set with likely-binding pairs.
    /// `None` = pure reactive cutting-plane (no pre-screening).
    /// Default: `Some(ScopfScreeningPolicy::default())`.
    /// Only used for DC formulation.
    pub screener: Option<ScopfScreeningPolicy>,

    // ── Penalty / soft constraints ────────────────────────────────────────
    /// Penalty configuration for soft-constraint violations.
    pub penalty_config: PenaltyConfig,

    // ── DC-specific ───────────────────────────────────────────────────────
    /// Base DC-OPF solver options. Ignored for AC formulation.
    pub dc_opf: DcOpfOptions,

    // ── AC-specific ───────────────────────────────────────────────────────
    /// AC-OPF base options and AC-SCOPF subproblem settings.
    /// Ignored for DC formulation.
    pub ac: ScopfAcSettings,

    // ── Corrective-specific ───────────────────────────────────────────────
    /// Corrective-mode settings (ramp windows, etc.).
    /// Ignored for Preventive mode.
    pub corrective: ScopfCorrectiveSettings,
}

/// AC-SCOPF–specific settings: base AC-OPF options plus contingency
/// subproblem parameters (NR iterations, voltage thresholds).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScopfAcSettings {
    /// Base AC-OPF solver options.
    pub opf: AcOpfOptions,
    /// Post-contingency voltage violation threshold in per-unit.
    pub voltage_threshold: f64,
    /// Maximum NR iterations for contingency subproblems.
    pub nr_max_iterations: u32,
    /// NR convergence tolerance for contingency subproblems.
    pub nr_convergence_tolerance: f64,
    /// Enforce post-contingency voltage limits in AC-SCOPF. Default: `true`.
    /// When true, voltage violations generate Benders cuts via ∂Vm/∂Pg sensitivity.
    /// When false, voltage violations are recorded but do not generate cuts;
    /// convergence is reported as false if violations remain.
    pub enforce_voltage_security: bool,
}

impl Default for ScopfAcSettings {
    fn default() -> Self {
        Self {
            opf: AcOpfOptions::default(),
            voltage_threshold: 0.01,
            nr_max_iterations: 30,
            nr_convergence_tolerance: 1e-6,
            enforce_voltage_security: true,
        }
    }
}

/// Corrective-mode SCOPF settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScopfCorrectiveSettings {
    /// Time window for corrective actions in minutes.
    ///
    /// Maximum corrective redispatch for generator g:
    ///   `ΔPg_max = ramp_rate(g) [MW/min] × ramp_window_min [min]`
    ///
    /// Ramp rate priority: `ramp_agc > ramp_up > capacity bounds`.
    /// Default: 10.0 (10-minute corrective action window, standard RTO N-1 criterion).
    pub ramp_window_min: f64,
}

impl Default for ScopfCorrectiveSettings {
    fn default() -> Self {
        Self {
            ramp_window_min: 10.0,
        }
    }
}

impl Default for ScopfOptions {
    fn default() -> Self {
        Self {
            formulation: ScopfFormulation::Dc,
            mode: ScopfMode::Preventive,
            max_iterations: 20,
            violation_tolerance_pu: 0.01,
            max_cuts_per_iteration: 100,
            contingencies: None,
            max_contingencies: 0,
            min_rate_a: 1.0,
            contingency_rating: ThermalRating::RateA,
            enforce_flowgates: true,
            enforce_angle_limits: true,
            screener: Some(ScopfScreeningPolicy::default()),
            penalty_config: PenaltyConfig::default(),
            dc_opf: DcOpfOptions::default(),
            ac: ScopfAcSettings::default(),
            corrective: ScopfCorrectiveSettings::default(),
        }
    }
}

/// Runtime execution controls for SCOPF.
#[derive(Debug, Clone, Default)]
pub struct ScopfRuntime {
    /// LP solver backend for DC-based SCOPF solves.
    pub lp_solver: Option<Arc<dyn LpSolver>>,
    /// NLP solver backend for AC-based SCOPF solves.
    pub nlp_solver: Option<Arc<dyn NlpSolver>>,
    /// Warm-start data from a prior SCOPF solve.
    pub warm_start: Option<ScopfWarmStart>,
}

impl ScopfRuntime {
    /// Set the LP solver backend for DC-SCOPF (builder pattern).
    pub fn with_lp_solver(mut self, solver: Arc<dyn LpSolver>) -> Self {
        self.lp_solver = Some(solver);
        self
    }

    /// Set the NLP solver backend for AC-SCOPF (builder pattern).
    pub fn with_nlp_solver(mut self, solver: Arc<dyn NlpSolver>) -> Self {
        self.nlp_solver = Some(solver);
        self
    }

    /// Set warm-start data from a prior SCOPF solve (builder pattern).
    pub fn with_warm_start(mut self, warm_start: ScopfWarmStart) -> Self {
        self.warm_start = Some(warm_start);
        self
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ScopfRunContext {
    pub runtime: ScopfRuntime,
}

// =============================================================================
// Result
// =============================================================================

/// Unified SCOPF result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopfResult {
    /// Base-case OPF solution (dispatch, LMPs).
    pub base_opf: surge_solution::OpfSolution,
    /// Formulation used.
    pub formulation: ScopfFormulation,
    /// Mode used.
    pub mode: ScopfMode,
    /// Constraint-generation iterations performed.
    pub iterations: u32,
    /// Whether the solver converged (no remaining violations above threshold).
    pub converged: bool,
    /// Total contingencies evaluated.
    pub total_contingencies_evaluated: usize,
    /// Total contingency constraints added to the problem.
    pub total_contingency_constraints: usize,
    /// Binding contingencies with shadow prices.
    pub binding_contingencies: Vec<BindingContingency>,
    /// Contingency-only LMP congestion component per bus ($/MWh).
    /// Non-empty for DC preventive; empty for other variants.
    pub lmp_contingency_congestion: Vec<f64>,
    /// Post-contingency violations remaining in final iteration.
    /// Non-empty for AC formulation; empty for DC.
    pub remaining_violations: Vec<ContingencyViolation>,
    /// Contingencies whose post-outage evaluation failed before security could
    /// be classified.
    pub failed_contingencies: Vec<FailedContingencyEvaluation>,
    /// Pre-screening statistics (empty/default if screener disabled).
    pub screening_stats: ScopfScreeningStats,
    /// Wall-clock solve time in seconds.
    pub solve_time_secs: f64,
}

// =============================================================================
// Error
// =============================================================================

crate::common::opf_common_errors!(ScopfError {
    /// Total generation capacity is less than total load.
    #[error("insufficient generation capacity: need {load_mw:.1} MW, max {capacity_mw:.1} MW")]
    InsufficientCapacity { load_mw: f64, capacity_mw: f64 },

    /// The cutting-plane or Benders loop did not converge within the iteration limit.
    #[error("did not converge in {iterations} iterations")]
    NotConverged { iterations: u32 },

    /// The solver returned a feasible but provably suboptimal solution.
    #[error("solver returned suboptimal solution")]
    SubOptimalSolution,

    /// The underlying OPF subproblem is infeasible.
    #[error("solver reported infeasible problem")]
    InfeasibleProblem,

    /// The underlying OPF subproblem is unbounded.
    #[error("solver reported unbounded problem")]
    UnboundedProblem,

    /// A configured HVDC link references a bus that is not present in the case.
    #[error(
        "invalid HVDC link {index} ({from_bus} -> {to_bus}): {reason}"
    )]
    InvalidHvdcLink {
        index: usize,
        from_bus: u32,
        to_bus: u32,
        reason: String,
    },

    /// The requested formulation/mode combination is not implemented.
    #[error("{formulation:?} + {mode:?} is not supported")]
    UnsupportedCombination {
        /// The OPF formulation (DC or AC).
        formulation: ScopfFormulation,
        /// The security mode (Preventive or Corrective).
        mode: ScopfMode,
    },

    /// The requested security-constraint class is not yet modeled faithfully.
    #[error("unsupported SCOPF security constraint: {detail}")]
    UnsupportedSecurityConstraint { detail: String },
});

use crate::dc::opf::DcOpfError;

crate::common::impl_opf_error_from!(DcOpfError => ScopfError {
    DcOpfError::InsufficientCapacity { load_mw, capacity_mw }
        => ScopfError::InsufficientCapacity { load_mw, capacity_mw },
    DcOpfError::NotConverged { iterations }
        => ScopfError::NotConverged { iterations },
    DcOpfError::SubOptimalSolution => ScopfError::SubOptimalSolution,
    DcOpfError::InfeasibleProblem => ScopfError::InfeasibleProblem,
    DcOpfError::UnboundedProblem => ScopfError::UnboundedProblem,
    DcOpfError::InvalidHvdcLink { index, from_bus, to_bus, reason }
        => ScopfError::InvalidHvdcLink {
            index,
            from_bus,
            to_bus,
            reason,
        },
});

impl From<crate::ac::types::AcOpfError> for ScopfError {
    fn from(e: crate::ac::types::AcOpfError) -> Self {
        ScopfError::SolverError(e.to_string())
    }
}

// =============================================================================
// Supporting types
// =============================================================================

/// A binding contingency constraint in the SCOPF solution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopfCutKind {
    BranchThermal,
    GeneratorTrip,
    MultiBranchN2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingContingency {
    /// Human-readable label for the contingency.
    pub contingency_label: String,
    /// Type of cut that bound this contingency.
    pub cut_kind: ScopfCutKind,
    /// Indices of the outaged branches (into `Network::branches`).
    /// Single element for N-1, two elements for N-2, empty for gen-only contingencies.
    pub outaged_branch_indices: Vec<usize>,
    /// Indices of the outaged generators (into `Network::generators`).
    pub outaged_generator_indices: Vec<usize>,
    /// Index of the monitored branch with the binding constraint.
    pub monitored_branch_idx: usize,
    /// Post-contingency loading percentage.
    pub loading_pct: f64,
    /// Shadow price (dual value) of this constraint ($/MWh).
    pub shadow_price: f64,
}

/// Stable identity for a previously binding SCOPF cut.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopfWarmStartCut {
    pub cut_kind: ScopfCutKind,
    pub outaged_branch_indices: Vec<usize>,
    pub outaged_generator_indices: Vec<usize>,
    pub monitored_branch_idx: usize,
}

/// A post-contingency violation record (used by AC-SCOPF).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContingencyViolation {
    /// Contingency that caused this violation.
    pub contingency_id: String,
    /// Human-readable label.
    pub contingency_label: String,
    /// Branch indices that were outaged.
    pub outaged_branches: Vec<usize>,
    /// Generator indices that were tripped.
    pub outaged_generators: Vec<usize>,
    /// Thermal violations: (branch_index, flow_mva, rating_mva, overload_fraction).
    pub thermal_violations: Vec<(usize, f64, f64, f64)>,
    /// Voltage violations: (bus_index, vm_pu, vm_min, vm_max).
    pub voltage_violations: Vec<(usize, f64, f64, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedContingencyEvaluation {
    pub contingency_id: String,
    pub contingency_label: String,
    pub outaged_branches: Vec<usize>,
    pub outaged_generators: Vec<usize>,
    pub reason: String,
}

/// N-1 contingency pre-screener for SCOPF.
///
/// Before the cutting-plane loop begins, the screener uses pre-contingency LODF
/// to estimate post-contingency branch loadings. Any (monitored, outaged) pair
/// whose estimated loading exceeds `threshold_fraction × rating` is inserted into
/// the initial constraint set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopfScreeningPolicy {
    /// Fraction of thermal rating used as the screening threshold.
    /// Default: 0.9 (90% of rating).
    pub threshold_fraction: f64,
    /// Maximum number of (monitored, contingency) pairs to include in the
    /// initial constraint set. Default: 500.
    pub max_initial_contingencies: usize,
}

impl Default for ScopfScreeningPolicy {
    fn default() -> Self {
        Self {
            threshold_fraction: 0.9,
            max_initial_contingencies: 500,
        }
    }
}

/// Statistics reported by the N-1 contingency pre-screener.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScopfScreeningStats {
    /// Number of (monitored, contingency) pairs evaluated during pre-screening.
    pub pairs_evaluated: usize,
    /// Number of pairs that exceeded the screening threshold.
    pub pre_screened_constraints: usize,
    /// Number of additional constraints added by the cutting-plane loop.
    pub cutting_plane_constraints: usize,
    /// Screening threshold that was used (fraction of thermal rating).
    pub threshold_fraction: f64,
}

/// Warm-start data for RT-SCOPF.
///
/// Allows subsequent SCOPF calls to re-use the base dispatch and pre-loaded
/// contingency cuts from a prior solve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopfWarmStart {
    /// Base generator dispatch (MW) from a prior SCOPF solution.
    pub base_pg: Vec<f64>,
    /// Bus voltage magnitudes (per-unit) from a prior SCOPF solution.
    pub base_vm: Vec<f64>,
    /// Stable descriptors for prior binding cuts that should be pre-loaded in
    /// the new solve.
    pub active_cuts: Vec<ScopfWarmStartCut>,
}
