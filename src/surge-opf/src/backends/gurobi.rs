#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Gurobi 13 LP/QP/MIP backend implementing [`LpSolver`], plus a native
//! AC-OPF NLP solver using Gurobi 13's expression-tree general nonlinear
//! constraints (`GRBaddgenconstrNL`).
//!
//! Uses the Gurobi 13 C API directly (no `grb` crate) so it works with
//! Gurobi 13.x and its new interior-point NLP solver and GPU-accelerated
//! PDHG barrier method.
//!
//! Runtime discovery:
//! ```sh
//! export GUROBI_HOME=/opt/gurobi1301/linux64
//! ```
//!
//! Runtime: place `gurobi.lic` at `~/gurobi.lic` or set `GRB_LICENSE_FILE`.
//!
//! GPU (Gurobi 13 PDHG): set `GRB_USE_GPU=1` in the environment to enable
//! GPU-accelerated LP via `Method=6` (PDHG) + `PDHGGPU=1`.  Requires a
//! CUDA-capable GPU and the GPU-enabled Gurobi 13 build.
//!
//! Quiet startup logging: Gurobi's environment startup banner is suppressed
//! by default. Set `SURGE_GUROBI_QUIET=0` (or `false`/`no`/`off`) to restore
//! the banner. Per-solve console logging is independent and still follows
//! `LpOptions::print_level`.

pub use self::impl_::GurobiLpSolver;
pub use self::impl_::GurobiNlpSolver;
pub use self::impl_::GurobiQcqpSolver;

#[allow(unsafe_op_in_unsafe_fn)]
mod impl_ {
    use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
    use std::fs::{self, File};
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::ptr;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::ac::types::{BranchAdmittance, build_branch_admittances, compute_branch_admittance};
    use crate::backends::{
        LpOptions, LpResult, LpSolveStatus, LpSolver, SparseProblem, VariableDomain,
    };
    use crate::common::context::OpfNetworkContext;

    fn is_integer_domain(domain: VariableDomain) -> bool {
        !matches!(domain, VariableDomain::Continuous)
    }

    fn gurobi_vtype(domain: VariableDomain) -> c_char {
        match domain {
            VariableDomain::Continuous => ffi::GRB_CONTINUOUS,
            VariableDomain::Binary => ffi::GRB_BINARY,
            VariableDomain::Integer => ffi::GRB_INTEGER,
        }
    }

    // ── Gurobi 13 C API FFI ────────────────────────────────────────────────────
    //
    // Loaded from libgurobi130.so at runtime. Set GUROBI_HOME to the install
    // prefix (e.g. /opt/gurobi1301/linux64) so the runtime loader can find it.
    //
    // NOTE: On Linux the __stdcall calling convention is ignored; all Gurobi
    // functions use the standard System V AMD64 ABI.

    #[allow(non_camel_case_types, clippy::upper_case_acronyms, dead_code)]
    mod ffi {
        use std::ffi::{c_char, c_int};

        // Opaque types for Gurobi environment and model.
        pub enum GRBenv_s {}
        pub enum GRBmodel_s {}
        pub type GRBenv = GRBenv_s;
        pub type GRBmodel = GRBmodel_s;

        // ── Optimization status codes (gurobi_c.h) ────────────────────────────
        pub const GRB_OPTIMAL: c_int = 2;
        pub const GRB_INFEASIBLE: c_int = 3;
        pub const GRB_INF_OR_UNBD: c_int = 4;
        pub const GRB_UNBOUNDED: c_int = 5;
        pub const GRB_TIME_LIMIT: c_int = 9;
        pub const GRB_SUBOPTIMAL: c_int = 13;
        pub const GRB_LOCALLY_OPTIMAL: c_int = 18;

        // ── Variable type byte values ─────────────────────────────────────────
        pub const GRB_CONTINUOUS: c_char = b'C' as c_char;
        pub const GRB_BINARY: c_char = b'B' as c_char;
        pub const GRB_INTEGER: c_char = b'I' as c_char;

        // ── Constraint sense byte values ──────────────────────────────────────
        pub const GRB_LESS_EQUAL: c_char = b'<' as c_char;
        pub const GRB_GREATER_EQUAL: c_char = b'>' as c_char;
        pub const GRB_EQUAL: c_char = b'=' as c_char;

        // ── Objective / model sense ───────────────────────────────────────────
        pub const GRB_MINIMIZE: c_int = 1;

        // ── Method parameter values ───────────────────────────────────────────
        pub const GRB_METHOD_PDHG: c_int = 6; // GPU-accelerated LP (Gurobi 13)

        // ── Attribute name strings (null-terminated byte literals) ────────────
        pub const ATTR_STATUS: &[u8] = b"Status\0";
        pub const ATTR_OBJVAL: &[u8] = b"ObjVal\0";
        pub const ATTR_X: &[u8] = b"X\0";
        pub const ATTR_SOLCOUNT: &[u8] = b"SolCount\0";
        pub const ATTR_PI: &[u8] = b"Pi\0";
        pub const ATTR_RC: &[u8] = b"RC\0";
        pub const ATTR_ITERCOUNT: &[u8] = b"IterCount\0";
        pub const ATTR_BARITERCOUNT: &[u8] = b"BarIterCount\0";
        pub const ATTR_VTYPE: &[u8] = b"VType\0";
        pub const ATTR_MODELSENSE: &[u8] = b"ModelSense\0";
        pub const ATTR_IIS_MINIMAL: &[u8] = b"IISMinimal\0";
        pub const ATTR_IIS_LB: &[u8] = b"IISLB\0";
        pub const ATTR_IIS_UB: &[u8] = b"IISUB\0";
        pub const ATTR_IIS_CONSTR: &[u8] = b"IISConstr\0";
        /// Wall-clock runtime of the most recent optimize call (seconds).
        pub const ATTR_RUNTIME: &[u8] = b"Runtime\0";
        /// Best dual bound from the most recent MIP optimize call.
        pub const ATTR_OBJBOUND: &[u8] = b"ObjBound\0";
        /// Model variable count (read on the original, pre-presolve model).
        pub const ATTR_NUMVARS: &[u8] = b"NumVars\0";
        /// Model binary variable count.
        pub const ATTR_NUMBINVARS: &[u8] = b"NumBinVars\0";
        /// Model integer (incl. binary) variable count.
        pub const ATTR_NUMINTVARS: &[u8] = b"NumIntVars\0";
        /// Model linear constraint count.
        pub const ATTR_NUMCONSTRS: &[u8] = b"NumConstrs\0";
        /// Model A-matrix nonzero count.
        pub const ATTR_NUMNZS: &[u8] = b"NumNZs\0";
        /// Number of branch-and-bound nodes explored in the most recent MIP solve.
        pub const ATTR_NODECOUNT: &[u8] = b"NodeCount\0";

        // ── Parameter name strings (null-terminated byte literals) ────────────
        pub const PAR_OUTPUTFLAG: &[u8] = b"OutputFlag\0";
        pub const PAR_LOGTOCONSOLE: &[u8] = b"LogToConsole\0";
        pub const PAR_METHOD: &[u8] = b"Method\0";
        pub const PAR_FEASIBILITYTOL: &[u8] = b"FeasibilityTol\0";
        pub const PAR_OPTIMALITYTOL: &[u8] = b"OptimalityTol\0";
        pub const PAR_MIPGAP: &[u8] = b"MIPGap\0";
        pub const PAR_TIMELIMIT: &[u8] = b"TimeLimit\0";
        pub const PAR_PDHGGPU: &[u8] = b"PDHGGPU\0";
        pub const PAR_NONCONVEX: &[u8] = b"NonConvex\0";
        pub const PAR_DUALREDUCTIONS: &[u8] = b"DualReductions\0";
        pub const PAR_INFUNBDINFO: &[u8] = b"InfUnbdInfo\0";
        /// Thread count for solver work. 0 = Gurobi auto.
        pub const PAR_THREADS: &[u8] = b"Threads\0";
        /// Random seed for tie-breaking / pseudo-random choices.
        pub const PAR_SEED: &[u8] = b"Seed\0";
        /// OptimalityTarget=1 → local NLP barrier (fast); default=0 → global B&B (slow).
        pub const PAR_OPTIMALITYTARGET: &[u8] = b"OptimalityTarget\0";

        // ── Attribute names for NLP ───────────────────────────────────────────
        pub const ATTR_START: &[u8] = b"Start\0";

        // ── Callback `where` values (GRB_CB_*) ────────────────────────────────
        pub const GRB_CB_MIP: c_int = 3;

        // ── Callback `what` query codes (GRBcbget) ────────────────────────────
        // Queried from where == GRB_CB_MIP:
        pub const GRB_CB_MIP_OBJBST: c_int = 3000;
        pub const GRB_CB_MIP_OBJBND: c_int = 3001;
        // Queried from any callback location:
        pub const GRB_CB_RUNTIME: c_int = 6001;

        // ── NL expression-tree opcode values (GRB_OPCODE_*) ──────────────────
        pub const OPCODE_CONSTANT: c_int = 0;
        pub const OPCODE_VARIABLE: c_int = 1;
        pub const OPCODE_PLUS: c_int = 2;
        pub const OPCODE_MINUS: c_int = 3;
        pub const OPCODE_MULTIPLY: c_int = 4;
        pub const OPCODE_UMINUS: c_int = 6;
        pub const OPCODE_SQUARE: c_int = 7;
        pub const OPCODE_SIN: c_int = 9;
        pub const OPCODE_COS: c_int = 10;

        /// Cast a null-terminated byte literal to a C string pointer.
        #[inline]
        pub fn cstr(s: &[u8]) -> *const c_char {
            debug_assert!(s.last() == Some(&0), "byte slice must be null-terminated");
            s.as_ptr().cast()
        }
    }

    // ── Runtime library loader ─────────────────────────────────────────────────

    use libloading::Library;
    use std::sync::{Arc, Mutex, OnceLock};

    /// Function pointer table for Gurobi 13 C API, loaded at runtime via dlopen.
    ///
    /// `_lib` keeps the shared library loaded; all function pointers are valid
    /// for the lifetime of this struct.
    #[allow(non_snake_case)]
    struct GurobiLib {
        _lib: Library,
        GRBloadenvinternal: unsafe extern "C" fn(
            *mut *mut ffi::GRBenv,
            *const c_char,
            c_int,
            c_int,
            c_int,
        ) -> c_int,
        GRBfreeenv: unsafe extern "C" fn(*mut ffi::GRBenv),
        GRBgetenv: unsafe extern "C" fn(*mut ffi::GRBmodel) -> *mut ffi::GRBenv,
        GRBnewmodel: unsafe extern "C" fn(
            *mut ffi::GRBenv,
            *mut *mut ffi::GRBmodel,
            *const c_char,
            c_int,
            *const c_double,
            *const c_double,
            *const c_double,
            *const c_char,
            *const *const c_char,
        ) -> c_int,
        GRBfreemodel: unsafe extern "C" fn(*mut ffi::GRBmodel) -> c_int,
        GRBupdatemodel: unsafe extern "C" fn(*mut ffi::GRBmodel) -> c_int,
        GRBaddvars: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            c_int,
            c_int,
            *const c_int,
            *const c_int,
            *const c_double,
            *const c_double,
            *const c_double,
            *const c_double,
            *const c_char,
            *const *const c_char,
        ) -> c_int,
        GRBaddconstrs: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            c_int,
            c_int,
            *const c_int,
            *const c_int,
            *const c_double,
            *const c_char,
            *const c_double,
            *const *const c_char,
        ) -> c_int,
        GRBaddrangeconstrs: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            c_int,
            c_int,
            *const c_int,
            *const c_int,
            *const c_double,
            *const c_double,
            *const c_double,
            *const *const c_char,
        ) -> c_int,
        GRBaddqpterms: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            c_int,
            *const c_int,
            *const c_int,
            *const c_double,
        ) -> c_int,
        GRBaddqconstr: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            c_int,           // numlnz
            *const c_int,    // lind
            *const c_double, // lval
            c_int,           // numqnz
            *const c_int,    // qrow
            *const c_int,    // qcol
            *const c_double, // qval
            c_char,          // sense
            c_double,        // rhs
            *const c_char,   // constrname
        ) -> c_int,
        GRBsetcharattrarray: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            *const c_char,
            c_int,
            c_int,
            *const c_char,
        ) -> c_int,
        GRBsetintattr: unsafe extern "C" fn(*mut ffi::GRBmodel, *const c_char, c_int) -> c_int,
        GRBsetdblattrarray: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            *const c_char,
            c_int,
            c_int,
            *const c_double,
        ) -> c_int,
        GRBwrite: unsafe extern "C" fn(*mut ffi::GRBmodel, *const c_char) -> c_int,
        GRBreset: unsafe extern "C" fn(*mut ffi::GRBmodel, c_int) -> c_int,
        GRBcomputeIIS: unsafe extern "C" fn(*mut ffi::GRBmodel) -> c_int,
        GRBoptimize: unsafe extern "C" fn(*mut ffi::GRBmodel) -> c_int,
        GRBgetintattr: unsafe extern "C" fn(*mut ffi::GRBmodel, *const c_char, *mut c_int) -> c_int,
        GRBgetdblattr:
            unsafe extern "C" fn(*mut ffi::GRBmodel, *const c_char, *mut c_double) -> c_int,
        GRBgetdblattrarray: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            *const c_char,
            c_int,
            c_int,
            *mut c_double,
        ) -> c_int,
        GRBgetintattrarray: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            *const c_char,
            c_int,
            c_int,
            *mut c_int,
        ) -> c_int,
        GRBgetmerrormsg: unsafe extern "C" fn(*mut ffi::GRBmodel) -> *const c_char,
        GRBsetintparam: unsafe extern "C" fn(*mut ffi::GRBenv, *const c_char, c_int) -> c_int,
        GRBsetdblparam: unsafe extern "C" fn(*mut ffi::GRBenv, *const c_char, c_double) -> c_int,
        GRBisgpubuild: unsafe extern "C" fn() -> c_int,
        GRBisgpusupported: unsafe extern "C" fn(*mut ffi::GRBenv) -> c_int,
        GRBaddgenconstrNL: unsafe extern "C" fn(
            *mut ffi::GRBmodel,
            *const c_char,
            c_int,
            c_int,
            *const c_int,
            *const c_double,
            *const c_int,
        ) -> c_int,
        // MIP progress callback surface. `GRBcbget` is queried from
        // inside the callback; `GRBterminate` lets the callback request
        // early termination; `GRBsetcallbackfunc` registers the callback
        // on a model.
        GRBsetcallbackfunc:
            unsafe extern "C" fn(*mut ffi::GRBmodel, Option<GurobiCallback>, *mut c_void) -> c_int,
        #[allow(dead_code)]
        GRBcbget: unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut c_void) -> c_int,
        #[allow(dead_code)]
        GRBterminate: unsafe extern "C" fn(*mut ffi::GRBmodel),
    }

    /// Gurobi callback function pointer type.
    ///
    /// Signature matches `gurobiCB` in `gurobi_c.h`. On Linux and macOS
    /// x86_64 the `__stdcall` attribute in the Gurobi header is a no-op;
    /// both platforms use the System V AMD64 ABI, which is what
    /// `extern "C"` selects. Windows x86_64 also uses a single default
    /// calling convention so `__stdcall` is similarly ignored. Returns 0
    /// for success, non-zero to signal an error that aborts optimization.
    pub(super) type GurobiCallback = unsafe extern "C" fn(
        model: *mut ffi::GRBmodel,
        cbdata: *mut c_void,
        where_: c_int,
        usrdata: *mut c_void,
    ) -> c_int;

    unsafe impl Send for GurobiLib {}
    unsafe impl Sync for GurobiLib {}

    static GUROBI: OnceLock<Result<Arc<GurobiLib>, String>> = OnceLock::new();
    #[cfg(unix)]
    static GUROBI_STDIO_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

    /// Return the cached GurobiLib, loading it on first call.
    fn get_gurobi() -> Result<&'static Arc<GurobiLib>, String> {
        GUROBI
            .get_or_init(|| {
                for path in gurobi_lib_paths() {
                    if let Ok(lib) = unsafe { Library::new(&path) } {
                        return unsafe { load_gurobi_symbols(lib) }.map(Arc::new);
                    }
                }
                Err(concat!(
                    "Gurobi 13 not found — libgurobi130.so (Linux), libgurobi130.dylib (macOS), ",
                    "or gurobi130.dll (Windows) is required. Set GUROBI_HOME=/opt/gurobi1301/linux64 ",
                    "or add the Gurobi 13 lib directory to LD_LIBRARY_PATH. ",
                    "Only Gurobi 13.x (API version 13, library libgurobi130) is supported; ",
                    "older versions (gurobi120, gurobi110, …) have incompatible APIs.",
                ).to_string())
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    fn env_flag_enabled(name: &str) -> bool {
        std::env::var(name)
            .ok()
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    fn env_flag_disabled(name: &str) -> bool {
        std::env::var(name)
            .ok()
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
            })
            .unwrap_or(false)
    }

    #[cfg(unix)]
    unsafe fn silence_stdio<T>(f: impl FnOnce() -> T) -> T {
        use std::fs::OpenOptions;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        unsafe extern "C" {
            fn dup(fd: c_int) -> c_int;
            fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
        }

        struct RestoreStdIo {
            stdout_fd: OwnedFd,
            stderr_fd: OwnedFd,
        }

        impl Drop for RestoreStdIo {
            fn drop(&mut self) {
                unsafe {
                    let _ = dup2(self.stdout_fd.as_raw_fd(), 1);
                    let _ = dup2(self.stderr_fd.as_raw_fd(), 2);
                }
            }
        }

        let mutex = GUROBI_STDIO_MUTEX.get_or_init(|| Mutex::new(()));
        let _lock = mutex.lock().expect("Gurobi stdio mutex poisoned");
        let devnull = match OpenOptions::new().write(true).open("/dev/null") {
            Ok(file) => file,
            Err(_) => return f(),
        };
        let stdout_fd = dup(1);
        let stderr_fd = dup(2);
        if stdout_fd < 0 || stderr_fd < 0 {
            return f();
        }
        if dup2(devnull.as_raw_fd(), 1) < 0 || dup2(devnull.as_raw_fd(), 2) < 0 {
            return f();
        }
        let restore = RestoreStdIo {
            stdout_fd: OwnedFd::from_raw_fd(stdout_fd),
            stderr_fd: OwnedFd::from_raw_fd(stderr_fd),
        };
        let result = f();
        drop(restore);
        result
    }

    #[cfg(not(unix))]
    fn silence_stdio<T>(f: impl FnOnce() -> T) -> T {
        f()
    }

    unsafe fn create_env(lib: &GurobiLib) -> Result<*mut ffi::GRBenv, String> {
        use ffi::{PAR_LOGTOCONSOLE, PAR_OUTPUTFLAG, cstr};

        let mut env: *mut ffi::GRBenv = ptr::null_mut();
        let quiet = !env_flag_disabled("SURGE_GUROBI_QUIET");
        let rc = if quiet {
            silence_stdio(|| (lib.GRBloadenvinternal)(&mut env, ptr::null(), 13, 0, 1))
        } else {
            (lib.GRBloadenvinternal)(&mut env, ptr::null(), 13, 0, 1)
        };
        if rc != 0 || env.is_null() {
            return Err(format!(
                "GRBloadenv failed (rc={rc}) — Gurobi 13 license check failed. \
                 Ensure ~/gurobi.lic (or GRB_LICENSE_FILE) is a valid Gurobi 13 license."
            ));
        }
        if quiet {
            let rc = (lib.GRBsetintparam)(env, cstr(PAR_OUTPUTFLAG), 0);
            if rc != 0 {
                (lib.GRBfreeenv)(env);
                return Err(format!("GRBsetintparam(OutputFlag) failed (rc={rc})"));
            }
            let rc = (lib.GRBsetintparam)(env, cstr(PAR_LOGTOCONSOLE), 0);
            if rc != 0 {
                (lib.GRBfreeenv)(env);
                return Err(format!("GRBsetintparam(LogToConsole) failed (rc={rc})"));
            }
        }
        Ok(env)
    }

    unsafe fn with_env<T>(
        lib: &GurobiLib,
        f: impl FnOnce(*mut ffi::GRBenv) -> Result<T, String>,
    ) -> Result<T, String> {
        let env = create_env(lib)?;

        struct EnvGuard(*mut ffi::GRBenv, unsafe extern "C" fn(*mut ffi::GRBenv));

        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    (self.1)(self.0);
                }
            }
        }

        let _guard = EnvGuard(env, lib.GRBfreeenv);
        f(env)
    }

    fn gurobi_iis_export_requested() -> bool {
        env_flag_enabled("SURGE_GUROBI_EXPORT_IIS")
            || std::env::var_os("SURGE_GUROBI_IIS_PREFIX").is_some()
    }

    fn gurobi_iis_export_prefix() -> PathBuf {
        if let Some(value) = std::env::var_os("SURGE_GUROBI_IIS_PREFIX") {
            let trimmed = value.to_string_lossy().trim().to_string();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(format!("surge-gurobi-iis-{}-{now}", std::process::id()))
    }

    fn append_artifact_suffix(prefix: &Path, suffix: &str) -> PathBuf {
        PathBuf::from(format!("{}{}", prefix.display(), suffix))
    }

    fn summary_field(value: &str) -> String {
        value.replace(['\t', '\n', '\r'], " ")
    }

    fn raw_problem_name(names: Option<&[String]>, idx: usize, prefix: &str) -> String {
        names
            .and_then(|values| values.get(idx))
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("{prefix}_{idx}"))
    }

    fn sanitize_gurobi_name(raw: &str, prefix: &str, idx: usize) -> String {
        let mut sanitized = String::with_capacity(raw.len() + 24);
        sanitized.push_str(prefix);
        sanitized.push('_');
        sanitized.push_str(&idx.to_string());
        sanitized.push('_');
        for ch in raw.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.') {
                sanitized.push(ch);
            } else {
                sanitized.push('_');
            }
        }
        sanitized.truncate(255);
        sanitized
    }

    fn build_gurobi_name_storage(
        names: Option<&[String]>,
        prefix: &str,
        count: usize,
    ) -> Vec<CString> {
        (0..count)
            .map(|idx| {
                CString::new(sanitize_gurobi_name(
                    &raw_problem_name(names, idx, prefix),
                    prefix,
                    idx,
                ))
                .expect("sanitized gurobi name contains no null bytes")
            })
            .collect()
    }

    fn cstring_ptrs(values: &[CString]) -> Vec<*const c_char> {
        values.iter().map(|value| value.as_ptr()).collect()
    }

    unsafe fn gurobi_model_error_message(
        lib: &GurobiLib,
        model: *mut ffi::GRBmodel,
    ) -> Option<String> {
        let ptr = (lib.GRBgetmerrormsg)(model);
        if ptr.is_null() {
            None
        } else {
            Some(CStr::from_ptr(ptr).to_string_lossy().trim().to_string())
                .filter(|message| !message.is_empty())
        }
    }

    fn gurobi_call_error(call: &str, rc: c_int, detail: Option<&str>) -> String {
        match detail {
            Some(detail) if !detail.is_empty() => format!("{call} failed (rc={rc}): {detail}"),
            _ => format!("{call} failed (rc={rc})"),
        }
    }

    fn gurobi_status_from_code(stat: c_int, solution_count: Option<c_int>) -> LpSolveStatus {
        use ffi::*;

        match stat {
            GRB_OPTIMAL | GRB_LOCALLY_OPTIMAL => LpSolveStatus::Optimal,
            GRB_SUBOPTIMAL => LpSolveStatus::SubOptimal,
            GRB_INFEASIBLE => LpSolveStatus::Infeasible,
            GRB_UNBOUNDED => LpSolveStatus::Unbounded,
            GRB_TIME_LIMIT if solution_count.unwrap_or(0) > 0 => LpSolveStatus::SubOptimal,
            GRB_TIME_LIMIT => {
                LpSolveStatus::SolverError("Gurobi status=9 (time limit, no incumbent)".to_string())
            }
            _ => LpSolveStatus::SolverError(format!("Gurobi status={stat}")),
        }
    }

    unsafe fn resolve_gurobi_status(
        lib: &GurobiLib,
        env: *mut ffi::GRBenv,
        model: *mut ffi::GRBmodel,
    ) -> Result<(c_int, LpSolveStatus), String> {
        use ffi::*;

        let mut stat: c_int = 0;
        let rc = (lib.GRBgetintattr)(model, cstr(ATTR_STATUS), &mut stat);
        if rc != 0 {
            return Err(gurobi_call_error(
                "GRBgetintattr(Status)",
                rc,
                gurobi_model_error_message(lib, model).as_deref(),
            ));
        }
        if stat == GRB_INF_OR_UNBD {
            let model_env = (lib.GRBgetenv)(model);
            let retry_env = if model_env.is_null() { env } else { model_env };
            let rc = (lib.GRBsetintparam)(retry_env, cstr(PAR_DUALREDUCTIONS), 0);
            if rc != 0 {
                return Err(gurobi_call_error(
                    "GRBsetintparam(DualReductions)",
                    rc,
                    gurobi_model_error_message(lib, model).as_deref(),
                ));
            }
            let rc = (lib.GRBsetintparam)(retry_env, cstr(PAR_INFUNBDINFO), 1);
            if rc != 0 {
                return Err(gurobi_call_error(
                    "GRBsetintparam(InfUnbdInfo)",
                    rc,
                    gurobi_model_error_message(lib, model).as_deref(),
                ));
            }
            let rc = (lib.GRBreset)(model, 0);
            if rc != 0 {
                return Err(gurobi_call_error(
                    "GRBreset",
                    rc,
                    gurobi_model_error_message(lib, model).as_deref(),
                ));
            }
            let rc = (lib.GRBoptimize)(model);
            if rc != 0 {
                return Err(gurobi_call_error(
                    "GRBoptimize",
                    rc,
                    gurobi_model_error_message(lib, model).as_deref(),
                ));
            }
            let rc = (lib.GRBgetintattr)(model, cstr(ATTR_STATUS), &mut stat);
            if rc != 0 {
                return Err(gurobi_call_error(
                    "GRBgetintattr(Status)",
                    rc,
                    gurobi_model_error_message(lib, model).as_deref(),
                ));
            }
        }
        let solution_count = if stat == GRB_TIME_LIMIT {
            let mut count: c_int = 0;
            let rc = (lib.GRBgetintattr)(model, cstr(ATTR_SOLCOUNT), &mut count);
            if rc != 0 {
                return Err(gurobi_call_error(
                    "GRBgetintattr(SolCount)",
                    rc,
                    gurobi_model_error_message(lib, model).as_deref(),
                ));
            }
            Some(count)
        } else {
            None
        };
        Ok((stat, gurobi_status_from_code(stat, solution_count)))
    }

    struct GurobiIisExport {
        ilp_path: PathBuf,
        summary_path: PathBuf,
        n_rows: usize,
        n_bounds: usize,
        minimal: bool,
    }

    unsafe fn export_gurobi_iis(
        lib: &GurobiLib,
        model: *mut ffi::GRBmodel,
        prob: &SparseProblem,
        orig_to_grb: &[usize],
    ) -> Result<GurobiIisExport, String> {
        use ffi::*;

        let rc = (lib.GRBcomputeIIS)(model);
        if rc != 0 {
            return Err(gurobi_call_error(
                "GRBcomputeIIS",
                rc,
                gurobi_model_error_message(lib, model).as_deref(),
            ));
        }

        let mut minimal = 0;
        let rc = (lib.GRBgetintattr)(model, cstr(ATTR_IIS_MINIMAL), &mut minimal);
        if rc != 0 {
            return Err(gurobi_call_error(
                "GRBgetintattr(IISMinimal)",
                rc,
                gurobi_model_error_message(lib, model).as_deref(),
            ));
        }

        let mut iis_rows = vec![0; prob.n_row];
        let rc = (lib.GRBgetintattrarray)(
            model,
            cstr(ATTR_IIS_CONSTR),
            0,
            prob.n_row as c_int,
            iis_rows.as_mut_ptr(),
        );
        if rc != 0 {
            return Err(gurobi_call_error(
                "GRBgetintattrarray(IISConstr)",
                rc,
                gurobi_model_error_message(lib, model).as_deref(),
            ));
        }

        let mut iis_lb = vec![0; prob.n_col];
        let rc = (lib.GRBgetintattrarray)(
            model,
            cstr(ATTR_IIS_LB),
            0,
            prob.n_col as c_int,
            iis_lb.as_mut_ptr(),
        );
        if rc != 0 {
            return Err(gurobi_call_error(
                "GRBgetintattrarray(IISLB)",
                rc,
                gurobi_model_error_message(lib, model).as_deref(),
            ));
        }

        let mut iis_ub = vec![0; prob.n_col];
        let rc = (lib.GRBgetintattrarray)(
            model,
            cstr(ATTR_IIS_UB),
            0,
            prob.n_col as c_int,
            iis_ub.as_mut_ptr(),
        );
        if rc != 0 {
            return Err(gurobi_call_error(
                "GRBgetintattrarray(IISUB)",
                rc,
                gurobi_model_error_message(lib, model).as_deref(),
            ));
        }

        let prefix = gurobi_iis_export_prefix();
        if let Some(parent) = prefix.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|err| {
                    format!(
                        "failed to create IIS export directory {}: {err}",
                        parent.display()
                    )
                })?;
            }
        }
        let ilp_path = append_artifact_suffix(&prefix, ".ilp");
        let summary_path = append_artifact_suffix(&prefix, ".summary.txt");

        let ilp_path_c = CString::new(ilp_path.to_string_lossy().into_owned())
            .expect("artifact path contains no null bytes");
        let rc = (lib.GRBwrite)(model, ilp_path_c.as_ptr());
        if rc != 0 {
            return Err(gurobi_call_error(
                "GRBwrite",
                rc,
                gurobi_model_error_message(lib, model).as_deref(),
            ));
        }

        let gurobi_row_names =
            build_gurobi_name_storage(prob.row_names.as_deref(), "r", prob.n_row);
        let gurobi_col_names =
            build_gurobi_name_storage(prob.col_names.as_deref(), "c", prob.n_col);
        let mut summary = BufWriter::new(
            File::create(&summary_path)
                .map_err(|err| format!("failed to create {}: {err}", summary_path.display()))?,
        );

        let row_count = orig_to_grb
            .iter()
            .enumerate()
            .filter(|(idx, grb_idx)| **grb_idx < iis_rows.len() && iis_rows[orig_to_grb[*idx]] != 0)
            .count();
        let bound_count = iis_lb.iter().filter(|&&flag| flag != 0).count()
            + iis_ub.iter().filter(|&&flag| flag != 0).count();

        writeln!(
            summary,
            "kind\tindex\tgurobi_index\tgurobi_name\tname\tbound\tlower\tupper"
        )
        .map_err(|err| format!("failed to write {}: {err}", summary_path.display()))?;
        writeln!(
            summary,
            "# minimal={}\trows={}\tbounds={}",
            minimal != 0,
            row_count,
            bound_count
        )
        .map_err(|err| format!("failed to write {}: {err}", summary_path.display()))?;

        for row_idx in 0..prob.n_row {
            let grb_idx = orig_to_grb[row_idx];
            if grb_idx >= iis_rows.len() || iis_rows[grb_idx] == 0 {
                continue;
            }
            writeln!(
                summary,
                "row\t{}\t{}\t{}\t{}\t-\t{}\t{}",
                row_idx,
                grb_idx,
                gurobi_row_names[row_idx].to_string_lossy(),
                summary_field(&raw_problem_name(prob.row_names.as_deref(), row_idx, "row")),
                prob.row_lower[row_idx],
                prob.row_upper[row_idx]
            )
            .map_err(|err| format!("failed to write {}: {err}", summary_path.display()))?;
        }

        for col_idx in 0..prob.n_col {
            if iis_lb[col_idx] != 0 {
                writeln!(
                    summary,
                    "col\t{}\t{}\t{}\t{}\tlb\t{}\t{}",
                    col_idx,
                    col_idx,
                    gurobi_col_names[col_idx].to_string_lossy(),
                    summary_field(&raw_problem_name(prob.col_names.as_deref(), col_idx, "col")),
                    prob.col_lower[col_idx],
                    prob.col_upper[col_idx]
                )
                .map_err(|err| format!("failed to write {}: {err}", summary_path.display()))?;
            }
            if iis_ub[col_idx] != 0 {
                writeln!(
                    summary,
                    "col\t{}\t{}\t{}\t{}\tub\t{}\t{}",
                    col_idx,
                    col_idx,
                    gurobi_col_names[col_idx].to_string_lossy(),
                    summary_field(&raw_problem_name(prob.col_names.as_deref(), col_idx, "col")),
                    prob.col_lower[col_idx],
                    prob.col_upper[col_idx]
                )
                .map_err(|err| format!("failed to write {}: {err}", summary_path.display()))?;
            }
        }
        summary
            .flush()
            .map_err(|err| format!("failed to finalize {}: {err}", summary_path.display()))?;

        Ok(GurobiIisExport {
            ilp_path,
            summary_path,
            n_rows: row_count,
            n_bounds: bound_count,
            minimal: minimal != 0,
        })
    }

    fn gurobi_lib_paths() -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        let lib_name = if cfg!(target_os = "linux") {
            "libgurobi130.so"
        } else if cfg!(target_os = "macos") {
            "libgurobi130.dylib"
        } else {
            "gurobi130.dll"
        };
        // GUROBI_HOME (set by Gurobi installer or user)
        if let Ok(home) = std::env::var("GUROBI_HOME") {
            paths.push(std::path::Path::new(&home).join("lib").join(lib_name));
        }
        // Common Linux install paths (Gurobi 13.0.x)
        #[cfg(target_os = "linux")]
        {
            paths.push("/opt/gurobi1301/linux64/lib/libgurobi130.so".into());
            paths.push("/opt/gurobi1300/linux64/lib/libgurobi130.so".into());
        }
        // `pip install gurobipy` bundles the real shared library inside
        // site-packages/gurobipy/.libs/.  Probe common site-packages roots.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mut base_dirs: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
                base_dirs.push(std::path::PathBuf::from(venv).join("lib"));
            }
            if let Ok(conda) = std::env::var("CONDA_PREFIX") {
                base_dirs.push(std::path::PathBuf::from(conda).join("lib"));
            }
            if let Ok(home) = std::env::var("HOME") {
                base_dirs.push(std::path::PathBuf::from(home).join(".local/lib"));
            }
            for base in &base_dirs {
                if let Ok(entries) = std::fs::read_dir(base) {
                    for entry in entries.flatten() {
                        if let Some(name) = entry.file_name().to_str() {
                            if name.starts_with("python3") {
                                paths.push(
                                    entry
                                        .path()
                                        .join("site-packages/gurobipy/.libs")
                                        .join(lib_name),
                                );
                            }
                        }
                    }
                }
            }
        }
        // Let OS linker search (LD_LIBRARY_PATH / DYLD_LIBRARY_PATH)
        paths.push(lib_name.into());
        paths
    }

    unsafe fn load_gurobi_symbols(lib: Library) -> Result<GurobiLib, String> {
        macro_rules! sym {
            ($name:literal, $ty:ty) => {
                *lib.get::<$ty>($name).map_err(|e| {
                    format!(
                        "Gurobi: symbol '{}' not found: {e}",
                        std::str::from_utf8($name).unwrap_or("?")
                    )
                })?
            };
        }
        #[allow(non_snake_case)]
        Ok(GurobiLib {
            GRBloadenvinternal: sym!(
                b"GRBloadenvinternal\0",
                unsafe extern "C" fn(
                    *mut *mut ffi::GRBenv,
                    *const c_char,
                    c_int,
                    c_int,
                    c_int,
                ) -> c_int
            ),
            GRBfreeenv: sym!(b"GRBfreeenv\0", unsafe extern "C" fn(*mut ffi::GRBenv)),
            GRBgetenv: sym!(
                b"GRBgetenv\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel) -> *mut ffi::GRBenv
            ),
            GRBnewmodel: sym!(
                b"GRBnewmodel\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBenv,
                    *mut *mut ffi::GRBmodel,
                    *const c_char,
                    c_int,
                    *const c_double,
                    *const c_double,
                    *const c_double,
                    *const c_char,
                    *const *const c_char,
                ) -> c_int
            ),
            GRBfreemodel: sym!(
                b"GRBfreemodel\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel) -> c_int
            ),
            GRBupdatemodel: sym!(
                b"GRBupdatemodel\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel) -> c_int
            ),
            GRBaddvars: sym!(
                b"GRBaddvars\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    c_int,
                    c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                    *const c_double,
                    *const c_double,
                    *const c_double,
                    *const c_char,
                    *const *const c_char,
                ) -> c_int
            ),
            GRBaddconstrs: sym!(
                b"GRBaddconstrs\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    c_int,
                    c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                    *const c_char,
                    *const c_double,
                    *const *const c_char,
                ) -> c_int
            ),
            GRBaddrangeconstrs: sym!(
                b"GRBaddrangeconstrs\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    c_int,
                    c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                    *const c_double,
                    *const c_double,
                    *const *const c_char,
                ) -> c_int
            ),
            GRBaddqpterms: sym!(
                b"GRBaddqpterms\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                ) -> c_int
            ),
            GRBaddqconstr: sym!(
                b"GRBaddqconstr\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
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
            GRBsetcharattrarray: sym!(
                b"GRBsetcharattrarray\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    *const c_char,
                    c_int,
                    c_int,
                    *const c_char,
                ) -> c_int
            ),
            GRBsetintattr: sym!(
                b"GRBsetintattr\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel, *const c_char, c_int) -> c_int
            ),
            GRBsetdblattrarray: sym!(
                b"GRBsetdblattrarray\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    *const c_char,
                    c_int,
                    c_int,
                    *const c_double,
                ) -> c_int
            ),
            GRBwrite: sym!(
                b"GRBwrite\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel, *const c_char) -> c_int
            ),
            GRBreset: sym!(
                b"GRBreset\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel, c_int) -> c_int
            ),
            GRBcomputeIIS: sym!(
                b"GRBcomputeIIS\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel) -> c_int
            ),
            GRBoptimize: sym!(
                b"GRBoptimize\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel) -> c_int
            ),
            GRBgetintattr: sym!(
                b"GRBgetintattr\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel, *const c_char, *mut c_int) -> c_int
            ),
            GRBgetdblattr: sym!(
                b"GRBgetdblattr\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel, *const c_char, *mut c_double) -> c_int
            ),
            GRBgetdblattrarray: sym!(
                b"GRBgetdblattrarray\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    *const c_char,
                    c_int,
                    c_int,
                    *mut c_double,
                ) -> c_int
            ),
            GRBgetintattrarray: sym!(
                b"GRBgetintattrarray\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    *const c_char,
                    c_int,
                    c_int,
                    *mut c_int,
                ) -> c_int
            ),
            GRBgetmerrormsg: sym!(
                b"GRBgetmerrormsg\0",
                unsafe extern "C" fn(*mut ffi::GRBmodel) -> *const c_char
            ),
            GRBsetintparam: sym!(
                b"GRBsetintparam\0",
                unsafe extern "C" fn(*mut ffi::GRBenv, *const c_char, c_int) -> c_int
            ),
            GRBsetdblparam: sym!(
                b"GRBsetdblparam\0",
                unsafe extern "C" fn(*mut ffi::GRBenv, *const c_char, c_double) -> c_int
            ),
            GRBisgpubuild: sym!(b"GRBisgpubuild\0", unsafe extern "C" fn() -> c_int),
            GRBisgpusupported: sym!(
                b"GRBisgpusupported\0",
                unsafe extern "C" fn(*mut ffi::GRBenv) -> c_int
            ),
            GRBaddgenconstrNL: sym!(
                b"GRBaddgenconstrNL\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    *const c_char,
                    c_int,
                    c_int,
                    *const c_int,
                    *const c_double,
                    *const c_int,
                ) -> c_int
            ),
            GRBsetcallbackfunc: sym!(
                b"GRBsetcallbackfunc\0",
                unsafe extern "C" fn(
                    *mut ffi::GRBmodel,
                    Option<GurobiCallback>,
                    *mut c_void,
                ) -> c_int
            ),
            GRBcbget: sym!(
                b"GRBcbget\0",
                unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut c_void) -> c_int
            ),
            GRBterminate: sym!(b"GRBterminate\0", unsafe extern "C" fn(*mut ffi::GRBmodel)),
            _lib: lib,
        })
    }

    // ── GurobiLpSolver ────────────────────────────────────────────────────────

    /// Gurobi 13 LP/QP/MIP solver (commercial license required).
    ///
    /// Supports LP, QP (quadratic objective with upper-triangle CSC Hessian),
    /// MILP, and MIQP.  GPU-accelerated LP is available when `GRB_USE_GPU=1`
    /// is set in the environment and Gurobi 13 detects a compatible CUDA device.
    pub struct GurobiLpSolver {
        lib: Arc<GurobiLib>,
    }

    impl std::fmt::Debug for GurobiLpSolver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("GurobiLpSolver").finish()
        }
    }

    impl GurobiLpSolver {
        /// Load the Gurobi 13 shared library at runtime.
        ///
        /// Searches for libgurobi130.so (Linux) / libgurobi130.dylib (macOS) /
        /// gurobi130.dll (Windows) via GUROBI_HOME env var and common paths.
        ///
        /// This does **not** validate that the runtime can create a licensed
        /// environment. Call [`Self::validate_runtime`] or
        /// [`Self::new_validated`] when the caller needs a fully usable solver.
        pub fn new() -> Result<Self, String> {
            let lib = get_gurobi()?.clone();
            Ok(Self { lib })
        }

        /// Load the Gurobi 13 shared library and validate runtime usability.
        pub fn new_validated() -> Result<Self, String> {
            let solver = Self::new()?;
            solver.validate_runtime()?;
            Ok(solver)
        }

        /// Validate that the loaded runtime can create a licensed Gurobi environment.
        pub fn validate_runtime(&self) -> Result<(), String> {
            unsafe { with_env(&self.lib, |_env| Ok(())) }
        }
    }

    impl LpSolver for GurobiLpSolver {
        fn name(&self) -> &'static str {
            "Gurobi"
        }

        fn version(&self) -> &'static str {
            "13.0"
        }

        fn solve(&self, prob: &SparseProblem, opts: &LpOptions) -> Result<LpResult, String> {
            unsafe { with_env(&self.lib, |env| solve_inner(&self.lib, env, prob, opts)) }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Transpose a CSC matrix to full CSR.
    ///
    /// Returns `(row_start, col_ind, csr_val)` where `row_start` has length
    /// `n_row + 1`.
    fn csc_to_csr(
        n_row: usize,
        n_col: usize,
        a_start: &[i32],
        a_index: &[i32],
        a_value: &[f64],
    ) -> (Vec<i32>, Vec<i32>, Vec<f64>) {
        let nnz = a_value.len();
        let mut row_nnz = vec![0i32; n_row];
        for &ri in a_index {
            row_nnz[ri as usize] += 1;
        }
        let mut row_start = vec![0i32; n_row + 1];
        for i in 0..n_row {
            row_start[i + 1] = row_start[i] + row_nnz[i];
        }
        let mut col_ind = vec![0i32; nnz];
        let mut csr_val = vec![0.0f64; nnz];
        let mut pos: Vec<i32> = row_start[..n_row].to_vec(); // write cursors
        for j in 0..n_col {
            for k in a_start[j] as usize..a_start[j + 1] as usize {
                let ri = a_index[k] as usize;
                let p = pos[ri] as usize;
                col_ind[p] = j as i32;
                csr_val[p] = a_value[k];
                pos[ri] += 1;
            }
        }
        (row_start, col_ind, csr_val)
    }

    // ── MIP progress callback ─────────────────────────────────────────────────

    /// Minimal set of Gurobi C function pointers needed inside the callback.
    /// Plain function pointers (no lifetimes, `Copy`) so the callback can
    /// invoke them without reaching back through an `Arc`.
    #[allow(dead_code)]
    #[derive(Copy, Clone)]
    struct MipCallbackPtrs {
        cbget: unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut c_void) -> c_int,
        terminate: unsafe extern "C" fn(*mut ffi::GRBmodel),
    }

    /// User-data payload passed to `gurobi_mip_progress_callback` through
    /// `GRBsetcallbackfunc`. Owned on the Rust stack for the duration of
    /// `GRBoptimize` and reclaimed when the solve returns.
    #[allow(dead_code)]
    struct MipCallbackContext {
        ptrs: MipCallbackPtrs,
        monitor: crate::backends::MipProgressMonitor,
    }

    /// Gurobi progress callback. Reads `runtime`/`objbst`/`objbnd` on every
    /// `GRB_CB_MIP` tick, feeds them into the solver-agnostic
    /// [`crate::backends::MipProgressMonitor`], and requests termination via
    /// `GRBterminate` when the monitor signals that the scheduled gap has
    /// been reached.
    #[allow(dead_code)]
    unsafe extern "C" fn gurobi_mip_progress_callback(
        model: *mut ffi::GRBmodel,
        cbdata: *mut c_void,
        where_: c_int,
        usrdata: *mut c_void,
    ) -> c_int {
        use ffi::*;

        if where_ != GRB_CB_MIP || usrdata.is_null() {
            return 0;
        }
        let ctx = unsafe { &mut *(usrdata as *mut MipCallbackContext) };
        let ptrs = ctx.ptrs;

        let mut runtime: c_double = 0.0;
        let mut objbst: c_double = 0.0;
        let mut objbnd: c_double = 0.0;
        unsafe {
            if (ptrs.cbget)(
                cbdata,
                where_,
                GRB_CB_RUNTIME,
                (&mut runtime as *mut c_double).cast(),
            ) != 0
            {
                return 0;
            }
            if (ptrs.cbget)(
                cbdata,
                where_,
                GRB_CB_MIP_OBJBST,
                (&mut objbst as *mut c_double).cast(),
            ) != 0
            {
                return 0;
            }
            if (ptrs.cbget)(
                cbdata,
                where_,
                GRB_CB_MIP_OBJBND,
                (&mut objbnd as *mut c_double).cast(),
            ) != 0
            {
                return 0;
            }
        }
        // GRB_CB_MIP reports OBJBST as +inf (for minimization) until the
        // first incumbent is known. Don't record events until then.
        if !objbst.is_finite() {
            return 0;
        }
        let should_terminate = ctx.monitor.tick(runtime, objbst, objbnd);
        if should_terminate {
            unsafe {
                (ptrs.terminate)(model);
            }
        }
        0
    }

    // ── Core solve ────────────────────────────────────────────────────────────

    unsafe fn solve_inner(
        lib: &GurobiLib,
        env: *mut ffi::GRBenv,
        prob: &SparseProblem,
        opts: &LpOptions,
    ) -> Result<LpResult, String> {
        use ffi::*;
        // Bind lib function pointers to local names matching the original C API,
        // so the rest of solve_inner is unchanged.
        #[allow(non_snake_case)]
        let (
            GRBsetintparam,
            GRBsetdblparam,
            GRBnewmodel,
            GRBfreemodel,
            GRBaddvars,
            GRBsetcharattrarray,
            GRBsetdblattrarray,
            GRBaddqpterms,
            GRBaddconstrs,
            GRBaddrangeconstrs,
            _GRBwrite,
            _GRBreset,
            _GRBcomputeIIS,
            GRBoptimize,
            GRBgetintattr,
            GRBgetdblattr,
            GRBgetdblattrarray,
            _GRBgetintattrarray,
            _GRBgetmerrormsg,
            GRBisgpubuild,
            GRBisgpusupported,
        ) = (
            lib.GRBsetintparam,
            lib.GRBsetdblparam,
            lib.GRBnewmodel,
            lib.GRBfreemodel,
            lib.GRBaddvars,
            lib.GRBsetcharattrarray,
            lib.GRBsetdblattrarray,
            lib.GRBaddqpterms,
            lib.GRBaddconstrs,
            lib.GRBaddrangeconstrs,
            lib.GRBwrite,
            lib.GRBreset,
            lib.GRBcomputeIIS,
            lib.GRBoptimize,
            lib.GRBgetintattr,
            lib.GRBgetdblattr,
            lib.GRBgetdblattrarray,
            lib.GRBgetintattrarray,
            lib.GRBgetmerrormsg,
            lib.GRBisgpubuild,
            lib.GRBisgpusupported,
        );

        // ── Configure environment parameters ─────────────────────────────────
        // `SURGE_GUROBI_PRINT=1` forces Gurobi's console output on from
        // the outside for diagnostics without editing caller code.
        let env_print = std::env::var("SURGE_GUROBI_PRINT").as_deref() == Ok("1");
        let print = c_int::from(opts.print_level > 0 || env_print);
        GRBsetintparam(env, cstr(PAR_OUTPUTFLAG), print);
        GRBsetintparam(env, cstr(PAR_LOGTOCONSOLE), print);
        let tol = opts.tolerance.clamp(1e-10, 1e-4);
        GRBsetdblparam(env, cstr(PAR_FEASIBILITYTOL), tol);
        GRBsetdblparam(env, cstr(PAR_OPTIMALITYTOL), tol);
        if let Some(tl) = opts.time_limit_secs {
            GRBsetdblparam(env, cstr(PAR_TIMELIMIT), tl);
        }
        // Always set Gurobi's MIPGap parameter explicitly. The caller's
        // `mip_rel_gap` wins; when absent we default to a deliberately-
        // loose 10 % so it is OBVIOUS in a diagnostic run that nobody
        // configured a gap (objectives at 10 % off the bound stand out
        // in any regression or SLA check). Gurobi's own unset default is
        // 1e-4 which would silently drag a hard SCUC to timeout.
        //
        // The `mip_gap_schedule` path is intentionally ignored here —
        // it introduced a subtle override where `sched.min_gap()` took
        // precedence over `mip_rel_gap`, which combined with any tight
        // first step (e.g. 1e-5) pushed Gurobi into a "keep going until
        // provably optimal" regime. Schedule data still flows through
        // the public API for callers that want to wire their own
        // callback logic, but this backend no longer honours it.
        const FALLBACK_MIP_GAP_WHEN_UNSET: f64 = 1e-1;
        let safety_net_gap = opts.mip_rel_gap.unwrap_or(FALLBACK_MIP_GAP_WHEN_UNSET);
        GRBsetdblparam(env, cstr(PAR_MIPGAP), safety_net_gap);

        // GPU: enable PDHG + PDHGGPU when GRB_USE_GPU=1 is set.
        let use_gpu = std::env::var("GRB_USE_GPU").as_deref() == Ok("1");
        if use_gpu && GRBisgpubuild() != 0 && GRBisgpusupported(env) != 0 {
            GRBsetintparam(env, cstr(PAR_METHOD), GRB_METHOD_PDHG);
            GRBsetintparam(env, cstr(PAR_PDHGGPU), 1);
        }

        // ── Default LP algorithm + thread count ──────────────────────────
        //
        // `Method = 3` is Gurobi's non-deterministic concurrent LP
        // algorithm: it races primal simplex, dual simplex, and
        // barrier on separate threads, takes the first to converge,
        // and terminates the rest. The default auto-method (`-1`)
        // tends to pick primal simplex for post-presolve SCUC LPs,
        // which handles densified rows poorly; concurrent avoids that
        // bad guess whenever there is at least one other thread to
        // spare for a parallel branch.
        //
        // `Threads = 8` gives Method 3 room to actually race (one
        // thread per algorithm plus headroom for the branch-and-bound
        // tree). On a 1-thread box this would collapse to Method 3
        // serialised, so callers that need single-threaded runs (e.g.
        // reproducible A/B benchmarking) should override via
        // `SURGE_GUROBI_THREADS=1` AND `SURGE_GUROBI_METHOD=1` (dual
        // simplex, the fastest deterministic single-thread choice we
        // measured on SCUC).
        const DEFAULT_METHOD: c_int = 3;
        const DEFAULT_THREADS: c_int = 8;
        GRBsetintparam(env, cstr(PAR_METHOD), DEFAULT_METHOD);
        GRBsetintparam(env, cstr(PAR_THREADS), DEFAULT_THREADS);

        // Env-var overrides: honoured last so benchmarking / debugging
        // can pin any parameter without touching policy structures.
        // `SURGE_GUROBI_THREADS=1 SURGE_GUROBI_SEED=0 SURGE_GUROBI_METHOD=1`
        // is the canonical deterministic configuration for MIP-
        // tightening A/Bs.
        if let Ok(v) = std::env::var("SURGE_GUROBI_THREADS") {
            if let Ok(n) = v.parse::<c_int>() {
                GRBsetintparam(env, cstr(PAR_THREADS), n);
            }
        }
        if let Ok(v) = std::env::var("SURGE_GUROBI_SEED") {
            if let Ok(n) = v.parse::<c_int>() {
                GRBsetintparam(env, cstr(PAR_SEED), n);
            }
        }
        if let Ok(v) = std::env::var("SURGE_GUROBI_METHOD") {
            if let Ok(n) = v.parse::<c_int>() {
                GRBsetintparam(env, cstr(PAR_METHOD), n);
            }
        }
        if let Ok(v) = std::env::var("SURGE_GUROBI_PRESOLVE") {
            if let Ok(n) = v.parse::<c_int>() {
                const PAR_PRESOLVE: &[u8] = b"Presolve\0";
                GRBsetintparam(env, cstr(PAR_PRESOLVE), n);
            }
        }

        // ── Create model ──────────────────────────────────────────────────────
        let name = CString::new("surge").expect("static string contains no null bytes");
        let mut model: *mut GRBmodel = ptr::null_mut();
        let rc = GRBnewmodel(
            env,
            &mut model,
            name.as_ptr(),
            0, // add variables below via GRBaddvars
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
        );
        if rc != 0 || model.is_null() {
            return Err(format!("GRBnewmodel failed (rc={rc})"));
        }

        // Ensure model is freed on all exit paths.
        struct ModelGuard(*mut GRBmodel, unsafe extern "C" fn(*mut GRBmodel) -> c_int);
        impl Drop for ModelGuard {
            fn drop(&mut self) {
                unsafe {
                    (self.1)(self.0);
                }
            }
        }
        let _guard = ModelGuard(model, GRBfreemodel);

        let col_name_storage =
            build_gurobi_name_storage(prob.col_names.as_deref(), "c", prob.n_col);
        let col_name_ptrs = cstring_ptrs(&col_name_storage);

        // ── Add variables ─────────────────────────────────────────────────────
        // All variables are continuous by default; MIP types set below.
        let vtypes: Vec<c_char> = vec![GRB_CONTINUOUS; prob.n_col];
        let rc = GRBaddvars(
            model,
            prob.n_col as c_int,
            0, // no matrix coefficients here (constraints added separately)
            ptr::null(),
            ptr::null(),
            ptr::null(),
            prob.col_cost.as_ptr(),
            prob.col_lower.as_ptr(),
            prob.col_upper.as_ptr(),
            vtypes.as_ptr(),
            if col_name_ptrs.is_empty() {
                ptr::null()
            } else {
                col_name_ptrs.as_ptr()
            },
        );
        if rc != 0 {
            return Err(format!("GRBaddvars failed (rc={rc})"));
        }

        // ── Integrality (MIP) ─────────────────────────────────────────────────
        let is_mip = prob
            .integrality
            .as_ref()
            .is_some_and(|iv| iv.iter().any(|&v| is_integer_domain(v)));

        if is_mip {
            let ctypes: Vec<c_char> = prob
                .integrality
                .as_ref()
                .expect("integrality Some when is_mip is true")
                .iter()
                .map(|&v| gurobi_vtype(v))
                .collect();
            let rc = GRBsetcharattrarray(
                model,
                cstr(ATTR_VTYPE),
                0,
                prob.n_col as c_int,
                ctypes.as_ptr(),
            );
            if rc != 0 {
                return Err(format!("GRBsetcharattrarray(VType) failed (rc={rc})"));
            }
        }

        // ── Primal start (warm-start hint) ────────────────────────────────────
        //
        // Gurobi's `Start` attribute works for both MIP (MIPSTART-equivalent)
        // and LP (interpreted as a basis hint). Accepting `LpPrimalStart`
        // on the LP path lets callers warm-start a re-solve from a prior
        // solution — e.g. the SCUC loss-factor LP re-solve reusing the
        // MIP's dispatch when the RHS shifts by only 1-3 % for loss
        // allocation. Cold re-solves on a 440k-col LP cost several
        // seconds; a good primal hint cuts that to sub-second.
        if let Some(start) = opts.primal_start.as_ref() {
            // Gurobi sentinel for "no start value at this index" is
            // GRB_UNDEFINED = 1e101. Use it for sparse starts so only
            // the supplied indices warm-start while everything else
            // remains free for the solver to initialize.
            const GRB_UNDEFINED: c_double = 1e101;
            let start_values: Option<Vec<c_double>> = match start {
                crate::backends::LpPrimalStart::Dense(values) => {
                    if values.len() == prob.n_col {
                        Some(values.to_vec())
                    } else {
                        None
                    }
                }
                crate::backends::LpPrimalStart::Sparse { indices, values } => {
                    let mut dense = vec![GRB_UNDEFINED; prob.n_col];
                    let mut ok = true;
                    for (&idx, &val) in indices.iter().zip(values.iter()) {
                        if idx >= prob.n_col {
                            ok = false;
                            break;
                        }
                        dense[idx] = val;
                    }
                    if ok { Some(dense) } else { None }
                }
            };
            if let Some(values) = start_values {
                let rc = GRBsetdblattrarray(
                    model,
                    cstr(ATTR_START),
                    0,
                    prob.n_col as c_int,
                    values.as_ptr(),
                );
                if rc != 0 {
                    // Log but don't fail — a bad start hint shouldn't
                    // abort an otherwise-valid solve.
                    tracing::warn!(
                        rc,
                        "Gurobi: GRBsetdblattrarray(Start) failed; continuing cold"
                    );
                }
            }
        }

        // ── Quadratic objective (QP) ──────────────────────────────────────────
        //
        // Gurobi's `GRBaddqpterms` adds  qval[k] * x[qrow[k]] * x[qcol[k]]
        // directly to the objective (no implicit 0.5 factor).
        //
        // Our SparseProblem convention: upper-triangle CSC Q where the model
        // is  min  0.5 * x'Qx + c'x.  Mapping to Gurobi:
        //   - Diagonal (i,i):      qval = q / 2  (so contribution = q/2 * x_i^2 ✓)
        //   - Off-diagonal (i,j):  qval = q      (so contribution = q * x_i * x_j ✓,
        //                           representing both (i,j) and (j,i) of the symmetric Q)
        if let (Some(qs), Some(qi), Some(qv)) = (
            prob.q_start.as_ref(),
            prob.q_index.as_ref(),
            prob.q_value.as_ref(),
        ) {
            let mut qrow = Vec::new();
            let mut qcol = Vec::new();
            let mut qval = Vec::new();
            for j in 0..prob.n_col {
                for k in qs[j] as usize..qs[j + 1] as usize {
                    let i = qi[k] as usize;
                    let v = qv[k];
                    qrow.push(i as c_int);
                    qcol.push(j as c_int);
                    qval.push(if i == j { v / 2.0 } else { v });
                }
            }
            if !qval.is_empty() {
                let rc = GRBaddqpterms(
                    model,
                    qval.len() as c_int,
                    qrow.as_ptr(),
                    qcol.as_ptr(),
                    qval.as_ptr(),
                );
                if rc != 0 {
                    return Err(format!("GRBaddqpterms failed (rc={rc})"));
                }
            }
        }

        // ── Add constraints ───────────────────────────────────────────────────
        //
        // Classify each row as range (`lb <= Ax <= ub` with distinct finite
        // bounds) vs non-range (equality, ≤, ≥), then distribute each CSC
        // entry directly into the appropriate row-oriented buffer in a
        // single O(nnz) pass. This replaces the previous
        // `csc_to_csr` + per-row slice-extend pair, saving one
        // matrix-sized allocation and one full-matrix memcpy per solve.
        //
        // We track orig_to_grb[i] = Gurobi constraint index for original
        // row i, needed to reassemble duals in the original SparseProblem
        // row order.
        let row_name_storage =
            build_gurobi_name_storage(prob.row_names.as_deref(), "r", prob.n_row);

        // Classify rows and count nonzeros per row in a single CSC scan,
        // so we can pre-size the split CSR buffers.
        let mut row_is_range = vec![false; prob.n_row];
        let mut row_nnz = vec![0i32; prob.n_row];
        let mut n_non_range = 0usize;
        let mut n_range = 0usize;
        for i in 0..prob.n_row {
            let lb = prob.row_lower[i];
            let ub = prob.row_upper[i];
            let is_range = lb > -1e29 && ub < 1e29 && (ub - lb).abs() > 1e-12 * ub.abs().max(1.0);
            row_is_range[i] = is_range;
            if is_range {
                n_range += 1;
            } else {
                n_non_range += 1;
            }
        }
        for &ri in &prob.a_index {
            row_nnz[ri as usize] += 1;
        }

        // Assign Gurobi row indices: non-range rows first, then range rows.
        // orig_to_grb lets us remap later when reading back row duals.
        let mut orig_to_grb = vec![0usize; prob.n_row];
        {
            let mut nr_idx = 0usize;
            let mut rn_idx = n_non_range;
            for i in 0..prob.n_row {
                if row_is_range[i] {
                    orig_to_grb[i] = rn_idx;
                    rn_idx += 1;
                } else {
                    orig_to_grb[i] = nr_idx;
                    nr_idx += 1;
                }
            }
        }

        // Pre-size split CSR buffers. `*_cbeg` holds per-(Gurobi row) start
        // offsets into `*_cind/*_cval`. We fill them with prefix sums of
        // row_nnz so we can place each CSC entry directly at its row's
        // write cursor without Vec::extend overhead.
        let mut nr_cbeg: Vec<c_int> = Vec::with_capacity(n_non_range + 1);
        let mut nr_sense: Vec<c_char> = Vec::with_capacity(n_non_range);
        let mut nr_rhs: Vec<c_double> = Vec::with_capacity(n_non_range);
        let mut nr_names: Vec<*const c_char> = Vec::with_capacity(n_non_range);

        let mut rng_cbeg: Vec<c_int> = Vec::with_capacity(n_range + 1);
        let mut rng_lower: Vec<c_double> = Vec::with_capacity(n_range);
        let mut rng_upper: Vec<c_double> = Vec::with_capacity(n_range);
        let mut rng_names: Vec<*const c_char> = Vec::with_capacity(n_range);

        let mut nr_nnz_total: usize = 0;
        let mut rng_nnz_total: usize = 0;
        for i in 0..prob.n_row {
            let lb = prob.row_lower[i];
            let ub = prob.row_upper[i];
            let nnz = row_nnz[i] as usize;
            if row_is_range[i] {
                rng_cbeg.push(rng_nnz_total as c_int);
                rng_lower.push(lb);
                rng_upper.push(ub);
                rng_names.push(row_name_storage[i].as_ptr());
                rng_nnz_total += nnz;
            } else {
                nr_cbeg.push(nr_nnz_total as c_int);
                // Determine constraint sense. Guard against inf before the
                // equality test — (inf - lb) == inf, and inf <= inf is true,
                // which would falsely classify ">= lb" rows as equalities.
                let (sense, rhs) = if lb.is_finite()
                    && ub.is_finite()
                    && (ub - lb).abs() <= 1e-12 * ub.abs().max(1.0)
                {
                    (GRB_EQUAL, ub)
                } else if !ub.is_finite() || ub >= 1e29 {
                    (GRB_GREATER_EQUAL, lb)
                } else {
                    (GRB_LESS_EQUAL, ub)
                };
                nr_sense.push(sense);
                nr_rhs.push(rhs);
                nr_names.push(row_name_storage[i].as_ptr());
                nr_nnz_total += nnz;
            }
        }

        // Distribute CSC entries into the two split-CSR buffers in a
        // single pass. `*_pos[i]` is the write cursor for Gurobi-row i.
        let mut nr_cind: Vec<c_int> = vec![0; nr_nnz_total];
        let mut nr_cval: Vec<c_double> = vec![0.0; nr_nnz_total];
        let mut rng_cind: Vec<c_int> = vec![0; rng_nnz_total];
        let mut rng_cval: Vec<c_double> = vec![0.0; rng_nnz_total];
        let mut nr_pos: Vec<usize> = nr_cbeg.iter().map(|&p| p as usize).collect();
        let mut rng_pos: Vec<usize> = rng_cbeg.iter().map(|&p| p as usize).collect();
        for j in 0..prob.n_col {
            for k in prob.a_start[j] as usize..prob.a_start[j + 1] as usize {
                let ri = prob.a_index[k] as usize;
                let v = prob.a_value[k];
                if row_is_range[ri] {
                    // Write cursor indexes into rng_cind/cval at row's
                    // Gurobi index relative to the range block (so
                    // rng_pos[gurobi_idx - n_non_range]).
                    let gi = orig_to_grb[ri] - n_non_range;
                    let p = rng_pos[gi];
                    rng_cind[p] = j as c_int;
                    rng_cval[p] = v;
                    rng_pos[gi] = p + 1;
                } else {
                    let gi = orig_to_grb[ri];
                    let p = nr_pos[gi];
                    nr_cind[p] = j as c_int;
                    nr_cval[p] = v;
                    nr_pos[gi] = p + 1;
                }
            }
        }

        // Add non-range constraints.
        if n_non_range > 0 {
            let rc = GRBaddconstrs(
                model,
                n_non_range as c_int,
                nr_cind.len() as c_int,
                nr_cbeg.as_ptr(),
                nr_cind.as_ptr(),
                nr_cval.as_ptr(),
                nr_sense.as_ptr(),
                nr_rhs.as_ptr(),
                nr_names.as_ptr(),
            );
            if rc != 0 {
                return Err(format!("GRBaddconstrs failed (rc={rc})"));
            }
        }

        // Add range constraints.
        if n_range > 0 {
            let rc = GRBaddrangeconstrs(
                model,
                n_range as c_int,
                rng_cind.len() as c_int,
                rng_cbeg.as_ptr(),
                rng_cind.as_ptr(),
                rng_cval.as_ptr(),
                rng_lower.as_ptr(),
                rng_upper.as_ptr(),
                rng_names.as_ptr(),
            );
            if rc != 0 {
                return Err(format!("GRBaddrangeconstrs failed (rc={rc})"));
            }
        }

        // MIP progress callback disabled. The schedule-driven early-
        // termination path is dormant in this backend (see the
        // `FALLBACK_MIP_GAP_WHEN_UNSET` comment above). `MipTrace` is
        // still populated post-solve with model-size and node-count
        // telemetry, just without per-event progress samples.
        let mut callback_ctx: Option<Box<MipCallbackContext>> = None;

        // ── Solve ─────────────────────────────────────────────────────────────
        let solve_rc = GRBoptimize(model);
        if solve_rc != 0 {
            return Err(format!("GRBoptimize failed (rc={solve_rc})"));
        }

        // Detach the callback once the solve returns so the upcoming status
        // resolution (which may call `GRBoptimize` again for INF_OR_UNBD
        // retries) does not race against a context whose lifetime we're
        // about to end.
        if callback_ctx.is_some() {
            let rc = (lib.GRBsetcallbackfunc)(model, None, ptr::null_mut());
            debug_assert_eq!(rc, 0, "GRBsetcallbackfunc(None) rc={rc}");
        }

        // ── Solution status ───────────────────────────────────────────────────
        let (_stat, status) = resolve_gurobi_status(lib, env, model)?;

        if !matches!(status, LpSolveStatus::Optimal | LpSolveStatus::SubOptimal) {
            if matches!(status, LpSolveStatus::Infeasible) && gurobi_iis_export_requested() {
                let export = export_gurobi_iis(lib, model, prob, &orig_to_grb)?;
                return Err(format!(
                    "Gurobi: {status:?} (IIS exported to {} and {}; minimal={}, rows={}, bounds={})",
                    export.ilp_path.display(),
                    export.summary_path.display(),
                    export.minimal,
                    export.n_rows,
                    export.n_bounds
                ));
            }
            return Err(format!("Gurobi: {status:?}"));
        }

        // ── Extract solution ──────────────────────────────────────────────────
        let nc = prob.n_col as c_int;
        let nr = (n_non_range + n_range) as c_int;

        let mut x = vec![0.0f64; prob.n_col];
        let rc = GRBgetdblattrarray(model, cstr(ATTR_X), 0, nc, x.as_mut_ptr());
        if rc != 0 {
            return Err(format!("GRBgetdblattrarray(X) failed (rc={rc})"));
        }

        let mut objval: c_double = 0.0;
        GRBgetdblattr(model, cstr(ATTR_OBJVAL), &mut objval);

        // Iteration count: try IterCount (LP simplex), then BarIterCount (barrier).
        let mut iter_dbl: c_double = 0.0;
        let iterations =
            if GRBgetdblattr(model, cstr(ATTR_ITERCOUNT), &mut iter_dbl) == 0 && iter_dbl > 0.0 {
                iter_dbl as u32
            } else {
                let mut bar_iter: c_int = 0;
                GRBgetintattr(model, cstr(ATTR_BARITERCOUNT), &mut bar_iter);
                bar_iter as u32
            };

        // Duals and reduced costs are not meaningful for MIP.
        let (row_dual, col_dual) = if !is_mip && prob.n_row > 0 {
            // Get duals in Gurobi's ordering (non-range first, then range).
            let mut grb_pi = vec![0.0f64; n_non_range + n_range];
            GRBgetdblattrarray(model, cstr(ATTR_PI), 0, nr, grb_pi.as_mut_ptr());

            // Reorder to original row order and negate to standard Lagrange convention.
            // Gurobi Pi is d(obj)/d(RHS); negating gives the standard multiplier
            // (positive dual = tighter constraint increases objective).
            let row_dual: Vec<f64> = (0..prob.n_row).map(|i| -grb_pi[orig_to_grb[i]]).collect();

            let mut dj = vec![0.0f64; prob.n_col];
            GRBgetdblattrarray(model, cstr(ATTR_RC), 0, nc, dj.as_mut_ptr());

            (row_dual, dj)
        } else {
            (vec![0.0; prob.n_row], vec![0.0; prob.n_col])
        };

        // ── MIP trace + post-solve telemetry ─────────────────────────────────
        //
        // Emit a `MipTrace` on every MIP solve (not just schedule-driven
        // ones) so run-report.json always carries model-size, node-count,
        // and final-gap stats for post-mortem analysis. The schedule's
        // progress events are only present when the caller supplied a
        // `mip_gap_schedule`.
        let mip_trace = if is_mip {
            use crate::backends::{MipProgressMonitor, MipTerminationReason, MipTrace};

            // Original-model (pre-presolve) sizes.
            let read_int = |name: &[u8]| -> Option<u64> {
                let mut v: c_int = 0;
                if GRBgetintattr(model, cstr(name), &mut v) == 0 && v >= 0 {
                    Some(v as u64)
                } else {
                    None
                }
            };
            let read_dbl = |name: &[u8]| -> Option<f64> {
                let mut v: c_double = 0.0;
                if GRBgetdblattr(model, cstr(name), &mut v) == 0 {
                    Some(v)
                } else {
                    None
                }
            };
            let n_vars = read_int(ATTR_NUMVARS);
            let n_bin_vars = read_int(ATTR_NUMBINVARS);
            let n_int_vars = read_int(ATTR_NUMINTVARS);
            // Count binaries already pinned by equal bounds — tight
            // pre-fixes done by the model builder (must-run pins,
            // derate-forced-offs, min-up/min-down carryover, etc.).
            // Gurobi's presolve removes them in short order; the
            // delta against `n_bin_vars` shows how much branching
            // freedom the solver actually gets.
            let pre_fixed_bin_vars = prob.integrality.as_ref().map(|iv| {
                iv.iter()
                    .enumerate()
                    .filter(|(_, d)| matches!(d, crate::backends::VariableDomain::Binary))
                    .filter(|(i, _)| (prob.col_upper[*i] - prob.col_lower[*i]).abs() <= 1e-9)
                    .count() as u64
            });
            let n_rows = read_int(ATTR_NUMCONSTRS);
            let n_nonzeros = read_int(ATTR_NUMNZS);
            let node_count = read_dbl(ATTR_NODECOUNT).map(|v| v.max(0.0).round() as u64);
            let runtime_secs = read_dbl(ATTR_RUNTIME).unwrap_or(0.0);
            let obj_bound = read_dbl(ATTR_OBJBOUND).filter(|v| v.is_finite());
            let final_gap = obj_bound
                .filter(|_| objval.is_finite())
                .map(|bnd| MipProgressMonitor::relative_gap(objval, bnd));

            let (schedule, events, terminated_by) = if let Some(ctx) = callback_ctx.take() {
                let monitor = ctx.monitor;
                let terminated_by = if monitor.has_terminated() {
                    MipTerminationReason::ScheduleGap
                } else {
                    match status {
                        LpSolveStatus::Optimal => MipTerminationReason::Optimal,
                        LpSolveStatus::SubOptimal => {
                            if matches!(_stat, ffi::GRB_TIME_LIMIT) {
                                MipTerminationReason::TimeLimit
                            } else {
                                MipTerminationReason::Other
                            }
                        }
                        LpSolveStatus::Infeasible => MipTerminationReason::Infeasible,
                        _ => MipTerminationReason::Other,
                    }
                };
                let trace = monitor.into_trace(
                    opts.time_limit_secs,
                    terminated_by,
                    runtime_secs,
                    final_gap,
                );
                (trace.schedule, trace.events, trace.terminated_by)
            } else {
                let terminated_by = match status {
                    LpSolveStatus::Optimal => MipTerminationReason::Optimal,
                    LpSolveStatus::SubOptimal => {
                        if matches!(_stat, ffi::GRB_TIME_LIMIT) {
                            MipTerminationReason::TimeLimit
                        } else {
                            MipTerminationReason::Other
                        }
                    }
                    LpSolveStatus::Infeasible => MipTerminationReason::Infeasible,
                    _ => MipTerminationReason::Other,
                };
                (None, Vec::new(), terminated_by)
            };

            Some(MipTrace {
                schedule,
                time_limit_secs: opts.time_limit_secs,
                final_gap,
                final_time_secs: runtime_secs,
                terminated_by,
                events,
                n_vars,
                n_bin_vars,
                pre_fixed_bin_vars,
                n_int_vars,
                n_rows,
                n_nonzeros,
                node_count,
                iter_count: Some(iterations as u64),
                objective: objval.is_finite().then_some(objval),
                objective_bound: obj_bound,
            })
        } else {
            None
        };

        Ok(LpResult {
            x,
            row_dual,
            col_dual,
            objective: objval,
            status,
            iterations,
            mip_trace,
        })
    }

    // ═══════════════════════════════════════════════════════════════════════
    // GurobiQcqpSolver — QCQP/SOC constraints via GRBaddqconstr
    // ═══════════════════════════════════════════════════════════════════════

    /// Gurobi 13 QCQP solver (commercial license required).
    ///
    /// Solves convex QCQPs (quadratically-constrained quadratic programs)
    /// using `GRBaddqconstr` for SOC / rotated SOC constraints.
    /// Used by the SOCP-OPF formulation.
    pub struct GurobiQcqpSolver {
        lib: Arc<GurobiLib>,
    }

    impl std::fmt::Debug for GurobiQcqpSolver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("GurobiQcqpSolver").finish()
        }
    }

    impl GurobiQcqpSolver {
        /// Load Gurobi 13 at runtime and validate the license.
        pub fn new() -> Result<Self, String> {
            let lib = get_gurobi()?.clone();
            Ok(Self { lib })
        }
    }

    impl crate::backends::QcqpSolver for GurobiQcqpSolver {
        fn name(&self) -> &'static str {
            "Gurobi-QCQP"
        }

        fn solve(
            &self,
            prob: &crate::backends::QcqpProblem,
            opts: &crate::backends::LpOptions,
        ) -> Result<crate::backends::QcqpResult, String> {
            unsafe {
                with_env(&self.lib, |env| {
                    qcqp_solve_inner(&self.lib, env, prob, opts)
                })
            }
        }
    }

    unsafe fn qcqp_solve_inner(
        lib: &GurobiLib,
        env: *mut ffi::GRBenv,
        prob: &crate::backends::QcqpProblem,
        opts: &crate::backends::LpOptions,
    ) -> Result<crate::backends::QcqpResult, String> {
        use crate::backends::{LpSolveStatus, QcqpResult};
        use ffi::*;

        let base = &prob.base;

        // ── Configure environment parameters ─────────────────────────────────
        let print = c_int::from(opts.print_level > 0);
        (lib.GRBsetintparam)(env, cstr(PAR_OUTPUTFLAG), print);
        (lib.GRBsetintparam)(env, cstr(PAR_LOGTOCONSOLE), print);
        let tol = opts.tolerance.clamp(1e-10, 1e-4);
        (lib.GRBsetdblparam)(env, cstr(PAR_FEASIBILITYTOL), tol);
        (lib.GRBsetdblparam)(env, cstr(PAR_OPTIMALITYTOL), tol);
        if let Some(tl) = opts.time_limit_secs {
            (lib.GRBsetdblparam)(env, cstr(PAR_TIMELIMIT), tl);
        }

        // ── Create model ─────────────────────────────────────────────────────
        let name = CString::new("surge_qcqp").expect("static string contains no null bytes");
        let mut model: *mut GRBmodel = ptr::null_mut();
        let rc = (lib.GRBnewmodel)(
            env,
            &mut model,
            name.as_ptr(),
            0,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
        );
        if rc != 0 || model.is_null() {
            return Err(format!("GRBnewmodel failed (rc={rc})"));
        }

        struct ModelGuard(*mut GRBmodel, unsafe extern "C" fn(*mut GRBmodel) -> c_int);
        impl Drop for ModelGuard {
            fn drop(&mut self) {
                unsafe {
                    (self.1)(self.0);
                }
            }
        }
        let _guard = ModelGuard(model, lib.GRBfreemodel);

        // ── Add variables ────────────────────────────────────────────────────
        let vtypes: Vec<c_char> = if let Some(integ) = &base.integrality {
            integ.iter().map(|&v| gurobi_vtype(v)).collect()
        } else {
            vec![GRB_CONTINUOUS; base.n_col]
        };
        let rc = (lib.GRBaddvars)(
            model,
            base.n_col as c_int,
            0,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            base.col_cost.as_ptr(),
            base.col_lower.as_ptr(),
            base.col_upper.as_ptr(),
            vtypes.as_ptr(),
            ptr::null(),
        );
        if rc != 0 {
            return Err(format!("GRBaddvars failed (rc={rc})"));
        }

        // ── Quadratic objective (optional) ───────────────────────────────────
        if let (Some(qs), Some(qi), Some(qv)) = (&base.q_start, &base.q_index, &base.q_value) {
            let mut qrow = Vec::new();
            let mut qcol = Vec::new();
            let mut qval = Vec::new();
            for j in 0..base.n_col {
                for k in qs[j] as usize..qs[j + 1] as usize {
                    let i = qi[k] as usize;
                    let v = qv[k];
                    qrow.push(i as c_int);
                    qcol.push(j as c_int);
                    qval.push(if i == j { v / 2.0 } else { v });
                }
            }
            if !qval.is_empty() {
                let rc = (lib.GRBaddqpterms)(
                    model,
                    qval.len() as c_int,
                    qrow.as_ptr(),
                    qcol.as_ptr(),
                    qval.as_ptr(),
                );
                if rc != 0 {
                    return Err(format!("GRBaddqpterms failed (rc={rc})"));
                }
            }
        }

        // ── Linear constraints (CSC → CSR → GRBaddconstrs) ──────────────────
        if base.n_row > 0 {
            let (csr_start, csr_col, csr_val) = csc_to_csr(
                base.n_row,
                base.n_col,
                &base.a_start,
                &base.a_index,
                &base.a_value,
            );

            let mut cbeg: Vec<c_int> = Vec::with_capacity(base.n_row);
            let mut cind: Vec<c_int> = Vec::new();
            let mut cval: Vec<c_double> = Vec::new();
            let mut sense: Vec<c_char> = Vec::with_capacity(base.n_row);
            let mut rhs: Vec<c_double> = Vec::with_capacity(base.n_row);

            for i in 0..base.n_row {
                let lb = base.row_lower[i];
                let ub = base.row_upper[i];
                let rs = csr_start[i] as usize;
                let re = csr_start[i + 1] as usize;

                let mut push_row = |row_sense: c_char, row_rhs: c_double| {
                    cbeg.push(cind.len() as c_int);
                    cind.extend_from_slice(&csr_col[rs..re]);
                    cval.extend_from_slice(&csr_val[rs..re]);
                    sense.push(row_sense);
                    rhs.push(row_rhs);
                };

                if lb.is_finite() && ub.is_finite() && (ub - lb).abs() <= 1e-12 * ub.abs().max(1.0)
                {
                    push_row(GRB_EQUAL, ub);
                } else {
                    if lb.is_finite() && lb > -1e29 {
                        push_row(GRB_GREATER_EQUAL, lb);
                    }
                    if ub.is_finite() && ub < 1e29 {
                        push_row(GRB_LESS_EQUAL, ub);
                    }
                }
            }

            if !sense.is_empty() {
                let rc = (lib.GRBaddconstrs)(
                    model,
                    sense.len() as c_int,
                    cind.len() as c_int,
                    cbeg.as_ptr(),
                    cind.as_ptr(),
                    cval.as_ptr(),
                    sense.as_ptr(),
                    rhs.as_ptr(),
                    ptr::null(),
                );
                if rc != 0 {
                    return Err(format!("GRBaddconstrs failed (rc={rc})"));
                }
            }
        }

        // ── Quadratic constraints (SOC) ──────────────────────────────────────
        for (idx, qc) in prob.quad_constraints.iter().enumerate() {
            let numlnz = qc.lin_idx.len() as c_int;
            let numqnz = qc.q_val.len() as c_int;

            let lind_ptr = if qc.lin_idx.is_empty() {
                ptr::null()
            } else {
                qc.lin_idx.as_ptr()
            };
            let lval_ptr = if qc.lin_val.is_empty() {
                ptr::null()
            } else {
                qc.lin_val.as_ptr()
            };
            let qrow_ptr = if qc.q_row.is_empty() {
                ptr::null()
            } else {
                qc.q_row.as_ptr()
            };
            let qcol_ptr = if qc.q_col.is_empty() {
                ptr::null()
            } else {
                qc.q_col.as_ptr()
            };
            let qval_ptr = if qc.q_val.is_empty() {
                ptr::null()
            } else {
                qc.q_val.as_ptr()
            };

            let grb_sense = match qc.sense {
                b'L' => b'<' as c_char,
                b'G' => b'>' as c_char,
                b'E' => b'=' as c_char,
                _ => qc.sense as c_char,
            };

            let rc = (lib.GRBaddqconstr)(
                model,
                numlnz,
                lind_ptr,
                lval_ptr,
                numqnz,
                qrow_ptr,
                qcol_ptr,
                qval_ptr,
                grb_sense,
                qc.rhs,
                ptr::null(), // anonymous constraint
            );
            if rc != 0 {
                return Err(format!(
                    "GRBaddqconstr failed (rc={rc}) for quadratic constraint {idx}"
                ));
            }
        }

        // ── Solve ────────────────────────────────────────────────────────────
        let solve_rc = (lib.GRBoptimize)(model);
        if solve_rc != 0 {
            return Err(format!("GRBoptimize (QCQP) failed (rc={solve_rc})"));
        }

        // ── Check status ─────────────────────────────────────────────────────
        let mut stat: c_int = 0;
        (lib.GRBgetintattr)(model, cstr(ATTR_STATUS), &mut stat);
        let solution_count = if stat == GRB_TIME_LIMIT {
            let mut count: c_int = 0;
            let rc = (lib.GRBgetintattr)(model, cstr(ATTR_SOLCOUNT), &mut count);
            if rc != 0 {
                return Err(gurobi_call_error(
                    "GRBgetintattr(SolCount)",
                    rc,
                    gurobi_model_error_message(lib, model).as_deref(),
                ));
            }
            Some(count)
        } else {
            None
        };
        let status = gurobi_status_from_code(stat, solution_count);

        if !matches!(status, LpSolveStatus::Optimal | LpSolveStatus::SubOptimal) {
            return Err(format!("Gurobi QCQP: {status:?}"));
        }

        // ── Extract solution ─────────────────────────────────────────────────
        let mut x = vec![0.0f64; base.n_col];
        let rc =
            (lib.GRBgetdblattrarray)(model, cstr(ATTR_X), 0, base.n_col as c_int, x.as_mut_ptr());
        if rc != 0 {
            return Err(format!("GRBgetdblattrarray(X) failed (rc={rc})"));
        }

        let mut objval: c_double = 0.0;
        (lib.GRBgetdblattr)(model, cstr(ATTR_OBJVAL), &mut objval);

        Ok(QcqpResult {
            x,
            objective: objval,
            status,
        })
    }

    // ═══════════════════════════════════════════════════════════════════════
    // GurobiNlpSolver — native AC-OPF using GRBaddgenconstrNL expression trees
    // ═══════════════════════════════════════════════════════════════════════

    /// Flat expression tree for a single `GRBaddgenconstrNL` call.
    ///
    /// Nodes are ordered so that each binary operator (PLUS, MINUS, MULTIPLY)
    /// appears **before** its children — the child with the lower array index
    /// is treated as the left operand for non-commutative operators (MINUS).
    struct NlTree {
        opcode: Vec<c_int>,
        data: Vec<f64>,
        parent: Vec<c_int>,
    }

    impl NlTree {
        fn new() -> Self {
            Self {
                opcode: Vec::new(),
                data: Vec::new(),
                parent: Vec::new(),
            }
        }

        fn len(&self) -> usize {
            self.opcode.len()
        }

        /// Push one node; returns its index.
        fn push(&mut self, opcode: c_int, data: f64, parent: c_int) -> usize {
            let idx = self.len();
            self.opcode.push(opcode);
            self.data.push(data);
            self.parent.push(parent);
            idx
        }

        fn constant(&mut self, v: f64, parent: c_int) -> usize {
            self.push(ffi::OPCODE_CONSTANT, v, parent)
        }
        fn variable(&mut self, vi: usize, parent: c_int) -> usize {
            self.push(ffi::OPCODE_VARIABLE, vi as f64, parent)
        }
        fn plus_op(&mut self, parent: c_int) -> usize {
            self.push(ffi::OPCODE_PLUS, -1.0, parent)
        }
        fn minus_op(&mut self, parent: c_int) -> usize {
            self.push(ffi::OPCODE_MINUS, -1.0, parent)
        }
        fn mul_op(&mut self, parent: c_int) -> usize {
            self.push(ffi::OPCODE_MULTIPLY, -1.0, parent)
        }
        fn sin_op(&mut self, parent: c_int) -> usize {
            self.push(ffi::OPCODE_SIN, -1.0, parent)
        }
        fn cos_op(&mut self, parent: c_int) -> usize {
            self.push(ffi::OPCODE_COS, -1.0, parent)
        }
        fn square_op(&mut self, parent: c_int) -> usize {
            self.push(ffi::OPCODE_SQUARE, -1.0, parent)
        }
        fn uminus_op(&mut self, parent: c_int) -> usize {
            self.push(ffi::OPCODE_UMINUS, -1.0, parent)
        }

        /// Build a LEFT-associative PLUS chain over `n` terms.
        ///
        /// Returns the root index.  Each term is built by calling
        /// `build_term(tree, term_idx, parent)` with the appropriate parent.
        /// Terms are indexed 0 .. n-1; the LAST term is the rightmost leaf.
        fn left_sum<F>(t: &mut NlTree, n: usize, build: &F, parent: c_int) -> usize
        where
            F: Fn(&mut NlTree, usize, c_int) -> usize,
        {
            match n {
                0 => t.constant(0.0, parent),
                1 => build(t, 0, parent),
                _ => {
                    let plus = t.plus_op(parent) as c_int;
                    NlTree::left_sum(t, n - 1, build, plus); // left subtree
                    build(t, n - 1, plus); // rightmost leaf
                    plus as usize
                }
            }
        }
    }

    struct GrbFlowExprEntry {
        adm: BranchAdmittance,
        coeff: f64,
    }

    #[derive(Default)]
    struct GrbFlowExprData {
        entries: Vec<GrbFlowExprEntry>,
    }

    // ── Variable layout helper ─────────────────────────────────────────────

    /// Variable index map for the Gurobi NLP model.
    ///
    /// Layout: `Va(n-1) | Vm(n) | Pg(ng) | Qg(ng) | pres(n) | qres(n) |
    ///          pft(nb) | qft(nb) | ptf(nb) | qtf(nb) | sft(nb) | stf(nb) |
    ///          ang(na) | fg(n_fg) | iface(n_iface)`
    ///
    /// The first `n_base` variables match the [`AcOpfMapping`] layout exactly
    /// so the returned `NlpSolution.x[0..n_base]` can be unpacked by the
    /// existing post-processing code in `solve_ac_opf`.
    #[allow(dead_code)] // Fields stored for debugging/future use; offsets are the hot path.
    struct GrbVarMap {
        n_bus: usize,
        slack_idx: usize,
        n_gen: usize,
        n_br_con: usize,
        n_ang: usize,
        n_fg: usize,
        n_iface: usize,
        // offsets — first `n_base` match AcOpfMapping
        va_off: usize, // 0
        vm_off: usize, // n_va = n_bus-1
        pg_off: usize, // vm_off + n_bus
        qg_off: usize, // pg_off + n_gen
        // auxiliary variables for the NL formulation
        pres_off: usize,  // qg_off + n_gen   (free: P-calc result)
        qres_off: usize,  // pres_off + n_bus  (free: Q-calc result)
        pft_off: usize,   // qres_off + n_bus  (free: from-side P flow)
        qft_off: usize,   // pft_off  + n_br_con
        ptf_off: usize,   // qft_off  + n_br_con
        qtf_off: usize,   // ptf_off  + n_br_con
        sft_off: usize,   // qtf_off  + n_br_con  (0 ≤ sft ≤ Smax²)
        stf_off: usize,   // sft_off  + n_br_con  (0 ≤ stf ≤ Smax²)
        ang_off: usize,   // stf_off  + n_br_con  (angmin ≤ ang ≤ angmax)
        fg_off: usize,    // ang_off  + n_ang     (-rev ≤ fg ≤ fwd)
        iface_off: usize, // fg_off + n_fg       (-rev ≤ iface ≤ fwd)
        n_var: usize,     // iface_off + n_iface
        n_base: usize,    // qg_off   + n_gen   (AcOpfMapping variables)
    }

    impl GrbVarMap {
        fn new(
            n_bus: usize,
            slack_idx: usize,
            n_gen: usize,
            n_br_con: usize,
            n_ang: usize,
            n_fg: usize,
            n_iface: usize,
        ) -> Self {
            let n_va = n_bus - 1;
            let va_off = 0;
            let vm_off = n_va;
            let pg_off = vm_off + n_bus;
            let qg_off = pg_off + n_gen;
            let n_base = qg_off + n_gen;
            let pres_off = n_base;
            let qres_off = pres_off + n_bus;
            let pft_off = qres_off + n_bus;
            let qft_off = pft_off + n_br_con;
            let ptf_off = qft_off + n_br_con;
            let qtf_off = ptf_off + n_br_con;
            let sft_off = qtf_off + n_br_con;
            let stf_off = sft_off + n_br_con;
            let ang_off = stf_off + n_br_con;
            let fg_off = ang_off + n_ang;
            let iface_off = fg_off + n_fg;
            let n_var = iface_off + n_iface;
            Self {
                n_bus,
                slack_idx,
                n_gen,
                n_br_con,
                n_ang,
                n_fg,
                n_iface,
                va_off,
                vm_off,
                pg_off,
                qg_off,
                pres_off,
                qres_off,
                pft_off,
                qft_off,
                ptf_off,
                qtf_off,
                sft_off,
                stf_off,
                ang_off,
                fg_off,
                iface_off,
                n_var,
                n_base,
            }
        }

        /// Variable index for Va[bus], or `None` for slack (angle=0 fixed).
        #[inline]
        fn va(&self, bus: usize) -> Option<usize> {
            if bus == self.slack_idx {
                None
            } else if bus < self.slack_idx {
                Some(self.va_off + bus)
            } else {
                Some(self.va_off + bus - 1)
            }
        }
        #[inline]
        fn vm(&self, bus: usize) -> usize {
            self.vm_off + bus
        }
        #[inline]
        fn pg(&self, j: usize) -> usize {
            self.pg_off + j
        }
        #[inline]
        fn qg(&self, j: usize) -> usize {
            self.qg_off + j
        }
        #[inline]
        fn pres(&self, i: usize) -> usize {
            self.pres_off + i
        }
        #[inline]
        fn qres(&self, i: usize) -> usize {
            self.qres_off + i
        }
        #[inline]
        fn pft(&self, k: usize) -> usize {
            self.pft_off + k
        }
        #[inline]
        fn qft(&self, k: usize) -> usize {
            self.qft_off + k
        }
        #[inline]
        fn ptf(&self, k: usize) -> usize {
            self.ptf_off + k
        }
        #[inline]
        fn qtf(&self, k: usize) -> usize {
            self.qtf_off + k
        }
        #[inline]
        fn sft(&self, k: usize) -> usize {
            self.sft_off + k
        }
        #[inline]
        fn stf(&self, k: usize) -> usize {
            self.stf_off + k
        }
        #[inline]
        fn ang(&self, k: usize) -> usize {
            self.ang_off + k
        }
        #[inline]
        fn fg(&self, k: usize) -> usize {
            self.fg_off + k
        }
        #[inline]
        fn iface(&self, k: usize) -> usize {
            self.iface_off + k
        }
    }

    // ── Tree-building helpers ─────────────────────────────────────────────

    /// Push `Va[from] - Va[to]` (angle difference), handling slack as 0.
    fn push_theta(t: &mut NlTree, from: usize, to: usize, vm: &GrbVarMap, parent: c_int) -> usize {
        match (vm.va(from), vm.va(to)) {
            (None, None) => t.constant(0.0, parent),
            (None, Some(vt)) => {
                // 0 - Va[to] = -Va[to]
                let u = t.uminus_op(parent) as c_int;
                t.variable(vt, u);
                u as usize
            }
            (Some(vf), None) => t.variable(vf, parent),
            (Some(vf), Some(vt)) => {
                let m = t.minus_op(parent) as c_int;
                t.variable(vf, m); // left = minuend
                t.variable(vt, m); // right = subtrahend
                m as usize
            }
        }
    }

    /// Push `G*cos(θ_from_to) + B*sin(θ_from_to)`.
    fn push_gcos_bsin(
        t: &mut NlTree,
        g: f64,
        b: f64,
        from: usize,
        to: usize,
        vm: &GrbVarMap,
        parent: c_int,
    ) -> usize {
        if b == 0.0 {
            let m = t.mul_op(parent) as c_int;
            t.constant(g, m);
            let c = t.cos_op(m) as c_int;
            push_theta(t, from, to, vm, c);
            m as usize
        } else if g == 0.0 {
            let m = t.mul_op(parent) as c_int;
            t.constant(b, m);
            let s = t.sin_op(m) as c_int;
            push_theta(t, from, to, vm, s);
            m as usize
        } else {
            let p = t.plus_op(parent) as c_int;
            let mg = t.mul_op(p) as c_int;
            t.constant(g, mg);
            let c = t.cos_op(mg) as c_int;
            push_theta(t, from, to, vm, c);
            let mb = t.mul_op(p) as c_int;
            t.constant(b, mb);
            let s = t.sin_op(mb) as c_int;
            push_theta(t, from, to, vm, s);
            p as usize
        }
    }

    /// Push `G*sin(θ_from_to) - B*cos(θ_from_to)`.
    fn push_gsin_mcos(
        t: &mut NlTree,
        g: f64,
        b: f64,
        from: usize,
        to: usize,
        vm: &GrbVarMap,
        parent: c_int,
    ) -> usize {
        if b == 0.0 {
            let m = t.mul_op(parent) as c_int;
            t.constant(g, m);
            let s = t.sin_op(m) as c_int;
            push_theta(t, from, to, vm, s);
            m as usize
        } else if g == 0.0 {
            // -B * cos(θ)
            let m = t.mul_op(parent) as c_int;
            t.constant(-b, m);
            let c = t.cos_op(m) as c_int;
            push_theta(t, from, to, vm, c);
            m as usize
        } else {
            let mn = t.minus_op(parent) as c_int;
            let mg = t.mul_op(mn) as c_int;
            t.constant(g, mg);
            let s = t.sin_op(mg) as c_int;
            push_theta(t, from, to, vm, s);
            let mb = t.mul_op(mn) as c_int;
            t.constant(b, mb);
            let c = t.cos_op(mb) as c_int;
            push_theta(t, from, to, vm, c);
            mn as usize
        }
    }

    /// Push `Vm[i] * Vm[j] * (G_ij * cos(θ) + B_ij * sin(θ))`.
    fn push_p_off_diag(
        t: &mut NlTree,
        g: f64,
        b: f64,
        bus_i: usize,
        bus_j: usize,
        vm_map: &GrbVarMap,
        parent: c_int,
    ) -> usize {
        let outer = t.mul_op(parent) as c_int;
        let inner = t.mul_op(outer) as c_int;
        t.variable(vm_map.vm(bus_i), inner);
        t.variable(vm_map.vm(bus_j), inner);
        push_gcos_bsin(t, g, b, bus_i, bus_j, vm_map, outer);
        outer as usize
    }

    /// Push `Vm[i] * Vm[j] * (G_ij * sin(θ) - B_ij * cos(θ))`.
    fn push_q_off_diag(
        t: &mut NlTree,
        g: f64,
        b: f64,
        bus_i: usize,
        bus_j: usize,
        vm_map: &GrbVarMap,
        parent: c_int,
    ) -> usize {
        let outer = t.mul_op(parent) as c_int;
        let inner = t.mul_op(outer) as c_int;
        t.variable(vm_map.vm(bus_i), inner);
        t.variable(vm_map.vm(bus_j), inner);
        push_gsin_mcos(t, g, b, bus_i, bus_j, vm_map, outer);
        outer as usize
    }

    /// Build the expression tree `pres[i] = P_calc(i)`.
    ///
    /// `P_calc(i) = Σ_{j: Y_ij≠0} injection_ij`
    /// where injection_ii = G_ii * Vm_i²  and  injection_ij = Vm_i*Vm_j*(…).
    fn build_p_calc_tree(
        t: &mut NlTree,
        bus: usize,
        ybus: &surge_ac::matrix::ybus::YBus,
        vm_map: &GrbVarMap,
    ) {
        let row = ybus.row(bus);
        let n = row.col_idx.len();

        // Collect (bus_j, g_ij, b_ij) — the Y-bus nonzero entries in this row.
        // For diagonal (j == bus): term = G_ii * Vm_i²
        // For off-diag (j ≠ bus):  term = Vm_i * Vm_j * (G_ij*cos(θ) + B_ij*sin(θ))
        let build_term = |t: &mut NlTree, k: usize, parent: c_int| -> usize {
            let j = row.col_idx[k];
            let g = row.g[k];
            let b = row.b[k];
            if j == bus {
                // G_ii * Vm_i²
                let m = t.mul_op(parent) as c_int;
                t.constant(g, m);
                let sq = t.square_op(m) as c_int;
                t.variable(vm_map.vm(bus), sq);
                m as usize
            } else {
                push_p_off_diag(t, g, b, bus, j, vm_map, parent)
            }
        };

        // Root is -1 (this tree is submitted directly as pres[i]'s NL constraint).
        NlTree::left_sum(t, n, &build_term, -1);
    }

    /// Build the expression tree `qres[i] = Q_calc(i)`.
    ///
    /// `Q_calc(i) = G_ii*0 - B_ii*Vm_i² + Σ_{j≠i} Vm_i*Vm_j*(G_ij*sin(θ) - B_ij*cos(θ))`
    fn build_q_calc_tree(
        t: &mut NlTree,
        bus: usize,
        ybus: &surge_ac::matrix::ybus::YBus,
        vm_map: &GrbVarMap,
    ) {
        let row = ybus.row(bus);
        let n = row.col_idx.len();

        let build_term = |t: &mut NlTree, k: usize, parent: c_int| -> usize {
            let j = row.col_idx[k];
            let g = row.g[k];
            let b = row.b[k];
            if j == bus {
                // -B_ii * Vm_i²
                let m = t.mul_op(parent) as c_int;
                t.constant(-b, m);
                let sq = t.square_op(m) as c_int;
                t.variable(vm_map.vm(bus), sq);
                m as usize
            } else {
                push_q_off_diag(t, g, b, bus, j, vm_map, parent)
            }
        };

        NlTree::left_sum(t, n, &build_term, -1);
    }

    /// Build `pft[k] = gff*Vf² + Vf*Vt*(gft*cos(θ_ft) + bft*sin(θ_ft))`.
    fn build_pft_tree(t: &mut NlTree, ba: &BranchAdmittance, vm_map: &GrbVarMap) {
        let (f, to) = (ba.from, ba.to);
        // Two terms: self + off-diag
        let p = t.plus_op(-1) as c_int;
        // Self: gff * Vf²
        let ms = t.mul_op(p) as c_int;
        t.constant(ba.g_ff, ms);
        let sq = t.square_op(ms) as c_int;
        t.variable(vm_map.vm(f), sq);
        // Off-diag: Vf * Vt * (gft*cos + bft*sin)
        push_p_off_diag(t, ba.g_ft, ba.b_ft, f, to, vm_map, p);
    }

    /// Build `qft[k] = -bff*Vf² + Vf*Vt*(gft*sin(θ_ft) - bft*cos(θ_ft))`.
    fn build_qft_tree(t: &mut NlTree, ba: &BranchAdmittance, vm_map: &GrbVarMap) {
        let (f, to) = (ba.from, ba.to);
        let p = t.plus_op(-1) as c_int;
        let ms = t.mul_op(p) as c_int;
        t.constant(-ba.b_ff, ms);
        let sq = t.square_op(ms) as c_int;
        t.variable(vm_map.vm(f), sq);
        push_q_off_diag(t, ba.g_ft, ba.b_ft, f, to, vm_map, p);
    }

    /// Build `ptf[k] = gtt*Vt² + Vt*Vf*(gtf*cos(θ_tf) + btf*sin(θ_tf))`.
    fn build_ptf_tree(t: &mut NlTree, ba: &BranchAdmittance, vm_map: &GrbVarMap) {
        let (f, to) = (ba.from, ba.to);
        let p = t.plus_op(-1) as c_int;
        let ms = t.mul_op(p) as c_int;
        t.constant(ba.g_tt, ms);
        let sq = t.square_op(ms) as c_int;
        t.variable(vm_map.vm(to), sq);
        // Note reversed buses: to→from for gtf/btf, theta = Va[to] - Va[from]
        push_p_off_diag(t, ba.g_tf, ba.b_tf, to, f, vm_map, p);
    }

    /// Build `qtf[k] = -btt*Vt² + Vt*Vf*(gtf*sin(θ_tf) - btf*cos(θ_tf))`.
    fn build_qtf_tree(t: &mut NlTree, ba: &BranchAdmittance, vm_map: &GrbVarMap) {
        let (f, to) = (ba.from, ba.to);
        let p = t.plus_op(-1) as c_int;
        let ms = t.mul_op(p) as c_int;
        t.constant(-ba.b_tt, ms);
        let sq = t.square_op(ms) as c_int;
        t.variable(vm_map.vm(to), sq);
        push_q_off_diag(t, ba.g_tf, ba.b_tf, to, f, vm_map, p);
    }

    /// Build `sft[k] = pft[k]² + qft[k]²`  (3 nodes).
    fn build_s_sq_tree(t: &mut NlTree, p_var: usize, q_var: usize) {
        let p = t.plus_op(-1) as c_int;
        let sq1 = t.square_op(p) as c_int;
        t.variable(p_var, sq1);
        let sq2 = t.square_op(p) as c_int;
        t.variable(q_var, sq2);
    }

    /// Push the from-side branch real-power expression under `parent`.
    fn push_pft_expr(t: &mut NlTree, ba: &BranchAdmittance, vm_map: &GrbVarMap, parent: c_int) {
        let (f, to) = (ba.from, ba.to);
        let p = t.plus_op(parent) as c_int;
        let ms = t.mul_op(p) as c_int;
        t.constant(ba.g_ff, ms);
        let sq = t.square_op(ms) as c_int;
        t.variable(vm_map.vm(f), sq);
        push_p_off_diag(t, ba.g_ft, ba.b_ft, f, to, vm_map, p);
    }

    fn build_weighted_flow_tree(t: &mut NlTree, expr: &GrbFlowExprData, vm_map: &GrbVarMap) {
        let build_term = |tree: &mut NlTree, idx: usize, parent: c_int| -> usize {
            let entry = &expr.entries[idx];
            if (entry.coeff - 1.0).abs() <= 1e-12 {
                push_pft_expr(tree, &entry.adm, vm_map, parent);
                parent as usize
            } else {
                let m = tree.mul_op(parent) as c_int;
                tree.constant(entry.coeff, m);
                push_pft_expr(tree, &entry.adm, vm_map, m);
                m as usize
            }
        };
        NlTree::left_sum(t, expr.entries.len(), &build_term, -1);
    }

    // ── Add one NL genconstr ──────────────────────────────────────────────

    /// Submit one NL general constraint: `x[resvar] = tree_expression`.
    unsafe fn add_genconstr_nl(
        lib: &GurobiLib,
        model: *mut ffi::GRBmodel,
        name: &std::ffi::CStr,
        resvar: usize,
        tree: &NlTree,
    ) -> Result<(), String> {
        let rc = (lib.GRBaddgenconstrNL)(
            model,
            name.as_ptr(),
            resvar as c_int,
            tree.len() as c_int,
            tree.opcode.as_ptr(),
            tree.data.as_ptr(),
            tree.parent.as_ptr(),
        );
        if rc != 0 {
            Err(format!(
                "GRBaddgenconstrNL('{}') failed rc={rc}",
                name.to_string_lossy()
            ))
        } else {
            Ok(())
        }
    }

    // ── GurobiNlpSolver ───────────────────────────────────────────────────

    /// Gurobi 13 native AC-OPF solver using `GRBaddgenconstrNL` expression trees.
    ///
    /// Dispatched from [`crate::ac::solve::solve_ac_opf`] when the selected NLP
    /// solver is `GurobiNlpSolver`.  The `NlpSolver::solve()` impl falls back
    /// to an error — Gurobi NLP is only used through the native dispatch path.
    pub struct GurobiNlpSolver {
        lib: Arc<GurobiLib>,
    }

    impl std::fmt::Debug for GurobiNlpSolver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("GurobiNlpSolver").finish()
        }
    }

    impl GurobiNlpSolver {
        pub fn new() -> Result<Self, String> {
            let lib = get_gurobi()?.clone();
            Ok(Self { lib })
        }

        pub fn new_validated() -> Result<Self, String> {
            let solver = Self::new()?;
            solver.validate_runtime()?;
            Ok(solver)
        }

        pub fn validate_runtime(&self) -> Result<(), String> {
            unsafe { with_env(&self.lib, |_env| Ok(())) }
        }
    }

    impl crate::backends::NlpSolver for GurobiNlpSolver {
        fn name(&self) -> &'static str {
            "Gurobi-NLP"
        }

        fn version(&self) -> &'static str {
            "13.0"
        }

        fn as_any(&self) -> Option<&dyn std::any::Any> {
            Some(self)
        }

        /// The Gurobi NLP solver is dispatched natively from `solve_ac_opf`
        /// via `as_any()` downcast before this trait method is called.
        /// Calling `solve()` directly means we weren't dispatched natively —
        /// return an error rather than silently fall back.
        fn solve(
            &self,
            _problem: &dyn crate::nlp::NlpProblem,
            _opts: &crate::nlp::NlpOptions,
        ) -> Result<crate::nlp::NlpSolution, String> {
            Err("GurobiNlpSolver::solve() called directly — \
                 use solve_ac_opf() with solver='gurobi' for native dispatch"
                .to_string())
        }
    }

    // ── Native AC-OPF solve ───────────────────────────────────────────────

    impl GurobiNlpSolver {
        /// Solve AC-OPF natively using Gurobi 13 expression-tree NL constraints.
        ///
        /// Returns a full [`OpfSolution`] (same as the Ipopt path).  Called
        /// from `solve_ac_opf` via `as_any()` downcast when `solver="gurobi"`.
        pub(crate) fn solve_native_ac_opf(
            &self,
            network: &surge_network::Network,
            options: &crate::ac::types::AcOpfOptions,
            context: &crate::ac::types::AcOpfRunContext,
            dc_opf_angles: Option<&[f64]>,
        ) -> Result<surge_solution::OpfSolution, String> {
            unsafe {
                with_env(&self.lib, |env| {
                    self.solve_inner(env, network, options, context, dc_opf_angles)
                })
            }
        }

        unsafe fn solve_inner(
            &self,
            env: *mut ffi::GRBenv,
            network: &surge_network::Network,
            options: &crate::ac::types::AcOpfOptions,
            context: &crate::ac::types::AcOpfRunContext,
            dc_opf_angles: Option<&[f64]>,
        ) -> Result<surge_solution::OpfSolution, String> {
            use ffi::*;
            use std::ffi::CString;
            // Bind lib function pointers to local names matching the original C API.
            #[allow(non_snake_case)]
            let (
                GRBsetintparam,
                GRBsetdblparam,
                GRBnewmodel,
                GRBfreemodel,
                GRBaddvars,
                GRBaddqpterms,
                GRBaddconstrs,
                GRBsetintattr,
                _GRBaddgenconstrNL,
                GRBoptimize,
                GRBgetintattr,
                GRBgetdblattr,
                GRBgetdblattrarray,
                GRBsetdblattrarray,
                _GRBupdatemodel,
            ) = (
                self.lib.GRBsetintparam,
                self.lib.GRBsetdblparam,
                self.lib.GRBnewmodel,
                self.lib.GRBfreemodel,
                self.lib.GRBaddvars,
                self.lib.GRBaddqpterms,
                self.lib.GRBaddconstrs,
                self.lib.GRBsetintattr,
                self.lib.GRBaddgenconstrNL,
                self.lib.GRBoptimize,
                self.lib.GRBgetintattr,
                self.lib.GRBgetdblattr,
                self.lib.GRBgetdblattrarray,
                self.lib.GRBsetdblattrarray,
                self.lib.GRBupdatemodel,
            );

            let base = network.base_mva;
            let net_context = OpfNetworkContext::for_ac(network).map_err(|e| e.to_string())?;
            let n_bus = net_context.n_bus;
            let bus_map = net_context.bus_map.clone();
            let branch_idx_map = net_context.branch_idx_map.clone();
            let bus_pd_mw = network.bus_load_p_mw();
            let bus_qd_mvar = network.bus_load_q_mvar();

            let slack_idx = net_context.slack_idx;
            let gen_indices = net_context.gen_indices.clone();
            let n_gen = gen_indices.len();
            if n_gen == 0 {
                return Err("no in-service generators".to_string());
            }
            let bus_gen_map = net_context.bus_gen_map.clone();

            // ── Constrained branches ──────────────────────────────────────
            let constrained: Vec<usize> = if options.enforce_thermal_limits {
                network
                    .branches
                    .iter()
                    .enumerate()
                    .filter(|(_, br)| br.in_service && br.rating_a_mva >= options.min_rate_a)
                    .map(|(i, _)| i)
                    .collect()
            } else {
                vec![]
            };
            let n_br_con = constrained.len();

            // ── Y-bus ─────────────────────────────────────────────────────
            let ybus = surge_ac::matrix::ybus::build_ybus(network);

            // ── Branch admittances ────────────────────────────────────────
            let branch_adm: Vec<BranchAdmittance> =
                build_branch_admittances(network, &constrained, &bus_map);

            // ── Angle-difference, flowgate, and interface constraints ─────
            const ANG_LO: f64 = -std::f64::consts::PI;
            const ANG_HI: f64 = std::f64::consts::PI;
            let angle_constraints: Vec<(usize, usize, usize, f64, f64)> =
                if options.enforce_angle_limits {
                    network
                        .branches
                        .iter()
                        .enumerate()
                        .filter_map(|(br_idx, br)| {
                            if !br.in_service {
                                return None;
                            }
                            let lo = br.angle_diff_min_rad.unwrap_or(f64::NEG_INFINITY);
                            let hi = br.angle_diff_max_rad.unwrap_or(f64::INFINITY);
                            if lo <= ANG_LO && hi >= ANG_HI {
                                return None;
                            }
                            Some((br_idx, bus_map[&br.from_bus], bus_map[&br.to_bus], lo, hi))
                        })
                        .collect()
                } else {
                    vec![]
                };

            let build_flow_expr_data =
                |branches: &[surge_network::network::WeightedBranchRef]| -> GrbFlowExprData {
                    let mut entries = Vec::with_capacity(branches.len());
                    for member in branches {
                        let coeff = member.coefficient;
                        let Some(&br_idx) = branch_idx_map.get(&(
                            member.branch.from_bus,
                            member.branch.to_bus,
                            member.branch.circuit.clone(),
                        )) else {
                            continue;
                        };
                        let br = &network.branches[br_idx];
                        if !br.in_service {
                            continue;
                        }
                        let from = bus_map[&br.from_bus];
                        let to = bus_map[&br.to_bus];
                        let adm = compute_branch_admittance(br, from, to, base);
                        entries.push(GrbFlowExprEntry { adm, coeff });
                    }
                    GrbFlowExprData { entries }
                };

            let flowgate_indices: Vec<usize> = if options.enforce_flowgates {
                network
                    .flowgates
                    .iter()
                    .enumerate()
                    .filter(|(_, fg)| fg.in_service)
                    .map(|(i, _)| i)
                    .collect()
            } else {
                vec![]
            };
            let interface_indices: Vec<usize> = if options.enforce_flowgates {
                network
                    .interfaces
                    .iter()
                    .enumerate()
                    .filter(|(_, iface)| iface.in_service && iface.limit_forward_mw > 0.0)
                    .map(|(i, _)| i)
                    .collect()
            } else {
                vec![]
            };
            let flowgate_data: Vec<GrbFlowExprData> = flowgate_indices
                .iter()
                .map(|&fgi| {
                    let fg = &network.flowgates[fgi];
                    build_flow_expr_data(&fg.monitored)
                })
                .collect();
            let interface_data: Vec<GrbFlowExprData> = interface_indices
                .iter()
                .map(|&ifi| {
                    let iface = &network.interfaces[ifi];
                    build_flow_expr_data(&iface.members)
                })
                .collect();
            let n_ang = angle_constraints.len();
            let n_fg = flowgate_indices.len();
            let n_iface = interface_indices.len();

            // ── Variable map ──────────────────────────────────────────────
            let vm = GrbVarMap::new(n_bus, slack_idx, n_gen, n_br_con, n_ang, n_fg, n_iface);
            let n_var = vm.n_var;
            let _n_va = n_bus - 1;

            // ── Cost constants (c0 terms accumulate outside objective) ────
            let mut cost_const = 0.0_f64;

            // ── Parameters (set on env BEFORE GRBnewmodel so model inherits) ─
            let verbose = c_int::from(options.print_level > 0);
            GRBsetintparam(env, cstr(PAR_OUTPUTFLAG), verbose);
            GRBsetintparam(env, cstr(PAR_LOGTOCONSOLE), verbose);
            GRBsetintparam(env, cstr(PAR_NONCONVEX), 2);
            // OptimalityTarget=1: local NLP barrier (Gurobi 13 interior-point).
            // Without this Gurobi defaults to global spatial B&B (100x slower).
            GRBsetintparam(env, cstr(PAR_OPTIMALITYTARGET), 1);
            // Presolve=0: Gurobi presolve can incorrectly substitute result-variables
            // in GRBaddgenconstrNL, causing huge NL constraint violations at optimum.
            GRBsetintparam(env, cstr(b"Presolve\0"), 0);
            let tol = options.tolerance.clamp(1e-10, 1e-4);
            GRBsetdblparam(env, cstr(PAR_FEASIBILITYTOL), tol);
            GRBsetdblparam(env, cstr(PAR_OPTIMALITYTOL), tol);
            // BarIterLimit: raise well above default (1000) so large problems
            // (ACTIVSg10k+) don't hit the iteration cap before convergence.
            GRBsetintparam(env, cstr(b"BarIterLimit\0"), 10_000);
            // AcOpfOptions has no time_limit_secs; rely on Gurobi's default.

            // ── Gurobi model (created AFTER params so it inherits them) ───
            let model_name =
                CString::new("surge_acopf").expect("static string contains no null bytes");
            let mut model: *mut GRBmodel = std::ptr::null_mut();
            let rc = GRBnewmodel(
                env,
                &mut model,
                model_name.as_ptr(),
                0,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            );
            if rc != 0 || model.is_null() {
                return Err(format!("GRBnewmodel failed rc={rc}"));
            }
            struct Mg(*mut GRBmodel, unsafe extern "C" fn(*mut GRBmodel) -> c_int);
            impl Drop for Mg {
                fn drop(&mut self) {
                    unsafe {
                        (self.1)(self.0);
                    }
                }
            }
            let _mg = Mg(model, GRBfreemodel);

            // ── Variable bounds & obj coefficients ───────────────────────
            let mut col_lb = vec![0.0_f64; n_var];
            let mut col_ub = vec![0.0_f64; n_var];
            let mut col_obj = vec![0.0_f64; n_var]; // linear obj

            // Va bounds [-π, π]
            for i in 0..n_bus {
                if let Some(vi) = vm.va(i) {
                    col_lb[vi] = -std::f64::consts::PI;
                    col_ub[vi] = std::f64::consts::PI;
                }
            }
            // Vm bounds [vmin, vmax]
            for i in 0..n_bus {
                let vi = vm.vm(i);
                col_lb[vi] = network.buses[i].voltage_min_pu;
                col_ub[vi] = network.buses[i].voltage_max_pu;
            }
            // Pg / Qg bounds + linear objective (c1*base)
            for (j, &gi) in gen_indices.iter().enumerate() {
                let g = &network.generators[gi];
                let pg_v = vm.pg(j);
                let qg_v = vm.qg(j);
                col_lb[pg_v] = g.pmin / base;
                col_ub[pg_v] = g.pmax / base;
                let qmin = if g.qmin.abs() > 1e10 { -9999.0 } else { g.qmin };
                let qmax = if g.qmax.abs() > 1e10 { 9999.0 } else { g.qmax };
                col_lb[qg_v] = qmin / base;
                col_ub[qg_v] = qmax / base;
                if let Some(cost) = g.cost.as_ref()
                    && let surge_network::market::CostCurve::Polynomial { ref coeffs, .. } = *cost
                {
                    // coeffs = [c2, c1, c0] highest-degree first
                    let c1 = if coeffs.len() >= 2 {
                        coeffs[coeffs.len() - 2]
                    } else {
                        0.0
                    };
                    let c0 = if !coeffs.is_empty() {
                        *coeffs.last().expect("coeffs non-empty checked")
                    } else {
                        0.0
                    };
                    col_obj[pg_v] = c1 * base;
                    cost_const += c0;
                }
            }
            // pres / qres: free auxiliary variables (bounds -∞..∞)
            for i in 0..n_bus {
                col_lb[vm.pres(i)] = f64::NEG_INFINITY;
                col_ub[vm.pres(i)] = f64::INFINITY;
                col_lb[vm.qres(i)] = f64::NEG_INFINITY;
                col_ub[vm.qres(i)] = f64::INFINITY;
            }
            // pft / qft / ptf / qtf: free branch flow variables
            for k in 0..n_br_con {
                for &v in &[vm.pft(k), vm.qft(k), vm.ptf(k), vm.qtf(k)] {
                    col_lb[v] = f64::NEG_INFINITY;
                    col_ub[v] = f64::INFINITY;
                }
                // sft / stf: bounded by 0 .. Smax²
                let s_max = branch_adm[k].s_max_sq; // already squared
                col_lb[vm.sft(k)] = 0.0;
                col_ub[vm.sft(k)] = s_max;
                col_lb[vm.stf(k)] = 0.0;
                col_ub[vm.stf(k)] = s_max;
            }
            // Angle-difference auxiliaries: bounded directly by angmin/angmax.
            for (k, &(_, _, _, lo, hi)) in angle_constraints.iter().enumerate() {
                col_lb[vm.ang(k)] = if lo.is_finite() {
                    lo
                } else {
                    f64::NEG_INFINITY
                };
                col_ub[vm.ang(k)] = if hi.is_finite() { hi } else { f64::INFINITY };
            }
            // Flowgate/interface auxiliaries: bounded by monitored forward/reverse limits.
            for (fi, &fgi) in flowgate_indices.iter().enumerate() {
                let fg = &network.flowgates[fgi];
                let rev = fg.effective_reverse_or_forward(0);
                col_lb[vm.fg(fi)] = -rev / base;
                col_ub[vm.fg(fi)] = fg.limit_mw / base;
            }
            for (ii, &ifi) in interface_indices.iter().enumerate() {
                let iface = &network.interfaces[ifi];
                col_lb[vm.iface(ii)] = -iface.limit_reverse_mw / base;
                col_ub[vm.iface(ii)] = iface.limit_forward_mw / base;
            }

            let vtypes: Vec<c_char> = vec![GRB_CONTINUOUS; n_var];
            let rc = GRBaddvars(
                model,
                n_var as c_int,
                0,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                col_obj.as_ptr(),
                col_lb.as_ptr(),
                col_ub.as_ptr(),
                vtypes.as_ptr(),
                std::ptr::null(),
            );
            if rc != 0 {
                return Err(format!("GRBaddvars failed rc={rc}"));
            }

            // ── Quadratic objective (c2 * base² * Pg²) ───────────────────
            let mut qrow = Vec::<c_int>::new();
            let mut qcol = Vec::<c_int>::new();
            let mut qval = Vec::<f64>::new();
            for (j, &gi) in gen_indices.iter().enumerate() {
                let g = &network.generators[gi];
                if let Some(cost) = g.cost.as_ref()
                    && let surge_network::market::CostCurve::Polynomial { ref coeffs, .. } = *cost
                {
                    let c2 = if coeffs.len() >= 3 {
                        coeffs[coeffs.len() - 3]
                    } else if coeffs.len() == 2 {
                        coeffs[0]
                    } else {
                        0.0
                    };
                    if c2 != 0.0 {
                        let pg_v = vm.pg(j) as c_int;
                        qrow.push(pg_v);
                        qcol.push(pg_v);
                        // Gurobi: adds qval*x[r]*x[c]. For diagonal that's
                        // qval*Pg² directly. Our model = c2*base²*Pg²,
                        // so qval = c2*base². Gurobi adds it directly (no 0.5).
                        qval.push(c2 * base * base);
                    }
                }
            }
            if !qval.is_empty() {
                let rc = GRBaddqpterms(
                    model,
                    qval.len() as c_int,
                    qrow.as_ptr(),
                    qcol.as_ptr(),
                    qval.as_ptr(),
                );
                if rc != 0 {
                    return Err(format!("GRBaddqpterms failed rc={rc}"));
                }
            }

            // ── Set objective sense ───────────────────────────────────────
            let rc = GRBsetintattr(model, cstr(ATTR_MODELSENSE), GRB_MINIMIZE);
            if rc != 0 {
                return Err(format!("GRBsetintattr(ModelSense) failed rc={rc}"));
            }

            // ── Warm-start initial values ─────────────────────────────────
            // Use flat start: Vm = bus.vm0, Va = 0, Pg = mid-range.
            let mut x0 = vec![0.0_f64; n_var];
            for i in 0..n_bus {
                if let Some(vi) = vm.va(i) {
                    x0[vi] = 0.0;
                }
                x0[vm.vm(i)] = network.buses[i].voltage_magnitude_pu.clamp(
                    network.buses[i].voltage_min_pu,
                    network.buses[i].voltage_max_pu,
                );
            }
            for (j, &gi) in gen_indices.iter().enumerate() {
                let g = &network.generators[gi];
                x0[vm.pg(j)] = ((g.pmin + g.pmax) / 2.0 / base).clamp(g.pmin / base, g.pmax / base);
                x0[vm.qg(j)] = 0.0;
            }
            // DC-OPF warm-start: use economically-optimal angles from the LP
            // solve as initial Va values.  Better than flat-start (all-zero)
            // and cheaper to compute than a full AC-OPF warm-start.
            if let Some(angles) = dc_opf_angles {
                for i in 0..n_bus {
                    if let Some(vi) = vm.va(i)
                        && i < angles.len()
                    {
                        x0[vi] = angles[i];
                    }
                }
            }
            // If warm_start (prior OpfSolution) provided, seed Va/Vm/Pg
            // (takes priority over dc_opf_angles).
            if let Some(ws) = context.runtime.warm_start.as_ref() {
                for i in 0..n_bus {
                    if let Some(vi) = vm.va(i)
                        && i < ws.voltage_angle_rad.len()
                    {
                        x0[vi] = ws.voltage_angle_rad[i];
                    }
                    if i < ws.voltage_magnitude_pu.len() {
                        x0[vm.vm(i)] = ws.voltage_magnitude_pu[i];
                    }
                }
                for (j, &_) in gen_indices.iter().enumerate() {
                    if j < ws.pg.len() {
                        x0[vm.pg(j)] = ws.pg[j];
                    }
                    if j < ws.qg.len() {
                        x0[vm.qg(j)] = ws.qg[j];
                    }
                }
            }
            // ── Initialize pres[i] and qres[i] from P/Q_calc at initial point ─
            // Without this, pres=0 at start violates NL constraints (P_calc ≠ 0),
            // which causes the local NLP barrier to start far from NL feasibility.
            {
                let va0: Vec<f64> = (0..n_bus)
                    .map(|i| vm.va(i).map_or(0.0, |v| x0[v]))
                    .collect();
                let vm0: Vec<f64> = (0..n_bus).map(|i| x0[vm.vm(i)]).collect();
                for i in 0..n_bus {
                    let row = ybus.row(i);
                    let mut p_calc = 0.0_f64;
                    let mut q_calc = 0.0_f64;
                    for k in 0..row.col_idx.len() {
                        let j = row.col_idx[k];
                        let g = row.g[k];
                        let b = row.b[k];
                        let dth = va0[i] - va0[j];
                        let vi = vm0[i];
                        let vj = vm0[j];
                        p_calc += vi * vj * (g * dth.cos() + b * dth.sin());
                        q_calc += vi * vj * (g * dth.sin() - b * dth.cos());
                    }
                    x0[vm.pres(i)] = p_calc;
                    x0[vm.qres(i)] = q_calc;
                }
                for (k, &(_, from, to, _, _)) in angle_constraints.iter().enumerate() {
                    x0[vm.ang(k)] = va0[from] - va0[to];
                }
                let eval_flow_expr = |expr: &GrbFlowExprData| -> f64 {
                    let mut flow = 0.0_f64;
                    for entry in &expr.entries {
                        let adm = &entry.adm;
                        let vi = vm0[adm.from];
                        let vj = vm0[adm.to];
                        let theta = va0[adm.from] - va0[adm.to];
                        let (sin_t, cos_t) = theta.sin_cos();
                        let pf =
                            vi * vi * adm.g_ff + vi * vj * (adm.g_ft * cos_t + adm.b_ft * sin_t);
                        flow += entry.coeff * pf;
                    }
                    flow
                };
                for (fi, expr) in flowgate_data.iter().enumerate() {
                    x0[vm.fg(fi)] = eval_flow_expr(expr);
                }
                for (ii, expr) in interface_data.iter().enumerate() {
                    x0[vm.iface(ii)] = eval_flow_expr(expr);
                }
            }

            let rc = GRBsetdblattrarray(model, cstr(ATTR_START), 0, n_var as c_int, x0.as_ptr());
            if rc != 0 {
                return Err(format!("GRBsetdblattrarray(Start) failed rc={rc}"));
            }

            // ── NL constraints: pres[i] = P_calc(i), qres[i] = Q_calc(i) ─
            for i in 0..n_bus {
                let mut tp = NlTree::new();
                build_p_calc_tree(&mut tp, i, &ybus, &vm);
                let nm =
                    CString::new(format!("pc{i}")).expect("format string contains no null bytes");
                add_genconstr_nl(&self.lib, model, &nm, vm.pres(i), &tp)?;

                let mut tq = NlTree::new();
                build_q_calc_tree(&mut tq, i, &ybus, &vm);
                let nm =
                    CString::new(format!("qc{i}")).expect("format string contains no null bytes");
                add_genconstr_nl(&self.lib, model, &nm, vm.qres(i), &tq)?;
            }
            // ── NL constraints: branch flows ──────────────────────────────
            for (k, ba) in branch_adm.iter().enumerate() {
                let mut t = NlTree::new();
                build_pft_tree(&mut t, ba, &vm);
                add_genconstr_nl(
                    &self.lib,
                    model,
                    &CString::new(format!("pft{k}")).expect("format string contains no null bytes"),
                    vm.pft(k),
                    &t,
                )?;

                let mut t = NlTree::new();
                build_qft_tree(&mut t, ba, &vm);
                add_genconstr_nl(
                    &self.lib,
                    model,
                    &CString::new(format!("qft{k}")).expect("format string contains no null bytes"),
                    vm.qft(k),
                    &t,
                )?;

                let mut t = NlTree::new();
                build_ptf_tree(&mut t, ba, &vm);
                add_genconstr_nl(
                    &self.lib,
                    model,
                    &CString::new(format!("ptf{k}")).expect("format string contains no null bytes"),
                    vm.ptf(k),
                    &t,
                )?;

                let mut t = NlTree::new();
                build_qtf_tree(&mut t, ba, &vm);
                add_genconstr_nl(
                    &self.lib,
                    model,
                    &CString::new(format!("qtf{k}")).expect("format string contains no null bytes"),
                    vm.qtf(k),
                    &t,
                )?;

                // sft = pft² + qft²  (bounds enforce thermal limit)
                let mut t = NlTree::new();
                build_s_sq_tree(&mut t, vm.pft(k), vm.qft(k));
                add_genconstr_nl(
                    &self.lib,
                    model,
                    &CString::new(format!("sft{k}")).expect("format string contains no null bytes"),
                    vm.sft(k),
                    &t,
                )?;

                let mut t = NlTree::new();
                build_s_sq_tree(&mut t, vm.ptf(k), vm.qtf(k));
                add_genconstr_nl(
                    &self.lib,
                    model,
                    &CString::new(format!("stf{k}")).expect("format string contains no null bytes"),
                    vm.stf(k),
                    &t,
                )?;
            }
            // ── NL constraints: flowgate/interface monitored-flow auxiliaries ─
            for (fi, expr) in flowgate_data.iter().enumerate() {
                let mut t = NlTree::new();
                build_weighted_flow_tree(&mut t, expr, &vm);
                add_genconstr_nl(
                    &self.lib,
                    model,
                    &CString::new(format!("fg{fi}")).expect("format string contains no null bytes"),
                    vm.fg(fi),
                    &t,
                )?;
            }
            for (ii, expr) in interface_data.iter().enumerate() {
                let mut t = NlTree::new();
                build_weighted_flow_tree(&mut t, expr, &vm);
                add_genconstr_nl(
                    &self.lib,
                    model,
                    &CString::new(format!("iface{ii}"))
                        .expect("format string contains no null bytes"),
                    vm.iface(ii),
                    &t,
                )?;
            }
            // ── Linear constraints: power balance equalities ──────────────
            //
            // P-balance at bus i:  pres[i] - Σ_j Pg[j] = -Pd_i/base
            // Q-balance at bus i:  qres[i] - Σ_j Qg[j] = -Qd_i/base
            //
            // These are standard LP equality constraints; their Pi duals give LMPs.
            //
            // We batch them: P-balance rows 0..n_bus, Q-balance rows n_bus..2*n_bus.
            let n_lin = 2 * n_bus;
            let mut lin_cbeg = vec![0i32; n_lin + 1];
            let mut lin_cind = Vec::<i32>::new();
            let mut lin_cval = Vec::<f64>::new();
            let mut lin_rhs = vec![0.0_f64; n_lin];

            // Count nonzeros per row first (for lin_cbeg).
            // P-row i: pres[i] (1 coeff) + |gens at bus i| coeffs
            // Q-row i: qres[i] (1 coeff) + |gens at bus i| coeffs
            // Two passes — Q-rows must continue from where P-rows end.
            for i in 0..n_bus {
                let ng = bus_gen_map[i].len() as i32;
                lin_cbeg[i + 1] = lin_cbeg[i] + 1 + ng;
            }
            // lin_cbeg[n_bus] now holds total P-row NZs; Q-rows continue from there.
            for i in 0..n_bus {
                let ng = bus_gen_map[i].len() as i32;
                lin_cbeg[n_bus + i + 1] = lin_cbeg[n_bus + i] + 1 + ng;
            }
            let total_nz = lin_cbeg[n_lin] as usize;
            lin_cind.resize(total_nz, 0);
            lin_cval.resize(total_nz, 0.0);

            for i in 0..n_bus {
                // P-balance row i
                let start_p = lin_cbeg[i] as usize;
                lin_cind[start_p] = vm.pres(i) as i32;
                lin_cval[start_p] = 1.0;
                for (off, &j) in bus_gen_map[i].iter().enumerate() {
                    lin_cind[start_p + 1 + off] = vm.pg(j) as i32;
                    lin_cval[start_p + 1 + off] = -1.0;
                }
                lin_rhs[i] = -bus_pd_mw[i] / base;

                // Q-balance row n_bus+i
                let start_q = lin_cbeg[n_bus + i] as usize;
                lin_cind[start_q] = vm.qres(i) as i32;
                lin_cval[start_q] = 1.0;
                for (off, &j) in bus_gen_map[i].iter().enumerate() {
                    lin_cind[start_q + 1 + off] = vm.qg(j) as i32;
                    lin_cval[start_q + 1 + off] = -1.0;
                }
                lin_rhs[n_bus + i] = -bus_qd_mvar[i] / base;
            }

            let senses: Vec<c_char> = vec![GRB_EQUAL; n_lin];
            let rc = GRBaddconstrs(
                model,
                n_lin as c_int,
                total_nz as c_int,
                lin_cbeg.as_ptr(),
                lin_cind.as_ptr(),
                lin_cval.as_ptr(),
                senses.as_ptr(),
                lin_rhs.as_ptr(),
                std::ptr::null(),
            );
            if rc != 0 {
                return Err(format!("GRBaddconstrs (power balance) failed rc={rc}"));
            }

            if !angle_constraints.is_empty() {
                let mut ang_cbeg = vec![0i32; angle_constraints.len() + 1];
                let mut ang_cind = Vec::<i32>::with_capacity(angle_constraints.len() * 3);
                let mut ang_cval = Vec::<f64>::with_capacity(angle_constraints.len() * 3);
                for (k, &(_, from, to, _, _)) in angle_constraints.iter().enumerate() {
                    ang_cbeg[k] = ang_cind.len() as i32;
                    ang_cind.push(vm.ang(k) as i32);
                    ang_cval.push(1.0);
                    if let Some(vf) = vm.va(from) {
                        ang_cind.push(vf as i32);
                        ang_cval.push(-1.0);
                    }
                    if let Some(vt) = vm.va(to) {
                        ang_cind.push(vt as i32);
                        ang_cval.push(1.0);
                    }
                }
                ang_cbeg[angle_constraints.len()] = ang_cind.len() as i32;
                let ang_rhs = vec![0.0_f64; angle_constraints.len()];
                let ang_sense: Vec<c_char> = vec![GRB_EQUAL; angle_constraints.len()];
                let rc = GRBaddconstrs(
                    model,
                    angle_constraints.len() as c_int,
                    ang_cind.len() as c_int,
                    ang_cbeg.as_ptr(),
                    ang_cind.as_ptr(),
                    ang_cval.as_ptr(),
                    ang_sense.as_ptr(),
                    ang_rhs.as_ptr(),
                    std::ptr::null(),
                );
                if rc != 0 {
                    return Err(format!("GRBaddconstrs (angle differences) failed rc={rc}"));
                }
            }

            // ── Solve ─────────────────────────────────────────────────────
            let solve_rc = GRBoptimize(model);
            if solve_rc != 0 {
                return Err(format!("GRBoptimize failed rc={solve_rc}"));
            }

            // ── Check status ──────────────────────────────────────────────
            let mut stat: c_int = 0;
            GRBgetintattr(model, cstr(ATTR_STATUS), &mut stat);
            if stat == GRB_SUBOPTIMAL {
                return Err(
                    "Gurobi AC-OPF: suboptimal termination is not accepted as a converged release-grade result".into(),
                );
            }
            if stat != GRB_OPTIMAL && stat != GRB_LOCALLY_OPTIMAL {
                return Err(format!("Gurobi AC-OPF: status={stat} (not optimal)"));
            }

            // ── Extract primal solution ───────────────────────────────────
            let mut x_all = vec![0.0_f64; n_var];
            GRBgetdblattrarray(model, cstr(ATTR_X), 0, n_var as c_int, x_all.as_mut_ptr());

            let mut obj_raw: f64 = 0.0;
            GRBgetdblattr(model, cstr(ATTR_OBJVAL), &mut obj_raw);
            let total_cost = obj_raw + cost_const;

            let mut iter_dbl: f64 = 0.0;
            let iterations = if GRBgetdblattr(model, cstr(ATTR_ITERCOUNT), &mut iter_dbl) == 0 {
                iter_dbl as u32
            } else {
                let mut bi: c_int = 0;
                GRBgetintattr(model, cstr(ATTR_BARITERCOUNT), &mut bi);
                bi as u32
            };

            // ── Post-solve dual LP: recover Pi duals for LMPs ────────────
            //
            // Gurobi's local NLP barrier does not populate Pi for linear
            // constraints when GRBaddgenconstrNL constraints are present
            // (GRB_ERROR_DATA_NOT_AVAILABLE).  Standard technique: after NLP
            // convergence, fix Va*/Vm* → P_calc/Q_calc become constants →
            // solve a tiny LP in Pg/Qg only → get exact Pi duals.
            //
            // The dual LP:
            //   min  Σ_j cost(Pg[j])
            //   s.t. Σ_{j: bus=i} Pg[j] = p_inj[i] + Pd[i]/base   (P-balance)
            //        Σ_{j: bus=i} Qg[j] = q_inj[i] + Qd[i]/base   (Q-balance)
            //        Pg[j] ∈ [pmin[j]/base, pmax[j]/base]
            //        Qg[j] ∈ [qmin[j]/base, qmax[j]/base]
            let pi_all: Vec<f64> = {
                // 1. Compute P/Q injections from optimal Va*/Vm*
                let va_opt: Vec<f64> = (0..n_bus)
                    .map(|i| vm.va(i).map_or(0.0, |v| x_all[v]))
                    .collect();
                let vm_opt: Vec<f64> = (0..n_bus).map(|i| x_all[vm.vm(i)]).collect();
                let mut p_inj = vec![0.0_f64; n_bus];
                let mut q_inj = vec![0.0_f64; n_bus];
                for i in 0..n_bus {
                    let row = ybus.row(i);
                    for k in 0..row.col_idx.len() {
                        let j = row.col_idx[k];
                        let g = row.g[k];
                        let b = row.b[k];
                        let dth = va_opt[i] - va_opt[j];
                        let vi = vm_opt[i];
                        let vj = vm_opt[j];
                        p_inj[i] += vi * vj * (g * dth.cos() + b * dth.sin());
                        q_inj[i] += vi * vj * (g * dth.sin() - b * dth.cos());
                    }
                }

                // 2. Build tiny LP: variables = Pg[0..n_gen] | Qg[0..n_gen]
                let n_lp = 2 * n_gen;
                let mut lp_lb = vec![0.0_f64; n_lp];
                let mut lp_ub = vec![0.0_f64; n_lp];
                let mut lp_obj = vec![0.0_f64; n_lp];
                for (j, &gi) in gen_indices.iter().enumerate() {
                    let g = &network.generators[gi];
                    lp_lb[j] = g.pmin / base;
                    lp_ub[j] = g.pmax / base;
                    let c1 = if let Some(surge_network::market::CostCurve::Polynomial {
                        ref coeffs,
                        ..
                    }) = g.cost
                    {
                        if coeffs.len() >= 2 {
                            coeffs[coeffs.len() - 2]
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };
                    lp_obj[j] = c1 * base;
                    let qmin = if g.qmin > -1e9 { g.qmin } else { -1e9 };
                    let qmax = if g.qmax < 1e9 { g.qmax } else { 1e9 };
                    lp_lb[n_gen + j] = qmin / base;
                    lp_ub[n_gen + j] = qmax / base;
                }
                let lp_model_name =
                    CString::new("grb_dual_lp").expect("static string contains no null bytes");
                let mut lp_model: *mut GRBmodel = std::ptr::null_mut();
                GRBnewmodel(
                    env,
                    &mut lp_model,
                    lp_model_name.as_ptr(),
                    n_lp as c_int,
                    lp_obj.as_ptr(),
                    lp_lb.as_ptr(),
                    lp_ub.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                );
                struct Mlp(*mut GRBmodel, unsafe extern "C" fn(*mut GRBmodel) -> c_int);
                impl Drop for Mlp {
                    fn drop(&mut self) {
                        unsafe {
                            (self.1)(self.0);
                        }
                    }
                }
                let _mlp = Mlp(lp_model, GRBfreemodel);

                // Quadratic cost terms
                let mut qrow2 = Vec::<c_int>::new();
                let mut qcol2 = Vec::<c_int>::new();
                let mut qval2 = Vec::<f64>::new();
                for (j, &gi) in gen_indices.iter().enumerate() {
                    let g = &network.generators[gi];
                    if let Some(surge_network::market::CostCurve::Polynomial {
                        ref coeffs, ..
                    }) = g.cost
                    {
                        let c2 = if coeffs.len() >= 3 {
                            coeffs[coeffs.len() - 3]
                        } else {
                            0.0
                        };
                        if c2 != 0.0 {
                            qrow2.push(j as c_int);
                            qcol2.push(j as c_int);
                            qval2.push(c2 * base * base);
                        }
                    }
                }
                if !qval2.is_empty() {
                    GRBaddqpterms(
                        lp_model,
                        qval2.len() as c_int,
                        qrow2.as_ptr(),
                        qcol2.as_ptr(),
                        qval2.as_ptr(),
                    );
                }
                GRBsetintattr(lp_model, cstr(ATTR_MODELSENSE), GRB_MINIMIZE);

                // Power balance rows (CSR): P-balance 0..n_bus, Q-balance n_bus..2*n_bus
                let n_bal = 2 * n_bus;
                let mut rhs = vec![0.0_f64; n_bal];
                for i in 0..n_bus {
                    rhs[i] = p_inj[i] + bus_pd_mw[i] / base;
                    rhs[n_bus + i] = q_inj[i] + bus_qd_mvar[i] / base;
                }
                // cbeg: count NZs per row first (two passes)
                let mut cbeg = vec![0i32; n_bal + 1];
                for i in 0..n_bus {
                    cbeg[i + 1] = cbeg[i] + bus_gen_map[i].len() as i32;
                }
                for i in 0..n_bus {
                    cbeg[n_bus + i + 1] = cbeg[n_bus + i] + bus_gen_map[i].len() as i32;
                }
                let nnz = cbeg[n_bal] as usize;
                let mut cind = vec![0i32; nnz];
                let cval = vec![1.0_f64; nnz];
                for i in 0..n_bus {
                    let sp = cbeg[i] as usize;
                    let sq = cbeg[n_bus + i] as usize;
                    for (off, &j) in bus_gen_map[i].iter().enumerate() {
                        cind[sp + off] = j as i32; // Pg col
                        cind[sq + off] = (n_gen + j) as i32; // Qg col
                    }
                }
                let senses2: Vec<c_char> = vec![GRB_EQUAL; n_bal];
                GRBaddconstrs(
                    lp_model,
                    n_bal as c_int,
                    nnz as c_int,
                    cbeg.as_ptr(),
                    cind.as_ptr(),
                    cval.as_ptr(),
                    senses2.as_ptr(),
                    rhs.as_ptr(),
                    std::ptr::null(),
                );

                // Warm-start LP at NLP optimal Pg/Qg
                let mut x_lp0 = vec![0.0_f64; n_lp];
                for j in 0..n_gen {
                    x_lp0[j] = x_all[vm.pg(j)];
                    x_lp0[n_gen + j] = x_all[vm.qg(j)];
                }
                GRBsetdblattrarray(lp_model, cstr(ATTR_START), 0, n_lp as c_int, x_lp0.as_ptr());

                GRBoptimize(lp_model);

                let mut pi_p = vec![0.0_f64; n_bal];
                GRBgetdblattrarray(
                    lp_model,
                    cstr(ATTR_PI),
                    0,
                    n_bal as c_int,
                    pi_p.as_mut_ptr(),
                );
                pi_p
            };
            // pi_all[0..n_bus]       = P-balance duals
            // pi_all[n_bus..2*n_bus] = Q-balance duals (unused for LMPs)

            // ── Extract reduced costs (bound duals) ───────────────────────
            let mut rc_all = vec![0.0_f64; n_var];
            GRBgetdblattrarray(model, cstr(ATTR_RC), 0, n_var as c_int, rc_all.as_mut_ptr());

            // ── Build solution vectors ────────────────────────────────────
            // Reconstruct full Va including slack=0
            let mut va_full = vec![0.0_f64; n_bus];
            for i in 0..n_bus {
                if let Some(vi) = vm.va(i) {
                    va_full[i] = x_all[vi];
                }
            }
            let vm_full: Vec<f64> = (0..n_bus).map(|i| x_all[vm.vm(i)]).collect();
            let pg_pu: Vec<f64> = (0..n_gen).map(|j| x_all[vm.pg(j)]).collect();
            let qg_pu: Vec<f64> = (0..n_gen).map(|j| x_all[vm.qg(j)]).collect();

            let gen_p_mw: Vec<f64> = pg_pu.iter().map(|&p| p * base).collect();
            let gen_q_mvar: Vec<f64> = qg_pu.iter().map(|&q| q * base).collect();

            // ── LMP from Pi duals ─────────────────────────────────────────
            // pi_all[i]       = d(obj)/d(RHS_p[i]) for P-balance row i
            // RHS_p[i] = -Pd_i/base; d(RHS)/d(Pd_MW) = -1/base
            // => LMP[i] = d(obj)/d(Pd_MW) = pi_all[i] * (-1/base)
            // But sign: in our formulation pres[i] - ΣPg = -Pd/base,
            // so Pi[i] has the same sign as the Ipopt lambda[i].
            // (Both are ∂obj/∂(power-balance-slack), with the same convention.)
            let lmp: Vec<f64> = (0..n_bus).map(|i| pi_all[i] / base).collect();

            // ── LMP decomposition ──────────────────────────────────────────
            let lambda_energy = lmp[slack_idx];
            let lmp_energy = vec![lambda_energy; n_bus];

            // Congestion via PTDF (same as Ipopt path)
            let lmp_congestion: Vec<f64> = if n_br_con > 0 {
                // Branch flow shadow prices from RC of sft/stf vars at upper bound.
                // At UB: RC < 0.  μ_from[k] = max(0, -RC[sft[k]]).
                let mu: Vec<f64> = (0..n_br_con)
                    .map(|k| {
                        let rc_sft = rc_all[vm.sft(k)];
                        let rc_stf = rc_all[vm.stf(k)];
                        // effective = sum of both sides (additive when both bind)
                        rc_sft.min(0.0).abs() + rc_stf.min(0.0).abs()
                    })
                    .collect();

                let monitored = constrained.clone();
                match surge_dc::compute_ptdf(
                    network,
                    &surge_dc::PtdfRequest::for_branches(&monitored),
                ) {
                    Ok(sparse_ptdf) => {
                        let mut cong = vec![0.0_f64; n_bus];
                        for (ci, &br_idx) in constrained.iter().enumerate() {
                            let mu_k = mu[ci];
                            if mu_k.abs() < 1e-30 {
                                continue;
                            }
                            if let Some(row) = sparse_ptdf.row(br_idx) {
                                for i in 0..n_bus {
                                    cong[i] += mu_k * row[i];
                                }
                            }
                        }
                        cong.iter_mut().for_each(|v| *v /= base);
                        // drop mu to silence unused-mut warning
                        drop(mu);
                        cong
                    }
                    Err(_) => vec![0.0; n_bus],
                }
            } else {
                vec![0.0; n_bus]
            };

            let lmp_loss: Vec<f64> = lmp
                .iter()
                .zip(&lmp_congestion)
                .map(|(&l, &c)| l - lambda_energy - c)
                .collect();

            // ── Reactive LMP ──────────────────────────────────────────────
            let lmp_reactive: Vec<f64> = (0..n_bus).map(|i| -pi_all[n_bus + i] / base).collect();

            // ── Branch shadow prices ───────────────────────────────────────
            let n_br_total = network.n_branches();
            let mut branch_shadow_prices = vec![0.0_f64; n_br_total];
            for (ci, &br_idx) in constrained.iter().enumerate() {
                let mu_from = rc_all[vm.sft(ci)].min(0.0).abs();
                let mu_to = rc_all[vm.stf(ci)].min(0.0).abs();
                branch_shadow_prices[br_idx] = (mu_from + mu_to) / base;
            }
            let mut shadow_price_angmin = vec![0.0_f64; n_br_total];
            let mut shadow_price_angmax = vec![0.0_f64; n_br_total];
            for (ai, &(br_idx, _, _, _, _)) in angle_constraints.iter().enumerate() {
                let rc = rc_all[vm.ang(ai)] / base;
                if rc > 0.0 {
                    shadow_price_angmin[br_idx] = rc;
                } else {
                    shadow_price_angmax[br_idx] = (-rc).max(0.0);
                }
            }
            let flowgate_shadow_prices = if flowgate_indices.is_empty() {
                vec![]
            } else {
                let mut v = vec![0.0_f64; network.flowgates.len()];
                for (fi, &fgi) in flowgate_indices.iter().enumerate() {
                    v[fgi] = -rc_all[vm.fg(fi)] / base;
                }
                v
            };
            let interface_shadow_prices = if interface_indices.is_empty() {
                vec![]
            } else {
                let mut v = vec![0.0_f64; network.interfaces.len()];
                for (ii, &ifi) in interface_indices.iter().enumerate() {
                    v[ifi] = -rc_all[vm.iface(ii)] / base;
                }
                v
            };

            // ── Bound duals (mu_pg, mu_vm, mu_qg) ────────────────────────
            // At lower bound: RC > 0 → mu_lower = RC
            // At upper bound: RC < 0 → mu_upper = -RC
            let shadow_price_pg_min: Vec<f64> = (0..n_gen)
                .map(|j| rc_all[vm.pg(j)].max(0.0) / base)
                .collect();
            let shadow_price_pg_max: Vec<f64> = (0..n_gen)
                .map(|j| (-rc_all[vm.pg(j)]).max(0.0) / base)
                .collect();
            let shadow_price_vm_min: Vec<f64> = (0..n_bus)
                .map(|i| rc_all[vm.vm(i)].max(0.0) / base)
                .collect();
            let shadow_price_vm_max: Vec<f64> = (0..n_bus)
                .map(|i| (-rc_all[vm.vm(i)]).max(0.0) / base)
                .collect();
            let shadow_price_qg_min: Vec<f64> = (0..n_gen)
                .map(|j| rc_all[vm.qg(j)].max(0.0) / base)
                .collect();
            let shadow_price_qg_max: Vec<f64> = (0..n_gen)
                .map(|j| (-rc_all[vm.qg(j)]).max(0.0) / base)
                .collect();

            // ── Branch flows from voltage solution ────────────────────────
            let mut branch_pf_mw = vec![0.0_f64; n_br_total];
            let mut branch_pt_mw = vec![0.0_f64; n_br_total];
            let mut branch_qf_mvar = vec![0.0_f64; n_br_total];
            let mut branch_qt_mvar = vec![0.0_f64; n_br_total];
            let mut branch_loading_pct = vec![0.0_f64; n_br_total];

            for (l, br) in network.branches.iter().enumerate() {
                if !br.in_service {
                    continue;
                }
                let fi = bus_map[&br.from_bus];
                let ti = bus_map[&br.to_bus];
                let vf = vm_full[fi];
                let vt = vm_full[ti];
                let theta_ft = va_full[fi] - va_full[ti];
                let flows = br.power_flows_pu(vf, vt, theta_ft, 1e-40);

                branch_pf_mw[l] = flows.p_from_pu * base;
                branch_qf_mvar[l] = flows.q_from_pu * base;
                branch_pt_mw[l] = flows.p_to_pu * base;
                branch_qt_mvar[l] = flows.q_to_pu * base;
                if br.rating_a_mva > 0.0 {
                    let sf = flows.s_from_pu() * base;
                    let st = flows.s_to_pu() * base;
                    branch_loading_pct[l] = sf.max(st) / br.rating_a_mva * 100.0;
                } else {
                    branch_loading_pct[l] = f64::NAN;
                }
            }

            // ── Power flow solution ───────────────────────────────────────
            let (p_inject, q_inject) =
                surge_ac::matrix::mismatch::compute_power_injection(&ybus, &vm_full, &va_full);
            let pf_solution = surge_solution::PfSolution {
                pf_model: surge_solution::PfModel::Ac,
                status: surge_solution::SolveStatus::Converged,
                iterations,
                max_mismatch: 0.0,
                solve_time_secs: 0.0,
                voltage_magnitude_pu: vm_full,
                voltage_angle_rad: va_full,
                active_power_injection_pu: p_inject,
                reactive_power_injection_pu: q_inject,
                branch_p_from_mw: branch_pf_mw.clone(),
                branch_p_to_mw: branch_pt_mw.clone(),
                branch_q_from_mvar: branch_qf_mvar.clone(),
                branch_q_to_mvar: branch_qt_mvar.clone(),
                bus_numbers: network.buses.iter().map(|b| b.number).collect(),
                island_ids: vec![],
                q_limited_buses: vec![],
                n_q_limit_switches: 0,
                gen_slack_contribution_mw: vec![],
                convergence_history: vec![],
                worst_mismatch_bus: None,
                area_interchange: None,
            };

            // ── Remaining scalars ─────────────────────────────────────────
            let gen_bus_numbers: Vec<u32> = gen_indices
                .iter()
                .map(|&gi| network.generators[gi].bus)
                .collect();
            let gen_ids: Vec<String> = gen_indices
                .iter()
                .map(|&gi| network.generators[gi].id.clone())
                .collect();
            let gen_machine_ids: Vec<String> = gen_indices
                .iter()
                .map(|&gi| {
                    network.generators[gi]
                        .machine_id
                        .clone()
                        .unwrap_or_else(|| "1".to_string())
                })
                .collect();
            let total_load_mw: f64 = network.total_load_mw();
            let total_generation_mw: f64 = gen_p_mw.iter().sum();
            let total_losses_mw = total_generation_mw - total_load_mw;

            Ok(surge_solution::OpfSolution {
                opf_type: surge_solution::OpfType::AcOpf,
                base_mva: base,
                power_flow: pf_solution,
                generators: surge_solution::OpfGeneratorResults {
                    gen_p_mw,
                    gen_q_mvar,
                    gen_bus_numbers,
                    gen_ids,
                    gen_machine_ids,
                    shadow_price_pg_min,
                    shadow_price_pg_max,
                    shadow_price_qg_min,
                    shadow_price_qg_max,
                },
                pricing: surge_solution::OpfPricing {
                    lmp,
                    lmp_energy,
                    lmp_congestion,
                    lmp_loss,
                    lmp_reactive,
                },
                branches: surge_solution::OpfBranchResults {
                    branch_loading_pct,
                    branch_shadow_prices,
                    shadow_price_angmin,
                    shadow_price_angmax,
                    thermal_limit_slack_from_mva: vec![],
                    thermal_limit_slack_to_mva: vec![],
                    flowgate_shadow_prices,
                    interface_shadow_prices,
                    shadow_price_vm_min,
                    shadow_price_vm_max,
                },
                devices: surge_solution::OpfDeviceDispatch::default(),
                total_cost,
                total_load_mw,
                total_generation_mw,
                total_losses_mw,
                par_results: vec![],
                virtual_bid_results: vec![],
                benders_cut_duals: vec![],
                objective_terms: vec![],
                audit: Default::default(),
                bus_q_slack_pos_mvar: vec![],
                bus_q_slack_neg_mvar: vec![],
                bus_p_slack_pos_mw: vec![],
                bus_p_slack_neg_mw: vec![],
                vm_slack_high_pu: vec![],
                vm_slack_low_pu: vec![],
                angle_diff_slack_high_rad: vec![],
                angle_diff_slack_low_rad: vec![],
                solve_time_secs: 0.0, // caller fills in timing
                iterations: Some(iterations),
                solver_name: Some("Gurobi-NLP".to_string()),
                solver_version: Some("13.0".to_string()),
                ac_opf_timings: None,
                nlp_trace: None,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{build_gurobi_name_storage, gurobi_status_from_code, sanitize_gurobi_name};
        use crate::backends::LpSolveStatus;

        #[test]
        fn sanitize_gurobi_name_normalizes_special_chars() {
            let name = sanitize_gurobi_name("h0:startup tier/g1", "r", 12);
            assert_eq!(name, "r_12_h0_startup_tier_g1");
        }

        #[test]
        fn build_gurobi_name_storage_falls_back_when_names_missing() {
            let names = build_gurobi_name_storage(None, "c", 2);
            let rendered: Vec<String> = names
                .iter()
                .map(|value| value.to_string_lossy().to_string())
                .collect();
            assert_eq!(rendered, vec!["c_0_c_0".to_string(), "c_1_c_1".to_string()]);
        }

        #[test]
        fn gurobi_time_limit_with_incumbent_maps_to_suboptimal() {
            assert_eq!(
                gurobi_status_from_code(9, Some(1)),
                LpSolveStatus::SubOptimal
            );
        }

        #[test]
        fn gurobi_time_limit_without_incumbent_stays_solver_error() {
            assert_eq!(
                gurobi_status_from_code(9, Some(0)),
                LpSolveStatus::SolverError(
                    "Gurobi status=9 (time limit, no incumbent)".to_string()
                )
            );
        }
    }
}
