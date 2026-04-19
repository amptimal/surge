// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Security-constrained SCUC via iterative N-1 constraint generation.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::network::BranchRatingCondition;
use tracing::{debug, info, warn};

use super::solve::{solve_scuc_with_owned_network, solve_scuc_with_problem_spec};
use super::types::SecurityDispatchSpec;
use crate::common::contingency::{ContingencyCut, ContingencyCutKind};
use crate::common::spec::{
    DispatchProblemSpec, ExplicitContingencyCase, ExplicitContingencyElement,
    ExplicitContingencyFlowgate,
};
use crate::dispatch::{CommitmentMode, RawDispatchSolution};
use crate::error::ScedError;
use crate::request::SecurityPreseedMethod;
use crate::result::SecurityDispatchMetadata;

/// An HVDC security cut constraining a monitored branch flow under an HVDC link trip.
///
/// The cut is a linear constraint on existing LP variables. For legacy links it
/// uses one HVDC coefficient; for banded links it uses one coefficient per band.
#[derive(Debug, Clone)]
struct HvdcSecurityCut {
    /// Period index this cut applies to.
    period: usize,
    /// Index into `options.input.hvdc_links`.
    hvdc_link_idx: usize,
    /// Monitored branch index.
    monitored_branch_idx: usize,
    /// Legacy link-level HVDC coefficient for non-banded links.
    coefficient: Option<f64>,
    /// Per-band HVDC coefficients for banded links.
    band_coefficients: Vec<(usize, f64)>,
    /// Thermal limit of the monitored branch (MW).
    f_max_mw: f64,
    /// Violation severity in p.u. over the monitored branch thermal limit.
    excess_pu: f64,
}

#[derive(Debug, Clone)]
struct BranchSecurityViolation {
    period: usize,
    contingency_branch_idx: usize,
    monitored_branch_idx: usize,
    severity_pu: f64,
}

#[derive(Debug, Clone)]
struct HourlyBranchContingency {
    branch_idx: usize,
    from_idx: usize,
    to_idx: usize,
    denom: f64,
}

#[derive(Debug, Clone)]
struct HourlyHvdcContingency {
    hvdc_idx: usize,
    from_idx: usize,
    to_idx: usize,
    band_loss_b: Vec<f64>,
}

struct HourlySecurityContext {
    monitored: Vec<usize>,
    ptdf: surge_dc::PtdfRows,
    branch_contingencies: HashMap<usize, HourlyBranchContingency>,
    connectivity_contingency_branches: Vec<usize>,
    hvdc_contingencies: Vec<HourlyHvdcContingency>,
}

const MAX_CONNECTIVITY_CUT_ROUNDS: usize = 5;

/// Per-unit magnitude cutoff below which a `(contingency, monitored
/// branch)` LODF pair is not emitted as a contingency cut in the
/// explicit-N-1 SCUC LP. Pairs with `|lodf| < cutoff` produce a
/// post-contingency flow contribution of at most
/// `cutoff × pre-contingency flow`, which rounds to near-zero in
/// Gurobi's coefficient tolerance and is presolved away anyway.
///
/// 0.01 is tight enough to retain every practically-binding cut
/// (any pair that can actually drive a post-contingency overload
/// under realistic dispatch shifts), while still dropping the long
/// tail of effectively-zero LODF pairs that would otherwise bloat
/// the LP. Production SCUC tools typically sit in the 0.01–0.05
/// range; 0.01 is the conservative end.
///
/// On 617-bus D1 explicit N-1 (562 contingencies × ~850 monitored
/// × 18 periods = 8.6M mathematical LODF pairs), this filter
/// retains the binding cuts and drops the trivially-zero tail.
/// The iterative SCUC path doesn't consult this cutoff — it only
/// materialises cuts for pairs that actually violate (and uses
/// `violation_tolerance_pu` for that test).
const DEFAULT_CONTINGENCY_CUT_LODF_CUTOFF_PU: f64 = 0.01;

fn solved_switching_state_for_period(
    solution: &RawDispatchSolution,
    hourly_network: &Network,
    period: usize,
) -> Vec<bool> {
    solution
        .branch_commitment_state
        .get(period)
        .cloned()
        .unwrap_or_else(|| {
            hourly_network
                .branches
                .iter()
                .map(|branch| branch.in_service)
                .collect()
        })
}

fn push_connectivity_cut(
    connectivity_cuts: &mut Vec<super::connectivity::IndexedConnectivityCut>,
    period: usize,
    mut cut_set: Vec<usize>,
) -> bool {
    cut_set.sort_unstable();
    cut_set.dedup();
    if connectivity_cuts
        .iter()
        .any(|existing| existing.period == period && existing.cut_set == cut_set)
    {
        return false;
    }
    connectivity_cuts.push(super::connectivity::IndexedConnectivityCut { period, cut_set });
    true
}

fn add_connectivity_cuts_from_solution(
    solution: &RawDispatchSolution,
    hourly_networks: &[Network],
    hourly_contexts: &[HourlySecurityContext],
    connectivity_cuts: &mut Vec<super::connectivity::IndexedConnectivityCut>,
    iteration_label: &str,
    iteration_value: usize,
    refit_round: usize,
) -> bool {
    let mut new_cuts_added = false;
    for (period, (hourly_network, context)) in hourly_networks
        .iter()
        .zip(hourly_contexts.iter())
        .enumerate()
    {
        let switching_state = solved_switching_state_for_period(solution, hourly_network, period);
        for outaged_branch_idx in std::iter::once(None).chain(
            context
                .connectivity_contingency_branches
                .iter()
                .copied()
                .map(Some),
        ) {
            match super::connectivity::check_period_connectivity(
                hourly_network,
                &switching_state,
                outaged_branch_idx,
                None,
            ) {
                super::connectivity::ConnectivityCheck::Connected => {}
                super::connectivity::ConnectivityCheck::Disconnected { cut_set } => {
                    if cut_set.is_empty() {
                        warn!(
                            connectivity_iteration_kind = iteration_label,
                            connectivity_iteration = iteration_value,
                            refit_round,
                            period,
                            contingency_branch_idx = outaged_branch_idx,
                            "SCUC: disconnected switching graph has empty cut set; no \
                             available branch can re-connect it under the current \
                             contingency."
                        );
                        continue;
                    }
                    if push_connectivity_cut(connectivity_cuts, period, cut_set.clone()) {
                        debug!(
                            connectivity_iteration_kind = iteration_label,
                            connectivity_iteration = iteration_value,
                            refit_round,
                            period,
                            contingency_branch_idx = outaged_branch_idx,
                            cut_set_len = cut_set.len(),
                            "SCUC: adding connectivity cut"
                        );
                        new_cuts_added = true;
                    }
                }
            }
        }
    }
    new_cuts_added
}

/// Solve security-constrained dispatch from normalized internal options.
///
/// Algorithm:
/// 1. Solve base SCUC (with any user-provided flowgates).
/// 2. Build hourly network snapshots after applying dispatch profiles.
/// 3. For each hour, compute PTDF rows and contingency metadata for that
///    hour's topology and branch limits.
/// 4. Screen branch and HVDC contingencies against the solved hourly angles and
///    dispatch.
/// 5. Convert the worst violations into hour-specific [`Flowgate`] constraints
///    that are active only in the violating hour.
/// 6. Re-solve SCUC with the augmented flowgate set.
/// 7. Repeat until no violations or `max_iterations` reached.
pub(crate) fn solve_security_dispatch(
    network: &Network,
    options: &SecurityDispatchSpec,
) -> Result<RawDispatchSolution, ScedError> {
    use surge_network::network::Flowgate;

    let base = network.base_mva;
    let scuc_spec = DispatchProblemSpec::from_request(&options.input, &options.commitment);
    let n_bus = network.n_buses();
    let n_periods = options.input.n_periods;
    let min_rate = options.input.min_rate_a;

    // Accumulated flowgates from security screening
    let mut security_flowgates: Vec<Flowgate> = Vec::new();

    let mut last_solution: Option<RawDispatchSolution> = None;
    let mut total_cuts = 0usize;
    let mut last_branch_violations = 0usize;
    let mut last_hvdc_violations = 0usize;
    let mut last_max_branch_violation_pu: Option<f64> = None;
    let mut last_max_hvdc_violation_pu: Option<f64> = None;

    if options.max_iterations == 0 {
        let sol = solve_scuc_with_problem_spec(network, scuc_spec)?;
        return Ok(attach_security_metadata(
            sol,
            SecurityDispatchMetadata {
                iterations: 0,
                n_cuts: 0,
                converged: true,
                last_branch_violations: 0,
                last_hvdc_violations: 0,
                max_branch_violation_pu: None,
                max_hvdc_violation_pu: None,
                n_preseed_cuts: 0,
                n_preseed_pairs_binding: None,
            },
        ));
    }

    let hourly_networks: Vec<Network> = (0..n_periods)
        .map(|hour| super::snapshot::network_at_hour_with_spec(network, &scuc_spec, hour))
        .collect();
    let hourly_contexts: Vec<HourlySecurityContext> = hourly_networks
        .iter()
        .map(|hourly_network| build_hourly_security_context(hourly_network, options, min_rate))
        .collect::<Result<_, _>>()?;

    let mut constrained_pairs: HashSet<(usize, usize, usize)> = HashSet::new();
    let mut hvdc_constrained_pairs: HashSet<(usize, usize, usize)> = HashSet::new();

    // Accumulated connectivity cuts across the security iteration
    // loop. We cap the cut-refit inner loop at 5 rounds per outer
    // iteration — in practice the LP usually converges to a connected
    // pattern within 1-2 rounds; the cap is a safety net for
    // pathological topologies.
    let mut connectivity_cuts: Vec<super::connectivity::IndexedConnectivityCut> = Vec::new();

    info!(
        max_iterations = options.max_iterations,
        max_cuts_per_iteration = options.max_cuts_per_iteration,
        violation_tolerance_pu = options.violation_tolerance_pu,
        n_branch_contingencies = options.contingency_branches.len(),
        n_hvdc_contingencies = options.hvdc_contingency_indices.len(),
        n_periods,
        "Iterative Security SCUC: starting loop"
    );

    // Pre-seed iter 0 with top-ranked (ctg, mon) cuts so the first SCUC
    // already sees a skeleton of structurally-binding N-1 pairs instead
    // of solving blind. The screener dedup set is primed with the same
    // keys so the post-solve pass won't re-materialize them. See
    // `preseed_branch_flowgates` for the ranking heuristic.
    let n_preseed_cuts = if options.preseed_count_per_period > 0
        && !matches!(options.preseed_method, SecurityPreseedMethod::None)
    {
        let preseed_start = std::time::Instant::now();
        let preseeded = preseed_branch_flowgates(
            options,
            &hourly_networks,
            &hourly_contexts,
            n_periods,
            &mut constrained_pairs,
        );
        let n = preseeded.len();
        security_flowgates.extend(preseeded);
        total_cuts += n;
        info!(
            method = ?options.preseed_method,
            count_per_period = options.preseed_count_per_period,
            n_preseed_cuts = n,
            preseed_secs = preseed_start.elapsed().as_secs_f64(),
            "Security SCUC: pre-seeded iter 0 with structural N-1 cuts"
        );
        n
    } else {
        0
    };

    // Cross-iteration MIP warm-start option. When a caller supplies a
    // `warm_start_commitment` via policy, the light warm-start path in
    // `solve_problem` threads it into the MIP's `Start` attribute. We
    // *do not* auto-capture this iter's commitment to feed the next
    // iter — empirically on 617-bus (measured 2026-04-17) it regresses
    // wall time: Gurobi treats the prior commitment as infeasible
    // under the new cut set and spends time repairing rather than
    // reusing. Infrastructure stays wired (policy-supplied hints still
    // apply); the auto-capture step is left off as a knob to flip on
    // when workload dynamics favor it.
    let last_commitment: Option<Vec<Vec<bool>>> = None;

    for iter in 0..options.max_iterations {
        let iter_start = std::time::Instant::now();
        // Build network with accumulated security flowgates
        let mut net = network.clone();
        net.flowgates.extend(security_flowgates.iter().cloned());

        // Build a per-iter CommitmentMode carrying the prior iter's
        // commitment as `warm_start_commitment`. For iter 0 we reuse
        // the caller's original CommitmentMode unchanged. The owned
        // value must live across the solve call so `iter_spec` can
        // reference it.
        let iter_commitment: CommitmentMode = match (&last_commitment, scuc_spec.commitment) {
            (Some(schedule), CommitmentMode::Optimize(opts)) => {
                let mut next_opts = opts.clone();
                next_opts.warm_start_commitment = Some(schedule.clone());
                next_opts.warm_start_commitment_mask = None;
                CommitmentMode::Optimize(next_opts)
            }
            (
                Some(schedule),
                CommitmentMode::Additional {
                    da_commitment,
                    options,
                },
            ) => {
                let mut next_opts = options.clone();
                next_opts.warm_start_commitment = Some(schedule.clone());
                next_opts.warm_start_commitment_mask = None;
                CommitmentMode::Additional {
                    da_commitment: da_commitment.clone(),
                    options: next_opts,
                }
            }
            _ => scuc_spec.commitment.clone(),
        };
        let iter_scuc_spec = scuc_spec.with_commitment(&iter_commitment);

        // Solve SCUC with any accumulated connectivity cuts threaded
        // through the spec so the LP builder emits them as row family
        // `Σ branch_commitment ≥ 1` over the cut set. The first
        // iteration has no cuts; subsequent rounds grow the cut pool
        // until the LP's switching pattern is connected across every
        // period (or we hit the 5-round cap).
        let mut sol: Option<RawDispatchSolution> = None;
        if iter_scuc_spec.allow_branch_switching {
            for refit_round in 0..MAX_CONNECTIVITY_CUT_ROUNDS {
                let spec_with_cuts = iter_scuc_spec.with_connectivity_cuts(&connectivity_cuts);
                let round_sol = solve_scuc_with_problem_spec(&net, spec_with_cuts)?;
                let new_cuts_added = add_connectivity_cuts_from_solution(
                    &round_sol,
                    &hourly_networks,
                    &hourly_contexts,
                    &mut connectivity_cuts,
                    "outer",
                    iter,
                    refit_round,
                );
                if !new_cuts_added {
                    sol = Some(round_sol);
                    break;
                }
                if refit_round + 1 == MAX_CONNECTIVITY_CUT_ROUNDS {
                    warn!(
                        iter,
                        cuts = connectivity_cuts.len(),
                        "SCUC connectivity cut loop hit {MAX_CONNECTIVITY_CUT_ROUNDS}-round \
                         cap — accepting the last solve even though one or more \
                         periods remain disconnected. A downstream connectivity \
                         penalty may apply."
                    );
                    sol = Some(round_sol);
                }
            }
        }
        let sol = match sol {
            Some(s) => s,
            None => solve_scuc_with_problem_spec(&net, iter_scuc_spec)?,
        };
        let inner_solve_secs = iter_start.elapsed().as_secs_f64();

        // Check N-1 violations across all periods
        let mut violations: Vec<BranchSecurityViolation> = Vec::new();
        let mut hvdc_violations: Vec<HvdcSecurityCut> = Vec::new();

        for (t, angles) in sol.bus_angles_rad.iter().enumerate() {
            if angles.len() != n_bus {
                continue;
            }
            let Some(period) = sol.periods.get(t) else {
                continue;
            };
            let hourly_network = &hourly_networks[t];
            let context = &hourly_contexts[t];

            violations.extend(screen_branch_violations(
                t,
                angles,
                hourly_network,
                context,
                base,
                options.violation_tolerance_pu,
                &constrained_pairs,
            ));
            hvdc_violations.extend(screen_hvdc_violations(
                t,
                angles,
                period,
                hourly_network,
                context,
                &options.input.hvdc_links,
                base,
                options.violation_tolerance_pu,
                &hvdc_constrained_pairs,
            ));
        }

        last_branch_violations = violations.len();
        last_hvdc_violations = hvdc_violations.len();
        last_max_branch_violation_pu = violations
            .iter()
            .map(|violation| violation.severity_pu)
            .fold(None::<f64>, |acc, severity| {
                Some(acc.map_or(severity, |prev| prev.max(severity)))
            });
        last_max_hvdc_violation_pu = hvdc_violations.iter().fold(None, |acc, cut| {
            let severity = cut.excess_pu;
            Some(acc.map_or(severity, |prev| prev.max(severity)))
        });

        let screen_secs = iter_start.elapsed().as_secs_f64() - inner_solve_secs;

        if violations.is_empty() && hvdc_violations.is_empty() {
            info!(
                iterations = iter + 1,
                n_security_cuts = total_cuts,
                inner_solve_secs,
                screen_secs,
                total_iter_secs = iter_start.elapsed().as_secs_f64(),
                "Security SCUC converged — no N-1 violations"
            );
            return Ok(attach_security_metadata(
                sol,
                SecurityDispatchMetadata {
                    iterations: iter + 1,
                    n_cuts: total_cuts,
                    converged: true,
                    last_branch_violations,
                    last_hvdc_violations,
                    max_branch_violation_pu: last_max_branch_violation_pu,
                    max_hvdc_violation_pu: last_max_hvdc_violation_pu,
                    n_preseed_cuts,
                    n_preseed_pairs_binding: None,
                },
            ));
        }

        // Sort by severity (worst first) and take top max_cuts_per_iteration unique pairs
        violations.sort_by(|a, b| {
            b.severity_pu
                .partial_cmp(&a.severity_pu)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hvdc_violations.sort_by(|a, b| {
            b.excess_pu
                .partial_cmp(&a.excess_pu)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut new_cuts = 0usize;
        for violation in &violations {
            if new_cuts >= options.max_cuts_per_iteration {
                break;
            }
            let key = (
                violation.period,
                violation.contingency_branch_idx,
                violation.monitored_branch_idx,
            );
            if constrained_pairs.contains(&key) {
                continue;
            }
            constrained_pairs.insert(key);

            let context = &hourly_contexts[violation.period];
            let hourly_network = &hourly_networks[violation.period];
            let fg = build_branch_security_flowgate(violation, hourly_network, context, n_periods);
            let br_l = &hourly_network.branches[violation.monitored_branch_idx];
            let br_k = &hourly_network.branches[violation.contingency_branch_idx];

            debug!(
                iter,
                period = violation.period,
                ctg = format!("{}-{}", br_k.from_bus, br_k.to_bus),
                mon = format!("{}-{}", br_l.from_bus, br_l.to_bus),
                severity_pu = violation.severity_pu,
                limit_mw = br_l.rating_for(BranchRatingCondition::Emergency),
                "Adding N-1 branch security flowgate"
            );

            security_flowgates.push(fg);
            new_cuts += 1;
        }

        // --- Generate HVDC security cuts ---
        // Each HVDC cut creates a flowgate on the monitored branch with an
        // hvdc_coefficients entry encoding the OTDF.
        // Constraint: b_dc_l*(θ_from - θ_to) + (-OTDF_lk)*P_hvdc[k] ∈ [-f_max, f_max]
        for cut in &hvdc_violations {
            if new_cuts >= options.max_cuts_per_iteration {
                break;
            }
            let k = cut.hvdc_link_idx;
            let l = cut.monitored_branch_idx;
            let key = (cut.period, k, l);
            if hvdc_constrained_pairs.contains(&key) {
                continue;
            }
            hvdc_constrained_pairs.insert(key);

            let hourly_network = &hourly_networks[cut.period];
            let fg = build_hvdc_security_flowgate(cut, hourly_network, n_periods);
            let br_l = &hourly_network.branches[l];
            let hvdc = &options.input.hvdc_links[k];

            debug!(
                iter,
                period = cut.period,
                hvdc_link = %hvdc.name,
                mon = format!("{}-{}", br_l.from_bus, br_l.to_bus),
                limit_mw = cut.f_max_mw,
                "Adding N-1 HVDC security flowgate"
            );

            security_flowgates.push(fg);
            new_cuts += 1;
        }

        total_cuts += new_cuts;
        info!(
            iter = iter + 1,
            n_branch_violations = violations.len(),
            n_hvdc_violations = hvdc_violations.len(),
            max_branch_violation_pu = last_max_branch_violation_pu.unwrap_or(0.0),
            max_hvdc_violation_pu = last_max_hvdc_violation_pu.unwrap_or(0.0),
            new_cuts,
            total_cuts,
            inner_solve_secs,
            screen_secs,
            "Security SCUC: added cuts, re-solving"
        );

        last_solution = Some(sol);
    }

    // Max iterations reached — return last solution
    warn!(
        max_iterations = options.max_iterations,
        remaining_violations = "unknown",
        "Security SCUC: max iterations reached"
    );
    Ok(attach_security_metadata(
        last_solution.expect("loop ran at least one iteration"),
        SecurityDispatchMetadata {
            iterations: options.max_iterations,
            n_cuts: total_cuts,
            converged: false,
            last_branch_violations,
            last_hvdc_violations,
            max_branch_violation_pu: last_max_branch_violation_pu,
            max_hvdc_violation_pu: last_max_hvdc_violation_pu,
            n_preseed_cuts,
            n_preseed_pairs_binding: None,
        },
    ))
}

fn attach_security_metadata(
    mut dispatch: RawDispatchSolution,
    metadata: SecurityDispatchMetadata,
) -> RawDispatchSolution {
    dispatch.diagnostics.security = Some(metadata);
    dispatch
}

/// Solve a DC time-coupled dispatch with the full linearized N-1
/// contingency set built into the SCUC up front.
pub(crate) fn solve_explicit_security_dispatch(
    network: &Network,
    options: &SecurityDispatchSpec,
) -> Result<RawDispatchSolution, ScedError> {
    // Measure everything in this function outside the inner
    // solve_scuc_with_problem_spec call so the "security overhead"
    // has a named line in DispatchPhaseTimings.security_setup_secs.
    let fn_start = std::time::Instant::now();
    let base_spec = DispatchProblemSpec::from_request(&options.input, &options.commitment);
    let n_periods = options.input.n_periods;
    let min_rate = options.input.min_rate_a;

    let hourly_networks: Vec<Network> = (0..n_periods)
        .map(|hour| super::snapshot::network_at_hour_with_spec(network, &base_spec, hour))
        .collect();
    let hourly_contexts: Vec<HourlySecurityContext> = hourly_networks
        .iter()
        .map(|hourly_network| build_hourly_security_context(hourly_network, options, min_rate))
        .collect::<Result<_, _>>()?;

    // Option C: build compact `ContingencyCut` entries directly (one
    // LP row per (contingency × monitored × period)), bypassing the
    // ~500-byte-per-entry `surge_network::network::Flowgate` route and
    // the matching per-flowgate `resolved_flowgates` allocations. The
    // cuts carry exactly the five numbers the LP row emitter needs
    // (`scuc::problem::build_problem`) to materialize the constraint,
    // plus the `case_index` + `period` bookkeeping used by
    // `ExplicitContingencyObjectivePlan` for case-level penalty
    // aggregation and by `extract.rs` for per-case reporting.
    let lodf_cutoff = DEFAULT_CONTINGENCY_CUT_LODF_CUTOFF_PU;
    let mut explicit_cases: Vec<ExplicitContingencyCase> = Vec::new();
    let mut contingency_cuts: Vec<ContingencyCut> = Vec::new();
    let mut hvdc_band_coefs: Vec<(u32, f64)> = Vec::new();
    let mut n_cuts_dropped_by_lodf_cutoff: usize = 0;
    for (period, (hourly_network, context)) in hourly_networks
        .iter()
        .zip(hourly_contexts.iter())
        .enumerate()
    {
        let mut branch_case_indices: Vec<usize> =
            context.branch_contingencies.keys().copied().collect();
        branch_case_indices.sort_unstable();
        for contingency_branch_idx in branch_case_indices {
            let case_index = explicit_cases.len();
            explicit_cases.push(ExplicitContingencyCase {
                period,
                element: ExplicitContingencyElement::Branch(contingency_branch_idx),
            });
            let Some(contingency) = context.branch_contingencies.get(&contingency_branch_idx)
            else {
                continue;
            };
            for &monitored_idx in &context.monitored {
                if monitored_idx == contingency.branch_idx {
                    continue;
                }
                let Some(ptdf_l) = context.ptdf.row(monitored_idx) else {
                    continue;
                };
                let lodf_lk =
                    (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
                if !lodf_lk.is_finite() {
                    continue;
                }
                // LODF magnitude filter — see
                // `DEFAULT_CONTINGENCY_CUT_LODF_CUTOFF_PU` for
                // rationale. Cuts below the threshold are numerically
                // trivial; dropping them here keeps the SCUC LP
                // tractable on full N-1.
                if lodf_lk.abs() < lodf_cutoff {
                    n_cuts_dropped_by_lodf_cutoff += 1;
                    continue;
                }
                let monitored_branch = &hourly_network.branches[monitored_idx];
                // Contingency flowgates use the emergency rating
                // (`rating_c_mva`) so the LODF cut limit matches the
                // post-contingency thermal envelope.
                let limit_mw = monitored_branch.rating_for(BranchRatingCondition::Emergency);
                contingency_cuts.push(ContingencyCut {
                    period: period as u32,
                    case_index: case_index as u32,
                    monitored_branch_idx: monitored_idx as u32,
                    contingency_kind: ContingencyCutKind::Branch,
                    contingency_idx: contingency.branch_idx as u32,
                    coefficient: lodf_lk,
                    limit_mw,
                    hvdc_band_range: (0, 0),
                });
            }
        }

        for contingency in &context.hvdc_contingencies {
            let case_index = explicit_cases.len();
            explicit_cases.push(ExplicitContingencyCase {
                period,
                element: ExplicitContingencyElement::Hvdc(contingency.hvdc_idx),
            });
            let Some(hvdc) = options.input.hvdc_links.get(contingency.hvdc_idx) else {
                continue;
            };
            for &monitored_idx in &context.monitored {
                let Some(ptdf_l) = context.ptdf.row(monitored_idx) else {
                    continue;
                };
                let monitored_branch = &hourly_network.branches[monitored_idx];
                let limit_mw = monitored_branch.rating_for(BranchRatingCondition::Emergency);
                if hvdc.is_banded() {
                    let band_coefficients: Vec<(u32, f64)> = contingency
                        .band_loss_b
                        .iter()
                        .enumerate()
                        .map(|(band_idx, &loss_b)| {
                            (
                                band_idx as u32,
                                ptdf_l[contingency.from_idx]
                                    - (1.0 - loss_b) * ptdf_l[contingency.to_idx],
                            )
                        })
                        .collect();
                    // HVDC-banded: drop when every band coefficient
                    // is below the LODF-equivalent cutoff (same
                    // magnitude interpretation as the branch LODF —
                    // both are DC sensitivities of post-contingency
                    // flow on the monitored branch).
                    if band_coefficients
                        .iter()
                        .all(|(_, coeff)| !coeff.is_finite() || coeff.abs() < lodf_cutoff)
                    {
                        n_cuts_dropped_by_lodf_cutoff += 1;
                        continue;
                    }
                    let band_start = hvdc_band_coefs.len() as u32;
                    hvdc_band_coefs.extend(band_coefficients);
                    let band_end = hvdc_band_coefs.len() as u32;
                    contingency_cuts.push(ContingencyCut {
                        period: period as u32,
                        case_index: case_index as u32,
                        monitored_branch_idx: monitored_idx as u32,
                        contingency_kind: ContingencyCutKind::HvdcBanded,
                        contingency_idx: contingency.hvdc_idx as u32,
                        coefficient: 0.0,
                        limit_mw,
                        hvdc_band_range: (band_start, band_end),
                    });
                } else {
                    let coefficient = ptdf_l[contingency.from_idx]
                        - (1.0 - hvdc.loss_b_frac) * ptdf_l[contingency.to_idx];
                    if !coefficient.is_finite() || coefficient.abs() < lodf_cutoff {
                        n_cuts_dropped_by_lodf_cutoff += 1;
                        continue;
                    }
                    contingency_cuts.push(ContingencyCut {
                        period: period as u32,
                        case_index: case_index as u32,
                        monitored_branch_idx: monitored_idx as u32,
                        contingency_kind: ContingencyCutKind::HvdcLegacy,
                        contingency_idx: contingency.hvdc_idx as u32,
                        coefficient,
                        limit_mw,
                        hvdc_band_range: (0, 0),
                    });
                }
            }
        }
    }

    // Option C: we keep `explicit_network` as a plain clone of the
    // source network (no security Flowgate structs pushed into
    // `flowgates`). The contingency cuts ride the LP through
    // `scuc_spec.contingency_cuts`, and the SCUC problem builder
    // emits one LP row per cut directly. This replaces the
    // ~500-byte-per-cut Flowgate route + matching `resolved_flowgates`
    // allocations (~5 GB on 617-bus D1 explicit N-1) with ~40 bytes
    // per cut.
    let explicit_network = network.clone();
    let n_cuts = contingency_cuts.len();
    info!(
        n_contingency_cuts = n_cuts,
        n_cases = explicit_cases.len(),
        n_cuts_dropped_by_lodf_cutoff,
        lodf_cutoff_pu = lodf_cutoff,
        "Security SCUC: solving with compact contingency cuts"
    );

    // `enforce_flowgates` is *not* flipped on this path — the cut
    // rows emit their own LP rows + slacks independent of the
    // `fg_rows` plumbing, so the base-case flowgate flag stays at
    // the caller's setting.
    let empty_flowgate_mappings: &[ExplicitContingencyFlowgate] = &[];
    let mut scuc_spec =
        base_spec.with_explicit_contingencies(&explicit_cases, empty_flowgate_mappings);
    scuc_spec.contingency_cuts = &contingency_cuts;
    scuc_spec.contingency_cut_hvdc_band_coefs = &hvdc_band_coefs;
    let mut connectivity_cuts: Vec<super::connectivity::IndexedConnectivityCut> = Vec::new();
    let mut result: Option<RawDispatchSolution> = None;
    if scuc_spec.allow_branch_switching {
        for refit_round in 0..MAX_CONNECTIVITY_CUT_ROUNDS {
            let spec_with_cuts = scuc_spec.with_connectivity_cuts(&connectivity_cuts);
            let round_sol = solve_scuc_with_problem_spec(&explicit_network, spec_with_cuts)?;
            let new_cuts_added = add_connectivity_cuts_from_solution(
                &round_sol,
                &hourly_networks,
                &hourly_contexts,
                &mut connectivity_cuts,
                "explicit",
                0,
                refit_round,
            );
            if !new_cuts_added {
                result = Some(round_sol);
                break;
            }
            if refit_round + 1 == MAX_CONNECTIVITY_CUT_ROUNDS {
                warn!(
                    cuts = connectivity_cuts.len(),
                    "Security SCUC explicit connectivity cut loop hit \
                         {MAX_CONNECTIVITY_CUT_ROUNDS}-round cap; accepting the \
                         last solve even though one or more base or contingency \
                         switching graphs remain disconnected."
                );
                result = Some(round_sol);
            }
        }
    }
    // Stop the security-setup clock before handing off to SCUC so the
    // inner solve's wall isn't double-counted here. attach_security_metadata
    // + destructor tail below restart a second accumulator.
    let security_setup_pre = fn_start.elapsed().as_secs_f64();

    let result = match result {
        Some(solution) => solution,
        None => {
            // SW0 (no connectivity-cut loop) and the final solve on
            // SW1 don't need hourly_networks after this point — move
            // them into solve_scuc so build_model_plan can reuse them
            // instead of rebuilding an identical Vec<Network>.
            // hourly_contexts is kept (still borrowed by the caller)
            // but we don't use it below anyway.
            //
            // Move `explicit_network` by value: its `flowgates` array
            // holds up to ~10M security cuts on large N-1 scenarios
            // (617-bus D1 = 8.6M). The old borrowed entry cloned the
            // whole network (flowgates included) inside SCUC. Passing
            // by value saves a peak-RSS copy in that range.
            solve_scuc_with_owned_network(explicit_network, scuc_spec, Some(hourly_networks))?
        }
    };

    let t_tail = std::time::Instant::now();
    let mut result = attach_security_metadata(
        result,
        SecurityDispatchMetadata {
            iterations: 0,
            n_cuts: n_cuts + connectivity_cuts.len(),
            converged: true,
            last_branch_violations: 0,
            last_hvdc_violations: 0,
            max_branch_violation_pu: None,
            max_hvdc_violation_pu: None,
            n_preseed_cuts: 0,
            n_preseed_pairs_binding: None,
        },
    );
    // `security_setup_tail` covers attach_security_metadata itself.
    // Destructors of hourly_contexts + explicit_cases +
    // explicit_case_flowgates fire at function-return epilogue and are
    // NOT in this accumulator — they show up as the residual
    // `solve_scuc_external_secs − (solve_scuc_self_total_secs +
    // security_setup_secs)` in the caller's view. `explicit_flowgates`
    // was moved into `explicit_network.flowgates` above, and
    // `explicit_network` + `hourly_networks` are now moved into
    // `solve_scuc_with_owned_network`, so their destructor walls land
    // inside `scuc_local_drops_secs` on the SCUC timing block instead
    // of leaking into the residual.
    let security_setup_tail = t_tail.elapsed().as_secs_f64();
    if let Some(pt) = result.diagnostics.phase_timings.as_mut() {
        pt.security_setup_secs = security_setup_pre + security_setup_tail;
    }
    Ok(result)
}

/// Uniform bus participation factors (`α_i = 1/|I|`) for
/// post-contingency slack distribution.
fn uniform_participation_weights(network: &Network) -> Vec<(usize, f64)> {
    let n = network.buses.len();
    if n == 0 {
        return Vec::new();
    }
    let w = 1.0 / n as f64;
    (0..n).map(|i| (i, w)).collect()
}

fn build_hourly_security_context(
    hourly_network: &Network,
    options: &SecurityDispatchSpec,
    min_rate: f64,
) -> Result<HourlySecurityContext, ScedError> {
    let bus_map = hourly_network.bus_index_map();
    let monitored: Vec<usize> = hourly_network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| br.in_service && br.rating_a_mva > min_rate && br.x.abs() > 1e-20)
        .map(|(idx, _)| idx)
        .collect();

    let mut connectivity_contingency_branches: Vec<usize> =
        if options.contingency_branches.is_empty() {
            hourly_network
                .branches
                .iter()
                .enumerate()
                .filter_map(|(idx, branch)| branch.in_service.then_some(idx))
                .collect()
        } else {
            options
                .contingency_branches
                .iter()
                .copied()
                .filter(|&idx| {
                    idx < hourly_network.branches.len()
                        && hourly_network
                            .branches
                            .get(idx)
                            .is_some_and(|branch| branch.in_service)
                })
                .collect()
        };
    connectivity_contingency_branches.sort_unstable();
    connectivity_contingency_branches.dedup();

    let contingency_candidates: Vec<usize> = if options.contingency_branches.is_empty() {
        monitored.clone()
    } else {
        connectivity_contingency_branches.clone()
    };

    let mut ptdf_branch_set: HashSet<usize> = monitored.iter().copied().collect();
    ptdf_branch_set.extend(contingency_candidates.iter().copied().filter(|&idx| {
        hourly_network
            .branches
            .get(idx)
            .is_some_and(|branch| branch.in_service && branch.x.abs() > 1e-20)
    }));
    let ptdf_branches: Vec<usize> = ptdf_branch_set.into_iter().collect();
    let ptdf = if ptdf_branches.is_empty() {
        surge_dc::PtdfRows::default()
    } else {
        let mut ptdf_branches = ptdf_branches;
        ptdf_branches.sort_unstable();
        // Uniform participation factors `α_i = 1/|I|` for
        // post-contingency slack distribution.
        let uniform_weights = uniform_participation_weights(hourly_network);
        let sensitivity_options =
            surge_dc::DcSensitivityOptions::with_slack_weights(&uniform_weights);
        let ptdf_request =
            surge_dc::PtdfRequest::for_branches(&ptdf_branches).with_options(sensitivity_options);
        surge_dc::compute_ptdf(hourly_network, &ptdf_request)
            .map_err(|e| ScedError::SolverError(e.to_string()))?
    };

    let branch_contingencies: HashMap<usize, HourlyBranchContingency> = contingency_candidates
        .iter()
        .filter_map(|&branch_idx| {
            let branch = hourly_network.branches.get(branch_idx)?;
            if !branch.in_service || branch.x.abs() < 1e-20 {
                return None;
            }
            let from_idx = *bus_map.get(&branch.from_bus)?;
            let to_idx = *bus_map.get(&branch.to_bus)?;
            let ptdf_k = ptdf.row(branch_idx)?;
            let denom = 1.0 - (ptdf_k[from_idx] - ptdf_k[to_idx]);
            if denom.abs() < 1e-10 {
                return None;
            }
            Some((
                branch_idx,
                HourlyBranchContingency {
                    branch_idx,
                    from_idx,
                    to_idx,
                    denom,
                },
            ))
        })
        .collect();

    let hvdc_contingencies: Vec<HourlyHvdcContingency> = options
        .hvdc_contingency_indices
        .iter()
        .filter_map(|&hvdc_idx| {
            let hvdc = options.input.hvdc_links.get(hvdc_idx)?;
            let from_idx = *bus_map.get(&hvdc.from_bus)?;
            let to_idx = *bus_map.get(&hvdc.to_bus)?;
            let band_loss_b = if hvdc.is_banded() {
                hvdc.bands.iter().map(|band| band.loss_b_frac).collect()
            } else {
                Vec::new()
            };
            Some(HourlyHvdcContingency {
                hvdc_idx,
                from_idx,
                to_idx,
                band_loss_b,
            })
        })
        .collect();

    Ok(HourlySecurityContext {
        monitored,
        ptdf,
        branch_contingencies,
        connectivity_contingency_branches,
        hvdc_contingencies,
    })
}

fn screen_branch_violations(
    period: usize,
    angles: &[f64],
    hourly_network: &Network,
    context: &HourlySecurityContext,
    base: f64,
    tolerance_pu: f64,
    constrained_pairs: &HashSet<(usize, usize, usize)>,
) -> Vec<BranchSecurityViolation> {
    let bus_map = hourly_network.bus_index_map();
    let mut violations = Vec::new();

    for contingency in context.branch_contingencies.values() {
        let k = contingency.branch_idx;
        let branch_k = &hourly_network.branches[k];
        let flow_k = branch_k.b_dc()
            * (angles[contingency.from_idx]
                - angles[contingency.to_idx]
                - branch_k.phase_shift_rad);

        for &l in &context.monitored {
            if l == k || constrained_pairs.contains(&(period, k, l)) {
                continue;
            }
            let Some(ptdf_l) = context.ptdf.row(l) else {
                continue;
            };
            let lodf_lk =
                (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
            if !lodf_lk.is_finite() {
                continue;
            }

            let branch_l = &hourly_network.branches[l];
            let Some(&from_l) = bus_map.get(&branch_l.from_bus) else {
                continue;
            };
            let Some(&to_l) = bus_map.get(&branch_l.to_bus) else {
                continue;
            };
            let flow_l =
                branch_l.b_dc() * (angles[from_l] - angles[to_l] - branch_l.phase_shift_rad);
            let post_flow = flow_l + lodf_lk * flow_k;
            // Contingency-state thermal limits may exceed base-case
            // (`s^max,ctg ≥ s^max`). Use `Emergency`, whose fallback
            // chain is `rating_c → rating_b → rating_a`, so datasets
            // that only populate RATE_A still work.
            let limit_pu = branch_l.rating_for(BranchRatingCondition::Emergency) / base;
            let excess = post_flow.abs() - limit_pu;
            if excess > tolerance_pu {
                violations.push(BranchSecurityViolation {
                    period,
                    contingency_branch_idx: k,
                    monitored_branch_idx: l,
                    severity_pu: excess,
                });
            }
        }
    }

    violations
}

fn screen_hvdc_violations(
    period: usize,
    angles: &[f64],
    dispatch: &crate::solution::RawDispatchPeriodResult,
    hourly_network: &Network,
    context: &HourlySecurityContext,
    hvdc_links: &[crate::hvdc::HvdcDispatchLink],
    base: f64,
    tolerance_pu: f64,
    constrained_pairs: &HashSet<(usize, usize, usize)>,
) -> Vec<HvdcSecurityCut> {
    let bus_map = hourly_network.bus_index_map();
    let mut violations = Vec::new();

    for contingency in &context.hvdc_contingencies {
        let k = contingency.hvdc_idx;
        let Some(hvdc) = hvdc_links.get(k) else {
            continue;
        };
        let total_dispatch_pu = dispatch.hvdc_dispatch_mw.get(k).copied().unwrap_or(0.0) / base;
        let band_dispatch_pu: Vec<f64> = dispatch
            .hvdc_band_dispatch_mw
            .get(k)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mw| mw / base)
            .collect();

        if hvdc.is_banded() {
            if band_dispatch_pu.len() != hvdc.bands.len()
                || band_dispatch_pu.iter().all(|mw| mw.abs() < 1e-10)
            {
                continue;
            }
        } else if total_dispatch_pu.abs() < 1e-10 {
            continue;
        }

        for &l in &context.monitored {
            if constrained_pairs.contains(&(period, k, l)) {
                continue;
            }
            let Some(ptdf_l) = context.ptdf.row(l) else {
                continue;
            };
            let branch_l = &hourly_network.branches[l];
            let Some(&from_l) = bus_map.get(&branch_l.from_bus) else {
                continue;
            };
            let Some(&to_l) = bus_map.get(&branch_l.to_bus) else {
                continue;
            };
            let flow_l =
                branch_l.b_dc() * (angles[from_l] - angles[to_l] - branch_l.phase_shift_rad);

            let (coefficient, band_coefficients, post_flow) = if hvdc.is_banded() {
                let band_coefficients: Vec<(usize, f64)> = contingency
                    .band_loss_b
                    .iter()
                    .enumerate()
                    .map(|(band_idx, &loss_b)| {
                        (
                            band_idx,
                            ptdf_l[contingency.from_idx]
                                - (1.0 - loss_b) * ptdf_l[contingency.to_idx],
                        )
                    })
                    .collect();
                if band_coefficients
                    .iter()
                    .all(|(_, coeff)| !coeff.is_finite() || coeff.abs() < 1e-12)
                {
                    continue;
                }
                let impact = band_coefficients
                    .iter()
                    .zip(band_dispatch_pu.iter())
                    .map(|((_, coeff), dispatch_pu)| coeff * dispatch_pu)
                    .sum::<f64>();
                (None, band_coefficients, flow_l - impact)
            } else {
                let coefficient = ptdf_l[contingency.from_idx]
                    - (1.0 - hvdc.loss_b_frac) * ptdf_l[contingency.to_idx];
                if !coefficient.is_finite() || coefficient.abs() < 1e-12 {
                    continue;
                }
                (
                    Some(coefficient),
                    Vec::new(),
                    flow_l - coefficient * total_dispatch_pu,
                )
            };

            // Eq (271) contingency rating: HVDC cuts use the same
            // post-contingency thermal limit policy as branch cuts.
            let limit_mva = branch_l.rating_for(BranchRatingCondition::Emergency);
            let excess = post_flow.abs() - limit_mva / base;
            if excess > tolerance_pu {
                violations.push(HvdcSecurityCut {
                    period,
                    hvdc_link_idx: k,
                    monitored_branch_idx: l,
                    coefficient,
                    band_coefficients,
                    f_max_mw: limit_mva,
                    excess_pu: excess,
                });
            }
        }
    }

    violations
}

/// Rank (contingency, monitored) pairs per period and emit the top-N as
/// security flowgates before iter 0 of the iterative SCUC. Registers the
/// dedup key in `constrained_pairs` so the post-solve screener does not
/// re-discover the same pair.
///
/// Ranking uses
///     severity = |LODF_{l,k}| * rating_c(k) / rating_c(l)
/// — a dimensionless, dispatch-free proxy for "if contingency k trips at
/// emergency capacity, what fraction of monitored-branch l's limit could
/// get shifted onto it". All inputs are already cached in the
/// [`HourlySecurityContext`] (PTDF, LODF denominator), so this is cheap
/// compared with one extra SCUC re-solve.
///
/// HVDC contingencies are not pre-seeded in this first pass; HVDC
/// flowgate row counts are typically far smaller than AC branch
/// counts, and the iterative screener picks them up quickly. Extend
/// here if that assumption changes.
fn preseed_branch_flowgates(
    options: &SecurityDispatchSpec,
    hourly_networks: &[Network],
    hourly_contexts: &[HourlySecurityContext],
    n_periods: usize,
    constrained_pairs: &mut HashSet<(usize, usize, usize)>,
) -> Vec<surge_network::network::Flowgate> {
    let per_period = options.preseed_count_per_period;
    if per_period == 0 || matches!(options.preseed_method, SecurityPreseedMethod::None) {
        return Vec::new();
    }

    let mut out: Vec<surge_network::network::Flowgate> = Vec::new();

    for (period, (hourly_network, context)) in hourly_networks
        .iter()
        .zip(hourly_contexts.iter())
        .enumerate()
    {
        let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
        for contingency in context.branch_contingencies.values() {
            let k = contingency.branch_idx;
            let branch_k = &hourly_network.branches[k];
            let rating_k_mw = branch_k.rating_for(BranchRatingCondition::Emergency);
            if rating_k_mw <= 0.0 || rating_k_mw.is_nan() {
                continue;
            }
            for &l in &context.monitored {
                if l == k {
                    continue;
                }
                let Some(ptdf_l) = context.ptdf.row(l) else {
                    continue;
                };
                let lodf_lk =
                    (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
                if !lodf_lk.is_finite() {
                    continue;
                }
                let branch_l = &hourly_network.branches[l];
                let rating_l_mw = branch_l.rating_for(BranchRatingCondition::Emergency);
                if rating_l_mw <= 0.0 || rating_l_mw.is_nan() {
                    continue;
                }
                let severity = lodf_lk.abs() * rating_k_mw / rating_l_mw;
                if !severity.is_finite() || severity <= 0.0 {
                    continue;
                }
                candidates.push((severity, k, l));
            }
        }
        candidates
            .sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut taken = 0usize;
        for (_severity, k, l) in candidates {
            if taken >= per_period {
                break;
            }
            let key = (period, k, l);
            if !constrained_pairs.insert(key) {
                continue;
            }
            let violation = BranchSecurityViolation {
                period,
                contingency_branch_idx: k,
                monitored_branch_idx: l,
                severity_pu: 0.0,
            };
            out.push(build_branch_security_flowgate(
                &violation,
                hourly_network,
                context,
                n_periods,
            ));
            taken += 1;
        }
    }

    out
}

fn build_branch_security_flowgate(
    violation: &BranchSecurityViolation,
    hourly_network: &Network,
    context: &HourlySecurityContext,
    n_periods: usize,
) -> surge_network::network::Flowgate {
    use surge_network::network::{BranchRef, Flowgate, WeightedBranchRef};

    let monitored_branch = &hourly_network.branches[violation.monitored_branch_idx];
    let contingency_branch = &hourly_network.branches[violation.contingency_branch_idx];
    let contingency = context
        .branch_contingencies
        .get(&violation.contingency_branch_idx)
        .expect("branch contingency metadata should exist");
    let ptdf_l = context
        .ptdf
        .row(violation.monitored_branch_idx)
        .expect("monitored branch PTDF row should exist");
    let lodf_lk = (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
    let limit_mw = monitored_branch.rating_for(BranchRatingCondition::Emergency);
    // Compact single-period encoding: see Flowgate::effective_limit_mw.
    // For explicit N-1 SCUC this replaces a dense `Vec<f64>` schedule of
    // length n_periods with 17/18 sentinel entries.
    let active_period =
        (n_periods > 1 && violation.period < n_periods).then_some(violation.period as u32);

    Flowgate {
        name: format!(
            "N1_t{}_{}_{}_{}_{}",
            violation.period,
            contingency_branch.from_bus,
            contingency_branch.to_bus,
            monitored_branch.from_bus,
            monitored_branch.to_bus
        ),
        monitored: vec![
            WeightedBranchRef {
                branch: BranchRef::new(
                    monitored_branch.from_bus,
                    monitored_branch.to_bus,
                    monitored_branch.circuit.clone(),
                ),
                coefficient: 1.0,
            },
            WeightedBranchRef {
                branch: BranchRef::new(
                    contingency_branch.from_bus,
                    contingency_branch.to_bus,
                    contingency_branch.circuit.clone(),
                ),
                coefficient: lodf_lk,
            },
        ],
        contingency_branch: Some(BranchRef::new(
            contingency_branch.from_bus,
            contingency_branch.to_bus,
            contingency_branch.circuit.clone(),
        )),
        // Contingency flowgates use the emergency rating so the LODF
        // cut limit matches the post-contingency thermal envelope.
        limit_mw,
        limit_reverse_mw: 0.0,
        in_service: true,
        limit_mw_schedule: Vec::new(),
        limit_reverse_mw_schedule: Vec::new(),
        hvdc_coefficients: Vec::new(),
        hvdc_band_coefficients: Vec::new(),
        limit_mw_active_period: active_period,
    }
}

fn build_hvdc_security_flowgate(
    cut: &HvdcSecurityCut,
    hourly_network: &Network,
    n_periods: usize,
) -> surge_network::network::Flowgate {
    use surge_network::network::{BranchRef, Flowgate, WeightedBranchRef};

    let monitored_branch = &hourly_network.branches[cut.monitored_branch_idx];
    let active_period = (n_periods > 1 && cut.period < n_periods).then_some(cut.period as u32);

    Flowgate {
        name: format!(
            "HVDC_N1_t{}_{}_{}_{}",
            cut.period, cut.hvdc_link_idx, monitored_branch.from_bus, monitored_branch.to_bus
        ),
        monitored: vec![WeightedBranchRef {
            branch: BranchRef::new(
                monitored_branch.from_bus,
                monitored_branch.to_bus,
                monitored_branch.circuit.clone(),
            ),
            coefficient: 1.0,
        }],
        contingency_branch: None,
        limit_mw: cut.f_max_mw,
        limit_reverse_mw: 0.0,
        in_service: true,
        limit_mw_schedule: Vec::new(),
        limit_reverse_mw_schedule: Vec::new(),
        hvdc_coefficients: cut
            .coefficient
            .map(|coefficient| vec![(cut.hvdc_link_idx, -coefficient)])
            .unwrap_or_default(),
        hvdc_band_coefficients: cut
            .band_coefficients
            .iter()
            .map(|&(band_idx, coefficient)| (cut.hvdc_link_idx, band_idx, -coefficient))
            .collect(),
        limit_mw_active_period: active_period,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::dispatch::CommitmentMode;
    use crate::request::DispatchInput;
    use crate::sced::{HvdcBand, HvdcDispatchLink};
    use crate::solution::RawDispatchPeriodResult;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    fn triangle_security_network(monitored_limit_mw: f64) -> Network {
        let mut net = Network::new("security_triangle");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 138.0));

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 100.0;
        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 100.0;
        let mut br13 = Branch::new_line(1, 3, 0.0, 0.1, 0.0);
        br13.rating_a_mva = monitored_limit_mw;
        net.branches = vec![br12, br23, br13];
        net.generators.push(Generator::new(1, 50.0, 1.0));
        net
    }

    fn security_spec(
        input: DispatchInput,
        contingency_branches: Vec<usize>,
        hvdc_contingency_indices: Vec<usize>,
    ) -> SecurityDispatchSpec {
        SecurityDispatchSpec {
            input,
            commitment: CommitmentMode::AllCommitted,
            max_iterations: 1,
            violation_tolerance_pu: 0.0,
            max_cuts_per_iteration: 10,
            contingency_branches,
            hvdc_contingency_indices,
            preseed_count_per_period: 0,
            preseed_method: SecurityPreseedMethod::None,
        }
    }

    #[test]
    fn branch_screening_uses_hourly_derated_limits() {
        let hour0 = triangle_security_network(80.0);
        let hour1 = triangle_security_network(60.0);
        let options = security_spec(
            DispatchInput {
                n_periods: 2,
                min_rate_a: 1.0,
                ..DispatchInput::default()
            },
            vec![0],
            vec![],
        );

        let context0 = build_hourly_security_context(&hour0, &options, 1.0).unwrap();
        let context1 = build_hourly_security_context(&hour1, &options, 1.0).unwrap();
        let angles = [0.0, -0.05, -0.025];

        let hour0_violations = screen_branch_violations(
            0,
            &angles,
            &hour0,
            &context0,
            hour0.base_mva,
            1e-6,
            &HashSet::new(),
        );
        let hour1_violations = screen_branch_violations(
            1,
            &angles,
            &hour1,
            &context1,
            hour1.base_mva,
            1e-6,
            &HashSet::new(),
        );

        assert!(
            hour0_violations.is_empty(),
            "80 MW monitored limit should clear the contingency, got {hour0_violations:?}"
        );
        assert!(
            hour1_violations
                .iter()
                .any(|violation| violation.contingency_branch_idx == 0
                    && violation.monitored_branch_idx == 2),
            "60 MW monitored limit should trigger the 1-2 outage / 1-3 monitored violation, got {hour1_violations:?}"
        );
    }

    #[test]
    fn connectivity_contingencies_include_in_service_branches_skipped_by_ptdf_screening() {
        let mut hourly_network = triangle_security_network(100.0);
        hourly_network.branches[1].x = 0.0;
        let options = security_spec(
            DispatchInput {
                n_periods: 1,
                min_rate_a: 1.0,
                ..DispatchInput::default()
            },
            vec![],
            vec![],
        );

        let context = build_hourly_security_context(&hourly_network, &options, 1.0).unwrap();

        assert!(
            !context.branch_contingencies.contains_key(&1),
            "zero-reactance branch should be skipped by PTDF-based branch screening"
        );
        assert!(
            context.connectivity_contingency_branches.contains(&1),
            "connectivity screening should still consider the in-service branch outage"
        );
    }

    #[test]
    fn hvdc_screening_uses_per_band_loss_coefficients() {
        let hourly_network = triangle_security_network(1.0);
        let options = security_spec(
            DispatchInput {
                n_periods: 1,
                min_rate_a: 0.5,
                hvdc_links: vec![HvdcDispatchLink {
                    id: String::new(),
                    name: "BandHVDC".into(),
                    from_bus: 1,
                    to_bus: 2,
                    p_dc_min_mw: 0.0,
                    p_dc_max_mw: 100.0,
                    loss_a_mw: 0.0,
                    loss_b_frac: 0.0,
                    ramp_mw_per_min: 0.0,
                    cost_per_mwh: 0.0,
                    bands: vec![
                        HvdcBand {
                            id: "firm".into(),
                            p_min_mw: 0.0,
                            p_max_mw: 50.0,
                            cost_per_mwh: 0.0,
                            loss_b_frac: 0.0,
                            ramp_mw_per_min: 0.0,
                            reserve_eligible_up: false,
                            reserve_eligible_down: false,
                            max_duration_hours: 0.0,
                        },
                        HvdcBand {
                            id: "econ".into(),
                            p_min_mw: 0.0,
                            p_max_mw: 50.0,
                            cost_per_mwh: 0.0,
                            loss_b_frac: 0.5,
                            ramp_mw_per_min: 0.0,
                            reserve_eligible_up: false,
                            reserve_eligible_down: false,
                            max_duration_hours: 0.0,
                        },
                    ],
                }],
                ..DispatchInput::default()
            },
            vec![],
            vec![0],
        );

        let context = build_hourly_security_context(&hourly_network, &options, 0.5).unwrap();
        let dispatch = RawDispatchPeriodResult {
            hvdc_dispatch_mw: vec![75.0],
            hvdc_band_dispatch_mw: vec![vec![50.0, 25.0]],
            ..RawDispatchPeriodResult::default()
        };
        let violations = screen_hvdc_violations(
            0,
            &[0.0, 0.0, 0.0],
            &dispatch,
            &hourly_network,
            &context,
            &options.input.hvdc_links,
            hourly_network.base_mva,
            1e-6,
            &HashSet::new(),
        );

        let cut = violations
            .iter()
            .find(|cut| cut.hvdc_link_idx == 0 && cut.monitored_branch_idx == 2)
            .expect("expected monitored branch 1-3 HVDC cut");
        let hvdc_ctg = context
            .hvdc_contingencies
            .iter()
            .find(|ctg| ctg.hvdc_idx == 0)
            .expect("HVDC contingency metadata");
        let ptdf_l = context.ptdf.row(2).expect("PTDF row for monitored branch");
        let expected_coefficients = [
            ptdf_l[hvdc_ctg.from_idx] - (1.0 - 0.0) * ptdf_l[hvdc_ctg.to_idx],
            ptdf_l[hvdc_ctg.from_idx] - (1.0 - 0.5) * ptdf_l[hvdc_ctg.to_idx],
        ];

        assert_eq!(
            cut.coefficient, None,
            "banded link should not use legacy link coefficient"
        );
        assert_eq!(cut.band_coefficients.len(), 2);
        assert!(
            (cut.band_coefficients[0].1 - expected_coefficients[0]).abs() < 1e-9,
            "firm-band coefficient mismatch: expected {}, got {}",
            expected_coefficients[0],
            cut.band_coefficients[0].1
        );
        assert!(
            (cut.band_coefficients[1].1 - expected_coefficients[1]).abs() < 1e-9,
            "economic-band coefficient mismatch: expected {}, got {}",
            expected_coefficients[1],
            cut.band_coefficients[1].1
        );
        // With uniform participation factors (`α_i = 1/|I|`) on a
        // symmetric 3-bus triangle, PTDF values at the HVDC from/to
        // buses can coincide. Relax to an approximate inequality that
        // tolerates this while still checking the `loss_b` effect when
        // PTDF endpoints differ.
        let diff = (cut.band_coefficients[0].1 - cut.band_coefficients[1].1).abs();
        let scale = cut.band_coefficients[0]
            .1
            .abs()
            .max(cut.band_coefficients[1].1.abs())
            .max(1e-12);
        assert!(
            diff > 1e-12 || ptdf_l[hvdc_ctg.to_idx].abs() < 1e-12,
            "distinct band losses should produce distinct security coefficients \
             unless PTDF at HVDC to-bus is zero (diff={diff:.2e}, scale={scale:.2e})"
        );
    }
}
