// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 market adapter.
//!
//! Thin format-specific layer that bridges GO C3 problem/solution JSON
//! schemas and the canonical market formulation in [`crate`]. All
//! market-rule encoding — reserve products, startup tiers, piecewise
//! offers, commitment initial conditions, startup/shutdown
//! trajectories, AC SCED machinery, retry policy, feedback providers
//! — lives in the parent crate's canonical modules. This module reads
//! GO C3 field names and passes primitives into those modules.
//!
//! Three public entry points:
//!
//! * [`build_dispatch_request`] — GO C3 → [`DispatchRequest`]
//! * [`export_go_c3_solution`] — [`DispatchSolution`] → [`GoC3Solution`]
//! * [`build_canonical_workflow`] — canonical two-stage `DC SCUC + AC
//!   SCED` [`crate::workflow::MarketWorkflow`]

mod bus_profiles;
mod commitment;
mod dispatchable_loads;
mod export;
mod hvdc;
mod presets;
mod request;
mod reserves;
mod workflow;
mod zones;

pub use export::{export_go_c3_solution, export_go_c3_solution_with_reserve_source};
pub use presets::{
    apply_goc3_policy_to_ac_opf, apply_reactive_support_pin_to_request, goc3_ac_opf_options,
    goc3_bandable_criteria, goc3_classification, goc3_consumer_q_to_p_ratios,
    goc3_dispatch_pinning_bands, goc3_max_additional_bandable, goc3_opf_retry_attempts,
    goc3_peak_load, goc3_penalty_config, goc3_producer_ramp_limits,
    goc3_reactive_support_commitment_schedule, goc3_reactive_support_pin_criteria,
    goc3_reserve_product_ids, goc3_retry_policy, goc3_wide_q_anchor_criteria,
    merge_reactive_pin_must_runs,
};
pub use request::build_dispatch_request;
pub use workflow::{build_canonical_workflow, promote_q_capable_generators_to_pv};

/// Errors surfaced by the GO C3 request builder and solution exporter.
#[derive(Debug, thiserror::Error)]
pub enum GoC3DispatchError {
    #[error("GO C3 problem/context mismatch: {0}")]
    Mismatch(String),
    #[error("GO C3 dispatch solution export error: {0}")]
    Export(String),
}
