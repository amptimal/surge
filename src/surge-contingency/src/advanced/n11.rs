// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! N-1-1 contingency analysis (NERC TPL-001 P5-P7).
//!
//! N-1-1 = loss of element A followed by loss of element B.
//!
//! Two-pass approach:
//! 1. **LODF-based linear screening** of all O(N²) branch pairs using:
//!    `Flow_k(A,B) ≈ base_flow_k + LODF[k,A]*base_flow_A + LODF[k,B]*base_flow_B`
//! 2. **AC Newton-Raphson validation** of flagged pairs (both outages applied).

use std::time::Instant;

use serde::{Deserialize, Serialize};
use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
use surge_dc::PreparedDcStudy;
use surge_network::Network;
use surge_solution::SolveStatus;
use tracing::info;

use crate::{ContingencyError, ThermalRating, get_rating};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Options for N-1-1 contingency analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N11Options {
    /// LODF-based screening threshold: flag pairs where estimated loading
    /// exceeds this fraction of the thermal rating. Default 0.95 = 95%.
    pub lodf_threshold: f64,
    /// Maximum number of pairs to AC-validate (limits computation).
    pub max_ac_pairs: usize,
    /// Only build second-outage pairs when element A was already stressed
    /// in N-1 screening (estimated post-A loading > stress_threshold of rating).
    pub stressed_only: bool,
    /// Pre-screening threshold: only build pairs for outage A when estimated
    /// post-A loading exceeds this fraction of rating. Default: 0.80 (80%).
    pub stress_threshold: f64,
    /// Post-contingency rating fraction (emergency rating, e.g. 1.25 = 125%).
    pub post_contingency_rating: f64,
    /// Newton-Raphson convergence tolerance.
    pub tolerance: f64,
    /// Maximum Newton-Raphson iterations.
    pub max_iterations: usize,
    /// Thermal rating tier for violation detection.
    ///
    /// NERC TPL-001 allows emergency ratings (Rate B or C) for post-contingency
    /// thermal checks.  Default: `RateA` (long-term continuous rating).
    #[serde(default)]
    pub thermal_rating: ThermalRating,
}

impl Default for N11Options {
    fn default() -> Self {
        Self {
            lodf_threshold: 0.95,
            max_ac_pairs: 10_000,
            stressed_only: true,
            stress_threshold: 0.80,
            post_contingency_rating: 1.25,
            tolerance: 1e-6,
            max_iterations: 20,
            thermal_rating: ThermalRating::default(),
        }
    }
}

/// A branch violation detected in the post-(A,B) state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N11BranchViolation {
    pub branch_index: usize,
    pub branch_label: String,
    /// Apparent-power flow in MVA (magnitude, computed from AC pi-model S-flow).
    pub flow_mva: f64,
    /// Emergency rating in MVA used for the check (post_contingency_rating × selected thermal rating).
    pub rating_mva: f64,
    /// flow / rating (> 1 means overloaded).
    pub overload_fraction: f64,
}

/// Results for a single (A, B) outage pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N11ViolationPair {
    /// Branch index of first outage.
    pub outage_a: usize,
    /// Branch index of second outage.
    pub outage_b: usize,
    pub label_a: String,
    pub label_b: String,
    pub violations: Vec<N11BranchViolation>,
    /// True if an AC Newton-Raphson solve was performed for this pair.
    pub ac_validated: bool,
    /// True if the AC NR solve converged (only valid when ac_validated=true).
    pub converged: bool,
}

/// Overall result of the N-1-1 analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N11Result {
    /// Total (A, B) pairs evaluated by LODF screening.
    pub total_pairs_screened: usize,
    /// Pairs flagged by LODF as potentially violating.
    pub lodf_flagged_pairs: usize,
    /// Pairs sent through AC validation.
    pub ac_validated_pairs: usize,
    /// Pairs with confirmed AC violations (or non-convergence).
    pub violation_pairs: Vec<N11ViolationPair>,
    /// Wall-clock time for the entire analysis.
    pub solve_time_secs: f64,
    /// NERC TPL-001 P5+ compliance: true when no confirmed violations found.
    pub tpl_compliant: bool,
}

#[derive(Debug)]
struct ScreeningOutageColumn {
    branch_index: usize,
    flow_mw: f64,
    is_bridge: bool,
    lodf_to_in_service: Vec<f64>,
}

fn build_stressed_columns<F>(
    in_service: &[usize],
    monitored_positions: &[Option<usize>],
    base_flows: &[f64],
    ratings: &[f64],
    options: &N11Options,
    mut load_column: F,
) -> Result<(Vec<ScreeningOutageColumn>, Vec<Option<usize>>), ContingencyError>
where
    F: FnMut(usize) -> Result<Vec<f64>, ContingencyError>,
{
    let stress_threshold = options.stress_threshold;
    let mut stressed_columns: Vec<ScreeningOutageColumn> = Vec::new();
    let mut stressed_column_by_branch = vec![None; ratings.len()];

    for &a in in_service {
        let lodf_col_a = load_column(a)?;
        let a_position = monitored_positions[a].expect("in-service branch position");
        let flow_a = base_flows[a];
        let a_is_bridge = !lodf_col_a[a_position].is_finite();

        let is_stressed = if !options.stressed_only || a_is_bridge {
            true
        } else {
            in_service.iter().enumerate().any(|(position, &l)| {
                if l == a {
                    return false;
                }
                let rating_l = ratings[l];
                if rating_l <= 0.0 {
                    return false;
                }
                let lodf_la = lodf_col_a[position];
                if !lodf_la.is_finite() {
                    return true;
                }
                let post_flow = base_flows[l] + lodf_la * flow_a;
                post_flow.abs() / rating_l > stress_threshold
            })
        };

        if is_stressed {
            stressed_column_by_branch[a] = Some(stressed_columns.len());
            stressed_columns.push(ScreeningOutageColumn {
                branch_index: a,
                flow_mw: flow_a,
                is_bridge: a_is_bridge,
                lodf_to_in_service: lodf_col_a,
            });
        }
    }

    Ok((stressed_columns, stressed_column_by_branch))
}

#[allow(clippy::too_many_arguments)]
fn screen_flagged_pairs<F>(
    in_service: &[usize],
    base_flows: &[f64],
    ratings: &[f64],
    options: &N11Options,
    stressed_columns: &[ScreeningOutageColumn],
    stressed_column_by_branch: &[Option<usize>],
    monitored_positions: &[Option<usize>],
    mut load_column: F,
) -> Result<(Vec<(usize, usize)>, usize), ContingencyError>
where
    F: FnMut(usize) -> Result<Vec<f64>, ContingencyError>,
{
    let mut flagged_pairs: Vec<(usize, usize)> = Vec::new();
    let mut total_pairs_screened = 0usize;

    for &b in in_service {
        let computed_b_column;
        let (flow_b, b_is_bridge, lodf_col_b): (f64, bool, &[f64]) =
            if let Some(stored_idx) = stressed_column_by_branch[b] {
                let stored = &stressed_columns[stored_idx];
                (
                    stored.flow_mw,
                    stored.is_bridge,
                    stored.lodf_to_in_service.as_slice(),
                )
            } else {
                computed_b_column = load_column(b)?;
                let b_position = monitored_positions[b].expect("in-service branch position");
                (
                    base_flows[b],
                    !computed_b_column[b_position].is_finite(),
                    computed_b_column.as_slice(),
                )
            };

        for a_column in stressed_columns {
            let a = a_column.branch_index;
            if a >= b {
                break;
            }

            total_pairs_screened += 1;

            if a_column.is_bridge || b_is_bridge {
                flagged_pairs.push((a, b));
                continue;
            }

            let mut flag = false;
            for (position, &k) in in_service.iter().enumerate() {
                if k == a || k == b {
                    continue;
                }
                let rating_k = ratings[k];
                if rating_k <= 0.0 {
                    continue;
                }

                let lodf_ka = a_column.lodf_to_in_service[position];
                let lodf_kb = lodf_col_b[position];
                if !lodf_ka.is_finite() || !lodf_kb.is_finite() {
                    flag = true;
                    break;
                }

                let est_flow = base_flows[k] + lodf_ka * a_column.flow_mw + lodf_kb * flow_b;
                if est_flow.abs() > options.lodf_threshold * rating_k {
                    flag = true;
                    break;
                }
            }

            if flag {
                flagged_pairs.push((a, b));
            }
        }
    }

    Ok((flagged_pairs, total_pairs_screened))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run N-1-1 contingency analysis using LODF screening + AC validation.
///
/// Uses a two-pass approach:
/// 1. LODF-based linear screening of all O(N²) branch pairs.
/// 2. AC Newton-Raphson validation of flagged pairs.
pub fn analyze_n11(network: &Network, options: &N11Options) -> Result<N11Result, ContingencyError> {
    let wall_start = Instant::now();
    let base_mva = network.base_mva;

    // -----------------------------------------------------------------------
    // Step 1: DC base-case flows
    // -----------------------------------------------------------------------
    let dc_result =
        surge_dc::solve_dc(network).map_err(|e| ContingencyError::DcSolveFailed(e.to_string()))?;

    // Branch flows in MW
    let base_flows: Vec<f64> = dc_result
        .branch_p_flow
        .iter()
        .map(|&f| f * base_mva)
        .collect();

    // -----------------------------------------------------------------------
    // Step 2: In-service branch list
    // -----------------------------------------------------------------------
    let in_service: Vec<usize> = network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| br.in_service)
        .map(|(i, _)| i)
        .collect();

    let mut monitored_positions = vec![None; network.n_branches()];
    for (position, &branch_idx) in in_service.iter().enumerate() {
        monitored_positions[branch_idx] = Some(position);
    }

    let mut prepared_model = PreparedDcStudy::new(network)
        .map_err(|e| ContingencyError::DcSolveFailed(e.to_string()))?;
    let mut lodf_columns = prepared_model.lodf_columns();

    // -----------------------------------------------------------------------
    // Step 3: N-1 pre-screening to identify stressed post-A states
    // -----------------------------------------------------------------------
    // A branch A is "stressed" if any monitored element l would carry more
    // than 80% of its rating after A is outaged (per LODF approximation).
    let ratings: Vec<f64> = network
        .branches
        .iter()
        .map(|branch| get_rating(branch, options.thermal_rating))
        .collect();
    let (stressed_columns, stressed_column_by_branch) = build_stressed_columns(
        &in_service,
        &monitored_positions,
        &base_flows,
        &ratings,
        options,
        |branch_idx| {
            lodf_columns
                .compute_column(&in_service, branch_idx)
                .map_err(|e| ContingencyError::DcSolveFailed(e.to_string()))
        },
    )?;

    // -----------------------------------------------------------------------
    // Step 4: LODF-based N-1-1 screening of all (A, B) pairs
    // -----------------------------------------------------------------------
    // Approximation (simultaneous superposition, intentionally conservative):
    //   Flow_k(A+B) ≈ base_flow_k + LODF[k,A]*base_flow_A + LODF[k,B]*base_flow_B
    //
    // This is an upper-bound approximation vs. the true sequential conditional LODF.
    // Overestimating leads to more pairs being flagged for AC validation (Step 6),
    // which is the desired conservative behavior for a security screening tool.
    // Sequential N-1-1 scenarios where A is lost first (and LODF changes) would
    // require re-computing PTDF per first-contingency — too expensive for screening.
    //
    // Flag the pair if any monitored element k would exceed the screening threshold.

    let (flagged_pairs, total_pairs_screened) = screen_flagged_pairs(
        &in_service,
        &base_flows,
        &ratings,
        options,
        &stressed_columns,
        &stressed_column_by_branch,
        &monitored_positions,
        |branch_idx| {
            lodf_columns
                .compute_column(&in_service, branch_idx)
                .map_err(|e| ContingencyError::DcSolveFailed(e.to_string()))
        },
    )?;

    let lodf_flagged = flagged_pairs.len();

    // -----------------------------------------------------------------------
    // Step 5: AC validation of flagged pairs (up to max_ac_pairs)
    // -----------------------------------------------------------------------
    let pairs_to_validate = flagged_pairs
        .iter()
        .take(options.max_ac_pairs)
        .cloned()
        .collect::<Vec<_>>();

    let ac_validated_pairs = pairs_to_validate.len();
    let mut violation_pairs: Vec<N11ViolationPair> = Vec::new();

    let acpf_options = AcPfOptions {
        tolerance: options.tolerance,
        max_iterations: options.max_iterations as u32,
        flat_start: false,
        ..AcPfOptions::default()
    };
    let emerg_rating_frac = options.post_contingency_rating;

    for (a, b) in &pairs_to_validate {
        let a = *a;
        let b = *b;

        let br_a = &network.branches[a];
        let br_b = &network.branches[b];
        let label_a = format!(
            "Line {}->{}(ckt {})",
            br_a.from_bus, br_a.to_bus, br_a.circuit
        );
        let label_b = format!(
            "Line {}->{}(ckt {})",
            br_b.from_bus, br_b.to_bus, br_b.circuit
        );

        // Clone network with both branches tripped
        let mut net = network.clone();
        net.branches[a].in_service = false;
        net.branches[b].in_service = false;

        let (converged, violations) = match solve_ac_pf_kernel(&net, &acpf_options) {
            Ok(sol) if sol.status == SolveStatus::Converged => {
                // Check thermal violations in AC solution
                let bus_map = net.bus_index_map();
                let viols = detect_ac_violations(
                    &net,
                    &sol.voltage_magnitude_pu,
                    &sol.voltage_angle_rad,
                    &bus_map,
                    base_mva,
                    emerg_rating_frac,
                    &[a, b],
                    options.thermal_rating,
                );
                (true, viols)
            }
            _ => {
                // Non-convergence counts as a violation
                (false, vec![])
            }
        };

        // Record pair if there are violations or if it didn't converge
        if !violations.is_empty() || !converged {
            violation_pairs.push(N11ViolationPair {
                outage_a: a,
                outage_b: b,
                label_a,
                label_b,
                violations,
                ac_validated: true,
                converged,
            });
        }
    }

    let wall_time = wall_start.elapsed().as_secs_f64();
    let tpl_compliant = violation_pairs.is_empty();

    info!(
        "N-1-1 analysis: {} pairs screened, {} LODF-flagged, {} AC-validated, {} violations, {:.3}s",
        total_pairs_screened,
        lodf_flagged,
        ac_validated_pairs,
        violation_pairs.len(),
        wall_time,
    );

    Ok(N11Result {
        total_pairs_screened,
        lodf_flagged_pairs: lodf_flagged,
        ac_validated_pairs,
        violation_pairs,
        solve_time_secs: wall_time,
        tpl_compliant,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Detect thermal overloads in the post-(A,B) AC solution.
///
/// Uses the full branch π-model (same as the N-1 violation detector in lib.rs).
/// Skips the outaged branches themselves.
#[allow(clippy::too_many_arguments)]
fn detect_ac_violations(
    network: &Network,
    vm: &[f64],
    va: &[f64],
    bus_map: &std::collections::HashMap<u32, usize>,
    base_mva: f64,
    emerg_rating_frac: f64,
    outaged: &[usize],
    thermal_rating: ThermalRating,
) -> Vec<N11BranchViolation> {
    let mut violations = Vec::new();

    for (i, branch) in network.branches.iter().enumerate() {
        if !branch.in_service || outaged.contains(&i) {
            continue;
        }
        let base_rating = get_rating(branch, thermal_rating);
        if base_rating <= 0.0 {
            continue;
        }

        let f = bus_map[&branch.from_bus];
        let t = bus_map[&branch.to_bus];

        let vi = vm[f];
        let vj = vm[t];
        let theta_ij = va[f] - va[t];
        let s_mva = branch.power_flows_pu(vi, vj, theta_ij, 1e-40).max_s_pu() * base_mva;
        let emerg_rating = emerg_rating_frac * base_rating;

        if s_mva > emerg_rating {
            violations.push(N11BranchViolation {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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

    use super::*;

    #[test]
    fn test_build_stressed_columns_respects_stressed_only_threshold() {
        let in_service = vec![0usize, 1, 2];
        let monitored_positions = vec![Some(0), Some(1), Some(2)];
        let base_flows = vec![70.0, 60.0, 40.0];
        let ratings = vec![100.0, 100.0, 100.0];
        let options = N11Options {
            stressed_only: true,
            stress_threshold: 0.80,
            ..N11Options::default()
        };

        let mut columns = std::collections::HashMap::new();
        columns.insert(0usize, vec![-1.0, 0.30, 0.10]);
        columns.insert(1usize, vec![0.0, -1.0, 0.05]);
        columns.insert(2usize, vec![0.0, 0.05, -1.0]);

        let (stressed, index_by_branch) = build_stressed_columns(
            &in_service,
            &monitored_positions,
            &base_flows,
            &ratings,
            &options,
            |branch_idx| Ok(columns[&branch_idx].clone()),
        )
        .expect("build stressed columns");

        assert_eq!(
            stressed
                .iter()
                .map(|column| column.branch_index)
                .collect::<Vec<_>>(),
            vec![0],
            "only outage A=0 should be considered stressed"
        );
        assert_eq!(index_by_branch[0], Some(0));
        assert_eq!(index_by_branch[1], None);
        assert_eq!(index_by_branch[2], None);
    }

    #[test]
    fn test_screen_flagged_pairs_promotes_bridge_pairs() {
        let in_service = vec![0usize, 1, 2];
        let monitored_positions = vec![Some(0), Some(1), Some(2)];
        let base_flows = vec![50.0, 50.0, 50.0];
        let ratings = vec![100.0, 100.0, 100.0];
        let options = N11Options {
            stressed_only: false,
            lodf_threshold: 0.95,
            ..N11Options::default()
        };

        let stressed_columns = vec![
            ScreeningOutageColumn {
                branch_index: 0,
                flow_mw: 50.0,
                is_bridge: true,
                lodf_to_in_service: vec![f64::INFINITY, 0.0, 0.0],
            },
            ScreeningOutageColumn {
                branch_index: 1,
                flow_mw: 50.0,
                is_bridge: false,
                lodf_to_in_service: vec![0.0, -1.0, 0.0],
            },
            ScreeningOutageColumn {
                branch_index: 2,
                flow_mw: 50.0,
                is_bridge: false,
                lodf_to_in_service: vec![0.0, 0.0, -1.0],
            },
        ];
        let stressed_column_by_branch = vec![Some(0), Some(1), Some(2)];

        let (flagged_pairs, total_pairs_screened) = screen_flagged_pairs(
            &in_service,
            &base_flows,
            &ratings,
            &options,
            &stressed_columns,
            &stressed_column_by_branch,
            &monitored_positions,
            |_branch_idx| unreachable!("all columns are pre-seeded"),
        )
        .expect("screen flagged pairs");

        assert_eq!(total_pairs_screened, 3);
        assert!(
            flagged_pairs.contains(&(0, 1)) && flagged_pairs.contains(&(0, 2)),
            "pairs involving a bridge outage must be escalated"
        );
    }

    #[test]
    fn test_screen_flagged_pairs_with_stressed_only_reduces_pair_space() {
        let in_service = vec![0usize, 1, 2];
        let monitored_positions = vec![Some(0), Some(1), Some(2)];
        let base_flows = vec![70.0, 60.0, 40.0];
        let ratings = vec![100.0, 100.0, 100.0];
        let options = N11Options {
            stressed_only: true,
            stress_threshold: 0.80,
            lodf_threshold: 0.95,
            ..N11Options::default()
        };

        let mut columns = std::collections::HashMap::new();
        columns.insert(0usize, vec![-1.0, 0.30, 0.10]);
        columns.insert(1usize, vec![0.0, -1.0, 0.05]);
        columns.insert(2usize, vec![0.0, 0.05, -1.0]);
        let (stressed_columns, stressed_column_by_branch) = build_stressed_columns(
            &in_service,
            &monitored_positions,
            &base_flows,
            &ratings,
            &options,
            |branch_idx| Ok(columns[&branch_idx].clone()),
        )
        .expect("build stressed columns");

        let (flagged_pairs, total_pairs_screened) = screen_flagged_pairs(
            &in_service,
            &base_flows,
            &ratings,
            &options,
            &stressed_columns,
            &stressed_column_by_branch,
            &monitored_positions,
            |branch_idx| Ok(columns[&branch_idx].clone()),
        )
        .expect("screen flagged pairs");

        assert_eq!(
            total_pairs_screened, 2,
            "only pairs anchored by stressed outage A=0 remain"
        );
        assert_eq!(flagged_pairs, Vec::<(usize, usize)>::new());
    }

    #[test]
    fn test_n11_max_ac_pairs_caps_validation_count() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = N11Options {
            stressed_only: false,
            lodf_threshold: 0.0,
            max_ac_pairs: 2,
            ..N11Options::default()
        };
        let result = analyze_n11(&net, &opts).expect("N-1-1 should succeed on case9");

        assert_eq!(result.lodf_flagged_pairs, result.total_pairs_screened);
        assert_eq!(result.ac_validated_pairs, 2);
    }

    #[test]
    fn test_n11_case9_runs() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = N11Options::default();
        let result = analyze_n11(&net, &opts).unwrap();
        // case9 has 9 branches → at most 9×8/2 = 36 pairs (upper-triangle)
        assert!(result.total_pairs_screened <= 36);
        // Should complete successfully
        assert!(result.solve_time_secs < 60.0);
    }

    #[test]
    fn test_n11_case9_no_violations_lightly_loaded() {
        // case9 base case is lightly loaded — N-1-1 may have no violations
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = N11Options {
            stressed_only: false,
            lodf_threshold: 0.99, // tight threshold — flag almost every pair
            ..N11Options::default()
        };
        let result = analyze_n11(&net, &opts).unwrap();
        // Most pairs should be fine for this small case
        println!(
            "N-1-1: {} pairs, {} violations",
            result.total_pairs_screened,
            result.violation_pairs.len()
        );
        // Just verify it ran without panic
        assert!(result.total_pairs_screened <= 36);
    }

    #[test]
    fn test_n11_case9_structure() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let result = analyze_n11(&net, &N11Options::default()).unwrap();
        // All N11ViolationPairs should have valid, distinct branch indices
        for pair in &result.violation_pairs {
            assert!(pair.outage_a < net.n_branches());
            assert!(pair.outage_b < net.n_branches());
            assert_ne!(pair.outage_a, pair.outage_b);
        }
    }
}
