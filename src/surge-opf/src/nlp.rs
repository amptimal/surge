// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Nonlinear Programming (NLP) trait and types.
//!
//! Defines the interface for nonlinear optimization problems solved by Ipopt
//! or other NLP backends. The formulation uses standard form:
//!
//! ```text
//! min  f(x)
//! s.t. g_L <= g(x) <= g_U
//!      x_L <= x    <= x_U
//! ```

/// Hessian approximation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HessianMode {
    /// Exact analytical Hessian of the Lagrangian (requires eval_hessian).
    Exact,
    /// L-BFGS quasi-Newton approximation (Ipopt handles internally).
    LimitedMemory,
}

/// NLP solver options.
#[derive(Debug, Clone)]
pub struct NlpOptions {
    /// Convergence tolerance for optimality.
    pub tolerance: f64,
    /// Maximum number of iterations.
    pub max_iterations: u32,
    /// Print level (0 = silent, 5 = verbose).
    pub print_level: i32,
    /// Hessian approximation mode.
    pub hessian_mode: HessianMode,
    /// Enable Ipopt warm-start from `NlpProblem::initial_point()`.
    ///
    /// When true, Ipopt's `warm_start_init_point` option is set to `"yes"`,
    /// telling the interior-point method to initialise from the provided
    /// primal variables rather than computing an interior starting point.
    pub warm_start: bool,
    /// Ipopt `nlp_scaling_method` (exact-Hessian path only). Default
    /// `"gradient-based"`. Set to `"none"` to apply `tolerance` to
    /// unscaled residuals — required when an external pi-model
    /// reconstruction must see exact bus balance.
    pub nlp_scaling_method: String,
}

impl Default for NlpOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-6,
            max_iterations: 200,
            print_level: 0,
            hessian_mode: HessianMode::LimitedMemory,
            warm_start: false,
            nlp_scaling_method: "gradient-based".to_string(),
        }
    }
}

/// Solution from an NLP solver.
#[derive(Debug, Clone)]
pub struct NlpSolution {
    /// Optimal variable values.
    pub x: Vec<f64>,
    /// Constraint multipliers (Lagrange multipliers for g(x)).
    pub lambda: Vec<f64>,
    /// Lower bound multipliers.
    pub z_lower: Vec<f64>,
    /// Upper bound multipliers.
    pub z_upper: Vec<f64>,
    /// Optimal objective value.
    pub objective: f64,
    /// Number of iterations, when the backend exposes it.
    pub iterations: Option<u32>,
    /// Whether the solver converged.
    pub converged: bool,
    /// Backend-reported stats for the solve (problem size, termination
    /// status, final residuals). None for backends that don't populate
    /// it; Ipopt populates it on every success path.
    pub trace: Option<surge_solution::NlpTrace>,
}

/// Trait defining a nonlinear programming problem.
///
/// Implementors provide the objective, constraints, and their derivatives.
/// The problem is in standard form: min f(x) s.t. g_L <= g(x) <= g_U, x_L <= x <= x_U.
pub trait NlpProblem {
    /// Number of decision variables.
    fn n_vars(&self) -> usize;

    /// Number of constraints.
    fn n_constraints(&self) -> usize;

    /// Variable bounds: (lower, upper) for each variable.
    fn var_bounds(&self) -> (Vec<f64>, Vec<f64>);

    /// Constraint bounds: (lower, upper) for each constraint.
    fn constraint_bounds(&self) -> (Vec<f64>, Vec<f64>);

    /// Initial point for optimization.
    fn initial_point(&self) -> Vec<f64>;

    /// Evaluate objective f(x).
    fn eval_objective(&self, x: &[f64]) -> f64;

    /// Evaluate gradient of objective ∇f(x).
    fn eval_gradient(&self, x: &[f64], grad: &mut [f64]);

    /// Evaluate constraint values g(x).
    fn eval_constraints(&self, x: &[f64], g: &mut [f64]);

    /// Sparsity structure of constraint Jacobian.
    /// Returns (row_indices, col_indices) in triplet format (0-indexed).
    fn jacobian_structure(&self) -> (Vec<i32>, Vec<i32>);

    /// Evaluate constraint Jacobian values at x.
    /// `values` has the same length as the triplet arrays from `jacobian_structure`.
    fn eval_jacobian(&self, x: &[f64], values: &mut [f64]);

    /// Whether exact Hessian is available.
    fn has_hessian(&self) -> bool {
        false
    }

    /// Sparsity structure of Hessian of Lagrangian (lower triangle).
    fn hessian_structure(&self) -> (Vec<i32>, Vec<i32>) {
        (vec![], vec![])
    }

    /// Evaluate Hessian of Lagrangian (lower triangle values).
    fn eval_hessian(&self, _x: &[f64], _obj_factor: f64, _lambda: &[f64], _values: &mut [f64]) {}
}
