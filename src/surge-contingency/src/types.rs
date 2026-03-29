// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared types for contingency analysis: options, results, violations.

use crate::scrd::ScrdSolution;
use serde::{Deserialize, Serialize};
use surge_ac::AcPfOptions;
use surge_network::market::PenaltyConfig;
use surge_network::network::{Branch, TplCategory};
use surge_solution::PfSolution;

// ---------------------------------------------------------------------------
// Thermal rating
// ---------------------------------------------------------------------------

/// Thermal rating tier for violation detection.
///
/// Determines which branch thermal rating is used for overload checks:
/// - `RateA` (default): Long-term continuous rating (`branch.rating_a_mva`).
/// - `RateB`: Short-term emergency rating (`branch.rating_b_mva`), falling back to `rate_a` if zero.
/// - `RateC`: Ultimate emergency rating (`branch.rating_c_mva`), falling back to `rate_a` if zero.
///
/// NERC TPL-001 allows emergency ratings (Rate B or C) for post-contingency
/// thermal checks, whereas normal operating conditions use Rate A.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum ThermalRating {
    #[default]
    RateA,
    RateB,
    RateC,
}

/// Return the thermal rating for a branch given the selected tier.
///
/// Falls back to `rate_a` when the selected tier's value is zero (unset).
pub fn get_rating(branch: &Branch, rating: ThermalRating) -> f64 {
    match rating {
        ThermalRating::RateA => branch.rating_a_mva,
        ThermalRating::RateB => {
            if branch.rating_b_mva > 0.0 {
                branch.rating_b_mva
            } else {
                branch.rating_a_mva
            }
        }
        ThermalRating::RateC => {
            if branch.rating_c_mva > 0.0 {
                branch.rating_c_mva
            } else {
                branch.rating_a_mva
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Screening & voltage stability enums
// ---------------------------------------------------------------------------

/// Screening strategy for filtering contingencies before AC solve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreeningMode {
    /// No screening — solve all contingencies with AC.
    Off,
    /// Use LODF-based DC screening to filter non-critical contingencies.
    ///
    /// When [`ContingencyOptions::voltage_pre_screen`] is `true` (default),
    /// also runs a parallel FDPF voltage screening pass on contingencies
    /// not flagged by thermal LODF screening.
    Lodf,
    /// Two-pass: FDPF screening (fast, approximate) → NR only for violations.
    ///
    /// FDPF solves each contingency with ~3 half-iterations from warm start.
    /// Contingencies with no violations are accepted with FDPF result.
    /// Only contingencies with violations or non-convergence get full NR.
    Fdpf,
}

/// Voltage stress post-processing mode for converged contingencies.
///
/// | Mode | Cost | What it does |
/// |------|------|-------------|
/// | `Off` | Zero | No voltage stress computation |
/// | `Proxy` | Cheap | Local Q-V stress proxy per PQ bus |
/// | `ExactLIndex` | ~1 KLU factor/ctg | Full Kessel-Glavitsch L-index + [`VsmCategory`] classification |
///
/// When `ExactLIndex` is selected, the engine computes the full network-coupled
/// L-index via the internal sparse `F_LG` solve for every converged contingency,
/// then assigns a [`VsmCategory`] based on `l_index_threshold`.
///
/// # Example
///
/// ```ignore
/// let options = ContingencyOptions {
///     voltage_stress_mode: VoltageStressMode::ExactLIndex {
///         l_index_threshold: 0.7,
///     },
///     ..Default::default()
/// };
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum VoltageStressMode {
    /// Do not compute voltage-stress post-processing metrics.
    Off,
    /// Compute the cheap local Q-V stress proxy (default).
    ///
    /// For each PQ bus: `L_j = |Q_j| / (|V_j|^2 * |B_jj|)`.
    /// This is a screening-quality metric, not the full network-coupled L-index.
    #[default]
    Proxy,
    /// Compute the full Kessel-Glavitsch exact L-index and assign [`VsmCategory`].
    ///
    /// Cost: ~1 KLU factorization + n_gen back-substitutions per contingency.
    ExactLIndex {
        /// L-index value above which a contingency is classified as
        /// [`VsmCategory::Critical`]. Default: 0.7.
        #[serde(default = "default_l_threshold")]
        l_index_threshold: f64,
    },
}

fn default_l_threshold() -> f64 {
    0.7
}

/// Configuration for base-case voltage-stress evaluation.
///
/// This is the canonical base-case counterpart to contingency
/// `voltage_stress_mode`: run an AC power flow, then compute the requested
/// voltage-stability metrics from the solved operating point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoltageStressOptions {
    /// Newton-Raphson options for the base-case AC solve.
    pub acpf_options: AcPfOptions,
    /// Post-solve voltage-stress metric configuration.
    pub mode: VoltageStressMode,
}

impl Default for VoltageStressOptions {
    fn default() -> Self {
        Self {
            acpf_options: AcPfOptions::default(),
            mode: VoltageStressMode::ExactLIndex {
                l_index_threshold: default_l_threshold(),
            },
        }
    }
}

/// Classification of voltage stability risk for a contingency.
///
/// Assigned by [`VoltageStressMode::ExactLIndex`] based on L-index value.
///
/// # Thresholds
///
/// | Category | L-index condition |
/// |----------|-------------------|
/// | `Secure` | L-index < 0.5 |
/// | `Marginal` | 0.5 ≤ L-index < 0.7 |
/// | `Critical` | 0.7 ≤ L-index < 0.9 |
/// | `Unstable` | L-index ≥ 0.9 |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VsmCategory {
    /// L-index < 0.5 — adequate voltage stability margin.
    Secure,
    /// 0.5 ≤ L-index < 0.7 — reduced margin, monitor.
    Marginal,
    /// 0.7 ≤ L-index < 0.9 — near collapse, action needed.
    Critical,
    /// L-index ≥ 0.9 — at voltage collapse boundary.
    Unstable,
}

/// Voltage stress assessment for a single contingency.
///
/// Populated when [`ContingencyOptions::voltage_stress_mode`] is not [`VoltageStressMode::Off`].
/// Contains per-bus metrics and system-level summaries.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VoltageStressResult {
    /// Per-bus voltage stress metrics.
    pub per_bus: Vec<BusVoltageStress>,
    /// Maximum local Q-V stress proxy across PQ buses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_qv_stress_proxy: Option<f64>,
    /// PQ bus with the highest local Q-V stress proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critical_proxy_bus: Option<u32>,
    /// Maximum exact Kessel-Glavitsch L-index across PQ buses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_l_index: Option<f64>,
    /// PQ bus with the highest exact Kessel-Glavitsch L-index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critical_l_index_bus: Option<u32>,
    /// Voltage stability classification based on L-index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<VsmCategory>,
}

/// Per-bus voltage stress summary.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BusVoltageStress {
    /// External bus number.
    pub bus_number: u32,
    /// Cheap local Q-V stress proxy `|Q_net| / (|V|^2 |B_kk|)` for PQ buses.
    ///
    /// This is a screening metric, not the full Kessel-Glavitsch network-coupled
    /// L-index. `None` for non-PQ buses or when proxy mode is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_qv_stress_proxy: Option<f64>,
    /// Exact Kessel-Glavitsch L-index from the full `F_LG` formulation.
    ///
    /// Returned only when [`VoltageStressMode::ExactLIndex`] is enabled and the
    /// bus is PQ. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_l_index: Option<f64>,
    /// Voltage magnitude margin to `Vmin` (`V - Vmin`, p.u.). Negative means
    /// the bus violates its minimum-voltage limit.
    pub voltage_margin_to_vmin: f64,
}

// ---------------------------------------------------------------------------
// Progress callback
// ---------------------------------------------------------------------------

/// A thread-safe progress callback for contingency analysis.
///
/// Wraps `Arc<dyn Fn(usize, usize) + Send + Sync>` with manual `Debug`
/// so that `ContingencyOptions` can still derive `Debug`.
pub struct ProgressCallback(pub std::sync::Arc<dyn Fn(usize, usize) + Send + Sync>);

impl std::fmt::Debug for ProgressCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProgressCallback(<fn>)")
    }
}

impl Clone for ProgressCallback {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Configuration for contingency analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContingencyOptions {
    /// Newton-Raphson solver options for each contingency solve.
    pub acpf_options: AcPfOptions,
    /// Screening strategy.
    pub screening: ScreeningMode,
    /// Post-contingency thermal limit as fraction of rating (1.0 = 100%). Default: 1.0.
    pub thermal_threshold_frac: f64,
    /// Minimum acceptable voltage magnitude (p.u.). Default: 0.95.
    pub vm_min: f64,
    /// Maximum acceptable voltage magnitude (p.u.). Default: 1.05.
    pub vm_max: f64,
    /// LODF screening threshold as fraction of thermal rating (0.80 = 80%). Default: 0.80.
    /// Contingencies with max post-outage loading above this are sent to AC.
    pub lodf_screening_threshold: f64,
    /// CTG-10: If `Some(k)`, return only the top-k worst contingencies ranked by
    /// severity (worst thermal overload percentage, then worst voltage deviation).
    /// Contingencies with no violations are always ranked below those with violations.
    /// `None` (default) returns all contingencies.
    pub top_k: Option<usize>,
    /// CTG-02: When `true`, attempt corrective redispatch (SCRD) for every
    /// contingency that has thermal overload violations after N-1 AC solve.
    /// The redispatch result is stored in [`ContingencyResult::corrective_dispatch`].
    /// Default: `false`.
    pub corrective_dispatch: bool,
    /// CTG-09: When `true`, detect electrical islands after applying a contingency
    /// (branch or generator outage) and solve each island independently with its
    /// own slack bus.  When `false`, the solver is called directly (may silently
    /// diverge on island-creating outages).  Default: `true`.
    pub detect_islands: bool,
    /// PNL-005: Penalty config for corrective SCRD post-contingency solve.
    /// When `Some`, corrective redispatch uses these penalty curves instead of
    /// hard thermal constraints.  Passed through to `ScrdOptions::penalty_config`
    /// so the corrective dispatch objective matches the base-case SCOPF penalty
    /// for consistent market clearing signals across security analysis.
    #[serde(default)]
    pub penalty_config: Option<PenaltyConfig>,
    /// Voltage stress post-processing for converged contingencies.
    ///
    /// - `Off`: no voltage stress computation.
    /// - `Proxy` (default): cheap local Q-V stress proxy.
    /// - `ExactLIndex { l_index_threshold }`: full Kessel-Glavitsch L-index
    ///   with [`VsmCategory`] classification.
    #[serde(default)]
    pub voltage_stress_mode: VoltageStressMode,
    /// When true, store post-contingency Vm/Va vectors and branch flows in each
    /// solved [`ContingencyResult`], including FDPF fallback results with
    /// [`ContingencyStatus::Approximate`]. Default: false (saves memory for
    /// production runs). Enable for validation against MATPOWER or other
    /// reference solvers.
    #[serde(default)]
    pub store_post_voltages: bool,
    /// When true, initialize each per-contingency NR solve from a flat start
    /// (Vm = 1.0 p.u., Va = 0.0 rad) instead of the base-case warm start.
    /// Use this to match MATPOWER's default behaviour for apples-to-apples
    /// validation; flat start is slower but independent of base-case voltages.
    /// Default: false (warm start from base-case solution).
    #[serde(default)]
    pub contingency_flat_start: bool,
    /// Maximum FDPF iterations for screening passes. Default: 20.
    pub fdpf_max_iterations: u32,
    /// Thermal rating tier for violation detection.
    ///
    /// NERC TPL-001 allows use of emergency ratings (Rate B or C) for
    /// post-contingency thermal checks.  Default: `RateA` (long-term
    /// continuous rating), preserving backward compatibility.
    #[serde(default)]
    pub thermal_rating: ThermalRating,
    /// Optional progress callback invoked after each contingency is processed.
    ///
    /// Called as `callback(completed, total)` where `completed` is the
    /// number of contingencies finished so far and `total` is the total count.
    /// The callback executes on a rayon worker thread; implementations must
    /// be `Send + Sync`.  Typical use: call `Python::with_gil` to update a
    /// Python progress bar or log to the terminal.
    ///
    /// `None` (default) disables progress reporting.  The callback is skipped
    /// if `#[serde(skip)]` — it is not persisted to JSON.
    #[serde(skip)]
    pub progress_cb: Option<ProgressCallback>,
    /// When `true`, populate `acpf_options.oltc_controls`, `acpf_options.par_controls`,
    /// and `acpf_options.controls.switched_shunts` from the network's `oltc_specs`, `par_specs`,
    /// and `switched_shunts` before each contingency solve (and the base-case solve).
    /// This enables discrete voltage/flow control devices in post-contingency NR.
    /// Default: `false`.
    pub discrete_controls: bool,
    /// When `true`, `analyze_n1_branch` also generates breaker contingencies from the
    /// network's `NodeBreakerTopology` (if present) and appends them to the branch
    /// contingency list.  Each closed breaker produces one contingency that opens
    /// the breaker and rebuild_topologys the network before solving.
    ///
    /// Default: `false` — only branch-trip contingencies are generated.
    #[serde(default)]
    pub include_breaker_contingencies: bool,
    /// When `true` and screening is [`ScreeningMode::Lodf`], run a parallel
    /// FDPF voltage screening pass on contingencies not flagged by thermal
    /// LODF screening. Contingencies with voltage violations are promoted
    /// to the AC solve set.
    ///
    /// Default: `true`.
    #[serde(default = "default_true")]
    pub voltage_pre_screen: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ContingencyOptions {
    fn default() -> Self {
        Self {
            acpf_options: AcPfOptions::default(),
            screening: ScreeningMode::Fdpf,
            thermal_threshold_frac: 1.0,
            vm_min: 0.95,
            vm_max: 1.05,
            lodf_screening_threshold: 0.80,
            top_k: None,
            corrective_dispatch: false,
            detect_islands: true,
            penalty_config: None,
            voltage_stress_mode: VoltageStressMode::default(),
            store_post_voltages: false,
            contingency_flat_start: false,
            fdpf_max_iterations: 20,
            thermal_rating: ThermalRating::default(),
            progress_cb: None,
            discrete_controls: false,
            include_breaker_contingencies: false,
            voltage_pre_screen: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Violations
// ---------------------------------------------------------------------------

/// A single violation detected in a contingency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Violation {
    /// Branch flow exceeds thermal rating.
    ThermalOverload {
        branch_idx: usize,
        from_bus: u32,
        to_bus: u32,
        loading_pct: f64,
        flow_mw: f64,
        flow_mva: f64,
        limit_mva: f64,
    },
    /// Bus voltage below minimum.
    VoltageLow {
        bus_number: u32,
        vm: f64,
        limit: f64,
    },
    /// Bus voltage above maximum.
    VoltageHigh {
        bus_number: u32,
        vm: f64,
        limit: f64,
    },
    /// Contingency did not converge (island or extreme conditions).
    ///
    /// Includes diagnostic info so operators know why and where.
    NonConvergent {
        /// Final max mismatch when iterations stopped (p.u.).
        max_mismatch: f64,
        /// Number of iterations attempted before giving up.
        iterations: u32,
    },
    /// CTG-09: The contingency created electrical islands (multiple disconnected
    /// components).  Each island is solved independently; this violation flags
    /// the presence of islanding so operators can investigate.
    Islanding {
        /// Number of disconnected components detected after the outage.
        n_components: usize,
    },
    /// Post-contingency flowgate flow exceeds limit.
    FlowgateOverload {
        name: String,
        flow_mw: f64,
        limit_mw: f64,
        loading_pct: f64,
    },
    /// Post-contingency interface flow exceeds limit.
    InterfaceOverload {
        name: String,
        flow_mw: f64,
        limit_mw: f64,
        loading_pct: f64,
    },
}

/// High-level contingency solve status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContingencyStatus {
    /// A full AC solve converged on a connected post-contingency network.
    Converged,
    /// A full AC solve converged after island detection split the network.
    Islanded,
    /// An approximate FDPF fallback produced actionable violations after NR failed.
    Approximate,
    /// No converged solution was obtained.
    #[default]
    NonConverged,
}

impl ContingencyStatus {
    /// Return `true` when the result should be counted as converged.
    pub fn is_converged(self) -> bool {
        matches!(self, Self::Converged | Self::Islanded)
    }
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// Result for a single contingency.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContingencyResult {
    /// Contingency identifier.
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Branch indices outaged in this contingency (propagated from `Contingency::branch_indices`).
    /// Used by post-processing steps (e.g. RAS application) to identify which elements were tripped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branch_indices: Vec<usize>,
    /// Generator indices outaged in this contingency (propagated from `Contingency::generator_indices`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generator_indices: Vec<usize>,
    /// High-level outcome of the contingency solve.
    #[serde(default)]
    pub status: ContingencyStatus,
    /// Whether the AC power flow converged.
    pub converged: bool,
    /// Number of NR iterations (0 if non-convergent).
    pub iterations: u32,
    /// All violations found for this contingency.
    pub violations: Vec<Violation>,
    /// CTG-09: Number of electrical islands detected after applying this
    /// contingency.  `1` = connected (no islanding), `>1` = islanding occurred.
    /// `0` when island detection is disabled.
    pub n_islands: usize,
    /// CTG-02: Corrective redispatch solution, populated when
    /// [`ContingencyOptions::corrective_dispatch`] is `true` and this
    /// contingency has thermal overload violations.  `None` when corrective
    /// dispatch was not requested or there were no thermal violations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrective_dispatch: Option<ScrdSolution>,
    /// Voltage stress assessment results.
    ///
    /// `None` when voltage stress mode is [`VoltageStressMode::Off`] or the
    /// contingency did not converge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage_stress: Option<VoltageStressResult>,
    /// Post-contingency bus voltage magnitudes (p.u.), indexed by internal bus order.
    /// Only populated when [`ContingencyOptions::store_post_voltages`] is true and
    /// the result carries a solved post-state (`Converged` or `Approximate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_vm: Option<Vec<f64>>,
    /// Post-contingency bus voltage angles (radians), indexed by internal bus order.
    /// Only populated when [`ContingencyOptions::store_post_voltages`] is true and
    /// the result carries a solved post-state (`Converged` or `Approximate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_va: Option<Vec<f64>>,
    /// Post-contingency from-side apparent power (MVA) per branch, indexed by branch order.
    /// Only populated when [`ContingencyOptions::store_post_voltages`] is true and
    /// the result carries a solved post-state (`Converged` or `Approximate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_branch_flows: Option<Vec<f64>>,
    /// When `true`, this result was produced by an FDPF fallback after NR failed
    /// to converge.  The violations are approximate (from FDPF solution) and the
    /// result should be treated as [`ContingencyStatus::Approximate`].
    #[serde(default)]
    pub fdpf_fallback: bool,
    /// NERC TPL-001 event category for this contingency.
    #[serde(default)]
    pub tpl_category: TplCategory,
    /// Per-scheme RAS audit trail from corrective action evaluation.
    /// Empty unless `apply_corrective_actions()` was called for this contingency.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scheme_outcomes: Vec<crate::corrective::SchemeOutcome>,
}

/// Summary statistics for the full contingency analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisSummary {
    /// Total number of contingencies evaluated.
    pub total_contingencies: usize,
    /// Number resolved approximately during screening without an exact AC solve.
    pub screened_out: usize,
    /// Number attempted with full AC power flow (converged or not).
    pub ac_solved: usize,
    /// Number of approximate contingency results returned in `results`.
    #[serde(default)]
    pub approximate_returned: usize,
    /// Number of AC solves that converged.
    pub converged: usize,
    /// Number of contingencies with at least one violation.
    pub with_violations: usize,
    /// Wall-clock time for the entire analysis (seconds).
    pub solve_time_secs: f64,

    // ── Voltage stability summary ──
    /// Number of contingencies classified as voltage-critical
    /// ([`VsmCategory::Critical`] or [`VsmCategory::Unstable`]).
    /// Zero when `voltage_stress_mode` is not [`VoltageStressMode::ExactLIndex`].
    #[serde(default)]
    pub n_voltage_critical: usize,
}

/// Full contingency analysis results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContingencyAnalysis {
    /// Base case power flow solution.
    pub base_case: PfSolution,
    /// Results for each contingency that was AC-solved.
    pub results: Vec<ContingencyResult>,
    /// Summary statistics.
    pub summary: AnalysisSummary,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ContingencyError {
    #[error("base case power flow failed: {0}")]
    BaseCaseFailed(String),

    #[error("DC power flow failed for screening: {0}")]
    DcSolveFailed(String),

    #[error("invalid contingency options: {0}")]
    InvalidOptions(String),
}

// ---------------------------------------------------------------------------
// Ranking metric
// ---------------------------------------------------------------------------

/// Ranking criterion for [`rank_contingencies`](crate::ranking::rank_contingencies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContingencyMetric {
    /// Worst branch overload (maximum loading percentage across all branches).
    MaxFlowPct,
    /// Worst low voltage (minimum bus voltage magnitude in p.u.).
    MinVoltagePu,
    /// Worst high voltage (maximum bus voltage magnitude in p.u.).
    MaxVoltagePu,
}
