// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical DC SCUC + AC SCED two-stage workflow builder.
//!
//! Extends the bare [`crate::canonical_workflow`] two-stage helper
//! with the full AC-SCED machinery: AC OPF presets, reactive-reserve
//! market filter, bandable-subset dispatch pinning, commitment
//! augmentation, warm start, retry policy, feedback providers,
//! SCUC→SCED branch-thermal relaxation.
//!
//! Adapters (GO C3 today; future: ERCOT, MISO, research variants)
//! prepare all the canonical primitives — networks, requests,
//! AC-OPF options, [`AcScedSetup`], [`RetryPolicy`],
//! [`DispatchPinningConfig`] — and hand them to
//! [`build_two_stage_workflow`] which stitches the two
//! [`MarketStage`]s together in the canonical shape.
//!
//! Everything that was previously hard-coded inside GO C3's workflow
//! builder (retry policy assembly, feedback provider wiring, commitment
//! handoff) lives here; GO-C3-specific values (100× safety multiplier,
//! ±5 % default band, reactive pin Q-factor, etc.) live in the adapter
//! as presets.

use surge_dispatch::{
    CommitmentPolicy, CommitmentSchedule, DispatchModel, DispatchRequest, DispatchSolveOptions,
};
use surge_network::Network;
use surge_opf::AcOpfOptions;

use crate::ac_reconcile::AcScedSetup;
use crate::ac_refinement::RetryPolicy;
use crate::canonical_workflow::{CANONICAL_ED_STAGE_ID, CANONICAL_UC_STAGE_ID};
use crate::workflow::{
    BranchRelaxFromDcSlack, DispatchPinningConfig, MarketStage, MarketStageRole, MarketWorkflow,
};

/// Errors surfaced by [`build_two_stage_workflow`].
#[derive(Debug, thiserror::Error)]
pub enum TwoStageError {
    #[error("DispatchModel::prepare failed for stage '{stage_id}': {error}")]
    ModelPrepare { stage_id: String, error: String },
}

/// Per-stage network, request, and solve options. The stages may be
/// prepared against different networks (e.g. DC SCUC against the raw
/// network, AC SCED against a network with synthetic HVDC reactive-
/// support generators layered on).
#[derive(Clone)]
pub struct StageInputs {
    pub network: Network,
    pub request: DispatchRequest,
    pub options: DispatchSolveOptions,
}

/// The AC SCED preset — everything that makes the economic dispatch
/// stage harder than a plain `MarketStage::new(...)`.
#[derive(Clone)]
pub struct EconomicDispatchPreset {
    /// AC OPF options to install on `ed.request.runtime_mut().ac_opf`.
    pub ac_opf_options: AcOpfOptions,
    /// Canonical AC SCED setup (reactive-reserve filter, commitment
    /// augmentation, bandable-subset pin, warm start, Q overrides).
    pub ac_sced_setup: AcScedSetup,
    /// Retry policy — OPF attempts, band attempts, pmin-relax sweep,
    /// feedback providers, commitment probes.
    pub retry_policy: RetryPolicy,
    /// Canonical pre-pin band. The retry runtime re-applies per-attempt
    /// producer dispatch pinning from the AC SCED setup's own pinning,
    /// so this controls only the first pass baseline plus the anchor
    /// set that the retry grid's last-ditch rung relies on.
    pub dispatch_pinning: DispatchPinningConfig,
    /// Optional SCUC→SCED branch-thermal relaxation hook.
    pub branch_relax_from_dc_slack: Option<BranchRelaxFromDcSlack>,
    /// When true, set
    /// `runtime.ac_relax_committed_pmin_to_zero` on the ed request so
    /// the AC NLP can floor a committed generator's `pmin` at 0.
    pub ac_relax_committed_pmin_to_zero: bool,
    /// Per-period AC SCED concurrency, mirrored onto
    /// `ed_request.runtime_mut().ac_sced_period_concurrency`. See
    /// [`surge_dispatch::request::DispatchRuntime::ac_sced_period_concurrency`]
    /// for full semantics.
    pub ac_sced_period_concurrency: Option<usize>,
}

/// Assemble the canonical two-stage `DC SCUC + AC SCED` workflow from
/// pre-built per-stage inputs and an AC SCED preset.
///
/// What this function does:
///
/// * Prepares a [`DispatchModel`] for each stage's network.
/// * Installs `preset.ac_opf_options` on the ed request's runtime.
/// * Seeds an empty `CommitmentPolicy::Fixed` on the ed request
///   (the workflow executor replaces this with the solved UC
///   commitment at solve time).
/// * Optionally sets `ac_relax_committed_pmin_to_zero` on the ed
///   request.
/// * Wires the ed stage with `derived_from(UC)`, `commitment_from(UC)`,
///   `pin_dispatch_from(preset.dispatch_pinning)`, the AC SCED setup,
///   the retry policy, and the optional branch-relax hook.
///
/// What this function does **not** do:
///
/// * It does not mutate `uc.network` or `ed.network` (PV promotion,
///   synthetic-terminal injection, etc. are adapter responsibilities).
/// * It does not build the requests (format-specific).
/// * It does not apply reactive-support-pin midpoint bounds to the ed
///   request or merge must-run schedules — adapters do this on their
///   inputs before calling into here.
pub fn build_two_stage_workflow(
    uc: StageInputs,
    ed: StageInputs,
    preset: EconomicDispatchPreset,
) -> Result<MarketWorkflow, TwoStageError> {
    let uc_model =
        DispatchModel::prepare(&uc.network).map_err(|err| TwoStageError::ModelPrepare {
            stage_id: CANONICAL_UC_STAGE_ID.to_string(),
            error: err.to_string(),
        })?;
    let uc_stage = MarketStage::new(
        CANONICAL_UC_STAGE_ID,
        MarketStageRole::UnitCommitment,
        uc_model,
        uc.request,
    )
    .with_options(uc.options);

    let mut ed_request = ed.request;
    // Stage-2 commitment is overridden by the executor at solve time;
    // seed with an empty `Fixed` placeholder so the request shape is
    // consistent even before stage 1 runs.
    if !matches!(ed_request.commitment(), CommitmentPolicy::Fixed(_)) {
        ed_request.set_commitment(CommitmentPolicy::Fixed(CommitmentSchedule {
            resources: Vec::new(),
        }));
    }
    ed_request.runtime_mut().ac_opf = Some(preset.ac_opf_options);
    if preset.ac_relax_committed_pmin_to_zero {
        ed_request.runtime_mut().ac_relax_committed_pmin_to_zero = true;
    }
    ed_request.runtime_mut().ac_sced_period_concurrency = preset.ac_sced_period_concurrency;

    let ed_model =
        DispatchModel::prepare(&ed.network).map_err(|err| TwoStageError::ModelPrepare {
            stage_id: CANONICAL_ED_STAGE_ID.to_string(),
            error: err.to_string(),
        })?;

    let mut ed_stage = MarketStage::new(
        CANONICAL_ED_STAGE_ID,
        MarketStageRole::EconomicDispatch,
        ed_model,
        ed_request,
    )
    .derived_from(CANONICAL_UC_STAGE_ID)
    .commitment_from(CANONICAL_UC_STAGE_ID)
    .pin_dispatch_from(preset.dispatch_pinning)
    .with_ac_sced_setup(preset.ac_sced_setup)
    .with_retry_policy(preset.retry_policy)
    .with_options(ed.options);

    if let Some(relax) = preset.branch_relax_from_dc_slack {
        ed_stage = ed_stage.with_branch_relax_from_dc_slack(relax);
    }

    Ok(MarketWorkflow::new(vec![uc_stage, ed_stage]))
}
