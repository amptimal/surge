// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Network-facing request configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use surge_network::market::PenaltyCurve;
use surge_solution::ParSetpoint;

use crate::hvdc::HvdcDispatchLink;

/// Stable branch selector.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BranchRef {
    pub from_bus: u32,
    #[serde(default = "default_branch_circuit")]
    pub circuit: String,
    pub to_bus: u32,
}

/// Stable HVDC selector.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HvdcLinkRef {
    pub link_id: String,
}

fn default_branch_circuit() -> String {
    "1".to_string()
}

fn default_branch_switching_big_m_factor() -> f64 {
    10.0
}

/// How N-1 contingencies are embedded into DC time-coupled dispatch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecurityEmbedding {
    /// Build the full contingency constraint set directly into the SCUC.
    #[default]
    ExplicitContingencies,
    /// Solve a base model, screen post-contingency violations, and add cuts iteratively.
    IterativeScreening,
}

/// Method for ranking contingency pairs when pre-seeding iter 0 of
/// iterative-screening SCUC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecurityPreseedMethod {
    /// No pre-seeding; iter 0 solves base SCUC with zero contingency cuts.
    #[default]
    None,
    /// Rank (contingency, monitored) pairs by
    /// `|LODF| * rating_ctg / rating_mon` — a dimensionless, dispatch-free
    /// structural severity score. Cheapest option; computed from the
    /// already-cached PTDF.
    MaxLodfTopology,
}

/// Strategy for selecting iterative security cuts after each screening pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecurityCutStrategy {
    /// Preserve the historical behavior: add up to
    /// `max_cuts_per_iteration` worst violations every round.
    Fixed,
    /// Use large batches while the screen is violation-heavy, then switch
    /// to a smaller last-mile batch once the remaining set is modest.
    #[default]
    Adaptive,
}

/// Optional N-1 security policy for DC time-coupled dispatch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct SecurityPolicy {
    /// How contingencies enter the optimization model.
    pub embedding: SecurityEmbedding,
    /// Maximum outer-loop iterations.
    pub max_iterations: usize,
    /// Post-contingency flow violation tolerance in p.u. on system base.
    pub violation_tolerance_pu: f64,
    /// Maximum number of new cuts added per iteration.
    pub max_cuts_per_iteration: usize,
    /// Branches to consider as contingencies. Empty = all monitored branches.
    pub branch_contingencies: Vec<BranchRef>,
    /// HVDC links to consider as contingencies.
    pub hvdc_contingencies: Vec<HvdcLinkRef>,
    /// Pre-seed iter 0 of `IterativeScreening` with this many top-ranked
    /// (contingency, monitored) cuts per period. `0` disables pre-seeding
    /// (default — preserves prior behavior). The goal is to reduce outer
    /// iteration count by starting with a skeleton of the most structurally
    /// binding N-1 pairs, avoiding at least one full SCUC re-solve.
    pub preseed_count_per_period: usize,
    /// Ranking method used to pick the top-N pairs when pre-seeding.
    pub preseed_method: SecurityPreseedMethod,
    /// Cut-selection strategy used by the iterative screener.
    pub cut_strategy: SecurityCutStrategy,
    /// Optional cap on active iterative cuts retained in the model. When
    /// set, stale active cuts are retired first after a round adds new
    /// cuts; retired pairs may be rediscovered by later screens if they
    /// become violated again.
    pub max_active_cuts: Option<usize>,
    /// Optional activity-aging threshold. Active cuts whose slack and shadow
    /// price remain near zero for this many solved rounds are retired even
    /// before `max_active_cuts` is reached.
    pub cut_retire_after_rounds: Option<usize>,
    /// Violation-count threshold below which `Adaptive` switches to the
    /// smaller targeted batch cap.
    pub targeted_cut_threshold: usize,
    /// Last-mile per-round cap used by `Adaptive` once the remaining
    /// violation count is at or below `targeted_cut_threshold`.
    pub targeted_cut_cap: usize,
    /// Emit the final near-binding contingency report. This is an
    /// informational diagnostic only; disabling it does not change the
    /// security screen, added cuts, validation-relevant solution, or
    /// aggregate security metadata.
    pub near_binding_report: bool,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            embedding: SecurityEmbedding::default(),
            max_iterations: 10,
            violation_tolerance_pu: 0.01,
            max_cuts_per_iteration: 50,
            branch_contingencies: Vec::new(),
            hvdc_contingencies: Vec::new(),
            preseed_count_per_period: 0,
            preseed_method: SecurityPreseedMethod::None,
            cut_strategy: SecurityCutStrategy::Fixed,
            max_active_cuts: None,
            cut_retire_after_rounds: None,
            targeted_cut_threshold: 50_000,
            targeted_cut_cap: 50_000,
            near_binding_report: false,
        }
    }
}

/// Thermal-limit enforcement policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct ThermalLimitPolicy {
    pub enforce: bool,
    pub min_rate_a: f64,
}

impl Default for ThermalLimitPolicy {
    fn default() -> Self {
        Self {
            enforce: true,
            min_rate_a: 1.0,
        }
    }
}

/// Flowgate/interface enforcement policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct FlowgatePolicy {
    pub enabled: bool,
    pub max_nomogram_iterations: usize,
}

impl Default for FlowgatePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_nomogram_iterations: 10,
        }
    }
}

/// Iterative DC loss-factor policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct LossFactorPolicy {
    pub enabled: bool,
    pub max_iterations: usize,
    pub tolerance: f64,
    /// Cold-start strategy the SCUC security loop uses to seed the
    /// first iteration's loss-factor state. Subsequent iterations
    /// always warm-start from the prior iteration's converged
    /// `dloss_dp`, so this only affects the very first MIP solve. See
    /// [`LossFactorWarmStartMode`] for the supported strategies.
    #[serde(default)]
    pub warm_start_mode: LossFactorWarmStartMode,
    /// How losses are represented inside SCUC across security iterations
    /// when running in `scuc_disable_bus_power_balance` (system-row) mode.
    /// See [`ScucLossTreatment`] for the three options. Default
    /// [`ScucLossTreatment::Static`] preserves prior behavior — losses
    /// are a static `rate × total_load` adjustment that does not update
    /// across iterations.
    #[serde(default)]
    pub scuc_loss_treatment: ScucLossTreatment,
}

impl Default for LossFactorPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_iterations: 3,
            tolerance: 1e-3,
            warm_start_mode: LossFactorWarmStartMode::default(),
            scuc_loss_treatment: ScucLossTreatment::default(),
        }
    }
}

/// How the SCUC system-balance row represents transmission losses across
/// security iterations.
///
/// Only consulted when `runtime.scuc_disable_bus_power_balance = true`
/// (the GO C3 default). In per-bus-balance mode the existing
/// `iterate_loss_factors` machinery handles loss representation directly
/// from the per-bus rows, and this knob is ignored.
///
/// The three modes form a complexity/accuracy ladder:
///
/// * [`Static`](ScucLossTreatment::Static) — single scalar
///   `rate × total_load` per period, baked into the system-row RHS.
///   Same value every security iteration. Cheapest; ignores the realized
///   dispatch.
/// * [`ScalarFeedback`](ScucLossTreatment::ScalarFeedback) — after each
///   security iteration's repaired DC PF, compute realized total losses
///   per period and feed that back as next iteration's RHS. Damped with
///   asymmetric bias toward higher (under-commitment costs more than
///   over-commitment because AC SCED can't commit new units).
/// * [`PenaltyFactors`](ScucLossTreatment::PenaltyFactors) — full
///   marginal-loss-factor formulation: `Σ (1 − LF_g) · pg = Σ Pd + L_0`
///   with the linearization-point correction. LFs are computed from
///   realized DC flows + the loss PTDF, gauge-fixed against
///   distributed-load slack, damped, and magnitude-capped. Most accurate;
///   captures *where* gen is preferable (a renewable at a high-loss bus
///   contributes effective MW < raw MW, so SCUC commits more thermal
///   nearby up-front).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScucLossTreatment {
    /// Static `rate × total_load` per period; never updated across
    /// security iterations. Preserves prior behavior.
    #[default]
    Static,
    /// Per-period realized total losses fed forward across security
    /// iterations with damping + asymmetric upward bias. RHS-only —
    /// LHS coefficients stay at face value.
    ScalarFeedback,
    /// Full per-bus penalty factors applied to system-row LHS
    /// coefficients (`(1 − LF_g)` on every injection), with RHS
    /// linearization correction. Reference gauge: distributed-load
    /// slack.
    PenaltyFactors,
}

/// Cold-start strategy for the SCUC loss-factor warm-start on the
/// first security iteration.
///
/// Defaults to [`LossFactorWarmStartMode::Disabled`] — first MIP is
/// solved lossless, refinement LP corrects after. Set to one of the
/// other variants to inject a loss estimate before the first MIP and
/// (when the estimate is close to the converged state) skip the
/// refinement LP re-solve entirely.
#[derive(
    Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize, JsonSchema,
)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LossFactorWarmStartMode {
    /// No cold-start warm-start on iter 0. The first MIP is solved
    /// lossless; the refinement LP corrects for losses after.
    /// Subsequent security iterations still warm-start from the prior
    /// iteration's `dloss_dp` if available.
    #[default]
    Disabled,
    /// Seed every bus's `dloss` to the same rate `rate ∈ [0, 0.5]`
    /// (typical `0.02` for 2%). `total_losses_mw = rate × total_load`.
    /// No per-bus variation; cheapest cold-start. Good when the
    /// network's losses are dominated by a roughly uniform background
    /// loss rate rather than strong per-bus asymmetries.
    Uniform { rate: f64 },
    /// Seed `dloss` from a synthetic load-pattern DC PF plus sparse
    /// adjoint loss sensitivities, normalised so total weighted losses
    /// match `rate × total_load`. Captures per-bus variation from
    /// network topology + load pattern without materialising loss PTDFs.
    LoadPattern { rate: f64 },
    /// Seed from a DC power flow on each hourly load pattern with
    /// pmax-balanced generation. Most accurate cold-start; costs one DC
    /// PF plus one adjoint solve per period. Falls back to
    /// `Uniform { rate: 0.02 }` if the DC PF fails.
    DcPf,
}

/// Forbidden-operating-zone enforcement policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct ForbiddenZonePolicy {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_transit_periods: Option<usize>,
}

/// How startup/shutdown output trajectories are modeled across intervals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentTrajectoryMode {
    /// Model transition output as same-interval online deloading.
    #[default]
    InlineDeloading,
    /// Model transition output as offline neighboring-interval trajectories.
    OfflineTrajectory,
}

/// Commitment transition modeling policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct CommitmentTransitionPolicy {
    pub shutdown_deloading: bool,
    pub trajectory_mode: CommitmentTrajectoryMode,
}

/// Whether a constraint family is enforced as hard or soft.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintEnforcement {
    #[default]
    Soft,
    Hard,
}

impl ConstraintEnforcement {
    pub fn is_hard(self) -> bool {
        matches!(self, Self::Hard)
    }
}

/// Public pumped-hydro head curve keyed by resource id.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PhHeadCurve {
    pub resource_id: String,
    pub breakpoints: Vec<(f64, f64)>,
}

/// Public pumped-hydro mode constraint keyed by resource id.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PhModeConstraint {
    pub resource_id: String,
    pub min_gen_run_periods: usize,
    pub min_pump_run_periods: usize,
    pub pump_to_gen_periods: usize,
    pub gen_to_pump_periods: usize,
    pub max_pump_starts: Option<u32>,
}

/// Stepped penalty curves for power balance slack variables.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct PowerBalancePenalty {
    /// Penalty segments for unserved load (generation shortfall).
    pub curtailment: Vec<(f64, f64)>,
    /// Penalty segments for excess generation (over-supply).
    pub excess: Vec<(f64, f64)>,
}

impl Default for PowerBalancePenalty {
    fn default() -> Self {
        Self {
            curtailment: vec![(f64::MAX, 1e7)],
            excess: vec![(f64::MAX, 1e5)],
        }
    }
}

impl PowerBalancePenalty {
    fn segments_from_curve(curve: &PenaltyCurve) -> Vec<(f64, f64)> {
        match curve {
            PenaltyCurve::Linear { cost_per_unit } => vec![(f64::MAX, *cost_per_unit)],
            PenaltyCurve::PiecewiseLinear { segments } => {
                let mut out = Vec::with_capacity(segments.len().max(1));
                let mut prev_max = 0.0_f64;
                for seg in segments {
                    let width = if seg.max_violation.is_infinite() {
                        f64::MAX
                    } else {
                        (seg.max_violation - prev_max).max(0.0)
                    };
                    prev_max = seg.max_violation;
                    if width <= 0.0 {
                        continue;
                    }
                    out.push((width, seg.cost_per_unit));
                }
                if out.is_empty() {
                    vec![(f64::MAX, curve.marginal_cost_at(0.0))]
                } else {
                    out
                }
            }
            PenaltyCurve::Quadratic { cost_coefficient } => {
                let approx_cost = (*cost_coefficient).max(0.0);
                tracing::warn!(
                    approx_cost_per_mw = approx_cost,
                    "quadratic power-balance penalty is not LP-compatible; approximating with a linear penalty"
                );
                vec![(f64::MAX, approx_cost)]
            }
        }
    }

    pub fn from_curves(curtailment: &PenaltyCurve, excess: &PenaltyCurve) -> Self {
        let curtailment_segments = Self::segments_from_curve(curtailment);
        let excess_segments = Self::segments_from_curve(excess);
        Self {
            curtailment: curtailment_segments,
            excess: excess_segments,
        }
    }
}

impl From<&PenaltyCurve> for PowerBalancePenalty {
    fn from(curve: &PenaltyCurve) -> Self {
        Self::from_curves(curve, curve)
    }
}

/// How piecewise ramp curves are applied in dispatch LP formulations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RampMode {
    /// Weighted-average rate integrated over [Pmin, Pmax].
    #[default]
    Averaged,
    /// Interpolate the ramp curve at the unit's current operating point.
    Interpolated,
    /// Incremental block decomposition.
    Block {
        /// Enable exact per-block reserve coupling.
        per_block_reserves: bool,
    },
}

/// Ramp-constraint modeling policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct RampPolicy {
    pub mode: RampMode,
    pub enforcement: ConstraintEnforcement,
}

impl Default for RampPolicy {
    fn default() -> Self {
        Self {
            mode: RampMode::default(),
            enforcement: ConstraintEnforcement::Soft,
        }
    }
}

/// Multi-interval energy-window enforcement policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct EnergyWindowPolicy {
    pub enforcement: ConstraintEnforcement,
    pub penalty_per_puh: f64,
}

impl Default for EnergyWindowPolicy {
    fn default() -> Self {
        Self {
            enforcement: ConstraintEnforcement::Soft,
            penalty_per_puh: 0.0,
        }
    }
}

/// Network topology-control policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TopologyControlMode {
    #[default]
    Fixed,
    Switchable,
}

/// Topology-control modeling policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct TopologyControlPolicy {
    pub mode: TopologyControlMode,
    #[serde(default = "default_branch_switching_big_m_factor")]
    pub branch_switching_big_m_factor: f64,
}

impl Default for TopologyControlPolicy {
    fn default() -> Self {
        Self {
            mode: TopologyControlMode::Fixed,
            branch_switching_big_m_factor: default_branch_switching_big_m_factor(),
        }
    }
}

/// Network-facing study policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct DispatchNetwork {
    pub thermal_limits: ThermalLimitPolicy,
    pub flowgates: FlowgatePolicy,
    /// External `surge_solution::ParSetpoint`; treated as opaque JSON.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub par_setpoints: Vec<ParSetpoint>,
    pub hvdc_links: Vec<HvdcDispatchLink>,
    pub loss_factors: LossFactorPolicy,
    pub forbidden_zones: ForbiddenZonePolicy,
    pub commitment_transitions: CommitmentTransitionPolicy,
    pub ramping: RampPolicy,
    pub energy_windows: EnergyWindowPolicy,
    pub topology_control: TopologyControlPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityPolicy>,
    pub ph_head_curves: Vec<PhHeadCurve>,
    pub ph_mode_constraints: Vec<PhModeConstraint>,
}
