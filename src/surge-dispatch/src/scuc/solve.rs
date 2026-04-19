// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Core solve function for SCUC.

use std::time::Instant;

use surge_network::Network;

use super::extract::{ScucExtractionInput, extract_solution};
use super::plan::{ScucModelPlanInput, ScucProblemPlanInput, build_model_plan, build_problem_plan};
use super::pricing::{PricingRunInput, run_pricing, skip_pricing};
use super::problem::{ScucProblemBuildInput, ScucProblemInput};
use super::snapshot::network_at_hour_with_spec;
use crate::common::dc::build_horizon_solve_session;
use crate::common::spec::DispatchProblemSpec;
use crate::dispatch::RawDispatchSolution;
use crate::error::ScedError;
use crate::result::DispatchPhaseTimings;

/// Borrowed-network wrapper: clones once, then calls the owned variant.
///
/// Callers that already own a `Network` (e.g. `solve_explicit_security_
/// dispatch`, which builds an `explicit_network` with millions of
/// security flowgates) should prefer [`solve_scuc_with_owned_network`]
/// to avoid the duplicate clone.
pub(crate) fn solve_scuc_with_problem_spec(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
) -> Result<RawDispatchSolution, ScedError> {
    solve_scuc_with_owned_network(network.clone(), problem_spec, None)
}

/// Canonical SCUC entry that takes the network by value (no internal
/// clone). Prefer this from call sites that already own their network
/// — explicit-contingency security in particular, where the owned
/// network carries millions of security flowgates and an extra
/// internal clone costs gigabytes.
pub(crate) fn solve_scuc_with_owned_network(
    mut network: Network,
    problem_spec: DispatchProblemSpec<'_>,
    prebuilt_hourly_networks: Option<Vec<Network>>,
) -> Result<RawDispatchSolution, ScedError> {
    let fn_start = Instant::now();
    let mut timings = DispatchPhaseTimings::default();

    let t = Instant::now();
    network.canonicalize_generator_ids();
    // Use hour-0 network for topology (bus/branch structure is fixed)
    let net0 = network_at_hour_with_spec(&network, &problem_spec, 0);
    timings.network_snapshot_secs = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let solve_session = build_horizon_solve_session(&net0, problem_spec)?;
    let solve_clock = solve_session.clock();
    timings.build_session_secs = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let model_plan = build_model_plan(ScucModelPlanInput {
        network: &network,
        solve: &solve_session,
        hourly_networks: prebuilt_hourly_networks,
    })?;
    timings.build_model_plan_secs = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let problem_plan = build_problem_plan(ScucProblemPlanInput {
        network: &network,
        solve: &solve_session,
        model_plan: &model_plan,
    });
    timings.build_problem_plan_secs = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let problem_build = super::problem::build_problem(ScucProblemBuildInput {
        network: &network,
        solve: &solve_session,
        problem_plan: &problem_plan,
    });
    timings.build_problem_secs = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let problem_state = super::problem::solve_problem(ScucProblemInput {
        solve: &solve_session,
        problem: problem_build,
        problem_plan,
    })?;
    timings.solve_problem_secs = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let pricing = if solve_session.spec.run_pricing {
        run_pricing(PricingRunInput {
            network: &network,
            solve: &solve_session,
            primary_state: problem_state,
        })?
    } else {
        skip_pricing(PricingRunInput {
            network: &network,
            solve: &solve_session,
            primary_state: problem_state,
        })
    };
    timings.pricing_secs = t.elapsed().as_secs_f64();

    let solve_time_secs = solve_clock.elapsed_secs();

    let t = Instant::now();
    let mut solution = extract_solution(ScucExtractionInput {
        network: &network,
        solve: &solve_session,
        pricing_state: pricing,
        solve_time_secs,
    });
    let extract_secs = t.elapsed().as_secs_f64();
    timings.extract_solution_secs = extract_secs;

    // Explicit drops with timing so the destructor cost of the big
    // owned locals is attributable. The remaining "destructor gap"
    // visible to the caller then lives entirely inside
    // extract_solution's own return (PricingRunState + ScucProblemState).
    //
    // Note: `model_plan` is borrowed by `problem_plan` (which embeds a
    // `&ScucModelPlan<'a>`), and `problem_plan` is consumed by the
    // SCUC problem build → solve → pricing → extract_solution chain.
    // Those borrows only end once extract_solution returns, so
    // `model_plan` can't be explicitly dropped until here. It holds 18
    // per-hour `Network` clones + plan tables and is the biggest
    // remaining drop on the function-return path.
    let t_drops = Instant::now();
    drop(model_plan);
    drop(solve_session);
    drop(net0);
    drop(network);
    timings.scuc_local_drops_secs = t_drops.elapsed().as_secs_f64();

    timings.solve_scuc_self_total_secs = fn_start.elapsed().as_secs_f64();
    tracing::info!(
        total = timings.solve_scuc_self_total_secs,
        network_snapshot = timings.network_snapshot_secs,
        build_session = timings.build_session_secs,
        build_model_plan = timings.build_model_plan_secs,
        build_problem_plan = timings.build_problem_plan_secs,
        build_problem = timings.build_problem_secs,
        solve_problem = timings.solve_problem_secs,
        pricing = timings.pricing_secs,
        extract_solution = timings.extract_solution_secs,
        scuc_local_drops = timings.scuc_local_drops_secs,
        "SCUC iteration phase breakdown",
    );
    solution.diagnostics.phase_timings = Some(timings);
    Ok(solution)
}
