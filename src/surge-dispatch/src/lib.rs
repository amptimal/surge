// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge Dispatch — unified economic dispatch and unit commitment.
//!
//! # Canonical API
//!
//! The primary workflow is:
//!
//! 1. prepare a [`DispatchModel`] from a raw [`surge_network::Network`]
//! 2. build a typed [`DispatchRequest`]
//! 3. optionally preflight it with [`DispatchModel::prepare_request`]
//! 4. call [`solve_dispatch`]
//!
//! The request model exposes three orthogonal choices:
//!
//! - formulation: DC or AC
//! - coupling: period-by-period or time-coupled
//! - commitment policy: all committed, fixed, optimize, or additional
//!
//! That means the public API describes studies in terms of explicit study axes
//! rather than workflow nicknames. For example:
//!
//! - DC period-by-period dispatch: `Dc + PeriodByPeriod + AllCommitted`
//! - DC time-coupled dispatch: `Dc + TimeCoupled + Fixed`
//! - DC commitment optimization: `Dc + TimeCoupled + Optimize`
//! - reliability-only additional commitment: `Dc + TimeCoupled + Additional`
//! - AC period-by-period dispatch: `Ac + PeriodByPeriod + AllCommitted`
//! - optional N-1 screening on DC time-coupled studies via [`SecurityScreening`]
//!
//! All production dispatch results land on [`DispatchSolution`] with per-period
//! detail in [`DispatchPeriodResult`].
//!
//! # Example
//!
//! ```ignore
//! use surge_dispatch::{
//!     DispatchRequest, DispatchTimeline, solve_dispatch,
//! };
//!
//! let model = surge_dispatch::DispatchModel::prepare(&network)?;
//! let request = DispatchRequest::builder()
//!     .dc()
//!     .time_coupled()
//!     .all_committed()
//!     .timeline(DispatchTimeline::hourly(24))
//!     .build();
//!
//! model.validate_request(&request)?;
//! let result = model.solve(&request)?;
//! println!("periods = {}", result.study().periods);
//! println!("first bus lmp = {}", result.periods()[0].bus_results()[0].lmp);
//! # Ok::<(), surge_dispatch::DispatchError>(())
//! ```
//!
//! Internally the crate still has SCED- and SCUC-specific engines, but they are
//! implementation details behind the unified request/result surface.

pub mod blocks;
pub(crate) mod config;
pub mod datasets;
pub(crate) mod economics;
pub(crate) mod error;
pub mod hvdc;
pub mod ids;
#[cfg(test)]
pub(crate) mod legacy;
pub mod model;
pub mod model_diagnostic;
pub mod powerflow;
pub mod request;
mod result;
pub mod scrd;
pub(crate) mod solution;
pub mod violations;

/// Serde skip helper for zero-valued f64 fields.
pub(crate) fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

// Internal modules — accessed only through solve_dispatch routing
pub(crate) mod common;
pub(crate) mod dispatch;
pub(crate) mod report_ids;
pub(crate) mod sced;
pub(crate) mod scuc;

// ── Primary API (unified dispatch) ──────────────────────
pub use dispatch::{solve_dispatch, solve_dispatch_with_options};
pub use model::DispatchModel;
pub use request::{
    AcBusLoadProfile, AcBusLoadProfiles, BranchDerateProfile, BranchDerateProfiles, BranchRef,
    BusAreaAssignment, BusLoadProfile, BusLoadProfiles, CombinedCycleConfigOfferSchedule,
    CommitmentConstraint, CommitmentInitialCondition, CommitmentOptions, CommitmentPolicy,
    CommitmentPolicyKind, CommitmentSchedule, CommitmentTerm, CommitmentTrajectoryMode,
    CommitmentTransitionPolicy, ConstraintEnforcement, DispatchInitialState, DispatchMarket,
    DispatchNetwork, DispatchProfiles, DispatchRequest, DispatchRequestBuilder, DispatchRuntime,
    DispatchSolveOptions, DispatchState, DispatchTimeline, DispatchableLoadOfferSchedule,
    DispatchableLoadReserveOfferSchedule, EmissionProfile, EnergyWindowPolicy, FlowgatePolicy,
    ForbiddenZonePolicy, Formulation, GeneratorCostModeling, GeneratorDerateProfile,
    GeneratorDerateProfiles, GeneratorOfferSchedule, GeneratorReserveOfferSchedule,
    HvdcDerateProfile, HvdcDerateProfiles, HvdcDispatchPoint, HvdcLinkRef, IntervalCoupling,
    LossFactorPolicy, MustRunUnits, PhHeadCurve, PhModeConstraint, PowerBalancePenalty,
    PreparedDispatchRequest, RampMode, RampPolicy, RenewableProfile, RenewableProfiles,
    ReserveOfferSchedule, ResourceAreaAssignment, ResourceCommitmentSchedule,
    ResourceDispatchPoint, ResourceEligibility, ResourceEmissionRate, ResourceEnergyWindowLimit,
    ResourcePeriodCommitment, ResourceStartupWindowLimit, ScedAcBendersCut, ScedAcBendersRunParams,
    ScedAcBendersRuntime, SecurityCutStrategy, SecurityEmbedding, SecurityPolicy,
    SecurityPreseedMethod, StoragePowerSchedule, StorageReserveSocImpact, StorageSocOverride,
    ThermalLimitPolicy, TopologyControlMode, TopologyControlPolicy,
};

pub(crate) mod sced_ac_benders;
pub use sced_ac_benders::{
    BendersCutRecord, BendersDiagnostics, BendersIterationRecord, BendersTerminalReason,
};

pub use model_diagnostic::{
    ActiveBound, ActivePenalty, BindingConstraint, BoundSide, ConstraintFamily,
    ConstraintFamilyStats, DiagnosticStage, ModelDiagnostic, ModelStats, SolverStats,
};

// ── Shared configuration types ─────────────────────────
pub use config::emissions::{CarbonPrice, TieLineLimits};
pub use config::frequency::FrequencySecurityOptions;
pub use error::DispatchError;
pub use hvdc::{HvdcBand, HvdcDispatchLink};
pub use ids::{AreaId, ZoneId};
pub use result::{
    BusPeriodResult, CombinedCyclePlantResult, CommitmentSource, ConstraintKind,
    ConstraintPeriodResult, ConstraintScope, DispatchBus, DispatchDiagnostics,
    DispatchPeriodResult, DispatchResource, DispatchResourceKind, DispatchSolution,
    DispatchStageMetadata, DispatchStageRole, DispatchStudy, DispatchSummary,
    DispatchableLoadPeriodDetail, EmissionsPeriodResult, FrequencyPeriodResult,
    GeneratorPeriodDetail, HvdcBandPeriodResult, HvdcPeriodResult, PenaltySummary,
    ReservePeriodResult, ReserveScope, ResourceHorizonResult, ResourcePeriodDetail,
    ResourcePeriodResult, SecurityDispatchMetadata, SecurityIterationReport, SecuritySetupTimings,
    StoragePeriodDetail,
};
pub use surge_solution::{
    ObjectiveBucket, ObjectiveLedgerMismatch, ObjectiveLedgerScopeKind, ObjectiveQuantityUnit,
    ObjectiveSubjectKind, ObjectiveTerm, ObjectiveTermKind, SolutionAuditReport,
};
pub use violations::{
    BranchThermalViolation, BusBalanceViolation, PeriodViolations, ViolationAssessment,
    ViolationCosts, assess_dispatch_violations,
};
