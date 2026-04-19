// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Security-constrained AC SCED via iterative LODF cut generation.
//!
//! This is the SCED-side counterpart to `scuc/security.rs`. SCUC handles
//! a multi-period DC LP and adds N-1 cuts across the whole horizon; SCED
//! is single-period and runs after the commitment is fixed, so the cut
//! loop only iterates within one period at a time.
//!
//! Algorithm:
//!   1. Solve the base AC SCED for the period (existing pipeline,
//!      `solve_ac_sced_with_problem_spec`).
//!   2. Build a `LodfPeriodContext` for the period's snapshot if not
//!      already cached.
//!   3. Screen the solved bus angles for post-contingency overloads
//!      using `common::security::screen_branch_violations`.
//!   4. For each violation (capped at `max_cuts_per_iteration`), build a
//!      `Flowgate` constraint via `common::security::build_branch_lodf_flowgate`
//!      and append it to a *cloned* network's `flowgates` list. Cloning
//!      avoids mutating the caller's network.
//!   5. Re-solve the AC SCED on the augmented network with
//!      `enforce_flowgates: true`. The AC NLP enforces the flowgate
//!      natively — no new row machinery is required because flowgates
//!      are first-class constraints in `surge-opf::ac::problem`.
//!   6. Iterate until no new violations or `max_rounds` reached.
//!
//! ## Why this lives in SCED rather than only in SCUC
//!
//! Surge's SCUC stage is a DC LP that approximates AC physics. SCUC
//! security cuts give the LP a feasible commitment under the linearized
//! contingency model, but the AC SCED that follows can still produce a
//! solution that overloads a contingency state because:
//!   * AC losses redistribute power between branches in ways DC ignores;
//!   * voltage / reactive constraints can shift the optimal AC dispatch
//!     away from the LP solution far enough that previously-clean
//!     contingencies become binding.
//!
//! Running the LODF screen against the *AC* angles closes that gap. The
//! same cut shape (`Flowgate`) feeds the AC NLP and the validator's
//! exact post-contingency check uses the same `s^max,ctg` rating, so a
//! solution that clears this loop is much more likely to clear the
//! validator's `z_k` term.
//!
//! ## Default state
//!
//! Off. Callers must opt in via `SecurityConfig::enabled = true` (see
//! `common::security::SecurityConfig`). Until more broadly validated,
//! the existing single-shot SCED-AC path remains the production
//! default.

use std::collections::HashSet;

use surge_network::Network;
use tracing::{debug, info, warn};

use super::ac::{
    AcScedPeriodArtifacts, AcScedPeriodSolution, solve_ac_sced_with_problem_spec,
    solve_ac_sced_with_problem_spec_artifacts,
};
use crate::common::runtime::DispatchPeriodContext;
use crate::common::security::{
    BranchSecurityViolation, SecurityConfig, build_branch_lodf_flowgate, build_lodf_period_context,
    screen_branch_violations,
};
use crate::common::spec::DispatchProblemSpec;
use crate::error::ScedError;
use surge_solution::OpfSolution;

/// Solve a single-period AC SCED with iterative N-1 LODF security cuts.
///
/// When `security_cfg.enabled = false` (the default) this is a thin
/// passthrough to `solve_ac_sced_with_problem_spec` — no extra solves,
/// no cut machinery, the public API is just an explicit "I don't want
/// security cuts" signal.
///
/// When enabled, the function runs the LODF screening loop documented
/// at the top of this module. It returns the final period solution,
/// which carries `flowgate_shadow_prices` for any cuts that ended up
/// binding so callers can introspect what the loop added.
///
/// The caller's `network` is left unchanged. Each iteration clones the
/// network locally to append flowgates so concurrent SCED period solves
/// can share the same input network without contention.
pub fn solve_ac_sced_with_security_cuts(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    ac_opf: &surge_opf::ac::AcOpfOptions,
    base_runtime: &surge_opf::ac::AcOpfRuntime,
    context: DispatchPeriodContext<'_>,
    security_cfg: &SecurityConfig,
) -> Result<AcScedPeriodSolution, ScedError> {
    // Disabled fast path mirrors solve_ac_sced_with_problem_spec exactly
    // so existing callers see identical output.
    if !security_cfg.enabled || security_cfg.max_rounds == 0 {
        return solve_ac_sced_with_problem_spec(
            network,
            problem_spec,
            ac_opf,
            base_runtime,
            context,
        );
    }
    Ok(run_security_loop(
        network,
        problem_spec,
        ac_opf,
        base_runtime,
        context,
        security_cfg,
        None,
    )?
    .period_solution)
}

/// Multi-period SCED wrapper around `solve_ac_sced_with_problem_spec_artifacts`
/// that optionally runs the LODF security cut loop per period. The
/// `previous_solution` is threaded through to warm-start the base AC
/// solve in the same way the non-security path does, so multi-period
/// sequential callers can swap this in without giving up warm starts.
pub fn solve_ac_sced_with_security_cuts_artifacts(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    ac_opf: &surge_opf::ac::AcOpfOptions,
    base_runtime: &surge_opf::ac::AcOpfRuntime,
    context: DispatchPeriodContext<'_>,
    previous_solution: Option<&OpfSolution>,
    security_cfg: &SecurityConfig,
) -> Result<AcScedPeriodArtifacts, ScedError> {
    if !security_cfg.enabled || security_cfg.max_rounds == 0 {
        return solve_ac_sced_with_problem_spec_artifacts(
            network,
            problem_spec,
            ac_opf,
            base_runtime,
            context,
            previous_solution,
        );
    }
    run_security_loop(
        network,
        problem_spec,
        ac_opf,
        base_runtime,
        context,
        security_cfg,
        previous_solution,
    )
}

/// Shared iteration body for both single-period and multi-period callers.
/// Builds an LODF context once (topology doesn't change across rounds)
/// and appends `Flowgate` constraints until the post-contingency state
/// is clean or `max_rounds` is reached.
fn run_security_loop(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    ac_opf: &surge_opf::ac::AcOpfOptions,
    base_runtime: &surge_opf::ac::AcOpfRuntime,
    context: DispatchPeriodContext<'_>,
    security_cfg: &SecurityConfig,
    previous_solution: Option<&OpfSolution>,
) -> Result<AcScedPeriodArtifacts, ScedError> {
    let mut current_network = network.clone();

    // Round 0: solve the base AC SCED on the unmodified network. We go
    // through the artifacts entry point unconditionally so the security
    // loop always has access to the `OpfSolution` for the final re-solve,
    // which is what the multi-period sequential accumulator needs.
    let mut artifacts = solve_ac_sced_with_problem_spec_artifacts(
        &current_network,
        problem_spec,
        ac_opf,
        base_runtime,
        context,
        previous_solution,
    )?;

    let lodf_context = build_lodf_period_context(
        &current_network,
        &security_cfg.contingency_branch_indices,
        security_cfg.min_rate_a,
    )?;

    if lodf_context.branch_contingencies.is_empty() || lodf_context.monitored.is_empty() {
        // Nothing to screen — small networks or test fixtures with no
        // contingency-eligible branches.
        return Ok(artifacts);
    }

    let n_periods = 1; // SCED is single-period; flowgate schedules carry only one slot
    let mut excluded_pairs: HashSet<(usize, usize, usize)> = HashSet::new();
    let mut total_cuts = 0usize;

    // Enable flowgate enforcement from round 1 onward. We never mutate
    // the caller's `ac_opf`; we just flip the bit locally.
    let mut ac_opf_with_flowgates = ac_opf.clone();
    ac_opf_with_flowgates.enforce_flowgates = true;

    for round in 0..security_cfg.max_rounds {
        let violations = screen_branch_violations(
            0,
            &artifacts.period_solution.bus_angle_rad,
            &current_network,
            &lodf_context,
            current_network.base_mva,
            security_cfg.violation_tolerance_pu,
            &excluded_pairs,
        );

        if violations.is_empty() {
            info!(
                round,
                total_cuts, "AC-SCED security loop converged: no post-contingency violations"
            );
            return Ok(artifacts);
        }

        // Sort violations by severity (worst first) so the cut budget
        // is spent on the most binding ones.
        let mut sorted = violations;
        sorted.sort_by(|a, b| {
            b.severity_pu
                .partial_cmp(&a.severity_pu)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut new_cuts_this_round = 0usize;
        for violation in sorted
            .iter()
            .take(security_cfg.max_cuts_per_iteration)
            .filter(|v| insert_pair(&mut excluded_pairs, v))
        {
            let fg =
                build_branch_lodf_flowgate(violation, &current_network, &lodf_context, n_periods);
            debug!(
                round,
                ctg_branch = violation.contingency_branch_idx,
                mon_branch = violation.monitored_branch_idx,
                severity_pu = violation.severity_pu,
                "AC-SCED adding LODF flowgate"
            );
            current_network.flowgates.push(fg);
            new_cuts_this_round += 1;
        }
        total_cuts += new_cuts_this_round;

        if new_cuts_this_round == 0 {
            // All worst violations were already excluded — the loop is
            // not making progress. Bail rather than spinning forever.
            warn!(
                round,
                total_cuts,
                "AC-SCED security loop stalled: all top violations are already constrained"
            );
            return Ok(artifacts);
        }

        // Re-solve on the augmented network. The original previous
        // solution (if any) was already consumed for the base solve;
        // subsequent rounds warm-start from the round's `opf_solution`
        // so the NLP has a good seed for the cut-augmented problem.
        let prev = Some(&artifacts.opf_solution);
        artifacts = solve_ac_sced_with_problem_spec_artifacts(
            &current_network,
            problem_spec,
            &ac_opf_with_flowgates,
            base_runtime,
            context,
            prev,
        )?;

        if round + 1 == security_cfg.max_rounds {
            warn!(
                round,
                total_cuts,
                "AC-SCED security loop hit max_rounds without converging — accepting last solve"
            );
        }
    }

    Ok(artifacts)
}

/// Insert `(period, contingency_branch_idx, monitored_branch_idx)` into
/// `excluded_pairs`. Returns `true` if the pair was newly inserted (i.e.
/// the caller should add a cut for it), `false` if it was already there.
#[inline]
fn insert_pair(
    excluded_pairs: &mut HashSet<(usize, usize, usize)>,
    violation: &BranchSecurityViolation,
) -> bool {
    excluded_pairs.insert((
        violation.period,
        violation.contingency_branch_idx,
        violation.monitored_branch_idx,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::common::security::SecurityConfig;
    // Test fixtures that need the legacy DispatchOptions live here so
    // we don't have to wire up the full request/spec stack just to
    // exercise the security loop's disabled fast path.
    use crate::legacy::DispatchOptions;
    use crate::sced::ac::solve_ac_sced;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn case3_bus_with_load() -> Network {
        let mut net = Network::new("sced_security_case3");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 138.0));
        net.loads.push(Load::new(3, 80.0, 10.0));

        let mut br12 = Branch::new_line(1, 2, 0.001, 0.05, 0.0);
        br12.rating_a_mva = 200.0;
        let mut br23 = Branch::new_line(2, 3, 0.001, 0.05, 0.0);
        br23.rating_a_mva = 200.0;
        let mut br13 = Branch::new_line(1, 3, 0.001, 0.05, 0.0);
        br13.rating_a_mva = 200.0;
        net.branches = vec![br12, br23, br13];

        let mut g = Generator::new(1, 50.0, 1.0);
        g.pmin = 0.0;
        g.pmax = 200.0;
        g.qmin = -100.0;
        g.qmax = 100.0;
        g.in_service = true;
        g.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g);
        net
    }

    #[test]
    fn disabled_security_cfg_is_pure_passthrough() {
        // When SecurityConfig::disabled() is passed, the security loop
        // function must produce a solution bit-identical to calling
        // solve_ac_sced directly. This protects every existing caller
        // that opts out of security cuts.
        let net = case3_bus_with_load();
        let opts = DispatchOptions {
            n_periods: 1,
            dt_hours: 1.0,
            enforce_thermal_limits: false,
            ..DispatchOptions::default()
        };

        let direct = solve_ac_sced(&net, &opts).expect("base AC SCED should solve");

        let problem_spec = DispatchProblemSpec::from_options(&opts);
        let ac_runtime = surge_opf::ac::AcOpfRuntime::default();
        let cfg = SecurityConfig::disabled();
        let context = DispatchPeriodContext::initial(&opts.initial_state);
        let via_loop = solve_ac_sced_with_security_cuts(
            &net,
            problem_spec,
            &opts.ac_opf,
            &ac_runtime,
            context,
            &cfg,
        )
        .expect("disabled security loop should solve");

        assert_eq!(direct.pg_mw.len(), via_loop.pg_mw.len());
        for (i, (&a, &b)) in direct.pg_mw.iter().zip(via_loop.pg_mw.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-6,
                "pg_mw[{i}] disagrees: direct={a}, via_loop={b}"
            );
        }
        assert!(
            (direct.total_cost - via_loop.total_cost).abs() < 1e-3,
            "total cost disagrees: direct={}, via_loop={}",
            direct.total_cost,
            via_loop.total_cost
        );
    }

    #[test]
    fn enabled_security_cfg_short_circuits_when_no_contingencies_eligible() {
        // A 2-bus radial network has no contingency-eligible branches
        // (any single trip disconnects the system, so the LODF screen
        // skips them). The loop should still return a feasible solution
        // identical to the base solve.
        let mut net = Network::new("sced_security_radial");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.loads.push(Load::new(2, 50.0, 10.0));
        let mut br = Branch::new_line(1, 2, 0.001, 0.05, 0.0);
        br.rating_a_mva = 200.0;
        net.branches.push(br);
        let mut g = Generator::new(1, 50.0, 1.0);
        g.pmin = 0.0;
        g.pmax = 200.0;
        g.qmin = -100.0;
        g.qmax = 100.0;
        g.in_service = true;
        g.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g);

        let opts = DispatchOptions {
            n_periods: 1,
            dt_hours: 1.0,
            enforce_thermal_limits: false,
            ..DispatchOptions::default()
        };

        let direct = solve_ac_sced(&net, &opts).expect("radial AC SCED should solve");

        let problem_spec = DispatchProblemSpec::from_options(&opts);
        let ac_runtime = surge_opf::ac::AcOpfRuntime::default();
        let cfg = SecurityConfig::enabled_with_defaults();
        let context = DispatchPeriodContext::initial(&opts.initial_state);
        let via_loop = solve_ac_sced_with_security_cuts(
            &net,
            problem_spec,
            &opts.ac_opf,
            &ac_runtime,
            context,
            &cfg,
        )
        .expect("enabled security loop should solve");

        // Same dispatch since no cuts were added.
        for (a, b) in direct.pg_mw.iter().zip(via_loop.pg_mw.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }
}
