// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DataFrame helper, logging, thread pool, and blocking helpers.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};

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

// ---------------------------------------------------------------------------
// In-process tracing broadcast (deadlock-free Python log capture)
// ---------------------------------------------------------------------------
//
// Earlier revisions captured Rust ``tracing`` output by ``os.dup2``-ing
// stdout/stderr to a pipe (see ``src/surge-py/python/surge/market/
// logging.py::SolveLogger``). That works for foreground processes but
// deadlocks under FastAPI's worker thread when a high-volume run fills
// the 64 KB pipe buffer faster than the reader thread can drain it.
//
// The broadcast layer below sits inside the global tracing subscriber
// alongside the existing stderr fmt layer. Each formatted record is
// written to a process-wide registry of ``Sender<String>`` channels;
// Python attaches by calling :func:`attach_log_listener` (returns a
// handle) and detaches via :func:`detach_log_listener`. No fd
// manipulation, no pipe — log records flow through Rust channels.

#[derive(Default)]
struct ListenerRegistry {
    senders: HashMap<u64, mpsc::Sender<String>>,
}

fn listener_registry() -> &'static Mutex<ListenerRegistry> {
    static REGISTRY: OnceLock<Mutex<ListenerRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn next_listener_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Tee writer: each formatted ``tracing`` record is written to
/// stderr (the canonical sink) AND fanned out to every currently-
/// attached Python listener. Senders that fail to receive (closed
/// channel) are pruned on the next emit. Combining both sinks
/// behind a single ``MakeWriter`` keeps the ``tracing-subscriber``
/// layer stack flat — stacking two ``fmt::layer()``s with
/// different writers wedges the generic explosion in the trait
/// bounds.
#[derive(Clone, Copy, Default)]
struct TeeMakeWriter;

struct TeeWriter;

fn broadcast_to_listeners(buf: &[u8]) {
    // Records arrive UTF-8 encoded with a trailing newline. Strip
    // it so each Python listener sees one line per receive.
    let line = match std::str::from_utf8(buf) {
        Ok(s) => s.trim_end_matches('\n').to_owned(),
        Err(_) => String::from_utf8_lossy(buf)
            .trim_end_matches('\n')
            .to_owned(),
    };
    if line.is_empty() {
        return;
    }
    let mut to_remove: Vec<u64> = Vec::new();
    if let Ok(reg) = listener_registry().lock() {
        for (id, tx) in reg.senders.iter() {
            if tx.send(line.clone()).is_err() {
                to_remove.push(*id);
            }
        }
    }
    if !to_remove.is_empty() {
        if let Ok(mut reg) = listener_registry().lock() {
            for id in to_remove {
                reg.senders.remove(&id);
            }
        }
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        broadcast_to_listeners(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        std::io::stderr().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TeeMakeWriter {
    type Writer = TeeWriter;
    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter
    }
}

/// Initialize Rust-side logging so that tracing output from the solver is
/// printed to stderr AND broadcast to any attached log listeners.
/// Call this once before calling any solver functions.
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

        // Single fmt subscriber whose writer tees stderr +
        // broadcasts to attached Python listeners. ANSI off so
        // dashboards / log files see clean strings — stderr in a
        // local terminal stays readable too.
        if json {
            let _ = tracing_subscriber::fmt()
                .json()
                .with_env_filter(env_filter)
                .with_target(true)
                .with_writer(TeeMakeWriter)
                .with_ansi(false)
                .try_init();
        } else {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_target(false)
                .with_writer(TeeMakeWriter)
                .with_ansi(false)
                .try_init();
        }
    });

    Ok(())
}

/// Attach a listener that receives every formatted Rust ``tracing``
/// record. Returns an opaque handle (uint) the caller must pass to
/// :func:`detach_log_listener` when it no longer wants events.
///
/// The companion :class:`surge.LogListener` Python class wraps this
/// for convenient ``with``-block usage; most callers should reach
/// for that instead of using the raw handle directly.
#[pyfunction]
pub fn attach_log_listener(py: Python<'_>) -> PyResult<(u64, Bound<'_, PyAny>)> {
    let (tx, rx) = mpsc::channel::<String>();
    let id = next_listener_id();
    listener_registry()
        .lock()
        .map_err(|err| PyValueError::new_err(format!("listener registry poisoned: {err}")))?
        .senders
        .insert(id, tx);
    let receiver = LogReceiver {
        rx: Mutex::new(rx),
        id,
    };
    Ok((id, receiver.into_pyobject(py)?.into_any()))
}

/// Detach a listener previously returned by :func:`attach_log_listener`.
/// Idempotent — detaching an unknown id is a no-op.
#[pyfunction]
pub fn detach_log_listener(handle: u64) -> PyResult<()> {
    if let Ok(mut reg) = listener_registry().lock() {
        reg.senders.remove(&handle);
    }
    Ok(())
}

/// Python-facing receiver for a tracing log channel. Returned alongside
/// its handle by :func:`attach_log_listener`.
#[pyclass(name = "LogReceiver")]
pub struct LogReceiver {
    rx: Mutex<mpsc::Receiver<String>>,
    id: u64,
}

#[pymethods]
impl LogReceiver {
    /// Block for up to ``timeout_secs`` waiting for the next record.
    /// Returns the line (without trailing newline) or ``None`` when no
    /// record arrived in that window.
    fn recv(&self, py: Python<'_>, timeout_secs: f64) -> PyResult<Option<String>> {
        let timeout = std::time::Duration::from_secs_f64(timeout_secs.max(0.0));
        py.detach(|| {
            let rx = self
                .rx
                .lock()
                .map_err(|err| PyValueError::new_err(format!("recv: receiver poisoned: {err}")))?;
            match rx.recv_timeout(timeout) {
                Ok(line) => Ok(Some(line)),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => Ok(None),
            }
        })
    }

    /// Pull every queued record without blocking.
    fn drain(&self) -> PyResult<Vec<String>> {
        let rx = self
            .rx
            .lock()
            .map_err(|err| PyValueError::new_err(format!("drain: receiver poisoned: {err}")))?;
        let mut out = Vec::new();
        while let Ok(line) = rx.try_recv() {
            out.push(line);
        }
        Ok(out)
    }

    #[getter]
    fn handle(&self) -> u64 {
        self.id
    }
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
