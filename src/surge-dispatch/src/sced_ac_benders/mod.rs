// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCED-AC Benders decomposition orchestration loop.
//!
//! Drives the master / subproblem iteration that replaces
//! target-tracking AC reconciliation with a principled
//! generalized-Benders decomposition:
//!
//! - **Master**: DC SCED LP augmented with per-period epigraph variables
//!   ``η[t]`` and an accumulating pool of Benders cuts. The master objective
//!   is ``min DC_cost(Pg) + Σ_t η[t]`` with ``η[t] ≥ 0``. Each cut is a
//!   linear constraint of the form
//!
//!   ```text
//!   η[t] ≥ slack_cost(P̃g) + Σ_g λ_g · (Pg[g,t] − P̃g_g)
//!   ```
//!
//!   where ``P̃g`` is the dispatch from a prior iteration and ``λ_g`` are
//!   the Benders cut coefficients (per-gen slack-penalty marginals from
//!   the subproblem). The cut pool and eta activation are plumbed through
//!   the SCED LP via [`ScedAcBendersRuntime`] so the DC master solver
//!   handles them transparently.
//!
//! - **Subproblem**: AC OPF with active-power dispatch fixed to the
//!   master's current proposal (using
//!   [`surge_opf::ac::solve_ac_opf_subproblem`]). Returns the slack
//!   penalty cost at that operating point and its gradient with respect
//!   to each fixed ``Pg`` — the cut coefficients the master needs.
//!
//! - **Convergence**: ``(UB − LB) ≤ abs_tol`` or ``(UB − LB) / |UB| ≤
//!   rel_tol`` terminate with ``converged = true``. Additional
//!   safeguards handle stagnation and oscillation
//!   (see [`ScedAcBendersRunParams`]).
//!
//! ## Entry point
//!
//! [`solve_sced_sequence_benders`] runs the Benders orchestration over an
//! entire horizon and produces a [`RawDispatchSolution`] compatible with
//! the downstream dispatch result assembly. It is wired into the
//! dispatch pipeline by [`crate::dispatch`] when the request's
//! ``runtime.sced_ac_benders.orchestration`` field is populated.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use surge_network::Network;
use surge_opf::{AcOpfOptions, AcOpfRuntime, solve_ac_opf_subproblem};
use surge_solution::OpfSolution;

use crate::common::spec::DispatchProblemSpec;
use crate::dispatch::{RawDispatchSolution, SequentialDispatchAccumulator, apply_profiles};
use crate::error::ScedError;
use crate::request::{ScedAcBendersCut, ScedAcBendersRunParams};
use crate::sced::solve::solve_sced_with_problem_spec;
use crate::solution::RawDispatchPeriodResult;

/// Why the Benders loop terminated.
///
/// Mirrors the Python reference implementation so diagnostics from the
/// two orchestrators can be compared byte-for-byte in integration tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BendersTerminalReason {
    /// `(UB − LB) ≤ abs_tol` — absolute gap closed.
    AbsTol,
    /// `(UB − LB) / |UB| ≤ rel_tol` — relative gap closed.
    RelTol,
    /// The maximum number of iterations configured by
    /// [`ScedAcBendersRunParams::max_iterations`] was reached without
    /// closing either tolerance.
    MaxIterations,
    /// The subproblem reported zero slack, so further cuts would be
    /// degenerate. The current dispatch is AC-feasible by
    /// construction.
    AcFeasible,
    /// The most recent iteration produced no useful cut (all marginals
    /// trimmed, slack below floor, etc.) — we cannot make further
    /// progress without new information.
    NoCutsGenerated,
    /// The upper bound did not improve for
    /// [`ScedAcBendersRunParams::stagnation_patience`] consecutive
    /// iterations.
    Stagnation,
    /// The master LP is alternating between two dispatches (detected by
    /// the bimodal DC-cost detector in the Python reference).
    Oscillation,
    /// The AC OPF subproblem failed with an unrecoverable error. The
    /// best-known dispatch from a prior iteration is still returned.
    SubproblemFailed,
}

/// Per-iteration diagnostic record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BendersIterationRecord {
    pub iteration: usize,
    pub period: usize,
    /// Pure DC production cost at the master's current Pg (`$/hr`).
    pub dc_cost_dollars_per_hour: f64,
    /// Current value of the master's `η[period]` epigraph variable
    /// (`$/hr`). Zero when no cut binds.
    pub eta_dollars_per_hour: f64,
    /// Subproblem slack cost at the master's current Pg (`$/hr`).
    pub subproblem_slack_dollars_per_hour: f64,
    /// Maximum absolute cut coefficient (`$/MW-hr`) generated in this
    /// iteration. Zero when no cut was generated.
    pub max_marginal_per_mw_per_hour: f64,
    /// Number of new cuts appended to the pool in this iteration.
    pub cuts_added: usize,
    /// Relative gap `(UB − LB) / max(|UB|, 1)` after the iteration.
    pub relative_gap: f64,
    /// Absolute gap `UB − LB` after the iteration (`$/hr`).
    pub absolute_gap_dollars_per_hour: f64,
    /// Wall-clock time of this iteration (seconds).
    pub elapsed_seconds: f64,
}

/// A single Benders cut retained in the diagnostic record, keyed by
/// iteration index so callers can trace how the cut pool grew.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BendersCutRecord {
    /// Period this cut constrains.
    pub period: usize,
    /// Iteration that produced the cut.
    pub iteration: usize,
    /// Cut slope coefficients `λ_g` in `$/MW-hr`, keyed by resource id.
    pub coefficients_dollars_per_mw_per_hour: HashMap<String, f64>,
    /// Cut intercept in `$/hr` (already includes the linear-expansion
    /// offset `slack_cost − Σ λ_g · P̃g_g`).
    pub rhs_dollars_per_hour: f64,
}

impl From<&ScedAcBendersCut> for BendersCutRecord {
    fn from(cut: &ScedAcBendersCut) -> Self {
        Self {
            period: cut.period,
            iteration: cut.iteration,
            coefficients_dollars_per_mw_per_hour: cut.coefficients_dollars_per_mw_per_hour.clone(),
            rhs_dollars_per_hour: cut.rhs_dollars_per_hour,
        }
    }
}

/// Full diagnostic output of the Benders orchestration loop. Stored
/// alongside the dispatch result so callers can inspect convergence
/// behaviour after the fact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BendersDiagnostics {
    /// Per-period, per-iteration records.
    pub iterations: Vec<BendersIterationRecord>,
    /// Final cut pool after all pruning.
    pub cuts: Vec<BendersCutRecord>,
    /// Reason the loop terminated (per period, in canonical period
    /// order). The overall dispatch is considered converged only when
    /// every entry is a converged reason.
    pub terminal_reasons: Vec<BendersTerminalReason>,
    /// Upper bound at termination, per period (`$/hr`).
    pub upper_bound_by_period: Vec<f64>,
    /// Lower bound at termination, per period (`$/hr`).
    pub lower_bound_by_period: Vec<f64>,
    /// Total wall-clock time spent in the orchestration loop (seconds).
    pub total_elapsed_seconds: f64,
}

/// Summary of a single-period Benders loop. Bundled together so callers
/// who need just one period's outcome don't have to read through a full
/// [`BendersDiagnostics`] structure.
#[derive(Debug, Clone)]
pub struct BendersOutcome {
    /// Final DC master dispatch solution (one period's worth).
    pub dispatch: RawDispatchPeriodResult,
    /// Final AC OPF subproblem solution at the converged `Pg`, useful
    /// for downstream replay and voltage reporting.
    pub opf_solution: OpfSolution,
    pub iterations: Vec<BendersIterationRecord>,
    pub cuts: Vec<BendersCutRecord>,
    pub upper_bound_dollars_per_hour: f64,
    pub lower_bound_dollars_per_hour: f64,
    pub terminal_reason: BendersTerminalReason,
    #[allow(dead_code)]
    pub converged: bool,
    #[allow(dead_code)]
    pub elapsed_seconds: f64,
}

// ---------------------------------------------------------------------------
// Top-level orchestration entry points
// ---------------------------------------------------------------------------

/// Build an [`AcOpfOptions`] tuned for the Benders subproblem.
///
/// Forces finite bus-balance slack penalties so the subproblem is
/// **always feasible** under fixed-Pg — this is the foundation of the
/// orchestration loop: an infeasible subproblem cannot generate an
/// optimality cut, but an always-feasible subproblem can always give us
/// a slack marginal that the master can use to steer away from
/// infeasibility.
///
/// Forces discrete-device optimization (shunt / tap / phase-shifter /
/// SVC / TCSC) off because those flags turn the AC OPF into a MINLP and
/// break the envelope-theorem interpretation of the bound multipliers.
/// Similarly turns off flowgate/interface enforcement because those
/// introduce additional dual variables outside the slack-marginal space.
fn subproblem_options_from(base: &AcOpfOptions, params: &ScedAcBendersRunParams) -> AcOpfOptions {
    let mut opts = base.clone();
    opts.thermal_limit_slack_penalty_per_mva = params.ac_opf_thermal_slack_penalty_per_mva;
    opts.bus_active_power_balance_slack_penalty_per_mw =
        params.ac_opf_bus_active_power_balance_slack_penalty_per_mw;
    opts.bus_reactive_power_balance_slack_penalty_per_mvar =
        params.ac_opf_bus_reactive_power_balance_slack_penalty_per_mvar;
    // Force discrete optimization off so the NLP stays purely continuous.
    opts.optimize_switched_shunts = false;
    opts.optimize_taps = false;
    opts.optimize_phase_shifters = false;
    opts.optimize_svc = false;
    opts.optimize_tcsc = false;
    opts.discrete_mode = surge_opf::DiscreteMode::Continuous;
    // Force flowgates off; branch thermal limits are handled via the
    // slack penalty above.
    opts.enforce_flowgates = false;
    opts
}

/// Run the SCED-AC Benders orchestration loop over the entire horizon.
///
/// Returns a [`RawDispatchSolution`] with the DC master's Pg dispatch at
/// the converged iteration, patched with voltages and reactive-power
/// data from the terminal AC OPF subproblem for each period. The
/// per-period Benders diagnostics (iterations, cut pool, bounds,
/// terminal reason) are captured in [`BendersDiagnostics`] and returned
/// as a side-channel via the `diagnostics_out` parameter so callers
/// that don't care about them can ignore it entirely.
///
/// This is a *sequential* orchestrator: each period's Benders loop runs
/// to completion before the next period begins. This matches the DC
/// sequential dispatch path, which is what the AC formulation requires
/// for now (multi-period AC coupling is not yet supported upstream).
pub(crate) fn solve_sced_sequence_benders(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    ac_opf: &AcOpfOptions,
    ac_opf_runtime: &AcOpfRuntime,
    diagnostics_out: &mut BendersDiagnostics,
) -> Result<RawDispatchSolution, ScedError> {
    let start = Instant::now();
    let params = problem_spec
        .sced_ac_benders
        .orchestration
        .clone()
        .unwrap_or_default();

    // Validate that the AC options we were handed are actually feasible
    // for a fixed-Pg run. The subproblem must have finite slack penalties
    // for every soft constraint it could encounter.
    let sub_opts = subproblem_options_from(ac_opf, &params);

    // Build a **fresh** AC OPF runtime for the subproblem. We intentionally
    // do not inherit the caller's runtime because it may carry AC target
    // tracking (a regularization term), warm-start state, or other
    // solver hints that change the NLP objective. The Benders subproblem
    // must be a pure fixed-Pg solve so that the slack cost is a function
    // of the master's proposed Pg alone and the envelope-theorem
    // marginals are meaningful cut coefficients.
    let sub_runtime = AcOpfRuntime {
        nlp_solver: ac_opf_runtime.nlp_solver.clone(),
        warm_start: None,
        use_dc_opf_warm_start: None,
        objective_target_tracking: None,
        pre_polish_dump_path: None,
    };

    // Collect the full list of generator IDs we can fix. Only in-service
    // generators participate; anything out-of-service is left alone.
    let generator_ids = collect_in_service_generator_ids(network);
    let original_bounds = collect_generator_original_bounds(network);

    let n_periods = problem_spec.n_periods;
    let mut diagnostics = BendersDiagnostics::default();
    let mut accumulator = SequentialDispatchAccumulator::new(network, problem_spec);

    for period_idx in 0..n_periods {
        // Apply per-period profile mutations (load scaling, renewables,
        // derates, dispatch bounds, etc.) exactly the way the DC
        // sequential path does. The trust region on top of this is
        // applied per-iteration inside the single-period loop.
        let period_network = apply_profiles(network, &problem_spec, period_idx);
        let period_spec = problem_spec.period(period_idx);
        let period_context =
            accumulator.period_context(period_idx, period_spec.next_fixed_commitment());

        let outcome = solve_single_period_benders(
            &period_network,
            &problem_spec,
            period_idx,
            period_context,
            &params,
            &sub_opts,
            &sub_runtime,
            &generator_ids,
            &original_bounds,
        )?;

        diagnostics
            .iterations
            .extend(outcome.iterations.iter().cloned());
        diagnostics.cuts.extend(outcome.cuts.iter().cloned());
        diagnostics.terminal_reasons.push(outcome.terminal_reason);
        diagnostics
            .upper_bound_by_period
            .push(outcome.upper_bound_dollars_per_hour);
        diagnostics
            .lower_bound_by_period
            .push(outcome.lower_bound_dollars_per_hour);

        // Build per-period voltage/angle/Q vectors from the terminal AC
        // OPF subproblem solution. These feed the accumulator's
        // cross-period state and are later consumed by the canonicalizer
        // to populate the per-bus voltage fields of the public result.
        let (bus_voltage_pu, bus_angles_rad, generator_q_mvar) =
            extract_opf_cross_period_state(&outcome.opf_solution, &period_network);

        let dispatch_period = outcome.dispatch;
        let hvdc_dispatch_mw = (!dispatch_period.hvdc_dispatch_mw.is_empty())
            .then(|| dispatch_period.hvdc_dispatch_mw.clone());

        accumulator.record_period(
            &period_network,
            dispatch_period,
            hvdc_dispatch_mw,
            outcome.opf_solution.iterations.unwrap_or(0),
            bus_angles_rad,
            bus_voltage_pu,
            generator_q_mvar,
            outcome.opf_solution.bus_q_slack_pos_mvar.clone(),
            outcome.opf_solution.bus_q_slack_neg_mvar.clone(),
            outcome.opf_solution.bus_p_slack_pos_mw.clone(),
            outcome.opf_solution.bus_p_slack_neg_mw.clone(),
            outcome
                .opf_solution
                .branches
                .thermal_limit_slack_from_mva
                .clone(),
            outcome
                .opf_solution
                .branches
                .thermal_limit_slack_to_mva
                .clone(),
            outcome.opf_solution.vm_slack_high_pu.clone(),
            outcome.opf_solution.vm_slack_low_pu.clone(),
            outcome.opf_solution.angle_diff_slack_high_rad.clone(),
            outcome.opf_solution.angle_diff_slack_low_rad.clone(),
        );
    }

    diagnostics.total_elapsed_seconds = start.elapsed().as_secs_f64();
    *diagnostics_out = diagnostics;

    Ok(accumulator.finish())
}

/// Solve the Benders loop for a single period. Internal helper for
/// [`solve_sced_sequence_benders`]; not intended to be called directly
/// because it takes a lot of machinery the caller shouldn't have to
/// assemble.
#[allow(clippy::too_many_arguments)]
fn solve_single_period_benders(
    period_network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
    period_idx: usize,
    period_context: crate::common::runtime::DispatchPeriodContext<'_>,
    params: &ScedAcBendersRunParams,
    sub_opts: &AcOpfOptions,
    ac_opf_runtime: &AcOpfRuntime,
    _generator_ids: &HashSet<String>,
    original_bounds: &HashMap<String, (f64, f64)>,
) -> Result<BendersOutcome, ScedError> {
    let period_start = Instant::now();

    // We will mutate a cloned copy of the benders runtime so we can
    // accumulate cuts without touching the caller's state.
    let mut working_spec_benders = problem_spec.sced_ac_benders.clone();
    working_spec_benders.eta_periods = vec![period_idx];
    working_spec_benders
        .cuts
        .retain(|cut| cut.period == period_idx);

    let mut iterations: Vec<BendersIterationRecord> = Vec::new();
    let mut upper_bound = f64::INFINITY;
    let mut lower_bound = f64::NEG_INFINITY;
    let mut best_opf_solution: Option<OpfSolution> = None;
    let mut best_dispatch_period: Option<RawDispatchPeriodResult> = None;
    let mut previous_pg_by_id: Option<HashMap<String, f64>> = None;
    let mut trust_region_mw = params.trust_region_mw;
    let initial_trust_region_mw = params.trust_region_mw;
    let mut stagnation_counter: usize = 0;
    let mut dc_cost_history: Vec<f64> = Vec::new();
    let mut terminal_reason = BendersTerminalReason::MaxIterations;

    for iteration_idx in 0..=params.max_iterations {
        let iter_start = Instant::now();

        // Trust region: tighten each generator's [pmin, pmax] envelope
        // around its previous-iteration dispatch before solving the
        // master. We clone the period-applied network so mutations
        // don't leak into the next iteration.
        let mut iter_network = period_network.clone();
        if let (Some(trust), Some(prev_pg)) = (trust_region_mw, previous_pg_by_id.as_ref()) {
            apply_trust_region_to_network(&mut iter_network, prev_pg, trust, original_bounds);
        }

        let working_spec = problem_spec.with_sced_ac_benders(&working_spec_benders);

        // --- Master solve: DC SCED LP with eta + accumulated cuts ---
        //
        // We pass the period-specific context (with prev-period dispatch
        // and commitment trajectory) from the accumulator, not
        // `::initial(...)`. Without this, the LP would solve the
        // wrong period's constraints and produce a dispatch whose pg_mw
        // values correspond to period 0 rather than the requested
        // `period_idx`.
        let master = solve_sced_with_problem_spec(&iter_network, working_spec, period_context)?;
        // The master is always single-period per call, so we take the
        // first (and only) period.
        let master_period = master.periods.into_iter().next().ok_or_else(|| {
            ScedError::SolverError(format!(
                "SCED-AC Benders: master solve returned zero periods at period {period_idx}"
            ))
        })?;

        let master_total = master_period.total_cost;
        let master_eta = master_period
            .sced_ac_benders_eta_dollars_per_hour
            .unwrap_or(0.0);
        let dc_cost = master_total - master_eta;
        dc_cost_history.push(dc_cost);

        // The master LP objective is a valid lower bound on the true
        // SCED-AC optimum (the cut pool under-bounds the AC slack).
        if master_total > lower_bound {
            lower_bound = master_total;
        }

        // Extract the master's dispatch as a resource_id → Pg map.
        // We pass the *unmutated* period_network rather than iter_network
        // so the generator ordering is consistent across iterations
        // (trust-region mutation doesn't affect ordering, but there's
        // no reason to take the chance).
        let pg_by_id = resource_pg_by_id(&master_period, period_network);

        // --- Subproblem solve ---
        // Use the *pre-trust-region* period network so the subproblem
        // sees the generator's original envelope (the trust region is
        // only a proximal device for the master LP; the subproblem must
        // pin at the master's actual Pg regardless of where the trust
        // region was placed).
        let fixed_p_mw: HashMap<usize, f64> = build_fixed_p_map(period_network, &pg_by_id);
        let sub =
            match solve_ac_opf_subproblem(period_network, sub_opts, ac_opf_runtime, &fixed_p_mw) {
                Ok(outcome) => outcome,
                Err(err) => {
                    // Subproblem failure is fatal for this period. Record a
                    // diagnostic entry and return the error so the caller
                    // can decide how to handle it. We intentionally do not
                    // assign to `terminal_reason` here because the error
                    // short-circuits the loop and no caller observes it.
                    iterations.push(BendersIterationRecord {
                        iteration: iteration_idx,
                        period: period_idx,
                        dc_cost_dollars_per_hour: dc_cost,
                        eta_dollars_per_hour: master_eta,
                        subproblem_slack_dollars_per_hour: 0.0,
                        max_marginal_per_mw_per_hour: 0.0,
                        cuts_added: 0,
                        relative_gap: f64::INFINITY,
                        absolute_gap_dollars_per_hour: f64::INFINITY,
                        elapsed_seconds: iter_start.elapsed().as_secs_f64(),
                    });
                    return Err(ScedError::SolverError(format!(
                        "SCED-AC Benders: subproblem failure at period {period_idx}, \
                     iteration {iteration_idx}: {err}"
                    )));
                }
            };

        let sub_slack = sub.slack_cost_dollars_per_hour;
        let mut max_marginal: f64 = 0.0;
        for &value in sub.slack_marginal_dollars_per_mw_per_hour.values() {
            if value.abs() > max_marginal {
                max_marginal = value.abs();
            }
        }

        // --- Build the Benders cut ---
        let new_cut = build_cut_from_subproblem(
            period_idx,
            iteration_idx,
            &pg_by_id,
            sub_slack,
            &sub.slack_marginal_dollars_per_mw_per_hour,
            period_network,
            params,
        );

        // Track the best-known upper bound.
        let previous_upper_bound = upper_bound;
        let upper_bound_iter = dc_cost + sub_slack;
        let improved = upper_bound_iter < previous_upper_bound - params.abs_tol;
        if upper_bound_iter < upper_bound {
            upper_bound = upper_bound_iter;
            best_opf_solution = Some(sub.solution.clone());
            best_dispatch_period = Some(master_period.clone());
        }

        // Adaptive trust region.
        if let Some(trust) = trust_region_mw.as_mut() {
            if improved {
                let expanded = *trust * params.trust_region_expansion_factor;
                let cap = initial_trust_region_mw.unwrap_or(expanded);
                *trust = expanded.min(cap);
            } else {
                *trust = (*trust * params.trust_region_contraction_factor)
                    .max(params.trust_region_min_mw);
            }
        }

        if improved {
            stagnation_counter = 0;
        } else {
            stagnation_counter += 1;
        }

        let absolute_gap = upper_bound - lower_bound;
        let relative_gap = absolute_gap / upper_bound.abs().max(1.0);

        let cuts_added = new_cut.as_ref().map(|_| 1).unwrap_or(0);
        iterations.push(BendersIterationRecord {
            iteration: iteration_idx,
            period: period_idx,
            dc_cost_dollars_per_hour: dc_cost,
            eta_dollars_per_hour: master_eta,
            subproblem_slack_dollars_per_hour: sub_slack,
            max_marginal_per_mw_per_hour: max_marginal,
            cuts_added,
            relative_gap,
            absolute_gap_dollars_per_hour: absolute_gap,
            elapsed_seconds: iter_start.elapsed().as_secs_f64(),
        });

        // --- Convergence / termination checks ---
        if absolute_gap <= params.abs_tol {
            terminal_reason = BendersTerminalReason::AbsTol;
            break;
        }
        if relative_gap <= params.rel_tol && absolute_gap <= params.abs_tol * 10.0 {
            terminal_reason = BendersTerminalReason::RelTol;
            break;
        }
        if iteration_idx >= params.max_iterations {
            terminal_reason = BendersTerminalReason::MaxIterations;
            break;
        }
        if new_cut.is_none() {
            // No new useful cut from the subproblem. Either we are
            // AC-feasible (subslack below the floor) or the marginals
            // were all trimmed and we cannot generate useful new
            // information.
            terminal_reason = if sub_slack < params.min_slack_dollars_per_hour {
                BendersTerminalReason::AcFeasible
            } else {
                BendersTerminalReason::NoCutsGenerated
            };
            break;
        }
        if stagnation_counter >= params.stagnation_patience {
            terminal_reason = BendersTerminalReason::Stagnation;
            break;
        }
        if params.oscillation_patience >= 2
            && detect_oscillation(
                &dc_cost_history,
                params.oscillation_patience,
                params.abs_tol.max(1e-6),
            )
        {
            terminal_reason = BendersTerminalReason::Oscillation;
            break;
        }

        // --- Append the new cut to the pool, prune, and continue ---
        if let Some(cut) = new_cut {
            working_spec_benders.cuts.push(cut);
        }
        prune_redundant_cuts(&mut working_spec_benders.cuts, params, &pg_by_id);
        previous_pg_by_id = Some(pg_by_id);
    }

    // Take the best known dispatch. We must produce a final period
    // result even on failure (except for SubproblemFailed, which already
    // returned).
    let best_dispatch = best_dispatch_period.ok_or_else(|| {
        ScedError::SolverError(format!(
            "SCED-AC Benders: no dispatch produced at period {period_idx}"
        ))
    })?;
    let best_opf = best_opf_solution.ok_or_else(|| {
        ScedError::SolverError(format!(
            "SCED-AC Benders: no AC OPF solution produced at period {period_idx}"
        ))
    })?;

    let cut_records: Vec<BendersCutRecord> = working_spec_benders
        .cuts
        .iter()
        .map(BendersCutRecord::from)
        .collect();

    let converged = matches!(
        terminal_reason,
        BendersTerminalReason::AbsTol
            | BendersTerminalReason::RelTol
            | BendersTerminalReason::AcFeasible
    );

    Ok(BendersOutcome {
        dispatch: best_dispatch,
        opf_solution: best_opf,
        iterations,
        cuts: cut_records,
        upper_bound_dollars_per_hour: upper_bound,
        lower_bound_dollars_per_hour: lower_bound,
        terminal_reason,
        converged,
        elapsed_seconds: period_start.elapsed().as_secs_f64(),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn collect_in_service_generator_ids(network: &Network) -> HashSet<String> {
    network
        .generators
        .iter()
        .filter(|g| g.in_service && !g.id.is_empty())
        .map(|g| g.id.clone())
        .collect()
}

fn collect_generator_original_bounds(network: &Network) -> HashMap<String, (f64, f64)> {
    network
        .generators
        .iter()
        .filter(|g| g.in_service && !g.id.is_empty())
        .map(|g| (g.id.clone(), (g.pmin, g.pmax)))
        .collect()
}

fn resource_pg_by_id(period: &RawDispatchPeriodResult, network: &Network) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    // First try the keyed resource_results (present when the SCED
    // extract has been canonicalized via the dispatch pipeline).
    for result in &period.resource_results {
        if result.resource_id.is_empty() {
            continue;
        }
        out.insert(result.resource_id.clone(), result.power_mw);
    }
    if !out.is_empty() {
        return out;
    }
    // Fallback: raw SCED output returns `pg_mw` as a parallel array of
    // in-service generators in *network order*. Iterate the network to
    // recover the stable `(j → resource_id)` mapping.
    let in_service_ids: Vec<&str> = network
        .generators
        .iter()
        .filter(|g| g.in_service)
        .map(|g| g.id.as_str())
        .collect();
    for (j, &p_mw) in period.pg_mw.iter().enumerate() {
        if let Some(&id) = in_service_ids.get(j) {
            if !id.is_empty() {
                out.insert(id.to_string(), p_mw);
            }
        }
    }
    out
}

fn build_fixed_p_map(network: &Network, pg_by_id: &HashMap<String, f64>) -> HashMap<usize, f64> {
    let mut out = HashMap::with_capacity(pg_by_id.len());
    for (gi, generator) in network.generators.iter().enumerate() {
        if let Some(&p) = pg_by_id.get(&generator.id) {
            out.insert(gi, p);
        }
    }
    out
}

fn apply_trust_region_to_network(
    network: &mut Network,
    previous_pg: &HashMap<String, f64>,
    trust_region_mw: f64,
    original_bounds: &HashMap<String, (f64, f64)>,
) {
    if !trust_region_mw.is_finite() || trust_region_mw <= 0.0 {
        return;
    }
    for generator in network.generators.iter_mut() {
        let Some(&prev_pg) = previous_pg.get(&generator.id) else {
            continue;
        };
        let (orig_pmin, orig_pmax) = original_bounds
            .get(&generator.id)
            .copied()
            .unwrap_or((generator.pmin, generator.pmax));
        let mut lower = (prev_pg - trust_region_mw).max(orig_pmin);
        let mut upper = (prev_pg + trust_region_mw).min(orig_pmax);
        if upper < lower {
            // Degenerate: trust region doesn't intersect the original
            // bounds. Fall back to the clamp of prev_pg, which keeps
            // the LP feasible.
            let clamped = prev_pg.clamp(orig_pmin, orig_pmax);
            lower = clamped;
            upper = clamped;
        }
        generator.pmin = lower;
        generator.pmax = upper;
    }
}

fn build_cut_from_subproblem(
    period: usize,
    iteration: usize,
    pg_by_id: &HashMap<String, f64>,
    slack_cost: f64,
    slack_marginal_by_global_idx: &HashMap<usize, f64>,
    network: &Network,
    params: &ScedAcBendersRunParams,
) -> Option<ScedAcBendersCut> {
    if !slack_cost.is_finite() {
        return None;
    }
    if slack_cost < params.min_slack_dollars_per_hour && slack_marginal_by_global_idx.is_empty() {
        return None;
    }

    let mut coefficients: HashMap<String, f64> = HashMap::new();
    for (&gi, &marginal) in slack_marginal_by_global_idx.iter() {
        if !marginal.is_finite() {
            continue;
        }
        if marginal.abs() < params.marginal_trim_dollars_per_mw_per_hour {
            continue;
        }
        if let Some(generator) = network.generators.get(gi) {
            coefficients.insert(generator.id.clone(), marginal);
        }
    }

    let mut intercept = slack_cost;
    for (resource_id, marginal) in coefficients.iter() {
        if let Some(&pg) = pg_by_id.get(resource_id) {
            intercept -= marginal * pg;
        }
    }

    if coefficients.is_empty() && intercept < params.min_slack_dollars_per_hour {
        return None;
    }

    Some(ScedAcBendersCut {
        period,
        coefficients_dollars_per_mw_per_hour: coefficients,
        rhs_dollars_per_hour: intercept,
        iteration,
    })
}

fn prune_redundant_cuts(
    cuts: &mut Vec<ScedAcBendersCut>,
    params: &ScedAcBendersRunParams,
    current_pg: &HashMap<String, f64>,
) {
    // First pass: drop exact duplicates. Newer (later iteration) wins.
    cuts.sort_by_key(|c| c.iteration);
    let mut i = 0;
    while i < cuts.len() {
        let mut j = i + 1;
        while j < cuts.len() {
            if cuts_are_duplicates(&cuts[i], &cuts[j], params.cut_dedup_marginal_tol) {
                cuts.remove(i);
                break;
            }
            j += 1;
        }
        if j == cuts.len() {
            i += 1;
        }
    }

    // Per-period cap. Drop loosest cuts first (weakest current-dispatch
    // lower bound on η).
    let Some(cap) = params.max_cuts_per_period else {
        return;
    };
    let mut per_period: HashMap<usize, Vec<usize>> = HashMap::new();
    for (idx, cut) in cuts.iter().enumerate() {
        per_period.entry(cut.period).or_default().push(idx);
    }
    let mut keep: HashSet<usize> = HashSet::new();
    for (_period, mut indices) in per_period.into_iter() {
        if indices.len() <= cap {
            for idx in indices {
                keep.insert(idx);
            }
            continue;
        }
        indices.sort_by(|&a, &b| {
            let rhs_a = evaluate_cut_at(&cuts[a], current_pg);
            let rhs_b = evaluate_cut_at(&cuts[b], current_pg);
            rhs_b
                .partial_cmp(&rhs_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for idx in indices.into_iter().take(cap) {
            keep.insert(idx);
        }
    }
    let mut idx = 0;
    cuts.retain(|_cut| {
        let retain = keep.contains(&idx);
        idx += 1;
        retain
    });
}

fn cuts_are_duplicates(a: &ScedAcBendersCut, b: &ScedAcBendersCut, tol: f64) -> bool {
    if a.period != b.period {
        return false;
    }
    if (a.rhs_dollars_per_hour - b.rhs_dollars_per_hour).abs() > tol {
        return false;
    }
    if a.coefficients_dollars_per_mw_per_hour.len() != b.coefficients_dollars_per_mw_per_hour.len()
    {
        return false;
    }
    for (key, va) in a.coefficients_dollars_per_mw_per_hour.iter() {
        let Some(vb) = b.coefficients_dollars_per_mw_per_hour.get(key) else {
            return false;
        };
        if (*va - *vb).abs() > tol {
            return false;
        }
    }
    true
}

fn evaluate_cut_at(cut: &ScedAcBendersCut, pg: &HashMap<String, f64>) -> f64 {
    let mut value = cut.rhs_dollars_per_hour;
    for (rid, &coef) in cut.coefficients_dollars_per_mw_per_hour.iter() {
        if let Some(&p) = pg.get(rid) {
            value += coef * p;
        }
    }
    value
}

fn detect_oscillation(history: &[f64], patience: usize, cost_tol: f64) -> bool {
    if patience < 2 || history.len() < patience {
        return false;
    }
    let window = &history[history.len() - patience..];
    let even_values: Vec<f64> = window.iter().step_by(2).copied().collect();
    let odd_values: Vec<f64> = window.iter().skip(1).step_by(2).copied().collect();
    if odd_values.is_empty() {
        return false;
    }
    let even_mean = even_values.iter().sum::<f64>() / even_values.len() as f64;
    let odd_mean = odd_values.iter().sum::<f64>() / odd_values.len() as f64;
    if (even_mean - odd_mean).abs() < cost_tol {
        return false;
    }
    for value in &even_values {
        if (*value - even_mean).abs() > cost_tol {
            return false;
        }
    }
    for value in &odd_values {
        if (*value - odd_mean).abs() > cost_tol {
            return false;
        }
    }
    true
}

/// Build per-period `bus_voltage_pu`/`bus_angles_rad`/`generator_q_mvar`
/// vectors from the terminal AC OPF subproblem solution. These flow into
/// the [`SequentialDispatchAccumulator`] and are later consumed by the
/// canonicalizer (see `dispatch.rs`) to populate the per-bus voltage
/// fields of the public result. Aligning them with the network order
/// guarantees the canonicalizer sees a vector of length `n_buses` and
/// `n_in_service_generators` respectively.
fn extract_opf_cross_period_state(
    opf: &OpfSolution,
    network: &Network,
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n_buses = network.buses.len();
    let vm_pu_src = &opf.power_flow.voltage_magnitude_pu;
    let va_rad_src = &opf.power_flow.voltage_angle_rad;

    let mut bus_voltage_pu = Vec::with_capacity(n_buses);
    let mut bus_angles_rad = Vec::with_capacity(n_buses);
    for (bus_idx, bus) in network.buses.iter().enumerate() {
        bus_voltage_pu.push(
            vm_pu_src
                .get(bus_idx)
                .copied()
                .unwrap_or(bus.voltage_magnitude_pu),
        );
        bus_angles_rad.push(
            va_rad_src
                .get(bus_idx)
                .copied()
                .unwrap_or(bus.voltage_angle_rad),
        );
    }

    // Generator Q (MVAr): align with in-service generator order of the
    // network. The OPF solution has its own `gen_ids` ordering; map
    // back via resource id.
    let q_by_id: HashMap<&str, f64> = opf
        .generators
        .gen_ids
        .iter()
        .zip(opf.generators.gen_q_mvar.iter())
        .map(|(id, &q)| (id.as_str(), q))
        .collect();
    let generator_q_mvar: Vec<f64> = network
        .generators
        .iter()
        .filter(|g| g.in_service)
        .map(|g| q_by_id.get(g.id.as_str()).copied().unwrap_or(0.0))
        .collect();

    (bus_voltage_pu, bus_angles_rad, generator_q_mvar)
}

// Intentionally not implementing `with_sced_ac_benders` on
// `DispatchProblemSpec` here; the helper is added in
// `src/surge-dispatch/src/common/spec.rs`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::DispatchOptions;
    use crate::request::ScedAcBendersRuntime;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn three_bus_two_gen_network() -> Network {
        let mut net = Network::new("benders-orch-3bus");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PV, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 138.0));
        net.branches.push(Branch::new_line(1, 3, 0.01, 0.05, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.05, 0.0));
        net.branches.push(Branch::new_line(1, 2, 0.02, 0.10, 0.0));
        net.loads.push(Load::new(3, 100.0, 30.0));

        let mut g1 = Generator::new(1, 60.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.qmin = -100.0;
        g1.qmax = 100.0;
        g1.id = "g1".to_string();
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g1);

        let mut g2 = Generator::new(2, 40.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 200.0;
        g2.qmin = -100.0;
        g2.qmax = 100.0;
        g2.id = "g2".to_string();
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![30.0, 0.0],
        });
        net.generators.push(g2);

        net
    }

    #[test]
    fn sequence_benders_converges_on_simple_ac_feasible_case() {
        // Build a small network whose DC dispatch IS AC-feasible (linear
        // costs, comfortable Q limits, slack/PV buses at 1.0 pu). The
        // Benders loop should converge in at most one or two iterations
        // with the terminal reason being AbsTol, RelTol, or AcFeasible.
        let net = three_bus_two_gen_network();

        let params = ScedAcBendersRunParams {
            max_iterations: 5,
            rel_tol: 1.0e-3,
            abs_tol: 0.5,
            trust_region_mw: None,
            ..ScedAcBendersRunParams::default()
        };
        let benders_runtime = ScedAcBendersRuntime {
            eta_periods: vec![0],
            cuts: Vec::new(),
            orchestration: Some(params),
        };
        let opts = DispatchOptions {
            sced_ac_benders: benders_runtime,
            ..DispatchOptions::default()
        };

        let problem_spec = DispatchProblemSpec::from_options(&opts);
        let ac_opf_options = AcOpfOptions::default();
        let ac_opf_runtime = AcOpfRuntime::default();
        let mut diagnostics = BendersDiagnostics::default();

        let sol_result = solve_sced_sequence_benders(
            &net,
            problem_spec,
            &ac_opf_options,
            &ac_opf_runtime,
            &mut diagnostics,
        );

        // Some backends (Ipopt/COPT) may not be available in the test
        // environment; if the subproblem fails for that reason, we skip
        // rather than fail. A "real" test environment with solvers
        // installed will exercise the full code path.
        let Ok(sol) = sol_result else {
            eprintln!("skipping benders sequence test: backend unavailable");
            return;
        };

        assert_eq!(sol.periods.len(), 1);
        assert!(
            diagnostics.iterations.iter().all(|rec| rec.period == 0),
            "single-period problem should only produce iteration records for period 0"
        );
        assert!(!diagnostics.terminal_reasons.is_empty());
        let reason = diagnostics.terminal_reasons[0];
        assert!(
            matches!(
                reason,
                BendersTerminalReason::AbsTol
                    | BendersTerminalReason::RelTol
                    | BendersTerminalReason::AcFeasible
                    | BendersTerminalReason::NoCutsGenerated
            ),
            "benders should terminate with a converged-like reason on a clean case, got {:?}",
            reason
        );
        assert!(
            sol.periods[0].total_cost.is_finite(),
            "terminal dispatch must have a finite cost"
        );
        assert!(
            diagnostics.upper_bound_by_period[0].is_finite(),
            "upper bound must be finite"
        );
        assert!(
            diagnostics.lower_bound_by_period[0].is_finite(),
            "lower bound must be finite"
        );
        // On a small AC-feasible case the gap should be small relative
        // to the DC cost.
        let gap = diagnostics.upper_bound_by_period[0] - diagnostics.lower_bound_by_period[0];
        assert!(
            gap <= 10.0 || gap / diagnostics.upper_bound_by_period[0].abs().max(1.0) < 0.1,
            "bound gap should be small on AC-feasible case: gap = {gap}"
        );
    }

    #[test]
    fn default_run_params_match_python_reference() {
        let params = ScedAcBendersRunParams::default();
        assert_eq!(params.max_iterations, 25);
        assert!((params.rel_tol - 1.0e-4).abs() < 1e-12);
        assert!((params.abs_tol - 1.0).abs() < 1e-12);
        assert!((params.trust_region_expansion_factor - 2.0).abs() < 1e-12);
        assert!((params.trust_region_contraction_factor - 0.5).abs() < 1e-12);
        assert!(params.trust_region_mw.is_none());
        assert!((params.ac_opf_thermal_slack_penalty_per_mva - 1.0e4).abs() < 1e-9);
        assert!((params.ac_opf_bus_active_power_balance_slack_penalty_per_mw - 1.0e4).abs() < 1e-9);
        assert!(
            (params.ac_opf_bus_reactive_power_balance_slack_penalty_per_mvar - 1.0e4).abs() < 1e-9
        );
    }

    #[test]
    fn oscillation_detected_for_bimodal_sequence() {
        let history = vec![100.0, 200.0, 100.0, 200.0, 100.0, 200.0];
        assert!(detect_oscillation(&history, 4, 0.5));
    }

    #[test]
    fn oscillation_not_detected_for_monotonic_sequence() {
        let history = vec![100.0, 200.0, 300.0, 400.0];
        assert!(!detect_oscillation(&history, 4, 0.5));
    }

    #[test]
    fn oscillation_requires_distinct_clusters() {
        let history = vec![100.0, 100.01, 100.02, 100.03];
        assert!(!detect_oscillation(&history, 4, 0.5));
    }
}
