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

use serde::{Deserialize, Serialize};

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
pub mod reduce;

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
    /// Optional human-readable variable names aligned with columns.
    pub col_names: Option<Vec<String>>,
    /// Optional human-readable constraint names aligned with rows.
    pub row_names: Option<Vec<String>>,
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
    /// MIP progress trace. Populated when the solve was a MIP, the caller
    /// passed `LpOptions::mip_gap_schedule = Some(..)`, and the backend
    /// supports progress callbacks. `None` otherwise — including for LP
    /// solves, backends without callback support, and MIP solves where no
    /// schedule was supplied.
    pub mip_trace: Option<MipTrace>,
}

/// Time-varying MIP gap target: piecewise-constant "by time `t_secs` from solve
/// start, the caller will accept an optimality gap of `gap`".
///
/// The schedule is evaluated as a step function over entries sorted by
/// `t_secs`. At solve time `t`, the target gap is the `gap` of the latest
/// entry whose `t_secs <= t`. Before the first entry the schedule provides
/// no target (i.e. the solver keeps going). Once the solver finds an
/// incumbent whose gap is within the current target, solver-specific
/// callback machinery (see [`MipProgressMonitor`]) terminates the solve.
///
/// The schedule is solver-agnostic: each backend that supports MIP progress
/// callbacks (Gurobi first, HiGHS to follow) hooks its callback into the
/// shared [`MipProgressMonitor`].
///
/// Typical use: front-load a tight gap for the first few seconds, loosen
/// it as time runs out. Example:
/// ```ignore
/// MipGapSchedule::new(vec![
///     (0.0,  1e-5),  // first 10s: prove near-optimal
///     (10.0, 1e-4),  //  10-20s: 0.01% gap
///     (20.0, 1e-3),  //  20-30s: 0.1% gap
///     (30.0, 1e-2),  //  30-45s: 1% gap
///     (45.0, 1e-1),  //  45s+:   accept 10% gap as safety net
/// ])
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MipGapSchedule {
    /// Breakpoints `(t_secs, gap)` sorted by `t_secs` ascending.
    /// All `t_secs >= 0` and all `gap >= 0`; constructed via `new` which
    /// sorts and validates.
    pub breakpoints: Vec<(f64, f64)>,
}

impl MipGapSchedule {
    /// Construct and validate. Entries are sorted by `t_secs` ascending.
    /// Returns `Err` when the list is empty or contains a non-finite /
    /// negative entry. Duplicate `t_secs` values are allowed (last one wins
    /// in ties).
    pub fn new(mut breakpoints: Vec<(f64, f64)>) -> Result<Self, String> {
        if breakpoints.is_empty() {
            return Err("MipGapSchedule requires at least one breakpoint".to_string());
        }
        for (t, g) in &breakpoints {
            if !t.is_finite() || *t < 0.0 {
                return Err(format!("MipGapSchedule: invalid time_secs={t}"));
            }
            if !g.is_finite() || *g < 0.0 {
                return Err(format!("MipGapSchedule: invalid gap={g}"));
            }
        }
        breakpoints.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("finite t_secs"));
        Ok(Self { breakpoints })
    }

    /// Target gap at wall time `t_secs`. Returns `None` before the first
    /// entry's `t_secs` (caller keeps solving).
    pub fn target_at(&self, t_secs: f64) -> Option<f64> {
        let mut target = None;
        for (bt, bg) in &self.breakpoints {
            if *bt <= t_secs {
                target = Some(*bg);
            } else {
                break;
            }
        }
        target
    }

    /// Largest gap any step of the schedule ever accepts.
    ///
    /// Not safe to pass as the solver's static `MIPGap` — doing so would
    /// let the solver auto-terminate at the *loosest* gap the schedule
    /// contemplates, which almost always fires long before the
    /// callback's tighter early targets can. Use [`Self::min_gap`] for
    /// the static safety net.
    pub fn max_gap(&self) -> f64 {
        self.breakpoints
            .iter()
            .map(|(_, g)| *g)
            .fold(0.0_f64, f64::max)
    }

    /// Tightest gap any step of the schedule ever requires — the correct
    /// floor for the solver's static `MIPGap` parameter.
    ///
    /// Rationale: the progress callback enforces the schedule (tight
    /// early, looser later) by calling `GRBterminate` when the current
    /// gap is within the target at wall time `t`. The solver's own
    /// `MIPGap` check runs on every node / incumbent update and races
    /// the callback. Setting `MIPGap = max_gap` would let the solver
    /// auto-exit at the loosest target the schedule will ever accept
    /// (e.g., 10 %) well before the callback's tight early target
    /// (e.g., 1e-5 at t=0) can fire. Setting `MIPGap = min_gap`
    /// restricts the solver's auto-termination to gaps the callback
    /// would also approve at the very start of the schedule — a true
    /// fallback rather than a short-circuit. `TimeLimit` remains the
    /// absolute wall.
    pub fn min_gap(&self) -> f64 {
        self.breakpoints
            .iter()
            .map(|(_, g)| *g)
            .fold(f64::INFINITY, f64::min)
    }

    /// Time (seconds) of the latest breakpoint.
    pub fn final_deadline_secs(&self) -> f64 {
        self.breakpoints.last().map(|(t, _)| *t).unwrap_or_default()
    }
}

/// Kind of a recorded [`MipEvent`]. Used for downstream reporting; not
/// consumed by the solver itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MipEventKind {
    /// Callback observed a strictly better incumbent than the last tick.
    NewIncumbent,
    /// Callback observed a strictly tighter best bound than the last tick.
    BoundImproved,
    /// Wall time crossed a schedule breakpoint (target gap loosened).
    BreakpointCrossed,
    /// Caller requested solver termination because the current gap met
    /// the scheduled target. This is always the last event in a successful
    /// trace.
    ScheduleTerminate,
}

/// One observation from the MIP progress callback. See [`MipTrace`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MipEvent {
    /// Seconds from solve start.
    pub t_secs: f64,
    /// Best known primal objective at the callback.
    pub incumbent_obj: f64,
    /// Best known dual bound at the callback.
    pub best_bound: f64,
    /// Computed relative gap = `|incumbent - bound| / (|incumbent| + 1e-10)`.
    pub gap: f64,
    /// Target gap from the schedule at this wall time. `NaN` when the
    /// schedule has not yet started (no entry with `t_secs <= event.t`).
    pub target_gap: f64,
    /// What triggered the event.
    pub kind: MipEventKind,
}

/// How the MIP solve terminated, captured for downstream reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MipTerminationReason {
    /// Solver proved optimality (or equivalent native-status optimal).
    Optimal,
    /// Our callback tripped `GRBterminate` / equivalent because the
    /// current gap met the scheduled target.
    ScheduleGap,
    /// Solver hit its own wall-clock time limit.
    TimeLimit,
    /// Solver reported infeasibility.
    Infeasible,
    /// Anything else (including sub-optimal without schedule, solver
    /// errors). Callers inspect `LpResult::status` for the precise code.
    Other,
}

/// Trace + post-solve telemetry for a MIP solve.
///
/// Populated by MIP-capable backends on every MIP solve. The progress-
/// event stream (`events`) and the echoed `schedule` are only filled in
/// when the caller supplied a [`MipGapSchedule`] and the backend supports
/// progress callbacks (Gurobi today). Size/node/gap stats are filled in
/// unconditionally when the backend has them available; LP-only solves
/// and backends that can't report stats leave `LpResult::mip_trace =
/// None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MipTrace {
    /// The schedule the caller supplied; echoed so the trace is
    /// self-describing in reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<MipGapSchedule>,
    /// Hard wall-clock time limit the solver was given, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_limit_secs: Option<f64>,
    /// Final relative gap at termination; `None` when no incumbent was
    /// found before termination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_gap: Option<f64>,
    /// Seconds from solve start to termination.
    pub final_time_secs: f64,
    /// How the solve ended.
    pub terminated_by: MipTerminationReason,
    /// All recorded callback events in order. Empty when no schedule
    /// was supplied or the backend does not support progress callbacks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<MipEvent>,
    /// Original-model (pre-presolve) variable count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_vars: Option<u64>,
    /// Original-model binary variable count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_bin_vars: Option<u64>,
    /// Binary variables already pinned by tight bounds before the
    /// solve starts (col_lower == col_upper on a Binary column).
    /// These contribute to `n_bin_vars` but Gurobi's presolve
    /// removes them — the "free" binary count Gurobi actually
    /// branches on is `n_bin_vars - pre_fixed_bin_vars`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_fixed_bin_vars: Option<u64>,
    /// Original-model integer (incl. binary) variable count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_int_vars: Option<u64>,
    /// Original-model constraint count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_rows: Option<u64>,
    /// Original-model A-matrix nonzero count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_nonzeros: Option<u64>,
    /// Branch-and-bound nodes explored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_count: Option<u64>,
    /// Simplex (or barrier) iterations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iter_count: Option<u64>,
    /// Final incumbent objective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<f64>,
    /// Final best bound (dual bound).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_bound: Option<f64>,
}

/// Solver-agnostic progress monitor.
///
/// Each supporting backend wraps its native progress callback around
/// [`Self::tick`], which records events and decides whether the solve
/// should be terminated early. The monitor is consumed at solve end via
/// [`Self::into_trace`] to produce the reportable [`MipTrace`].
///
/// Not `Send`/`Sync` by design: the pointer to a `MipProgressMonitor` is
/// handed to the solver library through an `extern "C"` callback and must
/// remain pinned on the owning thread's stack while the solver runs.
#[derive(Debug)]
pub struct MipProgressMonitor {
    schedule: MipGapSchedule,
    events: Vec<MipEvent>,
    last_obj: Option<f64>,
    last_bnd: Option<f64>,
    crossed_breakpoints: Vec<bool>,
    should_terminate: bool,
}

impl MipProgressMonitor {
    /// Build a fresh monitor for one solve.
    pub fn new(schedule: MipGapSchedule) -> Self {
        let n = schedule.breakpoints.len();
        Self {
            schedule,
            events: Vec::new(),
            last_obj: None,
            last_bnd: None,
            crossed_breakpoints: vec![false; n],
            should_terminate: false,
        }
    }

    /// Immutable access for inspection (e.g., after solve end).
    pub fn schedule(&self) -> &MipGapSchedule {
        &self.schedule
    }

    /// Relative gap used both by this monitor and by its [`MipEvent`]s.
    /// Symmetric safeguard: `|incumbent - bound| / (|incumbent| + 1e-10)`.
    pub fn relative_gap(incumbent_obj: f64, best_bound: f64) -> f64 {
        let denom = incumbent_obj.abs() + 1e-10;
        (incumbent_obj - best_bound).abs() / denom
    }

    /// Called from the solver backend's callback on each progress tick.
    ///
    /// `t_secs` is the solver's reported wall-clock time since solve
    /// start. `incumbent_obj` and `best_bound` are the solver's current
    /// best primal and best dual. Returns `true` when the caller should
    /// request solver termination (e.g., `GRBterminate`).
    ///
    /// Records an event when any of the following changed since last
    /// tick: incumbent improved, bound tightened, schedule breakpoint
    /// crossed, or the termination condition fired.
    pub fn tick(&mut self, t_secs: f64, incumbent_obj: f64, best_bound: f64) -> bool {
        if self.should_terminate {
            return true;
        }
        if !t_secs.is_finite() || !incumbent_obj.is_finite() || !best_bound.is_finite() {
            return false;
        }

        let gap = Self::relative_gap(incumbent_obj, best_bound);
        let target = self.schedule.target_at(t_secs);
        let target_for_event = target.unwrap_or(f64::NAN);

        // Record breakpoint-crossing events (only once per breakpoint).
        for (idx, (bt, _bg)) in self.schedule.breakpoints.iter().enumerate() {
            if !self.crossed_breakpoints[idx] && t_secs >= *bt {
                self.crossed_breakpoints[idx] = true;
                self.events.push(MipEvent {
                    t_secs,
                    incumbent_obj,
                    best_bound,
                    gap,
                    target_gap: target_for_event,
                    kind: MipEventKind::BreakpointCrossed,
                });
            }
        }

        // Record strictly-better primal/dual observations.
        let new_incumbent = match self.last_obj {
            None => true,
            Some(prev) => incumbent_obj < prev - 1e-12 * prev.abs().max(1.0),
        };
        if new_incumbent {
            self.events.push(MipEvent {
                t_secs,
                incumbent_obj,
                best_bound,
                gap,
                target_gap: target_for_event,
                kind: MipEventKind::NewIncumbent,
            });
            self.last_obj = Some(incumbent_obj);
        }

        let bound_improved = match self.last_bnd {
            None => true,
            Some(prev) => best_bound > prev + 1e-12 * prev.abs().max(1.0),
        };
        if bound_improved && !new_incumbent {
            // Skip when we just recorded a NewIncumbent at this same tick
            // to avoid double-counting the same instant.
            self.events.push(MipEvent {
                t_secs,
                incumbent_obj,
                best_bound,
                gap,
                target_gap: target_for_event,
                kind: MipEventKind::BoundImproved,
            });
        }
        if bound_improved {
            self.last_bnd = Some(best_bound);
        }

        // Termination decision: fire only when we have an incumbent and
        // the schedule has reached an applicable target.
        if let Some(t) = target
            && self.last_obj.is_some()
            && gap <= t
        {
            self.events.push(MipEvent {
                t_secs,
                incumbent_obj,
                best_bound,
                gap,
                target_gap: t,
                kind: MipEventKind::ScheduleTerminate,
            });
            self.should_terminate = true;
            return true;
        }
        false
    }

    /// Whether [`Self::tick`] has requested termination on a previous tick.
    pub fn has_terminated(&self) -> bool {
        self.should_terminate
    }

    /// Consume the monitor into a reportable [`MipTrace`].
    pub fn into_trace(
        self,
        time_limit_secs: Option<f64>,
        terminated_by: MipTerminationReason,
        final_time_secs: f64,
        final_gap: Option<f64>,
    ) -> MipTrace {
        MipTrace {
            schedule: Some(self.schedule),
            time_limit_secs,
            final_gap,
            final_time_secs,
            terminated_by,
            events: self.events,
            n_vars: None,
            n_bin_vars: None,
            pre_fixed_bin_vars: None,
            n_int_vars: None,
            n_rows: None,
            n_nonzeros: None,
            node_count: None,
            iter_count: None,
            objective: None,
            objective_bound: None,
        }
    }
}

/// Options for `LpSolver`.
#[derive(Debug, Clone)]
pub struct LpOptions {
    /// Feasibility + optimality tolerance (default 1e-8).
    pub tolerance: f64,
    /// Optional wall-clock time limit.
    pub time_limit_secs: Option<f64>,
    /// Optional relative MIP optimality gap (e.g. 0.01 = 1%).
    pub mip_rel_gap: Option<f64>,
    /// Optional time-varying MIP gap schedule. When `Some` and the backend
    /// supports progress callbacks, the backend tightens `mip_rel_gap`
    /// over the wall-clock horizon, terminating early once the current
    /// incumbent is within the scheduled target. Backends without callback
    /// support silently ignore this field and fall back to the static
    /// `mip_rel_gap` / `time_limit_secs` safety net.
    pub mip_gap_schedule: Option<MipGapSchedule>,
    /// Optional primal start for LP/QP/MIP backends.
    ///
    /// Backends may ignore this hint when they do not support warm starts.
    pub primal_start: Option<LpPrimalStart>,
    /// Preferred continuous algorithm, when the backend supports it.
    pub algorithm: LpAlgorithm,
    /// Print level: 0 = silent, 1+ = verbose.
    pub print_level: u8,
}

/// Optional primal start representation for LP/QP/MIP backends.
#[derive(Debug, Clone)]
pub enum LpPrimalStart {
    /// Dense primal assignment for all columns.
    Dense(Vec<f64>),
    /// Sparse primal assignment for a subset of columns.
    Sparse {
        indices: Vec<usize>,
        values: Vec<f64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LpAlgorithm {
    #[default]
    Auto,
    Simplex,
    Ipm,
}

impl Default for LpOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-8,
            time_limit_secs: None,
            mip_rel_gap: None,
            mip_gap_schedule: None,
            primal_start: None,
            algorithm: LpAlgorithm::Auto,
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
/// - **unset / `"auto"`** — probe Gurobi → COPT → CPLEX → HiGHS at runtime
///   (commercial solvers first because they are typically 2–10× faster and
///   more robust on ill-conditioned QP/QCQP problems than HiGHS).
/// - **`"highs"`** — HiGHS (loaded at runtime via libhighs.{so,dylib})
/// - **`"gurobi"` / `"copt"` / `"cplex"`** — use that solver directly
///
/// Returns `Err` if the requested solver (or any solver, in the default probe
/// path) is not found.
pub fn try_default_lp_solver() -> Result<Arc<dyn LpSolver>, String> {
    let choice = std::env::var("SURGE_LP_SOLVER")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match choice.as_str() {
        "" | "auto" => probe_lp_solvers(),
        "highs" => {
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
