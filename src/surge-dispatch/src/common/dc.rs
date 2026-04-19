// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared DC solve-entry context helpers.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use surge_network::Network;
use surge_opf::advanced::IslandRefs;
use surge_opf::backends::{
    LpAlgorithm, LpOptions, LpPrimalStart, LpResult, LpSolveStatus, LpSolver, MipGapSchedule,
    SparseProblem,
};
use tracing::info;

use crate::common::network::{DcNetworkPlan, DcNetworkPlanInput, build_dc_network_plan};
use crate::common::setup::DispatchSetup;
use crate::common::spec::DispatchProblemSpec;
use crate::error::ScedError;

pub(crate) struct DcNetworkContext {
    pub bus_map: HashMap<u32, usize>,
    pub island_refs: IslandRefs,
    pub base_mva: f64,
}

pub(crate) struct DcModelContext<'a> {
    pub spec: DispatchProblemSpec<'a>,
    pub setup: DispatchSetup,
    pub network: DcNetworkContext,
    pub solver: Arc<dyn LpSolver>,
}

pub(crate) struct DcSolveSession<'a> {
    pub spec: DispatchProblemSpec<'a>,
    pub setup: DispatchSetup,
    pub bus_map: HashMap<u32, usize>,
    pub island_refs: IslandRefs,
    pub base_mva: f64,
    pub solver: Arc<dyn LpSolver>,
    wall_start: Instant,
}

pub(crate) struct DcSparseProblemInput {
    pub n_col: usize,
    pub n_row: usize,
    pub col_cost: Vec<f64>,
    pub col_lower: Vec<f64>,
    pub col_upper: Vec<f64>,
    pub row_lower: Vec<f64>,
    pub row_upper: Vec<f64>,
    pub a_start: Vec<i32>,
    pub a_index: Vec<i32>,
    pub a_value: Vec<f64>,
    pub q_start: Option<Vec<i32>>,
    pub q_index: Option<Vec<i32>>,
    pub q_value: Option<Vec<f64>>,
    pub col_names: Option<Vec<String>>,
    pub row_names: Option<Vec<String>>,
    pub integrality: Option<Vec<surge_opf::backends::VariableDomain>>,
}

impl<'a> DcModelContext<'a> {
    pub fn build_with_spec(
        network: &Network,
        spec: DispatchProblemSpec<'a>,
    ) -> Result<Self, ScedError> {
        let solver = spec.resolve_lp_solver();
        let setup = DispatchSetup::build(network, &spec)?;
        let network = build_network_context(network)?;
        Ok(Self {
            spec,
            setup,
            network,
            solver,
        })
    }

    pub fn build_for_period_with_spec(
        network: &Network,
        spec: DispatchProblemSpec<'a>,
        period: usize,
    ) -> Result<Self, ScedError> {
        let solver = spec.resolve_lp_solver();
        let setup = DispatchSetup::build_for_period(network, &spec, period)?;
        let network = build_network_context(network)?;
        Ok(Self {
            spec,
            setup,
            network,
            solver,
        })
    }

    pub fn build_network_plan(
        network: &Network,
        spec: &DispatchProblemSpec<'_>,
        bus_map: &HashMap<u32, usize>,
        excluded_branches: Option<&HashSet<usize>>,
    ) -> DcNetworkPlan {
        build_dc_network_plan(DcNetworkPlanInput {
            network,
            spec,
            bus_map,
            excluded_branches,
        })
    }

    pub fn into_session(self, wall_start: Instant) -> DcSolveSession<'a> {
        DcSolveSession {
            spec: self.spec,
            setup: self.setup,
            bus_map: self.network.bus_map,
            island_refs: self.network.island_refs,
            base_mva: self.network.base_mva,
            solver: self.solver,
            wall_start,
        }
    }
}

pub(crate) fn build_period_solve_session<'a>(
    network: &Network,
    spec: DispatchProblemSpec<'a>,
    period: usize,
) -> Result<DcSolveSession<'a>, ScedError> {
    let wall_start = Instant::now();
    let session =
        DcModelContext::build_for_period_with_spec(network, spec, period)?.into_session(wall_start);
    info!(
        buses = network.n_buses(),
        branches = network.n_branches(),
        n_reserve_products = session.spec.reserve_products.len(),
        n_system_requirements = session.spec.system_reserve_requirements.len(),
        enforce_thermal_limits = session.spec.enforce_thermal_limits,
        "SCED: starting solve"
    );
    Ok(session)
}

pub(crate) fn build_horizon_solve_session<'a>(
    network: &Network,
    spec: DispatchProblemSpec<'a>,
) -> Result<DcSolveSession<'a>, ScedError> {
    let wall_start = Instant::now();
    let session = DcModelContext::build_with_spec(network, spec)?.into_session(wall_start);
    info!(
        hours = session.spec.n_periods,
        buses = network.n_buses(),
        n_reserve_products = session.spec.reserve_products.len(),
        n_system_requirements = session.spec.system_reserve_requirements.len(),
        enforce_thermal_limits = session.spec.enforce_thermal_limits,
        "SCUC: starting solve"
    );
    Ok(session)
}

impl DcSolveSession<'_> {
    pub fn clock(&self) -> DcSolveClock {
        DcSolveClock {
            wall_start: self.wall_start,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DcSolveClock {
    wall_start: Instant,
}

impl DcSolveClock {
    pub fn elapsed_secs(&self) -> f64 {
        self.wall_start.elapsed().as_secs_f64()
    }
}

pub(crate) fn build_sparse_problem(input: DcSparseProblemInput) -> SparseProblem {
    SparseProblem {
        n_col: input.n_col,
        n_row: input.n_row,
        col_cost: input.col_cost,
        col_lower: input.col_lower,
        col_upper: input.col_upper,
        row_lower: input.row_lower,
        row_upper: input.row_upper,
        a_start: input.a_start,
        a_index: input.a_index,
        a_value: input.a_value,
        q_start: input.q_start,
        q_index: input.q_index,
        q_value: input.q_value,
        col_names: input.col_names,
        row_names: input.row_names,
        integrality: input.integrality,
    }
}

pub(crate) fn solve_sparse_problem(
    solver: &dyn LpSolver,
    problem: &SparseProblem,
    tolerance: f64,
    time_limit_secs: Option<f64>,
) -> Result<LpResult, ScedError> {
    solve_sparse_problem_with_start(solver, problem, tolerance, time_limit_secs, None, None)
}

pub(crate) fn solve_sparse_problem_with_start(
    solver: &dyn LpSolver,
    problem: &SparseProblem,
    tolerance: f64,
    time_limit_secs: Option<f64>,
    mip_rel_gap: Option<f64>,
    primal_start: Option<LpPrimalStart>,
) -> Result<LpResult, ScedError> {
    solve_sparse_problem_with_options(
        solver,
        problem,
        tolerance,
        time_limit_secs,
        mip_rel_gap,
        None,
        primal_start,
        LpAlgorithm::Auto,
    )
}

pub(crate) fn solve_sparse_problem_with_start_and_algorithm(
    solver: &dyn LpSolver,
    problem: &SparseProblem,
    tolerance: f64,
    time_limit_secs: Option<f64>,
    mip_rel_gap: Option<f64>,
    primal_start: Option<LpPrimalStart>,
    algorithm: LpAlgorithm,
) -> Result<LpResult, ScedError> {
    solve_sparse_problem_with_options(
        solver,
        problem,
        tolerance,
        time_limit_secs,
        mip_rel_gap,
        None,
        primal_start,
        algorithm,
    )
}

pub(crate) fn solve_sparse_problem_with_options(
    solver: &dyn LpSolver,
    problem: &SparseProblem,
    tolerance: f64,
    time_limit_secs: Option<f64>,
    mip_rel_gap: Option<f64>,
    mip_gap_schedule: Option<MipGapSchedule>,
    primal_start: Option<LpPrimalStart>,
    algorithm: LpAlgorithm,
) -> Result<LpResult, ScedError> {
    // Reduce the problem by eliminating columns whose bounds pin them
    // to a single value (and dropping rows that become trivially
    // satisfied as a result) before handing it to the backend. This
    // gives Gurobi / HiGHS / etc. a genuinely smaller problem rather
    // than relying on their presolve to undo our `col_lower = col_upper`
    // pins via constant substitution — the substitution path tends to
    // leave denser post-presolve rows and harder simplex iterations.
    //
    // Opt out with `SURGE_DISABLE_SPARSE_REDUCTION=1` for A/B comparison.
    // The identity-reduction fast path also fires automatically when the
    // problem has a quadratic objective or no fixed columns.
    let reduction = if std::env::var("SURGE_DISABLE_SPARSE_REDUCTION").as_deref() == Ok("1") {
        surge_opf::backends::reduce::SparseReduction::identity(problem.clone())
    } else {
        surge_opf::backends::reduce::reduce_by_fixed_vars(problem.clone())
    };

    if reduction.n_fixed_cols > 0 || reduction.n_dropped_rows > 0 {
        tracing::info!(
            stage = "lp_reduce",
            n_fixed_cols = reduction.n_fixed_cols,
            n_dropped_rows = reduction.n_dropped_rows,
            n_removed_nnz = reduction.n_removed_nnz,
            original_n_col = reduction.original_n_col,
            original_n_row = reduction.original_n_row,
            reduced_n_col = reduction.reduced.n_col,
            reduced_n_row = reduction.reduced.n_row,
            reduced_n_nnz = reduction.reduced.a_value.len(),
            "Sparse-problem reduction before backend solve"
        );
    }

    // Translate any primal-start hint into the reduced column indexing.
    // Dense starts are re-indexed through `ColKind::Kept`; fixed columns
    // are elided (their value is baked into the row bounds already).
    // Sparse starts filter to the kept columns. Callers that pass
    // malformed lengths get their start dropped silently — same
    // behaviour as the Gurobi backend's primal-start handling.
    let reduced_primal_start = primal_start.map(|start| match start {
        LpPrimalStart::Dense(values) if values.len() == problem.n_col => {
            let reduced: Vec<f64> = reduction
                .original_col_kind
                .iter()
                .enumerate()
                .filter_map(|(j, kind)| match kind {
                    surge_opf::backends::reduce::ColKind::Kept(_) => Some(values[j]),
                    surge_opf::backends::reduce::ColKind::Fixed(_) => None,
                })
                .collect();
            LpPrimalStart::Dense(reduced)
        }
        LpPrimalStart::Sparse { indices, values } => {
            let mut new_indices = Vec::new();
            let mut new_values = Vec::new();
            for (idx, val) in indices.into_iter().zip(values.into_iter()) {
                if idx >= problem.n_col {
                    continue;
                }
                if let surge_opf::backends::reduce::ColKind::Kept(k) =
                    reduction.original_col_kind[idx]
                {
                    new_indices.push(k as usize);
                    new_values.push(val);
                }
            }
            LpPrimalStart::Sparse {
                indices: new_indices,
                values: new_values,
            }
        }
        other => other,
    });

    let lp_opts = LpOptions {
        tolerance,
        time_limit_secs,
        mip_rel_gap,
        mip_gap_schedule,
        primal_start: reduced_primal_start,
        algorithm,
        ..Default::default()
    };
    let reduced_result = solver
        .solve(&reduction.reduced, &lp_opts)
        .map_err(ScedError::SolverError)?;
    Ok(surge_opf::backends::reduce::expand_solution(
        reduced_result,
        &reduction,
    ))
}

pub(crate) struct DcNomogramTighteningInput<'a, ComputeFlow, ApplyLimit>
where
    ComputeFlow: FnMut(usize, usize, &LpResult) -> f64,
    ApplyLimit: FnMut(&mut SparseProblem, usize, f64),
{
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub solver: &'a dyn LpSolver,
    pub lp_opts: &'a LpOptions,
    pub lp_sol: &'a mut LpResult,
    pub lp_prob: &'a mut SparseProblem,
    pub fg_rows: &'a [usize],
    pub fg_limits: &'a mut [f64],
    pub compute_flow_mw: ComputeFlow,
    pub apply_limit: ApplyLimit,
}

pub(crate) fn tighten_nomograms<ComputeFlow, ApplyLimit>(
    mut input: DcNomogramTighteningInput<'_, ComputeFlow, ApplyLimit>,
) -> Result<(), ScedError>
where
    ComputeFlow: FnMut(usize, usize, &LpResult) -> f64,
    ApplyLimit: FnMut(&mut SparseProblem, usize, f64),
{
    let has_nomograms = input.spec.enforce_flowgates
        && input.spec.max_nomogram_iter > 0
        && !input.network.nomograms.is_empty()
        && !input.fg_rows.is_empty();
    if !has_nomograms
        || !matches!(
            input.lp_sol.status,
            LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
        )
    {
        return Ok(());
    }

    let fg_name_to_ri: HashMap<&str, usize> = input
        .fg_rows
        .iter()
        .enumerate()
        .map(|(ri, &fgi)| (input.network.flowgates[fgi].name.as_str(), ri))
        .collect();

    for _ in 0..input.spec.max_nomogram_iter {
        let flowgate_flows_mw: Vec<f64> = input
            .fg_rows
            .iter()
            .enumerate()
            .map(|(ri, &fgi)| (input.compute_flow_mw)(ri, fgi, input.lp_sol))
            .collect();

        let flow_by_name: HashMap<&str, f64> = input
            .fg_rows
            .iter()
            .enumerate()
            .map(|(ri, &fgi)| {
                (
                    input.network.flowgates[fgi].name.as_str(),
                    flowgate_flows_mw[ri],
                )
            })
            .collect();

        let mut any_change = false;
        for nom in input.network.nomograms.iter().filter(|n| n.in_service) {
            let Some(&index_flow) = flow_by_name.get(nom.index_flowgate.as_str()) else {
                continue;
            };
            let Some(&ri) = fg_name_to_ri.get(nom.constrained_flowgate.as_str()) else {
                continue;
            };
            let new_limit = nom.evaluate(index_flow);
            if new_limit < input.fg_limits[ri] - 1e-3 {
                input.fg_limits[ri] = new_limit;
                (input.apply_limit)(input.lp_prob, ri, new_limit);
                any_change = true;
            }
        }
        if !any_change {
            break;
        }

        let new_solution = input
            .solver
            .solve(input.lp_prob, input.lp_opts)
            .map_err(ScedError::SolverError)?;
        if !matches!(
            new_solution.status,
            LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
        ) {
            break;
        }
        *input.lp_sol = new_solution;
    }

    Ok(())
}

fn build_network_context(network: &Network) -> Result<DcNetworkContext, ScedError> {
    let bus_map = network.bus_index_map();
    // Validate that a slack bus exists (needed for well-posed B-theta).
    let _ = network.slack_bus_index().ok_or(ScedError::NoSlackBus)?;
    let island_refs = surge_opf::advanced::detect_island_refs(network, &bus_map);
    Ok(DcNetworkContext {
        bus_map,
        island_refs,
        base_mva: network.base_mva,
    })
}
