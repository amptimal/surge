// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! HiGHS LP/QP/MIP backend.
//!
//! Uses the HiGHS C API via runtime `libloading` — no link-time dependency.
//! Install HiGHS as a system package (`brew install highs`, `apt install
//! libhighs-dev`) or set `HIGHS_LIB_DIR` to the directory containing
//! `libhighs.{so,dylib}`.

use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use libloading::Library;
use tracing::{debug, info, warn};

use super::{
    LpAlgorithm, LpOptions, LpPrimalStart, LpResult, LpSolveStatus, LpSolver, SparseProblem,
    VariableDomain,
};

fn log_highs_mip_trace(message: impl AsRef<str>) {
    info!("highs_mip: {}", message.as_ref());
}

// ---------------------------------------------------------------------------
// HiGHS C API types and constants
// ---------------------------------------------------------------------------

/// Integer type used by the HiGHS C API (`int32_t`).
pub type HighsInt = i32;

const STATUS_OK: HighsInt = 0;
const MODEL_STATUS_SOLVE_ERROR: HighsInt = 4;
const MODEL_STATUS_OPTIMAL: HighsInt = 7;
const MODEL_STATUS_INFEASIBLE: HighsInt = 8;
const MODEL_STATUS_UNBOUNDED: HighsInt = 10;
const MODEL_STATUS_OBJECTIVE_BOUND: HighsInt = 11;
const MODEL_STATUS_OBJECTIVE_TARGET: HighsInt = 12;
const MODEL_STATUS_REACHED_TIME_LIMIT: HighsInt = 13;
const MATRIX_FORMAT_COLUMN_WISE: HighsInt = 1;
const OBJECTIVE_SENSE_MINIMIZE: HighsInt = 1;
const CALLBACK_MIP_SOLUTION: HighsInt = 3;
const CALLBACK_MIP_IMPROVING_SOLUTION: HighsInt = 4;
const CALLBACK_MIP_LOGGING: HighsInt = 5;
const CALLBACK_MIP_INTERRUPT: HighsInt = 6;
const MODEL_STATUS_INTERRUPT: HighsInt = 17;

fn is_integer_domain(domain: VariableDomain) -> bool {
    !matches!(domain, VariableDomain::Continuous)
}

fn highs_integrality_value(domain: VariableDomain) -> HighsInt {
    match domain {
        VariableDomain::Continuous => 0,
        VariableDomain::Binary | VariableDomain::Integer => 1,
    }
}

#[repr(C)]
struct HighsCallbackDataOut {
    cbdata: *mut std::ffi::c_void,
    log_type: i32,
    running_time: f64,
    simplex_iteration_count: HighsInt,
    ipm_iteration_count: HighsInt,
    pdlp_iteration_count: HighsInt,
    objective_function_value: f64,
    mip_node_count: i64,
    mip_total_lp_iterations: i64,
    mip_primal_bound: f64,
    mip_dual_bound: f64,
    mip_gap: f64,
    mip_solution: *mut f64,
    mip_solution_size: HighsInt,
}

#[repr(C)]
struct HighsCallbackDataIn {
    user_interrupt: i32,
    user_solution: *mut f64,
    cbdata: *mut std::ffi::c_void,
    user_has_solution: i32,
    user_solution_size: HighsInt,
}

type HighsCCallbackType = unsafe extern "C" fn(
    HighsInt,
    *const c_char,
    *const HighsCallbackDataOut,
    *mut HighsCallbackDataIn,
    *mut std::ffi::c_void,
);

struct MipIncumbentCapture {
    best_solution: Mutex<Option<Vec<f64>>>,
    interrupt_after_secs: Option<f64>,
    interrupt_requested: AtomicBool,
}

unsafe extern "C" fn capture_highs_mip_solution_callback(
    _callback_type: HighsInt,
    _message: *const c_char,
    data_out: *const HighsCallbackDataOut,
    data_in: *mut HighsCallbackDataIn,
    user_data: *mut std::ffi::c_void,
) {
    if data_out.is_null() || user_data.is_null() {
        return;
    }

    let capture = unsafe { &*(user_data as *const MipIncumbentCapture) };
    let data_out = unsafe { &*data_out };
    if !data_out.mip_solution.is_null() && data_out.mip_solution_size > 0 {
        let values = unsafe {
            std::slice::from_raw_parts(data_out.mip_solution, data_out.mip_solution_size as usize)
                .to_vec()
        };
        if let Ok(mut slot) = capture.best_solution.lock() {
            *slot = Some(values);
        }
    }
    if let Some(limit) = capture.interrupt_after_secs
        && data_out.running_time + 1e-9 >= limit
        && !data_in.is_null()
        && capture
            .best_solution
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(Vec::len))
            .is_some()
    {
        unsafe {
            (*data_in).user_interrupt = 1;
        }
        capture.interrupt_requested.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Runtime library loader (same pattern as ipopt.rs / gurobi.rs)
// ---------------------------------------------------------------------------

/// Function pointer table for HiGHS C API, loaded at runtime via dlopen.
///
/// `_lib` keeps the shared library loaded; all function pointers are valid
/// for the lifetime of this struct.
#[allow(non_snake_case)]
struct HighsLib {
    _lib: Library,
    Highs_create: unsafe extern "C" fn() -> *mut std::ffi::c_void,
    Highs_destroy: unsafe extern "C" fn(*mut std::ffi::c_void),
    Highs_passLp: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        HighsInt,
        HighsInt,
        HighsInt,
        HighsInt,
        HighsInt,
        f64,
        *const f64,
        *const f64,
        *const f64,
        *const f64,
        *const f64,
        *const HighsInt,
        *const HighsInt,
        *const f64,
    ) -> HighsInt,
    Highs_passModel: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        HighsInt,
        HighsInt,
        HighsInt,
        HighsInt,
        HighsInt,
        HighsInt,
        HighsInt,
        f64,
        *const f64,
        *const f64,
        *const f64,
        *const f64,
        *const f64,
        *const HighsInt,
        *const HighsInt,
        *const f64,
        *const HighsInt,
        *const HighsInt,
        *const f64,
        *const HighsInt,
    ) -> HighsInt,
    Highs_passMip: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        HighsInt,
        HighsInt,
        HighsInt,
        HighsInt,
        HighsInt,
        f64,
        *const f64,
        *const f64,
        *const f64,
        *const f64,
        *const f64,
        *const HighsInt,
        *const HighsInt,
        *const f64,
        *const HighsInt,
    ) -> HighsInt,
    Highs_run: unsafe extern "C" fn(*mut std::ffi::c_void) -> HighsInt,
    Highs_getModelStatus: unsafe extern "C" fn(*mut std::ffi::c_void) -> HighsInt,
    Highs_getSolution: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *mut f64,
        *mut f64,
        *mut f64,
        *mut f64,
    ) -> HighsInt,
    Highs_setBasis:
        unsafe extern "C" fn(*mut std::ffi::c_void, *const HighsInt, *const HighsInt) -> HighsInt,
    Highs_getBasis:
        unsafe extern "C" fn(*mut std::ffi::c_void, *mut HighsInt, *mut HighsInt) -> HighsInt,
    Highs_setSolution: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *const f64,
        *const f64,
        *const f64,
        *const f64,
    ) -> HighsInt,
    Highs_setSparseSolution: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        HighsInt,
        *const HighsInt,
        *const f64,
    ) -> HighsInt,
    Highs_setBoolOptionValue:
        unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, HighsInt) -> HighsInt,
    Highs_setDoubleOptionValue:
        unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, f64) -> HighsInt,
    Highs_setStringOptionValue:
        unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, *const c_char) -> HighsInt,
    Highs_getDoubleInfoValue:
        unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, *mut f64) -> HighsInt,
    Highs_getIntInfoValue:
        unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, *mut HighsInt) -> HighsInt,
    Highs_setCallback: Option<
        unsafe extern "C" fn(
            *mut std::ffi::c_void,
            HighsCCallbackType,
            *mut std::ffi::c_void,
        ) -> HighsInt,
    >,
    Highs_startCallback: Option<unsafe extern "C" fn(*mut std::ffi::c_void, HighsInt) -> HighsInt>,
    Highs_stopCallback: Option<unsafe extern "C" fn(*mut std::ffi::c_void, HighsInt) -> HighsInt>,
    Highs_getIis: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *mut HighsInt,
        *mut HighsInt,
        *mut HighsInt,
        *mut HighsInt,
        *mut HighsInt,
        *mut HighsInt,
        *mut HighsInt,
        *mut HighsInt,
    ) -> HighsInt,
    Highs_getPrimalRay:
        unsafe extern "C" fn(*mut std::ffi::c_void, *mut HighsInt, *mut f64) -> HighsInt,
}

unsafe impl Send for HighsLib {}
unsafe impl Sync for HighsLib {}

static HIGHS: OnceLock<Result<Arc<HighsLib>, String>> = OnceLock::new();

/// Load (and cache) the HiGHS shared library.  Returns `Err` if not found.
fn get_highs() -> Result<&'static Arc<HighsLib>, String> {
    HIGHS
        .get_or_init(|| {
            for path in highs_lib_paths() {
                if let Ok(lib) = unsafe { Library::new(&path) } {
                    match unsafe { load_highs_symbols(lib) } {
                        Ok(hlib) => return Ok(Arc::new(hlib)),
                        Err(e) => return Err(e),
                    }
                }
            }
            Err(
                "HiGHS not found — Surge requires the HiGHS C library (libhighs.so / \
                 libhighs.dylib), not the Python package. Install via your system package \
                 manager (brew install highs / apt install libhighs-dev) or build from \
                 source. `pip install highspy` does NOT provide the shared library. \
                 Set HIGHS_LIB_DIR to override the search path."
                    .to_string(),
            )
        })
        .as_ref()
        .map_err(|e| e.clone())
}

fn highs_lib_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Ok(dir) = std::env::var("HIGHS_LIB_DIR") {
        paths.push(std::path::PathBuf::from(format!("{dir}/libhighs.so")));
        paths.push(std::path::PathBuf::from(format!("{dir}/libhighs.dylib")));
        paths.push(std::path::PathBuf::from(format!("{dir}/highs.dll")));
    }
    for prefix in &["/opt/homebrew", "/usr/local", "/usr", "/opt/highs"] {
        paths.push(std::path::PathBuf::from(format!(
            "{prefix}/lib/libhighs.so"
        )));
        paths.push(std::path::PathBuf::from(format!(
            "{prefix}/lib/libhighs.dylib"
        )));
    }
    // System / LD_LIBRARY_PATH / DYLD_LIBRARY_PATH
    paths.push(std::path::PathBuf::from("libhighs.so"));
    paths.push(std::path::PathBuf::from("libhighs.dylib"));
    paths
}

unsafe fn load_highs_symbols(lib: Library) -> Result<HighsLib, String> {
    macro_rules! sym {
        ($name:literal, $ty:ty) => {
            *unsafe { lib.get::<$ty>($name) }
                .map_err(|e| format!("HiGHS symbol {} not found: {e}", stringify!($name)))?
        };
    }
    macro_rules! optional_sym {
        ($name:literal, $ty:ty) => {
            unsafe { lib.get::<$ty>($name) }.ok().map(|symbol| *symbol)
        };
    }
    Ok(HighsLib {
        Highs_create: sym!(
            b"Highs_create\0",
            unsafe extern "C" fn() -> *mut std::ffi::c_void
        ),
        Highs_destroy: sym!(
            b"Highs_destroy\0",
            unsafe extern "C" fn(*mut std::ffi::c_void)
        ),
        Highs_passLp: sym!(
            b"Highs_passLp\0",
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                HighsInt,
                HighsInt,
                HighsInt,
                HighsInt,
                HighsInt,
                f64,
                *const f64,
                *const f64,
                *const f64,
                *const f64,
                *const f64,
                *const HighsInt,
                *const HighsInt,
                *const f64,
            ) -> HighsInt
        ),
        Highs_passModel: sym!(
            b"Highs_passModel\0",
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                HighsInt,
                HighsInt,
                HighsInt,
                HighsInt,
                HighsInt,
                HighsInt,
                HighsInt,
                f64,
                *const f64,
                *const f64,
                *const f64,
                *const f64,
                *const f64,
                *const HighsInt,
                *const HighsInt,
                *const f64,
                *const HighsInt,
                *const HighsInt,
                *const f64,
                *const HighsInt,
            ) -> HighsInt
        ),
        Highs_passMip: sym!(
            b"Highs_passMip\0",
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                HighsInt,
                HighsInt,
                HighsInt,
                HighsInt,
                HighsInt,
                f64,
                *const f64,
                *const f64,
                *const f64,
                *const f64,
                *const f64,
                *const HighsInt,
                *const HighsInt,
                *const f64,
                *const HighsInt,
            ) -> HighsInt
        ),
        Highs_run: sym!(
            b"Highs_run\0",
            unsafe extern "C" fn(*mut std::ffi::c_void) -> HighsInt
        ),
        Highs_getModelStatus: sym!(
            b"Highs_getModelStatus\0",
            unsafe extern "C" fn(*mut std::ffi::c_void) -> HighsInt
        ),
        Highs_getSolution: sym!(
            b"Highs_getSolution\0",
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                *mut f64,
                *mut f64,
                *mut f64,
                *mut f64,
            ) -> HighsInt
        ),
        Highs_setBasis: sym!(
            b"Highs_setBasis\0",
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                *const HighsInt,
                *const HighsInt,
            ) -> HighsInt
        ),
        Highs_getBasis: sym!(
            b"Highs_getBasis\0",
            unsafe extern "C" fn(*mut std::ffi::c_void, *mut HighsInt, *mut HighsInt) -> HighsInt
        ),
        Highs_setSolution: sym!(
            b"Highs_setSolution\0",
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                *const f64,
                *const f64,
                *const f64,
                *const f64,
            ) -> HighsInt
        ),
        Highs_setSparseSolution: sym!(
            b"Highs_setSparseSolution\0",
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                HighsInt,
                *const HighsInt,
                *const f64,
            ) -> HighsInt
        ),
        Highs_setBoolOptionValue: sym!(
            b"Highs_setBoolOptionValue\0",
            unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, HighsInt) -> HighsInt
        ),
        Highs_setDoubleOptionValue: sym!(
            b"Highs_setDoubleOptionValue\0",
            unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, f64) -> HighsInt
        ),
        Highs_setStringOptionValue: sym!(
            b"Highs_setStringOptionValue\0",
            unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, *const c_char) -> HighsInt
        ),
        Highs_getDoubleInfoValue: sym!(
            b"Highs_getDoubleInfoValue\0",
            unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, *mut f64) -> HighsInt
        ),
        Highs_getIntInfoValue: sym!(
            b"Highs_getIntInfoValue\0",
            unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, *mut HighsInt) -> HighsInt
        ),
        Highs_setCallback: optional_sym!(
            b"Highs_setCallback\0",
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                HighsCCallbackType,
                *mut std::ffi::c_void,
            ) -> HighsInt
        ),
        Highs_startCallback: optional_sym!(
            b"Highs_startCallback\0",
            unsafe extern "C" fn(*mut std::ffi::c_void, HighsInt) -> HighsInt
        ),
        Highs_stopCallback: optional_sym!(
            b"Highs_stopCallback\0",
            unsafe extern "C" fn(*mut std::ffi::c_void, HighsInt) -> HighsInt
        ),
        Highs_getIis: sym!(
            b"Highs_getIis\0",
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                *mut HighsInt,
                *mut HighsInt,
                *mut HighsInt,
                *mut HighsInt,
                *mut HighsInt,
                *mut HighsInt,
                *mut HighsInt,
                *mut HighsInt,
            ) -> HighsInt
        ),
        Highs_getPrimalRay: sym!(
            b"Highs_getPrimalRay\0",
            unsafe extern "C" fn(*mut std::ffi::c_void, *mut HighsInt, *mut f64) -> HighsInt
        ),
        _lib: lib,
    })
}

// ---------------------------------------------------------------------------
// Solver logic (unchanged, just calls through lib.* instead of bare FFI)
// ---------------------------------------------------------------------------

fn set_presolve_option(lib: &HighsLib, highs: *mut std::ffi::c_void, override_value: &str) {
    let option = CString::new("presolve").expect("static string contains no null bytes");
    let setting = CString::new(override_value).expect("static string contains no null bytes");
    unsafe {
        (lib.Highs_setStringOptionValue)(highs, option.as_ptr(), setting.as_ptr());
    }
}

fn set_string_option(lib: &HighsLib, highs: *mut std::ffi::c_void, option_name: &str, value: &str) {
    let option = CString::new(option_name).expect("static string contains no null bytes");
    let setting = CString::new(value).expect("static string contains no null bytes");
    unsafe {
        (lib.Highs_setStringOptionValue)(highs, option.as_ptr(), setting.as_ptr());
    }
}

fn configure_presolve(lib: &HighsLib, highs: *mut std::ffi::c_void, preserve_primal_start: bool) {
    if let Ok(value) = std::env::var("SURGE_HIGHS_PRESOLVE") {
        let normalized = value.trim().to_ascii_lowercase();
        let override_value = match normalized.as_str() {
            "0" | "off" | "false" => "off",
            "1" | "on" | "true" => "on",
            _ => return,
        };
        set_presolve_option(lib, highs, override_value);
        return;
    }

    if preserve_primal_start {
        set_presolve_option(lib, highs, "off");
    }
}

fn configure_lp_algorithm(lib: &HighsLib, highs: *mut std::ffi::c_void, algorithm: LpAlgorithm) {
    let Some(value) = (match algorithm {
        LpAlgorithm::Auto => None,
        LpAlgorithm::Simplex => Some("simplex"),
        LpAlgorithm::Ipm => Some("ipm"),
    }) else {
        return;
    };
    set_string_option(lib, highs, "solver", value);
    if matches!(algorithm, LpAlgorithm::Ipm) {
        set_string_option(lib, highs, "run_crossover", "off");
    }
}

fn configure_mip_lp_solver(lib: &HighsLib, highs: *mut std::ffi::c_void) {
    let value = std::env::var("SURGE_HIGHS_MIP_LP_SOLVER")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "simplex".to_string());
    set_string_option(lib, highs, "mip_lp_solver", &value);
}

fn apply_primal_start_hint(
    lib: &HighsLib,
    highs: *mut std::ffi::c_void,
    primal_start: Option<&LpPrimalStart>,
    expected_n_col: usize,
    trace_prefix: &str,
) {
    let Some(start) = primal_start else {
        return;
    };

    match start {
        LpPrimalStart::Dense(start) => {
            if start.len() != expected_n_col {
                warn!(
                    expected = expected_n_col,
                    actual = start.len(),
                    trace_prefix,
                    "Ignoring HiGHS dense primal start with mismatched column count"
                );
                return;
            }
            let set_solution_status = unsafe {
                (lib.Highs_setSolution)(
                    highs,
                    start.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            };
            if set_solution_status != STATUS_OK {
                warn!(
                    set_solution_status,
                    trace_prefix, "HiGHS rejected the provided dense primal start"
                );
            } else if env_flag("SURGE_DEBUG_HIGHS_MIP") {
                log_highs_mip_trace(format!(
                    "{trace_prefix} start_type=dense assigned={}",
                    start.len()
                ));
            }
        }
        LpPrimalStart::Sparse { indices, values } => {
            if indices.len() != values.len() {
                warn!(
                    indices = indices.len(),
                    values = values.len(),
                    trace_prefix,
                    "Ignoring HiGHS sparse primal start with mismatched arrays"
                );
                return;
            }
            let sparse_indices: Vec<HighsInt> = indices
                .iter()
                .filter_map(|&idx| HighsInt::try_from(idx).ok())
                .collect();
            if sparse_indices.len() != indices.len() {
                warn!(
                    trace_prefix,
                    "Ignoring HiGHS sparse primal start with out-of-range column indices"
                );
                return;
            }
            let set_solution_status = unsafe {
                (lib.Highs_setSparseSolution)(
                    highs,
                    sparse_indices.len() as HighsInt,
                    sparse_indices.as_ptr(),
                    values.as_ptr(),
                )
            };
            if set_solution_status != STATUS_OK {
                warn!(
                    set_solution_status,
                    assigned = sparse_indices.len(),
                    trace_prefix,
                    "HiGHS rejected the provided sparse primal start"
                );
            } else if env_flag("SURGE_DEBUG_HIGHS_MIP") {
                log_highs_mip_trace(format!(
                    "{trace_prefix} start_type=sparse assigned={}",
                    sparse_indices.len()
                ));
            }
        }
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "on" | "yes")
    )
}

fn objective_scale_factor(col_cost: &[f64], q_value: Option<&[f64]>) -> f64 {
    let max_linear = col_cost
        .iter()
        .fold(0.0_f64, |acc, value| acc.max(value.abs()));
    let max_quadratic = q_value
        .map(|values| {
            values
                .iter()
                .fold(0.0_f64, |acc, value| acc.max(value.abs()))
        })
        .unwrap_or(0.0);
    let max_coeff = max_linear.max(max_quadratic);
    if !max_coeff.is_finite() || max_coeff <= 1.0e4 {
        return 1.0;
    }
    let exponent = (max_coeff.log10().floor() - 3.0).max(0.0);
    10.0_f64.powf(exponent)
}

/// Sparse QP solution (primal + dual).
pub struct SparseQpSolution {
    /// Primal variable values.
    pub x: Vec<f64>,
    /// Row duals (raw HiGHS convention: positive at lower bound, negative at upper).
    pub row_dual: Vec<f64>,
    /// Column duals (reduced costs).
    pub col_dual: Vec<f64>,
    /// Objective value.
    pub objective: f64,
    /// Number of solver iterations.
    pub iterations: u32,
    /// Whether the solver converged to optimality.
    pub converged: bool,
}

/// Extract IIS (Irreducible Infeasible Set) detail from a HiGHS model after
/// infeasibility is detected. Returns a formatted string listing the
/// conflicting row and column indices.
fn extract_iis_detail(
    lib: &HighsLib,
    highs: *mut std::ffi::c_void,
    n_col: usize,
    n_row: usize,
    row_lower_in: &[f64],
    row_upper_in: &[f64],
    col_lower_in: &[f64],
    col_upper_in: &[f64],
) -> String {
    let mut iis_num_col: HighsInt = 0;
    let mut iis_num_row: HighsInt = 0;
    let mut col_index = vec![0 as HighsInt; n_col];
    let mut row_index = vec![0 as HighsInt; n_row];
    let mut col_bound = vec![0 as HighsInt; n_col];
    let mut row_bound = vec![0 as HighsInt; n_row];
    let mut col_status = vec![0 as HighsInt; n_col];
    let mut row_status = vec![0 as HighsInt; n_row];

    let status = unsafe {
        (lib.Highs_getIis)(
            highs,
            &mut iis_num_col,
            &mut iis_num_row,
            col_index.as_mut_ptr(),
            row_index.as_mut_ptr(),
            col_bound.as_mut_ptr(),
            row_bound.as_mut_ptr(),
            col_status.as_mut_ptr(),
            row_status.as_mut_ptr(),
        )
    };

    if status != STATUS_OK {
        // IIS not available for this problem type or HiGHS build.
        // Return problem dimensions as basic diagnostic info.
        return format!(" (problem has {n_row} constraints, {n_col} variables; IIS unavailable)");
    }

    let n_col = iis_num_col as usize;
    let n_row = iis_num_row as usize;

    if n_col == 0 && n_row == 0 {
        return format!(
            " (problem has {n_row} constraints, {n_col} variables; infeasibility detected in presolve)"
        );
    }

    let bound_label = |b: HighsInt| -> &'static str {
        match b {
            1 => "free",
            2 => "lower",
            3 => "upper",
            4 => "boxed",
            _ => "?",
        }
    };

    let mut parts = vec![format!(" (IIS: {n_row} rows, {n_col} cols)")];

    if n_row > 0 {
        let rows: Vec<String> = row_index[..n_row]
            .iter()
            .zip(row_bound[..n_row].iter())
            .take(10)
            .map(|(&idx, &bound)| {
                let ri = idx as usize;
                let lo = if ri < row_lower_in.len() {
                    row_lower_in[ri]
                } else {
                    f64::NAN
                };
                let hi = if ri < row_upper_in.len() {
                    row_upper_in[ri]
                } else {
                    f64::NAN
                };
                format!("row[{idx}]({}, lo={lo:.4}, hi={hi:.4})", bound_label(bound))
            })
            .collect();
        parts.push(format!(" rows=[{}]", rows.join(", ")));
    }

    if n_col > 0 {
        let cols: Vec<String> = col_index[..n_col]
            .iter()
            .zip(col_bound[..n_col].iter())
            .take(10)
            .map(|(&idx, &bound)| {
                let ci = idx as usize;
                let lo = if ci < col_lower_in.len() {
                    col_lower_in[ci]
                } else {
                    f64::NAN
                };
                let hi = if ci < col_upper_in.len() {
                    col_upper_in[ci]
                } else {
                    f64::NAN
                };
                format!("col[{idx}]({}, lo={lo:.4}, hi={hi:.4})", bound_label(bound))
            })
            .collect();
        parts.push(format!(" cols=[{}]", cols.join(", ")));
    }

    info!(
        iis_rows = n_row,
        iis_cols = n_col,
        "IIS extracted for infeasible problem"
    );

    parts.concat()
}

/// Solve a sparse LP/QP using `Highs_passLp` / `Highs_passModel`.
///
/// The constraint matrix is provided in CSC format (column-wise).
/// Row bounds define constraints as `row_lower <= A*x <= row_upper`.
///
/// For QP, the Hessian is upper-triangular CSC: `q_start`, `q_index`, `q_value`.
#[allow(clippy::too_many_arguments)]
fn solve_sparse_qp(
    lib: &HighsLib,
    n_col: usize,
    n_row: usize,
    col_cost: &[f64],
    col_lower: &[f64],
    col_upper: &[f64],
    row_lower: &[f64],
    row_upper: &[f64],
    a_start: &[HighsInt],
    a_index: &[HighsInt],
    a_value: &[f64],
    q_start: Option<&[HighsInt]>,
    q_index: Option<&[HighsInt]>,
    q_value: Option<&[f64]>,
    tolerance: f64,
    time_limit_secs: Option<f64>,
    primal_start: Option<&LpPrimalStart>,
    algorithm: LpAlgorithm,
) -> Result<SparseQpSolution, String> {
    let has_hessian = q_value.is_some_and(|v| !v.is_empty());
    debug!(
        cols = n_col,
        rows = n_row,
        nnz = a_value.len(),
        quadratic = has_hessian,
        "HiGHS sparse QP: starting solve"
    );
    if n_col == 0 {
        return Ok(SparseQpSolution {
            x: vec![],
            row_dual: vec![],
            col_dual: vec![],
            objective: 0.0,
            iterations: 0,
            converged: true,
        });
    }

    unsafe {
        let highs = (lib.Highs_create)();
        if highs.is_null() {
            return Err("Failed to create HiGHS instance".into());
        }

        let result = solve_sparse_qp_inner(
            lib,
            highs,
            n_col,
            n_row,
            col_cost,
            col_lower,
            col_upper,
            row_lower,
            row_upper,
            a_start,
            a_index,
            a_value,
            q_start,
            q_index,
            q_value,
            tolerance,
            time_limit_secs,
            primal_start,
            algorithm,
        );

        (lib.Highs_destroy)(highs);
        result
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn solve_sparse_qp_inner(
    lib: &HighsLib,
    highs: *mut std::ffi::c_void,
    n_col: usize,
    n_row: usize,
    col_cost: &[f64],
    col_lower: &[f64],
    col_upper: &[f64],
    row_lower: &[f64],
    row_upper: &[f64],
    a_start: &[HighsInt],
    a_index: &[HighsInt],
    a_value: &[f64],
    q_start: Option<&[HighsInt]>,
    q_index: Option<&[HighsInt]>,
    q_value: Option<&[f64]>,
    tolerance: f64,
    time_limit_secs: Option<f64>,
    primal_start: Option<&LpPrimalStart>,
    algorithm: LpAlgorithm,
) -> Result<SparseQpSolution, String> {
    // Suppress output (set SURGE_HIGHS_VERBOSE=1 to enable HiGHS logging)
    let verbose = std::env::var("SURGE_HIGHS_VERBOSE").is_ok();
    let output_flag = CString::new("output_flag").expect("static string contains no null bytes");
    unsafe {
        (lib.Highs_setBoolOptionValue)(highs, output_flag.as_ptr(), if verbose { 1 } else { 0 })
    };
    configure_presolve(lib, highs, primal_start.is_some());
    configure_lp_algorithm(lib, highs, algorithm);

    let objective_scale = objective_scale_factor(col_cost, q_value);
    let scaled_col_cost_storage = if objective_scale != 1.0 {
        Some(
            col_cost
                .iter()
                .map(|value| *value / objective_scale)
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let scaled_q_value_storage = if objective_scale != 1.0 {
        q_value.map(|values| {
            values
                .iter()
                .map(|value| *value / objective_scale)
                .collect::<Vec<_>>()
        })
    } else {
        None
    };
    let scaled_col_cost = scaled_col_cost_storage.as_deref().unwrap_or(col_cost);
    let scaled_q_value = scaled_q_value_storage.as_deref().or(q_value);

    // Set tolerances
    let tol = tolerance.max(1e-10);
    let primal_tol =
        CString::new("primal_feasibility_tolerance").expect("static string contains no null bytes");
    let dual_tol =
        CString::new("dual_feasibility_tolerance").expect("static string contains no null bytes");
    unsafe { (lib.Highs_setDoubleOptionValue)(highs, primal_tol.as_ptr(), tol) };
    unsafe { (lib.Highs_setDoubleOptionValue)(highs, dual_tol.as_ptr(), tol) };
    if let Some(limit) = time_limit_secs {
        let time_opt = CString::new("time_limit").expect("static string contains no null bytes");
        unsafe { (lib.Highs_setDoubleOptionValue)(highs, time_opt.as_ptr(), limit) };
    }

    // Load the model.  For QP problems we use Highs_passModel, which passes
    // the LP and Hessian atomically so HiGHS knows from the start that it is
    // solving a QP and selects IPM.  Using Highs_passLp followed by a separate
    // Highs_passHessian call sometimes causes HiGHS to misclassify the problem
    // as an LP and return UNBOUNDED (model status 10) for PSD Hessians.
    let a_num_nz = a_value.len();
    let q_nnz = q_value.map_or(0, |v| v.len());
    let q_start_len = q_start.map_or(0, |v| v.len());

    if verbose {
        tracing::debug!(
            n_col,
            n_row,
            a_nnz = a_num_nz,
            q_nnz,
            q_start_len,
            "HiGHS problem dimensions"
        );
    }

    // Extract QP Hessian arrays once (needed for passModel and for adaptive regularization).
    // q_nnz > 0 implies all three Option slices are Some.
    let qp_arrays: Option<(&[i32], &[i32], &[f64])> = if q_nnz > 0 {
        Some((
            q_start.expect("q_start Some when q_nnz > 0"),
            q_index.expect("q_index Some when q_nnz > 0"),
            scaled_q_value.expect("q_value Some when q_nnz > 0"),
        ))
    } else {
        None
    };

    // LP warm-start for QP problems.
    //
    // HiGHS QUASS (active-set QP) internally runs an LP phase to get a starting
    // basis, but on large degenerate problems (10k+ bus DC-OPF) this internal LP
    // phase stalls or terminates early, leaving QUASS at a terrible starting point
    // (large thermal slacks active → $5T objective vs correct $2.4M).  By solving
    // the LP relaxation explicitly in a separate HiGHS instance, we guarantee a
    // high-quality feasible starting point before launching the QP active-set steps.
    //
    // We extract BOTH the primal solution (col_vals) AND the simplex basis
    // (col_status, row_status) from the LP solve. QUASS is an active-set method
    // that operates on a basis (which constraints are active); passing the
    // LP-optimal basis via Highs_setBasis skips QUASS's internal LP phase entirely
    // and starts directly from the correct active set. Highs_setSolution (primal
    // hint only) is kept as belt-and-suspenders but the basis is what matters.
    //
    // LP warm-start for large QP problems only (HiGHS-specific workaround).
    // On large degenerate problems (10k+ bus DC-OPF), QUASS's internal LP phase
    // stalls, leaving it at a terrible starting point ($5T vs $2.4M). Solving the
    // LP relaxation first and passing the basis via Highs_setBasis fixes this.
    // Note: LP warm-start does NOT fix non-PSD B-bus cases (e.g. case300 with
    // series-compensated branches) — QUASS stalls even with a good basis.
    // Small problems (< 2000 vars) don't need LP warm-start: SOLVE_ERROR recovery
    // below handles the rare case where QUASS finds the correct solution but
    // HiGHS's internal consistency check rejects it.
    let lp_warm_start: Option<(Vec<f64>, Vec<HighsInt>, Vec<HighsInt>)> =
        if q_nnz > 0 && n_col > 2000 {
            let lp_highs = unsafe { (lib.Highs_create)() };
            let ws = if !lp_highs.is_null() {
                unsafe {
                    let out =
                        CString::new("output_flag").expect("static string contains no null bytes");
                    (lib.Highs_setBoolOptionValue)(lp_highs, out.as_ptr(), 0);
                    let ptol = CString::new("primal_feasibility_tolerance")
                        .expect("static string contains no null bytes");
                    let dtol = CString::new("dual_feasibility_tolerance")
                        .expect("static string contains no null bytes");
                    (lib.Highs_setDoubleOptionValue)(lp_highs, ptol.as_ptr(), tol);
                    (lib.Highs_setDoubleOptionValue)(lp_highs, dtol.as_ptr(), tol);
                    if let Some(limit) = time_limit_secs {
                        let time_opt = CString::new("time_limit")
                            .expect("static string contains no null bytes");
                        (lib.Highs_setDoubleOptionValue)(lp_highs, time_opt.as_ptr(), limit);
                    }
                    (lib.Highs_passLp)(
                        lp_highs,
                        n_col as HighsInt,
                        n_row as HighsInt,
                        a_num_nz as HighsInt,
                        MATRIX_FORMAT_COLUMN_WISE,
                        OBJECTIVE_SENSE_MINIMIZE,
                        0.0,
                        scaled_col_cost.as_ptr(),
                        col_lower.as_ptr(),
                        col_upper.as_ptr(),
                        row_lower.as_ptr(),
                        row_upper.as_ptr(),
                        a_start.as_ptr(),
                        a_index.as_ptr(),
                        a_value.as_ptr(),
                    );
                    (lib.Highs_run)(lp_highs);
                };
                let lp_status = unsafe { (lib.Highs_getModelStatus)(lp_highs) };
                if lp_status == MODEL_STATUS_OPTIMAL {
                    let mut col_vals = vec![0.0f64; n_col];
                    let mut col_duals = vec![0.0f64; n_col];
                    let mut row_vals = vec![0.0f64; n_row];
                    let mut row_duals = vec![0.0f64; n_row];
                    unsafe {
                        (lib.Highs_getSolution)(
                            lp_highs,
                            col_vals.as_mut_ptr(),
                            col_duals.as_mut_ptr(),
                            row_vals.as_mut_ptr(),
                            row_duals.as_mut_ptr(),
                        )
                    };
                    // Extract LP simplex basis (col/row status integers).
                    // kHighsBasisStatus: 0=Lower, 1=Basic, 2=Upper, 3=Free, 4=Zero
                    let mut col_status_vec = vec![0 as HighsInt; n_col];
                    let mut row_status_vec = vec![0 as HighsInt; n_row];
                    unsafe {
                        (lib.Highs_getBasis)(
                            lp_highs,
                            col_status_vec.as_mut_ptr(),
                            row_status_vec.as_mut_ptr(),
                        )
                    };
                    Some((col_vals, col_status_vec, row_status_vec))
                } else {
                    None
                }
            } else {
                None
            };
            unsafe { (lib.Highs_destroy)(lp_highs) };
            ws
        } else {
            None
        };

    let status = if let Some((qs, qi, qv)) = qp_arrays {
        // QP: use Highs_passModel (LP + Hessian in one call)
        unsafe {
            (lib.Highs_passModel)(
                highs,
                n_col as HighsInt,
                n_row as HighsInt,
                a_num_nz as HighsInt,
                q_nnz as HighsInt,
                MATRIX_FORMAT_COLUMN_WISE, // CSC constraint matrix
                1,                         // kHighsHessianFormatTriangular
                OBJECTIVE_SENSE_MINIMIZE,
                0.0, // offset
                scaled_col_cost.as_ptr(),
                col_lower.as_ptr(),
                col_upper.as_ptr(),
                row_lower.as_ptr(),
                row_upper.as_ptr(),
                a_start.as_ptr(),
                a_index.as_ptr(),
                a_value.as_ptr(),
                qs.as_ptr(),
                qi.as_ptr(),
                qv.as_ptr(),
                std::ptr::null(), // integrality: null = continuous
            )
        }
    } else {
        // Pure LP
        unsafe {
            (lib.Highs_passLp)(
                highs,
                n_col as HighsInt,
                n_row as HighsInt,
                a_num_nz as HighsInt,
                MATRIX_FORMAT_COLUMN_WISE,
                OBJECTIVE_SENSE_MINIMIZE,
                0.0,
                scaled_col_cost.as_ptr(),
                col_lower.as_ptr(),
                col_upper.as_ptr(),
                row_lower.as_ptr(),
                row_upper.as_ptr(),
                a_start.as_ptr(),
                a_index.as_ptr(),
                a_value.as_ptr(),
            )
        }
    };
    if status < 0 {
        return Err(format!("Highs_passModel/passLp failed: status {status}"));
    }

    // Apply LP warm-start basis and solution (if computed above).
    //
    // Highs_setBasis passes the LP-optimal simplex basis directly to QUASS,
    // which skips its internal LP phase and starts active-set updates from the
    // correct working set. This is the critical fix: Highs_setSolution alone
    // (primal values only) does not affect QUASS's active-set initialization,
    // so QUASS could still start from a degenerate basis and stall at $5T.
    if let Some((ref col_vals, ref col_st, ref row_st)) = lp_warm_start {
        unsafe {
            (lib.Highs_setBasis)(highs, col_st.as_ptr(), row_st.as_ptr());
        }
        // Also pass primal values as a hint for the dual variables.
        let null_ptr: *const f64 = std::ptr::null();
        unsafe { (lib.Highs_setSolution)(highs, col_vals.as_ptr(), null_ptr, null_ptr, null_ptr) };
    }
    apply_primal_start_hint(lib, highs, primal_start, n_col, "highs_lp_trace");

    // For QP problems, set an adaptive diagonal regularization value.
    //
    // Root cause of HiGHS QP numerical failures on large cases:
    //   HiGHS ASM uses `pQp_zero_threshold = 1e-7` to detect zero-curvature
    //   directions.  Variables with no explicit Hessian entry (theta, slacks)
    //   are internally augmented via `completeHessianDiagonal` to Q[i,i] = reg.
    //   The fixed value 1e-3 was chosen to clear the 1e-7 threshold, but it
    //   creates a ~20,000:1 ratio against generator cost entries (2·c2·base²
    //   ≈ 20–2000).  For large cases (10k–82k buses) this drives the KKT
    //   condition number to ~10⁹, causing the ASM to produce catastrophically
    //   wrong objectives (e.g. $5 trillion vs correct $2.4 million).
    //
    // Fix: target a 100:1 ratio between the smallest explicit Hessian entry
    //   and the regularization, so the fill-in never dominates.  A minimum of
    //   1e-5 (vs threshold 1e-7) still clears the zero-curvature detector.
    //   The optimal solution is unchanged: theta variables are fully determined
    //   by equality constraints, so their Hessian curvature does not affect the
    //   primal solution.
    //
    // NOTE: `solver=ipm` is a no-op for QP — HiGHS 1.13.1 unconditionally routes
    //   QP models to callSolveQp() (ASM) regardless of the solver option.
    if let Some((_, _, qv)) = qp_arrays {
        let q_min = qv
            .iter()
            .copied()
            .filter(|&v| v > 1e-20)
            .fold(f64::INFINITY, f64::min);
        // 100:1 ratio; floor at 1e-5 to stay well above the 1e-7 threshold.
        let reg_val = if q_min.is_finite() {
            (q_min * 0.01).max(1e-5)
        } else {
            1e-3 // fallback: no positive Hessian entries (degenerate)
        };
        let opt =
            CString::new("qp_regularization_value").expect("static string contains no null bytes");
        unsafe { (lib.Highs_setDoubleOptionValue)(highs, opt.as_ptr(), reg_val) };
    }

    // Solve
    let run_status = unsafe { (lib.Highs_run)(highs) };
    let model_status = unsafe { (lib.Highs_getModelStatus)(highs) };

    // HiGHS QUASS (QP active-set) occasionally finds the correct primal solution
    // but fails its own internal `getPrimalDualBasisErrors` consistency check,
    // returning run_status=-1 with MODEL_STATUS_SOLVE_ERROR (4).  The solution
    // IS correct (verified against Gurobi) but the internal post-solve check
    // rejects it due to small basis residuals (~1e-3) on thermal constraint rows.
    //
    // When this happens, extract the primal solution and verify feasibility at
    // our own tolerance (1e-2 per-unit, ~1 MW on a 100 MW base).  If the solution
    // passes, accept it with a warning rather than discarding a correct result.
    if run_status == -1 && model_status != MODEL_STATUS_SOLVE_ERROR {
        return Err("HiGHS solver returned error status".into());
    }
    if run_status == -1 && model_status == MODEL_STATUS_SOLVE_ERROR {
        // Attempt solution recovery: extract primal and check feasibility.
        let mut col_val_rec = vec![0.0f64; n_col];
        let mut col_dual_rec = vec![0.0f64; n_col];
        let mut row_val_rec = vec![0.0f64; n_row];
        let mut row_dual_rec = vec![0.0f64; n_row];
        unsafe {
            (lib.Highs_getSolution)(
                highs,
                col_val_rec.as_mut_ptr(),
                col_dual_rec.as_mut_ptr(),
                row_val_rec.as_mut_ptr(),
                row_dual_rec.as_mut_ptr(),
            )
        };

        // Maximum primal infeasibility across all columns and rows.
        let recovery_tol = (tolerance * 1e4).max(1e-2);
        let col_viol = col_val_rec
            .iter()
            .zip(col_lower.iter().zip(col_upper.iter()))
            .map(|(&x, (&lo, &hi))| (lo - x).max(x - hi).max(0.0))
            .fold(0.0_f64, f64::max);
        let row_viol = row_val_rec
            .iter()
            .zip(row_lower.iter().zip(row_upper.iter()))
            .map(|(&rx, (&lo, &hi))| (lo - rx).max(rx - hi).max(0.0))
            .fold(0.0_f64, f64::max);
        let max_viol = col_viol.max(row_viol);

        if max_viol <= recovery_tol {
            let mut obj_val_rec = 0.0f64;
            let obj_info_c =
                CString::new("objective_function_value").expect("static CStr has no null bytes");
            unsafe { (lib.Highs_getDoubleInfoValue)(highs, obj_info_c.as_ptr(), &mut obj_val_rec) };
            obj_val_rec *= objective_scale;
            // Unscale duals/reduced costs to match the unscaled objective.
            // HiGHS solved a problem with cost coefficients divided by
            // `objective_scale`, so the duals it returns are also scaled
            // down by the same factor.
            if objective_scale != 1.0 {
                for v in row_dual_rec.iter_mut() {
                    *v *= objective_scale;
                }
                for v in col_dual_rec.iter_mut() {
                    *v *= objective_scale;
                }
            }

            warn!(
                max_viol,
                model_status,
                "HiGHS QP SOLVE_ERROR: recovering solution (max_viol={max_viol:.2e} <= {recovery_tol:.2e})"
            );
            return Ok(SparseQpSolution {
                x: col_val_rec,
                row_dual: row_dual_rec,
                col_dual: col_dual_rec,
                objective: obj_val_rec,
                iterations: 0,
                converged: true,
            });
        }
        // Recovery failed — infeasibility too large, report as unsolvable.
        return Err(format!(
            "HiGHS QP SOLVE_ERROR: solution recovery failed (max_viol={max_viol:.2e} > {recovery_tol:.2e})"
        ));
    }
    // For QP: only accept OPTIMAL. kReachedIterationLimit indicates QUASS stalled;
    // on large degenerate problems (10k+ buses) the stall point is far from the
    // true optimum (e.g., $5B objective vs $2.4M correct), so we must NOT treat
    // it as converged. The LP warm-start above makes OPTIMAL reachable for most
    // cases; if it still fails, report as not-converged rather than returning garbage.
    //
    // Known HiGHS QUASS limitation: networks with series-compensated branches
    // (x < 0) produce non-PSD constraint matrices that cause QUASS to stall.
    // QUASS has a hardcoded internal iteration cap (~2×num_rows) that ignores
    // both qp_iteration_limit and simplex_iteration_limit options. Even with
    // LP warm-start providing a good basis, QUASS gets stuck at a suboptimal
    // point with basis errors (e.g. case300: $759K vs correct $706K, 7.5% off).
    // Presolve=off and higher iteration limits have no effect.
    // Gurobi and COPT use barrier/IPM QP algorithms that handle this naturally.
    // Workarounds: use --solver gurobi/copt, or use_pwl_costs=true (LP path).
    let converged = model_status == MODEL_STATUS_OPTIMAL;

    if !converged {
        debug!(
            model_status,
            "HiGHS QP: not optimal (7=optimal, 8=infeasible, 9=unbounded, 13=iteration_limit)"
        );
    }

    if model_status == MODEL_STATUS_INFEASIBLE {
        let iis_detail = extract_iis_detail(
            lib, highs, n_col, n_row, row_lower, row_upper, col_lower, col_upper,
        );
        return Err(format!("Problem is infeasible{iis_detail}"));
    }
    if model_status == MODEL_STATUS_UNBOUNDED {
        // Extract primal ray to identify which variable is unbounded
        let mut has_ray: HighsInt = 0;
        let mut ray = vec![0.0f64; n_col];
        unsafe { (lib.Highs_getPrimalRay)(highs, &mut has_ray, ray.as_mut_ptr()) };
        if has_ray != 0 {
            // Log ray direction: variables with large magnitude drive unboundedness
            let mut top: Vec<(usize, f64)> =
                ray.iter().enumerate().map(|(i, &v)| (i, v.abs())).collect();
            top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            tracing::warn!("UNBOUNDED primal ray (top 10 variables by magnitude):");
            for (idx, _mag) in top.iter().take(10) {
                tracing::warn!("  var[{idx}] ray={:.6e}", ray[*idx]);
            }
        }
        return Err("Problem is unbounded".into());
    }

    // Extract solution
    let mut col_value = vec![0.0; n_col];
    let mut col_dual_out = vec![0.0; n_col];
    let mut row_value = vec![0.0; n_row];
    let mut row_dual_out = vec![0.0; n_row];

    let get_solution_status = unsafe {
        (lib.Highs_getSolution)(
            highs,
            col_value.as_mut_ptr(),
            col_dual_out.as_mut_ptr(),
            row_value.as_mut_ptr(),
            row_dual_out.as_mut_ptr(),
        )
    };
    if get_solution_status != STATUS_OK {
        warn!(
            get_solution_status,
            model_status, "HiGHS_getSolution returned a non-OK status for MIP solve"
        );
    }

    let mut obj_val = 0.0;
    let obj_info =
        CString::new("objective_function_value").expect("static string contains no null bytes");
    unsafe { (lib.Highs_getDoubleInfoValue)(highs, obj_info.as_ptr(), &mut obj_val) };
    obj_val *= objective_scale;
    // Unscale duals/reduced costs to match the unscaled objective.
    // HiGHS solved a problem with cost coefficients divided by
    // `objective_scale`, so the duals it returns are also scaled
    // down by the same factor.
    if objective_scale != 1.0 {
        for v in row_dual_out.iter_mut() {
            *v *= objective_scale;
        }
        for v in col_dual_out.iter_mut() {
            *v *= objective_scale;
        }
    }

    let mut iters: HighsInt = 0;
    let iter_info =
        CString::new("simplex_iteration_count").expect("static string contains no null bytes");
    unsafe { (lib.Highs_getIntInfoValue)(highs, iter_info.as_ptr(), &mut iters) };
    let mut ipm_iters: HighsInt = 0;
    let ipm_info =
        CString::new("ipm_iteration_count").expect("static string contains no null bytes");
    unsafe { (lib.Highs_getIntInfoValue)(highs, ipm_info.as_ptr(), &mut ipm_iters) };

    Ok(SparseQpSolution {
        x: col_value,
        row_dual: row_dual_out,
        col_dual: col_dual_out,
        objective: obj_val,
        iterations: (iters + ipm_iters) as u32,
        converged,
    })
}

/// Solve a sparse MIP (Mixed-Integer Program) using `Highs_passMip`,
/// with an optional time limit in seconds.
#[allow(clippy::too_many_arguments)]
fn solve_sparse_mip_with_limit(
    lib: &HighsLib,
    n_col: usize,
    n_row: usize,
    col_cost: &[f64],
    col_lower: &[f64],
    col_upper: &[f64],
    row_lower: &[f64],
    row_upper: &[f64],
    a_start: &[HighsInt],
    a_index: &[HighsInt],
    a_value: &[f64],
    integrality: &[HighsInt],
    tolerance: f64,
    time_limit_secs: Option<f64>,
    mip_rel_gap: Option<f64>,
    primal_start: Option<&LpPrimalStart>,
) -> Result<SparseQpSolution, String> {
    let n_integer = integrality.iter().filter(|&&v| v != 0).count();
    info!(
        cols = n_col,
        rows = n_row,
        nnz = a_value.len(),
        integer_vars = n_integer,
        time_limit = ?time_limit_secs,
        "HiGHS sparse MIP: starting solve"
    );
    if n_col == 0 {
        return Ok(SparseQpSolution {
            x: vec![],
            row_dual: vec![],
            col_dual: vec![],
            objective: 0.0,
            iterations: 0,
            converged: true,
        });
    }

    unsafe {
        let highs = (lib.Highs_create)();
        if highs.is_null() {
            return Err("Failed to create HiGHS instance".into());
        }

        let result = solve_sparse_mip_inner(
            lib,
            highs,
            n_col,
            n_row,
            col_cost,
            col_lower,
            col_upper,
            row_lower,
            row_upper,
            a_start,
            a_index,
            a_value,
            integrality,
            tolerance,
            time_limit_secs,
            mip_rel_gap,
            primal_start,
        );

        (lib.Highs_destroy)(highs);
        result
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn solve_sparse_mip_inner(
    lib: &HighsLib,
    highs: *mut std::ffi::c_void,
    n_col: usize,
    n_row: usize,
    col_cost: &[f64],
    col_lower: &[f64],
    col_upper: &[f64],
    row_lower: &[f64],
    row_upper: &[f64],
    a_start: &[HighsInt],
    a_index: &[HighsInt],
    a_value: &[f64],
    integrality: &[HighsInt],
    tolerance: f64,
    time_limit_secs: Option<f64>,
    mip_rel_gap: Option<f64>,
    primal_start: Option<&LpPrimalStart>,
) -> Result<SparseQpSolution, String> {
    // Suppress output
    let output_flag = CString::new("output_flag").expect("static string contains no null bytes");
    unsafe { (lib.Highs_setBoolOptionValue)(highs, output_flag.as_ptr(), 0) };
    configure_presolve(lib, highs, primal_start.is_some());
    configure_mip_lp_solver(lib, highs);

    let objective_scale = objective_scale_factor(col_cost, None);
    let scaled_col_cost_storage = if objective_scale != 1.0 {
        Some(
            col_cost
                .iter()
                .map(|value| *value / objective_scale)
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let scaled_col_cost = scaled_col_cost_storage.as_deref().unwrap_or(col_cost);

    // Set tolerances
    let tol = tolerance.max(1e-10);
    let primal_tol =
        CString::new("primal_feasibility_tolerance").expect("static string contains no null bytes");
    let dual_tol =
        CString::new("dual_feasibility_tolerance").expect("static string contains no null bytes");
    unsafe { (lib.Highs_setDoubleOptionValue)(highs, primal_tol.as_ptr(), tol) };
    unsafe { (lib.Highs_setDoubleOptionValue)(highs, dual_tol.as_ptr(), tol) };

    // MIP integrality tolerance
    let mip_tol =
        CString::new("mip_feasibility_tolerance").expect("static string contains no null bytes");
    unsafe { (lib.Highs_setDoubleOptionValue)(highs, mip_tol.as_ptr(), tol.max(1e-6)) };

    // Time limit
    if let Some(limit) = time_limit_secs {
        let time_opt = CString::new("time_limit").expect("static string contains no null bytes");
        unsafe { (lib.Highs_setDoubleOptionValue)(highs, time_opt.as_ptr(), limit) };
    }

    // Set MIP relative gap.  When the caller provides an explicit target, use
    // that.  Otherwise, for large warm-started UC models the HiGHS default
    // 0.01% gap can spend a long time in root-node cut separation; fall back
    // to a 2% gap in that case.
    {
        let effective_gap = mip_rel_gap.or(
            if matches!(primal_start, Some(LpPrimalStart::Dense(_))) && n_col >= 100_000 {
                Some(0.02)
            } else {
                None
            },
        );
        if let Some(gap) = effective_gap {
            let rel_gap_opt =
                CString::new("mip_rel_gap").expect("static string contains no null bytes");
            unsafe { (lib.Highs_setDoubleOptionValue)(highs, rel_gap_opt.as_ptr(), gap) };
        }
        if matches!(primal_start, Some(LpPrimalStart::Dense(_)))
            && n_col >= 100_000
            && mip_rel_gap.is_none()
        {
            let heuristic_effort_opt =
                CString::new("mip_heuristic_effort").expect("static string contains no null bytes");
            unsafe { (lib.Highs_setDoubleOptionValue)(highs, heuristic_effort_opt.as_ptr(), 0.0) };
        }
    }

    // Pass MIP model in one call (CSC format)
    let a_num_nz = a_value.len();
    let status = unsafe {
        (lib.Highs_passMip)(
            highs,
            n_col as HighsInt,
            n_row as HighsInt,
            a_num_nz as HighsInt,
            MATRIX_FORMAT_COLUMN_WISE,
            OBJECTIVE_SENSE_MINIMIZE,
            0.0, // offset
            scaled_col_cost.as_ptr(),
            col_lower.as_ptr(),
            col_upper.as_ptr(),
            row_lower.as_ptr(),
            row_upper.as_ptr(),
            a_start.as_ptr(),
            a_index.as_ptr(),
            a_value.as_ptr(),
            integrality.as_ptr(),
        )
    };
    if status < 0 {
        return Err(format!("Highs_passMip failed: status {status}"));
    }

    apply_primal_start_hint(lib, highs, primal_start, n_col, "highs_mip_trace");

    let incumbent_capture = MipIncumbentCapture {
        best_solution: Mutex::new(None),
        interrupt_after_secs: time_limit_secs.filter(|limit| limit.is_finite() && *limit > 0.0),
        interrupt_requested: AtomicBool::new(false),
    };
    let callback_registered = if let (Some(set_callback), Some(start_callback)) =
        (lib.Highs_setCallback, lib.Highs_startCallback)
    {
        let status = unsafe {
            set_callback(
                highs,
                capture_highs_mip_solution_callback,
                (&incumbent_capture as *const MipIncumbentCapture)
                    .cast_mut()
                    .cast(),
            )
        };
        if status == STATUS_OK {
            unsafe {
                start_callback(highs, CALLBACK_MIP_SOLUTION);
                start_callback(highs, CALLBACK_MIP_IMPROVING_SOLUTION);
                start_callback(highs, CALLBACK_MIP_LOGGING);
                start_callback(highs, CALLBACK_MIP_INTERRUPT);
            }
            true
        } else {
            false
        }
    } else {
        false
    };

    // Solve
    let run_status = unsafe { (lib.Highs_run)(highs) };
    if run_status == -1 {
        return Err("HiGHS MIP solver returned error status".into());
    }

    let model_status = unsafe { (lib.Highs_getModelStatus)(highs) };
    // "converged" means the solver proved optimality or met a gap target — NOT time-limit.
    // Time-limit is handled separately: we return the best feasible solution found but mark
    // converged=false so the backend maps it to LpSolveStatus::SubOptimal rather than Optimal.
    // This lets callers distinguish a proven-optimal MIP solve from a time-limited grab.
    let converged = model_status == MODEL_STATUS_OPTIMAL
        || model_status == MODEL_STATUS_OBJECTIVE_BOUND
        || model_status == MODEL_STATUS_OBJECTIVE_TARGET;
    let interrupted = model_status == MODEL_STATUS_INTERRUPT;
    let time_limit_feasible = model_status == MODEL_STATUS_REACHED_TIME_LIMIT;

    if model_status == MODEL_STATUS_INFEASIBLE {
        let iis_detail = extract_iis_detail(
            lib, highs, n_col, n_row, row_lower, row_upper, col_lower, col_upper,
        );
        return Err(format!("MIP is infeasible{iis_detail}"));
    }
    if model_status == MODEL_STATUS_UNBOUNDED {
        return Err("MIP is unbounded".into());
    }
    if !converged && !time_limit_feasible && !interrupted {
        return Err(format!(
            "MIP solver stopped with model status {model_status}"
        ));
    }
    if time_limit_feasible || interrupted {
        warn!(
            model_status,
            "HiGHS MIP stopped before proving optimality — returning best feasible solution; \
             solution is NOT proven optimal (LpSolveStatus will be SubOptimal)"
        );
    }

    let mut mip_primal_bound = f64::NAN;
    let mut mip_dual_bound = f64::NAN;
    let mut mip_gap = f64::NAN;
    let mip_primal_bound_info =
        CString::new("mip_primal_bound").expect("static string contains no null bytes");
    let mip_dual_bound_info =
        CString::new("mip_dual_bound").expect("static string contains no null bytes");
    let mip_gap_info = CString::new("mip_gap").expect("static string contains no null bytes");
    unsafe {
        (lib.Highs_getDoubleInfoValue)(
            highs,
            mip_primal_bound_info.as_ptr(),
            &mut mip_primal_bound,
        );
        (lib.Highs_getDoubleInfoValue)(highs, mip_dual_bound_info.as_ptr(), &mut mip_dual_bound);
        (lib.Highs_getDoubleInfoValue)(highs, mip_gap_info.as_ptr(), &mut mip_gap);
    }
    if mip_primal_bound.is_finite() {
        mip_primal_bound *= objective_scale;
    }
    if mip_dual_bound.is_finite() {
        mip_dual_bound *= objective_scale;
    }

    // Extract solution (duals are not meaningful for MIP)
    let mut col_value = vec![0.0; n_col];
    let mut col_dual_out = vec![0.0; n_col];
    let mut row_value = vec![0.0; n_row];
    let mut row_dual_out = vec![0.0; n_row];

    let get_solution_status = unsafe {
        (lib.Highs_getSolution)(
            highs,
            col_value.as_mut_ptr(),
            col_dual_out.as_mut_ptr(),
            row_value.as_mut_ptr(),
            row_dual_out.as_mut_ptr(),
        )
    };
    if get_solution_status != STATUS_OK {
        warn!(
            get_solution_status,
            model_status, "HiGHS_getSolution returned a non-OK status for MIP solve"
        );
    }

    if callback_registered {
        if let Some(stop_callback) = lib.Highs_stopCallback {
            unsafe {
                stop_callback(highs, CALLBACK_MIP_SOLUTION);
                stop_callback(highs, CALLBACK_MIP_IMPROVING_SOLUTION);
                stop_callback(highs, CALLBACK_MIP_LOGGING);
                stop_callback(highs, CALLBACK_MIP_INTERRUPT);
            }
        }
    }

    let callback_incumbent = incumbent_capture
        .best_solution
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().cloned());
    let has_callback_incumbent = callback_incumbent
        .as_ref()
        .is_some_and(|best_solution| best_solution.len() == n_col);
    let has_feasible_incumbent = mip_primal_bound.is_finite() || has_callback_incumbent;

    if (time_limit_feasible || interrupted) && !has_feasible_incumbent {
        return Err(
            "HiGHS MIP reached the time limit before producing any feasible incumbent".into(),
        );
    }

    if (!converged || time_limit_feasible || interrupted)
        && let Some(best_solution) = callback_incumbent.as_ref()
        && best_solution.len() == n_col
    {
        col_value.clone_from(best_solution);
    }

    if env_flag("SURGE_DEBUG_HIGHS_MIP") {
        let incumbent_captured = callback_incumbent.as_ref().map(Vec::len).unwrap_or(0);
        log_highs_mip_trace(format!(
            "highs_mip_trace model_status={} converged={} time_limit_feasible={} has_feasible_incumbent={} get_solution_status={} incumbent_captured_len={} primal_bound={:.12} dual_bound={:.12} mip_gap={:.12}",
            model_status,
            converged,
            time_limit_feasible || interrupted,
            has_feasible_incumbent,
            get_solution_status,
            incumbent_captured,
            mip_primal_bound,
            mip_dual_bound,
            mip_gap,
        ));
    }

    let mut obj_val = 0.0;
    let obj_info =
        CString::new("objective_function_value").expect("static string contains no null bytes");
    unsafe { (lib.Highs_getDoubleInfoValue)(highs, obj_info.as_ptr(), &mut obj_val) };
    obj_val *= objective_scale;
    // MIP: duals are not meaningful, but unscale defensively in case any
    // downstream consumer treats them as if they were.
    if objective_scale != 1.0 {
        for v in row_dual_out.iter_mut() {
            *v *= objective_scale;
        }
        for v in col_dual_out.iter_mut() {
            *v *= objective_scale;
        }
    }

    let mut iters: HighsInt = 0;
    let iter_info =
        CString::new("simplex_iteration_count").expect("static string contains no null bytes");
    unsafe { (lib.Highs_getIntInfoValue)(highs, iter_info.as_ptr(), &mut iters) };
    let mut mip_node_count: HighsInt = 0;
    let mip_info = CString::new("mip_node_count").expect("static string contains no null bytes");
    unsafe { (lib.Highs_getIntInfoValue)(highs, mip_info.as_ptr(), &mut mip_node_count) };

    Ok(SparseQpSolution {
        x: col_value,
        row_dual: row_dual_out,
        col_dual: col_dual_out,
        objective: obj_val,
        iterations: (iters.max(mip_node_count)) as u32,
        converged,
    })
}

// ---------------------------------------------------------------------------
// LpSolver trait implementation
// ---------------------------------------------------------------------------

/// HiGHS LP/QP/MIP solver (open-source, MIT license).
///
/// Loaded at runtime via `libloading`. Install HiGHS as a system package
/// (`brew install highs` / `apt install libhighs-dev`) or set `HIGHS_LIB_DIR`.
#[derive(Debug)]
pub struct HiGHSLpSolver {
    lib: Arc<HighsLib>,
}

impl std::fmt::Debug for HighsLib {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HighsLib")
    }
}

impl HiGHSLpSolver {
    /// Create a new HiGHS solver, loading the shared library at runtime.
    pub fn new() -> Result<Self, String> {
        let lib = get_highs()?.clone();
        Ok(Self { lib })
    }
}

impl LpSolver for HiGHSLpSolver {
    fn name(&self) -> &'static str {
        "HiGHS"
    }

    fn version(&self) -> &'static str {
        "runtime"
    }

    fn solve(&self, prob: &SparseProblem, opts: &LpOptions) -> Result<LpResult, String> {
        let is_mip = prob
            .integrality
            .as_ref()
            .is_some_and(|iv| iv.iter().any(|&v| is_integer_domain(v)));

        if is_mip {
            let integ = prob
                .integrality
                .as_ref()
                .expect("integrality Some when is_mip is true");
            let highs_integ: Vec<HighsInt> =
                integ.iter().map(|&v| highs_integrality_value(v)).collect();
            let sol = solve_sparse_mip_with_limit(
                &self.lib,
                prob.n_col,
                prob.n_row,
                &prob.col_cost,
                &prob.col_lower,
                &prob.col_upper,
                &prob.row_lower,
                &prob.row_upper,
                &prob.a_start,
                &prob.a_index,
                &prob.a_value,
                &highs_integ,
                opts.tolerance,
                opts.time_limit_secs,
                opts.mip_rel_gap,
                opts.primal_start.as_ref(),
            )?;

            let status = if sol.converged {
                LpSolveStatus::Optimal
            } else {
                LpSolveStatus::SubOptimal
            };

            Ok(LpResult {
                x: sol.x,
                row_dual: sol.row_dual.iter().map(|&d| -d).collect(),
                col_dual: sol.col_dual,
                objective: sol.objective,
                status,
                iterations: sol.iterations,
                mip_trace: None,
            })
        } else {
            let sol = solve_sparse_qp(
                &self.lib,
                prob.n_col,
                prob.n_row,
                &prob.col_cost,
                &prob.col_lower,
                &prob.col_upper,
                &prob.row_lower,
                &prob.row_upper,
                &prob.a_start,
                &prob.a_index,
                &prob.a_value,
                prob.q_start.as_deref(),
                prob.q_index.as_deref(),
                prob.q_value.as_deref(),
                opts.tolerance,
                opts.time_limit_secs,
                opts.primal_start.as_ref(),
                opts.algorithm,
            )?;

            let status = if sol.converged {
                LpSolveStatus::Optimal
            } else {
                LpSolveStatus::SubOptimal
            };

            Ok(LpResult {
                x: sol.x,
                row_dual: sol.row_dual.iter().map(|&d| -d).collect(),
                col_dual: sol.col_dual,
                objective: sol.objective,
                status,
                iterations: sol.iterations,
                mip_trace: None,
            })
        }
    }
}
