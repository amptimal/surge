// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! N-2 contingency analysis — tiered screening for simultaneous double-branch outages.
//!
//! Implements the NERC TPL-001 N-2 study using a three-tier approach that
//! balances accuracy against the O(n²) scaling problem:
//!
//! **Tier 1: Conditional LODF screening** (very fast)
//!   Uses the closed-form conditional LODF formula to estimate post-N-2 flows:
//!
//!   `LODF_{k,j|i} = (LODF_{k,j} - LODF_{k,i} × LODF_{i,j}) / (1 - LODF_{i,j} × LODF_{j,i})`
//!
//!   This is more accurate than simple superposition because it accounts for
//!   the interaction between the two outages.  Pairs where the denominator
//!   approaches 0 (radially-connected branches) are flagged or skipped.
//!
//! **Tier 2: FDPF verification** (fast, for flagged pairs)
//!   AC FDPF solve with both outages applied; filters out false positives from
//!   the linear approximation before expensive NR confirmation.
//!
//! **Tier 3: NR confirmation** (exact, for critical pairs only)
//!   Full Newton-Raphson solve for the worst-case pairs identified by Tier 2.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use surge_ac::AcPfOptions;
use surge_ac::matrix::ybus::{YBus, build_ybus_from_parts};
use surge_ac::{FdpfFactors, solve_ac_pf_kernel};
use surge_dc::PreparedDcStudy;
use surge_network::Network;
use surge_network::network::Branch;
use surge_solution::SolveStatus;
use tracing::info;

use crate::{ContingencyError, ThermalRating, get_rating};

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Options controlling the tiered N-2 contingency analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N2Options {
    /// Tier 1 threshold: loading % above which a pair is flagged for Tier 2.
    /// Default: 90.0 (90% of thermal rating).
    pub tier1_threshold: f64,

    /// Tier 2 threshold: loading % above which a pair is escalated to Tier 3 NR.
    /// Default: 95.0 (95% of thermal rating).
    pub tier2_threshold: f64,

    /// Maximum number of pairs to carry from Tier 1 → Tier 2.
    /// Limits FDPF cost for very large networks.  Default: 10_000.
    pub max_candidates_tier2: usize,

    /// Maximum number of pairs to carry from Tier 2 → Tier 3 NR.
    /// Default: 1_000.
    pub max_candidates_tier3: usize,

    /// Skip pairs where the conditional LODF denominator `(1 - LODF_{i,j} × LODF_{j,i})`
    /// is near zero (radially-connected pairs that would island the network).
    /// Default: true.
    pub skip_radial: bool,

    /// Thermal limit fraction for the NR violation check (1.0 = 100% of selected thermal rating).
    /// Use >1.0 (e.g. 1.25) for emergency ratings per NERC TPL.
    pub post_contingency_rating: f64,

    /// Newton-Raphson convergence tolerance.
    pub tolerance: f64,

    /// Maximum Newton-Raphson iterations.
    pub max_iterations: usize,

    /// FDPF convergence tolerance for Tier 2 screening.
    pub fdpf_tolerance: f64,

    /// Maximum FDPF iterations for Tier 2 screening.
    pub fdpf_max_iterations: usize,

    /// Thermal rating tier for violation detection.
    ///
    /// NERC TPL-001 allows emergency ratings (Rate B or C) for post-contingency
    /// thermal checks.  Default: `RateA` (long-term continuous rating).
    #[serde(default)]
    pub thermal_rating: ThermalRating,
}

impl Default for N2Options {
    fn default() -> Self {
        Self {
            tier1_threshold: 90.0,
            tier2_threshold: 95.0,
            max_candidates_tier2: 10_000,
            max_candidates_tier3: 1_000,
            skip_radial: true,
            post_contingency_rating: 1.0,
            tolerance: 1e-6,
            max_iterations: 20,
            fdpf_tolerance: 1e-4,
            fdpf_max_iterations: 20,
            thermal_rating: ThermalRating::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Status of a single N-2 pair after analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum N2Status {
    /// Screened out by Tier 1 conditional LODF (estimated loading below threshold).
    Tier1Screened,
    /// Screened out by Tier 2 FDPF (no violation after FDPF solve).
    Tier2Screened,
    /// Confirmed clear by Tier 3 NR (no violation after full AC solve).
    Tier3Clear,
    /// Violation confirmed by Tier 3 NR.
    Tier3Violation,
    /// Did not converge in Tier 3 NR (treated as a violation for NERC compliance).
    NonConvergent,
    /// Pair skipped (radially-connected pair that would island the network).
    RadialSkipped,
}

/// A single overloaded branch in the post-N-2 state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N2BranchViolation {
    /// Internal branch index of the overloaded element.
    pub branch_index: usize,
    /// Human-readable label for the overloaded branch.
    pub branch_label: String,
    /// Apparent power flow (MVA magnitude from AC π-model).
    pub flow_mva: f64,
    /// Limit used for the check (post_contingency_rating × selected thermal rating).
    pub rating_mva: f64,
    /// flow_mva / rating_mva (> 1.0 means overloaded).
    pub overload_fraction: f64,
}

/// Result for a single N-2 (branch_i, branch_j) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N2ContingencyResult {
    /// Internal index of the first outaged branch.
    pub branch_i: usize,
    /// Internal index of the second outaged branch.
    pub branch_j: usize,
    /// Human-readable label for the pair.
    pub label: String,
    /// Analysis outcome (which tier resolved it, and whether a violation was found).
    pub status: N2Status,
    /// Maximum estimated loading % from Tier 1 conditional LODF screening.
    pub tier1_max_loading_pct: f64,
    /// Overloaded branches confirmed by Tier 3 AC solve (empty for non-NR tiers).
    pub violations: Vec<N2BranchViolation>,
    /// Whether the Tier 3 NR solve converged.
    pub converged: bool,
    /// Number of NR iterations used (0 for pairs not reaching Tier 3).
    pub nr_iterations: u32,
}

/// Summary statistics for a complete N-2 analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N2AnalysisResult {
    /// Total C(n,2) pairs considered.
    pub total_pairs: usize,
    /// Pairs skipped because they are radially connected.
    pub radial_skipped: usize,
    /// Pairs screened out by Tier 1 (conditional LODF below threshold).
    pub tier1_screened: usize,
    /// Pairs flagged by Tier 1 and passed to Tier 2 FDPF.
    pub tier1_violations: usize,
    /// Pairs screened out by Tier 2 FDPF (no violation in FDPF solve).
    pub tier2_screened: usize,
    /// Pairs escalated from Tier 2 to Tier 3 NR.
    pub tier2_violations: usize,
    /// Pairs confirmed clear by Tier 3 NR.
    pub tier3_clear: usize,
    /// Pairs with confirmed violations from Tier 3 NR.
    pub tier3_violations: usize,
    /// Pairs that did not converge in Tier 3 NR.
    pub non_convergent: usize,
    /// Per-pair results (pairs resolved at Tier 2 or beyond, plus radial-skipped).
    pub results: Vec<N2ContingencyResult>,
    /// Wall-clock time for the entire analysis.
    pub solve_time_s: f64,
    /// NERC TPL compliance: true when no confirmed violations or non-convergences.
    pub tpl_compliant: bool,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run tiered N-2 simultaneous double-branch contingency analysis.
///
/// # Algorithm
///
/// 1. **Tier 1 (Conditional LODF)**: For each (i, j) pair of in-service branches,
///    compute the conditional LODF `LODF_{k,j|i}` using the closed-form formula:
///
///    ```text
///    LODF_{k,j|i} = (LODF_{k,j} - LODF_{k,i} × LODF_{i,j}) / (1 - LODF_{i,j} × LODF_{j,i})
///    ```
///
///    Post-N-2 flow on branch k:
///    ```text
///    f_k^{i,j} ≈ f_k + LODF_{k,i} × f_i + LODF_{k,j|i} × f_j^{post-i}
///    ```
///    where `f_j^{post-i} = f_j + LODF_{j,i} × f_i`.
///
///    Pairs where the denominator `(1 - LODF_{i,j} × LODF_{j,i})` is near zero
///    (radially connected) are skipped or flagged as critical.
///
/// 2. **Tier 2 (FDPF)**: For pairs flagged in Tier 1, apply both outages and
///    solve with Fast Decoupled Power Flow from the base-case warm start.
///
/// 3. **Tier 3 (NR)**: For pairs still showing violations after FDPF, solve
///    with full Newton-Raphson for exact thermal and voltage assessment.
pub fn run_n2_contingency_analysis(
    network: &Network,
    options: &N2Options,
) -> Result<N2AnalysisResult, ContingencyError> {
    let wall_start = Instant::now();

    // -----------------------------------------------------------------------
    // Step 1: DC base-case flows
    // -----------------------------------------------------------------------
    let dc_result =
        surge_dc::solve_dc(network).map_err(|e| ContingencyError::DcSolveFailed(e.to_string()))?;
    let base_mva = network.base_mva;

    // Branch flows in MW (positive = from→to direction)
    let base_flows_mw: Vec<f64> = dc_result
        .branch_p_flow
        .iter()
        .map(|&f| f * base_mva)
        .collect();

    // -----------------------------------------------------------------------
    // Step 2: In-service branch list + pair count
    // -----------------------------------------------------------------------
    let _n_branches = network.n_branches();
    let in_service: Vec<usize> = network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| br.in_service)
        .map(|(i, _)| i)
        .collect();
    let n_in_service = in_service.len();
    let total_pairs = n_in_service * (n_in_service.saturating_sub(1)) / 2;

    info!(
        "N-2 analysis: {} in-service branches → {} pairs",
        n_in_service, total_pairs
    );

    // -----------------------------------------------------------------------
    // Step 3: Tier 1 — Conditional LODF screening
    // -----------------------------------------------------------------------
    // Uses the canonical streamed N-2 LODF column builder from surge-dc, with
    // single-outage column caching for repeated pair evaluation.
    let tier1_results: Vec<(usize, usize, f64, bool)> =
        compute_tier1_sparse(network, &in_service, &base_flows_mw, options);

    let (radial_skipped, tier1_screened, tier1_flagged) = classify_and_limit_tier1_candidates(
        &tier1_results,
        options.tier1_threshold,
        options.max_candidates_tier2,
    );

    let tier1_violations = tier1_flagged.len();
    info!(
        "N-2 Tier 1: {} radial skipped, {} screened, {} flagged for Tier 2",
        radial_skipped, tier1_screened, tier1_violations
    );

    // -----------------------------------------------------------------------
    // Step 4: Tier 2 — FDPF verification of flagged pairs
    // -----------------------------------------------------------------------
    let (tier2_clear, mut tier3_pairs) = run_tier2_fdpf(network, &tier1_flagged, options)?;

    let tier2_screened = tier2_clear.len();
    let tier2_violations = tier3_pairs.len();

    info!(
        "N-2 Tier 2 (FDPF): {} clear, {} escalated to Tier 3",
        tier2_screened, tier2_violations
    );

    // -----------------------------------------------------------------------
    // Step 5: Tier 3 — Full NR for escalated pairs
    // -----------------------------------------------------------------------
    // Already sorted by severity from Tier 1; truncate to max_candidates_tier3
    truncate_pairs_by_severity(&mut tier3_pairs, options.max_candidates_tier3, "Tier 3");

    let tier3_results = run_tier3_nr(network, &tier3_pairs, options)?;

    // -----------------------------------------------------------------------
    // Step 6: Aggregate results
    // -----------------------------------------------------------------------
    let mut tier3_clear = 0usize;
    let mut tier3_violation_count = 0usize;
    let mut non_convergent = 0usize;
    let mut results: Vec<N2ContingencyResult> = Vec::new();

    // Tier 2 clear results (accepted by FDPF, no NR needed)
    for &(bi, bj, max_loading) in &tier2_clear {
        let br_i = &network.branches[bi];
        let br_j = &network.branches[bj];
        results.push(N2ContingencyResult {
            branch_i: bi,
            branch_j: bj,
            label: n2_label(br_i, br_j),
            status: N2Status::Tier2Screened,
            tier1_max_loading_pct: max_loading,
            violations: vec![],
            converged: true,
            nr_iterations: 0,
        });
    }

    // Tier 3 NR results
    for r in tier3_results {
        match r.status {
            N2Status::Tier3Clear => tier3_clear += 1,
            N2Status::Tier3Violation => tier3_violation_count += 1,
            N2Status::NonConvergent => non_convergent += 1,
            _ => {}
        }
        results.push(r);
    }

    let solve_time_s = wall_start.elapsed().as_secs_f64();
    let tpl_compliant = tier3_violation_count == 0 && non_convergent == 0;

    info!(
        "N-2 complete in {:.3}s: {} pairs total, {} T1-screened, {} T2-clear, \
         {} T3-clear, {} violations, {} non-convergent",
        solve_time_s,
        total_pairs,
        tier1_screened,
        tier2_screened,
        tier3_clear,
        tier3_violation_count,
        non_convergent
    );

    Ok(N2AnalysisResult {
        total_pairs,
        radial_skipped,
        tier1_screened,
        tier1_violations,
        tier2_screened,
        tier2_violations,
        tier3_clear,
        tier3_violations: tier3_violation_count,
        non_convergent,
        results,
        solve_time_s,
        tpl_compliant,
    })
}

fn truncate_pairs_by_severity(pairs: &mut Vec<(usize, usize, f64)>, max_pairs: usize, label: &str) {
    if pairs.len() > max_pairs {
        info!(
            "N-2 {label}: truncating to max_candidates={} (had {})",
            max_pairs,
            pairs.len()
        );
        pairs.truncate(max_pairs);
    }
}

fn classify_and_limit_tier1_candidates(
    tier1_results: &[(usize, usize, f64, bool)],
    tier1_threshold: f64,
    max_candidates_tier2: usize,
) -> (usize, usize, Vec<(usize, usize, f64)>) {
    let mut radial_skipped = 0usize;
    let mut tier1_screened = 0usize;
    let mut tier1_flagged: Vec<(usize, usize, f64)> = Vec::new();

    for (i, j, max_loading, is_radial) in tier1_results {
        if *is_radial {
            radial_skipped += 1;
        } else if *max_loading <= tier1_threshold {
            tier1_screened += 1;
        } else {
            tier1_flagged.push((*i, *j, *max_loading));
        }
    }

    tier1_flagged.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    truncate_pairs_by_severity(&mut tier1_flagged, max_candidates_tier2, "Tier 1");

    (radial_skipped, tier1_screened, tier1_flagged)
}

// ---------------------------------------------------------------------------
// Helpers shared across tiers
// ---------------------------------------------------------------------------

/// Human-readable label for an N-2 pair.
fn n2_label(br_i: &Branch, br_j: &Branch) -> String {
    format!(
        "N-2: Line {}->{}(ckt {}) & Line {}->{}(ckt {})",
        br_i.from_bus, br_i.to_bus, br_i.circuit, br_j.from_bus, br_j.to_bus, br_j.circuit,
    )
}

// ---------------------------------------------------------------------------
// Tier 1: Conditional LODF screening — sparse path (large networks)
// ---------------------------------------------------------------------------

/// Screen all pairs using cached streamed N-2 LODF columns.
///
/// Sequential because the prepared DC model uses a mutable KLU factorization.
/// For large networks this is still fast: single-outage columns are cached once
/// and reused across all ordered pair evaluations.
fn compute_tier1_sparse(
    network: &Network,
    in_service: &[usize],
    base_flows_mw: &[f64],
    options: &N2Options,
) -> Vec<(usize, usize, f64, bool)> {
    let mut model = match PreparedDcStudy::new(network) {
        Ok(model) => model,
        Err(e) => {
            tracing::warn!(
                "N-2 sparse Tier 1: DC model preparation failed ({}), flagging all pairs critical",
                e
            );
            // Treat every pair as critical (will get FDPF/NR pass)
            let n = in_service.len();
            return (0..n)
                .flat_map(|ia| {
                    ((ia + 1)..n).map(move |ja| (in_service[ia], in_service[ja], 101.0_f64, false))
                })
                .collect();
        }
    };
    let mut n2_columns = match model.n2_lodf_columns(in_service, in_service) {
        Ok(columns) => columns,
        Err(e) => {
            tracing::warn!(
                "N-2 sparse Tier 1: N-2 LODF builder preparation failed ({}), flagging all pairs critical",
                e
            );
            let n = in_service.len();
            return (0..n)
                .flat_map(|ia| {
                    ((ia + 1)..n).map(move |ja| (in_service[ia], in_service[ja], 101.0_f64, false))
                })
                .collect();
        }
    };

    let n = in_service.len();
    let mut results = Vec::with_capacity(n * (n - 1) / 2);

    for ia in 0..n {
        let bi = in_service[ia];
        let fi_mw = base_flows_mw[bi];

        for ja in (ia + 1)..n {
            let bj = in_service[ja];
            let fj_mw = base_flows_mw[bj];

            let n2_from_i = match n2_columns.compute_column(bi, bj) {
                Ok(column) => column,
                Err(_) => {
                    results.push(if options.skip_radial {
                        (bi, bj, 0.0, true)
                    } else {
                        (bi, bj, f64::INFINITY, false)
                    });
                    continue;
                }
            };
            let n2_from_j = match n2_columns.compute_column(bj, bi) {
                Ok(column) => column,
                Err(_) => {
                    results.push(if options.skip_radial {
                        (bi, bj, 0.0, true)
                    } else {
                        (bi, bj, f64::INFINITY, false)
                    });
                    continue;
                }
            };

            if n2_from_i.iter().any(|value| !value.is_finite())
                || n2_from_j.iter().any(|value| !value.is_finite())
            {
                results.push(if options.skip_radial {
                    (bi, bj, 0.0, true)
                } else {
                    (bi, bj, f64::INFINITY, false)
                });
                continue;
            }

            // Screen all monitored branches
            let mut max_loading_pct = 0.0f64;
            for (monitored_pos, &bk) in in_service.iter().enumerate() {
                if bk == bi || bk == bj {
                    continue;
                }
                let br_k = &network.branches[bk];
                let rating_k = get_rating(br_k, options.thermal_rating);
                if rating_k <= 0.0 {
                    continue;
                }

                let fk_post = base_flows_mw[bk]
                    + n2_from_i[monitored_pos] * fi_mw
                    + n2_from_j[monitored_pos] * fj_mw;
                let loading_pct = fk_post.abs() / rating_k * 100.0;

                if loading_pct > max_loading_pct {
                    max_loading_pct = loading_pct;
                }
            }

            results.push((bi, bj, max_loading_pct, false));
        }
    }

    results
}

// ---------------------------------------------------------------------------
// Tier 2: FDPF verification
// ---------------------------------------------------------------------------

/// Tier 2 parallel FDPF verification of flagged pairs.
///
/// For each pair (bi, bj, max_loading) from Tier 1:
/// - Apply both branch outages to the base Y-bus via delta updates
/// - Solve FDPF from the base-case AC warm start
/// - If converged with no thermal violation above `tier2_threshold` → clear
/// - Otherwise (violation or non-convergence) → escalate to Tier 3
///
/// Returns `(clear_pairs, escalated_pairs)`.
#[allow(clippy::type_complexity)]
fn run_tier2_fdpf(
    network: &Network,
    flagged_pairs: &[(usize, usize, f64)],
    options: &N2Options,
) -> Result<(Vec<(usize, usize, f64)>, Vec<(usize, usize, f64)>), ContingencyError> {
    if flagged_pairs.is_empty() {
        return Ok((vec![], vec![]));
    }

    let bus_map = network.bus_index_map();
    let p_spec = network.bus_p_injection_pu();
    let q_spec = network.bus_q_injection_pu();
    let base_mva = network.base_mva;

    // Solve base-case AC for warm start
    let acpf_options = AcPfOptions {
        tolerance: 1e-8,
        max_iterations: 500,
        flat_start: false,
        ..AcPfOptions::default()
    };
    let base_case = solve_ac_pf_kernel(network, &acpf_options)
        .map_err(|e| ContingencyError::BaseCaseFailed(format!("Base NR for Tier 2: {e}")))?;

    if base_case.status != SolveStatus::Converged {
        return Err(ContingencyError::BaseCaseFailed(
            "Base case did not converge for Tier 2 warm start".to_string(),
        ));
    }

    let base_vm = base_case.voltage_magnitude_pu.clone();
    let base_va = base_case.voltage_angle_rad.clone();

    // Build base Y-bus once; each FDPF applies a pair of branch-removal deltas
    let base_ybus = build_ybus_from_parts(
        &network.branches,
        &network.buses,
        base_mva,
        &bus_map,
        &network.metadata.impedance_corrections,
    );

    // Impedance correction map — mirrors the one used in build_ybus_from_parts.
    let corr_map: HashMap<
        u32,
        &surge_network::network::impedance_correction::ImpedanceCorrectionTable,
    > = network
        .metadata
        .impedance_corrections
        .iter()
        .map(|t| (t.number, t))
        .collect();

    // Pre-compute Y-bus removal deltas for each unique branch referenced in the pairs
    let unique_branches: HashSet<usize> = flagged_pairs
        .iter()
        .flat_map(|&(bi, bj, _)| [bi, bj])
        .collect();
    let delta_cache: HashMap<usize, [(usize, usize, f64, f64); 4]> = unique_branches
        .into_iter()
        .filter_map(|br_idx| {
            let branch = &network.branches[br_idx];
            if branch.in_service {
                Some((
                    br_idx,
                    YBus::branch_removal_delta(branch, &bus_map, &corr_map),
                ))
            } else {
                None
            }
        })
        .collect();

    // Pool of FdpfFactors — one per rayon thread (KLU solve is mutable)
    let fdpf_pool: Mutex<Vec<FdpfFactors>> = Mutex::new(Vec::new());

    let fdpf_tol = options.fdpf_tolerance;
    let fdpf_max_iters = options.fdpf_max_iterations as u32;
    let tier2_threshold = options.tier2_threshold;
    let post_rating_frac = options.post_contingency_rating;

    // true = clear (no violation), false = escalate
    let screening_results: Vec<bool> = flagged_pairs
        .par_iter()
        .map(|&(bi, bj, _)| {
            let mut fdpf = match fdpf_pool
                .lock()
                .expect("fdpf_pool mutex should not be poisoned")
                .pop()
            {
                Some(f) => f,
                None => match FdpfFactors::new(network) {
                    Ok(f) => f,
                    // Construction failure (singular B'): escalate to Tier 3.
                    Err(_) => return false,
                },
            };

            // Apply both outage deltas to the Y-bus
            let mut ybus = base_ybus.clone();
            for &br_idx in &[bi, bj] {
                if let Some(delta) = delta_cache.get(&br_idx) {
                    ybus.apply_deltas(delta);
                }
            }

            let fdpf_result = fdpf.solve_from_ybus(
                &ybus,
                &p_spec,
                &q_spec,
                &base_vm,
                &base_va,
                fdpf_tol,
                fdpf_max_iters,
            );

            fdpf_pool
                .lock()
                .expect("fdpf_pool mutex should not be poisoned")
                .push(fdpf);

            match fdpf_result {
                Some(r) => {
                    let outaged: HashSet<usize> = [bi, bj].iter().cloned().collect();
                    // Clear if no branch exceeds tier2_threshold
                    !has_thermal_violation(
                        network,
                        &r.vm,
                        &r.va,
                        &bus_map,
                        base_mva,
                        post_rating_frac,
                        &outaged,
                        tier2_threshold,
                        options.thermal_rating,
                    )
                }
                // FDPF non-convergence → escalate to Tier 3
                None => false,
            }
        })
        .collect();

    let mut clear_pairs = Vec::new();
    let mut escalated_pairs = Vec::new();

    for (&(bi, bj, max_loading), &is_clear) in flagged_pairs.iter().zip(screening_results.iter()) {
        if is_clear {
            clear_pairs.push((bi, bj, max_loading));
        } else {
            escalated_pairs.push((bi, bj, max_loading));
        }
    }

    Ok((clear_pairs, escalated_pairs))
}

// ---------------------------------------------------------------------------
// Tier 3: Full NR confirmation
// ---------------------------------------------------------------------------

/// Tier 3: Full Newton-Raphson solve for the most critical pairs (parallel).
fn run_tier3_nr(
    network: &Network,
    pairs: &[(usize, usize, f64)],
    options: &N2Options,
) -> Result<Vec<N2ContingencyResult>, ContingencyError> {
    if pairs.is_empty() {
        return Ok(vec![]);
    }

    let base_mva = network.base_mva;
    let acpf_options = AcPfOptions {
        tolerance: options.tolerance,
        max_iterations: options.max_iterations as u32,
        flat_start: false,
        ..AcPfOptions::default()
    };
    let post_rating_frac = options.post_contingency_rating;

    let results: Vec<N2ContingencyResult> = pairs
        .par_iter()
        .map(|&(bi, bj, tier1_max_loading)| {
            let br_i = &network.branches[bi];
            let br_j = &network.branches[bj];
            let label = n2_label(br_i, br_j);

            // Clone network with both branches tripped
            let mut net = network.clone();
            net.branches[bi].in_service = false;
            net.branches[bj].in_service = false;

            match solve_ac_pf_kernel(&net, &acpf_options) {
                Ok(sol) if sol.status == SolveStatus::Converged => {
                    let bus_map = net.bus_index_map();
                    let outaged: HashSet<usize> = [bi, bj].iter().cloned().collect();
                    let violations = collect_thermal_violations(
                        // Use original network for branch parameters (clone has OOS branches)
                        network,
                        &sol.voltage_magnitude_pu,
                        &sol.voltage_angle_rad,
                        &bus_map,
                        base_mva,
                        post_rating_frac,
                        &outaged,
                        options.thermal_rating,
                    );

                    let status = if violations.is_empty() {
                        N2Status::Tier3Clear
                    } else {
                        N2Status::Tier3Violation
                    };

                    N2ContingencyResult {
                        branch_i: bi,
                        branch_j: bj,
                        label,
                        status,
                        tier1_max_loading_pct: tier1_max_loading,
                        violations,
                        converged: true,
                        nr_iterations: sol.iterations,
                    }
                }
                _ => N2ContingencyResult {
                    branch_i: bi,
                    branch_j: bj,
                    label,
                    status: N2Status::NonConvergent,
                    tier1_max_loading_pct: tier1_max_loading,
                    violations: vec![],
                    converged: false,
                    nr_iterations: 0,
                },
            }
        })
        .collect();

    Ok(results)
}

// ---------------------------------------------------------------------------
// Violation helpers
// ---------------------------------------------------------------------------

/// Return true if any monitored branch (not in `outaged`) has apparent power
/// flow exceeding `threshold_pct`% of its emergency rating.
#[allow(clippy::too_many_arguments)]
fn has_thermal_violation(
    network: &Network,
    vm: &[f64],
    va: &[f64],
    bus_map: &HashMap<u32, usize>,
    base_mva: f64,
    rating_frac: f64,
    outaged: &HashSet<usize>,
    threshold_pct: f64,
    thermal_rating: ThermalRating,
) -> bool {
    for (i, branch) in network.branches.iter().enumerate() {
        let rating = get_rating(branch, thermal_rating);
        if !branch.in_service || outaged.contains(&i) || rating <= 0.0 {
            continue;
        }
        let s_mva = branch_flow_mva(branch, vm, va, bus_map, base_mva);
        if s_mva / (rating_frac * rating) * 100.0 > threshold_pct {
            return true;
        }
    }
    false
}

/// Collect all thermal overload violations in the post-N-2 AC solution.
#[allow(clippy::too_many_arguments)]
fn collect_thermal_violations(
    network: &Network,
    vm: &[f64],
    va: &[f64],
    bus_map: &HashMap<u32, usize>,
    base_mva: f64,
    rating_frac: f64,
    outaged: &HashSet<usize>,
    thermal_rating: ThermalRating,
) -> Vec<N2BranchViolation> {
    let mut violations = Vec::new();

    for (i, branch) in network.branches.iter().enumerate() {
        let rating = get_rating(branch, thermal_rating);
        if !branch.in_service || outaged.contains(&i) || rating <= 0.0 {
            continue;
        }
        let s_mva = branch_flow_mva(branch, vm, va, bus_map, base_mva);
        let emerg_rating = rating_frac * rating;

        if s_mva > emerg_rating {
            violations.push(N2BranchViolation {
                branch_index: i,
                branch_label: format!(
                    "Line {}->{}(ckt {})",
                    branch.from_bus, branch.to_bus, branch.circuit
                ),
                flow_mva: s_mva,
                rating_mva: emerg_rating,
                overload_fraction: s_mva / emerg_rating,
            });
        }
    }

    violations
}

/// Compute apparent power flow (MVA) on a branch from post-contingency voltages.
///
/// Uses the standard π-model (from-side S = V_f × I_f*).
fn branch_flow_mva(
    branch: &Branch,
    vm: &[f64],
    va: &[f64],
    bus_map: &HashMap<u32, usize>,
    base_mva: f64,
) -> f64 {
    let f = bus_map[&branch.from_bus];
    let t = bus_map[&branch.to_bus];

    let vi = vm[f];
    let vj = vm[t];
    let theta_ij = va[f] - va[t];

    branch.power_flows_pu(vi, vj, theta_ij, 1e-40).max_s_pu() * base_mva
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn data_available() -> bool {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::Path::new(&p).exists();
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
            .exists()
    }
    #[allow(dead_code)]
    fn test_data_dir() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::PathBuf::from(p);
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
    }

    use super::*;

    fn load_case(name: &str) -> Network {
        let path = test_data_dir().join(format!("{name}.m"));
        surge_io::matpower::load(&path).unwrap_or_else(|e| panic!("failed to parse {name}: {e}"))
    }

    #[test]
    fn test_classify_and_limit_tier1_candidates_keeps_worst_pairs() {
        let tier1_results = vec![
            (0usize, 1usize, 91.0, false),
            (0usize, 2usize, 150.0, false),
            (1usize, 2usize, 89.0, false),
            (2usize, 3usize, 120.0, false),
            (3usize, 4usize, 80.0, true),
        ];

        let (radial_skipped, tier1_screened, flagged) =
            classify_and_limit_tier1_candidates(&tier1_results, 90.0, 2);

        assert_eq!(radial_skipped, 1);
        assert_eq!(tier1_screened, 1);
        assert_eq!(flagged, vec![(0, 2, 150.0), (2, 3, 120.0)]);
    }

    #[test]
    fn test_truncate_pairs_by_severity_preserves_existing_order() {
        let mut pairs = vec![(0usize, 1usize, 150.0), (2, 3, 120.0), (4, 5, 110.0)];
        truncate_pairs_by_severity(&mut pairs, 2, "Tier 3");
        assert_eq!(pairs, vec![(0, 1, 150.0), (2, 3, 120.0)]);
    }

    // -----------------------------------------------------------------------
    // test_n2_lodf_screening_case9
    // -----------------------------------------------------------------------

    /// N-2 Tier 1 screening on case9 (9 branches → 36 pairs).
    ///
    /// Verifies:
    /// - Correct number of pairs C(n,2) where n is in-service branch count
    /// - All pairs are accounted for: radial_skipped + tier1_screened + tier1_violations = total
    /// - Analysis completes without panic
    #[test]
    fn test_n2_lodf_screening_case9() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let n_in_service = net.branches.iter().filter(|b| b.in_service).count();
        let expected_pairs = n_in_service * (n_in_service - 1) / 2;

        let opts = N2Options {
            tier1_threshold: 90.0,
            max_candidates_tier2: 10_000,
            max_candidates_tier3: 1_000,
            skip_radial: true,
            ..N2Options::default()
        };

        let result =
            run_n2_contingency_analysis(&net, &opts).expect("N-2 analysis should succeed on case9");

        assert_eq!(
            result.total_pairs, expected_pairs,
            "total_pairs should be C({n_in_service},2)={expected_pairs}"
        );

        // Every pair must be accounted for
        assert_eq!(
            result.radial_skipped + result.tier1_screened + result.tier1_violations,
            expected_pairs,
            "radial_skipped + tier1_screened + tier1_violations must sum to total_pairs"
        );

        eprintln!(
            "case9 N-2: {} total pairs, {} radial, {} T1-screened, {} → T2, {:.3}s",
            result.total_pairs,
            result.radial_skipped,
            result.tier1_screened,
            result.tier1_violations,
            result.solve_time_s
        );
    }

    // -----------------------------------------------------------------------
    // test_n2_conditional_lodf_formula
    // -----------------------------------------------------------------------

    /// Verify the conditional LODF formula against brute-force recomputation.
    ///
    /// For a triple (bi, bj, bk), the conditional LODF formula gives:
    ///   `LODF_{bk,bj|bi} = (LODF[bk,bj] - LODF[bk,bi] × LODF[bi,bj]) / (1 - LODF[bi,bj] × LODF[bj,bi])`
    ///
    /// The brute-force reference: remove branch bi, recompute LODF in the
    /// reduced network, read off LODF[bk, bj].  Both should agree to < 1e-6.
    #[test]
    fn test_n2_conditional_lodf_formula() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");

        let in_service: Vec<usize> = net
            .branches
            .iter()
            .enumerate()
            .filter(|(_, b)| b.in_service)
            .map(|(i, _)| i)
            .collect();

        // Test several (bi, bj, bk) triples
        let test_triples: &[(usize, usize, usize)] = &[
            (in_service[0], in_service[1], in_service[2]),
            (in_service[0], in_service[2], in_service[3]),
            (in_service[1], in_service[3], in_service[4]),
        ];

        for &(bi, bj, bk) in test_triples {
            let lodf_pairs = surge_dc::compute_lodf_pairs(&net, &[bk, bi, bj], &[bi, bj]).unwrap();
            let lodf_ij = lodf_pairs.get(bi, bj).expect("LODF(bi,bj)");
            let lodf_ji = lodf_pairs.get(bj, bi).expect("LODF(bj,bi)");
            let lodf_ki = lodf_pairs.get(bk, bi).expect("LODF(bk,bi)");
            let lodf_kj = lodf_pairs.get(bk, bj).expect("LODF(bk,bj)");

            let denom: f64 = 1.0 - lodf_ij * lodf_ji;
            if denom.abs() < 1e-6 {
                eprintln!("Skipping radial pair ({bi},{bj}) in conditional LODF test");
                continue;
            }

            // Conditional LODF formula: LODF_{bk, bj | bi outaged}
            let cond_lodf_formula = (lodf_kj - lodf_ki * lodf_ij) / denom;

            // Brute-force: build network without branch bi, compute LODF[bk, bj].
            // Removing branch bi may disconnect the network (radial topology),
            // causing KLU to fail — skip those pairs when PTDF returns an error.
            let mut net_no_i = net.clone();
            net_no_i.branches[bi].in_service = false;
            let cond_lodf_bf = match surge_dc::compute_lodf_pairs(&net_no_i, &[bk], &[bj]) {
                Ok(lodf_no_i) => lodf_no_i
                    .get(bk, bj)
                    .expect("LODF(bk,bj) after removing bi"),
                Err(_) => {
                    eprintln!(
                        "Skipping brute-force for ({bi},{bj},{bk}): network disconnected after removing branch {bi}"
                    );
                    continue;
                }
            };

            // Skip if either value is non-finite (radial/degenerate pair)
            if !cond_lodf_formula.is_finite() || !cond_lodf_bf.is_finite() {
                eprintln!(
                    "Skipping non-finite LODF for ({bi},{bj},{bk}): \
                     formula={cond_lodf_formula:.6}, bf={cond_lodf_bf:.6}"
                );
                continue;
            }

            let diff = (cond_lodf_formula - cond_lodf_bf).abs();
            assert!(
                diff < 1e-6,
                "Conditional LODF formula vs brute-force for ({bi},{bj},{bk}): \
                 formula={cond_lodf_formula:.6}, bf={cond_lodf_bf:.6}, diff={diff:.2e}"
            );

            eprintln!(
                "Conditional LODF ({bi} out, bj={bj}, bk={bk}): formula={cond_lodf_formula:.6}, \
                 bf={cond_lodf_bf:.6}, diff={diff:.2e}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // test_n2_full_case14
    // -----------------------------------------------------------------------

    /// Full three-tier N-2 analysis on case14 (20 branches → 190 pairs).
    ///
    /// Verifies:
    /// - Correct total pair count
    /// - Tier count consistency (radial + T1-screened + T1-flagged = total)
    /// - All Tier 3 results have valid indices and non-trivial violation fractions
    #[test]
    fn test_n2_full_case14() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case14");
        let n_in_service = net.branches.iter().filter(|b| b.in_service).count();
        let expected_pairs = n_in_service * (n_in_service - 1) / 2;

        let opts = N2Options {
            tier1_threshold: 80.0, // aggressive to exercise all tiers
            tier2_threshold: 90.0,
            max_candidates_tier2: 500,
            max_candidates_tier3: 100,
            skip_radial: true,
            post_contingency_rating: 1.0,
            ..N2Options::default()
        };

        let result = run_n2_contingency_analysis(&net, &opts)
            .expect("N-2 analysis should succeed on case14");

        assert_eq!(result.total_pairs, expected_pairs);

        // Tier 1 accounting must be exact
        assert_eq!(
            result.radial_skipped + result.tier1_screened + result.tier1_violations,
            expected_pairs,
            "Tier 1 accounting: {} + {} + {} ≠ {}",
            result.radial_skipped,
            result.tier1_screened,
            result.tier1_violations,
            expected_pairs
        );

        // Tier 3 results must have valid branch indices
        for r in &result.results {
            if matches!(
                r.status,
                N2Status::Tier3Violation | N2Status::Tier3Clear | N2Status::NonConvergent
            ) {
                assert!(r.branch_i < net.n_branches(), "branch_i out of range");
                assert!(r.branch_j < net.n_branches(), "branch_j out of range");
                assert_ne!(r.branch_i, r.branch_j, "branch_i and branch_j must differ");
            }
            for v in &r.violations {
                assert_ne!(
                    v.branch_index, r.branch_i,
                    "violation references outaged branch_i"
                );
                assert_ne!(
                    v.branch_index, r.branch_j,
                    "violation references outaged branch_j"
                );
                assert!(
                    v.overload_fraction > 1.0,
                    "violation fraction must be > 1.0"
                );
            }
        }

        eprintln!(
            "case14 N-2: {} pairs, {} radial, {} T1-screened, {} → T2, \
             {} T2-clear, {} → T3, {} T3-clear, {} violations, {:.3}s",
            result.total_pairs,
            result.radial_skipped,
            result.tier1_screened,
            result.tier1_violations,
            result.tier2_screened,
            result.tier2_violations,
            result.tier3_clear,
            result.tier3_violations,
            result.solve_time_s
        );
    }

    // -----------------------------------------------------------------------
    // test_n2_radial_skip
    // -----------------------------------------------------------------------

    /// Verify that radially-connected pairs are properly detected and skipped
    /// when skip_radial = true, and not skipped when skip_radial = false.
    #[test]
    fn test_n2_radial_skip() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let all_branches: Vec<usize> = (0..net.n_branches()).collect();
        let lodf = surge_dc::compute_lodf_pairs(&net, &all_branches, &all_branches).unwrap();

        let in_service: Vec<usize> = net
            .branches
            .iter()
            .enumerate()
            .filter(|(_, b)| b.in_service)
            .map(|(i, _)| i)
            .collect();

        // Count pairs our formula would mark as radial
        let mut expected_radial = 0usize;
        for ia in 0..in_service.len() {
            for ja in (ia + 1)..in_service.len() {
                let bi = in_service[ia];
                let bj = in_service[ja];
                let lodf_ij = lodf.get(bi, bj).expect("LODF(bi,bj)");
                let lodf_ji = lodf.get(bj, bi).expect("LODF(bj,bi)");
                let denom: f64 = 1.0 - lodf_ij * lodf_ji;
                if denom.abs() < 1e-6 || lodf_ij.is_infinite() || lodf_ji.is_infinite() {
                    expected_radial += 1;
                }
            }
        }

        let opts_skip = N2Options {
            skip_radial: true,
            tier1_threshold: 200.0, // flag nothing so all non-radial pairs go to tier1_screened
            ..N2Options::default()
        };
        let opts_no_skip = N2Options {
            skip_radial: false,
            tier1_threshold: 200.0,
            ..N2Options::default()
        };

        let result_skip = run_n2_contingency_analysis(&net, &opts_skip)
            .expect("N-2 with skip_radial=true should succeed");
        let result_no_skip = run_n2_contingency_analysis(&net, &opts_no_skip)
            .expect("N-2 with skip_radial=false should succeed");

        assert_eq!(
            result_skip.radial_skipped, expected_radial,
            "skip_radial=true: expected {} radial pairs, got {}",
            expected_radial, result_skip.radial_skipped
        );

        assert_eq!(
            result_no_skip.radial_skipped, 0,
            "skip_radial=false: radial_skipped must be 0"
        );

        eprintln!(
            "Radial detection: expected={expected_radial}, \
             skip=true: radial_skipped={}, skip=false: radial_skipped={}",
            result_skip.radial_skipped, result_no_skip.radial_skipped
        );
    }
}
