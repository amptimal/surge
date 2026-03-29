// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CTG-06: Probabilistic N-1 contingency analysis — LOLE / EENS.
//!
//! For each branch in the network, treats its forced outage rate (FOR) as the
//! probability that it is unavailable during any given hour.  Computes
//! Expected Energy Not Served (EENS) caused by thermal overloads that appear
//! when each branch is individually outaged.
//!
//! # Algorithm
//!
//! 1. Solve base-case DC power flow once to obtain pre-contingency flows.
//! 2. Stream one LODF outage column at a time from the prepared DC model.
//! 3. For each contingency branch `k` with FOR `p_k`:
//!    - Estimate post-outage flows using the LODF formula:
//!      `flow_j_post = flow_j_pre + LODF[j,k] × flow_k_pre`
//!    - Find overloaded branches:
//!      `overload_j = max(0, |flow_j_post| × base_mva − rate_a_j)` \[MW\]
//!    - EENS contribution:
//!      `EENS_k = p_k × Σ_j overload_j × hours_per_year` [MWh/year]
//! 4. Aggregate results and sort by EENS descending.

use serde::{Deserialize, Serialize};
use surge_dc::{PreparedDcStudy, solve_dc};
use surge_network::Network;

use crate::{ThermalRating, get_rating};

// ── Options ──────────────────────────────────────────────────────────────────

/// Options for probabilistic N-1 contingency analysis.
#[derive(Debug, Clone)]
pub struct BranchEensOptions {
    /// Forced outage rate (FOR) for each branch (0.0 to 1.0).
    ///
    /// Length must equal `network.n_branches()`.  When the vector is empty the
    /// default FOR of 0.01 (1%) is applied uniformly to every branch.
    pub branch_for: Vec<f64>,

    /// Number of hours per year used to convert per-unit overload to energy
    /// (default 8760).
    pub hours_per_year: f64,

    /// Minimum overload threshold in MW before a branch is counted.
    ///
    /// Overloads smaller than this value are treated as zero.  Useful for
    /// filtering out numerical noise on lightly loaded branches.
    pub overload_threshold_mw: f64,

    /// Thermal rating tier for overload detection.
    ///
    /// NERC TPL-001 allows emergency ratings (Rate B or C) for post-contingency
    /// thermal checks.  Default: `RateA` (long-term continuous rating).
    pub thermal_rating: ThermalRating,
}

impl Default for BranchEensOptions {
    fn default() -> Self {
        Self {
            branch_for: vec![],
            hours_per_year: 8760.0,
            overload_threshold_mw: 0.0,
            thermal_rating: ThermalRating::default(),
        }
    }
}

// ── Results ───────────────────────────────────────────────────────────────────

/// Probabilistic N-1 result for a single contingency branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchEensResult {
    /// Internal (0-based) index of the outaged branch.
    pub branch_index: usize,

    /// Human-readable label: `"Line {from}->{to}(ckt {circuit})"`.
    pub branch_id: String,

    /// Forced outage rate assigned to this branch.
    pub for_rate: f64,

    /// `true` when at least one monitored branch is overloaded post-contingency.
    pub has_overload: bool,

    /// Expected Energy Not Served attributable to this contingency (MWh/year).
    ///
    /// `EENS_k = FOR_k × Σ_j overload_j_MW × hours_per_year`
    pub eens_mwh_per_year: f64,

    /// P1-012: `true` when this branch is a bridge line (its removal creates
    /// electrical islands).  Bridge line EENS is reported separately in
    /// [`BranchEensSummary::bridge_line_eens_mwh_per_year`] and excluded
    /// from `total_eens_mwh_per_year` so a single sentinel does not dominate
    /// the system aggregate.
    pub is_bridge: bool,
}

/// System-level summary of the probabilistic N-1 analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchEensSummary {
    /// Per-contingency results, sorted by `eens_mwh_per_year` descending.
    pub results: Vec<BranchEensResult>,

    /// Sum of `eens_mwh_per_year` over all non-bridge contingencies (MWh/year).
    ///
    /// P1-012: Bridge lines are excluded from this aggregate and reported
    /// separately in `bridge_line_eens_mwh_per_year` so that the sentinel
    /// value (1e12 MWh/yr) assigned to bridge lines does not dominate the
    /// system-level result.
    pub total_eens_mwh_per_year: f64,

    /// P1-012: Sum of EENS for bridge line contingencies (MWh/year).
    ///
    /// Bridge lines are those whose removal creates electrical islands
    /// (LODF diagonal is infinite).  These are reported separately because
    /// a meaningful EENS estimate requires a full island load-shedding
    /// calculation, not the placeholder sentinel used for sorting.
    pub bridge_line_eens_mwh_per_year: f64,

    /// Weighted average overload rate:
    /// `Σ_k (FOR_k × has_overload_k) / Σ_k FOR_k`.
    ///
    /// Represents the fraction of forced-outage hours during which at least
    /// one branch is overloaded.
    pub weighted_overload_rate: f64,
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Run probabilistic N-1 analysis on `network`.
///
/// # Panics
///
/// Panics if the base-case DC power flow fails to converge (disconnected or
/// degenerate network).
///
/// # Performance
///
/// LODF computation is O(n_branches²).  For very large networks (> 50 k
/// branches) use a sparse LODF approach instead.
pub fn analyze_branch_eens(network: &Network, opts: &BranchEensOptions) -> BranchEensSummary {
    let n_br = network.n_branches();
    let base_mva = network.base_mva;

    // ── 1. Base-case DC power flow ────────────────────────────────────────────
    // Return an empty summary if the network is degenerate (no branches,
    // disconnected, or solver failure) rather than panicking.
    let dc = match solve_dc(network) {
        Ok(sol) => sol,
        Err(_) => {
            return BranchEensSummary {
                results: Vec::new(),
                total_eens_mwh_per_year: 0.0,
                bridge_line_eens_mwh_per_year: 0.0,
                weighted_overload_rate: 0.0,
            };
        }
    };

    // branch_p_flow is in per-unit; convert to MW for EENS calculation.
    let base_flows_pu: &[f64] = &dc.branch_p_flow;

    // ── 2. Prepare streamed LODF columns ─────────────────────────────────────
    let all_branches: Vec<usize> = (0..n_br).collect();
    let mut dc_model = match PreparedDcStudy::new(network) {
        Ok(model) => model,
        Err(_) => {
            return BranchEensSummary {
                results: Vec::new(),
                total_eens_mwh_per_year: 0.0,
                bridge_line_eens_mwh_per_year: 0.0,
                weighted_overload_rate: 0.0,
            };
        }
    };
    let mut lodf_columns = dc_model.lodf_columns();

    // ── 3. Resolve FOR rates ──────────────────────────────────────────────────
    let for_rates: Vec<f64> = if opts.branch_for.is_empty() {
        vec![0.01; n_br]
    } else {
        assert_eq!(
            opts.branch_for.len(),
            n_br,
            "branch_for length ({}) must equal n_branches ({})",
            opts.branch_for.len(),
            n_br
        );
        opts.branch_for.clone()
    };

    // ── 4. Per-contingency EENS ───────────────────────────────────────────────
    let mut results: Vec<BranchEensResult> = Vec::with_capacity(n_br);

    for k in 0..n_br {
        let branch_k = &network.branches[k];

        // Skip out-of-service branches — they cannot be "outaged" further.
        if !branch_k.in_service {
            continue;
        }

        let for_k = for_rates[k];
        let branch_id = format!(
            "Line {}->{}(ckt {})",
            branch_k.from_bus, branch_k.to_bus, branch_k.circuit
        );

        let lodf_col = match lodf_columns.compute_column(&all_branches, k) {
            Ok(column) => column,
            Err(_) => {
                continue;
            }
        };

        // Bridge lines have LODF = ∞ — outaging them would disconnect the
        // network. Use a large finite sentinel (1e12 MWh/yr) so results
        // remain sortable and the total_eens sum stays finite even when
        // for_k = 0 (avoids NaN from 0 × ∞).
        if lodf_col[k].is_infinite() {
            const BRIDGE_EENS_SENTINEL: f64 = 1e12; // MWh/year
            results.push(BranchEensResult {
                branch_index: k,
                branch_id,
                for_rate: for_k,
                has_overload: true,
                eens_mwh_per_year: if for_k > 0.0 {
                    BRIDGE_EENS_SENTINEL
                } else {
                    0.0
                },
                is_bridge: true,
            });
            continue;
        }

        // Compute post-outage overload summed over all monitored branches.
        let mut total_overload_mw = 0.0_f64;

        for j in 0..n_br {
            if j == k {
                continue; // outaged branch carries zero flow
            }
            let branch_j = &network.branches[j];
            if !branch_j.in_service {
                continue;
            }

            let lodf_jk = lodf_col[j];
            if lodf_jk.is_infinite() {
                // LODF is ∞ only when l is also a bridge-connected to k;
                // treat as full disconnection — add infinite overload.
                total_overload_mw = f64::INFINITY;
                break;
            }

            // Post-contingency flow in per-unit, then convert to MW.
            let flow_j_post_mw = (base_flows_pu[j] + lodf_jk * base_flows_pu[k]).abs() * base_mva;

            // Thermal rating for branch j (selected tier, 0 means unconstrained).
            let rating_j_mw = get_rating(branch_j, opts.thermal_rating);
            if rating_j_mw <= 0.0 {
                continue; // unconstrained — skip
            }

            let overload_mw = (flow_j_post_mw - rating_j_mw).max(0.0);
            if overload_mw > opts.overload_threshold_mw {
                total_overload_mw += overload_mw;
            }
        }

        let eens = for_k * total_overload_mw * opts.hours_per_year;
        let has_overload = total_overload_mw > 0.0;

        results.push(BranchEensResult {
            branch_index: k,
            branch_id,
            for_rate: for_k,
            has_overload,
            eens_mwh_per_year: eens,
            is_bridge: false,
        });
    }

    // ── 5. Sort by EENS descending (infinite values first) ───────────────────
    results.sort_by(|a, b| {
        b.eens_mwh_per_year
            .partial_cmp(&a.eens_mwh_per_year)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // ── 6. System totals ──────────────────────────────────────────────────────
    // P1-012: Separate bridge line EENS from the aggregate total.  Bridge
    // lines use a sentinel value (1e12 MWh/yr) that would overwhelm the
    // system EENS if included.  Report them separately so operators can
    // see which lines are critical topology elements without polluting the
    // actionable EENS metric.
    let total_eens: f64 = results
        .iter()
        .filter(|r| !r.is_bridge)
        .map(|r| r.eens_mwh_per_year)
        .sum();
    let bridge_eens: f64 = results
        .iter()
        .filter(|r| r.is_bridge)
        .map(|r| r.eens_mwh_per_year)
        .sum();

    let sum_for: f64 = results.iter().map(|r| r.for_rate).sum();
    let weighted_overload_rate = if sum_for > 0.0 {
        results
            .iter()
            .map(|r| r.for_rate * if r.has_overload { 1.0 } else { 0.0 })
            .sum::<f64>()
            / sum_for
    } else {
        0.0
    };

    BranchEensSummary {
        results,
        total_eens_mwh_per_year: total_eens,
        bridge_line_eens_mwh_per_year: bridge_eens,
        weighted_overload_rate,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::case_path;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    /// Build a simple 3-bus triangle network:
    ///
    /// ```text
    ///   Bus 1 (slack, gen 150 MW)
    ///      ↓ branch 0: x=0.1, rate_a=100 MW
    ///   Bus 2
    ///      ↓ branch 1: x=0.2, rate_a=200 MW
    ///   Bus 3 (load=100 MW)
    ///      ↗ branch 2: x=0.15, rate_a=150 MW (bus 3 → bus 1, closing the triangle)
    /// ```
    ///
    /// The triangle topology ensures no bridge lines so all LODF values are
    /// finite.
    fn make_3bus_triangle(br0_rate_a: f64) -> Network {
        let mut net = Network::new("ctg06_3bus_triangle");
        net.base_mva = 100.0;

        // Bus 1: slack
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        // Bus 2: PQ, no load
        let bus2 = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(bus2);

        // Bus 3: PQ, 100 MW load
        let bus3 = Bus::new(3, BusType::PQ, 138.0);
        net.buses.push(bus3);
        net.loads.push(Load::new(3, 100.0, 0.0));

        // Generator at bus 1: 150 MW
        let generator = Generator::new(1, 1.5, 1.0); // pg in p.u. (150 MW)
        net.generators.push(generator);

        // Branch 0: bus1 → bus2, configurable tight rating
        let mut br0 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br0.rating_a_mva = br0_rate_a;
        br0.rating_b_mva = br0_rate_a;
        br0.rating_c_mva = br0_rate_a;
        net.branches.push(br0);

        // Branch 1: bus2 → bus3, generous rating
        let mut br1 = Branch::new_line(2, 3, 0.0, 0.2, 0.0);
        br1.rating_a_mva = 200.0;
        br1.rating_b_mva = 200.0;
        br1.rating_c_mva = 200.0;
        net.branches.push(br1);

        // Branch 2: bus3 → bus1, moderate rating (closing the triangle)
        let mut br2 = Branch::new_line(3, 1, 0.0, 0.15, 0.0);
        br2.rating_a_mva = 150.0;
        br2.rating_b_mva = 150.0;
        br2.rating_c_mva = 150.0;
        net.branches.push(br2);

        net
    }

    /// CTG-06-A: Basic sanity — function runs and produces one result per
    /// in-service branch.
    #[test]
    fn test_ctg06_result_count() {
        let net = make_3bus_triangle(100.0);
        let opts = BranchEensOptions::default();
        let summary = analyze_branch_eens(&net, &opts);
        assert_eq!(
            summary.results.len(),
            net.n_branches(),
            "one result per branch"
        );
    }

    /// CTG-06-B: Results are sorted by EENS descending.
    #[test]
    fn test_ctg06_sorted_by_eens() {
        let net = make_3bus_triangle(100.0);
        let opts = BranchEensOptions::default();
        let summary = analyze_branch_eens(&net, &opts);
        for w in summary.results.windows(2) {
            assert!(
                w[0].eens_mwh_per_year >= w[1].eens_mwh_per_year
                    || !w[0].eens_mwh_per_year.is_finite(),
                "results must be sorted EENS descending: {} < {}",
                w[0].eens_mwh_per_year,
                w[1].eens_mwh_per_year
            );
        }
    }

    /// CTG-06-C: Total EENS equals the sum of individual non-bridge contributions.
    /// Bridge line EENS is reported separately (P1-012).
    #[test]
    fn test_ctg06_total_eens_consistent() {
        let net = make_3bus_triangle(100.0);
        let opts = BranchEensOptions::default();
        let summary = analyze_branch_eens(&net, &opts);

        let manual_non_bridge: f64 = summary
            .results
            .iter()
            .filter(|r| !r.is_bridge)
            .map(|r| r.eens_mwh_per_year)
            .sum();
        let diff = (summary.total_eens_mwh_per_year - manual_non_bridge).abs();
        assert!(
            diff < 1e-6 || !manual_non_bridge.is_finite(),
            "total EENS mismatch: {} vs {}",
            summary.total_eens_mwh_per_year,
            manual_non_bridge
        );

        // Bridge EENS should also be consistent.
        let manual_bridge: f64 = summary
            .results
            .iter()
            .filter(|r| r.is_bridge)
            .map(|r| r.eens_mwh_per_year)
            .sum();
        let diff_bridge = (summary.bridge_line_eens_mwh_per_year - manual_bridge).abs();
        assert!(
            diff_bridge < 1e-6 || !manual_bridge.is_finite(),
            "bridge EENS mismatch: {} vs {}",
            summary.bridge_line_eens_mwh_per_year,
            manual_bridge
        );
    }

    /// CTG-06-D: With a very tight rating on branch 0 (10 MW), outaging other
    /// branches should push flow above the 10 MW rating, producing EENS > 0.
    #[test]
    fn test_ctg06_overload_produces_eens() {
        // branch 0 is extremely tight — will overload under N-1
        let net = make_3bus_triangle(10.0);

        let opts = BranchEensOptions {
            branch_for: vec![0.01, 0.01, 0.05], // higher FOR on branch 2
            hours_per_year: 8760.0,
            overload_threshold_mw: 0.0,
            ..Default::default()
        };
        let summary = analyze_branch_eens(&net, &opts);

        assert!(
            summary.total_eens_mwh_per_year > 0.0,
            "expected EENS > 0 when a tight-rated branch can overload under N-1, got {}",
            summary.total_eens_mwh_per_year
        );
        assert!(
            summary.results.iter().any(|r| r.has_overload),
            "at least one contingency should flag an overload"
        );
    }

    /// CTG-06-E: Weighted overload rate is between 0 and 1.
    #[test]
    fn test_ctg06_weighted_overload_rate_bounds() {
        let net = make_3bus_triangle(100.0);
        let opts = BranchEensOptions::default();
        let summary = analyze_branch_eens(&net, &opts);
        assert!(
            (0.0..=1.0).contains(&summary.weighted_overload_rate),
            "weighted_overload_rate {} not in [0, 1]",
            summary.weighted_overload_rate
        );
    }

    /// CTG-06-F: FOR vector length mismatch panics with a clear message.
    #[test]
    #[should_panic(expected = "branch_for length")]
    fn test_ctg06_for_length_mismatch_panics() {
        let net = make_3bus_triangle(100.0);
        let opts = BranchEensOptions {
            branch_for: vec![0.01, 0.02], // wrong length (should be 3)
            ..Default::default()
        };
        analyze_branch_eens(&net, &opts);
    }

    /// CTG-06-G: case9 integration — runs without panic, produces one result
    /// per branch, EENS is non-negative.
    #[test]
    fn test_ctg06_case9() {
        let net = surge_io::load(case_path("case9")).expect("parse case9");
        let opts = BranchEensOptions::default();
        let summary = analyze_branch_eens(&net, &opts);
        assert_eq!(summary.results.len(), net.n_branches());
        assert!(summary.total_eens_mwh_per_year >= 0.0);
    }

    /// P1-012: Bridge line EENS is reported separately and does not pollute
    /// the system total_eens.  Build a 3-bus chain (bus1 - bus2 - bus3) where
    /// branch 0 (bus1-bus2) is a bridge line.  Its sentinel EENS must appear
    /// in bridge_line_eens, not in total_eens.
    #[test]
    fn test_p1_012_bridge_eens_separated() {
        let mut net = Network::new("bridge_eens_test");
        net.base_mva = 100.0;

        // Linear chain: 1 — 2 — 3 (no triangle, so both branches are bridges).
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);
        net.buses = vec![b1, b2, b3];
        net.loads.push(Load::new(3, 50.0, 0.0));

        let g1 = Generator::new(1, 1.0, 1.0);
        net.generators = vec![g1];

        let mut br0 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br0.rating_a_mva = 200.0;
        let mut br1 = Branch::new_line(2, 3, 0.0, 0.2, 0.0);
        br1.rating_a_mva = 200.0;
        net.branches = vec![br0, br1];

        let opts = BranchEensOptions {
            branch_for: vec![0.01, 0.01],
            ..Default::default()
        };
        let summary = analyze_branch_eens(&net, &opts);

        // Both branches are bridges in a linear chain.
        let n_bridges = summary.results.iter().filter(|r| r.is_bridge).count();
        assert!(
            n_bridges > 0,
            "expected at least one bridge line in a linear chain, got 0"
        );

        // total_eens should exclude bridge lines.
        assert_eq!(
            summary.total_eens_mwh_per_year, 0.0,
            "total_eens should be 0 when all contingencies are bridge lines, got {}",
            summary.total_eens_mwh_per_year
        );

        // bridge_line_eens should be > 0 (sentinel values for bridge lines with FOR > 0).
        assert!(
            summary.bridge_line_eens_mwh_per_year > 0.0,
            "bridge_line_eens should be > 0 for bridge lines with FOR > 0, got {}",
            summary.bridge_line_eens_mwh_per_year
        );

        // Verify the sentinel is NOT in total_eens.
        assert!(
            summary.total_eens_mwh_per_year < 1e6,
            "total_eens should not contain bridge sentinel values, got {}",
            summary.total_eens_mwh_per_year
        );
    }
}

// ── CTG-06: Simple probabilistic contingency API ──────────────────────────────
//
// Provides a lightweight interface for computing LOLE/EENS from a list of
// contingency descriptors with pre-computed forced outage rates and impact
// estimates. This API is suitable for post-processing N-1 screening results
// without re-running power flow.

/// Input descriptor for a single contingency in the probabilistic N-1 analysis.
#[derive(Debug, Clone)]
pub struct ReliabilityCtgDescriptor {
    /// Human-readable contingency identifier (e.g. "Line 1->2(ckt 1)").
    pub contingency_name: String,

    /// Forced outage rate — probability the element is unavailable (0.0 to 1.0).
    pub forced_outage_rate: f64,

    /// MW of load shedding required to restore feasibility post-contingency.
    /// Zero means the contingency causes no load shedding.
    pub impact_mw: f64,

    /// Expected hours of outage per trip event.
    pub unserved_hours_per_trip: f64,
}

/// System-level probabilistic reliability indices from a set of contingencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReliabilityIndices {
    /// Loss of Load Probability — fraction of time with a shortfall.
    pub lolp: f64,

    /// Loss of Load Expectation in days per year.
    pub lole_days_per_year: f64,

    /// Expected Unserved Energy in MWh per year.
    pub eens_mwh_per_year: f64,

    /// Contingency contributions sorted by `FOR × impact_mw` descending.
    /// Each entry is `(contingency_name, lole_contribution)`.
    pub critical_elements: Vec<(String, f64)>,

    /// Total number of contingencies provided (including zero-impact ones).
    pub n_contingencies: usize,
}

/// Compute probabilistic N-1 reliability indices from a list of contingency
/// descriptors.
///
/// # Formula
///
/// - `LOLP = Σ_k FOR_k  (for k where impact_mw_k > 0)`,  capped at 1.0
/// - `LOLE = LOLP × 8760 / 24`  (days/year)
/// - `EENS = Σ_k FOR_k × impact_mw_k × unserved_hours_per_trip_k`
///
/// # Notes
///
/// LOLP is the sum of independent failure probabilities under the rare-event
/// approximation (FOR ≪ 1 for each element). The result is clamped to [0, 1].
pub fn compute_reliability_indices(
    contingencies: &[ReliabilityCtgDescriptor],
) -> ReliabilityIndices {
    // LOLP: sum of FOR for contingencies that cause load shedding.
    let lolp: f64 = contingencies
        .iter()
        .filter(|c| c.impact_mw > 0.0)
        .map(|c| c.forced_outage_rate)
        .sum::<f64>()
        .min(1.0);

    let lole_days_per_year = lolp * 8760.0 / 24.0;

    // EENS: sum over all contingencies (zero-impact ones contribute 0).
    let eens_mwh_per_year: f64 = contingencies
        .iter()
        .map(|c| c.forced_outage_rate * c.impact_mw * c.unserved_hours_per_trip)
        .sum();

    // Critical elements: sort by FOR × impact descending.
    let mut critical_elements: Vec<(String, f64)> = contingencies
        .iter()
        .map(|c| {
            (
                c.contingency_name.clone(),
                c.forced_outage_rate * c.impact_mw,
            )
        })
        .collect();
    critical_elements.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    ReliabilityIndices {
        lolp,
        lole_days_per_year,
        eens_mwh_per_year,
        critical_elements,
        n_contingencies: contingencies.len(),
    }
}

#[cfg(test)]
mod ctg06_simple_tests {
    use super::*;

    /// CTG-06-H: All FOR=0 → LOLP=0, LOLE=0, EENS=0.
    #[test]
    fn test_ctg06_zero_for_means_zero_lolp() {
        let contingencies = vec![
            ReliabilityCtgDescriptor {
                contingency_name: "Line 1->2".to_string(),
                forced_outage_rate: 0.0,
                impact_mw: 100.0,
                unserved_hours_per_trip: 8.0,
            },
            ReliabilityCtgDescriptor {
                contingency_name: "Line 2->3".to_string(),
                forced_outage_rate: 0.0,
                impact_mw: 50.0,
                unserved_hours_per_trip: 4.0,
            },
        ];
        let result = compute_reliability_indices(&contingencies);
        assert_eq!(
            result.lolp, 0.0,
            "LOLP must be 0 when all FOR=0, got {}",
            result.lolp
        );
        assert_eq!(
            result.lole_days_per_year, 0.0,
            "LOLE must be 0 when all FOR=0"
        );
        assert_eq!(
            result.eens_mwh_per_year, 0.0,
            "EENS must be 0 when all FOR=0"
        );
        assert_eq!(result.n_contingencies, 2);
    }

    /// CTG-06-I: Single contingency with FOR=0.05, impact_mw=100, hours=8
    ///            → EENS = 0.05 × 100 × 8 = 40 MWh/yr.
    #[test]
    fn test_ctg06_single_contingency() {
        let contingencies = vec![ReliabilityCtgDescriptor {
            contingency_name: "Line A->B".to_string(),
            forced_outage_rate: 0.05,
            impact_mw: 100.0,
            unserved_hours_per_trip: 8.0,
        }];
        let result = compute_reliability_indices(&contingencies);

        let expected_eens = 0.05 * 100.0 * 8.0; // = 40.0 MWh/yr
        assert!(
            (result.eens_mwh_per_year - expected_eens).abs() < 1e-9,
            "EENS expected {expected_eens} MWh/yr, got {}",
            result.eens_mwh_per_year
        );
        assert!(
            (result.lolp - 0.05).abs() < 1e-9,
            "LOLP expected 0.05, got {}",
            result.lolp
        );
        // LOLE = 0.05 * 8760 / 24 = 18.25 days/year
        let expected_lole = 0.05 * 8760.0 / 24.0;
        assert!(
            (result.lole_days_per_year - expected_lole).abs() < 1e-9,
            "LOLE expected {expected_lole} days/yr, got {}",
            result.lole_days_per_year
        );
        assert_eq!(result.n_contingencies, 1);
        assert_eq!(result.critical_elements.len(), 1);
        assert_eq!(result.critical_elements[0].0, "Line A->B");
    }
}

// ---------------------------------------------------------------------------
// CTG-06: Generator N-1 LOLE/EENS
// ---------------------------------------------------------------------------

/// Generator forced outage availability for probabilistic N-1 analysis.
///
/// The availability is `1.0 - FOR` where FOR is the Forced Outage Rate.
pub struct GeneratorAvailability {
    /// 0-based index into `Network::generators`.
    pub gen_idx: usize,
    /// Generator availability probability (1.0 - FOR).  Must be in [0.0, 1.0].
    pub availability: f64,
}

/// Result of probabilistic N-1 generator contingency analysis (LOLE/EENS).
///
/// Computed by enumerating all N-1 generator outages, weighting each by
/// `(1 - availability_i) × product(availability_j for j≠i)` and accumulating
/// load-not-served hours and energy.
#[derive(Debug, Clone)]
pub struct GeneratorLoleEens {
    /// Loss of Load Expectation (hours per year).
    ///
    /// `LOLE = Σ_i (1 - avail_i) × Π_{j≠i} avail_j × unserved_hours_i`
    /// where `unserved_hours_i` is the hours the load would be unserved
    /// if generator i is out (approximated as 8760 × FOR_i for simple cases).
    pub lole_hr_yr: f64,

    /// Expected Energy Not Served (MWh per year).
    ///
    /// `EENS = Σ_i (1 - avail_i) × Π_{j≠i} avail_j × P_i_MW × 8760`
    /// where `P_i_MW` is the rated capacity of generator i.
    pub eens_mwh_yr: f64,
}

/// Compute LOLE and EENS from N-1 generator outage probabilities.
///
/// For each generator i, the outage probability is `FOR_i = 1 - availability_i`.
/// The LOLE/EENS contributions are:
/// ```text
/// P_outage_i = FOR_i × Π_{j≠i} avail_j   (probability that exactly gen i is out)
/// LOLE_i     = P_outage_i × 8760           (hours/year at full outage rate)
/// EENS_i     = P_outage_i × capacity_i_MW × 8760
/// ```
///
/// If `availabilities` is empty, returns zero LOLE and EENS.
///
/// # Arguments
/// * `network`       — power system network (used to look up generator capacities).
/// * `availabilities` — availability for each generator; must not exceed `network.generators.len()`.
///
/// # Example
/// ```text
/// // A single generator with FOR=0.1 and 100 MW capacity → EENS ≈ 87,600 MWh/yr
/// // (simplified: ignores whether the system survives the outage)
/// ```
pub fn compute_generator_lole_eens(
    network: &surge_network::Network,
    availabilities: &[GeneratorAvailability],
) -> GeneratorLoleEens {
    if availabilities.is_empty() {
        return GeneratorLoleEens {
            lole_hr_yr: 0.0,
            eens_mwh_yr: 0.0,
        };
    }

    // Product of all availabilities (Π avail_j)
    let total_avail: f64 = availabilities
        .iter()
        .map(|g| g.availability.clamp(0.0, 1.0))
        .product();

    let hours_per_year = 8760.0_f64;

    let mut lole_hr_yr = 0.0_f64;
    let mut eens_mwh_yr = 0.0_f64;

    for g in availabilities {
        let avail_i = g.availability.clamp(0.0, 1.0);
        let for_i = 1.0 - avail_i;

        if for_i < 1e-12 {
            continue; // effectively 100% available — no contribution
        }

        // P_outage_i = FOR_i × product of all other availabilities
        // = FOR_i × (total_avail / avail_i)  [when avail_i > 0]
        let p_outage_i = if avail_i > 1e-12 {
            for_i * (total_avail / avail_i)
        } else {
            for_i * total_avail // avail_i ≈ 0; numerically safe
        };

        // Get generator capacity (MW)
        let cap_mw = if g.gen_idx < network.generators.len() {
            network.generators[g.gen_idx].pmax.max(0.0)
        } else {
            0.0
        };

        // LOLE contribution (hours/year)
        lole_hr_yr += p_outage_i * hours_per_year;

        // EENS contribution (MWh/year)
        eens_mwh_yr += p_outage_i * cap_mw * hours_per_year;
    }

    GeneratorLoleEens {
        lole_hr_yr,
        eens_mwh_yr,
    }
}

// ---------------------------------------------------------------------------
// CTG-06 generator LOLE/EENS tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod ctg06_gen_tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Bus, BusType, Generator};

    fn build_simple_network(gen_pmax_mw: Vec<f64>) -> Network {
        let mut bus1 = Bus::new(1, BusType::Slack, 100.0);
        bus1.voltage_magnitude_pu = 1.0;
        let mut gens = vec![];
        for pmax in gen_pmax_mw.iter() {
            let mut g = Generator::new(1, *pmax, 1.0);
            g.pmax = *pmax;
            g.qmax = 100.0;
            g.qmin = -50.0;
            gens.push(g);
        }
        Network {
            name: "test_gen_lole".to_string(),
            base_mva: 100.0,
            freq_hz: 60.0,
            buses: vec![bus1],
            branches: vec![],
            generators: gens,
            loads: vec![],
            controls: Default::default(),
            market_data: Default::default(),
            ..Default::default()
        }
    }

    /// CTG-06: All FOR=0 (availability=1.0) → LOLE=0, EENS=0.
    #[test]
    fn test_perfect_availability_zero_lole() {
        let net = build_simple_network(vec![200.0, 150.0]);
        let availabilities = vec![
            GeneratorAvailability {
                gen_idx: 0,
                availability: 1.0,
            },
            GeneratorAvailability {
                gen_idx: 1,
                availability: 1.0,
            },
        ];
        let result = compute_generator_lole_eens(&net, &availabilities);
        assert_eq!(
            result.lole_hr_yr, 0.0,
            "LOLE must be 0 for perfect availability"
        );
        assert_eq!(
            result.eens_mwh_yr, 0.0,
            "EENS must be 0 for perfect availability"
        );
    }

    /// CTG-06: A generator with FOR=0.1 should yield LOLE > 0.
    #[test]
    fn test_nonzero_for_gives_positive_lole() {
        let net = build_simple_network(vec![100.0]);
        let availabilities = vec![
            GeneratorAvailability {
                gen_idx: 0,
                availability: 0.9,
            }, // FOR = 0.1
        ];
        let result = compute_generator_lole_eens(&net, &availabilities);
        assert!(
            result.lole_hr_yr > 0.0,
            "LOLE should be > 0 when FOR > 0, got {}",
            result.lole_hr_yr
        );
        assert!(
            result.eens_mwh_yr > 0.0,
            "EENS should be > 0 when FOR > 0, got {}",
            result.eens_mwh_yr
        );
        // Single generator: P_outage = FOR = 0.1; LOLE = 0.1 * 8760 = 876 hr/yr
        let expected_lole = 0.1 * 8760.0;
        assert!(
            (result.lole_hr_yr - expected_lole).abs() < 1e-6,
            "LOLE expected {expected_lole:.3}, got {:.3}",
            result.lole_hr_yr
        );
        // EENS = 0.1 * 100 MW * 8760 = 87600 MWh/yr
        let expected_eens = 0.1 * 100.0 * 8760.0;
        assert!(
            (result.eens_mwh_yr - expected_eens).abs() < 1e-6,
            "EENS expected {expected_eens:.1}, got {:.1}",
            result.eens_mwh_yr
        );
        eprintln!(
            "CTG-06: single gen FOR=0.1, LOLE={:.1} hr/yr, EENS={:.1} MWh/yr",
            result.lole_hr_yr, result.eens_mwh_yr
        );
    }

    /// CTG-06: Empty availabilities → zero LOLE and EENS.
    #[test]
    fn test_empty_availabilities() {
        let net = build_simple_network(vec![100.0]);
        let result = compute_generator_lole_eens(&net, &[]);
        assert_eq!(result.lole_hr_yr, 0.0);
        assert_eq!(result.eens_mwh_yr, 0.0);
    }

    /// CTG-06: Two generators with non-zero FOR; LOLE should be finite and positive.
    #[test]
    fn test_two_gen_lole_positive() {
        let net = build_simple_network(vec![200.0, 150.0]);
        let availabilities = vec![
            GeneratorAvailability {
                gen_idx: 0,
                availability: 0.95,
            }, // FOR=0.05
            GeneratorAvailability {
                gen_idx: 1,
                availability: 0.90,
            }, // FOR=0.10
        ];
        let result = compute_generator_lole_eens(&net, &availabilities);
        assert!(result.lole_hr_yr > 0.0 && result.lole_hr_yr.is_finite());
        assert!(result.eens_mwh_yr > 0.0 && result.eens_mwh_yr.is_finite());
        eprintln!(
            "CTG-06: 2-gen LOLE={:.2} hr/yr, EENS={:.1} MWh/yr",
            result.lole_hr_yr, result.eens_mwh_yr
        );
    }
}
