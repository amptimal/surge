// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! HiGHS LP/QP/MIP backend.
//!
//! Uses the HiGHS C API via runtime `libloading` — no link-time dependency.
//! Install HiGHS as a system package (`brew install highs`, `apt install
//! libhighs-dev`) or set `HIGHS_LIB_DIR` to the directory containing
//! `libhighs.{so,dylib}`.

use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::{Arc, OnceLock};

use libloading::Library;
use tracing::{debug, info, warn};

use super::{LpOptions, LpResult, LpSolveStatus, LpSolver, SparseProblem, VariableDomain};

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

fn is_integer_domain(domain: VariableDomain) -> bool {
    !matches!(domain, VariableDomain::Continuous)
}

fn highs_integrality_value(domain: VariableDomain) -> HighsInt {
    match domain {
        VariableDomain::Continuous => 0,
        VariableDomain::Binary | VariableDomain::Integer => 1,
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

fn maybe_override_presolve(lib: &HighsLib, highs: *mut std::ffi::c_void) {
    let Some(value) = std::env::var("SURGE_HIGHS_PRESOLVE").ok() else {
        return;
    };
    let normalized = value.trim().to_ascii_lowercase();
    let override_value = match normalized.as_str() {
        "0" | "off" | "false" => "off",
        "1" | "on" | "true" => "on",
        _ => return,
    };
    let option = CString::new("presolve").expect("static string contains no null bytes");
    let setting = CString::new(override_value).expect("static string contains no null bytes");
    unsafe {
        (lib.Highs_setStringOptionValue)(highs, option.as_ptr(), setting.as_ptr());
    }
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
            lib, highs, n_col, n_row, col_cost, col_lower, col_upper, row_lower, row_upper,
            a_start, a_index, a_value, q_start, q_index, q_value, tolerance,
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
) -> Result<SparseQpSolution, String> {
    // Suppress output (set SURGE_HIGHS_VERBOSE=1 to enable HiGHS logging)
    let verbose = std::env::var("SURGE_HIGHS_VERBOSE").is_ok();
    let output_flag = CString::new("output_flag").expect("static string contains no null bytes");
    unsafe {
        (lib.Highs_setBoolOptionValue)(highs, output_flag.as_ptr(), if verbose { 1 } else { 0 })
    };
    maybe_override_presolve(lib, highs);

    // Set tolerances
    let tol = tolerance.max(1e-10);
    let primal_tol =
        CString::new("primal_feasibility_tolerance").expect("static string contains no null bytes");
    let dual_tol =
        CString::new("dual_feasibility_tolerance").expect("static string contains no null bytes");
    unsafe { (lib.Highs_setDoubleOptionValue)(highs, primal_tol.as_ptr(), tol) };
    unsafe { (lib.Highs_setDoubleOptionValue)(highs, dual_tol.as_ptr(), tol) };

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
            q_value.expect("q_value Some when q_nnz > 0"),
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
                    (lib.Highs_passLp)(
                        lp_highs,
                        n_col as HighsInt,
                        n_row as HighsInt,
                        a_num_nz as HighsInt,
                        MATRIX_FORMAT_COLUMN_WISE,
                        OBJECTIVE_SENSE_MINIMIZE,
                        0.0,
                        col_cost.as_ptr(),
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
                col_cost.as_ptr(),
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
                col_cost.as_ptr(),
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

    unsafe {
        (lib.Highs_getSolution)(
            highs,
            col_value.as_mut_ptr(),
            col_dual_out.as_mut_ptr(),
            row_value.as_mut_ptr(),
            row_dual_out.as_mut_ptr(),
        )
    };

    let mut obj_val = 0.0;
    let obj_info =
        CString::new("objective_function_value").expect("static string contains no null bytes");
    unsafe { (lib.Highs_getDoubleInfoValue)(highs, obj_info.as_ptr(), &mut obj_val) };

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
) -> Result<SparseQpSolution, String> {
    // Suppress output
    let output_flag = CString::new("output_flag").expect("static string contains no null bytes");
    unsafe { (lib.Highs_setBoolOptionValue)(highs, output_flag.as_ptr(), 0) };
    maybe_override_presolve(lib, highs);

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
            col_cost.as_ptr(),
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
    if !converged && !time_limit_feasible {
        return Err(format!(
            "MIP solver stopped with model status {model_status}"
        ));
    }
    if time_limit_feasible {
        warn!(
            model_status,
            "HiGHS MIP reached time limit — returning best feasible solution; \
             solution is NOT proven optimal (LpSolveStatus will be SubOptimal)"
        );
    }

    // Extract solution (duals are not meaningful for MIP)
    let mut col_value = vec![0.0; n_col];
    let mut col_dual_out = vec![0.0; n_col];
    let mut row_value = vec![0.0; n_row];
    let mut row_dual_out = vec![0.0; n_row];

    unsafe {
        (lib.Highs_getSolution)(
            highs,
            col_value.as_mut_ptr(),
            col_dual_out.as_mut_ptr(),
            row_value.as_mut_ptr(),
            row_dual_out.as_mut_ptr(),
        )
    };

    let mut obj_val = 0.0;
    let obj_info =
        CString::new("objective_function_value").expect("static string contains no null bytes");
    unsafe { (lib.Highs_getDoubleInfoValue)(highs, obj_info.as_ptr(), &mut obj_val) };

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
            })
        }
    }
}
