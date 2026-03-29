// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python binding for [`PreparedAcPf`] — cached AC power flow solver.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::exceptions::to_pyerr;
use crate::network::Network;
use crate::solutions::AcPfResult;

/// Cached AC power flow solver for repeated solves on the same network.
///
/// Pre-computes the Y-bus admittance matrix, KLU symbolic factorization, and
/// NR workspace on construction.  Subsequent solves reuse all cached structures
/// — only the numerical re-factorization and Newton-Raphson iterations run.
///
/// Ideal for contingency screening, time-series studies, and parameter sweeps
/// where the network topology is fixed but the operating point changes.
///
/// **Limitations**: outer loops (OLTC, switched shunts, PAR, Q-limit switching,
/// island detection) are not supported.  Use :func:`solve_ac_pf` when those are
/// needed.
///
/// Example::
///
///     from surge import load_matpower, PreparedAcPf
///     net = load_matpower("case118.m")
///     solver = PreparedAcPf(net)
///     sol1 = solver.solve()           # case-data init
///     sol2 = solver.solve_with_warm_start(sol1)   # warm-start from prior
///     sol3 = solver.solve_with_flat_start()       # flat start
#[pyclass(unsendable)]
pub struct PreparedAcPf {
    inner: surge_ac::PreparedAcPf,
    net: Arc<surge_network::Network>,
}

#[pymethods]
impl PreparedAcPf {
    /// Create a new cached solver for the given network.
    ///
    /// Args:
    ///     network:                    The power system network to solve repeatedly.
    ///     tolerance:                  Convergence tolerance (max mismatch p.u.). Default 1e-8.
    ///     max_iterations:             Maximum Newton-Raphson iterations. Default 100.
    ///     line_search:                Enable backtracking line search. Default True.
    ///     dc_warm_start:              Use DC power flow angles for flat-start init. Default True.
    ///     record_convergence_history: Record per-iteration mismatch. Default False.
    #[new]
    #[pyo3(signature = (
        network,
        tolerance = 1e-8,
        max_iterations = 100,
        line_search = true,
        dc_warm_start = true,
        record_convergence_history = false,
    ))]
    fn new(
        network: &Network,
        tolerance: f64,
        max_iterations: u32,
        line_search: bool,
        dc_warm_start: bool,
        record_convergence_history: bool,
    ) -> PyResult<Self> {
        if !tolerance.is_finite() || tolerance <= 0.0 {
            return Err(PyValueError::new_err(format!(
                "tolerance must be a finite positive number, got {tolerance}"
            )));
        }
        network.validate()?;

        let net = Arc::clone(&network.inner);
        let opts = surge_ac::AcPfOptions {
            tolerance,
            max_iterations,
            line_search,
            dc_warm_start,
            record_convergence_history,
            // PreparedAcPf requires these to be off:
            enforce_q_limits: false,
            detect_islands: false,
            auto_merge_zero_impedance: false,
            auto_reduce_topology: false,
            oltc_enabled: false,
            par_enabled: false,
            shunt_enabled: false,
            enforce_interchange: false,
            startup_policy: surge_ac::StartupPolicy::Single,
            ..Default::default()
        };

        let inner = surge_ac::PreparedAcPf::new(Arc::clone(&net), &opts)
            .map_err(|e| to_pyerr(e.to_string()))?;

        Ok(Self { inner, net })
    }

    /// Solve from case-data initial conditions.
    fn solve(&mut self) -> PyResult<AcPfResult> {
        let inner = self.inner.solve().map_err(|e| to_pyerr(e.to_string()))?;
        Ok(AcPfResult {
            inner,
            net: Some(Arc::clone(&self.net)),
        })
    }

    /// Solve warm-started from a prior solution.
    fn solve_with_warm_start(&mut self, prior: &AcPfResult) -> PyResult<AcPfResult> {
        let ws = surge_ac::WarmStart::from_solution(&prior.inner);
        let inner = self
            .inner
            .solve_with_start(surge_ac::PreparedStart::Warm(&ws))
            .map_err(|e| to_pyerr(e.to_string()))?;
        Ok(AcPfResult {
            inner,
            net: Some(Arc::clone(&self.net)),
        })
    }

    /// Solve from flat start (Vm=1.0, Va=0.0).
    fn solve_with_flat_start(&mut self) -> PyResult<AcPfResult> {
        let inner = self
            .inner
            .solve_with_start(surge_ac::PreparedStart::Flat)
            .map_err(|e| to_pyerr(e.to_string()))?;
        Ok(AcPfResult {
            inner,
            net: Some(Arc::clone(&self.net)),
        })
    }

    fn __repr__(&self) -> String {
        format!("PreparedAcPf(n_buses={})", self.net.n_buses())
    }
}
