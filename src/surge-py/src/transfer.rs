// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::exceptions::to_pyerr;
use crate::matrices::InjectionCapabilityResult;
use crate::network::Network;

fn clone_paths(paths: &[PyRef<'_, TransferPath>]) -> Vec<surge_transfer::TransferPath> {
    paths.iter().map(|path| path.inner.clone()).collect()
}

fn clone_flowgates(flowgates: &[PyRef<'_, Flowgate>]) -> Vec<surge_transfer::Flowgate> {
    flowgates
        .iter()
        .map(|flowgate| flowgate.inner.clone())
        .collect()
}

fn atc_options_or_default(options: Option<&AtcOptions>) -> surge_transfer::AtcOptions {
    options
        .map(|options| options.inner.clone())
        .unwrap_or_default()
}

#[pyclass(name = "TransferPath")]
pub struct TransferPath {
    pub inner: surge_transfer::TransferPath,
}

#[pymethods]
impl TransferPath {
    #[new]
    fn new(name: String, source_buses: Vec<u32>, sink_buses: Vec<u32>) -> Self {
        Self {
            inner: surge_transfer::TransferPath::new(name, source_buses, sink_buses),
        }
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[setter]
    fn set_name(&mut self, name: String) {
        self.inner.name = name;
    }

    #[getter]
    fn source_buses(&self) -> Vec<u32> {
        self.inner.source_buses.clone()
    }

    #[setter]
    fn set_source_buses(&mut self, source_buses: Vec<u32>) {
        self.inner.source_buses = source_buses;
    }

    #[getter]
    fn sink_buses(&self) -> Vec<u32> {
        self.inner.sink_buses.clone()
    }

    #[setter]
    fn set_sink_buses(&mut self, sink_buses: Vec<u32>) {
        self.inner.sink_buses = sink_buses;
    }

    fn __repr__(&self) -> String {
        format!(
            "TransferPath(name='{}', sources={}, sinks={})",
            self.inner.name,
            self.inner.source_buses.len(),
            self.inner.sink_buses.len()
        )
    }
}

#[pyclass(name = "Flowgate")]
pub struct Flowgate {
    pub inner: surge_transfer::Flowgate,
}

#[pymethods]
impl Flowgate {
    #[new]
    #[pyo3(signature = (
        name,
        monitored_branch,
        normal_rating_mw,
        contingency_branch=None,
        contingency_rating_mw=None
    ))]
    fn new(
        name: String,
        monitored_branch: usize,
        normal_rating_mw: f64,
        contingency_branch: Option<usize>,
        contingency_rating_mw: Option<f64>,
    ) -> Self {
        Self {
            inner: surge_transfer::Flowgate::new(
                name,
                monitored_branch,
                contingency_branch,
                normal_rating_mw,
                contingency_rating_mw,
            ),
        }
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[setter]
    fn set_name(&mut self, name: String) {
        self.inner.name = name;
    }

    #[getter]
    fn monitored_branch(&self) -> usize {
        self.inner.monitored_branch
    }

    #[setter]
    fn set_monitored_branch(&mut self, monitored_branch: usize) {
        self.inner.monitored_branch = monitored_branch;
    }

    #[getter]
    fn contingency_branch(&self) -> Option<usize> {
        self.inner.contingency_branch
    }

    #[setter]
    fn set_contingency_branch(&mut self, contingency_branch: Option<usize>) {
        self.inner.contingency_branch = contingency_branch;
    }

    #[getter]
    fn normal_rating_mw(&self) -> f64 {
        self.inner.normal_rating_mw
    }

    #[setter]
    fn set_normal_rating_mw(&mut self, normal_rating_mw: f64) {
        self.inner.normal_rating_mw = normal_rating_mw;
    }

    #[getter]
    fn contingency_rating_mw(&self) -> Option<f64> {
        self.inner.contingency_rating_mw
    }

    #[setter]
    fn set_contingency_rating_mw(&mut self, contingency_rating_mw: Option<f64>) {
        self.inner.contingency_rating_mw = contingency_rating_mw;
    }

    fn __repr__(&self) -> String {
        format!(
            "Flowgate(name='{}', monitored_branch={}, contingency_branch={:?}, normal_rating_mw={:.1}, contingency_rating_mw={:?})",
            self.inner.name,
            self.inner.monitored_branch,
            self.inner.contingency_branch,
            self.inner.normal_rating_mw,
            self.inner.contingency_rating_mw
        )
    }
}

#[pyclass(name = "AtcOptions")]
pub struct AtcOptions {
    pub inner: surge_transfer::AtcOptions,
}

#[pymethods]
impl AtcOptions {
    #[new]
    #[pyo3(signature = (
        monitored_branches=None,
        contingency_branches=None,
        trm_fraction=0.05,
        cbm_mw=0.0,
        etc_mw=0.0
    ))]
    fn new(
        monitored_branches: Option<Vec<usize>>,
        contingency_branches: Option<Vec<usize>>,
        trm_fraction: f64,
        cbm_mw: f64,
        etc_mw: f64,
    ) -> Self {
        Self {
            inner: surge_transfer::AtcOptions {
                monitored_branches,
                contingency_branches,
                margins: surge_transfer::AtcMargins {
                    trm_fraction,
                    cbm_mw,
                    etc_mw,
                },
            },
        }
    }

    #[getter]
    fn monitored_branches(&self) -> Option<Vec<usize>> {
        self.inner.monitored_branches.clone()
    }

    #[setter]
    fn set_monitored_branches(&mut self, monitored_branches: Option<Vec<usize>>) {
        self.inner.monitored_branches = monitored_branches;
    }

    #[getter]
    fn contingency_branches(&self) -> Option<Vec<usize>> {
        self.inner.contingency_branches.clone()
    }

    #[setter]
    fn set_contingency_branches(&mut self, contingency_branches: Option<Vec<usize>>) {
        self.inner.contingency_branches = contingency_branches;
    }

    #[getter]
    fn trm_fraction(&self) -> f64 {
        self.inner.margins.trm_fraction
    }

    #[setter]
    fn set_trm_fraction(&mut self, trm_fraction: f64) {
        self.inner.margins.trm_fraction = trm_fraction;
    }

    #[getter]
    fn cbm_mw(&self) -> f64 {
        self.inner.margins.cbm_mw
    }

    #[setter]
    fn set_cbm_mw(&mut self, cbm_mw: f64) {
        self.inner.margins.cbm_mw = cbm_mw;
    }

    #[getter]
    fn etc_mw(&self) -> f64 {
        self.inner.margins.etc_mw
    }

    #[setter]
    fn set_etc_mw(&mut self, etc_mw: f64) {
        self.inner.margins.etc_mw = etc_mw;
    }

    fn __repr__(&self) -> String {
        format!(
            "AtcOptions(monitored_branches={:?}, contingency_branches={:?}, trm_fraction={:.3}, cbm_mw={:.1}, etc_mw={:.1})",
            self.inner.monitored_branches,
            self.inner.contingency_branches,
            self.inner.margins.trm_fraction,
            self.inner.margins.cbm_mw,
            self.inner.margins.etc_mw
        )
    }
}

#[pyclass(name = "AfcResult")]
pub struct AfcResult {
    pub flowgate_name: String,
    pub afc_mw: f64,
    pub binding_branch: usize,
    pub binding_contingency: Option<usize>,
}

#[pymethods]
impl AfcResult {
    #[getter]
    fn flowgate_name(&self) -> &str {
        &self.flowgate_name
    }

    #[getter]
    fn afc_mw(&self) -> f64 {
        self.afc_mw
    }

    #[getter]
    fn binding_branch(&self) -> usize {
        self.binding_branch
    }

    #[getter]
    fn binding_contingency(&self) -> Option<usize> {
        self.binding_contingency
    }

    fn __repr__(&self) -> String {
        format!(
            "AfcResult(flowgate='{}', afc_mw={:.1}, binding_branch={}, contingency={:?})",
            self.flowgate_name, self.afc_mw, self.binding_branch, self.binding_contingency
        )
    }
}

#[pyclass(name = "AcAtcResult")]
pub struct AcAtcResult {
    pub inner: surge_transfer::AcAtcResult,
}

#[pymethods]
impl AcAtcResult {
    #[getter]
    fn atc_mw(&self) -> f64 {
        self.inner.atc_mw
    }

    #[getter]
    fn thermal_limit_mw(&self) -> f64 {
        self.inner.thermal_limit_mw
    }

    #[getter]
    fn voltage_limit_mw(&self) -> f64 {
        self.inner.voltage_limit_mw
    }

    #[getter]
    fn limiting_bus(&self) -> Option<usize> {
        self.inner.limiting_bus
    }

    #[getter]
    fn binding_branch(&self) -> Option<usize> {
        self.inner.binding_branch
    }

    #[getter]
    fn limiting_constraint(&self) -> &str {
        self.inner.limiting_constraint.as_str()
    }

    /// Return the AC-ATC result as a JSON-serializable dictionary.
    ///
    /// Keys: ``atc_mw``, ``thermal_limit_mw``, ``voltage_limit_mw``,
    /// ``limiting_bus``, ``binding_branch``, ``limiting_constraint``.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("atc_mw", self.inner.atc_mw)?;
        d.set_item("thermal_limit_mw", self.inner.thermal_limit_mw)?;
        d.set_item("voltage_limit_mw", self.inner.voltage_limit_mw)?;
        d.set_item("limiting_bus", self.inner.limiting_bus)?;
        d.set_item("binding_branch", self.inner.binding_branch)?;
        d.set_item(
            "limiting_constraint",
            self.inner.limiting_constraint.as_str(),
        )?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "AcAtcResult(atc_mw={:.2}, thermal_limit_mw={:.2}, voltage_limit_mw={:.2}, limiting_constraint={})",
            self.inner.atc_mw,
            self.inner.thermal_limit_mw,
            self.inner.voltage_limit_mw,
            self.inner.limiting_constraint
        )
    }
}

#[pyclass(name = "NercAtcResult")]
pub struct NercAtcResult {
    pub inner: surge_transfer::NercAtcResult,
}

#[pymethods]
impl NercAtcResult {
    #[getter]
    fn atc_mw(&self) -> f64 {
        self.inner.atc_mw
    }

    #[getter]
    fn ttc_mw(&self) -> f64 {
        self.inner.ttc_mw
    }

    #[getter]
    fn trm_mw(&self) -> f64 {
        self.inner.trm_mw
    }

    #[getter]
    fn cbm_mw(&self) -> f64 {
        self.inner.cbm_mw
    }

    #[getter]
    fn etc_mw(&self) -> f64 {
        self.inner.etc_mw
    }

    #[getter]
    fn limit_cause(&self) -> String {
        self.inner.limit_cause.kind().to_string()
    }

    #[getter]
    fn binding_branch(&self) -> Option<usize> {
        self.inner.binding_branch()
    }

    #[getter]
    fn binding_contingency(&self) -> Option<usize> {
        self.inner.binding_contingency()
    }

    #[getter]
    fn monitored_branches(&self) -> Vec<usize> {
        self.inner.monitored_branches.clone()
    }

    #[getter]
    fn reactive_margin_warning(&self) -> bool {
        self.inner.reactive_margin_warning
    }

    #[getter]
    fn transfer_ptdf(&self) -> Vec<f64> {
        self.inner.transfer_ptdf.clone()
    }

    /// Return the NERC ATC result as a JSON-serializable dictionary.
    ///
    /// Keys: ``atc_mw``, ``ttc_mw``, ``trm_mw``, ``cbm_mw``, ``etc_mw``,
    /// ``limit_cause``, ``binding_branch``, ``binding_contingency``,
    /// ``monitored_branches``, ``reactive_margin_warning``, ``transfer_ptdf``.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("atc_mw", self.inner.atc_mw)?;
        d.set_item("ttc_mw", self.inner.ttc_mw)?;
        d.set_item("trm_mw", self.inner.trm_mw)?;
        d.set_item("cbm_mw", self.inner.cbm_mw)?;
        d.set_item("etc_mw", self.inner.etc_mw)?;
        d.set_item("limit_cause", self.inner.limit_cause.kind().to_string())?;
        d.set_item("binding_branch", self.inner.binding_branch())?;
        d.set_item("binding_contingency", self.inner.binding_contingency())?;
        d.set_item("monitored_branches", self.inner.monitored_branches.clone())?;
        d.set_item(
            "reactive_margin_warning",
            self.inner.reactive_margin_warning,
        )?;
        d.set_item("transfer_ptdf", self.inner.transfer_ptdf.clone())?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "NercAtcResult(atc_mw={:.1}, ttc_mw={:.1}, limit_cause='{}')",
            self.inner.atc_mw,
            self.inner.ttc_mw,
            self.inner.limit_cause.kind()
        )
    }
}

#[pyclass(name = "MultiTransferResult")]
pub struct MultiTransferResult {
    pub inner: surge_transfer::MultiTransferResult,
}

#[pymethods]
impl MultiTransferResult {
    #[getter]
    fn transfer_mw(&self) -> Vec<f64> {
        self.inner.transfer_mw.clone()
    }

    #[getter]
    fn binding_branch(&self) -> Vec<Option<usize>> {
        self.inner.binding_branch.clone()
    }

    #[getter]
    fn total_weighted_transfer(&self) -> f64 {
        self.inner.total_weighted_transfer
    }

    fn __repr__(&self) -> String {
        format!(
            "MultiTransferResult(paths={}, total_weighted_transfer={:.2})",
            self.inner.transfer_mw.len(),
            self.inner.total_weighted_transfer
        )
    }
}

#[pyclass(name = "TransferStudy")]
pub struct TransferStudy {
    study: surge_transfer::TransferStudy,
}

#[pymethods]
impl TransferStudy {
    #[pyo3(signature = (path, options=None))]
    fn compute_nerc_atc(
        &self,
        path: &TransferPath,
        options: Option<&AtcOptions>,
    ) -> PyResult<NercAtcResult> {
        let request = surge_transfer::NercAtcRequest {
            path: path.inner.clone(),
            options: atc_options_or_default(options),
        };
        let inner = self.study.compute_nerc_atc(&request).map_err(to_pyerr)?;
        Ok(NercAtcResult { inner })
    }

    #[pyo3(signature = (path, v_min=0.95, v_max=1.05))]
    fn compute_ac_atc(&self, path: &TransferPath, v_min: f64, v_max: f64) -> PyResult<AcAtcResult> {
        let request = surge_transfer::AcAtcRequest::new(path.inner.clone(), v_min, v_max);
        let inner = self.study.compute_ac_atc(&request).map_err(to_pyerr)?;
        Ok(AcAtcResult { inner })
    }

    fn compute_afc(
        &self,
        path: &TransferPath,
        flowgates: Vec<PyRef<'_, Flowgate>>,
    ) -> PyResult<Vec<AfcResult>> {
        let request = surge_transfer::AfcRequest {
            path: path.inner.clone(),
            flowgates: clone_flowgates(&flowgates),
        };
        let results = self.study.compute_afc(&request).map_err(to_pyerr)?;
        Ok(results
            .into_iter()
            .map(|result| AfcResult {
                flowgate_name: result.flowgate_name,
                afc_mw: result.afc_mw,
                binding_branch: result.binding_branch,
                binding_contingency: result.binding_contingency,
            })
            .collect())
    }

    #[pyo3(signature = (paths, weights=None, max_transfer_mw=None))]
    fn compute_multi_transfer(
        &self,
        paths: Vec<PyRef<'_, TransferPath>>,
        weights: Option<Vec<f64>>,
        max_transfer_mw: Option<Vec<f64>>,
    ) -> PyResult<MultiTransferResult> {
        let request = surge_transfer::MultiTransferRequest {
            paths: clone_paths(&paths),
            weights,
            max_transfer_mw,
        };
        let inner = self
            .study
            .compute_multi_transfer(&request)
            .map_err(to_pyerr)?;
        Ok(MultiTransferResult { inner })
    }

    #[pyo3(signature = (
        post_contingency_rating_fraction=1.0,
        exact=false,
        monitored_branches=None,
        contingency_branches=None,
        slack_weights=None
    ))]
    fn compute_injection_capability(
        &self,
        post_contingency_rating_fraction: f64,
        exact: bool,
        monitored_branches: Option<Vec<usize>>,
        contingency_branches: Option<Vec<usize>>,
        slack_weights: Option<Vec<(usize, f64)>>,
    ) -> PyResult<InjectionCapabilityResult> {
        let sensitivity_options =
            slack_weights.map(|w| surge_dc::DcSensitivityOptions::with_slack_weights(&w));
        let options = surge_transfer::injection::InjectionCapabilityOptions {
            monitored_branches,
            contingency_branches,
            post_contingency_rating_fraction,
            exact,
            sensitivity_options,
        };
        let result = self
            .study
            .compute_injection_capability(&options)
            .map_err(to_pyerr)?;
        Ok(InjectionCapabilityResult {
            by_bus: result.by_bus,
            failed_contingencies: result.failed_contingencies,
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "TransferStudy(buses={}, branches={})",
            self.study.network().n_buses(),
            self.study.network().n_branches()
        )
    }
}

#[pyfunction]
pub fn prepare_transfer_study(network: &Network) -> PyResult<TransferStudy> {
    let study = surge_transfer::TransferStudy::new(network.inner.as_ref()).map_err(to_pyerr)?;
    Ok(TransferStudy { study })
}

#[pyfunction]
#[pyo3(signature = (network, path, options=None))]
pub fn compute_nerc_atc(
    network: &Network,
    path: &TransferPath,
    options: Option<&AtcOptions>,
) -> PyResult<NercAtcResult> {
    let request = surge_transfer::NercAtcRequest {
        path: path.inner.clone(),
        options: atc_options_or_default(options),
    };
    let inner =
        surge_transfer::compute_nerc_atc(network.inner.as_ref(), &request).map_err(to_pyerr)?;
    Ok(NercAtcResult { inner })
}

#[pyfunction]
#[pyo3(signature = (network, path, v_min=0.95, v_max=1.05))]
pub fn compute_ac_atc(
    network: &Network,
    path: &TransferPath,
    v_min: f64,
    v_max: f64,
) -> PyResult<AcAtcResult> {
    let request = surge_transfer::AcAtcRequest::new(path.inner.clone(), v_min, v_max);
    let inner =
        surge_transfer::compute_ac_atc(network.inner.as_ref(), &request).map_err(to_pyerr)?;
    Ok(AcAtcResult { inner })
}

#[pyfunction]
pub fn compute_afc(
    network: &Network,
    path: &TransferPath,
    flowgates: Vec<PyRef<'_, Flowgate>>,
) -> PyResult<Vec<AfcResult>> {
    let request = surge_transfer::AfcRequest {
        path: path.inner.clone(),
        flowgates: clone_flowgates(&flowgates),
    };
    let results =
        surge_transfer::compute_afc(network.inner.as_ref(), &request).map_err(to_pyerr)?;
    Ok(results
        .into_iter()
        .map(|result| AfcResult {
            flowgate_name: result.flowgate_name,
            afc_mw: result.afc_mw,
            binding_branch: result.binding_branch,
            binding_contingency: result.binding_contingency,
        })
        .collect())
}

#[pyfunction]
#[pyo3(signature = (network, paths, weights=None, max_transfer_mw=None))]
pub fn compute_multi_transfer(
    network: &Network,
    paths: Vec<PyRef<'_, TransferPath>>,
    weights: Option<Vec<f64>>,
    max_transfer_mw: Option<Vec<f64>>,
) -> PyResult<MultiTransferResult> {
    let request = surge_transfer::MultiTransferRequest {
        paths: clone_paths(&paths),
        weights,
        max_transfer_mw,
    };
    let inner = surge_transfer::compute_multi_transfer(network.inner.as_ref(), &request)
        .map_err(to_pyerr)?;
    Ok(MultiTransferResult { inner })
}
