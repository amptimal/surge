// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! P6 Extreme Event: Two Overlapping Singles contingency analysis.
//!
//! NERC TPL-001-5.1 P6 requires evaluation of simultaneous loss of two
//! independent elements that are physically related:
//! - **P6a**: Two elements on the same tower or structure.
//! - **P6b**: Two elements in a common corridor or right-of-way.
//! - **P6c**: Two parallel circuits (same from/to bus pair).
//!
//! P6c (parallel circuits) can be auto-detected from the network model.
//! P6a and P6b require engineering judgment and are supplied as user-defined
//! branch pair lists.
//!
//! Performance criteria per TPL Table 1 (same as P4):
//! - Evaluate for consequential load loss and cascading
//! - Planned/controlled load shedding is acceptable
//! - System instability is not required to be prevented
//!
//! # Algorithm
//! 1. Auto-detect parallel circuits via [`generate_p6_parallel_contingencies`].
//! 2. Merge with user-specified same-tower and common-corridor pairs.
//! 3. Run all N-2 contingencies through [`analyze_contingencies`].

use std::time::Instant;

use serde::{Deserialize, Serialize};
use surge_network::Network;
use surge_network::network::{
    TplCategory, generate_p6_parallel_contingencies, generate_p6_user_pairs,
};
use tracing::info;

use crate::{
    ContingencyAnalysis, ContingencyError, ContingencyOptions, ThermalRating, analyze_contingencies,
};

/// Options for P6 overlapping-singles contingency analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct P6Options {
    /// Base contingency analysis options.
    pub ca_options: ContingencyOptions,
    /// Include auto-detected parallel circuits (P6c). Default: true.
    pub include_parallel: bool,
    /// User-specified same-tower pairs as `(branch_idx_a, branch_idx_b)`.
    pub same_tower_pairs: Vec<(usize, usize)>,
    /// User-specified common-corridor pairs as `(branch_idx_a, branch_idx_b)`.
    pub common_corridor_pairs: Vec<(usize, usize)>,
}

impl Default for P6Options {
    fn default() -> Self {
        Self {
            ca_options: ContingencyOptions {
                thermal_rating: ThermalRating::RateC, // P6 uses emergency rating
                ..ContingencyOptions::default()
            },
            include_parallel: true,
            same_tower_pairs: vec![],
            common_corridor_pairs: vec![],
        }
    }
}

/// Result of P6 overlapping-singles analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct P6Result {
    /// Full contingency analysis results.
    pub analysis: ContingencyAnalysis,
    /// Number of P6c parallel-circuit contingencies.
    pub n_parallel: usize,
    /// Number of P6a same-tower contingencies.
    pub n_same_tower: usize,
    /// Number of P6b common-corridor contingencies.
    pub n_common_corridor: usize,
    /// Total contingencies with at least one violation.
    pub n_with_violations: usize,
    /// Wall-clock time.
    pub solve_time_secs: f64,
}

/// Run P6 overlapping-singles contingency analysis.
///
/// Combines auto-detected parallel circuits (P6c) with user-specified
/// same-tower (P6a) and common-corridor (P6b) pairs, then runs
/// steady-state contingency analysis via [`analyze_contingencies`].
pub fn analyze_p6(network: &Network, options: &P6Options) -> Result<P6Result, ContingencyError> {
    let start = Instant::now();
    let mut contingencies = Vec::new();

    // P6c: auto-detect parallel circuits
    let n_parallel = if options.include_parallel {
        let parallel = generate_p6_parallel_contingencies(network);
        let n = parallel.len();
        contingencies.extend(parallel);
        n
    } else {
        0
    };

    // P6a: user-specified same-tower pairs
    let tower =
        generate_p6_user_pairs(network, &options.same_tower_pairs, TplCategory::P6SameTower);
    let n_same_tower = tower.len();
    contingencies.extend(tower);

    // P6b: user-specified common-corridor pairs
    let corridor = generate_p6_user_pairs(
        network,
        &options.common_corridor_pairs,
        TplCategory::P6CommonCorridor,
    );
    let n_common_corridor = corridor.len();
    contingencies.extend(corridor);

    let total = contingencies.len();
    info!(
        total,
        n_parallel, n_same_tower, n_common_corridor, "P6 overlapping: running contingency analysis"
    );

    let analysis = analyze_contingencies(network, &contingencies, &options.ca_options)?;
    let n_with_violations = analysis
        .results
        .iter()
        .filter(|r| !r.violations.is_empty() || !r.converged)
        .count();

    let wall_time = start.elapsed().as_secs_f64();
    info!(
        total,
        n_with_violations,
        solve_time_secs = format!("{wall_time:.3}"),
        "P6 overlapping: analysis complete"
    );

    Ok(P6Result {
        analysis,
        n_parallel,
        n_same_tower,
        n_common_corridor,
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
    fn test_p6_options_default() {
        let opts = P6Options::default();
        assert!(opts.include_parallel);
        assert!(opts.same_tower_pairs.is_empty());
        assert!(opts.common_corridor_pairs.is_empty());
        assert_eq!(opts.ca_options.thermal_rating, ThermalRating::RateC);
    }

    #[test]
    fn test_p6_case9_no_parallel() {
        // case9 has no parallel circuits
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = P6Options::default();
        let result = analyze_p6(&net, &opts).unwrap();

        assert_eq!(result.n_parallel, 0, "case9 has no parallel circuits");
        assert_eq!(result.n_same_tower, 0);
        assert_eq!(result.n_common_corridor, 0);
    }

    #[test]
    fn test_p6_with_user_pairs() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = P6Options {
            same_tower_pairs: vec![(0, 1), (2, 3)],
            ..P6Options::default()
        };
        let result = analyze_p6(&net, &opts).unwrap();

        assert_eq!(
            result.n_same_tower, 2,
            "two user-specified same-tower pairs"
        );
        assert!(result.solve_time_secs < 60.0);
    }
}
