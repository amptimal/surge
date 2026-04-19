// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Pre-solver `SparseProblem` reduction.
//!
//! When a caller pins variables via bounds (`col_lower[j] == col_upper[j]`)
//! we can eliminate them from the sparse problem before handing it to the
//! backend. This avoids the situation where Gurobi's own presolve
//! substitutes the pinned value into every row that references the
//! column — a step that keeps the pre-presolve model size unchanged,
//! leaves denser rows in the presolved model, and shifts simplex
//! degeneracy. Upfront reduction gives the backend a problem that is
//! already minimal in the rows/cols our formulation knows how to collapse.
//!
//! The reducer handles:
//!
//! * Fixed columns (lb == ub, finite). The column and its entries are
//!   dropped from the CSC matrix; each referencing row's lower / upper
//!   bounds absorb `coef × value` as a constant shift; `col_cost × value`
//!   accumulates into an objective offset added back after solving.
//!
//! * Rows that become trivially satisfied after substitution — no
//!   remaining triplets and the adjusted `[lb, ub]` interval contains 0.
//!   These drop entirely. Rows whose adjusted interval excludes 0 are
//!   kept (the backend will report infeasibility; we don't second-guess
//!   it).
//!
//! The reducer does NOT currently handle quadratic objectives (Q terms
//! involving fixed columns contribute constant and linear shifts). When
//! `q_start` is present we pass the problem through unchanged; that's
//! fine for all SCUC / DC-OPF callers which are LP/MIP only.
//!
//! After the backend solves the reduced problem the caller rebuilds a
//! full-size [`LpResult`] via [`expand_solution`], which splices the
//! fixed values back into the primal vector, zeroes the dual of every
//! dropped row, and adds `objective_offset` to the reported objective.

use super::{LpResult, SparseProblem, VariableDomain};

/// How each original column maps into the reduced problem.
#[derive(Debug, Clone, Copy)]
pub enum ColKind {
    /// Column survives; the stored index is its position in the reduced
    /// `SparseProblem`'s column arrays.
    Kept(u32),
    /// Column was fixed during reduction; the stored value is the
    /// constant that the LP must see in its place.
    Fixed(f64),
}

/// Output of [`reduce_by_fixed_vars`]: the reduced problem ready for the
/// solver plus the bookkeeping required to translate its solution back
/// into the original column / row indexing.
#[derive(Debug, Clone)]
pub struct SparseReduction {
    /// The reduced problem to hand to the LP/MIP solver.
    pub reduced: SparseProblem,
    /// Objective offset accumulated from fixed columns: `sum(col_cost[j] * v_j)`.
    /// The original problem's optimal objective is
    /// `reduced_result.objective + objective_offset`.
    pub objective_offset: f64,
    /// Where each original column ended up. `original_col_kind.len() ==
    /// original_n_col`.
    pub original_col_kind: Vec<ColKind>,
    /// For each row in the reduced problem, the corresponding original
    /// row index. `reduced_to_original_row.len() == reduced.n_row`.
    pub reduced_to_original_row: Vec<usize>,
    /// Count of columns eliminated.
    pub n_fixed_cols: usize,
    /// Count of rows dropped (trivially satisfied after substitution).
    pub n_dropped_rows: usize,
    /// Count of A-matrix nonzero entries removed (fixed-col entries plus
    /// any entries in dropped rows).
    pub n_removed_nnz: usize,
    /// Original column count, retained so [`expand_solution`] can size
    /// the expanded result correctly.
    pub original_n_col: usize,
    /// Original row count, retained for the same reason.
    pub original_n_row: usize,
}

impl SparseReduction {
    /// Identity (no-op) reduction — returned when reduction is not
    /// possible (e.g. quadratic problem) or when the caller explicitly
    /// disables it. Lets downstream logic treat the reduced path
    /// uniformly without a case split.
    pub fn identity(problem: SparseProblem) -> Self {
        let n_col = problem.n_col;
        let n_row = problem.n_row;
        Self {
            original_col_kind: (0..n_col).map(|j| ColKind::Kept(j as u32)).collect(),
            reduced_to_original_row: (0..n_row).collect(),
            original_n_col: n_col,
            original_n_row: n_row,
            reduced: problem,
            objective_offset: 0.0,
            n_fixed_cols: 0,
            n_dropped_rows: 0,
            n_removed_nnz: 0,
        }
    }
}

/// Tolerance on `col_upper - col_lower` for treating a column as fixed.
/// Bounds equal to within this tolerance count as fixed.
const FIXED_BOUND_TOL: f64 = 1e-9;
/// Tolerance on `row_lower <= 0 <= row_upper` for dropping an empty row
/// as trivially satisfied. Chosen loose enough to survive floating-point
/// residual from the `coef*value` substitution, tight enough to preserve
/// infeasibility signalling when the adjusted interval truly excludes 0.
const ROW_TRIVIAL_TOL: f64 = 1e-6;

/// Reduce a [`SparseProblem`] by eliminating columns whose bounds pin
/// them to a single value and dropping rows that become trivially
/// satisfied as a result.
///
/// Returns a [`SparseReduction`] whose `reduced` field is the problem
/// to pass to the backend; use [`expand_solution`] on the solver's
/// [`LpResult`] to recover the full-size answer in the original column
/// / row indexing.
///
/// Quadratic problems (`problem.q_start.is_some()`) are passed through
/// unchanged via [`SparseReduction::identity`]. Structural reductions
/// for Q matrices are deferred pending a caller that needs them.
pub fn reduce_by_fixed_vars(problem: SparseProblem) -> SparseReduction {
    if problem.q_start.is_some() {
        return SparseReduction::identity(problem);
    }

    let n_col = problem.n_col;
    let n_row = problem.n_row;

    // ── Pass 1: classify columns ────────────────────────────────────────
    let mut original_col_kind: Vec<ColKind> = Vec::with_capacity(n_col);
    let mut kept_original_cols: Vec<usize> = Vec::new();
    let mut objective_offset = 0.0_f64;
    let mut n_fixed_cols = 0usize;
    for j in 0..n_col {
        let lb = problem.col_lower[j];
        let ub = problem.col_upper[j];
        let fixed = lb.is_finite() && ub.is_finite() && (ub - lb).abs() <= FIXED_BOUND_TOL;
        if fixed {
            let v = 0.5 * (lb + ub);
            original_col_kind.push(ColKind::Fixed(v));
            objective_offset += problem.col_cost[j] * v;
            n_fixed_cols += 1;
        } else {
            let reduced_idx = kept_original_cols.len() as u32;
            original_col_kind.push(ColKind::Kept(reduced_idx));
            kept_original_cols.push(j);
        }
    }

    // Fast path: no fixed columns — pass through unchanged.
    if n_fixed_cols == 0 {
        return SparseReduction::identity(problem);
    }

    // ── Pass 2: adjust row bounds, count remaining triplets per row ─────
    let mut new_row_lower = problem.row_lower.clone();
    let mut new_row_upper = problem.row_upper.clone();
    let mut row_remaining_nnz: Vec<usize> = vec![0; n_row];
    let mut n_removed_nnz = 0usize;

    #[allow(clippy::needless_range_loop)]
    for j in 0..n_col {
        let start = problem.a_start[j] as usize;
        let end = problem.a_start[j + 1] as usize;
        match original_col_kind[j] {
            ColKind::Fixed(v) => {
                for k in start..end {
                    let row = problem.a_index[k] as usize;
                    let coef = problem.a_value[k];
                    let shift = coef * v;
                    if new_row_lower[row].is_finite() {
                        new_row_lower[row] -= shift;
                    }
                    if new_row_upper[row].is_finite() {
                        new_row_upper[row] -= shift;
                    }
                }
                n_removed_nnz += end - start;
            }
            ColKind::Kept(_) => {
                for k in start..end {
                    let row = problem.a_index[k] as usize;
                    row_remaining_nnz[row] += 1;
                }
            }
        }
    }

    // ── Pass 3: plan row drops ──────────────────────────────────────────
    // Only drop a row when ALL its entries came from fixed columns AND
    // the adjusted bounds straddle 0 (row is trivially satisfied). Rows
    // that remain infeasible after substitution stay in the problem so
    // the backend reports the infeasibility.
    let mut reduced_to_original_row: Vec<usize> = Vec::with_capacity(n_row);
    let mut original_to_reduced_row: Vec<u32> = vec![u32::MAX; n_row];
    let mut n_dropped_rows = 0usize;
    for row in 0..n_row {
        if row_remaining_nnz[row] == 0 {
            let lo = new_row_lower[row];
            let hi = new_row_upper[row];
            // Lower bound must be <= 0 (with slack tolerance) and upper
            // bound must be >= 0. Infinities satisfy one side trivially.
            let lo_ok = !lo.is_finite() || lo <= ROW_TRIVIAL_TOL;
            let hi_ok = !hi.is_finite() || hi >= -ROW_TRIVIAL_TOL;
            if lo_ok && hi_ok {
                n_dropped_rows += 1;
                continue;
            }
            // Adjusted bounds exclude zero → row is infeasible. Keep it
            // so the backend raises the infeasibility signal.
        }
        let new_idx = reduced_to_original_row.len() as u32;
        original_to_reduced_row[row] = new_idx;
        reduced_to_original_row.push(row);
    }

    // ── Pass 4: build the reduced CSC matrix ───────────────────────────
    let n_kept_cols = kept_original_cols.len();
    let mut new_a_start: Vec<i32> = Vec::with_capacity(n_kept_cols + 1);
    let mut new_a_index: Vec<i32> = Vec::new();
    let mut new_a_value: Vec<f64> = Vec::new();
    new_a_start.push(0);
    for &orig_j in &kept_original_cols {
        let start = problem.a_start[orig_j] as usize;
        let end = problem.a_start[orig_j + 1] as usize;
        for k in start..end {
            let orig_row = problem.a_index[k] as usize;
            let new_row = original_to_reduced_row[orig_row];
            if new_row != u32::MAX {
                new_a_index.push(new_row as i32);
                new_a_value.push(problem.a_value[k]);
            } else {
                // Row was dropped — its triplet goes away too.
                n_removed_nnz += 1;
            }
        }
        new_a_start.push(new_a_index.len() as i32);
    }

    // ── Pass 5: assemble the reduced SparseProblem ─────────────────────
    let new_col_cost: Vec<f64> = kept_original_cols
        .iter()
        .map(|&j| problem.col_cost[j])
        .collect();
    let new_col_lower: Vec<f64> = kept_original_cols
        .iter()
        .map(|&j| problem.col_lower[j])
        .collect();
    let new_col_upper: Vec<f64> = kept_original_cols
        .iter()
        .map(|&j| problem.col_upper[j])
        .collect();
    let new_row_lower_reduced: Vec<f64> = reduced_to_original_row
        .iter()
        .map(|&r| new_row_lower[r])
        .collect();
    let new_row_upper_reduced: Vec<f64> = reduced_to_original_row
        .iter()
        .map(|&r| new_row_upper[r])
        .collect();
    let new_col_names = problem.col_names.as_ref().map(|names| {
        kept_original_cols
            .iter()
            .map(|&j| names[j].clone())
            .collect::<Vec<_>>()
    });
    let new_row_names = problem.row_names.as_ref().map(|names| {
        reduced_to_original_row
            .iter()
            .map(|&r| names[r].clone())
            .collect::<Vec<_>>()
    });
    let new_integrality = problem.integrality.as_ref().map(|integ| {
        kept_original_cols
            .iter()
            .map(|&j| integ[j])
            .collect::<Vec<VariableDomain>>()
    });

    let reduced = SparseProblem {
        n_col: n_kept_cols,
        n_row: reduced_to_original_row.len(),
        col_cost: new_col_cost,
        col_lower: new_col_lower,
        col_upper: new_col_upper,
        row_lower: new_row_lower_reduced,
        row_upper: new_row_upper_reduced,
        a_start: new_a_start,
        a_index: new_a_index,
        a_value: new_a_value,
        q_start: None,
        q_index: None,
        q_value: None,
        col_names: new_col_names,
        row_names: new_row_names,
        integrality: new_integrality,
    };

    SparseReduction {
        reduced,
        objective_offset,
        original_col_kind,
        reduced_to_original_row,
        n_fixed_cols,
        n_dropped_rows,
        n_removed_nnz,
        original_n_col: n_col,
        original_n_row: n_row,
    }
}

/// Expand an [`LpResult`] produced by solving the reduced problem back
/// into the original column / row indexing.
///
/// Primal vector `x` gets the solver's reduced x in the kept slots and
/// the fixed value in the removed slots. Row duals get the solver's
/// duals in the kept rows and 0 in dropped rows (a dropped row is
/// trivially satisfied, so its Lagrange multiplier is zero at every
/// feasible dispatch). Column reduced costs likewise get 0 for fixed
/// columns; a fixed column's true reduced cost can be recovered from
/// the row duals if needed by downstream code.
pub fn expand_solution(reduced_result: LpResult, reduction: &SparseReduction) -> LpResult {
    let n_col = reduction.original_n_col;
    let n_row = reduction.original_n_row;

    let mut x = vec![0.0_f64; n_col];
    let mut col_dual = vec![0.0_f64; n_col];
    for (j, kind) in reduction.original_col_kind.iter().enumerate() {
        match *kind {
            ColKind::Kept(k) => {
                let k = k as usize;
                x[j] = reduced_result.x[k];
                if k < reduced_result.col_dual.len() {
                    col_dual[j] = reduced_result.col_dual[k];
                }
            }
            ColKind::Fixed(v) => {
                x[j] = v;
            }
        }
    }

    let mut row_dual = vec![0.0_f64; n_row];
    for (reduced_row, &orig_row) in reduction.reduced_to_original_row.iter().enumerate() {
        if reduced_row < reduced_result.row_dual.len() {
            row_dual[orig_row] = reduced_result.row_dual[reduced_row];
        }
    }

    LpResult {
        x,
        row_dual,
        col_dual,
        objective: reduced_result.objective + reduction.objective_offset,
        status: reduced_result.status,
        iterations: reduced_result.iterations,
        mip_trace: reduced_result.mip_trace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::{LpSolveStatus, VariableDomain};

    fn make_problem(
        n_col: usize,
        n_row: usize,
        col_cost: Vec<f64>,
        col_lower: Vec<f64>,
        col_upper: Vec<f64>,
        row_lower: Vec<f64>,
        row_upper: Vec<f64>,
        triplets: Vec<(usize, usize, f64)>, // (row, col, value)
        integrality: Option<Vec<VariableDomain>>,
    ) -> SparseProblem {
        // Build CSC from triplets.
        let mut by_col: Vec<Vec<(i32, f64)>> = vec![Vec::new(); n_col];
        for (row, col, val) in triplets {
            by_col[col].push((row as i32, val));
        }
        let mut a_start: Vec<i32> = vec![0];
        let mut a_index = Vec::new();
        let mut a_value = Vec::new();
        for entries in by_col.iter_mut().take(n_col) {
            entries.sort_by_key(|(r, _)| *r);
            for (r, v) in entries.iter() {
                a_index.push(*r);
                a_value.push(*v);
            }
            a_start.push(a_index.len() as i32);
        }
        SparseProblem {
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
            q_start: None,
            q_index: None,
            q_value: None,
            col_names: None,
            row_names: None,
            integrality,
        }
    }

    #[test]
    fn reduces_fixed_column_and_drops_trivial_row() {
        // min x0 + x1 + x2
        // s.t.  x0 + x2 = 1        (row 0)
        //       x1 = 2             (row 1 — trivial after x1 is fixed)
        //       x0, x1, x2 >= 0
        //       x1 pinned to 2 via bounds (lb=ub=2).
        //       x0, x2 free in [0, 10].
        let problem = make_problem(
            3,
            2,
            vec![1.0, 1.0, 1.0],
            vec![0.0, 2.0, 0.0],
            vec![10.0, 2.0, 10.0],
            vec![1.0, 2.0],
            vec![1.0, 2.0],
            vec![(0, 0, 1.0), (0, 2, 1.0), (1, 1, 1.0)],
            None,
        );
        let reduction = reduce_by_fixed_vars(problem);
        assert_eq!(reduction.n_fixed_cols, 1);
        assert_eq!(reduction.n_dropped_rows, 1);
        // Fixed x1 contributes 2.0 to the objective offset.
        assert!((reduction.objective_offset - 2.0).abs() < 1e-12);
        // Reduced problem has 2 cols (x0, x2) and 1 row (the x0+x2=1 row).
        assert_eq!(reduction.reduced.n_col, 2);
        assert_eq!(reduction.reduced.n_row, 1);
        assert_eq!(reduction.reduced.row_lower, vec![1.0]);
        assert_eq!(reduction.reduced.row_upper, vec![1.0]);
    }

    #[test]
    fn identity_when_no_fixed_columns() {
        let problem = make_problem(
            2,
            1,
            vec![1.0, 1.0],
            vec![0.0, 0.0],
            vec![5.0, 5.0],
            vec![1.0],
            vec![3.0],
            vec![(0, 0, 1.0), (0, 1, 1.0)],
            None,
        );
        let reduction = reduce_by_fixed_vars(problem);
        assert_eq!(reduction.n_fixed_cols, 0);
        assert_eq!(reduction.n_dropped_rows, 0);
        assert_eq!(reduction.reduced.n_col, 2);
        assert_eq!(reduction.reduced.n_row, 1);
    }

    #[test]
    fn shifts_row_bounds_for_fixed_column_in_active_row() {
        // x0 in [0, 10], x1 fixed to 3
        //  x0 + 2*x1 <= 7     (row 0, becomes x0 <= 7 - 6 = 1)
        let problem = make_problem(
            2,
            1,
            vec![1.0, 1.0],
            vec![0.0, 3.0],
            vec![10.0, 3.0],
            vec![f64::NEG_INFINITY],
            vec![7.0],
            vec![(0, 0, 1.0), (0, 1, 2.0)],
            None,
        );
        let reduction = reduce_by_fixed_vars(problem);
        assert_eq!(reduction.n_fixed_cols, 1);
        assert_eq!(reduction.n_dropped_rows, 0);
        assert_eq!(reduction.reduced.n_col, 1);
        assert_eq!(reduction.reduced.n_row, 1);
        assert_eq!(reduction.reduced.row_upper, vec![1.0]);
    }

    #[test]
    fn expand_restores_fixed_col_values_and_objective() {
        let problem = make_problem(
            3,
            1,
            vec![1.0, 5.0, 1.0],
            vec![0.0, 4.0, 0.0],
            vec![10.0, 4.0, 10.0],
            vec![1.0],
            vec![1.0],
            vec![(0, 0, 1.0), (0, 2, 1.0)],
            None,
        );
        let reduction = reduce_by_fixed_vars(problem);
        let fake_result = LpResult {
            x: vec![0.3, 0.7],
            row_dual: vec![1.5],
            col_dual: vec![0.1, 0.2],
            objective: 1.0,
            status: LpSolveStatus::Optimal,
            iterations: 1,
            mip_trace: None,
        };
        let expanded = expand_solution(fake_result, &reduction);
        assert_eq!(expanded.x.len(), 3);
        assert!((expanded.x[0] - 0.3).abs() < 1e-12);
        assert!((expanded.x[1] - 4.0).abs() < 1e-12);
        assert!((expanded.x[2] - 0.7).abs() < 1e-12);
        // Objective: reduced 1.0 + offset 20.0 = 21.0.
        assert!((expanded.objective - 21.0).abs() < 1e-12);
    }
}
