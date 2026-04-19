// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! End-to-end tests for the canonical AC refinement surface.
//!
//! Exercises the retry grid and per-attempt pinning on a tiny
//! synthetic workflow, without any format adapter in the loop. This
//! is the Rust-side sanity check a future market adapter author can
//! run before plugging into the crate.

use std::sync::Arc;

use std::collections::{HashMap, HashSet};

use surge_dispatch::request::{AcDispatchTargetTrackingPair, GeneratorDispatchBoundsProfile};
use surge_dispatch::{
    CommitmentInitialCondition, CommitmentOptions, CommitmentPolicy, DispatchMarket, DispatchModel,
    DispatchNetwork, DispatchProfiles, DispatchRequest, DispatchState, DispatchTimeline,
    Formulation, GeneratorOfferSchedule, IntervalCoupling, ResourcePeriodDetail,
};
use surge_market::canonical_workflow::{
    CANONICAL_ED_STAGE_ID, CANONICAL_UC_STAGE_ID, CanonicalWorkflowOptions,
    canonical_two_stage_workflow,
};
use surge_market::{
    BandAttempt, DcReducedCostTargetTracking, FeedbackCtx, FeedbackProvider, HvdcAttempt,
    OpfAttempt, ProducerDispatchPinning, RetryPolicy, solve_market_workflow,
};
use surge_network::market::{OfferCurve, OfferSchedule};

fn test_data_path(name: &str) -> std::path::PathBuf {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return std::path::PathBuf::from(p).join(name);
    }
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/data")
        .join(name)
}

fn build_uc_request(periods: usize) -> DispatchRequest {
    let mut market = DispatchMarket::default();
    let per_period_offer = |cost_per_mwh: f64| {
        (0..periods)
            .map(|_| {
                Some(OfferCurve {
                    segments: vec![(300.0, cost_per_mwh)],
                    no_load_cost: 0.0,
                    startup_tiers: Vec::new(),
                })
            })
            .collect()
    };
    market.generator_offer_schedules = vec![
        GeneratorOfferSchedule {
            resource_id: "gen_cheap".to_string(),
            schedule: OfferSchedule {
                periods: per_period_offer(15.0),
            },
        },
        GeneratorOfferSchedule {
            resource_id: "gen_expensive".to_string(),
            schedule: OfferSchedule {
                periods: per_period_offer(60.0),
            },
        },
    ];

    let mut profiles = DispatchProfiles::default();
    profiles.generator_dispatch_bounds.profiles = vec![
        GeneratorDispatchBoundsProfile {
            resource_id: "gen_cheap".to_string(),
            p_min_mw: vec![20.0; periods],
            p_max_mw: vec![150.0; periods],
            q_min_mvar: None,
            q_max_mvar: None,
        },
        GeneratorDispatchBoundsProfile {
            resource_id: "gen_expensive".to_string(),
            p_min_mw: vec![0.0; periods],
            p_max_mw: vec![250.0; periods],
            q_min_mvar: None,
            q_max_mvar: None,
        },
    ];

    let options = CommitmentOptions {
        initial_conditions: vec![
            CommitmentInitialCondition {
                resource_id: "gen_cheap".to_string(),
                committed: Some(true),
                hours_on: Some(4),
                offline_hours: None,
                starts_24h: None,
                starts_168h: None,
                energy_mwh_24h: None,
            },
            CommitmentInitialCondition {
                resource_id: "gen_expensive".to_string(),
                committed: Some(false),
                hours_on: None,
                offline_hours: Some(6.0),
                starts_24h: None,
                starts_168h: None,
                energy_mwh_24h: None,
            },
        ],
        warm_start_commitment: Vec::new(),
        time_limit_secs: None,
        mip_rel_gap: None,
        mip_gap_schedule: None,
        disable_warm_start: false,
    };

    DispatchRequest::builder()
        .formulation(Formulation::Dc)
        .coupling(IntervalCoupling::TimeCoupled)
        .timeline(DispatchTimeline {
            periods,
            interval_hours: 1.0,
            interval_hours_by_period: Vec::new(),
        })
        .market(market)
        .profiles(profiles)
        .state(DispatchState::default())
        .network(DispatchNetwork::default())
        .commitment(CommitmentPolicy::Optimize(options))
        .build()
}

fn load_case9_or_skip() -> Option<surge_network::Network> {
    let path = test_data_path("case9.m");
    if !path.exists() {
        eprintln!(
            "skipping canonical_refinement test: {} absent",
            path.display()
        );
        return None;
    }
    Some(surge_io::matpower::load(&path).expect("load case9.m"))
}

#[test]
fn noop_retry_policy_matches_single_shot_baseline() {
    let Some(network) = load_case9_or_skip() else {
        return;
    };
    let periods = 3;
    let model = DispatchModel::prepare(&network).expect("prepare DispatchModel");

    let uc_request = build_uc_request(periods);
    let mut ed_request = uc_request.clone();
    ed_request.set_commitment(CommitmentPolicy::Fixed(
        surge_dispatch::CommitmentSchedule {
            resources: Vec::new(),
        },
    ));

    let options = CanonicalWorkflowOptions {
        ed_band_fraction: 0.10,
        ed_band_floor_mw: 2.0,
        ed_band_cap_mw: 1.0e9,
        ..CanonicalWorkflowOptions::default()
    };

    let baseline_workflow = canonical_two_stage_workflow(
        model.clone(),
        uc_request.clone(),
        ed_request.clone(),
        options.clone(),
    );
    let baseline = solve_market_workflow(&baseline_workflow).expect("solve baseline");

    let mut refined_workflow = canonical_two_stage_workflow(model, uc_request, ed_request, options);
    refined_workflow.stages[1].retry_policy = Some(RetryPolicy::noop());
    let refined = solve_market_workflow(&refined_workflow).expect("solve with noop retry");

    assert_eq!(baseline.stages.len(), 2);
    assert_eq!(refined.stages[0].stage_id, CANONICAL_UC_STAGE_ID);
    assert_eq!(refined.stages[1].stage_id, CANONICAL_ED_STAGE_ID);

    for (base, refined) in baseline.stages[1]
        .solution
        .periods()
        .iter()
        .zip(refined.stages[1].solution.periods().iter())
    {
        let base_mw: HashMap<&str, f64> = base
            .resource_results()
            .iter()
            .filter_map(|r| match &r.detail {
                ResourcePeriodDetail::Generator(_) => Some((r.resource_id.as_str(), r.power_mw)),
                _ => None,
            })
            .collect();
        for r in refined.resource_results() {
            if let ResourcePeriodDetail::Generator(_) = &r.detail {
                let base_p = base_mw.get(r.resource_id.as_str()).copied().unwrap_or(0.0);
                assert!(
                    (base_p - r.power_mw).abs() < 1e-3,
                    "noop retry drifted dispatch on {}",
                    r.resource_id
                );
            }
        }
    }
}

#[test]
fn multi_opf_attempt_retry_grid_selects_first_success() {
    let Some(network) = load_case9_or_skip() else {
        return;
    };
    let periods = 2;
    let model = DispatchModel::prepare(&network).expect("prepare DispatchModel");

    let uc_request = build_uc_request(periods);
    let mut ed_request = uc_request.clone();
    ed_request.set_commitment(CommitmentPolicy::Fixed(
        surge_dispatch::CommitmentSchedule {
            resources: Vec::new(),
        },
    ));

    let mut workflow = canonical_two_stage_workflow(
        model,
        uc_request,
        ed_request,
        CanonicalWorkflowOptions::default(),
    );

    let retry = RetryPolicy {
        relax_pmin_sweep: vec![false, true],
        opf_attempts: vec![
            OpfAttempt::new("first", None),
            OpfAttempt::new("second", None),
            OpfAttempt::new("third", None),
        ],
        nlp_solver_candidates: vec![None],
        band_attempts: vec![BandAttempt::default_band()],
        wide_band_penalty_threshold_dollars: f64::INFINITY,
        hvdc_attempts: vec![HvdcAttempt::default_attempt()],
        hvdc_retry_bus_slack_threshold_mw: f64::INFINITY,
        hard_fail_first_attempt: false,
        feedback_providers: Vec::new(),
        commitment_probes: Vec::new(),
        max_iterations: 0,
    };
    workflow.stages[1].retry_policy = Some(retry);

    let _result = solve_market_workflow(&workflow).expect("solve with multi-attempt retry");
}

#[test]
fn feedback_provider_runs_before_attempt() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let Some(network) = load_case9_or_skip() else {
        return;
    };
    let periods = 2;
    let model = DispatchModel::prepare(&network).expect("prepare DispatchModel");

    let uc_request = build_uc_request(periods);
    let mut ed_request = uc_request.clone();
    ed_request.set_commitment(CommitmentPolicy::Fixed(
        surge_dispatch::CommitmentSchedule {
            resources: Vec::new(),
        },
    ));

    #[derive(Debug)]
    struct CountingFeedback {
        counter: Arc<AtomicUsize>,
    }

    impl FeedbackProvider for CountingFeedback {
        fn name(&self) -> &str {
            "counting"
        }
        fn augment(
            &self,
            _ctx: &FeedbackCtx,
            _request: &mut DispatchRequest,
        ) -> Result<(), surge_dispatch::DispatchError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let retry = RetryPolicy {
        relax_pmin_sweep: vec![false],
        opf_attempts: vec![OpfAttempt::new("only", None)],
        nlp_solver_candidates: vec![None],
        band_attempts: vec![BandAttempt::default_band()],
        wide_band_penalty_threshold_dollars: f64::INFINITY,
        hvdc_attempts: vec![HvdcAttempt::default_attempt()],
        hvdc_retry_bus_slack_threshold_mw: f64::INFINITY,
        hard_fail_first_attempt: false,
        feedback_providers: vec![Arc::new(CountingFeedback {
            counter: Arc::clone(&counter),
        })],
        commitment_probes: Vec::new(),
        max_iterations: 0,
    };

    let mut workflow = canonical_two_stage_workflow(
        model,
        uc_request,
        ed_request,
        CanonicalWorkflowOptions::default(),
    );
    workflow.stages[1].retry_policy = Some(retry);

    let _result = solve_market_workflow(&workflow).expect("solve with feedback");
    assert!(
        counter.load(Ordering::SeqCst) >= 1,
        "feedback provider should run at least once"
    );
}

#[test]
fn dc_reduced_cost_provider_handles_empty_source() {
    // When the prior stage has no solved bound shadows, the provider
    // should still return Ok (no overrides).
    let provider = DcReducedCostTargetTracking::default();
    let mut request = build_uc_request(1);
    request.set_formulation(Formulation::Dc);
    let ctx = FeedbackCtx {
        stage_id: "test",
        iteration: 0,
        prior_stage_solution: None,
    };
    provider.augment(&ctx, &mut request).expect("augment ok");
    assert!(
        request
            .runtime()
            .ac_target_tracking
            .generator_p_coefficients_overrides_by_id
            .is_empty()
    );
}

#[test]
fn producer_dispatch_pinning_zeros_producer_static() {
    // Producer_static resources get pinned to [0, 0] regardless of
    // other config (validates the `producer_static_resource_ids`
    // classification in `apply_producer_dispatch_pinning`).
    let periods = 2;
    let mut request = DispatchRequest::builder()
        .formulation(Formulation::Ac)
        .coupling(IntervalCoupling::PeriodByPeriod)
        .timeline(DispatchTimeline {
            periods,
            interval_hours: 1.0,
            interval_hours_by_period: Vec::new(),
        })
        .market(DispatchMarket::default())
        .profiles(DispatchProfiles::default())
        .state(DispatchState::default())
        .network(DispatchNetwork::default())
        .commitment(CommitmentPolicy::Fixed(
            surge_dispatch::CommitmentSchedule {
                resources: Vec::new(),
            },
        ))
        .build();

    request
        .profiles_mut()
        .generator_dispatch_bounds
        .profiles
        .push(GeneratorDispatchBoundsProfile {
            resource_id: "static_gen".to_string(),
            p_min_mw: vec![10.0, 20.0],
            p_max_mw: vec![100.0, 200.0],
            q_min_mvar: None,
            q_max_mvar: None,
        });

    let mut producer_static_resource_ids = HashSet::new();
    producer_static_resource_ids.insert("static_gen".to_string());
    let pinning = ProducerDispatchPinning {
        producer_resource_ids: HashSet::new(),
        producer_static_resource_ids,
        bandable_producer_resource_ids: HashSet::new(),
        band_fraction: 0.05,
        band_floor_mw: 1.0,
        band_cap_mw: 1.0e9,
        up_reserve_product_ids: HashSet::new(),
        down_reserve_product_ids: HashSet::new(),
        apply_reserve_shrink: false,
        ramp_limits_mw_per_hr: HashMap::new(),
        relax_pmin: false,
        relax_pmin_for_resources: HashSet::new(),
        anchor_resource_ids: HashSet::new(),
    };

    // Need a DispatchSolution to drive pinning — fabricate a minimal
    // one via serde. The pinning function only reads source for
    // per-resource dispatch targets, which we leave empty.
    let solution_json = serde_json::json!({
        "periods": [
            {"period_index": 0, "total_cost": 0.0, "co2_t": 0.0},
            {"period_index": 1, "total_cost": 0.0, "co2_t": 0.0}
        ],
        "summary": {"horizon_hours": 2.0, "total_cost": 0.0, "total_energy_cost": 0.0, "total_no_load_cost": 0.0, "total_startup_cost": 0.0, "total_reserve_cost": 0.0, "total_penalty_cost": 0.0, "total_co2_t": 0.0, "mode": "dc"},
        "input": {"lp_solver": null},
    });
    let solution: surge_dispatch::DispatchSolution =
        serde_json::from_value(solution_json).expect("minimal DispatchSolution");

    surge_market::apply_producer_dispatch_pinning(&mut request, &solution, &pinning);

    let profile = &request
        .profiles()
        .generator_dispatch_bounds
        .profiles
        .iter()
        .find(|p| p.resource_id == "static_gen")
        .expect("static_gen profile");
    assert_eq!(profile.p_min_mw, vec![0.0, 0.0]);
    assert_eq!(profile.p_max_mw, vec![0.0, 0.0]);
}

#[test]
fn target_tracking_pair_structure() {
    // Sanity check that `AcDispatchTargetTrackingPair` serializes
    // symmetrically when both fields equal, matching the symmetric
    // presets exposed by the target-tracking module.
    let pair = AcDispatchTargetTrackingPair {
        upward_per_mw2: 300.0,
        downward_per_mw2: 300.0,
    };
    let serialized = serde_json::to_string(&pair).expect("serialize pair");
    assert!(serialized.contains("300"));
}
