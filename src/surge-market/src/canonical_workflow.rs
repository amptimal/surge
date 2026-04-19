// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical two-stage market workflow builder.
//!
//! The standard day-ahead market clears in two sequential stages:
//!
//! 1. **Unit commitment** — DC / time-coupled / optimize-commitment
//!    MILP that commits units, sets startup/shutdown schedules, and
//!    produces an initial energy / reserve dispatch.
//! 2. **Economic dispatch / AC redispatch** — AC OPF with fixed
//!    commitment from stage 1; smooths the dispatch against the full
//!    AC network with explicit bus balance, thermal limits, and
//!    reactive constraints.
//!
//! This module assembles that canonical two-stage [`MarketWorkflow`]
//! directly from a prepared [`DispatchModel`] and a shared
//! [`DispatchRequest`]. It is the canonical "solve unit commitment,
//! then AC-redispatch against it" recipe in one call.

use surge_dispatch::{
    CommitmentPolicy, CommitmentSchedule, DispatchModel, DispatchRequest, DispatchSolveOptions,
    Formulation, IntervalCoupling, ResourceCommitmentSchedule,
};

use crate::workflow::{DispatchPinningConfig, MarketStage, MarketStageRole, MarketWorkflow};

/// IDs assigned to the canonical stages so callers can look them up
/// in [`MarketWorkflowResult::stage`].
pub const CANONICAL_UC_STAGE_ID: &str = "scuc";
pub const CANONICAL_ED_STAGE_ID: &str = "sced";

/// Options accepted by [`canonical_two_stage_workflow`].
#[derive(Clone)]
pub struct CanonicalWorkflowOptions {
    /// Solve-time options forwarded to stage 1 (unit commitment).
    pub uc_options: DispatchSolveOptions,
    /// Solve-time options forwarded to stage 2 (economic dispatch).
    pub ed_options: DispatchSolveOptions,
    /// Fractional P-band around each stage-1 dispatch target used to
    /// pin stage-2 generator bounds (default `0.05` = ±5 %).
    pub ed_band_fraction: f64,
    /// Minimum absolute band width in MW (default `1.0`).
    pub ed_band_floor_mw: f64,
    /// Maximum absolute band width in MW (default `1e9` — effectively
    /// unlimited; the pin still never widens an existing envelope).
    pub ed_band_cap_mw: f64,
}

impl Default for CanonicalWorkflowOptions {
    fn default() -> Self {
        Self {
            uc_options: DispatchSolveOptions::default(),
            ed_options: DispatchSolveOptions::default(),
            ed_band_fraction: 0.05,
            ed_band_floor_mw: 1.0,
            ed_band_cap_mw: 1.0e9,
        }
    }
}

/// Build the canonical two-stage market workflow from two pre-built
/// requests.
///
/// * `uc_request` is the DC SCUC request (formulation Dc, commitment
///   Optimize or Additional).
/// * `ed_request` is the AC SCED request (formulation Ac, commitment
///   will be overridden with stage 1's result at solve time).
/// * `model` is the prepared [`DispatchModel`] shared across both
///   stages.
///
/// At solve time, the workflow executor extracts the commitment
/// schedule from stage 1's solved result and substitutes it into
/// stage 2's request as a `CommitmentPolicy::Fixed` schedule before
/// solving stage 2.
pub fn canonical_two_stage_workflow(
    model: DispatchModel,
    uc_request: DispatchRequest,
    ed_request: DispatchRequest,
    options: CanonicalWorkflowOptions,
) -> MarketWorkflow {
    let uc_stage = MarketStage::new(
        CANONICAL_UC_STAGE_ID,
        MarketStageRole::UnitCommitment,
        model.clone(),
        uc_request,
    )
    .with_options(options.uc_options);

    // Stage 2: commitment placeholder is overwritten by the executor.
    let mut ed_request = ed_request;
    if !matches!(ed_request.commitment(), CommitmentPolicy::Fixed(_)) {
        ed_request.set_commitment(CommitmentPolicy::Fixed(CommitmentSchedule {
            resources: Vec::new(),
        }));
    }
    let ed_pinning = DispatchPinningConfig {
        source_stage_id: CANONICAL_UC_STAGE_ID.to_string(),
        band_fraction: options.ed_band_fraction,
        band_floor_mw: options.ed_band_floor_mw,
        band_cap_mw: options.ed_band_cap_mw,
        anchor_resource_ids: std::collections::HashSet::new(),
    };
    let ed_stage = MarketStage::new(
        CANONICAL_ED_STAGE_ID,
        MarketStageRole::EconomicDispatch,
        model,
        ed_request,
    )
    .derived_from(CANONICAL_UC_STAGE_ID)
    .commitment_from(CANONICAL_UC_STAGE_ID)
    .pin_dispatch_from(ed_pinning)
    .with_options(options.ed_options);

    MarketWorkflow::new(vec![uc_stage, ed_stage])
}

/// Build the canonical two-stage workflow from a single base request
/// by forcing stage-1 into DC + optimize and stage-2 into
/// Ac + PeriodByPeriod + Fixed. Adapters that build one request and
/// want the standard formulation axes can use this convenience; those
/// that maintain two fully-separate requests (DC with bus-load
/// profiles, AC with ac_bus_load profiles) should call
/// [`canonical_two_stage_workflow`] directly.
pub fn canonical_two_stage_workflow_from_base(
    model: DispatchModel,
    base_request: DispatchRequest,
    options: CanonicalWorkflowOptions,
) -> MarketWorkflow {
    let mut uc_request = base_request.clone();
    uc_request.set_formulation(Formulation::Dc);
    let mut ed_request = base_request;
    ed_request.set_formulation(Formulation::Ac);
    ed_request.set_coupling(IntervalCoupling::PeriodByPeriod);
    canonical_two_stage_workflow(model, uc_request, ed_request, options)
}

/// Fill stage 2's `Fixed` commitment schedule with the commitment
/// extracted from a solved stage-1 result.
///
/// Runtime helper used by the workflow executor after stage 1 returns.
pub fn apply_commitment_from_stage_one(
    stage_two_request: &mut DispatchRequest,
    stage_one_resources: Vec<ResourceCommitmentSchedule>,
) {
    stage_two_request.set_commitment(CommitmentPolicy::Fixed(CommitmentSchedule {
        resources: stage_one_resources,
    }));
}
