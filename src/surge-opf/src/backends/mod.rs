// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Pluggable solver backend traits for LP/QP/MIP and NLP problems.
//!
//! # LP/QP/MIP backend (`LpSolver`)
//!
//! Solves problems of the form:
//! ```text
//! min  0.5 * x'Qx + c'x
//! s.t. row_lower ≤ Ax ≤ row_upper
//!      col_lower ≤ x   ≤ col_upper
//!      x[i] ∈ {0,1}  for each i where integrality[i] = Binary
//!      x[i] ∈ Z      for each i where integrality[i] = Integer
//! ```
//! The constraint matrix A is in CSC (Compressed Sparse Column) format.
//!
//! # NLP backend (`NlpSolver`)
//!
//! Wraps `NlpProblem` (already well-designed in `nlp.rs`) for use by AC-OPF.
//!
//! # Default resolution
//!
//! Call `try_default_lp_solver` / `try_default_nlp_solver` to get the best available solver.
//! All backends are always compiled; availability is detected at runtime via libloading.
//! Pass `None` in options structs to use the default; pass `Some(Arc<dyn ...>)` to override.
//!
//! # Solver priority (runtime detection order)
//!
//! LP/QP/MIP: Gurobi → COPT → CPLEX → HiGHS
//!
//! NLP (generic): COPT (if shim built) → Ipopt
//! NLP (AC-OPF):  COPT (if shim built) → Ipopt → Gurobi
//!
//! Gurobi NLP is intentionally excluded from the generic NLP pool because it
//! requires native dispatch through `solve_ac_opf()` rather than the generic
//! `NlpSolver::solve()` trait. Use `try_default_ac_opf_nlp_solver` or
//! `ac_opf_nlp_solver_from_str` when the caller is specifically selecting an
//! AC-OPF runtime backend.
//!
//! All solvers (including HiGHS) are discovered at runtime via libloading.
//! If no LP solver is found, an error with install instructions is returned.

use std::sync::{Arc, Mutex, OnceLock};

pub use crate::nlp::{NlpOptions, NlpProblem, NlpSolution};

// Submodule declarations — each implements one solver backend.
// All backends are always compiled; availability is detected at runtime via libloading.
/// Clarabel conic interior-point solver (SOCP/SDP).
pub mod clarabel;
/// COPT commercial LP/QP/MIP and NLP backend.
pub mod copt;
/// CPLEX commercial LP/QP/MIP backend.
pub mod cplex;
/// Gurobi commercial LP/QP/MIP and NLP backend.
pub mod gurobi;
/// HiGHS open-source LP/QP/MIP backend (loaded at runtime via libloading).
pub mod highs;
/// Ipopt open-source NLP backend (default for AC-OPF).
pub mod ipopt;

// ---------------------------------------------------------------------------
// LP/QP/MIP types
// ---------------------------------------------------------------------------

/// Sparse LP/QP/MIP problem in canonical form.
///
/// Objective: `min  0.5 * x' Q x + c' x`
/// Constraints: `row_lower ≤ A x ≤ row_upper`, `col_lower ≤ x ≤ col_upper`
/// Variable domain:
/// - `Continuous` → continuous variable
/// - `Binary` → binary variable
/// - `Integer` → general integer variable
///
/// The constraint matrix A and Hessian Q are in CSC format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableDomain {
    Continuous,
    Binary,
    Integer,
}

#[derive(Debug, Clone)]
pub struct SparseProblem {
    /// Number of decision variables (columns).
    pub n_col: usize,
    /// Number of constraints (rows).
    pub n_row: usize,
    /// Linear objective coefficients.
    pub col_cost: Vec<f64>,
    /// Variable lower bounds.
    pub col_lower: Vec<f64>,
    /// Variable upper bounds.
    pub col_upper: Vec<f64>,
    /// Constraint lower bounds.
    pub row_lower: Vec<f64>,
    /// Constraint upper bounds.
    pub row_upper: Vec<f64>,
    /// CSC column pointers for A (length n_col + 1).
    pub a_start: Vec<i32>,
    /// CSC row indices for A.
    pub a_index: Vec<i32>,
    /// CSC nonzero values for A.
    pub a_value: Vec<f64>,
    /// Upper-triangle CSC Hessian Q (for QP; None = LP).
    pub q_start: Option<Vec<i32>>,
    pub q_index: Option<Vec<i32>>,
    pub q_value: Option<Vec<f64>>,
    /// Variable domains (None = pure LP/QP with all-continuous variables).
    pub integrality: Option<Vec<VariableDomain>>,
}

/// Solve status returned by `LpSolver`.
#[derive(Debug, Clone, PartialEq)]
pub enum LpSolveStatus {
    /// Solver found a proven optimal solution.
    Optimal,
    /// Solver found a feasible solution but cannot prove optimality (e.g. MIP gap).
    SubOptimal,
    /// The problem is infeasible (no solution satisfies all constraints).
    Infeasible,
    /// The problem is unbounded (objective can be made arbitrarily small).
    Unbounded,
    /// Solver encountered an internal error.
    SolverError(String),
}

/// Solution from an `LpSolver`.
#[derive(Debug, Clone)]
pub struct LpResult {
    /// Optimal variable values.
    pub x: Vec<f64>,
    /// Row duals in standard Lagrange multiplier convention.
    ///
    /// For a minimization problem:
    /// - Equality constraint dual: positive when the RHS increase would
    ///   increase the objective.
    /// - Binding `≤` constraint: positive (relaxing the upper bound
    ///   would decrease the objective).
    /// - Binding `≥` constraint: positive (tightening the lower bound
    ///   would increase the objective).
    ///
    /// LMP extraction: `lmp[i] = row_dual[balance_row_i] / base_mva`.
    pub row_dual: Vec<f64>,
    /// Column (variable bound) duals.
    pub col_dual: Vec<f64>,
    /// Optimal objective value.
    pub objective: f64,
    /// Solver termination status.
    pub status: LpSolveStatus,
    /// Number of solver iterations.
    pub iterations: u32,
}

/// Options for `LpSolver`.
#[derive(Debug, Clone)]
pub struct LpOptions {
    /// Feasibility + optimality tolerance (default 1e-8).
    pub tolerance: f64,
    /// Optional wall-clock time limit.
    pub time_limit_secs: Option<f64>,
    /// Print level: 0 = silent, 1+ = verbose.
    pub print_level: u8,
}

impl Default for LpOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-8,
            time_limit_secs: None,
            print_level: 0,
        }
    }
}

/// Concurrency contract for a solver backend instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolverConcurrency {
    /// Distinct solves may execute concurrently on the same backend instance.
    ParallelSafe,
    /// Distinct solves must be serialized process-wide.
    Serialized,
}

static SERIALIZED_NLP_SOLVE_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Solver traits
// ---------------------------------------------------------------------------

/// Pluggable LP/QP/MIP backend.
///
/// Implementors: `HiGHSLpSolver` (default, open-source),
/// `CoptLpSolver`, `CplexLpSolver`, `GurobiLpSolver`.
pub trait LpSolver: Send + Sync + std::fmt::Debug {
    /// Solve the given LP/QP/MIP problem with the specified options.
    fn solve(&self, prob: &SparseProblem, opts: &LpOptions) -> Result<LpResult, String>;
    /// Human-readable solver name (e.g. `"HiGHS"`, `"Gurobi"`).
    fn name(&self) -> &'static str;
    /// Solver version string.
    fn version(&self) -> &'static str {
        "unknown"
    }
    /// Whether this backend supports mixed-integer (binary/integer) variables.
    fn supports_mip(&self) -> bool {
        true
    }
}

/// Pluggable NLP backend.
///
/// Implementors: `IpoptNlpSolver` (default),
/// `CoptNlpSolver` (optional NLP shim),
/// `GurobiNlpSolver` (native expression-tree NLP).
pub trait NlpSolver: Send + Sync + std::fmt::Debug {
    /// Solve the given nonlinear programming problem with the specified options.
    fn solve(&self, problem: &dyn NlpProblem, opts: &NlpOptions) -> Result<NlpSolution, String>;
    /// Human-readable solver name (e.g. `"Ipopt"`, `"COPT"`).
    fn name(&self) -> &'static str;
    /// Solver version string.
    fn version(&self) -> &'static str {
        "unknown"
    }
    /// Whether this backend may run multiple solves concurrently.
    fn concurrency(&self) -> SolverConcurrency {
        SolverConcurrency::ParallelSafe
    }
    /// Return `self` as `&dyn Any` so callers can downcast to concrete types.
    ///
    /// Implementors that want native dispatch (e.g. `GurobiNlpSolver`) must
    /// override this to return `self`.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
}

/// Execute an NLP solve under the backend's declared concurrency policy.
pub fn run_nlp_solver_with_policy<T, E>(
    solver: &dyn NlpSolver,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    match solver.concurrency() {
        SolverConcurrency::ParallelSafe => f(),
        SolverConcurrency::Serialized => {
            let mutex = SERIALIZED_NLP_SOLVE_MUTEX.get_or_init(|| Mutex::new(()));
            let _guard = mutex.lock().unwrap_or_else(|e| e.into_inner());
            f()
        }
    }
}

// ---------------------------------------------------------------------------
// Default solver resolution
// ---------------------------------------------------------------------------

/// Return the default LP solver, detecting availability at runtime.
///
/// Controlled by the `SURGE_LP_SOLVER` environment variable:
/// - **unset / `"highs"`** — HiGHS (loaded at runtime via libhighs.{so,dylib})
/// - **`"auto"`** — probe Gurobi → COPT → CPLEX → HiGHS at runtime
/// - **`"gurobi"` / `"copt"` / `"cplex"`** — use that solver directly
///
/// Returns `Err` if the requested solver (or any solver, in auto mode) is not found.
pub fn try_default_lp_solver() -> Result<Arc<dyn LpSolver>, String> {
    let choice = std::env::var("SURGE_LP_SOLVER")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match choice.as_str() {
        "auto" => probe_lp_solvers(),
        "" | "highs" => {
            let s = self::highs::HiGHSLpSolver::new()?;
            tracing::info!("LP solver: HiGHS (open-source)");
            Ok(Arc::new(s))
        }
        name => lp_solver_from_str(name),
    }
}

/// Probe solvers at runtime.
///
/// Priority (first whose shared library is found at runtime wins):
/// 1. **Gurobi** — `GUROBI_HOME` env or `/opt/gurobi*/linux64/lib/libgurobi130.so`
/// 2. **COPT** — `COPT_HOME` env or `/opt/copt80/lib/libcopt.so`
/// 3. **CPLEX** — `CPLEX_STUDIO_HOME` env or system `libcplex.so`
/// 4. **HiGHS** — `HIGHS_LIB_DIR` env or system `libhighs.{so,dylib}`
///
/// **Warning**: `dlopen` of a commercial library compiled for a different CPU
/// micro-architecture can trigger SIGILL (uncatchable).  Use only when you
/// know the installed libraries are compatible with the current CPU.
fn probe_lp_solvers() -> Result<Arc<dyn LpSolver>, String> {
    match self::gurobi::GurobiLpSolver::new_validated() {
        Ok(s) => {
            tracing::info!("LP solver: Gurobi (commercial)");
            return Ok(Arc::new(s));
        }
        Err(e) => tracing::debug!("Gurobi not available ({}); trying next", e),
    }

    match self::copt::CoptLpSolver::new() {
        Ok(s) => {
            tracing::info!("LP solver: COPT (commercial)");
            return Ok(Arc::new(s));
        }
        Err(e) => tracing::debug!("COPT not available ({}); trying next", e),
    }

    match self::cplex::CplexLpSolver::new() {
        Ok(s) => {
            tracing::info!("LP solver: CPLEX (commercial)");
            return Ok(Arc::new(s));
        }
        Err(e) => tracing::debug!("CPLEX not available ({}); trying next", e),
    }

    match self::highs::HiGHSLpSolver::new() {
        Ok(s) => {
            tracing::info!("LP solver: HiGHS (open-source)");
            return Ok(Arc::new(s));
        }
        Err(e) => tracing::debug!("HiGHS not available ({})", e),
    }

    Err(
        "No LP solver found — install HiGHS (brew install highs / apt install libhighs-dev), \
         or a commercial solver (Gurobi, COPT, CPLEX). \
         See https://github.com/amptimal/surge#solvers"
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// QCQP types
// ---------------------------------------------------------------------------

/// A single quadratic constraint: Σ Q_ij·x_i·x_j + a'x {sense} rhs.
///
/// `q_row`/`q_col`/`q_val` are in **lower-triangular triplet format** (row ≥ col).
/// - Diagonal entry `(i, i, v)`: contributes `v · x_i²`.
/// - Off-diagonal `(i, j, v)` with `i > j`: contributes `v · x_i · x_j` (once).
#[derive(Debug, Clone)]
pub struct QuadConstraint {
    /// Quadratic term row indices (lower triangle: row >= col).
    pub q_row: Vec<i32>,
    /// Quadratic term col indices.
    pub q_col: Vec<i32>,
    /// Quadratic term values.
    pub q_val: Vec<f64>,
    /// Sparse linear term variable indices.
    pub lin_idx: Vec<i32>,
    /// Sparse linear term values.
    pub lin_val: Vec<f64>,
    /// Constraint sense: `b'L'` (≤), `b'G'` (≥), `b'E'` (=).
    pub sense: u8,
    /// Right-hand side.
    pub rhs: f64,
}

/// QCQP = `SparseProblem` (QP objective + linear constraints) + quadratic constraints.
///
/// Objective: `min  0.5 * x' Q x + c' x`
/// Linear constraints: `row_lower ≤ A x ≤ row_upper`
/// Quadratic constraints: for each `c` in `quad_constraints`:
///   `Σ c.Q_ij · x_i · x_j + c.lin' x  {c.sense}  c.rhs`
#[derive(Debug, Clone)]
pub struct QcqpProblem {
    /// Linear part (objective + linear constraints + variable bounds).
    pub base: SparseProblem,
    /// Quadratic constraints appended on top of the linear constraints.
    pub quad_constraints: Vec<QuadConstraint>,
}

/// Solution from a [`QcqpSolver`].
#[derive(Debug, Clone)]
pub struct QcqpResult {
    /// Optimal variable values.
    pub x: Vec<f64>,
    /// Optimal objective value.
    pub objective: f64,
    /// Solver termination status.
    pub status: LpSolveStatus,
}

/// Pluggable QCQP solver backend.
///
/// Implementors: `GurobiQcqpSolver`, `CoptQcqpSolver`.
pub trait QcqpSolver: Send + Sync + std::fmt::Debug {
    /// Solve the given QCQP problem with the specified options.
    fn solve(&self, prob: &QcqpProblem, opts: &LpOptions) -> Result<QcqpResult, String>;
    /// Human-readable solver name (e.g. `"Gurobi"`, `"COPT"`).
    fn name(&self) -> &'static str;
}

/// Return the default QCQP solver, detecting availability at runtime.
///
/// Priority: Gurobi (if `libgurobi130.so` found) → COPT (if `libcopt.so` found) → None.
pub fn default_qcqp_solver() -> Option<Arc<dyn QcqpSolver>> {
    if let Ok(s) = self::gurobi::GurobiQcqpSolver::new() {
        return Some(Arc::new(s));
    }
    if let Ok(s) = self::copt::CoptQcqpSolver::new() {
        return Some(Arc::new(s));
    }
    None
}

/// Return the default **generic** NLP solver, detecting availability at runtime.
///
/// Priority (first whose shared library is found wins):
/// 1. **COPT NLP** — if the standalone Surge shim can be loaded and `libcopt` is found
/// 2. **Ipopt** — if `libipopt.so` found
///
/// Gurobi NLP is intentionally excluded from this generic pool because it
/// cannot be invoked through the generic `NlpSolver::solve()` interface.
///
/// Returns `Err` if no NLP solver is available, with install instructions.
pub fn try_default_nlp_solver() -> Result<Arc<dyn NlpSolver>, String> {
    let choice = std::env::var("SURGE_NLP_SOLVER")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match choice.as_str() {
        "auto" | "" => probe_nlp_solvers(),
        name => nlp_solver_from_str(name),
    }
}

/// Probe generic NLP solvers at runtime.
///
/// Priority: COPT → Ipopt.
fn probe_nlp_solvers() -> Result<Arc<dyn NlpSolver>, String> {
    match self::copt::CoptNlpSolver::new() {
        Ok(s) => {
            tracing::info!("NLP solver: COPT (commercial)");
            return Ok(Arc::new(s));
        }
        Err(e) => tracing::debug!("COPT NLP not available ({}); trying next", e),
    }

    match self::ipopt::IpoptNlpSolver::new() {
        Ok(s) => {
            tracing::info!("NLP solver: Ipopt (open-source)");
            return Ok(Arc::new(s));
        }
        Err(e) => tracing::debug!("Ipopt not available ({}); trying next", e),
    }

    Err(
        "No generic NLP solver found. Install Ipopt (libipopt.so), or COPT 8.x with \
         the standalone Surge NLP shim (run scripts/build-copt-nlp-shim.sh or \
         use a surge-py wheel built with COPT_HOME). \
         See https://github.com/amptimal/surge#solvers"
            .to_string(),
    )
}

/// Return the default AC-OPF NLP solver, detecting availability at runtime.
///
/// Priority (first whose shared library is found wins):
/// 1. **COPT NLP** — if the standalone Surge shim can be loaded and `libcopt` is found
/// 2. **Ipopt** — if `libipopt.so` found
/// 3. **Gurobi NLP** — if `libgurobi130.so` is found and its runtime can be initialized
pub fn try_default_ac_opf_nlp_solver() -> Result<Arc<dyn NlpSolver>, String> {
    let choice = std::env::var("SURGE_NLP_SOLVER")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match choice.as_str() {
        "auto" | "" => probe_ac_opf_nlp_solvers(),
        name => ac_opf_nlp_solver_from_str(name),
    }
}

/// Probe AC-OPF-capable NLP solvers at runtime.
///
/// Priority: COPT → Ipopt → Gurobi.
fn probe_ac_opf_nlp_solvers() -> Result<Arc<dyn NlpSolver>, String> {
    match self::copt::CoptNlpSolver::new() {
        Ok(s) => {
            tracing::info!("AC-OPF NLP solver: COPT (commercial)");
            return Ok(Arc::new(s));
        }
        Err(e) => tracing::debug!("COPT NLP not available ({}); trying next", e),
    }

    match self::ipopt::IpoptNlpSolver::new() {
        Ok(s) => {
            tracing::info!("AC-OPF NLP solver: Ipopt (open-source)");
            return Ok(Arc::new(s));
        }
        Err(e) => tracing::debug!("Ipopt not available ({}); trying next", e),
    }

    match self::gurobi::GurobiNlpSolver::new_validated() {
        Ok(s) => {
            tracing::info!("AC-OPF NLP solver: Gurobi (commercial)");
            return Ok(Arc::new(s));
        }
        Err(e) => tracing::debug!("Gurobi NLP not available ({})", e),
    }

    Err(
        "No AC-OPF NLP solver found. Install Ipopt (libipopt.so), or COPT 8.x with \
         the standalone Surge NLP shim (run scripts/build-copt-nlp-shim.sh or \
         use a surge-py wheel built with COPT_HOME), or Gurobi 13.x. \
         See https://github.com/amptimal/surge#solvers"
            .to_string(),
    )
}

/// Instantiate a named LP solver backend.
///
/// `name` is case-insensitive. Valid values:
/// - `"default"` — same as [`try_default_lp_solver()`]
/// - `"highs"`   — HiGHS (loaded at runtime; requires libhighs.{so,dylib})
/// - `"gurobi"`  — Gurobi (loaded at runtime; requires libgurobi130.so + license)
/// - `"copt"`    — COPT (loaded at runtime; requires libcopt.so + license)
/// - `"cplex"`   — CPLEX (loaded at runtime; requires libcplex.so + license)
pub fn lp_solver_from_str(name: &str) -> Result<Arc<dyn LpSolver>, String> {
    match name.to_ascii_lowercase().as_str() {
        "default" => try_default_lp_solver(),
        "highs" => self::highs::HiGHSLpSolver::new().map(|s| Arc::new(s) as Arc<dyn LpSolver>),
        "gurobi" => {
            self::gurobi::GurobiLpSolver::new_validated().map(|s| Arc::new(s) as Arc<dyn LpSolver>)
        }
        "copt" => self::copt::CoptLpSolver::new().map(|s| Arc::new(s) as Arc<dyn LpSolver>),
        "cplex" => self::cplex::CplexLpSolver::new().map(|s| Arc::new(s) as Arc<dyn LpSolver>),
        other => Err(format!(
            "unknown lp_solver {other:?}; valid choices: \
             'default', 'highs', 'gurobi', 'copt', 'cplex'"
        )),
    }
}

/// Instantiate a named NLP solver backend.
///
/// `name` is case-insensitive. Valid values:
/// - `"default"` — same as [`try_default_nlp_solver()`]
/// - `"copt"`    — COPT NLP (loaded at runtime; requires libcopt + the standalone Surge shim)
/// - `"ipopt"`   — Ipopt (loaded at runtime; requires libipopt.so)
pub fn nlp_solver_from_str(name: &str) -> Result<Arc<dyn NlpSolver>, String> {
    match name.to_ascii_lowercase().as_str() {
        "default" => try_default_nlp_solver(),
        "copt" => self::copt::CoptNlpSolver::new().map(|s| Arc::new(s) as Arc<dyn NlpSolver>),
        "ipopt" => self::ipopt::IpoptNlpSolver::new().map(|s| Arc::new(s) as Arc<dyn NlpSolver>),
        "gurobi" => Err(
            "generic NLP backend 'gurobi' is not supported; use the AC-OPF-specific backend selection path instead"
                .to_string(),
        ),
        other => Err(format!(
            "unknown nlp_solver {other:?}; valid choices: \
             'default', 'copt', 'ipopt'"
        )),
    }
}

/// Instantiate a named AC-OPF NLP solver backend.
///
/// This is the solver-selection surface for [`crate::ac::solve::solve_ac_opf`].
/// It includes native Gurobi AC-OPF because that path bypasses the generic
/// `NlpSolver::solve()` trait.
pub fn ac_opf_nlp_solver_from_str(name: &str) -> Result<Arc<dyn NlpSolver>, String> {
    match name.to_ascii_lowercase().as_str() {
        "default" => try_default_ac_opf_nlp_solver(),
        "gurobi" => self::gurobi::GurobiNlpSolver::new_validated()
            .map(|s| Arc::new(s) as Arc<dyn NlpSolver>),
        "copt" => self::copt::CoptNlpSolver::new().map(|s| Arc::new(s) as Arc<dyn NlpSolver>),
        "ipopt" => self::ipopt::IpoptNlpSolver::new().map(|s| Arc::new(s) as Arc<dyn NlpSolver>),
        other => Err(format!(
            "unknown ac_opf nlp_solver {other:?}; valid choices: \
             'default', 'gurobi', 'copt', 'ipopt'"
        )),
    }
}
