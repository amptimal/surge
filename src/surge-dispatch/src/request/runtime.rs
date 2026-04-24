// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Runtime and process-local request options.

use std::sync::Arc;

use schemars::JsonSchema;
use surge_opf::AcOpfOptions;
use surge_opf::backends::{LpSolver, NlpSolver};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct AcDispatchWarmStart {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buses: Vec<BusPeriodVoltageSeries>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generators: Vec<ResourcePeriodPowerSeries>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatchable_loads: Vec<ResourcePeriodPowerSeries>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hvdc_links: Vec<HvdcPeriodPowerSeries>,
}

impl AcDispatchWarmStart {
    pub fn is_empty(&self) -> bool {
        self.buses.is_empty()
            && self.generators.is_empty()
            && self.dispatchable_loads.is_empty()
            && self.hvdc_links.is_empty()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusPeriodVoltageSeries {
    pub bus_number: u32,
    pub vm_pu: Vec<f64>,
    pub va_rad: Vec<f64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourcePeriodPowerSeries {
    pub resource_id: String,
    pub p_mw: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub q_mvar: Vec<f64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HvdcPeriodPowerSeries {
    pub link_id: String,
    pub p_mw: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub q_fr_mvar: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub q_to_mvar: Vec<f64>,
}

/// Asymmetric quadratic penalty pair for one generator or dispatchable
/// load in the AC-OPF target-tracking term.
///
/// The tracking objective applies
/// `upward_per_mw2 * max(0, p - target)² + downward_per_mw2 * max(0, target - p)²`.
/// Symmetric behaviour (the legacy case) can be encoded by setting both
/// fields to the same value.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, JsonSchema, Default)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct AcDispatchTargetTrackingPair {
    /// Penalty coefficient when `p > target` (resource over-produces).
    pub upward_per_mw2: f64,
    /// Penalty coefficient when `p < target` (resource under-produces).
    pub downward_per_mw2: f64,
}

impl AcDispatchTargetTrackingPair {
    pub fn is_zero(&self) -> bool {
        self.upward_per_mw2 <= 0.0 && self.downward_per_mw2 <= 0.0
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct AcDispatchTargetTracking {
    /// **Legacy** symmetric quadratic penalty applied to generator
    /// active-power deviation from the corresponding AC warm-start
    /// target. When nonzero, this value is used as the symmetric
    /// `(upward_per_mw2, downward_per_mw2)` default coefficient pair
    /// for every generator that does not have a per-resource override.
    pub generator_p_penalty_per_mw2: f64,
    /// Per-direction default coefficient pair for generators. Overrides
    /// the legacy symmetric field when at least one component is
    /// positive. Leave both fields at zero to use the legacy scalar.
    #[serde(default, skip_serializing_if = "AcDispatchTargetTrackingPair::is_zero")]
    pub generator_p_coefficients_default: AcDispatchTargetTrackingPair,
    /// Per-resource-id overrides for the generator coefficient pair.
    /// When an entry exists for a generator's `resource_id`, it replaces
    /// the default. Useful for giving free renewables a strong downward
    /// penalty and a zero upward penalty, or for pinning expensive
    /// peakers in both directions.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub generator_p_coefficients_overrides_by_id:
        std::collections::HashMap<String, AcDispatchTargetTrackingPair>,
    /// **Legacy** symmetric quadratic penalty applied to
    /// dispatchable-load served active-power deviation.
    pub dispatchable_load_p_penalty_per_mw2: f64,
    /// Per-direction default coefficient pair for dispatchable loads.
    #[serde(default, skip_serializing_if = "AcDispatchTargetTrackingPair::is_zero")]
    pub dispatchable_load_p_coefficients_default: AcDispatchTargetTrackingPair,
    /// Per-resource-id overrides for the dispatchable-load coefficient
    /// pair, keyed by `dispatchable_load.resource_id`.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub dispatchable_load_p_coefficients_overrides_by_id:
        std::collections::HashMap<String, AcDispatchTargetTrackingPair>,
}

impl AcDispatchTargetTracking {
    pub fn is_disabled(&self) -> bool {
        let gen_disabled = self.generator_p_penalty_per_mw2 <= 0.0
            && self.generator_p_coefficients_default.is_zero()
            && self
                .generator_p_coefficients_overrides_by_id
                .values()
                .all(AcDispatchTargetTrackingPair::is_zero);
        let load_disabled = self.dispatchable_load_p_penalty_per_mw2 <= 0.0
            && self.dispatchable_load_p_coefficients_default.is_zero()
            && self
                .dispatchable_load_p_coefficients_overrides_by_id
                .values()
                .all(AcDispatchTargetTrackingPair::is_zero);
        gen_disabled && load_disabled
    }
}

impl Default for AcDispatchTargetTracking {
    fn default() -> Self {
        Self {
            generator_p_penalty_per_mw2: 0.0,
            generator_p_coefficients_default: AcDispatchTargetTrackingPair::default(),
            generator_p_coefficients_overrides_by_id: std::collections::HashMap::new(),
            dispatchable_load_p_penalty_per_mw2: 0.0,
            dispatchable_load_p_coefficients_default: AcDispatchTargetTrackingPair::default(),
            dispatchable_load_p_coefficients_overrides_by_id: std::collections::HashMap::new(),
        }
    }
}

/// A single Benders optimality cut on the SCED LP from an AC-OPF subproblem.
///
/// Encodes a linear lower bound of the form
///
///   `eta[period] >= rhs_dollars_per_hour + Σ_g coefficient[gen] * Pg[gen, period]`
///
/// where `eta[period]` is a scalar epigraph variable that the SCED LP will
/// minimise (one per period). The cut is generated by solving the AC-OPF
/// subproblem with `Pg` fixed to a candidate master schedule and reading the
/// shadow prices on the bound constraints. See
/// `surge_opf::solve_ac_opf_subproblem` for the cut construction details
/// and the SCED-AC Benders module documentation for the convergence story.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScedAcBendersCut {
    /// Period index this cut applies to.
    pub period: usize,
    /// Per-generator slope coefficient `λ_g` in `$/MW-hr`. Keyed by the
    /// generator's `resource_id` (i.e. `Generator::id`) so the cut survives
    /// network re-build / re-canonicalisation between solver invocations.
    /// Generators absent from the map have an implicit coefficient of zero.
    pub coefficients_dollars_per_mw_per_hour: std::collections::HashMap<String, f64>,
    /// Constant term of the cut in `$/hr`. Already incorporates `slack_cost`
    /// and the linear-expansion offset `−Σ λ_g · P̃g_g` evaluated at the
    /// master point that produced this cut.
    pub rhs_dollars_per_hour: f64,
    /// Iteration index that generated this cut. Used by the orchestration
    /// loop for diagnostics and cut pruning.
    #[serde(default)]
    pub iteration: usize,
}

/// Orchestration parameters controlling the SCED-AC Benders master /
/// subproblem loop. When populated inside
/// [`ScedAcBendersRuntime::orchestration`], the dispatch solver runs the
/// full decomposition internally (using
/// [`crate::sced_ac_benders::solve_sced_sequence_benders`]) rather than
/// expecting the caller to drive iterations from outside.
///
/// Every field has a sensible default; callers typically populate only
/// the fields they want to override.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct ScedAcBendersRunParams {
    /// Maximum number of master iterations per period. Set low for
    /// smoke tests; production runs typically use 25–50.
    pub max_iterations: usize,
    /// Relative-gap tolerance `(UB − LB) / max(|UB|, 1)` below which the
    /// loop terminates with `converged = true`.
    pub rel_tol: f64,
    /// Absolute-gap tolerance `(UB − LB)` in `$/hr` summed over the
    /// horizon below which the loop terminates with `converged = true`.
    pub abs_tol: f64,
    /// Periods whose subproblem slack is below this threshold are
    /// considered AC-feasible and do not generate cuts. A tiny floor
    /// avoids wasting cuts on numerical noise.
    pub min_slack_dollars_per_hour: f64,
    /// Cut coefficients whose magnitude is below this trim are dropped
    /// before the cut is installed in the master LP.
    pub marginal_trim_dollars_per_mw_per_hour: f64,
    /// Optional trust region (MW). When `Some(width)`, each iteration's
    /// master dispatch is clipped to within `±width` of the previous
    /// iteration's dispatch (per-generator, per-period) by tightening
    /// the generator bounds on the LP. `None` disables the trust region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_region_mw: Option<f64>,
    /// Trust-region expansion factor after successful iterations.
    pub trust_region_expansion_factor: f64,
    /// Trust-region contraction factor after stalled iterations.
    pub trust_region_contraction_factor: f64,
    /// Minimum trust-region width (MW). Floor below which further
    /// contraction is pointless.
    pub trust_region_min_mw: f64,
    /// Optional cap on the number of cuts per period in the master cut
    /// pool. When exceeded, the loosest cuts at the current master
    /// dispatch are pruned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cuts_per_period: Option<usize>,
    /// Numerical tolerance for the cut dedup pass. Two cuts whose
    /// coefficients and rhs differ by less than this are treated as
    /// duplicates.
    pub cut_dedup_marginal_tol: f64,
    /// Number of consecutive iterations without a UB improvement before
    /// the loop terminates with `"stagnation"`.
    pub stagnation_patience: usize,
    /// Number of recent iterations considered for the bimodal
    /// alternation detector; `0` disables the check.
    pub oscillation_patience: usize,
    /// AC OPF subproblem thermal-overload slack penalty (`$/MVA-hr`).
    /// A large finite value keeps the subproblem feasible even when the
    /// master proposes an AC-infeasible dispatch.
    pub ac_opf_thermal_slack_penalty_per_mva: f64,
    /// AC OPF subproblem bus active-power balance slack penalty
    /// (`$/MW-hr`).
    pub ac_opf_bus_active_power_balance_slack_penalty_per_mw: f64,
    /// AC OPF subproblem bus reactive-power balance slack penalty
    /// (`$/MVAr-hr`).
    pub ac_opf_bus_reactive_power_balance_slack_penalty_per_mvar: f64,
}

impl Default for ScedAcBendersRunParams {
    fn default() -> Self {
        Self {
            max_iterations: 25,
            rel_tol: 1.0e-4,
            abs_tol: 1.0,
            min_slack_dollars_per_hour: 1.0e-3,
            marginal_trim_dollars_per_mw_per_hour: 1.0e-6,
            trust_region_mw: None,
            trust_region_expansion_factor: 2.0,
            trust_region_contraction_factor: 0.5,
            trust_region_min_mw: 1.0,
            max_cuts_per_period: None,
            cut_dedup_marginal_tol: 1.0e-9,
            stagnation_patience: 3,
            oscillation_patience: 4,
            ac_opf_thermal_slack_penalty_per_mva: 1.0e4,
            ac_opf_bus_active_power_balance_slack_penalty_per_mw: 1.0e4,
            ac_opf_bus_reactive_power_balance_slack_penalty_per_mvar: 1.0e4,
        }
    }
}

/// Per-period configuration for SCED-AC Benders decomposition.
///
/// When `period_eta_active` is true for a period, the SCED LP allocates a
/// scalar `eta[period]` variable with cost coefficient `+1.0` and adds a
/// row for every cut in `cuts` whose `period` matches. Periods not listed
/// run the standard SCED LP unchanged.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct ScedAcBendersRuntime {
    /// List of period indices for which the SCED LP should activate the
    /// `eta[period]` epigraph variable. Empty disables Benders entirely.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub eta_periods: Vec<usize>,
    /// Cut pool. Each cut is keyed to a single period via `cut.period`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuts: Vec<ScedAcBendersCut>,
    /// When `Some`, the dispatch solver runs the full SCED-AC Benders
    /// master/subproblem orchestration loop internally using these
    /// parameters. When `None`, the solver uses `eta_periods`/`cuts`
    /// as-is, leaving iteration control to an external driver.
    ///
    /// Enabling orchestration only has an effect for the AC formulation
    /// on sequential horizons; the DC formulation and time-coupled AC
    /// horizons ignore the flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<ScedAcBendersRunParams>,
}

impl ScedAcBendersRuntime {
    pub fn is_empty(&self) -> bool {
        self.eta_periods.is_empty() && self.cuts.is_empty() && self.orchestration.is_none()
    }

    /// Iterator over cuts that apply to a specific period.
    pub fn cuts_for_period(&self, period: usize) -> impl Iterator<Item = &ScedAcBendersCut> {
        self.cuts.iter().filter(move |c| c.period == period)
    }

    /// Whether the LP for the given period should allocate an `eta` variable.
    pub fn period_eta_active(&self, period: usize) -> bool {
        self.eta_periods.contains(&period)
    }
}

/// Runtime execution controls for dispatch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct DispatchRuntime {
    pub tolerance: f64,
    pub run_pricing: bool,
    pub ac_relax_committed_pmin_to_zero: bool,
    /// External `surge_opf::AcOpfOptions`; treated as opaque JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub ac_opf: Option<AcOpfOptions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed_hvdc_dispatch: Vec<HvdcPeriodPowerSeries>,
    #[serde(default, skip_serializing_if = "AcDispatchWarmStart::is_empty")]
    pub ac_dispatch_warm_start: AcDispatchWarmStart,
    #[serde(default, skip_serializing_if = "AcDispatchTargetTracking::is_disabled")]
    pub ac_target_tracking: AcDispatchTargetTracking,
    #[serde(default, skip_serializing_if = "ScedAcBendersRuntime::is_empty")]
    pub sced_ac_benders: ScedAcBendersRuntime,
    /// When true, capture a structured [`crate::model_diagnostic::ModelDiagnostic`]
    /// snapshot after each LP/MIP solve stage. The diagnostics are attached to
    /// the [`crate::DispatchSolution`] for post-solve inspection.
    ///
    /// Cost: a single scan over the primal/dual vectors (negligible vs solve time).
    #[serde(default)]
    pub capture_model_diagnostics: bool,
    /// Diagnostic knob: when `true`, pin every per-bus power-balance slack
    /// column (`pb_curtailment_bus`, `pb_excess_bus`, `pb_curtailment_seg`,
    /// `pb_excess_seg`) to `col_upper = 0`, making the DC SCUC bus-balance
    /// rows firm. Useful for measuring the LP weight of the soft-balance
    /// slack family on large scenarios. Off by default — production solves
    /// need slacks so infeasible inputs still produce a solve rather than
    /// an infeasibility status.
    #[serde(default)]
    pub scuc_firm_bus_balance_slacks: bool,
    /// Diagnostic knob: when `true`, pin every branch thermal slack column
    /// (`branch_lower_slack`, `branch_upper_slack`) to `col_upper = 0`,
    /// making the SCUC branch thermal rows firm. Different from
    /// [`crate::request::ThermalLimitPolicy::enforce`] = `false`, which
    /// skips the thermal rows entirely; this preserves the rows but
    /// removes the slack escape hatch. Off by default.
    #[serde(default)]
    pub scuc_firm_branch_thermal_slacks: bool,
    /// When `true`, drop the SCUC per-bus power-balance row family and
    /// the associated `pb_curtailment_bus` / `pb_excess_bus` /
    /// `pb_curtailment_seg` / `pb_excess_seg` column blocks from the
    /// layout entirely, replacing them with a single system-wide
    /// balance row per period. The `theta` and per-branch thermal rows
    /// remain allocated but become vestigial (no KCL couples them to
    /// `pg`), so this flag effectively produces a copperplate SCUC
    /// with soft reserves and all intertemporal logic intact.
    ///
    /// Motivation: on stressed large networks (6049-bus D1 and above)
    /// the per-bus balance + pb-slack families dominate the MIP's LP
    /// relaxation time and leave Gurobi trapped with dummy objectives.
    /// Enabling this knob lets the commitment search converge against
    /// a trivial transportation model; the AC SCED stage still
    /// enforces full nodal physics afterwards when it runs. Off by
    /// default — production solves keep per-bus balance.
    #[serde(default)]
    pub scuc_disable_bus_power_balance: bool,
    /// Per-period AC SCED concurrency.
    ///
    /// * `None` (default) — sequential per-period AC SCED. Each period
    ///   warm-starts from the previous period's `OpfSolution`, so the
    ///   solver chain inherits the prior NLP basin (faster convergence on
    ///   stressed scenarios at the cost of an unbreakable serial loop).
    /// * `Some(n)` (n ≥ 2) — run periods in parallel on a rayon thread
    ///   pool of size `n`. AC→AC warm-start is dropped; each period
    ///   instead falls back to the per-period AC power-flow warm-start
    ///   (`runtime_with_pf_warm_start_if_available`). The `prev_dispatch_mw`
    ///   anchor used for ramp constraints comes from the per-period
    ///   `generator_dispatch_bounds` midpoint — equivalent to the DC
    ///   SCUC dispatch when the bounds are tightly pinned around the
    ///   source-stage dispatch (as in a two-stage reconcile pipeline).
    ///   Wider bounds make this a heuristic; results may differ from
    ///   sequential at Ipopt's tolerance level.
    /// * `Some(0)` is normalized to sequential to keep the knob simple.
    ///
    /// Storage SoC continuity is enforced via sequential threading and is
    /// **not** preserved in parallel mode; networks with `is_storage()`
    /// generators force a sequential fallback regardless of this setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_sced_period_concurrency: Option<usize>,
}

impl Default for DispatchRuntime {
    fn default() -> Self {
        Self {
            tolerance: 1e-8,
            run_pricing: true,
            ac_relax_committed_pmin_to_zero: false,
            ac_opf: None,
            fixed_hvdc_dispatch: Vec::new(),
            ac_dispatch_warm_start: AcDispatchWarmStart::default(),
            ac_target_tracking: AcDispatchTargetTracking::default(),
            sced_ac_benders: ScedAcBendersRuntime::default(),
            capture_model_diagnostics: false,
            scuc_firm_bus_balance_slacks: false,
            scuc_firm_branch_thermal_slacks: false,
            scuc_disable_bus_power_balance: false,
            ac_sced_period_concurrency: None,
        }
    }
}

/// Process-local solve options that should not be embedded in a serialized request.
#[derive(Clone, Default)]
pub struct DispatchSolveOptions {
    /// Optional LP solver backend override for this process.
    pub lp_solver: Option<Arc<dyn LpSolver>>,
    /// Optional NLP solver backend override for AC dispatch / AC-OPF in this process.
    pub nlp_solver: Option<Arc<dyn NlpSolver>>,
}
