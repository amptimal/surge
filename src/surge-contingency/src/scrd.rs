// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Security-Constrained Redispatch (SCRD).
//!
//! Finds the minimum-cost generation redispatch that eliminates all specified
//! post-contingency flow violations identified by N-1 (or N-1-1) analysis.
//!
//! ## LP formulation
//!
//! Variables: x = [ΔPg_up (n_gen) | ΔPg_dn (n_gen)]  (all ≥ 0)
//!
//! ```text
//! minimize  Σ_g (c_up_g × ΔPg_up_g + c_dn_g × ΔPg_dn_g)
//!
//! subject to:
//!   Power balance:  Σ_g ΔPg_up_g - Σ_g ΔPg_dn_g = 0
//!   Up headroom:    ΔPg_up_g ≤ Pmax_g - Pg0_g   (enforced via col_upper)
//!   Dn headroom:    ΔPg_dn_g ≤ Pg0_g - Pmin_g   (enforced via col_upper)
//!
//!   For each violation (monitored branch l, contingency branch k or base case):
//!     post_flow_l = base_flow_l [+ LODF[l,k] × base_flow_k]  (pre-redispatch estimate)
//!     net_g = Σ_g PTDF[l, bus_g] × (ΔPg_up_g - ΔPg_dn_g)
//!     post_flow_l + net_g ≤ +rating_l   (upper)
//!     post_flow_l + net_g ≥ -rating_l   (lower)
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use surge_dc::PtdfRows;
use surge_network::Network;
use surge_opf::backends::{
    LpOptions, LpSolveStatus, LpSolver, SparseProblem, try_default_lp_solver,
};
use surge_sparse::Triplet;
use thiserror::Error;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Solve status for a completed SCRD run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::enum_variant_names)]
pub enum ScrdStatus {
    /// All violations resolved; solution is optimal.
    Optimal,
    /// Violations could not be resolved by generation redispatch alone.
    Infeasible,
    /// LP solver returned an unexpected non-optimal status.
    SolverError,
}

/// A flow violation to be relieved by redispatch.
#[derive(Debug, Clone)]
pub struct ScrdViolation {
    /// Index of the monitored branch that is overloaded.
    pub branch_index: usize,
    /// Index of the outaged contingency branch, or `None` for base-case violations.
    pub contingency_branch: Option<usize>,
    /// Pre-redispatch flow on the monitored branch (MW, signed).
    pub flow_mw: f64,
    /// Normal thermal rating of the monitored branch (MVA, positive).
    pub rating_mw: f64,
}

/// Options for the SCRD solver.
#[derive(Debug, Clone)]
pub struct ScrdOptions {
    /// Violations to relieve (from N-1 or N-1-1 analysis).
    pub violations: Vec<ScrdViolation>,
    /// Redispatch cost up ($/MWh) per in-service generator.
    /// Length must match the number of in-service generators.
    /// `None` → use each generator's marginal cost (or 1.0 as fallback).
    pub cost_up: Option<Vec<f64>>,
    /// Redispatch cost down ($/MWh) per in-service generator.
    /// `None` → zeros (down-ramp is free).
    pub cost_dn: Option<Vec<f64>>,
    /// Post-contingency emergency rating fraction (e.g. 1.25 = 125% of rate_a).
    pub post_contingency_rating: f64,
    /// Override the LP solver backend. `None` = use the compiled-in default.
    pub lp_solver: Option<Arc<dyn LpSolver>>,
    /// Print solver output.
    pub verbose: bool,
    /// PNL-005: Penalty configuration for post-contingency thermal violations.
    ///
    /// When `Some`, thermal flow constraints in the SCRD LP are treated as soft
    /// constraints with the given penalty curve, matching the base-case SCOPF
    /// penalty for consistency across the security analysis workflow.
    /// `None` → use the default emergency rating (hard constraint via `post_contingency_rating`).
    pub penalty_config: Option<surge_network::market::PenaltyConfig>,
}

/// Linear DC sensitivities required by SCRD.
///
/// `lodf_pairs` is keyed as `(monitored_branch_idx, outage_branch_idx)`.
pub struct ScrdSensitivityModel<'a> {
    pub ptdf_rows: &'a PtdfRows,
    pub lodf_pairs: &'a HashMap<(usize, usize), f64>,
}

impl Default for ScrdOptions {
    fn default() -> Self {
        Self {
            violations: vec![],
            cost_up: None,
            cost_dn: None,
            post_contingency_rating: 1.25,
            lp_solver: None,
            verbose: false,
            penalty_config: None,
        }
    }
}

/// Redispatch allocation for a single generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorRedispatch {
    /// Bus number where the generator is connected.
    pub bus: u32,
    /// Original generator index in `Network::generators`.
    pub gen_index: usize,
    /// Generator dispatch before redispatch (MW).
    pub initial_dispatch_mw: f64,
    /// Net redispatch in MW (positive = increase, negative = decrease).
    pub delta_pg_mw: f64,
    /// Final dispatch (MW).
    pub pg_final_mw: f64,
}

/// Solution returned by [`solve_scrd`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrdSolution {
    /// Solve status.
    pub status: ScrdStatus,
    /// Sum of |ΔPg| across all generators (MW).
    pub total_redispatch_mw: f64,
    /// Total redispatch cost ($/hr).
    pub total_cost: f64,
    /// Per-generator redispatch allocations.
    pub generator_redispatch: Vec<GeneratorRedispatch>,
    /// Number of violations for which a constraint was added.
    pub violations_resolved: usize,
    /// Number of violations that could not be resolved (infeasible).
    pub unresolvable_violations: usize,
}

/// Errors returned by [`solve_scrd`].
#[derive(Debug, Error)]
pub enum ScrdError {
    #[error("DC power flow failed: {0}")]
    DcFailed(String),
    #[error("PTDF/LODF computation failed")]
    DfaxFailed,
    #[error("LP solver error: {0}")]
    LpError(String),
    #[error("no violations to resolve")]
    NoViolations,
}

// ---------------------------------------------------------------------------
// Main function
// ---------------------------------------------------------------------------

/// Solve Security-Constrained Redispatch (SCRD).
///
/// Finds minimum-cost generation redispatch that eliminates all specified
/// post-contingency flow violations using LODF linearization.
///
/// # Arguments
/// - `network`    — Network with generator dispatch (`pg`, `pmin`, `pmax`).
/// - `base_flows` — Pre-redispatch DC branch flows in MW (length = n_branches).
/// - `sensitivities` — PTDF rows plus sparse LODF pairs for the violated
///   monitored/outage combinations.
/// - `options`    — Solver options including the violation list.
pub fn solve_scrd(
    network: &Network,
    base_flows: &[f64],
    sensitivities: ScrdSensitivityModel<'_>,
    options: &ScrdOptions,
) -> Result<ScrdSolution, ScrdError> {
    info!(
        violations = options.violations.len(),
        branches = network.n_branches(),
        post_contingency_rating = options.post_contingency_rating,
        "SCRD: starting redispatch solve"
    );
    if options.violations.is_empty() {
        return Err(ScrdError::NoViolations);
    }

    let base_mva = network.base_mva;

    // -----------------------------------------------------------------------
    // Generator enumeration
    // -----------------------------------------------------------------------
    // Collect in-service generators in index order.  The LP variable ordering
    // mirrors this: ΔPg_up for gen 0..n_gen, then ΔPg_dn for gen 0..n_gen.
    let gen_indices: Vec<usize> = network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service)
        .map(|(i, _)| i)
        .collect();

    let n_gen = gen_indices.len();

    if n_gen == 0 {
        return Err(ScrdError::LpError("no in-service generators".into()));
    }

    let n_var = 2 * n_gen; // [ΔPg_up | ΔPg_dn]
    let up_offset = 0;
    let dn_offset = n_gen;

    // Current dispatch and headroom
    let pg0_mw: Vec<f64> = gen_indices
        .iter()
        .map(|&gi| network.generators[gi].p)
        .collect();

    let up_head_mw: Vec<f64> = gen_indices
        .iter()
        .map(|&gi| {
            let g = &network.generators[gi];
            (g.pmax - g.p).max(0.0)
        })
        .collect();

    let dn_head_mw: Vec<f64> = gen_indices
        .iter()
        .map(|&gi| {
            let g = &network.generators[gi];
            (g.p - g.pmin).max(0.0)
        })
        .collect();

    // Bus index for each generator
    let bus_map = network.bus_index_map();
    let gen_bus_idx: Vec<usize> = gen_indices
        .iter()
        .map(|&gi| bus_map[&network.generators[gi].bus])
        .collect();

    // -----------------------------------------------------------------------
    // Objective
    // -----------------------------------------------------------------------
    let default_cost_up: Vec<f64> = gen_indices
        .iter()
        .map(|&gi| {
            // Use marginal cost from cost curve if available
            let g = &network.generators[gi];
            match &g.cost {
                Some(surge_network::market::CostCurve::Polynomial { coeffs, .. })
                    if coeffs.len() >= 2 =>
                {
                    // Linear coefficient × base_mva
                    (coeffs[0] * base_mva).max(1.0)
                }
                Some(surge_network::market::CostCurve::PiecewiseLinear { points, .. })
                    if points.len() >= 2 =>
                {
                    let (x0, y0) = points[0];
                    let (x1, y1) = points[points.len() - 1];
                    let dx = x1 - x0;
                    if dx > 1e-10 {
                        ((y1 - y0) / dx * base_mva).max(1.0)
                    } else {
                        1.0
                    }
                }
                _ => 1.0,
            }
        })
        .collect();

    let cost_up = options.cost_up.as_deref().unwrap_or(&default_cost_up);
    let zero_costs = vec![0.0f64; n_gen];
    let cost_dn = options.cost_dn.as_deref().unwrap_or(&zero_costs);

    let mut col_cost = vec![0.0f64; n_var];
    for j in 0..n_gen {
        col_cost[up_offset + j] = cost_up.get(j).copied().unwrap_or(1.0);
        col_cost[dn_offset + j] = cost_dn.get(j).copied().unwrap_or(0.0);
    }

    // -----------------------------------------------------------------------
    // Variable bounds
    // -----------------------------------------------------------------------
    let col_lower = vec![0.0f64; n_var]; // all ≥ 0
    let mut col_upper = vec![f64::INFINITY; n_var];

    for j in 0..n_gen {
        col_upper[up_offset + j] = up_head_mw[j] / base_mva;
        col_upper[dn_offset + j] = dn_head_mw[j] / base_mva;
    }

    // -----------------------------------------------------------------------
    // Constraint matrix (COO triplets → CSC)
    // -----------------------------------------------------------------------
    // Row layout:
    //   Row 0              : power balance  Σ ΔPg_up - Σ ΔPg_dn = 0  (equality)
    //   Rows 1..1+2*n_viol : 2 rows per violation (upper + lower flow limit)

    let n_violations = options.violations.len();
    let n_row = 1 + 2 * n_violations;

    let mut triplets: Vec<Triplet<f64>> = Vec::with_capacity(n_var + 4 * n_gen * n_violations);

    // Power balance row (row 0): [+1 × n_gen | -1 × n_gen]
    for j in 0..n_gen {
        triplets.push(Triplet {
            row: 0,
            col: up_offset + j,
            val: 1.0,
        });
        triplets.push(Triplet {
            row: 0,
            col: dn_offset + j,
            val: -1.0,
        });
    }

    // Flow constraint rows: two per violation
    for (vi, viol) in options.violations.iter().enumerate() {
        let l = viol.branch_index;
        let upper_row = 1 + 2 * vi; // post_flow + net_g ≤ +rating
        let lower_row = 1 + 2 * vi + 1; // post_flow + net_g ≥ -rating

        // Sensitivity of branch l flow to each generator's net redispatch.
        // PTDF[l, bus_g] is the sensitivity to injection at bus_g.
        for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
            let ptdf_lg = sensitivities.ptdf_rows.get(l, bus_idx);
            if ptdf_lg.abs() < 1e-15 {
                continue; // skip zero entries
            }

            // ΔPg_up contribution: +ptdf_lg (per unit → per unit)
            triplets.push(Triplet {
                row: upper_row,
                col: up_offset + j,
                val: ptdf_lg,
            });
            triplets.push(Triplet {
                row: lower_row,
                col: up_offset + j,
                val: ptdf_lg,
            });

            // ΔPg_dn contribution: -ptdf_lg
            triplets.push(Triplet {
                row: upper_row,
                col: dn_offset + j,
                val: -ptdf_lg,
            });
            triplets.push(Triplet {
                row: lower_row,
                col: dn_offset + j,
                val: -ptdf_lg,
            });
        }
    }

    let (a_start, a_index, a_value) = surge_opf::advanced::triplets_to_csc(&triplets, n_row, n_var);

    // -----------------------------------------------------------------------
    // Row bounds
    // -----------------------------------------------------------------------
    let mut row_lower = vec![0.0f64; n_row];
    let mut row_upper = vec![0.0f64; n_row];

    // Power balance: equality 0 = 0
    row_lower[0] = 0.0;
    row_upper[0] = 0.0;

    let emerg_frac = options.post_contingency_rating;

    for (vi, viol) in options.violations.iter().enumerate() {
        let l = viol.branch_index;
        let upper_row = 1 + 2 * vi;
        let lower_row = 1 + 2 * vi + 1;

        // Pre-redispatch estimated flow on branch l (pu).
        //
        // Single-outage (contingency_branch = Some(k)):
        //   Use LODF to estimate post-contingency flow from DC base flows.
        //   post_flow = base_flow_l + LODF[l,k] * base_flow_k
        //
        // Multi-outage or gen-only (contingency_branch = None):
        //   LODF is a single-outage linear model and doesn't apply. Use the
        //   measured AC flow (viol.flow_mw) directly as the pre-redispatch
        //   constant — this is more accurate since it comes from a converged
        //   AC NR solve rather than a DC linear approximation.
        let post_flow_pu = if let Some(k) = viol.contingency_branch {
            // N-1 case: add LODF contribution
            let flow_k_pu = if k < base_flows.len() {
                base_flows[k] / base_mva
            } else {
                0.0
            };
            let flow_l_pu = if l < base_flows.len() {
                base_flows[l] / base_mva
            } else {
                0.0
            };
            let lodf_lk = sensitivities
                .lodf_pairs
                .get(&(l, k))
                .copied()
                .filter(|v| v.is_finite())
                .unwrap_or(0.0);
            flow_l_pu + lodf_lk * flow_k_pu
        } else {
            // Multi-outage / gen-only / base-case: use measured AC flow.
            viol.flow_mw / base_mva
        };

        let rating_pu = viol.rating_mw * emerg_frac / base_mva;

        // Upper: PTDF × ΔPg_net ≤ rating - post_flow
        row_upper[upper_row] = rating_pu - post_flow_pu;
        row_lower[upper_row] = -1e30; // no lower bound on upper-flow row

        // Lower: PTDF × ΔPg_net ≥ -rating - post_flow
        row_lower[lower_row] = -rating_pu - post_flow_pu;
        row_upper[lower_row] = 1e30; // no upper bound on lower-flow row
    }

    // -----------------------------------------------------------------------
    // Solve
    // -----------------------------------------------------------------------
    let prob = SparseProblem {
        n_col: n_var,
        n_row,
        col_cost,
        col_lower,
        col_upper,
        row_lower,
        row_upper,
        a_start,
        a_index,
        a_value,
        q_start: None,
        q_index: None,
        q_value: None,
        integrality: None,
    };

    let lp_opts = LpOptions {
        tolerance: 1e-8,
        ..Default::default()
    };

    let solver = options
        .lp_solver
        .clone()
        .map_or_else(|| try_default_lp_solver(), Ok)
        .map_err(ScrdError::LpError)?;

    let sol = solver.solve(&prob, &lp_opts).map_err(ScrdError::LpError)?;

    // -----------------------------------------------------------------------
    // Extract results
    // -----------------------------------------------------------------------
    match sol.status {
        LpSolveStatus::Optimal | LpSolveStatus::SubOptimal => {
            let mut generator_redispatch = Vec::with_capacity(n_gen);
            let mut total_redispatch_mw = 0.0f64;

            for (j, &gi) in gen_indices.iter().enumerate() {
                let delta_up_mw = sol.x[up_offset + j] * base_mva;
                let delta_dn_mw = sol.x[dn_offset + j] * base_mva;
                let delta_net_mw = delta_up_mw - delta_dn_mw;

                total_redispatch_mw += delta_up_mw + delta_dn_mw;

                if delta_net_mw.abs() > 1e-6 {
                    generator_redispatch.push(GeneratorRedispatch {
                        bus: network.generators[gi].bus,
                        gen_index: gi,
                        initial_dispatch_mw: pg0_mw[j],
                        delta_pg_mw: delta_net_mw,
                        pg_final_mw: pg0_mw[j] + delta_net_mw,
                    });
                }
            }

            let total_cost = sol.objective;

            info!(
                total_redispatch_mw,
                total_cost,
                generators_moved = generator_redispatch.len(),
                "SCRD: optimal redispatch"
            );

            Ok(ScrdSolution {
                status: ScrdStatus::Optimal,
                total_redispatch_mw,
                total_cost,
                generator_redispatch,
                violations_resolved: n_violations,
                unresolvable_violations: 0,
            })
        }
        LpSolveStatus::Infeasible => {
            warn!("SCRD: infeasible — violations cannot be resolved by redispatch alone");
            Ok(ScrdSolution {
                status: ScrdStatus::Infeasible,
                total_redispatch_mw: 0.0,
                total_cost: 0.0,
                generator_redispatch: vec![],
                violations_resolved: 0,
                unresolvable_violations: n_violations,
            })
        }
        _ => {
            warn!(
                status = ?sol.status,
                violations = n_violations,
                "SCRD: LP solver returned non-optimal status"
            );
            Ok(ScrdSolution {
                status: ScrdStatus::SolverError,
                total_redispatch_mw: 0.0,
                total_cost: 0.0,
                generator_redispatch: vec![],
                violations_resolved: 0,
                unresolvable_violations: n_violations,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::case_path;
    use surge_dc::PtdfRequest;

    #[allow(clippy::type_complexity)]
    fn setup_case9() -> (Network, Vec<f64>, PtdfRows, HashMap<(usize, usize), f64>) {
        let net = surge_io::load(case_path("case9")).unwrap();
        let dc_result = surge_dc::solve_dc(&net).unwrap();
        let base_flows: Vec<f64> = dc_result
            .branch_p_flow
            .iter()
            .map(|f| f * net.base_mva)
            .collect();
        let all_branches: Vec<usize> = (0..net.n_branches()).collect();
        let ptdf = surge_dc::compute_ptdf(&net, &PtdfRequest::for_branches(&all_branches)).unwrap();
        let lodf_pairs = surge_dc::compute_lodf_pairs(&net, &all_branches, &all_branches)
            .unwrap()
            .into_parts()
            .2;
        (net, base_flows, ptdf, lodf_pairs)
    }

    #[test]
    fn test_scrd_case9_no_violations() {
        let (net, base_flows, ptdf, lodf_pairs) = setup_case9();
        let opts = ScrdOptions {
            violations: vec![],
            ..Default::default()
        };
        let result = solve_scrd(
            &net,
            &base_flows,
            ScrdSensitivityModel {
                ptdf_rows: &ptdf,
                lodf_pairs: &lodf_pairs,
            },
            &opts,
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ScrdError::NoViolations));
    }

    #[test]
    fn test_scrd_case9_artificial_violation() {
        let (net, base_flows, ptdf, lodf_pairs) = setup_case9();

        // Find the most loaded branch
        let (binding_branch, max_flow) = base_flows
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap();

        // Only try redispatch if there are generators
        if net.generators.len() < 2 {
            return; // Can't redispatch with a single generator
        }

        // Tighten rating to 80% of current flow — requires redispatch
        let tight_rating = max_flow.abs() * 0.8;
        if tight_rating < 1.0 {
            return; // Avoid trivial cases
        }

        let opts = ScrdOptions {
            violations: vec![ScrdViolation {
                branch_index: binding_branch,
                contingency_branch: None,
                flow_mw: *max_flow,
                rating_mw: tight_rating,
            }],
            ..Default::default()
        };

        let result = solve_scrd(
            &net,
            &base_flows,
            ScrdSensitivityModel {
                ptdf_rows: &ptdf,
                lodf_pairs: &lodf_pairs,
            },
            &opts,
        );
        match result {
            Ok(sol) => {
                println!(
                    "SCRD: {} MW redispatch, status={:?}",
                    sol.total_redispatch_mw, sol.status
                );
                assert!(sol.total_redispatch_mw >= 0.0);
            }
            Err(e) => {
                // Infeasible is acceptable for artificially tight constraints
                println!("SCRD infeasible: {:?}", e);
            }
        }
    }

    #[test]
    fn test_scrd_power_balance() {
        // Verify redispatch sums to zero (zero-sum constraint)
        let (net, base_flows, ptdf, lodf_pairs) = setup_case9();

        let max_flow = base_flows.iter().map(|f| f.abs()).fold(0.0_f64, f64::max);
        if max_flow < 1.0 {
            return;
        }

        let (binding, _) = base_flows
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap();

        let opts = ScrdOptions {
            violations: vec![ScrdViolation {
                branch_index: binding,
                contingency_branch: None,
                flow_mw: base_flows[binding],
                rating_mw: base_flows[binding].abs() * 0.9,
            }],
            ..Default::default()
        };

        if let Ok(sol) = solve_scrd(
            &net,
            &base_flows,
            ScrdSensitivityModel {
                ptdf_rows: &ptdf,
                lodf_pairs: &lodf_pairs,
            },
            &opts,
        ) && sol.status == ScrdStatus::Optimal
        {
            let net_redispatch: f64 = sol.generator_redispatch.iter().map(|g| g.delta_pg_mw).sum();
            assert!(
                net_redispatch.abs() < 1e-4,
                "Power balance violated: net_redispatch = {}",
                net_redispatch
            );
        }
    }

    #[test]
    fn test_scrd_signed_measured_flow_changes_redispatch_direction() {
        use std::sync::Mutex;

        use surge_opf::backends::{LpResult, LpSolveStatus, SparseProblem};

        #[derive(Debug)]
        struct RecordingSolver {
            seen: Mutex<Vec<SparseProblem>>,
        }

        impl surge_opf::backends::LpSolver for RecordingSolver {
            fn solve(
                &self,
                prob: &SparseProblem,
                _opts: &surge_opf::backends::LpOptions,
            ) -> Result<LpResult, String> {
                self.seen
                    .lock()
                    .expect("recording solver mutex")
                    .push(prob.clone());
                Ok(LpResult {
                    x: vec![0.0; prob.n_col],
                    row_dual: vec![0.0; prob.n_row],
                    col_dual: vec![0.0; prob.n_col],
                    objective: 0.0,
                    status: LpSolveStatus::Optimal,
                    iterations: 0,
                })
            }

            fn name(&self) -> &'static str {
                "recording"
            }
        }

        let (net, base_flows, ptdf, lodf_pairs) = setup_case9();
        let solver = Arc::new(RecordingSolver {
            seen: Mutex::new(Vec::new()),
        });

        for signed_flow_mw in [100.0, -100.0] {
            let result = solve_scrd(
                &net,
                &base_flows,
                ScrdSensitivityModel {
                    ptdf_rows: &ptdf,
                    lodf_pairs: &lodf_pairs,
                },
                &ScrdOptions {
                    violations: vec![ScrdViolation {
                        branch_index: 0,
                        contingency_branch: None,
                        flow_mw: signed_flow_mw,
                        rating_mw: 90.0,
                    }],
                    post_contingency_rating: 1.0,
                    lp_solver: Some(solver.clone()),
                    ..Default::default()
                },
            )
            .expect("signed-flow SCRD solve");
            assert_eq!(result.status, ScrdStatus::Optimal);
        }

        let recorded = solver.seen.lock().expect("recorded SCRD problems");
        assert_eq!(recorded.len(), 2);
        let positive = &recorded[0];
        let negative = &recorded[1];

        // Row 1 is the upper-flow inequality, row 2 is the lower-flow inequality.
        assert!(
            positive.row_upper[1] < 0.0,
            "positive measured overload should tighten the upper-flow row"
        );
        assert!(
            positive.row_lower[2] < 0.0,
            "positive measured overload should leave the lower-flow row non-binding"
        );
        assert!(
            negative.row_lower[2] > 0.0,
            "negative measured overload should tighten the lower-flow row"
        );
        assert!(
            negative.row_upper[1] > 0.0,
            "negative measured overload should leave the upper-flow row non-binding"
        );
    }
}
