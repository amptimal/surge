// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! P4 Extreme Event: Stuck Breaker (Breaker Failure) contingency analysis.
//!
//! NERC TPL-001-5.1 P4 requires evaluation of faults where the breaker
//! fails to clear, causing backup protection to trip all elements connected
//! through the stuck breaker's bus section.
//!
//! In a bus-branch model (no explicit breaker topology), a "stuck breaker at
//! bus B" is modeled as tripping **all** branches and generators connected
//! to bus B — the worst-case bus-section failure.
//!
//! Performance criteria per TPL Table 1:
//! - Evaluate for consequential load loss
//! - Evaluate for cascading (non-convergence)
//! - Planned/controlled load shedding is acceptable
//! - System instability is not required to be prevented
//!
//! # Algorithm
//! 1. [`generate_p4_stuck_breaker_contingencies`] produces one contingency per
//!    unique bus section (each branch endpoint). Contingencies are deduplicated.
//! 2. Contingencies with only 1 element (identical to N-1) are optionally
//!    filtered via `skip_simple_buses`.
//! 3. The universal [`analyze_contingencies`] solver handles parallel AC analysis
//!    with emergency thermal ratings.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use surge_network::Network;
use surge_network::network::generate_p4_stuck_breaker_contingencies;
use tracing::info;

use crate::{
    ContingencyAnalysis, ContingencyError, ContingencyOptions, ThermalRating, analyze_contingencies,
};

/// Options for P4 stuck-breaker contingency analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct P4Options {
    /// Base contingency analysis options (NR settings, screening, thermal rating).
    pub ca_options: ContingencyOptions,
    /// When true, only generate contingencies for buses with 2+ elements
    /// (buses with 1 element are identical to standard N-1).
    pub skip_simple_buses: bool,
}

impl Default for P4Options {
    fn default() -> Self {
        Self {
            ca_options: ContingencyOptions {
                thermal_rating: ThermalRating::RateC, // P4 uses emergency rating
                ..ContingencyOptions::default()
            },
            skip_simple_buses: true,
        }
    }
}

/// Result of P4 stuck-breaker analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct P4Result {
    /// Full contingency analysis results (reuses existing infrastructure).
    pub analysis: ContingencyAnalysis,
    /// Number of stuck-breaker contingencies generated (after filtering).
    pub n_contingencies: usize,
    /// Number of contingencies with at least one violation.
    pub n_with_violations: usize,
    /// Wall-clock time for the entire analysis.
    pub solve_time_secs: f64,
}

/// Run P4 stuck-breaker contingency analysis.
///
/// Generates stuck-breaker contingencies for every in-service branch endpoint,
/// optionally filters single-element contingencies (identical to N-1), then
/// runs steady-state contingency analysis via [`analyze_contingencies`].
pub fn analyze_p4(network: &Network, options: &P4Options) -> Result<P4Result, ContingencyError> {
    let start = Instant::now();

    let mut contingencies = generate_p4_stuck_breaker_contingencies(network);

    if options.skip_simple_buses {
        contingencies.retain(|c| c.branch_indices.len() + c.generator_indices.len() > 1);
    }

    let n_contingencies = contingencies.len();
    info!(
        n_contingencies,
        skip_simple = options.skip_simple_buses,
        "P4 stuck-breaker: running contingency analysis"
    );

    let analysis = analyze_contingencies(network, &contingencies, &options.ca_options)?;
    let n_with_violations = analysis
        .results
        .iter()
        .filter(|r| !r.violations.is_empty() || !r.converged)
        .count();

    let wall_time = start.elapsed().as_secs_f64();
    info!(
        n_contingencies,
        n_with_violations,
        solve_time_secs = format!("{wall_time:.3}"),
        "P4 stuck-breaker: analysis complete"
    );

    Ok(P4Result {
        analysis,
        n_contingencies,
        n_with_violations,
        solve_time_secs: wall_time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case_path(stem: &str) -> std::path::PathBuf {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let direct = workspace.join(format!("examples/cases/{stem}/{stem}.surge.json.zst"));
        if direct.exists() {
            return direct;
        }
        // case118 lives under ieee118/
        let num = stem.trim_start_matches("case");
        let alt = workspace.join(format!("examples/cases/ieee{num}/{stem}.surge.json.zst"));
        if alt.exists() {
            return alt;
        }
        direct
    }

    #[test]
    fn test_p4_options_default() {
        let opts = P4Options::default();
        assert!(opts.skip_simple_buses);
        assert_eq!(opts.ca_options.thermal_rating, ThermalRating::RateC);
    }

    #[test]
    fn test_p4_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = P4Options::default();
        let result = analyze_p4(&net, &opts).unwrap();

        // case9 has 9 buses and 9 branches — should produce some multi-element contingencies
        assert!(
            result.n_contingencies > 0,
            "should generate P4 contingencies"
        );
        assert!(
            result.solve_time_secs < 60.0,
            "should complete in reasonable time"
        );

        // All contingencies in results should have valid structure
        for r in &result.analysis.results {
            assert!(r.id.starts_with("p4_"), "P4 IDs should start with 'p4_'");
        }
    }

    #[test]
    fn test_p4_case14() {
        let net = surge_io::load(case_path("case14")).unwrap();
        let opts = P4Options::default();
        let result = analyze_p4(&net, &opts).unwrap();

        // case14 has higher connectivity — verify multi-element contingencies exist
        assert!(result.n_contingencies > 0);
        println!(
            "P4 case14: {} contingencies, {} with violations",
            result.n_contingencies, result.n_with_violations
        );
    }
}
