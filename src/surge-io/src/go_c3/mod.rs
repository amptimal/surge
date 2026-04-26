// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 format support.
//!
//! Provides typed serde structs for the GO C3 problem and solution JSON
//! schemas, a lossless conversion to `surge_network::Network`, and
//! solution serialization back to GO C3 format.
//!
//! # Usage
//!
//! ```no_run
//! use surge_io::go_c3;
//!
//! // Load a GO C3 scenario file.
//! let problem = go_c3::load_problem("scenario_303.json").unwrap();
//!
//! // Convert to a Surge network + context with default policy.
//! let (network, context) = go_c3::to_network(&problem).unwrap();
//!
//! println!("{} buses", network.n_buses());
//! ```

mod context;
mod enrich;
mod hvdc_q;
mod issues;
mod network;
mod policy;
mod reserves;
pub mod types;
mod voltage;

pub use context::{
    AcLineInitialState, BranchRef, DcLineInitialState, DcLineReactiveBounds,
    DcLineReactiveSupportResources, GoC3Context, GoC3DeviceKind, TransformerInitialState,
};
pub use enrich::enrich_network;
pub use hvdc_q::apply_hvdc_reactive_terminals;
pub use issues::{GoC3Issue, GoC3IssueSeverity};
pub use network::{to_network, to_network_with_policy};
pub use policy::{
    GoC3AcReconcileMode, GoC3CommitmentMode, GoC3ConsumerMode, GoC3Formulation, GoC3Policy,
    GoC3ScucLossTreatment, GoC3SlackInferenceMode,
};
pub use reserves::apply_reserves;
pub use types::*;
pub use voltage::apply_voltage_regulation;

use std::io::BufReader;
use std::path::Path;

use thiserror::Error;

/// Errors from GO C3 I/O operations.
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("GO C3 conversion error: {0}")]
    Conversion(String),
}

/// Load a GO C3 problem JSON file.
pub fn load_problem(path: impl AsRef<Path>) -> Result<GoC3Problem, Error> {
    let path = path.as_ref();
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let problem: GoC3Problem = serde_json::from_reader(reader)?;
    Ok(problem)
}

/// Parse a GO C3 problem from an in-memory JSON string.
pub fn load_problem_str(content: &str) -> Result<GoC3Problem, Error> {
    let problem: GoC3Problem = serde_json::from_str(content)?;
    Ok(problem)
}

/// Load a GO C3 problem and convert it to a [`surge_network::Network`] with
/// default policy.
pub fn load_network(
    path: impl AsRef<Path>,
) -> Result<(surge_network::Network, GoC3Context), Error> {
    let problem = load_problem(path)?;
    to_network(&problem)
}

/// Load a GO C3 problem, convert it to a network, and run the full
/// network-build pipeline (enrich → reserves).
///
/// This is the single call most callers want. After this returns, the
/// [`Network`] carries all the operational metadata the Python adapter
/// writes in `build_surge_network`:
///
/// * Generator pmin/pmax/qmin/qmax from the time-series envelope
/// * Commitment params (min up/down, startup/shutdown ramps, quick-start)
/// * Ramp curves
/// * Startup cost tiers (via `MarketParams::energy_offer`)
/// * Per-producer reserve offers (via `MarketParams::reserve_offers`)
/// * Slack bus inferred by reactive capability
/// * Per-side branch shunts and transition costs (via structural pass)
///
/// Policy-sensitive enrichments that depend on the dispatch-request types
/// (reserve product definitions, zonal requirements) live in
/// `surge-dispatch::go_c3` and are applied at request-build time.
pub fn load_enriched_network(
    path: impl AsRef<Path>,
    policy: &GoC3Policy,
) -> Result<(surge_network::Network, GoC3Context), Error> {
    let problem = load_problem(path)?;
    let (mut network, mut context) = to_network_with_policy(&problem, policy)?;
    enrich_network(&mut network, &mut context, &problem, policy)?;
    apply_reserves(&mut network, &mut context, &problem)?;
    // HVDC reactive terminals must run before voltage regulation so the
    // synthetic generators are present when voltage walks the adjacency
    // graph. They're flagged excluded so the fallback won't pick them.
    apply_hvdc_reactive_terminals(&mut network, &mut context, &problem, policy)?;
    apply_voltage_regulation(&mut network, &mut context, &problem, policy)?;
    Ok((network, context))
}

/// Serialize a GO C3 solution to a JSON file.
pub fn save_solution(solution: &GoC3Solution, path: impl AsRef<Path>) -> Result<(), Error> {
    let file = std::fs::File::create(path.as_ref())?;
    serde_json::to_writer_pretty(file, solution)?;
    Ok(())
}

/// Serialize a GO C3 solution to a JSON string.
pub fn dumps_solution(solution: &GoC3Solution) -> Result<String, Error> {
    let json = serde_json::to_string_pretty(solution)?;
    Ok(json)
}
