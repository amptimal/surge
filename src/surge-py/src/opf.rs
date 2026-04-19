// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::exceptions::{
    catch_panic, extract_panic_msg, to_ac_opf_pyerr, to_dc_opf_pyerr, to_pyerr, to_scopf_pyerr,
};
use crate::network::Network;
use crate::solutions::{
    AcOpfBendersSubproblemResult, AcOpfHvdcResult, BindingContingency, ContingencyViolation,
    DcOpfResult, FailedContingencyEvaluation, OpfSolution, ScopfResult, ScopfScreeningStats,
};

// ── Private helpers ───────────────────────────────────────────────────────────

fn scopf_formulation_name(formulation: surge_opf::ScopfFormulation) -> &'static str {
    match formulation {
        surge_opf::ScopfFormulation::Dc => "dc",
        surge_opf::ScopfFormulation::Ac => "ac",
    }
}

fn scopf_mode_name(mode: surge_opf::ScopfMode) -> &'static str {
    match mode {
        surge_opf::ScopfMode::Preventive => "preventive",
        surge_opf::ScopfMode::Corrective => "corrective",
    }
}

fn parse_scopf_cut_kind(kind: &str) -> PyResult<surge_opf::security::ScopfCutKind> {
    match kind {
        "BranchThermal" | "branch_thermal" | "branch-thermal" => {
            Ok(surge_opf::security::ScopfCutKind::BranchThermal)
        }
        "GeneratorTrip" | "generator_trip" | "generator-trip" => {
            Ok(surge_opf::security::ScopfCutKind::GeneratorTrip)
        }
        "MultiBranchN2" | "multi_branch_n2" | "multi-branch-n2" => {
            Ok(surge_opf::security::ScopfCutKind::MultiBranchN2)
        }
        other => Err(PyValueError::new_err(format!(
            "Unknown SCOPF cut kind '{other}' in warm start"
        ))),
    }
}

pub fn parse_virtual_bids(
    py: Python<'_>,
    bids: Option<Vec<std::collections::HashMap<String, pyo3::Py<pyo3::PyAny>>>>,
) -> Vec<surge_network::market::VirtualBid> {
    let Some(list) = bids else {
        return vec![];
    };
    list.into_iter()
        .map(|d| {
            let get_u32 = |k: &str| {
                d.get(k)
                    .and_then(|v| v.extract::<u32>(py).ok())
                    .unwrap_or(0)
            };
            let get_usize = |k: &str| {
                d.get(k)
                    .and_then(|v| v.extract::<usize>(py).ok())
                    .unwrap_or(0)
            };
            let get_f64 = |k: &str, def: f64| {
                d.get(k)
                    .and_then(|v| v.extract::<f64>(py).ok())
                    .unwrap_or(def)
            };
            let get_bool = |k: &str, def: bool| {
                d.get(k)
                    .and_then(|v| v.extract::<bool>(py).ok())
                    .unwrap_or(def)
            };
            let dir_str = d
                .get("direction")
                .and_then(|v| v.extract::<String>(py).ok())
                .unwrap_or_else(|| "inc".to_string());
            let direction = if dir_str.to_lowercase() == "dec" {
                surge_network::market::VirtualBidDirection::Dec
            } else {
                surge_network::market::VirtualBidDirection::Inc
            };
            surge_network::market::VirtualBid {
                position_id: d
                    .get("position_id")
                    .and_then(|v| v.extract::<String>(py).ok())
                    .unwrap_or_default(),
                bus: get_u32("bus"),
                period: get_usize("period"),
                mw_limit: get_f64("mw_limit", 0.0),
                price_per_mwh: get_f64("price_per_mwh", 0.0),
                direction,
                in_service: get_bool("in_service", true),
            }
        })
        .collect()
}

fn build_dc_opf_request(
    py: Python<'_>,
    tolerance: f64,
    enforce_thermal_limits: bool,
    lp_solver: Option<&str>,
    use_pwl_costs: bool,
    pwl_cost_breakpoints: usize,
    enforce_flowgates: bool,
    warm_start_theta: Option<Vec<f64>>,
    par_setpoints: Option<Vec<std::collections::HashMap<String, pyo3::Py<pyo3::PyAny>>>>,
    hvdc_links: Option<Vec<std::collections::HashMap<String, pyo3::Py<pyo3::PyAny>>>>,
    gen_limit_penalty: Option<f64>,
    virtual_bids: Option<Vec<std::collections::HashMap<String, pyo3::Py<pyo3::PyAny>>>>,
    max_iterations: u32,
    min_rate_a: f64,
    use_loss_factors: bool,
    max_loss_iter: usize,
    loss_tol: f64,
) -> PyResult<(surge_opf::DcOpfOptions, surge_opf::DcOpfRuntime)> {
    let solver = lp_solver
        .map(|s| surge_opf::backends::lp_solver_from_str(s).map_err(to_pyerr))
        .transpose()?;
    let parsed_par_setpoints: Vec<surge_solution::ParSetpoint> = par_setpoints
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| {
            let get_u32 =
                |k: &str| -> Option<u32> { d.get(k).and_then(|v| v.extract::<u32>(py).ok()) };
            let get_str = |k: &str| -> String {
                d.get(k)
                    .and_then(|v| v.extract::<String>(py).ok())
                    .unwrap_or_else(|| "1".to_string())
            };
            let get_f64 =
                |k: &str| -> Option<f64> { d.get(k).and_then(|v| v.extract::<f64>(py).ok()) };
            Some(surge_solution::ParSetpoint {
                from_bus: get_u32("from_bus")?,
                to_bus: get_u32("to_bus")?,
                circuit: get_str("circuit"),
                target_mw: get_f64("target_mw")?,
            })
        })
        .collect();
    let parsed_hvdc_links: Option<Vec<surge_opf::HvdcOpfLink>> = hvdc_links.map(|links| {
        links
            .into_iter()
            .filter_map(|d| {
                let get_u32 =
                    |k: &str| -> Option<u32> { d.get(k).and_then(|v| v.extract::<u32>(py).ok()) };
                let get_f64 =
                    |k: &str| -> Option<f64> { d.get(k).and_then(|v| v.extract::<f64>(py).ok()) };
                let get_f64_or = |k: &str, def: f64| -> f64 { get_f64(k).unwrap_or(def) };
                Some(surge_opf::HvdcOpfLink {
                    from_bus: get_u32("from_bus")?,
                    to_bus: get_u32("to_bus")?,
                    p_dc_min_mw: get_f64("p_dc_min_mw")?,
                    p_dc_max_mw: get_f64("p_dc_max_mw")?,
                    loss_a_mw: get_f64_or("loss_a_mw", 0.0),
                    loss_b_frac: get_f64_or("loss_b_frac", 0.0),
                    name: String::new(),
                })
            })
            .collect()
    });
    let parsed_virtual_bids = parse_virtual_bids(py, virtual_bids);
    let opts = surge_opf::DcOpfOptions {
        tolerance,
        max_iterations,
        enforce_thermal_limits,
        min_rate_a,
        use_pwl_costs,
        pwl_cost_breakpoints,
        enforce_flowgates,
        par_setpoints: parsed_par_setpoints,
        hvdc_links: parsed_hvdc_links,
        gen_limit_penalty,
        virtual_bids: parsed_virtual_bids,
        use_loss_factors,
        max_loss_iter,
        loss_tol,
        ..Default::default()
    };
    let mut runtime = surge_opf::DcOpfRuntime::default();
    if let Some(solver) = solver {
        runtime = runtime.with_lp_solver(solver);
    }
    if let Some(theta) = warm_start_theta {
        runtime = runtime.with_warm_start_theta(theta);
    }
    Ok((opts, runtime))
}

fn build_ac_opf_request(
    network: &surge_network::Network,
    tolerance: f64,
    max_iterations: u32,
    exact_hessian: bool,
    nlp_solver: Option<&str>,
    print_level: i32,
    enforce_thermal_limits: bool,
    thermal_limit_slack_penalty_per_mva: f64,
    min_rate_a: f64,
    enforce_angle_limits: bool,
    warm_start: Option<&OpfSolution>,
    warm_start_vm_pu: Option<Vec<f64>>,
    warm_start_va_rad: Option<Vec<f64>>,
    use_dc_opf_warm_start: Option<bool>,
    optimize_switched_shunts: bool,
    optimize_taps: bool,
    optimize_phase_shifters: bool,
    include_hvdc: Option<bool>,
    enforce_capability_curves: bool,
    discrete_mode: &str,
    optimize_svc: bool,
    optimize_tcsc: bool,
    dt_hours: f64,
    enforce_flowgates: bool,
    constraint_screening_threshold: Option<f64>,
    constraint_screening_min_buses: usize,
    screening_fallback_enabled: bool,
    storage_soc_override: Option<std::collections::HashMap<usize, f64>>,
) -> PyResult<(surge_opf::AcOpfOptions, surge_opf::AcOpfRuntime)> {
    let solver = nlp_solver
        .map(|s| surge_opf::backends::ac_opf_nlp_solver_from_str(s).map_err(to_pyerr))
        .transpose()?;
    let dm = match discrete_mode {
        "continuous" => surge_opf::DiscreteMode::Continuous,
        "round_and_check" | "round-and-check" => surge_opf::DiscreteMode::RoundAndCheck,
        other => {
            return Err(PyValueError::new_err(format!(
                "discrete_mode must be 'continuous' or 'round_and_check', got '{other}'"
            )));
        }
    };
    let opts = surge_opf::AcOpfOptions {
        tolerance,
        max_iterations,
        exact_hessian,
        print_level,
        enforce_thermal_limits,
        thermal_limit_slack_penalty_per_mva,
        bus_active_power_balance_slack_penalty_per_mw: 0.0,
        bus_reactive_power_balance_slack_penalty_per_mvar: 0.0,
        voltage_magnitude_slack_penalty_per_pu: 0.0,
        angle_difference_slack_penalty_per_rad: 0.0,
        min_rate_a,
        enforce_angle_limits,
        optimize_switched_shunts,
        optimize_taps,
        optimize_phase_shifters,
        optimize_svc,
        optimize_tcsc,
        include_hvdc,
        storage_soc_override,
        dt_hours,
        enforce_flowgates,
        constraint_screening_threshold,
        constraint_screening_min_buses,
        screening_fallback_enabled,
        enforce_capability_curves,
        enforce_regulated_bus_vm_targets: true,
        discrete_mode: dm,
        discrete_polish: true,
    };
    let mut runtime = surge_opf::AcOpfRuntime::default();
    if let Some(solver) = solver {
        runtime = runtime.with_nlp_solver(solver);
    }
    let expected_buses = network.buses.len();
    if let Some(ref voltage_magnitudes) = warm_start_vm_pu
        && voltage_magnitudes.len() != expected_buses
    {
        return Err(PyValueError::new_err(format!(
            "warm_start_vm_pu length {} does not match network bus count {}",
            voltage_magnitudes.len(),
            expected_buses
        )));
    }
    if let Some(ref voltage_angles) = warm_start_va_rad
        && voltage_angles.len() != expected_buses
    {
        return Err(PyValueError::new_err(format!(
            "warm_start_va_rad length {} does not match network bus count {}",
            voltage_angles.len(),
            expected_buses
        )));
    }
    if warm_start.is_some() || warm_start_vm_pu.is_some() || warm_start_va_rad.is_some() {
        let mut seed = warm_start
            .map(|prior| surge_opf::WarmStart::from_opf(&prior.inner))
            .unwrap_or_else(|| surge_opf::WarmStart {
                voltage_magnitude_pu: network
                    .buses
                    .iter()
                    .map(|bus| bus.voltage_magnitude_pu)
                    .collect(),
                voltage_angle_rad: network
                    .buses
                    .iter()
                    .map(|bus| bus.voltage_angle_rad)
                    .collect(),
                pg: Vec::new(),
                qg: Vec::new(),
                dispatchable_load_p: Vec::new(),
                dispatchable_load_q: Vec::new(),
            });
        if let Some(voltage_magnitudes) = warm_start_vm_pu {
            seed.voltage_magnitude_pu = voltage_magnitudes;
        }
        if let Some(voltage_angles) = warm_start_va_rad {
            seed.voltage_angle_rad = voltage_angles;
        }
        runtime = runtime.with_warm_start(seed);
    }
    if let Some(enabled) = use_dc_opf_warm_start {
        runtime = runtime.with_dc_opf_warm_start(enabled);
    }
    Ok((opts, runtime))
}

// ── DC OPF ────────────────────────────────────────────────────────────────────

/// Solve DC Optimal Power Flow and return the full solver result surface.
#[pyfunction]
#[pyo3(signature = (network, tolerance=1e-8, enforce_thermal_limits=true, lp_solver=None,
                    use_pwl_costs=false, pwl_cost_breakpoints=20, enforce_flowgates=true,
                    warm_start_theta=None, par_setpoints=None, hvdc_links=None,
                    gen_limit_penalty=None, virtual_bids=None, max_iterations=200,
                    min_rate_a=1.0, use_loss_factors=false, max_loss_iter=3, loss_tol=1e-3))]
pub fn solve_dc_opf_full(
    py: Python<'_>,
    network: &Network,
    tolerance: f64,
    enforce_thermal_limits: bool,
    lp_solver: Option<&str>,
    use_pwl_costs: bool,
    pwl_cost_breakpoints: usize,
    enforce_flowgates: bool,
    warm_start_theta: Option<Vec<f64>>,
    par_setpoints: Option<Vec<std::collections::HashMap<String, pyo3::Py<pyo3::PyAny>>>>,
    hvdc_links: Option<Vec<std::collections::HashMap<String, pyo3::Py<pyo3::PyAny>>>>,
    gen_limit_penalty: Option<f64>,
    virtual_bids: Option<Vec<std::collections::HashMap<String, pyo3::Py<pyo3::PyAny>>>>,
    max_iterations: u32,
    min_rate_a: f64,
    use_loss_factors: bool,
    max_loss_iter: usize,
    loss_tol: f64,
) -> PyResult<DcOpfResult> {
    let (opts, runtime) = build_dc_opf_request(
        py,
        tolerance,
        enforce_thermal_limits,
        lp_solver,
        use_pwl_costs,
        pwl_cost_breakpoints,
        enforce_flowgates,
        warm_start_theta,
        par_setpoints,
        hvdc_links,
        gen_limit_penalty,
        virtual_bids,
        max_iterations,
        min_rate_a,
        use_loss_factors,
        max_loss_iter,
        loss_tol,
    )?;
    let net = Arc::clone(&network.inner);
    let full_result = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_opf::dc::solve_dc_opf_with_runtime(&net, &opts, &runtime)
            }))
            .map_err(|e| {
                surge_opf::dc::opf::DcOpfError::SolverError(format!(
                    "solve_dc_opf_full failed: {}",
                    extract_panic_msg(e)
                ))
            })
            .and_then(|r| r)
        })
        .map_err(|e| to_dc_opf_pyerr(&e))?;
    Ok(DcOpfResult {
        opf: OpfSolution::from_core(full_result.opf, Arc::clone(&network.inner)),
        hvdc_dispatch_mw: full_result.hvdc_dispatch_mw,
        hvdc_shadow_prices: full_result.hvdc_shadow_prices,
        gen_limit_violations: full_result.gen_limit_violations,
        is_feasible: full_result.is_feasible,
    })
}

// ── AC OPF ────────────────────────────────────────────────────────────────────

fn solve_ac_opf_impl(
    py: Python<'_>,
    network: &Network,
    tolerance: f64,
    max_iterations: u32,
    exact_hessian: bool,
    nlp_solver: Option<&str>,
    print_level: i32,
    enforce_thermal_limits: bool,
    thermal_limit_slack_penalty_per_mva: f64,
    min_rate_a: f64,
    enforce_angle_limits: bool,
    warm_start: Option<&OpfSolution>,
    warm_start_vm_pu: Option<Vec<f64>>,
    warm_start_va_rad: Option<Vec<f64>>,
    use_dc_opf_warm_start: Option<bool>,
    optimize_switched_shunts: bool,
    optimize_taps: bool,
    optimize_phase_shifters: bool,
    include_hvdc: Option<bool>,
    enforce_capability_curves: bool,
    discrete_mode: &str,
    optimize_svc: bool,
    optimize_tcsc: bool,
    dt_hours: f64,
    enforce_flowgates: bool,
    constraint_screening_threshold: Option<f64>,
    constraint_screening_min_buses: usize,
    screening_fallback_enabled: bool,
    storage_soc_override: Option<std::collections::HashMap<usize, f64>>,
) -> PyResult<AcOpfHvdcResult> {
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return Err(PyValueError::new_err(format!(
            "tolerance must be a finite positive number, got {tolerance}"
        )));
    }
    network.validate()?;
    let (opts, runtime) = build_ac_opf_request(
        network.inner.as_ref(),
        tolerance,
        max_iterations,
        exact_hessian,
        nlp_solver,
        print_level,
        enforce_thermal_limits,
        thermal_limit_slack_penalty_per_mva,
        min_rate_a,
        enforce_angle_limits,
        warm_start,
        warm_start_vm_pu,
        warm_start_va_rad,
        use_dc_opf_warm_start,
        optimize_switched_shunts,
        optimize_taps,
        optimize_phase_shifters,
        include_hvdc,
        enforce_capability_curves,
        discrete_mode,
        optimize_svc,
        optimize_tcsc,
        dt_hours,
        enforce_flowgates,
        constraint_screening_threshold,
        constraint_screening_min_buses,
        screening_fallback_enabled,
        storage_soc_override,
    )?;
    let net = Arc::clone(&network.inner);
    let result = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_opf::ac::hvdc::solve_ac_opf_with_hvdc_with_runtime(&net, &opts, &runtime)
            }))
            .map_err(|e| {
                surge_opf::ac::types::AcOpfError::SolverError(format!(
                    "solve_ac_opf failed: {}",
                    extract_panic_msg(e)
                ))
            })
            .and_then(|r| r)
        })
        .map_err(|e| to_ac_opf_pyerr(&e))?;
    Ok(AcOpfHvdcResult {
        opf: OpfSolution::from_core(result.opf, Arc::clone(&network.inner)),
        hvdc_p_dc_mw: result.hvdc_p_dc_mw,
        hvdc_p_loss_mw: result.hvdc_p_loss_mw,
        hvdc_iterations: result.hvdc_iterations,
    })
}

/// Solve AC Optimal Power Flow with HVDC result reporting.
#[pyfunction]
#[pyo3(signature = (network, tolerance=1e-8, max_iterations=0, exact_hessian=true, nlp_solver=None,
                    print_level=0, enforce_thermal_limits=true,
                    thermal_limit_slack_penalty_per_mva=0.0, min_rate_a=1.0,
                    enforce_angle_limits=false, warm_start=None,
                    warm_start_vm_pu=None, warm_start_va_rad=None,
                    use_dc_opf_warm_start=None,
                    optimize_switched_shunts=false, optimize_taps=false,
                    optimize_phase_shifters=false, include_hvdc=None,
                    enforce_capability_curves=true, discrete_mode="continuous",
                    optimize_svc=false, optimize_tcsc=false, dt_hours=1.0,
                    enforce_flowgates=false, constraint_screening_threshold=None,
                    constraint_screening_min_buses=1000, screening_fallback_enabled=false,
                    storage_soc_override=None))]
pub fn solve_ac_opf(
    py: Python<'_>,
    network: &Network,
    tolerance: f64,
    max_iterations: u32,
    exact_hessian: bool,
    nlp_solver: Option<&str>,
    print_level: i32,
    enforce_thermal_limits: bool,
    thermal_limit_slack_penalty_per_mva: f64,
    min_rate_a: f64,
    enforce_angle_limits: bool,
    warm_start: Option<&OpfSolution>,
    warm_start_vm_pu: Option<Vec<f64>>,
    warm_start_va_rad: Option<Vec<f64>>,
    use_dc_opf_warm_start: Option<bool>,
    optimize_switched_shunts: bool,
    optimize_taps: bool,
    optimize_phase_shifters: bool,
    include_hvdc: Option<bool>,
    enforce_capability_curves: bool,
    discrete_mode: &str,
    optimize_svc: bool,
    optimize_tcsc: bool,
    dt_hours: f64,
    enforce_flowgates: bool,
    constraint_screening_threshold: Option<f64>,
    constraint_screening_min_buses: usize,
    screening_fallback_enabled: bool,
    storage_soc_override: Option<std::collections::HashMap<usize, f64>>,
) -> PyResult<AcOpfHvdcResult> {
    solve_ac_opf_impl(
        py,
        network,
        tolerance,
        max_iterations,
        exact_hessian,
        nlp_solver,
        print_level,
        enforce_thermal_limits,
        thermal_limit_slack_penalty_per_mva,
        min_rate_a,
        enforce_angle_limits,
        warm_start,
        warm_start_vm_pu,
        warm_start_va_rad,
        use_dc_opf_warm_start,
        optimize_switched_shunts,
        optimize_taps,
        optimize_phase_shifters,
        include_hvdc,
        enforce_capability_curves,
        discrete_mode,
        optimize_svc,
        optimize_tcsc,
        dt_hours,
        enforce_flowgates,
        constraint_screening_threshold,
        constraint_screening_min_buses,
        screening_fallback_enabled,
        storage_soc_override,
    )
}

// ── SCED-AC Benders subproblem ───────────────────────────────────────────────

/// Solve an AC Optimal Power Flow with the active-power dispatch of selected
/// generators **fixed** to caller-supplied target values, and return the
/// structured result required for a SCED-AC Benders master cut.
///
/// This is the AC subproblem half of the SCED-AC Benders decomposition: the
/// master is a DC SCED LP augmented with an epigraph variable ``η[t]`` and
/// accumulating linear cuts, and the subproblem is this function which
/// supplies fresh cuts of the form
///
///     η[t] ≥ slack_cost(P̃g) + Σ_g λ_g · (Pg[g,t] − P̃g_g)
///
/// where ``P̃g`` is the master's current dispatch proposal and ``λ_g`` is
/// ``slack_marginal_by_id[g]``.
///
/// Args:
///     network:             Power system network with generator cost curves.
///     fixed_p_mw:          Map keyed by **generator resource_id (string)**,
///                          giving the MW dispatch at which each generator
///                          should be pinned. Out-of-service generators are
///                          silently skipped. Targets outside a generator's
///                          ``[pmin, pmax]`` envelope are clipped to the
///                          envelope before the NLP sees them.
///     tolerance:           NLP convergence tolerance (default 1e-8).
///     max_iterations:      NLP max iterations (default 0 = backend default).
///     nlp_solver:          NLP backend ("ipopt", "copt", "gurobi", or None
///                          for the default runtime).
///     print_level:         NLP print level (default 0).
///     enforce_thermal_limits:     Enforce branch thermal limits.
///     thermal_limit_slack_penalty_per_mva: Slack penalty coefficient. When
///         this is > 0 (default 1e4 for Benders), the AC OPF can absorb
///         thermal overloads into a slack variable priced at this rate,
///         which keeps the subproblem feasible even when the DC dispatch
///         proposes a schedule the AC network cannot physically deliver.
///     bus_active_power_balance_slack_penalty_per_mw:
///         Bus-balance P slack penalty ($/MW). Default 1e4 gives the
///         subproblem a graceful fallback when the master proposes an
///         infeasible dispatch.
///     bus_reactive_power_balance_slack_penalty_per_mvar:
///         Bus-balance Q slack penalty ($/MVAr). Default 1e4.
///     enforce_angle_limits:       Enforce branch angle limits.
///     enforce_capability_curves:  Enforce Q capability curves.
///     include_hvdc:        HVDC policy (None = auto).
///     dt_hours:            Period length in hours (default 1.0).
///
/// Returns:
///     AcOpfBendersSubproblemResult with:
///       - ``opf``                         — full AC-OPF solution at the fix,
///       - ``slack_cost_dollars_per_hour`` — aggregate soft-penalty cost,
///       - ``slack_marginal_by_id``        — per-gen Benders cut coefficients.
#[pyfunction]
#[pyo3(signature = (network, fixed_p_mw, tolerance=1e-8, max_iterations=0,
                    exact_hessian=true, nlp_solver=None, print_level=0,
                    enforce_thermal_limits=true,
                    thermal_limit_slack_penalty_per_mva=1.0e4,
                    bus_active_power_balance_slack_penalty_per_mw=1.0e4,
                    bus_reactive_power_balance_slack_penalty_per_mvar=1.0e4,
                    min_rate_a=1.0,
                    enforce_angle_limits=false,
                    enforce_capability_curves=true,
                    include_hvdc=None, dt_hours=1.0))]
#[allow(clippy::too_many_arguments)]
pub fn solve_ac_opf_subproblem(
    py: Python<'_>,
    network: &Network,
    fixed_p_mw: std::collections::HashMap<String, f64>,
    tolerance: f64,
    max_iterations: u32,
    exact_hessian: bool,
    nlp_solver: Option<&str>,
    print_level: i32,
    enforce_thermal_limits: bool,
    thermal_limit_slack_penalty_per_mva: f64,
    bus_active_power_balance_slack_penalty_per_mw: f64,
    bus_reactive_power_balance_slack_penalty_per_mvar: f64,
    min_rate_a: f64,
    enforce_angle_limits: bool,
    enforce_capability_curves: bool,
    include_hvdc: Option<bool>,
    dt_hours: f64,
) -> PyResult<AcOpfBendersSubproblemResult> {
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return Err(PyValueError::new_err(format!(
            "tolerance must be a finite positive number, got {tolerance}"
        )));
    }
    network.validate()?;

    // Translate the resource_id → MW map into the global-index map that
    // `solve_ac_opf_subproblem` expects. We rely on `network.inner.generators`
    // being index-stable within a single solve call.
    let mut id_to_global: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::with_capacity(network.inner.generators.len());
    for (i, g) in network.inner.generators.iter().enumerate() {
        id_to_global.insert(g.id.as_str(), i);
    }
    let mut fixed_by_index: std::collections::HashMap<usize, f64> =
        std::collections::HashMap::with_capacity(fixed_p_mw.len());
    for (resource_id, p_mw) in fixed_p_mw.iter() {
        if !p_mw.is_finite() {
            return Err(PyValueError::new_err(format!(
                "fixed_p_mw[{resource_id}] is not finite: {p_mw}"
            )));
        }
        match id_to_global.get(resource_id.as_str()) {
            Some(&gi) => {
                fixed_by_index.insert(gi, *p_mw);
            }
            None => {
                return Err(PyValueError::new_err(format!(
                    "fixed_p_mw key '{resource_id}' does not match any generator id in the network"
                )));
            }
        }
    }

    let (mut opts, runtime) = build_ac_opf_request(
        network.inner.as_ref(),
        tolerance,
        max_iterations,
        exact_hessian,
        nlp_solver,
        print_level,
        enforce_thermal_limits,
        thermal_limit_slack_penalty_per_mva,
        min_rate_a,
        enforce_angle_limits,
        /* warm_start */ None,
        /* warm_start_vm_pu */ None,
        /* warm_start_va_rad */ None,
        /* use_dc_opf_warm_start */ None,
        /* optimize_switched_shunts */ false,
        /* optimize_taps */ false,
        /* optimize_phase_shifters */ false,
        include_hvdc,
        enforce_capability_curves,
        "continuous",
        /* optimize_svc */ false,
        /* optimize_tcsc */ false,
        dt_hours,
        /* enforce_flowgates */ false,
        /* constraint_screening_threshold */ None,
        /* constraint_screening_min_buses */ 1000,
        /* screening_fallback_enabled */ false,
        /* storage_soc_override */ None,
    )?;
    // `build_ac_opf_request` hard-codes both bus-balance slack penalties to
    // zero, which makes the AC OPF infeasible whenever the master's fixed
    // Pg proposal violates Kirchhoff's Q law (common on the first iteration
    // of a Benders loop). Override them here so the subproblem is always
    // feasible and the slack cost captures the infeasibility magnitude.
    opts.bus_active_power_balance_slack_penalty_per_mw =
        bus_active_power_balance_slack_penalty_per_mw;
    opts.bus_reactive_power_balance_slack_penalty_per_mvar =
        bus_reactive_power_balance_slack_penalty_per_mvar;

    let net = Arc::clone(&network.inner);
    let subproblem = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_opf::ac::solve_ac_opf_subproblem(&net, &opts, &runtime, &fixed_by_index)
            }))
            .map_err(|e| {
                surge_opf::ac::types::AcOpfError::SolverError(format!(
                    "solve_ac_opf_subproblem failed: {}",
                    extract_panic_msg(e)
                ))
            })
            .and_then(|r| r)
        })
        .map_err(|e| to_ac_opf_pyerr(&e))?;

    // Re-key the marginal map from global index → resource id so Python users
    // can correlate against their own resource tables without touching the
    // network generator vector.
    let mut slack_marginal_by_id: std::collections::HashMap<String, f64> =
        std::collections::HashMap::with_capacity(
            subproblem.slack_marginal_dollars_per_mw_per_hour.len(),
        );
    for (gi, marginal) in subproblem.slack_marginal_dollars_per_mw_per_hour.iter() {
        if let Some(generator) = network.inner.generators.get(*gi) {
            slack_marginal_by_id.insert(generator.id.clone(), *marginal);
        }
    }

    Ok(AcOpfBendersSubproblemResult {
        opf: OpfSolution::from_core(subproblem.solution, Arc::clone(&network.inner)),
        slack_cost_dollars_per_hour: subproblem.slack_cost_dollars_per_hour,
        slack_marginal_by_id,
        converged: subproblem.converged,
    })
}

// ── SCOPF ─────────────────────────────────────────────────────────────────────

/// Solve Security-Constrained OPF (unified API).
///
/// Dispatches to DC or AC formulation, preventive or corrective mode,
/// based on the ``formulation`` and ``mode`` parameters.
///
/// Args:
///     network:                    Power system network with generator cost curves.
///     formulation:                "dc" (default) or "ac".
///     mode:                       "preventive" (default) or "corrective".
///     tolerance:                  Violation tolerance in p.u. (default 0.01).
///     max_iterations:             Maximum constraint-generation iterations (default 20).
///     max_cuts_per_iteration:     Maximum cuts per iteration (default 100).
///     corrective_ramp_window_min: Corrective ramp window in minutes (default 10.0).
///     voltage_threshold:          Voltage violation threshold in p.u. (default 0.01).
///     contingency_rating:         Thermal rating for post-contingency limits: "rate-a" (default), "rate-b", "rate-c".
///     enforce_flowgates:          Enforce flowgate/interface constraints (default True).
///     enforce_voltage_security:   Enforce post-contingency voltage limits in AC-SCOPF (default True).
///     lp_solver:                  LP solver backend name for DC:
///                                 "default", "highs", "gurobi", "cplex", or "copt".
///     nlp_solver:                 NLP solver backend name for AC:
///                                 "default", "ipopt", "copt", or "gurobi".
///
/// Returns:
///     ScopfResult with the base-case OPF, screening stats, and contingency metadata.
#[pyfunction]
#[pyo3(signature = (network, formulation="dc", mode="preventive", tolerance=0.01,
                    max_iterations=20, max_cuts_per_iteration=100,
                    corrective_ramp_window_min=10.0, voltage_threshold=0.01,
                    contingency_rating="rate-a", enforce_flowgates=true,
                    enforce_angle_limits=true, enforce_voltage_security=true,
                    lp_solver=None, nlp_solver=None, max_contingencies=0,
                    min_rate_a=1.0, nr_max_iterations=30,
                    nr_convergence_tolerance=1e-6, enable_screener=true,
                    screener_threshold_fraction=0.9,
                    screener_max_initial_contingencies=500, warm_start=None,
                    use_pwl_costs=true, pwl_cost_breakpoints=20,
                    gen_limit_penalty=None, use_loss_factors=false,
                    max_loss_iter=3, loss_tol=1e-3,
                    enforce_thermal_limits=true,
                    par_setpoints=None, hvdc_links=None))]
pub fn solve_scopf(
    py: Python<'_>,
    network: &Network,
    formulation: &str,
    mode: &str,
    tolerance: f64,
    max_iterations: u32,
    max_cuts_per_iteration: usize,
    corrective_ramp_window_min: f64,
    voltage_threshold: f64,
    contingency_rating: &str,
    enforce_flowgates: bool,
    enforce_angle_limits: bool,
    enforce_voltage_security: bool,
    lp_solver: Option<&str>,
    nlp_solver: Option<&str>,
    max_contingencies: usize,
    min_rate_a: f64,
    nr_max_iterations: u32,
    nr_convergence_tolerance: f64,
    enable_screener: bool,
    screener_threshold_fraction: f64,
    screener_max_initial_contingencies: usize,
    warm_start: Option<&ScopfResult>,
    use_pwl_costs: bool,
    pwl_cost_breakpoints: usize,
    gen_limit_penalty: Option<f64>,
    use_loss_factors: bool,
    max_loss_iter: usize,
    loss_tol: f64,
    enforce_thermal_limits: bool,
    par_setpoints: Option<Vec<std::collections::HashMap<String, pyo3::Py<pyo3::PyAny>>>>,
    hvdc_links: Option<Vec<std::collections::HashMap<String, pyo3::Py<pyo3::PyAny>>>>,
) -> PyResult<ScopfResult> {
    catch_panic("solve_scopf", || {
        let form = match formulation {
            "ac" => surge_opf::ScopfFormulation::Ac,
            "dc" => surge_opf::ScopfFormulation::Dc,
            other => {
                return Err(PyValueError::new_err(format!(
                    "formulation must be 'dc' or 'ac', got '{other}'"
                )));
            }
        };
        let md = match mode {
            "preventive" => surge_opf::ScopfMode::Preventive,
            "corrective" => surge_opf::ScopfMode::Corrective,
            other => {
                return Err(PyValueError::new_err(format!(
                    "mode must be 'preventive' or 'corrective', got '{other}'"
                )));
            }
        };
        let lp = lp_solver
            .map(|s| surge_opf::backends::lp_solver_from_str(s).map_err(to_pyerr))
            .transpose()?;
        let nlp = nlp_solver
            .map(|s| {
                if form == surge_opf::ScopfFormulation::Ac {
                    surge_opf::backends::ac_opf_nlp_solver_from_str(s).map_err(to_pyerr)
                } else {
                    surge_opf::backends::nlp_solver_from_str(s).map_err(to_pyerr)
                }
            })
            .transpose()?;

        let ctg_rating = match contingency_rating {
            "rate-a" | "rate_a" | "RateA" => surge_opf::ThermalRating::RateA,
            "rate-b" | "rate_b" | "RateB" => surge_opf::ThermalRating::RateB,
            "rate-c" | "rate_c" | "RateC" => surge_opf::ThermalRating::RateC,
            other => {
                return Err(PyValueError::new_err(format!(
                    "contingency_rating must be 'RateA', 'RateB', or 'RateC', got '{other}'"
                )));
            }
        };
        let parsed_par_setpoints: Vec<surge_solution::ParSetpoint> = par_setpoints
            .unwrap_or_default()
            .into_iter()
            .filter_map(|d| {
                let get_u32 =
                    |k: &str| -> Option<u32> { d.get(k).and_then(|v| v.extract::<u32>(py).ok()) };
                let get_str = |k: &str| -> String {
                    d.get(k)
                        .and_then(|v| v.extract::<String>(py).ok())
                        .unwrap_or_else(|| "1".to_string())
                };
                let get_f64 =
                    |k: &str| -> Option<f64> { d.get(k).and_then(|v| v.extract::<f64>(py).ok()) };
                Some(surge_solution::ParSetpoint {
                    from_bus: get_u32("from_bus")?,
                    to_bus: get_u32("to_bus")?,
                    circuit: get_str("circuit"),
                    target_mw: get_f64("target_mw")?,
                })
            })
            .collect();
        let parsed_hvdc_links: Option<Vec<surge_opf::HvdcOpfLink>> = hvdc_links.map(|links| {
            links
                .into_iter()
                .filter_map(|d| {
                    let get_u32 = |k: &str| -> Option<u32> {
                        d.get(k).and_then(|v| v.extract::<u32>(py).ok())
                    };
                    let get_f64 = |k: &str| -> Option<f64> {
                        d.get(k).and_then(|v| v.extract::<f64>(py).ok())
                    };
                    let get_f64_or = |k: &str, def: f64| -> f64 { get_f64(k).unwrap_or(def) };
                    Some(surge_opf::HvdcOpfLink {
                        from_bus: get_u32("from_bus")?,
                        to_bus: get_u32("to_bus")?,
                        p_dc_min_mw: get_f64("p_dc_min_mw")?,
                        p_dc_max_mw: get_f64("p_dc_max_mw")?,
                        loss_a_mw: get_f64_or("loss_a_mw", 0.0),
                        loss_b_frac: get_f64_or("loss_b_frac", 0.0),
                        name: String::new(),
                    })
                })
                .collect()
        });
        let opts = surge_opf::ScopfOptions {
            formulation: form,
            mode: md,
            max_iterations,
            violation_tolerance_pu: tolerance,
            max_cuts_per_iteration,
            max_contingencies,
            min_rate_a,
            contingency_rating: ctg_rating,
            enforce_flowgates,
            enforce_angle_limits,
            screener: enable_screener.then_some(surge_opf::ScopfScreeningPolicy {
                threshold_fraction: screener_threshold_fraction,
                max_initial_contingencies: screener_max_initial_contingencies,
            }),
            dc_opf: surge_opf::DcOpfOptions {
                enforce_thermal_limits,
                use_pwl_costs,
                pwl_cost_breakpoints,
                gen_limit_penalty,
                use_loss_factors,
                max_loss_iter,
                loss_tol,
                par_setpoints: parsed_par_setpoints,
                hvdc_links: parsed_hvdc_links,
                ..Default::default()
            },
            ac: surge_opf::ScopfAcSettings {
                opf: surge_opf::AcOpfOptions {
                    tolerance,
                    exact_hessian: true,
                    ..Default::default()
                },
                voltage_threshold,
                nr_max_iterations,
                nr_convergence_tolerance,
                enforce_voltage_security,
            },
            corrective: surge_opf::ScopfCorrectiveSettings {
                ramp_window_min: corrective_ramp_window_min,
            },
            ..Default::default()
        };
        let mut runtime = surge_opf::ScopfRuntime::default();
        if let Some(lp) = lp {
            runtime = runtime.with_lp_solver(lp);
        }
        if let Some(nlp) = nlp {
            runtime = runtime.with_nlp_solver(nlp);
        }
        if let Some(warm_start) = warm_start {
            runtime = runtime.with_warm_start(surge_opf::ScopfWarmStart {
                base_pg: warm_start.base_opf.inner.generators.gen_p_mw.clone(),
                base_vm: warm_start
                    .base_opf
                    .inner
                    .power_flow
                    .voltage_magnitude_pu
                    .clone(),
                active_cuts: warm_start
                    .binding_contingencies
                    .iter()
                    .map(|cut| {
                        Ok(surge_opf::security::ScopfWarmStartCut {
                            cut_kind: parse_scopf_cut_kind(&cut.cut_kind)?,
                            outaged_branch_indices: cut.outaged_branch_indices.clone(),
                            outaged_generator_indices: cut.outaged_generator_indices.clone(),
                            monitored_branch_idx: cut.monitored_branch_idx,
                        })
                    })
                    .collect::<PyResult<Vec<_>>>()?,
            });
        }
        let net = Arc::clone(&network.inner);
        let scopf_sol = py
            .detach(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    surge_opf::solve_scopf_with_runtime(&net, &opts, &runtime)
                }))
                .map_err(|e| {
                    surge_opf::security::types::ScopfError::SolverError(format!(
                        "solve_scopf failed: {}",
                        extract_panic_msg(e)
                    ))
                })
                .and_then(|r| r)
            })
            .map_err(|e| to_scopf_pyerr(&e))?;
        Ok(ScopfResult {
            base_opf: OpfSolution::from_core(scopf_sol.base_opf, Arc::clone(&network.inner)),
            formulation: scopf_formulation_name(scopf_sol.formulation).to_string(),
            mode: scopf_mode_name(scopf_sol.mode).to_string(),
            iterations: scopf_sol.iterations,
            converged: scopf_sol.converged,
            total_contingencies_evaluated: scopf_sol.total_contingencies_evaluated,
            total_contingency_constraints: scopf_sol.total_contingency_constraints,
            binding_contingencies: scopf_sol
                .binding_contingencies
                .into_iter()
                .map(BindingContingency::from)
                .collect(),
            lmp_contingency_congestion: scopf_sol.lmp_contingency_congestion,
            remaining_violations: scopf_sol
                .remaining_violations
                .into_iter()
                .map(ContingencyViolation::from)
                .collect(),
            failed_contingencies: scopf_sol
                .failed_contingencies
                .into_iter()
                .map(FailedContingencyEvaluation::from)
                .collect(),
            screening_stats: ScopfScreeningStats::from(scopf_sol.screening_stats),
            solve_time_secs: scopf_sol.solve_time_secs,
        })
    })
}
