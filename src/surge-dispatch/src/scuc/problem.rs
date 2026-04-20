// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCUC primary LP/MIP problem build and solve helpers.

use std::cmp::Ordering;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicPtr, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use surge_network::Network;
use surge_network::market::{DispatchableLoad, LoadCostModel};
use surge_network::network::Generator;
use surge_opf::backends::{
    LpAlgorithm, LpPrimalStart, LpResult, LpSolveStatus, MipGapSchedule, MipTrace, SparseProblem,
    VariableDomain,
};
use surge_sparse::Triplet;
use tracing::{debug, info, warn};

use super::cuts::ScucCommitmentCutRowsInput;
use super::layout::ScucLayout;
use super::losses::{ScucLossIterationInput, iterate_loss_factors};
use super::plan::{ScucProblemPlan, ScucStartupPlan};
use super::rows::{
    ScucCapacityLogicReserveRowsInput, ScucCcRowsInput, ScucCommitmentPolicyRowsInput,
    ScucDlRampGroup, ScucDlRampGroupRowsInput, ScucDrActivationRowsInput, ScucDrReboundRowsInput,
    ScucFozCrossRowsInput, ScucFozHourlyRowsInput, ScucFrequencyBlockRegRowsInput,
    ScucHvdcRampRowsInput, ScucPumpedHydroRowsInput, ScucPumpedHydroTransitionRowsInput,
    ScucStorageRowsInput, ScucUnitIntertemporalRowsInput,
};
use crate::common::builders;
use crate::common::costs::{resolve_generator_economics_for_period, uses_convex_polynomial_pwl};
use crate::common::dc::{
    DcSolveSession, DcSparseProblemInput, build_sparse_problem, solve_sparse_problem_with_options,
    solve_sparse_problem_with_start, solve_sparse_problem_with_start_and_algorithm,
};
use crate::common::spec::DispatchProblemSpec;
use crate::dispatch::CommitmentMode;
use crate::error::ScedError;

fn log_scuc_problem_trace(message: impl AsRef<str>) {
    info!("scuc_problem: {}", message.as_ref());
}

pub(super) struct ScucProblemBuildInput<'a> {
    pub network: &'a Network,
    pub solve: &'a DcSolveSession<'a>,
    pub problem_plan: &'a ScucProblemPlan<'a>,
}

pub(super) struct ScucProblemBuildState {
    pub hour_row_bases: Vec<usize>,
    pub hour_reserve_row_bases: Vec<usize>,
    pub n_row: usize,
    pub n_branch_flow: usize,
    pub n_fg_rows: usize,
    pub n_cc_rows: usize,
    pub row_lower: Vec<f64>,
    pub row_upper: Vec<f64>,
    #[allow(dead_code)]
    pub row_labels: Vec<String>,
    pub a_start: Vec<i32>,
    pub a_index: Vec<i32>,
    pub a_value: Vec<f64>,
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "on" | "yes")
    )
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_time_limit_secs(time_limit_secs: Option<f64>) -> Option<f64> {
    time_limit_secs.filter(|limit| limit.is_finite() && *limit > 0.0)
}

fn deadline_for_time_limit(time_limit_secs: Option<f64>) -> Option<Instant> {
    normalize_time_limit_secs(time_limit_secs)
        .map(|limit| Instant::now() + Duration::from_secs_f64(limit))
}

fn remaining_time_limit_secs(deadline: Option<Instant>) -> Option<f64> {
    deadline.map(|limit| {
        limit
            .saturating_duration_since(Instant::now())
            .as_secs_f64()
            .max(1e-3)
    })
}

#[derive(Clone, Copy, Debug)]
enum WarmStartScheduleKind {
    Provided,
    LoadCover,
    ReducedRelaxed,
    ReducedCoreMip,
    Conservative,
}

fn warm_start_time_limit_secs(
    time_limit_secs: Option<f64>,
    n_gen: usize,
    kind: WarmStartScheduleKind,
) -> Option<f64> {
    match kind {
        WarmStartScheduleKind::Provided => match normalize_time_limit_secs(time_limit_secs) {
            Some(limit) if n_gen >= 300 => Some((limit * 0.35).clamp(2.0, 20.0).min(limit)),
            Some(limit) if n_gen >= 100 => Some((limit * 0.3).clamp(1.0, 12.0).min(limit)),
            Some(limit) => Some((limit * 0.25).clamp(0.5, 8.0).min(limit)),
            None if n_gen >= 300 => Some(12.0),
            None if n_gen >= 100 => Some(8.0),
            None => Some(4.0),
        },
        WarmStartScheduleKind::LoadCover => match normalize_time_limit_secs(time_limit_secs) {
            Some(limit) if n_gen >= 300 => Some((limit * 0.5).clamp(5.0, 60.0).min(limit)),
            Some(limit) => Some((limit * 0.2).clamp(0.5, 10.0).min(limit)),
            None if n_gen >= 300 => Some(20.0),
            None => Some(5.0),
        },
        WarmStartScheduleKind::ReducedRelaxed => match normalize_time_limit_secs(time_limit_secs) {
            Some(limit) if n_gen >= 300 => Some((limit * 0.4).clamp(3.0, 30.0).min(limit)),
            Some(limit) if n_gen >= 100 => Some((limit * 0.35).clamp(2.0, 20.0).min(limit)),
            Some(limit) => Some((limit * 0.3).clamp(1.0, 10.0).min(limit)),
            None if n_gen >= 300 => Some(15.0),
            None if n_gen >= 100 => Some(10.0),
            None => Some(5.0),
        },
        WarmStartScheduleKind::ReducedCoreMip => match normalize_time_limit_secs(time_limit_secs) {
            Some(limit) if n_gen >= 300 => Some((limit * 0.5).clamp(2.0, 30.0).min(limit)),
            Some(limit) if n_gen >= 100 => Some((limit * 0.45).clamp(2.0, 20.0).min(limit)),
            Some(limit) => Some((limit * 0.4).clamp(1.0, 10.0).min(limit)),
            None if n_gen >= 300 => Some(20.0),
            None if n_gen >= 100 => Some(12.0),
            None => Some(6.0),
        },
        WarmStartScheduleKind::Conservative => match normalize_time_limit_secs(time_limit_secs) {
            Some(limit) if n_gen >= 300 => Some((limit * 0.8).clamp(20.0, 180.0).min(limit)),
            Some(limit) if n_gen >= 100 => Some((limit * 0.75).clamp(10.0, 120.0).min(limit)),
            Some(limit) => Some((limit * 0.8).clamp(5.0, 60.0).min(limit)),
            None if n_gen >= 300 => Some(90.0),
            None if n_gen >= 100 => Some(45.0),
            None => Some(20.0),
        },
    }
}

fn piecewise_linear_served_target_mw_at_price(points: &[(f64, f64)], price: f64) -> Option<f64> {
    if points.is_empty() {
        return None;
    }
    if points.len() == 1 {
        return Some(if points[0].1 >= price {
            points[0].0.max(0.0)
        } else {
            0.0
        });
    }

    let mut target_mw = 0.0_f64;
    for window in points.windows(2) {
        let (p0, mu0) = window[0];
        let (p1, mu1) = window[1];
        if !p0.is_finite() || !mu0.is_finite() || !p1.is_finite() || !mu1.is_finite() {
            continue;
        }
        let p0 = p0.max(0.0);
        let p1 = p1.max(p0);
        if mu0 >= price && mu1 >= price {
            target_mw = p1;
            continue;
        }
        if (mu0 - price) * (mu1 - price) <= 0.0 && (mu1 - mu0).abs() > 1e-9 {
            let frac = ((price - mu0) / (mu1 - mu0)).clamp(0.0, 1.0);
            target_mw = p0 + frac * (p1 - p0);
            return Some(target_mw.max(0.0));
        }
        if mu0 >= price {
            target_mw = p0;
        }
    }

    if target_mw <= 0.0 && points[0].1 >= price {
        target_mw = points.last().map(|(p, _)| p.max(0.0)).unwrap_or(0.0);
    }
    Some(target_mw.max(0.0))
}

fn zero_price_dispatchable_load_target_pu(
    cost_model: &LoadCostModel,
    p_sched_pu: f64,
    p_min_pu: f64,
    p_max_pu: f64,
    base_mva: f64,
) -> f64 {
    let lower = p_min_pu.max(0.0);
    let upper = p_max_pu.max(lower);
    let sched = p_sched_pu.clamp(lower, upper);

    let utility_target_pu = match cost_model {
        LoadCostModel::LinearCurtailment { .. } | LoadCostModel::InterruptPenalty { .. } => sched,
        LoadCostModel::QuadraticUtility { a, b } => {
            let lower_mw = lower * base_mva;
            let upper_mw = upper * base_mva;
            let target_mw = if *b > 1e-9 {
                (*a / *b).clamp(lower_mw, upper_mw)
            } else if *a > 0.0 {
                upper_mw
            } else {
                lower_mw
            };
            target_mw / base_mva
        }
        LoadCostModel::PiecewiseLinear { points } => {
            let lower_mw = lower * base_mva;
            let upper_mw = upper * base_mva;
            let target_mw = piecewise_linear_served_target_mw_at_price(points, 0.0)
                .unwrap_or(lower_mw)
                .clamp(lower_mw, upper_mw);
            target_mw / base_mva
        }
    };

    utility_target_pu.max(sched).clamp(lower, upper)
}

fn dispatchable_load_warm_start_target_pu(
    dl_index: usize,
    period: usize,
    dl: &DispatchableLoad,
    spec: &DispatchProblemSpec<'_>,
    base_mva: f64,
) -> f64 {
    let (p_sched_pu, p_max_pu, _, _, _, cost_model) =
        crate::common::costs::resolve_dl_for_period_from_spec(dl_index, period, dl, spec);
    zero_price_dispatchable_load_target_pu(cost_model, p_sched_pu, dl.p_min_pu, p_max_pu, base_mva)
}

/// Expose the private `dispatchable_load_warm_start_target_pu` as
/// MW for crate-internal consumers (currently the loss-factor cold
/// start in `scuc::security`). Returns the zero-price expected served
/// demand in MW for a single dispatchable load at a single period.
pub(crate) fn dispatchable_load_warm_start_target_mw_pub(
    dl_index: usize,
    period: usize,
    dl: &DispatchableLoad,
    spec: &DispatchProblemSpec<'_>,
    base_mva: f64,
) -> f64 {
    dispatchable_load_warm_start_target_pu(dl_index, period, dl, spec, base_mva) * base_mva
}

fn estimated_dispatchable_load_target_mw(
    spec: &DispatchProblemSpec<'_>,
    hourly_networks: &[Network],
    hour: usize,
) -> f64 {
    let network = hourly_networks
        .get(hour)
        .or_else(|| hourly_networks.first())
        .expect("dispatchable-load target estimate requires at least one hourly network");
    let base_mva = network.base_mva;
    spec.dispatchable_loads
        .iter()
        .enumerate()
        .filter(|(_, dl)| dl.in_service)
        .map(|(dl_idx, dl)| {
            dispatchable_load_warm_start_target_pu(dl_idx, hour, dl, spec, base_mva) * base_mva
        })
        .sum::<f64>()
        .max(0.0)
}

fn estimated_total_demand_target_mw(
    spec: &DispatchProblemSpec<'_>,
    hourly_networks: &[Network],
    hour: usize,
) -> f64 {
    let profiled_network = hourly_networks
        .get(hour)
        .or_else(|| hourly_networks.first())
        .expect("demand target estimate requires at least one hourly network");
    profiled_network.total_load_mw().max(0.0)
        + estimated_dispatchable_load_target_mw(spec, hourly_networks, hour)
}

fn estimated_total_reserve_target_mw(
    spec: &DispatchProblemSpec<'_>,
    hourly_networks: &[Network],
    gen_indices: &[usize],
    hour: usize,
) -> f64 {
    let profiled_network = hourly_networks
        .get(hour)
        .or_else(|| hourly_networks.first())
        .expect("reserve target estimate requires at least one hourly network");
    let base_mva = profiled_network.base_mva;

    spec.system_reserve_requirements
        .iter()
        .map(|req| req.requirement_mw_for_period(hour))
        .sum::<f64>()
        + spec
            .zonal_reserve_requirements
            .iter()
            .map(|req| {
                let served_dl_by_zone_mw = req
                    .served_dispatchable_load_coefficient
                    .map(|_| {
                        spec.dispatchable_loads
                            .iter()
                            .enumerate()
                            .filter(|(_, dl)| dl.in_service)
                            .filter(|(_, dl)| {
                                crate::common::network::zonal_requirement_matches_bus(
                                    profiled_network,
                                    spec,
                                    req,
                                    dl.bus,
                                )
                            })
                            .map(|(dl_idx, dl)| {
                                dispatchable_load_warm_start_target_pu(
                                    dl_idx, hour, dl, spec, base_mva,
                                ) * base_mva
                            })
                            .sum::<f64>()
                    })
                    .unwrap_or(0.0);
                let largest_gen_by_zone_mw = req
                    .largest_generator_dispatch_coefficient
                    .map(|_| {
                        gen_indices
                            .iter()
                            .enumerate()
                            .filter_map(|(gen_idx, &gi)| {
                                crate::common::network::zonal_participant_bus_matches(
                                    req.zone_id,
                                    req.participant_bus_numbers.as_deref(),
                                    profiled_network.generators[gi].bus,
                                    spec.generator_area.get(gen_idx).copied(),
                                )
                                .then_some(profiled_network.generators[gi].pmax.max(0.0))
                            })
                            .fold(0.0, f64::max)
                    })
                    .unwrap_or(0.0);
                req.requirement_mw_for_period(hour)
                    + req.served_dispatchable_load_coefficient.unwrap_or(0.0) * served_dl_by_zone_mw
                    + req.largest_generator_dispatch_coefficient.unwrap_or(0.0)
                        * largest_gen_by_zone_mw
            })
            .sum::<f64>()
}

fn generator_warm_start_merit_score(
    spec: &DispatchProblemSpec<'_>,
    profiled_network: &Network,
    period: usize,
    network_gen_idx: usize,
    prior_on: bool,
    state_hours: f64,
) -> f64 {
    let generator = &profiled_network.generators[network_gen_idx];
    let pmax = generator.pmax.max(0.0);
    let pmin = generator.pmin.max(0.0).min(pmax);
    let Some(economics) = resolve_generator_economics_for_period(
        network_gen_idx,
        period,
        generator,
        spec.offer_schedules,
        Some(pmax),
    ) else {
        return if prior_on { 0.0 } else { 1e3 };
    };

    let marginal_cost = economics.cost.as_ref().marginal_cost(pmin.max(0.0));
    let startup_adder = if prior_on {
        0.0
    } else {
        let startup_cost = economics
            .startup_cost_for_offline_hours(state_hours)
            .max(0.0);
        startup_cost / pmax.max(pmin).max(1.0)
    };

    marginal_cost + startup_adder
}

#[cfg(test)]
mod tests {
    use super::{
        piecewise_linear_served_target_mw_at_price, zero_price_dispatchable_load_target_pu,
    };
    use surge_network::market::LoadCostModel;

    #[test]
    fn warm_start_linear_curtailment_targets_scheduled_service() {
        let target = zero_price_dispatchable_load_target_pu(
            &LoadCostModel::LinearCurtailment { cost_per_mw: 25.0 },
            0.7,
            0.0,
            1.0,
            100.0,
        );
        assert!((target - 0.7).abs() <= 1e-9);
    }

    #[test]
    fn warm_start_quadratic_utility_targets_zero_price_optimum() {
        let target = zero_price_dispatchable_load_target_pu(
            &LoadCostModel::QuadraticUtility { a: 80.0, b: 2.0 },
            0.1,
            0.0,
            1.0,
            100.0,
        );
        assert!((target - 0.4).abs() <= 1e-9);
    }

    #[test]
    fn warm_start_piecewise_target_interpolates_zero_price_crossing() {
        let target_mw = piecewise_linear_served_target_mw_at_price(
            &[(0.0, 100.0), (50.0, 20.0), (100.0, -20.0)],
            0.0,
        )
        .expect("piecewise target");
        assert!((target_mw - 75.0).abs() <= 1e-9);
    }
}

fn max_sparse_problem_primal_violation(
    prob: &surge_opf::backends::SparseProblem,
    x: &[f64],
) -> f64 {
    let col_viol = x
        .iter()
        .zip(prob.col_lower.iter().zip(prob.col_upper.iter()))
        .map(|(&value, (&lower, &upper))| (lower - value).max(value - upper).max(0.0))
        .fold(0.0_f64, f64::max);

    let mut row_activity = vec![0.0; prob.n_row];
    let col_count = x.len().min(prob.n_col);
    for (col, &value) in x.iter().enumerate().take(col_count) {
        if value.abs() <= 1e-12 {
            continue;
        }
        let start = prob.a_start[col] as usize;
        let end = prob.a_start[col + 1] as usize;
        for nz in start..end {
            let row = prob.a_index[nz] as usize;
            row_activity[row] += prob.a_value[nz] * value;
        }
    }
    let row_viol = row_activity
        .iter()
        .zip(prob.row_lower.iter().zip(prob.row_upper.iter()))
        .map(|(&value, (&lower, &upper))| (lower - value).max(value - upper).max(0.0))
        .fold(0.0_f64, f64::max);

    col_viol.max(row_viol)
}

fn max_sparse_problem_integrality_violation(
    prob: &surge_opf::backends::SparseProblem,
    x: &[f64],
) -> f64 {
    let Some(integrality) = prob.integrality.as_ref() else {
        return 0.0;
    };
    integrality
        .iter()
        .enumerate()
        .filter_map(|(col, domain)| {
            if !matches!(domain, VariableDomain::Binary | VariableDomain::Integer) {
                return None;
            }
            let value = *x.get(col)?;
            Some((value - value.round()).abs())
        })
        .fold(0.0_f64, f64::max)
}

fn sparse_problem_objective_value(prob: &surge_opf::backends::SparseProblem, x: &[f64]) -> f64 {
    let linear = prob
        .col_cost
        .iter()
        .zip(x.iter())
        .map(|(&cost, &value)| cost * value)
        .sum::<f64>();
    let quadratic = if let (Some(q_start), Some(q_index), Some(q_value)) = (
        prob.q_start.as_ref(),
        prob.q_index.as_ref(),
        prob.q_value.as_ref(),
    ) {
        let mut total = 0.0_f64;
        for (col, &x_col) in x.iter().enumerate().take(prob.n_col) {
            if x_col.abs() <= 1e-12 {
                continue;
            }
            let start = q_start[col] as usize;
            let end = q_start[col + 1] as usize;
            for nz in start..end {
                let row = q_index[nz] as usize;
                let coeff = q_value[nz];
                let x_row = *x.get(row).unwrap_or(&0.0);
                let scale = if row == col { 0.5 } else { 1.0 };
                total += scale * coeff * x_col * x_row;
            }
        }
        total
    } else {
        0.0
    };
    linear + quadratic
}

fn build_verified_dense_mip_incumbent(
    prob: &surge_opf::backends::SparseProblem,
    x: Vec<f64>,
    tolerance: f64,
) -> Option<LpResult> {
    let primal_violation = max_sparse_problem_primal_violation(prob, &x);
    let integrality_violation = max_sparse_problem_integrality_violation(prob, &x);
    let acceptance_tol = (tolerance * 1e4).max(1e-4);
    if primal_violation > acceptance_tol || integrality_violation > acceptance_tol {
        warn!(
            primal_violation,
            integrality_violation,
            acceptance_tol,
            "SCUC: rejecting dense warm-start incumbent because feasibility check failed on original MIP"
        );
        return None;
    }
    Some(LpResult {
        objective: sparse_problem_objective_value(prob, &x),
        x,
        row_dual: vec![0.0; prob.n_row],
        col_dual: vec![0.0; prob.n_col],
        status: LpSolveStatus::SubOptimal,
        iterations: 0,
        mip_trace: None,
    })
}

fn should_use_approximate_dense_mip_start(
    prob: &surge_opf::backends::SparseProblem,
    x: &[f64],
    tolerance: f64,
) -> bool {
    let primal_violation = max_sparse_problem_primal_violation(prob, x);
    let integrality_violation = max_sparse_problem_integrality_violation(prob, x);
    let dense_start_tol = (tolerance * 1e6).max(5e-2);
    primal_violation <= dense_start_tol && integrality_violation <= dense_start_tol
}

fn should_short_circuit_to_verified_incumbent(
    solver_name: &str,
    n_col: usize,
    time_limit_secs: Option<f64>,
) -> bool {
    solver_name.eq_ignore_ascii_case("HiGHS")
        && n_col >= 100_000
        && normalize_time_limit_secs(time_limit_secs).is_some_and(|limit| limit <= 120.0)
}

fn is_time_limit_without_incumbent_error(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains("time limit before producing any feasible incumbent")
}

fn trace_commitment_solution_for_unit(
    unit_id: &str,
    network: &Network,
    setup: &crate::common::setup::DispatchSetup,
    layout: &ScucLayout,
    n_hours: usize,
    base: f64,
    solution: &LpResult,
) {
    let Some((gen_idx, &network_gen_idx)) = setup
        .gen_indices
        .iter()
        .enumerate()
        .find(|&(_, &gi)| network.generators[gi].id == unit_id)
    else {
        log_scuc_problem_trace(format!(
            "scuc_solution_trace unit={unit_id} status=not_found"
        ));
        return;
    };

    let generator = &network.generators[network_gen_idx];
    log_scuc_problem_trace(format!(
        "scuc_solution_trace unit={} gen_idx={} bus={} pmin={:.6} pmax={:.6}",
        unit_id, gen_idx, generator.bus, generator.pmin, generator.pmax
    ));
    for t in 0..n_hours {
        let u_idx = layout.commitment_col(t, gen_idx);
        let v_idx = layout.startup_col(t, gen_idx);
        let w_idx = layout.shutdown_col(t, gen_idx);
        let pg_idx = layout.pg_col(t, gen_idx);
        let u = solution.x.get(u_idx).copied().unwrap_or(0.0);
        let v = solution.x.get(v_idx).copied().unwrap_or(0.0);
        let w = solution.x.get(w_idx).copied().unwrap_or(0.0);
        let pg = solution.x.get(pg_idx).copied().unwrap_or(0.0) * base;
        log_scuc_problem_trace(format!(
            "scuc_solution_trace unit={} t={} u={:.6} v={:.6} w={:.6} pg_mw={:.6}",
            unit_id, t, u, v, w, pg
        ));
    }
}

fn trace_commitment_row_activity_for_unit(
    unit_id: &str,
    network: &Network,
    setup: &crate::common::setup::DispatchSetup,
    layout: &ScucLayout,
    n_hours: usize,
    problem: &ScucProblemBuildState,
    solution: &LpResult,
) {
    let Some((gen_idx, _)) = setup
        .gen_indices
        .iter()
        .enumerate()
        .find(|&(_, &gi)| network.generators[gi].id == unit_id)
    else {
        return;
    };

    let mut touched_rows: std::collections::BTreeMap<usize, f64> =
        std::collections::BTreeMap::new();
    for t in 0..n_hours {
        for col in [
            layout.pg_col(t, gen_idx),
            layout.commitment_col(t, gen_idx),
            layout.startup_col(t, gen_idx),
            layout.shutdown_col(t, gen_idx),
        ] {
            let x = solution.x.get(col).copied().unwrap_or(0.0);
            let start = problem.a_start[col] as usize;
            let end = problem.a_start[col + 1] as usize;
            for nz in start..end {
                let row = problem.a_index[nz] as usize;
                *touched_rows.entry(row).or_insert(0.0) += problem.a_value[nz] * x;
            }
        }
    }

    for (row, activity) in touched_rows {
        let label = problem
            .row_labels
            .get(row)
            .map(String::as_str)
            .unwrap_or("");
        let lower = problem.row_lower.get(row).copied().unwrap_or(f64::NAN);
        let upper = problem.row_upper.get(row).copied().unwrap_or(f64::NAN);
        if !label.contains(&format!("_{}", gen_idx))
            && !label.contains(&format!("g{gen_idx}_"))
            && !label.contains(&format!("g{gen_idx}__"))
        {
            continue;
        }
        log_scuc_problem_trace(format!(
            "scuc_row_trace unit={} row={} label={} activity={:.6} lower={:.6} upper={:.6}",
            unit_id, row, label, activity, lower, upper
        ));
    }
}

fn debug_bound_conflicts(
    col_lower: &[f64],
    col_upper: &[f64],
    row_lower: &[f64],
    row_upper: &[f64],
    row_labels: &[String],
) -> Option<String> {
    const TOL: f64 = 1e-9;

    let mut parts = Vec::new();

    let col_conflicts: Vec<String> = col_lower
        .iter()
        .zip(col_upper.iter())
        .enumerate()
        .filter(|(_, pair)| {
            let lo = *pair.0;
            let hi = *pair.1;
            lo > hi + TOL
        })
        .take(10)
        .map(|(idx, (&lo, &hi))| format!("col[{idx}] lo={lo:.6} hi={hi:.6}"))
        .collect();
    if !col_conflicts.is_empty() {
        parts.push(format!("column bounds [{}]", col_conflicts.join(", ")));
    }

    let row_conflicts: Vec<String> = row_lower
        .iter()
        .zip(row_upper.iter())
        .enumerate()
        .filter(|(_, pair)| {
            let lo = *pair.0;
            let hi = *pair.1;
            lo > hi + TOL
        })
        .take(10)
        .map(|(idx, (&lo, &hi))| {
            let label = row_labels.get(idx).map(String::as_str).unwrap_or("");
            format!("row[{idx}] '{label}' lo={lo:.6} hi={hi:.6}")
        })
        .collect();
    if !row_conflicts.is_empty() {
        parts.push(format!("row bounds [{}]", row_conflicts.join(", ")));
    }

    (!parts.is_empty()).then(|| parts.join("; "))
}

fn extract_tagged_indices(text: &str, tag: &str) -> Vec<usize> {
    let needle = format!("{tag}[");
    let mut indices = Vec::new();
    let mut start = 0usize;
    while let Some(found) = text[start..].find(&needle) {
        let value_start = start + found + needle.len();
        let Some(end_rel) = text[value_start..].find(']') else {
            break;
        };
        let value_end = value_start + end_rel;
        if let Ok(idx) = text[value_start..value_end].parse::<usize>() {
            indices.push(idx);
        }
        start = value_end + 1;
    }
    indices
}

fn describe_generator(generator: &Generator, gen_idx: usize) -> String {
    let machine_id = generator.machine_id.as_deref().unwrap_or("?");
    format!("g{gen_idx}:{}:{machine_id}", generator.bus)
}

fn describe_bus(network: &Network, bus_idx: usize) -> String {
    network
        .buses
        .get(bus_idx)
        .map(|bus| format!("{}:{}", bus.number, bus.name))
        .unwrap_or_else(|| format!("bus_{bus_idx}"))
}

fn describe_dispatchable_load(
    dl_idx: usize,
    layout_plan: &super::layout::ScucLayoutPlan<'_>,
    _network: &Network,
) -> String {
    layout_plan
        .active
        .dl_list
        .get(dl_idx)
        .map(|load| {
            if !load.resource_id.is_empty() {
                load.resource_id.clone()
            } else {
                format!("dl{}:bus_{}", dl_idx, load.bus)
            }
        })
        .unwrap_or_else(|| format!("dl_{dl_idx}"))
}

fn describe_reserve_column(
    local: usize,
    hour: usize,
    layout_plan: &super::layout::ScucLayoutPlan<'_>,
    network: &Network,
    setup: &crate::common::setup::DispatchSetup,
) -> Option<String> {
    for product in &layout_plan.active.reserve_layout.products {
        let product_id = product.product.id.as_str();
        if (product.gen_var_offset..product.gen_var_offset + setup.n_gen).contains(&local) {
            let gen_idx = local - product.gen_var_offset;
            let generator = setup
                .gen_indices
                .get(gen_idx)
                .and_then(|&network_idx| network.generators.get(network_idx))?;
            return Some(format!(
                "h{hour}:reserve:{product_id}:gen:{}",
                describe_generator(generator, gen_idx)
            ));
        }
        if (product.dl_var_offset..product.dl_var_offset + layout_plan.active.dl_list.len())
            .contains(&local)
        {
            let dl_idx = local - product.dl_var_offset;
            return Some(format!(
                "h{hour}:reserve:{product_id}:dl:{}",
                describe_dispatchable_load(dl_idx, layout_plan, network)
            ));
        }
        if (product.slack_offset..product.slack_offset + product.n_penalty_slacks).contains(&local)
        {
            let seg_idx = local - product.slack_offset;
            return Some(format!(
                "h{hour}:reserve:{product_id}:system_slack_{seg_idx}"
            ));
        }
        if (product.zonal_slack_offset..product.zonal_slack_offset + product.n_zonal)
            .contains(&local)
        {
            let zonal_idx = local - product.zonal_slack_offset;
            return Some(format!(
                "h{hour}:reserve:{product_id}:zonal_slack_{zonal_idx}"
            ));
        }
    }
    None
}

fn describe_scuc_column(
    idx: usize,
    problem_plan: &ScucProblemPlan<'_>,
    network: &Network,
    setup: &crate::common::setup::DispatchSetup,
    n_hours: usize,
    penalty_slack_base: usize,
) -> String {
    let layout_plan = &problem_plan.model_plan.layout;
    let layout = &layout_plan.layout;
    let variable_plan = &problem_plan.model_plan.variable;
    let vars_per_hour = layout.vars_per_hour();
    if idx < vars_per_hour * n_hours {
        let hour = idx / vars_per_hour;
        let local = idx % vars_per_hour;
        if local < layout.dispatch.pg {
            return format!(
                "h{hour}:theta:{}",
                describe_bus(network, local - layout.dispatch.theta)
            );
        }
        if local < layout.commitment {
            let gen_idx = local - layout.dispatch.pg;
            return setup
                .gen_indices
                .get(gen_idx)
                .and_then(|&network_idx| network.generators.get(network_idx))
                .map(|generator| format!("h{hour}:pg:{}", describe_generator(generator, gen_idx)))
                .unwrap_or_else(|| format!("h{hour}:pg_{gen_idx}"));
        }
        if local < layout.startup {
            let gen_idx = local - layout.commitment;
            return setup
                .gen_indices
                .get(gen_idx)
                .and_then(|&network_idx| network.generators.get(network_idx))
                .map(|generator| format!("h{hour}:u:{}", describe_generator(generator, gen_idx)))
                .unwrap_or_else(|| format!("h{hour}:u_{gen_idx}"));
        }
        if local < layout.shutdown {
            let gen_idx = local - layout.startup;
            return setup
                .gen_indices
                .get(gen_idx)
                .and_then(|&network_idx| network.generators.get(network_idx))
                .map(|generator| format!("h{hour}:v:{}", describe_generator(generator, gen_idx)))
                .unwrap_or_else(|| format!("h{hour}:v_{gen_idx}"));
        }
        if local < layout.startup_delta {
            let gen_idx = local - layout.shutdown;
            return setup
                .gen_indices
                .get(gen_idx)
                .and_then(|&network_idx| network.generators.get(network_idx))
                .map(|generator| format!("h{hour}:w:{}", describe_generator(generator, gen_idx)))
                .unwrap_or_else(|| format!("h{hour}:w_{gen_idx}"));
        }
        if local < layout.plc_lambda {
            return format!("h{hour}:startup_delta_{}", local - layout.startup_delta);
        }
        if local < layout.plc_sos2_binary {
            let local_idx = local - layout.plc_lambda;
            let gen_idx = if problem_plan.model_plan.n_bp > 0 {
                local_idx / problem_plan.model_plan.n_bp
            } else {
                0
            };
            let bp_idx = if problem_plan.model_plan.n_bp > 0 {
                local_idx % problem_plan.model_plan.n_bp
            } else {
                0
            };
            return format!("h{hour}:plc_lambda:g{gen_idx}:bp{bp_idx}");
        }
        if local < layout.dispatch.sto_ch {
            let local_idx = local - layout.plc_sos2_binary;
            let gen_idx = if problem_plan.model_plan.n_sbp > 0 {
                local_idx / problem_plan.model_plan.n_sbp
            } else {
                0
            };
            let sbp_idx = if problem_plan.model_plan.n_sbp > 0 {
                local_idx % problem_plan.model_plan.n_sbp
            } else {
                0
            };
            return format!("h{hour}:plc_sos2:g{gen_idx}:seg{sbp_idx}");
        }
        if local < layout.dispatch.sto_dis {
            return format!("h{hour}:sto_ch_{}", local - layout.dispatch.sto_ch);
        }
        if local < layout.dispatch.sto_soc {
            return format!("h{hour}:sto_dis_{}", local - layout.dispatch.sto_dis);
        }
        if local < layout.dispatch.sto_epi_dis {
            return format!("h{hour}:sto_soc_{}", local - layout.dispatch.sto_soc);
        }
        if local < layout.dispatch.sto_epi_ch {
            return format!(
                "h{hour}:sto_dis_epi_{}",
                local - layout.dispatch.sto_epi_dis
            );
        }
        if local < layout.dispatch.hvdc {
            return format!("h{hour}:sto_ch_epi_{}", local - layout.dispatch.sto_epi_ch);
        }
        if local < layout.dispatch.e_g {
            return format!("h{hour}:hvdc_{}", local - layout.dispatch.hvdc);
        }
        if local < layout.dispatch.dl {
            return format!("h{hour}:e_g_{}", local - layout.dispatch.e_g);
        }
        if local < layout.dispatch.vbid {
            let dl_idx = local - layout.dispatch.dl;
            return format!(
                "h{hour}:dl:{}",
                describe_dispatchable_load(dl_idx, layout_plan, network)
            );
        }
        if local < layout.dispatch.block {
            let vbid_idx = local - layout.dispatch.vbid;
            let original_idx = layout_plan
                .active
                .active_vbids
                .get(vbid_idx)
                .copied()
                .unwrap_or(vbid_idx);
            return format!("h{hour}:vbid:{original_idx}");
        }
        if local < layout.regulation_mode {
            return format!("h{hour}:block_{}", local - layout.dispatch.block);
        }
        if local >= layout.regulation_mode && local < layout.dispatch.reserve {
            return format!("h{hour}:reg_mode_{}", local - layout.regulation_mode);
        }
        if local >= layout.dispatch.reserve && local < layout.dispatch.block_reserve {
            if let Some(label) = describe_reserve_column(local, hour, layout_plan, network, setup) {
                return label;
            }
            return format!("h{hour}:reserve_local_{}", local - layout.dispatch.reserve);
        }
        if local >= layout.dispatch.block_reserve && local < layout.foz_delta {
            return format!(
                "h{hour}:block_reserve_{}",
                local - layout.dispatch.block_reserve
            );
        }
        if local >= layout.foz_delta && local < layout.foz_phi {
            return format!("h{hour}:foz_delta_{}", local - layout.foz_delta);
        }
        if local >= layout.foz_phi && local < layout.foz_rho {
            return format!("h{hour}:foz_phi_{}", local - layout.foz_phi);
        }
        if local >= layout.foz_rho && local < layout.ph_mode {
            return format!("h{hour}:foz_rho_{}", local - layout.foz_rho);
        }
        if local >= layout.ph_mode && local < layout.pb_curtailment_bus {
            return format!("h{hour}:ph_mode_{}", local - layout.ph_mode);
        }
        if local >= layout.pb_curtailment_bus && local < layout.pb_excess_bus {
            return format!(
                "h{hour}:pb_curt_bus:{}",
                describe_bus(network, local - layout.pb_curtailment_bus)
            );
        }
        if local >= layout.pb_excess_bus && local < layout.pb_curtailment_seg {
            return format!(
                "h{hour}:pb_excess_bus:{}",
                describe_bus(network, local - layout.pb_excess_bus)
            );
        }
        if local >= layout.pb_curtailment_seg && local < layout.pb_excess_seg {
            return format!("h{hour}:pb_curt_seg_{}", local - layout.pb_curtailment_seg);
        }
        if local >= layout.pb_excess_seg && local < layout.branch_lower_slack {
            return format!("h{hour}:pb_excess_seg_{}", local - layout.pb_excess_seg);
        }
        if local >= layout.branch_lower_slack && local < layout.branch_upper_slack {
            return format!(
                "h{hour}:branch_lower_slack_{}",
                local - layout.branch_lower_slack
            );
        }
        if local >= layout.branch_upper_slack && local < layout.flowgate_lower_slack {
            return format!(
                "h{hour}:branch_upper_slack_{}",
                local - layout.branch_upper_slack
            );
        }
        if local >= layout.flowgate_lower_slack && local < layout.flowgate_upper_slack {
            return format!(
                "h{hour}:flowgate_lower_slack_{}",
                local - layout.flowgate_lower_slack
            );
        }
        if local >= layout.flowgate_upper_slack && local < layout.interface_lower_slack {
            return format!(
                "h{hour}:flowgate_upper_slack_{}",
                local - layout.flowgate_upper_slack
            );
        }
        if local >= layout.interface_lower_slack && local < layout.interface_upper_slack {
            return format!(
                "h{hour}:interface_lower_slack_{}",
                local - layout.interface_lower_slack
            );
        }
        if local >= layout.interface_upper_slack && local < layout.headroom_slack {
            return format!(
                "h{hour}:interface_upper_slack_{}",
                local - layout.interface_upper_slack
            );
        }
        if local >= layout.headroom_slack && local < layout.footroom_slack {
            let gen_idx = local - layout.headroom_slack;
            return format!("h{hour}:headroom_slack_{gen_idx}");
        }
        if local >= layout.footroom_slack && local < layout.ramp_up_slack {
            let gen_idx = local - layout.footroom_slack;
            return format!("h{hour}:footroom_slack_{gen_idx}");
        }
        if local >= layout.ramp_up_slack && local < layout.ramp_down_slack {
            let gen_idx = local - layout.ramp_up_slack;
            return format!("h{hour}:ramp_up_slack_{gen_idx}");
        }
        if local >= layout.ramp_down_slack && local < vars_per_hour {
            let gen_idx = local - layout.ramp_down_slack;
            return format!("h{hour}:ramp_down_slack_{gen_idx}");
        }
        return format!("h{hour}:offset_{local}");
    }

    if idx >= variable_plan.penalty_slack_base
        && idx < variable_plan.penalty_slack_base + variable_plan.n_penalty_slacks
    {
        return format!("penalty_slack_{}", idx - variable_plan.penalty_slack_base);
    }
    if idx >= variable_plan.cc_var_base && idx < variable_plan.dl_act_var_base {
        return format!("cc_col_{}", idx - variable_plan.cc_var_base);
    }
    if idx >= variable_plan.dl_act_var_base && idx < variable_plan.dl_rebound_var_base {
        let local_idx = idx - variable_plan.dl_act_var_base;
        let block_idx = local_idx / n_hours;
        let hour = local_idx % n_hours;
        return format!("dr_activation:block{block_idx}:h{hour}");
    }
    if idx >= variable_plan.dl_rebound_var_base && idx < variable_plan.energy_window_slack_base {
        let local_idx = idx - variable_plan.dl_rebound_var_base;
        let block_idx = local_idx / n_hours;
        let hour = local_idx % n_hours;
        return format!("dr_rebound:block{block_idx}:h{hour}");
    }
    if idx >= variable_plan.energy_window_slack_base
        && idx < variable_plan.energy_window_slack_base + variable_plan.n_energy_window_slacks
    {
        return format!(
            "energy_window_slack_{}",
            idx - variable_plan.energy_window_slack_base
        );
    }
    if let Some(explicit_ctg) = variable_plan.explicit_contingency.as_ref() {
        if idx >= explicit_ctg.case_penalty_base && idx < explicit_ctg.worst_case_base {
            return format!("ctg_penalty_{}", idx - explicit_ctg.case_penalty_base);
        }
        if idx >= explicit_ctg.worst_case_base && idx < explicit_ctg.avg_case_base {
            return format!("ctg_worst_h{}", idx - explicit_ctg.worst_case_base);
        }
        if idx >= explicit_ctg.avg_case_base && idx < variable_plan.n_var {
            return format!("ctg_avg_h{}", idx - explicit_ctg.avg_case_base);
        }
    }
    if idx >= penalty_slack_base {
        return format!("post:col_{idx}:after_penalty_slack_base");
    }

    format!("post:col_{idx}")
}

fn sanitize_lp_name(raw: &str, prefix: &str, idx: usize) -> String {
    let mut sanitized = String::with_capacity(raw.len() + 24);
    sanitized.push_str(prefix);
    sanitized.push('_');
    sanitized.push_str(&idx.to_string());
    sanitized.push('_');
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.') {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    sanitized
}

fn should_attach_scuc_problem_names(spec: &DispatchProblemSpec<'_>) -> bool {
    spec.capture_model_diagnostics
        || env_flag("SURGE_GUROBI_EXPORT_IIS")
        || env_value("SURGE_GUROBI_IIS_PREFIX").is_some()
}

fn build_scuc_problem_names(
    problem: &ScucProblemBuildState,
    problem_plan: &ScucProblemPlan<'_>,
    network: &Network,
    setup: &crate::common::setup::DispatchSetup,
    n_hours: usize,
) -> (Vec<String>, Vec<String>) {
    let col_names = (0..problem_plan.model_plan.variable.n_var)
        .map(|idx| {
            describe_scuc_column(
                idx,
                problem_plan,
                network,
                setup,
                n_hours,
                problem_plan.model_plan.variable.penalty_slack_base,
            )
        })
        .collect();
    let row_names = (0..problem.n_row)
        .map(|idx| {
            problem
                .row_labels
                .get(idx)
                .filter(|label| !label.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("row_{idx}"))
        })
        .collect();
    (col_names, row_names)
}

fn write_linear_expression(
    writer: &mut impl Write,
    terms: &[(usize, f64)],
    col_names: &[String],
) -> std::io::Result<()> {
    if terms.is_empty() {
        write!(writer, " 0")?;
        return Ok(());
    }
    for &(col_idx, coeff) in terms {
        let sign = if coeff.is_sign_negative() { '-' } else { '+' };
        let magnitude = coeff.abs();
        write!(writer, " {sign} {magnitude} {}", col_names[col_idx])?;
    }
    Ok(())
}

#[allow(clippy::needless_range_loop)]
fn dump_scuc_lp(
    path: &Path,
    prob: &surge_opf::backends::SparseProblem,
    problem: &ScucProblemBuildState,
    problem_plan: &ScucProblemPlan<'_>,
    network: &Network,
    setup: &crate::common::setup::DispatchSetup,
    n_hours: usize,
) -> Result<(), std::io::Error> {
    const INF_THRESHOLD: f64 = 1e29;

    let mut col_names = Vec::with_capacity(prob.n_col);
    for idx in 0..prob.n_col {
        let raw = describe_scuc_column(
            idx,
            problem_plan,
            network,
            setup,
            n_hours,
            problem_plan.model_plan.variable.penalty_slack_base,
        );
        col_names.push(sanitize_lp_name(&raw, "c", idx));
    }

    let mut row_names = Vec::with_capacity(prob.n_row);
    for idx in 0..prob.n_row {
        let raw = problem
            .row_labels
            .get(idx)
            .filter(|label| !label.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("row_{idx}"));
        row_names.push(sanitize_lp_name(&raw, "r", idx));
    }

    let mut rows = vec![Vec::<(usize, f64)>::new(); prob.n_row];
    for col in 0..prob.n_col {
        for nz in prob.a_start[col] as usize..prob.a_start[col + 1] as usize {
            rows[prob.a_index[nz] as usize].push((col, prob.a_value[nz]));
        }
    }

    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    writeln!(writer, "\\ SCUC debug dump")?;
    writeln!(writer, "Minimize")?;
    write!(writer, " obj:")?;
    let objective_terms: Vec<(usize, f64)> = prob
        .col_cost
        .iter()
        .enumerate()
        .filter_map(|(idx, &cost)| (cost.abs() > 0.0).then_some((idx, cost)))
        .collect();
    write_linear_expression(&mut writer, &objective_terms, &col_names)?;
    writeln!(writer)?;

    writeln!(writer, "Subject To")?;
    for row_idx in 0..prob.n_row {
        let lb = prob.row_lower[row_idx];
        let ub = prob.row_upper[row_idx];
        let is_lb_finite = lb > -INF_THRESHOLD && lb.is_finite();
        let is_ub_finite = ub < INF_THRESHOLD && ub.is_finite();
        let is_eq = is_lb_finite && is_ub_finite && (ub - lb).abs() <= 1e-12 * ub.abs().max(1.0);
        if is_eq {
            write!(writer, " {}:", row_names[row_idx])?;
            write_linear_expression(&mut writer, &rows[row_idx], &col_names)?;
            writeln!(writer, " = {ub}")?;
            continue;
        }
        if is_lb_finite {
            write!(writer, " {}__lo:", row_names[row_idx])?;
            write_linear_expression(&mut writer, &rows[row_idx], &col_names)?;
            writeln!(writer, " >= {lb}")?;
        }
        if is_ub_finite {
            write!(writer, " {}__hi:", row_names[row_idx])?;
            write_linear_expression(&mut writer, &rows[row_idx], &col_names)?;
            writeln!(writer, " <= {ub}")?;
        }
    }

    writeln!(writer, "Bounds")?;
    let integrality = prob.integrality.as_deref();
    let mut binaries = Vec::new();
    let mut generals = Vec::new();
    for col_idx in 0..prob.n_col {
        let name = &col_names[col_idx];
        let lo = prob.col_lower[col_idx];
        let hi = prob.col_upper[col_idx];
        let domain = integrality.and_then(|values| values.get(col_idx)).copied();
        let is_binary = domain == Some(surge_opf::backends::VariableDomain::Binary)
            && lo >= -1e-12
            && hi <= 1.0 + 1e-12;
        if is_binary && lo >= -1e-12 && hi >= 1.0 - 1e-12 {
            binaries.push(name.clone());
            continue;
        }
        if domain.is_some_and(|value| value != surge_opf::backends::VariableDomain::Continuous) {
            generals.push(name.clone());
        }
        if lo <= -INF_THRESHOLD && hi >= INF_THRESHOLD {
            writeln!(writer, " {name} free")?;
        } else if (lo - hi).abs() <= 1e-12 {
            writeln!(writer, " {name} = {lo}")?;
        } else if lo <= -INF_THRESHOLD {
            writeln!(writer, " {name} <= {hi}")?;
        } else if hi >= INF_THRESHOLD {
            writeln!(writer, " {lo} <= {name}")?;
        } else {
            writeln!(writer, " {lo} <= {name} <= {hi}")?;
        }
    }

    if !binaries.is_empty() {
        writeln!(writer, "Binaries")?;
        for name in &binaries {
            writeln!(writer, " {name}")?;
        }
    }
    if !generals.is_empty() {
        writeln!(writer, "Generals")?;
        for name in &generals {
            writeln!(writer, " {name}")?;
        }
    }

    writeln!(writer, "End")?;
    writer.flush()?;

    let mut labels = BufWriter::new(File::create(path.with_extension("labels.tsv"))?);
    writeln!(labels, "kind\tindex\tname\toriginal")?;
    for (idx, name) in row_names.iter().enumerate() {
        let original = problem
            .row_labels
            .get(idx)
            .filter(|label| !label.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("row_{idx}"));
        writeln!(labels, "row\t{idx}\t{name}\t{original}")?;
    }
    for (idx, name) in col_names.iter().enumerate() {
        let original = describe_scuc_column(
            idx,
            problem_plan,
            network,
            setup,
            n_hours,
            problem_plan.model_plan.variable.penalty_slack_base,
        );
        writeln!(labels, "col\t{idx}\t{name}\t{original}")?;
    }
    labels.flush()?;

    Ok(())
}

fn dump_scuc_row_duals(
    path: &Path,
    row_labels: &[String],
    row_lower: &[f64],
    row_upper: &[f64],
    row_dual: &[f64],
) -> std::io::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, "row_index\tlabel\trow_lower\trow_upper\trow_dual")?;
    for (row_idx, &dual) in row_dual.iter().enumerate() {
        let label = row_labels.get(row_idx).map(String::as_str).unwrap_or("");
        let lower = row_lower.get(row_idx).copied().unwrap_or(f64::NAN);
        let upper = row_upper.get(row_idx).copied().unwrap_or(f64::NAN);
        writeln!(
            writer,
            "{row_idx}\t{label}\t{lower:.12e}\t{upper:.12e}\t{dual:.12e}",
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn dump_scuc_column_solution(
    path: &Path,
    col_names: &[String],
    col_cost: &[f64],
    col_lower: &[f64],
    col_upper: &[f64],
    col_value: &[f64],
    col_dual: &[f64],
) -> std::io::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(
        writer,
        "col_index\tlabel\tcol_cost\tcol_lower\tcol_upper\tcol_value\tcol_dual",
    )?;
    for (col_idx, &value) in col_value.iter().enumerate() {
        let label = col_names.get(col_idx).map(String::as_str).unwrap_or("");
        let cost = col_cost.get(col_idx).copied().unwrap_or(f64::NAN);
        let lower = col_lower.get(col_idx).copied().unwrap_or(f64::NAN);
        let upper = col_upper.get(col_idx).copied().unwrap_or(f64::NAN);
        let dual = col_dual.get(col_idx).copied().unwrap_or(f64::NAN);
        writeln!(
            writer,
            "{col_idx}\t{label}\t{cost:.12e}\t{lower:.12e}\t{upper:.12e}\t{value:.12e}\t{dual:.12e}",
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn dump_scuc_selected_column_nnz(
    path: &Path,
    problem: &SparseProblem,
    row_dual: &[f64],
    filters: &[String],
) -> std::io::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(
        writer,
        "col_index\tcol_label\trow_index\trow_label\tcoeff\trow_lower\trow_upper\trow_dual",
    )?;
    let col_names = problem.col_names.as_deref().unwrap_or(&[]);
    let row_names = problem.row_names.as_deref().unwrap_or(&[]);
    for col_idx in 0..problem.n_col {
        let col_label = col_names.get(col_idx).map(String::as_str).unwrap_or("");
        if !filters.is_empty() && !filters.iter().any(|filter| col_label.contains(filter)) {
            continue;
        }
        let start = problem.a_start.get(col_idx).copied().unwrap_or(0).max(0) as usize;
        let end = problem
            .a_start
            .get(col_idx + 1)
            .copied()
            .unwrap_or(start as i32)
            .max(start as i32) as usize;
        for nz_idx in start..end.min(problem.a_index.len()).min(problem.a_value.len()) {
            let row_idx = problem.a_index[nz_idx].max(0) as usize;
            let row_label = row_names.get(row_idx).map(String::as_str).unwrap_or("");
            let coeff = problem.a_value[nz_idx];
            let row_lower = problem.row_lower.get(row_idx).copied().unwrap_or(f64::NAN);
            let row_upper = problem.row_upper.get(row_idx).copied().unwrap_or(f64::NAN);
            let dual = row_dual.get(row_idx).copied().unwrap_or(f64::NAN);
            writeln!(
                writer,
                "{col_idx}\t{col_label}\t{row_idx}\t{row_label}\t{coeff:.12e}\t{row_lower:.12e}\t{row_upper:.12e}\t{dual:.12e}",
            )?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn augment_solver_error(
    err: String,
    problem: &ScucProblemBuildState,
    problem_plan: &ScucProblemPlan<'_>,
    network: &Network,
    setup: &crate::common::setup::DispatchSetup,
    n_hours: usize,
    penalty_slack_base: usize,
) -> String {
    let row_indices = extract_tagged_indices(&err, "row");
    let col_indices = extract_tagged_indices(&err, "col");
    if row_indices.is_empty() && col_indices.is_empty() {
        return err;
    }

    let mut extras = Vec::new();
    if !row_indices.is_empty() {
        let labels: Vec<String> = row_indices
            .into_iter()
            .take(10)
            .map(|idx| {
                let label = problem
                    .row_labels
                    .get(idx)
                    .map(String::as_str)
                    .unwrap_or("");
                format!("row[{idx}]='{label}'")
            })
            .collect();
        extras.push(format!("row_labels=[{}]", labels.join(", ")));
    }
    if !col_indices.is_empty() {
        let labels: Vec<String> = col_indices
            .into_iter()
            .take(10)
            .map(|idx| {
                format!(
                    "col[{idx}]='{}'",
                    describe_scuc_column(
                        idx,
                        problem_plan,
                        network,
                        setup,
                        n_hours,
                        penalty_slack_base,
                    )
                )
            })
            .collect();
        extras.push(format!("col_labels=[{}]", labels.join(", ")));
    }

    format!("{err}; {}", extras.join("; "))
}

fn count_rows(input: &ScucProblemBuildInput<'_>) -> (usize, usize, usize, usize, usize) {
    let skip_capacity_logic = env_flag("SURGE_DEBUG_SKIP_SCUC_CAPACITY_LOGIC");
    let skip_unit_intertemporal = env_flag("SURGE_DEBUG_SKIP_SCUC_UNIT_INTERTEMPORAL");
    let skip_commitment_policy = env_flag("SURGE_DEBUG_SKIP_SCUC_COMMITMENT_POLICY");
    let skip_cc_rows = env_flag("SURGE_DEBUG_SKIP_SCUC_CC_ROWS");
    let skip_ph_transitions = env_flag("SURGE_DEBUG_SKIP_SCUC_PH_TRANSITIONS");
    let skip_dr_rows = env_flag("SURGE_DEBUG_SKIP_SCUC_DR_ROWS");
    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let model_plan = input.problem_plan.model_plan;
    let layout_plan = &model_plan.layout;
    let active_inputs = &layout_plan.active;
    let row_metadata = &input.problem_plan.row_state.row_metadata;
    let n_gen = setup.n_gen;
    let n_hours = spec.n_periods;
    let n_branch_flow = model_plan.network_plan.constrained_branches.len();
    let n_fg_rows = model_plan.network_plan.fg_rows.len();
    let n_flow = n_branch_flow + n_fg_rows + model_plan.network_plan.iface_rows.len();
    let n_capacity_logic_reserve_rows_per_hour =
        super::rows::capacity_logic_reserve_rows_per_hour(n_gen, &active_inputs.reserve_layout);
    let n_storage_rows_per_hour = super::rows::storage_rows_per_hour(setup, spec);
    let n_foz_rows_per_hour = super::rows::foz_rows_per_hour(&row_metadata.foz_groups);
    let n_foz_cross_rows = super::rows::foz_cross_rows(&row_metadata.foz_groups, n_hours);
    let n_frequency_block_reg_rows_per_hour = super::rows::frequency_block_reg_rows_per_hour(
        input.network,
        setup,
        &active_inputs.reserve_layout,
        &setup.gen_indices,
        spec,
        active_inputs.has_reg_products,
    );
    let n_pumped_hydro_rows_per_hour = super::rows::pumped_hydro_rows_per_hour(
        &row_metadata.ph_mode_units,
        &row_metadata.ph_head_units,
    );
    // Each angle-constrained branch yields 2 constraint rows (upper + lower) per hour.
    let n_angle_diff_rows_per_hour = 2 * model_plan.network_plan.angle_constrained_branches.len();
    let rows_per_hour = n_flow
        + input.network.n_buses()
        + 2
        + if skip_capacity_logic {
            0
        } else {
            n_capacity_logic_reserve_rows_per_hour
        }
        + if model_plan.use_plc {
            n_gen * (2 + model_plan.n_bp)
        } else {
            0
        }
        + n_storage_rows_per_hour
        + n_frequency_block_reg_rows_per_hour
        + n_foz_rows_per_hour
        + n_pumped_hydro_rows_per_hour
        + n_angle_diff_rows_per_hour;
    let n_row = rows_per_hour * n_hours
        + if skip_unit_intertemporal {
            0
        } else {
            super::rows::unit_intertemporal_rows(&row_metadata.unit_intertemporal_gens, n_hours)
        }
        + if skip_commitment_policy {
            0
        } else {
            super::rows::commitment_policy_rows(
                input.network,
                &setup.gen_indices,
                spec,
                &model_plan.commitment_policy.is_must_run_ext,
                model_plan.commitment_policy.da_commitment,
                n_hours,
            )
        }
        + builders::system_policy_rows(&setup.tie_line_pairs, spec, n_hours)
        + super::rows::hvdc_ramp_rows(&row_metadata.hvdc_ramp_vars, n_hours)
        + n_foz_cross_rows
        + if skip_cc_rows {
            0
        } else {
            super::rows::cc_rows(&row_metadata.cc_row_plants, n_hours)
        }
        + super::cuts::commitment_cut_rows(&model_plan.variable.commitment_cuts)
        + super::rows::explicit_contingency_objective_rows(
            model_plan.variable.explicit_contingency.as_ref(),
        )
        + if skip_ph_transitions {
            0
        } else {
            super::rows::pumped_hydro_transition_rows(&row_metadata.ph_mode_units, n_hours)
        }
        + if skip_dr_rows {
            0
        } else {
            super::rows::dr_activation_rows(&model_plan.variable.dr_activation_loads, n_hours)
                + super::rows::dr_rebound_rows(&model_plan.variable.dr_rebound_loads, n_hours)
        }
        + {
            let groups = build_dl_ramp_groups(&active_inputs.dl_list);
            super::rows::dl_ramp_group_rows(&groups, n_hours)
        }
        + model_plan.pwl.n_rows_total
        + super::rows::branch_state_rows_count(input.network, n_hours, spec.allow_branch_switching)
        + super::rows::branch_flow_definition_rows_count(
            input.network,
            n_hours,
            spec.allow_branch_switching,
        )
        + spec.connectivity_cuts.len()
        // Option C: one LP row per compact contingency cut, placed
        // after all the classical post-hourly row families. Each cut
        // emits a single post-contingency flow constraint with its
        // own σ⁻/σ⁺ slack pair (allocated in `ScucVariablePlan`).
        + spec.contingency_cuts.len();
    (rows_per_hour, n_row, n_branch_flow, n_fg_rows, n_flow)
}

/// Collect DL blocks that share a `ramp_group` label into aggregate
/// ramp groups. Adapters that split a physical consumer into several
/// price blocks tag them with the same `ramp_group`, and the SCUC LP
/// enforces a single ramp constraint per group against the sum of
/// those blocks' served MW. Blocks without a `ramp_group` are skipped
/// here (they carry no aggregate ramp constraint).
fn build_dl_ramp_groups(
    dl_list: &[&surge_network::market::DispatchableLoad],
) -> Vec<ScucDlRampGroup> {
    use std::collections::HashMap;

    let mut by_group: HashMap<String, ScucDlRampGroup> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (load_idx, dl) in dl_list.iter().enumerate() {
        if !dl.in_service {
            continue;
        }
        let Some(group_label) = dl.ramp_group.as_ref() else {
            continue;
        };
        let ramp_up = dl.ramp_up_pu_per_hr.unwrap_or(f64::INFINITY);
        let ramp_down = dl.ramp_down_pu_per_hr.unwrap_or(f64::INFINITY);
        if !ramp_up.is_finite() && !ramp_down.is_finite() {
            // Nothing to enforce.
            continue;
        }
        let initial_p_pu = dl.initial_p_pu;
        let entry = by_group.entry(group_label.clone()).or_insert_with(|| {
            order.push(group_label.clone());
            ScucDlRampGroup {
                member_load_indices: Vec::new(),
                ramp_up_pu_per_hr: ramp_up,
                ramp_down_pu_per_hr: ramp_down,
                initial_p_pu,
            }
        });
        entry.member_load_indices.push(load_idx);
        // All blocks in a group must carry matching parameters; keep the
        // tighter of any inconsistency to stay feasible against the
        // validator's strict ramp check.
        if ramp_up < entry.ramp_up_pu_per_hr {
            entry.ramp_up_pu_per_hr = ramp_up;
        }
        if ramp_down < entry.ramp_down_pu_per_hr {
            entry.ramp_down_pu_per_hr = ramp_down;
        }
        if entry.initial_p_pu.is_none() && initial_p_pu.is_some() {
            entry.initial_p_pu = initial_p_pu;
        }
    }
    order
        .into_iter()
        .map(|key| by_group.remove(&key).unwrap())
        .collect()
}

#[allow(clippy::too_many_lines)]
/// Build the primary SCUC sparse problem for the horizon MIP or fixed-commitment LP.
pub(super) fn build_problem(input: ScucProblemBuildInput<'_>) -> ScucProblemBuildState {
    let skip_capacity_logic = env_flag("SURGE_DEBUG_SKIP_SCUC_CAPACITY_LOGIC");
    let skip_unit_intertemporal = env_flag("SURGE_DEBUG_SKIP_SCUC_UNIT_INTERTEMPORAL");
    let skip_commitment_policy = env_flag("SURGE_DEBUG_SKIP_SCUC_COMMITMENT_POLICY");
    let skip_cc_rows = env_flag("SURGE_DEBUG_SKIP_SCUC_CC_ROWS");
    let skip_ph_transitions = env_flag("SURGE_DEBUG_SKIP_SCUC_PH_TRANSITIONS");
    let skip_dr_rows = env_flag("SURGE_DEBUG_SKIP_SCUC_DR_ROWS");
    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let bus_map = &input.solve.bus_map;
    let base = input.solve.base_mva;
    let model_plan = input.problem_plan.model_plan;
    let layout_plan = &model_plan.layout;
    let active_inputs = &layout_plan.active;
    let row_metadata = &input.problem_plan.row_state.row_metadata;
    let network_plan = &model_plan.network_plan;
    let variable_plan = &model_plan.variable;
    let layout = &layout_plan.layout;
    let n_hours = spec.n_periods;
    let n_bus = input.network.n_buses();
    let n_gen = setup.n_gen;
    let n_hvdc = setup.n_hvdc_links;
    let step_h = spec.dt_hours;
    let (rows_per_hour, n_row, n_branch_flow, n_fg_rows, n_flow) = count_rows(&input);
    let n_capacity_logic_reserve_rows_per_hour =
        super::rows::capacity_logic_reserve_rows_per_hour(n_gen, &active_inputs.reserve_layout);
    let n_storage_rows_per_hour = super::rows::storage_rows_per_hour(setup, spec);
    let n_foz_rows_per_hour = super::rows::foz_rows_per_hour(&row_metadata.foz_groups);
    let n_foz_cross_rows = super::rows::foz_cross_rows(&row_metadata.foz_groups, n_hours);
    let n_frequency_block_reg_rows_per_hour = super::rows::frequency_block_reg_rows_per_hour(
        input.network,
        setup,
        &active_inputs.reserve_layout,
        &setup.gen_indices,
        spec,
        active_inputs.has_reg_products,
    );
    let n_pumped_hydro_rows_per_hour = super::rows::pumped_hydro_rows_per_hour(
        &row_metadata.ph_mode_units,
        &row_metadata.ph_head_units,
    );
    let n_angle_diff_rows_per_hour = 2 * model_plan.network_plan.angle_constrained_branches.len();
    let n_capacity_logic_reserve_rows_per_hour = if skip_capacity_logic {
        0
    } else {
        n_capacity_logic_reserve_rows_per_hour
    };
    let n_unit_intertemporal_rows = if skip_unit_intertemporal {
        0
    } else {
        super::rows::unit_intertemporal_rows(&row_metadata.unit_intertemporal_gens, n_hours)
    };
    let n_commitment_policy_rows = if skip_commitment_policy {
        0
    } else {
        super::rows::commitment_policy_rows(
            input.network,
            &setup.gen_indices,
            spec,
            &model_plan.commitment_policy.is_must_run_ext,
            model_plan.commitment_policy.da_commitment,
            n_hours,
        )
    };
    let n_system_policy_rows = builders::system_policy_rows(&setup.tie_line_pairs, spec, n_hours);
    let n_hvdc_ramp_rows = super::rows::hvdc_ramp_rows(&row_metadata.hvdc_ramp_vars, n_hours);
    let n_commitment_cut_rows = super::cuts::commitment_cut_rows(&variable_plan.commitment_cuts);
    let n_explicit_contingency_rows = super::rows::explicit_contingency_objective_rows(
        variable_plan.explicit_contingency.as_ref(),
    );
    let n_pumped_hydro_transition_rows = if skip_ph_transitions {
        0
    } else {
        super::rows::pumped_hydro_transition_rows(&row_metadata.ph_mode_units, n_hours)
    };
    let n_cc_rows = if skip_cc_rows {
        0
    } else {
        super::rows::cc_rows(&row_metadata.cc_row_plants, n_hours)
    };
    let n_dl_act_rows = if skip_dr_rows {
        0
    } else {
        super::rows::dr_activation_rows(&variable_plan.dr_activation_loads, n_hours)
    };
    let n_dl_rebound_rows = if skip_dr_rows {
        0
    } else {
        super::rows::dr_rebound_rows(&variable_plan.dr_rebound_loads, n_hours)
    };
    let dl_ramp_groups = build_dl_ramp_groups(&active_inputs.dl_list);
    let n_dl_ramp_rows = super::rows::dl_ramp_group_rows(&dl_ramp_groups, n_hours);

    // Triplet pre-allocation. Historical formula assumed ~2 triplets per
    // flowgate-row per hour, but the actual builder emits ~6 per ACTIVE
    // (flowgate, hour) pair (4 theta + 2 slack). After Fix 4a inactive
    // (single-active-period) flowgates contribute 0 triplets in their
    // non-active hours; on 617-bus explicit N-1 that collapses 930M
    // flowgate triplets to ~52M. Sizing the Vec accurately here
    // (instead of rounding up to the old worst-case of n_flow × n_hours
    // × 2) saves ~6 GB of backing allocation on that workload — the
    // peak RSS reduction that Fix 4a alone couldn't deliver. Under-
    // sizing is safe (Vec grows dynamically); the goal is to stop
    // over-reserving for inactive rows we know we won't push into.
    let n_single_period_fgs = input
        .network
        .flowgates
        .iter()
        .filter(|fg| fg.in_service && fg.limit_mw_active_period.is_some())
        .count();
    let n_base_case_fgs = model_plan
        .network_plan
        .fg_rows
        .len()
        .saturating_sub(n_single_period_fgs);
    let est_flowgate_nnz = 6 * (n_base_case_fgs * n_hours + n_single_period_fgs);
    let est_nnz = n_hours
        * (6 * n_bus
            + 5 * n_gen
            + 2 * n_branch_flow
            + 6 * model_plan.network_plan.iface_rows.len())
        + est_flowgate_nnz
        + 10 * n_gen * n_hours
        + n_storage_rows_per_hour * n_hours;
    // The per-period row count is `rows_per_hour` (fixed) plus a
    // variable PWL row contribution that depends on this hour's active
    // PWL segments. Compute the per-hour PWL count upfront so
    // hour_row_bases can be a cumulative prefix sum.
    let n_pwl_rows_per_hour: Vec<usize> = (0..n_hours)
        .map(|hour| {
            model_plan
                .pwl
                .segments_by_hour
                .get(hour)
                .map(|row| row.iter().filter_map(|s| s.as_ref()).map(|s| s.len()).sum())
                .unwrap_or(0)
        })
        .collect();
    let total_per_period: usize = (0..n_hours)
        .map(|hour| rows_per_hour + n_pwl_rows_per_hour[hour])
        .sum();
    let hour_row_bases: Vec<usize> = {
        let mut bases = Vec::with_capacity(n_hours);
        let mut base = 0usize;
        for &pwl_rows in n_pwl_rows_per_hour.iter().take(n_hours) {
            bases.push(base);
            base += rows_per_hour + pwl_rows;
        }
        bases
    };
    debug_assert_eq!(
        hour_row_bases.last().copied().unwrap_or(0)
            + rows_per_hour
            + n_pwl_rows_per_hour.last().copied().unwrap_or(0),
        total_per_period,
        "per-period row accounting drift"
    );
    let mut row_lower: Vec<f64> = vec![0.0; n_row];
    let mut row_upper: Vec<f64> = vec![0.0; n_row];
    let mut row_labels: Vec<String> = vec![String::new(); n_row];
    // Immutable binding bodies access a `&mut` via the AtomicPtr
    // reconstruction; tell the compiler we need the Vecs to be mutable
    // so the ptr captures are valid.
    let _ = (&mut row_lower, &mut row_upper, &mut row_labels);
    let mut hour_reserve_row_bases: Vec<usize> = vec![0; n_hours];
    let hvdc_loss_a_bus =
        builders::compute_hvdc_loss_injection(spec, &network_plan.hvdc_from_idx, n_bus, base);
    let offsets = &layout.dispatch;
    let _build_problem_per_period_t0 = std::time::Instant::now();

    // SAFETY: each rayon task writes strictly to its own disjoint row
    // slice `[t*rows_per_hour, (t+1)*rows_per_hour)` of row_lower /
    // row_upper / row_labels and to its own slot in
    // `hour_reserve_row_bases[t]`. Per-task triplets land in local Vecs
    // that are merged sequentially after par_iter. The Vec backings are
    // not reallocated while the parallel section runs.
    let row_lower_ptr = AtomicPtr::new(row_lower.as_mut_ptr());
    let row_upper_ptr = AtomicPtr::new(row_upper.as_mut_ptr());
    let row_labels_ptr = AtomicPtr::new(row_labels.as_mut_ptr());
    let reserve_bases_ptr = AtomicPtr::new(hour_reserve_row_bases.as_mut_ptr());
    let row_lower_len = row_lower.len();
    let row_upper_len = row_upper.len();
    let row_labels_len = row_labels.len();
    let reserve_bases_len = hour_reserve_row_bases.len();

    let per_period_triplets: Vec<Vec<Triplet<f64>>> = (0..n_hours)
        .into_par_iter()
        .map(|hour| -> Vec<Triplet<f64>> {
            // SAFETY: see comment above. Each task reconstructs
            // mutable views whose lifetimes are bounded by this closure.
            // `mut` bindings are required so the row emitters can
            // reborrow them as `&mut` when they take `&mut Vec<_>` or
            // `&mut [_]` parameters.
            let mut row_lower: &mut [f64] = unsafe {
                std::slice::from_raw_parts_mut(
                    row_lower_ptr.load(AtomicOrdering::Relaxed),
                    row_lower_len,
                )
            };
            let mut row_upper: &mut [f64] = unsafe {
                std::slice::from_raw_parts_mut(
                    row_upper_ptr.load(AtomicOrdering::Relaxed),
                    row_upper_len,
                )
            };
            let mut row_labels: &mut [String] = unsafe {
                std::slice::from_raw_parts_mut(
                    row_labels_ptr.load(AtomicOrdering::Relaxed),
                    row_labels_len,
                )
            };
            let hour_reserve_row_bases: &mut [usize] = unsafe {
                std::slice::from_raw_parts_mut(
                    reserve_bases_ptr.load(AtomicOrdering::Relaxed),
                    reserve_bases_len,
                )
            };
            // Suppress "unused mut" warnings when some code paths don't
            // exercise reborrow for a given array in this build.
            let _ = (&mut row_lower, &mut row_upper, &mut row_labels);
            let net_t = &model_plan.hourly_networks[hour];
            let mut current_row = hour_row_bases[hour];
            let mut triplets: Vec<Triplet<f64>> = Vec::new();
            let mut power_balance_extra_terms: Vec<builders::PowerBalanceExtraTerm> =
                Vec::with_capacity(variable_plan.dl_rebound_infos.len() + 2 * n_bus);
            // Original loop body lives inside this closure unchanged.
            // Begin body ─────────────────────────────────────────────
            {
        // `hour_row_bases[hour]` is pre-computed upfront since
        // `rows_per_hour` is uniform. No push needed here.
        let col_base = layout.hour_col_base(hour);
        let pbusinj_t = builders::compute_phase_shift_injection(net_t, bus_map, n_bus);
        // When branch switching is enabled, build a per-branch pf_l
        // column vector for this hour so the DC network row builders
        // can swap the y-bus contribution for the switchable pf_l
        // injection and drop the thermal envelope in favour of the
        // Big-M flow definition rows.
        let switching_pf_l_cols_vec: Option<Vec<usize>> = if spec.allow_branch_switching {
            Some(
                (0..input.network.branches.len())
                    .map(|branch_local_idx| layout.branch_flow_col(hour, branch_local_idx))
                    .collect(),
            )
        } else {
            None
        };
        power_balance_extra_terms.clear();
        for (rebound_idx, &load_idx) in variable_plan.dl_rebound_infos.iter().enumerate() {
            let dl = active_inputs.dl_list[load_idx];
            let dl_bus_idx = bus_map.get(&dl.bus).copied().unwrap_or(0);
            power_balance_extra_terms.push(builders::PowerBalanceExtraTerm {
                bus_idx: dl_bus_idx,
                col: variable_plan.dl_rebound_var_base + rebound_idx * n_hours + hour,
                coeff: 1.0,
            });
        }
        for bus_idx in 0..n_bus {
            power_balance_extra_terms.push(builders::PowerBalanceExtraTerm {
                bus_idx,
                col: layout.pb_curtailment_bus_col(hour, bus_idx),
                coeff: -1.0,
            });
            power_balance_extra_terms.push(builders::PowerBalanceExtraTerm {
                bus_idx,
                col: layout.pb_excess_bus_col(hour, bus_idx),
                coeff: 1.0,
            });
        }

        // Inject startup/shutdown trajectory power into the bus balance.
        // When a unit starts up at period t', its startup trajectory
        // injects p^su at period t < t' (where u^on is still 0).
        // Symmetrically for shutdown trajectories. The trajectory MW
        // are keyed to the startup/shutdown binary variables of the
        // originating period, mirroring the headroom/footroom terms in
        // `add_commitment_trajectory_terms`.
        if spec.offline_commitment_trajectories {
            for (j, &gi) in setup.gen_indices.iter().enumerate() {
                let generator = &input.network.generators[gi];
                if generator.is_storage() {
                    continue;
                }
                let bus_idx = setup.gen_bus_idx[j];

                // Startup trajectory: future startups at t' > hour inject
                // power at the current hour.
                // p^supc = pmin - startup_rate × (end(t') - end(hour))
                let su_rate = generator.startup_ramp_mw_per_period(1.0);
                if su_rate.is_finite() && su_rate < 1e10 {
                    for startup_hour in hour + 1..n_hours {
                        let traj_mw = generator.pmin
                            - su_rate
                                * (spec.period_end_hours(startup_hour)
                                    - spec.period_end_hours(hour));
                        if traj_mw > 1e-9 {
                            power_balance_extra_terms.push(builders::PowerBalanceExtraTerm {
                                bus_idx,
                                col: layout.startup_col(startup_hour, j),
                                coeff: -traj_mw / base,
                            });
                        }
                    }
                }

                // Shutdown trajectory: past shutdowns at t' ≤ hour inject
                // power at the current hour.
                // p^sdpc = anchor - shutdown_rate × (end(hour) - start(t'))
                let sd_rate = generator.shutdown_ramp_mw_per_period(1.0);
                if sd_rate.is_finite() && sd_rate < 1e10 {
                    let initial_anchor_mw = spec.prev_dispatch_mw_at(j).unwrap_or(generator.pmin);
                    for shutdown_hour in 0..=hour {
                        let anchor_mw = if shutdown_hour == 0 {
                            initial_anchor_mw
                        } else {
                            generator.pmin
                        };
                        let traj_mw = anchor_mw
                            - sd_rate
                                * (spec.period_end_hours(hour)
                                    - spec.period_start_hours(shutdown_hour));
                        if traj_mw > 1e-9 {
                            power_balance_extra_terms.push(builders::PowerBalanceExtraTerm {
                                bus_idx,
                                col: layout.shutdown_col(shutdown_hour, j),
                                coeff: -traj_mw / base,
                            });
                        }
                    }
                }
            }
        }

        builders::build_dc_network_rows(builders::DcNetworkRowsInput {
            flow_network: input.network,
            dispatch_network: net_t,
            constrained_branches: &network_plan.constrained_branches,
            fg_rows: &network_plan.fg_rows,
            resolved_flowgates: &setup.resolved_flowgates,
            iface_rows: &network_plan.iface_rows,
            resolved_interfaces: &setup.resolved_interfaces,
            setup,
            gen_indices: &setup.gen_indices,
            gen_bus_idx: &setup.gen_bus_idx,
            spec,
            bus_map,
            pbusinj: &pbusinj_t,
            hvdc_loss_a_bus: &hvdc_loss_a_bus,
            hvdc_from_idx: &network_plan.hvdc_from_idx,
            hvdc_to_idx: &network_plan.hvdc_to_idx,
            hvdc_band_offsets: &setup.hvdc_band_offsets_rel,
            dl_list: &active_inputs.dl_list,
            active_vbids: &active_inputs.active_vbids,
            par_branch_set: None,
            extra_terms: &power_balance_extra_terms,
            col_base,
            row_base: current_row,
            theta_off: offsets.theta,
            pg_off: offsets.pg,
            sto_ch_off: offsets.sto_ch,
            sto_dis_off: offsets.sto_dis,
            hvdc_off: offsets.hvdc,
            branch_slack: Some(builders::SoftLimitSlackLayout {
                lower_off: layout.branch_lower_slack,
                upper_off: layout.branch_upper_slack,
            }),
            flowgate_slack: Some(builders::SoftLimitSlackLayout {
                lower_off: layout.flowgate_lower_slack,
                upper_off: layout.flowgate_upper_slack,
            }),
            interface_slack: Some(builders::SoftLimitSlackLayout {
                lower_off: layout.interface_lower_slack,
                upper_off: layout.interface_upper_slack,
            }),
            dl_off: offsets.dl,
            vbid_off: offsets.vbid,
            n_hvdc_links: n_hvdc,
            storage_in_pu: false,
            base,
            hour,
            switching_pf_l_cols: switching_pf_l_cols_vec.as_deref(),
        })
        .write_into_preallocated(
            &mut triplets,
            row_lower,
            row_upper,
            current_row,
        );
        let pb_agg_row = current_row + n_flow + n_bus;
        for bus_idx in 0..n_bus {
            triplets.push(Triplet {
                row: pb_agg_row,
                col: layout.pb_curtailment_bus_col(hour, bus_idx),
                val: 1.0,
            });
            triplets.push(Triplet {
                row: pb_agg_row + 1,
                col: layout.pb_excess_bus_col(hour, bus_idx),
                val: 1.0,
            });
        }
        for seg_idx in 0..layout_plan.n_pb_curt_segs {
            triplets.push(Triplet {
                row: pb_agg_row,
                col: layout.pb_curtailment_seg_col(hour, seg_idx),
                val: -1.0,
            });
        }
        for seg_idx in 0..layout_plan.n_pb_excess_segs {
            triplets.push(Triplet {
                row: pb_agg_row + 1,
                col: layout.pb_excess_seg_col(hour, seg_idx),
                val: -1.0,
            });
        }
        row_lower[pb_agg_row] = 0.0;
        row_upper[pb_agg_row] = 0.0;
        row_lower[pb_agg_row + 1] = 0.0;
        row_upper[pb_agg_row + 1] = 0.0;

        for (i, label) in row_labels[current_row..current_row + n_flow + n_bus + 2]
            .iter_mut()
            .enumerate()
        {
            if i < n_flow {
                *label = format!("h{hour}:flow_{i}");
            } else if i < n_flow + n_bus {
                *label = format!("h{hour}:bus_{}", i - n_flow);
            } else {
                *label = format!("h{hour}:pb_agg_{}", i - n_flow - n_bus);
            }
        }
        current_row += n_flow + n_bus + 2;
        hour_reserve_row_bases[hour] = current_row + 4 * n_gen;

        if !skip_capacity_logic {
            super::rows::build_capacity_logic_reserve_rows(ScucCapacityLogicReserveRowsInput {
                network: input.network,
                hourly_network: net_t,
                spec,
                reserve_layout: &active_inputs.reserve_layout,
                r_sys_reqs: &setup.r_sys_reqs,
                r_zonal_reqs: &setup.r_zonal_reqs,
                gen_indices: &setup.gen_indices,
                dl_list: &active_inputs.dl_list,
                dl_orig_idx: &active_inputs.dl_orig_idx,
                layout,
                hour,
                n_hours,
                row_base: current_row,
                base,
            })
            .write_into_preallocated(
                &mut triplets,
                row_lower,
                row_upper,
                current_row,
            );
            for (i, label) in row_labels
                [current_row..current_row + n_capacity_logic_reserve_rows_per_hour]
                .iter_mut()
                .enumerate()
            {
                *label = format!("h{hour}:cap_logic_{i}");
            }
        }
        current_row += n_capacity_logic_reserve_rows_per_hour;

        if model_plan.use_plc {
            build_plc_rows(
                input.network,
                &setup.gen_indices,
                layout,
                hour,
                n_gen,
                model_plan.n_bp,
                model_plan.n_sbp,
                offsets.pg,
                layout.plc_lambda,
                layout.plc_sos2_binary,
                base,
                &mut triplets,
                row_lower,
                row_upper,
                row_labels,
                &mut current_row,
            );
        }

        super::rows::build_storage_rows(ScucStorageRowsInput {
            network: input.network,
            spec,
            setup,
            reserve_layout: &active_inputs.reserve_layout,
            storage_initial_soc_mwh: &setup.storage_initial_soc_mwh,
            layout,
            hour,
            row_base: current_row,
            base,
        })
        .write_into_preallocated(
            &mut triplets,
            row_lower,
            row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_storage_rows_per_hour]
            .iter_mut()
            .enumerate()
        {
            *label = format!("h{hour}:storage_{i}");
        }
        current_row += n_storage_rows_per_hour;

        let pwl_row_start = current_row;
        build_pwl_rows(
            layout,
            hour,
            offsets.pg,
            offsets.e_g,
            &model_plan.pwl.gen_j,
            &model_plan.pwl.segments_by_hour[hour],
            &mut triplets,
            row_lower,
            row_upper,
            &mut current_row,
        );

        for (i, label) in row_labels[pwl_row_start..current_row]
            .iter_mut()
            .enumerate()
        {
            *label = format!("h{hour}:pwl_{i}");
        }

        super::rows::build_frequency_block_reg_rows(ScucFrequencyBlockRegRowsInput {
            network: input.network,
            hourly_network: net_t,
            spec,
            setup,
            reserve_layout: &active_inputs.reserve_layout,
            gen_indices: &setup.gen_indices,
            layout,
            hour,
            row_base: current_row,
            base,
            has_reg_products: active_inputs.has_reg_products,
        })
        .write_into_preallocated(
            &mut triplets,
            row_lower,
            row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_frequency_block_reg_rows_per_hour]
            .iter_mut()
            .enumerate()
        {
            *label = format!("h{hour}:freq_block_{i}");
        }
        current_row += n_frequency_block_reg_rows_per_hour;

        super::rows::build_foz_rows(ScucFozHourlyRowsInput {
            foz_groups: &row_metadata.foz_groups,
            layout,
            hour,
            row_base: current_row,
            base,
        })
        .write_into_preallocated(
            &mut triplets,
            row_lower,
            row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_foz_rows_per_hour]
            .iter_mut()
            .enumerate()
        {
            *label = format!("h{hour}:foz_{i}");
        }
        current_row += n_foz_rows_per_hour;

        super::rows::build_pumped_hydro_rows(ScucPumpedHydroRowsInput {
            ph_mode_units: &row_metadata.ph_mode_units,
            ph_head_units: &row_metadata.ph_head_units,
            layout,
            hour,
            row_base: current_row,
        })
        .write_into_preallocated(
            &mut triplets,
            row_lower,
            row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_pumped_hydro_rows_per_hour]
            .iter_mut()
            .enumerate()
        {
            *label = format!("h{hour}:pumped_hydro_{i}");
        }
        current_row += n_pumped_hydro_rows_per_hour;

        // Angle difference constraint rows.
        for (row_idx, acb) in model_plan
            .network_plan
            .angle_constrained_branches
            .iter()
            .enumerate()
        {
            let upper_row = current_row + 2 * row_idx;
            let lower_row = upper_row + 1;
            let col_base = layout.hour_col_base(hour);
            let theta_off = layout.dispatch.theta;
            // Upper: θ_from - θ_to - σ_upper ≤ angmax
            triplets.push(Triplet {
                row: upper_row,
                col: col_base + theta_off + acb.from_bus_idx,
                val: 1.0,
            });
            triplets.push(Triplet {
                row: upper_row,
                col: col_base + theta_off + acb.to_bus_idx,
                val: -1.0,
            });
            triplets.push(Triplet {
                row: upper_row,
                col: layout.angle_diff_upper_slack_col(hour, row_idx),
                val: -1.0,
            });
            row_lower[upper_row] = f64::NEG_INFINITY;
            row_upper[upper_row] = acb.angmax_rad;
            // Lower: -θ_from + θ_to - σ_lower ≤ -angmin
            triplets.push(Triplet {
                row: lower_row,
                col: col_base + theta_off + acb.from_bus_idx,
                val: -1.0,
            });
            triplets.push(Triplet {
                row: lower_row,
                col: col_base + theta_off + acb.to_bus_idx,
                val: 1.0,
            });
            triplets.push(Triplet {
                row: lower_row,
                col: layout.angle_diff_lower_slack_col(hour, row_idx),
                val: -1.0,
            });
            row_lower[lower_row] = f64::NEG_INFINITY;
            row_upper[lower_row] = -acb.angmin_rad;
            row_labels[upper_row] = format!("h{hour}:angle_diff_upper_{row_idx}");
            row_labels[lower_row] = format!("h{hour}:angle_diff_lower_{row_idx}");
        }
        current_row += n_angle_diff_rows_per_hour;
            } // end original loop body block
            // Invariant: per-period emitters advanced `current_row` to
            // this hour's base + fixed `rows_per_hour` + the hour's
            // variable PWL row count.
            let expected_end =
                hour_row_bases[hour] + rows_per_hour + n_pwl_rows_per_hour[hour];
            debug_assert_eq!(
                current_row, expected_end,
                "SCUC per-period row drift at hour {hour}: current_row={current_row}, expected={expected_end}",
            );
            let _ = power_balance_extra_terms; // suppress unused warning on the empty branch
            triplets
        })
        .collect();
    // After the parallel per-period section all disjoint row slices are
    // populated in-place; triplets are merged back sequentially so the
    // later post-hourly section can push onto the single Vec as before.
    let mut triplets: Vec<Triplet<f64>> = Vec::with_capacity(
        per_period_triplets.iter().map(|v| v.len()).sum::<usize>() + est_nnz / 8,
    );
    for local in per_period_triplets {
        triplets.extend(local);
    }
    let mut current_row = total_per_period;
    tracing::info!(
        stage = "build_problem.per_period_loop",
        secs = _build_problem_per_period_t0.elapsed().as_secs_f64(),
        n_triplets = triplets.len(),
        "SCUC build_problem timing"
    );
    let _post_hourly_t0 = std::time::Instant::now();

    let _post_hourly_start = current_row;
    if !skip_unit_intertemporal {
        super::rows::build_unit_intertemporal_rows(ScucUnitIntertemporalRowsInput {
            units: &row_metadata.unit_intertemporal_gens,
            layout,
            n_hours,
            row_base: current_row,
        })
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_unit_intertemporal_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("unit_intertemporal_{i}");
        }
    }
    current_row += n_unit_intertemporal_rows;

    let _commit_policy_start = current_row;
    if !skip_commitment_policy {
        super::rows::build_commitment_policy_rows(ScucCommitmentPolicyRowsInput {
            network: input.network,
            hourly_networks: &model_plan.hourly_networks,
            spec,
            gen_indices: &setup.gen_indices,
            is_must_run_ext: &model_plan.commitment_policy.is_must_run_ext,
            da_commitment: model_plan.commitment_policy.da_commitment,
            layout,
            n_hours,
            row_base: current_row,
            base,
            energy_window_slack_base: variable_plan.energy_window_slack_base,
            energy_window_slack_kinds: &variable_plan.energy_window_slack_kinds,
        })
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_commitment_policy_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("commitment_policy_{i}");
        }
    }
    current_row += n_commitment_policy_rows;

    let system_policy_hour_bases: Vec<usize> = (0..n_hours)
        .map(|hour| layout.hour_col_base(hour))
        .collect();
    builders::build_system_policy_rows(builders::DcSystemPolicyRowsInput {
        spec,
        hourly_networks: &model_plan.hourly_networks,
        effective_co2_rate: &setup.effective_co2_rate,
        tie_line_pairs: &setup.tie_line_pairs,
        hour_col_bases: &system_policy_hour_bases,
        theta_off: offsets.theta,
        pg_off: offsets.pg,
        hvdc_off: offsets.hvdc,
        hvdc_band_offsets: &setup.hvdc_band_offsets_rel,
        row_base: current_row,
        base,
        step_h,
    })
    .write_into_preallocated(&mut triplets, &mut row_lower, &mut row_upper, current_row);
    for (i, label) in row_labels[current_row..current_row + n_system_policy_rows]
        .iter_mut()
        .enumerate()
    {
        *label = format!("system_policy_{i}");
    }
    current_row += n_system_policy_rows;

    super::rows::build_hvdc_ramp_rows(ScucHvdcRampRowsInput {
        ramp_vars: &row_metadata.hvdc_ramp_vars,
        layout,
        n_hours,
        row_base: current_row,
    })
    .write_into_preallocated(&mut triplets, &mut row_lower, &mut row_upper, current_row);
    for (i, label) in row_labels[current_row..current_row + n_hvdc_ramp_rows]
        .iter_mut()
        .enumerate()
    {
        *label = format!("hvdc_ramp_{i}");
    }
    current_row += n_hvdc_ramp_rows;

    super::rows::build_foz_cross_rows(ScucFozCrossRowsInput {
        foz_groups: &row_metadata.foz_groups,
        layout,
        n_hours,
        row_base: current_row,
    })
    .write_into_preallocated(&mut triplets, &mut row_lower, &mut row_upper, current_row);
    for (i, label) in row_labels[current_row..current_row + n_foz_cross_rows]
        .iter_mut()
        .enumerate()
    {
        *label = format!("foz_cross_{i}");
    }
    current_row += n_foz_cross_rows;

    super::cuts::build_commitment_cut_rows(ScucCommitmentCutRowsInput {
        cuts: &variable_plan.commitment_cuts,
        layout,
        penalty_slack_base: variable_plan.penalty_slack_base,
        row_base: current_row,
    })
    .write_into_preallocated(&mut triplets, &mut row_lower, &mut row_upper, current_row);
    for (i, label) in row_labels[current_row..current_row + n_commitment_cut_rows]
        .iter_mut()
        .enumerate()
    {
        *label = format!("commitment_cut_{i}");
    }
    current_row += n_commitment_cut_rows;

    if let Some(explicit_ctg) = variable_plan.explicit_contingency.as_ref() {
        super::rows::build_explicit_contingency_objective_rows(
            super::rows::ScucExplicitContingencyObjectiveRowsInput {
                plan: explicit_ctg,
                spec,
                row_base: current_row,
                base,
            },
        )
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_explicit_contingency_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("explicit_contingency_{i}");
        }
    }
    current_row += n_explicit_contingency_rows;

    if !skip_ph_transitions {
        super::rows::build_pumped_hydro_transition_rows(ScucPumpedHydroTransitionRowsInput {
            ph_mode_units: &row_metadata.ph_mode_units,
            layout,
            n_hours,
            row_base: current_row,
        })
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_pumped_hydro_transition_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("pumped_hydro_transition_{i}");
        }
    }
    current_row += n_pumped_hydro_transition_rows;

    if !skip_cc_rows {
        super::rows::build_cc_rows(ScucCcRowsInput {
            cc_plants: &row_metadata.cc_row_plants,
            layout,
            n_hours,
            row_base: current_row,
        })
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_cc_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("cc_{i}");
        }
    }
    current_row += n_cc_rows;

    if !skip_dr_rows {
        super::rows::build_dr_activation_rows(ScucDrActivationRowsInput {
            activation_loads: &variable_plan.dr_activation_loads,
            layout,
            n_hours,
            row_base: current_row,
        })
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_dl_act_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("dr_activation_{i}");
        }
    }
    current_row += n_dl_act_rows;

    if !skip_dr_rows {
        super::rows::build_dr_rebound_rows(ScucDrReboundRowsInput {
            rebound_loads: &variable_plan.dr_rebound_loads,
            dl_list: &active_inputs.dl_list,
            spec,
            layout,
            n_hours,
            row_base: current_row,
        })
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_dl_rebound_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("dr_rebound_{i}");
        }
    }
    current_row += n_dl_rebound_rows;

    if n_dl_ramp_rows > 0 {
        super::rows::build_dl_ramp_group_rows(ScucDlRampGroupRowsInput {
            groups: &dl_ramp_groups,
            layout,
            spec,
            n_hours,
            row_base: current_row,
        })
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_dl_ramp_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("dl_ramp_{i}");
        }
    }
    current_row += n_dl_ramp_rows;

    // AC branch on/off binaries: state evolution and simultaneous
    // start/stop ban rows.
    let n_branch_state_rows =
        super::rows::branch_state_rows_count(input.network, n_hours, spec.allow_branch_switching);
    if n_branch_state_rows > 0 {
        super::rows::build_branch_state_rows(super::rows::ScucBranchStateRowsInput {
            network: input.network,
            layout,
            n_hours,
            row_base: current_row,
        })
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_branch_state_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("branch_state_{i}");
        }
    }
    current_row += n_branch_state_rows;

    // Big-M switchable branch flow definition: four rows per AC branch
    // per period when `allow_branch_switching = true`, zero otherwise.
    let n_branch_flow_def_rows = super::rows::branch_flow_definition_rows_count(
        input.network,
        n_hours,
        spec.allow_branch_switching,
    );
    if n_branch_flow_def_rows > 0 {
        super::rows::build_branch_flow_definition_rows(
            super::rows::ScucBranchFlowDefinitionRowsInput {
                network: input.network,
                layout,
                bus_map,
                n_hours,
                row_base: current_row,
                base_mva: base,
                big_m_factor: spec.branch_switching_big_m_factor,
            },
        )
        .write_into_preallocated(
            &mut triplets,
            &mut row_lower,
            &mut row_upper,
            current_row,
        );
        for (i, label) in row_labels[current_row..current_row + n_branch_flow_def_rows]
            .iter_mut()
            .enumerate()
        {
            *label = format!("branch_flow_def_{i}");
        }
    }
    current_row += n_branch_flow_def_rows;

    // Accumulated bus-branch connectivity cuts from the SCUC security
    // loop. Each cut emits a single row:
    //   Σ branch_commitment[period, j] ≥ 1  for j in cut_set
    // ensuring that at least one branch in the disconnecting cut set
    // must be back on. The cuts live on the spec so the loop can
    // thread accumulated state across re-solves without touching the
    // problem plan or variable layout.
    let n_connectivity_cut_rows = spec.connectivity_cuts.len();
    if n_connectivity_cut_rows > 0 {
        for (i, cut) in spec.connectivity_cuts.iter().enumerate() {
            let row_index = current_row + i;
            triplets.extend(cut.into_triplets(layout, row_index));
            row_lower[row_index] = 1.0;
            row_upper[row_index] = f64::INFINITY;
            row_labels[row_index] = format!("connectivity_cut_h{}_{}", cut.period, i);
        }
    }
    current_row += n_connectivity_cut_rows;

    // Option C: emit one LP row per compact `ContingencyCut`, drawn
    // from `spec.contingency_cuts`. Each cut has two slack columns
    // allocated in `variable_plan.cut_slack_{lower,upper}_base + k`;
    // the LP row pins
    //     b_m·(θ[from_m] − θ[to_m]) + coef·b_c·(θ[from_c] − θ[to_c])
    //         + σ⁻_k − σ⁺_k  ∈  [−limit_k/base − shift_k, +limit_k/base − shift_k]
    // at period `cut.period`, where `shift_k` folds the constant
    // phase-shifter contribution from both branches. HVDC-legacy and
    // HVDC-banded variants swap the contingency-branch term for
    // HVDC-dispatch-column terms (see
    // `common::builders::build_flowgate_rows` for the parallel
    // Flowgate-path implementation whose math this mirrors).
    let n_cut_rows = spec.contingency_cuts.len();
    if n_cut_rows > 0 {
        use crate::common::contingency::ContingencyCutKind;
        let bus_map = &input.solve.bus_map;
        let theta_off = layout.dispatch.theta;
        let hvdc_off = layout.dispatch.hvdc;
        let n_hvdc_links = setup.n_hvdc_links;
        let cut_slack_lower_base = variable_plan.cut_slack_lower_base;
        let cut_slack_upper_base = variable_plan.cut_slack_upper_base;

        for (cut_idx, cut) in spec.contingency_cuts.iter().enumerate() {
            let period = cut.period as usize;
            let hourly_network = match model_plan.hourly_networks.get(period) {
                Some(net) => net,
                None => continue,
            };
            let monitored_branch = &hourly_network.branches[cut.monitored_branch_idx as usize];
            if !monitored_branch.in_service || monitored_branch.x.abs() < 1e-20 {
                // Row is free but we still consume its slot; emit a
                // vacuous `[-INF, +INF]` row so Gurobi presolves it
                // in O(1).
                let row = current_row + cut_idx;
                row_lower[row] = f64::NEG_INFINITY;
                row_upper[row] = f64::INFINITY;
                row_labels[row] = format!("ctg_cut_{cut_idx}_inactive");
                continue;
            }
            let b_m = monitored_branch.b_dc();
            let from_m_idx = bus_map[&monitored_branch.from_bus];
            let to_m_idx = bus_map[&monitored_branch.to_bus];

            let col_base = layout.hour_col_base(period);
            let row = current_row + cut_idx;

            // Monitored-branch θ terms.
            triplets.push(Triplet {
                row,
                col: col_base + theta_off + from_m_idx,
                val: b_m,
            });
            triplets.push(Triplet {
                row,
                col: col_base + theta_off + to_m_idx,
                val: -b_m,
            });

            // Shift offset (phase-shifter contributions fold into RHS).
            let mut shift_offset = if monitored_branch.phase_shift_rad.abs() > 1e-12 {
                b_m * monitored_branch.phase_shift_rad
            } else {
                0.0
            };

            // Contingency-element terms.
            match cut.contingency_kind {
                ContingencyCutKind::Branch => {
                    let contingency_branch_idx = cut.contingency_idx as usize;
                    let contingency_branch = &hourly_network.branches[contingency_branch_idx];
                    if contingency_branch.in_service && contingency_branch.x.abs() > 1e-20 {
                        let b_c = contingency_branch.b_dc();
                        let from_c_idx = bus_map[&contingency_branch.from_bus];
                        let to_c_idx = bus_map[&contingency_branch.to_bus];
                        let coeff = cut.coefficient * b_c;
                        triplets.push(Triplet {
                            row,
                            col: col_base + theta_off + from_c_idx,
                            val: coeff,
                        });
                        triplets.push(Triplet {
                            row,
                            col: col_base + theta_off + to_c_idx,
                            val: -coeff,
                        });
                        if contingency_branch.phase_shift_rad.abs() > 1e-12 {
                            shift_offset += coeff * contingency_branch.phase_shift_rad;
                        }
                    }
                }
                ContingencyCutKind::HvdcLegacy => {
                    let hvdc_k = cut.contingency_idx as usize;
                    if hvdc_k < n_hvdc_links {
                        let hvdc = &spec.hvdc_links[hvdc_k];
                        let n_bands = hvdc.n_vars();
                        let band_base = setup.hvdc_band_offsets_rel[hvdc_k];
                        for b in 0..n_bands {
                            triplets.push(Triplet {
                                row,
                                col: col_base + hvdc_off + band_base + b,
                                val: cut.coefficient,
                            });
                        }
                    }
                }
                ContingencyCutKind::HvdcBanded => {
                    let hvdc_k = cut.contingency_idx as usize;
                    if hvdc_k < n_hvdc_links {
                        let hvdc = &spec.hvdc_links[hvdc_k];
                        let band_base = setup.hvdc_band_offsets_rel[hvdc_k];
                        let (band_start, band_end) = cut.hvdc_band_range;
                        let band_start = band_start as usize;
                        let band_end = band_end as usize;
                        for &(band_idx, coeff) in
                            &spec.contingency_cut_hvdc_band_coefs[band_start..band_end]
                        {
                            let band_idx = band_idx as usize;
                            if band_idx < hvdc.n_vars() {
                                triplets.push(Triplet {
                                    row,
                                    col: col_base + hvdc_off + band_base + band_idx,
                                    val: coeff,
                                });
                            }
                        }
                    }
                }
            }

            // Slack columns: one pair per cut.
            triplets.push(Triplet {
                row,
                col: cut_slack_lower_base + cut_idx,
                val: 1.0,
            });
            triplets.push(Triplet {
                row,
                col: cut_slack_upper_base + cut_idx,
                val: -1.0,
            });

            let limit_pu = cut.limit_mw / base;
            row_lower[row] = -limit_pu - shift_offset;
            row_upper[row] = limit_pu - shift_offset;
            row_labels[row] = format!(
                "ctg_cut_h{}_{}_{}_mon{}",
                period,
                match cut.contingency_kind {
                    ContingencyCutKind::Branch => "brn",
                    ContingencyCutKind::HvdcLegacy => "hvdc",
                    ContingencyCutKind::HvdcBanded => "hvdc_b",
                },
                cut.contingency_idx,
                cut.monitored_branch_idx
            );
        }
    }
    current_row += n_cut_rows;

    assert_eq!(
        current_row, n_row,
        "row count mismatch: built {current_row}, expected {n_row}"
    );
    tracing::info!(
        stage = "build_problem.post_hourly_rows",
        secs = _post_hourly_t0.elapsed().as_secs_f64(),
        n_triplets = triplets.len(),
        "SCUC build_problem timing"
    );

    let _csc_t0 = std::time::Instant::now();
    let (a_start, a_index, a_value) =
        surge_opf::advanced::triplets_to_csc(&triplets, n_row, variable_plan.n_var);
    tracing::info!(
        stage = "build_problem.triplets_to_csc",
        secs = _csc_t0.elapsed().as_secs_f64(),
        n_triplets = triplets.len(),
        "SCUC build_problem timing"
    );
    ScucProblemBuildState {
        hour_row_bases,
        hour_reserve_row_bases,
        n_row,
        n_branch_flow,
        n_fg_rows,
        n_cc_rows,
        row_lower,
        row_upper,
        row_labels,
        a_start,
        a_index,
        a_value,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_plc_rows(
    network: &Network,
    gen_indices: &[usize],
    layout: &ScucLayout,
    hour: usize,
    n_gen: usize,
    n_bp: usize,
    n_sbp: usize,
    pg_off: usize,
    lam_off: usize,
    sbp_off: usize,
    base: f64,
    triplets: &mut Vec<Triplet<f64>>,
    row_lower: &mut [f64],
    row_upper: &mut [f64],
    row_labels: &mut [String],
    current_row: &mut usize,
) {
    for (gen_idx, &network_gen_idx) in gen_indices.iter().enumerate().take(n_gen) {
        let generator = &network.generators[network_gen_idx];
        let pmin = generator.pmin;
        let pmax = generator.pmax;
        let uses_plc_cost = generator
            .cost
            .as_ref()
            .is_some_and(uses_convex_polynomial_pwl);
        if !uses_plc_cost {
            for row in 0..(2 + n_bp) {
                row_labels[*current_row + row] = format!("h{hour}:plc_unused:g{gen_idx}:row{row}");
            }
            *current_row += 2 + n_bp;
            continue;
        }

        let p_link_row = *current_row;
        triplets.push(Triplet {
            row: p_link_row,
            col: layout.col(hour, pg_off + gen_idx),
            val: 1.0,
        });
        for bp_idx in 0..n_bp {
            let breakpoint_mw = if n_bp > 1 {
                pmin + bp_idx as f64 * (pmax - pmin) / (n_bp - 1) as f64
            } else {
                pmin
            };
            triplets.push(Triplet {
                row: p_link_row,
                col: layout.col(hour, lam_off + gen_idx * n_bp + bp_idx),
                val: -(breakpoint_mw / base),
            });
        }
        row_lower[p_link_row] = 0.0;
        row_upper[p_link_row] = 0.0;
        row_labels[p_link_row] = format!("h{hour}:plc_pg_link:g{gen_idx}");
        *current_row += 1;

        let sum_lam_row = *current_row;
        for bp_idx in 0..n_bp {
            triplets.push(Triplet {
                row: sum_lam_row,
                col: layout.col(hour, lam_off + gen_idx * n_bp + bp_idx),
                val: 1.0,
            });
        }
        triplets.push(Triplet {
            row: sum_lam_row,
            col: layout.commitment_col(hour, gen_idx),
            val: -1.0,
        });
        row_lower[sum_lam_row] = 0.0;
        row_upper[sum_lam_row] = 0.0;
        row_labels[sum_lam_row] = format!("h{hour}:plc_sum_lambda:g{gen_idx}");
        *current_row += 1;

        for bp_idx in 0..n_bp {
            let row = *current_row + bp_idx;
            triplets.push(Triplet {
                row,
                col: layout.col(hour, lam_off + gen_idx * n_bp + bp_idx),
                val: 1.0,
            });
            if bp_idx > 0 && n_sbp > 0 {
                let s_idx = gen_idx * n_sbp + (bp_idx - 1).min(n_sbp - 1);
                triplets.push(Triplet {
                    row,
                    col: layout.col(hour, sbp_off + s_idx),
                    val: -1.0,
                });
            }
            if bp_idx < n_bp - 1 && n_sbp > 0 {
                let s_idx = gen_idx * n_sbp + bp_idx.min(n_sbp - 1);
                triplets.push(Triplet {
                    row,
                    col: layout.col(hour, sbp_off + s_idx),
                    val: -1.0,
                });
            }
            row_lower[row] = -1e30;
            row_upper[row] = 0.0;
            row_labels[row] = format!("h{hour}:plc_sos2:g{gen_idx}:bp{bp_idx}");
        }
        *current_row += n_bp;
    }
}

#[allow(clippy::too_many_arguments)]
fn build_pwl_rows(
    layout: &ScucLayout,
    hour: usize,
    pg_off: usize,
    eg_off: usize,
    pwl_gen_j: &[usize],
    pwl_segments_for_hour: &[Option<Vec<(f64, f64)>>],
    triplets: &mut Vec<Triplet<f64>>,
    row_lower: &mut [f64],
    row_upper: &mut [f64],
    current_row: &mut usize,
) {
    for (pwl_idx, maybe_segments) in pwl_segments_for_hour.iter().enumerate() {
        let Some(segments) = maybe_segments else {
            continue;
        };
        let gen_idx = pwl_gen_j[pwl_idx];
        for &(slope_pu, intercept) in segments {
            // Epigraph row: e_g >= slope_pu * pg + intercept * u_on
            //
            // The u_on gating is essential: without it, when u_on=0 (and thus
            // pg=0 via the commitment bound), the constraint would degenerate
            // to `e_g >= intercept`, forcing the LP to pay no_load_cost (=
            // intercept = first point of the cost curve) for every OFF gen in
            // every period. That made commitment optimization lose $100k+ of
            // incentive on 73-bus cases because the LP charged no_load either
            // way and only saw the *marginal energy* savings from leaving a
            // gen off. The u_on gating here is what makes the epigraph
            // cost == 0 when the gen is off, matching the physical intent.
            //
            // Rewritten as a single LP row:
            //   e_g - slope_pu * pg - intercept * u_on >= 0
            let row = *current_row;
            triplets.push(Triplet {
                row,
                col: layout.col(hour, eg_off + pwl_idx),
                val: 1.0,
            });
            triplets.push(Triplet {
                row,
                col: layout.col(hour, pg_off + gen_idx),
                val: -slope_pu,
            });
            triplets.push(Triplet {
                row,
                col: layout.commitment_col(hour, gen_idx),
                val: -intercept,
            });
            row_lower[row] = 0.0;
            row_upper[row] = 1e30;
            *current_row += 1;
        }
    }
}

pub(super) struct ScucProblemInput<'a> {
    pub solve: &'a DcSolveSession<'a>,
    pub problem: ScucProblemBuildState,
    pub problem_plan: ScucProblemPlan<'a>,
    /// Optional loss-factor warm start seeded by the caller. When
    /// present and `spec.use_loss_factors` is true, `solve_problem`
    /// applies the estimate to the bus-balance rows and injection
    /// coefficients BEFORE the first MIP, so the lossless-MIP
    /// dispatch is avoided and [`iterate_loss_factors`] typically
    /// converges in zero or one inner LP re-solves. Sources: the
    /// security-loop cache (prior iteration's final `dloss_dp`),
    /// a DC PF on a rough dispatch, a load-pattern approximation,
    /// or a uniform loss rate — see `markets::go_c3::policy`
    /// `scuc_warm_start_loss_factors` for the policy knob.
    pub initial_loss_warm_start: Option<crate::scuc::losses::LossFactorWarmStart>,
}

pub(super) struct ScucProblemState<'a> {
    pub problem: ScucProblemBuildState,
    pub problem_plan: ScucProblemPlan<'a>,
    pub is_fixed_commitment: bool,
    pub solution: LpResult,
    pub model_diagnostic: Option<crate::model_diagnostic::ModelDiagnostic>,
    /// Per-bus DC loss allocation (MW) per period: `[t][bus_idx]`.
    /// Empty when loss factors are disabled.
    pub bus_loss_allocation_mw: Vec<Vec<f64>>,
    /// MIP progress trace captured when the caller supplied a
    /// time-varying `mip_gap_schedule` and the backend supported it.
    /// Preserved here so pricing/extract can surface it on the
    /// final diagnostics even after downstream re-solves replace
    /// `solution`.
    pub commitment_mip_trace: Option<MipTrace>,
    /// Final loss-factor state from the refinement iteration. Carries
    /// the per-period `dloss_dp` sensitivities and the per-period
    /// total system losses in MW, computed from the solved theta of
    /// the last LP re-solve. `None` when loss factors are disabled or
    /// the network has ≤ 1 bus. Security-loop callers cache this
    /// across iterations and feed it back in via
    /// [`ScucProblemInput::initial_loss_warm_start`] so the next
    /// SCUC solve can skip the pre-iter warm-start miss.
    pub final_loss_warm_start: Option<crate::scuc::losses::LossFactorWarmStart>,
}

fn apply_commitment_schedule_bounds(
    spec: &DispatchProblemSpec<'_>,
    layout: &ScucLayout,
    n_gen: usize,
    fixed_commit: &[bool],
    fixed_per_period: Option<&[Vec<bool>]>,
    n_hours: usize,
    col_lower: &mut [f64],
    col_upper: &mut [f64],
) {
    let fixed_schedule = fixed_schedule_rows(fixed_commit, fixed_per_period, n_hours);

    for t in 0..n_hours {
        let schedule_t = fixed_schedule.get(t).copied().unwrap_or(fixed_commit);
        for j in 0..n_gen {
            let u_on = schedule_t.get(j).copied().unwrap_or(true);
            let u_val = if u_on { 1.0 } else { 0.0 };

            let u_idx = layout.commitment_col(t, j);
            col_lower[u_idx] = u_val;
            col_upper[u_idx] = u_val;

            let u_prev = if t == 0 {
                spec.initial_commitment_at(j)
                    .map(|ic| if ic { 1.0 } else { 0.0 })
                    .unwrap_or(1.0)
            } else {
                let prev_t = fixed_schedule.get(t - 1).copied().unwrap_or(fixed_commit);
                if prev_t.get(j).copied().unwrap_or(true) {
                    1.0
                } else {
                    0.0
                }
            };

            let v_val = (u_val - u_prev).max(0.0);
            let w_val = (u_prev - u_val).max(0.0);

            let v_idx = layout.startup_col(t, j);
            col_lower[v_idx] = v_val;
            col_upper[v_idx] = v_val;

            let w_idx = layout.shutdown_col(t, j);
            col_lower[w_idx] = w_val;
            col_upper[w_idx] = w_val;
        }
    }
}

fn fixed_schedule_rows<'a>(
    fixed_commit: &'a [bool],
    fixed_per_period: Option<&'a [Vec<bool>]>,
    n_hours: usize,
) -> Vec<&'a [bool]> {
    if let Some(pp) = fixed_per_period {
        pp.iter().map(Vec::as_slice).collect()
    } else {
        std::iter::repeat_n(fixed_commit, n_hours).collect()
    }
}

fn apply_fixed_commitment_bounds(
    spec: &DispatchProblemSpec<'_>,
    layout: &ScucLayout,
    gen_indices: &[usize],
    n_hours: usize,
    col_lower: &mut [f64],
    col_upper: &mut [f64],
) {
    let (fixed_commit, fixed_per_period) = match &spec.commitment {
        CommitmentMode::Fixed {
            commitment,
            per_period,
        } => (commitment.as_slice(), per_period.as_deref()),
        _ => unreachable!(),
    };
    apply_commitment_schedule_bounds(
        spec,
        layout,
        gen_indices.len(),
        fixed_commit,
        fixed_per_period,
        n_hours,
        col_lower,
        col_upper,
    );
}

fn reconcile_fixed_commitment_startup_delta_bounds(
    spec: &DispatchProblemSpec<'_>,
    layout: &ScucLayout,
    startup_plan: &ScucStartupPlan,
    n_hours: usize,
    col_lower: &mut [f64],
    col_upper: &mut [f64],
) {
    let (fixed_commit, fixed_per_period) = match &spec.commitment {
        CommitmentMode::Fixed {
            commitment,
            per_period,
        } => (commitment.as_slice(), per_period.as_deref()),
        _ => unreachable!(),
    };
    let fixed_schedule = fixed_schedule_rows(fixed_commit, fixed_per_period, n_hours);

    for j in 0..startup_plan.startup_tier_capacity.len() {
        let mut prior_on = spec.initial_commitment_at(j).unwrap_or(true);
        for t in 0..n_hours {
            let schedule_t = fixed_schedule.get(t).copied().unwrap_or(fixed_commit);
            let curr_on = schedule_t.get(j).copied().unwrap_or(true);
            let startup_active = !prior_on && curr_on;
            let active_tier_count = startup_plan.gen_tier_info_by_hour[j][t].len();
            for k in 0..startup_plan.startup_tier_capacity[j] {
                let d_idx = layout.col(t, layout.startup_delta + startup_plan.delta_gen_off[j] + k);
                col_lower[d_idx] = 0.0;
                col_upper[d_idx] = if startup_active && k < active_tier_count {
                    1.0
                } else {
                    0.0
                };
            }
            prior_on = curr_on;
        }
    }
}

fn build_mip_warm_start_commitment_schedule(
    spec: &DispatchProblemSpec<'_>,
    hourly_networks: &[Network],
    gen_indices: &[usize],
    layout: &ScucLayout,
    col_lower: &[f64],
    col_upper: &[f64],
    is_must_run_ext: &[bool],
    da_commitment: Option<&[Vec<bool>]>,
    n_hours: usize,
) -> Vec<Vec<bool>> {
    let network = hourly_networks
        .first()
        .expect("SCUC warm-start commitment schedule requires at least one hourly network");
    let n_gen = gen_indices.len();
    let mut schedule = vec![vec![false; n_gen]; n_hours];
    let mut prior_on = vec![false; n_gen];
    let mut state_hours = vec![0.0; n_gen];
    let mut static_on = vec![false; n_gen];
    let mut min_up_hours = vec![0.0; n_gen];
    let mut min_down_hours = vec![0.0; n_gen];

    for (gen_idx, &gi) in gen_indices.iter().enumerate() {
        let generator = &network.generators[gi];
        let initial_on = spec.initial_commitment_at(gen_idx).unwrap_or(true);
        prior_on[gen_idx] = initial_on;
        state_hours[gen_idx] = if initial_on {
            spec.initial_online_hours_at(gen_idx)
                .or_else(|| {
                    generator
                        .commitment
                        .as_ref()
                        .map(|params| params.hours_online)
                })
                .unwrap_or(0.0)
        } else {
            spec.initial_offline_hours_at(gen_idx)
                .or_else(|| {
                    generator
                        .commitment
                        .as_ref()
                        .map(|params| params.hours_offline)
                })
                .unwrap_or(0.0)
        };
        static_on[gen_idx] = generator.is_must_run()
            || is_must_run_ext.get(gen_idx).copied().unwrap_or(false)
            || generator.is_storage();
        min_up_hours[gen_idx] = generator
            .commitment
            .as_ref()
            .and_then(|params| params.min_up_time_hr)
            .unwrap_or(0.0);
        min_down_hours[gen_idx] = generator
            .commitment
            .as_ref()
            .and_then(|params| params.min_down_time_hr)
            .unwrap_or(0.0);
    }

    for (hour, schedule_hour) in schedule.iter_mut().enumerate().take(n_hours) {
        let profiled_network = hourly_networks.get(hour).unwrap_or(network);

        let reserve_target_mw =
            estimated_total_reserve_target_mw(spec, hourly_networks, gen_indices, hour);
        let load_target_mw = estimated_total_demand_target_mw(spec, hourly_networks, hour);
        let largest_generator_mw = gen_indices
            .iter()
            .map(|&gi| profiled_network.generators[gi].pmax.max(0.0))
            .fold(0.0, f64::max);
        let warm_start_capacity_margin_mw = (0.10 * load_target_mw)
            .max(0.25 * largest_generator_mw)
            .max(10.0);
        let required_capacity_mw =
            load_target_mw + reserve_target_mw + warm_start_capacity_margin_mw;

        let mut committed = vec![false; n_gen];
        let mut forced_off = vec![false; n_gen];
        let mut available_capacity_mw = 0.0;

        for gen_idx in 0..n_gen {
            let u_idx = layout.commitment_col(hour, gen_idx);
            let upper = col_upper.get(u_idx).copied().unwrap_or(0.0);
            if upper < 0.5 {
                forced_off[gen_idx] = true;
                continue;
            }

            let generator = &profiled_network.generators[gen_indices[gen_idx]];
            let da_on = da_commitment
                .and_then(|rows| rows.get(hour))
                .and_then(|row| row.get(gen_idx))
                .copied()
                .unwrap_or(false);
            let lower = col_lower.get(u_idx).copied().unwrap_or(0.0);
            let min_up_not_satisfied =
                prior_on[gen_idx] && state_hours[gen_idx] + 1e-9 < min_up_hours[gen_idx];
            let min_down_not_satisfied =
                !prior_on[gen_idx] && state_hours[gen_idx] + 1e-9 < min_down_hours[gen_idx];

            if lower > 0.5 || static_on[gen_idx] || da_on || min_up_not_satisfied {
                committed[gen_idx] = true;
                available_capacity_mw += generator.pmax.max(0.0);
            } else if min_down_not_satisfied {
                forced_off[gen_idx] = true;
            }
        }

        let mut candidates: Vec<usize> = (0..n_gen)
            .filter(|&gen_idx| !committed[gen_idx] && !forced_off[gen_idx])
            .collect();
        candidates.sort_by(|&lhs, &rhs| {
            let lhs_gen = &profiled_network.generators[gen_indices[lhs]];
            let rhs_gen = &profiled_network.generators[gen_indices[rhs]];
            let lhs_merit = generator_warm_start_merit_score(
                spec,
                profiled_network,
                hour,
                gen_indices[lhs],
                prior_on[lhs],
                state_hours[lhs],
            );
            let rhs_merit = generator_warm_start_merit_score(
                spec,
                profiled_network,
                hour,
                gen_indices[rhs],
                prior_on[rhs],
                state_hours[rhs],
            );
            let lhs_pmax = lhs_gen.pmax.max(0.0);
            let rhs_pmax = rhs_gen.pmax.max(0.0);
            let lhs_pmin = lhs_gen.pmin.max(0.0).min(lhs_pmax);
            let rhs_pmin = rhs_gen.pmin.max(0.0).min(rhs_pmax);
            let lhs_headroom = (lhs_pmax - lhs_pmin).max(0.0);
            let rhs_headroom = (rhs_pmax - rhs_pmin).max(0.0);

            prior_on[rhs]
                .cmp(&prior_on[lhs])
                .then_with(|| lhs_merit.partial_cmp(&rhs_merit).unwrap_or(Ordering::Equal))
                .then_with(|| {
                    rhs_headroom
                        .partial_cmp(&lhs_headroom)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| lhs_pmin.partial_cmp(&rhs_pmin).unwrap_or(Ordering::Equal))
                .then_with(|| rhs_pmax.partial_cmp(&lhs_pmax).unwrap_or(Ordering::Equal))
        });

        for gen_idx in candidates {
            if available_capacity_mw + 1e-6 >= required_capacity_mw {
                break;
            }
            let generator = &profiled_network.generators[gen_indices[gen_idx]];
            let additional_capacity_mw = generator.pmax.max(0.0);
            if additional_capacity_mw <= 1e-6 {
                continue;
            }
            committed[gen_idx] = true;
            available_capacity_mw += additional_capacity_mw;
        }

        let dt_hours = spec.period_hours(hour);
        for gen_idx in 0..n_gen {
            if committed[gen_idx] == prior_on[gen_idx] {
                state_hours[gen_idx] += dt_hours;
            } else {
                prior_on[gen_idx] = committed[gen_idx];
                state_hours[gen_idx] = dt_hours;
            }
        }
        *schedule_hour = committed;
    }

    let min_committed = schedule
        .iter()
        .map(|row| row.iter().filter(|&&on| on).count())
        .min()
        .unwrap_or(0);
    let max_committed = schedule
        .iter()
        .map(|row| row.iter().filter(|&&on| on).count())
        .max()
        .unwrap_or(0);
    debug!(
        min_committed,
        max_committed,
        periods = n_hours,
        "SCUC: built warm-start commitment schedule"
    );
    schedule
}

fn build_conservative_mip_warm_start_commitment_schedule(
    spec: &DispatchProblemSpec<'_>,
    hourly_networks: &[Network],
    gen_indices: &[usize],
    layout: &ScucLayout,
    col_upper: &[f64],
    n_hours: usize,
) -> Vec<Vec<bool>> {
    let network = hourly_networks
        .first()
        .expect("SCUC conservative warm-start schedule requires at least one hourly network");
    let n_gen = gen_indices.len();
    let mut schedule = vec![vec![false; n_gen]; n_hours];
    let mut prior_on = vec![false; n_gen];
    let mut state_hours = vec![0.0; n_gen];
    let mut min_up_hours = vec![0.0; n_gen];
    let mut min_down_hours = vec![0.0; n_gen];

    for (gen_idx, &gi) in gen_indices.iter().enumerate() {
        let generator = &network.generators[gi];
        let initial_on = spec.initial_commitment_at(gen_idx).unwrap_or(true);
        prior_on[gen_idx] = initial_on;
        state_hours[gen_idx] = if initial_on {
            spec.initial_online_hours_at(gen_idx)
                .or_else(|| {
                    generator
                        .commitment
                        .as_ref()
                        .map(|params| params.hours_online)
                })
                .unwrap_or(0.0)
        } else {
            spec.initial_offline_hours_at(gen_idx)
                .or_else(|| {
                    generator
                        .commitment
                        .as_ref()
                        .map(|params| params.hours_offline)
                })
                .unwrap_or(0.0)
        };
        min_up_hours[gen_idx] = generator
            .commitment
            .as_ref()
            .and_then(|params| params.min_up_time_hr)
            .unwrap_or(0.0);
        min_down_hours[gen_idx] = generator
            .commitment
            .as_ref()
            .and_then(|params| params.min_down_time_hr)
            .unwrap_or(0.0);
    }

    for (hour, schedule_hour) in schedule.iter_mut().enumerate().take(n_hours) {
        let dt_hours = spec.period_hours(hour);
        for gen_idx in 0..n_gen {
            let u_idx = layout.commitment_col(hour, gen_idx);
            let available = col_upper.get(u_idx).copied().unwrap_or(0.0) >= 0.5;
            let min_up_not_satisfied =
                prior_on[gen_idx] && state_hours[gen_idx] + 1e-9 < min_up_hours[gen_idx];
            let min_down_not_satisfied =
                !prior_on[gen_idx] && state_hours[gen_idx] + 1e-9 < min_down_hours[gen_idx];

            schedule_hour[gen_idx] = if !available {
                false
            } else if min_up_not_satisfied {
                true
            } else {
                !min_down_not_satisfied
            };
        }

        for gen_idx in 0..n_gen {
            if schedule_hour[gen_idx] == prior_on[gen_idx] {
                state_hours[gen_idx] += dt_hours;
            } else {
                prior_on[gen_idx] = schedule_hour[gen_idx];
                state_hours[gen_idx] = dt_hours;
            }
        }
    }

    let min_committed = schedule
        .iter()
        .map(|row| row.iter().filter(|&&on| on).count())
        .min()
        .unwrap_or(0);
    let max_committed = schedule
        .iter()
        .map(|row| row.iter().filter(|&&on| on).count())
        .max()
        .unwrap_or(0);
    debug!(
        min_committed,
        max_committed,
        periods = n_hours,
        "SCUC: built conservative warm-start commitment schedule"
    );
    schedule
}

fn build_network_relaxed_lp_problem(
    prob: &surge_opf::backends::SparseProblem,
    hour_row_bases: &[usize],
    n_flow_rows_per_hour: usize,
) -> surge_opf::backends::SparseProblem {
    let mut reduced_prob = prob.clone();
    reduced_prob.integrality = None;
    if n_flow_rows_per_hour == 0 {
        return reduced_prob;
    }
    for &row_base in hour_row_bases {
        let row_end = (row_base + n_flow_rows_per_hour).min(reduced_prob.n_row);
        for row in row_base..row_end {
            reduced_prob.row_lower[row] = -1e30;
            reduced_prob.row_upper[row] = 1e30;
        }
    }
    reduced_prob
}

fn build_core_commitment_guidance_problem(
    prob: &surge_opf::backends::SparseProblem,
    hour_row_bases: &[usize],
    n_flow_rows_per_hour: usize,
    layout: &ScucLayout,
    n_hours: usize,
) -> surge_opf::backends::SparseProblem {
    let mut reduced_prob =
        build_network_relaxed_lp_problem(prob, hour_row_bases, n_flow_rows_per_hour);
    let Some(integrality) = reduced_prob.integrality.as_mut() else {
        return reduced_prob;
    };
    let vars_per_hour = layout.vars_per_hour();
    let hourly_col_count = vars_per_hour * n_hours;
    for (col, domain) in integrality.iter_mut().enumerate() {
        if !matches!(domain, VariableDomain::Binary | VariableDomain::Integer) {
            continue;
        }
        let keep_integer = if col < hourly_col_count {
            let local = col % vars_per_hour;
            local >= layout.commitment && local < layout.startup_delta
        } else {
            false
        };
        if !keep_integer {
            *domain = VariableDomain::Continuous;
        }
    }
    reduced_prob
}

#[allow(clippy::too_many_arguments)]
fn build_relaxed_guided_mip_warm_start_commitment_schedule(
    spec: &DispatchProblemSpec<'_>,
    hourly_networks: &[Network],
    gen_indices: &[usize],
    layout: &ScucLayout,
    col_lower: &[f64],
    col_upper: &[f64],
    is_must_run_ext: &[bool],
    da_commitment: Option<&[Vec<bool>]>,
    relaxed_solution: &LpResult,
    n_hours: usize,
) -> Vec<Vec<bool>> {
    let network = hourly_networks
        .first()
        .expect("SCUC relaxed warm-start schedule requires at least one hourly network");
    let n_gen = gen_indices.len();
    let mut schedule = vec![vec![false; n_gen]; n_hours];
    let mut prior_on = vec![false; n_gen];
    let mut state_hours = vec![0.0; n_gen];
    let mut static_on = vec![false; n_gen];
    let mut min_up_hours = vec![0.0; n_gen];
    let mut min_down_hours = vec![0.0; n_gen];

    for (gen_idx, &gi) in gen_indices.iter().enumerate() {
        let generator = &network.generators[gi];
        let initial_on = spec.initial_commitment_at(gen_idx).unwrap_or(true);
        prior_on[gen_idx] = initial_on;
        state_hours[gen_idx] = if initial_on {
            spec.initial_online_hours_at(gen_idx)
                .or_else(|| {
                    generator
                        .commitment
                        .as_ref()
                        .map(|params| params.hours_online)
                })
                .unwrap_or(0.0)
        } else {
            spec.initial_offline_hours_at(gen_idx)
                .or_else(|| {
                    generator
                        .commitment
                        .as_ref()
                        .map(|params| params.hours_offline)
                })
                .unwrap_or(0.0)
        };
        static_on[gen_idx] = generator.is_must_run()
            || is_must_run_ext.get(gen_idx).copied().unwrap_or(false)
            || generator.is_storage();
        min_up_hours[gen_idx] = generator
            .commitment
            .as_ref()
            .and_then(|params| params.min_up_time_hr)
            .unwrap_or(0.0);
        min_down_hours[gen_idx] = generator
            .commitment
            .as_ref()
            .and_then(|params| params.min_down_time_hr)
            .unwrap_or(0.0);
    }

    for (hour, schedule_hour) in schedule.iter_mut().enumerate().take(n_hours) {
        let profiled_network = hourly_networks.get(hour).unwrap_or(network);
        let reserve_target_mw =
            estimated_total_reserve_target_mw(spec, hourly_networks, gen_indices, hour);
        let load_target_mw = estimated_total_demand_target_mw(spec, hourly_networks, hour);
        let largest_generator_mw = gen_indices
            .iter()
            .map(|&gi| profiled_network.generators[gi].pmax.max(0.0))
            .fold(0.0, f64::max);
        let warm_start_capacity_margin_mw = (0.10 * load_target_mw)
            .max(0.25 * largest_generator_mw)
            .max(10.0);
        let required_capacity_mw =
            load_target_mw + reserve_target_mw + warm_start_capacity_margin_mw;

        let mut committed = vec![false; n_gen];
        let mut forced_off = vec![false; n_gen];
        let mut available_capacity_mw = 0.0;

        for gen_idx in 0..n_gen {
            let u_idx = layout.commitment_col(hour, gen_idx);
            let upper = col_upper.get(u_idx).copied().unwrap_or(0.0);
            if upper < 0.5 {
                forced_off[gen_idx] = true;
                continue;
            }

            let generator = &profiled_network.generators[gen_indices[gen_idx]];
            let da_on = da_commitment
                .and_then(|rows| rows.get(hour))
                .and_then(|row| row.get(gen_idx))
                .copied()
                .unwrap_or(false);
            let lower = col_lower.get(u_idx).copied().unwrap_or(0.0);
            let min_up_not_satisfied =
                prior_on[gen_idx] && state_hours[gen_idx] + 1e-9 < min_up_hours[gen_idx];
            let min_down_not_satisfied =
                !prior_on[gen_idx] && state_hours[gen_idx] + 1e-9 < min_down_hours[gen_idx];
            let relaxed_u = relaxed_solution
                .x
                .get(u_idx)
                .copied()
                .unwrap_or(lower)
                .clamp(0.0, 1.0);

            if lower > 0.5
                || static_on[gen_idx]
                || da_on
                || min_up_not_satisfied
                || relaxed_u >= 0.5
            {
                committed[gen_idx] = true;
                available_capacity_mw += generator.pmax.max(0.0);
            } else if min_down_not_satisfied {
                forced_off[gen_idx] = true;
            }
        }

        let mut candidates: Vec<usize> = (0..n_gen)
            .filter(|&gen_idx| !committed[gen_idx] && !forced_off[gen_idx])
            .collect();
        candidates.sort_by(|&lhs, &rhs| {
            let lhs_gen = &profiled_network.generators[gen_indices[lhs]];
            let rhs_gen = &profiled_network.generators[gen_indices[rhs]];
            let lhs_merit = generator_warm_start_merit_score(
                spec,
                profiled_network,
                hour,
                gen_indices[lhs],
                prior_on[lhs],
                state_hours[lhs],
            );
            let rhs_merit = generator_warm_start_merit_score(
                spec,
                profiled_network,
                hour,
                gen_indices[rhs],
                prior_on[rhs],
                state_hours[rhs],
            );
            let lhs_u = relaxed_solution
                .x
                .get(layout.commitment_col(hour, lhs))
                .copied()
                .unwrap_or(0.0);
            let rhs_u = relaxed_solution
                .x
                .get(layout.commitment_col(hour, rhs))
                .copied()
                .unwrap_or(0.0);
            let lhs_pmax = lhs_gen.pmax.max(0.0);
            let rhs_pmax = rhs_gen.pmax.max(0.0);
            let lhs_pmin = lhs_gen.pmin.max(0.0).min(lhs_pmax);
            let rhs_pmin = rhs_gen.pmin.max(0.0).min(rhs_pmax);
            let lhs_headroom = (lhs_pmax - lhs_pmin).max(0.0);
            let rhs_headroom = (rhs_pmax - rhs_pmin).max(0.0);

            rhs_u
                .partial_cmp(&lhs_u)
                .unwrap_or(Ordering::Equal)
                .then_with(|| prior_on[rhs].cmp(&prior_on[lhs]))
                .then_with(|| lhs_merit.partial_cmp(&rhs_merit).unwrap_or(Ordering::Equal))
                .then_with(|| {
                    rhs_headroom
                        .partial_cmp(&lhs_headroom)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| lhs_pmin.partial_cmp(&rhs_pmin).unwrap_or(Ordering::Equal))
                .then_with(|| rhs_pmax.partial_cmp(&lhs_pmax).unwrap_or(Ordering::Equal))
        });

        for gen_idx in candidates {
            if available_capacity_mw + 1e-6 >= required_capacity_mw {
                break;
            }
            let generator = &profiled_network.generators[gen_indices[gen_idx]];
            let additional_capacity_mw = generator.pmax.max(0.0);
            if additional_capacity_mw <= 1e-6 {
                continue;
            }
            committed[gen_idx] = true;
            available_capacity_mw += additional_capacity_mw;
        }

        let dt_hours = spec.period_hours(hour);
        for gen_idx in 0..n_gen {
            if committed[gen_idx] == prior_on[gen_idx] {
                state_hours[gen_idx] += dt_hours;
            } else {
                prior_on[gen_idx] = committed[gen_idx];
                state_hours[gen_idx] = dt_hours;
            }
        }
        *schedule_hour = committed;
    }

    let min_committed = schedule
        .iter()
        .map(|row| row.iter().filter(|&&on| on).count())
        .min()
        .unwrap_or(0);
    let max_committed = schedule
        .iter()
        .map(|row| row.iter().filter(|&&on| on).count())
        .max()
        .unwrap_or(0);
    debug!(
        min_committed,
        max_committed,
        periods = n_hours,
        "SCUC: built relaxed-guided warm-start commitment schedule"
    );
    schedule
}

struct WarmStartArtifacts {
    warm_prob: surge_opf::backends::SparseProblem,
    sparse_primal_start: LpPrimalStart,
    total_integer_vars: usize,
    assigned_integer_vars: usize,
    fixed_integer_assignments: usize,
    commitment_assignments: usize,
}

#[derive(Default)]
struct MipWarmStart {
    primal_start: Option<LpPrimalStart>,
    verified_dense_incumbent: Option<LpResult>,
}

fn choose_better_mip_warm_start(
    current: Option<MipWarmStart>,
    candidate: MipWarmStart,
) -> Option<MipWarmStart> {
    let candidate_verified = candidate.verified_dense_incumbent.as_ref();
    match current {
        None => Some(candidate),
        Some(existing) => {
            let existing_verified = existing.verified_dense_incumbent.as_ref();
            match (existing_verified, candidate_verified) {
                (Some(existing_lp), Some(candidate_lp)) => {
                    if candidate_lp.objective + 1e-6 < existing_lp.objective {
                        Some(candidate)
                    } else {
                        Some(existing)
                    }
                }
                (None, Some(_)) => Some(candidate),
                (Some(_), None) => Some(existing),
                (None, None) => Some(existing),
            }
        }
    }
}

fn overlay_exact_commitment_warm_start_schedule(
    spec: &DispatchProblemSpec<'_>,
    base_schedule: &[Vec<bool>],
    n_gen: usize,
    n_hours: usize,
) -> Option<Vec<Vec<bool>>> {
    let mut seeded = base_schedule.to_vec();
    let mut any_seeded = false;
    for (t, row) in seeded.iter_mut().enumerate().take(n_hours) {
        for (j, value) in row.iter_mut().enumerate().take(n_gen) {
            if let Some(seed) = spec.warm_start_commitment_at(t, j) {
                *value = seed;
                any_seeded = true;
            }
        }
    }
    any_seeded.then_some(seeded)
}

/// True when the caller's commitment options carry a non-empty
/// `warm_start_commitment` schedule we can materialize into a light
/// sparse primal start.
fn has_warm_start_commitment(spec: &DispatchProblemSpec<'_>, n_gen: usize, n_hours: usize) -> bool {
    for t in 0..n_hours {
        for j in 0..n_gen {
            if spec.warm_start_commitment_at(t, j).is_some() {
                return true;
            }
        }
    }
    false
}

/// Materialize the caller-supplied `warm_start_commitment` sparse
/// mask into a dense `[t][j] -> bool` schedule. Missing entries
/// default to the initial commitment hint (if any) or true (on).
fn collect_warm_start_commitment_schedule(
    spec: &DispatchProblemSpec<'_>,
    n_gen: usize,
    n_hours: usize,
) -> Vec<Vec<bool>> {
    let mut out: Vec<Vec<bool>> = Vec::with_capacity(n_hours);
    for t in 0..n_hours {
        let mut row = Vec::with_capacity(n_gen);
        for j in 0..n_gen {
            let hint = spec
                .warm_start_commitment_at(t, j)
                .or_else(|| spec.initial_commitment_at(j))
                .unwrap_or(true);
            row.push(hint);
        }
        out.push(row);
    }
    out
}

fn build_commitment_sparse_primal_start_from_schedule(
    spec: &DispatchProblemSpec<'_>,
    layout: &ScucLayout,
    schedule: &[Vec<bool>],
    n_gen: usize,
    n_hours: usize,
) -> LpPrimalStart {
    let mut indices = Vec::with_capacity(3 * n_gen * n_hours);
    let mut values = Vec::with_capacity(3 * n_gen * n_hours);
    for t in 0..n_hours {
        for j in 0..n_gen {
            let u_on = schedule
                .get(t)
                .and_then(|row| row.get(j))
                .copied()
                .unwrap_or(false);
            let u_prev = if t == 0 {
                spec.initial_commitment_at(j).unwrap_or(true)
            } else {
                schedule
                    .get(t - 1)
                    .and_then(|row| row.get(j))
                    .copied()
                    .unwrap_or(false)
            };
            let assignments = [
                (layout.commitment_col(t, j), if u_on { 1.0 } else { 0.0 }),
                (
                    layout.startup_col(t, j),
                    if u_on && !u_prev { 1.0 } else { 0.0 },
                ),
                (
                    layout.shutdown_col(t, j),
                    if !u_on && u_prev { 1.0 } else { 0.0 },
                ),
            ];
            for (col, value) in assignments {
                indices.push(col);
                values.push(value);
            }
        }
    }
    LpPrimalStart::Sparse { indices, values }
}

#[allow(clippy::too_many_arguments)]
fn build_schedule_warm_start_artifacts(
    prob: &surge_opf::backends::SparseProblem,
    hourly_networks: &[Network],
    spec: &DispatchProblemSpec<'_>,
    layout: &ScucLayout,
    gen_indices: &[usize],
    schedule: &[Vec<bool>],
    use_plc: bool,
    n_bp: usize,
    n_sbp: usize,
    n_hours: usize,
) -> WarmStartArtifacts {
    let network = hourly_networks
        .first()
        .expect("SCUC warm-start helper requires at least one hourly network");
    let mut start_by_col = vec![None; prob.n_col];
    let mut assigned_integer_vars = 0usize;
    let mut fixed_integer_assignments = 0usize;
    let total_integer_vars = prob
        .integrality
        .as_ref()
        .map(|integrality| {
            integrality
                .iter()
                .filter(|domain| matches!(domain, VariableDomain::Binary | VariableDomain::Integer))
                .count()
        })
        .unwrap_or(0);
    if let Some(integrality) = prob.integrality.as_ref() {
        for (col, domain) in integrality.iter().enumerate() {
            if !matches!(domain, VariableDomain::Binary | VariableDomain::Integer) {
                continue;
            }
            let lo = prob.col_lower.get(col).copied().unwrap_or(0.0);
            let hi = prob.col_upper.get(col).copied().unwrap_or(0.0);
            if !lo.is_finite() || !hi.is_finite() {
                continue;
            }
            let value = match domain {
                VariableDomain::Binary => {
                    if hi <= 0.5 {
                        0.0
                    } else if lo >= 0.5 {
                        1.0
                    } else {
                        0.0
                    }
                }
                VariableDomain::Integer => {
                    let mut value = if lo <= 0.0 && 0.0 <= hi {
                        0.0
                    } else {
                        lo.ceil()
                    };
                    if value > hi {
                        value = hi.floor();
                    }
                    value
                }
                VariableDomain::Continuous => continue,
            };
            start_by_col[col] = Some(value);
            assigned_integer_vars += 1;
            if (lo - hi).abs() <= 1e-9 {
                fixed_integer_assignments += 1;
            }
        }
    }

    let mut commitment_assignments = 0usize;
    for t in 0..n_hours {
        for j in 0..gen_indices.len() {
            let u_on = schedule
                .get(t)
                .and_then(|row| row.get(j))
                .copied()
                .unwrap_or(false);
            let u_prev = if t == 0 {
                spec.initial_commitment_at(j).unwrap_or(true)
            } else {
                schedule
                    .get(t - 1)
                    .and_then(|row| row.get(j))
                    .copied()
                    .unwrap_or(false)
            };
            let u_val = if u_on { 1.0 } else { 0.0 };
            let v_val = if u_on && !u_prev { 1.0 } else { 0.0 };
            let w_val = if !u_on && u_prev { 1.0 } else { 0.0 };

            for (col, value) in [
                (layout.commitment_col(t, j), u_val),
                (layout.startup_col(t, j), v_val),
                (layout.shutdown_col(t, j), w_val),
            ] {
                start_by_col[col] = Some(value);
                commitment_assignments += 1;
            }

            if !u_on {
                start_by_col[layout.pg_col(t, j)] = Some(0.0);
                if use_plc {
                    for bp_idx in 0..n_bp {
                        start_by_col[layout.col(t, layout.plc_lambda + j * n_bp + bp_idx)] =
                            Some(0.0);
                    }
                    for sbp_idx in 0..n_sbp {
                        start_by_col[layout.col(t, layout.plc_sos2_binary + j * n_sbp + sbp_idx)] =
                            Some(0.0);
                    }
                }
            }
        }
    }

    let base_mva = network.base_mva;
    for hour in 0..n_hours {
        let profiled_network = hourly_networks.get(hour).unwrap_or(network);

        let committed: Vec<usize> = schedule
            .get(hour)
            .into_iter()
            .flat_map(|row| {
                row.iter()
                    .enumerate()
                    .filter_map(|(gen_idx, &on)| on.then_some(gen_idx))
            })
            .collect();
        if committed.is_empty() {
            continue;
        }

        let mut pg_targets_mw = vec![0.0; gen_indices.len()];
        let mut total_pmin_mw = 0.0;
        let mut total_headroom_mw = 0.0;
        for &gen_idx in &committed {
            let generator = &profiled_network.generators[gen_indices[gen_idx]];
            let pmax = generator.pmax.max(0.0);
            let pmin = generator.pmin.max(0.0).min(pmax);
            pg_targets_mw[gen_idx] = pmin;
            total_pmin_mw += pmin;
            total_headroom_mw += (pmax - pmin).max(0.0);
        }

        let target_dispatch_mw = estimated_total_demand_target_mw(spec, hourly_networks, hour);
        let residual_dispatch_mw = (target_dispatch_mw - total_pmin_mw).max(0.0);
        let mut remaining_dispatch_mw = residual_dispatch_mw;
        for &gen_idx in &committed {
            if remaining_dispatch_mw <= 1e-9 || total_headroom_mw <= 1e-9 {
                break;
            }
            let generator = &profiled_network.generators[gen_indices[gen_idx]];
            let pmax = generator.pmax.max(0.0);
            let pmin = generator.pmin.max(0.0).min(pmax);
            let headroom = (pmax - pmin).max(0.0);
            if headroom <= 1e-9 {
                continue;
            }
            let share = residual_dispatch_mw * (headroom / total_headroom_mw);
            let add_mw = share.min(headroom).min(remaining_dispatch_mw);
            pg_targets_mw[gen_idx] += add_mw;
            remaining_dispatch_mw -= add_mw;
        }
        if remaining_dispatch_mw > 1e-6 {
            for &gen_idx in &committed {
                if remaining_dispatch_mw <= 1e-9 {
                    break;
                }
                let generator = &profiled_network.generators[gen_indices[gen_idx]];
                let pmax = generator.pmax.max(0.0);
                let spare = (pmax - pg_targets_mw[gen_idx]).max(0.0);
                if spare <= 1e-9 {
                    continue;
                }
                let add_mw = spare.min(remaining_dispatch_mw);
                pg_targets_mw[gen_idx] += add_mw;
                remaining_dispatch_mw -= add_mw;
            }
        }

        for &gen_idx in &committed {
            let generator = &profiled_network.generators[gen_indices[gen_idx]];
            let pmax = generator.pmax.max(0.0);
            let pmin = generator.pmin.max(0.0).min(pmax);
            let pg_mw = pg_targets_mw[gen_idx].clamp(pmin, pmax);
            start_by_col[layout.pg_col(hour, gen_idx)] = Some(pg_mw / base_mva);

            if !use_plc
                || n_bp == 0
                || !generator
                    .cost
                    .as_ref()
                    .is_some_and(uses_convex_polynomial_pwl)
            {
                continue;
            }

            for bp_idx in 0..n_bp {
                start_by_col[layout.col(hour, layout.plc_lambda + gen_idx * n_bp + bp_idx)] =
                    Some(0.0);
            }
            for sbp_idx in 0..n_sbp {
                start_by_col
                    [layout.col(hour, layout.plc_sos2_binary + gen_idx * n_sbp + sbp_idx)] =
                    Some(0.0);
            }

            if n_bp == 1 || (pmax - pmin).abs() <= 1e-9 {
                start_by_col[layout.col(hour, layout.plc_lambda + gen_idx * n_bp)] = Some(1.0);
                continue;
            }

            let step_mw = (pmax - pmin) / (n_bp - 1) as f64;
            if step_mw <= 1e-9 {
                start_by_col[layout.col(hour, layout.plc_lambda + gen_idx * n_bp)] = Some(1.0);
                continue;
            }

            let breakpoint_pos = ((pg_mw - pmin) / step_mw).clamp(0.0, (n_bp - 1) as f64);
            let left_bp = breakpoint_pos.floor() as usize;
            let frac = breakpoint_pos - left_bp as f64;
            let lambda_base = layout.plc_lambda + gen_idx * n_bp;

            if left_bp >= n_bp - 1 || frac <= 1e-9 {
                let bp_idx = left_bp.min(n_bp - 1);
                start_by_col[layout.col(hour, lambda_base + bp_idx)] = Some(1.0);
                if n_sbp > 0 {
                    let sbp_idx = if bp_idx == 0 {
                        0
                    } else {
                        (bp_idx - 1).min(n_sbp - 1)
                    };
                    start_by_col
                        [layout.col(hour, layout.plc_sos2_binary + gen_idx * n_sbp + sbp_idx)] =
                        Some(1.0);
                }
            } else {
                start_by_col[layout.col(hour, lambda_base + left_bp)] = Some(1.0 - frac);
                start_by_col[layout.col(hour, lambda_base + left_bp + 1)] = Some(frac);
                if n_sbp > 0 {
                    let sbp_idx = left_bp.min(n_sbp - 1);
                    start_by_col
                        [layout.col(hour, layout.plc_sos2_binary + gen_idx * n_sbp + sbp_idx)] =
                        Some(1.0);
                }
            }
        }
    }

    let mut warm_prob = prob.clone();
    let fixed_commit = schedule.first().cloned().unwrap_or_default();
    apply_commitment_schedule_bounds(
        spec,
        layout,
        gen_indices.len(),
        fixed_commit.as_slice(),
        Some(schedule),
        n_hours,
        &mut warm_prob.col_lower,
        &mut warm_prob.col_upper,
    );
    warm_prob.integrality = None;
    let sparse_primal_start = {
        let mut indices = Vec::new();
        let mut values = Vec::new();
        for (col, value) in start_by_col.iter().enumerate() {
            if let Some(value) = value {
                indices.push(col);
                values.push(*value);
            }
        }
        LpPrimalStart::Sparse { indices, values }
    };

    WarmStartArtifacts {
        warm_prob,
        sparse_primal_start,
        total_integer_vars,
        assigned_integer_vars,
        fixed_integer_assignments,
        commitment_assignments,
    }
}

fn try_build_dense_primal_start_from_artifacts(
    solver: &dyn surge_opf::backends::LpSolver,
    spec: &DispatchProblemSpec<'_>,
    helper_time_limit: Option<f64>,
    artifacts: &WarmStartArtifacts,
    trace_label: &str,
) -> Option<Vec<f64>> {
    const MAX_APPROX_DENSE_START_PRIMAL_VIOLATION: f64 = 1.0;
    fn polish_dense_start_candidate(
        solver: &dyn surge_opf::backends::LpSolver,
        spec: &DispatchProblemSpec<'_>,
        helper_time_limit: Option<f64>,
        artifacts: &WarmStartArtifacts,
        trace_label: &str,
        dense_start: Vec<f64>,
    ) -> Option<Vec<f64>> {
        let polish_time_limit = match normalize_time_limit_secs(helper_time_limit) {
            Some(limit) => Some((limit * 0.35).clamp(1.0, 15.0).min(limit)),
            None => Some(5.0),
        };
        for (polish_label, primal_start) in [
            ("polished", Some(LpPrimalStart::Dense(dense_start.clone()))),
            ("polished_cold", None),
        ] {
            let _scuc_helper_t0 = Instant::now();
            let _scuc_helper_stage = format!("warm_start.polish.{trace_label}.{polish_label}");
            let _scuc_helper_result = solve_sparse_problem_with_start_and_algorithm(
                solver,
                &artifacts.warm_prob,
                spec.tolerance,
                polish_time_limit,
                spec.mip_rel_gap(),
                primal_start,
                LpAlgorithm::Simplex,
            );
            info!(
                stage = %_scuc_helper_stage,
                secs = _scuc_helper_t0.elapsed().as_secs_f64(),
                "SCUC helper solve timing"
            );
            match _scuc_helper_result {
                Ok(polished_solution)
                    if matches!(
                        polished_solution.status,
                        LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
                    ) =>
                {
                    let max_primal_violation = max_sparse_problem_primal_violation(
                        &artifacts.warm_prob,
                        &polished_solution.x,
                    );
                    let acceptance_tol = (spec.tolerance * 1e4).max(1e-4);
                    if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                        log_scuc_problem_trace(format!(
                            "scuc_warm_start_trace label={} {}_status={:?} {}_vars={} {}_max_primal_violation={:.6e} {}_acceptance_tol={:.6e}",
                            trace_label,
                            polish_label,
                            polished_solution.status,
                            polish_label,
                            polished_solution.x.len(),
                            polish_label,
                            max_primal_violation,
                            polish_label,
                            acceptance_tol,
                        ));
                    }
                    if max_primal_violation <= acceptance_tol {
                        return Some(polished_solution.x);
                    }
                }
                Ok(polished_solution) => {
                    if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                        log_scuc_problem_trace(format!(
                            "scuc_warm_start_trace label={} {}_rejected_status={:?}",
                            trace_label, polish_label, polished_solution.status
                        ));
                    }
                }
                Err(err) => {
                    debug!(
                        error = %err,
                        trace_label,
                        polish_label,
                        "SCUC: dense LP warm-start polish solve failed"
                    );
                }
            }
        }
        None
    }

    let mut best_dense_candidate: Option<(f64, Vec<f64>)> = None;
    let mut record_candidate = |max_primal_violation: f64, x: &[f64]| {
        if !max_primal_violation.is_finite() {
            return;
        }
        let should_replace = best_dense_candidate
            .as_ref()
            .is_none_or(|(best_violation, _)| max_primal_violation < *best_violation);
        if should_replace {
            best_dense_candidate = Some((max_primal_violation, x.to_vec()));
        }
    };

    let prefer_cold_first = artifacts.warm_prob.n_col >= 100_000;
    // Warm-start helper LPs are reoptimization problems, so simplex is a
    // better default than interior-point once the model gets large.
    let helper_algorithm = if prefer_cold_first {
        LpAlgorithm::Simplex
    } else {
        LpAlgorithm::Auto
    };

    if prefer_cold_first {
        let _scuc_helper_t0 = Instant::now();
        let _scuc_helper_result = solve_sparse_problem_with_start_and_algorithm(
            solver,
            &artifacts.warm_prob,
            spec.tolerance,
            helper_time_limit,
            spec.mip_rel_gap(),
            None,
            helper_algorithm,
        );
        info!(
            stage = format!("warm_start.cold_dense.{trace_label}"),
            secs = _scuc_helper_t0.elapsed().as_secs_f64(),
            "SCUC helper solve timing"
        );
        match _scuc_helper_result {
            Ok(warm_solution)
                if matches!(
                    warm_solution.status,
                    LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
                ) =>
            {
                let max_primal_violation =
                    max_sparse_problem_primal_violation(&artifacts.warm_prob, &warm_solution.x);
                let acceptance_tol = (spec.tolerance * 1e4).max(1e-4);
                debug!(
                    status = ?warm_solution.status,
                    vars = warm_solution.x.len(),
                    max_primal_violation,
                    trace_label,
                    "SCUC: built dense LP warm-start incumbent from cold helper solve"
                );
                if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                    log_scuc_problem_trace(format!(
                        "scuc_warm_start_trace label={} cold_dense_status={:?} cold_dense_vars={} cold_dense_max_primal_violation={:.6e} cold_dense_acceptance_tol={:.6e}",
                        trace_label,
                        warm_solution.status,
                        warm_solution.x.len(),
                        max_primal_violation,
                        acceptance_tol,
                    ));
                }
                record_candidate(max_primal_violation, &warm_solution.x);
                if max_primal_violation <= acceptance_tol {
                    return Some(warm_solution.x);
                }
            }
            Ok(warm_solution) => {
                if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                    log_scuc_problem_trace(format!(
                        "scuc_warm_start_trace label={} cold_dense_rejected_status={:?}",
                        trace_label, warm_solution.status
                    ));
                }
            }
            Err(err) => {
                debug!(error = %err, trace_label, "SCUC: cold dense LP warm-start solve failed");
            }
        }
    }

    let _scuc_helper_t0 = Instant::now();
    let _scuc_helper_result = solve_sparse_problem_with_start_and_algorithm(
        solver,
        &artifacts.warm_prob,
        spec.tolerance,
        helper_time_limit,
        spec.mip_rel_gap(),
        Some(artifacts.sparse_primal_start.clone()),
        helper_algorithm,
    );
    info!(
        stage = format!("warm_start.dense_seeded.{trace_label}"),
        secs = _scuc_helper_t0.elapsed().as_secs_f64(),
        "SCUC helper solve timing"
    );
    match _scuc_helper_result {
        Ok(warm_solution)
            if matches!(
                warm_solution.status,
                LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
            ) =>
        {
            let max_primal_violation =
                max_sparse_problem_primal_violation(&artifacts.warm_prob, &warm_solution.x);
            let acceptance_tol = (spec.tolerance * 1e4).max(1e-4);
            debug!(
                status = ?warm_solution.status,
                vars = warm_solution.x.len(),
                max_primal_violation,
                trace_label,
                "SCUC: built dense LP warm-start incumbent"
            );
            if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                log_scuc_problem_trace(format!(
                    "scuc_warm_start_trace label={} dense_status={:?} dense_vars={} dense_max_primal_violation={:.6e} dense_acceptance_tol={:.6e}",
                    trace_label,
                    warm_solution.status,
                    warm_solution.x.len(),
                    max_primal_violation,
                    acceptance_tol,
                ));
            }
            record_candidate(max_primal_violation, &warm_solution.x);
            if max_primal_violation <= acceptance_tol {
                return Some(warm_solution.x);
            }
        }
        Ok(warm_solution) => {
            if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                log_scuc_problem_trace(format!(
                    "scuc_warm_start_trace label={} dense_rejected_status={:?}",
                    trace_label, warm_solution.status
                ));
            }
        }
        Err(err) => {
            debug!(error = %err, trace_label, "SCUC: dense LP warm-start solve failed");
        }
    }

    if prefer_cold_first {
        if let Some((best_violation, dense_start)) = best_dense_candidate {
            if best_violation <= MAX_APPROX_DENSE_START_PRIMAL_VIOLATION {
                if let Some(polished_start) = polish_dense_start_candidate(
                    solver,
                    spec,
                    helper_time_limit,
                    artifacts,
                    trace_label,
                    dense_start.clone(),
                ) {
                    return Some(polished_start);
                }
                if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                    log_scuc_problem_trace(format!(
                        "scuc_warm_start_trace label={} approximate_dense_start max_primal_violation={:.6e}",
                        trace_label, best_violation
                    ));
                }
                return Some(dense_start);
            }
        }
        return None;
    }

    let _scuc_helper_t0 = Instant::now();
    let _scuc_helper_result = solve_sparse_problem_with_start_and_algorithm(
        solver,
        &artifacts.warm_prob,
        spec.tolerance,
        helper_time_limit,
        spec.mip_rel_gap(),
        None,
        helper_algorithm,
    );
    info!(
        stage = format!("warm_start.cold_dense_fallback.{trace_label}"),
        secs = _scuc_helper_t0.elapsed().as_secs_f64(),
        "SCUC helper solve timing"
    );
    match _scuc_helper_result {
        Ok(warm_solution)
            if matches!(
                warm_solution.status,
                LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
            ) =>
        {
            let max_primal_violation =
                max_sparse_problem_primal_violation(&artifacts.warm_prob, &warm_solution.x);
            let acceptance_tol = (spec.tolerance * 1e4).max(1e-4);
            debug!(
                status = ?warm_solution.status,
                vars = warm_solution.x.len(),
                max_primal_violation,
                trace_label,
                "SCUC: built dense LP warm-start incumbent from cold helper solve"
            );
            if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                log_scuc_problem_trace(format!(
                    "scuc_warm_start_trace label={} cold_dense_status={:?} cold_dense_vars={} cold_dense_max_primal_violation={:.6e} cold_dense_acceptance_tol={:.6e}",
                    trace_label,
                    warm_solution.status,
                    warm_solution.x.len(),
                    max_primal_violation,
                    acceptance_tol,
                ));
            }
            record_candidate(max_primal_violation, &warm_solution.x);
            if max_primal_violation <= acceptance_tol {
                return Some(warm_solution.x);
            }
        }
        Ok(warm_solution) => {
            if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                log_scuc_problem_trace(format!(
                    "scuc_warm_start_trace label={} cold_dense_rejected_status={:?}",
                    trace_label, warm_solution.status
                ));
            }
        }
        Err(err) => {
            debug!(error = %err, trace_label, "SCUC: cold dense LP warm-start solve failed");
        }
    }

    if let Some((best_violation, dense_start)) = best_dense_candidate {
        if best_violation <= MAX_APPROX_DENSE_START_PRIMAL_VIOLATION {
            if let Some(polished_start) = polish_dense_start_candidate(
                solver,
                spec,
                helper_time_limit,
                artifacts,
                trace_label,
                dense_start.clone(),
            ) {
                return Some(polished_start);
            }
            if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                log_scuc_problem_trace(format!(
                    "scuc_warm_start_trace label={} approximate_dense_start max_primal_violation={:.6e}",
                    trace_label, best_violation
                ));
            }
            return Some(dense_start);
        }
    }

    None
}

#[allow(clippy::too_many_arguments)]
fn try_build_mip_primal_start(
    solver: &dyn surge_opf::backends::LpSolver,
    prob: &surge_opf::backends::SparseProblem,
    hour_row_bases: &[usize],
    n_flow_rows_per_hour: usize,
    hourly_networks: &[Network],
    spec: &DispatchProblemSpec<'_>,
    layout: &ScucLayout,
    gen_indices: &[usize],
    is_must_run_ext: &[bool],
    da_commitment: Option<&[Vec<bool>]>,
    use_plc: bool,
    n_bp: usize,
    n_sbp: usize,
    n_hours: usize,
    time_limit_secs: Option<f64>,
) -> MipWarmStart {
    let helper_deadline = deadline_for_time_limit(time_limit_secs);
    let helper_time_limit_for = |kind: WarmStartScheduleKind| -> Option<f64> {
        warm_start_time_limit_secs(
            remaining_time_limit_secs(helper_deadline),
            gen_indices.len(),
            kind,
        )
    };
    let load_cover_schedule = build_mip_warm_start_commitment_schedule(
        spec,
        hourly_networks,
        gen_indices,
        layout,
        &prob.col_lower,
        &prob.col_upper,
        is_must_run_ext,
        da_commitment,
        n_hours,
    );
    let seeded_schedule = overlay_exact_commitment_warm_start_schedule(
        spec,
        load_cover_schedule.as_slice(),
        gen_indices.len(),
        n_hours,
    );
    let seeded_artifacts = seeded_schedule.as_ref().map(|schedule| {
        build_schedule_warm_start_artifacts(
            prob,
            hourly_networks,
            spec,
            layout,
            gen_indices,
            schedule.as_slice(),
            use_plc,
            n_bp,
            n_sbp,
            n_hours,
        )
    });
    if let Some(schedule) = seeded_schedule.as_ref() {
        let provided_helper_time_limit = helper_time_limit_for(WarmStartScheduleKind::Provided);
        if let Some(dense_start) = seeded_artifacts.as_ref().and_then(|artifacts| {
            try_build_dense_primal_start_from_artifacts(
                solver,
                spec,
                provided_helper_time_limit,
                artifacts,
                "provided_schedule",
            )
        }) {
            let verified_dense_incumbent =
                build_verified_dense_mip_incumbent(prob, dense_start.clone(), spec.tolerance);
            let use_dense_start = verified_dense_incumbent.is_some()
                || should_use_approximate_dense_mip_start(prob, &dense_start, spec.tolerance);
            return MipWarmStart {
                primal_start: Some(if use_dense_start {
                    LpPrimalStart::Dense(dense_start)
                } else {
                    build_commitment_sparse_primal_start_from_schedule(
                        spec,
                        layout,
                        schedule.as_slice(),
                        gen_indices.len(),
                        n_hours,
                    )
                }),
                verified_dense_incumbent,
            };
        }
    }
    let load_cover_artifacts = build_schedule_warm_start_artifacts(
        prob,
        hourly_networks,
        spec,
        layout,
        gen_indices,
        load_cover_schedule.as_slice(),
        use_plc,
        n_bp,
        n_sbp,
        n_hours,
    );
    let mut best_warm_start: Option<MipWarmStart> = None;
    let reduced_seed_artifacts = seeded_artifacts.as_ref().unwrap_or(&load_cover_artifacts);
    let load_cover_helper_time_limit = helper_time_limit_for(WarmStartScheduleKind::LoadCover);
    if let Some(dense_start) = try_build_dense_primal_start_from_artifacts(
        solver,
        spec,
        load_cover_helper_time_limit,
        &load_cover_artifacts,
        "load_cover",
    ) {
        let verified_dense_incumbent =
            build_verified_dense_mip_incumbent(prob, dense_start.clone(), spec.tolerance);
        let use_dense_start = verified_dense_incumbent.is_some();
        let candidate = MipWarmStart {
            primal_start: Some(if use_dense_start {
                LpPrimalStart::Dense(dense_start)
            } else {
                build_commitment_sparse_primal_start_from_schedule(
                    spec,
                    layout,
                    load_cover_schedule.as_slice(),
                    gen_indices.len(),
                    n_hours,
                )
            }),
            verified_dense_incumbent,
        };
        // Short-circuit: once load-cover produces a verified-feasible
        // MIP incumbent there's no work remaining helpers can do that
        // would beat "feasible, same commitment". Saves 4-5 additional
        // Gurobi solves on problems where commitment is easy.
        if candidate.verified_dense_incumbent.is_some() {
            info!(
                stage = "warm_start.load_cover",
                "SCUC: load-cover produced verified dense incumbent; skipping remaining helpers"
            );
            return candidate;
        }
        best_warm_start = choose_better_mip_warm_start(best_warm_start, candidate);
    }

    let reduced_relaxed_helper_time_limit =
        helper_time_limit_for(WarmStartScheduleKind::ReducedRelaxed);
    if n_flow_rows_per_hour > 0 {
        let reduced_relaxed_prob =
            build_network_relaxed_lp_problem(prob, hour_row_bases, n_flow_rows_per_hour);
        let _scuc_helper_t0 = Instant::now();
        let _scuc_helper_result = solve_sparse_problem_with_start_and_algorithm(
            solver,
            &reduced_relaxed_prob,
            spec.tolerance,
            reduced_relaxed_helper_time_limit,
            spec.mip_rel_gap(),
            Some(reduced_seed_artifacts.sparse_primal_start.clone()),
            LpAlgorithm::Ipm,
        );
        info!(
            stage = "warm_start.reduced_relaxed_lp",
            secs = _scuc_helper_t0.elapsed().as_secs_f64(),
            "SCUC helper solve timing"
        );
        match _scuc_helper_result {
            Ok(relaxed_solution)
                if matches!(
                    relaxed_solution.status,
                    LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
                ) =>
            {
                let max_primal_violation =
                    max_sparse_problem_primal_violation(&reduced_relaxed_prob, &relaxed_solution.x);
                debug!(
                    status = ?relaxed_solution.status,
                    vars = relaxed_solution.x.len(),
                    max_primal_violation,
                    "SCUC: built reduced relaxed commitment guidance"
                );
                if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                    log_scuc_problem_trace(format!(
                        "scuc_warm_start_trace label=reduced_relaxed status={:?} vars={} max_primal_violation={:.6e}",
                        relaxed_solution.status,
                        relaxed_solution.x.len(),
                        max_primal_violation,
                    ));
                }
                let relaxed_guided_schedule =
                    build_relaxed_guided_mip_warm_start_commitment_schedule(
                        spec,
                        hourly_networks,
                        gen_indices,
                        layout,
                        &prob.col_lower,
                        &prob.col_upper,
                        is_must_run_ext,
                        da_commitment,
                        &relaxed_solution,
                        n_hours,
                    );
                let relaxed_guided_artifacts = build_schedule_warm_start_artifacts(
                    prob,
                    hourly_networks,
                    spec,
                    layout,
                    gen_indices,
                    relaxed_guided_schedule.as_slice(),
                    use_plc,
                    n_bp,
                    n_sbp,
                    n_hours,
                );
                if let Some(dense_start) = try_build_dense_primal_start_from_artifacts(
                    solver,
                    spec,
                    reduced_relaxed_helper_time_limit,
                    &relaxed_guided_artifacts,
                    "reduced_relaxed",
                ) {
                    let verified_dense_incumbent = build_verified_dense_mip_incumbent(
                        prob,
                        dense_start.clone(),
                        spec.tolerance,
                    );
                    let use_dense_start = verified_dense_incumbent.is_some()
                        || should_use_approximate_dense_mip_start(
                            prob,
                            &dense_start,
                            spec.tolerance,
                        );
                    best_warm_start = choose_better_mip_warm_start(
                        best_warm_start,
                        MipWarmStart {
                            primal_start: Some(if use_dense_start {
                                LpPrimalStart::Dense(dense_start)
                            } else {
                                build_commitment_sparse_primal_start_from_schedule(
                                    spec,
                                    layout,
                                    relaxed_guided_schedule.as_slice(),
                                    gen_indices.len(),
                                    n_hours,
                                )
                            }),
                            verified_dense_incumbent,
                        },
                    );
                }
            }
            Ok(relaxed_solution) => {
                if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                    log_scuc_problem_trace(format!(
                        "scuc_warm_start_trace label=reduced_relaxed rejected_status={:?}",
                        relaxed_solution.status
                    ));
                }
            }
            Err(err) => {
                debug!(error = %err, "SCUC: reduced relaxed warm-start solve failed");
            }
        }
    }

    // Short-circuit before the core-commitment MIP helper if we already
    // have a verified MIP incumbent from reduced-relaxed.
    if best_warm_start
        .as_ref()
        .is_some_and(|ws| ws.verified_dense_incumbent.is_some())
    {
        info!(
            stage = "warm_start.reduced_relaxed",
            "SCUC: reduced-relaxed produced verified dense incumbent; skipping remaining helpers"
        );
        return best_warm_start.expect("verified incumbent implies Some");
    }

    let reduced_core_mip_helper_time_limit =
        helper_time_limit_for(WarmStartScheduleKind::ReducedCoreMip);
    if n_flow_rows_per_hour > 0 {
        let reduced_core_mip_prob = build_core_commitment_guidance_problem(
            prob,
            hour_row_bases,
            n_flow_rows_per_hour,
            layout,
            n_hours,
        );
        let _scuc_helper_t0 = Instant::now();
        let _scuc_helper_result = solve_sparse_problem_with_start(
            solver,
            &reduced_core_mip_prob,
            spec.tolerance,
            reduced_core_mip_helper_time_limit,
            spec.mip_rel_gap(),
            Some(reduced_seed_artifacts.sparse_primal_start.clone()),
        );
        info!(
            stage = "warm_start.reduced_core_mip",
            secs = _scuc_helper_t0.elapsed().as_secs_f64(),
            "SCUC helper solve timing"
        );
        match _scuc_helper_result {
            Ok(core_solution)
                if matches!(
                    core_solution.status,
                    LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
                ) =>
            {
                let max_primal_violation =
                    max_sparse_problem_primal_violation(&reduced_core_mip_prob, &core_solution.x);
                let max_integrality_violation = max_sparse_problem_integrality_violation(
                    &reduced_core_mip_prob,
                    &core_solution.x,
                );
                if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                    log_scuc_problem_trace(format!(
                        "scuc_warm_start_trace label=reduced_core_mip status={:?} vars={} max_primal_violation={:.6e} max_integrality_violation={:.6e}",
                        core_solution.status,
                        core_solution.x.len(),
                        max_primal_violation,
                        max_integrality_violation,
                    ));
                }
                let core_guided_schedule = build_relaxed_guided_mip_warm_start_commitment_schedule(
                    spec,
                    hourly_networks,
                    gen_indices,
                    layout,
                    &prob.col_lower,
                    &prob.col_upper,
                    is_must_run_ext,
                    da_commitment,
                    &core_solution,
                    n_hours,
                );
                let core_guided_artifacts = build_schedule_warm_start_artifacts(
                    prob,
                    hourly_networks,
                    spec,
                    layout,
                    gen_indices,
                    core_guided_schedule.as_slice(),
                    use_plc,
                    n_bp,
                    n_sbp,
                    n_hours,
                );
                if let Some(dense_start) = try_build_dense_primal_start_from_artifacts(
                    solver,
                    spec,
                    reduced_core_mip_helper_time_limit,
                    &core_guided_artifacts,
                    "reduced_core_mip",
                ) {
                    let verified_dense_incumbent = build_verified_dense_mip_incumbent(
                        prob,
                        dense_start.clone(),
                        spec.tolerance,
                    );
                    let use_dense_start = verified_dense_incumbent.is_some()
                        || should_use_approximate_dense_mip_start(
                            prob,
                            &dense_start,
                            spec.tolerance,
                        );
                    best_warm_start = choose_better_mip_warm_start(
                        best_warm_start,
                        MipWarmStart {
                            primal_start: Some(if use_dense_start {
                                LpPrimalStart::Dense(dense_start)
                            } else {
                                build_commitment_sparse_primal_start_from_schedule(
                                    spec,
                                    layout,
                                    core_guided_schedule.as_slice(),
                                    gen_indices.len(),
                                    n_hours,
                                )
                            }),
                            verified_dense_incumbent,
                        },
                    );
                }
            }
            Ok(core_solution) => {
                if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
                    log_scuc_problem_trace(format!(
                        "scuc_warm_start_trace label=reduced_core_mip rejected_status={:?}",
                        core_solution.status
                    ));
                }
            }
            Err(err) => {
                debug!(error = %err, "SCUC: reduced core-commitment warm-start MIP failed");
            }
        }
    }

    // Short-circuit before the conservative fallback helper if we now
    // have a verified MIP incumbent from reduced-core-MIP.
    if best_warm_start
        .as_ref()
        .is_some_and(|ws| ws.verified_dense_incumbent.is_some())
    {
        info!(
            stage = "warm_start.reduced_core_mip",
            "SCUC: reduced-core-MIP produced verified dense incumbent; skipping remaining helpers"
        );
        return best_warm_start.expect("verified incumbent implies Some");
    }

    let conservative_schedule = build_conservative_mip_warm_start_commitment_schedule(
        spec,
        hourly_networks,
        gen_indices,
        layout,
        &prob.col_upper,
        n_hours,
    );
    let conservative_artifacts = build_schedule_warm_start_artifacts(
        prob,
        hourly_networks,
        spec,
        layout,
        gen_indices,
        conservative_schedule.as_slice(),
        use_plc,
        n_bp,
        n_sbp,
        n_hours,
    );
    let conservative_helper_time_limit = helper_time_limit_for(WarmStartScheduleKind::Conservative);
    if let Some(dense_start) = try_build_dense_primal_start_from_artifacts(
        solver,
        spec,
        conservative_helper_time_limit,
        &conservative_artifacts,
        "conservative",
    ) {
        let verified_dense_incumbent =
            build_verified_dense_mip_incumbent(prob, dense_start.clone(), spec.tolerance);
        let use_dense_start = verified_dense_incumbent.is_some()
            || should_use_approximate_dense_mip_start(prob, &dense_start, spec.tolerance);
        best_warm_start = choose_better_mip_warm_start(
            best_warm_start,
            MipWarmStart {
                primal_start: Some(if use_dense_start {
                    LpPrimalStart::Dense(dense_start)
                } else {
                    build_commitment_sparse_primal_start_from_schedule(
                        spec,
                        layout,
                        conservative_schedule.as_slice(),
                        gen_indices.len(),
                        n_hours,
                    )
                }),
                verified_dense_incumbent,
            },
        );
    }

    if let Some(best_warm_start) = best_warm_start {
        return best_warm_start;
    }

    let fallback_artifacts = seeded_artifacts.as_ref().unwrap_or(&load_cover_artifacts);
    let (indices, values) = match &fallback_artifacts.sparse_primal_start {
        LpPrimalStart::Sparse { indices, values } => (indices, values),
        LpPrimalStart::Dense(_) => {
            unreachable!("warm-start helper only builds sparse primal starts")
        }
    };
    let sparse_assignment_ratio = if prob.n_col == 0 {
        0.0
    } else {
        indices.len() as f64 / prob.n_col as f64
    };
    let integer_assignment_ratio = if fallback_artifacts.total_integer_vars == 0 {
        1.0
    } else {
        fallback_artifacts.assigned_integer_vars as f64
            / fallback_artifacts.total_integer_vars as f64
    };
    if prob.n_col >= 50_000 && sparse_assignment_ratio < 0.10 && integer_assignment_ratio < 0.75 {
        warn!(
            assigned = indices.len(),
            cols = prob.n_col,
            sparse_assignment_ratio,
            integer_assignment_ratio,
            "SCUC: skipping sparse MIP warm start because low-coverage starts hurt large HiGHS solves"
        );
        if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
            log_scuc_problem_trace(format!(
                "scuc_warm_start_trace skipped_sparse assigned={} cols={} sparse_assignment_ratio={:.6e} integer_assignment_ratio={:.6e}",
                indices.len(),
                prob.n_col,
                sparse_assignment_ratio,
                integer_assignment_ratio,
            ));
        }
        return MipWarmStart::default();
    }
    debug!(
        assigned = indices.len(),
        total_integer_vars = fallback_artifacts.total_integer_vars,
        assigned_integer_vars = fallback_artifacts.assigned_integer_vars,
        fixed_integer_assignments = fallback_artifacts.fixed_integer_assignments,
        commitment_assignments = fallback_artifacts.commitment_assignments,
        integer_assignment_ratio,
        "SCUC: built sparse MIP primal start"
    );
    if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
        log_scuc_problem_trace(format!(
            "scuc_warm_start_trace assigned={} total_integer_vars={} assigned_integer_vars={} fixed_integer_assignments={} commitment_assignments={} integer_assignment_ratio={:.6e}",
            indices.len(),
            fallback_artifacts.total_integer_vars,
            fallback_artifacts.assigned_integer_vars,
            fallback_artifacts.fixed_integer_assignments,
            fallback_artifacts.commitment_assignments,
            integer_assignment_ratio,
        ));
    }
    MipWarmStart {
        primal_start: (!indices.is_empty()).then_some(LpPrimalStart::Sparse {
            indices: indices.clone(),
            values: values.clone(),
        }),
        verified_dense_incumbent: None,
    }
}

/// Solve the primary SCUC formulation before any pricing LP re-solve.
pub(super) fn solve_problem(
    input: ScucProblemInput<'_>,
) -> Result<ScucProblemState<'_>, ScedError> {
    let ScucProblemInput {
        solve,
        mut problem,
        mut problem_plan,
        initial_loss_warm_start,
    } = input;
    let spec = &solve.spec;
    let solver = solve.solver.as_ref();
    let setup = &solve.setup;
    let model_plan = problem_plan.model_plan;
    let layout = &model_plan.layout.layout;
    let cc_infos = &model_plan.variable.cc_infos;
    let cc_var_base = model_plan.variable.cc_var_base;
    let cc_block_size = model_plan.variable.cc_block_size;
    let n_var = model_plan.variable.n_var;
    let n_hours = spec.n_periods;
    let n_gen = setup.n_gen;
    let network = model_plan
        .hourly_networks
        .first()
        .expect("SCUC model plan always includes at least one hourly network");
    let solve_deadline = deadline_for_time_limit(spec.time_limit_secs());
    let is_fixed_commitment = matches!(spec.commitment, CommitmentMode::Fixed { .. });
    if is_fixed_commitment {
        apply_fixed_commitment_bounds(
            spec,
            layout,
            &setup.gen_indices,
            n_hours,
            &mut problem_plan.columns.col_lower,
            &mut problem_plan.columns.col_upper,
        );
        reconcile_fixed_commitment_startup_delta_bounds(
            spec,
            layout,
            &model_plan.startup,
            n_hours,
            &mut problem_plan.columns.col_lower,
            &mut problem_plan.columns.col_upper,
        );
        info!("SCUC: Fixed commitment mode — solving as pure LP (no MILP)");
    }

    if let Some(conflict) = debug_bound_conflicts(
        &problem_plan.columns.col_lower,
        &problem_plan.columns.col_upper,
        &problem.row_lower,
        &problem.row_upper,
        &problem.row_labels,
    ) {
        return Err(ScedError::SolverError(format!(
            "SCUC bound contradiction before solve: {conflict}"
        )));
    }

    debug!(
        vars = n_var,
        binary_vars = 3 * n_gen * n_hours,
        constraints = problem.n_row,
        hours = n_hours,
        generators = n_gen,
        "SCUC: MIP problem dimensions"
    );

    // Per-group variable breakdown. Sum of hourly-blocks × n_hours plus
    // post-hourly allocations should equal n_var exactly. Logged at info
    // so post-mortems on large scenarios don't need --log-level=debug.
    let hourly_blocks = layout.block_breakdown_per_hour();
    let vars_per_hour = layout.vars_per_hour();
    let hourly_total: usize = hourly_blocks.iter().map(|(_, c)| c * n_hours).sum();
    let post_hourly_total = n_var.saturating_sub(vars_per_hour * n_hours);
    let cc_vars = cc_block_size;
    let penalty_slacks = model_plan.variable.n_penalty_slacks;
    let other_post_hourly = post_hourly_total
        .saturating_sub(cc_vars)
        .saturating_sub(penalty_slacks);
    let mut breakdown_lines = String::new();
    for (name, per_period) in &hourly_blocks {
        let total = per_period * n_hours;
        let pct = if n_var > 0 {
            total as f64 / n_var as f64 * 100.0
        } else {
            0.0
        };
        breakdown_lines.push_str(&format!(
            "\n  {:<28} {:>10} × {:>3} = {:>14}  {:>6.2}%",
            name, per_period, n_hours, total, pct,
        ));
    }
    if cc_vars > 0 {
        let pct = cc_vars as f64 / n_var as f64 * 100.0;
        breakdown_lines.push_str(&format!(
            "\n  {:<28} {:>10} (post-hourly)         {:>14}  {:>6.2}%",
            "combined_cycle_vars", cc_vars, cc_vars, pct,
        ));
    }
    if penalty_slacks > 0 {
        let pct = penalty_slacks as f64 / n_var as f64 * 100.0;
        breakdown_lines.push_str(&format!(
            "\n  {:<28} {:>10} (post-hourly)         {:>14}  {:>6.2}%",
            "penalty_slacks", penalty_slacks, penalty_slacks, pct,
        ));
    }
    if other_post_hourly > 0 {
        let pct = other_post_hourly as f64 / n_var as f64 * 100.0;
        breakdown_lines.push_str(&format!(
            "\n  {:<28} {:>10} (post-hourly)         {:>14}  {:>6.2}%",
            "other_post_hourly", other_post_hourly, other_post_hourly, pct,
        ));
    }
    let contiguity_delta = (hourly_total + post_hourly_total) as i64 - n_var as i64;
    info!(
        n_var,
        vars_per_hour,
        hourly_total,
        post_hourly_total,
        cc_vars,
        penalty_slacks,
        contiguity_delta,
        "SCUC variable breakdown (n_hours={}, n_gen={}):{}\n  {:<28} {:>10}        {:>14}       100%",
        n_hours,
        n_gen,
        breakdown_lines,
        "TOTAL",
        "",
        n_var,
    );
    if !cc_infos.is_empty() {
        info!(
            n_cc_plants = cc_infos.len(),
            n_cc_configs = cc_infos.iter().map(|ci| ci.n_configs).sum::<usize>(),
            n_cc_member_gens = cc_infos
                .iter()
                .map(|ci| ci.member_gen_j.len())
                .sum::<usize>(),
            n_cc_vars = cc_block_size,
            n_cc_rows = problem.n_cc_rows,
            "SCUC: combined cycle plants enabled"
        );
        debug!(
            cc_var_base,
            cc_block_size,
            n_cc_rows = problem.n_cc_rows,
            "SCUC: CC problem dimensions"
        );
        for (plant_idx, info) in cc_infos.iter().enumerate() {
            for config_idx in 0..info.n_configs {
                let z_base = cc_var_base + info.z_block_off + config_idx * n_hours;
                for hour in 0..n_hours {
                    let z_idx = z_base + hour;
                    debug_assert!(
                        problem_plan.columns.col_upper[z_idx] >= 1.0,
                        "CC z[{plant_idx},{config_idx},{hour}] col_upper={} should be 1.0",
                        problem_plan.columns.col_upper[z_idx]
                    );
                }
            }
        }
    }

    let (col_names, row_names) = if should_attach_scuc_problem_names(spec) {
        let (col_names, row_names) =
            build_scuc_problem_names(&problem, &problem_plan, network, setup, n_hours);
        (Some(col_names), Some(row_names))
    } else {
        (None, None)
    };

    // When the LP repricing re-solve won't run, the row/column vectors
    // in `problem` and the modifiable vectors in `problem_plan.columns`
    // are never read again after they land in `SparseProblem`. Move
    // them instead of cloning to skip O(nonzeros) memory traffic — on
    // 617-bus × 18 periods with 8.6M flowgate rows the matrix holds
    // tens of millions of nonzeros; on 23643-bus this is ~linear with
    // bus count × periods and grows fast. `col_cost` stays cloned —
    // `extract_solution` still reads it. Per-vector behaviour:
    //   - row_lower / row_upper           ← read only by run_pricing
    //   - a_start / a_index / a_value     ← read only by run_pricing
    //   - col_lower / col_upper           ← mutated here by
    //                                       apply_fixed_commitment_bounds
    //                                       and otherwise unused post-solve
    //   - integrality                      ← never read post-solve
    //   - col_cost                         ← still needed by extract
    let skip_repricing = !spec.run_pricing;
    let (row_lower, row_upper, a_start, a_index, a_value) = if skip_repricing {
        (
            std::mem::take(&mut problem.row_lower),
            std::mem::take(&mut problem.row_upper),
            std::mem::take(&mut problem.a_start),
            std::mem::take(&mut problem.a_index),
            std::mem::take(&mut problem.a_value),
        )
    } else {
        (
            problem.row_lower.clone(),
            problem.row_upper.clone(),
            problem.a_start.clone(),
            problem.a_index.clone(),
            problem.a_value.clone(),
        )
    };
    let col_lower = if skip_repricing {
        std::mem::take(&mut problem_plan.columns.col_lower)
    } else {
        problem_plan.columns.col_lower.clone()
    };
    let col_upper = if skip_repricing {
        std::mem::take(&mut problem_plan.columns.col_upper)
    } else {
        problem_plan.columns.col_upper.clone()
    };
    let integrality = if is_fixed_commitment {
        None
    } else if skip_repricing {
        Some(std::mem::take(&mut problem_plan.columns.integrality))
    } else {
        Some(problem_plan.columns.integrality.clone())
    };

    let _build_sparse_t0 = Instant::now();
    let mut prob = build_sparse_problem(DcSparseProblemInput {
        n_col: n_var,
        n_row: problem.n_row,
        col_cost: problem_plan.columns.col_cost.clone(),
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
        col_names,
        row_names,
        integrality,
    });
    info!(
        stage = "build_sparse_problem",
        secs = _build_sparse_t0.elapsed().as_secs_f64(),
        n_row = prob.n_row,
        n_col = prob.n_col,
        n_nz = prob.a_value.len(),
        "SCUC helper solve timing"
    );
    if let Some(col_idx) = env_usize("SURGE_DEBUG_TRACE_SCUC_COL_IDX") {
        if col_idx < problem_plan.columns.col_lower.len() && col_idx < prob.col_lower.len() {
            log_scuc_problem_trace(format!(
                "scuc_problem_trace col={} pre_build=[{:.6},{:.6}] post_build=[{:.6},{:.6}]",
                col_idx,
                problem_plan.columns.col_lower[col_idx],
                problem_plan.columns.col_upper[col_idx],
                prob.col_lower[col_idx],
                prob.col_upper[col_idx],
            ));
        }
    }
    // Build the loss-factor setup once per solve, to be shared between
    // the optional pre-MIP warm-start application and the post-MIP
    // refinement iteration. Costs one O(n_nz) column walk + per-period
    // loss-PTDF build — shared so the refinement loop doesn't rebuild.
    let loss_prep = if spec.use_loss_factors && network.n_buses() > 1 {
        let _prep_t0 = Instant::now();
        let prep = crate::scuc::losses::build_loss_factor_prep(
            &prob,
            &model_plan.hourly_networks,
            &solve.bus_map,
            layout,
            &setup.gen_bus_idx,
            &problem.hour_row_bases,
            problem.n_branch_flow + problem.n_fg_rows + model_plan.network_plan.iface_rows.len(),
            network.n_buses(),
        )?;
        info!(
            stage = "build_loss_factor_prep",
            secs = _prep_t0.elapsed().as_secs_f64(),
            "SCUC helper solve timing"
        );
        Some(prep)
    } else {
        None
    };

    // Pre-MIP loss-factor warm start: when a caller supplies an
    // `initial_loss_warm_start` (security-loop cache from the prior
    // iteration, DC-PF-on-rough-dispatch estimate, load-pattern
    // approximation, or a uniform loss rate), scale the LP
    // coefficients + bus-balance RHS with it BEFORE the first MIP.
    // Without this, the first MIP is solved lossless and the
    // refinement loop needs one full LP re-solve to correct for
    // losses. With a decent warm start the lossless-MIP dispatch
    // already matches the loss-aware optimum, so `iterate_loss_factors`
    // can fire the `loss_iter > 0 || initial_dloss.is_some()`
    // convergence gate at iter 0 and avoid the re-solve entirely.
    if let (Some(prep), Some(warm)) = (&loss_prep, initial_loss_warm_start.as_ref()) {
        let base_mva = network.base_mva;
        let total_losses_pu: Vec<f64> = warm
            .total_losses_mw
            .iter()
            .map(|mw| if base_mva > 0.0 { mw / base_mva } else { 0.0 })
            .collect();
        let _warm_t0 = Instant::now();
        crate::scuc::losses::apply_bus_loss_factors(
            &mut prob,
            prep,
            &problem.hour_row_bases,
            problem.n_branch_flow + problem.n_fg_rows + model_plan.network_plan.iface_rows.len(),
            &warm.dloss_dp,
            &total_losses_pu,
        );
        info!(
            stage = "apply_loss_warm_start",
            secs = _warm_t0.elapsed().as_secs_f64(),
            total_losses_mw_t0 = warm.total_losses_mw.first().copied().unwrap_or(0.0),
            "SCUC helper solve timing: pre-MIP loss-factor warm-start applied"
        );
    }

    if let Some(path) = std::env::var_os("SURGE_DEBUG_DUMP_SCUC_LP") {
        let path = Path::new(&path);
        dump_scuc_lp(
            path,
            &prob,
            &problem,
            &problem_plan,
            network,
            setup,
            n_hours,
        )
        .map_err(|err| {
            ScedError::SolverError(format!(
                "failed to dump SCUC LP to {}: {err}",
                path.display()
            ))
        })?;
        info!(path = %path.display(), "SCUC: dumped debug LP");
    }
    let mip_warm_start = if is_fixed_commitment || spec.disable_warm_start() {
        if spec.disable_warm_start() {
            // Light warm-start path: if the caller supplied a
            // `warm_start_commitment` schedule we still want to hand the
            // MIP a sparse primal start for the commitment binaries —
            // cheaper than the full helper pipeline, but enough for
            // Gurobi to skip root LP work when iterating the security
            // loop with the same-ish commitment across rounds.
            if has_warm_start_commitment(spec, setup.n_gen, n_hours) {
                let schedule = collect_warm_start_commitment_schedule(spec, setup.n_gen, n_hours);
                let primal_start = build_commitment_sparse_primal_start_from_schedule(
                    spec,
                    layout,
                    schedule.as_slice(),
                    setup.n_gen,
                    n_hours,
                );
                info!("SCUC: using light warm-start (commitment hint, skipping helper pipeline)");
                MipWarmStart {
                    primal_start: Some(primal_start),
                    verified_dense_incumbent: None,
                }
            } else {
                info!("SCUC: skipping MIP warm-start pipeline (disable_warm_start=true)");
                MipWarmStart::default()
            }
        } else {
            MipWarmStart::default()
        }
    } else {
        try_build_mip_primal_start(
            solver,
            &prob,
            &problem.hour_row_bases,
            problem.n_branch_flow + problem.n_fg_rows + model_plan.network_plan.iface_rows.len(),
            &model_plan.hourly_networks,
            spec,
            layout,
            &setup.gen_indices,
            &model_plan.commitment_policy.is_must_run_ext,
            model_plan.commitment_policy.da_commitment,
            model_plan.use_plc,
            model_plan.n_bp,
            model_plan.n_sbp,
            n_hours,
            remaining_time_limit_secs(solve_deadline),
        )
    };
    let mut verified_dense_incumbent = mip_warm_start.verified_dense_incumbent;
    let mip_primal_start = mip_warm_start.primal_start;
    if env_flag("SURGE_DEBUG_SCUC_WARM_START") {
        log_scuc_problem_trace(format!(
            "scuc_warm_start_trace built={}",
            mip_primal_start
                .as_ref()
                .map(|start| match start {
                    LpPrimalStart::Dense(values) => values.len(),
                    LpPrimalStart::Sparse { indices, .. } => indices.len(),
                })
                .unwrap_or_default()
        ));
    }

    let mip_time_limit_secs = remaining_time_limit_secs(solve_deadline);
    let mut solution = if !is_fixed_commitment
        && verified_dense_incumbent.is_some()
        && should_short_circuit_to_verified_incumbent(
            solver.name(),
            prob.n_col,
            mip_time_limit_secs,
        ) {
        info!(
            cols = prob.n_col,
            solver = solver.name(),
            time_limit_secs = mip_time_limit_secs,
            "SCUC: using verified dense warm-start incumbent directly for large budgeted MIP"
        );
        verified_dense_incumbent
            .take()
            .expect("verified_dense_incumbent checked Some above")
    } else {
        // Build the optional time-varying gap schedule. Malformed schedules
        // are logged and dropped rather than failing the solve — the static
        // `mip_rel_gap` safety net still applies.
        let mip_gap_schedule = match spec.mip_gap_schedule() {
            Some(breakpoints) if !breakpoints.is_empty() => {
                match MipGapSchedule::new(breakpoints.to_vec()) {
                    Ok(schedule) => Some(schedule),
                    Err(err) => {
                        warn!(error = %err, "SCUC: ignoring invalid mip_gap_schedule");
                        None
                    }
                }
            }
            _ => None,
        };
        let _scuc_final_mip_t0 = Instant::now();
        let _scuc_final_mip_result = solve_sparse_problem_with_options(
            solver,
            &prob,
            spec.tolerance,
            mip_time_limit_secs,
            spec.mip_rel_gap(),
            mip_gap_schedule,
            mip_primal_start,
            LpAlgorithm::Auto,
        );
        info!(
            stage = "final_mip",
            secs = _scuc_final_mip_t0.elapsed().as_secs_f64(),
            "SCUC helper solve timing"
        );
        match _scuc_final_mip_result {
            Ok(solution) => solution,
            Err(err)
                if !is_fixed_commitment
                    && verified_dense_incumbent.is_some()
                    && is_time_limit_without_incumbent_error(&err.to_string()) =>
            {
                warn!(
                    cols = prob.n_col,
                    error = %err,
                    "SCUC: falling back to verified dense warm-start incumbent after MIP produced no incumbent"
                );
                verified_dense_incumbent
                    .take()
                    .expect("verified_dense_incumbent checked Some above")
            }
            Err(err) => {
                return Err(ScedError::SolverError(augment_solver_error(
                    err.to_string(),
                    &problem,
                    &problem_plan,
                    network,
                    setup,
                    n_hours,
                    model_plan.variable.penalty_slack_base,
                )));
            }
        }
    };

    // Capture the MIP progress trace (if any) before downstream helpers
    // (loss iteration, pricing) replace `solution`. The commitment MIP
    // is the only stage whose trace we currently report.
    let commitment_mip_trace = solution.mip_trace.clone();

    if !matches!(
        solution.status,
        LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
    ) {
        warn!(
            status = ?solution.status,
            iterations = solution.iterations,
            hours = n_hours,
            generators = n_gen,
            "SCUC: {} did not converge",
            if is_fixed_commitment { "LP" } else { "MIP" }
        );
        return Err(ScedError::NotConverged {
            iterations: solution.iterations,
        });
    }

    let mut bus_loss_allocation_mw: Vec<Vec<f64>> = Vec::new();
    let mut final_loss_warm_start: Option<crate::scuc::losses::LossFactorWarmStart> = None;
    if let Some(ref prep) = loss_prep {
        let _loss_iter_t0 = Instant::now();
        // Thread the caller's initial_dloss (if any) into the
        // convergence-detection logic so a well-seeded solve can
        // short-circuit on iter 0.
        let initial_dloss_slice: Option<Vec<Vec<f64>>> = initial_loss_warm_start
            .as_ref()
            .map(|ws| ws.dloss_dp.clone());
        let loss_result = iterate_loss_factors(
            ScucLossIterationInput {
                solver,
                spec,
                hourly_networks: &model_plan.hourly_networks,
                bus_map: &solve.bus_map,
                layout,
                gen_bus_idx: &setup.gen_bus_idx,
                hour_row_bases: &problem.hour_row_bases,
                n_flow: problem.n_branch_flow
                    + problem.n_fg_rows
                    + model_plan.network_plan.iface_rows.len(),
                n_bus: network.n_buses(),
                time_limit_secs: remaining_time_limit_secs(solve_deadline),
                problem: &mut prob,
                solution: &mut solution,
            },
            prep,
            initial_dloss_slice.as_deref(),
        )?;
        info!(
            stage = "iterate_loss_factors",
            secs = _loss_iter_t0.elapsed().as_secs_f64(),
            "SCUC helper solve timing"
        );
        bus_loss_allocation_mw = loss_result.loss_allocation_mw;

        // Compose the final warm-start state to return to the caller
        // (the security loop). Captures both the refined `dloss_dp`
        // and the per-period total losses (in MW) consistent with
        // the solved theta.
        let theta_by_hour = crate::scuc::losses::extract_theta_by_hour(
            &solution,
            layout,
            n_hours,
            network.n_buses(),
        );
        let total_losses_mw = crate::scuc::losses::compute_total_losses_mw_from_theta(
            &model_plan.hourly_networks,
            &solve.bus_map,
            &theta_by_hour,
        );
        final_loss_warm_start = Some(crate::scuc::losses::LossFactorWarmStart {
            dloss_dp: loss_result.dloss_dp,
            total_losses_mw,
        });
    }

    if !matches!(
        solution.status,
        LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
    ) {
        warn!(
            status = ?solution.status,
            iterations = solution.iterations,
            hours = n_hours,
            generators = n_gen,
            "SCUC: {} loss-adjusted solve did not converge",
            if is_fixed_commitment { "LP" } else { "MIP" }
        );
        return Err(ScedError::NotConverged {
            iterations: solution.iterations,
        });
    }

    if let Some(unit_id) = env_value("SURGE_DEBUG_TRACE_SCUC_UNIT") {
        trace_commitment_solution_for_unit(
            &unit_id,
            network,
            setup,
            layout,
            n_hours,
            solve.base_mva,
            &solution,
        );
        trace_commitment_row_activity_for_unit(
            &unit_id, network, setup, layout, n_hours, &problem, &solution,
        );
    }

    let need_debug_col_names = env_value("SURGE_DEBUG_DUMP_SCUC_PRIMARY_COLS").is_some()
        || env_value("SURGE_DEBUG_DUMP_SCUC_PRIMARY_COL_NNZ").is_some();
    let need_debug_row_names = env_value("SURGE_DEBUG_DUMP_SCUC_PRIMARY_ROWS").is_some()
        || env_value("SURGE_DEBUG_DUMP_SCUC_PRIMARY_COL_NNZ").is_some();
    let col_names_for_debug = if prob.col_names.is_some() {
        None
    } else if need_debug_col_names {
        Some(
            (0..n_var)
                .map(|j| {
                    describe_scuc_column(
                        j,
                        &problem_plan,
                        network,
                        setup,
                        n_hours,
                        model_plan.variable.penalty_slack_base,
                    )
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let row_names_for_debug = if prob.row_names.is_some() {
        None
    } else if need_debug_row_names {
        Some(
            (0..problem.n_row)
                .map(|idx| {
                    problem
                        .row_labels
                        .get(idx)
                        .filter(|label| !label.is_empty())
                        .cloned()
                        .unwrap_or_else(|| format!("row_{idx}"))
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    if let Some(path) = std::env::var_os("SURGE_DEBUG_DUMP_SCUC_PRIMARY_ROWS") {
        let path = Path::new(&path);
        let row_names = row_names_for_debug.as_ref().unwrap_or_else(|| {
            prob.row_names
                .as_ref()
                .expect("row names present when debug dump requested")
        });
        dump_scuc_row_duals(
            path,
            row_names,
            &problem.row_lower,
            &problem.row_upper,
            &solution.row_dual,
        )
        .map_err(|err| {
            ScedError::SolverError(format!(
                "failed to dump SCUC primary row duals to {}: {err}",
                path.display()
            ))
        })?;
        info!(path = %path.display(), "SCUC: dumped primary row duals");
    }
    if let Some(path) = std::env::var_os("SURGE_DEBUG_DUMP_SCUC_PRIMARY_COLS") {
        let path = Path::new(&path);
        let col_names = col_names_for_debug.as_ref().unwrap_or_else(|| {
            prob.col_names
                .as_ref()
                .expect("column names present when debug dump requested")
        });
        dump_scuc_column_solution(
            path,
            col_names,
            &prob.col_cost,
            &prob.col_lower,
            &prob.col_upper,
            &solution.x,
            &solution.col_dual,
        )
        .map_err(|err| {
            ScedError::SolverError(format!(
                "failed to dump SCUC primary columns to {}: {err}",
                path.display()
            ))
        })?;
        info!(path = %path.display(), "SCUC: dumped primary columns");
    }
    if let Some(path) = std::env::var_os("SURGE_DEBUG_DUMP_SCUC_PRIMARY_COL_NNZ") {
        let path = Path::new(&path);
        let col_names = col_names_for_debug
            .clone()
            .or_else(|| prob.col_names.clone())
            .unwrap_or_default();
        let row_names = row_names_for_debug
            .clone()
            .or_else(|| prob.row_names.clone())
            .unwrap_or_default();
        let debug_problem = SparseProblem {
            n_col: prob.n_col,
            n_row: prob.n_row,
            col_cost: prob.col_cost.clone(),
            col_lower: prob.col_lower.clone(),
            col_upper: prob.col_upper.clone(),
            row_lower: problem.row_lower.clone(),
            row_upper: problem.row_upper.clone(),
            a_start: prob.a_start.clone(),
            a_index: prob.a_index.clone(),
            a_value: prob.a_value.clone(),
            q_start: prob.q_start.clone(),
            q_index: prob.q_index.clone(),
            q_value: prob.q_value.clone(),
            col_names: Some(col_names),
            row_names: Some(row_names),
            integrality: prob.integrality.clone(),
        };
        let filters = env_value("SURGE_DEBUG_SCUC_COL_LABEL_FILTER")
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        dump_scuc_selected_column_nnz(path, &debug_problem, &solution.row_dual, &filters).map_err(
            |err| {
                ScedError::SolverError(format!(
                    "failed to dump SCUC primary column structure to {}: {err}",
                    path.display()
                ))
            },
        )?;
        info!(path = %path.display(), filters = ?filters, "SCUC: dumped primary column structure");
    }

    // ── Capture model diagnostic if requested ──
    let model_diagnostic = if spec.capture_model_diagnostics {
        let col_names = prob.col_names.clone().unwrap_or_else(|| {
            (0..n_var)
                .map(|j| {
                    describe_scuc_column(
                        j,
                        &problem_plan,
                        network,
                        setup,
                        n_hours,
                        model_plan.variable.penalty_slack_base,
                    )
                })
                .collect()
        });
        let row_names = prob.row_names.clone().unwrap_or_else(|| {
            (0..problem.n_row)
                .map(|idx| {
                    problem
                        .row_labels
                        .get(idx)
                        .filter(|label| !label.is_empty())
                        .cloned()
                        .unwrap_or_else(|| format!("row_{idx}"))
                })
                .collect()
        });
        let stage = if is_fixed_commitment {
            crate::model_diagnostic::DiagnosticStage::ScedDispatch
        } else {
            crate::model_diagnostic::DiagnosticStage::ScucCommitment
        };
        Some(crate::model_diagnostic::ModelDiagnostic::build(
            stage,
            n_var,
            problem.n_row,
            problem.a_value.len(),
            &prob.col_lower,
            &prob.col_upper,
            &prob.col_cost,
            &problem.row_lower,
            &problem.row_upper,
            prob.integrality.as_deref(),
            &solution.x,
            &solution.row_dual,
            &solution.col_dual,
            solution.objective,
            &format!("{:?}", solution.status),
            solver.name(),
            0.0, // solve_time captured separately at higher level
            solution.iterations,
            None,
            false,
            &col_names,
            &row_names,
        ))
    } else {
        None
    };

    Ok(ScucProblemState {
        problem,
        problem_plan,
        is_fixed_commitment,
        solution,
        model_diagnostic,
        bus_loss_allocation_mw,
        commitment_mip_trace,
        final_loss_warm_start,
    })
}
