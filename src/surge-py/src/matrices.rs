// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Sensitivity matrix types and compute functions:
//! PTDF, LODF, OTDF, BLDF, GSF, InjectionCapability, AFC, Y-bus, Jacobian.

use std::collections::HashMap;
use std::sync::Arc;

use faer::Mat;
use num_complex::Complex64;
use numpy::ndarray::{Array2, Array3};
use numpy::{IntoPyArray, PyArray1, PyArray2, PyArray3};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::exceptions::{NetworkError, extract_panic_msg, to_pyerr};
use crate::network::Network;
use crate::pf::build_dc_pf_options;
use crate::solutions::{DcPfResult, dc_pf_result_from_result};
use crate::utils::dict_to_dataframe_with_index;

// ---------------------------------------------------------------------------
// PTDF
// ---------------------------------------------------------------------------

/// PTDF result for a monitored branch set.
///
/// The PTDF matrix is stored row-major with shape
/// ``(n_monitored_branches, n_buses)``.
#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PtdfResult {
    pub ptdf_data: Vec<f64>,
    pub n_monitored: usize,
    pub n_buses: usize,
    pub bus_indices_vec: Vec<usize>,
    pub bus_numbers_vec: Vec<u32>,
    pub monitored_branch_indices: Vec<usize>,
    pub branch_from_vec: Vec<u32>,
    pub branch_to_vec: Vec<u32>,
    pub branch_circuit_vec: Vec<String>,
}

#[pymethods]
impl PtdfResult {
    /// PTDF matrix as numpy array of shape ``(n_monitored, n_buses)``.
    #[getter]
    fn ptdf<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<f64>>> {
        row_major_pyarray2(py, self.ptdf_data.clone(), self.n_monitored, self.n_buses)
    }

    /// PTDF row for one monitored branch as a 1-D numpy array of length ``n_buses``.
    fn get_row<'py>(
        &self,
        py: Python<'py>,
        branch_idx: usize,
    ) -> PyResult<Bound<'py, PyArray1<f64>>> {
        for (row_pos, &monitored_branch_idx) in self.monitored_branch_indices.iter().enumerate() {
            if monitored_branch_idx == branch_idx {
                return Ok(
                    self.ptdf_data[row_pos * self.n_buses..(row_pos + 1) * self.n_buses]
                        .to_vec()
                        .into_pyarray(py),
                );
            }
        }
        Err(NetworkError::new_err(format!(
            "branch index {branch_idx} not in monitored set"
        )))
    }

    /// External bus numbers (element order in each column).
    #[getter]
    fn bus_numbers(&self) -> Vec<u32> {
        self.bus_numbers_vec.clone()
    }

    /// Internal bus indices (column order).
    #[getter]
    fn bus_indices(&self) -> Vec<usize> {
        self.bus_indices_vec.clone()
    }

    /// Internal monitored branch indices (row order).
    #[getter]
    fn monitored_branches(&self) -> Vec<usize> {
        self.monitored_branch_indices.clone()
    }

    /// External from-bus numbers for monitored branches (row order).
    #[getter]
    fn branch_from(&self) -> Vec<u32> {
        self.branch_from_vec.clone()
    }

    /// External to-bus numbers for monitored branches (row order).
    #[getter]
    fn branch_to(&self) -> Vec<u32> {
        self.branch_to_vec.clone()
    }

    /// Circuit identifiers for monitored branches (row order).
    #[getter]
    fn branch_circuit(&self) -> Vec<String> {
        self.branch_circuit_vec.clone()
    }

    /// Stable branch keys for monitored branches (row order).
    #[getter]
    fn branch_keys(&self) -> Vec<(u32, u32, String)> {
        self.branch_from_vec
            .iter()
            .zip(self.branch_to_vec.iter())
            .zip(self.branch_circuit_vec.iter())
            .map(|((&from_bus, &to_bus), circuit)| (from_bus, to_bus, circuit.clone()))
            .collect()
    }

    /// Return a pandas DataFrame with (from_bus, to_bus) row index and
    /// bus numbers as columns.
    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let np = py.import("numpy")?;
        let arr = row_major_pyarray2(py, self.ptdf_data.clone(), self.n_monitored, self.n_buses)?;
        let pd = py.import("pandas")?;
        let cols: Vec<String> = self.bus_numbers_vec.iter().map(|n| n.to_string()).collect();
        let idx_tuples: Vec<(u32, u32, String)> = self
            .branch_from_vec
            .iter()
            .zip(self.branch_to_vec.iter())
            .zip(self.branch_circuit_vec.iter())
            .map(|((&f, &t), circuit)| (f, t, circuit.clone()))
            .collect();
        let mi = pd
            .getattr("MultiIndex")?
            .call_method1("from_tuples", (idx_tuples,))?;
        mi.call_method1("set_names", (vec!["from_bus", "to_bus", "circuit"],))?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("data", np.call_method1("asarray", (arr,))?)?;
        kwargs.set_item("index", mi)?;
        kwargs.set_item("columns", cols)?;
        pd.call_method("DataFrame", (), Some(&kwargs))
    }

    fn __repr__(&self) -> String {
        format!(
            "PtdfResult(n_buses={}, monitored={})",
            self.n_buses, self.n_monitored
        )
    }
}

fn build_ptdf_result_from_rows(network: &Network, columns: surge_dc::PtdfRows) -> PtdfResult {
    let (monitored_branch_indices, bus_indices_vec, ptdf_data) = columns.into_parts();
    let bus_numbers_vec: Vec<u32> = bus_indices_vec
        .iter()
        .map(|&bus_idx| network.inner.buses[bus_idx].number)
        .collect();
    let branch_from_vec: Vec<u32> = monitored_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].from_bus)
        .collect();
    let branch_to_vec: Vec<u32> = monitored_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].to_bus)
        .collect();
    let branch_circuit_vec: Vec<String> = monitored_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].circuit.clone())
        .collect();
    PtdfResult {
        ptdf_data,
        n_monitored: monitored_branch_indices.len(),
        n_buses: bus_indices_vec.len(),
        bus_indices_vec,
        bus_numbers_vec,
        monitored_branch_indices,
        branch_from_vec,
        branch_to_vec,
        branch_circuit_vec,
    }
}

fn row_major_pyarray2<'py>(
    py: Python<'py>,
    data: Vec<f64>,
    nrows: usize,
    ncols: usize,
) -> PyResult<Bound<'py, PyArray2<f64>>> {
    let array = Array2::from_shape_vec((nrows, ncols), data).map_err(to_pyerr)?;
    Ok(array.into_pyarray(py))
}

fn build_dc_sensitivity_options(
    net: &Arc<surge_network::Network>,
    slack_weights: Option<&HashMap<u32, f64>>,
    headroom_slack: bool,
    headroom_slack_buses: Option<&[u32]>,
) -> PyResult<surge_dc::DcSensitivityOptions> {
    if slack_weights.is_some() && (headroom_slack || headroom_slack_buses.is_some()) {
        return Err(PyValueError::new_err(
            "slack_weights cannot be combined with headroom_slack or headroom_slack_buses",
        ));
    }

    if let Some(slack_weights) = slack_weights {
        let bus_map = net.bus_index_map();
        let mut internal_weights = Vec::with_capacity(slack_weights.len());
        for (&bus_num, &weight) in slack_weights {
            let Some(&bus_idx) = bus_map.get(&bus_num) else {
                return Err(NetworkError::new_err(format!(
                    "slack_weights: bus {bus_num} not found in network"
                )));
            };
            internal_weights.push((bus_idx, weight));
        }
        return Ok(surge_dc::DcSensitivityOptions::with_slack_weights(
            &internal_weights,
        ));
    }

    let pf_options = build_dc_pf_options(
        net,
        headroom_slack,
        headroom_slack_buses,
        None,
        "preserve_initial",
    )?;
    Ok(pf_options
        .headroom_slack_bus_indices
        .as_deref()
        .map(surge_dc::DcSensitivityOptions::with_headroom_slack)
        .unwrap_or_default())
}

fn borrowed_dc_network(
    network_arc: &Arc<surge_network::Network>,
) -> &'static surge_network::Network {
    unsafe {
        // SAFETY: the Arc is stored alongside the prepared DC model, keeping the
        // allocation alive for the model's full lifetime.
        &*Arc::as_ptr(network_arc)
    }
}

#[pyclass(unsendable, name = "PreparedDcStudy")]
pub struct PreparedDcStudy {
    prepared: Option<surge_dc::PreparedDcStudy<'static>>,
    network: Arc<surge_network::Network>,
}

impl PreparedDcStudy {
    fn prepared_mut(&mut self) -> PyResult<&mut surge_dc::PreparedDcStudy<'static>> {
        self.prepared
            .as_mut()
            .ok_or_else(|| PyValueError::new_err("prepared DC study is no longer available"))
    }
}

impl Drop for PreparedDcStudy {
    fn drop(&mut self) {
        let _ = self.prepared.take();
    }
}

#[pyfunction]
pub fn prepare_dc_study(network: &Network) -> PyResult<PreparedDcStudy> {
    network.validate()?;
    let network_arc = Arc::clone(&network.inner);
    let prepared =
        surge_dc::PreparedDcStudy::new(borrowed_dc_network(&network_arc)).map_err(to_pyerr)?;
    Ok(PreparedDcStudy {
        prepared: Some(prepared),
        network: network_arc,
    })
}

/// Compute PTDF for a subset of monitored branches (memory-efficient).
///
/// Uses one KLU sparse solve per monitored branch — does NOT materialize B'^-1.
/// Suitable for any network size.
///
/// Args:
///     network: Power system network.
///     monitored_branches: Optional list of internal branch indices to compute
///         PTDF for. When omitted, computes PTDF for all branches.
///
/// Returns:
///     PtdfResult with ``.ptdf`` matrix and monitored-branch metadata.
#[pyfunction]
#[pyo3(signature = (
    network,
    monitored_branches = None,
    bus_indices = None,
    slack_weights = None,
    headroom_slack = false,
    headroom_slack_buses = None,
))]
pub fn compute_ptdf(
    py: Python<'_>,
    network: &Network,
    monitored_branches: Option<Vec<usize>>,
    bus_indices: Option<Vec<usize>>,
    slack_weights: Option<HashMap<u32, f64>>,
    headroom_slack: bool,
    headroom_slack_buses: Option<Vec<u32>>,
) -> PyResult<PtdfResult> {
    network.validate()?;
    let net = Arc::clone(&network.inner);
    let monitored_branches =
        monitored_branches.unwrap_or_else(|| (0..network.inner.n_branches()).collect());
    let options = build_dc_sensitivity_options(
        &net,
        slack_weights.as_ref(),
        headroom_slack,
        headroom_slack_buses.as_deref(),
    )?;
    let mon = monitored_branches.clone();
    let columns = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut request = surge_dc::PtdfRequest::for_branches(&mon).with_options(options);
                if let Some(bus_indices) = bus_indices.as_deref() {
                    request = request.with_bus_indices(bus_indices);
                }
                surge_dc::compute_ptdf(&net, &request).map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("compute_ptdf failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(build_ptdf_result_from_rows(network, columns))
}

// ---------------------------------------------------------------------------
// LODF
// ---------------------------------------------------------------------------

/// Rectangular LODF result with monitored/outage metadata.
///
/// Returned by ``compute_lodf()``. Contains a monitored-by-outage LODF matrix.
#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct LodfResult {
    pub lodf_data: Vec<f64>,
    pub n_monitored: usize,
    pub n_outages: usize,
    pub monitored_branch_indices: Vec<usize>,
    pub outage_branch_indices: Vec<usize>,
    pub monitored_from_vec: Vec<u32>,
    pub monitored_to_vec: Vec<u32>,
    pub monitored_circuit_vec: Vec<String>,
    pub outage_from_vec: Vec<u32>,
    pub outage_to_vec: Vec<u32>,
    pub outage_circuit_vec: Vec<String>,
}

#[pymethods]
impl LodfResult {
    /// LODF matrix as numpy array of shape (n_monitored, n_outages).
    #[getter]
    fn lodf<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<f64>>> {
        row_major_pyarray2(py, self.lodf_data.clone(), self.n_monitored, self.n_outages)
    }

    /// Internal monitored branch indices (row order).
    #[getter]
    fn monitored_branches(&self) -> Vec<usize> {
        self.monitored_branch_indices.clone()
    }

    /// Internal outage branch indices (column order).
    #[getter]
    fn outage_branches(&self) -> Vec<usize> {
        self.outage_branch_indices.clone()
    }

    /// External from-bus numbers for monitored branches (row order).
    #[getter]
    fn monitored_from(&self) -> Vec<u32> {
        self.monitored_from_vec.clone()
    }

    /// External to-bus numbers for monitored branches (row order).
    #[getter]
    fn monitored_to(&self) -> Vec<u32> {
        self.monitored_to_vec.clone()
    }

    /// Circuit identifiers for monitored branches (row order).
    #[getter]
    fn monitored_circuit(&self) -> Vec<String> {
        self.monitored_circuit_vec.clone()
    }

    /// Stable monitored branch keys (row order).
    #[getter]
    fn monitored_keys(&self) -> Vec<(u32, u32, String)> {
        self.monitored_from_vec
            .iter()
            .zip(self.monitored_to_vec.iter())
            .zip(self.monitored_circuit_vec.iter())
            .map(|((&from_bus, &to_bus), circuit)| (from_bus, to_bus, circuit.clone()))
            .collect()
    }

    /// External from-bus numbers for outage branches (column order).
    #[getter]
    fn outage_from(&self) -> Vec<u32> {
        self.outage_from_vec.clone()
    }

    /// External to-bus numbers for outage branches (column order).
    #[getter]
    fn outage_to(&self) -> Vec<u32> {
        self.outage_to_vec.clone()
    }

    /// Circuit identifiers for outage branches (column order).
    #[getter]
    fn outage_circuit(&self) -> Vec<String> {
        self.outage_circuit_vec.clone()
    }

    /// Stable outage branch keys (column order).
    #[getter]
    fn outage_keys(&self) -> Vec<(u32, u32, String)> {
        self.outage_from_vec
            .iter()
            .zip(self.outage_to_vec.iter())
            .zip(self.outage_circuit_vec.iter())
            .map(|((&from_bus, &to_bus), circuit)| (from_bus, to_bus, circuit.clone()))
            .collect()
    }

    /// Return a pandas DataFrame with branch-key row and column indices.
    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let np = py.import("numpy")?;
        let arr = row_major_pyarray2(py, self.lodf_data.clone(), self.n_monitored, self.n_outages)?;
        let pd = py.import("pandas")?;
        let row_tuples: Vec<(u32, u32, String)> = self
            .monitored_from_vec
            .iter()
            .zip(self.monitored_to_vec.iter())
            .zip(self.monitored_circuit_vec.iter())
            .map(|((&f, &t), circuit)| (f, t, circuit.clone()))
            .collect();
        let col_tuples: Vec<(u32, u32, String)> = self
            .outage_from_vec
            .iter()
            .zip(self.outage_to_vec.iter())
            .zip(self.outage_circuit_vec.iter())
            .map(|((&f, &t), circuit)| (f, t, circuit.clone()))
            .collect();
        let row_mi = pd
            .getattr("MultiIndex")?
            .call_method1("from_tuples", (row_tuples,))?;
        row_mi.call_method1("set_names", (vec!["from_bus", "to_bus", "circuit"],))?;
        let col_mi = pd
            .getattr("MultiIndex")?
            .call_method1("from_tuples", (col_tuples,))?;
        col_mi.call_method1(
            "set_names",
            (vec!["outage_from", "outage_to", "outage_circuit"],),
        )?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("data", np.call_method1("asarray", (arr,))?)?;
        kwargs.set_item("index", row_mi)?;
        kwargs.set_item("columns", col_mi)?;
        pd.call_method("DataFrame", (), Some(&kwargs))
    }

    fn __repr__(&self) -> String {
        format!(
            "LodfResult(monitored={}, outage={})",
            self.n_monitored, self.n_outages
        )
    }
}

fn mat_to_row_major(lodf: &Mat<f64>) -> Vec<f64> {
    let mut data = vec![0.0; lodf.nrows() * lodf.ncols()];
    for i in 0..lodf.nrows() {
        for j in 0..lodf.ncols() {
            data[i * lodf.ncols() + j] = lodf[(i, j)];
        }
    }
    data
}

fn build_lodf_result(network: &Network, lodf: surge_dc::LodfResult) -> LodfResult {
    let (monitored_branches, outage_branches, lodf) = lodf.into_parts();
    let monitored_from: Vec<u32> = monitored_branches
        .iter()
        .map(|&idx| network.inner.branches[idx].from_bus)
        .collect();
    let monitored_to: Vec<u32> = monitored_branches
        .iter()
        .map(|&idx| network.inner.branches[idx].to_bus)
        .collect();
    let monitored_circuit: Vec<String> = monitored_branches
        .iter()
        .map(|&idx| network.inner.branches[idx].circuit.clone())
        .collect();
    let outage_from: Vec<u32> = outage_branches
        .iter()
        .map(|&idx| network.inner.branches[idx].from_bus)
        .collect();
    let outage_to: Vec<u32> = outage_branches
        .iter()
        .map(|&idx| network.inner.branches[idx].to_bus)
        .collect();
    let outage_circuit: Vec<String> = outage_branches
        .iter()
        .map(|&idx| network.inner.branches[idx].circuit.clone())
        .collect();

    LodfResult {
        lodf_data: mat_to_row_major(&lodf),
        n_monitored: monitored_branches.len(),
        n_outages: outage_branches.len(),
        monitored_branch_indices: monitored_branches,
        outage_branch_indices: outage_branches,
        monitored_from_vec: monitored_from,
        monitored_to_vec: monitored_to,
        monitored_circuit_vec: monitored_circuit,
        outage_from_vec: outage_from,
        outage_to_vec: outage_to,
        outage_circuit_vec: outage_circuit,
    }
}

/// Dense LODF matrix result with branch metadata.
///
/// Returned by ``compute_lodf_matrix()``. Contains the dense
/// `n_branches × n_branches` LODF matrix.
#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct LodfMatrixResult {
    pub lodf_data: Vec<f64>,
    pub n_branches: usize,
    pub branch_from_vec: Vec<u32>,
    pub branch_to_vec: Vec<u32>,
    pub branch_circuit_vec: Vec<String>,
}

#[pymethods]
impl LodfMatrixResult {
    /// LODF matrix as numpy array of shape (n_branches, n_branches).
    #[getter]
    fn lodf<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<f64>>> {
        row_major_pyarray2(py, self.lodf_data.clone(), self.n_branches, self.n_branches)
    }

    /// External from-bus numbers (row/column order).
    #[getter]
    fn branch_from(&self) -> Vec<u32> {
        self.branch_from_vec.clone()
    }

    /// External to-bus numbers (row/column order).
    #[getter]
    fn branch_to(&self) -> Vec<u32> {
        self.branch_to_vec.clone()
    }

    /// Circuit identifiers (row/column order).
    #[getter]
    fn branch_circuit(&self) -> Vec<String> {
        self.branch_circuit_vec.clone()
    }

    /// Stable branch keys (row/column order).
    #[getter]
    fn branch_keys(&self) -> Vec<(u32, u32, String)> {
        self.branch_from_vec
            .iter()
            .zip(self.branch_to_vec.iter())
            .zip(self.branch_circuit_vec.iter())
            .map(|((&from_bus, &to_bus), circuit)| (from_bus, to_bus, circuit.clone()))
            .collect()
    }

    /// Return a pandas DataFrame with branch keys as both row and
    /// column index.
    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let np = py.import("numpy")?;
        let arr = row_major_pyarray2(py, self.lodf_data.clone(), self.n_branches, self.n_branches)?;
        let pd = py.import("pandas")?;
        let tuples: Vec<(u32, u32, String)> = self
            .branch_from_vec
            .iter()
            .zip(self.branch_to_vec.iter())
            .zip(self.branch_circuit_vec.iter())
            .map(|((&f, &t), circuit)| (f, t, circuit.clone()))
            .collect();
        let mi = pd
            .getattr("MultiIndex")?
            .call_method1("from_tuples", (tuples,))?;
        mi.call_method1("set_names", (vec!["from_bus", "to_bus", "circuit"],))?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("data", np.call_method1("asarray", (arr,))?)?;
        kwargs.set_item("index", mi.clone())?;
        kwargs.set_item("columns", mi)?;
        pd.call_method("DataFrame", (), Some(&kwargs))
    }

    fn __repr__(&self) -> String {
        format!("LodfMatrixResult(branches={})", self.n_branches)
    }
}

fn build_lodf_matrix_result(
    network: &Network,
    lodf: surge_dc::LodfMatrixResult,
) -> LodfMatrixResult {
    let (branches, lodf) = lodf.into_parts();
    let branch_from: Vec<u32> = branches
        .iter()
        .map(|&idx| network.inner.branches[idx].from_bus)
        .collect();
    let branch_to: Vec<u32> = branches
        .iter()
        .map(|&idx| network.inner.branches[idx].to_bus)
        .collect();
    let branch_circuit: Vec<String> = branches
        .iter()
        .map(|&idx| network.inner.branches[idx].circuit.clone())
        .collect();

    LodfMatrixResult {
        lodf_data: mat_to_row_major(&lodf),
        n_branches: branches.len(),
        branch_from_vec: branch_from,
        branch_to_vec: branch_to,
        branch_circuit_vec: branch_circuit,
    }
}

/// Compute LODF for explicit monitored and outage branch sets.
///
/// Uses sparse KLU solves and returns a rectangular matrix ordered exactly like
/// the requested branch lists.
///
/// Args:
///     network: Power system network.
///     monitored_branches: Optional list of internal branch indices to monitor.
///         When omitted, monitors all branches.
///     outage_branches: Optional list of internal branch indices to outage.
///         When omitted, uses the monitored branch set.
///
/// Returns:
///     LodfResult with .lodf as a numpy array of shape (n_monitored, n_outages).
#[pyfunction]
#[pyo3(signature = (
    network,
    monitored_branches = None,
    outage_branches = None,
))]
pub fn compute_lodf(
    py: Python<'_>,
    network: &Network,
    monitored_branches: Option<Vec<usize>>,
    outage_branches: Option<Vec<usize>>,
) -> PyResult<LodfResult> {
    network.validate()?;
    let net = Arc::clone(&network.inner);
    let monitored_branches =
        monitored_branches.unwrap_or_else(|| (0..network.inner.n_branches()).collect());
    let outage_branches = outage_branches.unwrap_or_else(|| monitored_branches.clone());
    let monitored = monitored_branches.clone();
    let outage = outage_branches.clone();
    let lodf = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let request = surge_dc::LodfRequest::for_branches(&monitored, &outage);
                surge_dc::compute_lodf(&net, &request).map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("compute_lodf failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(build_lodf_result(network, lodf))
}

/// Compute dense all-pairs LODF matrix for the given branch set.
#[pyfunction]
#[pyo3(signature = (
    network,
    branches = None,
))]
pub fn compute_lodf_matrix(
    py: Python<'_>,
    network: &Network,
    branches: Option<Vec<usize>>,
) -> PyResult<LodfMatrixResult> {
    network.validate()?;
    let net = Arc::clone(&network.inner);
    let branches = branches.unwrap_or_else(|| (0..network.inner.n_branches()).collect());
    let br = branches.clone();
    let lodf = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let request = surge_dc::LodfMatrixRequest::for_branches(&br);
                surge_dc::compute_lodf_matrix(&net, &request).map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("compute_lodf_matrix failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(build_lodf_matrix_result(network, lodf))
}

// ---------------------------------------------------------------------------
// N-2 LODF
// ---------------------------------------------------------------------------

/// Batched N-2 LODF result with monitored/outage-pair metadata.
#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct N2LodfResult {
    pub lodf_data: Vec<f64>,
    pub monitored_branch_indices: Vec<usize>,
    pub monitored_from_vec: Vec<u32>,
    pub monitored_to_vec: Vec<u32>,
    pub monitored_circuit_vec: Vec<String>,
    pub outage_pair: (usize, usize),
    pub outage_pair_key: ((u32, u32, String), (u32, u32, String)),
}

#[pymethods]
impl N2LodfResult {
    /// N-2 LODF vector as numpy array of shape (n_monitored,).
    #[getter]
    fn lodf<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.lodf_data.clone().into_pyarray(py)
    }

    /// Internal monitored branch indices (value order).
    #[getter]
    fn monitored_branches(&self) -> Vec<usize> {
        self.monitored_branch_indices.clone()
    }

    /// Stable monitored branch keys (value order).
    #[getter]
    fn monitored_keys(&self) -> Vec<(u32, u32, String)> {
        self.monitored_from_vec
            .iter()
            .zip(self.monitored_to_vec.iter())
            .zip(self.monitored_circuit_vec.iter())
            .map(|((&from_bus, &to_bus), circuit)| (from_bus, to_bus, circuit.clone()))
            .collect()
    }

    /// Internal outage branch indices for the ordered outage pair.
    #[getter]
    fn outage_pair(&self) -> (usize, usize) {
        self.outage_pair
    }

    /// Stable branch keys for the ordered outage pair.
    #[getter]
    fn outage_pair_key(&self) -> ((u32, u32, String), (u32, u32, String)) {
        self.outage_pair_key.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "N2LodfResult(monitored={}, outage_pair=({}, {}))",
            self.lodf_data.len(),
            self.outage_pair.0,
            self.outage_pair.1
        )
    }
}

fn build_n2_lodf_result(network: &Network, result: surge_dc::N2LodfResult) -> N2LodfResult {
    let (monitored_branch_indices, outage_pair, lodf_data) = result.into_parts();
    let monitored_from_vec: Vec<u32> = monitored_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].from_bus)
        .collect();
    let monitored_to_vec: Vec<u32> = monitored_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].to_bus)
        .collect();
    let monitored_circuit_vec: Vec<String> = monitored_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].circuit.clone())
        .collect();
    let outage_pair_key = (
        (
            network.inner.branches[outage_pair.0].from_bus,
            network.inner.branches[outage_pair.0].to_bus,
            network.inner.branches[outage_pair.0].circuit.clone(),
        ),
        (
            network.inner.branches[outage_pair.1].from_bus,
            network.inner.branches[outage_pair.1].to_bus,
            network.inner.branches[outage_pair.1].circuit.clone(),
        ),
    );
    N2LodfResult {
        lodf_data,
        monitored_branch_indices,
        monitored_from_vec,
        monitored_to_vec,
        monitored_circuit_vec,
        outage_pair,
        outage_pair_key,
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct N2LodfBatchResult {
    pub lodf_data: Vec<f64>,
    pub n_monitored: usize,
    pub n_pairs: usize,
    pub monitored_branch_indices: Vec<usize>,
    pub outage_pairs: Vec<(usize, usize)>,
    pub monitored_from_vec: Vec<u32>,
    pub monitored_to_vec: Vec<u32>,
    pub monitored_circuit_vec: Vec<String>,
    pub outage_pair_keys: Vec<((u32, u32, String), (u32, u32, String))>,
}

#[pymethods]
impl N2LodfBatchResult {
    /// N-2 LODF matrix as numpy array of shape (n_monitored, n_pairs).
    #[getter]
    fn lodf<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<f64>>> {
        row_major_pyarray2(py, self.lodf_data.clone(), self.n_monitored, self.n_pairs)
    }

    /// Internal monitored branch indices (row order).
    #[getter]
    fn monitored_branches(&self) -> Vec<usize> {
        self.monitored_branch_indices.clone()
    }

    /// Stable monitored branch keys (row order).
    #[getter]
    fn monitored_keys(&self) -> Vec<(u32, u32, String)> {
        self.monitored_from_vec
            .iter()
            .zip(self.monitored_to_vec.iter())
            .zip(self.monitored_circuit_vec.iter())
            .map(|((&from_bus, &to_bus), circuit)| (from_bus, to_bus, circuit.clone()))
            .collect()
    }

    /// Ordered outage branch pairs (column order).
    #[getter]
    fn outage_pairs(&self) -> Vec<(usize, usize)> {
        self.outage_pairs.clone()
    }

    /// Stable branch keys for ordered outage pairs (column order).
    #[getter]
    fn outage_pair_keys(&self) -> Vec<((u32, u32, String), (u32, u32, String))> {
        self.outage_pair_keys.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "N2LodfBatchResult(monitored={}, pairs={})",
            self.n_monitored, self.n_pairs
        )
    }
}

fn build_n2_lodf_batch_result(
    network: &Network,
    batch: surge_dc::N2LodfBatchResult,
) -> N2LodfBatchResult {
    let (monitored_branches, outage_pairs, batch) = batch.into_parts();
    let monitored_from_vec: Vec<u32> = monitored_branches
        .iter()
        .map(|&idx| network.inner.branches[idx].from_bus)
        .collect();
    let monitored_to_vec: Vec<u32> = monitored_branches
        .iter()
        .map(|&idx| network.inner.branches[idx].to_bus)
        .collect();
    let monitored_circuit_vec: Vec<String> = monitored_branches
        .iter()
        .map(|&idx| network.inner.branches[idx].circuit.clone())
        .collect();
    let outage_pair_keys = outage_pairs
        .iter()
        .map(|&(first, second)| {
            (
                (
                    network.inner.branches[first].from_bus,
                    network.inner.branches[first].to_bus,
                    network.inner.branches[first].circuit.clone(),
                ),
                (
                    network.inner.branches[second].from_bus,
                    network.inner.branches[second].to_bus,
                    network.inner.branches[second].circuit.clone(),
                ),
            )
        })
        .collect();
    N2LodfBatchResult {
        lodf_data: mat_to_row_major(&batch),
        n_monitored: monitored_branches.len(),
        n_pairs: outage_pairs.len(),
        monitored_branch_indices: monitored_branches,
        outage_pairs,
        monitored_from_vec,
        monitored_to_vec,
        monitored_circuit_vec,
        outage_pair_keys,
    }
}

/// Compute N-2 LODF factors for a simultaneous double outage.
///
/// Args:
///     network: Power system network.
///     outage_pair: Two internal branch indices forming the simultaneous outage.
///     monitored_branches: Optional list of internal branch indices to monitor.
///         When omitted, computes factors for all branches.
///
/// Returns:
///     N2LodfResult with one N-2 LODF factor per monitored branch.
#[pyfunction]
#[pyo3(signature = (
    network,
    outage_pair,
    monitored_branches = None,
))]
pub fn compute_n2_lodf(
    py: Python<'_>,
    network: &Network,
    outage_pair: (usize, usize),
    monitored_branches: Option<Vec<usize>>,
) -> PyResult<N2LodfResult> {
    network.validate()?;
    let net = Arc::clone(&network.inner);
    let monitored_branches =
        monitored_branches.unwrap_or_else(|| (0..network.inner.n_branches()).collect());
    let monitored = monitored_branches.clone();
    let result = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let request =
                    surge_dc::N2LodfRequest::new(outage_pair).with_monitored_branches(&monitored);
                surge_dc::compute_n2_lodf(&net, &request).map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("compute_n2_lodf failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(build_n2_lodf_result(network, result))
}

/// Compute N-2 LODF factors for a batch of simultaneous double outages.
///
/// Args:
///     network: Power system network.
///     outage_pairs: Ordered list of internal outage-branch pairs.
///     monitored_branches: Optional list of internal branch indices to monitor.
///         When omitted, computes factors for all branches.
///
/// Returns:
///     N2LodfBatchResult with a monitored-by-pair matrix and outage-pair metadata.
#[pyfunction]
#[pyo3(signature = (
    network,
    outage_pairs,
    monitored_branches = None,
))]
pub fn compute_n2_lodf_batch(
    py: Python<'_>,
    network: &Network,
    outage_pairs: Vec<(usize, usize)>,
    monitored_branches: Option<Vec<usize>>,
) -> PyResult<N2LodfBatchResult> {
    network.validate()?;
    let net = Arc::clone(&network.inner);
    let monitored_branches =
        monitored_branches.unwrap_or_else(|| (0..network.inner.n_branches()).collect());
    let monitored = monitored_branches.clone();
    let pairs = outage_pairs.clone();
    let batch = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let request =
                    surge_dc::N2LodfBatchRequest::new(&pairs).with_monitored_branches(&monitored);
                surge_dc::compute_n2_lodf_batch(&net, &request).map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("compute_n2_lodf_batch failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(build_n2_lodf_batch_result(network, batch))
}

fn network_view(network: &Arc<surge_network::Network>) -> Network {
    Network {
        inner: Arc::clone(network),
        oltc_controls: Vec::new(),
        switched_shunts: Vec::new(),
    }
}

#[pymethods]
impl PreparedDcStudy {
    #[pyo3(name = "solve_pf", signature = (headroom_slack = false, headroom_slack_buses = None, participation_factors = None, angle_reference = "preserve_initial"))]
    fn solve_pf(
        &mut self,
        headroom_slack: bool,
        headroom_slack_buses: Option<Vec<u32>>,
        participation_factors: Option<HashMap<u32, f64>>,
        angle_reference: &str,
    ) -> PyResult<DcPfResult> {
        let options = build_dc_pf_options(
            &self.network,
            headroom_slack,
            headroom_slack_buses.as_deref(),
            participation_factors.as_ref(),
            angle_reference,
        )?;
        let result = self.prepared_mut()?.solve(&options).map_err(to_pyerr)?;
        Ok(dc_pf_result_from_result(Arc::clone(&self.network), result))
    }

    #[pyo3(signature = (
        monitored_branches = None,
        bus_indices = None,
        slack_weights = None,
        headroom_slack = false,
        headroom_slack_buses = None,
    ))]
    fn compute_ptdf(
        &mut self,
        monitored_branches: Option<Vec<usize>>,
        bus_indices: Option<Vec<usize>>,
        slack_weights: Option<HashMap<u32, f64>>,
        headroom_slack: bool,
        headroom_slack_buses: Option<Vec<u32>>,
    ) -> PyResult<PtdfResult> {
        let network = network_view(&self.network);
        let monitored =
            monitored_branches.unwrap_or_else(|| (0..self.network.n_branches()).collect());
        let options = build_dc_sensitivity_options(
            &self.network,
            slack_weights.as_ref(),
            headroom_slack,
            headroom_slack_buses.as_deref(),
        )?;
        let mut request = surge_dc::PtdfRequest::for_branches(&monitored).with_options(options);
        if let Some(bus_indices) = bus_indices.as_deref() {
            request = request.with_bus_indices(bus_indices);
        }
        let rows = self
            .prepared_mut()?
            .compute_ptdf_request(&request)
            .map_err(to_pyerr)?;
        Ok(build_ptdf_result_from_rows(&network, rows))
    }

    #[pyo3(signature = (
        monitored_branches = None,
        outage_branches = None,
    ))]
    fn compute_lodf(
        &mut self,
        monitored_branches: Option<Vec<usize>>,
        outage_branches: Option<Vec<usize>>,
    ) -> PyResult<LodfResult> {
        let network = network_view(&self.network);
        let monitored =
            monitored_branches.unwrap_or_else(|| (0..self.network.n_branches()).collect());
        let outage = outage_branches.unwrap_or_else(|| monitored.clone());
        let request = surge_dc::LodfRequest::for_branches(&monitored, &outage);
        let lodf = self
            .prepared_mut()?
            .compute_lodf_request(&request)
            .map_err(to_pyerr)?;
        Ok(build_lodf_result(&network, lodf))
    }

    #[pyo3(signature = (branches = None))]
    fn compute_lodf_matrix(&mut self, branches: Option<Vec<usize>>) -> PyResult<LodfMatrixResult> {
        let network = network_view(&self.network);
        let branches = branches.unwrap_or_else(|| (0..self.network.n_branches()).collect());
        let request = surge_dc::LodfMatrixRequest::for_branches(&branches);
        let lodf = self
            .prepared_mut()?
            .compute_lodf_matrix_request(&request)
            .map_err(to_pyerr)?;
        Ok(build_lodf_matrix_result(&network, lodf))
    }

    #[pyo3(signature = (
        monitored_branches,
        outage_branches,
        bus_indices = None,
        slack_weights = None,
        headroom_slack = false,
        headroom_slack_buses = None,
    ))]
    fn compute_otdf(
        &mut self,
        monitored_branches: Vec<usize>,
        outage_branches: Vec<usize>,
        bus_indices: Option<Vec<usize>>,
        slack_weights: Option<HashMap<u32, f64>>,
        headroom_slack: bool,
        headroom_slack_buses: Option<Vec<u32>>,
    ) -> PyResult<OtdfResult> {
        let network = network_view(&self.network);
        let options = build_dc_sensitivity_options(
            &self.network,
            slack_weights.as_ref(),
            headroom_slack,
            headroom_slack_buses.as_deref(),
        )?;
        let mut request =
            surge_dc::OtdfRequest::new(&monitored_branches, &outage_branches).with_options(options);
        if let Some(bus_indices) = bus_indices.as_deref() {
            request = request.with_bus_indices(bus_indices);
        }
        let otdf = self
            .prepared_mut()?
            .compute_otdf_request(&request)
            .map_err(to_pyerr)?;
        Ok(build_otdf_result(&network, otdf))
    }

    #[pyo3(signature = (outage_pair, monitored_branches = None))]
    fn compute_n2_lodf(
        &mut self,
        outage_pair: (usize, usize),
        monitored_branches: Option<Vec<usize>>,
    ) -> PyResult<N2LodfResult> {
        let monitored =
            monitored_branches.unwrap_or_else(|| (0..self.network.n_branches()).collect());
        let network = network_view(&self.network);
        let request = surge_dc::N2LodfRequest::new(outage_pair).with_monitored_branches(&monitored);
        let result = self
            .prepared_mut()?
            .compute_n2_lodf_request(&request)
            .map_err(to_pyerr)?;
        Ok(build_n2_lodf_result(&network, result))
    }

    #[pyo3(signature = (outage_pairs, monitored_branches = None))]
    fn compute_n2_lodf_batch(
        &mut self,
        outage_pairs: Vec<(usize, usize)>,
        monitored_branches: Option<Vec<usize>>,
    ) -> PyResult<N2LodfBatchResult> {
        let monitored =
            monitored_branches.unwrap_or_else(|| (0..self.network.n_branches()).collect());
        let request =
            surge_dc::N2LodfBatchRequest::new(&outage_pairs).with_monitored_branches(&monitored);
        let batch = self
            .prepared_mut()?
            .compute_n2_lodf_batch_request(&request)
            .map_err(to_pyerr)?;
        Ok(build_n2_lodf_batch_result(
            &network_view(&self.network),
            batch,
        ))
    }

    fn __repr__(&self) -> String {
        format!(
            "PreparedDcStudy(buses={}, branches={})",
            self.network.n_buses(),
            self.network.n_branches()
        )
    }
}

// ---------------------------------------------------------------------------
// OTDF
// ---------------------------------------------------------------------------

/// OTDF result for a set of (monitored, outage) branch pairs.
///
/// Returned by ``compute_otdf(network, monitored_branches, outage_branches)``.
/// The canonical OTDF tensor is stored with shape
/// ``(n_monitored_branches, n_outage_branches, n_buses)``.
#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct OtdfResult {
    pub otdf_data: Vec<f64>,
    pub n_monitored: usize,
    pub n_outages: usize,
    pub n_buses: usize,
    pub bus_indices_vec: Vec<usize>,
    pub bus_numbers_vec: Vec<u32>,
    pub monitored_branch_indices: Vec<usize>,
    pub outage_branch_indices: Vec<usize>,
    pub monitored_from_vec: Vec<u32>,
    pub monitored_to_vec: Vec<u32>,
    pub monitored_circuit_vec: Vec<String>,
    pub outage_from_vec: Vec<u32>,
    pub outage_to_vec: Vec<u32>,
    pub outage_circuit_vec: Vec<String>,
}

#[pymethods]
impl OtdfResult {
    /// OTDF tensor as numpy array of shape (n_monitored, n_outages, n_buses).
    #[getter]
    fn otdf<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray3<f64>>> {
        let array = Array3::from_shape_vec(
            (self.n_monitored, self.n_outages, self.n_buses),
            self.otdf_data.clone(),
        )
        .map_err(to_pyerr)?;
        Ok(array.into_pyarray(py))
    }

    /// Get the OTDF vector for a (monitored_branch_idx, outage_branch_idx) pair.
    ///
    /// Returns a 1-D numpy array of length n_buses.
    /// Raises KeyError if the pair was not computed.
    fn get<'py>(
        &self,
        py: Python<'py>,
        monitored: usize,
        outage: usize,
    ) -> PyResult<Bound<'py, PyArray1<f64>>> {
        let monitored_pos = self
            .monitored_branch_indices
            .iter()
            .position(|&idx| idx == monitored)
            .ok_or_else(|| {
                PyErr::new::<pyo3::exceptions::PyKeyError, _>(format!(
                    "no OTDF entry for monitored branch {monitored}"
                ))
            })?;
        let outage_pos = self
            .outage_branch_indices
            .iter()
            .position(|&idx| idx == outage)
            .ok_or_else(|| {
                PyErr::new::<pyo3::exceptions::PyKeyError, _>(format!(
                    "no OTDF entry for outage branch {outage}"
                ))
            })?;
        let start = (monitored_pos * self.n_outages + outage_pos) * self.n_buses;
        let end = start + self.n_buses;
        Ok(self.otdf_data[start..end].to_vec().into_pyarray(py))
    }

    /// Internal monitored branch indices (row order).
    #[getter]
    fn monitored_branches(&self) -> Vec<usize> {
        self.monitored_branch_indices.clone()
    }

    /// Internal outage branch indices (column order).
    #[getter]
    fn outage_branches(&self) -> Vec<usize> {
        self.outage_branch_indices.clone()
    }

    /// Number of buses (length of each OTDF vector).
    #[getter]
    fn n_buses(&self) -> usize {
        self.n_buses
    }

    /// Internal bus indices for the OTDF bus axis.
    #[getter]
    fn bus_indices(&self) -> Vec<usize> {
        self.bus_indices_vec.clone()
    }

    /// External bus numbers for the bus axis.
    #[getter]
    fn bus_numbers(&self) -> Vec<u32> {
        self.bus_numbers_vec.clone()
    }

    /// External from-bus numbers for monitored branches (row order).
    #[getter]
    fn monitored_from(&self) -> Vec<u32> {
        self.monitored_from_vec.clone()
    }

    /// External to-bus numbers for monitored branches (row order).
    #[getter]
    fn monitored_to(&self) -> Vec<u32> {
        self.monitored_to_vec.clone()
    }

    /// Circuit identifiers for monitored branches (row order).
    #[getter]
    fn monitored_circuit(&self) -> Vec<String> {
        self.monitored_circuit_vec.clone()
    }

    /// Stable monitored branch keys (row order).
    #[getter]
    fn monitored_keys(&self) -> Vec<(u32, u32, String)> {
        self.monitored_from_vec
            .iter()
            .zip(self.monitored_to_vec.iter())
            .zip(self.monitored_circuit_vec.iter())
            .map(|((&from_bus, &to_bus), circuit)| (from_bus, to_bus, circuit.clone()))
            .collect()
    }

    /// External from-bus numbers for outage branches (column order).
    #[getter]
    fn outage_from(&self) -> Vec<u32> {
        self.outage_from_vec.clone()
    }

    /// External to-bus numbers for outage branches (column order).
    #[getter]
    fn outage_to(&self) -> Vec<u32> {
        self.outage_to_vec.clone()
    }

    /// Circuit identifiers for outage branches (column order).
    #[getter]
    fn outage_circuit(&self) -> Vec<String> {
        self.outage_circuit_vec.clone()
    }

    /// Stable outage branch keys (column order).
    #[getter]
    fn outage_keys(&self) -> Vec<(u32, u32, String)> {
        self.outage_from_vec
            .iter()
            .zip(self.outage_to_vec.iter())
            .zip(self.outage_circuit_vec.iter())
            .map(|((&from_bus, &to_bus), circuit)| (from_bus, to_bus, circuit.clone()))
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "OtdfResult(monitored={}, outage={}, n_buses={})",
            self.n_monitored, self.n_outages, self.n_buses
        )
    }
}

fn build_otdf_result(network: &Network, otdf: surge_dc::OtdfResult) -> OtdfResult {
    let (monitored_branch_indices, outage_branch_indices, bus_indices_vec, otdf_data) =
        otdf.into_parts();
    let bus_numbers_vec: Vec<u32> = bus_indices_vec
        .iter()
        .map(|&idx| network.inner.buses[idx].number)
        .collect();
    let monitored_from_vec: Vec<u32> = monitored_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].from_bus)
        .collect();
    let monitored_to_vec: Vec<u32> = monitored_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].to_bus)
        .collect();
    let monitored_circuit_vec: Vec<String> = monitored_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].circuit.clone())
        .collect();
    let outage_from_vec: Vec<u32> = outage_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].from_bus)
        .collect();
    let outage_to_vec: Vec<u32> = outage_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].to_bus)
        .collect();
    let outage_circuit_vec: Vec<String> = outage_branch_indices
        .iter()
        .map(|&idx| network.inner.branches[idx].circuit.clone())
        .collect();

    OtdfResult {
        otdf_data,
        n_monitored: monitored_branch_indices.len(),
        n_outages: outage_branch_indices.len(),
        n_buses: bus_indices_vec.len(),
        bus_indices_vec,
        bus_numbers_vec,
        monitored_branch_indices,
        outage_branch_indices,
        monitored_from_vec,
        monitored_to_vec,
        monitored_circuit_vec,
        outage_from_vec,
        outage_to_vec,
        outage_circuit_vec,
    }
}

/// Compute Outage Transfer Distribution Factors (OTDF).
///
/// ``OTDF[(m, k)][bus] = PTDF[m][bus] + LODF[m, k] × PTDF[k][bus]``
///
/// This is the post-contingency sensitivity of flow on monitored branch ``m``
/// to a 1 p.u. injection at ``bus`` when outage branch ``k`` is tripped.
///
/// Factors B' once; performs one KLU solve per unique branch index in the
/// union of monitored and outage sets.
///
/// Args:
///     network: Power system network.
///     monitored_branches: List of internal branch indices to monitor.
///     outage_branches: List of internal branch indices to outage.
///     bus_indices: Optional list of internal bus indices for the OTDF bus axis.
///
/// Returns:
///     OtdfResult — use ``.get(monitored_idx, outage_idx)`` to access vectors.
#[pyfunction]
#[pyo3(signature = (
    network,
    monitored_branches,
    outage_branches,
    bus_indices = None,
    slack_weights = None,
    headroom_slack = false,
    headroom_slack_buses = None,
))]
pub fn compute_otdf(
    py: Python<'_>,
    network: &Network,
    monitored_branches: Vec<usize>,
    outage_branches: Vec<usize>,
    bus_indices: Option<Vec<usize>>,
    slack_weights: Option<HashMap<u32, f64>>,
    headroom_slack: bool,
    headroom_slack_buses: Option<Vec<u32>>,
) -> PyResult<OtdfResult> {
    network.validate()?;
    let net = Arc::clone(&network.inner);
    let mon = monitored_branches.clone();
    let out = outage_branches.clone();
    let buses = bus_indices.unwrap_or_else(|| (0..network.inner.n_buses()).collect());
    let options = build_dc_sensitivity_options(
        &net,
        slack_weights.as_ref(),
        headroom_slack,
        headroom_slack_buses.as_deref(),
    )?;
    let otdf = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let request = surge_dc::OtdfRequest::new(&mon, &out)
                    .with_bus_indices(&buses)
                    .with_options(options);
                surge_dc::compute_otdf(&net, &request).map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("compute_otdf failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(build_otdf_result(network, otdf))
}

// ---------------------------------------------------------------------------
// BLDF
// ---------------------------------------------------------------------------

/// Bus Load Distribution Factor matrix result.
///
/// ``bldf[b, l]`` = change in per-unit flow on branch ``l`` per 1 p.u.
/// load increase at bus ``b`` (slack absorbs the difference).
/// Dimensions: n_buses × n_branches.
#[pyclass]
pub struct BldfResult {
    data: Vec<f64>,
    n_buses: usize,
    n_branches: usize,
    bus_numbers_vec: Vec<u32>,
    branch_from_vec: Vec<u32>,
    branch_to_vec: Vec<u32>,
}

#[pymethods]
impl BldfResult {
    /// BLDF matrix as numpy array of shape (n_buses, n_branches).
    #[getter]
    fn matrix<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<f64>>> {
        numpy::PyArray2::from_vec2(
            py,
            &(0..self.n_buses)
                .map(|i| self.data[i * self.n_branches..(i + 1) * self.n_branches].to_vec())
                .collect::<Vec<_>>(),
        )
        .map_err(to_pyerr)
    }

    /// External bus numbers (row order).
    #[getter]
    fn bus_numbers(&self) -> Vec<u32> {
        self.bus_numbers_vec.clone()
    }

    /// External from-bus numbers (column order).
    #[getter]
    fn branch_from(&self) -> Vec<u32> {
        self.branch_from_vec.clone()
    }

    /// External to-bus numbers (column order).
    #[getter]
    fn branch_to(&self) -> Vec<u32> {
        self.branch_to_vec.clone()
    }

    /// Number of buses (rows).
    #[getter]
    fn n_buses(&self) -> usize {
        self.n_buses
    }

    /// Number of branches (columns).
    #[getter]
    fn n_branches(&self) -> usize {
        self.n_branches
    }

    fn __repr__(&self) -> String {
        format!(
            "BldfResult(buses={}, branches={})",
            self.n_buses, self.n_branches
        )
    }
}

/// Compute Bus Load Distribution Factors.
///
/// ``bldf[b, l] = -ptdf[l, b]``: the change in per-unit flow on branch ``l``
/// per 1 p.u. load increase at bus ``b``.
///
/// Args:
///     network: Power system network.
///
/// Returns:
///     BldfResult with shape (n_buses, n_branches).
#[pyfunction]
pub fn compute_bldf(py: Python<'_>, network: &Network) -> PyResult<BldfResult> {
    let n_bus = network.inner.n_buses();
    let n_br = network.inner.n_branches();
    network.validate()?;

    let net = Arc::clone(&network.inner);
    let (bldf_data, bus_nums, br_from, br_to) = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let bldf =
                    surge_transfer::matrices::compute_bldf(&net).map_err(|e| e.to_string())?;
                let nb = net.n_buses();
                let nbr = net.n_branches();
                let mut data = vec![0.0; nb * nbr];
                for b in 0..nb {
                    for l in 0..nbr {
                        data[b * nbr + l] = bldf.values[(b, l)];
                    }
                }
                let bus_nums: Vec<u32> = net.buses.iter().map(|b| b.number).collect();
                let br_from: Vec<u32> = net.branches.iter().map(|b| b.from_bus).collect();
                let br_to: Vec<u32> = net.branches.iter().map(|b| b.to_bus).collect();
                Ok::<_, String>((data, bus_nums, br_from, br_to))
            }))
            .map_err(|e| format!("compute_bldf failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;

    Ok(BldfResult {
        data: bldf_data,
        n_buses: n_bus,
        n_branches: n_br,
        bus_numbers_vec: bus_nums,
        branch_from_vec: br_from,
        branch_to_vec: br_to,
    })
}

// ---------------------------------------------------------------------------
// GSF
// ---------------------------------------------------------------------------

/// Generation Shift Factor matrix result.
///
/// ``gsf[l, g]`` = change in per-unit flow on branch ``l`` per 1 p.u. injection
/// increase at generator ``g``'s bus (slack absorbs the difference).
#[pyclass]
pub struct GsfResult {
    gsf_data: Vec<f64>,
    n_branches: usize,
    n_gen: usize,
    gen_buses_vec: Vec<u32>,
    branch_from_vec: Vec<u32>,
    branch_to_vec: Vec<u32>,
}

#[pymethods]
impl GsfResult {
    /// GSF matrix as numpy array of shape (n_branches, n_generators).
    #[getter]
    fn gsf<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<f64>>> {
        row_major_pyarray2(py, self.gsf_data.clone(), self.n_branches, self.n_gen)
    }

    /// External bus numbers for each generator (column order).
    #[getter]
    fn gen_buses(&self) -> Vec<u32> {
        self.gen_buses_vec.clone()
    }

    /// External from-bus numbers for each branch (row order).
    #[getter]
    fn branch_from(&self) -> Vec<u32> {
        self.branch_from_vec.clone()
    }

    /// External to-bus numbers for each branch (row order).
    #[getter]
    fn branch_to(&self) -> Vec<u32> {
        self.branch_to_vec.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "GsfResult(branches={}, generators={})",
            self.n_branches, self.n_gen
        )
    }
}

/// Compute Generation Shift Factors in canonical branch-by-generator orientation.
///
/// Internally prepares the required PTDF rows. GSF[l, g] = PTDF[l, bus_g].
///
/// Args:
///     network: Power system network.
///
/// Returns:
///     GsfResult with gsf matrix, gen_buses, branch_from, branch_to.
#[pyfunction]
pub fn compute_gsf<'py>(py: Python<'py>, network: &Network) -> PyResult<GsfResult> {
    let n_br = network.inner.n_branches();
    let net = Arc::clone(&network.inner);
    let gsf = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_transfer::matrices::compute_gsf(&net).map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("compute_gsf failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;

    let n_gen = gsf.gen_buses.len();
    let mut data = vec![0.0; n_br * n_gen];
    for i in 0..n_br {
        for j in 0..n_gen {
            data[i * n_gen + j] = gsf.values[(i, j)];
        }
    }

    let branch_from: Vec<u32> = network.inner.branches.iter().map(|b| b.from_bus).collect();
    let branch_to: Vec<u32> = network.inner.branches.iter().map(|b| b.to_bus).collect();

    Ok(GsfResult {
        gsf_data: data,
        n_branches: n_br,
        n_gen,
        gen_buses_vec: gsf.gen_buses,
        branch_from_vec: branch_from,
        branch_to_vec: branch_to,
    })
}

// ---------------------------------------------------------------------------
// InjectionCapability
// ---------------------------------------------------------------------------

/// Per-bus injection capability result.
///
/// Shows the maximum MW injection at each bus before any branch exceeds its
/// thermal limit under N-0 and N-1 conditions.
#[pyclass]
pub struct InjectionCapabilityResult {
    pub by_bus: Vec<(u32, f64)>,
    pub failed_contingencies: Vec<usize>,
}

#[pymethods]
impl InjectionCapabilityResult {
    /// List of (bus_number, max_injection_mw) tuples.
    #[getter]
    fn by_bus(&self) -> Vec<(u32, f64)> {
        self.by_bus.clone()
    }

    /// Branch indices of contingencies that failed during evaluation.
    /// Bus limits affected by these contingencies are conservatively zeroed.
    #[getter]
    fn failed_contingencies(&self) -> Vec<usize> {
        self.failed_contingencies.clone()
    }

    /// Return a pandas DataFrame with bus_id and max_injection_mw columns.
    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        let buses: Vec<u32> = self.by_bus.iter().map(|(b, _)| *b).collect();
        let caps: Vec<f64> = self.by_bus.iter().map(|(_, c)| *c).collect();
        dict.set_item("bus_id", buses)?;
        dict.set_item("max_injection_mw", caps)?;
        dict_to_dataframe_with_index(py, dict, &["bus_id"])
    }

    fn __repr__(&self) -> String {
        format!("InjectionCapabilityResult(n_buses={})", self.by_bus.len())
    }
}

/// Compute per-bus injection capability under N-0/N-1 thermal limits.
///
/// Internally prepares the required PTDF rows and LODF columns. Determines how
/// much additional MW can be injected at each bus before any monitored branch
/// hits its thermal limit under base case and N-1 contingency conditions.
///
/// Args:
///     network: Power system network.
///     post_contingency_rating_fraction: Emergency rating factor (default 1.0).
///     exact: When true, re-solves each contingency exactly instead of using
///         first-order LODF screening.
///     monitored_branches: Optional list of monitored branch indices.
///     contingency_branches: Optional list of outage branch indices.
///
/// Returns:
///     InjectionCapabilityResult with per-bus maximum injection (MW).
#[pyfunction]
#[pyo3(signature = (
    network,
    post_contingency_rating_fraction=1.0,
    exact=false,
    monitored_branches=None,
    contingency_branches=None,
    slack_weights=None
))]
pub fn compute_injection_capability(
    py: Python<'_>,
    network: &Network,
    post_contingency_rating_fraction: f64,
    exact: bool,
    monitored_branches: Option<Vec<usize>>,
    contingency_branches: Option<Vec<usize>>,
    slack_weights: Option<Vec<(usize, f64)>>,
) -> PyResult<InjectionCapabilityResult> {
    let net = Arc::clone(&network.inner);
    let sensitivity_options =
        slack_weights.map(|w| surge_dc::DcSensitivityOptions::with_slack_weights(&w));
    let options = surge_transfer::injection::InjectionCapabilityOptions {
        monitored_branches,
        contingency_branches,
        post_contingency_rating_fraction,
        exact,
        sensitivity_options,
    };

    let (by_bus, failed_contingencies) = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let result =
                    surge_transfer::injection::compute_injection_capability(&net, &options)
                        .map_err(|e| e.to_string())?;
                Ok::<_, String>((result.by_bus, result.failed_contingencies))
            }))
            .map_err(|e| {
                format!(
                    "compute_injection_capability failed: {}",
                    extract_panic_msg(e)
                )
            })
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;

    Ok(InjectionCapabilityResult {
        by_bus,
        failed_contingencies,
    })
}

// ---------------------------------------------------------------------------
// Y-bus and Jacobian sparse matrix results
// ---------------------------------------------------------------------------

/// Sparse Y-bus admittance matrix in CSC format with complex values.
///
/// Use ``to_scipy()`` to get a ``scipy.sparse.csc_matrix`` (complex128).
/// Alternatively access raw CSC arrays via ``indptr``, ``indices``, ``data``.
#[pyclass]
pub struct YBusResult {
    pub col_ptr: Vec<i64>,
    pub row_idx: Vec<i64>,
    pub data: Vec<Complex64>,
    pub n: usize,
    pub bus_numbers_vec: Vec<u32>,
}

#[pymethods]
impl YBusResult {
    /// CSC column pointers (length n+1).
    #[getter]
    fn indptr<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<i64>> {
        self.col_ptr.clone().into_pyarray(py)
    }

    /// CSC row indices (length nnz).
    #[getter]
    fn indices<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<i64>> {
        self.row_idx.clone().into_pyarray(py)
    }

    /// CSC complex admittance values (length nnz, complex128).
    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<Complex64>> {
        self.data.clone().into_pyarray(py)
    }

    /// Matrix shape (n_buses, n_buses).
    #[getter]
    fn shape(&self) -> (usize, usize) {
        (self.n, self.n)
    }

    /// Number of non-zero entries.
    #[getter]
    fn nnz(&self) -> usize {
        self.data.len()
    }

    /// External bus numbers (row/column ordering).
    #[getter]
    fn bus_numbers(&self) -> Vec<u32> {
        self.bus_numbers_vec.clone()
    }

    /// Convert to a ``scipy.sparse.csc_matrix`` (complex128).
    ///
    /// Requires ``scipy`` to be installed.
    fn to_scipy<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let scipy_sparse = py.import("scipy.sparse")?;
        let indptr = self.col_ptr.clone().into_pyarray(py);
        let indices = self.row_idx.clone().into_pyarray(py);
        let data = self.data.clone().into_pyarray(py);
        let shape = (self.n, self.n).into_pyobject(py)?;
        scipy_sparse.call_method1("csc_matrix", ((&data, &indices, &indptr), &shape))
    }

    /// Dense numpy array (complex128). Only practical for small networks.
    fn to_dense<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<Complex64>>> {
        let mut dense = vec![Complex64::new(0.0, 0.0); self.n * self.n];
        for col in 0..self.n {
            let start = self.col_ptr[col] as usize;
            let end = self.col_ptr[col + 1] as usize;
            for idx in start..end {
                let row = self.row_idx[idx] as usize;
                dense[row * self.n + col] = self.data[idx];
            }
        }
        let rows: Vec<Vec<Complex64>> = (0..self.n)
            .map(|i| dense[i * self.n..(i + 1) * self.n].to_vec())
            .collect();
        PyArray2::from_vec2(py, &rows).map_err(to_pyerr)
    }

    fn __repr__(&self) -> String {
        format!("YBusResult(n={}, nnz={})", self.n, self.data.len())
    }
}

/// Sparse Jacobian matrix in CSC format (real-valued).
///
/// The Jacobian J = [H N; M L] maps voltage corrections to power mismatches.
/// Rows: [ΔP(pvpq), ΔQ(pq)]; Columns: [Δθ(pvpq), ΔVm(pq)].
///
/// Use ``to_scipy()`` to get a ``scipy.sparse.csc_matrix`` (float64).
#[pyclass]
pub struct JacobianResult {
    pub col_ptr: Vec<i64>,
    pub row_idx: Vec<i64>,
    pub data: Vec<f64>,
    pub nrows: usize,
    pub ncols: usize,
    /// External bus numbers for pvpq buses (theta variables / P equations).
    pub pvpq_buses_vec: Vec<u32>,
    /// External bus numbers for PQ buses (Vm variables / Q equations).
    pub pq_buses_vec: Vec<u32>,
}

#[pymethods]
impl JacobianResult {
    /// CSC column pointers.
    #[getter]
    fn indptr<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<i64>> {
        self.col_ptr.clone().into_pyarray(py)
    }

    /// CSC row indices.
    #[getter]
    fn indices<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<i64>> {
        self.row_idx.clone().into_pyarray(py)
    }

    /// CSC values (float64).
    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.data.clone().into_pyarray(py)
    }

    /// Matrix shape (n_pvpq + n_pq, n_pvpq + n_pq).
    #[getter]
    fn shape(&self) -> (usize, usize) {
        (self.nrows, self.ncols)
    }

    /// Number of non-zero entries.
    #[getter]
    fn nnz(&self) -> usize {
        self.data.len()
    }

    /// External bus numbers for PV+PQ buses (theta variable ordering).
    #[getter]
    fn pvpq_buses(&self) -> Vec<u32> {
        self.pvpq_buses_vec.clone()
    }

    /// External bus numbers for PQ buses (Vm variable ordering).
    #[getter]
    fn pq_buses(&self) -> Vec<u32> {
        self.pq_buses_vec.clone()
    }

    /// Convert to a ``scipy.sparse.csc_matrix`` (float64).
    fn to_scipy<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let scipy_sparse = py.import("scipy.sparse")?;
        let indptr = self.col_ptr.clone().into_pyarray(py);
        let indices = self.row_idx.clone().into_pyarray(py);
        let data = self.data.clone().into_pyarray(py);
        let shape = (self.nrows, self.ncols).into_pyobject(py)?;
        scipy_sparse.call_method1("csc_matrix", ((&data, &indices, &indptr), &shape))
    }

    /// Dense numpy array (float64). Only practical for small networks.
    fn to_dense<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<f64>>> {
        let mut dense = vec![0.0f64; self.nrows * self.ncols];
        for col in 0..self.ncols {
            let start = self.col_ptr[col] as usize;
            let end = self.col_ptr[col + 1] as usize;
            for idx in start..end {
                let row = self.row_idx[idx] as usize;
                dense[row * self.ncols + col] = self.data[idx];
            }
        }
        let rows: Vec<Vec<f64>> = (0..self.nrows)
            .map(|i| dense[i * self.ncols..(i + 1) * self.ncols].to_vec())
            .collect();
        PyArray2::from_vec2(py, &rows).map_err(to_pyerr)
    }

    fn __repr__(&self) -> String {
        format!(
            "JacobianResult(shape=({}, {}), nnz={}, n_pvpq={}, n_pq={})",
            self.nrows,
            self.ncols,
            self.data.len(),
            self.pvpq_buses_vec.len(),
            self.pq_buses_vec.len()
        )
    }
}
