// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Public keyed dispatch result types.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use surge_network::market::{PowerBalanceViolation, VirtualBidResult};
use surge_solution::{
    AuditableSolution, ObjectiveBucket, ObjectiveLedgerMismatch, ObjectiveLedgerScopeKind,
    ObjectiveSubjectKind, ObjectiveTerm, ObjectiveTermKind, ParResult, SolutionAuditReport,
};

use crate::ids::{AreaId, ZoneId};
use crate::request::{CommitmentPolicyKind, Formulation, IntervalCoupling};

/// Semantic role of one solve stage inside a larger market workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStageRole {
    UnitCommitment,
    EconomicDispatch,
    Pricing,
    ReliabilityCommitment,
    AcRedispatch,
    Custom(String),
}

/// Optional workflow provenance attached to one dispatch solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchStageMetadata {
    /// Stable stage id within the parent workflow.
    pub stage_id: String,
    /// Semantic role for the stage.
    pub role: DispatchStageRole,
    /// Upstream stage whose outputs seeded this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from_stage_id: Option<String>,
    /// Upstream stage that supplied the commitment schedule when fixed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment_source_stage_id: Option<String>,
}

/// Study metadata attached to a dispatch result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchStudy {
    /// Network formulation.
    pub formulation: Formulation,
    /// Interval coupling policy.
    pub coupling: IntervalCoupling,
    /// Commitment policy kind.
    pub commitment: CommitmentPolicyKind,
    /// Number of solved periods.
    pub periods: usize,
    /// Whether N-1 screening was active.
    pub security_enabled: bool,
    /// Workflow provenance when this solve was one stage in a larger market.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<DispatchStageMetadata>,
}

impl Default for DispatchStudy {
    fn default() -> Self {
        Self {
            formulation: Formulation::Dc,
            coupling: IntervalCoupling::PeriodByPeriod,
            commitment: CommitmentPolicyKind::AllCommitted,
            periods: 0,
            security_enabled: false,
            stage: None,
        }
    }
}

/// Derived rollups over the detailed period-level result surface.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DispatchSummary {
    /// Total cost across all solved periods.
    pub total_cost: f64,
    /// Total energy-offer cost across all resources and periods.
    pub total_energy_cost: f64,
    /// Total reserve-offer cost across all resources and periods.
    pub total_reserve_cost: f64,
    /// Total no-load cost across all resources and periods.
    pub total_no_load_cost: f64,
    /// Total startup cost across all resources and periods.
    pub total_startup_cost: f64,
    /// Total shutdown cost across all resources and periods.
    #[serde(default)]
    pub total_shutdown_cost: f64,
    /// Total target-tracking cost across all periods.
    #[serde(default)]
    pub total_tracking_cost: f64,
    /// Total non-energy adder cost across all periods.
    #[serde(default)]
    pub total_adder_cost: f64,
    /// Residual objective cost not covered by the named convenience buckets.
    #[serde(default)]
    pub total_other_cost: f64,
    /// Total CO2 emissions across all periods.
    pub total_co2_t: f64,
    /// Total penalty/slack cost across all periods (dollars).
    /// Sum of all soft-constraint violation costs: power balance, reactive balance,
    /// thermal, ramp, reserve shortfall, headroom/footroom, energy window.
    #[serde(default)]
    pub total_penalty_cost: f64,
    /// Horizon-aggregated exact objective decomposition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objective_terms: Vec<ObjectiveTerm>,
}

/// Public resource category for keyed dispatch results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchResourceKind {
    #[default]
    Generator,
    Storage,
    DispatchableLoad,
}

/// Stable public identity for a dispatch resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchResource {
    /// Stable public resource id within the result payload.
    pub resource_id: String,
    /// Resource kind.
    pub kind: DispatchResourceKind,
    /// Connected bus number when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus_number: Option<u32>,
    /// Generator machine id when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// Human-readable label when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Source-array index in the originating model object list.
    #[serde(skip)]
    pub(crate) source_index: usize,
}

/// Stable public identity for a bus in the result payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchBus {
    /// External bus number.
    pub bus_number: u32,
    /// Human-readable bus name.
    pub name: String,
    /// Area id.
    pub area: AreaId,
    /// Zone id.
    pub zone: ZoneId,
}

/// Security-constrained dispatch metadata attached to a dispatch solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityDispatchMetadata {
    /// Number of outer-loop iterations performed.
    pub iterations: usize,
    /// Total number of contingency cuts added.
    pub n_cuts: usize,
    /// Whether the security outer loop ended with no remaining violations.
    pub converged: bool,
    /// Number of branch-outage violations observed in the last screening pass.
    pub last_branch_violations: usize,
    /// Number of HVDC-outage violations observed in the last screening pass.
    pub last_hvdc_violations: usize,
    /// Worst branch-outage thermal violation seen in the last screening pass, in p.u.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_branch_violation_pu: Option<f64>,
    /// Worst HVDC-outage thermal violation seen in the last screening pass, in p.u.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hvdc_violation_pu: Option<f64>,
    /// Number of contingency cuts pre-seeded into iter 0 (before the first
    /// SCUC solve). `0` when pre-seeding is disabled.
    #[serde(default)]
    pub n_preseed_cuts: usize,
    /// Of the `n_preseed_cuts` pre-seeded pairs, how many turned out to
    /// produce a post-contingency violation on the final converged dispatch
    /// (i.e., would have been re-discovered by the screener). A high ratio
    /// means the pre-seed ranking was well-tuned; a low ratio means the LP
    /// is carrying dead constraint rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_preseed_pairs_binding: Option<usize>,
}

/// Per-phase wall-clock timings for the dispatch pipeline (seconds).
///
/// Fields are only populated when measured; `0.0` means "not applicable"
/// or "below resolution". The sum of all fields approximates the total
/// wall time inside `DispatchModel::solve_with_options`, which is
/// typically ≳ `solve_time_secs` (the pure optimizer kernel wall).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DispatchPhaseTimings {
    /// `request.resolve_with_options` + `validate_dispatch_request_inputs`.
    pub prepare_request_secs: f64,
    /// `normalized.problem_spec()` — construct the DispatchProblemSpec
    /// from the prepared request.
    pub problem_spec_secs: f64,
    /// `runtime_network.clone()` + `canonicalize_generator_ids` inside
    /// the SCUC/SCED entry. Also covers `network_at_hour_with_spec`.
    pub network_snapshot_secs: f64,
    /// `build_horizon_solve_session` / `build_period_solve_session` —
    /// create the solve session wrapper (also starts the session clock).
    pub build_session_secs: f64,
    /// `build_model_plan` — structural plan for the MIP/LP.
    pub build_model_plan_secs: f64,
    /// `build_problem_plan` — per-period problem plan (rows/cols).
    pub build_problem_plan_secs: f64,
    /// `build_problem` — allocate the concrete MIP/LP data structures.
    pub build_problem_secs: f64,
    /// `solve_problem` — hand off to the solver backend and block. This
    /// is the closest to pure optimizer wall and usually matches
    /// `solve_time_secs` within a small delta.
    pub solve_problem_secs: f64,
    /// `run_pricing` / `skip_pricing` post-solve pricing stage.
    pub pricing_secs: f64,
    /// `extract_solution` — convert raw optimizer output into the public
    /// `RawDispatchSolution` (per-device dispatch schedules, costs,
    /// commitment flags, LMPs, reserves).
    pub extract_solution_secs: f64,
    /// `attach_public_catalogs_and_solve_metadata` — populate the public
    /// resource/bus catalogs on the RawDispatchSolution.
    pub attach_public_catalogs_secs: f64,
    /// `attach_keyed_period_views` — build per-period keyed views
    /// (bus-level injections, LMPs, flows) on the RawDispatchSolution.
    pub attach_keyed_period_views_secs: f64,
    /// `emit_public_keyed_solution` — map canonical generator/bus IDs
    /// back to public UIDs for the final keyed DispatchSolution.
    pub emit_keyed_secs: f64,
    /// Total wall of `solve_prepared_dispatch_raw` measured from the
    /// outside. Difference between this and the sum of the phases
    /// inside it reveals destructor time and any un-instrumented
    /// overhead (e.g. dropping the problem MIP model, pricing state,
    /// solve session) that can't be charged to a specific phase.
    pub solve_prepared_raw_total_secs: f64,
    /// Total wall of `solve_scuc_with_problem_spec` measured from the
    /// outside (inside `solve_prepared_dispatch_raw`). Difference
    /// against the sum of internal SCUC phases isolates destructor time
    /// of locals owned by the SCUC entry function (solve_session,
    /// runtime_network, model_plan, problem_plan, problem_build,
    /// problem_state, pricing state) dropping at function return.
    pub solve_scuc_external_secs: f64,
    /// Wall spent in `solve_explicit_security_dispatch` *around* its
    /// inner `solve_scuc_with_problem_spec` call: building hourly
    /// networks + PTDF/LODF hourly contexts, the flowgate-row loop
    /// over (period × contingency × monitored branch), cloning the
    /// network and extending its flowgate list, `attach_security_metadata`,
    /// and the destructor tail when security-local state drops at
    /// function return. Zero when the dispatch doesn't take the
    /// security path. `solve_scuc_external_secs =
    /// security_setup_secs + solve_scuc_self_total_secs` (modulo tiny
    /// instrument overhead) when `security_setup_secs > 0`.
    pub security_setup_secs: f64,
    /// Wall time spent explicitly dropping `runtime_network` +
    /// `solve_session` + `net0` at the tail of
    /// `solve_scuc_with_problem_spec`. The remaining SCUC destructor
    /// gap (raw_total − phases − this) is destructors of the
    /// PricingRunState+ScucProblemState graph, which happen INSIDE
    /// extract_solution's return path.
    pub scuc_local_drops_secs: f64,
    /// Total wall of `solve_scuc_with_problem_spec` measured by its
    /// own first/last `Instant::now()` pair — i.e. including any
    /// compiler-inserted work between the last phase and the function
    /// return, plus the destructors of *all* its locals.
    pub solve_scuc_self_total_secs: f64,
}

/// Solver/process diagnostics separated from the core keyed result surface.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DispatchDiagnostics {
    /// Solver iterations / node count.
    pub iterations: u32,
    /// Wall-clock solve time in seconds.
    pub solve_time_secs: f64,
    /// Per-phase wall-clock timings inside the dispatch pipeline.
    ///
    /// `solve_time_secs` is the window from `build_horizon_solve_session`
    /// through pricing (the optimizer wall). `phase_timings` breaks the
    /// full `DispatchModel::solve_with_options` wall into ten phases so
    /// model-build and solution-extract overhead is visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_timings: Option<DispatchPhaseTimings>,
    /// Whether a separate pricing stage converged when one was run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_converged: Option<bool>,
    /// Raw soft-constraint slack magnitudes retained for diagnostics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub penalty_slack_values: Vec<f64>,
    /// Security-constrained outer-loop metadata when active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityDispatchMetadata>,
    /// SCED-AC Benders orchestration diagnostics. Populated when the
    /// request's `runtime.sced_ac_benders.orchestration` field was set
    /// and the Rust Benders loop was invoked for the solve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sced_ac_benders: Option<crate::sced_ac_benders::BendersDiagnostics>,
    /// Per-period AC SCED timing breakdown. One entry per period,
    /// populated by the sequential (or parallel) AC SCED path.
    /// Empty for DC-only solves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ac_sced_period_timings: Vec<crate::sced::ac::AcScedPeriodTimings>,
    /// Commitment (SCUC) MIP progress trace. Populated only when the
    /// caller supplied a `mip_gap_schedule` on the commitment options
    /// and the LP/MIP backend supports progress callbacks (Gurobi today).
    /// Holds the schedule, per-event samples of `(time, incumbent, bound,
    /// gap, target)`, and the final termination reason — intended for
    /// post-mortem "gap vs time" analysis and schedule tuning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment_mip_trace: Option<surge_opf::backends::MipTrace>,
}

/// How the commitment state for a resource-period was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentSource {
    #[default]
    AllCommitted,
    Fixed,
    Optimized,
    DayAhead,
    Additional,
}

/// Scope for a reserve requirement bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReserveScope {
    #[default]
    System,
    Zone,
}

/// Public promoted constraint classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    ReserveRequirement,
    ReserveCoupling,
    ReserveRampSharing,
    BranchThermal,
    Flowgate,
    Interface,
    PowerBalance,
    GeneratorBound,
    DispatchBlockBound,
    DispatchableLoadBound,
    StorageSoc,
    StorageSocBound,
    StorageReserveCoupling,
    StorageDispatchLink,
    Ramp,
    Frequency,
    Commitment,
    CommitmentCapacity,
    ReactiveBalance,
    VoltageBound,
    EnergyWindow,
    AngleDifference,
    #[default]
    Other,
}

/// Scope descriptor for promoted constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintScope {
    System,
    Zone,
    Bus,
    Resource,
    Branch,
    Flowgate,
    Interface,
    Hvdc,
    #[default]
    Other,
}

/// Generator-specific period detail.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneratorPeriodDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment_source: Option<CommitmentSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regulation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_mvar: Option<f64>,
}

/// Storage-specific period detail.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoragePeriodDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment_source: Option<CommitmentSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regulation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_mvar: Option<f64>,
    pub charge_mw: f64,
    pub discharge_mw: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soc_mwh: Option<f64>,
}

/// Dispatchable-load-specific period detail.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DispatchableLoadPeriodDetail {
    pub served_p_mw: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_q_mvar: Option<f64>,
    pub curtailed_mw: f64,
    pub curtailment_pct: f64,
    pub lmp_at_bus: f64,
    pub net_curtailment_benefit: f64,
}

/// Resource-specific keyed period detail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "detail_type", content = "detail", rename_all = "snake_case")]
pub enum ResourcePeriodDetail {
    Generator(GeneratorPeriodDetail),
    Storage(StoragePeriodDetail),
    DispatchableLoad(DispatchableLoadPeriodDetail),
}

impl Default for ResourcePeriodDetail {
    fn default() -> Self {
        Self::Generator(GeneratorPeriodDetail::default())
    }
}

/// Per-resource keyed market and physical outcome for one period.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResourcePeriodResult {
    pub resource_id: String,
    pub kind: DispatchResourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus_number: Option<u32>,
    /// Signed grid-connection power in MW. Positive = injection, negative = withdrawal.
    pub power_mw: f64,
    /// Exact total objective contribution for this resource in the period.
    #[serde(default)]
    pub objective_cost: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_load_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reserve_awards: HashMap<String, f64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reserve_costs: HashMap<String, f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objective_terms: Vec<ObjectiveTerm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub co2_t: Option<f64>,
    #[serde(flatten)]
    pub detail: ResourcePeriodDetail,
}

/// Per-resource keyed horizon summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResourceHorizonResult {
    pub resource_id: String,
    pub kind: DispatchResourceKind,
    pub objective_cost: f64,
    pub total_energy_cost: f64,
    pub total_no_load_cost: f64,
    pub total_startup_cost: f64,
    #[serde(default)]
    pub total_shutdown_cost: f64,
    pub total_reserve_cost: f64,
    pub total_co2_t: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objective_terms: Vec<ObjectiveTerm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub co2_shadow_price_per_mwh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment_schedule: Option<Vec<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_schedule: Option<Vec<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_schedule: Option<Vec<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regulation_schedule: Option<Vec<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_soc_mwh: Option<Vec<f64>>,
}

/// Per-bus keyed market and physical outcome for one period.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BusPeriodResult {
    pub bus_number: u32,
    pub lmp: f64,
    pub mec: f64,
    pub mcc: f64,
    pub mlc: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_rad: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage_pu: Option<f64>,
    pub net_injection_mw: f64,
    pub withdrawals_mw: f64,
    /// DC transmission loss allocation at this bus (MW).
    #[serde(default)]
    pub loss_allocation_mw: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_reactive_injection_mvar: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawals_mvar: Option<f64>,
    /// Positive reactive-power balance slack at this bus (MVAr).
    /// Non-zero when the AC OPF cannot fully satisfy Q balance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_slack_pos_mvar: Option<f64>,
    /// Negative reactive-power balance slack at this bus (MVAr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_slack_neg_mvar: Option<f64>,
    /// Positive active-power balance slack at this bus (MW).
    /// Non-zero when the AC OPF cannot fully satisfy P balance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_slack_pos_mw: Option<f64>,
    /// Negative active-power balance slack at this bus (MW).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_slack_neg_mw: Option<f64>,
}

/// Cleared reserve requirement bucket in one period.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReservePeriodResult {
    pub product_id: String,
    pub scope: ReserveScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<ZoneId>,
    pub requirement_mw: f64,
    pub provided_mw: f64,
    pub shortfall_mw: f64,
    pub clearing_price: f64,
    /// Penalty cost for shortfall (dollars for this period).
    #[serde(default)]
    pub shortfall_cost: f64,
}

/// Generic promoted constraint result in one period.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConstraintPeriodResult {
    pub constraint_id: String,
    pub kind: ConstraintKind,
    pub scope: ConstraintScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack_mw: Option<f64>,
    /// Penalty rate ($/MW or $/MVA per unit violation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub penalty_cost: Option<f64>,
    /// Actual penalty dollars for this period (slack × rate × dt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub penalty_dollars: Option<f64>,
}

/// Per-band keyed HVDC dispatch outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HvdcBandPeriodResult {
    pub band_id: String,
    pub mw: f64,
}

/// Per-link keyed HVDC outcome for one period.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HvdcPeriodResult {
    pub link_id: String,
    pub name: String,
    pub mw: f64,
    pub delivered_mw: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub band_results: Vec<HvdcBandPeriodResult>,
}

/// Period emissions rollup and per-resource breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EmissionsPeriodResult {
    pub total_co2_t: f64,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub by_resource_t: HashMap<String, f64>,
}

/// Frequency-security metrics for one period when configured.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FrequencyPeriodResult {
    pub system_inertia_s: f64,
    pub estimated_rocof_hz_per_s: f64,
    pub frequency_secure: bool,
}

/// Combined-cycle keyed horizon result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CombinedCyclePlantResult {
    pub plant_id: String,
    pub name: String,
    pub active_configuration_schedule: Vec<Option<String>>,
    #[serde(default)]
    pub objective_cost: f64,
    pub transition_cost: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objective_terms: Vec<ObjectiveTerm>,
}

/// Public keyed per-period dispatch result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DispatchPeriodResult {
    pub(crate) period_index: usize,
    pub(crate) total_cost: f64,
    pub(crate) co2_t: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) resource_results: Vec<ResourcePeriodResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) bus_results: Vec<BusPeriodResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) reserve_results: Vec<ReservePeriodResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) constraint_results: Vec<ConstraintPeriodResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) hvdc_results: Vec<HvdcPeriodResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) tap_dispatch: Vec<(usize, f64, f64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) phase_dispatch: Vec<(usize, f64, f64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) switched_shunt_dispatch: Vec<(String, u32, f64, f64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) branch_commitment_state: Vec<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) virtual_bid_results: Vec<VirtualBidResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) par_results: Vec<ParResult>,
    #[serde(default)]
    pub(crate) power_balance_violation: PowerBalanceViolation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) objective_terms: Vec<ObjectiveTerm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) emissions_results: Option<EmissionsPeriodResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) frequency_results: Option<FrequencyPeriodResult>,
    /// SCED-AC Benders epigraph variable value (`$/hr`) for this period,
    /// when the master LP allocated an `η[period]` column. `None` for
    /// periods that did not opt in to Benders. Propagated from
    /// [`crate::solution::RawDispatchPeriodResult::sced_ac_benders_eta_dollars_per_hour`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sced_ac_benders_eta_dollars_per_hour: Option<f64>,
    #[serde(skip)]
    pub(crate) resource_index_by_id: OnceLock<HashMap<String, usize>>,
    #[serde(skip)]
    pub(crate) bus_index_by_number: OnceLock<HashMap<u32, usize>>,
    #[serde(skip)]
    pub(crate) hvdc_index_by_id: OnceLock<HashMap<String, usize>>,
    #[serde(skip)]
    pub(crate) reserve_index_by_key: OnceLock<HashMap<(String, Option<ZoneId>), usize>>,
    #[serde(skip)]
    pub(crate) constraint_index_by_id: OnceLock<HashMap<String, usize>>,
}

/// Aggregated penalty/slack cost summary across all solved periods.
///
/// Rolls up penalty dollars from all constraint results and reserve shortfalls.
/// Per-bus, per-branch, and per-product detail is available in each period's
/// `constraint_results` and `reserve_results` vectors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PenaltySummary {
    /// Active-power balance penalty: total MW of curtailment + excess across all periods.
    pub power_balance_p_total_mw: f64,
    /// Active-power balance penalty: total dollars.
    pub power_balance_p_total_cost: f64,
    /// Reactive-power balance penalty: total MVAr of slack across all periods.
    pub power_balance_q_total_mvar: f64,
    /// Reactive-power balance penalty: total dollars.
    pub power_balance_q_total_cost: f64,
    /// Voltage-magnitude penalty: total pu of slack across all periods.
    #[serde(default)]
    pub voltage_total_pu: f64,
    /// Voltage-magnitude penalty: total dollars.
    #[serde(default)]
    pub voltage_total_cost: f64,
    /// Angle-difference penalty: total radians of slack across all periods.
    #[serde(default)]
    pub angle_total_rad: f64,
    /// Angle-difference penalty: total dollars.
    #[serde(default)]
    pub angle_total_cost: f64,
    /// Branch thermal overload penalty: total MW of slack across all periods.
    pub thermal_total_mw: f64,
    /// Branch thermal overload penalty: total dollars.
    pub thermal_total_cost: f64,
    /// Flowgate + interface overload penalty: total MW of slack across all periods.
    pub flowgate_total_mw: f64,
    /// Flowgate + interface overload penalty: total dollars.
    pub flowgate_total_cost: f64,
    /// Ramp violation penalty: total MW of slack across all periods.
    pub ramp_total_mw: f64,
    /// Ramp violation penalty: total dollars.
    pub ramp_total_cost: f64,
    /// Reserve shortfall penalty: total MW of shortfall across all periods.
    pub reserve_shortfall_total_mw: f64,
    /// Reserve shortfall penalty: total dollars.
    pub reserve_shortfall_total_cost: f64,
    /// Headroom/footroom violation penalty: total MW across all periods (SCUC).
    pub headroom_footroom_total_mw: f64,
    /// Headroom/footroom violation penalty: total dollars (SCUC).
    pub headroom_footroom_total_cost: f64,
    /// Energy window violation penalty: total dollars (SCUC).
    pub energy_window_total_cost: f64,
    /// Grand total: sum of all penalty categories.
    pub total_penalty_cost: f64,
}

/// Public keyed dispatch result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DispatchSolution {
    #[serde(default)]
    pub(crate) study: DispatchStudy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) resources: Vec<DispatchResource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) buses: Vec<DispatchBus>,
    #[serde(default)]
    pub(crate) summary: DispatchSummary,
    #[serde(default)]
    pub(crate) diagnostics: DispatchDiagnostics,
    pub(crate) periods: Vec<DispatchPeriodResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) resource_summaries: Vec<ResourceHorizonResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) combined_cycle_results: Vec<CombinedCyclePlantResult>,
    /// Structured model diagnostic snapshots, one per solve stage.
    /// Populated when `DispatchRuntime::capture_model_diagnostics` is enabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) model_diagnostics: Vec<crate::model_diagnostic::ModelDiagnostic>,
    /// Aggregated penalty/slack cost summary across all solved periods.
    #[serde(default)]
    pub(crate) penalty_summary: PenaltySummary,
    /// Persisted exact-audit status for this keyed solution payload.
    #[serde(default)]
    pub(crate) audit: SolutionAuditReport,
    #[serde(skip)]
    pub(crate) resource_index_by_id: OnceLock<HashMap<String, usize>>,
    #[serde(skip)]
    pub(crate) bus_index_by_number: OnceLock<HashMap<u32, usize>>,
    #[serde(skip)]
    pub(crate) period_index_by_number: OnceLock<HashMap<usize, usize>>,
    #[serde(skip)]
    pub(crate) resource_summary_index_by_id: OnceLock<HashMap<String, usize>>,
    #[serde(skip)]
    pub(crate) combined_cycle_index_by_id: OnceLock<HashMap<String, usize>>,
}

const OBJECTIVE_LEDGER_TOLERANCE: f64 = 1e-6;

fn objective_term_total(terms: &[ObjectiveTerm]) -> f64 {
    terms.iter().map(|term| term.dollars).sum()
}

fn objective_bucket_total(terms: &[ObjectiveTerm], bucket: ObjectiveBucket) -> f64 {
    terms
        .iter()
        .filter(|term| term.bucket == bucket)
        .map(|term| term.dollars)
        .sum()
}

fn objective_kind_total(terms: &[ObjectiveTerm], kind: ObjectiveTermKind) -> f64 {
    terms
        .iter()
        .filter(|term| term.kind == kind)
        .map(|term| term.dollars)
        .sum()
}

fn objective_kind_quantity_total(terms: &[ObjectiveTerm], kind: ObjectiveTermKind) -> f64 {
    terms
        .iter()
        .filter(|term| term.kind == kind)
        .map(|term| term.quantity.unwrap_or(0.0))
        .sum()
}

fn objective_component_total(terms: &[ObjectiveTerm], component_id: &str) -> f64 {
    terms
        .iter()
        .filter(|term| term.component_id == component_id)
        .map(|term| term.dollars)
        .sum()
}

fn objective_subject_total(
    terms: &[ObjectiveTerm],
    subject_kind: ObjectiveSubjectKind,
    subject_id: &str,
) -> f64 {
    terms
        .iter()
        .filter(|term| term.subject_kind == subject_kind && term.subject_id == subject_id)
        .map(|term| term.dollars)
        .sum()
}

fn residual_term_total(terms: &[ObjectiveTerm]) -> f64 {
    terms
        .iter()
        .filter(|term| term.kind == ObjectiveTermKind::Other && term.component_id == "residual")
        .map(|term| term.dollars)
        .sum()
}

fn maybe_push_objective_ledger_mismatch(
    mismatches: &mut Vec<ObjectiveLedgerMismatch>,
    scope_kind: ObjectiveLedgerScopeKind,
    scope_id: impl Into<String>,
    field: impl Into<String>,
    expected_dollars: f64,
    actual_dollars: f64,
) {
    let difference = actual_dollars - expected_dollars;
    if difference.abs() > OBJECTIVE_LEDGER_TOLERANCE {
        mismatches.push(ObjectiveLedgerMismatch {
            scope_kind,
            scope_id: scope_id.into(),
            field: field.into(),
            expected_dollars,
            actual_dollars,
            difference,
        });
    }
}

fn validate_resource_period_objective_ledger(
    resource: &ResourcePeriodResult,
    period_index: usize,
    mismatches: &mut Vec<ObjectiveLedgerMismatch>,
) {
    let scope_id = format!("period:{period_index}:{}", resource.resource_id);
    maybe_push_objective_ledger_mismatch(
        mismatches,
        ObjectiveLedgerScopeKind::ResourcePeriod,
        scope_id.clone(),
        "objective_cost",
        objective_term_total(&resource.objective_terms),
        resource.objective_cost,
    );
    if let Some(energy_cost) = resource.energy_cost {
        maybe_push_objective_ledger_mismatch(
            mismatches,
            ObjectiveLedgerScopeKind::ResourcePeriod,
            scope_id.clone(),
            "energy_cost",
            objective_bucket_total(&resource.objective_terms, ObjectiveBucket::Energy),
            energy_cost,
        );
    }
    if let Some(no_load_cost) = resource.no_load_cost {
        maybe_push_objective_ledger_mismatch(
            mismatches,
            ObjectiveLedgerScopeKind::ResourcePeriod,
            scope_id.clone(),
            "no_load_cost",
            objective_bucket_total(&resource.objective_terms, ObjectiveBucket::NoLoad),
            no_load_cost,
        );
    }
    if let Some(startup_cost) = resource.startup_cost {
        maybe_push_objective_ledger_mismatch(
            mismatches,
            ObjectiveLedgerScopeKind::ResourcePeriod,
            scope_id.clone(),
            "startup_cost",
            objective_bucket_total(&resource.objective_terms, ObjectiveBucket::Startup),
            startup_cost,
        );
    }
    if let Some(shutdown_cost) = resource.shutdown_cost {
        maybe_push_objective_ledger_mismatch(
            mismatches,
            ObjectiveLedgerScopeKind::ResourcePeriod,
            scope_id.clone(),
            "shutdown_cost",
            objective_bucket_total(&resource.objective_terms, ObjectiveBucket::Shutdown),
            shutdown_cost,
        );
    }
    for (product_id, reserve_cost) in &resource.reserve_costs {
        maybe_push_objective_ledger_mismatch(
            mismatches,
            ObjectiveLedgerScopeKind::ResourcePeriod,
            scope_id.clone(),
            format!("reserve_costs:{product_id}"),
            objective_component_total(&resource.objective_terms, product_id),
            *reserve_cost,
        );
    }
}

fn validate_resource_summary_objective_ledger(
    summary: &ResourceHorizonResult,
    mismatches: &mut Vec<ObjectiveLedgerMismatch>,
) {
    let scope_id = summary.resource_id.clone();
    maybe_push_objective_ledger_mismatch(
        mismatches,
        ObjectiveLedgerScopeKind::ResourceSummary,
        scope_id.clone(),
        "objective_cost",
        objective_term_total(&summary.objective_terms),
        summary.objective_cost,
    );
    maybe_push_objective_ledger_mismatch(
        mismatches,
        ObjectiveLedgerScopeKind::ResourceSummary,
        scope_id.clone(),
        "total_energy_cost",
        objective_bucket_total(&summary.objective_terms, ObjectiveBucket::Energy),
        summary.total_energy_cost,
    );
    maybe_push_objective_ledger_mismatch(
        mismatches,
        ObjectiveLedgerScopeKind::ResourceSummary,
        scope_id.clone(),
        "total_no_load_cost",
        objective_bucket_total(&summary.objective_terms, ObjectiveBucket::NoLoad),
        summary.total_no_load_cost,
    );
    maybe_push_objective_ledger_mismatch(
        mismatches,
        ObjectiveLedgerScopeKind::ResourceSummary,
        scope_id.clone(),
        "total_startup_cost",
        objective_bucket_total(&summary.objective_terms, ObjectiveBucket::Startup),
        summary.total_startup_cost,
    );
    maybe_push_objective_ledger_mismatch(
        mismatches,
        ObjectiveLedgerScopeKind::ResourceSummary,
        scope_id.clone(),
        "total_shutdown_cost",
        objective_bucket_total(&summary.objective_terms, ObjectiveBucket::Shutdown),
        summary.total_shutdown_cost,
    );
    maybe_push_objective_ledger_mismatch(
        mismatches,
        ObjectiveLedgerScopeKind::ResourceSummary,
        scope_id,
        "total_reserve_cost",
        objective_bucket_total(&summary.objective_terms, ObjectiveBucket::Reserve),
        summary.total_reserve_cost,
    );
}

impl DispatchPeriodResult {
    pub(crate) fn empty(period_index: usize) -> Self {
        Self {
            period_index,
            total_cost: 0.0,
            co2_t: 0.0,
            resource_results: Vec::new(),
            bus_results: Vec::new(),
            reserve_results: Vec::new(),
            constraint_results: Vec::new(),
            hvdc_results: Vec::new(),
            tap_dispatch: Vec::new(),
            phase_dispatch: Vec::new(),
            switched_shunt_dispatch: Vec::new(),
            branch_commitment_state: Vec::new(),
            virtual_bid_results: Vec::new(),
            par_results: Vec::new(),
            power_balance_violation: PowerBalanceViolation::default(),
            objective_terms: Vec::new(),
            emissions_results: None,
            frequency_results: None,
            sced_ac_benders_eta_dollars_per_hour: None,
            resource_index_by_id: OnceLock::new(),
            bus_index_by_number: OnceLock::new(),
            hvdc_index_by_id: OnceLock::new(),
            reserve_index_by_key: OnceLock::new(),
            constraint_index_by_id: OnceLock::new(),
        }
    }

    /// SCED-AC Benders epigraph variable value (`$/hr`) for this period.
    ///
    /// Returns `Some(value)` when the master LP allocated an `η[period]`
    /// column (i.e. the `runtime.sced_ac_benders.eta_periods` list
    /// included this period), and `None` otherwise. The returned value
    /// is the LP-optimal lower bound on the AC physics adder consistent
    /// with all currently-applied Benders cuts for this period.
    pub fn sced_ac_benders_eta_dollars_per_hour(&self) -> Option<f64> {
        self.sced_ac_benders_eta_dollars_per_hour
    }

    fn resource_index_by_id(&self) -> &HashMap<String, usize> {
        self.resource_index_by_id.get_or_init(|| {
            self.resource_results
                .iter()
                .enumerate()
                .map(|(index, resource)| (resource.resource_id.clone(), index))
                .collect()
        })
    }

    fn bus_index_by_number(&self) -> &HashMap<u32, usize> {
        self.bus_index_by_number.get_or_init(|| {
            self.bus_results
                .iter()
                .enumerate()
                .map(|(index, bus)| (bus.bus_number, index))
                .collect()
        })
    }

    fn hvdc_index_by_id(&self) -> &HashMap<String, usize> {
        self.hvdc_index_by_id.get_or_init(|| {
            self.hvdc_results
                .iter()
                .enumerate()
                .map(|(index, result)| (result.link_id.clone(), index))
                .collect()
        })
    }

    fn reserve_index_by_key(&self) -> &HashMap<(String, Option<ZoneId>), usize> {
        self.reserve_index_by_key.get_or_init(|| {
            self.reserve_results
                .iter()
                .enumerate()
                .map(|(index, result)| ((result.product_id.clone(), result.zone_id), index))
                .collect()
        })
    }

    fn constraint_index_by_id(&self) -> &HashMap<String, usize> {
        self.constraint_index_by_id.get_or_init(|| {
            self.constraint_results
                .iter()
                .enumerate()
                .map(|(index, result)| (result.constraint_id.clone(), index))
                .collect()
        })
    }

    pub fn resource(&self, resource_id: &str) -> Option<&ResourcePeriodResult> {
        self.resource_index_by_id()
            .get(resource_id)
            .and_then(|&index| self.resource_results.get(index))
    }

    pub fn period_index(&self) -> usize {
        self.period_index
    }

    pub fn total_cost(&self) -> f64 {
        self.total_cost
    }

    pub fn co2_t(&self) -> f64 {
        self.co2_t
    }

    pub fn resource_results(&self) -> &[ResourcePeriodResult] {
        &self.resource_results
    }

    pub fn bus(&self, bus_number: u32) -> Option<&BusPeriodResult> {
        self.bus_index_by_number()
            .get(&bus_number)
            .and_then(|&index| self.bus_results.get(index))
    }

    pub fn bus_results(&self) -> &[BusPeriodResult] {
        &self.bus_results
    }

    pub fn hvdc(&self, link_id: &str) -> Option<&HvdcPeriodResult> {
        self.hvdc_index_by_id()
            .get(link_id)
            .and_then(|&index| self.hvdc_results.get(index))
    }

    pub fn reserve_results(&self) -> &[ReservePeriodResult] {
        &self.reserve_results
    }

    pub fn reserve(
        &self,
        product_id: &str,
        zone_id: Option<ZoneId>,
    ) -> Option<&ReservePeriodResult> {
        self.reserve_index_by_key()
            .get(&(product_id.to_string(), zone_id))
            .and_then(|&index| self.reserve_results.get(index))
    }

    pub fn constraint(&self, constraint_id: &str) -> Option<&ConstraintPeriodResult> {
        self.constraint_index_by_id()
            .get(constraint_id)
            .and_then(|&index| self.constraint_results.get(index))
    }

    pub fn constraint_results(&self) -> &[ConstraintPeriodResult] {
        &self.constraint_results
    }

    /// Per-period switched-shunt dispatch as `(control_id, bus_number, b_continuous_pu, b_rounded_pu)`.
    pub fn switched_shunt_dispatch(&self) -> &[(String, u32, f64, f64)] {
        &self.switched_shunt_dispatch
    }

    /// Per-period transformer tap dispatch as `(branch_idx, tap_continuous, tap_rounded)`.
    pub fn tap_dispatch(&self) -> &[(usize, f64, f64)] {
        &self.tap_dispatch
    }

    /// Per-period phase-shifter dispatch as `(branch_idx, phase_continuous_rad, phase_rounded_rad)`.
    pub fn phase_dispatch(&self) -> &[(usize, f64, f64)] {
        &self.phase_dispatch
    }

    pub fn hvdc_results(&self) -> &[HvdcPeriodResult] {
        &self.hvdc_results
    }

    pub fn virtual_bid_results(&self) -> &[VirtualBidResult] {
        &self.virtual_bid_results
    }

    pub fn par_results(&self) -> &[ParResult] {
        &self.par_results
    }

    pub fn power_balance_violation(&self) -> &PowerBalanceViolation {
        &self.power_balance_violation
    }

    pub fn objective_terms(&self) -> &[ObjectiveTerm] {
        &self.objective_terms
    }

    /// Return every objective-ledger mismatch found in this period view.
    pub fn objective_ledger_mismatches(&self) -> Vec<ObjectiveLedgerMismatch> {
        let mut mismatches = Vec::new();
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchPeriod,
            format!("period:{}", self.period_index),
            "total_cost",
            objective_term_total(&self.objective_terms),
            self.total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchPeriod,
            format!("period:{}", self.period_index),
            "residual",
            0.0,
            residual_term_total(&self.objective_terms),
        );
        for resource in &self.resource_results {
            validate_resource_period_objective_ledger(resource, self.period_index, &mut mismatches);
        }
        for reserve in &self.reserve_results {
            let subject_id = match reserve.zone_id {
                Some(zone_id) => format!("reserve:zone:{}:{}", zone_id.0, reserve.product_id),
                None => format!("reserve:system:{}", reserve.product_id),
            };
            maybe_push_objective_ledger_mismatch(
                &mut mismatches,
                ObjectiveLedgerScopeKind::DispatchPeriod,
                format!("period:{}:{subject_id}", self.period_index),
                "reserve_shortfall_cost",
                objective_subject_total(
                    &self.objective_terms,
                    ObjectiveSubjectKind::ReserveRequirement,
                    &subject_id,
                ),
                reserve.shortfall_cost,
            );
        }
        mismatches
    }

    /// Whether this period's exact objective ledger reconciles cleanly.
    pub fn objective_ledger_is_consistent(&self) -> bool {
        self.objective_ledger_mismatches().is_empty()
    }

    pub fn emissions_results(&self) -> Option<&EmissionsPeriodResult> {
        self.emissions_results.as_ref()
    }

    pub fn frequency_results(&self) -> Option<&FrequencyPeriodResult> {
        self.frequency_results.as_ref()
    }
}

impl DispatchSolution {
    pub(crate) fn new(
        study: DispatchStudy,
        resources: Vec<DispatchResource>,
        buses: Vec<DispatchBus>,
        summary: DispatchSummary,
        diagnostics: DispatchDiagnostics,
        periods: Vec<DispatchPeriodResult>,
        resource_summaries: Vec<ResourceHorizonResult>,
        combined_cycle_results: Vec<CombinedCyclePlantResult>,
    ) -> Self {
        let mut solution = Self {
            study,
            resources,
            buses,
            summary,
            diagnostics,
            periods,
            resource_summaries,
            combined_cycle_results,
            model_diagnostics: Vec::new(),
            penalty_summary: PenaltySummary::default(),
            audit: SolutionAuditReport::default(),
            resource_index_by_id: OnceLock::new(),
            bus_index_by_number: OnceLock::new(),
            period_index_by_number: OnceLock::new(),
            resource_summary_index_by_id: OnceLock::new(),
            combined_cycle_index_by_id: OnceLock::new(),
        };
        solution.refresh_audit();
        solution
    }

    fn resource_index_by_id(&self) -> &HashMap<String, usize> {
        self.resource_index_by_id.get_or_init(|| {
            self.resources
                .iter()
                .enumerate()
                .map(|(index, resource)| (resource.resource_id.clone(), index))
                .collect()
        })
    }

    fn bus_index_by_number(&self) -> &HashMap<u32, usize> {
        self.bus_index_by_number.get_or_init(|| {
            self.buses
                .iter()
                .enumerate()
                .map(|(index, bus)| (bus.bus_number, index))
                .collect()
        })
    }

    fn period_index_by_number(&self) -> &HashMap<usize, usize> {
        self.period_index_by_number.get_or_init(|| {
            self.periods
                .iter()
                .enumerate()
                .map(|(index, period)| (period.period_index, index))
                .collect()
        })
    }

    fn resource_summary_index_by_id(&self) -> &HashMap<String, usize> {
        self.resource_summary_index_by_id.get_or_init(|| {
            self.resource_summaries
                .iter()
                .enumerate()
                .map(|(index, summary)| (summary.resource_id.clone(), index))
                .collect()
        })
    }

    fn combined_cycle_index_by_id(&self) -> &HashMap<String, usize> {
        self.combined_cycle_index_by_id.get_or_init(|| {
            self.combined_cycle_results
                .iter()
                .enumerate()
                .map(|(index, result)| (result.plant_id.clone(), index))
                .collect()
        })
    }

    pub fn resource(&self, resource_id: &str) -> Option<&DispatchResource> {
        self.resource_index_by_id()
            .get(resource_id)
            .and_then(|&index| self.resources.get(index))
    }

    pub fn study(&self) -> &DispatchStudy {
        &self.study
    }

    /// Attach or replace workflow provenance for this solved dispatch payload.
    pub fn set_stage_metadata(&mut self, metadata: DispatchStageMetadata) {
        self.study.stage = Some(metadata);
    }

    /// Builder-style variant of [`DispatchSolution::set_stage_metadata`].
    pub fn with_stage_metadata(mut self, metadata: DispatchStageMetadata) -> Self {
        self.set_stage_metadata(metadata);
        self
    }

    pub fn resources(&self) -> &[DispatchResource] {
        &self.resources
    }

    pub fn buses(&self) -> &[DispatchBus] {
        &self.buses
    }

    pub fn summary(&self) -> &DispatchSummary {
        &self.summary
    }

    pub fn diagnostics(&self) -> &DispatchDiagnostics {
        &self.diagnostics
    }

    /// Mutable access to the diagnostics payload. Used by the dispatch
    /// wrappers to patch in pipeline-level phase timings that aren't
    /// known at solution-construction time (e.g. `prepare_request_secs`,
    /// `emit_keyed_secs`).
    pub(crate) fn diagnostics_mut(&mut self) -> &mut DispatchDiagnostics {
        &mut self.diagnostics
    }

    pub fn penalty_summary(&self) -> &PenaltySummary {
        &self.penalty_summary
    }

    pub fn audit(&self) -> &SolutionAuditReport {
        &self.audit
    }

    pub fn objective_terms(&self) -> &[ObjectiveTerm] {
        &self.summary.objective_terms
    }

    pub fn refresh_audit(&mut self) {
        self.audit = <Self as AuditableSolution>::computed_solution_audit(self);
    }

    /// Return every objective-ledger mismatch found in this keyed dispatch result.
    pub fn objective_ledger_mismatches(&self) -> Vec<ObjectiveLedgerMismatch> {
        let mut mismatches = Vec::new();
        let summary_terms = &self.summary.objective_terms;
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_cost",
            objective_term_total(summary_terms),
            self.summary.total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "residual",
            0.0,
            residual_term_total(summary_terms),
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "period_total_cost_sum",
            self.periods
                .iter()
                .map(DispatchPeriodResult::total_cost)
                .sum(),
            self.summary.total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_energy_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::Energy),
            self.summary.total_energy_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_reserve_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::Reserve),
            self.summary.total_reserve_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_no_load_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::NoLoad),
            self.summary.total_no_load_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_startup_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::Startup),
            self.summary.total_startup_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_shutdown_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::Shutdown),
            self.summary.total_shutdown_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_tracking_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::Tracking),
            self.summary.total_tracking_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_adder_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::Adder),
            self.summary.total_adder_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_other_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::Other),
            self.summary.total_other_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "summary",
            "total_penalty_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::Penalty),
            self.summary.total_penalty_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "total_penalty_cost",
            objective_bucket_total(summary_terms, ObjectiveBucket::Penalty),
            self.penalty_summary.total_penalty_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "power_balance_p_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::PowerBalancePenalty),
            self.penalty_summary.power_balance_p_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "power_balance_q_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::ReactiveBalancePenalty),
            self.penalty_summary.power_balance_q_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "voltage_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::VoltagePenalty),
            self.penalty_summary.voltage_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "angle_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::AngleDifferencePenalty),
            self.penalty_summary.angle_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "thermal_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::ThermalLimitPenalty),
            self.penalty_summary.thermal_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "flowgate_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::FlowgatePenalty)
                + objective_kind_total(summary_terms, ObjectiveTermKind::InterfacePenalty),
            self.penalty_summary.flowgate_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "ramp_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::RampPenalty),
            self.penalty_summary.ramp_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "reserve_shortfall_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::ReserveShortfall)
                + objective_kind_total(summary_terms, ObjectiveTermKind::ReactiveReserveShortfall),
            self.penalty_summary.reserve_shortfall_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "headroom_footroom_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::CommitmentCapacityPenalty),
            self.penalty_summary.headroom_footroom_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "energy_window_total_cost",
            objective_kind_total(summary_terms, ObjectiveTermKind::EnergyWindowPenalty),
            self.penalty_summary.energy_window_total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "power_balance_p_total_mw",
            objective_kind_quantity_total(summary_terms, ObjectiveTermKind::PowerBalancePenalty),
            self.penalty_summary.power_balance_p_total_mw,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "power_balance_q_total_mvar",
            objective_kind_quantity_total(summary_terms, ObjectiveTermKind::ReactiveBalancePenalty),
            self.penalty_summary.power_balance_q_total_mvar,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "voltage_total_pu",
            objective_kind_quantity_total(summary_terms, ObjectiveTermKind::VoltagePenalty),
            self.penalty_summary.voltage_total_pu,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "angle_total_rad",
            objective_kind_quantity_total(summary_terms, ObjectiveTermKind::AngleDifferencePenalty),
            self.penalty_summary.angle_total_rad,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "thermal_total_mw",
            objective_kind_quantity_total(summary_terms, ObjectiveTermKind::ThermalLimitPenalty),
            self.penalty_summary.thermal_total_mw,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "flowgate_total_mw",
            objective_kind_quantity_total(summary_terms, ObjectiveTermKind::FlowgatePenalty)
                + objective_kind_quantity_total(summary_terms, ObjectiveTermKind::InterfacePenalty),
            self.penalty_summary.flowgate_total_mw,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "ramp_total_mw",
            objective_kind_quantity_total(summary_terms, ObjectiveTermKind::RampPenalty),
            self.penalty_summary.ramp_total_mw,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "reserve_shortfall_total_mw",
            objective_kind_quantity_total(summary_terms, ObjectiveTermKind::ReserveShortfall)
                + objective_kind_quantity_total(
                    summary_terms,
                    ObjectiveTermKind::ReactiveReserveShortfall,
                ),
            self.penalty_summary.reserve_shortfall_total_mw,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::DispatchSolution,
            "penalty_summary",
            "headroom_footroom_total_mw",
            objective_kind_quantity_total(
                summary_terms,
                ObjectiveTermKind::CommitmentCapacityPenalty,
            ),
            self.penalty_summary.headroom_footroom_total_mw,
        );

        for period in &self.periods {
            mismatches.extend(period.objective_ledger_mismatches());
        }
        for summary in &self.resource_summaries {
            validate_resource_summary_objective_ledger(summary, &mut mismatches);
        }
        for plant in &self.combined_cycle_results {
            maybe_push_objective_ledger_mismatch(
                &mut mismatches,
                ObjectiveLedgerScopeKind::CombinedCyclePlant,
                plant.plant_id.clone(),
                "objective_cost",
                objective_term_total(&plant.objective_terms),
                plant.objective_cost,
            );
            maybe_push_objective_ledger_mismatch(
                &mut mismatches,
                ObjectiveLedgerScopeKind::CombinedCyclePlant,
                plant.plant_id.clone(),
                "transition_cost",
                objective_kind_total(
                    &plant.objective_terms,
                    ObjectiveTermKind::CombinedCycleTransition,
                ),
                plant.transition_cost,
            );
        }

        mismatches
    }

    /// Whether the keyed dispatch result reconciles cleanly against its exact objective ledger.
    pub fn objective_ledger_is_consistent(&self) -> bool {
        self.objective_ledger_mismatches().is_empty()
    }

    pub fn periods(&self) -> &[DispatchPeriodResult] {
        &self.periods
    }

    pub fn resource_summary(&self, resource_id: &str) -> Option<&ResourceHorizonResult> {
        self.resource_summary_index_by_id()
            .get(resource_id)
            .and_then(|&index| self.resource_summaries.get(index))
    }

    pub fn resource_summaries(&self) -> &[ResourceHorizonResult] {
        &self.resource_summaries
    }

    pub fn bus(&self, bus_number: u32) -> Option<&DispatchBus> {
        self.bus_index_by_number()
            .get(&bus_number)
            .and_then(|&index| self.buses.get(index))
    }

    pub fn period(&self, period_index: usize) -> Option<&DispatchPeriodResult> {
        self.period_index_by_number()
            .get(&period_index)
            .and_then(|&index| self.periods.get(index))
    }

    pub fn combined_cycle_plant(&self, plant_id: &str) -> Option<&CombinedCyclePlantResult> {
        self.combined_cycle_index_by_id()
            .get(plant_id)
            .and_then(|&index| self.combined_cycle_results.get(index))
    }

    pub fn combined_cycle_results(&self) -> &[CombinedCyclePlantResult] {
        &self.combined_cycle_results
    }

    /// Model diagnostic snapshots captured during solve, one per stage.
    ///
    /// Empty unless `DispatchRuntime::capture_model_diagnostics` was enabled.
    pub fn model_diagnostics(&self) -> &[crate::model_diagnostic::ModelDiagnostic] {
        &self.model_diagnostics
    }
}

impl AuditableSolution for DispatchSolution {
    fn computed_solution_audit(&self) -> SolutionAuditReport {
        SolutionAuditReport::from_mismatches(self.objective_ledger_mismatches())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_solution::{ObjectiveBucket, ObjectiveQuantityUnit, ObjectiveTermKind};

    #[test]
    fn dispatch_period_lookup_helpers_cover_keyed_collections() {
        let period = DispatchPeriodResult {
            period_index: 0,
            total_cost: 0.0,
            co2_t: 0.0,
            resource_results: vec![ResourcePeriodResult {
                resource_id: "gen_a".to_string(),
                kind: DispatchResourceKind::Generator,
                bus_number: None,
                power_mw: 0.0,
                objective_cost: 0.0,
                energy_cost: None,
                no_load_cost: None,
                startup_cost: None,
                shutdown_cost: None,
                reserve_awards: HashMap::new(),
                reserve_costs: HashMap::new(),
                objective_terms: Vec::new(),
                co2_t: None,
                detail: ResourcePeriodDetail::Generator(GeneratorPeriodDetail::default()),
            }],
            bus_results: vec![BusPeriodResult {
                bus_number: 101,
                lmp: 0.0,
                mec: 0.0,
                mcc: 0.0,
                mlc: 0.0,
                angle_rad: None,
                voltage_pu: None,
                net_injection_mw: 0.0,
                withdrawals_mw: 0.0,
                loss_allocation_mw: 0.0,
                net_reactive_injection_mvar: None,
                withdrawals_mvar: None,
                p_slack_pos_mw: None,
                p_slack_neg_mw: None,
                q_slack_pos_mvar: None,
                q_slack_neg_mvar: None,
            }],
            reserve_results: vec![ReservePeriodResult {
                product_id: "spin".to_string(),
                scope: ReserveScope::Zone,
                zone_id: Some(ZoneId::from(3_usize)),
                requirement_mw: 0.0,
                provided_mw: 0.0,
                shortfall_mw: 0.0,
                clearing_price: 0.0,
                shortfall_cost: 0.0,
            }],
            constraint_results: vec![ConstraintPeriodResult {
                constraint_id: "branch:1".to_string(),
                kind: ConstraintKind::Other,
                scope: ConstraintScope::Other,
                shadow_price: None,
                slack_mw: None,
                penalty_cost: None,
                penalty_dollars: None,
            }],
            hvdc_results: vec![HvdcPeriodResult {
                link_id: "hvdc_a".to_string(),
                name: String::new(),
                mw: 0.0,
                delivered_mw: 0.0,
                band_results: Vec::new(),
            }],
            tap_dispatch: Vec::new(),
            phase_dispatch: Vec::new(),
            switched_shunt_dispatch: Vec::new(),
            branch_commitment_state: Vec::new(),
            virtual_bid_results: Vec::new(),
            par_results: Vec::new(),
            power_balance_violation: PowerBalanceViolation::default(),
            objective_terms: Vec::new(),
            emissions_results: None,
            frequency_results: None,
            sced_ac_benders_eta_dollars_per_hour: None,
            resource_index_by_id: OnceLock::new(),
            bus_index_by_number: OnceLock::new(),
            hvdc_index_by_id: OnceLock::new(),
            reserve_index_by_key: OnceLock::new(),
            constraint_index_by_id: OnceLock::new(),
        };

        assert!(period.resource("gen_a").is_some());
        assert!(period.bus(101).is_some());
        assert!(
            period
                .reserve("spin", Some(ZoneId::from(3_usize)))
                .is_some()
        );
        assert!(period.constraint("branch:1").is_some());
        assert!(period.hvdc("hvdc_a").is_some());
    }

    #[test]
    fn objective_ledger_validation_detects_summary_mismatches() {
        let energy_term = ObjectiveTerm {
            component_id: "energy".to_string(),
            bucket: ObjectiveBucket::Energy,
            kind: ObjectiveTermKind::GeneratorEnergy,
            subject_kind: ObjectiveSubjectKind::Resource,
            subject_id: "gen_a".to_string(),
            dollars: 100.0,
            quantity: Some(10.0),
            quantity_unit: Some(ObjectiveQuantityUnit::Mwh),
            unit_rate: Some(10.0),
        };
        let period = DispatchPeriodResult {
            period_index: 0,
            total_cost: 100.0,
            co2_t: 0.0,
            resource_results: vec![ResourcePeriodResult {
                resource_id: "gen_a".to_string(),
                kind: DispatchResourceKind::Generator,
                bus_number: Some(1),
                power_mw: 10.0,
                objective_cost: 100.0,
                energy_cost: Some(100.0),
                no_load_cost: None,
                startup_cost: None,
                shutdown_cost: None,
                reserve_awards: HashMap::new(),
                reserve_costs: HashMap::new(),
                objective_terms: vec![energy_term.clone()],
                co2_t: None,
                detail: ResourcePeriodDetail::Generator(GeneratorPeriodDetail::default()),
            }],
            bus_results: Vec::new(),
            reserve_results: Vec::new(),
            constraint_results: Vec::new(),
            hvdc_results: Vec::new(),
            tap_dispatch: Vec::new(),
            phase_dispatch: Vec::new(),
            switched_shunt_dispatch: Vec::new(),
            branch_commitment_state: Vec::new(),
            virtual_bid_results: Vec::new(),
            par_results: Vec::new(),
            power_balance_violation: PowerBalanceViolation::default(),
            objective_terms: vec![energy_term.clone()],
            emissions_results: None,
            frequency_results: None,
            sced_ac_benders_eta_dollars_per_hour: None,
            resource_index_by_id: OnceLock::new(),
            bus_index_by_number: OnceLock::new(),
            hvdc_index_by_id: OnceLock::new(),
            reserve_index_by_key: OnceLock::new(),
            constraint_index_by_id: OnceLock::new(),
        };
        let mut solution = DispatchSolution::new(
            DispatchStudy::default(),
            Vec::new(),
            Vec::new(),
            DispatchSummary {
                total_cost: 100.0,
                total_energy_cost: 100.0,
                objective_terms: vec![energy_term.clone()],
                ..DispatchSummary::default()
            },
            DispatchDiagnostics::default(),
            vec![period],
            vec![ResourceHorizonResult {
                resource_id: "gen_a".to_string(),
                kind: DispatchResourceKind::Generator,
                objective_cost: 100.0,
                total_energy_cost: 100.0,
                total_no_load_cost: 0.0,
                total_startup_cost: 0.0,
                total_shutdown_cost: 0.0,
                total_reserve_cost: 0.0,
                total_co2_t: 0.0,
                objective_terms: vec![energy_term.clone()],
                co2_shadow_price_per_mwh: None,
                commitment_schedule: None,
                startup_schedule: None,
                shutdown_schedule: None,
                regulation_schedule: None,
                storage_soc_mwh: None,
            }],
            Vec::new(),
        );
        solution.penalty_summary = PenaltySummary::default();
        assert!(solution.objective_ledger_is_consistent());

        solution.summary.total_energy_cost = 90.0;
        let mismatches = solution.objective_ledger_mismatches();
        assert_eq!(mismatches.len(), 1);
        assert_eq!(
            mismatches[0].scope_kind,
            ObjectiveLedgerScopeKind::DispatchSolution
        );
        assert_eq!(mismatches[0].field, "total_energy_cost");
    }
}
