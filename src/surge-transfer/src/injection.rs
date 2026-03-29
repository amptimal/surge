// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Injection-capability screening types and entry points.

use serde::{Deserialize, Serialize};
use surge_network::Network;
use tracing::info;

use crate::dfax::PreparedTransferModel;
use crate::error::TransferError;

/// Per-bus injection capability (FERC Order 2023 heatmap / Worst Cluster TrLim).
///
/// For each bus `b` this is the maximum MW that can be injected before any
/// monitored element violates its post-contingency thermal rating.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectionCapabilityMap {
    /// `(bus_number, max_injection)` pairs, one per non-slack bus.
    pub by_bus: Vec<(u32, f64)>,
    /// Branch indices of contingencies that failed during evaluation.
    /// When non-empty, the `by_bus` limits for buses affected by these
    /// contingencies are conservatively set to zero.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_contingencies: Vec<usize>,
}

/// Options controlling injection-capability screening scope and accuracy.
#[derive(Debug, Clone)]
pub struct InjectionCapabilityOptions {
    /// Branches monitored for thermal constraints. `None` means all in-service
    /// branches with positive thermal ratings.
    pub monitored_branches: Option<Vec<usize>>,
    /// Candidate outage branches for the N-1 pass. `None` means all in-service
    /// AC branches with non-zero reactance.
    pub contingency_branches: Option<Vec<usize>>,
    /// Emergency rating factor applied post-contingency.
    pub post_contingency_rating_fraction: f64,
    /// When `true`, re-solve the post-contingency DC model exactly per outage.
    /// When `false`, use first-order LODF screening.
    pub exact: bool,
    /// Sensitivity slack policy for PTDF computation.
    ///
    /// When `Some`, uses distributed-slack D-PTDF instead of single-slack PTDF.
    /// This enables all buses (including the angle reference) to have non-zero
    /// injection capability. Weights are typically derived from generator AGC
    /// participation factors via `Network::agc_participation_by_bus()`.
    pub sensitivity_options: Option<surge_dc::DcSensitivityOptions>,
}

impl Default for InjectionCapabilityOptions {
    fn default() -> Self {
        Self {
            monitored_branches: None,
            contingency_branches: None,
            post_contingency_rating_fraction: 1.0,
            exact: false,
            sensitivity_options: None,
        }
    }
}

/// Compute per-bus injection capability (FERC Order 2023 heatmap / Worst Cluster TrLim).
///
/// For each non-slack bus `b`, computes the maximum MW injection before any
/// monitored element violates its post-contingency thermal rating.
pub fn compute_injection_capability(
    network: &Network,
    options: &InjectionCapabilityOptions,
) -> Result<InjectionCapabilityMap, TransferError> {
    let frac = options.post_contingency_rating_fraction;
    if !frac.is_finite() || frac <= 0.0 {
        return Err(TransferError::InvalidRequest(format!(
            "post_contingency_rating_fraction must be a positive finite number, got {frac}"
        )));
    }

    info!(
        buses = network.n_buses(),
        branches = network.n_branches(),
        post_ctg_rating_frac = frac,
        exact = options.exact,
        "computing injection capability map"
    );
    PreparedTransferModel::new(network)?.compute_injection_capability(options)
}
