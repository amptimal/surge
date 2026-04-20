// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python bindings for the canonical [`surge_market`] workflow types.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};

use std::sync::Arc;

use surge_dispatch::{
    CommitmentPolicy, CommitmentSchedule, DispatchSolveOptions, DispatchStageRole,
    ResourceCommitmentSchedule,
};
use surge_market::workflow::{MarketStage, MarketWorkflow, solve_market_workflow_with_options};
use surge_opf::backends::{NlpSolver, ac_opf_nlp_solver_from_str, lp_solver_from_str};

use crate::exceptions::SurgeError;
use crate::network::Network;

fn serialize_to_py<'py, T: serde::Serialize + ?Sized>(
    py: Python<'py>,
    value: &T,
) -> PyResult<Bound<'py, PyAny>> {
    let json = serde_json::to_string(value)
        .map_err(|err| SurgeError::new_err(format!("serialize failed: {err}")))?;
    py.import("json")?.call_method1("loads", (json,))
}

/// Python-facing wrapper around a single [`MarketStage`].
///
/// Construct from Python via [`market_stage`]; solve the enclosing
/// workflow via [`solve_market_workflow`]. This is the Rust-native
/// stage type — markets that *compose* multi-stage workflows in
/// Python should use [`surge.market.MarketWorkflow`] +
/// [`surge.market.WorkflowRunner`] instead.
#[pyclass(name = "NativeMarketStage", module = "surge._surge")]
pub struct PyMarketStage {
    pub(crate) inner: MarketStage,
}

#[pymethods]
impl PyMarketStage {
    #[getter]
    fn stage_id(&self) -> String {
        self.inner.stage_id.clone()
    }
    #[getter]
    fn role(&self) -> String {
        format_role(&self.inner.role)
    }
    #[getter]
    fn derived_from_stage_id(&self) -> Option<String> {
        self.inner.derived_from_stage_id.clone()
    }
    #[getter]
    fn commitment_source_stage_id(&self) -> Option<String> {
        self.inner.commitment_source_stage_id.clone()
    }
}

/// Python-facing wrapper around a Rust-native [`MarketWorkflow`].
///
/// This type is returned by canonical workflow builders such as
/// [`surge.market.go_c3.build_workflow`] and consumed by
/// [`solve_market_workflow_py`]. Users composing their own Python
/// stages should reach for the Python-side
/// [`surge.market.MarketWorkflow`] + [`surge.market.WorkflowRunner`]
/// instead — that's the public composition layer.
#[pyclass(name = "NativeMarketWorkflow", module = "surge._surge")]
pub struct PyMarketWorkflow {
    pub(crate) inner: MarketWorkflow,
}

#[pymethods]
impl PyMarketWorkflow {
    #[new]
    fn new(stages: Vec<PyRef<PyMarketStage>>) -> Self {
        let inner_stages: Vec<MarketStage> = stages
            .into_iter()
            .map(|stage| stage.inner.clone())
            .collect();
        Self {
            inner: MarketWorkflow::new(inner_stages),
        }
    }

    fn stages(&self) -> Vec<String> {
        self.inner
            .stages
            .iter()
            .map(|s| s.stage_id.clone())
            .collect()
    }

    fn n_stages(&self) -> usize {
        self.inner.stages.len()
    }

    /// Override a stage's commitment to a `Fixed` schedule.
    ///
    /// `schedule` is `{resource_id: [bool, bool, ...]}` — per-period
    /// commitment status. The first period's value is also used as
    /// the initial-period (pre-horizon) commitment.
    ///
    /// When set on stage 1 (DC SCUC), the LP is forced to honour this
    /// commitment instead of optimising over commitment binaries.
    /// Used by the `solve_sced_fixed` debug path to pin a known-good
    /// reference commitment (e.g. the GO C3 winner submission) into
    /// the workflow.
    fn set_stage_commitment(
        &mut self,
        stage_idx: usize,
        schedule: std::collections::HashMap<String, Vec<bool>>,
    ) -> PyResult<()> {
        if stage_idx >= self.inner.stages.len() {
            return Err(PyValueError::new_err(format!(
                "set_stage_commitment: stage_idx {stage_idx} out of range \
                (workflow has {} stages)",
                self.inner.stages.len()
            )));
        }
        let mut resources: Vec<ResourceCommitmentSchedule> = schedule
            .into_iter()
            .map(|(resource_id, periods)| ResourceCommitmentSchedule {
                initial: periods.first().copied().unwrap_or(false),
                periods: Some(periods),
                resource_id,
            })
            .collect();
        resources.sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
        self.inner.stages[stage_idx]
            .request
            .set_commitment(CommitmentPolicy::Fixed(CommitmentSchedule { resources }));
        // The downstream stage's `commitment_source_stage_id` handoff
        // still extracts commitment from the source stage's solved
        // result, so this Fixed override flows naturally to the AC
        // SCED stage at solve time.
        Ok(())
    }

    /// Serialize a stage's internal [`DispatchRequest`] to a Python dict.
    ///
    /// Pair with [`set_stage_request`] to read, mutate (pin Pg/Qg,
    /// dispatchable-load schedules, reserve offers, etc.), and write back.
    fn stage_request<'py>(&self, py: Python<'py>, stage_idx: usize) -> PyResult<Bound<'py, PyAny>> {
        if stage_idx >= self.inner.stages.len() {
            return Err(PyValueError::new_err(format!(
                "stage_request: stage_idx {stage_idx} out of range \
                (workflow has {} stages)",
                self.inner.stages.len()
            )));
        }
        serialize_to_py(py, &self.inner.stages[stage_idx].request)
    }

    /// Replace a stage's internal [`DispatchRequest`] from a Python dict.
    ///
    /// The dict must match the `DispatchRequest` serde schema (typically
    /// obtained from [`stage_request`] and mutated in place).
    fn set_stage_request(&mut self, stage_idx: usize, request: &Bound<'_, PyAny>) -> PyResult<()> {
        if stage_idx >= self.inner.stages.len() {
            return Err(PyValueError::new_err(format!(
                "set_stage_request: stage_idx {stage_idx} out of range \
                (workflow has {} stages)",
                self.inner.stages.len()
            )));
        }
        self.inner.stages[stage_idx].request = deserialize_request_local(request)?;
        Ok(())
    }

    /// Return a clone of the network used by a workflow stage.
    ///
    /// Stage 0 (DC SCUC) and stage 1 (AC SCED) may have different
    /// networks — e.g. the AC network includes synthetic HVDC reactive
    /// generators. The returned ``Network`` object supports
    /// ``compute_gsf``, ``compute_ptdf``, ``solve_ac_pf``, etc.
    fn stage_network(&self, stage_idx: usize) -> PyResult<Network> {
        if stage_idx >= self.inner.stages.len() {
            return Err(PyValueError::new_err(format!(
                "stage_network: stage_idx {stage_idx} out of range \
                (workflow has {} stages)",
                self.inner.stages.len()
            )));
        }
        Ok(Network::from_inner(
            self.inner.stages[stage_idx].model.network().clone(),
        ))
    }

    /// Return the effective Pg/Qg dispatch bounds for a generator at a
    /// given period in the specified stage's request.
    ///
    /// These reflect all workflow-builder transforms (reactive support
    /// pin, band config) but NOT solve-time transforms (which operate
    /// on an ephemeral clone). Returns ``(p_min_mw, p_max_mw,
    /// q_min_mvar, q_max_mvar)`` or raises if the generator is not
    /// found.
    fn effective_bounds(
        &self,
        stage_idx: usize,
        resource_id: &str,
        period: usize,
    ) -> PyResult<(f64, f64, Option<f64>, Option<f64>)> {
        if stage_idx >= self.inner.stages.len() {
            return Err(PyValueError::new_err(format!(
                "effective_bounds: stage_idx {stage_idx} out of range"
            )));
        }
        let profiles = &self.inner.stages[stage_idx]
            .request
            .profiles()
            .generator_dispatch_bounds
            .profiles;
        for entry in profiles {
            if entry.resource_id != resource_id {
                continue;
            }
            if period >= entry.p_min_mw.len() || period >= entry.p_max_mw.len() {
                return Err(PyValueError::new_err(format!(
                    "effective_bounds: period {period} out of range for {resource_id}"
                )));
            }
            let q_min = entry
                .q_min_mvar
                .as_ref()
                .and_then(|v| v.get(period).copied());
            let q_max = entry
                .q_max_mvar
                .as_ref()
                .and_then(|v| v.get(period).copied());
            return Ok((entry.p_min_mw[period], entry.p_max_mw[period], q_min, q_max));
        }
        Err(PyValueError::new_err(format!(
            "effective_bounds: resource_id '{resource_id}' not found in stage {stage_idx}"
        )))
    }
}

/// Build a [`MarketStage`] from Python arguments.
///
/// * `stage_id` — stable identifier
/// * `role` — `"unit_commitment"`, `"economic_dispatch"`, `"pricing"`,
///   `"reliability_commitment"`, `"ac_redispatch"`, or a custom string
/// * `network` — prepared [`Network`]; the stage takes a fresh
///   [`DispatchModel`] over a copy
/// * `request` — dispatch request as a dict (matching `DispatchRequest`'s
///   serde schema)
#[pyfunction]
#[pyo3(signature = (stage_id, role, network, request, derived_from = None, commitment_from = None))]
pub fn market_stage(
    stage_id: String,
    role: &str,
    network: &Network,
    request: &Bound<'_, PyAny>,
    derived_from: Option<String>,
    commitment_from: Option<String>,
) -> PyResult<PyMarketStage> {
    let model = surge_dispatch::DispatchModel::prepare(&network.inner)
        .map_err(|err| SurgeError::new_err(format!("DispatchModel::prepare failed: {err}")))?;
    let parsed_request = deserialize_request_local(request)?;
    let parsed_role = parse_role(role)?;

    let mut stage = MarketStage::new(stage_id, parsed_role, model, parsed_request);
    if let Some(s) = derived_from {
        stage = stage.derived_from(s);
    }
    if let Some(s) = commitment_from {
        stage = stage.commitment_from(s);
    }
    Ok(PyMarketStage { inner: stage })
}

/// Solve a [`MarketWorkflow`] and return the result as a Python dict.
///
/// The result always contains `"stages"` (list of successfully solved
/// stages) and optionally `"error"` (the first stage that failed).
/// When a stage fails, prior successful stages are still available in
/// `"stages"` — callers can read SCUC output even when SCED fails.
///
/// Pass `stop_after_stage` (e.g. `"scuc"`) to solve only up to that
/// stage and return early. Useful for extracting SCUC output without
/// running SCED.
#[pyfunction]
#[pyo3(signature = (workflow, lp_solver = None, nlp_solver = None, stop_after_stage = None))]
pub fn solve_market_workflow_py<'py>(
    py: Python<'py>,
    workflow: &PyMarketWorkflow,
    lp_solver: Option<&str>,
    nlp_solver: Option<&str>,
    stop_after_stage: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let mut options = DispatchSolveOptions::default();
    if let Some(name) = lp_solver {
        let solver = lp_solver_from_str(name).map_err(SurgeError::new_err)?;
        options.lp_solver = Some(solver);
    }
    if let Some(name) = nlp_solver {
        let solver = ac_opf_nlp_solver_from_str(name).map_err(SurgeError::new_err)?;
        options.nlp_solver = Some(solver as Arc<dyn NlpSolver>);
    }
    let has_options = options.lp_solver.is_some() || options.nlp_solver.is_some();
    let opts = if has_options { Some(&options) } else { None };

    let result = if let Some(stop) = stop_after_stage {
        surge_market::solve_market_workflow_until(&workflow.inner, stop, opts)
    } else if let Some(opts) = opts {
        solve_market_workflow_with_options(&workflow.inner, opts)
    } else {
        surge_market::solve_market_workflow(&workflow.inner)
    }
    .map_err(|err| SurgeError::new_err(format!("solve_market_workflow failed: {err}")))?;

    // Stage failures are returned in result["error"] along with any
    // successful prior stages in result["stages"]. Callers inspect
    // result["error"] to detect stage failures — the previous behavior
    // of raising discarded the partial stages and stage-error metadata
    // that debuggers need.
    serialize_to_py(py, &result)
}

fn parse_role(role: &str) -> PyResult<DispatchStageRole> {
    match role {
        "unit_commitment" => Ok(DispatchStageRole::UnitCommitment),
        "economic_dispatch" => Ok(DispatchStageRole::EconomicDispatch),
        "pricing" => Ok(DispatchStageRole::Pricing),
        "reliability_commitment" => Ok(DispatchStageRole::ReliabilityCommitment),
        "ac_redispatch" => Ok(DispatchStageRole::AcRedispatch),
        _ => Ok(DispatchStageRole::Custom(role.to_string())),
    }
}

fn format_role(role: &DispatchStageRole) -> String {
    match role {
        DispatchStageRole::UnitCommitment => "unit_commitment".into(),
        DispatchStageRole::EconomicDispatch => "economic_dispatch".into(),
        DispatchStageRole::Pricing => "pricing".into(),
        DispatchStageRole::ReliabilityCommitment => "reliability_commitment".into(),
        DispatchStageRole::AcRedispatch => "ac_redispatch".into(),
        DispatchStageRole::Custom(s) => s.clone(),
    }
}

/// Local deserialize for DispatchRequest from a Python dict.
fn deserialize_request_local(
    request: &Bound<'_, PyAny>,
) -> PyResult<surge_dispatch::DispatchRequest> {
    let py = request.py();
    let json_module = py.import("json")?;
    let json: String = json_module.call_method1("dumps", (request,))?.extract()?;
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    let mut ignored = Vec::new();
    let parsed: surge_dispatch::DispatchRequest =
        serde_ignored::deserialize(&mut deserializer, |path| {
            ignored.push(path.to_string());
        })
        .map_err(|err| PyValueError::new_err(format!("invalid dispatch request: {err}")))?;
    if !ignored.is_empty() {
        return Err(PyValueError::new_err(format!(
            "invalid dispatch request: unexpected field(s): {}",
            ignored.join(", ")
        )));
    }
    Ok(parsed)
}

#[allow(dead_code)]
fn _unused(_: PyDict) {}
