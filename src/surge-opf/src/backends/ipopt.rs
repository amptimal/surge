// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Ipopt NLP solver backend via raw C FFI.
//!
//! Uses the Ipopt interior-point solver (EPL license) for nonlinear
//! optimization. Direct `extern "C"` calls via libloading — no link-time dependency.
//! The C API is defined in `IpStdCInterface.h` from coinor-libipopt-dev.

use libloading::Library;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock};
use tracing::{info, warn};

use super::{NlpSolver, SolverConcurrency};
use crate::nlp::{HessianMode, NlpOptions, NlpProblem, NlpSolution};

// ---------------------------------------------------------------------------
// Ipopt C FFI declarations
// ---------------------------------------------------------------------------

type IpoptProblemPtr = *mut c_void;
type Number = f64;
type Index = c_int;
type Bool = c_int;
type UserDataPtr = *mut c_void;

type EvalFCB = unsafe extern "C" fn(
    n: Index,
    x: *const Number,
    new_x: Bool,
    obj_value: *mut Number,
    user_data: UserDataPtr,
) -> Bool;

type EvalGradFCB = unsafe extern "C" fn(
    n: Index,
    x: *const Number,
    new_x: Bool,
    grad_f: *mut Number,
    user_data: UserDataPtr,
) -> Bool;

type EvalGCB = unsafe extern "C" fn(
    n: Index,
    x: *const Number,
    new_x: Bool,
    m: Index,
    g: *mut Number,
    user_data: UserDataPtr,
) -> Bool;

type EvalJacGCB = unsafe extern "C" fn(
    n: Index,
    x: *const Number,
    new_x: Bool,
    m: Index,
    nele_jac: Index,
    i_row: *mut Index,
    j_col: *mut Index,
    values: *mut Number,
    user_data: UserDataPtr,
) -> Bool;

type EvalHCB = unsafe extern "C" fn(
    n: Index,
    x: *const Number,
    new_x: Bool,
    obj_factor: Number,
    m: Index,
    lambda: *const Number,
    new_lambda: Bool,
    nele_hess: Index,
    i_row: *mut Index,
    j_col: *mut Index,
    values: *mut Number,
    user_data: UserDataPtr,
) -> Bool;

type IntermediateCB = unsafe extern "C" fn(
    alg_mod: Index,
    iter_count: Index,
    obj_value: Number,
    inf_pr: Number,
    inf_du: Number,
    mu: Number,
    d_norm: Number,
    regularization_size: Number,
    alpha_du: Number,
    alpha_pr: Number,
    ls_trials: Index,
    user_data: UserDataPtr,
) -> Bool;

// ---------------------------------------------------------------------------
// Ipopt libloading — runtime dynamic library detection
// ---------------------------------------------------------------------------

#[allow(non_snake_case)]
pub struct IpoptLib {
    _lib: Library,
    CreateIpoptProblem: unsafe extern "C" fn(
        Index,
        *const Number,
        *const Number,
        Index,
        *const Number,
        *const Number,
        Index,
        Index,
        Index,
        EvalFCB,
        EvalGCB,
        EvalGradFCB,
        EvalJacGCB,
        EvalHCB,
    ) -> IpoptProblemPtr,
    FreeIpoptProblem: unsafe extern "C" fn(IpoptProblemPtr),
    AddIpoptStrOption: unsafe extern "C" fn(IpoptProblemPtr, *const c_char, *const c_char) -> Bool,
    AddIpoptNumOption: unsafe extern "C" fn(IpoptProblemPtr, *const c_char, Number) -> Bool,
    AddIpoptIntOption: unsafe extern "C" fn(IpoptProblemPtr, *const c_char, Index) -> Bool,
    SetIntermediateCallback: unsafe extern "C" fn(IpoptProblemPtr, IntermediateCB) -> Bool,
    IpoptSolve: unsafe extern "C" fn(
        IpoptProblemPtr,
        *mut Number,
        *mut Number,
        *mut Number,
        *mut Number,
        *mut Number,
        *mut Number,
        UserDataPtr,
    ) -> c_int,
}

static IPOPT: OnceLock<Result<Arc<IpoptLib>, String>> = OnceLock::new();

/// Load (and cache) the Ipopt shared library.  Returns `Err` if not found.
pub fn get_ipopt() -> Result<&'static Arc<IpoptLib>, String> {
    IPOPT
        .get_or_init(|| {
            for path in ipopt_lib_paths() {
                if let Ok(lib) = unsafe { Library::new(&path) } {
                    match unsafe { load_ipopt_symbols(lib) } {
                        Ok(ilib) => return Ok(Arc::new(ilib)),
                        Err(e) => return Err(e),
                    }
                }
            }
            Err(
                "Ipopt not found — Surge requires the Ipopt C library (libipopt.so / \
                 libipopt.dylib), not the Python package. Install via your system package \
                 manager (brew install ipopt / apt install coinor-libipopt-dev) or build \
                 from source. `pip install cyipopt` does NOT bundle the shared library. \
                 Only Ipopt 3.x is supported. Set IPOPT_LIB_DIR to override the search path."
                    .to_string(),
            )
        })
        .as_ref()
        .map_err(|e| e.clone())
}

fn ipopt_lib_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Ok(dir) = std::env::var("IPOPT_LIB_DIR") {
        paths.push(std::path::PathBuf::from(format!("{dir}/libipopt.so")));
        paths.push(std::path::PathBuf::from(format!("{dir}/libipopt.dylib")));
        paths.push(std::path::PathBuf::from(format!("{dir}/ipopt.dll")));
    }
    // Custom build paths (our scripts install here) plus common Homebrew/macOS locations.
    for prefix in &[
        "/opt/ipopt-spral",
        "/opt/ipopt",
        "/opt/homebrew",
        "/usr/local",
        "/usr",
    ] {
        paths.push(std::path::PathBuf::from(format!(
            "{prefix}/lib/libipopt.so"
        )));
        paths.push(std::path::PathBuf::from(format!(
            "{prefix}/lib/libipopt.dylib"
        )));
    }
    // System/LD_LIBRARY_PATH
    paths.push(std::path::PathBuf::from("libipopt.so"));
    paths.push(std::path::PathBuf::from("libipopt.dylib"));
    paths
}

unsafe fn load_ipopt_symbols(lib: Library) -> Result<IpoptLib, String> {
    macro_rules! sym {
        ($name:literal, $ty:ty) => {
            *unsafe { lib.get::<$ty>($name) }
                .map_err(|e| format!("Ipopt symbol {} not found: {e}", stringify!($name)))?
        };
    }
    Ok(IpoptLib {
        CreateIpoptProblem: sym!(
            b"CreateIpoptProblem\0",
            unsafe extern "C" fn(
                Index,
                *const Number,
                *const Number,
                Index,
                *const Number,
                *const Number,
                Index,
                Index,
                Index,
                EvalFCB,
                EvalGCB,
                EvalGradFCB,
                EvalJacGCB,
                EvalHCB,
            ) -> IpoptProblemPtr
        ),
        FreeIpoptProblem: sym!(b"FreeIpoptProblem\0", unsafe extern "C" fn(IpoptProblemPtr)),
        AddIpoptStrOption: sym!(
            b"AddIpoptStrOption\0",
            unsafe extern "C" fn(IpoptProblemPtr, *const c_char, *const c_char) -> Bool
        ),
        AddIpoptNumOption: sym!(
            b"AddIpoptNumOption\0",
            unsafe extern "C" fn(IpoptProblemPtr, *const c_char, Number) -> Bool
        ),
        AddIpoptIntOption: sym!(
            b"AddIpoptIntOption\0",
            unsafe extern "C" fn(IpoptProblemPtr, *const c_char, Index) -> Bool
        ),
        SetIntermediateCallback: sym!(
            b"SetIntermediateCallback\0",
            unsafe extern "C" fn(IpoptProblemPtr, IntermediateCB) -> Bool
        ),
        IpoptSolve: sym!(
            b"IpoptSolve\0",
            unsafe extern "C" fn(
                IpoptProblemPtr,
                *mut Number,
                *mut Number,
                *mut Number,
                *mut Number,
                *mut Number,
                *mut Number,
                UserDataPtr,
            ) -> c_int
        ),
        _lib: lib,
    })
}

// ---------------------------------------------------------------------------
// User data wrapper
// ---------------------------------------------------------------------------

/// Wrapper to pass a `&dyn NlpProblem` (fat pointer, 16 bytes) through
/// Ipopt's `UserDataPtr` (thin pointer, 8 bytes). We store the fat pointer
/// in a struct and pass a thin pointer to that struct.
struct CallbackData<'a> {
    problem: &'a dyn NlpProblem,
    iterations: Cell<u32>,
    error: RefCell<Option<String>>,
}

/// Recover the callback data from Ipopt's user_data pointer.
#[inline]
unsafe fn get_callback_data<'a>(user_data: UserDataPtr) -> &'a CallbackData<'a> {
    unsafe { &*(user_data as *const CallbackData<'a>) }
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

fn record_callback_error(data: &CallbackData<'_>, callback: &str, payload: Box<dyn Any + Send>) {
    let mut slot = data.error.borrow_mut();
    if slot.is_none() {
        *slot = Some(format!(
            "{callback} panicked: {}",
            panic_payload_to_string(payload)
        ));
    }
}

// ---------------------------------------------------------------------------
// Callback trampolines
// ---------------------------------------------------------------------------

unsafe extern "C" fn eval_f_cb(
    n: Index,
    x: *const Number,
    _new_x: Bool,
    obj_value: *mut Number,
    user_data: UserDataPtr,
) -> Bool {
    let data = unsafe { get_callback_data(user_data) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let problem = data.problem;
        let x_slice = unsafe { std::slice::from_raw_parts(x, n as usize) };
        unsafe { *obj_value = problem.eval_objective(x_slice) };
    }));
    match result {
        Ok(()) => 1, // TRUE
        Err(panic) => {
            record_callback_error(data, "Ipopt eval_f", panic);
            0
        }
    }
}

unsafe extern "C" fn eval_grad_f_cb(
    n: Index,
    x: *const Number,
    _new_x: Bool,
    grad_f: *mut Number,
    user_data: UserDataPtr,
) -> Bool {
    let data = unsafe { get_callback_data(user_data) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let problem = data.problem;
        let x_slice = unsafe { std::slice::from_raw_parts(x, n as usize) };
        let grad_slice = unsafe { std::slice::from_raw_parts_mut(grad_f, n as usize) };
        problem.eval_gradient(x_slice, grad_slice);
    }));
    match result {
        Ok(()) => 1,
        Err(panic) => {
            record_callback_error(data, "Ipopt eval_grad_f", panic);
            0
        }
    }
}

unsafe extern "C" fn eval_g_cb(
    n: Index,
    x: *const Number,
    _new_x: Bool,
    m: Index,
    g: *mut Number,
    user_data: UserDataPtr,
) -> Bool {
    let data = unsafe { get_callback_data(user_data) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let problem = data.problem;
        let x_slice = unsafe { std::slice::from_raw_parts(x, n as usize) };
        let g_slice = unsafe { std::slice::from_raw_parts_mut(g, m as usize) };
        problem.eval_constraints(x_slice, g_slice);
    }));
    match result {
        Ok(()) => 1,
        Err(panic) => {
            record_callback_error(data, "Ipopt eval_g", panic);
            0
        }
    }
}

unsafe extern "C" fn eval_jac_g_cb(
    n: Index,
    x: *const Number,
    _new_x: Bool,
    _m: Index,
    nele_jac: Index,
    i_row: *mut Index,
    j_col: *mut Index,
    values: *mut Number,
    user_data: UserDataPtr,
) -> Bool {
    let data = unsafe { get_callback_data(user_data) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let problem = data.problem;

        if values.is_null() {
            // Structure request
            let (rows, cols) = problem.jacobian_structure();
            let i_row_slice = unsafe { std::slice::from_raw_parts_mut(i_row, nele_jac as usize) };
            let j_col_slice = unsafe { std::slice::from_raw_parts_mut(j_col, nele_jac as usize) };
            i_row_slice.copy_from_slice(&rows);
            j_col_slice.copy_from_slice(&cols);
        } else {
            // Values request
            let x_slice = unsafe { std::slice::from_raw_parts(x, n as usize) };
            let val_slice = unsafe { std::slice::from_raw_parts_mut(values, nele_jac as usize) };
            problem.eval_jacobian(x_slice, val_slice);
        }
    }));
    match result {
        Ok(()) => 1,
        Err(panic) => {
            record_callback_error(data, "Ipopt eval_jac_g", panic);
            0
        }
    }
}

unsafe extern "C" fn eval_h_cb(
    n: Index,
    x: *const Number,
    _new_x: Bool,
    obj_factor: Number,
    m: Index,
    lambda: *const Number,
    _new_lambda: Bool,
    nele_hess: Index,
    i_row: *mut Index,
    j_col: *mut Index,
    values: *mut Number,
    user_data: UserDataPtr,
) -> Bool {
    let data = unsafe { get_callback_data(user_data) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let problem = data.problem;

        if values.is_null() {
            let (rows, cols) = problem.hessian_structure();
            let i_row_slice = unsafe { std::slice::from_raw_parts_mut(i_row, nele_hess as usize) };
            let j_col_slice = unsafe { std::slice::from_raw_parts_mut(j_col, nele_hess as usize) };
            i_row_slice.copy_from_slice(&rows);
            j_col_slice.copy_from_slice(&cols);
        } else {
            let x_slice = unsafe { std::slice::from_raw_parts(x, n as usize) };
            let lambda_slice = unsafe { std::slice::from_raw_parts(lambda, m as usize) };
            let val_slice = unsafe { std::slice::from_raw_parts_mut(values, nele_hess as usize) };
            problem.eval_hessian(x_slice, obj_factor, lambda_slice, val_slice);
        }
    }));
    match result {
        Ok(()) => 1,
        Err(panic) => {
            record_callback_error(data, "Ipopt eval_h", panic);
            0
        }
    }
}

unsafe extern "C" fn intermediate_cb(
    _alg_mod: Index,
    iter_count: Index,
    _obj_value: Number,
    _inf_pr: Number,
    _inf_du: Number,
    _mu: Number,
    _d_norm: Number,
    _regularization_size: Number,
    _alpha_du: Number,
    _alpha_pr: Number,
    _ls_trials: Index,
    user_data: UserDataPtr,
) -> Bool {
    let data = unsafe { get_callback_data(user_data) };
    data.iterations.set(iter_count.max(0) as u32);
    1
}

// ---------------------------------------------------------------------------
// Public solver interface
// ---------------------------------------------------------------------------

/// Solve an NLP problem using Ipopt.
pub fn solve_ipopt(problem: &dyn NlpProblem, options: &NlpOptions) -> Result<NlpSolution, String> {
    let lib = get_ipopt()?;
    #[allow(non_snake_case)]
    let (
        CreateIpoptProblem,
        FreeIpoptProblem,
        AddIpoptStrOption,
        AddIpoptNumOption,
        AddIpoptIntOption,
        SetIntermediateCallback,
        IpoptSolve,
    ) = (
        lib.CreateIpoptProblem,
        lib.FreeIpoptProblem,
        lib.AddIpoptStrOption,
        lib.AddIpoptNumOption,
        lib.AddIpoptIntOption,
        lib.SetIntermediateCallback,
        lib.IpoptSolve,
    );

    // Linear solver: defaults to sequential MUMPS (fast, thread-safe, no special
    // env vars required). Override via IPOPT_LINEAR_SOLVER=spral for large problems
    // where SPRAL's parallel SSIDS may help (but note: SPRAL requires
    // OMP_CANCELLATION=TRUE set before OpenMP initialises).
    let n = problem.n_vars();
    let m = problem.n_constraints();
    info!(
        variables = n,
        constraints = m,
        tol = options.tolerance,
        max_iter = options.max_iterations,
        hessian_mode = ?options.hessian_mode,
        "Ipopt NLP: starting solve"
    );

    let (x_l, x_u) = problem.var_bounds();
    let (g_l, g_u) = problem.constraint_bounds();
    let (jac_rows, _jac_cols) = problem.jacobian_structure();
    let nele_jac = jac_rows.len();

    let (nele_hess, use_exact_hessian) =
        if options.hessian_mode == HessianMode::Exact && problem.has_hessian() {
            let (h_rows, _) = problem.hessian_structure();
            (h_rows.len(), true)
        } else {
            (0, false)
        };

    // Create Ipopt problem
    let ipopt = unsafe {
        CreateIpoptProblem(
            n as Index,
            x_l.as_ptr(),
            x_u.as_ptr(),
            m as Index,
            g_l.as_ptr(),
            g_u.as_ptr(),
            nele_jac as Index,
            nele_hess as Index,
            0, // C-style indexing
            eval_f_cb,
            eval_g_cb,
            eval_grad_f_cb,
            eval_jac_g_cb,
            eval_h_cb,
        )
    };

    if ipopt.is_null() {
        return Err("Failed to create Ipopt problem".into());
    }

    unsafe { SetIntermediateCallback(ipopt, intermediate_cb) };

    // Set options
    // Note: AddIpoptXxxOption takes non-const pointers in Ipopt 3.11.
    // CString::as_ptr() returns *const, but Ipopt doesn't actually mutate.
    let set_str = |key: &str, val: &str| {
        let k = CString::new(key).expect("Ipopt option key contains no null bytes");
        let v = CString::new(val).expect("Ipopt option value contains no null bytes");
        unsafe { AddIpoptStrOption(ipopt, k.as_ptr(), v.as_ptr()) };
    };

    let set_num = |key: &str, val: f64| {
        let k = CString::new(key).expect("Ipopt option key contains no null bytes");
        unsafe { AddIpoptNumOption(ipopt, k.as_ptr(), val) };
    };

    let set_int = |key: &str, val: i32| {
        let k = CString::new(key).expect("Ipopt option key contains no null bytes");
        unsafe { AddIpoptIntOption(ipopt, k.as_ptr(), val) };
    };

    // Suppress Ipopt's options file (PARAMS.DAT)
    set_str("option_file_name", "");

    // Suppress the Ipopt copyright banner so stdout only contains our JSON
    set_str("sb", "yes");

    set_num("tol", options.tolerance);
    set_int("max_iter", options.max_iterations as i32);
    set_int("print_level", options.print_level);

    // Allow the barrier method to operate near (and at) variable bounds.
    // Default is 1e-8 which keeps Ipopt strictly interior; on hard scenarios
    // where the optimum sits right at a voltage limit (e.g. winner V=1.05 at
    // multiple buses), the strict interior prevents convergence — the
    // barrier pushes V down, bus Q balance fails, and the NLP stalls.
    //
    // The value has to balance two competing failure modes:
    // * Too tight (≤1e-8): Ipopt refuses to hit envelope corners
    //   (V=V_max, P=P_max) and converges with residual penalty — the
    //   "strict interior" problem.
    // * Too loose (1e-2): variables escape bounds by ~1% of the bound
    //   magnitude. At V=1.05 that's ΔV≈0.01, which reshapes q_calc by
    //   V²·|B_ii| ≈ 0.01 pu on buses with heavy line charging (acl_075
    //   at bus_57 has b_ch=0.46 so B_ii≈−83, ΔQ≈0.008 pu). The exporter
    //   then clamps V to 1.05, leaving the validator with a phantom
    //   residual matching the V gap.
    //
    // 1e-6 is within Ipopt's default constr_viol_tol and roughly an
    // order of magnitude below our sced_ac_opf_tolerance=1e-3, so the
    // bound "float" is strictly smaller than the residual tolerance we
    // accept elsewhere — no measurable envelope escape on the test
    // cases while still permitting the barrier to approach bounds.
    set_num("bound_relax_factor", 1e-8);

    if !use_exact_hessian {
        set_str("hessian_approximation", "limited-memory");
        // L-BFGS converges slowly on dual residual. Configure acceptable
        // tolerances so Ipopt declares success when primal feasibility is
        // good even if dual hasn't fully converged.
        set_num("acceptable_tol", 1e-3);
        set_num("acceptable_dual_inf_tol", 10.0);
        set_num("acceptable_constr_viol_tol", 1e-4);
        set_num("acceptable_compl_inf_tol", 1e-2);
        set_int("acceptable_iter", 5);
        // More L-BFGS memory helps dual convergence
        set_int("limited_memory_max_history", 20);
    } else {
        // Exact Hessian mode: gradient-based scaling helps large HVDC problems.
        // Adaptive barrier reduces iteration count by 10-20% on medium/large networks.
        set_str("nlp_scaling_method", "gradient-based");
        set_str("mu_strategy", "adaptive");
        // Acceptable termination for near-converged iterates (avoids tail iterations).
        set_num("acceptable_tol", 1e-4);
        set_int("acceptable_iter", 10);
    }

    // Use sequential MUMPS linear solver — fast, thread-safe, no special env vars.
    // Override: set IPOPT_LINEAR_SOLVER=spral for parallel SSIDS on large problems
    // (requires OMP_CANCELLATION=TRUE before process start).
    let linear_solver =
        std::env::var("IPOPT_LINEAR_SOLVER").unwrap_or_else(|_| "mumps".to_string());
    set_str("linear_solver", &linear_solver);

    // Warm-start: tell Ipopt to initialise from the provided primal point.
    if options.warm_start {
        set_str("warm_start_init_point", "yes");
    }

    // Solve
    let mut x = problem.initial_point();
    let mut g = vec![0.0; m];
    let mut obj_val = 0.0;
    let mut mult_g = vec![0.0; m];
    let mut mult_x_l = vec![0.0; n];
    let mut mult_x_u = vec![0.0; n];

    // Store the fat pointer (problem ref) in a struct on the stack,
    // then pass a thin pointer to the struct through user_data.
    let cb_data = CallbackData {
        problem,
        iterations: Cell::new(0),
        error: RefCell::new(None),
    };
    let user_data_ptr = &cb_data as *const CallbackData as UserDataPtr;

    let status = unsafe {
        IpoptSolve(
            ipopt,
            x.as_mut_ptr(),
            g.as_mut_ptr(),
            &mut obj_val,
            mult_g.as_mut_ptr(),
            mult_x_l.as_mut_ptr(),
            mult_x_u.as_mut_ptr(),
            user_data_ptr,
        )
    };

    let callback_error = cb_data.error.borrow().clone();
    unsafe { FreeIpoptProblem(ipopt) };

    if let Some(err) = callback_error {
        return Err(err);
    }

    // Status 0 = Solve_Succeeded, 1 = Solved_To_Acceptable_Level
    let converged = status == 0 || status == 1;

    if status == 2 {
        warn!("Ipopt: infeasible problem detected");
        return Err("Ipopt: infeasible problem detected".into());
    }
    if status == -1 {
        warn!("Ipopt: maximum iterations exceeded");
        return Err("Ipopt: maximum iterations exceeded".into());
    }
    if status < -1 {
        warn!(status = status, "Ipopt: solver error");
        return Err(format!("Ipopt solver error (status={status})"));
    }

    info!(
        converged = converged,
        objective = obj_val,
        ipopt_status = status,
        "Ipopt NLP: solve complete"
    );

    Ok(NlpSolution {
        x,
        lambda: mult_g,
        z_lower: mult_x_l,
        z_upper: mult_x_u,
        objective: obj_val,
        iterations: Some(cb_data.iterations.get()),
        converged,
    })
}

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
    fn eval_f_cb_captures_panics() {
        let problem = PanicProblem;
        let data = CallbackData {
            problem: &problem,
            iterations: Cell::new(0),
            error: RefCell::new(None),
        };
        let mut obj_value = 123.0;
        let rc = unsafe {
            eval_f_cb(
                1,
                [0.0_f64].as_ptr(),
                1,
                &mut obj_value,
                &data as *const CallbackData as UserDataPtr,
            )
        };

        assert_eq!(rc, 0);
        assert_eq!(obj_value, 123.0);
        let err = data
            .error
            .borrow()
            .clone()
            .expect("callback error recorded");
        assert!(err.contains("Ipopt eval_f"));
        assert!(err.contains("boom"));
    }
}

// ---------------------------------------------------------------------------
// NlpSolver trait implementation
// ---------------------------------------------------------------------------

/// Ipopt interior-point NLP solver (EPL-2.0 license).
///
/// Loaded at runtime via libloading. Install `coinor-libipopt-dev` or
/// set `IPOPT_LIB_DIR` to the directory containing `libipopt.so`.
#[derive(Debug)]
pub struct IpoptNlpSolver;

impl IpoptNlpSolver {
    /// Create a new Ipopt solver, validating that `libipopt.so` is accessible.
    ///
    /// Returns `Err` if Ipopt is not found or cannot be loaded.
    pub fn new() -> Result<Self, String> {
        get_ipopt()?;
        Ok(Self)
    }
}

impl NlpSolver for IpoptNlpSolver {
    fn name(&self) -> &'static str {
        "Ipopt"
    }

    fn version(&self) -> &'static str {
        "3.14.20"
    }

    fn concurrency(&self) -> SolverConcurrency {
        // Ipopt's C API (IpStdCInterface.h) creates a distinct
        // `IpoptProblem` per solve, and all callback state (`CallbackData`)
        // is stack-local in `solve_ipopt`. The default linear solver
        // (sequential MUMPS) is reentrant. Concurrent solves are safe
        // provided each call uses its own `IpoptProblem` instance — which
        // our FFI path already guarantees.
        SolverConcurrency::ParallelSafe
    }

    fn solve(&self, problem: &dyn NlpProblem, opts: &NlpOptions) -> Result<NlpSolution, String> {
        solve_ipopt(problem, opts)
    }
}
