// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python bindings for contingency analysis (N-1, N-2, generator N-1, RAS).

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::exceptions::{NetworkError, catch_panic, extract_panic_msg, to_pyerr};
use crate::input_types::PyBranchKey;
use crate::network::Network;
use crate::solutions::ContingencyAnalysis;
use crate::utils::make_thread_pool;

/// Options for contingency analysis functions.
///
/// All parameters have sensible defaults. Create with
/// `ContingencyOptions(screening="lodf", thermal_threshold_pct=90.0)` etc.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct ContingencyOptions {
    #[pyo3(get, set)]
    screening: String,
    #[pyo3(get, set)]
    thermal_threshold_pct: f64,
    #[pyo3(get, set)]
    thermal_rating: Option<String>,
    #[pyo3(get, set)]
    vm_min: f64,
    #[pyo3(get, set)]
    vm_max: f64,
    #[pyo3(get, set)]
    lodf_screening_pct: f64,
    #[pyo3(get, set)]
    top_k: Option<usize>,
    #[pyo3(get, set)]
    corrective_dispatch: bool,
    #[pyo3(get, set)]
    detect_islands: bool,
    /// Voltage stress post-processing mode: "off", "proxy", or "exact_l_index".
    #[pyo3(get, set)]
    voltage_stress_mode: String,
    /// L-index threshold for voltage-stability classification (default: 0.7).
    /// Only used when voltage_stress_mode is "exact_l_index".
    #[pyo3(get, set)]
    l_index_threshold: f64,
    #[pyo3(get, set)]
    store_post_voltages: bool,
    #[pyo3(get, set)]
    contingency_flat_start: bool,
    #[pyo3(get, set)]
    discrete_controls: bool,
    #[pyo3(get, set)]
    include_breaker_contingencies: bool,
}

impl Default for ContingencyOptions {
    fn default() -> Self {
        Self {
            screening: "fdpf".to_string(),
            thermal_threshold_pct: 100.0,
            thermal_rating: None,
            vm_min: 0.95,
            vm_max: 1.05,
            lodf_screening_pct: 80.0,
            top_k: None,
            corrective_dispatch: false,
            detect_islands: true,
            voltage_stress_mode: "proxy".to_string(),
            l_index_threshold: 0.7,
            store_post_voltages: false,
            contingency_flat_start: false,
            discrete_controls: false,
            include_breaker_contingencies: false,
        }
    }
}

#[pymethods]
impl ContingencyOptions {
    #[new]
    #[pyo3(signature = (
        screening = "fdpf",
        thermal_threshold_pct = 100.0,
        thermal_rating = None,
        vm_min = 0.95,
        vm_max = 1.05,
        lodf_screening_pct = 80.0,
        top_k = None,
        corrective_dispatch = false,
        detect_islands = true,
        voltage_stress_mode = "proxy",
        l_index_threshold = 0.7,
        store_post_voltages = false,
        contingency_flat_start = false,
        discrete_controls = false,
        include_breaker_contingencies = false,
    ))]
    fn new(
        screening: &str,
        thermal_threshold_pct: f64,
        thermal_rating: Option<String>,
        vm_min: f64,
        vm_max: f64,
        lodf_screening_pct: f64,
        top_k: Option<usize>,
        corrective_dispatch: bool,
        detect_islands: bool,
        voltage_stress_mode: &str,
        l_index_threshold: f64,
        store_post_voltages: bool,
        contingency_flat_start: bool,
        discrete_controls: bool,
        include_breaker_contingencies: bool,
    ) -> Self {
        Self {
            screening: screening.to_string(),
            thermal_threshold_pct,
            thermal_rating,
            vm_min,
            vm_max,
            lodf_screening_pct,
            top_k,
            corrective_dispatch,
            detect_islands,
            voltage_stress_mode: voltage_stress_mode.to_string(),
            l_index_threshold,
            store_post_voltages,
            contingency_flat_start,
            discrete_controls,
            include_breaker_contingencies,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "ContingencyOptions(screening='{}', thermal={:.0}%, vm=[{:.2},{:.2}])",
            self.screening, self.thermal_threshold_pct, self.vm_min, self.vm_max
        )
    }
}

#[pyclass(unsendable, name = "PreparedCorrectiveDispatchStudy")]
pub struct PreparedCorrectiveDispatchStudy {
    inner: Option<surge_contingency::prepared::PreparedCorrectiveDispatchStudy<'static>>,
    network: Arc<surge_network::Network>,
}

impl PreparedCorrectiveDispatchStudy {
    fn inner_mut(
        &mut self,
    ) -> PyResult<&mut surge_contingency::prepared::PreparedCorrectiveDispatchStudy<'static>> {
        self.inner.as_mut().ok_or_else(|| {
            PyValueError::new_err("prepared corrective dispatch study is no longer available")
        })
    }
}

impl Drop for PreparedCorrectiveDispatchStudy {
    fn drop(&mut self) {
        let _ = self.inner.take();
    }
}

fn borrowed_network(network_arc: &Arc<surge_network::Network>) -> &'static surge_network::Network {
    unsafe {
        // SAFETY: the Arc is stored alongside the prepared study, keeping the
        // allocation alive for the study's full lifetime.
        &*Arc::as_ptr(network_arc)
    }
}

fn build_prepared_corrective_dispatch_study(
    network_arc: Arc<surge_network::Network>,
) -> PyResult<PreparedCorrectiveDispatchStudy> {
    let inner = surge_contingency::prepared::prepare_corrective_dispatch_study(borrowed_network(
        &network_arc,
    ))
    .map_err(to_pyerr)?;
    Ok(PreparedCorrectiveDispatchStudy {
        inner: Some(inner),
        network: network_arc,
    })
}

fn corrective_dispatch_results_to_pylist<'py>(
    py: Python<'py>,
    results: &[surge_contingency::prepared::CorrectiveDispatchResult],
) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for result in results {
        let item = PyDict::new(py);
        item.set_item("id", &result.contingency_id)?;
        item.set_item(
            "status",
            match result.status {
                surge_contingency::scrd::ScrdStatus::Optimal => "Optimal",
                surge_contingency::scrd::ScrdStatus::Infeasible => "Infeasible",
                surge_contingency::scrd::ScrdStatus::SolverError => "SolverError",
            },
        )?;
        item.set_item("total_redispatch_mw", result.total_redispatch_mw)?;
        item.set_item("total_cost", result.total_cost)?;
        item.set_item("violations_resolved", result.violations_resolved)?;
        item.set_item("unresolvable_violations", result.unresolvable_violations)?;
        list.append(item)?;
    }
    Ok(list)
}

#[pymethods]
impl PreparedCorrectiveDispatchStudy {
    fn solve_corrective_dispatch<'py>(
        &mut self,
        py: Python<'py>,
        contingency_analysis: &ContingencyAnalysis,
    ) -> PyResult<Bound<'py, PyList>> {
        let results = self
            .inner_mut()?
            .solve(&contingency_analysis.inner, None)
            .map_err(to_pyerr)?;
        corrective_dispatch_results_to_pylist(py, &results)
    }

    fn __repr__(&self) -> String {
        format!(
            "PreparedCorrectiveDispatchStudy(buses={}, branches={})",
            self.network.n_buses(),
            self.network.n_branches()
        )
    }
}

#[pyfunction]
pub fn prepare_corrective_dispatch_study(
    network: &Network,
) -> PyResult<PreparedCorrectiveDispatchStudy> {
    build_prepared_corrective_dispatch_study(Arc::clone(&network.inner))
}

#[pyclass(unsendable, name = "ContingencyStudy")]
pub struct ContingencyStudy {
    inner: Option<surge_contingency::prepared::ContingencyStudy<'static>>,
    network: Arc<surge_network::Network>,
    options: ContingencyOptions,
}

impl ContingencyStudy {
    fn inner_mut(
        &mut self,
    ) -> PyResult<&mut surge_contingency::prepared::ContingencyStudy<'static>> {
        self.inner
            .as_mut()
            .ok_or_else(|| PyValueError::new_err("contingency study is no longer available"))
    }
}

impl Drop for ContingencyStudy {
    fn drop(&mut self) {
        let _ = self.inner.take();
    }
}

#[pymethods]
impl ContingencyStudy {
    #[getter]
    fn kind(&mut self) -> PyResult<&'static str> {
        Ok(self.inner_mut()?.kind().as_str())
    }

    fn analyze(&mut self) -> PyResult<ContingencyAnalysis> {
        let inner = self.inner_mut()?.analyze_cloned().map_err(to_pyerr)?;
        Ok(ContingencyAnalysis { inner })
    }

    fn solve_corrective_dispatch<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let results = self
            .inner_mut()?
            .solve_corrective_dispatch()
            .map_err(to_pyerr)?;
        corrective_dispatch_results_to_pylist(py, &results)
    }

    fn __repr__(&self) -> String {
        format!(
            "ContingencyStudy(buses={}, branches={}, screening='{}')",
            self.network.n_buses(),
            self.network.n_branches(),
            self.options.screening
        )
    }
}

fn build_contingency_study_with_kind(
    py: Python<'_>,
    network: &Network,
    options: Option<ContingencyOptions>,
    kind: surge_contingency::prepared::ContingencyStudyKind,
) -> PyResult<ContingencyStudy> {
    let options = options.unwrap_or_default();
    if options.corrective_dispatch {
        return Err(PyValueError::new_err(
            "contingency study objects do not embed corrective redispatch; use solve_corrective_dispatch() explicitly after analyze()",
        ));
    }
    let net = Arc::clone(&network.inner);
    let pool = make_thread_pool()?;
    let prepared = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let opts = options.to_core(None).map_err(|e| e.to_string())?;
                pool.install(|| match kind {
                    surge_contingency::prepared::ContingencyStudyKind::N1Branch => {
                        surge_contingency::prepared::ContingencyStudy::n1_branch(
                            borrowed_network(&net),
                            &opts,
                        )
                        .map_err(|e| e.to_string())
                    }
                    surge_contingency::prepared::ContingencyStudyKind::N1Generator => {
                        surge_contingency::prepared::ContingencyStudy::n1_generator(
                            borrowed_network(&net),
                            &opts,
                        )
                        .map_err(|e| e.to_string())
                    }
                    surge_contingency::prepared::ContingencyStudyKind::N2Branch => {
                        surge_contingency::prepared::ContingencyStudy::n2_branch(
                            borrowed_network(&net),
                            &opts,
                        )
                        .map_err(|e| e.to_string())
                    }
                })
            }))
            .map_err(|e| format!("build_contingency_study failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(ContingencyStudy {
        inner: Some(prepared),
        network: Arc::clone(&network.inner),
        options,
    })
}

#[pyfunction]
#[pyo3(signature = (network, options=None))]
pub fn n1_branch_study(
    py: Python<'_>,
    network: &Network,
    options: Option<ContingencyOptions>,
) -> PyResult<ContingencyStudy> {
    build_contingency_study_with_kind(
        py,
        network,
        options,
        surge_contingency::prepared::ContingencyStudyKind::N1Branch,
    )
}

#[pyfunction]
#[pyo3(signature = (network, options=None))]
pub fn n1_generator_study(
    py: Python<'_>,
    network: &Network,
    options: Option<ContingencyOptions>,
) -> PyResult<ContingencyStudy> {
    build_contingency_study_with_kind(
        py,
        network,
        options,
        surge_contingency::prepared::ContingencyStudyKind::N1Generator,
    )
}

#[pyfunction]
#[pyo3(signature = (network, options=None))]
pub fn n2_branch_study(
    py: Python<'_>,
    network: &Network,
    options: Option<ContingencyOptions>,
) -> PyResult<ContingencyStudy> {
    build_contingency_study_with_kind(
        py,
        network,
        options,
        surge_contingency::prepared::ContingencyStudyKind::N2Branch,
    )
}

impl ContingencyOptions {
    pub fn to_core(
        &self,
        progress_cb: Option<surge_contingency::ProgressCallback>,
    ) -> Result<surge_contingency::ContingencyOptions, String> {
        let screening_mode = match self.screening.as_str() {
            "lodf" => surge_contingency::ScreeningMode::Lodf,
            "fdpf" => surge_contingency::ScreeningMode::Fdpf,
            "off" => surge_contingency::ScreeningMode::Off,
            other => {
                return Err(format!(
                    "screening must be 'lodf', 'fdpf', or 'off', got '{other}'"
                ));
            }
        };
        let rating = match self.thermal_rating.as_deref() {
            Some("rate_a") | None => surge_contingency::ThermalRating::RateA,
            Some("rate_b") => surge_contingency::ThermalRating::RateB,
            Some("rate_c") => surge_contingency::ThermalRating::RateC,
            Some(other) => {
                return Err(format!(
                    "thermal_rating must be 'rate_a', 'rate_b', or 'rate_c', got '{other}'"
                ));
            }
        };
        Ok(surge_contingency::ContingencyOptions {
            screening: screening_mode,
            thermal_threshold_frac: self.thermal_threshold_pct / 100.0,
            thermal_rating: rating,
            vm_min: self.vm_min,
            vm_max: self.vm_max,
            lodf_screening_threshold: self.lodf_screening_pct / 100.0,
            top_k: self.top_k,
            corrective_dispatch: self.corrective_dispatch,
            detect_islands: self.detect_islands,
            voltage_stress_mode: match self.voltage_stress_mode.as_str() {
                "off" => surge_contingency::VoltageStressMode::Off,
                "proxy" => surge_contingency::VoltageStressMode::Proxy,
                "exact_l_index" => surge_contingency::VoltageStressMode::ExactLIndex {
                    l_index_threshold: self.l_index_threshold,
                },
                other => {
                    return Err(format!(
                        "voltage_stress_mode must be 'off', 'proxy', or 'exact_l_index', got '{other}'"
                    ));
                }
            },
            store_post_voltages: self.store_post_voltages,
            contingency_flat_start: self.contingency_flat_start,
            discrete_controls: self.discrete_controls,
            include_breaker_contingencies: self.include_breaker_contingencies,
            progress_cb,
            ..Default::default()
        })
    }
}

/// Compute N-1 branch contingency analysis.
#[pyfunction]
#[pyo3(signature = (network, options=None, on_progress=None))]
pub fn analyze_n1_branch(
    py: Python<'_>,
    network: &Network,
    options: Option<ContingencyOptions>,
    on_progress: Option<Py<PyAny>>,
) -> PyResult<ContingencyAnalysis> {
    let opts_py = options.unwrap_or_default();
    // Wrap the Python callable in a Rust closure that re-acquires the GIL
    // and calls on_progress(completed, total) from whichever rayon thread
    // finishes a contingency.  GIL acquisition serialises Python calls but
    // does not affect other rayon workers.
    let progress_cb: Option<surge_contingency::ProgressCallback> = on_progress.map(|cb| {
        let arc_cb = std::sync::Arc::new(cb);
        surge_contingency::ProgressCallback(std::sync::Arc::new(
            move |done: usize, total: usize| {
                let _ = Python::attach(|py| arc_cb.call1(py, (done, total)));
            },
        ))
    });
    let opts = opts_py.to_core(progress_cb).map_err(to_pyerr)?;
    let net = Arc::clone(&network.inner);
    let pool = make_thread_pool()?;
    let inner = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pool.install(|| {
                    surge_contingency::analyze_n1_branch(&net, &opts).map_err(|e| e.to_string())
                })
            }))
            .map_err(|e| format!("analyze_n1_branch failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(ContingencyAnalysis { inner })
}

/// Generate breaker contingencies from a network's retained node-breaker topology.
///
/// Creates one contingency per closed breaker in the network's
/// ``NodeBreakerTopology``.  Each contingency opens one breaker and
/// rebuilds the network before solving.
///
/// Args:
///     network: Network with retained node-breaker topology (e.g. from CGMES or PSS/E v35+).
///
/// Returns:
///     List of ``Contingency`` objects with ``switches`` populated.
///
/// Raises:
///     NetworkError: If the network has no retained node-breaker topology.
#[pyfunction]
pub fn generate_breaker_contingencies(network: &Network) -> PyResult<Vec<Contingency>> {
    let sm =
        network.inner.topology.as_ref().ok_or_else(|| {
            NetworkError::new_err("network has no retained node-breaker topology")
        })?;
    let ctgs = surge_network::network::generate_breaker_contingencies(sm);
    Ok(ctgs
        .into_iter()
        .map(|c| Contingency {
            id: c.id,
            label: c.label,
            branches: vec![],
            generators: vec![],
            three_winding_transformers: vec![],
            switches: c.switch_ids,
            modifications: vec![],
        })
        .collect())
}

/// Compute N-2 simultaneous double branch contingency analysis (CTG-05).
///
/// Generates all C(n,2) branch pairs from the set of in-service branches, then
/// evaluates each pair with AC Newton-Raphson (optionally with LODF screening).
///
/// **Scale note**: grows as O(n²) — use screening='lodf' and top_k for large networks.
///
/// Args:
///     network: the power system network.
///     options: ContingencyOptions (default: all defaults).
///
/// Returns:
///     ContingencyAnalysis with all N-2 pair results.
#[pyfunction]
#[pyo3(signature = (network, options=None))]
pub fn analyze_n2_branch(
    py: Python<'_>,
    network: &Network,
    options: Option<ContingencyOptions>,
) -> PyResult<ContingencyAnalysis> {
    let opts_py = options.unwrap_or_default();
    let opts = opts_py.to_core(None).map_err(to_pyerr)?;
    let net = Arc::clone(&network.inner);
    let pool = make_thread_pool()?;
    let inner = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pool.install(|| {
                    surge_contingency::analyze_n2_branch(&net, &opts).map_err(|e| e.to_string())
                })
            }))
            .map_err(|e| format!("analyze_n2_branch failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(ContingencyAnalysis { inner })
}

/// Compute N-1 generator contingency analysis (CTG-01).
///
/// For each in-service generator, removes it from service and evaluates the
/// resulting power flow using the fast injection-vector path (no Y-bus rebuild).
///
/// This is significantly faster than branch N-1 for large generator fleets
/// because generator outages only change bus injections, not admittances.
///
/// Args:
///     network: the power system network.
///     options: ContingencyOptions (default: all defaults).
///
/// Returns:
///     ContingencyAnalysis with one result per in-service generator.
#[pyfunction]
#[pyo3(signature = (network, options=None))]
pub fn analyze_n1_generator(
    py: Python<'_>,
    network: &Network,
    options: Option<ContingencyOptions>,
) -> PyResult<ContingencyAnalysis> {
    let opts_py = options.unwrap_or_default();
    let opts = opts_py.to_core(None).map_err(to_pyerr)?;
    let net = Arc::clone(&network.inner);
    let inner = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_contingency::analyze_n1_generator(&net, &opts).map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("analyze_n1_generator failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(ContingencyAnalysis { inner })
}

// ---------------------------------------------------------------------------
// Phase 2: Contingency Python class + compute_contingencies()
// ---------------------------------------------------------------------------

/// Convert a list of Python dicts to `ContingencyModification` objects.
///
/// Each dict must have a `"type"` key identifying the modification variant.
/// Internally converts via JSON (dict → JSON string → serde deserialization).
pub fn py_mods_to_rust(
    py: Python<'_>,
    py_mods: &[pyo3::Py<pyo3::PyAny>],
) -> PyResult<Vec<surge_network::network::ContingencyModification>> {
    let json_mod = py.import("json")?;
    py_mods
        .iter()
        .map(|obj| {
            let json_str: String = json_mod.call_method1("dumps", (obj.bind(py),))?.extract()?;
            serde_json::from_str::<surge_network::network::ContingencyModification>(&json_str)
                .map_err(|e| {
                    pyo3::exceptions::PyValueError::new_err(format!(
                        "Invalid modification dict: {e}. \
                     Each dict must have a 'type' key matching a ContingencyModification variant \
                     (e.g. 'BranchTap', 'LoadSet', 'GenOutputSet', etc.)"
                    ))
                })
        })
        .collect()
}

/// A user-defined contingency (element trip or N-k outage).
///
/// Use in ``analyze_contingencies()`` to analyze specific contingencies
/// instead of the full N-1 set.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct Contingency {
    pub id: String,
    pub label: String,
    /// (from_bus, to_bus, circuit) tuples
    pub branches: Vec<(u32, u32, String)>,
    /// (bus, machine_id) tuples
    pub generators: Vec<(u32, String)>,
    /// (bus_i, bus_j, bus_k, circuit) tuples — three-winding transformer trips.
    ///
    /// Each entry represents a complete three-winding transformer outage.
    /// At contingency evaluation time, the star bus is located and all three
    /// winding branches are tripped.
    pub three_winding_transformers: Vec<(u32, u32, u32, String)>,
    /// Switch/breaker mRIDs to toggle for breaker contingencies.
    ///
    /// When non-empty, the contingency engine opens these switches and
    /// rebuild_topologys the network before solving the post-contingency power flow.
    pub switches: Vec<String>,
    /// Simultaneous network modifications (PSS/E .con SET/CHANGE commands).
    ///
    /// Applied to the per-contingency network clone before the power flow solve.
    pub modifications: Vec<surge_network::network::ContingencyModification>,
}

#[pymethods]
impl Contingency {
    /// Create a new contingency definition.
    ///
    /// Args:
    ///   id: Unique identifier string.
    ///   branches: List of (from_bus, to_bus, circuit) tuples to trip.
    ///   generators: List of (bus, machine_id) tuples to trip.
    ///   three_winding_transformers: List of (bus_i, bus_j, bus_k, circuit) tuples
    ///       for three-winding transformer trips.
    ///   label: Human-readable label (defaults to id).
    ///   modifications: List of dicts describing simultaneous network modifications
    ///       (PSS/E .con SET/CHANGE commands). Each dict must have a ``"type"`` key
    ///       and type-specific fields. Example::
    ///
    ///           [{"type": "BranchTap", "from_bus": 1, "to_bus": 2, "circuit": 1, "tap": 1.05},
    ///            {"type": "LoadSet", "bus": 10, "p_mw": 50.0, "q_mvar": 0.0}]
    #[new]
    #[pyo3(signature = (id, branches=None, generators=None, three_winding_transformers=None, label=None, modifications=None, switches=None))]
    fn new(
        py: Python<'_>,
        id: String,
        branches: Option<Vec<PyBranchKey>>,
        generators: Option<Vec<(u32, String)>>,
        three_winding_transformers: Option<Vec<(u32, u32, u32, String)>>,
        label: Option<String>,
        modifications: Option<Vec<pyo3::Py<pyo3::PyAny>>>,
        switches: Option<Vec<String>>,
    ) -> PyResult<Self> {
        let mods = if let Some(py_mods) = modifications {
            py_mods_to_rust(py, &py_mods)?
        } else {
            vec![]
        };
        Ok(Contingency {
            label: label.unwrap_or_else(|| id.clone()),
            id,
            branches: branches
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect(),
            generators: generators.unwrap_or_default(),
            three_winding_transformers: three_winding_transformers.unwrap_or_default(),
            switches: switches.unwrap_or_default(),
            modifications: mods,
        })
    }

    #[getter]
    fn id(&self) -> &str {
        &self.id
    }

    #[getter]
    fn label(&self) -> &str {
        &self.label
    }

    #[getter]
    fn branches(&self) -> Vec<(u32, u32, String)> {
        self.branches.clone()
    }

    #[getter]
    fn generators(&self) -> Vec<(u32, String)> {
        self.generators.clone()
    }

    #[getter]
    fn three_winding_transformers(&self) -> Vec<(u32, u32, u32, String)> {
        self.three_winding_transformers.clone()
    }

    #[getter]
    fn switches(&self) -> Vec<String> {
        self.switches.clone()
    }

    /// Network modifications as a list of JSON strings (one per modification).
    ///
    /// Each JSON object has a ``"type"`` key identifying the modification kind,
    /// plus type-specific fields. Returns an empty list for standard contingencies.
    #[getter]
    fn modifications<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<pyo3::Bound<'py, pyo3::types::PyList>> {
        let items: Vec<pyo3::Bound<'py, pyo3::types::PyAny>> = self
            .modifications
            .iter()
            .map(|m| {
                let json = serde_json::to_string(m).map_err(|e| {
                    PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string())
                })?;
                let obj =
                    pyo3::types::PyModule::import(py, "json")?.call_method1("loads", (json,))?;
                Ok(obj)
            })
            .collect::<PyResult<_>>()?;
        pyo3::types::PyList::new(py, items)
    }

    fn __repr__(&self) -> String {
        let mut parts = vec![
            format!("branches={}", self.branches.len()),
            format!("generators={}", self.generators.len()),
        ];
        if !self.three_winding_transformers.is_empty() {
            parts.push(format!(
                "three_winding_xfmrs={}",
                self.three_winding_transformers.len()
            ));
        }
        if !self.switches.is_empty() {
            parts.push(format!("switches={}", self.switches.len()));
        }
        if !self.modifications.is_empty() {
            parts.push(format!("modifications={}", self.modifications.len()));
        }
        format!("Contingency(id='{}', {})", self.id, parts.join(", "))
    }
}

/// Helper: convert Python Contingency list → surge_network::network::Contingency list.
///
/// Resolves (from_bus, to_bus, circuit) → internal branch index.
/// Resolves (bus, machine_id) → internal generator index.
pub fn to_core_contingencies(
    py_ctgs: &[Contingency],
    network: &surge_network::Network,
) -> PyResult<Vec<surge_network::network::Contingency>> {
    let bus_map = network.bus_index_map();
    let mut result = Vec::with_capacity(py_ctgs.len());
    for c in py_ctgs {
        // Resolve branch indices
        let mut branch_indices = Vec::new();
        for (fb, tb, ckt) in &c.branches {
            let idx = network
                .branches
                .iter()
                .enumerate()
                .find(|(_, br)| br.from_bus == *fb && br.to_bus == *tb && br.circuit == *ckt)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    NetworkError::new_err(format!(
                        "Contingency '{}': branch ({}, {}, ckt={}) not found",
                        c.id, fb, tb, ckt
                    ))
                })?;
            branch_indices.push(idx);
        }
        // Resolve three-winding transformer trips → branch indices.
        //
        // Each 3W xfmr is star-bus-expanded: a star bus S connects to buses
        // (i, j, k) via 3 branches.  We find S by looking for a common
        // neighbour of all three winding buses (with matching circuit).
        for (bi, bj, bk, ckt) in &c.three_winding_transformers {
            // Collect neighbour sets for each winding bus.
            let neighbours = |bus: u32| -> std::collections::HashMap<u32, usize> {
                let mut map = std::collections::HashMap::new();
                for (idx, br) in network.branches.iter().enumerate() {
                    if br.circuit != *ckt {
                        continue;
                    }
                    if br.from_bus == bus {
                        map.insert(br.to_bus, idx);
                    } else if br.to_bus == bus {
                        map.insert(br.from_bus, idx);
                    }
                }
                map
            };
            let ni = neighbours(*bi);
            let nj = neighbours(*bj);
            let nk = neighbours(*bk);

            // Star bus is the bus present in all three neighbour sets.
            let star_bus = ni
                .keys()
                .find(|s| nj.contains_key(s) && nk.contains_key(s))
                .copied()
                .ok_or_else(|| {
                    NetworkError::new_err(format!(
                        "Contingency '{}': three-winding xfmr ({},{},{} ckt={}) — \
                         could not find star bus connecting all three windings",
                        c.id, bi, bj, bk, ckt
                    ))
                })?;

            branch_indices.push(ni[&star_bus]);
            branch_indices.push(nj[&star_bus]);
            branch_indices.push(nk[&star_bus]);
        }

        // Resolve generator indices
        let mut generator_indices = Vec::new();
        for (bus, mid) in &c.generators {
            let bus_idx = bus_map.get(bus).copied().ok_or_else(|| {
                NetworkError::new_err(format!("Contingency '{}': bus {} not found", c.id, bus))
            })?;
            let gen_idx = network
                .generators
                .iter()
                .enumerate()
                .find(|(_, g)| {
                    bus_map.get(&g.bus).copied() == Some(bus_idx)
                        && g.machine_id.as_deref().unwrap_or("1") == mid.as_str()
                })
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    NetworkError::new_err(format!(
                        "Contingency '{}': generator (bus={}, id='{}') not found",
                        c.id, bus, mid
                    ))
                })?;
            generator_indices.push(gen_idx);
        }
        result.push(surge_network::network::Contingency {
            id: c.id.clone(),
            label: c.label.clone(),
            branch_indices,
            generator_indices,
            switch_ids: c.switches.clone(),
            modifications: c.modifications.clone(),
            ..Default::default()
        });
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Phase 1B: Remedial Action Scheme (RAS/SPS) Python classes
// ---------------------------------------------------------------------------

/// A corrective action to apply post-contingency.
///
/// Use class methods to construct specific action types:
/// - ``CorrectiveAction.gen_redispatch(bus, machine_id, delta_mw)``
/// - ``CorrectiveAction.tap_change(from_bus, to_bus, circuit, new_tap)``
/// - ``CorrectiveAction.shunt_switch(bus, delta_mvar)``
/// - ``CorrectiveAction.load_shed(bus, fraction)``
#[pyclass(name = "CorrectiveAction", from_py_object)]
#[derive(Clone)]
pub struct CorrectiveAction {
    kind: String,
    // Store the external identifiers; resolve to internal indices at apply time.
    bus: Option<u32>,
    from_bus: Option<u32>,
    to_bus: Option<u32>,
    circuit: Option<String>,
    machine_id: Option<String>,
    delta_p_mw: Option<f64>,
    new_tap: Option<f64>,
    delta_mvar: Option<f64>,
    shed_fraction: Option<f64>,
}

#[pymethods]
impl CorrectiveAction {
    /// Redispatch a generator (increase or decrease MW output).
    #[staticmethod]
    #[pyo3(signature = (bus, machine_id="1", delta_mw=0.0))]
    fn gen_redispatch(bus: u32, machine_id: &str, delta_mw: f64) -> Self {
        CorrectiveAction {
            kind: "gen_redispatch".into(),
            bus: Some(bus),
            machine_id: Some(machine_id.to_string()),
            delta_p_mw: Some(delta_mw),
            from_bus: None,
            to_bus: None,
            circuit: None,
            new_tap: None,
            delta_mvar: None,
            shed_fraction: None,
        }
    }

    /// Change a transformer tap ratio.
    #[staticmethod]
    #[pyo3(signature = (from_bus, to_bus, circuit=None, new_tap=1.0))]
    fn tap_change(
        from_bus: u32,
        to_bus: u32,
        circuit: Option<pyo3::Bound<'_, pyo3::PyAny>>,
        new_tap: f64,
    ) -> PyResult<Self> {
        let ckt_str: String = match circuit {
            None => "1".to_string(),
            Some(c) => {
                if let Ok(n) = c.extract::<i64>() {
                    n.to_string()
                } else {
                    c.extract::<String>()?
                }
            }
        };
        Ok(CorrectiveAction {
            kind: "tap_change".into(),
            from_bus: Some(from_bus),
            to_bus: Some(to_bus),
            circuit: Some(ckt_str),
            new_tap: Some(new_tap),
            bus: None,
            machine_id: None,
            delta_p_mw: None,
            delta_mvar: None,
            shed_fraction: None,
        })
    }

    /// Switch shunt susceptance at a bus (positive = capacitive Mvar).
    #[staticmethod]
    #[pyo3(signature = (bus, delta_mvar=0.0))]
    fn shunt_switch(bus: u32, delta_mvar: f64) -> Self {
        CorrectiveAction {
            kind: "shunt_switch".into(),
            bus: Some(bus),
            delta_mvar: Some(delta_mvar),
            from_bus: None,
            to_bus: None,
            circuit: None,
            machine_id: None,
            delta_p_mw: None,
            new_tap: None,
            shed_fraction: None,
        }
    }

    /// Shed a fraction of load at a bus (0.0 = none, 1.0 = all).
    #[staticmethod]
    #[pyo3(signature = (bus, fraction=0.0))]
    fn load_shed(bus: u32, fraction: f64) -> Self {
        CorrectiveAction {
            kind: "load_shed".into(),
            bus: Some(bus),
            shed_fraction: Some(fraction),
            from_bus: None,
            to_bus: None,
            circuit: None,
            machine_id: None,
            delta_p_mw: None,
            new_tap: None,
            delta_mvar: None,
        }
    }

    #[getter]
    fn action_type(&self) -> &str {
        &self.kind
    }

    #[setter]
    fn set_action_type(&mut self, action_type: String) {
        self.kind = action_type;
    }

    #[getter]
    fn bus(&self) -> Option<u32> {
        self.bus
    }

    #[setter]
    fn set_bus(&mut self, bus: Option<u32>) {
        self.bus = bus;
    }

    #[getter(from_bus)]
    fn get_from_bus(&self) -> Option<u32> {
        self.from_bus
    }

    #[setter(from_bus)]
    fn set_from_bus_prop(&mut self, from_bus: Option<u32>) {
        self.from_bus = from_bus;
    }

    #[getter]
    fn to_bus(&self) -> Option<u32> {
        self.to_bus
    }

    #[setter]
    fn set_to_bus(&mut self, to_bus: Option<u32>) {
        self.to_bus = to_bus;
    }

    #[getter]
    fn circuit(&self) -> Option<String> {
        self.circuit.clone()
    }

    #[setter]
    fn set_circuit(&mut self, circuit: Option<String>) {
        self.circuit = circuit;
    }

    #[getter]
    fn machine_id(&self) -> Option<String> {
        self.machine_id.clone()
    }

    #[setter]
    fn set_machine_id(&mut self, machine_id: Option<String>) {
        self.machine_id = machine_id;
    }

    #[getter]
    fn delta_mw(&self) -> Option<f64> {
        self.delta_p_mw
    }

    #[setter]
    fn set_delta_mw(&mut self, delta_mw: Option<f64>) {
        self.delta_p_mw = delta_mw;
    }

    #[getter]
    fn new_tap(&self) -> Option<f64> {
        self.new_tap
    }

    #[setter]
    fn set_new_tap(&mut self, new_tap: Option<f64>) {
        self.new_tap = new_tap;
    }

    #[getter]
    fn delta_mvar(&self) -> Option<f64> {
        self.delta_mvar
    }

    #[setter]
    fn set_delta_mvar(&mut self, delta_mvar: Option<f64>) {
        self.delta_mvar = delta_mvar;
    }

    #[getter]
    fn shed_fraction(&self) -> Option<f64> {
        self.shed_fraction
    }

    #[setter]
    fn set_shed_fraction(&mut self, shed_fraction: Option<f64>) {
        self.shed_fraction = shed_fraction;
    }

    fn __repr__(&self) -> String {
        match self.kind.as_str() {
            "gen_redispatch" => format!(
                "CorrectiveAction.gen_redispatch(bus={}, id='{}', delta_mw={:.1})",
                self.bus.unwrap_or(0),
                self.machine_id.as_deref().unwrap_or("1"),
                self.delta_p_mw.unwrap_or(0.0),
            ),
            "tap_change" => format!(
                "CorrectiveAction.tap_change({}->{} ckt={}, tap={:.4})",
                self.from_bus.unwrap_or(0),
                self.to_bus.unwrap_or(0),
                self.circuit.as_deref().unwrap_or("1"),
                self.new_tap.unwrap_or(1.0),
            ),
            "shunt_switch" => format!(
                "CorrectiveAction.shunt_switch(bus={}, delta_mvar={:.1})",
                self.bus.unwrap_or(0),
                self.delta_mvar.unwrap_or(0.0),
            ),
            "load_shed" => format!(
                "CorrectiveAction.load_shed(bus={}, fraction={:.2})",
                self.bus.unwrap_or(0),
                self.shed_fraction.unwrap_or(0.0),
            ),
            _ => format!("CorrectiveAction(kind='{}')", self.kind),
        }
    }
}

/// Convert a Python CorrectiveAction → Rust CorrectiveAction.
pub fn to_core_action(
    a: &CorrectiveAction,
    net: &surge_network::Network,
) -> PyResult<surge_contingency::corrective::CorrectiveAction> {
    let bus_map = net.bus_index_map();
    match a.kind.as_str() {
        "gen_redispatch" => {
            let bus = a.bus.unwrap_or(0);
            let mid = a.machine_id.as_deref().unwrap_or("1");
            let gen_idx = net
                .generators
                .iter()
                .enumerate()
                .find(|(_, g)| g.bus == bus && g.machine_id.as_deref().unwrap_or("1") == mid)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    NetworkError::new_err(format!(
                        "CorrectiveAction: generator (bus={bus}, id='{mid}') not found"
                    ))
                })?;
            Ok(
                surge_contingency::corrective::CorrectiveAction::GeneratorRedispatch {
                    gen_idx,
                    delta_p_mw: a.delta_p_mw.unwrap_or(0.0),
                },
            )
        }
        "tap_change" => {
            let fb = a.from_bus.unwrap_or(0);
            let tb = a.to_bus.unwrap_or(0);
            let ckt = a.circuit.clone().unwrap_or_else(|| "1".to_string());
            let branch_idx = net
                .branches
                .iter()
                .enumerate()
                .find(|(_, br)| br.from_bus == fb && br.to_bus == tb && br.circuit == ckt)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    NetworkError::new_err(format!(
                        "CorrectiveAction: branch ({fb}, {tb}, ckt={ckt}) not found"
                    ))
                })?;
            Ok(
                surge_contingency::corrective::CorrectiveAction::TransformerTapChange {
                    branch_idx,
                    new_tap: a.new_tap.unwrap_or(1.0),
                },
            )
        }
        "shunt_switch" => {
            let bus = a.bus.unwrap_or(0);
            let bus_idx = *bus_map.get(&bus).ok_or_else(|| {
                NetworkError::new_err(format!("CorrectiveAction: bus {bus} not found"))
            })?;
            Ok(
                surge_contingency::corrective::CorrectiveAction::ShuntSwitch {
                    bus: bus_idx,
                    delta_b_pu: a.delta_mvar.unwrap_or(0.0) / net.base_mva,
                },
            )
        }
        "load_shed" => {
            let bus = a.bus.unwrap_or(0);
            let bus_idx = *bus_map.get(&bus).ok_or_else(|| {
                NetworkError::new_err(format!("CorrectiveAction: bus {bus} not found"))
            })?;
            Ok(surge_contingency::corrective::CorrectiveAction::LoadShed {
                bus: bus_idx,
                shed_fraction: a.shed_fraction.unwrap_or(0.0),
            })
        }
        "breaker_switch" => {
            let fb = a.from_bus.unwrap_or(0);
            let tb = a.to_bus.unwrap_or(0);
            let ckt = a.circuit.clone().unwrap_or_else(|| "1".to_string());
            let branch_idx = net
                .branches
                .iter()
                .enumerate()
                .find(|(_, br)| br.from_bus == fb && br.to_bus == tb && br.circuit == ckt)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    NetworkError::new_err(format!(
                        "CorrectiveAction: branch ({fb}, {tb}, ckt={ckt}) not found"
                    ))
                })?;
            // delta_p_mw > 0 means close, <= 0 means open
            let close = a.delta_p_mw.unwrap_or(1.0) > 0.0;
            Ok(
                surge_contingency::corrective::CorrectiveAction::BreakerSwitch {
                    branch_idx,
                    close,
                },
            )
        }
        "gen_voltage_setpoint" => {
            let bus = a.bus.unwrap_or(0);
            let mid = a.machine_id.as_deref().unwrap_or("1");
            let gen_idx = net
                .generators
                .iter()
                .enumerate()
                .find(|(_, g)| g.bus == bus && g.machine_id.as_deref().unwrap_or("1") == mid)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    NetworkError::new_err(format!(
                        "CorrectiveAction: generator (bus={bus}, id='{mid}') not found"
                    ))
                })?;
            Ok(
                surge_contingency::corrective::CorrectiveAction::GeneratorVoltageSetpoint {
                    gen_idx,
                    new_vs_pu: a.new_tap.unwrap_or(1.0),
                },
            )
        }
        "gen_reactive_dispatch" => {
            let bus = a.bus.unwrap_or(0);
            let mid = a.machine_id.as_deref().unwrap_or("1");
            let gen_idx = net
                .generators
                .iter()
                .enumerate()
                .find(|(_, g)| g.bus == bus && g.machine_id.as_deref().unwrap_or("1") == mid)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    NetworkError::new_err(format!(
                        "CorrectiveAction: generator (bus={bus}, id='{mid}') not found"
                    ))
                })?;
            Ok(
                surge_contingency::corrective::CorrectiveAction::GeneratorReactiveDispatch {
                    gen_idx,
                    delta_q_mvar: a.delta_mvar.unwrap_or(0.0),
                },
            )
        }
        other => Err(PyValueError::new_err(format!(
            "Unknown CorrectiveAction kind: '{other}'"
        ))),
    }
}

/// A Remedial Action Scheme (RAS/SPS) triggered by specific contingencies.
///
/// Example::
///
///     ras = surge.RemedialAction(
///         name="GenTrip_RAS",
///         trigger_branches=[(100, 200, 1)],
///         actions=[
///             surge.CorrectiveAction.gen_redispatch(500, "1", delta_mw=-50),
///             surge.CorrectiveAction.load_shed(600, fraction=0.1),
///         ],
///         modifications=[
///             {"type": "BranchClose", "from_bus": 200, "to_bus": 300, "circuit": 1},
///         ],
///     )
#[pyclass(name = "RemedialAction", from_py_object)]
pub struct RemedialAction {
    name: String,
    trigger_branches: Vec<(u32, u32, String)>,
    trigger_conditions: Vec<Py<PyAny>>,
    arm_conditions: Vec<Py<PyAny>>,
    priority: i32,
    exclusion_group: Option<String>,
    actions: Vec<CorrectiveAction>,
    modifications: Vec<surge_network::network::ContingencyModification>,
    max_redispatch_mw: f64,
}

impl Clone for RemedialAction {
    fn clone(&self) -> Self {
        Python::try_attach(|py| RemedialAction {
            name: self.name.clone(),
            trigger_branches: self.trigger_branches.clone(),
            trigger_conditions: self
                .trigger_conditions
                .iter()
                .map(|o| o.clone_ref(py))
                .collect(),
            arm_conditions: self
                .arm_conditions
                .iter()
                .map(|o| o.clone_ref(py))
                .collect(),
            priority: self.priority,
            exclusion_group: self.exclusion_group.clone(),
            actions: self.actions.clone(),
            modifications: self.modifications.clone(),
            max_redispatch_mw: self.max_redispatch_mw,
        })
        .expect("RemedialAction::clone requires the Python GIL to be held")
    }
}

#[pymethods]
impl RemedialAction {
    #[new]
    #[pyo3(signature = (name, trigger_branches=vec![], actions=vec![], modifications=None, max_redispatch_mw=f64::INFINITY, priority=0, exclusion_group=None, trigger_conditions=vec![], arm_conditions=vec![]))]
    fn new(
        py: Python<'_>,
        name: String,
        trigger_branches: Vec<(u32, u32, String)>,
        actions: Vec<CorrectiveAction>,
        modifications: Option<Vec<pyo3::Py<pyo3::PyAny>>>,
        max_redispatch_mw: f64,
        priority: i32,
        exclusion_group: Option<String>,
        trigger_conditions: Vec<Py<PyAny>>,
        arm_conditions: Vec<Py<PyAny>>,
    ) -> PyResult<Self> {
        let modifications = match modifications {
            Some(mods) => py_mods_to_rust(py, &mods)?,
            None => vec![],
        };
        Ok(RemedialAction {
            name,
            trigger_branches,
            trigger_conditions,
            arm_conditions,
            priority,
            exclusion_group,
            actions,
            modifications,
            max_redispatch_mw,
        })
    }

    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[setter]
    fn set_name(&mut self, name: String) {
        self.name = name;
    }

    #[getter]
    fn trigger_branches(&self) -> Vec<(u32, u32, String)> {
        self.trigger_branches.clone()
    }

    #[setter]
    fn set_trigger_branches(&mut self, trigger_branches: Vec<(u32, u32, String)>) {
        self.trigger_branches = trigger_branches;
    }

    #[getter]
    fn trigger_conditions<'py>(&self, py: Python<'py>) -> Vec<Py<PyAny>> {
        self.trigger_conditions
            .iter()
            .map(|condition| condition.clone_ref(py))
            .collect()
    }

    #[setter]
    fn set_trigger_conditions(&mut self, trigger_conditions: Vec<Py<PyAny>>) {
        self.trigger_conditions = trigger_conditions;
    }

    #[getter]
    fn arm_conditions<'py>(&self, py: Python<'py>) -> Vec<Py<PyAny>> {
        self.arm_conditions
            .iter()
            .map(|condition| condition.clone_ref(py))
            .collect()
    }

    #[setter]
    fn set_arm_conditions(&mut self, arm_conditions: Vec<Py<PyAny>>) {
        self.arm_conditions = arm_conditions;
    }

    #[getter]
    fn actions(&self) -> Vec<CorrectiveAction> {
        self.actions.clone()
    }

    #[setter]
    fn set_actions(&mut self, actions: Vec<CorrectiveAction>) {
        self.actions = actions;
    }

    #[getter]
    fn modifications<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<pyo3::Bound<'py, pyo3::types::PyList>> {
        let items: Vec<pyo3::Bound<'py, pyo3::types::PyAny>> = self
            .modifications
            .iter()
            .map(|m| {
                let json = serde_json::to_string(m).map_err(|e| {
                    PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string())
                })?;
                let obj =
                    pyo3::types::PyModule::import(py, "json")?.call_method1("loads", (json,))?;
                Ok(obj)
            })
            .collect::<PyResult<_>>()?;
        pyo3::types::PyList::new(py, items)
    }

    #[setter]
    fn set_modifications(&mut self, py: Python<'_>, modifications: Vec<Py<PyAny>>) -> PyResult<()> {
        self.modifications = py_mods_to_rust(py, &modifications)?;
        Ok(())
    }

    #[getter]
    fn max_redispatch_mw(&self) -> f64 {
        self.max_redispatch_mw
    }

    #[setter]
    fn set_max_redispatch_mw(&mut self, max_redispatch_mw: f64) {
        self.max_redispatch_mw = max_redispatch_mw;
    }

    #[getter]
    fn priority(&self) -> i32 {
        self.priority
    }

    #[setter]
    fn set_priority(&mut self, priority: i32) {
        self.priority = priority;
    }

    #[getter]
    fn exclusion_group(&self) -> Option<String> {
        self.exclusion_group.clone()
    }

    #[setter]
    fn set_exclusion_group(&mut self, exclusion_group: Option<String>) {
        self.exclusion_group = exclusion_group;
    }

    fn __repr__(&self) -> String {
        format!(
            "RemedialAction(name='{}', priority={}, triggers={}, actions={})",
            self.name,
            self.priority,
            self.trigger_branches.len() + self.trigger_conditions.len(),
            self.actions.len(),
        )
    }
}

/// Convert a Python dict trigger condition to a Rust RasTriggerCondition.
///
/// Supported dict formats:
///   {"type": "BranchOutaged", "branch_idx": 5}
///   {"type": "PostCtgBranchLoading", "branch_idx": 5, "threshold_pct": 110.0}
///   {"type": "PostCtgVoltageLow", "bus_number": 42, "threshold_pu": 0.92}
///   {"type": "PostCtgVoltageHigh", "bus_number": 42, "threshold_pu": 1.06}
///   {"type": "PostCtgFlowgateOverload", "flowgate_name": "WN", "threshold_pct": 100.0}
///   {"type": "PostCtgInterfaceOverload", "interface_name": "WN", "threshold_pct": 100.0}
pub fn py_trigger_condition_to_rust(
    _py: Python<'_>,
    obj: &pyo3::Bound<'_, PyAny>,
    _net: &surge_network::Network,
) -> PyResult<surge_contingency::corrective::RasTriggerCondition> {
    use surge_contingency::corrective::RasTriggerCondition;
    let ty: String = obj.get_item("type")?.extract()?;
    match ty.as_str() {
        "BranchOutaged" => {
            let idx: usize = obj.get_item("branch_idx")?.extract()?;
            Ok(RasTriggerCondition::BranchOutaged { branch_idx: idx })
        }
        "PostCtgBranchLoading" => {
            let idx: usize = obj.get_item("branch_idx")?.extract()?;
            let pct: f64 = obj.get_item("threshold_pct")?.extract()?;
            Ok(RasTriggerCondition::PostCtgBranchLoading {
                branch_idx: idx,
                threshold_pct: pct,
            })
        }
        "PostCtgVoltageLow" => {
            let bus: u32 = obj.get_item("bus_number")?.extract()?;
            let pu: f64 = obj.get_item("threshold_pu")?.extract()?;
            Ok(RasTriggerCondition::PostCtgVoltageLow {
                bus_number: bus,
                threshold_pu: pu,
            })
        }
        "PostCtgVoltageHigh" => {
            let bus: u32 = obj.get_item("bus_number")?.extract()?;
            let pu: f64 = obj.get_item("threshold_pu")?.extract()?;
            Ok(RasTriggerCondition::PostCtgVoltageHigh {
                bus_number: bus,
                threshold_pu: pu,
            })
        }
        "PostCtgFlowgateOverload" => {
            let name: String = obj.get_item("flowgate_name")?.extract()?;
            let pct: f64 = obj.get_item("threshold_pct")?.extract()?;
            Ok(RasTriggerCondition::PostCtgFlowgateOverload {
                flowgate_name: name,
                threshold_pct: pct,
            })
        }
        "PostCtgInterfaceOverload" => {
            let name: String = obj.get_item("interface_name")?.extract()?;
            let pct: f64 = obj.get_item("threshold_pct")?.extract()?;
            Ok(RasTriggerCondition::PostCtgInterfaceOverload {
                interface_name: name,
                threshold_pct: pct,
            })
        }
        other => Err(PyValueError::new_err(format!(
            "Unknown trigger condition type: '{other}'. Expected one of: BranchOutaged, \
             PostCtgBranchLoading, PostCtgVoltageLow, PostCtgVoltageHigh, \
             PostCtgFlowgateOverload, PostCtgInterfaceOverload"
        ))),
    }
}

/// Convert a Python dict arm condition to a Rust ArmCondition.
///
/// Supported dict formats:
///   {"type": "BranchLoading", "branch_idx": 5, "threshold_pct": 80.0}
///   {"type": "VoltageLow", "bus_idx": 42, "threshold_pu": 0.95}
///   {"type": "VoltageHigh", "bus_idx": 42, "threshold_pu": 1.05}
///   {"type": "InterfaceFlow", "name": "WN", "branches": [(5, 1.0), (6, -1.0)], "threshold_mw": 5000.0}
///   {"type": "SystemGenerationAbove", "threshold_mw": 50000.0}
///   {"type": "SystemGenerationBelow", "threshold_mw": 20000.0}
pub fn py_arm_condition_to_rust(
    _py: Python<'_>,
    obj: &pyo3::Bound<'_, PyAny>,
    _net: &surge_network::Network,
) -> PyResult<surge_contingency::corrective::ArmCondition> {
    use surge_contingency::corrective::ArmCondition;
    let ty: String = obj.get_item("type")?.extract()?;
    match ty.as_str() {
        "BranchLoading" => {
            let idx: usize = obj.get_item("branch_idx")?.extract()?;
            let pct: f64 = obj.get_item("threshold_pct")?.extract()?;
            Ok(ArmCondition::BranchLoading {
                branch_idx: idx,
                threshold_pct: pct,
            })
        }
        "VoltageLow" => {
            let idx: usize = obj.get_item("bus_idx")?.extract()?;
            let pu: f64 = obj.get_item("threshold_pu")?.extract()?;
            Ok(ArmCondition::VoltageLow {
                bus_idx: idx,
                threshold_pu: pu,
            })
        }
        "VoltageHigh" => {
            let idx: usize = obj.get_item("bus_idx")?.extract()?;
            let pu: f64 = obj.get_item("threshold_pu")?.extract()?;
            Ok(ArmCondition::VoltageHigh {
                bus_idx: idx,
                threshold_pu: pu,
            })
        }
        "InterfaceFlow" => {
            let name: String = obj.get_item("name")?.extract()?;
            let branches: Vec<(usize, f64)> = obj.get_item("branches")?.extract()?;
            let mw: f64 = obj.get_item("threshold_mw")?.extract()?;
            Ok(ArmCondition::InterfaceFlow {
                name,
                branch_coefficients: branches,
                threshold_mw: mw,
            })
        }
        "SystemGenerationAbove" => {
            let mw: f64 = obj.get_item("threshold_mw")?.extract()?;
            Ok(ArmCondition::SystemGenerationAbove { threshold_mw: mw })
        }
        "SystemGenerationBelow" => {
            let mw: f64 = obj.get_item("threshold_mw")?.extract()?;
            Ok(ArmCondition::SystemGenerationBelow { threshold_mw: mw })
        }
        other => Err(PyValueError::new_err(format!(
            "Unknown arm condition type: '{other}'. Expected one of: BranchLoading, \
             VoltageLow, VoltageHigh, InterfaceFlow, SystemGenerationAbove, \
             SystemGenerationBelow"
        ))),
    }
}

/// Convert Python RAS list → Rust RAS config.
pub fn to_core_ras(
    py: Python<'_>,
    py_ras: &[RemedialAction],
    net: &surge_network::Network,
) -> PyResult<surge_contingency::corrective::CorrectiveActionConfig> {
    let mut schemes = Vec::with_capacity(py_ras.len());
    for ras in py_ras {
        // Convert trigger_branches tuples → BranchOutaged trigger conditions.
        let mut trigger_conditions = Vec::new();
        for (fb, tb, ckt) in &ras.trigger_branches {
            let idx = net
                .branches
                .iter()
                .enumerate()
                .find(|(_, br)| br.from_bus == *fb && br.to_bus == *tb && br.circuit == *ckt)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    NetworkError::new_err(format!(
                        "RemedialAction '{}': trigger branch ({fb}, {tb}, ckt={ckt}) not found",
                        ras.name
                    ))
                })?;
            trigger_conditions.push(
                surge_contingency::corrective::RasTriggerCondition::BranchOutaged {
                    branch_idx: idx,
                },
            );
        }

        // Convert explicit trigger_conditions from Python dicts.
        for tc_obj in &ras.trigger_conditions {
            let tc = py_trigger_condition_to_rust(py, tc_obj.bind(py), net)?;
            trigger_conditions.push(tc);
        }

        // Convert arm_conditions from Python dicts.
        let mut arm_conditions = Vec::new();
        for ac_obj in &ras.arm_conditions {
            let ac = py_arm_condition_to_rust(py, ac_obj.bind(py), net)?;
            arm_conditions.push(ac);
        }

        // Resolve actions.
        let mut core_actions = Vec::with_capacity(ras.actions.len());
        for a in &ras.actions {
            core_actions.push(to_core_action(a, net)?);
        }
        schemes.push(surge_contingency::corrective::RemedialActionScheme {
            name: ras.name.clone(),
            priority: ras.priority,
            exclusion_group: ras.exclusion_group.clone(),
            arm_conditions,
            trigger_conditions,
            actions: core_actions,
            modifications: ras.modifications.clone(),
            max_redispatch_mw: ras.max_redispatch_mw,
        });
    }
    Ok(surge_contingency::corrective::CorrectiveActionConfig {
        schemes,
        ..Default::default()
    })
}

/// Apply corrective actions (RAS/SPS) to contingency results.
///
/// Takes a completed ``ContingencyAnalysis`` and a list of ``RemedialAction``
/// schemes, then re-evaluates contingencies that had violations. Returns a new
/// ``ContingencyAnalysis`` with corrective action results.
#[pyfunction]
#[pyo3(signature = (network, ca_result, ras_schemes))]
pub fn apply_ras(
    py: Python<'_>,
    network: &Network,
    ca_result: &ContingencyAnalysis,
    ras_schemes: Vec<RemedialAction>,
) -> PyResult<ContingencyAnalysis> {
    catch_panic("apply_ras", || {
        let config = to_core_ras(py, &ras_schemes, &network.inner)?;
        let ctg_opts = surge_contingency::ContingencyOptions::default();

        // Solve base case once for arming evaluation.
        let acpf_opts = surge_ac::AcPfOptions::default();
        let base_state = match surge_ac::solve_ac_pf_kernel(&network.inner, &acpf_opts) {
            Ok(sol) if sol.status == surge_solution::SolveStatus::Converged => {
                Some(surge_contingency::corrective::BaseCaseState::from_solution(
                    &network.inner,
                    &sol.voltage_magnitude_pu,
                    &sol.voltage_angle_rad,
                ))
            }
            _ => None,
        };

        let mut updated = ca_result.inner.clone();
        for result in &mut updated.results {
            if result.violations.is_empty() {
                continue;
            }
            if result.branch_indices.is_empty() && result.generator_indices.is_empty() {
                return Err(PyValueError::new_err(format!(
                    "ContingencyResult '{}' has violations but no branch_indices or \
                     generator_indices — cannot apply RAS. Ensure contingency results \
                     were produced by surge (not loaded from an older JSON snapshot).",
                    result.id
                )));
            }
            let ca_result_inner = surge_contingency::corrective::apply_corrective_actions(
                &network.inner,
                &result.branch_indices.clone(),
                result,
                base_state.as_ref(),
                &config,
                &ctg_opts,
            );
            if ca_result_inner.correctable {
                result.violations = ca_result_inner.violations_after_correction;
            }
            // Store scheme outcomes on the ContingencyResult for auditability.
            result.scheme_outcomes = ca_result_inner.scheme_outcomes;
        }
        updated.summary.with_violations = updated
            .results
            .iter()
            .filter(|r| !r.violations.is_empty())
            .count();
        Ok(ContingencyAnalysis { inner: updated })
    })
}

/// Compute AC contingency analysis for a user-defined contingency list.
///
/// Unlike ``analyze_n1_branch()`` which runs all N-1 branch contingencies, this
/// function accepts an explicit list of contingencies so you can analyze
/// specific elements, N-k outages, or mixed branch+generator events.
///
/// Args:
///   network: Power system network.
///   contingencies: List of ``Contingency`` objects defining the events to analyze.
///   options: ContingencyOptions (default: all defaults).
///   monitored_branches: Optional list of (from, to, circuit) tuples to restrict
///     which branches are monitored for thermal violations.
///
/// Returns:
///   ContingencyAnalysis with per-contingency results.
#[pyfunction]
#[pyo3(signature = (network, contingencies, options=None, monitored_branches=None))]
pub fn analyze_contingencies(
    py: Python<'_>,
    network: &Network,
    contingencies: Vec<Contingency>,
    options: Option<ContingencyOptions>,
    monitored_branches: Option<Vec<(u32, u32, String)>>,
) -> PyResult<ContingencyAnalysis> {
    let core_ctgs = to_core_contingencies(&contingencies, &network.inner)?;
    let opts_py = options.unwrap_or_default();
    let opts = opts_py.to_core(None).map_err(to_pyerr)?;
    // Build set of monitored branch indices (0-based internal).
    // Use a pre-built bidirectional map for O(n) total instead of O(n_monitored × n_branches).
    let monitored_set: Option<std::collections::HashSet<usize>> = monitored_branches.map(|mb| {
        let branch_map: std::collections::HashMap<(u32, u32, &str), usize> = network
            .inner
            .branches
            .iter()
            .enumerate()
            .filter(|(_, br)| br.in_service)
            .flat_map(|(i, br)| {
                let ckt = br.circuit.as_str();
                [
                    ((br.from_bus, br.to_bus, ckt), i),
                    ((br.to_bus, br.from_bus, ckt), i),
                ]
            })
            .collect();
        mb.iter()
            .filter_map(|(f, t, c)| branch_map.get(&(*f, *t, c.as_str())).copied())
            .collect()
    });
    let net = Arc::clone(&network.inner);
    let mut inner = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_contingency::analyze_contingencies(&net, &core_ctgs, &opts)
                    .map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("analyze_contingencies failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;

    // Post-filter: keep only violations on monitored branches
    if let Some(ref mon) = monitored_set {
        for result in &mut inner.results {
            result.violations.retain(|v| match v {
                surge_contingency::Violation::ThermalOverload { branch_idx, .. } => {
                    mon.contains(branch_idx)
                }
                // Voltage and convergence violations are not branch-specific — keep them
                _ => true,
            });
        }
    }

    Ok(ContingencyAnalysis { inner })
}

/// Solve corrective redispatch (SCRD) for all contingencies with thermal violations (CTG-02).
///
/// Given a network with a pre-computed N-1 contingency analysis (from `analyze_n1_branch` or
/// `analyze_n1_generator`), attempts corrective generation redispatch for every
/// contingency result that has thermal overload violations.
///
/// Args:
///     network: the power system network (provides base dispatch and limits).
///     contingency_analysis: result from analyze_n1_branch() or analyze_n1_generator().
///
/// Returns:
///     List of dicts, one per contingency result, with keys:
///       'id' (str), 'status' (str), 'total_redispatch_mw' (float),
///       'total_cost' (float), 'violations_resolved' (int),
///       'unresolvable_violations' (int).
///     Contingencies without thermal violations are omitted.
#[pyfunction]
pub fn solve_corrective_dispatch<'py>(
    py: Python<'py>,
    network: &Network,
    contingency_analysis: &ContingencyAnalysis,
) -> PyResult<Bound<'py, PyList>> {
    catch_panic("solve_corrective_dispatch", || {
        let mut prepared = prepare_corrective_dispatch_study(network)?;
        prepared.solve_corrective_dispatch(py, contingency_analysis)
    })
}

// ---------------------------------------------------------------------------
// rank_contingencies
// ---------------------------------------------------------------------------

/// Rank contingency results by a severity metric and return the top-k worst.
///
/// Args:
///     ca: ContingencyAnalysis result from analyze_n1_branch, etc.
///     metric: Ranking criterion — ``"max_flow_pct"``, ``"min_voltage_pu"``,
///         or ``"max_voltage_pu"``.
///     k: Number of worst contingencies to return.
///
/// Returns:
///     List of dicts with keys ``contingency_id``, ``label``, ``score``,
///     ``converged``, ``n_violations``.
#[pyfunction]
#[pyo3(signature = (ca, metric = "max_flow_pct", k = 10))]
pub fn rank_contingencies<'py>(
    py: Python<'py>,
    ca: &ContingencyAnalysis,
    metric: &str,
    k: usize,
) -> PyResult<Bound<'py, PyList>> {
    let metric_enum = match metric {
        "max_flow_pct" => surge_contingency::ContingencyMetric::MaxFlowPct,
        "min_voltage_pu" => surge_contingency::ContingencyMetric::MinVoltagePu,
        "max_voltage_pu" => surge_contingency::ContingencyMetric::MaxVoltagePu,
        other => {
            return Err(PyValueError::new_err(format!(
                "metric must be 'max_flow_pct', 'min_voltage_pu', or 'max_voltage_pu', got '{other}'"
            )));
        }
    };
    let ranked = surge_contingency::ranking::rank_contingencies(&ca.inner.results, metric_enum, k);
    let list = PyList::empty(py);
    for r in &ranked {
        let d = PyDict::new(py);
        d.set_item("contingency_id", &r.id)?;
        d.set_item("label", &r.label)?;
        d.set_item("converged", r.converged)?;
        d.set_item("n_violations", r.violations.len())?;
        list.append(d)?;
    }
    Ok(list)
}

// ---------------------------------------------------------------------------
// analyze_branch_eens
// ---------------------------------------------------------------------------

/// Run probabilistic N-1 analysis (EENS/LOLE from forced outage rates).
///
/// For each branch, computes Expected Energy Not Served (EENS) caused by
/// thermal overloads when that branch is outaged, weighted by its forced
/// outage rate (FOR).
///
/// Args:
///     network: Power system network.
///     branch_for: Optional list of per-branch forced outage rates (0.0–1.0).
///         Length must equal number of branches, or empty for uniform 1% default.
///     hours_per_year: Hours per year for EENS conversion (default: 8760.0).
///     overload_threshold_mw: Minimum overload in MW to count (default: 0.0).
///     thermal_rating: Rating tier — ``"rate_a"``, ``"rate_b"``, or ``"rate_c"``
///         (default: ``"rate_a"``).
///
/// Returns:
///     Dict with ``total_eens_mwh_per_year``, ``bridge_line_eens_mwh_per_year``,
///     ``weighted_overload_rate``, and ``results`` (list of per-branch dicts).
#[pyfunction]
#[pyo3(signature = (network, branch_for=None, hours_per_year=8760.0, overload_threshold_mw=0.0, thermal_rating=None))]
pub fn analyze_branch_eens<'py>(
    py: Python<'py>,
    network: &Network,
    branch_for: Option<Vec<f64>>,
    hours_per_year: f64,
    overload_threshold_mw: f64,
    thermal_rating: Option<&str>,
) -> PyResult<Bound<'py, PyDict>> {
    let rating = match thermal_rating {
        Some("rate_a") | None => surge_contingency::ThermalRating::RateA,
        Some("rate_b") => surge_contingency::ThermalRating::RateB,
        Some("rate_c") => surge_contingency::ThermalRating::RateC,
        Some(other) => {
            return Err(PyValueError::new_err(format!(
                "thermal_rating must be 'rate_a', 'rate_b', or 'rate_c', got '{other}'"
            )));
        }
    };
    let opts = surge_contingency::probabilistic::BranchEensOptions {
        branch_for: branch_for.unwrap_or_default(),
        hours_per_year,
        overload_threshold_mw,
        thermal_rating: rating,
    };
    let net = network.inner.clone();
    let summary = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_contingency::probabilistic::analyze_branch_eens(&net, &opts)
            }))
            .map_err(|e| format!("analyze_branch_eens panicked: {}", extract_panic_msg(e)))
        })
        .map_err(to_pyerr)?;

    let dict = PyDict::new(py);
    dict.set_item("total_eens_mwh_per_year", summary.total_eens_mwh_per_year)?;
    dict.set_item(
        "bridge_line_eens_mwh_per_year",
        summary.bridge_line_eens_mwh_per_year,
    )?;
    dict.set_item("weighted_overload_rate", summary.weighted_overload_rate)?;

    let results_list = PyList::empty(py);
    for r in &summary.results {
        let d = PyDict::new(py);
        d.set_item("branch_index", r.branch_index)?;
        d.set_item("branch_id", &r.branch_id)?;
        d.set_item("for_rate", r.for_rate)?;
        d.set_item("has_overload", r.has_overload)?;
        d.set_item("eens_mwh_per_year", r.eens_mwh_per_year)?;
        d.set_item("is_bridge", r.is_bridge)?;
        results_list.append(d)?;
    }
    dict.set_item("results", results_list)?;

    Ok(dict)
}
