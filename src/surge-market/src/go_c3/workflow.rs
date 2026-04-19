// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 canonical workflow builder — thin adapter.
//!
//! Assembles the standard two-stage market workflow for a GO C3
//! scenario:
//!
//! 1. **DC SCUC** — `GoC3Formulation::Dc` with `commitment_mode =
//!    Optimize`. Commits units and sets the energy dispatch.
//! 2. **AC SCED** — `GoC3Formulation::Ac` with `commitment_mode =
//!    FixedInitial`; the workflow executor overrides the commitment
//!    schedule with stage 1's solution at solve time.
//!
//! All the market-algorithm machinery (AC OPF presets, reactive-
//! reserve market filter, bandable-subset pinning, warm start, retry
//! ladders, feedback providers, branch-relax hook) lives in the
//! parent crate's canonical modules (`two_stage`, `ac_sced_setup`,
//! `ac_opf_presets`, `heuristics`, etc.). This file is purely the
//! GO-C3 field-mapping + preset-selection layer.

use std::collections::HashSet;

use surge_io::go_c3::{
    GoC3CommitmentMode, GoC3Context, GoC3Formulation, GoC3Policy, GoC3Problem,
    apply_hvdc_reactive_terminals,
};
use surge_network::Network;

use crate::ac_sced_setup::{AcScedSetupInputs, build_ac_sced_setup};
use crate::canonical_workflow::{CANONICAL_UC_STAGE_ID, CanonicalWorkflowOptions};
use crate::heuristics::{
    select_bandable_producers, select_reactive_support_pin_generators, select_widest_q_anchors,
};
use crate::two_stage::{EconomicDispatchPreset, StageInputs, build_two_stage_workflow};
use crate::workflow::{BranchRelaxFromDcSlack, DispatchPinningConfig, MarketWorkflow};
use crate::{DcReducedCostTargetTracking, LmpMarginalCostTargetTracking};

use super::GoC3DispatchError;
use super::presets::{
    apply_goc3_policy_to_ac_opf, apply_reactive_support_pin_to_request, goc3_ac_opf_options,
    goc3_bandable_criteria, goc3_classification, goc3_consumer_q_to_p_ratios,
    goc3_dispatch_pinning_bands, goc3_max_additional_bandable, goc3_peak_load,
    goc3_producer_ramp_limits, goc3_reactive_support_commitment_schedule,
    goc3_reactive_support_pin_criteria, goc3_reserve_product_ids, goc3_retry_policy,
    goc3_wide_q_anchor_criteria, merge_reactive_pin_must_runs,
};

pub use crate::heuristics::promote_q_capable_generators_to_pv;

/// Build the canonical GO C3 two-stage market workflow.
///
/// The DC SCUC and AC SCED stages run on *different* networks: stage 1
/// uses the raw network (no synthetic HVDC reactive-support gens);
/// stage 2 uses a network with those synthetics layered on. Sharing
/// one network would force the DC LP to allocate vars for generators
/// it doesn't model and can nudge commitment off Python-orchestration
/// parity. The context is shared — mutated to AC mode so it carries
/// the synthetic resource mappings the exporter and AC SCED setup
/// depend on.
pub fn build_canonical_workflow(
    problem: &GoC3Problem,
    context: &mut GoC3Context,
    policy: &GoC3Policy,
    network: &mut Network,
    options: CanonicalWorkflowOptions,
) -> Result<MarketWorkflow, GoC3DispatchError> {
    // ── Network preparation ──────────────────────────────────────────
    let mut dc_network = network.clone();
    promote_q_capable_generators_to_pv(&mut dc_network);

    let mut ac_network = dc_network.clone();
    if context.internal_support_commitment_schedule.is_empty() {
        let mut ac_policy = policy.clone();
        ac_policy.formulation = GoC3Formulation::Ac;
        apply_hvdc_reactive_terminals(&mut ac_network, context, problem, &ac_policy).map_err(
            |err| GoC3DispatchError::Export(format!("apply_hvdc_reactive_terminals: {err}")),
        )?;
    }
    // Propagate PV + synthetics back onto the caller's network so
    // downstream tooling sees the post-mutation state.
    *network = ac_network.clone();

    // ── Requests ─────────────────────────────────────────────────────
    let mut uc_policy = policy.clone();
    uc_policy.formulation = GoC3Formulation::Dc;
    let uc_request = super::build_dispatch_request(problem, context, &uc_policy)?;

    let mut ed_policy = policy.clone();
    ed_policy.formulation = GoC3Formulation::Ac;
    ed_policy.commitment_mode = GoC3CommitmentMode::FixedInitial;
    let mut ed_request = super::build_dispatch_request(problem, context, &ed_policy)?;

    // ── Canonical inputs ─────────────────────────────────────────────
    let classification = goc3_classification(context);
    let periods = problem.time_series_input.general.time_periods;

    // Reactive-support pin selection + Pg midpoint pin on the ed request.
    let reactive_pin_ids: HashSet<String> = if policy.reactive_support_pin_factor > 0.0 {
        let (peak_load_by_bus, peak_system_load) = goc3_peak_load(problem, context);
        select_reactive_support_pin_generators(
            &ac_network,
            &classification,
            &peak_load_by_bus,
            peak_system_load,
            &goc3_reactive_support_pin_criteria(policy.reactive_support_pin_factor),
        )
        .into_iter()
        .collect()
    } else {
        HashSet::new()
    };
    apply_reactive_support_pin_to_request(&mut ed_request, &reactive_pin_ids);

    // AC OPF options — canonical GO-C3 preset plus policy overrides.
    let mut ac_opf_options =
        goc3_ac_opf_options(problem, policy.sced_bus_balance_safety_multiplier);
    apply_goc3_policy_to_ac_opf(&mut ac_opf_options, policy);

    // Bandable producers (GO-C3-tuned size thresholds).
    let bandable_ids = select_bandable_producers(
        &ac_network,
        &classification,
        &goc3_bandable_criteria(goc3_max_additional_bandable(ac_network.buses.len())),
    );

    // Anchors (wide-Q producers) + reactive-pin overlap for dispatch pinning.
    let anchor_ids = select_widest_q_anchors(
        &ac_network,
        &classification,
        &goc3_wide_q_anchor_criteria(5),
    );
    let mut anchor_set: HashSet<String> = anchor_ids.into_iter().collect();
    for rid in &reactive_pin_ids {
        anchor_set.insert(rid.clone());
    }

    // AC SCED setup — commitment augmentation (reactive support + pins).
    let mut commitment_augmentation =
        goc3_reactive_support_commitment_schedule(problem, context, periods);
    merge_reactive_pin_must_runs(&mut commitment_augmentation, &reactive_pin_ids, periods);

    let ac_sced_setup = build_ac_sced_setup(AcScedSetupInputs {
        source_stage_id: CANONICAL_UC_STAGE_ID.to_string(),
        classification,
        bandable_producer_resource_ids: bandable_ids,
        reserve_product_ids: goc3_reserve_product_ids(),
        producer_ramp_limits_mw_per_hr: goc3_producer_ramp_limits(problem, context),
        bus_uid_to_number: context.bus_uid_to_number.clone(),
        consumer_q_to_p_ratios: goc3_consumer_q_to_p_ratios(problem),
        commitment_augmentation,
        pinning_bands: goc3_dispatch_pinning_bands(),
    });

    // Retry policy: canonical 3-rung + canonical target-tracking feedback.
    let retry_policy = goc3_retry_policy(&ac_opf_options)
        .with_feedback(std::sync::Arc::new(DcReducedCostTargetTracking::default()))
        .with_feedback(std::sync::Arc::new(
            LmpMarginalCostTargetTracking::for_go_c3(problem, context),
        ));

    // Dispatch pinning: wide pre-pin that collapses to physical envelope
    // (via clip), with explicit anchor set so the retry grid's last-
    // ditch rung retains full flexibility on those generators.
    let dispatch_pinning = DispatchPinningConfig {
        source_stage_id: CANONICAL_UC_STAGE_ID.to_string(),
        band_fraction: 1.0e6,
        band_floor_mw: 0.0,
        band_cap_mw: 1.0e9,
        anchor_resource_ids: anchor_set,
    };

    // Branch thermal relaxation hook (opt-in via policy).
    let branch_relax =
        policy
            .relax_sced_branch_limits_to_dc_slack
            .then(|| BranchRelaxFromDcSlack {
                source_stage_id: CANONICAL_UC_STAGE_ID.to_string(),
                margin_mva: policy.sced_branch_relax_margin_mva,
            });

    // ── Assemble via canonical two-stage builder ─────────────────────
    build_two_stage_workflow(
        StageInputs {
            network: dc_network,
            request: uc_request,
            options: options.uc_options.clone(),
        },
        StageInputs {
            network: ac_network,
            request: ed_request,
            options: options.ed_options,
        },
        EconomicDispatchPreset {
            ac_opf_options,
            ac_sced_setup,
            retry_policy,
            dispatch_pinning,
            branch_relax_from_dc_slack: branch_relax,
            ac_relax_committed_pmin_to_zero: policy.ac_relax_committed_pmin_to_zero,
            ac_sced_period_concurrency: policy.ac_sced_period_concurrency,
        },
    )
    .map_err(|err| GoC3DispatchError::Export(err.to_string()))
}
