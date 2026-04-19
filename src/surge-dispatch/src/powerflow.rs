// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Physical replay of dispatch schedules through power flow.

use std::collections::HashMap;
use std::time::Instant;

use surge_ac::FdpfFactors;
use surge_ac::matrix::ybus::build_ybus;
use surge_ac::{AcPfOptions, solve_ac_pf};
use surge_ac::{FdpfOptions, solve_fdpf};
use surge_dc::{solve_dc, to_pf_solution};
use surge_network::Network;
use surge_network::network::BusType;
use surge_solution::{
    PfModel, PfSolution, SolveStatus, apply_dispatch_mw, compute_branch_power_flows,
};
use thiserror::Error;
use tracing::{info, warn};

use crate::request::Formulation;
use crate::request::{DispatchRequest, DispatchSolveOptions};
use crate::{
    DispatchError, DispatchModel, DispatchPeriodResult, DispatchResourceKind, DispatchSolution,
    ResourcePeriodDetail,
};

/// Solver used for each replay period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaySolver {
    DcPf,
    AcNewtonRaphson,
    AcFastDecoupled,
}

/// Voltage violation thresholds in per-unit.
#[derive(Debug, Clone, Copy)]
pub struct VoltageLimits {
    pub min_pu: f64,
    pub max_pu: f64,
}

impl Default for VoltageLimits {
    fn default() -> Self {
        Self {
            min_pu: 0.95,
            max_pu: 1.05,
        }
    }
}

/// Optional dense outputs to retain from the replay.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplayOutputs {
    pub bus_voltages: bool,
    pub branch_flows: bool,
    pub branch_loading: bool,
}

/// Replay configuration.
#[derive(Debug, Clone)]
pub struct ReplayOptions {
    pub runtime: ReplaySolver,
    pub max_iterations: usize,
    pub tolerance: f64,
    pub warm_start: bool,
    pub voltage_limits: VoltageLimits,
    pub outputs: ReplayOutputs,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            runtime: ReplaySolver::AcNewtonRaphson,
            max_iterations: 30,
            tolerance: 1e-6,
            warm_start: true,
            voltage_limits: VoltageLimits::default(),
            outputs: ReplayOutputs::default(),
        }
    }
}

/// Stable branch identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BranchKey {
    pub from_bus: u32,
    pub to_bus: u32,
    pub circuit: String,
}

/// A single voltage limit violation.
#[derive(Debug, Clone)]
pub struct VoltageViolation {
    pub bus: u32,
    pub vm_pu: f64,
    pub limit_pu: f64,
}

/// A single thermal overload.
#[derive(Debug, Clone)]
pub struct ThermalViolation {
    pub branch: BranchKey,
    pub loading_pct: f64,
}

/// Per-period replay result.
#[derive(Debug, Clone)]
pub struct ReplayPeriod {
    pub period: usize,
    pub converged: bool,
    pub solve_time_ms: f64,
    pub total_load_mw: f64,
    pub total_loss_mw: f64,
    pub min_voltage_pu: Option<f64>,
    pub max_voltage_pu: Option<f64>,
    pub peak_branch_loading_pct: f64,
    pub voltage_violations: Vec<VoltageViolation>,
    pub thermal_violations: Vec<ThermalViolation>,
    pub error: Option<String>,
}

/// Per-branch thermal summary across the replay horizon.
#[derive(Debug, Clone)]
pub struct BranchThermalSummary {
    pub branch: BranchKey,
    pub max_loading_pct: f64,
    pub avg_loading_pct: f64,
    pub overloaded_periods: u32,
    pub total_overload_mwh: f64,
}

/// Per-bus voltage summary across the replay horizon.
#[derive(Debug, Clone)]
pub struct BusVoltageSummary {
    pub bus: u32,
    pub min_voltage_pu: Option<f64>,
    pub max_voltage_pu: Option<f64>,
    pub avg_voltage_pu: Option<f64>,
    pub below_limit_periods: u32,
    pub above_limit_periods: u32,
}

/// Aggregated replay summary statistics.
#[derive(Debug, Clone)]
pub struct ReplaySummary {
    pub converged_periods: usize,
    pub peak_loss_mw: f64,
    pub total_losses_mwh: f64,
    pub peak_load_mw: f64,
    pub peak_load_period: usize,
    pub min_voltage_pu: Option<f64>,
    pub max_voltage_pu: Option<f64>,
    pub voltage_violation_count: usize,
    pub thermal_violation_count: usize,
    pub branches: Vec<BranchThermalSummary>,
    pub buses: Vec<BusVoltageSummary>,
}

/// Optional dense matrices plus their ordering metadata.
#[derive(Debug, Clone)]
pub struct ReplayMatrices {
    pub bus_numbers: Vec<u32>,
    pub branch_keys: Vec<BranchKey>,
    pub bus_voltages_pu: Option<Vec<Vec<f64>>>,
    pub branch_flows_mw: Option<Vec<Vec<f64>>>,
    pub branch_loading_pct: Option<Vec<Vec<f64>>>,
}

/// Full replay output.
#[derive(Debug, Clone)]
pub struct ReplayResult {
    pub runtime: ReplaySolver,
    pub dt_hours: f64,
    pub periods: Vec<ReplayPeriod>,
    pub summary: ReplaySummary,
    pub matrices: ReplayMatrices,
    pub total_solve_time_secs: f64,
}

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("invalid replay configuration: {message}")]
    InvalidConfiguration { message: String },

    #[error("invalid dispatch request: {0}")]
    InvalidRequest(String),

    #[error("dispatch result has {actual} periods but request expects {expected}")]
    PeriodCountMismatch { expected: usize, actual: usize },

    #[error("dispatch result is incompatible with the replay network/request: {message}")]
    IncompatibleDispatch { message: String },

    #[error(
        "powerflow replay requires per-bus withdrawal results for all buses in period {period}"
    )]
    MissingBusWithdrawals { period: usize },

    #[error("failed to apply dispatch schedule in period {period}: {message}")]
    InvalidDispatchSchedule { period: usize, message: String },
}

/// Replay a solved dispatch schedule through DC or AC power flow using a prepared model.
pub fn replay_dispatch_with_model(
    model: &DispatchModel,
    request: &DispatchRequest,
    dispatch: &DispatchSolution,
    options: &ReplayOptions,
) -> Result<ReplayResult, ReplayError> {
    let canonical_network = model.network();
    let normalized = request
        .resolve_with_options(canonical_network, &DispatchSolveOptions::default())
        .map_err(|error| ReplayError::InvalidRequest(error.to_string()))?;
    validate_inputs(canonical_network, request, &normalized, dispatch, options)?;

    let sim_start = Instant::now();
    let n_periods = normalized.input.n_periods;
    let dt_hours = normalized.input.dt_hours;
    let n_buses = canonical_network.buses.len();
    let n_branches = canonical_network.branches.len();
    let spec = normalized.problem_spec();
    let branch_keys: Vec<BranchKey> = canonical_network
        .branches
        .iter()
        .map(|branch| BranchKey {
            from_bus: branch.from_bus,
            to_bus: branch.to_bus,
            circuit: branch.circuit.clone(),
        })
        .collect();

    info!(
        n_periods = n_periods,
        solver = ?options.runtime,
        warm_start = options.warm_start,
        max_iterations = options.max_iterations,
        tolerance = options.tolerance,
        "replay_dispatch: entry"
    );

    let mut branch_max_loading = vec![0.0_f64; n_branches];
    let mut branch_sum_loading = vec![0.0_f64; n_branches];
    let mut branch_periods_overloaded = vec![0_u32; n_branches];
    let mut branch_total_overload_mwh = vec![0.0; n_branches];

    let mut bus_min_voltage = vec![f64::INFINITY; n_buses];
    let mut bus_max_voltage = vec![f64::NEG_INFINITY; n_buses];
    let mut bus_sum_voltage = vec![0.0; n_buses];
    let mut bus_periods_low = vec![0_u32; n_buses];
    let mut bus_periods_high = vec![0_u32; n_buses];

    let mut bus_voltage_matrix = if options.outputs.bus_voltages {
        Some(vec![Vec::with_capacity(n_periods); n_buses])
    } else {
        None
    };
    let mut branch_flow_matrix = if options.outputs.branch_flows {
        Some(vec![Vec::with_capacity(n_periods); n_branches])
    } else {
        None
    };
    let mut branch_loading_matrix = if options.outputs.branch_loading {
        Some(vec![Vec::with_capacity(n_periods); n_branches])
    } else {
        None
    };

    let mut warm_solution: Option<PfSolution> = None;
    let mut periods = Vec::with_capacity(n_periods);
    let mut peak_load_mw = 0.0_f64;
    let mut peak_load_period = 0_usize;
    let mut total_losses_mwh = 0.0_f64;

    for period_idx in 0..n_periods {
        let period_start = Instant::now();
        let dispatch_period = dispatch
            .periods
            .get(period_idx)
            .expect("dispatch length validated earlier");
        let mut snapshot = canonical_network.clone();
        match normalized.formulation {
            Formulation::Dc => {
                crate::common::profiles::apply_dc_time_series_profiles(
                    &mut snapshot,
                    &spec,
                    period_idx,
                );
            }
            Formulation::Ac => {
                crate::common::profiles::apply_ac_time_series_profiles(
                    &mut snapshot,
                    &spec,
                    period_idx,
                );
            }
        }
        apply_dispatchable_loads_from_dispatch(&mut snapshot, dispatch_period);
        let generator_dispatch = generator_dispatch_vector(dispatch, dispatch_period);
        apply_dispatch_mw(&mut snapshot, &generator_dispatch).map_err(|error| {
            ReplayError::InvalidDispatchSchedule {
                period: period_idx,
                message: error.to_string(),
            }
        })?;
        apply_hvdc_dispatch(&mut snapshot, &normalized.input.hvdc_links, dispatch_period);

        let total_load_mw: f64 = snapshot
            .loads
            .iter()
            .filter(|l| l.in_service)
            .map(|l| l.active_power_demand_mw)
            .sum();
        let solve_result = solve_period(&snapshot, options, warm_solution.as_ref());
        let solve_time_ms = period_start.elapsed().as_secs_f64() * 1000.0;

        let (converged, solution, error) = match solve_result {
            Ok(solution) if solution.status == SolveStatus::Converged => {
                (true, Some(solution), None)
            }
            Ok(solution) => {
                let status = solution.status;
                (
                    false,
                    Some(solution),
                    Some(format!("solver returned {:?}", status)),
                )
            }
            Err(message) => (false, None, Some(message)),
        };

        let mut min_voltage_pu = None;
        let mut max_voltage_pu = None;
        let mut total_loss_mw = 0.0;
        let mut peak_branch_loading_pct = 0.0;
        let mut voltage_violations = Vec::new();
        let mut thermal_violations = Vec::new();

        if let Some(solution) = solution.as_ref()
            && converged
        {
            if options.warm_start {
                warm_solution = Some(solution.clone());
            }

            let vm = &solution.voltage_magnitude_pu;
            let period_min_v = vm.iter().copied().fold(f64::INFINITY, f64::min);
            let period_max_v = vm.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            min_voltage_pu = Some(period_min_v);
            max_voltage_pu = Some(period_max_v);
            total_loss_mw = compute_losses_mw(solution, &snapshot);

            let loading_pct = solution.branch_loading_pct(&snapshot).unwrap_or_default();
            peak_branch_loading_pct = loading_pct.iter().copied().fold(0.0, f64::max);

            for (bus_index, &vm_pu) in vm.iter().enumerate() {
                let bus_number = snapshot.buses[bus_index].number;
                if vm_pu < options.voltage_limits.min_pu {
                    voltage_violations.push(VoltageViolation {
                        bus: bus_number,
                        vm_pu,
                        limit_pu: options.voltage_limits.min_pu,
                    });
                } else if vm_pu > options.voltage_limits.max_pu {
                    voltage_violations.push(VoltageViolation {
                        bus: bus_number,
                        vm_pu,
                        limit_pu: options.voltage_limits.max_pu,
                    });
                }
            }

            for (branch_index, &loading_pct_value) in loading_pct.iter().enumerate() {
                if loading_pct_value > 100.0 {
                    thermal_violations.push(ThermalViolation {
                        branch: branch_keys[branch_index].clone(),
                        loading_pct: loading_pct_value,
                    });
                }
            }

            for (index, &loading_pct_value) in loading_pct.iter().enumerate() {
                branch_max_loading[index] = branch_max_loading[index].max(loading_pct_value);
                branch_sum_loading[index] += loading_pct_value;
                if loading_pct_value > 100.0 {
                    branch_periods_overloaded[index] += 1;
                    let rate_a_mva = snapshot.branches[index].rating_a_mva;
                    if rate_a_mva > 0.0 {
                        branch_total_overload_mwh[index] +=
                            (loading_pct_value - 100.0) * rate_a_mva / 100.0 * dt_hours;
                    }
                }
            }

            for (index, &vm_pu) in vm.iter().enumerate() {
                bus_min_voltage[index] = bus_min_voltage[index].min(vm_pu);
                bus_max_voltage[index] = bus_max_voltage[index].max(vm_pu);
                bus_sum_voltage[index] += vm_pu;
                if vm_pu < options.voltage_limits.min_pu {
                    bus_periods_low[index] += 1;
                }
                if vm_pu > options.voltage_limits.max_pu {
                    bus_periods_high[index] += 1;
                }
            }

            total_losses_mwh += total_loss_mw * dt_hours;

            if total_load_mw > peak_load_mw {
                peak_load_mw = total_load_mw;
                peak_load_period = period_idx;
            }

            if let Some(matrix) = bus_voltage_matrix.as_mut() {
                for (index, &value) in vm.iter().enumerate() {
                    matrix[index].push(value);
                }
            }
            if let Some(matrix) = branch_flow_matrix.as_mut() {
                let branch_flows = solution.branch_pq_flows();
                for (index, (p_mw, _q_mvar)) in branch_flows.iter().enumerate() {
                    matrix[index].push(*p_mw);
                }
            }
            if let Some(matrix) = branch_loading_matrix.as_mut() {
                for (index, &value) in loading_pct.iter().enumerate() {
                    matrix[index].push(value);
                }
            }
        } else {
            if let Some(matrix) = bus_voltage_matrix.as_mut() {
                for row in matrix.iter_mut() {
                    row.push(f64::NAN);
                }
            }
            if let Some(matrix) = branch_flow_matrix.as_mut() {
                for row in matrix.iter_mut() {
                    row.push(f64::NAN);
                }
            }
            if let Some(matrix) = branch_loading_matrix.as_mut() {
                for row in matrix.iter_mut() {
                    row.push(f64::NAN);
                }
            }
        }

        periods.push(ReplayPeriod {
            period: period_idx,
            converged,
            solve_time_ms,
            total_load_mw,
            total_loss_mw,
            min_voltage_pu,
            max_voltage_pu,
            peak_branch_loading_pct,
            voltage_violations,
            thermal_violations,
            error,
        });
    }

    let converged_periods = periods.iter().filter(|period| period.converged).count();
    let converged_periods_f64 = converged_periods as f64;
    let peak_loss_mw = periods
        .iter()
        .map(|period| period.total_loss_mw)
        .fold(0.0, f64::max);
    let min_voltage_pu = periods
        .iter()
        .filter_map(|period| period.min_voltage_pu)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let max_voltage_pu = periods
        .iter()
        .filter_map(|period| period.max_voltage_pu)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let voltage_violation_count = periods
        .iter()
        .map(|period| period.voltage_violations.len())
        .sum();
    let thermal_violation_count = periods
        .iter()
        .map(|period| period.thermal_violations.len())
        .sum();

    let branch_summaries = branch_keys
        .iter()
        .enumerate()
        .map(|(index, branch)| BranchThermalSummary {
            branch: branch.clone(),
            max_loading_pct: branch_max_loading[index],
            avg_loading_pct: if converged_periods > 0 {
                branch_sum_loading[index] / converged_periods_f64
            } else {
                0.0
            },
            overloaded_periods: branch_periods_overloaded[index],
            total_overload_mwh: branch_total_overload_mwh[index],
        })
        .collect();

    let bus_summaries = canonical_network
        .buses
        .iter()
        .enumerate()
        .map(|(index, bus)| BusVoltageSummary {
            bus: bus.number,
            min_voltage_pu: if bus_min_voltage[index].is_finite() {
                Some(bus_min_voltage[index])
            } else {
                None
            },
            max_voltage_pu: if bus_max_voltage[index].is_finite() {
                Some(bus_max_voltage[index])
            } else {
                None
            },
            avg_voltage_pu: if converged_periods > 0 {
                Some(bus_sum_voltage[index] / converged_periods_f64)
            } else {
                None
            },
            below_limit_periods: bus_periods_low[index],
            above_limit_periods: bus_periods_high[index],
        })
        .collect();

    if voltage_violation_count > 0 {
        warn!(
            voltage_violation_count = voltage_violation_count,
            "replay_dispatch: voltage violations detected"
        );
    }
    if thermal_violation_count > 0 {
        warn!(
            thermal_violation_count = thermal_violation_count,
            "replay_dispatch: thermal violations detected"
        );
    }

    let result = ReplayResult {
        runtime: options.runtime,
        dt_hours,
        periods,
        summary: ReplaySummary {
            converged_periods,
            peak_loss_mw,
            total_losses_mwh,
            peak_load_mw,
            peak_load_period,
            min_voltage_pu,
            max_voltage_pu,
            voltage_violation_count,
            thermal_violation_count,
            branches: branch_summaries,
            buses: bus_summaries,
        },
        matrices: ReplayMatrices {
            bus_numbers: canonical_network
                .buses
                .iter()
                .map(|bus| bus.number)
                .collect(),
            branch_keys,
            bus_voltages_pu: bus_voltage_matrix,
            branch_flows_mw: branch_flow_matrix,
            branch_loading_pct: branch_loading_matrix,
        },
        total_solve_time_secs: sim_start.elapsed().as_secs_f64(),
    };

    info!(
        converged_periods = result.summary.converged_periods,
        total_solve_time_secs = result.total_solve_time_secs,
        "replay_dispatch: complete"
    );
    Ok(result)
}

/// Replay a solved dispatch schedule through DC or AC power flow.
///
/// This is a convenience wrapper that prepares a temporary [`DispatchModel`]
/// before replay. Prefer [`replay_dispatch_with_model`] or
/// [`DispatchModel::replay_dispatch`] when you already have a prepared model.
pub fn replay_dispatch(
    network: &Network,
    request: &DispatchRequest,
    dispatch: &DispatchSolution,
    options: &ReplayOptions,
) -> Result<ReplayResult, ReplayError> {
    let model =
        DispatchModel::prepare(network).map_err(|error| ReplayError::InvalidConfiguration {
            message: error.to_string(),
        })?;
    replay_dispatch_with_model(&model, request, dispatch, options)
}

impl DispatchModel {
    /// Replay a solved dispatch schedule through DC or AC power flow.
    pub fn replay_dispatch(
        &self,
        request: &DispatchRequest,
        dispatch: &DispatchSolution,
        options: &ReplayOptions,
    ) -> Result<ReplayResult, ReplayError> {
        replay_dispatch_with_model(self, request, dispatch, options)
    }
}

fn validate_inputs(
    network: &Network,
    request: &DispatchRequest,
    normalized: &crate::request::NormalizedDispatchRequest,
    dispatch: &DispatchSolution,
    options: &ReplayOptions,
) -> Result<(), ReplayError> {
    let input = &normalized.input;
    network
        .clone()
        .validate()
        .map_err(|error| ReplayError::InvalidConfiguration {
            message: format!("invalid network: {error}"),
        })?;

    if input.n_periods == 0 {
        return Err(ReplayError::InvalidConfiguration {
            message: "dispatch request must include at least one period".to_string(),
        });
    }
    if input.dt_hours <= 0.0 {
        return Err(ReplayError::InvalidConfiguration {
            message: format!("dt_hours must be > 0.0, got {}", input.dt_hours),
        });
    }
    if options.max_iterations == 0 {
        return Err(ReplayError::InvalidConfiguration {
            message: "max_iterations must be > 0".to_string(),
        });
    }
    if options.tolerance <= 0.0 {
        return Err(ReplayError::InvalidConfiguration {
            message: format!("tolerance must be > 0.0, got {}", options.tolerance),
        });
    }
    if options.voltage_limits.min_pu >= options.voltage_limits.max_pu {
        return Err(ReplayError::InvalidConfiguration {
            message: format!(
                "voltage_limits.min_pu ({}) must be < voltage_limits.max_pu ({})",
                options.voltage_limits.min_pu, options.voltage_limits.max_pu
            ),
        });
    }
    if dispatch.periods.len() != input.n_periods {
        return Err(ReplayError::PeriodCountMismatch {
            expected: input.n_periods,
            actual: dispatch.periods.len(),
        });
    }
    if dispatch.study.formulation != normalized.formulation {
        return Err(ReplayError::IncompatibleDispatch {
            message: format!(
                "solution formulation {:?} does not match replay request {:?}",
                dispatch.study.formulation, normalized.formulation
            ),
        });
    }
    if dispatch.study.coupling != normalized.coupling {
        return Err(ReplayError::IncompatibleDispatch {
            message: format!(
                "solution coupling {:?} does not match replay request {:?}",
                dispatch.study.coupling, normalized.coupling
            ),
        });
    }
    if dispatch.study.commitment != request.commitment().kind() {
        return Err(ReplayError::IncompatibleDispatch {
            message: format!(
                "solution commitment {:?} does not match replay request {:?}",
                dispatch.study.commitment,
                request.commitment().kind()
            ),
        });
    }
    let expected_resources =
        crate::report_ids::build_resource_catalog(network, &normalized.input.dispatchable_loads);
    if dispatch.resources != expected_resources {
        return Err(ReplayError::IncompatibleDispatch {
            message: "resource catalog does not match the replay network/request".to_string(),
        });
    }
    let expected_buses = crate::dispatch::build_bus_catalog(network);
    if dispatch.buses != expected_buses {
        return Err(ReplayError::IncompatibleDispatch {
            message: "bus catalog does not match the replay network/request".to_string(),
        });
    }
    let expected_hvdc: Vec<(String, String)> = normalized
        .input
        .hvdc_links
        .iter()
        .enumerate()
        .map(|(index, link)| {
            (
                crate::dispatch::hvdc_link_id(link, index),
                link.name.clone(),
            )
        })
        .collect();
    for period in &dispatch.periods {
        let actual_hvdc: Vec<(String, String)> = period
            .hvdc_results
            .iter()
            .map(|result| (result.link_id.clone(), result.name.clone()))
            .collect();
        if actual_hvdc != expected_hvdc {
            return Err(ReplayError::IncompatibleDispatch {
                message: format!(
                    "HVDC result catalog for period {} does not match the replay network/request",
                    period.period_index
                ),
            });
        }
    }

    for bus in &network.buses {
        if bus.bus_type != BusType::Slack {
            continue;
        }
        let n_in_service_generators = network
            .generators
            .iter()
            .filter(|generator| generator.in_service && generator.bus == bus.number)
            .count();
        if n_in_service_generators > 1 {
            return Err(ReplayError::InvalidConfiguration {
                message: format!(
                    "cannot compute exact PF generator dispatch with {} in-service generators on slack bus {}; use at most one in-service generator on each slack bus",
                    n_in_service_generators, bus.number
                ),
            });
        }
    }

    Ok(())
}

fn apply_dispatchable_loads_from_dispatch(network: &mut Network, period: &DispatchPeriodResult) {
    for resource in &period.resource_results {
        let Some(bus_number) = resource.bus_number else {
            continue;
        };
        if !matches!(resource.kind, DispatchResourceKind::DispatchableLoad) {
            continue;
        }
        if let ResourcePeriodDetail::DispatchableLoad(detail) = &resource.detail {
            if detail.served_p_mw.abs() > 1e-12 || detail.served_q_mvar.unwrap_or(0.0).abs() > 1e-12
            {
                network.loads.push(surge_network::network::Load::new(
                    bus_number,
                    detail.served_p_mw,
                    detail.served_q_mvar.unwrap_or(0.0),
                ));
            }
        }
    }
}

fn apply_hvdc_dispatch(
    network: &mut Network,
    request_links: &[crate::hvdc::HvdcDispatchLink],
    period: &DispatchPeriodResult,
) {
    if !network.hvdc.has_point_to_point_links() || period.hvdc_results.is_empty() {
        return;
    }

    let dc_line_by_name: HashMap<String, usize> = network
        .hvdc
        .links
        .iter()
        .enumerate()
        .map(|(idx, link)| (link.name().to_string(), idx))
        .collect();

    for (link_idx, hvdc_result) in period.hvdc_results.iter().enumerate() {
        let name = period
            .hvdc_results
            .get(link_idx)
            .map(|result| result.name.as_str())
            .or_else(|| request_links.get(link_idx).map(|link| link.name.as_str()));
        if let Some(name) = name
            && let Some(&dc_idx) = dc_line_by_name.get(name)
        {
            if let Some(dc_line) = network.hvdc.links[dc_idx].as_lcc_mut() {
                dc_line.scheduled_setpoint = hvdc_result.mw;
            } else if let Some(vsc_line) = network.hvdc.links[dc_idx].as_vsc_mut() {
                vsc_line.mode = surge_network::network::VscHvdcControlMode::PowerControl;
                vsc_line.converter1.dc_setpoint = hvdc_result.mw;
                vsc_line.converter2.dc_setpoint = -hvdc_result.mw;
            }
        }
    }
}

fn generator_dispatch_vector(
    dispatch: &DispatchSolution,
    period: &DispatchPeriodResult,
) -> Vec<f64> {
    let resource_dispatch_by_id: HashMap<&str, f64> = period
        .resource_results
        .iter()
        .map(|resource| (resource.resource_id.as_str(), resource.power_mw))
        .collect();

    dispatch
        .resources
        .iter()
        .filter(|resource| resource.kind != DispatchResourceKind::DispatchableLoad)
        .map(|resource| {
            resource_dispatch_by_id
                .get(resource.resource_id.as_str())
                .copied()
                .unwrap_or(0.0)
        })
        .collect()
}

fn solve_period(
    network: &Network,
    options: &ReplayOptions,
    warm_solution: Option<&PfSolution>,
) -> Result<PfSolution, String> {
    match options.runtime {
        ReplaySolver::DcPf => solve_dc(network)
            .map(|result| to_pf_solution(&result, network))
            .map_err(|error| error.to_string()),
        ReplaySolver::AcNewtonRaphson => {
            let solver_options = AcPfOptions {
                tolerance: options.tolerance,
                max_iterations: options.max_iterations as u32,
                flat_start: false,
                warm_start: if options.warm_start {
                    warm_solution.map(surge_ac::WarmStart::from_solution)
                } else {
                    None
                },
                ..AcPfOptions::default()
            };
            solve_ac_pf(network, &solver_options).map_err(|error| error.to_string())
        }
        ReplaySolver::AcFastDecoupled => {
            if let Some(warm) = warm_solution
                && options.warm_start
            {
                let ybus = build_ybus(network);
                let mut factors = FdpfFactors::new(network).map_err(|error| error.to_string())?;
                let p_spec = network.bus_p_injection_pu();
                let q_spec = network.bus_q_injection_pu();
                let result = factors.solve_from_ybus(
                    &ybus,
                    &p_spec,
                    &q_spec,
                    &warm.voltage_magnitude_pu,
                    &warm.voltage_angle_rad,
                    options.tolerance,
                    options.max_iterations as u32,
                );
                return result
                    .map(|fdpf| {
                        let (branch_pf, branch_pt, branch_qf, branch_qt) =
                            compute_branch_power_flows(
                                network,
                                &fdpf.vm,
                                &fdpf.va,
                                network.base_mva,
                            );
                        PfSolution {
                            pf_model: PfModel::Ac,
                            status: SolveStatus::Converged,
                            iterations: fdpf.iterations,
                            max_mismatch: fdpf.max_mismatch,
                            solve_time_secs: 0.0,
                            voltage_magnitude_pu: fdpf.vm,
                            voltage_angle_rad: fdpf.va,
                            active_power_injection_pu: p_spec,
                            reactive_power_injection_pu: q_spec,
                            branch_p_from_mw: branch_pf,
                            branch_p_to_mw: branch_pt,
                            branch_q_from_mvar: branch_qf,
                            branch_q_to_mvar: branch_qt,
                            bus_numbers: network.buses.iter().map(|bus| bus.number).collect(),
                            island_ids: vec![],
                            q_limited_buses: vec![],
                            n_q_limit_switches: 0,
                            gen_slack_contribution_mw: vec![],
                            convergence_history: vec![],
                            worst_mismatch_bus: None,
                            area_interchange: None,
                        }
                    })
                    .ok_or_else(|| "fast-decoupled solver did not converge".to_string());
            }

            let solver_options = FdpfOptions {
                tolerance: options.tolerance,
                max_iterations: options.max_iterations as u32,
                flat_start: false,
                enforce_q_limits: false,
                ..FdpfOptions::default()
            };
            solve_fdpf(network, &solver_options).map_err(|error| error.to_string())
        }
    }
}

fn compute_losses_mw(solution: &PfSolution, network: &Network) -> f64 {
    solution.active_power_injection_pu.iter().sum::<f64>().abs() * network.base_mva
}

/// Write a replay period summary CSV.
pub fn write_replay_csv(result: &ReplayResult, path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;

    let mut file = std::fs::File::create(path)?;
    writeln!(
        file,
        "period,converged,total_load_mw,total_loss_mw,min_voltage_pu,max_voltage_pu,peak_branch_loading_pct,voltage_violations,thermal_violations,error"
    )?;
    for period in &result.periods {
        writeln!(
            file,
            "{},{},{:.6},{:.6},{},{},{:.6},{},{},{}",
            period.period,
            period.converged,
            period.total_load_mw,
            period.total_loss_mw,
            option_to_csv(period.min_voltage_pu),
            option_to_csv(period.max_voltage_pu),
            period.peak_branch_loading_pct,
            period.voltage_violations.len(),
            period.thermal_violations.len(),
            period.error.as_deref().unwrap_or("")
        )?;
    }
    Ok(())
}

/// Write a replay period summary Parquet file.
#[cfg(feature = "parquet")]
pub fn write_replay_parquet(result: &ReplayResult, path: &std::path::Path) -> std::io::Result<()> {
    use arrow_array::{
        BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
    };
    use arrow_schema::{DataType, Field, Schema};
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;

    let schema = Arc::new(Schema::new(vec![
        Field::new("period", DataType::Int64, false),
        Field::new("converged", DataType::Boolean, false),
        Field::new("total_load_mw", DataType::Float64, false),
        Field::new("total_loss_mw", DataType::Float64, false),
        Field::new("min_voltage_pu", DataType::Float64, true),
        Field::new("max_voltage_pu", DataType::Float64, true),
        Field::new("peak_branch_loading_pct", DataType::Float64, false),
        Field::new("voltage_violations", DataType::Int32, false),
        Field::new("thermal_violations", DataType::Int32, false),
        Field::new("error", DataType::Utf8, true),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.period as i64)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(BooleanArray::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.converged)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.total_load_mw)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.total_loss_mw)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.min_voltage_pu)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.max_voltage_pu)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.peak_branch_loading_pct)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Int32Array::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.voltage_violations.len() as i32)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Int32Array::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.thermal_violations.len() as i32)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                result
                    .periods
                    .iter()
                    .map(|period| period.error.clone())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, None).map_err(std::io::Error::other)?;
    writer.write(&batch).map_err(std::io::Error::other)?;
    writer.close().map_err(std::io::Error::other)?;
    Ok(())
}

/// Write a replay period summary Parquet file.
#[cfg(not(feature = "parquet"))]
pub fn write_replay_parquet(
    _result: &ReplayResult,
    _path: &std::path::Path,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "surge-dispatch was built without the `parquet` feature",
    ))
}

fn option_to_csv(value: Option<f64>) -> String {
    value.map(|value| format!("{value:.6}")).unwrap_or_default()
}

impl From<DispatchError> for ReplayError {
    fn from(error: DispatchError) -> Self {
        Self::InvalidRequest(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn build_9bus_network() -> Network {
        let mut network = Network::new("test9");
        network.base_mva = 100.0;

        let mut bus1 = Bus::new(1, BusType::Slack, 138.0);
        bus1.voltage_magnitude_pu = 1.0;
        bus1.voltage_max_pu = 1.1;
        bus1.voltage_min_pu = 0.9;
        network.buses.push(bus1);

        for bus_number in 2..=9 {
            let mut bus = Bus::new(bus_number, BusType::PQ, 138.0);
            bus.voltage_max_pu = 1.1;
            bus.voltage_min_pu = 0.9;
            network.buses.push(bus);
            network
                .loads
                .push(surge_network::network::Load::new(bus_number, 20.0, 5.0));
        }

        let mut generator = Generator::new(1, 160.0, 1.0);
        generator.pmax = 500.0;
        generator.pmin = 0.0;
        generator.qmax = 200.0;
        generator.qmin = -200.0;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 10.0],
        });
        network.generators.push(generator);

        let branch_pairs = [
            (1_u32, 2_u32),
            (2, 3),
            (3, 4),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 8),
            (8, 9),
            (9, 1),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        for (from_bus, to_bus) in branch_pairs {
            let mut branch = Branch::new_line(from_bus, to_bus, 0.01, 0.1, 0.05);
            branch.rating_a_mva = 200.0;
            network.branches.push(branch);
        }

        network
    }

    fn build_request() -> DispatchRequest {
        let network = build_9bus_network();
        let n_periods = 24;
        let profiles = network
            .buses
            .iter()
            .filter(|bus| bus.number != 1)
            .map(|bus| surge_network::market::LoadProfile {
                bus: bus.number,
                load_mw: (0..n_periods)
                    .map(|hour| {
                        let angle =
                            2.0 * std::f64::consts::PI * (hour as f64 - 6.0) / n_periods as f64;
                        20.0 * (1.0 + 0.25 * angle.sin())
                    })
                    .collect(),
            })
            .collect();

        DispatchRequest {
            formulation: crate::Formulation::Dc,
            coupling: crate::IntervalCoupling::TimeCoupled,
            commitment: crate::CommitmentPolicy::AllCommitted,
            timeline: crate::DispatchTimeline {
                periods: n_periods,
                interval_hours: 1.0,
                interval_hours_by_period: Vec::new(),
            },
            profiles: crate::DispatchProfiles {
                load: surge_network::market::LoadProfiles {
                    profiles,
                    n_timesteps: n_periods,
                }
                .into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn build_one_bus_replay_network() -> Network {
        let mut network = Network::new("one_bus_replay");
        network.base_mva = 100.0;
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        network.loads.push(Load::new(1, 80.0, 20.0));

        let mut generator = Generator::new(1, 80.0, 1.0);
        generator.pmax = 200.0;
        generator.pmin = 0.0;
        generator.qmax = 200.0;
        generator.qmin = -200.0;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 10.0],
        });
        network.generators.push(generator);
        network
    }

    fn load_market30_network() -> Network {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let case_path = repo_root.join("examples/cases/market30/market30.surge.json.zst");
        surge_io::load(&case_path).expect("load market30 network")
    }

    #[test]
    fn test_replay_dispatch_dc_24h() {
        let network = build_9bus_network();
        let request = build_request();
        let model = crate::DispatchModel::prepare(&network).unwrap();
        let dispatch = crate::solve_dispatch(&model, &request).unwrap();
        let result = replay_dispatch(
            &network,
            &request,
            &dispatch,
            &ReplayOptions {
                runtime: ReplaySolver::DcPf,
                ..ReplayOptions::default()
            },
        )
        .unwrap();

        assert_eq!(result.periods.len(), 24);
        assert_eq!(result.summary.converged_periods, 24);
    }

    #[test]
    fn test_replay_dispatch_nr_24h() {
        let network = build_9bus_network();
        let request = build_request();
        let model = crate::DispatchModel::prepare(&network).unwrap();
        let dispatch = crate::solve_dispatch(&model, &request).unwrap();
        let result = replay_dispatch(
            &network,
            &request,
            &dispatch,
            &ReplayOptions {
                runtime: ReplaySolver::AcNewtonRaphson,
                max_iterations: 50,
                ..ReplayOptions::default()
            },
        )
        .unwrap();

        assert_eq!(result.periods.len(), 24);
        assert_eq!(result.summary.converged_periods, 24);
    }

    #[test]
    fn test_apply_dispatchable_loads_from_dispatch_preserves_profiled_ac_loads() {
        let mut network = build_one_bus_replay_network();
        let mut fixed0 = Load::new(1, 60.0, 30.0);
        fixed0.id = "L0".to_string();
        let mut fixed1 = Load::new(1, 40.0, 10.0);
        fixed1.id = "L1".to_string();
        network.loads = vec![fixed0, fixed1];

        let request = crate::DispatchRequest {
            formulation: crate::Formulation::Ac,
            coupling: crate::IntervalCoupling::PeriodByPeriod,
            commitment: crate::CommitmentPolicy::AllCommitted,
            timeline: crate::DispatchTimeline {
                periods: 1,
                interval_hours: 1.0,
                interval_hours_by_period: Vec::new(),
            },
            profiles: crate::DispatchProfiles {
                ac_bus_load: crate::AcBusLoadProfiles {
                    profiles: vec![crate::AcBusLoadProfile {
                        bus_number: 1,
                        p_mw: Some(vec![50.0]),
                        q_mvar: Some(vec![12.0]),
                    }],
                },
                ..Default::default()
            },
            ..Default::default()
        };

        let normalized = request.normalize().expect("request should normalize");
        let spec = normalized.problem_spec();
        crate::common::profiles::apply_ac_time_series_profiles(&mut network, &spec, 0);
        apply_dispatchable_loads_from_dispatch(
            &mut network,
            &crate::DispatchPeriodResult::empty(0),
        );

        assert_eq!(
            network.loads.len(),
            2,
            "replay should preserve existing fixed loads"
        );
        assert_eq!(network.loads[0].id, "L0");
        assert_eq!(network.loads[1].id, "L1");

        let total_p: f64 = network
            .loads
            .iter()
            .map(|load| load.active_power_demand_mw)
            .sum();
        let total_q: f64 = network
            .loads
            .iter()
            .map(|load| load.reactive_power_demand_mvar)
            .sum();
        assert!(
            (total_p - 50.0).abs() < 1e-9,
            "expected 50 MW, got {total_p}"
        );
        assert!(
            (total_q - 12.0).abs() < 1e-9,
            "expected 12 MVAr, got {total_q}"
        );
    }

    #[test]
    fn test_replay_dispatch_ac_does_not_require_bus_results() {
        let network = build_one_bus_replay_network();
        let request = crate::DispatchRequest {
            formulation: crate::Formulation::Ac,
            coupling: crate::IntervalCoupling::PeriodByPeriod,
            commitment: crate::CommitmentPolicy::AllCommitted,
            timeline: crate::DispatchTimeline {
                periods: 1,
                interval_hours: 1.0,
                interval_hours_by_period: Vec::new(),
            },
            profiles: crate::DispatchProfiles {
                ac_bus_load: crate::AcBusLoadProfiles {
                    profiles: vec![crate::AcBusLoadProfile {
                        bus_number: 1,
                        p_mw: Some(vec![50.0]),
                        q_mvar: Some(vec![12.0]),
                    }],
                },
                ..Default::default()
            },
            ..Default::default()
        };

        let model = crate::DispatchModel::prepare(&network).expect("prepare dispatch model");
        let mut dispatch = crate::solve_dispatch(&model, &request).expect("dispatch should solve");
        dispatch.periods[0].bus_results.clear();

        let replay = replay_dispatch(
            &network,
            &request,
            &dispatch,
            &ReplayOptions {
                runtime: ReplaySolver::DcPf,
                ..ReplayOptions::default()
            },
        )
        .expect("replay should reconstruct fixed AC loads without bus_results");

        assert_eq!(replay.summary.converged_periods, 1);
        assert!(
            (replay.periods[0].total_load_mw - 50.0).abs() < 1e-6,
            "replay total load should reflect the AC active override, got {}",
            replay.periods[0].total_load_mw
        );
    }

    #[test]
    fn test_apply_hvdc_dispatch_updates_market30_vsc_link_setpoints() {
        let mut network = load_market30_network();
        let link_name = network.hvdc.links[0].name().to_string();
        let dispatch_mw = 62.5;
        let mut period = crate::DispatchPeriodResult::empty(0);
        period.hvdc_results = vec![crate::HvdcPeriodResult {
            link_id: link_name.clone(),
            name: link_name,
            mw: dispatch_mw,
            delivered_mw: dispatch_mw,
            band_results: Vec::new(),
        }];

        apply_hvdc_dispatch(&mut network, &[], &period);

        let vsc = network.hvdc.links[0]
            .as_vsc()
            .expect("market30 link should remain a VSC link");
        assert_eq!(
            vsc.mode,
            surge_network::network::VscHvdcControlMode::PowerControl
        );
        assert!(
            (vsc.converter1.dc_setpoint - dispatch_mw).abs() < 1e-9,
            "converter1 setpoint should track replayed dispatch"
        );
        assert!(
            (vsc.converter2.dc_setpoint + dispatch_mw).abs() < 1e-9,
            "converter2 setpoint should mirror replayed dispatch with opposite sign"
        );
    }

    #[test]
    fn test_write_replay_csv() {
        let network = build_9bus_network();
        let request = build_request();
        let model = crate::DispatchModel::prepare(&network).unwrap();
        let dispatch = crate::solve_dispatch(&model, &request).unwrap();
        let result =
            replay_dispatch(&network, &request, &dispatch, &ReplayOptions::default()).unwrap();
        let path = std::env::temp_dir().join("surge-dispatch-replay.csv");
        write_replay_csv(&result, &path).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("period,converged"));
    }

    #[cfg(feature = "parquet")]
    #[test]
    fn test_write_replay_parquet() {
        let network = build_9bus_network();
        let request = build_request();
        let model = crate::DispatchModel::prepare(&network).unwrap();
        let dispatch = crate::solve_dispatch(&model, &request).unwrap();
        let result =
            replay_dispatch(&network, &request, &dispatch, &ReplayOptions::default()).unwrap();
        let path = std::env::temp_dir().join("surge-dispatch-replay.parquet");
        write_replay_parquet(&result, &path).unwrap();
        assert!(path.exists());
    }

    #[cfg(not(feature = "parquet"))]
    #[test]
    fn test_write_replay_parquet_requires_feature() {
        let network = build_9bus_network();
        let request = build_request();
        let model = crate::DispatchModel::prepare(&network).unwrap();
        let dispatch = crate::solve_dispatch(&model, &request).unwrap();
        let result =
            replay_dispatch(&network, &request, &dispatch, &ReplayOptions::default()).unwrap();
        let path = std::env::temp_dir().join("surge-dispatch-replay.parquet");
        let error = write_replay_parquet(&result, &path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }
}
