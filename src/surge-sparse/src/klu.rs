// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SuiteSparse KLU sparse LU factorization wrapper.

use std::ffi::c_int;
use std::os::raw::c_void;
use std::ptr;

use crate::csc::{CscMatrix, try_usize_to_i32, validate_csc_pattern};
use crate::error::{SparseError, SparseResult};

const STRICT_RCOND_THRESHOLD: f64 = 1e-12;

#[repr(C)]
struct KluCommon {
    tol: f64,
    memgrow: f64,
    initmem_amd: f64,
    initmem: f64,
    maxwork: f64,
    btf: c_int,
    ordering: c_int,
    scale: c_int,
    user_order:
        Option<unsafe extern "C" fn(i32, *mut i32, *mut i32, *mut i32, *mut KluCommon) -> i32>,
    user_data: *mut c_void,
    halt_if_singular: c_int,
    status: c_int,
    nrealloc: c_int,
    structural_rank: i32,
    numerical_rank: i32,
    singular_col: i32,
    noffdiag: i32,
    flops: f64,
    rcond: f64,
    condest: f64,
    rgrowth: f64,
    work: f64,
    memusage: usize,
    mempeak: usize,
}

enum KluSymbolic {}
enum KluNumeric {}

unsafe extern "C" {
    fn klu_defaults(common: *mut KluCommon) -> c_int;
    fn klu_analyze(
        n: i32,
        ap: *const i32,
        ai: *const i32,
        common: *mut KluCommon,
    ) -> *mut KluSymbolic;
    fn klu_factor(
        ap: *const i32,
        ai: *const i32,
        ax: *const f64,
        symbolic: *mut KluSymbolic,
        common: *mut KluCommon,
    ) -> *mut KluNumeric;
    fn klu_refactor(
        ap: *const i32,
        ai: *const i32,
        ax: *const f64,
        symbolic: *mut KluSymbolic,
        numeric: *mut KluNumeric,
        common: *mut KluCommon,
    ) -> c_int;
    fn klu_solve(
        symbolic: *mut KluSymbolic,
        numeric: *mut KluNumeric,
        ldim: i32,
        nrhs: i32,
        b: *mut f64,
        common: *mut KluCommon,
    ) -> c_int;
    fn klu_tsolve(
        symbolic: *mut KluSymbolic,
        numeric: *mut KluNumeric,
        ldim: i32,
        nrhs: i32,
        b: *mut f64,
        common: *mut KluCommon,
    ) -> c_int;
    fn klu_free_symbolic(symbolic: *mut *mut KluSymbolic, common: *mut KluCommon) -> c_int;
    fn klu_free_numeric(numeric: *mut *mut KluNumeric, common: *mut KluCommon) -> c_int;
    fn klu_rcond(
        symbolic: *mut KluSymbolic,
        numeric: *mut KluNumeric,
        common: *mut KluCommon,
    ) -> c_int;
}

/// SuiteSparse KLU factorization for a fixed square CSC sparsity pattern.
pub struct KluSolver {
    common: KluCommon,
    symbolic: *mut KluSymbolic,
    numeric: *mut KluNumeric,
    dim: i32,
    nnz: usize,
    col_ptrs: Vec<i32>,
    row_indices: Vec<i32>,
}

unsafe impl Send for KluSolver {}

impl KluSolver {
    /// Analyze a square CSC sparsity pattern.
    pub fn new(dim: usize, col_ptrs: &[usize], row_indices: &[usize]) -> SparseResult<Self> {
        validate_csc_pattern(dim, dim, col_ptrs, row_indices)?;
        Self::analyze(dim, col_ptrs, row_indices)
    }

    /// Analyze the sparsity pattern from a validated CSC matrix.
    pub fn from_csc<T>(matrix: &CscMatrix<T>) -> SparseResult<Self> {
        if !matrix.is_square() {
            return Err(SparseError::MatrixNotSquare {
                nrows: matrix.nrows(),
                ncols: matrix.ncols(),
            });
        }
        Self::analyze(matrix.nrows(), matrix.col_ptrs(), matrix.row_indices())
    }

    fn analyze(dim: usize, col_ptrs: &[usize], row_indices: &[usize]) -> SparseResult<Self> {
        if dim == 0 {
            return Err(SparseError::EmptyMatrix);
        }

        let dim = try_usize_to_i32("matrix dimension", dim)?;
        let col_ptrs = col_ptrs
            .iter()
            .copied()
            .map(|value| try_usize_to_i32("column pointer", value))
            .collect::<SparseResult<Vec<_>>>()?;
        let row_indices = row_indices
            .iter()
            .copied()
            .map(|value| try_usize_to_i32("row index", value))
            .collect::<SparseResult<Vec<_>>>()?;

        let mut common = unsafe { std::mem::zeroed() };
        unsafe {
            klu_defaults(&mut common);
        }

        let symbolic =
            unsafe { klu_analyze(dim, col_ptrs.as_ptr(), row_indices.as_ptr(), &mut common) };
        if symbolic.is_null() {
            return Err(SparseError::KluAnalyzeFailed);
        }

        Ok(Self {
            common,
            symbolic,
            numeric: ptr::null_mut(),
            dim,
            nnz: row_indices.len(),
            col_ptrs,
            row_indices,
        })
    }

    fn clear_numeric(&mut self) {
        if !self.numeric.is_null() {
            unsafe {
                klu_free_numeric(&mut self.numeric, &mut self.common);
            }
        }
    }

    pub fn factor(&mut self, values: &[f64]) -> SparseResult<()> {
        self.validate_values(values)?;

        self.clear_numeric();

        self.numeric = unsafe {
            klu_factor(
                self.col_ptrs.as_ptr(),
                self.row_indices.as_ptr(),
                values.as_ptr(),
                self.symbolic,
                &mut self.common,
            )
        };
        if self.numeric.is_null() {
            return Err(SparseError::KluFactorFailed);
        }

        if let Err(error) = self.finish_factorization(SparseError::KluFactorFailed) {
            self.clear_numeric();
            return Err(error);
        }
        Ok(())
    }

    pub fn refactor(&mut self, values: &[f64]) -> SparseResult<()> {
        self.validate_values(values)?;
        if self.numeric.is_null() {
            return Err(SparseError::NotFactorized);
        }

        let ok = unsafe {
            klu_refactor(
                self.col_ptrs.as_ptr(),
                self.row_indices.as_ptr(),
                values.as_ptr(),
                self.symbolic,
                self.numeric,
                &mut self.common,
            )
        };
        if ok == 0 {
            self.clear_numeric();
            return Err(SparseError::KluRefactorFailed);
        }

        if let Err(error) = self.finish_factorization(SparseError::KluRefactorFailed) {
            self.clear_numeric();
            return Err(error);
        }
        Ok(())
    }

    pub fn solve(&mut self, rhs: &mut [f64]) -> SparseResult<()> {
        self.validate_rhs(rhs)?;

        let ok = unsafe {
            klu_solve(
                self.symbolic,
                self.numeric,
                self.dim,
                1,
                rhs.as_mut_ptr(),
                &mut self.common,
            )
        };
        if ok == 0 {
            return Err(SparseError::KluSolveFailed);
        }
        Ok(())
    }

    pub fn solve_transpose(&mut self, rhs: &mut [f64]) -> SparseResult<()> {
        self.validate_rhs(rhs)?;

        let ok = unsafe {
            klu_tsolve(
                self.symbolic,
                self.numeric,
                self.dim,
                1,
                rhs.as_mut_ptr(),
                &mut self.common,
            )
        };
        if ok == 0 {
            return Err(SparseError::KluSolveFailed);
        }
        Ok(())
    }

    pub fn rcond(&self) -> f64 {
        self.common.rcond
    }

    fn validate_values(&self, values: &[f64]) -> SparseResult<()> {
        if values.len() != self.nnz {
            return Err(SparseError::ValueCountMismatch {
                expected: self.nnz,
                found: values.len(),
            });
        }
        Ok(())
    }

    fn validate_rhs(&self, rhs: &[f64]) -> SparseResult<()> {
        if self.numeric.is_null() {
            return Err(SparseError::NotFactorized);
        }
        if rhs.len() != self.dim as usize {
            return Err(SparseError::RhsLengthMismatch {
                expected: self.dim as usize,
                found: rhs.len(),
            });
        }
        Ok(())
    }

    fn finish_factorization(&mut self, ill_conditioned_error: SparseError) -> SparseResult<()> {
        let ok = unsafe { klu_rcond(self.symbolic, self.numeric, &mut self.common) };
        if ok == 0 {
            return Err(SparseError::KluRcondFailed);
        }
        let rcond = self.common.rcond;
        if !rcond.is_finite() || rcond < STRICT_RCOND_THRESHOLD {
            return Err(match ill_conditioned_error {
                SparseError::KluFactorFailed => SparseError::KluIllConditioned {
                    rcond,
                    threshold: STRICT_RCOND_THRESHOLD,
                },
                SparseError::KluRefactorFailed => SparseError::KluIllConditioned {
                    rcond,
                    threshold: STRICT_RCOND_THRESHOLD,
                },
                other => other,
            });
        }
        Ok(())
    }

    /// Solve AX = B in-place for multiple RHS columns.
    ///
    /// `rhs` must contain `dim * nrhs` values laid out column-major, so
    /// column `j` occupies `rhs[j * dim .. (j + 1) * dim]`.
    pub fn solve_many(&mut self, rhs: &mut [f64], nrhs: usize) -> SparseResult<()> {
        if self.numeric.is_null() {
            return Err(SparseError::NotFactorized);
        }
        let expected = self.dim as usize * nrhs;
        if rhs.len() != expected {
            return Err(SparseError::RhsLengthMismatch {
                expected,
                found: rhs.len(),
            });
        }
        if nrhs == 0 {
            return Ok(());
        }
        let nrhs_i32 = try_usize_to_i32("nrhs", nrhs)?;
        let ok = unsafe {
            klu_solve(
                self.symbolic,
                self.numeric,
                self.dim,
                nrhs_i32,
                rhs.as_mut_ptr(),
                &mut self.common,
            )
        };
        if ok == 0 {
            return Err(SparseError::KluSolveFailed);
        }
        Ok(())
    }
}

impl Drop for KluSolver {
    fn drop(&mut self) {
        unsafe {
            if !self.numeric.is_null() {
                klu_free_numeric(&mut self.numeric, &mut self.common);
            }
            if !self.symbolic.is_null() {
                klu_free_symbolic(&mut self.symbolic, &mut self.common);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CscMatrix, Triplet};

    /// 2x2 identity: [[1,0],[0,1]] * x = [3,7] → x = [3,7]
    #[test]
    fn klu_identity_2x2() {
        let mat = CscMatrix::try_from_triplets(
            2,
            2,
            &[
                Triplet {
                    row: 0,
                    col: 0,
                    val: 1.0,
                },
                Triplet {
                    row: 1,
                    col: 1,
                    val: 1.0,
                },
            ],
        )
        .unwrap();

        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        let mut rhs = vec![3.0, 7.0];
        solver.solve(&mut rhs).unwrap();
        assert!((rhs[0] - 3.0).abs() < 1e-14);
        assert!((rhs[1] - 7.0).abs() < 1e-14);
    }

    /// 3x3 lower-triangular: [[2,0,0],[1,3,0],[0,4,5]] * x = [6,10,29] → x = [3,7/3,...]
    #[test]
    fn klu_lower_triangular() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 2.0,
            },
            Triplet {
                row: 1,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 3.0,
            },
            Triplet {
                row: 2,
                col: 1,
                val: 4.0,
            },
            Triplet {
                row: 2,
                col: 2,
                val: 5.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(3, 3, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        // Solve Ax = b where b = A * [1, 2, 3]
        let x_exact = [1.0, 2.0, 3.0];
        let mut rhs = vec![2.0 * 1.0, 1.0 * 1.0 + 3.0 * 2.0, 4.0 * 2.0 + 5.0 * 3.0];
        solver.solve(&mut rhs).unwrap();
        for i in 0..3 {
            assert!(
                (rhs[i] - x_exact[i]).abs() < 1e-12,
                "x[{i}] = {} expected {}",
                rhs[i],
                x_exact[i],
            );
        }
    }

    /// Sparse 4x4 SPD matrix solve.
    #[test]
    fn klu_sparse_4x4() {
        // Tridiagonal: [[4,-1,0,0],[-1,4,-1,0],[0,-1,4,-1],[0,0,-1,4]]
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 4.0,
            },
            Triplet {
                row: 1,
                col: 0,
                val: -1.0,
            },
            Triplet {
                row: 0,
                col: 1,
                val: -1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 4.0,
            },
            Triplet {
                row: 2,
                col: 1,
                val: -1.0,
            },
            Triplet {
                row: 1,
                col: 2,
                val: -1.0,
            },
            Triplet {
                row: 2,
                col: 2,
                val: 4.0,
            },
            Triplet {
                row: 3,
                col: 2,
                val: -1.0,
            },
            Triplet {
                row: 2,
                col: 3,
                val: -1.0,
            },
            Triplet {
                row: 3,
                col: 3,
                val: 4.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(4, 4, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        let mut rhs = vec![1.0, 0.0, 0.0, 1.0];
        solver.solve(&mut rhs).unwrap();

        // Verify A * x ≈ b (original rhs)
        let b_orig = [1.0, 0.0, 0.0, 1.0];
        let ax = [
            4.0 * rhs[0] - rhs[1],
            -rhs[0] + 4.0 * rhs[1] - rhs[2],
            -rhs[1] + 4.0 * rhs[2] - rhs[3],
            -rhs[2] + 4.0 * rhs[3],
        ];
        for i in 0..4 {
            assert!(
                (ax[i] - b_orig[i]).abs() < 1e-12,
                "residual[{i}] = {:.2e}",
                (ax[i] - b_orig[i]).abs(),
            );
        }
    }

    /// Refactoring with new values on the same pattern.
    #[test]
    fn klu_refactor_same_pattern() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 2.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 3.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        let mut rhs = vec![4.0, 9.0];
        solver.solve(&mut rhs).unwrap();
        assert!((rhs[0] - 2.0).abs() < 1e-14);
        assert!((rhs[1] - 3.0).abs() < 1e-14);

        // Refactor with different values: [[5,0],[0,10]]
        solver.refactor(&[5.0, 10.0]).unwrap();
        let mut rhs2 = vec![15.0, 30.0];
        solver.solve(&mut rhs2).unwrap();
        assert!((rhs2[0] - 3.0).abs() < 1e-14);
        assert!((rhs2[1] - 3.0).abs() < 1e-14);
    }

    /// Transpose solve: A^T x = b.
    #[test]
    fn klu_transpose_solve() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 0,
                val: 2.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 3.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        // A^T = [[1,2],[0,3]], A^T * x = [5, 6] → x = [1, 2]
        let mut rhs = vec![5.0, 6.0];
        solver.solve_transpose(&mut rhs).unwrap();
        assert!((rhs[0] - 1.0).abs() < 1e-14);
        assert!((rhs[1] - 2.0).abs() < 1e-14);
    }

    /// Multiple RHS solve.
    #[test]
    fn klu_solve_many() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 2.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 5.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        // Two RHS stored column-major: [4,10, 6,15]
        let mut rhs = vec![4.0, 10.0, 6.0, 15.0];
        solver.solve_many(&mut rhs, 2).unwrap();
        assert!((rhs[0] - 2.0).abs() < 1e-14); // 4/2
        assert!((rhs[1] - 2.0).abs() < 1e-14); // 10/5
        assert!((rhs[2] - 3.0).abs() < 1e-14); // 6/2
        assert!((rhs[3] - 3.0).abs() < 1e-14); // 15/5
    }

    /// Error: solve before factoring.
    #[test]
    fn klu_solve_before_factor_errors() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 1.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();

        let mut rhs = vec![1.0, 2.0];
        let err = solver.solve(&mut rhs).unwrap_err();
        assert!(matches!(err, SparseError::NotFactorized));
    }

    /// Error: wrong RHS length.
    #[test]
    fn klu_rhs_length_mismatch() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 1.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        let mut rhs = vec![1.0, 2.0, 3.0]; // wrong length
        let err = solver.solve(&mut rhs).unwrap_err();
        assert!(matches!(err, SparseError::RhsLengthMismatch { .. }));
    }

    /// COO to CSC with duplicate entries are summed.
    #[test]
    fn csc_triplet_duplicates_summed() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 0,
                col: 0,
                val: 2.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 5.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        assert_eq!(mat.nnz(), 2); // duplicates merged
        assert!((mat.values()[0] - 3.0_f64).abs() < 1e-14); // 1 + 2
    }

    /// Large sparse identity: factor and solve.
    #[test]
    fn klu_large_identity() {
        let n = 500;
        let triplets: Vec<Triplet<f64>> = (0..n)
            .map(|i| Triplet {
                row: i,
                col: i,
                val: 1.0,
            })
            .collect();
        let mat = CscMatrix::try_from_triplets(n, n, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        let mut rhs: Vec<f64> = (0..n).map(|i| i as f64).collect();
        solver.solve(&mut rhs).unwrap();
        for (i, val) in rhs.iter().enumerate() {
            assert!((val - i as f64).abs() < 1e-12);
        }
    }

    /// Structurally singular matrix: column 1 has no entries.
    /// The pattern [[1,0],[0,0]] has an empty column so factor should fail.
    #[test]
    fn klu_structurally_singular_empty_column() {
        // Only one entry at (0,0), column 1 is entirely empty.
        let triplets = vec![Triplet {
            row: 0,
            col: 0,
            val: 1.0,
        }];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        let result = solver.factor(mat.values());
        assert!(
            result.is_err(),
            "factoring a structurally singular matrix should fail"
        );
    }

    /// Numerically singular matrix: [[1,1],[1,1]] has rank 1.
    /// Strict factorization should reject the low reciprocal condition number.
    #[test]
    fn klu_numerically_singular_matrix() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 0,
                col: 1,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 1.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        let err = solver.factor(mat.values()).unwrap_err();
        assert!(
            matches!(
                err,
                SparseError::KluIllConditioned { .. } | SparseError::KluFactorFailed
            ),
            "singular matrix should be rejected, got {err:?}"
        );
    }

    /// Ill-conditioned matrix should produce a very small rcond.
    #[test]
    fn klu_ill_conditioned_rcond() {
        // Hilbert-like 3x3 matrix is ill-conditioned.
        // [[1, 1/2, 1/3], [1/2, 1/3, 1/4], [1/3, 1/4, 1/5]]
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 0,
                val: 0.5,
            },
            Triplet {
                row: 2,
                col: 0,
                val: 1.0 / 3.0,
            },
            Triplet {
                row: 0,
                col: 1,
                val: 0.5,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 1.0 / 3.0,
            },
            Triplet {
                row: 2,
                col: 1,
                val: 0.25,
            },
            Triplet {
                row: 0,
                col: 2,
                val: 1.0 / 3.0,
            },
            Triplet {
                row: 1,
                col: 2,
                val: 0.25,
            },
            Triplet {
                row: 2,
                col: 2,
                val: 0.2,
            },
        ];
        let mat = CscMatrix::try_from_triplets(3, 3, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        // Hilbert matrices are notoriously ill-conditioned; rcond should be small.
        assert!(
            solver.rcond() < 0.1,
            "rcond for a 3x3 Hilbert matrix should be small, got {}",
            solver.rcond()
        );
        assert!(solver.rcond() > 0.0, "rcond should still be positive");
    }

    /// Refactor with values that become zero (diagonal zeros).
    #[test]
    fn klu_refactor_with_zero_values() {
        // Start with a well-conditioned diagonal.
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 2.0,
            },
            Triplet {
                row: 0,
                col: 1,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 3.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        // Verify the initial factorization works.
        let mut rhs = vec![3.0, 4.0];
        solver.solve(&mut rhs).unwrap();

        // Refactor with values making the matrix singular: [[0,1],[1,0]] has det = -1 but
        // refactor with [[1,1],[1,1]] to make it singular.
        let singular_vals: Vec<f64> = vec![1.0, 1.0, 1.0, 1.0]; // [[1,1],[1,1]]
        let err = solver.refactor(&singular_vals).unwrap_err();
        assert!(
            matches!(
                err,
                SparseError::KluIllConditioned { .. } | SparseError::KluRefactorFailed
            ),
            "refactoring to singular values should be rejected, got {err:?}"
        );

        let mut stale_rhs = vec![3.0, 4.0];
        let solve_err = solver.solve(&mut stale_rhs).unwrap_err();
        assert!(
            matches!(solve_err, SparseError::NotFactorized),
            "failed refactor must invalidate numeric factors, got {solve_err:?}"
        );
    }

    /// solve_many with multiple RHS columns on a non-diagonal matrix.
    #[test]
    fn klu_solve_many_non_diagonal() {
        // A = [[2, 1], [1, 3]]
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 2.0,
            },
            Triplet {
                row: 1,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 0,
                col: 1,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 3.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        // Three RHS columns: b1 = A*[1,0], b2 = A*[0,1], b3 = A*[1,1]
        // b1 = [2,1], b2 = [1,3], b3 = [3,4]
        let mut rhs = vec![2.0, 1.0, 1.0, 3.0, 3.0, 4.0];
        solver.solve_many(&mut rhs, 3).unwrap();
        assert!((rhs[0] - 1.0).abs() < 1e-12, "col1[0]={}", rhs[0]);
        assert!((rhs[1] - 0.0).abs() < 1e-12, "col1[1]={}", rhs[1]);
        assert!((rhs[2] - 0.0).abs() < 1e-12, "col2[0]={}", rhs[2]);
        assert!((rhs[3] - 1.0).abs() < 1e-12, "col2[1]={}", rhs[3]);
        assert!((rhs[4] - 1.0).abs() < 1e-12, "col3[0]={}", rhs[4]);
        assert!((rhs[5] - 1.0).abs() < 1e-12, "col3[1]={}", rhs[5]);
    }

    /// solve_many with zero RHS columns is a no-op.
    #[test]
    fn klu_solve_many_zero_columns() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 1.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        let mut rhs = vec![];
        solver.solve_many(&mut rhs, 0).unwrap();
        assert!(rhs.is_empty());
    }

    /// solve_many rejects incorrect buffer length.
    #[test]
    fn klu_solve_many_wrong_length() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 1.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();
        solver.factor(mat.values()).unwrap();

        let mut rhs = vec![1.0, 2.0, 3.0]; // dim=2, nrhs=2 needs 4 elements
        let err = solver.solve_many(&mut rhs, 2).unwrap_err();
        assert!(matches!(err, SparseError::RhsLengthMismatch { .. }));
    }

    /// Wrong value count for factor should fail.
    #[test]
    fn klu_factor_wrong_value_count() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 1.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();

        let err = solver.factor(&[1.0]).unwrap_err(); // expects 2 values
        assert!(matches!(err, SparseError::ValueCountMismatch { .. }));
    }

    /// Refactor before any factor should fail.
    #[test]
    fn klu_refactor_before_factor() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 1.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        let mut solver = KluSolver::from_csc(&mat).unwrap();

        let err = solver.refactor(&[2.0, 3.0]).unwrap_err();
        assert!(matches!(err, SparseError::NotFactorized));
    }

    /// Constructing a KluSolver with a non-square matrix should fail.
    #[test]
    fn klu_rejects_non_square_from_csc() {
        let mat =
            CscMatrix::try_new(2, 3, vec![0, 1, 2, 3], vec![0, 1, 0], vec![1.0, 2.0, 3.0]).unwrap();
        let result = KluSolver::from_csc(&mat);
        assert!(matches!(result, Err(SparseError::MatrixNotSquare { .. })));
    }

    /// Constructing a KluSolver with a 0x0 matrix should fail.
    #[test]
    fn klu_rejects_empty_matrix() {
        let mat = CscMatrix::<f64>::try_new(0, 0, vec![0], vec![], vec![]).unwrap();
        let result = KluSolver::from_csc(&mat);
        assert!(matches!(result, Err(SparseError::EmptyMatrix)));
    }
}
