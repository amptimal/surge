// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical market formulation crate, built on top of [`surge_dispatch`].
//!
//! `surge-dispatch` is the canonical single-stage execution kernel:
//! prepare a [`surge_dispatch::DispatchModel`], build a
//! [`surge_dispatch::DispatchRequest`], then solve it.
//!
//! This crate hosts the canonical market formulation layer above that
//! kernel. It is the home of:
//!
//! - standard reserve product constructors (regulation, synchronous,
//!   non-synchronous, ramping, reactive headroom) — see [`reserves`]
//! - standard commitment helpers (initial conditions, startup tiers)
//!   — see [`commitment`]
//! - standard offer segment construction from piecewise cost data
//!   — see [`offers`]
//! - standard profile aggregation helpers — see [`profiles`]
//! - startup/shutdown trajectory derivation and online-status
//!   inference — see [`trajectory`]
//! - the canonical multi-stage market workflow (`MarketWorkflow`)
//!   — see [`workflow`]
//! - thin format adapters that translate specific market data formats
//!   into canonical inputs and typed solutions (today: GO Competition
//!   Challenge 3 — see [`go_c3`])
//!
//! The underlying optimization formulation — tiered startup costs, min
//! up/down time, zonal reserve requirements, ramp-shared energy-and-
//! reserve LPs, trajectory-aware startup/shutdown, AC commitment
//! refinement — is the canonical formulation for a properly-scoped
//! day-ahead/real-time power market. GO C3 is the first format adapter
//! plugged into this crate; further adapters (ERCOT, MISO, CAISO, etc.)
//! share the same canonical formulation layer.

pub mod ac_opf_presets;
pub mod ac_reconcile;
pub mod ac_refinement;
pub mod ac_sced_setup;
pub mod canonical_workflow;
pub mod commitment;
pub mod go_c3;
pub mod heuristics;
pub mod offers;
pub mod penalties;
pub mod profiles;
pub mod reserves;
pub mod trajectory;
pub mod two_stage;
pub mod windows;
pub mod workflow;

pub use ac_reconcile::{
    AcScedSetup, AcWarmStartConfig, DcReducedCostTargetTracking, LmpMarginalCostTargetTracking,
    ProducerDispatchPinning, RampLimits, TargetTrackingBoundPenalties,
    TargetTrackingPenaltyPresets, apply_ac_sced_setup, apply_producer_dispatch_pinning,
    apply_reactive_reserve_filter, build_ac_dispatch_warm_start, merge_commitment_augmentation,
};
pub use ac_refinement::{
    BandAttempt, CommitmentProbe, FeedbackCtx, FeedbackProvider, HvdcAttempt, HvdcStrategy,
    OpfAttempt, ProbeCtx, ProbeOutcome, RefinementAttemptReport, RefinementInputs,
    RefinementReport, RefinementRuntime, RetryPolicy,
};
pub use workflow::{
    MarketStage, MarketStageError, MarketStageResult, MarketStageRole, MarketWorkflow,
    MarketWorkflowResult, solve_market_workflow, solve_market_workflow_until,
    solve_market_workflow_with_options,
};
