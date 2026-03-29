// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DataFrame helper, logging, thread pool, and blocking helpers.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

// ---------------------------------------------------------------------------
// Helper: convert a PyDict to a pandas DataFrame if pandas is available.
// Falls back to returning the dict if pandas is not installed.
// ---------------------------------------------------------------------------
pub fn dict_to_dataframe<'py>(
    py: Python<'py>,
    dict: Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    match py.import("pandas") {
        Ok(pd) => pd.call_method1("DataFrame", (dict,)),
        Err(_) => Ok(dict.into_any()),
    }
}

/// Convert a column dict to a pandas DataFrame and set its index when pandas is
/// available. Without pandas, return the original dict unchanged.
pub fn dict_to_dataframe_with_index<'py>(
    py: Python<'py>,
    dict: Bound<'py, PyDict>,
    index_cols: &[&str],
) -> PyResult<Bound<'py, PyAny>> {
    let df = dict_to_dataframe(py, dict)?;
    if !df.hasattr("set_index")? || index_cols.is_empty() {
        return Ok(df);
    }

    if index_cols.len() == 1 {
        df.call_method1("set_index", (index_cols[0],))
    } else {
        let index = PyList::new(py, index_cols)?;
        df.call_method1("set_index", (index,))
    }
}

// ---------------------------------------------------------------------------
// Logging initialization for Python callers
// ---------------------------------------------------------------------------

/// Initialize Rust-side logging so that tracing output from the solver is
/// printed to stderr.  Call this once before calling any solver functions.
///
/// Arguments:
///   level: "error", "warn", "info", "debug", or "trace" (default "warn").
///          Also respects the RUST_LOG environment variable, which overrides
///          the `level` argument.
///   json:  If True, log in machine-readable JSON format.
///
/// Example:
///   surge.init_logging("info")
///   surge.init_logging("debug", json=True)
#[pyfunction]
#[pyo3(signature = (level="warn", json=false))]
pub fn init_logging(level: &str, json: bool) -> PyResult<()> {
    use std::sync::Once;
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let env_filter = if std::env::var("RUST_LOG").is_ok() {
            tracing_subscriber::EnvFilter::from_default_env()
        } else {
            tracing_subscriber::EnvFilter::new(level)
        };

        if json {
            let _ = tracing_subscriber::fmt()
                .json()
                .with_env_filter(env_filter)
                .with_target(true)
                .try_init();
        } else {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_target(false)
                .try_init();
        }
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// Rayon thread-pool configuration
// ---------------------------------------------------------------------------

/// Desired thread count for surge parallel operations; 0 = rayon default (logical CPUs).
static SURGE_NUM_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Build a rayon thread pool sized to the value set by ``set_max_threads``.
///
/// If ``set_max_threads`` was never called, uses rayon's default (logical CPUs).
/// Each major parallel entry point (``analyze_n1_branch``, ``analyze_n2_branch``,
/// ``parameter_sweep``) builds its own scoped pool so the limit takes effect
/// even after the first call.
pub(crate) fn make_thread_pool() -> PyResult<rayon::ThreadPool> {
    let n = SURGE_NUM_THREADS.load(std::sync::atomic::Ordering::Relaxed);
    let mut builder = rayon::ThreadPoolBuilder::new();
    if n > 0 {
        builder = builder.num_threads(n);
    }
    builder.build().map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("failed to build thread pool: {e}"))
    })
}

/// Set the maximum number of threads used for parallel computation.
///
/// Unlike the previous implementation which configured rayon's global pool
/// (and silently had no effect after the first parallel call), this stores
/// the requested count and each parallel function (``analyze_n1_branch``,
/// ``analyze_n2_branch``, ``parameter_sweep``) builds a scoped thread pool with
/// that count. The setting takes effect on the **next** parallel call
/// regardless of when it is made.
///
/// Arguments:
///   n: Maximum number of worker threads (must be >= 1).
///
/// Example::
///
///   import surge
///   surge.set_max_threads(4)
///   result = surge.analyze_n1_branch(net)  # uses 4 threads
///   surge.set_max_threads(2)
///   result2 = surge.analyze_n1_branch(net)  # now uses 2 threads
#[pyfunction]
pub fn set_max_threads(n: usize) -> PyResult<()> {
    if n == 0 {
        return Err(PyValueError::new_err("max_threads must be >= 1"));
    }
    SURGE_NUM_THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Return the configured thread count for surge parallel operations.
///
/// Returns the value set by ``set_max_threads``, or the number of logical
/// CPUs if ``set_max_threads`` was never called.
#[pyfunction]
pub fn get_max_threads() -> usize {
    let n = SURGE_NUM_THREADS.load(std::sync::atomic::Ordering::Relaxed);
    if n > 0 {
        n
    } else {
        rayon::current_num_threads()
    }
}
