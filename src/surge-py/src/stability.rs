// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python bindings for voltage stress, FDPF, and cascade analysis.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::exceptions::{catch_panic, to_pyerr};
use crate::network::Network;
use crate::solutions::AcPfResult;
use crate::utils::dict_to_dataframe_with_index;

// ---------------------------------------------------------------------------
// CTG-04: Base-case voltage stress
// ---------------------------------------------------------------------------

#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct VoltageStressBus {
    pub inner: surge_contingency::BusVoltageStress,
}

#[pymethods]
impl VoltageStressBus {
    #[getter]
    fn bus_number(&self) -> u32 {
        self.inner.bus_number
    }

    #[getter]
    fn local_qv_stress_proxy(&self) -> Option<f64> {
        self.inner.local_qv_stress_proxy
    }

    #[getter]
    fn exact_l_index(&self) -> Option<f64> {
        self.inner.exact_l_index
    }

    #[getter]
    fn voltage_margin_to_vmin(&self) -> f64 {
        self.inner.voltage_margin_to_vmin
    }

    fn __repr__(&self) -> String {
        format!(
            "VoltageStressBus(bus_number={}, exact_l_index={:?}, local_qv_stress_proxy={:?})",
            self.inner.bus_number, self.inner.exact_l_index, self.inner.local_qv_stress_proxy
        )
    }
}

#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct VoltageStressOptions {
    #[pyo3(get, set)]
    mode: String,
    #[pyo3(get, set)]
    l_index_threshold: f64,
    #[pyo3(get, set)]
    tolerance: f64,
    #[pyo3(get, set)]
    max_iterations: u32,
    #[pyo3(get, set)]
    flat_start: bool,
    #[pyo3(get, set)]
    dc_warm_start: bool,
    #[pyo3(get, set)]
    enforce_q_limits: bool,
    #[pyo3(get, set)]
    vm_min: f64,
    #[pyo3(get, set)]
    vm_max: f64,
}

impl Default for VoltageStressOptions {
    fn default() -> Self {
        Self {
            mode: "exact_l_index".to_string(),
            l_index_threshold: 0.7,
            tolerance: 1e-6,
            max_iterations: 50,
            flat_start: false,
            dc_warm_start: true,
            enforce_q_limits: false,
            vm_min: 0.5,
            vm_max: 2.0,
        }
    }
}

impl VoltageStressOptions {
    fn to_rust(&self) -> PyResult<surge_contingency::VoltageStressOptions> {
        let mode = match self.mode.as_str() {
            "off" => surge_contingency::VoltageStressMode::Off,
            "proxy" => surge_contingency::VoltageStressMode::Proxy,
            "exact_l_index" => surge_contingency::VoltageStressMode::ExactLIndex {
                l_index_threshold: self.l_index_threshold,
            },
            other => {
                return Err(PyValueError::new_err(format!(
                    "mode must be 'off', 'proxy', or 'exact_l_index', got '{other}'"
                )));
            }
        };

        Ok(surge_contingency::VoltageStressOptions {
            acpf_options: surge_ac::AcPfOptions {
                tolerance: self.tolerance,
                max_iterations: self.max_iterations,
                flat_start: self.flat_start,
                dc_warm_start: self.dc_warm_start,
                enforce_q_limits: self.enforce_q_limits,
                vm_min: self.vm_min,
                vm_max: self.vm_max,
                ..Default::default()
            },
            mode,
        })
    }
}

#[pymethods]
impl VoltageStressOptions {
    #[new]
    #[pyo3(signature = (
        mode = "exact_l_index",
        l_index_threshold = 0.7,
        tolerance = 1e-6,
        max_iterations = 50,
        flat_start = false,
        dc_warm_start = true,
        enforce_q_limits = false,
        vm_min = 0.5,
        vm_max = 2.0,
    ))]
    fn new(
        mode: &str,
        l_index_threshold: f64,
        tolerance: f64,
        max_iterations: u32,
        flat_start: bool,
        dc_warm_start: bool,
        enforce_q_limits: bool,
        vm_min: f64,
        vm_max: f64,
    ) -> Self {
        Self {
            mode: mode.to_string(),
            l_index_threshold,
            tolerance,
            max_iterations,
            flat_start,
            dc_warm_start,
            enforce_q_limits,
            vm_min,
            vm_max,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "VoltageStressOptions(mode='{}', l_index_threshold={:.2}, tolerance={:.1e})",
            self.mode, self.l_index_threshold, self.tolerance
        )
    }
}

#[pyclass]
pub struct VoltageStressResult {
    pub inner: surge_contingency::VoltageStressResult,
}

#[pymethods]
impl VoltageStressResult {
    #[getter]
    fn per_bus<'py>(&self, py: Python<'py>) -> PyResult<Vec<Py<VoltageStressBus>>> {
        self.inner
            .per_bus
            .iter()
            .cloned()
            .map(|inner| Py::new(py, VoltageStressBus { inner }))
            .collect()
    }

    #[getter]
    fn max_qv_stress_proxy(&self) -> Option<f64> {
        self.inner.max_qv_stress_proxy
    }

    #[getter]
    fn critical_proxy_bus(&self) -> Option<u32> {
        self.inner.critical_proxy_bus
    }

    #[getter]
    fn max_l_index(&self) -> Option<f64> {
        self.inner.max_l_index
    }

    #[getter]
    fn critical_l_index_bus(&self) -> Option<u32> {
        self.inner.critical_l_index_bus
    }

    #[getter]
    fn category(&self) -> Option<String> {
        self.inner.category.map(|category| match category {
            surge_contingency::VsmCategory::Secure => "secure".to_string(),
            surge_contingency::VsmCategory::Marginal => "marginal".to_string(),
            surge_contingency::VsmCategory::Critical => "critical".to_string(),
            surge_contingency::VsmCategory::Unstable => "unstable".to_string(),
        })
    }

    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<pyo3::Bound<'py, pyo3::PyAny>> {
        let dict = PyDict::new(py);
        dict.set_item(
            "bus_id",
            self.inner
                .per_bus
                .iter()
                .map(|bus| bus.bus_number)
                .collect::<Vec<_>>(),
        )?;
        dict.set_item(
            "local_qv_stress_proxy",
            self.inner
                .per_bus
                .iter()
                .map(|bus| bus.local_qv_stress_proxy.unwrap_or(f64::NAN))
                .collect::<Vec<_>>(),
        )?;
        dict.set_item(
            "exact_l_index",
            self.inner
                .per_bus
                .iter()
                .map(|bus| bus.exact_l_index.unwrap_or(f64::NAN))
                .collect::<Vec<_>>(),
        )?;
        dict.set_item(
            "voltage_margin_to_vmin",
            self.inner
                .per_bus
                .iter()
                .map(|bus| bus.voltage_margin_to_vmin)
                .collect::<Vec<_>>(),
        )?;
        dict_to_dataframe_with_index(py, dict, &["bus_id"])
    }

    fn __repr__(&self) -> String {
        format!(
            "VoltageStressResult(max_l_index={:?}, category={:?})",
            self.inner.max_l_index,
            self.category()
        )
    }
}

/// Compute base-case voltage stress for a network.
///
/// Runs AC Newton-Raphson power flow and returns the exact/proxy result shape
/// used by contingency analysis. By default this uses the exact L-index mode.
#[pyfunction]
#[pyo3(signature = (network, options = None))]
pub fn compute_voltage_stress(
    network: &Network,
    options: Option<VoltageStressOptions>,
) -> PyResult<VoltageStressResult> {
    catch_panic("compute_voltage_stress", || {
        let net = Arc::clone(&network.inner);
        let options = options.unwrap_or_default().to_rust()?;
        let inner =
            surge_contingency::voltage::compute_voltage_stress(&net, &options).map_err(to_pyerr)?;
        Ok(VoltageStressResult { inner })
    })
}

// ---------------------------------------------------------------------------
// Fast Decoupled Power Flow
// ---------------------------------------------------------------------------

/// Solve Fast Decoupled Power Flow.
///
/// Args:
///     network:          Power system network.
///     tolerance:        Convergence tolerance (p.u. mismatch), default 1e-6.
///     max_iterations:   Maximum iterations, default 100.
///     flat_start:       If True (default), start from Vm=1.0, Va=0.0.
///                       If False, initialise from case data (PV/slack setpoints).
///     variant:          FDPF variant: ``"xb"`` (default) or ``"bx"``.
///     enforce_q_limits: Enforce generator reactive power limits (PV→PQ switching).
///
/// Returns:
///     AcPfResult with converged flag, iterations, vm, va, solve_time_secs.
#[pyfunction]
#[pyo3(signature = (
    network,
    tolerance = 1e-6,
    max_iterations = 100,
    flat_start = true,
    variant = "xb",
    enforce_q_limits = true,
))]
pub fn solve_fdpf(
    network: &Network,
    tolerance: f64,
    max_iterations: u32,
    flat_start: bool,
    variant: &str,
    enforce_q_limits: bool,
) -> PyResult<AcPfResult> {
    catch_panic("solve_fdpf", || {
        if !tolerance.is_finite() || tolerance <= 0.0 {
            return Err(PyValueError::new_err(format!(
                "tolerance must be a finite positive number, got {tolerance}"
            )));
        }
        if max_iterations > 10_000 {
            return Err(PyValueError::new_err(format!(
                "max_iterations={max_iterations} exceeds limit of 10,000"
            )));
        }
        network.validate()?;

        let net = Arc::clone(&network.inner);

        let fdpf_opts = surge_ac::FdpfOptions {
            tolerance,
            max_iterations,
            flat_start,
            variant: match variant {
                "bx" | "BX" | "Bx" => surge_ac::FdpfVariant::Bx,
                "xb" | "XB" | "Xb" => surge_ac::FdpfVariant::Xb,
                other => {
                    return Err(PyValueError::new_err(format!(
                        "variant must be 'xb' or 'bx', got '{other}'"
                    )));
                }
            },
            enforce_q_limits,
            ..Default::default()
        };
        let inner = surge_ac::solve_fdpf(&net, &fdpf_opts)
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        Ok(AcPfResult {
            inner,
            net: Some(net),
        })
    })
}

// ---------------------------------------------------------------------------
// Cascade analysis bindings (Zone-3 relay cascade + OPA Monte Carlo)
// ---------------------------------------------------------------------------

/// Options for Zone-3 relay cascade simulation.
///
/// Controls the relay pickup threshold, trip delay, cascade depth limit,
/// and blackout detection fraction.
#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct CascadeOptions {
    inner: surge_contingency::advanced::cascade::CascadeOptions,
}

#[pymethods]
impl CascadeOptions {
    /// Create cascade simulation options.
    ///
    /// Args:
    ///     z3_pickup_fraction: Zone 3 pickup fraction of thermal rating (default 0.8).
    ///     z3_delay_s: Zone 3 trip delay in seconds (default 1.0).
    ///     max_cascade_levels: Maximum cascade depth before stopping (default 5).
    ///     blackout_fraction: Load-interruption fraction to declare blackout (default 0.5).
    ///     thermal_rating: Rating tier: 'rate_a' (default), 'rate_b', or 'rate_c'.
    #[new]
    #[pyo3(signature = (
        z3_pickup_fraction = 0.8,
        z3_delay_s = 1.0,
        max_cascade_levels = 5,
        blackout_fraction = 0.5,
        thermal_rating = None,
    ))]
    fn new(
        z3_pickup_fraction: f64,
        z3_delay_s: f64,
        max_cascade_levels: u32,
        blackout_fraction: f64,
        thermal_rating: Option<&str>,
    ) -> PyResult<Self> {
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
        Ok(Self {
            inner: surge_contingency::advanced::cascade::CascadeOptions {
                z3_pickup_fraction,
                z3_delay_s,
                max_cascade_levels,
                blackout_fraction,
                thermal_rating: rating,
            },
        })
    }

    #[getter]
    fn z3_pickup_fraction(&self) -> f64 {
        self.inner.z3_pickup_fraction
    }
    #[getter]
    fn z3_delay_s(&self) -> f64 {
        self.inner.z3_delay_s
    }
    #[getter]
    fn max_cascade_levels(&self) -> u32 {
        self.inner.max_cascade_levels
    }
    #[getter]
    fn blackout_fraction(&self) -> f64 {
        self.inner.blackout_fraction
    }
    #[getter]
    fn thermal_rating(&self) -> &str {
        match self.inner.thermal_rating {
            surge_contingency::ThermalRating::RateA => "rate_a",
            surge_contingency::ThermalRating::RateB => "rate_b",
            surge_contingency::ThermalRating::RateC => "rate_c",
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "CascadeOptions(z3_pickup={:.2}, delay={:.1}s, max_levels={}, blackout_frac={:.2})",
            self.inner.z3_pickup_fraction,
            self.inner.z3_delay_s,
            self.inner.max_cascade_levels,
            self.inner.blackout_fraction,
        )
    }
}

/// A single trip event in a relay cascade sequence.
#[pyclass(name = "CascadeEvent", skip_from_py_object)]
#[derive(Clone)]
pub struct CascadeEvent {
    inner: surge_contingency::advanced::cascade::CascadeEvent,
}

#[pymethods]
impl CascadeEvent {
    /// Cascade level at which this trip occurred (0 = initiating event).
    #[getter]
    fn cascade_level(&self) -> u32 {
        self.inner.cascade_level
    }

    /// Internal 0-based index of the tripped branch.
    #[getter]
    fn branch_index(&self) -> usize {
        self.inner.tripped_branch_index
    }

    /// Human-readable branch label ("from_bus->to_bus").
    #[getter]
    fn branch_label(&self) -> &str {
        &self.inner.branch_label
    }

    /// Branch flow in MW immediately before the trip.
    #[getter]
    fn flow_mw_before(&self) -> f64 {
        self.inner.flow_before_trip_mw
    }

    /// Thermal rating of the tripped branch in MW.
    #[getter]
    fn rating_mw(&self) -> f64 {
        self.inner.rating_mw
    }

    /// Simulation time in seconds when the trip occurred.
    #[getter]
    fn time_s(&self) -> f64 {
        self.inner.time_s
    }

    /// Cause of the trip: "initial", "zone3_relay", or "zone2_relay".
    #[getter]
    fn cause(&self) -> &str {
        match self.inner.cause {
            surge_contingency::advanced::cascade::CascadeCause::Initial => "initial",
            surge_contingency::advanced::cascade::CascadeCause::Zone3Relay => "zone3_relay",
            surge_contingency::advanced::cascade::CascadeCause::Zone2Relay => "zone2_relay",
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "CascadeEvent(level={}, branch={} [{}], cause={}, flow={:.1} MW)",
            self.inner.cascade_level,
            self.inner.tripped_branch_index,
            self.inner.branch_label,
            self.cause(),
            self.inner.flow_before_trip_mw,
        )
    }
}

/// Result of a Zone-3 relay cascade simulation for one initiating contingency.
#[pyclass(name = "CascadeResult", skip_from_py_object)]
#[derive(Clone)]
pub struct CascadeResult {
    inner: surge_contingency::advanced::cascade::CascadeResult,
}

#[pymethods]
impl CascadeResult {
    /// Internal index of the branch that initiated the cascade.
    #[getter]
    fn initiating_branch(&self) -> usize {
        self.inner.initiating_contingency
    }

    /// Ordered list of trip events from the cascade sequence.
    #[getter]
    fn events(&self) -> Vec<CascadeEvent> {
        self.inner
            .cascade_events
            .iter()
            .map(|e| CascadeEvent { inner: e.clone() })
            .collect()
    }

    /// Depth of the cascade (number of levels beyond the initiating event).
    #[getter]
    fn cascade_depth(&self) -> u32 {
        self.inner.cascade_depth
    }

    /// Total load interrupted in MW.
    #[getter]
    fn total_load_interrupted_mw(&self) -> f64 {
        self.inner.total_load_interrupted_mw
    }

    /// True if load interrupted exceeds the blackout fraction threshold.
    #[getter]
    fn blackout(&self) -> bool {
        self.inner.blackout
    }

    fn __repr__(&self) -> String {
        format!(
            "CascadeResult(branch={}, depth={}, load_interrupted={:.1} MW, blackout={})",
            self.inner.initiating_contingency,
            self.inner.cascade_depth,
            self.inner.total_load_interrupted_mw,
            self.inner.blackout,
        )
    }
}

/// Options for OPA Monte Carlo cascading failure simulation.
#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct OpaOptions {
    inner: surge_contingency::advanced::opa_cascade::OpaOptions,
}

#[pymethods]
impl OpaOptions {
    /// Create OPA cascade simulation options.
    ///
    /// Args:
    ///     beta: Overload-to-trip probability exponent (default 2.0).
    ///     max_steps: Maximum simulation steps per trial (default 100).
    ///     n_trials: Number of Monte Carlo trials (default 1000).
    ///     seed: Random seed for reproducibility (default None = fixed seed).
    ///     thermal_rating: Rating tier: 'rate_a' (default), 'rate_b', or 'rate_c'.
    #[new]
    #[pyo3(signature = (
        beta = 2.0,
        max_steps = 100,
        n_trials = 1000,
        seed = None,
        thermal_rating = None,
    ))]
    fn new(
        beta: f64,
        max_steps: u32,
        n_trials: u32,
        seed: Option<u64>,
        thermal_rating: Option<&str>,
    ) -> PyResult<Self> {
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
        Ok(Self {
            inner: surge_contingency::advanced::opa_cascade::OpaOptions {
                beta,
                max_steps,
                n_trials,
                seed,
                thermal_rating: rating,
            },
        })
    }

    #[getter]
    fn beta(&self) -> f64 {
        self.inner.beta
    }
    #[getter]
    fn max_steps(&self) -> u32 {
        self.inner.max_steps
    }
    #[getter]
    fn n_trials(&self) -> u32 {
        self.inner.n_trials
    }
    #[getter]
    fn seed(&self) -> Option<u64> {
        self.inner.seed
    }
    #[getter]
    fn thermal_rating(&self) -> &str {
        match self.inner.thermal_rating {
            surge_contingency::ThermalRating::RateA => "rate_a",
            surge_contingency::ThermalRating::RateB => "rate_b",
            surge_contingency::ThermalRating::RateC => "rate_c",
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "OpaOptions(beta={:.1}, max_steps={}, n_trials={}, seed={:?})",
            self.inner.beta, self.inner.max_steps, self.inner.n_trials, self.inner.seed,
        )
    }
}

/// Aggregate results from OPA Monte Carlo cascading failure simulation.
#[pyclass(name = "OpaCascadeResult", skip_from_py_object)]
#[derive(Clone)]
pub struct OpaCascadeResult {
    inner: surge_contingency::advanced::opa_cascade::OpaCascadeResult,
}

#[pymethods]
impl OpaCascadeResult {
    /// Mean load shed in MW across all trials.
    #[getter]
    fn mean_load_shed_mw(&self) -> f64 {
        self.inner.mean_load_shed_mw
    }

    /// Standard deviation of load shed in MW.
    #[getter]
    fn std_load_shed_mw(&self) -> f64 {
        self.inner.std_load_shed_mw
    }

    /// Probability of a large cascade (shed >= 50% of total load).
    #[getter]
    fn blackout_probability(&self) -> f64 {
        self.inner.p_blackout
    }

    /// Empirical CDF of load-shed fraction.
    ///
    /// List of (load_shed_fraction, cumulative_probability) tuples,
    /// sorted by load_shed_fraction ascending.
    #[getter]
    fn load_shed_cdf(&self) -> Vec<(f64, f64)> {
        self.inner.cascade_size_distribution.clone()
    }

    /// Most critical branches ranked by expected load shed contribution.
    ///
    /// List of (branch_index, expected_load_shed_mw) tuples, sorted
    /// descending by expected load shed. Capped at 20 entries.
    #[getter]
    fn critical_branches(&self) -> Vec<(usize, f64)> {
        self.inner.most_critical_branches.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "OpaCascadeResult(mean_shed={:.1} MW, std={:.1} MW, p_blackout={:.3})",
            self.inner.mean_load_shed_mw, self.inner.std_load_shed_mw, self.inner.p_blackout,
        )
    }
}

/// Simulate a Zone-3 relay cascade for a single initiating branch outage.
///
/// Uses a prepared DC model and single-outage LODF columns to screen the
/// cascade progression. This is a first-order relay-cascade approximation, not
/// a full topology rebuild after every trip.
///
/// Args:
///     network: Power system network.
///     initiating_branch: 0-based index of the branch to outage.
///     options: CascadeOptions (default: z3_pickup=0.8, delay=1s, max_levels=5).
///
/// Returns:
///     CascadeResult with the full cascade event sequence.
///
/// Raises:
///     SurgeError: If relay-cascade preparation or simulation fails.
#[pyfunction]
#[pyo3(name = "analyze_cascade", signature = (network, initiating_branch, options=None))]
pub fn simulate_cascade_py(
    network: &Network,
    initiating_branch: usize,
    options: Option<&CascadeOptions>,
) -> PyResult<CascadeResult> {
    catch_panic("analyze_cascade", || {
        let net = (*network.inner).clone();
        net.validate().map_err(to_pyerr)?;

        let n_br = net.n_branches();
        if initiating_branch >= n_br {
            return Err(PyValueError::new_err(format!(
                "initiating_branch={initiating_branch} out of range (network has {n_br} branches)"
            )));
        }

        let opts = options.map(|o| o.inner.clone()).unwrap_or_default();

        let result =
            surge_contingency::advanced::cascade::simulate_cascade(&net, initiating_branch, &opts)
                .map_err(to_pyerr)?;

        Ok(CascadeResult { inner: result })
    })
}

/// Screen all branches for cascade risk and return the most severe results.
///
/// Reuses one prepared relay-cascade model for every in-service branch with a
/// valid thermal rating. Results are sorted by severity (cascade depth
/// descending, then load interrupted descending).
///
/// Args:
///     network: Power system network.
///     options: CascadeOptions (default settings if None).
///     top_n: Return only the top N most severe cascades (default 10).
///
/// Returns:
///     List of CascadeResult, sorted most-severe first, capped at top_n.
///
/// Raises:
///     SurgeError: If relay-cascade preparation or simulation fails.
#[pyfunction]
#[pyo3(signature = (network, options=None, top_n=10))]
pub fn analyze_cascade_screening(
    network: &Network,
    options: Option<&CascadeOptions>,
    top_n: usize,
) -> PyResult<Vec<CascadeResult>> {
    catch_panic("analyze_cascade_screening", || {
        let net = (*network.inner).clone();
        net.validate().map_err(to_pyerr)?;

        let opts = options.map(|o| o.inner.clone()).unwrap_or_default();

        let results =
            surge_contingency::advanced::cascade::analyze_cascade(&net, &opts).map_err(to_pyerr)?;

        Ok(results
            .into_iter()
            .take(top_n)
            .map(|r| CascadeResult { inner: r })
            .collect())
    })
}

/// Run OPA Monte Carlo cascading failure simulation.
///
/// Performs probabilistic cascade analysis using the OPA model. Internally
/// solves DC power flow, computes PTDF/LODF, then runs `n_trials` Monte
/// Carlo trials with probabilistic relay tripping.
///
/// Args:
///     network: Power system network.
///     initial_outages: List of 0-based branch indices to outage at time 0.
///     options: OpaOptions (default: beta=2.0, max_steps=100, n_trials=1000).
///
/// Returns:
///     OpaCascadeResult with mean/std load shed, blackout probability,
///     empirical CDF, and critical branch ranking.
///
/// Raises:
///     SurgeError: If inputs are invalid or DC power flow fails.
#[pyfunction]
#[pyo3(name = "analyze_opa_cascade", signature = (network, initial_outages, options=None))]
pub fn analyze_opa_cascade_py(
    network: &Network,
    initial_outages: Vec<usize>,
    options: Option<&OpaOptions>,
) -> PyResult<OpaCascadeResult> {
    catch_panic("analyze_opa_cascade", || {
        let net = (*network.inner).clone();
        net.validate().map_err(to_pyerr)?;

        let opts = options.map(|o| o.inner.clone()).unwrap_or_default();

        let result = surge_contingency::advanced::opa_cascade::analyze_opa_cascade(
            &net,
            &initial_outages,
            &opts,
        )
        .map_err(to_pyerr)?;

        Ok(OpaCascadeResult { inner: result })
    })
}
