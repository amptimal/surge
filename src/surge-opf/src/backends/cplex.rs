// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! IBM CPLEX LP/QP/MIP backend implementing [`LpSolver`].
//!
//! Runtime discovery of the CPLEX Callable Library:
//! ```sh
//! # Typical install paths:
//! export CPLEX_STUDIO_HOME=/opt/ibm/ILOG/CPLEX_Studio2211
//! export LD_LIBRARY_PATH=$CPLEX_STUDIO_HOME/cplex/lib/x86-64_linux/static_pic:$LD_LIBRARY_PATH
//! ```
//!
//! Requires a valid IBM CPLEX license. Academic licenses are available from
//! the IBM Academic Initiative program.
//!
//! # Dual sign convention
//!
//! CPLEX `CPXgetpi` returns shadow prices in standard Lagrange convention
//! (positive dual = tighter constraint increases objective), which matches
//! `LpResult::row_dual` directly — no sign transformation needed.

pub use self::impl_::CplexLpSolver;

mod impl_ {
    use libloading::Library;
    use std::ffi::{CString, c_char, c_double, c_int};
    use std::ptr;
    use std::sync::{Arc, OnceLock};

    use crate::backends::{
        LpOptions, LpResult, LpSolveStatus, LpSolver, SparseProblem, VariableDomain,
    };

    fn is_integer_domain(domain: VariableDomain) -> bool {
        !matches!(domain, VariableDomain::Continuous)
    }

    fn cplex_col_type(domain: VariableDomain) -> c_char {
        match domain {
            VariableDomain::Continuous => ffi::CTYPE_CONT as c_char,
            VariableDomain::Binary => ffi::CTYPE_BIN as c_char,
            VariableDomain::Integer => ffi::CTYPE_INT as c_char,
        }
    }

    // ── CPLEX Callable Library FFI ────────────────────────────────────────────
    //
    // Load the CPLEX Callable Library at runtime. Set `CPLEX_STUDIO_HOME`
    // and/or add the library directory to `LD_LIBRARY_PATH` so `libcplex.so`
    // (Linux) or `libcplex.dylib` (macOS) can be discovered.

    #[allow(non_camel_case_types, clippy::upper_case_acronyms, dead_code)]
    mod ffi {
        use std::ffi::c_int;

        // Opaque environment and problem pointers.
        pub enum CPXenv {}
        pub enum CPXlp {}

        pub type CPXENVptr = *mut CPXenv;
        pub type CPXLPptr = *mut CPXlp;
        pub type CPXCENVptr = *const CPXenv;
        pub type CPXCLPptr = *const CPXlp;

        // Objective sense constants (from cplex.h).
        pub const CPX_MIN: c_int = 1;

        // Constraint sense byte values.
        pub const SENSE_EQ: u8 = b'E'; // equality
        pub const SENSE_LE: u8 = b'L'; // ≤
        pub const SENSE_GE: u8 = b'G'; // ≥
        pub const SENSE_RG: u8 = b'R'; // range [lb, ub]

        // Variable type byte values.
        pub const CTYPE_CONT: u8 = b'C'; // continuous
        pub const CTYPE_INT: u8 = b'I'; // general integer
        pub const CTYPE_BIN: u8 = b'B'; // binary

        // Solution status codes.
        pub const CPX_STAT_OPTIMAL: c_int = 1;
        pub const CPX_STAT_UNBOUNDED: c_int = 2;
        pub const CPX_STAT_INFEASIBLE: c_int = 3;
        pub const CPX_STAT_INF_OR_UNBD: c_int = 4;
        pub const CPXMIP_OPTIMAL: c_int = 101;
        pub const CPXMIP_OPTIMAL_TOL: c_int = 102;
        pub const CPXMIP_INFEASIBLE: c_int = 103;
        pub const CPXMIP_UNBOUNDED: c_int = 118;

        // Integer parameter codes (from cplex.h CPX_PARAM_*).
        pub const CPX_PARAM_SCRIND: c_int = 1035; // screen output (0=off)
        pub const CPX_PARAM_THREADS: c_int = 1067; // thread count

        // Double parameter codes.
        pub const CPX_PARAM_EPOPT: c_int = 1014; // optimality tolerance
        pub const CPX_PARAM_EPRHS: c_int = 1016; // feasibility tolerance
        pub const CPX_PARAM_TILIM: c_int = 1039; // time limit (seconds)
    }

    // ── CPLEX libloading — runtime dynamic library detection ──────────────────

    #[allow(non_snake_case, dead_code)]
    struct CplexLib {
        _lib: Library,
        CPXopenCPLEX: unsafe extern "C" fn(*mut c_int) -> ffi::CPXENVptr,
        CPXcloseCPLEX: unsafe extern "C" fn(*mut ffi::CPXENVptr) -> c_int,
        CPXsetintparam: unsafe extern "C" fn(ffi::CPXENVptr, c_int, c_int) -> c_int,
        CPXsetdblparam: unsafe extern "C" fn(ffi::CPXENVptr, c_int, c_double) -> c_int,
        CPXcreateprob:
            unsafe extern "C" fn(ffi::CPXENVptr, *mut c_int, *const c_char) -> ffi::CPXLPptr,
        CPXfreeprob: unsafe extern "C" fn(ffi::CPXENVptr, *mut ffi::CPXLPptr) -> c_int,
        CPXcopylp: unsafe extern "C" fn(
            ffi::CPXCENVptr,
            ffi::CPXLPptr,
            c_int,
            c_int,
            c_int,
            *const c_double,
            *const c_double,
            *const c_char,
            *const c_int,
            *const c_int,
            *const c_int,
            *const c_double,
            *const c_double,
            *const c_double,
            *const c_double,
        ) -> c_int,
        CPXcopyquad: unsafe extern "C" fn(
            ffi::CPXCENVptr,
            ffi::CPXLPptr,
            *const c_int,
            *const c_int,
            *const c_int,
            *const c_double,
        ) -> c_int,
        CPXcopyctype: unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXLPptr, *const c_char) -> c_int,
        CPXlpopt: unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXLPptr) -> c_int,
        CPXmipopt: unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXLPptr) -> c_int,
        CPXgetstat: unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int,
        CPXgetobjval: unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr, *mut c_double) -> c_int,
        CPXgetx: unsafe extern "C" fn(
            ffi::CPXCENVptr,
            ffi::CPXCLPptr,
            *mut c_double,
            c_int,
            c_int,
        ) -> c_int,
        CPXgetpi: unsafe extern "C" fn(
            ffi::CPXCENVptr,
            ffi::CPXCLPptr,
            *mut c_double,
            c_int,
            c_int,
        ) -> c_int,
        CPXgetdj: unsafe extern "C" fn(
            ffi::CPXCENVptr,
            ffi::CPXCLPptr,
            *mut c_double,
            c_int,
            c_int,
        ) -> c_int,
        CPXgetitcnt: unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int,
        CPXgetnodecnt: unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int,
        CPXgetnumrows: unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int,
        CPXgetnumcols: unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int,
    }

    static CPLEX: OnceLock<Result<Arc<CplexLib>, String>> = OnceLock::new();

    fn get_cplex() -> Result<&'static Arc<CplexLib>, String> {
        CPLEX
            .get_or_init(|| {
                for path in cplex_lib_paths() {
                    if let Ok(lib) = unsafe { Library::new(&path) } {
                        match unsafe { load_cplex_symbols(lib) } {
                            Ok(clib) => return Ok(Arc::new(clib)),
                            Err(e) => return Err(e),
                        }
                    }
                }
                Err(
                    "CPLEX not found — set CPLEX_STUDIO_HOME or install IBM CPLEX. \
                 Only CPLEX 22.x Callable Library (libcplex.so) is supported."
                        .to_string(),
                )
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    fn cplex_lib_paths() -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        if let Ok(home) = std::env::var("CPLEX_STUDIO_HOME") {
            // CPLEX puts its shared lib in a platform-specific sub-directory.
            for sub in &[
                "cplex/lib/x86-64_linux",
                "cplex/lib/x86-64_osx",
                "cplex/lib",
            ] {
                paths.push(std::path::PathBuf::from(format!(
                    "{home}/{sub}/libcplex.so"
                )));
                paths.push(std::path::PathBuf::from(format!(
                    "{home}/{sub}/libcplex.dylib"
                )));
            }
        }
        // Common install prefixes
        for prefix in &[
            "/opt/ibm/ILOG/CPLEX_Studio2211/cplex/lib/x86-64_linux",
            "/opt/ibm/ILOG/CPLEX_Studio221/cplex/lib/x86-64_linux",
        ] {
            paths.push(std::path::PathBuf::from(format!("{prefix}/libcplex.so")));
        }
        paths.push(std::path::PathBuf::from("libcplex.so"));
        paths.push(std::path::PathBuf::from("libcplex.dylib"));
        paths
    }

    unsafe fn load_cplex_symbols(lib: Library) -> Result<CplexLib, String> {
        macro_rules! sym {
            ($name:literal, $ty:ty) => {
                *unsafe { lib.get::<$ty>($name) }
                    .map_err(|e| format!("CPLEX symbol {} not found: {e}", stringify!($name)))?
            };
        }
        Ok(CplexLib {
            CPXopenCPLEX: sym!(
                b"CPXopenCPLEX\0",
                unsafe extern "C" fn(*mut c_int) -> ffi::CPXENVptr
            ),
            CPXcloseCPLEX: sym!(
                b"CPXcloseCPLEX\0",
                unsafe extern "C" fn(*mut ffi::CPXENVptr) -> c_int
            ),
            CPXsetintparam: sym!(
                b"CPXsetintparam\0",
                unsafe extern "C" fn(ffi::CPXENVptr, c_int, c_int) -> c_int
            ),
            CPXsetdblparam: sym!(
                b"CPXsetdblparam\0",
                unsafe extern "C" fn(ffi::CPXENVptr, c_int, c_double) -> c_int
            ),
            CPXcreateprob: sym!(
                b"CPXcreateprob\0",
                unsafe extern "C" fn(ffi::CPXENVptr, *mut c_int, *const c_char) -> ffi::CPXLPptr
            ),
            CPXfreeprob: sym!(
                b"CPXfreeprob\0",
                unsafe extern "C" fn(ffi::CPXENVptr, *mut ffi::CPXLPptr) -> c_int
            ),
            CPXcopylp: sym!(
                b"CPXcopylp\0",
                unsafe extern "C" fn(
                    ffi::CPXCENVptr,
                    ffi::CPXLPptr,
                    c_int,
                    c_int,
                    c_int,
                    *const c_double,
                    *const c_double,
                    *const c_char,
                    *const c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                    *const c_double,
                    *const c_double,
                    *const c_double,
                ) -> c_int
            ),
            CPXcopyquad: sym!(
                b"CPXcopyquad\0",
                unsafe extern "C" fn(
                    ffi::CPXCENVptr,
                    ffi::CPXLPptr,
                    *const c_int,
                    *const c_int,
                    *const c_int,
                    *const c_double,
                ) -> c_int
            ),
            CPXcopyctype: sym!(
                b"CPXcopyctype\0",
                unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXLPptr, *const c_char) -> c_int
            ),
            CPXlpopt: sym!(
                b"CPXlpopt\0",
                unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXLPptr) -> c_int
            ),
            CPXmipopt: sym!(
                b"CPXmipopt\0",
                unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXLPptr) -> c_int
            ),
            CPXgetstat: sym!(
                b"CPXgetstat\0",
                unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int
            ),
            CPXgetobjval: sym!(
                b"CPXgetobjval\0",
                unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr, *mut c_double) -> c_int
            ),
            CPXgetx: sym!(
                b"CPXgetx\0",
                unsafe extern "C" fn(
                    ffi::CPXCENVptr,
                    ffi::CPXCLPptr,
                    *mut c_double,
                    c_int,
                    c_int,
                ) -> c_int
            ),
            CPXgetpi: sym!(
                b"CPXgetpi\0",
                unsafe extern "C" fn(
                    ffi::CPXCENVptr,
                    ffi::CPXCLPptr,
                    *mut c_double,
                    c_int,
                    c_int,
                ) -> c_int
            ),
            CPXgetdj: sym!(
                b"CPXgetdj\0",
                unsafe extern "C" fn(
                    ffi::CPXCENVptr,
                    ffi::CPXCLPptr,
                    *mut c_double,
                    c_int,
                    c_int,
                ) -> c_int
            ),
            CPXgetitcnt: sym!(
                b"CPXgetitcnt\0",
                unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int
            ),
            CPXgetnodecnt: sym!(
                b"CPXgetnodecnt\0",
                unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int
            ),
            CPXgetnumrows: sym!(
                b"CPXgetnumrows\0",
                unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int
            ),
            CPXgetnumcols: sym!(
                b"CPXgetnumcols\0",
                unsafe extern "C" fn(ffi::CPXCENVptr, ffi::CPXCLPptr) -> c_int
            ),
            _lib: lib,
        })
    }

    // ── CplexLpSolver ─────────────────────────────────────────────────────────

    /// IBM CPLEX LP/QP/MIP solver (commercial license required).
    ///
    /// Supports LP, QP (quadratic objective), and MILP/MIQP.
    /// Does not support general NLP; use the Gurobi, COPT or Ipopt backend for that.
    /// Loaded at runtime via libloading — no link-time dependency on libcplex.
    pub struct CplexLpSolver {
        env: ffi::CPXENVptr,
        lib: Arc<CplexLib>,
    }

    impl std::fmt::Debug for CplexLpSolver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CplexLpSolver").finish()
        }
    }

    // SAFETY: CPLEX is thread-safe for distinct (env, lp) pairs.
    unsafe impl Send for CplexLpSolver {}
    unsafe impl Sync for CplexLpSolver {}

    impl Drop for CplexLpSolver {
        fn drop(&mut self) {
            unsafe {
                (self.lib.CPXcloseCPLEX)(&mut self.env as *mut _);
            }
        }
    }

    impl CplexLpSolver {
        /// Open a CPLEX environment.
        ///
        /// Returns `Err` if `libcplex.so` is not found or no valid license is detected.
        pub fn new() -> Result<Self, String> {
            let lib = get_cplex()?.clone();
            let mut status: c_int = 0;
            let env = unsafe { (lib.CPXopenCPLEX)(&mut status) };
            if env.is_null() || status != 0 {
                return Err(format!(
                    "CPXopenCPLEX failed (status={status}). \
                     Check CPLEX 22.x installation and license. \
                     Only CPLEX 22.x Callable Library (libcplex.so) is supported."
                ));
            }
            Ok(Self { env, lib })
        }
    }

    impl LpSolver for CplexLpSolver {
        fn name(&self) -> &'static str {
            "CPLEX"
        }

        fn solve(&self, prob: &SparseProblem, opts: &LpOptions) -> Result<LpResult, String> {
            unsafe { solve_inner(&self.lib, self.env, prob, opts) }
        }
    }

    // ── Core solve implementation ─────────────────────────────────────────────

    /// Convert `SparseProblem` row bounds to CPLEX (rhs, sense, rngval) format.
    fn build_row_format(
        row_lower: &[f64],
        row_upper: &[f64],
    ) -> (Vec<c_double>, Vec<u8>, Vec<c_double>, bool) {
        let n = row_lower.len();
        let mut rhs = vec![0.0f64; n];
        let mut sense = vec![0u8; n];
        let mut rngval = vec![0.0f64; n];
        let mut has_range = false;

        for i in 0..n {
            let lb = row_lower[i];
            let ub = row_upper[i];
            if (ub - lb).abs() < 1e-14 * (ub.abs().max(1.0)) {
                // Equality
                sense[i] = ffi::SENSE_EQ;
                rhs[i] = ub;
            } else if lb <= -1e29 {
                // Upper bounded only (≤)
                sense[i] = ffi::SENSE_LE;
                rhs[i] = ub;
            } else if ub >= 1e29 {
                // Lower bounded only (≥)
                sense[i] = ffi::SENSE_GE;
                rhs[i] = lb;
            } else {
                // Range constraint: lb ≤ Ax ≤ ub
                // CPLEX range: rhs = ub, rngval = ub - lb (must be ≥ 0)
                sense[i] = ffi::SENSE_RG;
                rhs[i] = ub;
                rngval[i] = ub - lb;
                has_range = true;
            }
        }
        (rhs, sense, rngval, has_range)
    }

    /// Compute per-column nonzero counts from CSC column pointers.
    fn matcnt_from_start(a_start: &[i32], n_col: usize) -> Vec<c_int> {
        (0..n_col).map(|j| a_start[j + 1] - a_start[j]).collect()
    }

    unsafe fn solve_inner(
        lib: &CplexLib,
        env: ffi::CPXENVptr,
        prob: &SparseProblem,
        opts: &LpOptions,
    ) -> Result<LpResult, String> {
        unsafe {
            use ffi::*; // constants only (CPX_MIN, CPX_STAT_OPTIMAL, etc.)
            #[allow(non_snake_case)]
            let (
                CPXsetintparam,
                CPXsetdblparam,
                CPXcreateprob,
                CPXfreeprob,
                CPXcopylp,
                CPXcopyquad,
                CPXcopyctype,
                CPXlpopt,
                CPXmipopt,
                CPXgetstat,
                CPXgetobjval,
                CPXgetx,
                CPXgetpi,
                CPXgetdj,
                CPXgetitcnt,
            ) = (
                lib.CPXsetintparam,
                lib.CPXsetdblparam,
                lib.CPXcreateprob,
                lib.CPXfreeprob,
                lib.CPXcopylp,
                lib.CPXcopyquad,
                lib.CPXcopyctype,
                lib.CPXlpopt,
                lib.CPXmipopt,
                lib.CPXgetstat,
                lib.CPXgetobjval,
                lib.CPXgetx,
                lib.CPXgetpi,
                lib.CPXgetdj,
                lib.CPXgetitcnt,
            );

            // ── Configure environment parameters ─────────────────────────────────
            let print = c_int::from(opts.print_level > 0);
            CPXsetintparam(env, CPX_PARAM_SCRIND, print);
            CPXsetdblparam(env, CPX_PARAM_EPOPT, opts.tolerance.max(1e-10));
            CPXsetdblparam(env, CPX_PARAM_EPRHS, opts.tolerance.max(1e-10));
            if let Some(tl) = opts.time_limit_secs {
                CPXsetdblparam(env, CPX_PARAM_TILIM, tl);
            }

            // ── Create problem ────────────────────────────────────────────────────
            let prob_name = CString::new("surge").expect("static string contains no null bytes");
            let mut lp_status: c_int = 0;
            let lp = CPXcreateprob(env, &mut lp_status, prob_name.as_ptr());
            if lp.is_null() || lp_status != 0 {
                return Err(format!("CPXcreateprob failed (status={lp_status})"));
            }
            // Ensure `lp` is freed on all exit paths.
            struct LpGuard {
                env: ffi::CPXENVptr,
                lp: ffi::CPXLPptr,
                freeprob: unsafe extern "C" fn(ffi::CPXENVptr, *mut ffi::CPXLPptr) -> c_int,
            }
            impl Drop for LpGuard {
                fn drop(&mut self) {
                    unsafe {
                        (self.freeprob)(self.env, &mut self.lp as *mut _);
                    }
                }
            }
            let guard = LpGuard {
                env,
                lp,
                freeprob: CPXfreeprob,
            };
            let lp = guard.lp;

            // ── Convert constraint bounds to CPLEX format ─────────────────────────
            let (rhs, sense, rngval, has_range) =
                build_row_format(&prob.row_lower, &prob.row_upper);
            let sense_chars: Vec<c_char> = sense.iter().map(|&b| b as c_char).collect();
            let rngval_ptr = if has_range {
                rngval.as_ptr()
            } else {
                ptr::null()
            };

            // Column-wise nonzero counts.
            let matcnt = matcnt_from_start(&prob.a_start, prob.n_col);

            // ── Load LP ───────────────────────────────────────────────────────────
            let rc = CPXcopylp(
                env,
                lp,
                prob.n_col as c_int,
                prob.n_row as c_int,
                CPX_MIN,
                prob.col_cost.as_ptr(),
                rhs.as_ptr(),
                sense_chars.as_ptr(),
                prob.a_start.as_ptr(), // matbeg: a_start[0..n_col]
                matcnt.as_ptr(),
                prob.a_index.as_ptr(),
                prob.a_value.as_ptr(),
                prob.col_lower.as_ptr(),
                prob.col_upper.as_ptr(),
                rngval_ptr,
            );
            if rc != 0 {
                return Err(format!("CPXcopylp failed (rc={rc})"));
            }

            // ── Load quadratic objective (QP) ─────────────────────────────────────
            //
            // CPLEX minimizes `0.5 * x' Q x + c' x`.  The factor 0.5 is implicit;
            // pass q_value as-is (matching our SparseProblem convention).
            let is_qp = prob.q_start.is_some();
            if is_qp {
                let qs = prob
                    .q_start
                    .as_ref()
                    .expect("q_start Some when is_qp is true");
                let qi = prob
                    .q_index
                    .as_ref()
                    .expect("q_index Some when is_qp is true");
                let qv = prob
                    .q_value
                    .as_ref()
                    .expect("q_value Some when is_qp is true");
                let qmatcnt = matcnt_from_start(qs, prob.n_col);
                let rc = CPXcopyquad(
                    env,
                    lp,
                    qs.as_ptr(),
                    qmatcnt.as_ptr(),
                    qi.as_ptr(),
                    qv.as_ptr(),
                );
                if rc != 0 {
                    return Err(format!("CPXcopyquad failed (rc={rc})"));
                }
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
                    .map(|&v| cplex_col_type(v))
                    .collect();
                let rc = CPXcopyctype(env, lp, ctypes.as_ptr());
                if rc != 0 {
                    return Err(format!("CPXcopyctype failed (rc={rc})"));
                }
            }

            // ── Solve ─────────────────────────────────────────────────────────────
            let solve_rc = if is_mip {
                CPXmipopt(env, lp)
            } else {
                CPXlpopt(env, lp)
            };
            if solve_rc != 0 {
                return Err(format!(
                    "CPLEX solve failed (rc={solve_rc}). Check for numerical issues."
                ));
            }

            // ── Solution status ───────────────────────────────────────────────────
            let stat = CPXgetstat(env, lp);
            let status = if is_mip {
                match stat {
                    CPXMIP_OPTIMAL | CPXMIP_OPTIMAL_TOL => LpSolveStatus::Optimal,
                    CPXMIP_INFEASIBLE => LpSolveStatus::Infeasible,
                    CPXMIP_UNBOUNDED => LpSolveStatus::Unbounded,
                    _ => LpSolveStatus::SolverError(format!("CPLEX MIP status={stat}")),
                }
            } else {
                match stat {
                    CPX_STAT_OPTIMAL => LpSolveStatus::Optimal,
                    CPX_STAT_INFEASIBLE => LpSolveStatus::Infeasible,
                    CPX_STAT_UNBOUNDED => LpSolveStatus::Unbounded,
                    CPX_STAT_INF_OR_UNBD => LpSolveStatus::Infeasible,
                    _ => LpSolveStatus::SolverError(format!("CPLEX LP status={stat}")),
                }
            };

            if !matches!(status, LpSolveStatus::Optimal | LpSolveStatus::SubOptimal) {
                return Err(format!("CPLEX: {status:?}"));
            }

            // ── Extract solution ──────────────────────────────────────────────────
            let nc = prob.n_col as c_int;
            let nr = prob.n_row as c_int;

            let mut x = vec![0.0f64; prob.n_col];
            let rc = CPXgetx(env, lp, x.as_mut_ptr(), 0, nc - 1);
            if rc != 0 {
                return Err(format!("CPXgetx failed (rc={rc})"));
            }

            let mut objval: c_double = 0.0;
            CPXgetobjval(env, lp, &mut objval);

            let iterations = CPXgetitcnt(env, lp) as u32;

            // Duals are only available for LP/QP (not MIP).
            let (row_dual, col_dual) = if !is_mip && prob.n_row > 0 {
                let mut pi = vec![0.0f64; prob.n_row];
                CPXgetpi(env, lp, pi.as_mut_ptr(), 0, nr - 1);
                // CPLEX Pi is already standard Lagrange convention — store directly.
                let row_dual: Vec<f64> = pi;

                let mut dj = vec![0.0f64; prob.n_col];
                CPXgetdj(env, lp, dj.as_mut_ptr(), 0, nc - 1);
                (row_dual, dj)
            } else {
                (vec![0.0; prob.n_row], vec![0.0; prob.n_col])
            };

            Ok(LpResult {
                x,
                row_dual,
                col_dual,
                objective: objval,
                status,
                iterations,
            })
        }
    }
}
