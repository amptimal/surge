// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Custom exception hierarchy and panic-handling helpers for Surge Python bindings.

use pyo3::prelude::*;

// ---------------------------------------------------------------------------
// Custom exception hierarchy
// ---------------------------------------------------------------------------
pyo3::create_exception!(_surge, SurgeError, pyo3::exceptions::PyException);
pyo3::create_exception!(_surge, ConvergenceError, SurgeError);
pyo3::create_exception!(_surge, InfeasibleError, SurgeError);
pyo3::create_exception!(_surge, UnsupportedFeatureError, SurgeError);
pyo3::create_exception!(_surge, NetworkError, SurgeError);
pyo3::create_exception!(_surge, TopologyError, SurgeError);
pyo3::create_exception!(_surge, MissingTopologyError, TopologyError);
pyo3::create_exception!(_surge, StaleTopologyError, TopologyError);
pyo3::create_exception!(_surge, AmbiguousTopologyError, TopologyError);
pyo3::create_exception!(_surge, TopologyIntegrityError, TopologyError);
pyo3::create_exception!(_surge, SurgeIOError, SurgeError);

// ---------------------------------------------------------------------------
// Helper: convert Rust errors to Python exceptions
// ---------------------------------------------------------------------------
pub(crate) fn to_pyerr<E: std::fmt::Display>(e: E) -> PyErr {
    SurgeError::new_err(e.to_string())
}

pub(crate) fn to_io_pyerr<E: std::fmt::Display>(e: E) -> PyErr {
    SurgeIOError::new_err(e.to_string())
}

pub(crate) fn to_network_pyerr<E: std::fmt::Display>(e: E) -> PyErr {
    NetworkError::new_err(e.to_string())
}

pub(crate) fn to_topology_pyerr(error: &surge_topology::TopologyError) -> PyErr {
    use surge_topology::TopologyError as RustTopologyError;

    match error {
        RustTopologyError::NoNodeBreakerTopology | RustTopologyError::MissingTopologyMapping => {
            MissingTopologyError::new_err(error.to_string())
        }
        RustTopologyError::AmbiguousBusSplit { .. } => {
            AmbiguousTopologyError::new_err(error.to_string())
        }
        RustTopologyError::DuplicateConnectivityNode(_)
        | RustTopologyError::DuplicateVoltageLevel(_)
        | RustTopologyError::UnknownSwitchConnectivityNode { .. }
        | RustTopologyError::MissingVoltageLevel { .. }
        | RustTopologyError::DuplicateEquipmentTerminal { .. }
        | RustTopologyError::MissingBusMapping { .. }
        | RustTopologyError::InvalidBusIndex { .. } => {
            TopologyIntegrityError::new_err(error.to_string())
        }
    }
}

pub(crate) fn to_ac_opf_pyerr(error: &surge_opf::ac::types::AcOpfError) -> PyErr {
    use surge_opf::ac::types::AcOpfError;

    match error {
        AcOpfError::InvalidNetwork(_)
        | AcOpfError::NoSlackBus
        | AcOpfError::NoGenerators
        | AcOpfError::MissingCost { .. } => NetworkError::new_err(error.to_string()),
        AcOpfError::NotConverged => ConvergenceError::new_err(error.to_string()),
        AcOpfError::SolverError(_) => SurgeError::new_err(error.to_string()),
    }
}

pub(crate) fn to_dc_opf_pyerr(error: &surge_opf::dc::opf::DcOpfError) -> PyErr {
    use surge_opf::dc::opf::DcOpfError;

    match error {
        DcOpfError::InvalidNetwork(_)
        | DcOpfError::NoSlackBus
        | DcOpfError::NoGenerators
        | DcOpfError::MissingCost { .. }
        | DcOpfError::InvalidHvdcLink { .. } => NetworkError::new_err(error.to_string()),
        DcOpfError::NotConverged { .. } => ConvergenceError::new_err(error.to_string()),
        DcOpfError::InsufficientCapacity { .. } | DcOpfError::InfeasibleProblem => {
            InfeasibleError::new_err(error.to_string())
        }
        DcOpfError::SubOptimalSolution
        | DcOpfError::UnboundedProblem
        | DcOpfError::SolverError(_) => SurgeError::new_err(error.to_string()),
    }
}

pub(crate) fn to_scopf_pyerr(error: &surge_opf::security::types::ScopfError) -> PyErr {
    use surge_opf::security::types::ScopfError;

    match error {
        ScopfError::InvalidNetwork(_)
        | ScopfError::NoSlackBus
        | ScopfError::NoGenerators
        | ScopfError::MissingCost { .. }
        | ScopfError::InvalidHvdcLink { .. } => NetworkError::new_err(error.to_string()),
        ScopfError::NotConverged { .. } => ConvergenceError::new_err(error.to_string()),
        ScopfError::InsufficientCapacity { .. } | ScopfError::InfeasibleProblem => {
            InfeasibleError::new_err(error.to_string())
        }
        ScopfError::SubOptimalSolution
        | ScopfError::UnboundedProblem
        | ScopfError::UnsupportedCombination { .. }
        | ScopfError::UnsupportedSecurityConstraint { .. } => {
            UnsupportedFeatureError::new_err(error.to_string())
        }
        ScopfError::SolverError(_) => SurgeError::new_err(error.to_string()),
    }
}

/// Extract a human-readable message from a caught panic payload.
pub(crate) fn extract_panic_msg(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = panic.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown internal error".to_string()
    }
}

pub(crate) fn catch_panic<F, T>(name: &str, f: F) -> PyResult<T>
where
    F: FnOnce() -> PyResult<T>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(panic) => Err(SurgeError::new_err(format!(
            "{name} failed: {}",
            extract_panic_msg(panic)
        ))),
    }
}
