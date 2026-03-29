// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Contingency severity scoring and ranking utilities.

use crate::types::{ContingencyMetric, ContingencyResult, Violation};

// ---------------------------------------------------------------------------
// Severity scoring (CTG-10 helper)
// ---------------------------------------------------------------------------

/// Compute a scalar severity score for a contingency result used by top-K ranking.
///
/// Higher score = more severe.  Score semantics:
/// - `f64::INFINITY` — non-convergent contingency (most severe).
/// - `>= 100.0`      — thermal overload; score = worst `loading_pct`.
/// - `< 100.0`       — voltage violation only; score = max `|Δv| × 100`.
/// - `0.0`           — no violations.
pub(crate) fn contingency_severity_score(result: &ContingencyResult) -> f64 {
    let mut score = 0.0_f64;
    for v in &result.violations {
        let s = match v {
            Violation::NonConvergent { .. } => f64::INFINITY,
            Violation::ThermalOverload { loading_pct, .. } => *loading_pct,
            Violation::VoltageLow { vm, limit, .. } => (limit - vm).abs() * 100.0,
            Violation::VoltageHigh { vm, limit, .. } => (vm - limit).abs() * 100.0,
            // Islanding is serious — score as a large fixed penalty (worse than voltage,
            // not as bad as full non-convergence which gets INFINITY).
            Violation::Islanding { .. } => 500.0,
            Violation::FlowgateOverload { loading_pct, .. } => *loading_pct,
            Violation::InterfaceOverload { loading_pct, .. } => *loading_pct,
        };
        if s > score {
            score = s;
        }
    }
    score
}

// ---------------------------------------------------------------------------
// CTG-10: Top-K worst contingency ranking
// ---------------------------------------------------------------------------

/// CTG-10: Filter and rank contingency results by worst impact.
///
/// `metric` selects the ranking criterion:
/// - [`ContingencyMetric::MaxFlowPct`] — worst branch overload (% of rating); sorted descending
/// - [`ContingencyMetric::MinVoltagePu`] — worst low voltage (minimum bus voltage); sorted ascending
/// - [`ContingencyMetric::MaxVoltagePu`] — worst high voltage (maximum bus voltage); sorted descending
///
/// Returns the top-k results sorted by descending severity (largest impact first).
/// Unconverged results are always considered worse than converged ones.
pub fn rank_contingencies(
    results: &[ContingencyResult],
    metric: ContingencyMetric,
    k: usize,
) -> Vec<&ContingencyResult> {
    if results.is_empty() || k == 0 {
        return vec![];
    }

    // Compute a scalar severity for each result according to the chosen metric.
    // Higher value = worse (more severe) for all metrics:
    //   - MaxFlowPct: max loading_pct (already % overload — higher = worse)
    //   - MinVoltagePu: negate min voltage so lower voltage → higher score
    //   - MaxVoltagePu: max voltage (higher = worse)
    // Non-converged entries get f64::INFINITY so they always sort first.
    let score = |r: &ContingencyResult| -> f64 {
        if !r.converged {
            return f64::INFINITY;
        }
        match metric {
            ContingencyMetric::MaxFlowPct => r
                .violations
                .iter()
                .filter_map(|v| {
                    if let Violation::ThermalOverload { loading_pct, .. } = v {
                        Some(*loading_pct)
                    } else {
                        None
                    }
                })
                .fold(f64::NEG_INFINITY, f64::max)
                .max(0.0),
            ContingencyMetric::MinVoltagePu => {
                // Negate min voltage: lower voltage → higher (worse) score.
                let min_vm = r
                    .violations
                    .iter()
                    .filter_map(|v| {
                        if let Violation::VoltageLow { vm, .. } = v {
                            Some(*vm)
                        } else {
                            None
                        }
                    })
                    .fold(f64::INFINITY, f64::min);
                if min_vm.is_finite() { -min_vm } else { 0.0 }
            }
            ContingencyMetric::MaxVoltagePu => r
                .violations
                .iter()
                .filter_map(|v| {
                    if let Violation::VoltageHigh { vm, .. } = v {
                        Some(*vm)
                    } else {
                        None
                    }
                })
                .fold(f64::NEG_INFINITY, f64::max)
                .max(0.0),
        }
    };

    let mut indexed: Vec<(usize, f64)> = results
        .iter()
        .enumerate()
        .map(|(i, r)| (i, score(r)))
        .collect();

    // Sort descending by severity score.
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    indexed
        .into_iter()
        .take(k)
        .map(|(i, _)| &results[i])
        .collect()
}
