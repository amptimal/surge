// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python-facing result/solution wrapper types for the Surge power flow solver.
//!
//! This module contains all `#[pyclass]` result containers returned by solver
//! functions. No solver logic lives here — only the Python API surface.

use std::collections::HashMap;
use std::sync::Arc;

use numpy::{IntoPyArray, PyArray1, PyArray2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};

use crate::exceptions::{ConvergenceError, SurgeError};
use crate::network::Network;
use crate::rich_objects;
use crate::utils::dict_to_dataframe_with_index;

// ---------------------------------------------------------------------------
// Power flow solution wrapper
// ---------------------------------------------------------------------------

/// AC power flow solution.
#[pyclass(name = "AcPfResult")]
pub struct AcPfResult {
    pub(crate) inner: surge_solution::PfSolution,
    pub(crate) net: Option<Arc<surge_network::Network>>,
}

#[pymethods]
impl AcPfResult {
    fn attach_network(&mut self, network: &Network) -> PyResult<()> {
        let net = Arc::clone(&network.inner);
        if !self.inner.bus_numbers.is_empty() && self.inner.bus_numbers.len() != net.buses.len() {
            return Err(PyValueError::new_err(format!(
                "network has {} buses but solution has {} bus entries",
                net.buses.len(),
                self.inner.bus_numbers.len()
            )));
        }
        self.net = Some(net);
        Ok(())
    }

    /// Whether the solver converged.
    #[getter]
    fn converged(&self) -> bool {
        self.inner.status == surge_solution::SolveStatus::Converged
    }

    /// Solve status as a string.
    ///
    /// Possible values:
    ///
    /// * ``"Converged"`` — solver reached tolerance within ``max_iterations``.
    ///   ``vm`` and ``va`` hold the converged solution.
    ///
    /// * ``"MaxIterations"`` — solver exhausted ``max_iterations`` without
    ///   reaching tolerance.
    ///
    /// * ``"Diverged"`` — solver detected numerical instability and aborted
    ///   early.
    ///
    /// * ``"Unsolved"`` — solution object was constructed without a solve
    ///   (should not appear in normal usage).
    #[getter]
    fn status(&self) -> &'static str {
        match self.inner.status {
            surge_solution::SolveStatus::Converged => "Converged",
            surge_solution::SolveStatus::MaxIterations => "MaxIterations",
            surge_solution::SolveStatus::Diverged => "Diverged",
            surge_solution::SolveStatus::Unsolved => "Unsolved",
        }
    }

    /// Number of iterations.
    #[getter]
    fn iterations(&self) -> u32 {
        self.inner.iterations
    }

    /// Maximum power mismatch (p.u.).
    #[getter]
    fn max_mismatch(&self) -> f64 {
        self.inner.max_mismatch
    }

    /// Solve time in seconds.
    #[getter]
    fn solve_time_secs(&self) -> f64 {
        self.inner.solve_time_secs
    }

    /// Per-iteration convergence data as Nx2 numpy array ``[iteration, max_mismatch_pu]``.
    ///
    /// Empty (0×2) array if ``record_convergence_history=False`` was used (the default).
    #[getter]
    fn convergence_history<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray2<f64>> {
        let rows: Vec<[f64; 2]> = self
            .inner
            .convergence_history
            .iter()
            .map(|&(iter, mm)| [iter as f64, mm])
            .collect();
        let n = rows.len();
        let flat: Vec<f64> = rows.into_iter().flatten().collect();
        numpy::PyArray2::from_vec2(
            py,
            &if n == 0 {
                vec![]
            } else {
                flat.chunks(2).map(|c| c.to_vec()).collect()
            },
        )
        .unwrap()
    }

    /// Bus voltage magnitudes (p.u.) as numpy array.
    #[getter]
    fn vm<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.voltage_magnitude_pu.clone().into_pyarray(py)
    }

    /// Bus voltage angles (radians) as numpy array.
    ///
    /// For degrees use `va_deg` instead.
    #[getter]
    fn va_rad<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.voltage_angle_rad.clone().into_pyarray(py)
    }

    /// Bus voltage angles in **degrees** as numpy array.
    #[getter]
    fn va_deg<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        let deg: Vec<f64> = self
            .inner
            .voltage_angle_rad
            .iter()
            .map(|&a| a.to_degrees())
            .collect();
        deg.into_pyarray(py)
    }

    /// Active power injections (MW) as numpy array.
    #[getter]
    fn p_inject_mw<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray1<f64>>> {
        let net = self.attached_net()?;
        let base = net.base_mva;
        Ok(self
            .inner
            .active_power_injection_pu
            .iter()
            .map(|&v| v * base)
            .collect::<Vec<_>>()
            .into_pyarray(py))
    }

    /// Reactive power injections (MVAr) as numpy array.
    #[getter]
    fn q_inject_mvar<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray1<f64>>> {
        let net = self.attached_net()?;
        let base = net.base_mva;
        Ok(self
            .inner
            .reactive_power_injection_pu
            .iter()
            .map(|&v| v * base)
            .collect::<Vec<_>>()
            .into_pyarray(py))
    }

    /// Area interchange enforcement results, or ``None`` if enforcement was
    /// not enabled (``enforce_interchange=False``).
    ///
    /// Returns a dict with keys:
    ///
    /// * ``converged`` (bool) — whether all areas met their interchange targets.
    /// * ``iterations`` (int) — number of outer-loop iterations used.
    /// * ``areas`` — list of dicts, each with:
    ///   ``area`` (int), ``scheduled_mw`` (float), ``actual_mw`` (float),
    ///   ``error_mw`` (float), ``dispatch_method`` (str: ``"apf"``,
    ///   ``"slack_bus_fallback"``, or ``"converged"``).
    #[getter]
    fn area_interchange<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let res = match &self.inner.area_interchange {
            Some(r) => r,
            None => return Ok(None),
        };
        let d = PyDict::new(py);
        d.set_item("converged", res.converged)?;
        d.set_item("iterations", res.iterations)?;

        let mut area_list = Vec::with_capacity(res.areas.len());
        for entry in &res.areas {
            let ad = PyDict::new(py);
            ad.set_item("area", entry.area)?;
            ad.set_item("scheduled_mw", entry.scheduled_mw)?;
            ad.set_item("actual_mw", entry.actual_mw)?;
            ad.set_item("error_mw", entry.error_mw)?;
            let method_str = match entry.dispatch_method {
                surge_solution::AreaDispatchMethod::Apf => "apf",
                surge_solution::AreaDispatchMethod::SlackBusFallback => "slack_bus_fallback",
                surge_solution::AreaDispatchMethod::Converged => "converged",
            };
            ad.set_item("dispatch_method", method_str)?;
            area_list.push(ad);
        }
        d.set_item("areas", area_list)?;
        Ok(Some(d))
    }

    /// Branch apparent power flows (MVA) as numpy array.
    #[getter]
    fn branch_apparent_power<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.branch_apparent_power().into_pyarray(py)
    }

    /// Return a pandas DataFrame (or dict if pandas is not installed).
    ///
    /// Columns: bus_id, vm_pu, va_deg, p_mw, q_mvar.
    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let net = self.attached_net()?;
        let dict = PyDict::new(py);
        let bus_ids: Vec<u32> = net.buses.iter().map(|b| b.number).collect();
        dict.set_item("bus_id", bus_ids)?;
        dict.set_item("vm_pu", self.inner.voltage_magnitude_pu.clone())?;
        let va_deg: Vec<f64> = self
            .inner
            .voltage_angle_rad
            .iter()
            .map(|&a| a.to_degrees())
            .collect();
        dict.set_item("va_deg", va_deg)?;
        let base = net.base_mva;
        let p_mw: Vec<f64> = self
            .inner
            .active_power_injection_pu
            .iter()
            .map(|&p| p * base)
            .collect();
        dict.set_item("p_mw", p_mw)?;
        let q_mvar: Vec<f64> = self
            .inner
            .reactive_power_injection_pu
            .iter()
            .map(|&q| q * base)
            .collect();
        dict.set_item("q_mvar", q_mvar)?;
        dict_to_dataframe_with_index(py, dict, &["bus_id"])
    }

    /// Branch loading percentage as numpy array.
    #[getter]
    fn branch_loading_pct<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray1<f64>>> {
        let net = self.attached_net()?;
        let loading = self
            .inner
            .branch_loading_pct(net)
            .map_err(|err| PyValueError::new_err(err.to_string()))?;
        Ok(loading.into_pyarray(py))
    }

    /// Per-generator reactive power output (MVAr) as numpy array, in network.generators order.
    ///
    /// Computed from the post-convergence bus Q injection:
    ///   Qg_bus = q_inject[bus] * base_mva + Qd_bus - Bs_bus * Vm²
    /// For buses with multiple generators, Q is apportioned by (Qmax - Qmin) range.
    /// PQ-clamped generators (at Qmax or Qmin) are assigned their limit directly.
    #[getter]
    fn gen_q_mvar<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray1<f64>>> {
        let net = self.attached_net()?;
        Ok(self
            .inner
            .generator_reactive_power_mvar(net)
            .into_pyarray(py))
    }

    /// External bus numbers of buses that were switched PV→PQ by reactive limit enforcement.
    #[getter]
    fn q_limited_buses(&self) -> Vec<u32> {
        self.inner.q_limited_buses.clone()
    }

    /// Total number of PV↔PQ bus-type switches performed during Q-limit enforcement.
    #[getter]
    fn n_q_limit_switches(&self) -> u32 {
        self.inner.n_q_limit_switches
    }

    /// Island membership for each bus (0-indexed island ID, per internal bus order).
    /// Empty when island detection was not performed.
    #[getter]
    fn island_ids(&self) -> Vec<usize> {
        self.inner.island_ids.clone()
    }

    /// Number of distinct islands detected.  0 when detection was not performed.
    #[getter]
    fn n_islands(&self) -> usize {
        self.inner.n_islands()
    }

    // ───────────────────────────────────────────────────────────────────────
    // Rich object accessors
    // ───────────────────────────────────────────────────────────────────────

    /// All buses with solved power flow results.
    ///
    /// Returns a list of `BusSolved` objects, one per bus, each combining
    /// the static network model data with the solved voltage magnitudes,
    /// angles, and power injections.
    ///
    /// Example::
    ///
    ///     for b in sol.buses:
    ///         print(b.number, b.vm_pu, b.va_deg, b.p_inject_mw)
    ///
    ///     low_v = [b for b in sol.buses if b.vm_pu < 0.95]
    #[getter]
    fn buses(&self) -> PyResult<Vec<rich_objects::BusSolved>> {
        Ok(rich_objects::buses_solved(
            &self.inner,
            self.attached_net()?,
        ))
    }

    /// All branches with solved power flow results.
    ///
    /// Returns a list of `BranchSolved` objects with from-end and to-end
    /// flows (MW, MVAr), loading percentage, and line losses.
    ///
    /// Example::
    ///
    ///     overloaded = [b for b in sol.branches if b.loading_pct > 90]
    #[getter]
    fn branches(&self) -> PyResult<Vec<rich_objects::BranchSolved>> {
        Ok(rich_objects::branches_solved(
            &self.inner,
            self.attached_net()?,
        ))
    }

    /// All generators with solved reactive power outputs.
    ///
    /// Returns a list of `GenSolved` objects combining generator model data
    /// with the post-solve reactive power from the `gen_q_mvar` computation.
    ///
    /// Example::
    ///
    ///     for g in sol.generators:
    ///         print(g.machine_id, g.p_mw, g.q_mvar_solved)
    #[getter]
    fn generators(&self) -> PyResult<Vec<rich_objects::GenSolved>> {
        Ok(rich_objects::generators_solved(
            &self.inner,
            self.attached_net()?,
        ))
    }

    /// Look up a single bus's solved state by external bus number.
    fn bus(&self, number: u32) -> PyResult<rich_objects::BusSolved> {
        rich_objects::bus_solved(&self.inner, self.attached_net()?, number)
    }

    // ─── Phase F: Solution query helpers ──────────────────────────────────

    /// Buses with voltage outside limits [vmin, vmax] (pu).
    fn violated_buses(&self, vmin: f64, vmax: f64) -> PyResult<Vec<rich_objects::BusSolved>> {
        Ok(
            rich_objects::buses_solved(&self.inner, self.attached_net()?)
                .into_iter()
                .filter(|b| b.vm_pu < vmin || b.vm_pu > vmax)
                .collect(),
        )
    }
    /// Branches loaded above threshold percentage.
    fn overloaded_branches(&self, threshold_pct: f64) -> PyResult<Vec<rich_objects::BranchSolved>> {
        Ok(
            rich_objects::branches_solved(&self.inner, self.attached_net()?)
                .into_iter()
                .filter(|b| b.loading_pct > threshold_pct)
                .collect(),
        )
    }

    /// Validate the power flow solution for consistency.
    ///
    /// Checks:
    ///   - Solver converged (converged == True)
    ///   - vm and va arrays are non-empty and have the same length
    ///   - All vm and va values are finite (no NaN/Inf)
    ///
    /// Returns True if all checks pass. Raises ValueError with details on
    /// the first failure found.
    fn validate(&self) -> PyResult<bool> {
        if !self.converged() {
            return Err(ConvergenceError::new_err(format!(
                "AcPfResult did not converge (max_mismatch={:.3e} pu after {} iterations)",
                self.inner.max_mismatch, self.inner.iterations
            )));
        }
        let n_vm = self.inner.voltage_magnitude_pu.len();
        let n_va = self.inner.voltage_angle_rad.len();
        if n_vm == 0 {
            return Err(PyValueError::new_err("AcPfResult: vm array is empty"));
        }
        if n_vm != n_va {
            return Err(PyValueError::new_err(format!(
                "AcPfResult: vm length ({}) != va length ({})",
                n_vm, n_va
            )));
        }
        for (i, &v) in self.inner.voltage_magnitude_pu.iter().enumerate() {
            if !v.is_finite() {
                return Err(PyValueError::new_err(format!(
                    "AcPfResult: vm[{}] is non-finite ({})",
                    i, v
                )));
            }
        }
        for (i, &a) in self.inner.voltage_angle_rad.iter().enumerate() {
            if !a.is_finite() {
                return Err(PyValueError::new_err(format!(
                    "AcPfResult: va[{}] is non-finite ({})",
                    i, a
                )));
            }
        }
        Ok(true)
    }

    /// Serialise the power flow solution to a JSON string.
    ///
    /// The JSON includes all solver metadata (status, iterations, mismatch,
    /// solve_time_secs) and all per-bus arrays (vm, va, p_inject, q_inject,
    /// bus_numbers).  Reload with ``AcPfResult.from_json()``.
    ///
    /// Returns:
    ///     str: JSON representation of the solution.
    fn to_json(&self) -> PyResult<String> {
        serde_json::to_string_pretty(&self.inner)
            .map_err(|e| SurgeError::new_err(format!("Failed to serialize to JSON: {e}")))
    }

    /// Deserialise a power flow solution from a JSON string produced by ``to_json()``.
    ///
    /// Returns:
    ///     AcPfResult: reconstructed solution.  Note: the companion
    ///     ``Network`` reference is not stored in the JSON, so methods that
    ///     require network topology (e.g., ``branch_loading_pct``,
    ///     ``get_buses``) will not be available on the deserialized object.
    #[staticmethod]
    fn from_json(s: &str) -> PyResult<AcPfResult> {
        let inner: surge_solution::PfSolution = serde_json::from_str(s)
            .map_err(|e| PyValueError::new_err(format!("Failed to parse JSON: {e}")))?;
        Ok(AcPfResult { inner, net: None })
    }

    /// Return the solution as a Python dictionary.
    ///
    /// Keys: ``status``, ``converged``, ``iterations``, ``max_mismatch``,
    /// ``solve_time_secs``, ``vm``, ``va_deg``, ``bus_numbers``.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("status", self.status())?;
        d.set_item("converged", self.converged())?;
        d.set_item("iterations", self.inner.iterations)?;
        d.set_item("max_mismatch", self.inner.max_mismatch)?;
        d.set_item("solve_time_secs", self.inner.solve_time_secs)?;
        d.set_item("vm", self.inner.voltage_magnitude_pu.clone())?;
        d.set_item("va_rad", self.inner.voltage_angle_rad.clone())?;
        let va_deg: Vec<f64> = self
            .inner
            .voltage_angle_rad
            .iter()
            .map(|&a| a.to_degrees())
            .collect();
        d.set_item("va_deg", va_deg)?;
        d.set_item("bus_numbers", self.inner.bus_numbers.clone())?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "AcPfResult(converged={}, iterations={}, mismatch={:.2e}, time={:.3}ms)",
            self.converged(),
            self.inner.iterations,
            self.inner.max_mismatch,
            self.inner.solve_time_secs * 1000.0
        )
    }
}

impl AcPfResult {
    fn attached_net(&self) -> PyResult<&Arc<surge_network::Network>> {
        self.net.as_ref().ok_or_else(|| {
            PyValueError::new_err(
                "this result is detached from its network; call attach_network(network) first",
            )
        })
    }
}

// ---------------------------------------------------------------------------
// DC power flow solution wrapper
// ---------------------------------------------------------------------------

/// DC power flow solution.
#[pyclass(skip_from_py_object, name = "DcPfResult")]
#[derive(Clone)]
pub struct DcPfResult {
    pub(crate) va_rad: Vec<f64>,
    pub(crate) branch_p_mw: Vec<f64>,
    pub(crate) slack_p_mw: f64,
    pub(crate) solve_time_secs: f64,
    pub(crate) total_generation_mw: f64,
    pub(crate) slack_distribution_mw: HashMap<u32, f64>,
    pub(crate) bus_p_inject_mw: Vec<f64>,
    pub(crate) bus_numbers: Vec<u32>,
    pub(crate) branch_from: Vec<u32>,
    pub(crate) branch_to: Vec<u32>,
    pub(crate) branch_circuit: Vec<String>,
    pub(crate) net: Arc<surge_network::Network>,
}

#[pymethods]
impl DcPfResult {
    /// Bus voltage angles in radians as numpy array.
    #[getter]
    fn va_rad<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.va_rad.clone().into_pyarray(py)
    }

    /// Bus voltage angles in degrees as numpy array.
    #[getter]
    fn va_deg<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.va_rad
            .iter()
            .map(|angle| angle.to_degrees())
            .collect::<Vec<_>>()
            .into_pyarray(py)
    }

    /// Branch active power flows (MW) as numpy array.
    #[getter]
    fn branch_p_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.branch_p_mw.clone().into_pyarray(py)
    }

    /// Slack bus real power injection (MW).
    #[getter]
    fn slack_p_mw(&self) -> f64 {
        self.slack_p_mw
    }

    /// Solve time in seconds.
    #[getter]
    fn solve_time_secs(&self) -> f64 {
        self.solve_time_secs
    }

    /// Total system generation after slack balancing (MW).
    #[getter]
    fn total_generation_mw(&self) -> f64 {
        self.total_generation_mw
    }

    /// Per-bus slack distribution (bus number → MW share).
    ///
    /// Non-empty only when headroom slack is used. Maps each participating
    /// bus number to its share of the slack power absorption in MW.
    #[getter]
    fn slack_distribution_mw(&self) -> HashMap<u32, f64> {
        self.slack_distribution_mw.clone()
    }

    /// Net real power injection at each bus (MW).
    ///
    /// Includes generation minus load, after slack balancing. Same bus order
    /// as ``va_rad``.
    #[getter]
    fn bus_p_inject_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.bus_p_inject_mw.clone().into_pyarray(py)
    }

    /// External bus numbers in the same order as ``va_rad``.
    #[getter]
    fn bus_numbers(&self) -> Vec<u32> {
        self.bus_numbers.clone()
    }

    /// External from-bus numbers in branch order.
    #[getter]
    fn branch_from(&self) -> Vec<u32> {
        self.branch_from.clone()
    }

    /// External to-bus numbers in branch order.
    #[getter]
    fn branch_to(&self) -> Vec<u32> {
        self.branch_to.clone()
    }

    /// Circuit identifiers in branch order.
    #[getter]
    fn branch_circuit(&self) -> Vec<String> {
        self.branch_circuit.clone()
    }

    /// Stable branch keys in branch order.
    #[getter]
    fn branch_keys(&self) -> Vec<(u32, u32, String)> {
        self.branch_from
            .iter()
            .zip(self.branch_to.iter())
            .zip(self.branch_circuit.iter())
            .map(|((&from_bus, &to_bus), circuit)| (from_bus, to_bus, circuit.clone()))
            .collect()
    }

    /// Return a pandas DataFrame (or dict if pandas is not installed).
    ///
    /// Columns: bus_id, va_rad, va_deg.
    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        dict.set_item("bus_id", self.bus_numbers.clone())?;
        dict.set_item("va_rad", self.va_rad.clone())?;
        let va_deg: Vec<f64> = self.va_rad.iter().map(|&a| a.to_degrees()).collect();
        dict.set_item("va_deg", va_deg)?;
        dict_to_dataframe_with_index(py, dict, &["bus_id"])
    }

    /// Return a branch DataFrame (or dict).
    ///
    /// Columns: from_bus, to_bus, circuit, p_mw.
    fn branch_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        dict.set_item("from_bus", self.branch_from.clone())?;
        dict.set_item("to_bus", self.branch_to.clone())?;
        dict.set_item("circuit", self.branch_circuit.clone())?;
        dict.set_item("p_mw", self.branch_p_mw.clone())?;
        dict_to_dataframe_with_index(py, dict, &["from_bus", "to_bus", "circuit"])
    }

    /// Return a list of `BusDcSolved` objects with DC power flow results.
    #[getter]
    fn buses(&self) -> Vec<rich_objects::BusDcSolved> {
        rich_objects::buses_dc_solved(&self.va_rad, &self.bus_numbers, &self.net)
    }

    /// Return a list of `BranchDcSolved` objects with DC power flow results.
    #[getter]
    fn branches(&self) -> Vec<rich_objects::BranchDcSolved> {
        rich_objects::branches_dc_solved(&self.branch_p_mw, &self.net)
    }

    /// Return the DC power flow solution as a JSON-serializable dictionary.
    ///
    /// Keys: ``solve_time_secs``, ``slack_p_mw``, ``total_generation_mw``,
    /// ``va_rad``, ``va_deg``, ``branch_p_mw``, ``bus_p_inject_mw``,
    /// ``bus_numbers``, ``branch_from``, ``branch_to``, ``branch_circuit``,
    /// ``slack_distribution_mw``.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("solve_time_secs", self.solve_time_secs)?;
        d.set_item("slack_p_mw", self.slack_p_mw)?;
        d.set_item("total_generation_mw", self.total_generation_mw)?;
        d.set_item("va_rad", self.va_rad.clone())?;
        let va_deg: Vec<f64> = self.va_rad.iter().map(|&a| a.to_degrees()).collect();
        d.set_item("va_deg", va_deg)?;
        d.set_item("branch_p_mw", self.branch_p_mw.clone())?;
        d.set_item("bus_p_inject_mw", self.bus_p_inject_mw.clone())?;
        d.set_item("bus_numbers", self.bus_numbers.clone())?;
        d.set_item("branch_from", self.branch_from.clone())?;
        d.set_item("branch_to", self.branch_to.clone())?;
        d.set_item("branch_circuit", self.branch_circuit.clone())?;
        d.set_item("slack_distribution_mw", self.slack_distribution_mw.clone())?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "DcPfResult(slack_p_mw={:.2}, time={:.3}ms, buses={}, branches={})",
            self.slack_p_mw,
            self.solve_time_secs * 1000.0,
            self.bus_numbers.len(),
            self.branch_p_mw.len()
        )
    }
}

pub(crate) fn dc_pf_result_from_result(
    net: Arc<surge_network::Network>,
    result: surge_dc::DcPfSolution,
) -> DcPfResult {
    let base_mva = net.base_mva;
    let bus_numbers: Vec<u32> = net.buses.iter().map(|b| b.number).collect();
    let branch_from: Vec<u32> = net.branches.iter().map(|b| b.from_bus).collect();
    let branch_to: Vec<u32> = net.branches.iter().map(|b| b.to_bus).collect();
    let branch_circuit: Vec<String> = net.branches.iter().map(|b| b.circuit.clone()).collect();
    let branch_p_mw: Vec<f64> = result.branch_p_flow.iter().map(|&v| v * base_mva).collect();
    let bus_p_inject_mw: Vec<f64> = result.p_inject_pu.iter().map(|&v| v * base_mva).collect();

    // Convert slack_distribution from internal bus index → bus number.
    let slack_distribution_mw: HashMap<u32, f64> = result
        .slack_distribution
        .iter()
        .map(|(&bus_idx, &share_mw)| (net.buses[bus_idx].number, share_mw))
        .collect();

    DcPfResult {
        va_rad: result.theta,
        branch_p_mw,
        slack_p_mw: result.slack_p_injection * base_mva,
        solve_time_secs: result.solve_time_secs,
        total_generation_mw: result.total_generation_mw,
        slack_distribution_mw,
        bus_p_inject_mw,
        bus_numbers,
        branch_from,
        branch_to,
        branch_circuit,
        net,
    }
}

// ---------------------------------------------------------------------------
// OPF solution wrapper
// ---------------------------------------------------------------------------

/// Optimal power flow solution.
#[pyclass(name = "OpfResult", from_py_object)]
#[derive(Clone)]
pub struct OpfSolution {
    pub(crate) inner: surge_solution::OpfSolution,
    /// The network this solution was computed from.
    pub(crate) net: Option<Arc<surge_network::Network>>,
    /// External bus numbers in the same order as `lmp` / `vm` / `va` vectors.
    /// Extracted from `inner.power_flow.bus_numbers` at solve time so that
    /// `to_dataframe()` can return external bus IDs without needing the Network.
    pub(crate) bus_numbers: Vec<u32>,
    /// External bus numbers for each generator (one per entry in gen_p_mw).
    pub(crate) gen_bus_numbers: Vec<u32>,
    /// Canonical generator IDs for each generator (one per entry in gen_p_mw).
    pub(crate) gen_ids: Vec<String>,
    /// PSS/E machine_id for each generator (one per entry in gen_p_mw).
    pub(crate) gen_machine_ids: Vec<String>,
}

#[pymethods]
impl OpfSolution {
    fn attach_network(&mut self, network: &Network) -> PyResult<()> {
        let net = Arc::clone(&network.inner);
        if !self.bus_numbers.is_empty() && self.bus_numbers.len() != net.buses.len() {
            return Err(PyValueError::new_err(format!(
                "network has {} buses but solution has {} bus entries",
                net.buses.len(),
                self.bus_numbers.len()
            )));
        }
        let n_in_service_generators = net.generators.iter().filter(|g| g.in_service).count();
        if !self.inner.generators.gen_p_mw.is_empty()
            && self.inner.generators.gen_p_mw.len() != n_in_service_generators
        {
            return Err(PyValueError::new_err(format!(
                "network has {} in-service generators but solution has {} generator dispatch entries",
                n_in_service_generators,
                self.inner.generators.gen_p_mw.len()
            )));
        }
        self.net = Some(Arc::clone(&net));
        let bus_numbers: Vec<u32> = net.buses.iter().map(|b| b.number).collect();
        let gen_bus_numbers: Vec<u32> = net
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.bus)
            .collect();
        let gen_machine_ids: Vec<String> = net
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.machine_id.clone().unwrap_or_else(|| "1".to_string()))
            .collect();
        let gen_ids: Vec<String> = net
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.id.clone())
            .collect();

        self.bus_numbers = bus_numbers.clone();
        self.gen_bus_numbers = gen_bus_numbers.clone();
        self.gen_machine_ids = gen_machine_ids.clone();
        self.gen_ids = gen_ids.clone();
        self.inner.power_flow.bus_numbers = bus_numbers;
        self.inner.generators.gen_bus_numbers = gen_bus_numbers;
        self.inner.generators.gen_machine_ids = gen_machine_ids;
        self.inner.generators.gen_ids = gen_ids;
        Ok(())
    }

    #[getter]
    fn has_attached_network(&self) -> bool {
        self.net.is_some()
    }

    /// Total cost ($/hr).
    #[getter]
    fn total_cost(&self) -> f64 {
        self.inner.total_cost
    }

    /// OPF formulation type.
    #[getter]
    fn opf_type(&self) -> &'static str {
        match self.inner.opf_type {
            surge_solution::OpfType::DcOpf => "dc_opf",
            surge_solution::OpfType::AcOpf => "ac_opf",
            surge_solution::OpfType::DcScopf => "dc_scopf",
            surge_solution::OpfType::AcScopf => "ac_scopf",
            surge_solution::OpfType::HvdcOpf => "hvdc_opf",
        }
    }

    /// System MVA base used by the solve.
    #[getter]
    fn base_mva(&self) -> f64 {
        self.inner.base_mva
    }

    /// Whether the solver converged successfully.
    ///
    /// Always ``True`` — if the solver fails to converge, a Python exception
    /// is raised and no ``OpfResult`` object is created.
    #[getter]
    fn converged(&self) -> bool {
        true
    }

    /// Solve time in seconds.
    #[getter]
    fn solve_time_secs(&self) -> f64 {
        self.inner.solve_time_secs
    }

    /// Solver iterations.
    #[getter]
    fn iterations(&self) -> Option<u32> {
        self.inner.iterations
    }

    /// Name of the LP/NLP solver used (e.g. ``"HiGHS"``, ``"Gurobi"``, ``"Ipopt"``).
    /// Returns ``None`` when the solver identity was not recorded.
    #[getter]
    fn solver_name(&self) -> Option<&str> {
        self.inner.solver_name.as_deref()
    }

    /// Version string of the solver (e.g. ``"1.13.1"`` for HiGHS).
    /// Returns ``None`` when the version was not recorded.
    #[getter]
    fn solver_version(&self) -> Option<&str> {
        self.inner.solver_version.as_deref()
    }

    /// Generator active power dispatch (MW) as numpy array.
    #[getter]
    fn gen_p_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.generators.gen_p_mw.clone().into_pyarray(py)
    }

    /// External bus number for each in-service generator in `gen_p_mw` order.
    #[getter]
    fn gen_bus_numbers(&self) -> Vec<u32> {
        self.gen_bus_numbers.clone()
    }

    /// Canonical generator ID for each in-service generator in `gen_p_mw` order.
    #[getter]
    fn gen_ids(&self) -> Vec<String> {
        self.gen_ids.clone()
    }

    /// Machine ID for each in-service generator in `gen_p_mw` order.
    #[getter]
    fn gen_machine_ids(&self) -> Vec<String> {
        self.gen_machine_ids.clone()
    }

    /// Locational marginal prices ($/MWh) as numpy array.
    #[getter]
    fn lmp<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.pricing.lmp.clone().into_pyarray(py)
    }

    /// LMP congestion component ($/MWh) as numpy array.
    #[getter]
    fn lmp_congestion<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.pricing.lmp_congestion.clone().into_pyarray(py)
    }

    /// LMP loss component ($/MWh) as numpy array.
    #[getter]
    fn lmp_loss<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.pricing.lmp_loss.clone().into_pyarray(py)
    }

    /// Energy component of LMP ($/MWh) as numpy array.
    #[getter]
    fn lmp_energy<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.pricing.lmp_energy.clone().into_pyarray(py)
    }

    /// Reactive LMP ($/MVAr-h) as numpy array.
    #[getter]
    fn lmp_reactive<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.pricing.lmp_reactive.clone().into_pyarray(py)
    }

    /// Return a pandas DataFrame (or dict if pandas is not installed).
    ///
    /// Columns: bus_id (external bus number), lmp, lmp_congestion, lmp_loss.
    ///
    /// `bus_id` uses **external bus numbers** (e.g. 1, 2, 3, 30 for IEEE-30)
    /// matching the bus numbering in the original case file — NOT zero-based
    /// internal indices.  This is consistent with `AcPfResult.to_dataframe()`.
    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        dict.set_item("bus_id", self.bus_numbers.clone())?;
        dict.set_item("lmp", self.inner.pricing.lmp.clone())?;
        dict.set_item("lmp_congestion", self.inner.pricing.lmp_congestion.clone())?;
        dict.set_item("lmp_loss", self.inner.pricing.lmp_loss.clone())?;
        dict_to_dataframe_with_index(py, dict, &["bus_id"])
    }

    /// Return a pandas DataFrame of generator dispatch (or dict if pandas is not installed).
    ///
    /// Index: MultiIndex (generator_id, bus_id, machine_id).
    /// Columns: gen_idx, gen_p_mw.
    fn to_gen_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let n = self.gen_bus_numbers.len();
        let dict = PyDict::new(py);
        dict.set_item("generator_id", self.gen_ids.clone())?;
        dict.set_item("bus_id", self.gen_bus_numbers.clone())?;
        dict.set_item("machine_id", self.gen_machine_ids.clone())?;
        dict.set_item("gen_idx", (0..n).collect::<Vec<usize>>())?;
        dict.set_item("gen_p_mw", self.inner.generators.gen_p_mw.clone())?;
        dict_to_dataframe_with_index(py, dict, &["generator_id", "bus_id", "machine_id"])
    }

    /// Bus voltage magnitudes (p.u.) as numpy array.
    ///
    /// DC-OPF: flat (all 1.0) — no voltage variables in DC formulation.
    /// AC-OPF: optimal voltages from the NLP solution.
    #[getter]
    fn vm<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .power_flow
            .voltage_magnitude_pu
            .clone()
            .into_pyarray(py)
    }

    /// Bus voltage angles in **radians** as numpy array.
    ///
    /// DC-OPF: optimal angles from the B-theta formulation.
    /// AC-OPF: optimal angles from the NLP solution.
    #[getter]
    fn va_rad<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .power_flow
            .voltage_angle_rad
            .clone()
            .into_pyarray(py)
    }

    /// Branch shadow prices as numpy array.
    #[getter]
    fn branch_shadow_prices<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .branches
            .branch_shadow_prices
            .clone()
            .into_pyarray(py)
    }

    /// From-end active power flow per branch (MW).
    #[getter]
    fn branch_pf_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .power_flow
            .branch_p_from_mw
            .clone()
            .into_pyarray(py)
    }

    /// To-end active power flow per branch (MW).
    #[getter]
    fn branch_pt_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .power_flow
            .branch_p_to_mw
            .clone()
            .into_pyarray(py)
    }

    /// From-end reactive power flow per branch (MVAr).
    #[getter]
    fn branch_qf_mvar<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .power_flow
            .branch_q_from_mvar
            .clone()
            .into_pyarray(py)
    }

    /// To-end reactive power flow per branch (MVAr).
    #[getter]
    fn branch_qt_mvar<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .power_flow
            .branch_q_to_mvar
            .clone()
            .into_pyarray(py)
    }

    /// Branch loading as a percentage of rate A.
    #[getter]
    fn branch_loading_pct<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .branches
            .branch_loading_pct
            .clone()
            .into_pyarray(py)
    }

    /// Indices of branches with binding thermal constraints.
    #[getter]
    fn binding_branch_indices(&self) -> Vec<usize> {
        self.inner.branches.binding_branch_indices()
    }

    /// Flowgate shadow prices as a dict mapping flowgate name → $/MWh.
    ///
    /// Keys are base-case flowgate names (``contingency_branch = None``) in
    /// the same order as the network's ``flowgates`` list.  A non-zero value
    /// indicates that flowgate was binding at the OPF/SCED optimum.
    /// Returns an empty dict for AC-OPF or when no flowgates are defined.
    #[getter]
    fn flowgate_shadow_prices(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let d = PyDict::new(py);
        let net = self.attached_net()?;
        let prices = &self.inner.branches.flowgate_shadow_prices;
        let base_case_fg: Vec<_> = net
            .flowgates
            .iter()
            .filter(|fg| fg.in_service && fg.contingency_branch.is_none())
            .collect();
        for (fg, &price) in base_case_fg.iter().zip(prices.iter()) {
            d.set_item(&fg.name, price)?;
        }
        // If price vec is longer than named flowgates (e.g. unnamed entries),
        // add them with a synthetic key.
        for (i, &price) in prices.iter().enumerate().skip(base_case_fg.len()) {
            d.set_item(format!("flowgate_{i}"), price)?;
        }
        Ok(d.into())
    }

    /// Interface / area-interchange shadow prices as a dict mapping name → $/MWh.
    ///
    /// Keys are interface names in the same order as the network's ``interfaces``
    /// list.  A non-zero value indicates that interface limit was binding at the
    /// OPF/SCED optimum.  Returns an empty dict for AC-OPF or when no interfaces
    /// are defined.
    #[getter]
    fn interface_shadow_prices(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let d = PyDict::new(py);
        let net = self.attached_net()?;
        let prices = &self.inner.branches.interface_shadow_prices;
        let interfaces: Vec<_> = net.interfaces.iter().filter(|i| i.in_service).collect();
        for (iface, &price) in interfaces.iter().zip(prices.iter()) {
            d.set_item(&iface.name, price)?;
        }
        for (i, &price) in prices.iter().enumerate().skip(interfaces.len()) {
            d.set_item(format!("interface_{i}"), price)?;
        }
        Ok(d.into())
    }

    /// Generator reactive power dispatch (MVAr) as numpy array.
    ///
    /// AC-OPF: optimal reactive power from the NLP solution.
    /// DC-OPF: empty array (no reactive variables in DC formulation).
    #[getter]
    fn gen_q_mvar<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.generators.gen_q_mvar.clone().into_pyarray(py)
    }

    /// Lower reactive-power bound duals ($/MVAr-h), one per in-service generator.
    #[getter]
    fn mu_qg_min<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .generators
            .shadow_price_qg_min
            .clone()
            .into_pyarray(py)
    }

    /// Upper reactive-power bound duals ($/MVAr-h), one per in-service generator.
    #[getter]
    fn mu_qg_max<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .generators
            .shadow_price_qg_max
            .clone()
            .into_pyarray(py)
    }

    /// Lower voltage-magnitude bound duals, one per bus.
    #[getter]
    fn mu_vm_min<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .branches
            .shadow_price_vm_min
            .clone()
            .into_pyarray(py)
    }

    /// Upper voltage-magnitude bound duals, one per bus.
    #[getter]
    fn mu_vm_max<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .branches
            .shadow_price_vm_max
            .clone()
            .into_pyarray(py)
    }

    /// Lower branch angle-difference bound duals, one per branch.
    #[getter]
    fn mu_angmin<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .branches
            .shadow_price_angmin
            .clone()
            .into_pyarray(py)
    }

    /// Upper branch angle-difference bound duals, one per branch.
    #[getter]
    fn mu_angmax<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .branches
            .shadow_price_angmax
            .clone()
            .into_pyarray(py)
    }

    /// Total system load (MW).
    #[getter]
    fn total_load_mw(&self) -> f64 {
        self.inner.total_load_mw
    }

    /// Total generation (MW) — sum of all in-service generator dispatches.
    #[getter]
    fn total_generation_mw(&self) -> f64 {
        self.inner.total_generation_mw
    }

    /// Total system losses (MW) — total_generation_mw minus total_load_mw.
    ///
    /// Zero for DC-OPF (lossless). Non-zero for AC-OPF.
    #[getter]
    fn total_losses_mw(&self) -> f64 {
        self.inner.total_losses_mw
    }

    /// Lower active-power bound duals ($/MWh), one per in-service generator.
    ///
    /// mu_pg_min[j] > 0 means generator j is at its pmin limit.
    /// Empty for DC-OPF (column duals not extracted by default).
    #[getter]
    fn mu_pg_min<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .generators
            .shadow_price_pg_min
            .clone()
            .into_pyarray(py)
    }

    /// Upper active-power bound duals ($/MWh), one per in-service generator.
    ///
    /// mu_pg_max[j] > 0 means generator j is at its pmax limit.
    #[getter]
    fn mu_pg_max<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .generators
            .shadow_price_pg_max
            .clone()
            .into_pyarray(py)
    }

    /// Bus voltage angles in **degrees** as numpy array.
    #[getter]
    fn va_deg<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner
            .power_flow
            .voltage_angle_rad
            .iter()
            .map(|v| v.to_degrees())
            .collect::<Vec<_>>()
            .into_pyarray(py)
    }

    // ───────────────────────────────────────────────────────────────────────
    // Rich object accessors
    // ───────────────────────────────────────────────────────────────────────

    /// All buses with OPF results (LMPs, voltage, shadow prices).
    ///
    /// Returns a list of `BusOpf` objects, one per bus, each combining static
    /// network model data with LMPs (lmp, lmp_energy, lmp_congestion, lmp_loss),
    /// solved voltages, and voltage constraint shadow prices.
    ///
    /// Example::
    ///
    ///     for b in result.buses:
    ///         print(b.number, b.lmp, b.lmp_congestion)
    ///
    ///     congested_buses = [b for b in result.buses if abs(b.lmp_congestion) > 1.0]
    #[getter]
    fn buses(&self) -> PyResult<Vec<rich_objects::BusOpf>> {
        Ok(rich_objects::buses_opf(&self.inner, self.attached_net()?))
    }

    /// All branches with OPF flow results and shadow prices.
    ///
    /// Returns a list of `BranchOpf` objects with power flows, loading percentage,
    /// thermal constraint shadow prices, and angle constraint multipliers.
    ///
    /// Example::
    ///
    ///     binding = [b for b in result.branches if b.is_binding]
    ///     for b in binding:
    ///         print(f"Branch {b.from_bus}→{b.to_bus}: μ={b.shadow_price:.2f} $/MWh/MW")
    #[getter]
    fn branches(&self) -> PyResult<Vec<rich_objects::BranchOpf>> {
        Ok(rich_objects::branches_opf(
            &self.inner,
            self.attached_net()?,
        ))
    }

    /// All generators with OPF dispatch and KKT multipliers.
    ///
    /// Returns a list of `GenOpf` objects with optimal dispatch (`p_mw`, `q_mvar`),
    /// active/reactive power bound shadow prices, and actual dispatch cost.
    ///
    /// Example::
    ///
    ///     for g in result.generators:
    ///         print(g.machine_id, g.p_mw, g.cost_actual, g.mu_pmax)
    ///
    ///     at_limit = [g for g in result.generators if g.at_pmax or g.at_pmin]
    #[getter]
    fn generators(&self) -> PyResult<Vec<rich_objects::GenOpf>> {
        Ok(rich_objects::generators_opf(
            &self.inner,
            self.attached_net()?,
        ))
    }

    /// LMP DataFrame including bus names.
    ///
    /// Columns: bus_name, lmp, lmp_energy, lmp_congestion, lmp_loss.
    fn lmp_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let net = self.attached_net()?;
        let dict = PyDict::new(py);
        let bus_ids: Vec<u32> = net.buses.iter().map(|b| b.number).collect();
        let bus_names: Vec<String> = net.buses.iter().map(|b| b.name.clone()).collect();
        dict.set_item("bus_id", bus_ids)?;
        dict.set_item("bus_name", bus_names)?;
        dict.set_item("lmp", self.inner.pricing.lmp.clone())?;
        dict.set_item("lmp_energy", self.inner.pricing.lmp_energy.clone())?;
        dict.set_item("lmp_congestion", self.inner.pricing.lmp_congestion.clone())?;
        dict.set_item("lmp_loss", self.inner.pricing.lmp_loss.clone())?;
        if !self.inner.pricing.lmp_reactive.is_empty() {
            dict.set_item("lmp_reactive", self.inner.pricing.lmp_reactive.clone())?;
        }
        dict_to_dataframe_with_index(py, dict, &["bus_id"])
    }

    // ─── Phase F: OPF solution query helpers ─────────────────────────────

    /// Branches with binding thermal constraints (|shadow_price| >= threshold).
    fn binding_branches(&self, threshold: f64) -> PyResult<Vec<rich_objects::BranchOpf>> {
        Ok(
            rich_objects::branches_opf(&self.inner, self.attached_net()?)
                .into_iter()
                .filter(|b| b.shadow_price.abs() >= threshold)
                .collect(),
        )
    }
    /// Buses with significant congestion LMP component.
    fn congested_buses(&self, threshold: f64) -> PyResult<Vec<rich_objects::BusOpf>> {
        Ok(rich_objects::buses_opf(&self.inner, self.attached_net()?)
            .into_iter()
            .filter(|b| b.lmp_congestion.abs() >= threshold)
            .collect())
    }
    /// Switched shunts with OPF dispatch results.
    fn switched_shunts(&self) -> PyResult<Vec<rich_objects::SwitchedShuntOpf>> {
        let network = self.attached_net()?;
        let base = network.base_mva;
        Ok(self
            .inner
            .devices
            .switched_shunt_dispatch
            .iter()
            .enumerate()
            .filter_map(|(i, &(_, b_cont, b_round))| {
                network.controls.switched_shunts_opf.get(i).map(|ss| {
                    rich_objects::SwitchedShuntOpf::from_core(
                        ss,
                        b_cont,
                        b_round,
                        &network.buses,
                        base,
                    )
                })
            })
            .collect())
    }

    /// Transformer tap dispatch from AC-OPF.
    ///
    /// Returns:
    ///     list[tuple[int, float, float]]: Each tuple is (branch_idx, continuous_tap, rounded_tap).
    #[getter]
    fn tap_dispatch(&self) -> Vec<(usize, f64, f64)> {
        self.inner.devices.tap_dispatch.clone()
    }

    /// Phase-shifter dispatch from AC-OPF.
    ///
    /// Returns:
    ///     list[tuple[int, float, float]]: Each tuple is (branch_idx, continuous_rad, rounded_rad).
    #[getter]
    fn phase_dispatch(&self) -> Vec<(usize, f64, f64)> {
        self.inner.devices.phase_dispatch.clone()
    }

    /// SVC dispatch from AC-OPF with ``optimize_svc=True``.
    ///
    /// Returns:
    ///     list[tuple[int, float, float, float]]: Each tuple is
    ///     (bus_idx, b_svc_pu, q_inject_mvar, v_bus_pu).
    ///     Empty when ``optimize_svc`` is disabled.
    #[getter]
    fn svc_dispatch(&self) -> Vec<(usize, f64, f64, f64)> {
        self.inner.devices.svc_dispatch.clone()
    }

    /// TCSC dispatch from AC-OPF with ``optimize_tcsc=True``.
    ///
    /// Returns:
    ///     list[tuple[int, float, float, float]]: Each tuple is
    ///     (branch_idx, x_comp_pu, x_eff_pu, p_flow_mw).
    ///     Empty when ``optimize_tcsc`` is disabled.
    #[getter]
    fn tcsc_dispatch(&self) -> Vec<(usize, f64, f64, f64)> {
        self.inner.devices.tcsc_dispatch.clone()
    }

    /// Whether the discrete round-and-check verification passed.
    ///
    /// Returns:
    ///     bool | None: None if continuous mode, True if feasible, False if violations.
    #[getter]
    fn discrete_feasible(&self) -> Option<bool> {
        self.inner.devices.discrete_feasible
    }

    /// Violation descriptions from discrete round-and-check.
    ///
    /// Returns:
    ///     list[str]: Human-readable violation descriptions (empty if feasible).
    #[getter]
    fn discrete_violations(&self) -> Vec<String> {
        self.inner.devices.discrete_violations.clone()
    }

    /// Persisted exact-audit block for the OPF solution payload.
    #[getter]
    fn audit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let json = serde_json::to_string(&self.inner.audit)
            .map_err(|e| SurgeError::new_err(format!("Failed to serialize audit block: {e}")))?;
        py.import("json")?.call_method1("loads", (json,))
    }

    /// Serialise the OPF solution to a JSON string.
    ///
    /// Includes all dispatch, LMP, and power-flow sub-solution fields.
    /// Reload with ``OpfResult.from_json()``.
    ///
    /// Returns:
    ///     str: JSON representation of the OPF solution.
    fn to_json(&self) -> PyResult<String> {
        let json = if self.inner.has_objective_ledger() {
            surge_io::json::encode_checked_audited_solution(&self.inner)
        } else {
            surge_io::json::encode_audited_solution(&self.inner)
        }
        .map_err(|e| SurgeError::new_err(format!("Failed to serialize to JSON: {e}")))?;
        serde_json::to_string_pretty(&json)
            .map_err(|e| SurgeError::new_err(format!("Failed to serialize to JSON: {e}")))
    }

    /// Deserialise an OPF solution from a JSON string produced by ``to_json()``.
    ///
    /// Returns:
    ///     OpfResult: reconstructed solution.  Network-topology-dependent
    ///     methods (e.g., ``get_buses``) require re-attaching the network.
    #[staticmethod]
    fn from_json(s: &str) -> PyResult<OpfSolution> {
        let inner: surge_solution::OpfSolution = serde_json::from_str(s)
            .map_err(|e| PyValueError::new_err(format!("Failed to parse JSON: {e}")))?;
        let bus_numbers = inner.power_flow.bus_numbers.clone();
        let gen_bus_numbers = inner.generators.gen_bus_numbers.clone();
        let gen_ids = inner.generators.gen_ids.clone();
        let gen_machine_ids = inner.generators.gen_machine_ids.clone();
        Ok(OpfSolution {
            inner,
            net: None,
            bus_numbers,
            gen_bus_numbers,
            gen_ids,
            gen_machine_ids,
        })
    }

    /// Return the OPF solution as a Python dictionary.
    ///
    /// Keys: ``opf_type``, ``total_cost``, ``iterations``, ``solve_time_secs``,
    /// ``gen_p_mw``, ``gen_q_mvar``, ``gen_bus_numbers``, ``lmp``,
    /// ``lmp_energy``, ``lmp_congestion``, ``lmp_loss``.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("opf_type", format!("{:?}", self.inner.opf_type))?;
        d.set_item("total_cost", self.inner.total_cost)?;
        d.set_item("iterations", self.inner.iterations)?;
        d.set_item("solve_time_secs", self.inner.solve_time_secs)?;
        d.set_item("gen_p_mw", self.inner.generators.gen_p_mw.clone())?;
        d.set_item("gen_q_mvar", self.inner.generators.gen_q_mvar.clone())?;
        d.set_item("gen_bus_numbers", self.gen_bus_numbers.clone())?;
        d.set_item("gen_ids", self.gen_ids.clone())?;
        d.set_item("gen_machine_ids", self.gen_machine_ids.clone())?;
        d.set_item("lmp", self.inner.pricing.lmp.clone())?;
        d.set_item("lmp_energy", self.inner.pricing.lmp_energy.clone())?;
        d.set_item("lmp_congestion", self.inner.pricing.lmp_congestion.clone())?;
        d.set_item("lmp_loss", self.inner.pricing.lmp_loss.clone())?;
        d.set_item(
            "branch_loading_pct",
            self.inner.branches.branch_loading_pct.clone(),
        )?;
        Ok(d)
    }

    /// Virtual bid clearing results (list of dicts with bus, direction, cleared_mw, price_per_mwh, lmp).
    ///
    /// Empty when no virtual bids were submitted.
    #[getter]
    fn virtual_bid_results(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .virtual_bid_results
            .iter()
            .map(|r| {
                let d = pyo3::types::PyDict::new(py);
                d.set_item("bus", r.bus)?;
                d.set_item("direction", format!("{:?}", r.direction).to_lowercase())?;
                d.set_item("cleared_mw", r.cleared_mw)?;
                d.set_item("price_per_mwh", r.price_per_mwh)?;
                d.set_item("lmp", r.lmp)?;
                Ok(d.into())
            })
            .collect()
    }

    /// Net storage dispatch (MW), positive for discharge and negative for charge.
    #[getter]
    fn storage_net_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.devices.storage_net_mw.clone().into_pyarray(py)
    }

    /// PAR implied-shift results as a list of dictionaries.
    #[getter]
    fn par_results(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .par_results
            .iter()
            .map(|pr| {
                let d = pyo3::types::PyDict::new(py);
                d.set_item("from_bus", pr.from_bus)?;
                d.set_item("to_bus", pr.to_bus)?;
                d.set_item("circuit", &pr.circuit)?;
                d.set_item("target_mw", pr.target_mw)?;
                d.set_item("implied_shift_deg", pr.implied_shift_deg)?;
                d.set_item("within_limits", pr.within_limits)?;
                Ok(d.into())
            })
            .collect()
    }

    /// Benders-cut duals from AC-SCOPF.
    #[getter]
    fn benders_cut_duals<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.inner.benders_cut_duals.clone().into_pyarray(py)
    }

    fn __repr__(&self) -> String {
        format!(
            "OpfResult(cost={:.2}, iterations={}, time={:.3}ms)",
            self.inner.total_cost,
            self.inner
                .iterations
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            self.inner.solve_time_secs * 1000.0
        )
    }
}

impl OpfSolution {
    pub(crate) fn from_core(
        inner: surge_solution::OpfSolution,
        net: Arc<surge_network::Network>,
    ) -> Self {
        let bus_numbers = if !inner.power_flow.bus_numbers.is_empty() {
            inner.power_flow.bus_numbers.clone()
        } else {
            net.buses.iter().map(|b| b.number).collect()
        };
        let gen_bus_numbers = if !inner.generators.gen_bus_numbers.is_empty() {
            inner.generators.gen_bus_numbers.clone()
        } else {
            net.generators
                .iter()
                .filter(|g| g.in_service)
                .map(|g| g.bus)
                .collect()
        };
        let gen_ids = if !inner.generators.gen_ids.is_empty() {
            inner.generators.gen_ids.clone()
        } else {
            net.generators
                .iter()
                .filter(|g| g.in_service)
                .map(surge_solution::generator_resource_id)
                .collect()
        };
        let gen_machine_ids = if !inner.generators.gen_machine_ids.is_empty() {
            inner.generators.gen_machine_ids.clone()
        } else {
            net.generators
                .iter()
                .filter(|g| g.in_service)
                .map(|g| surge_solution::default_machine_id(g.machine_id.as_deref()))
                .collect()
        };
        Self {
            inner,
            net: Some(net),
            bus_numbers,
            gen_bus_numbers,
            gen_ids,
            gen_machine_ids,
        }
    }

    pub(crate) fn attached_net(&self) -> PyResult<&Arc<surge_network::Network>> {
        self.net.as_ref().ok_or_else(|| {
            PyValueError::new_err(
                "this result is detached from its network; call attach_network(network) first",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_pf_result_requires_attached_network_for_topology_queries() {
        let result = AcPfResult {
            inner: surge_solution::PfSolution {
                voltage_magnitude_pu: vec![1.0],
                voltage_angle_rad: vec![0.0],
                ..Default::default()
            },
            net: None,
        };

        let err = result.attached_net().unwrap_err();
        assert!(err.to_string().contains("detached from its network"));
    }

    #[test]
    fn attach_network_refreshes_generator_identity_metadata() {
        let mut core_net = surge_network::Network::new("test");
        core_net.buses.push(surge_network::network::Bus::new(
            1,
            surge_network::network::BusType::Slack,
            138.0,
        ));
        let mut generator = surge_network::network::Generator::with_id("gen-1", 1, 10.0, 1.0);
        generator.machine_id = Some("7".to_string());
        core_net.generators.push(generator);

        let network = Network {
            inner: Arc::new(core_net),
            oltc_controls: Vec::new(),
            switched_shunts: Vec::new(),
        };
        let mut solution = OpfSolution {
            inner: surge_solution::OpfSolution {
                opf_type: surge_solution::OpfType::DcOpf,
                base_mva: 100.0,
                power_flow: surge_solution::PfSolution {
                    voltage_magnitude_pu: vec![1.0],
                    voltage_angle_rad: vec![0.0],
                    ..Default::default()
                },
                generators: surge_solution::OpfGeneratorResults {
                    gen_p_mw: vec![10.0],
                    gen_q_mvar: vec![],
                    ..Default::default()
                },
                ..Default::default()
            },
            net: None,
            bus_numbers: Vec::new(),
            gen_bus_numbers: Vec::new(),
            gen_ids: Vec::new(),
            gen_machine_ids: Vec::new(),
        };

        solution
            .attach_network(&network)
            .expect("attach_network should succeed");

        assert!(solution.has_attached_network());
        assert_eq!(solution.bus_numbers, vec![1]);
        assert_eq!(solution.gen_bus_numbers, vec![1]);
        assert_eq!(solution.gen_ids, vec!["gen-1".to_string()]);
        assert_eq!(solution.gen_machine_ids, vec!["7".to_string()]);
        assert_eq!(solution.gen_bus_numbers(), vec![1]);
        assert_eq!(solution.gen_ids(), vec!["gen-1".to_string()]);
        assert_eq!(solution.gen_machine_ids(), vec!["7".to_string()]);
        assert_eq!(solution.inner.power_flow.bus_numbers, vec![1]);
        assert_eq!(solution.inner.generators.gen_bus_numbers, vec![1]);
        assert_eq!(solution.inner.generators.gen_ids, vec!["gen-1".to_string()]);
        assert_eq!(
            solution.inner.generators.gen_machine_ids,
            vec!["7".to_string()]
        );
    }
}

#[pyclass(name = "BindingContingency", skip_from_py_object)]
#[derive(Clone)]
pub struct BindingContingency {
    #[pyo3(get)]
    pub contingency_label: String,
    #[pyo3(get)]
    pub cut_kind: String,
    #[pyo3(get)]
    pub outaged_branch_indices: Vec<usize>,
    #[pyo3(get)]
    pub outaged_generator_indices: Vec<usize>,
    #[pyo3(get)]
    pub monitored_branch_idx: usize,
    #[pyo3(get)]
    pub loading_pct: f64,
    #[pyo3(get)]
    pub shadow_price: f64,
}

impl From<surge_opf::BindingContingency> for BindingContingency {
    fn from(value: surge_opf::BindingContingency) -> Self {
        Self {
            contingency_label: value.contingency_label,
            cut_kind: format!("{:?}", value.cut_kind),
            outaged_branch_indices: value.outaged_branch_indices,
            outaged_generator_indices: value.outaged_generator_indices,
            monitored_branch_idx: value.monitored_branch_idx,
            loading_pct: value.loading_pct,
            shadow_price: value.shadow_price,
        }
    }
}

#[pymethods]
impl BindingContingency {
    /// Return this binding contingency as a JSON-serializable dictionary.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("contingency_label", self.contingency_label.clone())?;
        d.set_item("cut_kind", self.cut_kind.clone())?;
        d.set_item(
            "outaged_branch_indices",
            self.outaged_branch_indices.clone(),
        )?;
        d.set_item(
            "outaged_generator_indices",
            self.outaged_generator_indices.clone(),
        )?;
        d.set_item("monitored_branch_idx", self.monitored_branch_idx)?;
        d.set_item("loading_pct", self.loading_pct)?;
        d.set_item("shadow_price", self.shadow_price)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "BindingContingency(label={:?}, kind={}, monitored_branch_idx={}, loading_pct={:.2}, shadow_price={:.4})",
            self.contingency_label,
            self.cut_kind,
            self.monitored_branch_idx,
            self.loading_pct,
            self.shadow_price
        )
    }
}

#[pyclass(name = "ContingencyViolation", skip_from_py_object)]
#[derive(Clone)]
pub struct ContingencyViolation {
    #[pyo3(get)]
    pub contingency_id: String,
    #[pyo3(get)]
    pub contingency_label: String,
    #[pyo3(get)]
    pub outaged_branches: Vec<usize>,
    #[pyo3(get)]
    pub outaged_generators: Vec<usize>,
    #[pyo3(get)]
    pub thermal_violations: Vec<(usize, f64, f64, f64)>,
    #[pyo3(get)]
    pub voltage_violations: Vec<(usize, f64, f64, f64)>,
}

impl From<surge_opf::ContingencyViolation> for ContingencyViolation {
    fn from(value: surge_opf::ContingencyViolation) -> Self {
        Self {
            contingency_id: value.contingency_id,
            contingency_label: value.contingency_label,
            outaged_branches: value.outaged_branches,
            outaged_generators: value.outaged_generators,
            thermal_violations: value.thermal_violations,
            voltage_violations: value.voltage_violations,
        }
    }
}

#[pymethods]
impl ContingencyViolation {
    /// Return this contingency violation as a JSON-serializable dictionary.
    ///
    /// ``thermal_violations`` is a list of dicts with keys
    /// ``branch_idx``, ``loading_pct``, ``flow_mva``, ``limit_mva``.
    /// ``voltage_violations`` is a list of dicts with keys
    /// ``bus_idx``, ``vm_pu``, ``vm_min``, ``vm_max``.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("contingency_id", self.contingency_id.clone())?;
        d.set_item("contingency_label", self.contingency_label.clone())?;
        d.set_item("outaged_branches", self.outaged_branches.clone())?;
        d.set_item("outaged_generators", self.outaged_generators.clone())?;
        let thermal: Vec<Bound<'py, PyDict>> = self
            .thermal_violations
            .iter()
            .map(|(branch_idx, loading_pct, flow_mva, limit_mva)| {
                let e = PyDict::new(py);
                e.set_item("branch_idx", branch_idx)?;
                e.set_item("loading_pct", loading_pct)?;
                e.set_item("flow_mva", flow_mva)?;
                e.set_item("limit_mva", limit_mva)?;
                Ok::<_, PyErr>(e)
            })
            .collect::<PyResult<Vec<_>>>()?;
        d.set_item("thermal_violations", thermal)?;
        let voltage: Vec<Bound<'py, PyDict>> = self
            .voltage_violations
            .iter()
            .map(|(bus_idx, vm_pu, vm_min, vm_max)| {
                let e = PyDict::new(py);
                e.set_item("bus_idx", bus_idx)?;
                e.set_item("vm_pu", vm_pu)?;
                e.set_item("vm_min", vm_min)?;
                e.set_item("vm_max", vm_max)?;
                Ok::<_, PyErr>(e)
            })
            .collect::<PyResult<Vec<_>>>()?;
        d.set_item("voltage_violations", voltage)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "ContingencyViolation(id={:?}, thermal_violations={}, voltage_violations={})",
            self.contingency_id,
            self.thermal_violations.len(),
            self.voltage_violations.len()
        )
    }
}

#[pyclass(name = "FailedContingencyEvaluation", skip_from_py_object)]
#[derive(Clone)]
pub struct FailedContingencyEvaluation {
    #[pyo3(get)]
    pub contingency_id: String,
    #[pyo3(get)]
    pub contingency_label: String,
    #[pyo3(get)]
    pub outaged_branches: Vec<usize>,
    #[pyo3(get)]
    pub outaged_generators: Vec<usize>,
    #[pyo3(get)]
    pub reason: String,
}

impl From<surge_opf::security::FailedContingencyEvaluation> for FailedContingencyEvaluation {
    fn from(value: surge_opf::security::FailedContingencyEvaluation) -> Self {
        Self {
            contingency_id: value.contingency_id,
            contingency_label: value.contingency_label,
            outaged_branches: value.outaged_branches,
            outaged_generators: value.outaged_generators,
            reason: value.reason,
        }
    }
}

#[pymethods]
impl FailedContingencyEvaluation {
    /// Return this failed contingency evaluation as a JSON-serializable dictionary.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("contingency_id", self.contingency_id.clone())?;
        d.set_item("contingency_label", self.contingency_label.clone())?;
        d.set_item("outaged_branches", self.outaged_branches.clone())?;
        d.set_item("outaged_generators", self.outaged_generators.clone())?;
        d.set_item("reason", self.reason.clone())?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "FailedContingencyEvaluation(id={:?}, reason={:?})",
            self.contingency_id, self.reason
        )
    }
}

#[pyclass(name = "ScopfScreeningStats", skip_from_py_object)]
#[derive(Clone)]
pub struct ScopfScreeningStats {
    #[pyo3(get)]
    pub pairs_evaluated: usize,
    #[pyo3(get)]
    pub pre_screened_constraints: usize,
    #[pyo3(get)]
    pub cutting_plane_constraints: usize,
    #[pyo3(get)]
    pub threshold_fraction: f64,
}

impl From<surge_opf::ScopfScreeningStats> for ScopfScreeningStats {
    fn from(value: surge_opf::ScopfScreeningStats) -> Self {
        Self {
            pairs_evaluated: value.pairs_evaluated,
            pre_screened_constraints: value.pre_screened_constraints,
            cutting_plane_constraints: value.cutting_plane_constraints,
            threshold_fraction: value.threshold_fraction,
        }
    }
}

#[pymethods]
impl ScopfScreeningStats {
    /// Return these screening stats as a JSON-serializable dictionary.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("pairs_evaluated", self.pairs_evaluated)?;
        d.set_item("pre_screened_constraints", self.pre_screened_constraints)?;
        d.set_item("cutting_plane_constraints", self.cutting_plane_constraints)?;
        d.set_item("threshold_fraction", self.threshold_fraction)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "ScopfScreeningStats(pairs_evaluated={}, pre_screened_constraints={}, cutting_plane_constraints={}, threshold_fraction={:.3})",
            self.pairs_evaluated,
            self.pre_screened_constraints,
            self.cutting_plane_constraints,
            self.threshold_fraction
        )
    }
}

#[pyclass(name = "DcOpfResult", skip_from_py_object)]
#[derive(Clone)]
pub struct DcOpfResult {
    pub(crate) opf: OpfSolution,
    pub(crate) hvdc_dispatch_mw: Vec<f64>,
    pub(crate) hvdc_shadow_prices: Vec<f64>,
    pub(crate) gen_limit_violations: Vec<(usize, f64)>,
    pub(crate) is_feasible: bool,
}

#[pymethods]
impl DcOpfResult {
    #[getter]
    fn opf(&self) -> OpfSolution {
        self.opf.clone()
    }

    #[getter]
    fn hvdc_dispatch_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.hvdc_dispatch_mw.clone().into_pyarray(py)
    }

    #[getter]
    fn hvdc_shadow_prices<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.hvdc_shadow_prices.clone().into_pyarray(py)
    }

    #[getter]
    fn gen_limit_violations(&self) -> Vec<(usize, f64)> {
        self.gen_limit_violations.clone()
    }

    #[getter]
    fn is_feasible(&self) -> bool {
        self.is_feasible
    }

    /// Return the DC-OPF result as a JSON-serializable dictionary.
    ///
    /// Extends the underlying ``OpfResult.to_dict()`` payload with
    /// ``hvdc_dispatch_mw``, ``hvdc_shadow_prices``, ``gen_limit_violations``,
    /// and ``is_feasible``.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = self.opf.to_dict(py)?;
        d.set_item("hvdc_dispatch_mw", self.hvdc_dispatch_mw.clone())?;
        d.set_item("hvdc_shadow_prices", self.hvdc_shadow_prices.clone())?;
        d.set_item("gen_limit_violations", self.gen_limit_violations.clone())?;
        d.set_item("is_feasible", self.is_feasible)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "DcOpfResult(cost={:.2}, feasible={}, hvdc_links={}, gen_limit_violations={})",
            self.opf.inner.total_cost,
            self.is_feasible,
            self.hvdc_dispatch_mw.len(),
            self.gen_limit_violations.len()
        )
    }
}

#[pyclass(name = "ScopfResult", skip_from_py_object)]
#[derive(Clone)]
pub struct ScopfResult {
    pub(crate) base_opf: OpfSolution,
    pub(crate) formulation: String,
    pub(crate) mode: String,
    pub(crate) iterations: u32,
    pub(crate) converged: bool,
    pub(crate) total_contingencies_evaluated: usize,
    pub(crate) total_contingency_constraints: usize,
    pub(crate) binding_contingencies: Vec<BindingContingency>,
    pub(crate) lmp_contingency_congestion: Vec<f64>,
    pub(crate) remaining_violations: Vec<ContingencyViolation>,
    pub(crate) failed_contingencies: Vec<FailedContingencyEvaluation>,
    pub(crate) screening_stats: ScopfScreeningStats,
    pub(crate) solve_time_secs: f64,
}

#[pymethods]
impl ScopfResult {
    #[getter]
    fn base_opf(&self) -> OpfSolution {
        self.base_opf.clone()
    }

    #[getter]
    fn formulation(&self) -> &str {
        &self.formulation
    }

    #[getter]
    fn mode(&self) -> &str {
        &self.mode
    }

    #[getter]
    fn iterations(&self) -> u32 {
        self.iterations
    }

    #[getter]
    fn converged(&self) -> bool {
        self.converged
    }

    #[getter]
    fn total_contingencies_evaluated(&self) -> usize {
        self.total_contingencies_evaluated
    }

    #[getter]
    fn total_contingency_constraints(&self) -> usize {
        self.total_contingency_constraints
    }

    #[getter]
    fn binding_contingencies(&self) -> Vec<BindingContingency> {
        self.binding_contingencies.clone()
    }

    #[getter]
    fn lmp_contingency_congestion<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.lmp_contingency_congestion.clone().into_pyarray(py)
    }

    #[getter]
    fn remaining_violations(&self) -> Vec<ContingencyViolation> {
        self.remaining_violations.clone()
    }

    #[getter]
    fn failed_contingencies(&self) -> Vec<FailedContingencyEvaluation> {
        self.failed_contingencies.clone()
    }

    #[getter]
    fn screening_stats(&self) -> ScopfScreeningStats {
        self.screening_stats.clone()
    }

    #[getter]
    fn solve_time_secs(&self) -> f64 {
        self.solve_time_secs
    }

    /// Return the SCOPF result as a JSON-serializable dictionary.
    ///
    /// Extends the underlying ``OpfResult.to_dict()`` payload (keyed under
    /// ``base_opf``) with SCOPF metadata (``formulation``, ``mode``,
    /// ``converged``, ``iterations``, ``solve_time_secs``), contingency
    /// counts, and serialized lists of ``binding_contingencies``,
    /// ``remaining_violations``, ``failed_contingencies`` (each via its own
    /// ``to_dict()``), plus ``screening_stats`` and
    /// ``lmp_contingency_congestion``.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("base_opf", self.base_opf.to_dict(py)?)?;
        d.set_item("formulation", self.formulation.clone())?;
        d.set_item("mode", self.mode.clone())?;
        d.set_item("iterations", self.iterations)?;
        d.set_item("converged", self.converged)?;
        d.set_item("solve_time_secs", self.solve_time_secs)?;
        d.set_item(
            "total_contingencies_evaluated",
            self.total_contingencies_evaluated,
        )?;
        d.set_item(
            "total_contingency_constraints",
            self.total_contingency_constraints,
        )?;
        let binding: Vec<Bound<'py, PyDict>> = self
            .binding_contingencies
            .iter()
            .map(|c| c.to_dict(py))
            .collect::<PyResult<Vec<_>>>()?;
        d.set_item("binding_contingencies", binding)?;
        let remaining: Vec<Bound<'py, PyDict>> = self
            .remaining_violations
            .iter()
            .map(|v| v.to_dict(py))
            .collect::<PyResult<Vec<_>>>()?;
        d.set_item("remaining_violations", remaining)?;
        let failed: Vec<Bound<'py, PyDict>> = self
            .failed_contingencies
            .iter()
            .map(|f| f.to_dict(py))
            .collect::<PyResult<Vec<_>>>()?;
        d.set_item("failed_contingencies", failed)?;
        d.set_item("screening_stats", self.screening_stats.to_dict(py)?)?;
        d.set_item(
            "lmp_contingency_congestion",
            self.lmp_contingency_congestion.clone(),
        )?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "ScopfResult(formulation={:?}, mode={:?}, converged={}, iterations={}, constraints={}, failed={})",
            self.formulation,
            self.mode,
            self.converged,
            self.iterations,
            self.total_contingency_constraints,
            self.failed_contingencies.len()
        )
    }
}

#[pyclass(name = "AcOpfHvdcResult", skip_from_py_object)]
#[derive(Clone)]
pub struct AcOpfHvdcResult {
    pub(crate) opf: OpfSolution,
    pub(crate) hvdc_p_dc_mw: Vec<f64>,
    pub(crate) hvdc_p_loss_mw: Vec<f64>,
    pub(crate) hvdc_iterations: u32,
}

#[pymethods]
impl AcOpfHvdcResult {
    #[getter]
    fn opf(&self) -> OpfSolution {
        self.opf.clone()
    }

    #[getter]
    fn hvdc_p_dc_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.hvdc_p_dc_mw.clone().into_pyarray(py)
    }

    #[getter]
    fn hvdc_p_loss_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.hvdc_p_loss_mw.clone().into_pyarray(py)
    }

    #[getter]
    fn hvdc_iterations(&self) -> u32 {
        self.hvdc_iterations
    }

    /// Return the AC-OPF result as a JSON-serializable dictionary.
    ///
    /// Extends the underlying ``OpfResult.to_dict()`` payload with
    /// ``hvdc_p_dc_mw``, ``hvdc_p_loss_mw``, and ``hvdc_iterations``.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = self.opf.to_dict(py)?;
        d.set_item("hvdc_p_dc_mw", self.hvdc_p_dc_mw.clone())?;
        d.set_item("hvdc_p_loss_mw", self.hvdc_p_loss_mw.clone())?;
        d.set_item("hvdc_iterations", self.hvdc_iterations)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "AcOpfHvdcResult(cost={:.2}, hvdc_links={}, hvdc_iterations={})",
            self.opf.inner.total_cost,
            self.hvdc_p_dc_mw.len(),
            self.hvdc_iterations
        )
    }
}

/// AC-OPF Benders subproblem result: OpfSolution at a fixed operating point,
/// plus the slack cost and per-generator marginals used to construct a master
/// cut in the SCED-AC Benders decomposition.
#[pyclass(name = "AcOpfBendersSubproblemResult", skip_from_py_object)]
#[derive(Clone)]
pub struct AcOpfBendersSubproblemResult {
    pub(crate) opf: OpfSolution,
    pub(crate) slack_cost_dollars_per_hour: f64,
    pub(crate) slack_marginal_by_id: HashMap<String, f64>,
    pub(crate) converged: bool,
}

#[pymethods]
impl AcOpfBendersSubproblemResult {
    /// The underlying AC-OPF solution at the fixed operating point.
    #[getter]
    fn opf(&self) -> OpfSolution {
        self.opf.clone()
    }

    /// Whether the NLP backend reported convergence at the fixed point.
    /// Even when ``True``, callers should check ``slack_cost_dollars_per_hour``
    /// to decide whether the operating point is operationally acceptable —
    /// converged-with-large-slacks is a valid state.
    #[getter]
    fn converged(&self) -> bool {
        self.converged
    }

    /// Total slack penalty cost added by the AC physics at this dispatch
    /// ($/hr). Equal to ``opf.total_cost`` minus the pure energy cost of the
    /// fixed Pg schedule, i.e. the aggregated branch-thermal slack, bus P/Q
    /// balance slack, and any other soft-constraint violations priced at
    /// their penalty rates.
    #[getter]
    fn slack_cost_dollars_per_hour(&self) -> f64 {
        self.slack_cost_dollars_per_hour
    }

    /// Per-generator marginal of the slack cost with respect to the fixed
    /// ``Pg_target`` ($/MW-hr). Keyed by **stable resource_id (generator id
    /// string)** so the cut survives across network re-builds. Only
    /// generators that were fixed and produced a nonzero marginal appear.
    ///
    /// Represents the gradient of the slack penalty *alone* — the fixed
    /// generator's own marginal production cost has been subtracted out, so
    /// the values are safe to use as Benders cut coefficients on top of a
    /// master objective that already contains DC_cost(Pg).
    #[getter]
    fn slack_marginal_by_id(&self) -> HashMap<String, f64> {
        self.slack_marginal_by_id.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "AcOpfBendersSubproblemResult(converged={}, slack_cost=${:.2}/hr, {} marginals)",
            self.converged,
            self.slack_cost_dollars_per_hour,
            self.slack_marginal_by_id.len(),
        )
    }
}

#[pyclass(name = "OtsResult", skip_from_py_object)]
#[derive(Clone)]
pub struct OtsResult {
    pub(crate) converged: bool,
    pub(crate) objective: f64,
    pub(crate) switched_out: Vec<(u32, u32, String)>,
    pub(crate) n_switches: usize,
    pub(crate) gen_dispatch: Vec<f64>,
    pub(crate) branch_flows: Vec<f64>,
    pub(crate) lmps: Vec<f64>,
    pub(crate) solve_time_ms: f64,
    pub(crate) mip_gap: f64,
}

#[pymethods]
impl OtsResult {
    #[getter]
    fn converged(&self) -> bool {
        self.converged
    }

    #[getter]
    fn objective(&self) -> f64 {
        self.objective
    }

    #[getter]
    fn switched_out(&self) -> Vec<(u32, u32, String)> {
        self.switched_out.clone()
    }

    #[getter]
    fn n_switches(&self) -> usize {
        self.n_switches
    }

    #[getter]
    fn gen_dispatch<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.gen_dispatch.clone().into_pyarray(py)
    }

    #[getter]
    fn branch_flows<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.branch_flows.clone().into_pyarray(py)
    }

    #[getter]
    fn lmps<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.lmps.clone().into_pyarray(py)
    }

    #[getter]
    fn solve_time_ms(&self) -> f64 {
        self.solve_time_ms
    }

    #[getter]
    fn mip_gap(&self) -> f64 {
        self.mip_gap
    }

    fn __repr__(&self) -> String {
        format!(
            "OtsResult(converged={}, objective={:.2}, n_switches={}, mip_gap={:.4})",
            self.converged, self.objective, self.n_switches, self.mip_gap
        )
    }
}

#[pyclass(name = "OrpdResult", skip_from_py_object)]
#[derive(Clone)]
pub struct OrpdResult {
    pub(crate) converged: bool,
    pub(crate) objective: f64,
    pub(crate) total_losses_mw: f64,
    pub(crate) voltage_deviation: f64,
    pub(crate) vm: Vec<f64>,
    pub(crate) va_rad: Vec<f64>,
    pub(crate) q_dispatch_pu: Vec<f64>,
    pub(crate) p_dispatch_pu: Vec<f64>,
    pub(crate) base_mva: f64,
    pub(crate) iterations: Option<u32>,
    pub(crate) solve_time_ms: f64,
}

#[pymethods]
impl OrpdResult {
    #[getter]
    fn converged(&self) -> bool {
        self.converged
    }

    #[getter]
    fn objective(&self) -> f64 {
        self.objective
    }

    #[getter]
    fn total_losses_mw(&self) -> f64 {
        self.total_losses_mw
    }

    #[getter]
    fn voltage_deviation(&self) -> f64 {
        self.voltage_deviation
    }

    #[getter]
    fn vm<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.vm.clone().into_pyarray(py)
    }

    #[getter]
    fn va_rad<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.va_rad.clone().into_pyarray(py)
    }

    #[getter]
    fn va_deg<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.va_rad
            .iter()
            .map(|a| a.to_degrees())
            .collect::<Vec<_>>()
            .into_pyarray(py)
    }

    #[getter]
    fn q_dispatch_pu<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.q_dispatch_pu.clone().into_pyarray(py)
    }

    #[getter]
    fn p_dispatch_pu<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.p_dispatch_pu.clone().into_pyarray(py)
    }

    #[getter]
    fn q_dispatch_mvar<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.q_dispatch_pu
            .iter()
            .map(|q| q * self.base_mva)
            .collect::<Vec<_>>()
            .into_pyarray(py)
    }

    #[getter]
    fn p_dispatch_mw<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.p_dispatch_pu
            .iter()
            .map(|p| p * self.base_mva)
            .collect::<Vec<_>>()
            .into_pyarray(py)
    }

    #[getter]
    fn iterations(&self) -> Option<u32> {
        self.iterations
    }

    #[getter]
    fn solve_time_ms(&self) -> f64 {
        self.solve_time_ms
    }

    fn __repr__(&self) -> String {
        format!(
            "OrpdResult(converged={}, objective={:.4}, total_losses_mw={:.3}, iterations={})",
            self.converged,
            self.objective,
            self.total_losses_mw,
            self.iterations
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )
    }
}

#[pyclass(name = "ReconfigResult", skip_from_py_object)]
#[derive(Clone)]
pub struct ReconfigResult {
    #[pyo3(get)]
    pub open_branches: Vec<usize>,
    #[pyo3(get)]
    pub objective: f64,
    #[pyo3(get)]
    pub converged: bool,
    #[pyo3(get)]
    pub solve_time_s: f64,
}

#[pymethods]
impl ReconfigResult {
    fn __repr__(&self) -> String {
        format!(
            "ReconfigResult(converged={}, objective={:.2}, open_branches={})",
            self.converged,
            self.objective,
            self.open_branches.len()
        )
    }
}

// ---------------------------------------------------------------------------
// Contingency analysis wrapper
// ---------------------------------------------------------------------------

/// N-1 contingency analysis result.
#[pyclass]
pub struct ContingencyAnalysis {
    pub(crate) inner: surge_contingency::ContingencyAnalysis,
}

#[pymethods]
impl ContingencyAnalysis {
    /// Total contingencies analyzed.
    #[getter]
    fn n_contingencies(&self) -> usize {
        self.inner.summary.total_contingencies
    }

    /// Contingencies screened out by LODF/FDPF filter.
    #[getter]
    fn n_screened_out(&self) -> usize {
        self.inner.summary.screened_out
    }

    /// Contingencies solved with full AC Newton-Raphson.
    #[getter]
    fn n_ac_solved(&self) -> usize {
        self.inner.summary.ac_solved
    }

    /// AC-solved contingencies that converged.
    #[getter]
    fn n_converged(&self) -> usize {
        self.inner.summary.converged
    }

    /// Contingencies that produced at least one violation.
    #[getter]
    fn n_with_violations(&self) -> usize {
        self.inner.summary.with_violations
    }

    /// Wall clock time (seconds).
    #[getter]
    fn solve_time_secs(&self) -> f64 {
        self.inner.summary.solve_time_secs
    }

    /// Return a pandas DataFrame (or dict if pandas is not installed).
    ///
    /// Columns: contingency_id, label, n_violations, converged, max_overload_pct,
    /// max_l_index, vsm_category.
    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        let ids: Vec<&str> = self.inner.results.iter().map(|r| r.id.as_str()).collect();
        let labels: Vec<&str> = self
            .inner
            .results
            .iter()
            .map(|r| r.label.as_str())
            .collect();
        let n_viols: Vec<usize> = self
            .inner
            .results
            .iter()
            .map(|r| r.violations.len())
            .collect();
        let converged: Vec<bool> = self.inner.results.iter().map(|r| r.converged).collect();
        let max_overload: Vec<f64> = self
            .inner
            .results
            .iter()
            .map(|r| {
                r.violations
                    .iter()
                    .filter_map(|v| {
                        if let surge_contingency::Violation::ThermalOverload {
                            loading_pct, ..
                        } = v
                        {
                            Some(*loading_pct)
                        } else {
                            None
                        }
                    })
                    .fold(0.0_f64, f64::max)
            })
            .collect();
        let max_l_index: Vec<f64> = self
            .inner
            .results
            .iter()
            .map(|r| {
                r.voltage_stress
                    .as_ref()
                    .and_then(|vs| vs.max_l_index)
                    .unwrap_or(f64::NAN)
            })
            .collect();
        let vsm_category: Vec<&str> = self
            .inner
            .results
            .iter()
            .map(
                |r| match r.voltage_stress.as_ref().and_then(|vs| vs.category) {
                    Some(surge_contingency::VsmCategory::Secure) => "secure",
                    Some(surge_contingency::VsmCategory::Marginal) => "marginal",
                    Some(surge_contingency::VsmCategory::Critical) => "critical",
                    Some(surge_contingency::VsmCategory::Unstable) => "unstable",
                    None => "",
                },
            )
            .collect();
        dict.set_item("contingency_id", ids)?;
        dict.set_item("label", labels)?;
        dict.set_item("converged", converged)?;
        dict.set_item("n_violations", n_viols)?;
        dict.set_item("max_overload_pct", max_overload)?;
        dict.set_item("max_l_index", max_l_index)?;
        dict.set_item("vsm_category", vsm_category)?;
        dict_to_dataframe_with_index(py, dict, &["contingency_id"])
    }

    /// Number of voltage-critical contingencies (Critical or Unstable).
    #[getter]
    fn n_voltage_critical(&self) -> usize {
        self.inner.summary.n_voltage_critical
    }

    /// Return voltage-critical contingencies as a DataFrame.
    ///
    /// Filters to contingencies classified as Critical or Unstable, sorted by
    /// L-index descending (worst first). Columns: contingency_id, label,
    /// max_l_index, critical_bus, vsm_category.
    fn voltage_critical_df<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        let mut ids = Vec::new();
        let mut labels = Vec::new();
        let mut l_idx = Vec::new();
        let mut crit_bus: Vec<Option<u32>> = Vec::new();
        let mut category = Vec::new();

        let mut critical: Vec<&surge_contingency::ContingencyResult> = self
            .inner
            .results
            .iter()
            .filter(|r| {
                r.voltage_stress.as_ref().is_some_and(|vs| {
                    matches!(
                        vs.category,
                        Some(
                            surge_contingency::VsmCategory::Critical
                                | surge_contingency::VsmCategory::Unstable
                        )
                    )
                })
            })
            .collect();
        // Sort by L-index descending (worst first).
        critical.sort_by(|a, b| {
            let la = a
                .voltage_stress
                .as_ref()
                .and_then(|vs| vs.max_l_index)
                .unwrap_or(0.0);
            let lb = b
                .voltage_stress
                .as_ref()
                .and_then(|vs| vs.max_l_index)
                .unwrap_or(0.0);
            lb.partial_cmp(&la).unwrap_or(std::cmp::Ordering::Equal)
        });

        for r in &critical {
            let vs = r.voltage_stress.as_ref();
            ids.push(r.id.as_str());
            labels.push(r.label.as_str());
            l_idx.push(vs.and_then(|v| v.max_l_index).unwrap_or(f64::NAN));
            crit_bus.push(vs.and_then(|v| v.critical_l_index_bus));
            category.push(match vs.and_then(|v| v.category) {
                Some(surge_contingency::VsmCategory::Critical) => "critical",
                Some(surge_contingency::VsmCategory::Unstable) => "unstable",
                _ => "",
            });
        }

        dict.set_item("contingency_id", ids)?;
        dict.set_item("label", labels)?;
        dict.set_item("max_l_index", l_idx)?;
        dict.set_item("critical_bus", crit_bus)?;
        dict.set_item("vsm_category", category)?;
        dict_to_dataframe_with_index(py, dict, &["contingency_id"])
    }

    /// Number of violations found.
    #[getter]
    fn n_violations(&self) -> usize {
        self.inner.results.iter().map(|r| r.violations.len()).sum()
    }

    /// Return a per-contingency summary as a pandas DataFrame (or dict if pandas is not installed).
    ///
    /// Columns: contingency_id, label, converged, n_violations, max_loading_pct,
    ///          min_vm_pu, n_islands.
    fn results_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        let n = self.inner.results.len();
        let mut ids = Vec::with_capacity(n);
        let mut labels = Vec::with_capacity(n);
        let mut converged = Vec::with_capacity(n);
        let mut n_viols = Vec::with_capacity(n);
        let mut max_loading = Vec::with_capacity(n);
        let mut min_vm = Vec::with_capacity(n);
        let mut n_islands = Vec::with_capacity(n);
        for r in &self.inner.results {
            ids.push(r.id.as_str());
            labels.push(r.label.as_str());
            converged.push(r.converged);
            n_viols.push(r.violations.len());
            let ml = r
                .violations
                .iter()
                .filter_map(|v| {
                    if let surge_contingency::Violation::ThermalOverload { loading_pct, .. } = v {
                        Some(*loading_pct)
                    } else {
                        None
                    }
                })
                .fold(0.0_f64, f64::max);
            max_loading.push(ml);
            let mv = r
                .violations
                .iter()
                .filter_map(|v| match v {
                    surge_contingency::Violation::VoltageLow { vm, .. } => Some(*vm),
                    surge_contingency::Violation::VoltageHigh { vm, .. } => Some(*vm),
                    _ => None,
                })
                .fold(f64::INFINITY, f64::min);
            min_vm.push(if mv.is_finite() { mv } else { f64::NAN });
            n_islands.push(r.n_islands);
        }
        dict.set_item("contingency_id", ids)?;
        dict.set_item("label", labels)?;
        dict.set_item("converged", converged)?;
        dict.set_item("n_violations", n_viols)?;
        dict.set_item("max_loading_pct", max_loading)?;
        dict.set_item("min_vm_pu", min_vm)?;
        dict.set_item("n_islands", n_islands)?;
        dict_to_dataframe_with_index(py, dict, &["contingency_id"])
    }

    /// Return a per-violation flat DataFrame (or dict if pandas is not installed).
    ///
    /// Columns: contingency_id, violation_type, from_bus, to_bus, bus_number,
    ///          loading_pct, flow_mw, flow_mva, limit_mva, vm_pu, vm_limit_pu.
    fn violations_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        use surge_contingency::Violation;
        let dict = PyDict::new(py);
        let mut ctg_id: Vec<String> = Vec::new();
        let mut vtype: Vec<String> = Vec::new();
        let mut from_bus: Vec<Option<u32>> = Vec::new();
        let mut to_bus: Vec<Option<u32>> = Vec::new();
        let mut bus_num: Vec<Option<u32>> = Vec::new();
        let mut loading_pct: Vec<Option<f64>> = Vec::new();
        let mut flow_mw_col: Vec<Option<f64>> = Vec::new();
        let mut flow_mva: Vec<Option<f64>> = Vec::new();
        let mut limit_mva: Vec<Option<f64>> = Vec::new();
        let mut vm_pu: Vec<Option<f64>> = Vec::new();
        let mut vm_limit: Vec<Option<f64>> = Vec::new();
        for r in &self.inner.results {
            for v in &r.violations {
                ctg_id.push(r.id.clone());
                match v {
                    Violation::ThermalOverload {
                        from_bus: fb,
                        to_bus: tb,
                        loading_pct: lp,
                        flow_mw: fp,
                        flow_mva: fm,
                        limit_mva: lm,
                        ..
                    } => {
                        vtype.push("ThermalOverload".into());
                        from_bus.push(Some(*fb));
                        to_bus.push(Some(*tb));
                        bus_num.push(None);
                        loading_pct.push(Some(*lp));
                        flow_mw_col.push(Some(*fp));
                        flow_mva.push(Some(*fm));
                        limit_mva.push(Some(*lm));
                        vm_pu.push(None);
                        vm_limit.push(None);
                    }
                    Violation::VoltageLow {
                        bus_number,
                        vm,
                        limit,
                    } => {
                        vtype.push("VoltageLow".into());
                        from_bus.push(None);
                        to_bus.push(None);
                        bus_num.push(Some(*bus_number));
                        loading_pct.push(None);
                        flow_mw_col.push(None);
                        flow_mva.push(None);
                        limit_mva.push(None);
                        vm_pu.push(Some(*vm));
                        vm_limit.push(Some(*limit));
                    }
                    Violation::VoltageHigh {
                        bus_number,
                        vm,
                        limit,
                    } => {
                        vtype.push("VoltageHigh".into());
                        from_bus.push(None);
                        to_bus.push(None);
                        bus_num.push(Some(*bus_number));
                        loading_pct.push(None);
                        flow_mw_col.push(None);
                        flow_mva.push(None);
                        limit_mva.push(None);
                        vm_pu.push(Some(*vm));
                        vm_limit.push(Some(*limit));
                    }
                    Violation::NonConvergent { max_mismatch, .. } => {
                        vtype.push("NonConvergent".into());
                        from_bus.push(None);
                        to_bus.push(None);
                        bus_num.push(None);
                        loading_pct.push(Some(*max_mismatch));
                        flow_mw_col.push(None);
                        flow_mva.push(None);
                        limit_mva.push(None);
                        vm_pu.push(None);
                        vm_limit.push(None);
                    }
                    Violation::Islanding { n_components } => {
                        vtype.push("Islanding".into());
                        from_bus.push(None);
                        to_bus.push(None);
                        bus_num.push(None);
                        loading_pct.push(Some(*n_components as f64));
                        flow_mw_col.push(None);
                        flow_mva.push(None);
                        limit_mva.push(None);
                        vm_pu.push(None);
                        vm_limit.push(None);
                    }
                    Violation::FlowgateOverload {
                        name,
                        flow_mw,
                        limit_mw,
                        loading_pct: lp,
                    } => {
                        vtype.push(format!("FlowgateOverload:{name}"));
                        from_bus.push(None);
                        to_bus.push(None);
                        bus_num.push(None);
                        loading_pct.push(Some(*lp));
                        flow_mw_col.push(Some(*flow_mw));
                        flow_mva.push(Some(*flow_mw));
                        limit_mva.push(Some(*limit_mw));
                        vm_pu.push(None);
                        vm_limit.push(None);
                    }
                    Violation::InterfaceOverload {
                        name,
                        flow_mw,
                        limit_mw,
                        loading_pct: lp,
                    } => {
                        vtype.push(format!("InterfaceOverload:{name}"));
                        from_bus.push(None);
                        to_bus.push(None);
                        bus_num.push(None);
                        loading_pct.push(Some(*lp));
                        flow_mw_col.push(Some(*flow_mw));
                        flow_mva.push(Some(*flow_mw));
                        limit_mva.push(Some(*limit_mw));
                        vm_pu.push(None);
                        vm_limit.push(None);
                    }
                }
            }
        }
        dict.set_item("contingency_id", ctg_id)?;
        dict.set_item("violation_type", vtype)?;
        dict.set_item("from_bus", from_bus)?;
        dict.set_item("to_bus", to_bus)?;
        dict.set_item("bus_number", bus_num)?;
        dict.set_item("loading_pct", loading_pct)?;
        dict.set_item("flow_mw", flow_mw_col)?;
        dict.set_item("flow_mva", flow_mva)?;
        dict.set_item("limit_mva", limit_mva)?;
        dict.set_item("vm_pu", vm_pu)?;
        dict.set_item("vm_limit_pu", vm_limit)?;
        dict_to_dataframe_with_index(py, dict, &["contingency_id"])
    }

    /// Validate the contingency analysis results for consistency.
    ///
    /// Checks:
    ///   - At least one contingency was analyzed
    ///   - n_converged <= n_ac_solved (sanity on summary counters)
    ///   - n_with_violations <= n_converged
    ///
    /// Returns True if all checks pass. Raises ValueError with details on
    /// the first inconsistency found.
    fn validate(&self) -> PyResult<bool> {
        let s = &self.inner.summary;
        if s.total_contingencies == 0 {
            return Err(PyValueError::new_err(
                "ContingencyAnalysis: no contingencies were analyzed",
            ));
        }
        if s.converged > s.ac_solved {
            return Err(PyValueError::new_err(format!(
                "ContingencyAnalysis: n_converged ({}) > n_ac_solved ({}) — summary inconsistent",
                s.converged, s.ac_solved
            )));
        }
        if s.with_violations > s.converged {
            return Err(PyValueError::new_err(format!(
                "ContingencyAnalysis: n_with_violations ({}) > n_converged ({}) —                  summary inconsistent",
                s.with_violations, s.converged
            )));
        }
        Ok(true)
    }

    /// Post-contingency bus voltage magnitudes (p.u.) for a given contingency ID.
    ///
    /// Returns a numpy array of Vm values, or None if the contingency was not
    /// found or ``store_post_voltages`` was not enabled during analysis.
    ///
    /// Args:
    ///     contingency_id: The contingency identifier string.
    ///
    /// Returns:
    ///     numpy array of floats, or None.
    fn post_contingency_vm<'py>(
        &self,
        py: Python<'py>,
        contingency_id: &str,
    ) -> Option<Bound<'py, PyArray1<f64>>> {
        self.inner
            .results
            .iter()
            .find(|r| r.id == contingency_id)
            .and_then(|r| r.post_vm.as_ref())
            .map(|v| v.clone().into_pyarray(py))
    }

    /// Post-contingency bus voltage angles (radians) for a given contingency ID.
    ///
    /// Returns a numpy array of Va values, or None if the contingency was not
    /// found or ``store_post_voltages`` was not enabled during analysis.
    fn post_contingency_va<'py>(
        &self,
        py: Python<'py>,
        contingency_id: &str,
    ) -> Option<Bound<'py, PyArray1<f64>>> {
        self.inner
            .results
            .iter()
            .find(|r| r.id == contingency_id)
            .and_then(|r| r.post_va.as_ref())
            .map(|v| v.clone().into_pyarray(py))
    }

    /// Post-contingency branch apparent power flows (MVA) for a given contingency ID.
    ///
    /// Returns a numpy array of from-side apparent power per branch, or None if
    /// the contingency was not found or ``store_post_voltages`` was not enabled.
    fn post_contingency_flows<'py>(
        &self,
        py: Python<'py>,
        contingency_id: &str,
    ) -> Option<Bound<'py, PyArray1<f64>>> {
        self.inner
            .results
            .iter()
            .find(|r| r.id == contingency_id)
            .and_then(|r| r.post_branch_flows.as_ref())
            .map(|v| v.clone().into_pyarray(py))
    }

    /// Return the contingency analysis as a JSON-serializable dictionary.
    ///
    /// Keys:
    ///   * Summary counts: ``n_contingencies``, ``n_screened_out``,
    ///     ``n_ac_solved``, ``n_converged``, ``n_with_violations``,
    ///     ``n_violations``, ``n_voltage_critical``, ``solve_time_secs``.
    ///   * ``results``: list of per-contingency summary dicts
    ///     (contingency_id, label, converged, n_violations,
    ///     max_loading_pct, min_vm_pu, n_islands, vsm_category, max_l_index).
    ///   * ``violations``: flat list of per-violation dicts
    ///     (contingency_id, violation_type, from_bus, to_bus, bus_number,
    ///     loading_pct, flow_mw, flow_mva, limit_mva, vm_pu, vm_limit_pu).
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        use surge_contingency::Violation;
        let d = PyDict::new(py);
        let s = &self.inner.summary;
        d.set_item("n_contingencies", s.total_contingencies)?;
        d.set_item("n_screened_out", s.screened_out)?;
        d.set_item("n_ac_solved", s.ac_solved)?;
        d.set_item("n_converged", s.converged)?;
        d.set_item("n_with_violations", s.with_violations)?;
        d.set_item(
            "n_violations",
            self.inner
                .results
                .iter()
                .map(|r| r.violations.len())
                .sum::<usize>(),
        )?;
        d.set_item("n_voltage_critical", s.n_voltage_critical)?;
        d.set_item("solve_time_secs", s.solve_time_secs)?;

        let mut results: Vec<Bound<'py, PyDict>> = Vec::with_capacity(self.inner.results.len());
        for r in &self.inner.results {
            let rd = PyDict::new(py);
            rd.set_item("contingency_id", r.id.clone())?;
            rd.set_item("label", r.label.clone())?;
            rd.set_item("converged", r.converged)?;
            rd.set_item("n_violations", r.violations.len())?;
            let max_loading = r
                .violations
                .iter()
                .filter_map(|v| {
                    if let Violation::ThermalOverload { loading_pct, .. } = v {
                        Some(*loading_pct)
                    } else {
                        None
                    }
                })
                .fold(0.0_f64, f64::max);
            rd.set_item("max_loading_pct", max_loading)?;
            let min_vm = r
                .violations
                .iter()
                .filter_map(|v| match v {
                    Violation::VoltageLow { vm, .. } => Some(*vm),
                    Violation::VoltageHigh { vm, .. } => Some(*vm),
                    _ => None,
                })
                .fold(f64::INFINITY, f64::min);
            rd.set_item(
                "min_vm_pu",
                if min_vm.is_finite() { min_vm } else { f64::NAN },
            )?;
            rd.set_item("n_islands", r.n_islands)?;
            let category = r
                .voltage_stress
                .as_ref()
                .and_then(|vs| vs.category)
                .map(|c| match c {
                    surge_contingency::VsmCategory::Secure => "secure",
                    surge_contingency::VsmCategory::Marginal => "marginal",
                    surge_contingency::VsmCategory::Critical => "critical",
                    surge_contingency::VsmCategory::Unstable => "unstable",
                });
            rd.set_item("vsm_category", category)?;
            let max_l = r.voltage_stress.as_ref().and_then(|vs| vs.max_l_index);
            rd.set_item("max_l_index", max_l)?;
            results.push(rd);
        }
        d.set_item("results", results)?;

        let mut violations: Vec<Bound<'py, PyDict>> = Vec::new();
        for r in &self.inner.results {
            for v in &r.violations {
                let vd = PyDict::new(py);
                vd.set_item("contingency_id", r.id.clone())?;
                match v {
                    Violation::ThermalOverload {
                        from_bus,
                        to_bus,
                        loading_pct,
                        flow_mw,
                        flow_mva,
                        limit_mva,
                        ..
                    } => {
                        vd.set_item("violation_type", "ThermalOverload")?;
                        vd.set_item("from_bus", *from_bus)?;
                        vd.set_item("to_bus", *to_bus)?;
                        vd.set_item("bus_number", py.None())?;
                        vd.set_item("loading_pct", *loading_pct)?;
                        vd.set_item("flow_mw", *flow_mw)?;
                        vd.set_item("flow_mva", *flow_mva)?;
                        vd.set_item("limit_mva", *limit_mva)?;
                        vd.set_item("vm_pu", py.None())?;
                        vd.set_item("vm_limit_pu", py.None())?;
                    }
                    Violation::VoltageLow {
                        bus_number,
                        vm,
                        limit,
                    } => {
                        vd.set_item("violation_type", "VoltageLow")?;
                        vd.set_item("from_bus", py.None())?;
                        vd.set_item("to_bus", py.None())?;
                        vd.set_item("bus_number", *bus_number)?;
                        vd.set_item("loading_pct", py.None())?;
                        vd.set_item("flow_mw", py.None())?;
                        vd.set_item("flow_mva", py.None())?;
                        vd.set_item("limit_mva", py.None())?;
                        vd.set_item("vm_pu", *vm)?;
                        vd.set_item("vm_limit_pu", *limit)?;
                    }
                    Violation::VoltageHigh {
                        bus_number,
                        vm,
                        limit,
                    } => {
                        vd.set_item("violation_type", "VoltageHigh")?;
                        vd.set_item("from_bus", py.None())?;
                        vd.set_item("to_bus", py.None())?;
                        vd.set_item("bus_number", *bus_number)?;
                        vd.set_item("loading_pct", py.None())?;
                        vd.set_item("flow_mw", py.None())?;
                        vd.set_item("flow_mva", py.None())?;
                        vd.set_item("limit_mva", py.None())?;
                        vd.set_item("vm_pu", *vm)?;
                        vd.set_item("vm_limit_pu", *limit)?;
                    }
                    Violation::NonConvergent { max_mismatch, .. } => {
                        vd.set_item("violation_type", "NonConvergent")?;
                        vd.set_item("from_bus", py.None())?;
                        vd.set_item("to_bus", py.None())?;
                        vd.set_item("bus_number", py.None())?;
                        vd.set_item("loading_pct", *max_mismatch)?;
                        vd.set_item("flow_mw", py.None())?;
                        vd.set_item("flow_mva", py.None())?;
                        vd.set_item("limit_mva", py.None())?;
                        vd.set_item("vm_pu", py.None())?;
                        vd.set_item("vm_limit_pu", py.None())?;
                    }
                    Violation::Islanding { n_components } => {
                        vd.set_item("violation_type", "Islanding")?;
                        vd.set_item("from_bus", py.None())?;
                        vd.set_item("to_bus", py.None())?;
                        vd.set_item("bus_number", py.None())?;
                        vd.set_item("loading_pct", *n_components as f64)?;
                        vd.set_item("flow_mw", py.None())?;
                        vd.set_item("flow_mva", py.None())?;
                        vd.set_item("limit_mva", py.None())?;
                        vd.set_item("vm_pu", py.None())?;
                        vd.set_item("vm_limit_pu", py.None())?;
                    }
                    Violation::FlowgateOverload {
                        name,
                        flow_mw,
                        limit_mw,
                        loading_pct,
                    } => {
                        vd.set_item("violation_type", format!("FlowgateOverload:{name}"))?;
                        vd.set_item("from_bus", py.None())?;
                        vd.set_item("to_bus", py.None())?;
                        vd.set_item("bus_number", py.None())?;
                        vd.set_item("loading_pct", *loading_pct)?;
                        vd.set_item("flow_mw", *flow_mw)?;
                        vd.set_item("flow_mva", *flow_mw)?;
                        vd.set_item("limit_mva", *limit_mw)?;
                        vd.set_item("vm_pu", py.None())?;
                        vd.set_item("vm_limit_pu", py.None())?;
                    }
                    Violation::InterfaceOverload {
                        name,
                        flow_mw,
                        limit_mw,
                        loading_pct,
                    } => {
                        vd.set_item("violation_type", format!("InterfaceOverload:{name}"))?;
                        vd.set_item("from_bus", py.None())?;
                        vd.set_item("to_bus", py.None())?;
                        vd.set_item("bus_number", py.None())?;
                        vd.set_item("loading_pct", *loading_pct)?;
                        vd.set_item("flow_mw", *flow_mw)?;
                        vd.set_item("flow_mva", *flow_mw)?;
                        vd.set_item("limit_mva", *limit_mw)?;
                        vd.set_item("vm_pu", py.None())?;
                        vd.set_item("vm_limit_pu", py.None())?;
                    }
                }
                violations.push(vd);
            }
        }
        d.set_item("violations", violations)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "ContingencyAnalysis(total={}, violations={}, time={:.2}s)",
            self.inner.summary.total_contingencies,
            self.inner.summary.with_violations,
            self.inner.summary.solve_time_secs
        )
    }
}

#[pyclass(name = "HvdcLccDetail", skip_from_py_object)]
#[derive(Clone)]
pub struct HvdcLccDetail {
    #[pyo3(get)]
    pub alpha_deg: f64,
    #[pyo3(get)]
    pub gamma_deg: f64,
    #[pyo3(get)]
    pub i_dc_pu: f64,
    #[pyo3(get)]
    pub power_factor: f64,
}

impl HvdcLccDetail {
    pub(crate) fn from_inner(detail: surge_hvdc::HvdcLccDetail) -> Self {
        Self {
            alpha_deg: detail.alpha_deg,
            gamma_deg: detail.gamma_deg,
            i_dc_pu: detail.i_dc_pu,
            power_factor: detail.power_factor,
        }
    }
}

#[pymethods]
impl HvdcLccDetail {
    fn __repr__(&self) -> String {
        format!(
            "HvdcLccDetail(alpha_deg={:.2}, gamma_deg={:.2}, i_dc_pu={:.4}, power_factor={:.4})",
            self.alpha_deg, self.gamma_deg, self.i_dc_pu, self.power_factor
        )
    }
}

#[pyclass(name = "HvdcStationSolution", skip_from_py_object)]
#[derive(Clone)]
pub struct HvdcStationSolution {
    #[pyo3(get)]
    pub name: Option<String>,
    #[pyo3(get)]
    pub technology: String,
    #[pyo3(get)]
    pub ac_bus: u32,
    #[pyo3(get)]
    pub dc_bus: Option<u32>,
    #[pyo3(get)]
    pub p_ac_mw: f64,
    #[pyo3(get)]
    pub q_ac_mvar: f64,
    #[pyo3(get)]
    pub p_dc_mw: f64,
    #[pyo3(get)]
    pub v_dc_pu: f64,
    #[pyo3(get)]
    pub converter_loss_mw: f64,
    #[pyo3(get)]
    pub converged: bool,
    pub lcc_detail_data: Option<HvdcLccDetail>,
}

impl HvdcStationSolution {
    pub(crate) fn from_inner(station: surge_hvdc::HvdcStationSolution) -> Self {
        Self {
            name: station.name,
            technology: match station.technology {
                surge_hvdc::HvdcTechnology::Lcc => "lcc".to_string(),
                surge_hvdc::HvdcTechnology::Vsc => "vsc".to_string(),
            },
            ac_bus: station.ac_bus,
            dc_bus: station.dc_bus,
            p_ac_mw: station.p_ac_mw,
            q_ac_mvar: station.q_ac_mvar,
            p_dc_mw: station.p_dc_mw,
            v_dc_pu: station.v_dc_pu,
            converter_loss_mw: station.converter_loss_mw,
            converged: station.converged,
            lcc_detail_data: station.lcc_detail.map(HvdcLccDetail::from_inner),
        }
    }
}

#[pymethods]
impl HvdcStationSolution {
    #[getter]
    fn lcc_detail(&self) -> Option<HvdcLccDetail> {
        self.lcc_detail_data.clone()
    }

    #[getter]
    fn power_balance_error_mw(&self) -> f64 {
        (self.p_ac_mw + self.p_dc_mw + self.converter_loss_mw).abs()
    }

    fn __repr__(&self) -> String {
        format!(
            "HvdcStationSolution(technology='{}', ac_bus={}, dc_bus={:?}, p_ac_mw={:.2}, q_ac_mvar={:.2}, p_dc_mw={:.2}, v_dc_pu={:.4})",
            self.technology,
            self.ac_bus,
            self.dc_bus,
            self.p_ac_mw,
            self.q_ac_mvar,
            self.p_dc_mw,
            self.v_dc_pu
        )
    }
}

#[pyclass(name = "HvdcDcBusSolution", skip_from_py_object)]
#[derive(Clone)]
pub struct HvdcDcBusSolution {
    #[pyo3(get)]
    pub dc_bus: u32,
    #[pyo3(get)]
    pub voltage_pu: f64,
}

impl HvdcDcBusSolution {
    pub(crate) fn from_inner(bus: surge_hvdc::HvdcDcBusSolution) -> Self {
        Self {
            dc_bus: bus.dc_bus,
            voltage_pu: bus.voltage_pu,
        }
    }
}

#[pymethods]
impl HvdcDcBusSolution {
    fn __repr__(&self) -> String {
        format!(
            "HvdcDcBusSolution(dc_bus={}, voltage_pu={:.4})",
            self.dc_bus, self.voltage_pu
        )
    }
}

#[pyclass(name = "HvdcResult", skip_from_py_object)]
#[derive(Clone)]
pub struct HvdcSolution {
    pub(crate) stations_data: Vec<HvdcStationSolution>,
    pub(crate) dc_buses_data: Vec<HvdcDcBusSolution>,
    #[pyo3(get)]
    pub(crate) total_converter_loss_mw: f64,
    #[pyo3(get)]
    pub(crate) total_dc_network_loss_mw: f64,
    #[pyo3(get)]
    pub(crate) total_loss_mw: f64,
    #[pyo3(get)]
    pub(crate) iterations: u32,
    #[pyo3(get)]
    pub(crate) converged: bool,
    #[pyo3(get)]
    pub(crate) method: String,
}

impl HvdcSolution {
    pub(crate) fn from_inner(solution: surge_hvdc::HvdcSolution) -> Self {
        Self {
            stations_data: solution
                .stations
                .into_iter()
                .map(HvdcStationSolution::from_inner)
                .collect(),
            dc_buses_data: solution
                .dc_buses
                .into_iter()
                .map(HvdcDcBusSolution::from_inner)
                .collect(),
            total_converter_loss_mw: solution.total_converter_loss_mw,
            total_dc_network_loss_mw: solution.total_dc_network_loss_mw,
            total_loss_mw: solution.total_loss_mw,
            iterations: solution.iterations,
            converged: solution.converged,
            method: match solution.method {
                surge_hvdc::HvdcMethod::Auto => "auto",
                surge_hvdc::HvdcMethod::Sequential => "sequential",
                surge_hvdc::HvdcMethod::BlockCoupled => "block_coupled",
                surge_hvdc::HvdcMethod::Hybrid => "hybrid",
            }
            .to_string(),
        }
    }
}

#[pymethods]
impl HvdcSolution {
    #[getter]
    fn stations(&self) -> Vec<HvdcStationSolution> {
        self.stations_data.clone()
    }

    #[getter]
    fn dc_buses(&self) -> Vec<HvdcDcBusSolution> {
        self.dc_buses_data.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "HvdcResult(converged={}, iterations={}, method='{}', stations={}, dc_buses={})",
            self.converged,
            self.iterations,
            self.method,
            self.stations_data.len(),
            self.dc_buses_data.len()
        )
    }
}
