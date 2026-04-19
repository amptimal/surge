// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! COPT (Cardinal Optimizer) LP/QP/MIP/NLP backend — COPT 8.x API.
//!
//! Implements [`LpSolver`] (`CoptLpSolver`), [`NlpSolver`] (`CoptNlpSolver`),
//! and [`QcqpSolver`] (`CoptQcqpSolver`).
//!
//! NLP is supported via `copt_nlp_shim.cpp`, a C++ bridge that reconstructs
//! COPT 8.x's `INlpCallback` vtable interface (the header `nlpcallback.h` is
//! not distributed, so we reconstruct it from `nlpcallbackbase.h`). The shim
//! is compiled into a standalone shared library (`libsurge_copt_nlp.so`) via
//! `scripts/build-copt-nlp-shim.sh` and loaded at runtime via `libloading`.
//!
//! `CoptNlpSolver` is selected by `default_nlp_solver()` when COPT is
//! installed and licensed, and the NLP shim library is found; Ipopt is the
//! open-source fallback.
//!
//! COPT LP/QP/MIP support only needs the COPT 8.x C library at runtime:
//! ```sh
//! export COPT_HOME=/opt/copt80
//! export LD_LIBRARY_PATH=$COPT_HOME/lib:$LD_LIBRARY_PATH
//! ```
//!
//! To also enable COPT NLP, build the standalone shim:
//! ```sh
//! scripts/build-copt-nlp-shim.sh
//! ```
//!
//! A valid COPT license is required.  Evaluation licenses are available at
//! <https://www.shanshu.ai/copt>.
//!
//! # LP/QP/MIP loading strategy
//!
//! We use a two-step pattern following the COPT 8.x official examples:
//! 1. `COPT_AddCols` — adds variables with obj/bounds/type but **no** constraint
//!    matrix (COPT 8.x requires rows to exist before CSC matrix can reference them).
//! 2. `COPT_AddRows` — adds constraints in CSR format (transposed from our CSC).
//!
//! The CSC→CSR transposition is done in `csc_to_csr`.
//!
//! # Row encoding for `COPT_AddRows`
//!
//! COPT 8.x uses a sense/bound/upper triple:
//! - `'L'` (`COPT_LESS_EQUAL`): `Ax <= rowBound[i]`
//! - `'G'` (`COPT_GREATER_EQUAL`): `Ax >= rowBound[i]`
//! - `'E'` (`COPT_EQUAL`): `Ax == rowBound[i]`
//! - `'R'` (`COPT_RANGE`): `rowBound[i] <= Ax <= rowUpper[i]`
//!
//! We map from the internal `SparseProblem` row_lower/row_upper format.
//!
//! # Dual sign convention
//!
//! COPT row duals have the opposite sign from standard Lagrange convention.
//! We negate before storing in `LpResult::row_dual`.

pub use self::impl_::{CoptLpSolver, CoptNlpSolver, CoptQcqpSolver};

// Rust 2024 requires explicit `unsafe {}` blocks inside `unsafe fn` bodies.
// This module contains FFI-heavy `unsafe fn` helpers where every line calls
// an unsafe C API.  The `#[allow]` annotation is intentional: the functions
// are correctly `unsafe` (callers must hold the COPT env/prob lifetimes), and
// wrapping each individual FFI call in its own `unsafe {}` would add syntactic
// noise without adding safety information.
#[allow(unsafe_op_in_unsafe_fn)]
mod impl_ {
    use libloading::Library;
    use std::any::Any;
    use std::cell::RefCell;
    use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::ptr;
    use std::sync::{Arc, OnceLock};

    use crate::backends::{
        LpOptions, LpResult, LpSolveStatus, LpSolver, SparseProblem, VariableDomain,
    };
    use crate::backends::{NlpOptions, NlpProblem, NlpSolution, NlpSolver};

    fn is_integer_domain(domain: VariableDomain) -> bool {
        !matches!(domain, VariableDomain::Continuous)
    }

    fn copt_col_type(domain: VariableDomain) -> c_char {
        match domain {
            VariableDomain::Continuous => ffi::CTYPE_CONT as c_char,
            VariableDomain::Binary => ffi::CTYPE_BIN as c_char,
            VariableDomain::Integer => ffi::CTYPE_INT as c_char,
        }
    }

    // ── Suppress COPT license warnings on stdout ───────────────────────────────
    //
    // COPT's `COPT_CreateEnv` / `Envr()` prints license check diagnostics
    // directly to stdout before any logging parameter can be set.  When the CLI
    // uses `-o json`, this contaminates the JSON output.  We temporarily redirect
    // fd 1 to /dev/null around environment creation.
    #[cfg(unix)]
    struct StdoutSuppressor {
        saved_fd: c_int,
    }

    #[cfg(unix)]
    unsafe extern "C" {
        fn dup(fd: c_int) -> c_int;
        fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
        fn close(fd: c_int) -> c_int;
        fn open(path: *const c_char, oflag: c_int) -> c_int;
    }

    #[cfg(unix)]
    impl StdoutSuppressor {
        fn new() -> Option<Self> {
            unsafe {
                let saved = dup(1);
                if saved < 0 {
                    return None;
                }
                let devnull = open(c"/dev/null".as_ptr(), 1); // O_WRONLY = 1
                if devnull >= 0 {
                    dup2(devnull, 1);
                    close(devnull);
                    Some(Self { saved_fd: saved })
                } else {
                    close(saved);
                    None
                }
            }
        }
    }

    #[cfg(unix)]
    impl Drop for StdoutSuppressor {
        fn drop(&mut self) {
            unsafe {
                dup2(self.saved_fd, 1);
                close(self.saved_fd);
            }
        }
    }

    #[cfg(not(unix))]
    struct StdoutSuppressor;

    #[cfg(not(unix))]
    impl StdoutSuppressor {
        fn new() -> Option<Self> {
            // Not supported on Windows — COPT warnings will appear on stdout.
            None
        }
    }

    // ── COPT C API FFI (COPT 8.x) ─────────────────────────────────────────────

    #[allow(non_camel_case_types, clippy::upper_case_acronyms, dead_code)]
    mod ffi {
        use std::ffi::c_int;

        // Opaque COPT types.
        pub enum copt_env {}
        pub enum copt_prob {}

        pub type CoptEnvPtr = *mut copt_env;
        pub type CoptProbPtr = *mut copt_prob;

        pub const COPT_BUFFSIZE: c_int = 1000;

        // Objective sense.
        pub const COPT_MINIMIZE: c_int = 1;

        // Row sense characters (COPT 8.x).
        pub const COPT_LESS_EQUAL: u8 = b'L';
        pub const COPT_GREATER_EQUAL: u8 = b'G';
        pub const COPT_EQUAL: u8 = b'E';
        pub const COPT_RANGE: u8 = b'R';

        // Column type characters.
        pub const CTYPE_CONT: u8 = b'C';
        pub const CTYPE_INT: u8 = b'I';
        pub const CTYPE_BIN: u8 = b'B';

        // Attribute names (NUL-terminated byte slices).
        pub const ATTR_LPOBJVAL: &[u8] = b"LpObjval\0";
        pub const ATTR_BESTOBJ: &[u8] = b"BestObj\0";
        pub const ATTR_LPSTATUS: &[u8] = b"LpStatus\0";
        pub const ATTR_MIPSTATUS: &[u8] = b"MipStatus\0";
        pub const ATTR_SIMPLEXITER: &[u8] = b"SimplexIter\0";
        pub const ATTR_NODECNT: &[u8] = b"NodeCnt\0";

        // LP status codes (COPT 8.x).
        pub const COPT_LPSTATUS_OPTIMAL: c_int = 1;
        pub const COPT_LPSTATUS_INFEASIBLE: c_int = 2;
        pub const COPT_LPSTATUS_UNBOUNDED: c_int = 3;
        pub const COPT_LPSTATUS_TIMEOUT: c_int = 8;

        // MIP status codes (COPT 8.x).
        pub const COPT_MIPSTATUS_OPTIMAL: c_int = 1;
        pub const COPT_MIPSTATUS_INFEASIBLE: c_int = 2;
        pub const COPT_MIPSTATUS_UNBOUNDED: c_int = 3;
        pub const COPT_MIPSTATUS_TIMEOUT: c_int = 8;

        // API return codes.
        pub const COPT_RETCODE_OK: c_int = 0;
        pub const COPT_RETCODE_LICENSE: c_int = 4;
    }

    // ── COPT libloading — runtime dynamic library detection ───────────────────

    #[allow(non_snake_case, dead_code)]
    struct CoptLib {
        _lib: Library,
        COPT_CreateEnv: unsafe extern "C" fn(*mut ffi::CoptEnvPtr) -> c_int,
        COPT_DeleteEnv: unsafe extern "C" fn(*mut ffi::CoptEnvPtr) -> c_int,
        COPT_CreateProb: unsafe extern "C" fn(ffi::CoptEnvPtr, *mut ffi::CoptProbPtr) -> c_int,
        COPT_DeleteProb: unsafe extern "C" fn(*mut ffi::CoptProbPtr) -> c_int,
        COPT_AddCols: unsafe extern "C" fn(
            ffi::CoptProbPtr,
            c_int,
            *const c_double,
            *const c_int,
            *const c_int,
            *const c_int,
            *const c_double,
            *const c_char,
            *const c_double,
            *const c_double,
            *const *const c_char,
        ) -> c_int,
        COPT_AddRows: unsafe extern "C" fn(
            ffi::CoptProbPtr,
            c_int,
            *const c_int,
            *const c_int,
            *const c_int,
            *const c_double,
            *const c_char,
            *const c_double,
            *const c_double,
            *const *const c_char,
        ) -> c_int,
        COPT_SetQuadObj: unsafe extern "C" fn(
            ffi::CoptProbPtr,
            c_int,
            *const c_int,
            *const c_int,
            *const c_double,
        ) -> c_int,
        COPT_AddQConstr: unsafe extern "C" fn(
            ffi::CoptProbPtr,
            c_int,
            *const c_int,
            *const c_double,
            c_int,
            *const c_int,
            *const c_int,
            *const c_double,
            c_char,
            c_double,
            *const c_char,
        ) -> c_int,
        COPT_SetObjSense: unsafe extern "C" fn(ffi::CoptProbPtr, c_int) -> c_int,
        COPT_SolveLp: unsafe extern "C" fn(ffi::CoptProbPtr) -> c_int,
        COPT_Solve: unsafe extern "C" fn(ffi::CoptProbPtr) -> c_int,
        COPT_GetLpSolution: unsafe extern "C" fn(
            ffi::CoptProbPtr,
            *mut c_double,
            *mut c_double,
            *mut c_double,
            *mut c_double,
        ) -> c_int,
        COPT_GetSolution: unsafe extern "C" fn(ffi::CoptProbPtr, *mut c_double) -> c_int,
        COPT_GetDblAttr:
            unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char, *mut c_double) -> c_int,
        COPT_GetIntAttr: unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char, *mut c_int) -> c_int,
        COPT_GetLicenseMsg: unsafe extern "C" fn(ffi::CoptEnvPtr, *mut c_char, c_int) -> c_int,
        COPT_SetDblParam: unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char, c_double) -> c_int,
        COPT_SetIntParam: unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char, c_int) -> c_int,
        COPT_SetLogFile: unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char) -> c_int,
    }

    static COPT_LIB: OnceLock<Result<Arc<CoptLib>, String>> = OnceLock::new();

    fn get_copt() -> Result<&'static Arc<CoptLib>, String> {
        COPT_LIB
            .get_or_init(|| {
                for path in copt_lib_paths() {
                    if let Ok(lib) = unsafe { Library::new(&path) } {
                        match unsafe { load_copt_symbols(lib) } {
                            Ok(clib) => return Ok(Arc::new(clib)),
                            Err(e) => return Err(e),
                        }
                    }
                }
                Err(
                    "COPT not found — set COPT_HOME or install COPT 8.x to /opt/copt80. \
                 Only COPT 8.x (libcopt.so) is supported; older versions have incompatible APIs."
                        .to_string(),
                )
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    fn copt_lib_paths() -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        if let Ok(home) = std::env::var("COPT_HOME") {
            paths.push(std::path::PathBuf::from(format!("{home}/lib/libcopt.so")));
            paths.push(std::path::PathBuf::from(format!(
                "{home}/lib/libcopt.dylib"
            )));
        }
        for prefix in &["/opt/copt80", "/opt/copt70", "/opt/copt"] {
            paths.push(std::path::PathBuf::from(format!("{prefix}/lib/libcopt.so")));
        }
        paths.push(std::path::PathBuf::from("libcopt.so"));
        paths.push(std::path::PathBuf::from("libcopt.dylib"));
        paths
    }

    unsafe fn load_copt_symbols(lib: Library) -> Result<CoptLib, String> {
        macro_rules! sym {
            ($name:literal, $ty:ty) => {
                *unsafe { lib.get::<$ty>($name) }
                    .map_err(|e| format!("COPT symbol {} not found: {e}", stringify!($name)))?
            };
        }
        Ok(CoptLib {
            COPT_CreateEnv: sym!(
                b"COPT_CreateEnv\0",
                unsafe extern "C" fn(*mut ffi::CoptEnvPtr) -> c_int
            ),
            COPT_DeleteEnv: sym!(
                b"COPT_DeleteEnv\0",
                unsafe extern "C" fn(*mut ffi::CoptEnvPtr) -> c_int
            ),
            COPT_CreateProb: sym!(
                b"COPT_CreateProb\0",
                unsafe extern "C" fn(ffi::CoptEnvPtr, *mut ffi::CoptProbPtr) -> c_int
            ),
            COPT_DeleteProb: sym!(
                b"COPT_DeleteProb\0",
                unsafe extern "C" fn(*mut ffi::CoptProbPtr) -> c_int
            ),
            COPT_AddCols: sym!(
                b"COPT_AddCols\0",
                unsafe extern "C" fn(
                    ffi::CoptProbPtr,
                    c_int,
                    *const c_double,
                    *const c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                    *const c_char,
                    *const c_double,
                    *const c_double,
                    *const *const c_char,
                ) -> c_int
            ),
            COPT_AddRows: sym!(
                b"COPT_AddRows\0",
                unsafe extern "C" fn(
                    ffi::CoptProbPtr,
                    c_int,
                    *const c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                    *const c_char,
                    *const c_double,
                    *const c_double,
                    *const *const c_char,
                ) -> c_int
            ),
            COPT_SetQuadObj: sym!(
                b"COPT_SetQuadObj\0",
                unsafe extern "C" fn(
                    ffi::CoptProbPtr,
                    c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                ) -> c_int
            ),
            COPT_AddQConstr: sym!(
                b"COPT_AddQConstr\0",
                unsafe extern "C" fn(
                    ffi::CoptProbPtr,
                    c_int,
                    *const c_int,
                    *const c_double,
                    c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                    c_char,
                    c_double,
                    *const c_char,
                ) -> c_int
            ),
            COPT_SetObjSense: sym!(
                b"COPT_SetObjSense\0",
                unsafe extern "C" fn(ffi::CoptProbPtr, c_int) -> c_int
            ),
            COPT_SolveLp: sym!(
                b"COPT_SolveLp\0",
                unsafe extern "C" fn(ffi::CoptProbPtr) -> c_int
            ),
            COPT_Solve: sym!(
                b"COPT_Solve\0",
                unsafe extern "C" fn(ffi::CoptProbPtr) -> c_int
            ),
            COPT_GetLpSolution: sym!(
                b"COPT_GetLpSolution\0",
                unsafe extern "C" fn(
                    ffi::CoptProbPtr,
                    *mut c_double,
                    *mut c_double,
                    *mut c_double,
                    *mut c_double,
                ) -> c_int
            ),
            COPT_GetSolution: sym!(
                b"COPT_GetSolution\0",
                unsafe extern "C" fn(ffi::CoptProbPtr, *mut c_double) -> c_int
            ),
            COPT_GetDblAttr: sym!(
                b"COPT_GetDblAttr\0",
                unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char, *mut c_double) -> c_int
            ),
            COPT_GetIntAttr: sym!(
                b"COPT_GetIntAttr\0",
                unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char, *mut c_int) -> c_int
            ),
            COPT_GetLicenseMsg: sym!(
                b"COPT_GetLicenseMsg\0",
                unsafe extern "C" fn(ffi::CoptEnvPtr, *mut c_char, c_int) -> c_int
            ),
            COPT_SetDblParam: sym!(
                b"COPT_SetDblParam\0",
                unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char, c_double) -> c_int
            ),
            COPT_SetIntParam: sym!(
                b"COPT_SetIntParam\0",
                unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char, c_int) -> c_int
            ),
            COPT_SetLogFile: sym!(
                b"COPT_SetLogFile\0",
                unsafe extern "C" fn(ffi::CoptProbPtr, *const c_char) -> c_int
            ),
            _lib: lib,
        })
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Transpose a CSC constraint matrix to CSR format.
    ///
    /// Input (CSC): `a_start` (len n_col+1), `a_index` (row indices), `a_value`.
    /// Output (CSR): `row_start` (len n_row+1), `col_index`, `values`.
    fn csc_to_csr(
        n_col: usize,
        n_row: usize,
        a_start: &[i32],
        a_index: &[i32],
        a_value: &[f64],
    ) -> (Vec<i32>, Vec<i32>, Vec<f64>) {
        let nnz = a_value.len();

        // Count nonzeros per row.
        let mut row_count = vec![0i32; n_row];
        for &row in a_index {
            row_count[row as usize] += 1;
        }

        // Build CSR start pointers.
        let mut row_start = vec![0i32; n_row + 1];
        for i in 0..n_row {
            row_start[i + 1] = row_start[i] + row_count[i];
        }

        // Fill col_index and values using a working cursor per row.
        let mut col_index = vec![0i32; nnz];
        let mut values = vec![0.0f64; nnz];
        let mut row_pos = row_start[..n_row].to_vec();

        for j in 0..n_col {
            for k in (a_start[j] as usize)..(a_start[j + 1] as usize) {
                let row = a_index[k] as usize;
                let pos = row_pos[row] as usize;
                col_index[pos] = j as i32;
                values[pos] = a_value[k];
                row_pos[row] += 1;
            }
        }

        (row_start, col_index, values)
    }

    /// Convert row_lower / row_upper (SparseProblem format) to COPT 8.x
    /// sense / row_bound / row_upper triple for `COPT_AddRows`.
    ///
    /// COPT 8.x RANGE convention (empirically verified):
    ///   'R': `rowBound - rowUpper <= Ax <= rowBound`
    ///   i.e. rowBound = hi (upper bound), rowUpper = hi - lo (width, non-negative).
    ///
    /// - lower == upper (equality): sense='E', bound=lower
    /// - lower == -inf (LE): sense='L', bound=upper
    /// - upper == +inf (GE): sense='G', bound=lower
    /// - both finite, different (range): sense='R', bound=hi, upper=(hi-lo)
    /// - both infinite (free): sense='L', bound=1e30
    fn convert_row_bounds(
        row_lower: &[f64],
        row_upper: &[f64],
    ) -> (Vec<c_char>, Vec<f64>, Vec<f64>) {
        const COPT_INF: f64 = 1e30;
        let n = row_lower.len();
        let mut sense = Vec::with_capacity(n);
        let mut bound = Vec::with_capacity(n);
        let mut upper = Vec::with_capacity(n);

        for i in 0..n {
            let lo = row_lower[i];
            let hi = row_upper[i];
            let lo_inf = lo <= -COPT_INF || lo.is_infinite();
            let hi_inf = hi >= COPT_INF || hi.is_infinite();

            if !lo_inf && !hi_inf && (lo - hi).abs() <= 1e-12 * (1.0 + lo.abs()) {
                // Equality.
                sense.push(ffi::COPT_EQUAL as c_char);
                bound.push(lo);
                upper.push(lo);
            } else if lo_inf && !hi_inf {
                // LE: Ax <= hi.
                sense.push(ffi::COPT_LESS_EQUAL as c_char);
                bound.push(hi);
                upper.push(hi);
            } else if !lo_inf && hi_inf {
                // GE: Ax >= lo.
                sense.push(ffi::COPT_GREATER_EQUAL as c_char);
                bound.push(lo);
                upper.push(lo);
            } else if !lo_inf && !hi_inf {
                // Range: lo <= Ax <= hi.
                // COPT 8.x 'R' semantics: [rowBound - rowUpper, rowBound]
                // so rowBound = hi, rowUpper = hi - lo (the width).
                sense.push(ffi::COPT_RANGE as c_char);
                bound.push(hi);
                upper.push(hi - lo);
            } else {
                // Free row.
                sense.push(ffi::COPT_LESS_EQUAL as c_char);
                bound.push(COPT_INF);
                upper.push(COPT_INF);
            }
        }
        (sense, bound, upper)
    }

    /// Build a CString from a NUL-terminated byte slice constant.
    fn attr_cstr(bytes: &[u8]) -> CString {
        CString::new(
            std::str::from_utf8(bytes)
                .expect("COPT attribute bytes are valid UTF-8")
                .trim_end_matches('\0'),
        )
        .expect("COPT attribute string contains no null bytes")
    }

    fn copt_license_message(lib: &CoptLib, env: ffi::CoptEnvPtr) -> Option<String> {
        if env.is_null() {
            return None;
        }
        let mut buf = vec![0 as c_char; ffi::COPT_BUFFSIZE as usize];
        let rc = unsafe { (lib.COPT_GetLicenseMsg)(env, buf.as_mut_ptr(), buf.len() as c_int) };
        if rc != ffi::COPT_RETCODE_OK {
            return None;
        }
        let msg = unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .trim()
            .to_string();
        if msg.is_empty() { None } else { Some(msg) }
    }

    fn format_copt_api_error(
        lib: &CoptLib,
        env: ffi::CoptEnvPtr,
        context: &str,
        rc: c_int,
    ) -> String {
        if rc == ffi::COPT_RETCODE_LICENSE {
            if let Some(msg) = copt_license_message(lib, env) {
                return format!("{context} failed (rc={rc}): {msg}");
            }
        }
        format!("{context} failed (rc={rc})")
    }

    // ── CoptLpSolver ──────────────────────────────────────────────────────────

    /// COPT LP/QP/MIP solver backend (COPT 8.x, commercial license required).
    ///
    /// Loaded at runtime via libloading — no link-time dependency on libcopt.
    pub struct CoptLpSolver {
        env: ffi::CoptEnvPtr,
        lib: Arc<CoptLib>,
    }

    impl std::fmt::Debug for CoptLpSolver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CoptLpSolver").finish()
        }
    }

    unsafe impl Send for CoptLpSolver {}
    unsafe impl Sync for CoptLpSolver {}

    impl Drop for CoptLpSolver {
        fn drop(&mut self) {
            unsafe {
                (self.lib.COPT_DeleteEnv)(&mut self.env as *mut _);
            }
        }
    }

    impl CoptLpSolver {
        /// Create a new COPT environment.
        ///
        /// Returns `Err` if `libcopt.so` is not found or no valid COPT license is detected.
        pub fn new() -> Result<Self, String> {
            let lib = get_copt()?.clone();
            let mut env: ffi::CoptEnvPtr = ptr::null_mut();
            // Suppress COPT license warnings that go to stdout.
            let _suppress = StdoutSuppressor::new();
            let rc = unsafe { (lib.COPT_CreateEnv)(&mut env) };
            drop(_suppress);
            if rc != 0 || env.is_null() {
                return Err(format!(
                    "COPT_CreateEnv failed (rc={rc}). \
                     Check COPT 8.x installation and license (~/copt/license.key). \
                     Only COPT 8.x (libcopt.so) is supported."
                ));
            }
            Ok(Self { env, lib })
        }
    }

    impl LpSolver for CoptLpSolver {
        fn name(&self) -> &'static str {
            "COPT"
        }

        fn version(&self) -> &'static str {
            "8.0"
        }

        fn solve(&self, prob: &SparseProblem, opts: &LpOptions) -> Result<LpResult, String> {
            unsafe { lp_solve_inner(&self.lib, self.env, prob, opts) }
        }
    }

    unsafe fn lp_solve_inner(
        lib: &CoptLib,
        env: ffi::CoptEnvPtr,
        prob: &SparseProblem,
        opts: &LpOptions,
    ) -> Result<LpResult, String> {
        use ffi::*; // constants only (COPT_MINIMIZE, COPT_LESS_EQUAL, etc.)
        #[allow(non_snake_case)]
        let (
            COPT_CreateProb,
            COPT_DeleteProb,
            COPT_SetLogFile,
            COPT_SetIntParam,
            COPT_SetDblParam,
            COPT_SetObjSense,
            COPT_AddCols,
            COPT_AddRows,
            COPT_SetQuadObj,
            COPT_Solve,
            COPT_SolveLp,
            COPT_GetIntAttr,
            COPT_GetDblAttr,
            COPT_GetLpSolution,
            COPT_GetSolution,
        ) = (
            lib.COPT_CreateProb,
            lib.COPT_DeleteProb,
            lib.COPT_SetLogFile,
            lib.COPT_SetIntParam,
            lib.COPT_SetDblParam,
            lib.COPT_SetObjSense,
            lib.COPT_AddCols,
            lib.COPT_AddRows,
            lib.COPT_SetQuadObj,
            lib.COPT_Solve,
            lib.COPT_SolveLp,
            lib.COPT_GetIntAttr,
            lib.COPT_GetDblAttr,
            lib.COPT_GetLpSolution,
            lib.COPT_GetSolution,
        );

        // ── Create problem ────────────────────────────────────────────────────
        let mut copt_prob: CoptProbPtr = ptr::null_mut();
        let rc = COPT_CreateProb(env, &mut copt_prob);
        if rc != 0 || copt_prob.is_null() {
            return Err(format_copt_api_error(lib, env, "COPT_CreateProb", rc));
        }
        struct ProbGuard(
            ffi::CoptProbPtr,
            unsafe extern "C" fn(*mut ffi::CoptProbPtr) -> c_int,
        );
        impl Drop for ProbGuard {
            fn drop(&mut self) {
                unsafe {
                    (self.1)(&mut self.0 as *mut _);
                }
            }
        }
        let _guard = ProbGuard(copt_prob, COPT_DeleteProb);

        // ── Parameters ───────────────────────────────────────────────────────
        // Explicitly disable file logging — COPT 8.x defaults to writing
        // copt.log to the working directory, which fills the disk on long
        // benchmark runs.  COPT_SetLogFile("") disables file logging.
        let empty = CString::new("").expect("static string contains no null bytes");
        COPT_SetLogFile(copt_prob, empty.as_ptr());

        let log_param = CString::new("LogToConsole").expect("static string contains no null bytes");
        COPT_SetIntParam(
            copt_prob,
            log_param.as_ptr(),
            c_int::from(opts.print_level > 0),
        );

        let tol_dual = CString::new("DualTol").expect("static string contains no null bytes");
        COPT_SetDblParam(copt_prob, tol_dual.as_ptr(), opts.tolerance.max(1e-10));
        let tol_feas = CString::new("FeasTol").expect("static string contains no null bytes");
        COPT_SetDblParam(copt_prob, tol_feas.as_ptr(), opts.tolerance.max(1e-10));

        // Race simplex and barrier in parallel, take whichever finishes first.
        // COPT defaults to dual simplex (LpMethod=1) which is slower than HiGHS
        // on medium-to-large power system LPs.  Concurrent mode (LpMethod=4)
        // lets barrier win on large cases while simplex still wins on small ones.
        let lp_method = CString::new("LpMethod").expect("static string contains no null bytes");
        COPT_SetIntParam(copt_prob, lp_method.as_ptr(), 4); // 4 = concurrent

        // COPT GPU acceleration is permanently disabled.  Setting GPUMode=1 /
        // GPUDevice=0 causes the COPT solver to hang indefinitely during the LP
        // solve phase in the test suite (observed with COPT 7.x on both Linux and
        // macOS).  Root cause is unresolved — likely a COPT GPU driver or license
        // interaction.  Do NOT re-enable without first reproducing a fix for the
        // hang on a GPU-equipped CI runner.

        if let Some(tl) = opts.time_limit_secs {
            let timelimit =
                CString::new("TimeLimit").expect("static string contains no null bytes");
            COPT_SetDblParam(copt_prob, timelimit.as_ptr(), tl);
        }

        // ── Objective sense ───────────────────────────────────────────────────
        let rc = COPT_SetObjSense(copt_prob, COPT_MINIMIZE);
        if rc != 0 {
            return Err(format_copt_api_error(lib, env, "COPT_SetObjSense", rc));
        }

        // ── Problem type ──────────────────────────────────────────────────────
        let is_mip = prob
            .integrality
            .as_ref()
            .is_some_and(|iv| iv.iter().any(|&v| is_integer_domain(v)));

        // ── Column type vector ────────────────────────────────────────────────
        let col_type_vec: Option<Vec<c_char>> = if is_mip {
            Some(
                prob.integrality
                    .as_ref()
                    .expect("integrality Some when is_mip is true")
                    .iter()
                    .map(|&v| copt_col_type(v))
                    .collect(),
            )
        } else {
            None
        };
        let col_type_ptr = col_type_vec.as_ref().map_or(ptr::null(), |v| v.as_ptr());

        // ── Step 1: Add columns WITHOUT constraint matrix ─────────────────────
        // We must add columns first (they are referenced by constraint indices),
        // but the constraint matrix must be loaded via COPT_AddRows (CSR format).
        // Passing NULL for colMatBeg/Cnt/Idx/Elem skips the matrix for this call.
        let rc = COPT_AddCols(
            copt_prob,
            prob.n_col as c_int,
            prob.col_cost.as_ptr(),
            ptr::null(), // no constraint matrix yet
            ptr::null(),
            ptr::null(),
            ptr::null(),
            col_type_ptr,
            prob.col_lower.as_ptr(),
            prob.col_upper.as_ptr(),
            ptr::null(), // column names
        );
        if rc != 0 {
            return Err(format_copt_api_error(lib, env, "COPT_AddCols", rc));
        }

        // ── Step 2: Add rows with CSR constraint matrix ───────────────────────
        // Transpose CSC → CSR for COPT_AddRows.
        let (csr_start, csr_colind, csr_vals) = csc_to_csr(
            prob.n_col,
            prob.n_row,
            &prob.a_start,
            &prob.a_index,
            &prob.a_value,
        );

        // Per-row nonzero counts from CSR start pointers.
        let csr_cnt: Vec<i32> = (0..prob.n_row)
            .map(|i| csr_start[i + 1] - csr_start[i])
            .collect();

        // Convert row bounds.
        let (row_sense, row_bound, row_upper_v) =
            convert_row_bounds(&prob.row_lower, &prob.row_upper);

        let rc = COPT_AddRows(
            copt_prob,
            prob.n_row as c_int,
            csr_start.as_ptr(),
            csr_cnt.as_ptr(),
            csr_colind.as_ptr(),
            csr_vals.as_ptr(),
            row_sense.as_ptr(),
            row_bound.as_ptr(),
            row_upper_v.as_ptr(),
            ptr::null(), // row names
        );
        if rc != 0 {
            return Err(format_copt_api_error(lib, env, "COPT_AddRows", rc));
        }

        // ── Quadratic objective (QP) ──────────────────────────────────────────
        // Convention: SparseProblem stores Q in 0.5*x'Qx form (same as HiGHS).
        // COPT_SetQuadObj uses the Gurobi convention: qElem[i,i]*x_i^2 (no 0.5
        // factor applied internally). Multiply by 0.5 to convert.
        if let (Some(qs), Some(qi), Some(qv)) = (&prob.q_start, &prob.q_index, &prob.q_value) {
            let n_q = qv.len();
            let mut q_row_t = Vec::with_capacity(n_q);
            let mut q_col_t = Vec::with_capacity(n_q);
            let mut q_val_t = Vec::with_capacity(n_q);

            for j in 0..prob.n_col {
                for k in (qs[j] as usize)..(qs[j + 1] as usize) {
                    let row = qi[k] as usize;
                    q_row_t.push(row as c_int);
                    q_col_t.push(j as c_int);
                    q_val_t.push(qv[k] * 0.5);
                }
            }

            let rc = COPT_SetQuadObj(
                copt_prob,
                n_q as c_int,
                q_row_t.as_ptr(),
                q_col_t.as_ptr(),
                q_val_t.as_ptr(),
            );
            if rc != 0 {
                return Err(format_copt_api_error(lib, env, "COPT_SetQuadObj", rc));
            }
        }

        // ── Solve ─────────────────────────────────────────────────────────────
        let solve_rc = if is_mip {
            COPT_Solve(copt_prob)
        } else {
            COPT_SolveLp(copt_prob)
        };
        if solve_rc != 0 {
            return Err(format_copt_api_error(lib, env, "COPT solve", solve_rc));
        }

        // ── Solution status ───────────────────────────────────────────────────
        let stat_attr = if is_mip {
            attr_cstr(ATTR_MIPSTATUS)
        } else {
            attr_cstr(ATTR_LPSTATUS)
        };
        let mut stat: c_int = 0;
        COPT_GetIntAttr(copt_prob, stat_attr.as_ptr(), &mut stat);

        let status = if is_mip {
            match stat {
                COPT_MIPSTATUS_OPTIMAL => LpSolveStatus::Optimal,
                COPT_MIPSTATUS_TIMEOUT => LpSolveStatus::SubOptimal,
                COPT_MIPSTATUS_INFEASIBLE => LpSolveStatus::Infeasible,
                COPT_MIPSTATUS_UNBOUNDED => LpSolveStatus::Unbounded,
                _ => LpSolveStatus::SolverError(format!("COPT MIP status={stat}")),
            }
        } else {
            match stat {
                COPT_LPSTATUS_OPTIMAL => LpSolveStatus::Optimal,
                COPT_LPSTATUS_TIMEOUT => LpSolveStatus::SubOptimal,
                COPT_LPSTATUS_INFEASIBLE => LpSolveStatus::Infeasible,
                COPT_LPSTATUS_UNBOUNDED => LpSolveStatus::Unbounded,
                _ => LpSolveStatus::SolverError(format!("COPT LP status={stat}")),
            }
        };

        if !matches!(status, LpSolveStatus::Optimal | LpSolveStatus::SubOptimal) {
            return Err(format!("COPT: {status:?}"));
        }

        // ── Extract solution ──────────────────────────────────────────────────
        let mut x = vec![0.0f64; prob.n_col];
        let (row_dual, col_dual) = if !is_mip {
            let mut pi = vec![0.0f64; prob.n_row];
            let mut dj = vec![0.0f64; prob.n_col];
            // COPT 8.x: (prob, colVal, rowSlack, rowDual, redCost)
            COPT_GetLpSolution(
                copt_prob,
                x.as_mut_ptr(),
                ptr::null_mut(), // slack not needed
                pi.as_mut_ptr(),
                dj.as_mut_ptr(),
            );
            // Negate COPT Pi to standard Lagrange convention (positive dual =
            // tighter constraint increases objective).
            let row_dual: Vec<f64> = pi.iter().map(|&d| -d).collect();
            (row_dual, dj)
        } else {
            COPT_GetSolution(copt_prob, x.as_mut_ptr());
            (vec![0.0; prob.n_row], vec![0.0; prob.n_col])
        };

        // ── Objective value ───────────────────────────────────────────────────
        let obj_attr = if is_mip {
            attr_cstr(ATTR_BESTOBJ)
        } else {
            attr_cstr(ATTR_LPOBJVAL)
        };
        let mut objval: c_double = 0.0;
        COPT_GetDblAttr(copt_prob, obj_attr.as_ptr(), &mut objval);

        // ── Iteration count ───────────────────────────────────────────────────
        let iter_attr = if is_mip {
            attr_cstr(ATTR_NODECNT)
        } else {
            attr_cstr(ATTR_SIMPLEXITER)
        };
        let mut iters: c_int = 0;
        COPT_GetIntAttr(copt_prob, iter_attr.as_ptr(), &mut iters);

        Ok(LpResult {
            x,
            row_dual,
            col_dual,
            objective: objval,
            status,
            iterations: iters as u32,
            mip_trace: None,
        })
    }

    // ── CoptQcqpSolver ────────────────────────────────────────────────────────

    /// COPT QCQP solver backend (COPT 8.x).
    ///
    /// Solves convex QCQPs of the form:
    /// ```text
    /// min  0.5 * x'Qx + c'x
    /// s.t. Ax {sense} b     (linear constraints)
    ///      Σ Q_k_ij * x_i * x_j + a_k' x {sense_k} rhs_k   (quadratic constraints)
    ///      col_lower ≤ x ≤ col_upper
    /// ```
    ///
    /// Used by the SOCP-OPF QCQP path.  Falls back to Ipopt NLP when unavailable.
    pub struct CoptQcqpSolver {
        env: ffi::CoptEnvPtr,
        lib: Arc<CoptLib>,
    }

    impl std::fmt::Debug for CoptQcqpSolver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CoptQcqpSolver").finish()
        }
    }

    unsafe impl Send for CoptQcqpSolver {}
    unsafe impl Sync for CoptQcqpSolver {}

    impl Drop for CoptQcqpSolver {
        fn drop(&mut self) {
            unsafe {
                (self.lib.COPT_DeleteEnv)(&mut self.env as *mut _);
            }
        }
    }

    impl CoptQcqpSolver {
        /// Create a new COPT QCQP solver, validating the license.
        pub fn new() -> Result<Self, String> {
            let lib = get_copt()?.clone();
            let mut env: ffi::CoptEnvPtr = ptr::null_mut();
            // Suppress COPT license warnings that go to stdout.
            let _suppress = StdoutSuppressor::new();
            let rc = unsafe { (lib.COPT_CreateEnv)(&mut env) };
            drop(_suppress);
            if rc != 0 || env.is_null() {
                return Err(format!(
                    "COPT_CreateEnv failed (rc={rc}). \
                     Check COPT 8.x installation and license. \
                     Only COPT 8.x (libcopt.so) is supported."
                ));
            }
            Ok(Self { env, lib })
        }
    }

    impl crate::backends::QcqpSolver for CoptQcqpSolver {
        fn name(&self) -> &'static str {
            "COPT-QCQP"
        }

        fn solve(
            &self,
            prob: &crate::backends::QcqpProblem,
            opts: &crate::backends::LpOptions,
        ) -> Result<crate::backends::QcqpResult, String> {
            unsafe { qcqp_solve_inner(&self.lib, self.env, prob, opts) }
        }
    }

    unsafe fn qcqp_solve_inner(
        lib: &CoptLib,
        env: ffi::CoptEnvPtr,
        prob: &crate::backends::QcqpProblem,
        opts: &crate::backends::LpOptions,
    ) -> Result<crate::backends::QcqpResult, String> {
        use crate::backends::{LpSolveStatus, QcqpResult};
        use ffi::*; // constants only
        #[allow(non_snake_case)]
        let (
            COPT_CreateProb,
            COPT_DeleteProb,
            COPT_SetLogFile,
            COPT_SetIntParam,
            COPT_SetDblParam,
            COPT_SetObjSense,
            COPT_AddCols,
            COPT_AddRows,
            COPT_SetQuadObj,
            COPT_AddQConstr,
            COPT_Solve,
            COPT_GetIntAttr,
            COPT_GetDblAttr,
            COPT_GetLpSolution,
            COPT_GetSolution,
        ) = (
            lib.COPT_CreateProb,
            lib.COPT_DeleteProb,
            lib.COPT_SetLogFile,
            lib.COPT_SetIntParam,
            lib.COPT_SetDblParam,
            lib.COPT_SetObjSense,
            lib.COPT_AddCols,
            lib.COPT_AddRows,
            lib.COPT_SetQuadObj,
            lib.COPT_AddQConstr,
            lib.COPT_Solve,
            lib.COPT_GetIntAttr,
            lib.COPT_GetDblAttr,
            lib.COPT_GetLpSolution,
            lib.COPT_GetSolution,
        );

        let base = &prob.base;

        // ── Create problem ────────────────────────────────────────────────────
        let mut copt_prob: ffi::CoptProbPtr = ptr::null_mut();
        let rc = COPT_CreateProb(env, &mut copt_prob);
        if rc != 0 || copt_prob.is_null() {
            return Err(format_copt_api_error(lib, env, "COPT_CreateProb", rc));
        }
        struct QcqpProbGuard(
            ffi::CoptProbPtr,
            unsafe extern "C" fn(*mut ffi::CoptProbPtr) -> c_int,
        );
        impl Drop for QcqpProbGuard {
            fn drop(&mut self) {
                unsafe {
                    (self.1)(&mut self.0 as *mut _);
                }
            }
        }
        let _guard = QcqpProbGuard(copt_prob, COPT_DeleteProb);

        // ── Parameters ───────────────────────────────────────────────────────
        let empty = CString::new("").expect("static string contains no null bytes");
        COPT_SetLogFile(copt_prob, empty.as_ptr());

        let log_param = CString::new("LogToConsole").expect("static string contains no null bytes");
        COPT_SetIntParam(
            copt_prob,
            log_param.as_ptr(),
            c_int::from(opts.print_level > 0),
        );

        let tol_dual = CString::new("DualTol").expect("static string contains no null bytes");
        COPT_SetDblParam(copt_prob, tol_dual.as_ptr(), opts.tolerance.max(1e-10));
        let tol_feas = CString::new("FeasTol").expect("static string contains no null bytes");
        COPT_SetDblParam(copt_prob, tol_feas.as_ptr(), opts.tolerance.max(1e-10));

        if let Some(tl) = opts.time_limit_secs {
            let timelimit =
                CString::new("TimeLimit").expect("static string contains no null bytes");
            COPT_SetDblParam(copt_prob, timelimit.as_ptr(), tl);
        }

        // ── Objective sense ───────────────────────────────────────────────────
        let rc = COPT_SetObjSense(copt_prob, COPT_MINIMIZE);
        if rc != 0 {
            return Err(format_copt_api_error(lib, env, "COPT_SetObjSense", rc));
        }

        // ── Step 1: Add columns (no constraint matrix yet) ───────────────────
        let col_types: Option<Vec<c_char>> = base
            .integrality
            .as_ref()
            .map(|integ| integ.iter().map(|&v| copt_col_type(v)).collect());
        let col_types_ptr = col_types
            .as_ref()
            .map(|v| v.as_ptr())
            .unwrap_or(ptr::null());
        let rc = COPT_AddCols(
            copt_prob,
            base.n_col as c_int,
            base.col_cost.as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            col_types_ptr,
            base.col_lower.as_ptr(),
            base.col_upper.as_ptr(),
            ptr::null(),
        );
        if rc != 0 {
            return Err(format_copt_api_error(lib, env, "COPT_AddCols", rc));
        }

        // ── Step 2: Add linear rows (CSR format) ─────────────────────────────
        if base.n_row > 0 {
            let (csr_start, csr_colind, csr_vals) = csc_to_csr(
                base.n_col,
                base.n_row,
                &base.a_start,
                &base.a_index,
                &base.a_value,
            );
            let csr_cnt: Vec<i32> = (0..base.n_row)
                .map(|i| csr_start[i + 1] - csr_start[i])
                .collect();
            let (row_sense, row_bound, row_upper_v) =
                convert_row_bounds(&base.row_lower, &base.row_upper);

            let rc = COPT_AddRows(
                copt_prob,
                base.n_row as c_int,
                csr_start.as_ptr(),
                csr_cnt.as_ptr(),
                csr_colind.as_ptr(),
                csr_vals.as_ptr(),
                row_sense.as_ptr(),
                row_bound.as_ptr(),
                row_upper_v.as_ptr(),
                ptr::null(),
            );
            if rc != 0 {
                return Err(format_copt_api_error(lib, env, "COPT_AddRows", rc));
            }
        }

        // ── Step 3: Quadratic objective (optional) ───────────────────────────
        // Same 0.5 factor correction as lp_solve_inner (COPT_SetQuadObj convention).
        if let (Some(qs), Some(qi), Some(qv)) = (&base.q_start, &base.q_index, &base.q_value) {
            let n_q = qv.len();
            let mut q_row_t = Vec::with_capacity(n_q);
            let mut q_col_t = Vec::with_capacity(n_q);
            let mut q_val_t = Vec::with_capacity(n_q);
            for j in 0..base.n_col {
                for k in (qs[j] as usize)..(qs[j + 1] as usize) {
                    q_row_t.push(qi[k]);
                    q_col_t.push(j as i32);
                    q_val_t.push(qv[k] * 0.5);
                }
            }
            let rc = COPT_SetQuadObj(
                copt_prob,
                n_q as c_int,
                q_row_t.as_ptr(),
                q_col_t.as_ptr(),
                q_val_t.as_ptr(),
            );
            if rc != 0 {
                return Err(format!("COPT_SetQuadObj failed (rc={rc})"));
            }
        }

        // ── Step 4: Quadratic constraints ────────────────────────────────────
        for qc in &prob.quad_constraints {
            let lin_cnt = qc.lin_idx.len() as c_int;
            let q_cnt = qc.q_val.len() as c_int;
            let lin_idx_ptr = if qc.lin_idx.is_empty() {
                ptr::null()
            } else {
                qc.lin_idx.as_ptr()
            };
            let lin_val_ptr = if qc.lin_val.is_empty() {
                ptr::null()
            } else {
                qc.lin_val.as_ptr()
            };
            let q_row_ptr = if qc.q_row.is_empty() {
                ptr::null()
            } else {
                qc.q_row.as_ptr()
            };
            let q_col_ptr = if qc.q_col.is_empty() {
                ptr::null()
            } else {
                qc.q_col.as_ptr()
            };
            let q_val_ptr = if qc.q_val.is_empty() {
                ptr::null()
            } else {
                qc.q_val.as_ptr()
            };

            let rc = COPT_AddQConstr(
                copt_prob,
                lin_cnt,
                lin_idx_ptr,
                lin_val_ptr,
                q_cnt,
                q_row_ptr,
                q_col_ptr,
                q_val_ptr,
                qc.sense as c_char,
                qc.rhs,
                ptr::null(),
            );
            if rc != 0 {
                return Err(format!(
                    "COPT_AddQConstr failed (rc={rc}) for one of the quadratic constraints"
                ));
            }
        }

        // ── Step 5: Solve (COPT_Solve handles QCQP and MIQCQP automatically) ─
        let is_mip = base
            .integrality
            .as_ref()
            .is_some_and(|iv| iv.iter().any(|&v| is_integer_domain(v)));

        let solve_rc = COPT_Solve(copt_prob);
        if solve_rc != 0 {
            return Err(format!("COPT_Solve (QCQP) failed (rc={solve_rc})"));
        }

        // ── Check status ─────────────────────────────────────────────────────
        let stat_attr = if is_mip {
            attr_cstr(ATTR_MIPSTATUS)
        } else {
            attr_cstr(ATTR_LPSTATUS)
        };
        let mut stat: c_int = 0;
        COPT_GetIntAttr(copt_prob, stat_attr.as_ptr(), &mut stat);

        let status = if is_mip {
            match stat {
                COPT_MIPSTATUS_OPTIMAL => LpSolveStatus::Optimal,
                COPT_MIPSTATUS_TIMEOUT => LpSolveStatus::SubOptimal,
                COPT_MIPSTATUS_INFEASIBLE => LpSolveStatus::Infeasible,
                COPT_MIPSTATUS_UNBOUNDED => LpSolveStatus::Unbounded,
                _ => LpSolveStatus::SolverError(format!("COPT QCQP MIP status={stat}")),
            }
        } else {
            match stat {
                COPT_LPSTATUS_OPTIMAL => LpSolveStatus::Optimal,
                COPT_LPSTATUS_TIMEOUT => LpSolveStatus::SubOptimal,
                COPT_LPSTATUS_INFEASIBLE => LpSolveStatus::Infeasible,
                COPT_LPSTATUS_UNBOUNDED => LpSolveStatus::Unbounded,
                _ => LpSolveStatus::SolverError(format!("COPT QCQP LP status={stat}")),
            }
        };

        if !matches!(status, LpSolveStatus::Optimal | LpSolveStatus::SubOptimal) {
            return Err(format!("COPT QCQP: {status:?}"));
        }

        // ── Extract solution ──────────────────────────────────────────────────
        let mut x = vec![0.0f64; base.n_col];
        if is_mip {
            COPT_GetSolution(copt_prob, x.as_mut_ptr());
        } else {
            COPT_GetLpSolution(
                copt_prob,
                x.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
        }

        // ── Objective value ───────────────────────────────────────────────────
        let obj_attr = if is_mip {
            attr_cstr(ATTR_BESTOBJ)
        } else {
            attr_cstr(ATTR_LPOBJVAL)
        };
        let mut objval: c_double = 0.0;
        COPT_GetDblAttr(copt_prob, obj_attr.as_ptr(), &mut objval);

        Ok(QcqpResult {
            x,
            objective: objval,
            status,
        })
    }

    // ── COPT NLP ──────────────────────────────────────────────────────────────
    //
    // The NLP solver uses a standalone shared library (libsurge_copt_nlp.so)
    // that bridges Rust callbacks to COPT's C++ INlpCallback vtable via
    // copt_nlp_shim.cpp.  The shim is loaded at runtime via libloading.

    mod nlp_ffi {
        use std::ffi::{c_double, c_int, c_void};

        /// C-compatible struct mirroring `CoptNlpFns` in copt_nlp_shim.cpp.
        /// Must be `repr(C)` with identical field order.
        #[repr(C)]
        pub struct CoptNlpFns {
            pub userdata: *mut c_void,
            pub eval_obj:
                unsafe extern "C" fn(c_int, *const c_double, *mut c_double, *mut c_void) -> c_int,
            pub eval_grad:
                unsafe extern "C" fn(c_int, *const c_double, *mut c_double, *mut c_void) -> c_int,
            pub eval_con: unsafe extern "C" fn(
                c_int,
                c_int,
                *const c_double,
                *mut c_double,
                *mut c_void,
            ) -> c_int,
            pub eval_jac: unsafe extern "C" fn(
                c_int,
                c_int,
                *const c_double,
                *mut c_double,
                *mut c_void,
            ) -> c_int,
            pub eval_hess: unsafe extern "C" fn(
                c_int,
                c_int,
                c_int,
                *const c_double,
                c_double,
                *const c_double,
                *mut c_double,
                *mut c_void,
            ) -> c_int,
            // Dimensions written by copt_nlp_solve before RustNlpCb uses them.
            pub n: c_int,
            pub m: c_int,
            pub nnz_jac: c_int,
            pub nnz_hess: c_int,
        }

        /// Function pointer type for the shim's `copt_nlp_solve` symbol.
        pub type CoptNlpSolveFn = unsafe extern "C" fn(
            n_col: c_int,
            n_row: c_int,
            n_obj_grad: c_int,
            obj_grad_idx: *const c_int,
            nnz_jac: c_int,
            jac_row: *const c_int,
            jac_col: *const c_int,
            nnz_hess: c_int,
            hess_row: *const c_int,
            hess_col: *const c_int,
            col_lo: *const c_double,
            col_hi: *const c_double,
            row_lo: *const c_double,
            row_hi: *const c_double,
            init_x: *const c_double,
            print_level: c_int,
            time_limit: c_double,
            tol: c_double,
            max_iter: c_int,
            fns: *const CoptNlpFns,
            objval_out: *mut c_double,
            x_out: *mut c_double,
            lambda_out: *mut c_double,
            status_out: *mut c_int,
        ) -> c_int;
    }

    // ── COPT NLP shim runtime loader ────────────────────────────────────────

    #[allow(non_snake_case)]
    struct CoptNlpShimLib {
        _copt_cpp: Option<Library>,
        _lib: Library,
        copt_nlp_solve: nlp_ffi::CoptNlpSolveFn,
    }

    static COPT_NLP_SHIM: OnceLock<Result<Arc<CoptNlpShimLib>, String>> = OnceLock::new();

    fn copt_cpp_lib_paths() -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();

        if let Ok(home) = std::env::var("COPT_HOME") {
            let lib_dir = format!("{home}/lib");
            paths.push(std::path::PathBuf::from(format!(
                "{lib_dir}/libcopt_cpp.so"
            )));
            paths.push(std::path::PathBuf::from(format!(
                "{lib_dir}/libcopt_cpp.dylib"
            )));
            paths.push(std::path::PathBuf::from(format!("{lib_dir}/copt_cpp.dll")));
        }

        #[cfg(not(target_os = "windows"))]
        for prefix in &["/opt/copt80", "/opt/copt70", "/opt/copt"] {
            paths.push(std::path::PathBuf::from(format!(
                "{prefix}/lib/libcopt_cpp.so"
            )));
            paths.push(std::path::PathBuf::from(format!(
                "{prefix}/lib/libcopt_cpp.dylib"
            )));
        }

        #[cfg(target_os = "windows")]
        for prefix in &["C:\\copt80", "C:\\copt70", "C:\\copt"] {
            paths.push(std::path::PathBuf::from(format!(
                "{prefix}\\lib\\copt_cpp.dll"
            )));
        }

        paths.push(std::path::PathBuf::from("libcopt_cpp.so"));
        paths.push(std::path::PathBuf::from("libcopt_cpp.dylib"));
        paths.push(std::path::PathBuf::from("copt_cpp.dll"));

        paths
    }

    fn preload_copt_cpp() -> Option<Library> {
        for path in copt_cpp_lib_paths() {
            if let Ok(lib) = unsafe { Library::new(&path) } {
                tracing::debug!("Loaded COPT C++ runtime from {}", path.display());
                return Some(lib);
            }
        }
        None
    }

    fn copt_nlp_shim_paths() -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();

        // 1. Explicit override.
        if let Ok(p) = std::env::var("SURGE_COPT_NLP_SHIM_PATH") {
            paths.push(std::path::PathBuf::from(p));
        }

        // 2. Next to libcopt in COPT_HOME/lib/ (or COPT_HOME\lib\ on Windows).
        if let Ok(home) = std::env::var("COPT_HOME") {
            let lib_dir = format!("{home}/lib");
            paths.push(std::path::PathBuf::from(format!(
                "{lib_dir}/libsurge_copt_nlp.so"
            )));
            paths.push(std::path::PathBuf::from(format!(
                "{lib_dir}/libsurge_copt_nlp.dylib"
            )));
            paths.push(std::path::PathBuf::from(format!(
                "{lib_dir}/surge_copt_nlp.dll"
            )));
        }

        // 3. Common COPT install prefixes (Unix).
        #[cfg(not(target_os = "windows"))]
        for prefix in &["/opt/copt80", "/opt/copt70", "/opt/copt"] {
            paths.push(std::path::PathBuf::from(format!(
                "{prefix}/lib/libsurge_copt_nlp.so"
            )));
            paths.push(std::path::PathBuf::from(format!(
                "{prefix}/lib/libsurge_copt_nlp.dylib"
            )));
        }

        // 3. Common COPT install prefixes (Windows).
        #[cfg(target_os = "windows")]
        for prefix in &["C:\\copt80", "C:\\copt70", "C:\\copt"] {
            paths.push(std::path::PathBuf::from(format!(
                "{prefix}\\lib\\surge_copt_nlp.dll"
            )));
        }

        // 4. OS linker search (LD_LIBRARY_PATH / DYLD_LIBRARY_PATH / PATH).
        paths.push(std::path::PathBuf::from("libsurge_copt_nlp.so"));
        paths.push(std::path::PathBuf::from("libsurge_copt_nlp.dylib"));
        paths.push(std::path::PathBuf::from("surge_copt_nlp.dll"));

        paths
    }

    fn get_copt_nlp_shim() -> Result<&'static Arc<CoptNlpShimLib>, String> {
        COPT_NLP_SHIM
            .get_or_init(|| {
                for path in copt_nlp_shim_paths() {
                    let copt_cpp = preload_copt_cpp();
                    if let Ok(lib) = unsafe { Library::new(&path) } {
                        match unsafe { load_copt_nlp_shim_symbols(lib, copt_cpp) } {
                            Ok(slib) => {
                                tracing::debug!("Loaded COPT NLP shim from {}", path.display());
                                return Ok(Arc::new(slib));
                            }
                            Err(e) => {
                                tracing::debug!("COPT NLP shim at {} failed: {e}", path.display());
                                return Err(e);
                            }
                        }
                    }
                }
                Err(
                    "COPT NLP shim not found (libsurge_copt_nlp.so / .dylib / .dll). \
                     COPT LP/QP/MIP works without it — only the NLP interface \
                     (AC-OPF, SCOPF) requires the shim. \
                     Build it with: scripts/build-copt-nlp-shim.sh (Linux/macOS) \
                     or scripts/build-copt-nlp-shim.ps1 (Windows). \
                     Requires COPT 8.x C++ headers at COPT_HOME. \
                     Install the resulting library into $COPT_HOME/lib/ or \
                     set SURGE_COPT_NLP_SHIM_PATH to its full path."
                        .to_string(),
                )
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    unsafe fn load_copt_nlp_shim_symbols(
        lib: Library,
        copt_cpp: Option<Library>,
    ) -> Result<CoptNlpShimLib, String> {
        let sym =
            *unsafe { lib.get::<nlp_ffi::CoptNlpSolveFn>(b"copt_nlp_solve\0") }.map_err(|e| {
                format!(
                    "COPT NLP shim: symbol 'copt_nlp_solve' not found: {e}. \
                 Rebuild with: scripts/build-copt-nlp-shim.sh"
                )
            })?;
        Ok(CoptNlpShimLib {
            _copt_cpp: copt_cpp,
            copt_nlp_solve: sym,
            _lib: lib,
        })
    }

    // ── NLP userdata context ──────────────────────────────────────────────────
    //
    // Passes `&dyn NlpProblem` (16-byte fat pointer) through C's `void*` (8-byte).
    // Pattern mirrors ipopt_solver.rs: store the fat pointer in a struct and pass
    // a thin pointer to that struct as userdata.

    struct NlpCtx {
        /// Fat pointer held with `'static` lifetime (soundness: the trampoline
        /// is only called synchronously from `copt_nlp_solve`, which returns
        /// before `solve()` returns and before the original borrow ends).
        problem: &'static dyn NlpProblem,
        error: RefCell<Option<String>>,
    }

    #[inline]
    unsafe fn ctx<'a>(ud: *mut c_void) -> &'a NlpCtx {
        unsafe { &*(ud as *const NlpCtx) }
    }

    fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
        if let Some(msg) = payload.downcast_ref::<&str>() {
            (*msg).to_string()
        } else if let Some(msg) = payload.downcast_ref::<String>() {
            msg.clone()
        } else {
            "panic payload is not a string".to_string()
        }
    }

    fn record_callback_error(ctx: &NlpCtx, callback: &str, payload: Box<dyn Any + Send>) {
        let mut slot = ctx.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(format!(
                "{callback} panicked: {}",
                panic_payload_to_string(payload)
            ));
        }
    }

    // ── Trampolines (only when NLP shim is compiled) ──────────────────────────

    unsafe extern "C" fn trampoline_obj(
        n: c_int,
        x: *const c_double,
        f_out: *mut c_double,
        ud: *mut c_void,
    ) -> c_int {
        let ctx = unsafe { ctx(ud) };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let problem = ctx.problem;
            let x = unsafe { std::slice::from_raw_parts(x, n as usize) };
            unsafe { *f_out = problem.eval_objective(x) };
        }));
        match result {
            Ok(()) => 0,
            Err(panic) => {
                record_callback_error(ctx, "COPT eval_obj", panic);
                1
            }
        }
    }

    unsafe extern "C" fn trampoline_grad(
        n: c_int,
        x: *const c_double,
        grad: *mut c_double,
        ud: *mut c_void,
    ) -> c_int {
        let ctx = unsafe { ctx(ud) };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let problem = ctx.problem;
            let x = unsafe { std::slice::from_raw_parts(x, n as usize) };
            let g = unsafe { std::slice::from_raw_parts_mut(grad, n as usize) };
            problem.eval_gradient(x, g);
        }));
        match result {
            Ok(()) => 0,
            Err(panic) => {
                record_callback_error(ctx, "COPT eval_grad", panic);
                1
            }
        }
    }

    unsafe extern "C" fn trampoline_con(
        n: c_int,
        m: c_int,
        x: *const c_double,
        g_out: *mut c_double,
        ud: *mut c_void,
    ) -> c_int {
        let ctx = unsafe { ctx(ud) };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let problem = ctx.problem;
            let x = unsafe { std::slice::from_raw_parts(x, n as usize) };
            let g = unsafe { std::slice::from_raw_parts_mut(g_out, m as usize) };
            problem.eval_constraints(x, g);
        }));
        match result {
            Ok(()) => 0,
            Err(panic) => {
                record_callback_error(ctx, "COPT eval_con", panic);
                1
            }
        }
    }

    unsafe extern "C" fn trampoline_jac(
        n: c_int,
        nnz: c_int,
        x: *const c_double,
        vals: *mut c_double,
        ud: *mut c_void,
    ) -> c_int {
        let ctx = unsafe { ctx(ud) };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let problem = ctx.problem;
            let x = unsafe { std::slice::from_raw_parts(x, n as usize) };
            let v = unsafe { std::slice::from_raw_parts_mut(vals, nnz as usize) };
            problem.eval_jacobian(x, v);
        }));
        match result {
            Ok(()) => 0,
            Err(panic) => {
                record_callback_error(ctx, "COPT eval_jac", panic);
                1
            }
        }
    }

    unsafe extern "C" fn trampoline_hess(
        n: c_int,
        m: c_int,
        nnz: c_int,
        x: *const c_double,
        sigma: c_double,
        lambda: *const c_double,
        vals: *mut c_double,
        ud: *mut c_void,
    ) -> c_int {
        let ctx = unsafe { ctx(ud) };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let problem = ctx.problem;
            let x = unsafe { std::slice::from_raw_parts(x, n as usize) };
            let lam = unsafe { std::slice::from_raw_parts(lambda, m as usize) };
            let v = unsafe { std::slice::from_raw_parts_mut(vals, nnz as usize) };
            problem.eval_hessian(x, sigma, lam, v);
        }));
        match result {
            Ok(()) => 0,
            Err(panic) => {
                record_callback_error(ctx, "COPT eval_hess", panic);
                1
            }
        }
    }

    // ── CoptNlpSolver ─────────────────────────────────────────────────────────

    /// COPT NLP solver using the C++ `LoadNlData` callback interface.
    ///
    /// The solver wraps Rust [`NlpProblem`] callbacks in a C++ vtable object
    /// via `copt_nlp_shim.cpp`, then calls COPT's interior-point NLP solver.
    ///
    /// Requires COPT 8.x with a valid license, `libcopt_cpp.so`, and the
    /// standalone NLP shim (`libsurge_copt_nlp.so`) at runtime.
    /// Build the shim with `scripts/build-copt-nlp-shim.sh`.
    pub struct CoptNlpSolver {
        shim: Arc<CoptNlpShimLib>,
    }

    impl std::fmt::Debug for CoptNlpSolver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CoptNlpSolver").finish()
        }
    }

    impl CoptNlpSolver {
        /// Create a new COPT NLP solver after validating the COPT license
        /// and loading the NLP shim shared library.
        pub fn new() -> Result<Self, String> {
            // Validate COPT is installed and licensed by loading libcopt.so.
            let lib = get_copt()?;
            let mut env: ffi::CoptEnvPtr = ptr::null_mut();
            // Suppress COPT license warnings that go to stdout.
            let _suppress = StdoutSuppressor::new();
            let rc = unsafe { (lib.COPT_CreateEnv)(&mut env) };
            drop(_suppress);
            if rc != 0 || env.is_null() {
                return Err(format!(
                    "COPT_CreateEnv failed (rc={rc}). \
                     Check COPT 8.x installation and license (~/copt/license.key). \
                     Only COPT 8.x (libcopt.so) is supported."
                ));
            }
            unsafe { (lib.COPT_DeleteEnv)(&mut env as *mut _) };

            // Load the NLP shim shared library.
            let shim = get_copt_nlp_shim()?.clone();

            Ok(Self { shim })
        }
    }

    impl NlpSolver for CoptNlpSolver {
        fn name(&self) -> &'static str {
            "COPT-NLP"
        }

        fn version(&self) -> &'static str {
            "8.0"
        }

        fn solve(
            &self,
            problem: &dyn NlpProblem,
            opts: &NlpOptions,
        ) -> Result<NlpSolution, String> {
            use crate::nlp::HessianMode;
            use nlp_ffi::CoptNlpFns;

            let n = problem.n_vars();
            let m = problem.n_constraints();
            let (col_lo, col_hi) = problem.var_bounds();
            let (row_lo, row_hi) = problem.constraint_bounds();
            let init_x = problem.initial_point();

            // Jacobian sparsity (always sparse; COPT requires structure up front).
            let (jac_row, jac_col) = problem.jacobian_structure();
            let nnz_jac = jac_row.len() as c_int;

            // Hessian sparsity (only if exact Hessian requested AND available).
            let use_hessian = opts.hessian_mode == HessianMode::Exact && problem.has_hessian();
            let (hess_row, hess_col) = if use_hessian {
                problem.hessian_structure()
            } else {
                (vec![], vec![])
            };
            let nnz_hess = hess_row.len() as c_int;

            // Userdata context (stack-pinned for the duration of copt_nlp_solve).
            // SAFETY: We transmute the lifetime to 'static because copt_nlp_solve is
            // synchronous — all callbacks fire before it returns, so the reference
            // outlives every callback invocation even though the type says 'static.
            let problem_static: &'static dyn NlpProblem =
                unsafe { std::mem::transmute::<&dyn NlpProblem, &'static dyn NlpProblem>(problem) };
            let ctx = NlpCtx {
                problem: problem_static,
                error: RefCell::new(None),
            };
            let userdata = &ctx as *const NlpCtx as *mut c_void;

            // Build the function-pointer struct.
            // The `n`, `m`, `nnz_jac`, `nnz_hess` fields are filled by copt_nlp_solve.
            let fns = CoptNlpFns {
                userdata,
                eval_obj: trampoline_obj,
                eval_grad: trampoline_grad,
                eval_con: trampoline_con,
                eval_jac: trampoline_jac,
                eval_hess: trampoline_hess,
                n: 0,
                m: 0,
                nnz_jac: 0,
                nnz_hess: 0,
            };

            let mut x_out = vec![0.0f64; n];
            let mut lambda_out = vec![0.0f64; m];
            let mut objval: f64 = 0.0;
            let mut status: c_int = -1;

            // COPT_DENSETYPE_ROWMAJOR = -1: dense gradient (NULL objGradIdx).
            // All callers fill the full gradient vector, so we always use dense mode.
            let rc = unsafe {
                (self.shim.copt_nlp_solve)(
                    n as c_int,
                    m as c_int,
                    -1_i32,      // n_obj_grad: COPT_DENSETYPE_ROWMAJOR → dense
                    ptr::null(), // obj_grad_idx: NULL for dense gradient
                    nnz_jac,
                    if jac_row.is_empty() {
                        ptr::null()
                    } else {
                        jac_row.as_ptr()
                    },
                    if jac_col.is_empty() {
                        ptr::null()
                    } else {
                        jac_col.as_ptr()
                    },
                    nnz_hess,
                    if hess_row.is_empty() {
                        ptr::null()
                    } else {
                        hess_row.as_ptr()
                    },
                    if hess_col.is_empty() {
                        ptr::null()
                    } else {
                        hess_col.as_ptr()
                    },
                    col_lo.as_ptr(),
                    col_hi.as_ptr(),
                    row_lo.as_ptr(),
                    row_hi.as_ptr(),
                    init_x.as_ptr(),
                    opts.print_level,
                    0.0_f64, // no time limit (NlpOptions has no time_limit field)
                    opts.tolerance,
                    opts.max_iterations as c_int,
                    &fns,
                    &mut objval,
                    x_out.as_mut_ptr(),
                    lambda_out.as_mut_ptr(),
                    &mut status,
                )
            };

            if rc != 0 {
                return Err(format!("COPT NLP shim returned error (rc={rc})"));
            }

            if let Some(err) = ctx.error.borrow().clone() {
                return Err(err);
            }

            // COPT_LPSTATUS_OPTIMAL = 1
            let converged = status == 1;
            if !converged && status != 7 {
                // status 7 = IMPRECISE (acceptable for L-BFGS warm-up)
                return Err(format!("COPT NLP did not converge (LpStatus={status})"));
            }

            Ok(NlpSolution {
                x: x_out,
                lambda: lambda_out,
                z_lower: vec![0.0; n],
                z_upper: vec![0.0; n],
                objective: objval,
                iterations: None, // COPT NLP does not expose iteration count via C API
                converged,
            })
        }
    }

    // Keep c_void in scope (used in NlpProblem trait via NlpSolver bound).
    const _: usize = std::mem::size_of::<c_void>();

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::nlp::NlpProblem;

        struct PanicProblem;

        impl NlpProblem for PanicProblem {
            fn n_vars(&self) -> usize {
                1
            }

            fn n_constraints(&self) -> usize {
                0
            }

            fn var_bounds(&self) -> (Vec<f64>, Vec<f64>) {
                (vec![-1.0], vec![1.0])
            }

            fn constraint_bounds(&self) -> (Vec<f64>, Vec<f64>) {
                (vec![], vec![])
            }

            fn initial_point(&self) -> Vec<f64> {
                vec![0.0]
            }

            fn eval_objective(&self, _x: &[f64]) -> f64 {
                panic!("boom")
            }

            fn eval_gradient(&self, _x: &[f64], _grad: &mut [f64]) {}

            fn eval_constraints(&self, _x: &[f64], _g: &mut [f64]) {}

            fn jacobian_structure(&self) -> (Vec<i32>, Vec<i32>) {
                (vec![], vec![])
            }

            fn eval_jacobian(&self, _x: &[f64], _values: &mut [f64]) {}
        }

        #[test]
        fn trampoline_obj_captures_panics() {
            let problem = PanicProblem;
            // SAFETY: The trampoline is invoked synchronously below, so the
            // widened lifetime cannot outlive `problem`.
            let problem_static: &'static dyn NlpProblem = unsafe {
                std::mem::transmute::<&dyn NlpProblem, &'static dyn NlpProblem>(&problem)
            };
            let ctx = NlpCtx {
                problem: problem_static,
                error: RefCell::new(None),
            };
            let mut f_out = 42.0;
            let rc = unsafe {
                trampoline_obj(
                    1,
                    [0.0_f64].as_ptr(),
                    &mut f_out,
                    &ctx as *const NlpCtx as *mut c_void,
                )
            };

            assert_eq!(rc, 1);
            assert_eq!(f_out, 42.0);
            let err = ctx.error.borrow().clone().expect("callback error recorded");
            assert!(err.contains("COPT eval_obj"));
            assert!(err.contains("boom"));
        }
    }
}
