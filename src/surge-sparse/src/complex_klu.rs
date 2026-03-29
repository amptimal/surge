// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Complex sparse direct solver backed by KLU via 2n x 2n real expansion.

use num_complex::Complex64;

use crate::KluSolver;
use crate::csc::CscMatrix;
use crate::error::{SparseError, SparseResult};

#[derive(Debug, Clone, Copy)]
struct SourceEntry {
    top_left: usize,
    top_right: usize,
    bottom_left: usize,
    bottom_right: usize,
}

type RealExpansion = (Vec<usize>, Vec<usize>, Vec<SourceEntry>, Vec<f64>);

/// Complex sparse direct solver for a fixed square CSC sparsity pattern.
pub struct ComplexKluSolver {
    n: usize,
    source_col_ptrs: Vec<usize>,
    source_row_indices: Vec<usize>,
    klu: KluSolver,
    entries: Vec<SourceEntry>,
    values_buf: Vec<f64>,
    real_rhs_buf: Vec<f64>,
}

impl ComplexKluSolver {
    pub fn new(matrix: &CscMatrix<Complex64>) -> SparseResult<Self> {
        if !matrix.is_square() {
            return Err(SparseError::MatrixNotSquare {
                nrows: matrix.nrows(),
                ncols: matrix.ncols(),
            });
        }
        let n = matrix.nrows();
        if n == 0 {
            return Err(SparseError::EmptyMatrix);
        }

        let n2 = checked_mul(n, 2, "real-expanded dimension")?;
        let (col_ptrs, row_indices, entries, values_buf) = build_real_expansion(matrix)?;
        let mut klu = KluSolver::new(n2, &col_ptrs, &row_indices)?;
        klu.factor(&values_buf)?;

        Ok(Self {
            n,
            source_col_ptrs: matrix.col_ptrs().to_vec(),
            source_row_indices: matrix.row_indices().to_vec(),
            klu,
            entries,
            values_buf,
            real_rhs_buf: vec![0.0; n2],
        })
    }

    pub fn factor(&mut self, matrix: &CscMatrix<Complex64>) -> SparseResult<()> {
        self.update_values(matrix)?;
        self.klu.factor(&self.values_buf)
    }

    pub fn refactor(&mut self, matrix: &CscMatrix<Complex64>) -> SparseResult<()> {
        self.update_values(matrix)?;
        match self.klu.refactor(&self.values_buf) {
            Ok(()) => Ok(()),
            Err(SparseError::KluRefactorFailed) => self.klu.factor(&self.values_buf),
            Err(error) => Err(error),
        }
    }

    pub fn solve(&mut self, rhs: &[Complex64]) -> SparseResult<Vec<Complex64>> {
        let mut solution = rhs.to_vec();
        self.solve_in_place(&mut solution)?;
        Ok(solution)
    }

    pub fn solve_in_place(&mut self, rhs: &mut [Complex64]) -> SparseResult<()> {
        if rhs.len() != self.n {
            return Err(SparseError::RhsLengthMismatch {
                expected: self.n,
                found: rhs.len(),
            });
        }

        for (index, value) in rhs.iter().enumerate() {
            self.real_rhs_buf[index] = value.re;
            self.real_rhs_buf[index + self.n] = value.im;
        }
        self.klu.solve(&mut self.real_rhs_buf)?;

        for (index, value) in rhs.iter_mut().enumerate() {
            *value = Complex64::new(self.real_rhs_buf[index], self.real_rhs_buf[index + self.n]);
        }
        Ok(())
    }

    pub fn dim(&self) -> usize {
        self.n
    }

    pub fn rcond(&self) -> f64 {
        self.klu.rcond()
    }

    fn update_values(&mut self, matrix: &CscMatrix<Complex64>) -> SparseResult<()> {
        if !matrix.has_same_pattern_slices(&self.source_col_ptrs, &self.source_row_indices) {
            return Err(SparseError::PatternMismatch);
        }

        let values = matrix.values();
        for (source_index, entry) in self.entries.iter().enumerate() {
            let value = values[source_index];
            self.values_buf[entry.top_left] = value.re;
            self.values_buf[entry.top_right] = -value.im;
            self.values_buf[entry.bottom_left] = value.im;
            self.values_buf[entry.bottom_right] = value.re;
        }
        Ok(())
    }
}

fn build_real_expansion(source: &CscMatrix<Complex64>) -> SparseResult<RealExpansion> {
    let n = source.nrows();
    let source_nnz = source.nnz();
    let n2 = checked_mul(n, 2, "real-expanded dimension")?;
    let total_nnz = checked_mul(source_nnz, 4, "real-expanded nnz")?;
    let col_ptr_len = checked_add(n2, 1, "real-expanded column-pointer length")?;

    let mut col_ptrs = vec![0usize; col_ptr_len];
    let mut row_indices = Vec::with_capacity(total_nnz);
    let mut entries = Vec::with_capacity(total_nnz);
    let mut values = Vec::with_capacity(total_nnz);

    entries.resize(
        source_nnz,
        SourceEntry {
            top_left: 0,
            top_right: 0,
            bottom_left: 0,
            bottom_right: 0,
        },
    );

    let mut nnz = 0usize;
    for (col, window) in source.col_ptrs().windows(2).enumerate() {
        let start = window[0];
        let end = window[1];
        col_ptrs[col] = nnz;

        for ((entry, &row), &value) in entries[start..end]
            .iter_mut()
            .zip(source.row_indices()[start..end].iter())
            .zip(source.values()[start..end].iter())
        {
            row_indices.push(row);
            entry.top_left = nnz;
            values.push(value.re);
            nnz += 1;
        }
        for ((entry, &row), &value) in entries[start..end]
            .iter_mut()
            .zip(source.row_indices()[start..end].iter())
            .zip(source.values()[start..end].iter())
        {
            row_indices.push(row + n);
            entry.bottom_left = nnz;
            values.push(value.im);
            nnz += 1;
        }
        col_ptrs[col + 1] = nnz;
    }

    for (col, window) in source.col_ptrs().windows(2).enumerate() {
        let start = window[0];
        let end = window[1];
        let real_col = col + n;
        col_ptrs[real_col] = nnz;

        for ((entry, &row), &value) in entries[start..end]
            .iter_mut()
            .zip(source.row_indices()[start..end].iter())
            .zip(source.values()[start..end].iter())
        {
            row_indices.push(row);
            entry.top_right = nnz;
            values.push(-value.im);
            nnz += 1;
        }
        for ((entry, &row), &value) in entries[start..end]
            .iter_mut()
            .zip(source.row_indices()[start..end].iter())
            .zip(source.values()[start..end].iter())
        {
            row_indices.push(row + n);
            entry.bottom_right = nnz;
            values.push(value.re);
            nnz += 1;
        }
        col_ptrs[real_col + 1] = nnz;
    }

    Ok((col_ptrs, row_indices, entries, values))
}

fn checked_mul(lhs: usize, rhs: usize, what: &'static str) -> SparseResult<usize> {
    lhs.checked_mul(rhs)
        .ok_or(SparseError::SizeOverflow { what })
}

fn checked_add(lhs: usize, rhs: usize, what: &'static str) -> SparseResult<usize> {
    lhs.checked_add(rhs)
        .ok_or(SparseError::SizeOverflow { what })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CscMatrix;

    fn complex_csc(
        n: usize,
        col_ptrs: Vec<usize>,
        row_indices: Vec<usize>,
        values: Vec<Complex64>,
    ) -> CscMatrix<Complex64> {
        CscMatrix::try_new(n, n, col_ptrs, row_indices, values).unwrap()
    }

    #[test]
    fn rejects_pattern_changes() {
        let mat = complex_csc(1, vec![0, 1], vec![0], vec![Complex64::new(1.0, 2.0)]);
        let mut solver = ComplexKluSolver::new(&mat).expect("valid factorization");

        let changed = complex_csc(1, vec![0, 1], vec![0], vec![Complex64::new(2.0, 3.0)]);
        solver
            .refactor(&changed)
            .expect("value-only change should be allowed");

        let changed_pattern = complex_csc(
            2,
            vec![0, 2, 4],
            vec![0, 1, 0, 1],
            vec![
                Complex64::new(1.0, 0.0),
                Complex64::new(-1.0, 0.0),
                Complex64::new(-1.0, 0.0),
                Complex64::new(1.0, 0.0),
            ],
        );

        let error = solver
            .refactor(&changed_pattern)
            .expect_err("pattern change must be rejected");
        assert_eq!(error, SparseError::PatternMismatch);
    }

    #[test]
    fn solve_in_place_reuses_caller_buffer() {
        let mat = complex_csc(1, vec![0, 1], vec![0], vec![Complex64::new(2.0, 0.0)]);
        let mut solver = ComplexKluSolver::new(&mat).expect("valid factorization");
        let mut rhs = vec![Complex64::new(4.0, 0.0)];
        solver
            .solve_in_place(&mut rhs)
            .expect("solve_in_place should succeed");
        assert_eq!(rhs, vec![Complex64::new(2.0, 0.0)]);
    }

    #[test]
    fn complex_klu_zero_rhs_returns_zero() {
        let mat = complex_csc(
            2,
            vec![0, 2, 4],
            vec![0, 1, 0, 1],
            vec![
                Complex64::new(4.0, -1.0),
                Complex64::new(-1.0, 0.0),
                Complex64::new(-1.0, 0.0),
                Complex64::new(4.0, -1.0),
            ],
        );
        let mut solver = ComplexKluSolver::new(&mat).unwrap();

        let result = solver
            .solve(&[Complex64::new(0.0, 0.0), Complex64::new(0.0, 0.0)])
            .unwrap();
        for val in &result {
            assert!(val.re.abs() < 1e-14, "re should be ~0, got {}", val.re);
            assert!(val.im.abs() < 1e-14, "im should be ~0, got {}", val.im);
        }
    }

    #[test]
    fn complex_klu_pure_real_rhs() {
        let mat = complex_csc(
            2,
            vec![0, 2, 4],
            vec![0, 1, 0, 1],
            vec![
                Complex64::new(2.0, 0.0),
                Complex64::new(1.0, 0.0),
                Complex64::new(1.0, 0.0),
                Complex64::new(3.0, 0.0),
            ],
        );
        let mut solver = ComplexKluSolver::new(&mat).unwrap();

        let result = solver
            .solve(&[Complex64::new(3.0, 0.0), Complex64::new(4.0, 0.0)])
            .unwrap();
        assert!((result[0].re - 1.0).abs() < 1e-12);
        assert!((result[1].re - 1.0).abs() < 1e-12);
        assert!(result[0].im.abs() < 1e-14);
        assert!(result[1].im.abs() < 1e-14);
    }

    #[test]
    fn complex_klu_dim_and_rcond() {
        let mat = complex_csc(
            3,
            vec![0, 1, 2, 3],
            vec![0, 1, 2],
            vec![
                Complex64::new(1.0, 0.0),
                Complex64::new(2.0, 0.0),
                Complex64::new(3.0, 0.0),
            ],
        );
        let solver = ComplexKluSolver::new(&mat).unwrap();

        assert_eq!(solver.dim(), 3);
        assert!(solver.rcond() > 0.0);
        assert!(solver.rcond() <= 1.0);
    }

    #[test]
    fn complex_klu_full_complex_solve() {
        let mat = complex_csc(
            2,
            vec![0, 1, 2],
            vec![0, 1],
            vec![Complex64::new(1.0, 2.0), Complex64::new(3.0, 4.0)],
        );
        let mut solver = ComplexKluSolver::new(&mat).unwrap();

        let result = solver
            .solve(&[Complex64::new(1.0, 2.0), Complex64::new(3.0, 4.0)])
            .unwrap();
        assert!((result[0].re - 1.0).abs() < 1e-12);
        assert!(result[0].im.abs() < 1e-12);
        assert!((result[1].re - 1.0).abs() < 1e-12);
        assert!(result[1].im.abs() < 1e-12);
    }

    #[test]
    fn complex_klu_pure_imaginary_rhs() {
        let mat = complex_csc(
            2,
            vec![0, 1, 2],
            vec![0, 1],
            vec![Complex64::new(2.0, 0.0), Complex64::new(4.0, 0.0)],
        );
        let mut solver = ComplexKluSolver::new(&mat).unwrap();

        let result = solver
            .solve(&[Complex64::new(0.0, 2.0), Complex64::new(0.0, 8.0)])
            .unwrap();
        assert!(result[0].re.abs() < 1e-14);
        assert!((result[0].im - 1.0).abs() < 1e-12);
        assert!(result[1].re.abs() < 1e-14);
        assert!((result[1].im - 2.0).abs() < 1e-12);
    }

    #[test]
    fn complex_klu_wrong_rhs_length() {
        let mat = complex_csc(
            2,
            vec![0, 1, 2],
            vec![0, 1],
            vec![Complex64::new(1.0, 0.0), Complex64::new(1.0, 0.0)],
        );
        let mut solver = ComplexKluSolver::new(&mat).unwrap();

        let err = solver.solve(&[Complex64::new(1.0, 0.0)]).unwrap_err();
        assert!(matches!(err, SparseError::RhsLengthMismatch { .. }));
    }

    #[test]
    fn complex_klu_factor_refactor() {
        let mat1 = complex_csc(1, vec![0, 1], vec![0], vec![Complex64::new(2.0, 0.0)]);
        let mut solver = ComplexKluSolver::new(&mat1).unwrap();

        let result = solver.solve(&[Complex64::new(6.0, 0.0)]).unwrap();
        assert!((result[0].re - 3.0).abs() < 1e-12);

        let mat2 = complex_csc(1, vec![0, 1], vec![0], vec![Complex64::new(3.0, 0.0)]);
        solver.factor(&mat2).unwrap();

        let result = solver.solve(&[Complex64::new(6.0, 0.0)]).unwrap();
        assert!((result[0].re - 2.0).abs() < 1e-12);
    }
}
