// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Sparse complex admittance matrix wrapper.

use num_complex::Complex64;

use surge_sparse::{CscMatrix, SparseError, SparseResult};

/// Complex admittance matrix in validated CSC form.
#[derive(Debug, Clone, PartialEq)]
pub struct YBus {
    matrix: CscMatrix<Complex64>,
    diagonal_positions: Vec<usize>,
}

impl YBus {
    /// Build a validated square Y-bus from a CSC matrix.
    pub fn try_new(matrix: CscMatrix<Complex64>) -> SparseResult<Self> {
        if !matrix.is_square() {
            return Err(SparseError::MatrixNotSquare {
                nrows: matrix.nrows(),
                ncols: matrix.ncols(),
            });
        }

        let n = matrix.ncols();
        let mut diagonal_positions = Vec::with_capacity(n);
        for index in 0..n {
            let start = matrix.col_ptrs()[index];
            let end = matrix.col_ptrs()[index + 1];
            let column = &matrix.row_indices()[start..end];
            match column.binary_search(&index) {
                Ok(offset) => diagonal_positions.push(start + offset),
                Err(_) => return Err(SparseError::MissingDiagonal { index }),
            }
        }

        Ok(Self {
            matrix,
            diagonal_positions,
        })
    }

    /// Build a Y-bus directly from owned CSC parts.
    pub fn try_from_csc(
        n: usize,
        col_ptrs: Vec<usize>,
        row_indices: Vec<usize>,
        values: Vec<Complex64>,
    ) -> SparseResult<Self> {
        Self::try_new(CscMatrix::try_new(n, n, col_ptrs, row_indices, values)?)
    }

    pub fn n(&self) -> usize {
        self.matrix.nrows()
    }

    pub fn nnz(&self) -> usize {
        self.matrix.nnz()
    }

    pub fn matrix(&self) -> &CscMatrix<Complex64> {
        &self.matrix
    }

    pub fn matrix_mut(&mut self) -> &mut CscMatrix<Complex64> {
        &mut self.matrix
    }

    /// Add a complex admittance to a diagonal entry.
    pub fn add_to_diagonal(&mut self, index: usize, value: Complex64) -> SparseResult<()> {
        let Some(&position) = self.diagonal_positions.get(index) else {
            return Err(SparseError::MissingDiagonal { index });
        };
        self.matrix.values_mut()[position] += value;
        Ok(())
    }

    pub fn to_dense(&self) -> Vec<Vec<Complex64>> {
        let n = self.n();
        let mut dense = vec![vec![Complex64::new(0.0, 0.0); n]; n];
        for (col, window) in self.matrix.col_ptrs().windows(2).enumerate() {
            for ptr in window[0]..window[1] {
                dense[self.matrix.row_indices()[ptr]][col] = self.matrix.values()[ptr];
            }
        }
        dense
    }

    pub fn fill_ratio(&self) -> f64 {
        if self.n() == 0 {
            return 0.0;
        }
        self.nnz() as f64 / (self.n() * self.n()) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ybus_requires_all_diagonal_entries() {
        let matrix = CscMatrix::try_new(
            2,
            2,
            vec![0, 1, 2],
            vec![0, 0],
            vec![Complex64::new(1.0, 0.0), Complex64::new(2.0, 0.0)],
        )
        .expect("matrix structure should validate");
        let error = YBus::try_new(matrix).expect_err("missing diagonal must be rejected");
        assert!(matches!(error, SparseError::MissingDiagonal { index: 1 }));
    }

    #[test]
    fn add_to_diagonal_updates_cached_position() {
        let mut ybus = YBus::try_from_csc(
            2,
            vec![0, 2, 4],
            vec![0, 1, 0, 1],
            vec![
                Complex64::new(1.0, 0.0),
                Complex64::new(-1.0, 0.0),
                Complex64::new(-1.0, 0.0),
                Complex64::new(1.0, 0.0),
            ],
        )
        .expect("valid Y-bus");

        ybus.add_to_diagonal(1, Complex64::new(0.5, -0.25))
            .expect("diagonal update should succeed");
        assert_eq!(ybus.matrix().values()[3], Complex64::new(1.5, -0.25));
    }

    #[test]
    fn ybus_rejects_non_square_try_from_csc() {
        let mat = CscMatrix::try_new(
            2,
            3,
            vec![0, 1, 2, 3],
            vec![0, 1, 0],
            vec![
                Complex64::new(1.0, 0.0),
                Complex64::new(2.0, 0.0),
                Complex64::new(3.0, 0.0),
            ],
        )
        .unwrap();
        let err = YBus::try_new(mat).unwrap_err();
        assert!(matches!(
            err,
            SparseError::MatrixNotSquare { nrows: 2, ncols: 3 }
        ));
    }

    #[test]
    fn ybus_to_dense_equivalence() {
        let ybus = YBus::try_from_csc(
            3,
            vec![0, 2, 4, 6],
            vec![0, 1, 0, 1, 1, 2],
            vec![
                Complex64::new(2.0, -1.0),
                Complex64::new(-1.0, 0.5),
                Complex64::new(-1.0, 0.5),
                Complex64::new(3.0, -2.0),
                Complex64::new(-2.0, 1.0),
                Complex64::new(2.0, -1.0),
            ],
        )
        .unwrap();

        let dense = ybus.to_dense();
        assert_eq!(dense.len(), 3);
        assert_eq!(dense[0].len(), 3);

        assert_eq!(dense[0][0], Complex64::new(2.0, -1.0));
        assert_eq!(dense[1][0], Complex64::new(-1.0, 0.5));
        assert_eq!(dense[0][1], Complex64::new(-1.0, 0.5));
        assert_eq!(dense[1][1], Complex64::new(3.0, -2.0));
        assert_eq!(dense[1][2], Complex64::new(-2.0, 1.0));
        assert_eq!(dense[2][2], Complex64::new(2.0, -1.0));

        assert_eq!(dense[2][0], Complex64::new(0.0, 0.0));
        assert_eq!(dense[0][2], Complex64::new(0.0, 0.0));
        assert_eq!(dense[2][1], Complex64::new(0.0, 0.0));
    }

    #[test]
    fn ybus_fill_ratio_computation() {
        let diag = YBus::try_from_csc(
            3,
            vec![0, 1, 2, 3],
            vec![0, 1, 2],
            vec![
                Complex64::new(1.0, 0.0),
                Complex64::new(1.0, 0.0),
                Complex64::new(1.0, 0.0),
            ],
        )
        .unwrap();
        assert!((diag.fill_ratio() - 3.0 / 9.0).abs() < 1e-14);

        let full = YBus::try_from_csc(
            2,
            vec![0, 2, 4],
            vec![0, 1, 0, 1],
            vec![
                Complex64::new(1.0, 0.0),
                Complex64::new(2.0, 0.0),
                Complex64::new(3.0, 0.0),
                Complex64::new(4.0, 0.0),
            ],
        )
        .unwrap();
        assert!((full.fill_ratio() - 1.0).abs() < 1e-14);
    }

    #[test]
    fn ybus_1x1() {
        let ybus =
            YBus::try_from_csc(1, vec![0, 1], vec![0], vec![Complex64::new(5.0, -3.0)]).unwrap();

        assert_eq!(ybus.n(), 1);
        assert_eq!(ybus.nnz(), 1);
        assert!((ybus.fill_ratio() - 1.0).abs() < 1e-14);

        let dense = ybus.to_dense();
        assert_eq!(dense[0][0], Complex64::new(5.0, -3.0));
    }

    #[test]
    fn ybus_add_to_diagonal_out_of_range() {
        let mut ybus =
            YBus::try_from_csc(1, vec![0, 1], vec![0], vec![Complex64::new(1.0, 0.0)]).unwrap();

        let err = ybus
            .add_to_diagonal(1, Complex64::new(1.0, 0.0))
            .unwrap_err();
        assert!(matches!(err, SparseError::MissingDiagonal { index: 1 }));
    }

    #[test]
    fn ybus_matrix_mut_modification() {
        let mut ybus =
            YBus::try_from_csc(1, vec![0, 1], vec![0], vec![Complex64::new(1.0, 0.0)]).unwrap();

        ybus.matrix_mut().values_mut()[0] = Complex64::new(99.0, -1.0);
        assert_eq!(ybus.matrix().values()[0], Complex64::new(99.0, -1.0));
    }
}
