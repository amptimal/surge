// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Clarabel conic interior-point solver backend.
//!
//! Provides `ClarabelLpSolver`, which implements [`LpSolver`] for LP/QP
//! problems by converting the `SparseProblem` (HiGHS-style ranges) into
//! Clarabel's standard conic form.
//!
//! # Clarabel problem form
//!
//! ```text
//! minimize    (1/2) x' P x + q' x
//! subject to  A x + s = b,  s ∈ K
//! ```
//!
//! where K is a product of cones.  Constraint rows are grouped by cone type
//! (the ordering of rows in A/b must match the ordering of cones in the
//! `cones` slice):
//!
//! | Cone                  | Meaning              |
//! |-----------------------|----------------------|
//! | `ZeroConeT(m)`        | s = 0  (equality)    |
//! | `NonnegativeConeT(m)` | s ≥ 0  (inequality)  |
//! | `SecondOrderConeT(n)` | `‖s[1:]‖ ≤ s[0]`    |
//!
//! # Rotated SOC encoding
//!
//! Clarabel 0.11 does **not** expose a `RotatedSecondOrderConeT` variant.
//! The rotated SOC constraint `W_fi * W_ti ≥ W_re² + W_im²` (used in the
//! SOCP-OPF relaxation) is encoded via the standard SOC identity:
//!
//! ```text
//! (u, v, w) ∈ RSOC  {2uv ≥ ‖w‖²}
//!   ↔  (u+v, u-v, √2·w) ∈ SOC
//! ```
//!
//! With `u = W_fi`, `v = W_ti`, `w = (√2·W_re, √2·W_im)`:
//!
//! ```text
//! (W_fi + W_ti, W_fi − W_ti, 2·W_re, 2·W_im) ∈ SOC(4)
//! ```
//!
//! Check: `(W_fi+W_ti)² ≥ (W_fi−W_ti)² + 4·W_re² + 4·W_im²`
//!        → `4·W_fi·W_ti ≥ 4·(W_re² + W_im²)` → `W_fi·W_ti ≥ W_re² + W_im²` ✓
//!
//! Each branch SOC constraint therefore occupies **4 rows** and one
//! `SecondOrderConeT(4)` cone.  When Clarabel adds native
//! `RotatedSecondOrderConeT` support this encoding can be simplified to
//! a direct `RotatedSecondOrderConeT(4)` with `(W_fi, W_ti, √2·W_re, √2·W_im)`.
//!
//! # Dual sign convention
//!
//! Clarabel returns dual variable `z` in the dual cone.  For equality
//! constraints (`ZeroConeT`) the dual is unconstrained and corresponds to
//! the standard Lagrange multiplier.  `LpResult.row_dual` stores standard
//! Lagrange multipliers (positive dual = tighter constraint increases
//! objective), so equality duals are stored directly.  Inequality duals
//! (`NonnegativeConeT`, z ≥ 0) map to standard multipliers directly.

#[allow(unused_imports)]
use clarabel::algebra::CscMatrix as ClarabelCscMatrix;
#[allow(unused_imports)]
use clarabel::solver::{DefaultSettings, DefaultSolver, IPSolver, SolverStatus, SupportedConeT};

use super::{LpOptions, LpResult, LpSolveStatus, LpSolver, SparseProblem};

// ---------------------------------------------------------------------------
// Helper: convert our CSC (i32 indices) to Clarabel CscMatrix (usize indices)
// ---------------------------------------------------------------------------

/// Build a Clarabel `CscMatrix<f64>` from CSC arrays with `i32` indices.
fn make_clarabel_csc(
    nrows: usize,
    ncols: usize,
    colptr: &[i32],
    rowval: &[i32],
    nzval: &[f64],
) -> ClarabelCscMatrix<f64> {
    let colptr_usize: Vec<usize> = colptr.iter().map(|&v| v as usize).collect();
    let rowval_usize: Vec<usize> = rowval.iter().map(|&v| v as usize).collect();
    ClarabelCscMatrix::new(nrows, ncols, colptr_usize, rowval_usize, nzval.to_vec())
}

/// Build an empty (all-zeros) Clarabel `CscMatrix<f64>` of shape m×n.
fn zero_csc(nrows: usize, ncols: usize) -> ClarabelCscMatrix<f64> {
    let colptr = vec![0usize; ncols + 1];
    ClarabelCscMatrix::new(nrows, ncols, colptr, vec![], vec![])
}

// ---------------------------------------------------------------------------
// Map Clarabel SolverStatus → our LpSolveStatus
// ---------------------------------------------------------------------------

fn clarabel_status(s: SolverStatus) -> LpSolveStatus {
    match s {
        SolverStatus::Solved | SolverStatus::AlmostSolved => LpSolveStatus::Optimal,
        SolverStatus::PrimalInfeasible | SolverStatus::AlmostPrimalInfeasible => {
            LpSolveStatus::Infeasible
        }
        SolverStatus::DualInfeasible | SolverStatus::AlmostDualInfeasible => {
            LpSolveStatus::Unbounded
        }
        _ => LpSolveStatus::SolverError(format!("{s:?}")),
    }
}

// ---------------------------------------------------------------------------
// ClarabelLpSolver
// ---------------------------------------------------------------------------

/// Clarabel LP/QP backend implementing [`LpSolver`].
///
/// Converts the HiGHS-style `SparseProblem`
/// (`row_lower ≤ Ax ≤ row_upper`, `col_lower ≤ x ≤ col_upper`) into
/// Clarabel's conic form using the following row grouping:
///
/// 1. **Equality rows** (`row_lower[i] == row_upper[i]`): `ZeroConeT`
/// 2. **Inequality rows** (general range):
///    Each `row_lower ≤ a'x ≤ row_upper` is split into two
///    `NonnegativeConeT` rows: `a'x - row_lower ≥ 0` and
///    `row_upper - a'x ≥ 0`.
/// 3. **Variable lower bounds** (`col_lower[j] > -1e29`): `NonnegativeConeT`
/// 4. **Variable upper bounds** (`col_upper[j] < 1e29`): `NonnegativeConeT`
///
/// MIP is **not** supported — returns `SolverError` when integrality
/// variables are present.
#[derive(Debug)]
pub struct ClarabelLpSolver;

impl ClarabelLpSolver {
    pub fn new() -> Self {
        ClarabelLpSolver
    }
}

impl Default for ClarabelLpSolver {
    fn default() -> Self {
        Self::new()
    }
}

impl LpSolver for ClarabelLpSolver {
    fn name(&self) -> &'static str {
        "Clarabel"
    }

    fn supports_mip(&self) -> bool {
        false
    }

    fn solve(&self, prob: &SparseProblem, opts: &LpOptions) -> Result<LpResult, String> {
        // MIP is unsupported
        if let Some(ref iv) = prob.integrality
            && iv
                .iter()
                .any(|&v| !matches!(v, crate::backends::VariableDomain::Continuous))
        {
            return Err("ClarabelLpSolver: MIP (integrality) is not supported by Clarabel".into());
        }

        let n = prob.n_col;
        let m = prob.n_row;
        const BIG: f64 = 1e29;

        // ── Classify rows ──────────────────────────────────────────────────────
        let mut eq_rows: Vec<usize> = Vec::new();
        let mut ineq_rows: Vec<usize> = Vec::new(); // rows with lb < ub (finite or both sides)

        for i in 0..m {
            let lb = prob.row_lower[i];
            let ub = prob.row_upper[i];
            // Equality: both finite and equal
            if (ub - lb).abs() < 1e-14 {
                eq_rows.push(i);
            } else {
                ineq_rows.push(i);
            }
        }

        // ── Classify column bounds ─────────────────────────────────────────────
        let mut col_lb_rows: Vec<usize> = Vec::new(); // j with col_lower[j] > -BIG
        let mut col_ub_rows: Vec<usize> = Vec::new(); // j with col_upper[j] < +BIG

        for j in 0..n {
            if prob.col_lower[j] > -BIG {
                col_lb_rows.push(j);
            }
            if prob.col_upper[j] < BIG {
                col_ub_rows.push(j);
            }
        }

        // ── Compute total cone structure ───────────────────────────────────────
        // Each ineq row contributes up to 2 NonnegativeConeT rows:
        //   - row with finite lb: a'x - lb ≥ 0
        //   - row with finite ub: ub - a'x ≥ 0
        struct IneqRow {
            row_idx: usize,
            has_lb: bool,
            has_ub: bool,
        }
        let mut ineq_expanded: Vec<IneqRow> = Vec::with_capacity(ineq_rows.len());
        let mut n_ineq_clar = 0usize;
        for &i in &ineq_rows {
            let has_lb = prob.row_lower[i] > -BIG;
            let has_ub = prob.row_upper[i] < BIG;
            n_ineq_clar += (has_lb as usize) + (has_ub as usize);
            ineq_expanded.push(IneqRow {
                row_idx: i,
                has_lb,
                has_ub,
            });
        }

        let n_eq = eq_rows.len();
        let n_col_lb = col_lb_rows.len();
        let n_col_ub = col_ub_rows.len();
        let total_rows = n_eq + n_ineq_clar + n_col_lb + n_col_ub;

        // ── Build A_clar (total_rows × n) and b_clar ──────────────────────────
        // We build in COO then convert to CSC.
        // Clarabel uses usize indices internally.
        let mut coo_row: Vec<usize> = Vec::new();
        let mut coo_col: Vec<usize> = Vec::new();
        let mut coo_val: Vec<f64> = Vec::new();
        let mut b_clar = vec![0.0f64; total_rows];

        let mut cur_row = 0usize;

        // Helper: add row entries from the original A matrix for original row i.
        // A is stored in CSC: for each column j, A[a_start[j]..a_start[j+1]] gives
        // (row, value) pairs. We need rows for original row i, so we must iterate columns.
        // Build a helper to get nonzeros per original row on demand.
        // Pre-compute row → (col, val) from CSC:
        let mut row_entries: Vec<Vec<(usize, f64)>> = vec![vec![]; m];
        for j in 0..n {
            let col_start = prob.a_start[j] as usize;
            let col_end = prob.a_start[j + 1] as usize;
            for idx in col_start..col_end {
                let orig_row = prob.a_index[idx] as usize;
                let val = prob.a_value[idx];
                row_entries[orig_row].push((j, val));
            }
        }

        // 1. Equality rows → ZeroConeT
        for &i in &eq_rows {
            let rhs = prob.row_lower[i]; // lb == ub
            // ZeroConeT: Ax + s = b, s ∈ {0} → A_clar[row, :] = A[i, :], b = rhs
            // Clarabel form: A_clar x + s = b → s = b - A_clar x
            // For equality: s = 0 → b = A_clar x → A_clar = A[i,:], b[row] = rhs
            for &(j, v) in &row_entries[i] {
                coo_row.push(cur_row);
                coo_col.push(j);
                coo_val.push(v);
            }
            b_clar[cur_row] = rhs;
            cur_row += 1;
        }

        // 2a. Inequality rows — lb side: a'x - lb ≥ 0  →  NonnegativeConeT
        //     Clarabel:  A x + s = b, s ≥ 0
        //     →  A_clar[row, :] = -A[i, :],  b[row] = -lb
        //     (because s = b - A_clar x = -lb + a'x = a'x - lb ≥ 0)
        // 2b. Inequality rows — ub side: ub - a'x ≥ 0  →  NonnegativeConeT
        //     →  A_clar[row, :] = A[i, :],  b[row] = ub

        for er in &ineq_expanded {
            let i = er.row_idx;
            if er.has_lb {
                let lb = prob.row_lower[i];
                for &(j, v) in &row_entries[i] {
                    coo_row.push(cur_row);
                    coo_col.push(j);
                    coo_val.push(-v);
                }
                b_clar[cur_row] = -lb;
                cur_row += 1;
            }
            if er.has_ub {
                let ub = prob.row_upper[i];
                for &(j, v) in &row_entries[i] {
                    coo_row.push(cur_row);
                    coo_col.push(j);
                    coo_val.push(v);
                }
                b_clar[cur_row] = ub;
                cur_row += 1;
            }
        }

        // 3. Column lower bounds: x[j] ≥ lb  →  x[j] - lb ≥ 0
        //    A_clar[row, j] = -1 (so s = b - A_clar x = -lb + x[j] = x[j] - lb ≥ 0)
        for &j in &col_lb_rows {
            let lb = prob.col_lower[j];
            coo_row.push(cur_row);
            coo_col.push(j);
            coo_val.push(-1.0);
            b_clar[cur_row] = -lb;
            cur_row += 1;
        }

        // 4. Column upper bounds: x[j] ≤ ub  →  ub - x[j] ≥ 0
        //    A_clar[row, j] = 1 (so s = b - A_clar x = ub - x[j] ≥ 0)
        for &j in &col_ub_rows {
            let ub = prob.col_upper[j];
            coo_row.push(cur_row);
            coo_col.push(j);
            coo_val.push(1.0);
            b_clar[cur_row] = ub;
            cur_row += 1;
        }

        debug_assert_eq!(cur_row, total_rows);

        // ── Convert COO → CSC ─────────────────────────────────────────────────
        let a_clar = coo_to_csc_usize(total_rows, n, &coo_row, &coo_col, &coo_val);

        // ── Build P (quadratic objective, upper triangle) ─────────────────────
        let p_clar = if let (Some(qs), Some(qi), Some(qv)) = (
            prob.q_start.as_deref(),
            prob.q_index.as_deref(),
            prob.q_value.as_deref(),
        ) {
            // Clarabel needs upper-triangular CSC (same as our convention)
            make_clarabel_csc(n, n, qs, qi, qv)
        } else {
            zero_csc(n, n)
        };

        // ── Cones ─────────────────────────────────────────────────────────────
        let mut cones: Vec<SupportedConeT<f64>> = Vec::new();
        if n_eq > 0 {
            cones.push(SupportedConeT::ZeroConeT(n_eq));
        }
        let n_ineq_total = n_ineq_clar + n_col_lb + n_col_ub;
        if n_ineq_total > 0 {
            cones.push(SupportedConeT::NonnegativeConeT(n_ineq_total));
        }

        // If no constraints at all, add a trivial zero cone of size 0.
        // (Clarabel requires cones to be non-empty if there are constraint rows.)
        // In practice this should not happen for power systems OPF.

        // ── Objective q ───────────────────────────────────────────────────────
        let q = prob.col_cost.clone();

        // ── Settings ──────────────────────────────────────────────────────────
        let settings = clarabel_settings(opts.tolerance, opts.time_limit_secs, opts.print_level);

        // ── Solve ─────────────────────────────────────────────────────────────
        let mut solver = DefaultSolver::new(&p_clar, &q, &a_clar, &b_clar, &cones, settings)
            .map_err(|e| format!("Clarabel setup error: {e}"))?;
        solver.solve();

        let sol = &solver.solution;
        let status = clarabel_status(sol.status);

        // Recover row duals for the original rows.
        // Clarabel's dual variable z has the same length as b (total_rows).
        // We map back to the original m rows.
        //
        // Dual convention: Clarabel z[i] is the Lagrange multiplier for the
        // cone constraint s[i] ∈ K_i.  For ZeroConeT (equality), z is
        // unconstrained; for NonnegativeConeT, z ≥ 0.
        //
        // Reconstruct standard Lagrange multipliers from Clarabel's dual cone
        // variables. Positive dual = tighter constraint increases objective.
        let z = &sol.z;
        let mut row_dual = vec![0.0f64; m];

        // Equality rows: Clarabel z for ZeroConeT = standard Lagrange multiplier.
        for (eq_pos, &i) in eq_rows.iter().enumerate() {
            row_dual[i] = z[eq_pos];
        }

        // Inequality rows (lb side then ub side)
        let mut z_pos = n_eq;
        for er in &ineq_expanded {
            let i = er.row_idx;
            if er.has_lb {
                // lb side: s = a'x - lb ≥ 0, z ≥ 0. Standard λ_lb = z.
                row_dual[i] += z[z_pos];
                z_pos += 1;
            }
            if er.has_ub {
                // ub side: s = ub - a'x ≥ 0, z ≥ 0. Standard λ_ub = z.
                row_dual[i] += z[z_pos];
                z_pos += 1;
            }
        }
        // (col bound duals are absorbed into col_dual below)

        // Column duals from bound constraints
        let mut col_dual = vec![0.0f64; n];
        for (k, &j) in col_lb_rows.iter().enumerate() {
            col_dual[j] += z[z_pos + k];
        }
        let z_pos_ub = z_pos + n_col_lb;
        for (k, &j) in col_ub_rows.iter().enumerate() {
            col_dual[j] -= z[z_pos_ub + k];
        }

        Ok(LpResult {
            x: sol.x.clone(),
            row_dual,
            col_dual,
            objective: sol.obj_val,
            status,
            iterations: sol.iterations,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build Clarabel `DefaultSettings` from `LpOptions`.
fn clarabel_settings(tol: f64, time_limit: Option<f64>, print_level: u8) -> DefaultSettings<f64> {
    use clarabel::solver::DefaultSettingsBuilder;
    DefaultSettingsBuilder::default()
        .tol_gap_abs(tol)
        .tol_gap_rel(tol)
        .tol_feas(tol)
        .verbose(print_level > 0)
        .time_limit(time_limit.unwrap_or(f64::INFINITY))
        .build()
        .expect("Clarabel settings build failed")
}

/// Convert COO sparse matrix to CSC (usize indices, for Clarabel).
///
/// Entries must be sorted by column (or will be sorted here).
/// Duplicate (row, col) entries are summed.
fn coo_to_csc_usize(
    nrows: usize,
    ncols: usize,
    row: &[usize],
    col: &[usize],
    val: &[f64],
) -> ClarabelCscMatrix<f64> {
    let nnz = row.len();
    if nnz == 0 {
        return ClarabelCscMatrix::new(nrows, ncols, vec![0usize; ncols + 1], vec![], vec![]);
    }

    // Sort by (col, row)
    let mut order: Vec<usize> = (0..nnz).collect();
    order.sort_by_key(|&i| (col[i], row[i]));

    // Fill rowval and nzval in sorted order, merging duplicates
    let mut rowval: Vec<usize> = Vec::with_capacity(nnz);
    let mut nzval: Vec<f64> = Vec::with_capacity(nnz);

    let mut cur_col = usize::MAX;
    let mut cur_row = usize::MAX;
    for &i in &order {
        let c = col[i];
        let r = row[i];
        let v = val[i];
        if c == cur_col && r == cur_row {
            // Merge duplicate entry
            *nzval
                .last_mut()
                .expect("nzval non-empty when duplicate (col,row) detected") += v;
        } else {
            rowval.push(r);
            nzval.push(v);
            cur_col = c;
            cur_row = r;
        }
    }

    // Build colptr from the deduplicated (sorted) entries
    let mut colptr = vec![0usize; ncols + 1];
    {
        let mut last_col = usize::MAX;
        let mut last_row = usize::MAX;
        for &i in &order {
            let c = col[i];
            let r = row[i];
            if c == last_col && r == last_row {
                continue;
            }
            colptr[c + 1] += 1;
            last_col = c;
            last_row = r;
        }
    }
    for j in 0..ncols {
        colptr[j + 1] += colptr[j];
    }

    debug_assert_eq!(colptr[ncols], rowval.len());

    ClarabelCscMatrix::new(nrows, ncols, colptr, rowval, nzval)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Simple 2-variable LP:
    ///
    /// minimize    x0 + 2*x1
    /// subject to  x0 + x1 = 1
    ///             x0 ≥ 0, x1 ≥ 0
    ///
    /// Optimal: x0 = 1, x1 = 0, obj = 1.0
    #[test]
    fn test_clarabel_lp_simple() {
        let solver = ClarabelLpSolver::new();

        // A = [[1, 1]]  (1×2 CSC)
        let prob = SparseProblem {
            n_col: 2,
            n_row: 1,
            col_cost: vec![1.0, 2.0],
            col_lower: vec![0.0, 0.0],
            col_upper: vec![1e30, 1e30],
            row_lower: vec![1.0],
            row_upper: vec![1.0],
            // CSC for [[1,1]]: colptr=[0,1,2], rowval=[0,0], val=[1,1]
            a_start: vec![0, 1, 2],
            a_index: vec![0, 0],
            a_value: vec![1.0, 1.0],
            q_start: None,
            q_index: None,
            q_value: None,
            integrality: None,
        };

        let opts = LpOptions {
            tolerance: 1e-8,
            time_limit_secs: None,
            print_level: 0,
        };

        let result = solver
            .solve(&prob, &opts)
            .expect("Clarabel LP solve failed");

        assert!(
            matches!(result.status, LpSolveStatus::Optimal),
            "Expected Optimal, got {:?}",
            result.status
        );

        let obj = result.objective;
        assert!(
            (obj - 1.0).abs() < 1e-5,
            "Expected objective ≈ 1.0, got {obj:.6}"
        );

        // x0 should be ≈ 1, x1 ≈ 0
        let x0 = result.x[0];
        let x1 = result.x[1];
        assert!((x0 - 1.0).abs() < 1e-5, "Expected x0 ≈ 1.0, got {x0:.6}");
        assert!(x1.abs() < 1e-5, "Expected x1 ≈ 0.0, got {x1:.6}");
    }

    /// Simple QP:
    ///
    /// minimize    x0^2 + x1^2
    /// subject to  x0 + x1 = 1
    ///             x0 ≥ 0, x1 ≥ 0
    ///
    /// Optimal: x0 = x1 = 0.5, obj = 0.5
    #[test]
    fn test_clarabel_qp_simple() {
        let solver = ClarabelLpSolver::new();

        // P = 2*I (Clarabel: 0.5*x'Px = x'x, so P = 2I)
        // CSC for 2x2 diagonal 2I: colptr=[0,1,2], rowval=[0,1], val=[2,2]
        let prob = SparseProblem {
            n_col: 2,
            n_row: 1,
            col_cost: vec![0.0, 0.0],
            col_lower: vec![0.0, 0.0],
            col_upper: vec![1e30, 1e30],
            row_lower: vec![1.0],
            row_upper: vec![1.0],
            a_start: vec![0, 1, 2],
            a_index: vec![0, 0],
            a_value: vec![1.0, 1.0],
            // Q = 2I upper triangle: P such that 0.5*x'Px = x'x
            q_start: Some(vec![0, 1, 2]),
            q_index: Some(vec![0, 1]),
            q_value: Some(vec![2.0, 2.0]),
            integrality: None,
        };

        let opts = LpOptions::default();
        let result = solver
            .solve(&prob, &opts)
            .expect("Clarabel QP solve failed");

        assert!(
            matches!(result.status, LpSolveStatus::Optimal),
            "Expected Optimal, got {:?}",
            result.status
        );

        let obj = result.objective;
        // 0.5 * x'Px = 0.5 * (0.5^2 * 2 + 0.5^2 * 2) = 0.5 * 1 = 0.5
        assert!(
            (obj - 0.5).abs() < 1e-5,
            "Expected objective ≈ 0.5, got {obj:.6}"
        );
    }
}
