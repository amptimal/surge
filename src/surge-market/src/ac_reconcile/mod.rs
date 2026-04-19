// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical AC SCED setup — applied between market stages after the
//! source stage (typically DC SCUC) has solved.
//!
//! The primary AC SCED request coming out of an adapter's
//! [`crate::go_c3::build_dispatch_request`] (or equivalent) is a
//! skeleton: it carries structural information (network, offer
//! schedules, reserves) but nothing that depends on the prior stage's
//! solution. Between stages, the workflow executor enriches the
//! skeleton with canonical handoffs:
//!
//! 1. **Commitment handoff** — extract stage-1's solved commitment and
//!    pin stage-2 to it (handled in [`crate::workflow`]).
//! 2. **Commitment augmentation** — merge extra must-run schedules
//!    (voltage-support generators that must stay online on the AC
//!    stage) onto the pinned commitment.
//! 3. **Reactive-reserves-only market filter** — strip active reserve
//!    products and awards that the AC stage cannot re-clear.
//! 4. **Bandable-subset dispatch pinning** — narrow most generators'
//!    P bounds to their stage-1 dispatch, but give a designated subset
//!    (slack-bus gens, top-Q-range gens) a wider band so the NLP has
//!    headroom to close reactive corners.
//! 5. **Warm start** — populate `runtime.ac_dispatch_warm_start` with
//!    per-bus V/θ and per-resource P/Q seeds read from stage-1's
//!    solution. Without a warm start, the AC NLP starts from flat
//!    voltages and zero Q and routinely runs out of iterations.
//! 6. **Q locks / fixes** — zero-bound Q for synthetic HVDC terminal
//!    support generators when their Q is being supplied elsewhere, or
//!    fix their Q to an external schedule when the caller pins HVDC
//!    terminal Q.
//!
//! All six pieces are *canonical* in the sense that any market format
//! with a two-stage DC→AC recipe benefits from them. The typed
//! [`AcScedSetup`] is a config bag; the workflow executor interprets
//! it against the source stage's [`DispatchSolution`] and mutates the
//! target stage's [`DispatchRequest`] in place before solve.

use std::collections::{HashMap, HashSet};

use surge_dispatch::{DispatchRequest, DispatchSolution, ResourceCommitmentSchedule};

mod commitment_merge;
mod dispatch_pinning;
mod market_filter;
mod target_tracking;
mod warm_start;

pub use commitment_merge::merge_commitment_augmentation;
pub use dispatch_pinning::{ProducerDispatchPinning, RampLimits, apply_producer_dispatch_pinning};
pub use market_filter::apply_reactive_reserve_filter;
pub use target_tracking::{
    DcReducedCostTargetTracking, LmpMarginalCostTargetTracking, TargetTrackingBoundPenalties,
    TargetTrackingPenaltyPresets,
};
pub use warm_start::{AcWarmStartConfig, build_ac_dispatch_warm_start};

/// Canonical AC SCED setup config. Typed bag of all the information a
/// market workflow executor needs to enrich a stage-2 AC request after
/// stage-1 has solved.
///
/// Adapters build this once (typically inside
/// `build_canonical_workflow`) and attach it to the AC stage via
/// [`crate::MarketStage::with_ac_sced_setup`]. The executor invokes
/// [`apply_ac_sced_setup`] between stages.
#[derive(Clone, Debug, Default)]
pub struct AcScedSetup {
    /// Stage ID whose solved dispatch/commitment drives the handoffs.
    pub source_stage_id: String,

    /// When `Some`, the market's reserve products are filtered to
    /// this set of product IDs; requirements, offer schedules, and
    /// awards for the others are dropped. Typical value:
    /// `{"q_res_up", "q_res_down"}`.
    pub reactive_reserve_product_ids: Option<HashSet<String>>,

    /// Extra per-resource commitment schedules merged onto the stage-2
    /// fixed commitment (`Fixed::Or` semantics — a period is committed
    /// if either the source stage or the augmentation says so).
    pub commitment_augmentation: Vec<ResourceCommitmentSchedule>,

    /// Producer dispatch-pinning configuration (bandable subset +
    /// reserve headroom shrink). When `None`, the bare
    /// [`crate::workflow::DispatchPinningConfig`] still applies.
    pub dispatch_pinning: Option<ProducerDispatchPinning>,

    /// Warm-start configuration (adapter-provided mappings). When
    /// `Some`, the executor populates `runtime.ac_dispatch_warm_start`
    /// from the source solution.
    pub warm_start: Option<AcWarmStartConfig>,

    /// Resource IDs whose reactive-power bounds should be locked to
    /// `[0, 0]` across all periods. Typical use: synthetic HVDC
    /// terminal support generators when their Q is being supplied by
    /// a fixed external schedule and shouldn't participate in AC OPF.
    pub generator_q_locks: HashSet<String>,

    /// Per-resource fixed Q schedules (`resource_id → per-period
    /// MVAr`). When set, the executor fixes `q_min_mvar[p] =
    /// q_max_mvar[p]` to this value. Typical use: HVDC terminal Q
    /// pins routed through synthetic support generators.
    pub generator_q_fixes: HashMap<String, Vec<f64>>,
}

/// Apply the AC SCED setup to a stage-2 request, given the solved
/// source stage's dispatch solution. Invoked by the workflow executor
/// after commitment handoff and dispatch pinning have already run
/// (the canonical `DispatchPinningConfig` on the stage) but before
/// the stage-2 solve.
///
/// When `include_dispatch_pinning = false`, the setup's producer
/// dispatch pinning is skipped — used when the refinement runtime
/// will apply the pinning per-attempt with different band params.
/// The pinning reads the request's existing dispatch bounds as the
/// physical envelope, so re-applying it after it's already been
/// applied would narrow against narrowed bounds (wrong). The retry
/// runtime orchestration needs the *original* physical envelope to
/// rebuild the pinning from scratch per attempt.
pub fn apply_ac_sced_setup(
    setup: &AcScedSetup,
    source_solution: &DispatchSolution,
    request: &mut DispatchRequest,
    include_dispatch_pinning: bool,
) {
    // Commitment augmentation — merge voltage-support schedules onto
    // the commitment the executor already pinned from the source.
    if !setup.commitment_augmentation.is_empty() {
        merge_commitment_augmentation(request, &setup.commitment_augmentation);
    }

    // Reactive-reserves-only filter — drop active reserve products.
    if let Some(product_ids) = &setup.reactive_reserve_product_ids {
        apply_reactive_reserve_filter(request, product_ids);
    }

    // Bandable-subset dispatch pinning — overrides the canonical
    // pin_generator_dispatch_bounds that ran earlier. We zero-initialize
    // each profile and rebuild from scratch so the producer_static +
    // reserve shrink + band logic all composes correctly.
    if include_dispatch_pinning {
        if let Some(pinning) = &setup.dispatch_pinning {
            apply_producer_dispatch_pinning(request, source_solution, pinning);
        }
    }

    // Warm start — populate ac_dispatch_warm_start from the source
    // solution's bus V/θ and per-resource P/Q. Skipped when the
    // request already carries a non-empty user-provided warm start
    // (e.g. seeded from a reference solution for replay/diagnostic
    // runs); we don't want the executor to clobber it.
    if let Some(warm_start_cfg) = &setup.warm_start {
        if request.runtime().ac_dispatch_warm_start.is_empty() {
            let warm_start = build_ac_dispatch_warm_start(request, source_solution, warm_start_cfg);
            request.runtime_mut().ac_dispatch_warm_start = warm_start;
        }
    }

    // Q locks / fixes — applied last so nothing else overwrites them.
    if !setup.generator_q_locks.is_empty() || !setup.generator_q_fixes.is_empty() {
        apply_q_bounds_overrides(request, &setup.generator_q_locks, &setup.generator_q_fixes);
    }
}

fn apply_q_bounds_overrides(
    request: &mut DispatchRequest,
    q_locks: &HashSet<String>,
    q_fixes: &HashMap<String, Vec<f64>>,
) {
    let timeline = request.timeline().clone();
    let periods = timeline.periods;
    let profiles = request.profiles_mut();
    let existing: HashMap<String, usize> = profiles
        .generator_dispatch_bounds
        .profiles
        .iter()
        .enumerate()
        .map(|(idx, entry)| (entry.resource_id.clone(), idx))
        .collect();

    for resource_id in q_locks {
        let zeros = vec![0.0; periods];
        if let Some(idx) = existing.get(resource_id) {
            let entry = &mut profiles.generator_dispatch_bounds.profiles[*idx];
            entry.q_min_mvar = Some(zeros.clone());
            entry.q_max_mvar = Some(zeros);
        } else {
            profiles.generator_dispatch_bounds.profiles.push(
                surge_dispatch::request::GeneratorDispatchBoundsProfile {
                    resource_id: resource_id.clone(),
                    p_min_mw: vec![0.0; periods],
                    p_max_mw: vec![0.0; periods],
                    q_min_mvar: Some(zeros.clone()),
                    q_max_mvar: Some(zeros),
                },
            );
        }
    }

    for (resource_id, q_series) in q_fixes {
        let truncated: Vec<f64> = q_series.iter().copied().take(periods).collect();
        let mut padded = truncated;
        while padded.len() < periods {
            padded.push(0.0);
        }
        if let Some(idx) = existing.get(resource_id) {
            let entry = &mut profiles.generator_dispatch_bounds.profiles[*idx];
            entry.q_min_mvar = Some(padded.clone());
            entry.q_max_mvar = Some(padded);
        } else {
            profiles.generator_dispatch_bounds.profiles.push(
                surge_dispatch::request::GeneratorDispatchBoundsProfile {
                    resource_id: resource_id.clone(),
                    p_min_mw: vec![0.0; periods],
                    p_max_mw: vec![0.0; periods],
                    q_min_mvar: Some(padded.clone()),
                    q_max_mvar: Some(padded),
                },
            );
        }
    }
}
