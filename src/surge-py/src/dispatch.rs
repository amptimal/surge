// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python-facing dispatch bindings.

use std::path::PathBuf;
use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyAny;
use surge_opf::backends::{ac_opf_nlp_solver_from_str, lp_solver_from_str};

use crate::exceptions::{SurgeError, extract_panic_msg, to_dispatch_pyerr, to_io_pyerr};
use crate::network::Network;

fn json_loads<'py>(py: Python<'py>, text: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import("json")?.call_method1("loads", (text,))
}

fn json_dumps(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
    py.import("json")?
        .call_method1("dumps", (value,))?
        .extract::<String>()
}

fn serialize_to_py<'py, T: serde::Serialize + ?Sized>(
    py: Python<'py>,
    value: &T,
) -> PyResult<Bound<'py, PyAny>> {
    let json = serde_json::to_string(value).map_err(|err| {
        SurgeError::new_err(format!("failed to serialize dispatch result: {err}"))
    })?;
    json_loads(py, &json)
}

fn parse_request_json(py: Python<'_>, request: Option<&Bound<'_, PyAny>>) -> PyResult<String> {
    let Some(request) = request else {
        return Ok("{}".to_string());
    };

    if request.is_none() {
        return Ok("{}".to_string());
    }

    Ok(match request.extract::<String>() {
        Ok(text) => text,
        Err(_) => json_dumps(py, request)?,
    })
}

fn parse_dispatch_request(
    py: Python<'_>,
    request: Option<&Bound<'_, PyAny>>,
) -> PyResult<surge_dispatch::DispatchRequest> {
    let json = parse_request_json(py, request)?;
    let value: serde_json::Value = serde_json::from_str(&json)
        .map_err(|err| PyValueError::new_err(format!("invalid dispatch request JSON: {err}")))?;
    let runtime = value.get("runtime");
    if runtime
        .and_then(|runtime| runtime.get("lp_solver"))
        .is_some()
    {
        return Err(PyValueError::new_err(
            "dispatch request runtime.lp_solver is not part of the JSON contract; pass lp_solver=... to solve_dispatch() instead",
        ));
    }
    if runtime
        .and_then(|runtime| runtime.get("nlp_solver"))
        .is_some()
    {
        return Err(PyValueError::new_err(
            "dispatch request runtime.nlp_solver is not part of the JSON contract; pass nlp_solver=... to solve_dispatch() instead",
        ));
    }

    let mut ignored_paths = Vec::new();
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    let request = serde_ignored::deserialize(&mut deserializer, |path| {
        ignored_paths.push(path.to_string());
    })
    .map_err(|err| PyValueError::new_err(format!("invalid dispatch request: {err}")))?;
    if !ignored_paths.is_empty() {
        return Err(PyValueError::new_err(format!(
            "invalid dispatch request: unexpected field(s): {}",
            ignored_paths.join(", ")
        )));
    }
    Ok(request)
}

fn parse_activsg_case(case: &str) -> PyResult<surge_dispatch::datasets::ActivsgCase> {
    match case.to_ascii_lowercase().as_str() {
        "2000" | "activsg2000" => Ok(surge_dispatch::datasets::ActivsgCase::Activsg2000),
        "10k" | "10000" | "activsg10k" => Ok(surge_dispatch::datasets::ActivsgCase::Activsg10k),
        _ => Err(PyValueError::new_err(format!(
            "unknown ACTIVSg case `{case}`; expected `2000` or `10k`"
        ))),
    }
}

fn resolve_periods(total_periods: usize, periods: Option<usize>) -> PyResult<usize> {
    let periods = periods.unwrap_or(total_periods);
    if periods == 0 {
        return Err(PyValueError::new_err("periods must be >= 1"));
    }
    if periods > total_periods {
        return Err(PyValueError::new_err(format!(
            "periods {periods} exceeds imported ACTIVSg horizon {total_periods}"
        )));
    }
    Ok(periods)
}

fn to_activsg_pyerr(error: &surge_dispatch::datasets::ActivsgImportError) -> PyErr {
    use surge_dispatch::datasets::ActivsgImportError;

    match error {
        ActivsgImportError::Io(_)
        | ActivsgImportError::Csv(_)
        | ActivsgImportError::MissingFile { .. } => to_io_pyerr(error),
        ActivsgImportError::InvalidCsv { .. }
        | ActivsgImportError::InvalidRequestedPeriods { .. }
        | ActivsgImportError::DatasetTimestampMismatch { .. }
        | ActivsgImportError::MissingTimestampFill { .. }
        | ActivsgImportError::UnexpectedTimestamp { .. }
        | ActivsgImportError::UnknownBus { .. }
        | ActivsgImportError::UnknownGenerator { .. }
        | ActivsgImportError::NoSolarCandidates { .. }
        | ActivsgImportError::NameplateOverrideTargetMissing { .. }
        | ActivsgImportError::NonPositivePmax { .. }
        | ActivsgImportError::CapacityFactorOutOfRange { .. } => {
            PyValueError::new_err(error.to_string())
        }
    }
}

/// Python-facing TAMU ACTIVSg time-series bundle.
#[pyclass(name = "ActivsgTimeSeries")]
pub struct ActivsgTimeSeries {
    pub(crate) inner: surge_dispatch::datasets::ActivsgTimeSeries,
}

#[pymethods]
impl ActivsgTimeSeries {
    #[getter]
    fn case(&self) -> &'static str {
        match self.inner.case {
            surge_dispatch::datasets::ActivsgCase::Activsg2000 => "ACTIVSg2000",
            surge_dispatch::datasets::ActivsgCase::Activsg10k => "ACTIVSg10k",
        }
    }

    #[getter]
    fn periods(&self) -> usize {
        self.inner.periods()
    }

    #[getter]
    fn timestamps<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, &self.inner.timestamps)
    }

    #[getter]
    fn report<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, &self.inner.report)
    }

    #[getter]
    fn generator_pmax_overrides<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, &self.inner.generator_pmax_overrides)
    }

    #[pyo3(signature = (periods = None))]
    fn timeline<'py>(
        &self,
        py: Python<'py>,
        periods: Option<usize>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let periods = resolve_periods(self.inner.periods(), periods)?;
        let timeline = self
            .inner
            .timeline_for(periods)
            .map_err(|err| to_activsg_pyerr(&err))?;
        serialize_to_py(py, &timeline)
    }

    #[pyo3(signature = (periods = None))]
    fn dc_dispatch_profiles<'py>(
        &self,
        py: Python<'py>,
        periods: Option<usize>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let periods = resolve_periods(self.inner.periods(), periods)?;
        let profiles = self
            .inner
            .dc_profiles(periods)
            .map_err(|err| to_activsg_pyerr(&err))?;
        serialize_to_py(py, &profiles)
    }

    #[pyo3(signature = (periods = None))]
    fn ac_dispatch_profiles<'py>(
        &self,
        py: Python<'py>,
        periods: Option<usize>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let periods = resolve_periods(self.inner.periods(), periods)?;
        let profiles = self
            .inner
            .ac_profiles(periods)
            .map_err(|err| to_activsg_pyerr(&err))?;
        serialize_to_py(py, &profiles)
    }

    fn network_with_nameplate_overrides(&self, network: &Network) -> PyResult<Network> {
        let adjusted = self
            .inner
            .network_with_nameplate_overrides(&network.inner)
            .map_err(|err| to_activsg_pyerr(&err))?;
        Ok(Network {
            inner: Arc::new(adjusted),
            oltc_controls: network.oltc_controls.clone(),
            switched_shunts: network.switched_shunts.clone(),
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "ActivsgTimeSeries(case={}, periods={})",
            self.case(),
            self.inner.periods()
        )
    }
}

/// Unified dispatch result.
#[pyclass(name = "DispatchResult")]
pub struct DispatchResult {
    pub(crate) inner: surge_dispatch::DispatchSolution,
}

impl DispatchResult {
    /// Access the underlying Rust `DispatchSolution`. Used by the GO C3
    /// solution exporter (surge-py's `go_c3` module).
    pub(crate) fn inner(&self) -> &surge_dispatch::DispatchSolution {
        &self.inner
    }
}

#[pymethods]
impl DispatchResult {
    /// Canonical study metadata.
    #[getter]
    fn study<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.study())
    }

    /// Explicit public resource catalog.
    #[getter]
    fn resources<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.resources())
    }

    /// Explicit public bus catalog.
    #[getter]
    fn buses<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.buses())
    }

    /// Summary totals over the solved horizon.
    #[getter]
    fn summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.summary())
    }

    /// Solver/process diagnostics.
    #[getter]
    fn diagnostics<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.diagnostics())
    }

    /// Aggregated penalty/slack cost summary across all solved periods.
    ///
    /// Rolls up penalty dollars from all `constraint_results` entries,
    /// bucketed by category (power balance, reactive balance, thermal,
    /// flowgate, ramp, reserve shortfall, headroom/footroom, energy window).
    /// Per-bus, per-branch, and per-product detail is available in each
    /// period's `constraint_results` list.
    #[getter]
    fn penalty_summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.penalty_summary())
    }

    /// Persisted exact-audit block for the keyed dispatch result.
    #[getter]
    fn audit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.audit())
    }

    /// Per-period dispatch outcomes.
    #[getter]
    fn periods<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.periods())
    }

    /// Per-resource horizon summaries keyed by stable resource ids.
    #[getter]
    fn resource_summaries<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.resource_summaries())
    }

    /// Combined-cycle plant summaries keyed by stable plant ids.
    #[getter]
    fn combined_cycle_results<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.combined_cycle_results())
    }

    /// Model diagnostic snapshots (one per solve stage).
    ///
    /// Empty unless ``request["runtime"]["capture_model_diagnostics"] = True``.
    #[getter]
    fn model_diagnostics<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialize_to_py(py, self.inner.model_diagnostics())
    }

    /// Return a JSON string for the full dispatch result.
    fn to_json(&self) -> PyResult<String> {
        // Keep result export usable even when the exact objective-ledger audit
        // reports residual mismatches. The serialized payload still carries the
        // fresh audit block so callers can inspect or gate on it explicitly.
        let json = surge_io::json::encode_audited_solution(&self.inner).map_err(|err| {
            SurgeError::new_err(format!("failed to serialize dispatch result: {err}"))
        })?;
        serde_json::to_string_pretty(&json).map_err(|err| {
            SurgeError::new_err(format!("failed to serialize dispatch result: {err}"))
        })
    }

    /// Restore a dispatch result from ``to_json()`` output.
    #[staticmethod]
    fn from_json(s: &str) -> PyResult<Self> {
        let inner = serde_json::from_str::<surge_dispatch::DispatchSolution>(s)
            .map_err(|err| PyValueError::new_err(format!("invalid DispatchResult JSON: {err}")))?;
        Ok(Self { inner })
    }

    /// Return the dispatch result as built-in Python dict/list/scalar objects.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        json_loads(py, &self.to_json()?)
    }

    fn __repr__(&self) -> String {
        format!(
            "DispatchResult(periods={}, formulation={:?}, coupling={:?}, total_cost={:.3})",
            self.inner.study().periods,
            self.inner.study().formulation,
            self.inner.study().coupling,
            self.inner.summary().total_cost
        )
    }
}

/// Solve a canonical dispatch study from a JSON-like request object.
#[pyfunction]
#[pyo3(signature = (network, request = None, lp_solver = None, nlp_solver = None))]
pub fn solve_dispatch(
    py: Python<'_>,
    network: &Network,
    request: Option<Py<PyAny>>,
    lp_solver: Option<&str>,
    nlp_solver: Option<&str>,
) -> PyResult<DispatchResult> {
    network.validate()?;

    let request_obj = request.as_ref().map(|obj| obj.bind(py));
    let request = parse_dispatch_request(py, request_obj)?;
    let mut solve_options = surge_dispatch::DispatchSolveOptions::default();
    if let Some(name) = lp_solver {
        let solver = lp_solver_from_str(name).map_err(SurgeError::new_err)?;
        solve_options.lp_solver = Some(solver);
    }
    if let Some(name) = nlp_solver {
        let solver = ac_opf_nlp_solver_from_str(name).map_err(SurgeError::new_err)?;
        solve_options.nlp_solver = Some(solver);
    }

    let net = Arc::clone(&network.inner);
    let result = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let model = surge_dispatch::DispatchModel::prepare(&net)?;
                surge_dispatch::solve_dispatch_with_options(&model, &request, &solve_options)
            }))
            .map_err(|panic| {
                surge_dispatch::DispatchError::SolverError(format!(
                    "solve_dispatch failed: {}",
                    extract_panic_msg(panic)
                ))
            })
            .and_then(|result| result)
        })
        .map_err(|err| to_dispatch_pyerr(&err))?;

    Ok(DispatchResult { inner: result })
}

/// Assess AC pi-model violations for a solved dispatch result.
///
/// Computes bus P/Q balance mismatches and branch thermal overloads using
/// the exact pi-model power flow equations, then reports violations with
/// penalty costs.
#[pyfunction]
#[pyo3(signature = (network, result, p_bus_vio_cost = 1_000_000.0, q_bus_vio_cost = 1_000_000.0, s_vio_cost = 500.0, interval_hours = None))]
pub fn assess_dispatch_violations<'py>(
    py: Python<'py>,
    network: &Network,
    result: &DispatchResult,
    p_bus_vio_cost: f64,
    q_bus_vio_cost: f64,
    s_vio_cost: f64,
    interval_hours: Option<Vec<f64>>,
) -> PyResult<Bound<'py, PyAny>> {
    let costs = surge_dispatch::ViolationCosts {
        p_bus_vio_cost,
        q_bus_vio_cost,
        s_vio_cost,
    };
    let n_periods = result.inner.periods().len();
    let hours = interval_hours.unwrap_or_else(|| vec![1.0; n_periods]);
    let assessment =
        surge_dispatch::assess_dispatch_violations(&network.inner, &result.inner, &costs, &hours);
    serialize_to_py(py, &assessment)
}

/// Read the public TAMU ACTIVSg time-series package and build dispatch-ready profiles.
#[pyfunction]
#[pyo3(signature = (network, root, case = "2000"))]
pub fn read_tamu_activsg_time_series(
    py: Python<'_>,
    network: &Network,
    root: &str,
    case: &str,
) -> PyResult<ActivsgTimeSeries> {
    network.validate()?;

    let case = parse_activsg_case(case)?;
    let net = Arc::clone(&network.inner);
    let root = PathBuf::from(root);
    let imported = py.detach(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            surge_dispatch::datasets::read_tamu_activsg_time_series(
                &net,
                &root,
                case,
                &Default::default(),
            )
        }))
        .map_err(|panic| {
            SurgeError::new_err(format!(
                "read_tamu_activsg_time_series failed: {}",
                extract_panic_msg(panic)
            ))
        })
    })?;

    match imported {
        Ok(result) => Ok(ActivsgTimeSeries { inner: result }),
        Err(err) => Err(to_activsg_pyerr(&err)),
    }
}
