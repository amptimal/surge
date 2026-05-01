// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python bindings for the GO Competition Challenge 3 adapter pipeline.
//!
//! Exposes three steps:
//!
//! 1. `go_c3_load_problem(path)` — parse a GO C3 `problem.json` into an
//!    opaque handle that carries the Rust-owned problem/context.
//! 2. `go_c3_build_network(problem_handle, policy_dict)` — run the full
//!    network-build pipeline (structural + enrich + reserves + hvdc_q +
//!    voltage) and return a [`Network`] plus the context.
//! 3. `go_c3_build_request(problem_handle, policy_dict)` — invoke the
//!    surge-dispatch request builder, returning a fully-formed
//!    [`DispatchRequest`] serialized as a Python dict.
//! 4. `go_c3_export_solution(problem_handle, dispatch_result)` — turn a
//!    `DispatchResult` back into a GO C3 `solution.json` dict.
//! 5. `go_c3_save_solution(solution_dict, path)` — pretty-print a GO C3
//!    solution dict to disk.
//!
//! The problem handle is a `#[pyclass]` wrapping a tuple of
//! `(GoC3Problem, GoC3Context)` so that the Python side doesn't need to
//! re-parse the problem JSON for each step.

use std::path::PathBuf;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};

use surge_io::go_c3 as io_go_c3;
use surge_io::go_c3::{
    GoC3AcReconcileMode, GoC3CommitmentMode, GoC3ConsumerMode, GoC3Context, GoC3Formulation,
    GoC3Policy, GoC3Problem, GoC3ScucLossTreatment, GoC3SecurityCutStrategy,
    GoC3SlackInferenceMode,
};
use surge_market::go_c3 as market_go_c3;

use crate::dispatch::DispatchResult;
use crate::exceptions::SurgeError;
use crate::network::Network;

// ─── Problem handle ────────────────────────────────────────────────────────

/// Opaque Python handle carrying a loaded GO C3 problem and the adapter
/// context produced by the network-build pipeline.
///
/// Created by `go_c3_load_problem`. Subsequent calls
/// (`go_c3_build_network`, `go_c3_build_request`, `go_c3_export_solution`)
/// accept this handle to avoid re-parsing the 1-10+ MB `problem.json`.
#[pyclass(name = "GoC3Handle", module = "surge._surge", frozen)]
pub struct GoC3Handle {
    pub(crate) problem: GoC3Problem,
    pub(crate) context: std::sync::Mutex<GoC3Context>,
}

impl GoC3Handle {
    fn with_context<F, R>(&self, f: F) -> PyResult<R>
    where
        F: FnOnce(&mut GoC3Context) -> R,
    {
        let mut guard = self
            .context
            .lock()
            .map_err(|_| SurgeError::new_err("GoC3Handle context poisoned"))?;
        Ok(f(&mut guard))
    }
}

#[pymethods]
impl GoC3Handle {
    fn __repr__(&self) -> String {
        format!(
            "GoC3Handle(buses={}, devices={}, ac_lines={}, dc_lines={}, periods={})",
            self.problem.network.bus.len(),
            self.problem.network.simple_dispatchable_device.len(),
            self.problem.network.ac_line.len(),
            self.problem.network.dc_line.len(),
            self.problem.time_series_input.general.time_periods,
        )
    }
}

// ─── Public entry points ───────────────────────────────────────────────────

/// Load a GO C3 `problem.json` and return an opaque handle.
#[pyfunction]
#[pyo3(signature = (path))]
pub fn go_c3_load_problem(path: PathBuf) -> PyResult<GoC3Handle> {
    let problem = io_go_c3::load_problem(&path)
        .map_err(|err| SurgeError::new_err(format!("load GO C3 problem failed: {err}")))?;
    Ok(GoC3Handle {
        problem,
        context: std::sync::Mutex::new(GoC3Context::new()),
    })
}

/// Run the full network-build pipeline (structural + enrich + reserves +
/// hvdc_q + voltage) and return a [`Network`] plus the current context as
/// a serialized Python dict.
#[pyfunction]
#[pyo3(signature = (handle, policy = None))]
pub fn go_c3_build_network<'py>(
    py: Python<'py>,
    handle: &GoC3Handle,
    policy: Option<&Bound<'_, PyDict>>,
) -> PyResult<(Network, Bound<'py, PyAny>)> {
    let policy = parse_policy(policy)?;

    let (mut network, mut ctx) = io_go_c3::to_network_with_policy(&handle.problem, &policy)
        .map_err(|err| SurgeError::new_err(format!("to_network failed: {err}")))?;
    io_go_c3::enrich_network(&mut network, &mut ctx, &handle.problem, &policy)
        .map_err(|err| SurgeError::new_err(format!("enrich_network failed: {err}")))?;
    io_go_c3::apply_reserves(&mut network, &mut ctx, &handle.problem)
        .map_err(|err| SurgeError::new_err(format!("apply_reserves failed: {err}")))?;
    io_go_c3::apply_hvdc_reactive_terminals(&mut network, &mut ctx, &handle.problem, &policy)
        .map_err(|err| {
            SurgeError::new_err(format!("apply_hvdc_reactive_terminals failed: {err}"))
        })?;
    io_go_c3::apply_voltage_regulation(&mut network, &mut ctx, &handle.problem, &policy)
        .map_err(|err| SurgeError::new_err(format!("apply_voltage_regulation failed: {err}")))?;

    // Persist the finished context on the handle so `go_c3_build_request`
    // and `go_c3_export_solution` can reuse it without rerunning the pipeline.
    handle.with_context(|ctx_handle| {
        *ctx_handle = ctx.clone();
    })?;

    let context_py = serialize_to_py(py, &ctx)?;
    Ok((Network::from_inner(network), context_py))
}

/// Build a typed dispatch request. Returns a Python dict matching
/// `DispatchRequest`'s serde schema — pass it straight to
/// `surge.solve_dispatch(network, request=...)`.
#[pyfunction]
#[pyo3(signature = (handle, policy = None))]
pub fn go_c3_build_request<'py>(
    py: Python<'py>,
    handle: &GoC3Handle,
    policy: Option<&Bound<'_, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let policy = parse_policy(policy)?;
    // build_dispatch_request mutates the context in place (populates
    // consumer dispatchable block IDs etc.) so the export path can
    // find them later.
    let request = handle
        .with_context(|ctx| market_go_c3::build_dispatch_request(&handle.problem, ctx, &policy))?;
    let request = request
        .map_err(|err| SurgeError::new_err(format!("build_dispatch_request failed: {err}")))?;
    serialize_to_py(py, &request)
}

/// Export a dispatch result back into a GO C3 `solution.json` dict.
///
/// Accepts either:
/// * a [`DispatchResult`] wrapper (returned by `solve_dispatch`)
/// * a serialized solution dict (for example
///   `result["stages"][n]["solution"]` from `solve_market_workflow_py`)
///
/// When `dc_reserve_source` is provided (same form as
/// `dispatch_result`), active (real-power) reserve awards are taken
/// from that solution while reactive (`q_res_up`, `q_res_down`)
/// reserve awards stay on `dispatch_result`. Mirrors Python's
/// `export_go3_solution(..., dc_dispatch_result=...)` merge.
///
/// `allow_consumer_reserve_shedding` (default `true`) caps each
/// consumer's exported active-reserve awards so the validator's
/// `viol_cs_t_p_on_min` / `viol_cs_t_p_on_max` constraints stay
/// feasible after AC SCED curtails the consumer's `p_on`. The shed
/// amount is implicitly accepted as zonal reserve shortfall. Set
/// `false` for diagnostics that need to expose the raw SCUC awards.
#[pyfunction]
#[pyo3(signature = (handle, dispatch_result, dc_reserve_source = None, allow_consumer_reserve_shedding = true))]
pub fn go_c3_export_solution<'py>(
    py: Python<'py>,
    handle: &GoC3Handle,
    dispatch_result: &Bound<'_, PyAny>,
    dc_reserve_source: Option<&Bound<'_, PyAny>>,
    allow_consumer_reserve_shedding: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let solution_inner = extract_dispatch_solution(py, dispatch_result)?;
    let dc_inner = match dc_reserve_source {
        Some(obj) => Some(extract_dispatch_solution(py, obj)?),
        None => None,
    };
    let options = market_go_c3::ExportOptions {
        allow_consumer_reserve_shedding,
    };
    let solution = handle.with_context(|ctx| {
        market_go_c3::export_go_c3_solution_with_options(
            &handle.problem,
            ctx,
            &solution_inner,
            dc_inner.as_ref(),
            &options,
        )
    })?;
    let solution = solution
        .map_err(|err| SurgeError::new_err(format!("export_go_c3_solution failed: {err}")))?;
    serialize_to_py(py, &solution)
}

fn extract_dispatch_solution(
    py: Python<'_>,
    obj: &Bound<'_, PyAny>,
) -> PyResult<surge_dispatch::DispatchSolution> {
    if let Ok(wrapped) = obj.extract::<PyRef<DispatchResult>>() {
        return Ok(wrapped.inner().clone());
    }
    let json: String = py
        .import("json")?
        .call_method1("dumps", (obj,))?
        .extract()?;
    serde_json::from_str(&json).map_err(|err| {
        PyValueError::new_err(format!(
            "dispatch solution argument must be a DispatchResult or a serialized solution dict: {err}"
        ))
    })
}

/// Build the canonical two-stage market workflow (DC SCUC → AC SCED)
/// for a GO C3 scenario. Returns a Python-facing `MarketWorkflow`.
#[pyfunction]
#[pyo3(signature = (handle, network, policy = None))]
pub fn go_c3_build_workflow(
    handle: &GoC3Handle,
    network: &Network,
    policy: Option<&Bound<'_, PyDict>>,
) -> PyResult<crate::market::PyMarketWorkflow> {
    let policy = parse_policy(policy)?;
    // The canonical workflow mutates the network (promotes Q-capable
    // generators to PV for the AC SCED stage). Clone and own it for
    // the DispatchModel that drives both stages.
    let mut owned_network: surge_network::Network = (*network.inner).clone();
    let workflow = handle.with_context(|ctx| {
        market_go_c3::build_canonical_workflow(
            &handle.problem,
            ctx,
            &policy,
            &mut owned_network,
            surge_market::canonical_workflow::CanonicalWorkflowOptions::default(),
        )
    })?;
    let workflow = workflow
        .map_err(|err| SurgeError::new_err(format!("build_canonical_workflow failed: {err}")))?;
    Ok(crate::market::PyMarketWorkflow { inner: workflow })
}

/// Write a GO C3 solution dict to disk as pretty-printed JSON.
#[pyfunction]
#[pyo3(signature = (solution, path))]
pub fn go_c3_save_solution<'py>(
    py: Python<'py>,
    solution: &Bound<'_, PyAny>,
    path: PathBuf,
) -> PyResult<()> {
    // Round-trip through serde_json so we always get stable pretty output.
    let json = py
        .import("json")?
        .call_method1("dumps", (solution,))?
        .extract::<String>()?;
    let value: serde_json::Value = serde_json::from_str(&json)
        .map_err(|err| PyValueError::new_err(format!("solution dict is not JSON: {err}")))?;
    let pretty = serde_json::to_string_pretty(&value)
        .map_err(|err| SurgeError::new_err(format!("failed to serialize solution: {err}")))?;
    std::fs::write(&path, pretty)
        .map_err(|err| PyRuntimeError::new_err(format!("write {}: {err}", path.display())))?;
    Ok(())
}

// ─── Policy parsing ────────────────────────────────────────────────────────

/// Convert an optional Python dict into a typed [`GoC3Policy`].
///
/// Recognised keys (all optional, all string-valued unless noted):
///
/// * `formulation`: `"dc"` | `"ac"` (default `"dc"`)
/// * `ac_reconcile_mode`: `"ac_dispatch"` | `"none"` (default `"ac_dispatch"`)
/// * `consumer_mode`: `"dispatchable"` | `"fixed"` (default `"dispatchable"`)
/// * `commitment_mode`: `"optimize"` | `"fixed_initial"` | `"all_committed"` (default `"optimize"`)
/// * `slack_mode`: `"explicit"` | `"reactive_capability"` (default `"reactive_capability"`)
/// * `allow_branch_switching`: bool (default `False`)
/// * `switchable_branch_uids`: iterable of strings
fn parse_policy(policy: Option<&Bound<'_, PyDict>>) -> PyResult<GoC3Policy> {
    let mut out = GoC3Policy::default();
    let Some(policy) = policy else {
        return Ok(out);
    };

    if let Some(value) = policy.get_item("formulation")? {
        out.formulation = match value.extract::<String>()?.as_str() {
            "dc" => GoC3Formulation::Dc,
            "ac" => GoC3Formulation::Ac,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown GoC3Policy.formulation: {other}"
                )));
            }
        };
    }
    if let Some(value) = policy.get_item("ac_reconcile_mode")? {
        out.ac_reconcile_mode = match value.extract::<String>()?.as_str() {
            "ac_dispatch" => GoC3AcReconcileMode::AcDispatch,
            "none" => GoC3AcReconcileMode::None,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown GoC3Policy.ac_reconcile_mode: {other}"
                )));
            }
        };
    }
    if let Some(value) = policy.get_item("consumer_mode")? {
        out.consumer_mode = match value.extract::<String>()?.as_str() {
            "dispatchable" => GoC3ConsumerMode::Dispatchable,
            "fixed" => GoC3ConsumerMode::Fixed,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown GoC3Policy.consumer_mode: {other}"
                )));
            }
        };
    }
    if let Some(value) = policy.get_item("commitment_mode")? {
        out.commitment_mode = match value.extract::<String>()?.as_str() {
            "optimize" => GoC3CommitmentMode::Optimize,
            "fixed_initial" => GoC3CommitmentMode::FixedInitial,
            "all_committed" => GoC3CommitmentMode::AllCommitted,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown GoC3Policy.commitment_mode: {other}"
                )));
            }
        };
    }
    if let Some(value) = policy.get_item("slack_mode")? {
        out.slack_mode = match value.extract::<String>()?.as_str() {
            "explicit" => GoC3SlackInferenceMode::Explicit,
            "reactive_capability" => GoC3SlackInferenceMode::ReactiveCapability,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown GoC3Policy.slack_mode: {other}"
                )));
            }
        };
    }
    if let Some(value) = policy.get_item("allow_branch_switching")? {
        out.allow_branch_switching = value.extract::<bool>()?;
    }
    if let Some(value) = policy.get_item("switchable_branch_uids")? {
        if !value.is_none() {
            let uids: Vec<String> = value.extract()?;
            out.switchable_branch_uids = Some(uids.into_iter().collect());
        }
    }
    if let Some(value) = policy.get_item("scuc_thermal_penalty_multiplier")? {
        if !value.is_none() {
            out.scuc_thermal_penalty_multiplier = value.extract::<f64>()?;
        }
    }
    if let Some(value) = policy.get_item("sced_thermal_penalty_multiplier")? {
        if !value.is_none() {
            out.sced_thermal_penalty_multiplier = value.extract::<f64>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_reserve_penalty_multiplier")? {
        if !value.is_none() {
            out.scuc_reserve_penalty_multiplier = value.extract::<f64>()?;
        }
    }
    if let Some(value) = policy.get_item("ac_sced_period_concurrency")? {
        if !value.is_none() {
            out.ac_sced_period_concurrency = Some(value.extract::<usize>()?);
        }
    }
    if let Some(value) = policy.get_item("scuc_security_preseed_count_per_period")? {
        if !value.is_none() {
            out.scuc_security_preseed_count_per_period = value.extract::<usize>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_security_max_iterations")? {
        if !value.is_none() {
            out.scuc_security_max_iterations = value.extract::<usize>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_security_max_cuts_per_iteration")? {
        if !value.is_none() {
            out.scuc_security_max_cuts_per_iteration = value.extract::<usize>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_security_cut_strategy")? {
        if !value.is_none() {
            out.scuc_security_cut_strategy = match value.extract::<String>()?.as_str() {
                "fixed" => GoC3SecurityCutStrategy::Fixed,
                "adaptive" => GoC3SecurityCutStrategy::Adaptive,
                other => {
                    return Err(PyValueError::new_err(format!(
                        "unknown GoC3Policy.scuc_security_cut_strategy: {other}"
                    )));
                }
            };
        }
    }
    if let Some(value) = policy.get_item("scuc_security_max_active_cuts")? {
        if !value.is_none() {
            out.scuc_security_max_active_cuts = Some(value.extract::<usize>()?);
        }
    }
    if let Some(value) = policy.get_item("scuc_security_cut_retire_after_rounds")? {
        if !value.is_none() {
            out.scuc_security_cut_retire_after_rounds = Some(value.extract::<usize>()?);
        }
    }
    if let Some(value) = policy.get_item("scuc_security_targeted_cut_threshold")? {
        if !value.is_none() {
            out.scuc_security_targeted_cut_threshold = value.extract::<usize>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_security_targeted_cut_cap")? {
        if !value.is_none() {
            out.scuc_security_targeted_cut_cap = value.extract::<usize>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_security_near_binding_report")? {
        if !value.is_none() {
            out.scuc_security_near_binding_report = value.extract::<bool>()?;
        }
    }
    // scuc_loss_factor_warm_start: accepts either None or a 2-tuple
    // `(mode_str, rate)`. Mode is one of "uniform", "load_pattern",
    // "dc_pf"; rate is an f64 (ignored for "dc_pf"). See
    // [`surge_io::go_c3::policy::GoC3Policy::scuc_loss_factor_warm_start`].
    if let Some(value) = policy.get_item("scuc_loss_factor_warm_start")? {
        if !value.is_none() {
            let tuple: (String, f64) = value.extract().map_err(|err| {
                PyValueError::new_err(format!(
                    "scuc_loss_factor_warm_start must be None or a (mode, rate) tuple: {err}"
                ))
            })?;
            out.scuc_loss_factor_warm_start = Some(tuple);
        }
    }
    if let Some(value) = policy.get_item("scuc_loss_factor_max_iterations")? {
        if !value.is_none() {
            out.scuc_loss_factor_max_iterations = Some(value.extract::<usize>()?);
        }
    }
    if let Some(value) = policy.get_item("relax_sced_branch_limits_to_dc_slack")? {
        if !value.is_none() {
            out.relax_sced_branch_limits_to_dc_slack = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("sced_branch_relax_margin_mva")? {
        if !value.is_none() {
            out.sced_branch_relax_margin_mva = value.extract::<f64>()?;
        }
    }
    if let Some(value) = policy.get_item("disable_sced_thermal_limits")? {
        if !value.is_none() {
            out.disable_sced_thermal_limits = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("sced_bus_balance_safety_multiplier")? {
        if !value.is_none() {
            out.sced_bus_balance_safety_multiplier = value.extract::<f64>()?;
        }
    }
    if let Some(value) = policy.get_item("ac_relax_committed_pmin_to_zero")? {
        if !value.is_none() {
            out.ac_relax_committed_pmin_to_zero = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("sced_ac_opf_tolerance")? {
        if !value.is_none() {
            out.sced_ac_opf_tolerance = Some(value.extract::<f64>()?);
        }
    }
    if let Some(value) = policy.get_item("sced_ac_opf_max_iterations")? {
        if !value.is_none() {
            out.sced_ac_opf_max_iterations = Some(value.extract::<u32>()?);
        }
    }
    if let Some(value) = policy.get_item("sced_enforce_regulated_bus_vm_targets")? {
        if !value.is_none() {
            out.sced_enforce_regulated_bus_vm_targets = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("reactive_support_pin_factor")? {
        if !value.is_none() {
            out.reactive_support_pin_factor = value.extract::<f64>()?;
        }
    }
    if let Some(value) = policy.get_item("run_pricing")? {
        if !value.is_none() {
            out.run_pricing = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("commitment_mip_rel_gap")? {
        if !value.is_none() {
            out.commitment_mip_rel_gap = Some(value.extract::<f64>()?);
        }
    }
    if let Some(value) = policy.get_item("commitment_time_limit_secs")? {
        if !value.is_none() {
            out.commitment_time_limit_secs = Some(value.extract::<f64>()?);
        }
    }
    if let Some(value) = policy.get_item("commitment_mip_gap_schedule")? {
        if !value.is_none() {
            let breakpoints: Vec<(f64, f64)> = value.extract().map_err(|err| {
                PyValueError::new_err(format!(
                    "commitment_mip_gap_schedule must be a list of (time_secs, gap) pairs: {err}"
                ))
            })?;
            out.commitment_mip_gap_schedule = Some(breakpoints);
        }
    }
    if let Some(value) = policy.get_item("disable_flowgates")? {
        if !value.is_none() {
            out.disable_flowgates = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("disable_scuc_warm_start")? {
        if !value.is_none() {
            out.disable_scuc_warm_start = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_firm_bus_balance_slacks")? {
        if !value.is_none() {
            out.scuc_firm_bus_balance_slacks = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_firm_branch_thermal_slacks")? {
        if !value.is_none() {
            out.scuc_firm_branch_thermal_slacks = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("disable_scuc_thermal_limits")? {
        if !value.is_none() {
            out.disable_scuc_thermal_limits = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_copperplate")? {
        if !value.is_none() {
            out.scuc_copperplate = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_power_balance_penalty_multiplier")? {
        if !value.is_none() {
            out.scuc_power_balance_penalty_multiplier = value.extract::<f64>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_disable_bus_power_balance")? {
        if !value.is_none() {
            out.scuc_disable_bus_power_balance = value.extract::<bool>()?;
        }
    }
    if let Some(value) = policy.get_item("scuc_loss_treatment")? {
        if !value.is_none() {
            out.scuc_loss_treatment = match value.extract::<String>()?.as_str() {
                "static" => GoC3ScucLossTreatment::Static,
                "scalar_feedback" => GoC3ScucLossTreatment::ScalarFeedback,
                "penalty_factors" => GoC3ScucLossTreatment::PenaltyFactors,
                other => {
                    return Err(PyValueError::new_err(format!(
                        "unknown GoC3Policy.scuc_loss_treatment: {other}"
                    )));
                }
            };
        }
    }
    Ok(out)
}

// ─── Helpers ───────────────────────────────────────────────────────────────

fn serialize_to_py<'py, T: serde::Serialize + ?Sized>(
    py: Python<'py>,
    value: &T,
) -> PyResult<Bound<'py, PyAny>> {
    let json = serde_json::to_string(value)
        .map_err(|err| SurgeError::new_err(format!("serialize failed: {err}")))?;
    py.import("json")?.call_method1("loads", (json,))
}
