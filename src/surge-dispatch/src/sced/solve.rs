// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Core solve function for SCED.

use surge_network::Network;
use tracing::info;

#[cfg(test)]
use super::extract::dispatch_result_to_sced_solution;
use super::extract::{ScedExtractionInput, extract_solution};
use super::plan::{
    ScedModelPlanInput, ScedProblemPlanInput, build_explicit_model_plan, build_model_plan,
    build_problem_plan, prepare_network,
};
use super::problem::{ScedProblemBuildInput, ScedProblemInput, build_problem, solve_problem};
use crate::common::dc::build_period_solve_session;
use crate::common::runtime::DispatchPeriodContext;
use crate::common::security::build_lodf_period_context;
use crate::common::spec::{
    DispatchProblemSpec, ExplicitContingencyCase, ExplicitContingencyElement,
    ExplicitContingencyFlowgate,
};
use crate::dispatch::RawDispatchSolution;
use crate::error::ScedError;
#[cfg(test)]
use crate::legacy::DispatchOptions;
#[cfg(test)]
use crate::solution::RawScedSolution;

/// Solve a single-period SCED.
#[cfg(test)]
pub fn solve_sced(
    network: &Network,
    options: &DispatchOptions,
) -> Result<RawScedSolution, ScedError> {
    let result = solve_sced_with_problem_spec(
        network,
        DispatchProblemSpec::from_options(options),
        DispatchPeriodContext::initial(&options.initial_state),
    )?;
    Ok(dispatch_result_to_sced_solution(result))
}

pub(crate) fn solve_sced_with_problem_spec(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    context: DispatchPeriodContext<'_>,
) -> Result<RawDispatchSolution, ScedError> {
    let network = prepare_network(network);
    let network = &network;
    let solve_session = build_period_solve_session(network, problem_spec, context.period)?;
    let solve_clock = solve_session.clock();
    let model_plan = build_model_plan(ScedModelPlanInput {
        network,
        context,
        solve: &solve_session,
    })?;
    let problem_plan = build_problem_plan(ScedProblemPlanInput {
        network,
        context,
        solve: &solve_session,
        model_plan: &model_plan,
    });
    let problem_build = build_problem(ScedProblemBuildInput {
        network,
        context,
        solve: &solve_session,
        problem_plan: &problem_plan,
    });

    // --- Solve (with operating-nomogram tightening) ---
    let problem_state = solve_problem(ScedProblemInput {
        network,
        solve: &solve_session,
        problem: problem_build,
        problem_plan,
    })?;

    // --- Extract results ---
    let solve_time_secs = solve_clock.elapsed_secs();

    Ok(extract_solution(ScedExtractionInput {
        network,
        context,
        solve: &solve_session,
        problem_state,
        solve_time_secs,
    }))
}

/// Solve a single-period DC SCED with the full linearized N-1
/// contingency set built into the LP up front, including a worst-case /
/// average-case contingency objective.
///
/// This is the SCED analogue of [`crate::scuc::security::solve_explicit_security_dispatch`].
/// It enumerates all (contingency, monitored) branch pairs, builds one
/// [`Flowgate`] per pair, attaches per-case penalty / worst / avg columns,
/// and solves the augmented DC LP once.
#[allow(dead_code)]
pub(crate) fn solve_explicit_security_sced(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    context: DispatchPeriodContext<'_>,
    contingency_branch_indices: &[usize],
    min_rate_a: f64,
) -> Result<RawDispatchSolution, ScedError> {
    use surge_network::network::Flowgate;

    let network = prepare_network(network);
    let network = &network;
    let _base = network.base_mva;

    let lodf_context = build_lodf_period_context(network, contingency_branch_indices, min_rate_a)?;

    let base_flowgate_count = network.flowgates.len();
    let mut explicit_flowgates: Vec<Flowgate> = Vec::new();
    let mut explicit_cases: Vec<ExplicitContingencyCase> = Vec::new();
    let mut explicit_case_flowgates: Vec<ExplicitContingencyFlowgate> = Vec::new();

    // Enumerate all (contingency, monitored) pairs for the single period.
    let mut branch_case_indices: Vec<usize> =
        lodf_context.branch_contingencies.keys().copied().collect();
    branch_case_indices.sort_unstable();

    for contingency_branch_idx in branch_case_indices {
        let Some(contingency) = lodf_context
            .branch_contingencies
            .get(&contingency_branch_idx)
        else {
            continue;
        };
        let case_index = explicit_cases.len();
        explicit_cases.push(ExplicitContingencyCase {
            period: 0,
            element: ExplicitContingencyElement::Branch(contingency_branch_idx),
        });

        for &monitored_idx in &lodf_context.monitored {
            if monitored_idx == contingency.branch_idx {
                continue;
            }
            let Some(ptdf_l) = lodf_context.ptdf.row(monitored_idx) else {
                continue;
            };
            let lodf_lk =
                (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
            if !lodf_lk.is_finite() {
                continue;
            }

            let flowgate_idx = base_flowgate_count + explicit_flowgates.len();
            let violation = crate::common::security::BranchSecurityViolation {
                period: 0,
                contingency_branch_idx: contingency.branch_idx,
                monitored_branch_idx: monitored_idx,
                severity_pu: 0.0,
            };
            let fg = crate::common::security::build_branch_lodf_flowgate(
                &violation,
                network,
                &lodf_context,
                1, // n_periods = 1 for SCED
            );
            explicit_flowgates.push(fg);
            explicit_case_flowgates.push(ExplicitContingencyFlowgate {
                case_index,
                flowgate_idx,
            });
        }
    }

    info!(
        n_security_flowgates = explicit_flowgates.len(),
        n_contingency_cases = explicit_cases.len(),
        "Explicit-security SCED: solving with contingency flowgates"
    );

    // Augment the network with the explicit flowgates.
    let mut explicit_network = network.clone();
    explicit_network.flowgates.extend(explicit_flowgates);
    let explicit_network = &explicit_network;

    let spec = problem_spec.with_explicit_contingencies(&explicit_cases, &explicit_case_flowgates);

    let solve_session = build_period_solve_session(explicit_network, spec, context.period)?;
    let solve_clock = solve_session.clock();
    let model_plan = build_explicit_model_plan(
        ScedModelPlanInput {
            network: explicit_network,
            context,
            solve: &solve_session,
        },
        &explicit_cases,
        &explicit_case_flowgates,
    )?;
    let problem_plan = build_problem_plan(ScedProblemPlanInput {
        network: explicit_network,
        context,
        solve: &solve_session,
        model_plan: &model_plan,
    });
    let problem_build = build_problem(ScedProblemBuildInput {
        network: explicit_network,
        context,
        solve: &solve_session,
        problem_plan: &problem_plan,
    });
    let problem_state = solve_problem(ScedProblemInput {
        network: explicit_network,
        solve: &solve_session,
        problem: problem_build,
        problem_plan,
    })?;

    let solve_time_secs = solve_clock.elapsed_secs();
    Ok(extract_solution(ScedExtractionInput {
        network: explicit_network,
        context,
        solve: &solve_session,
        problem_state,
        solve_time_secs,
    }))
}
