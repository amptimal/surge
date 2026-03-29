// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical validated CSC sparse matrix representation.

use std::ops::AddAssign;

use crate::error::{SparseError, SparseResult};

/// Sparse triplet entry (row, col, value).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Triplet<T> {
    pub row: usize,
    pub col: usize,
    pub val: T,
}

/// Sparse matrix in validated compressed sparse column (CSC) form.
#[derive(Debug, Clone, PartialEq)]
pub struct CscMatrix<T> {
    nrows: usize,
    ncols: usize,
    col_ptrs: Vec<usize>,
    row_indices: Vec<usize>,
    values: Vec<T>,
}

impl<T> CscMatrix<T> {
    /// Build a validated CSC matrix from owned parts.
    pub fn try_new(
        nrows: usize,
        ncols: usize,
        col_ptrs: Vec<usize>,
        row_indices: Vec<usize>,
        values: Vec<T>,
    ) -> SparseResult<Self> {
        validate_csc_pattern(nrows, ncols, &col_ptrs, &row_indices)?;
        if row_indices.len() != values.len() {
            return Err(SparseError::ValueCountMismatch {
                expected: row_indices.len(),
                found: values.len(),
            });
        }
        Ok(Self {
            nrows,
            ncols,
            col_ptrs,
            row_indices,
            values,
        })
    }

    pub fn nrows(&self) -> usize {
        self.nrows
    }

    pub fn ncols(&self) -> usize {
        self.ncols
    }

    pub fn nnz(&self) -> usize {
        self.values.len()
    }

    pub fn is_square(&self) -> bool {
        self.nrows == self.ncols
    }

    pub fn col_ptrs(&self) -> &[usize] {
        &self.col_ptrs
    }

    pub fn row_indices(&self) -> &[usize] {
        &self.row_indices
    }

    pub fn values(&self) -> &[T] {
        &self.values
    }

    pub fn values_mut(&mut self) -> &mut [T] {
        &mut self.values
    }

    pub fn into_parts(self) -> (usize, usize, Vec<usize>, Vec<usize>, Vec<T>) {
        (
            self.nrows,
            self.ncols,
            self.col_ptrs,
            self.row_indices,
            self.values,
        )
    }

    pub fn has_same_pattern<U>(&self, other: &CscMatrix<U>) -> bool {
        self.nrows == other.nrows
            && self.ncols == other.ncols
            && self.col_ptrs == other.col_ptrs
            && self.row_indices == other.row_indices
    }

    pub fn has_same_pattern_slices(&self, col_ptrs: &[usize], row_indices: &[usize]) -> bool {
        self.col_ptrs == col_ptrs && self.row_indices == row_indices
    }
}

impl<T> CscMatrix<T>
where
    T: Clone,
{
    /// Convert CSC indices to `i32` with checked bounds.
    pub fn try_to_i32(&self) -> SparseResult<(Vec<i32>, Vec<i32>, Vec<T>)> {
        let col_ptrs = self
            .col_ptrs
            .iter()
            .copied()
            .map(|value| try_usize_to_i32("column pointer", value))
            .collect::<SparseResult<Vec<_>>>()?;
        let row_indices = self
            .row_indices
            .iter()
            .copied()
            .map(|value| try_usize_to_i32("row index", value))
            .collect::<SparseResult<Vec<_>>>()?;
        Ok((col_ptrs, row_indices, self.values.clone()))
    }
}

impl<T> CscMatrix<T>
where
    T: Copy + AddAssign,
{
    /// Convert triplets to canonical CSC, summing duplicate entries.
    pub fn try_from_triplets(
        nrows: usize,
        ncols: usize,
        triplets: &[Triplet<T>],
    ) -> SparseResult<Self> {
        let tuples = triplets
            .iter()
            .map(|triplet| (triplet.row, triplet.col, triplet.val));
        build_from_triplets(nrows, ncols, tuples)
    }
}

fn build_from_triplets<T, I>(nrows: usize, ncols: usize, triplets: I) -> SparseResult<CscMatrix<T>>
where
    T: Copy + AddAssign,
    I: IntoIterator<Item = (usize, usize, T)>,
{
    let mut sorted = Vec::new();
    for (row, col, value) in triplets {
        if row >= nrows || col >= ncols {
            return Err(SparseError::TripletOutOfBounds {
                row,
                col,
                nrows,
                ncols,
            });
        }
        sorted.push((row, col, value));
    }

    sorted.sort_by(|lhs, rhs| lhs.1.cmp(&rhs.1).then(lhs.0.cmp(&rhs.0)));

    let mut row_indices = Vec::with_capacity(sorted.len());
    let mut values = Vec::with_capacity(sorted.len());
    let mut entry_cols = Vec::with_capacity(sorted.len());

    let mut previous = None;
    for (row, col, value) in sorted {
        if previous == Some((row, col)) {
            *values
                .last_mut()
                .expect("duplicate entry requires an existing merged value") += value;
        } else {
            row_indices.push(row);
            values.push(value);
            entry_cols.push(col);
            previous = Some((row, col));
        }
    }

    let mut col_ptrs = vec![0usize; ncols + 1];
    for col in entry_cols {
        col_ptrs[col + 1] += 1;
    }
    for col in 0..ncols {
        col_ptrs[col + 1] += col_ptrs[col];
    }

    CscMatrix::try_new(nrows, ncols, col_ptrs, row_indices, values)
}

pub(crate) fn validate_csc_pattern(
    nrows: usize,
    ncols: usize,
    col_ptrs: &[usize],
    row_indices: &[usize],
) -> SparseResult<()> {
    let expected = ncols + 1;
    if col_ptrs.len() != expected {
        return Err(SparseError::InvalidColPtrLen {
            expected,
            found: col_ptrs.len(),
        });
    }

    let Some(&first) = col_ptrs.first() else {
        return Err(SparseError::InvalidColPtrLen { expected, found: 0 });
    };
    if first != 0 {
        return Err(SparseError::ColPtrMustStartAtZero { found: first });
    }

    for index in 1..col_ptrs.len() {
        if col_ptrs[index] < col_ptrs[index - 1] {
            return Err(SparseError::ColPtrNotMonotonic {
                index,
                current: col_ptrs[index],
                prev_index: index - 1,
                previous: col_ptrs[index - 1],
            });
        }
    }

    let last = *col_ptrs.last().expect("validated non-empty col_ptrs");
    if last != row_indices.len() {
        return Err(SparseError::InvalidColPtrEnd {
            expected: row_indices.len(),
            found: last,
        });
    }

    for (index, &row) in row_indices.iter().enumerate() {
        if row >= nrows {
            return Err(SparseError::RowIndexOutOfBounds { index, row, nrows });
        }
    }

    for col in 0..ncols {
        let start = col_ptrs[col];
        let end = col_ptrs[col + 1];
        for index in start + 1..end {
            if row_indices[index] <= row_indices[index - 1] {
                return Err(SparseError::RowIndicesNotStrictlyIncreasing {
                    col,
                    previous: row_indices[index - 1],
                    current: row_indices[index],
                });
            }
        }
    }

    Ok(())
}

pub(crate) fn try_usize_to_i32(what: &'static str, value: usize) -> SparseResult<i32> {
    i32::try_from(value).map_err(|_| SparseError::IndexTooLarge { what, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triplets_merge_duplicates_into_sorted_csc() {
        let matrix = CscMatrix::try_from_triplets(
            3,
            3,
            &[
                Triplet {
                    row: 2,
                    col: 2,
                    val: 1.0,
                },
                Triplet {
                    row: 0,
                    col: 0,
                    val: 1.0,
                },
                Triplet {
                    row: 1,
                    col: 0,
                    val: 4.0,
                },
                Triplet {
                    row: 0,
                    col: 0,
                    val: 2.0,
                },
            ],
        )
        .expect("triplets should produce a valid CSC matrix");

        assert_eq!(matrix.col_ptrs(), &[0, 2, 2, 3]);
        assert_eq!(matrix.row_indices(), &[0, 1, 2]);
        assert_eq!(matrix.values(), &[3.0, 4.0, 1.0]);
    }

    #[test]
    fn rejects_out_of_bounds_triplet() {
        let error = CscMatrix::try_from_triplets(
            2,
            2,
            &[
                Triplet {
                    row: 0,
                    col: 0,
                    val: 1.0,
                },
                Triplet {
                    row: 2,
                    col: 1,
                    val: 5.0,
                },
            ],
        )
        .expect_err("triplets with invalid coordinates must be rejected");
        assert!(matches!(
            error,
            SparseError::TripletOutOfBounds {
                row: 2,
                col: 1,
                nrows: 2,
                ncols: 2
            }
        ));
    }

    #[test]
    fn rejects_duplicate_rows_in_existing_csc() {
        let error = CscMatrix::try_new(2, 1, vec![0, 2], vec![0, 0], vec![1.0, 2.0])
            .expect_err("existing CSC with duplicate row indices must be rejected");
        assert!(matches!(
            error,
            SparseError::RowIndicesNotStrictlyIncreasing { .. }
        ));
    }

    #[test]
    fn checked_i32_conversion_rejects_large_indices() {
        let matrix =
            CscMatrix::try_new(1, 1, vec![0, 1], vec![0], vec![2.0]).expect("valid matrix");
        let overflow = CscMatrix::try_new(1, 1, vec![0, i32::MAX as usize + 1], vec![0], vec![2.0])
            .expect_err("oversized CSC structure must be rejected by validation");
        assert!(matches!(overflow, SparseError::InvalidColPtrEnd { .. }));

        let (col_ptrs, row_indices, values) =
            matrix.try_to_i32().expect("small indices should convert");
        assert_eq!(col_ptrs, vec![0, 1]);
        assert_eq!(row_indices, vec![0]);
        assert_eq!(values, vec![2.0]);
    }

    /// Empty matrix: all entries are zero (no triplets) for a 3x3 matrix.
    #[test]
    fn empty_matrix_all_zeros() {
        let mat = CscMatrix::<f64>::try_from_triplets(3, 3, &[]).unwrap();
        assert_eq!(mat.nrows(), 3);
        assert_eq!(mat.ncols(), 3);
        assert_eq!(mat.nnz(), 0);
        assert_eq!(mat.col_ptrs(), &[0, 0, 0, 0]);
        assert!(mat.row_indices().is_empty());
        assert!(mat.values().is_empty());
    }

    /// Out-of-order triplets that still merge correctly.
    #[test]
    fn out_of_order_triplets_merge_correctly() {
        // Triplets submitted in reverse order: (1,1), (0,1), (1,0), (0,0)
        let triplets = vec![
            Triplet {
                row: 1,
                col: 1,
                val: 4.0,
            },
            Triplet {
                row: 0,
                col: 1,
                val: 2.0,
            },
            Triplet {
                row: 1,
                col: 0,
                val: 3.0,
            },
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        // CSC should be column-sorted with rows in order.
        assert_eq!(mat.col_ptrs(), &[0, 2, 4]);
        assert_eq!(mat.row_indices(), &[0, 1, 0, 1]);
        assert_eq!(mat.values(), &[1.0, 3.0, 2.0, 4.0]);
    }

    /// Dense matrix: every entry present in a 3x3 matrix.
    #[test]
    fn dense_matrix_all_entries() {
        let mut triplets = Vec::new();
        for row in 0..3 {
            for col in 0..3 {
                triplets.push(Triplet {
                    row,
                    col,
                    val: (row * 3 + col + 1) as f64,
                });
            }
        }
        let mat = CscMatrix::try_from_triplets(3, 3, &triplets).unwrap();
        assert_eq!(mat.nnz(), 9);
        assert_eq!(mat.col_ptrs(), &[0, 3, 6, 9]);
        // Column 0: rows 0,1,2 with values 1,4,7
        assert_eq!(mat.row_indices(), &[0, 1, 2, 0, 1, 2, 0, 1, 2]);
        assert_eq!(mat.values(), &[1.0, 4.0, 7.0, 2.0, 5.0, 8.0, 3.0, 6.0, 9.0]);
    }

    /// Duplicates across multiple triplets at the same position are summed.
    #[test]
    fn multiple_duplicates_summed() {
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
                row: 0,
                col: 0,
                val: 3.0,
            },
            Triplet {
                row: 1,
                col: 1,
                val: 10.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(2, 2, &triplets).unwrap();
        assert_eq!(mat.nnz(), 2);
        assert!((mat.values()[0] - 6.0_f64).abs() < 1e-14); // 1 + 2 + 3
        assert!((mat.values()[1] - 10.0_f64).abs() < 1e-14);
    }

    /// 0x0 matrix can be constructed.
    #[test]
    fn zero_dimension_matrix() {
        let mat = CscMatrix::<f64>::try_new(0, 0, vec![0], vec![], vec![]).unwrap();
        assert_eq!(mat.nrows(), 0);
        assert_eq!(mat.ncols(), 0);
        assert_eq!(mat.nnz(), 0);
        assert!(mat.is_square());
    }

    /// has_same_pattern correctly detects matching and non-matching patterns.
    #[test]
    fn has_same_pattern_check() {
        let a = CscMatrix::try_new(2, 2, vec![0, 1, 2], vec![0, 1], vec![1.0, 2.0]).unwrap();
        let b = CscMatrix::try_new(2, 2, vec![0, 1, 2], vec![0, 1], vec![9.0, 8.0]).unwrap();
        let c = CscMatrix::try_new(
            2,
            2,
            vec![0, 2, 4],
            vec![0, 1, 0, 1],
            vec![1.0, 2.0, 3.0, 4.0],
        )
        .unwrap();

        assert!(a.has_same_pattern(&b), "same pattern, different values");
        assert!(!a.has_same_pattern(&c), "different patterns");
    }

    /// into_parts decomposes the matrix correctly.
    #[test]
    fn into_parts_roundtrip() {
        let mat = CscMatrix::try_new(2, 2, vec![0, 1, 2], vec![0, 1], vec![5.0, 6.0]).unwrap();
        let (nrows, ncols, cp, ri, vals) = mat.into_parts();
        assert_eq!(nrows, 2);
        assert_eq!(ncols, 2);
        assert_eq!(cp, vec![0, 1, 2]);
        assert_eq!(ri, vec![0, 1]);
        assert_eq!(vals, vec![5.0, 6.0]);
    }

    /// Single-column matrix (tall).
    #[test]
    fn single_column_matrix() {
        let triplets = vec![
            Triplet {
                row: 0,
                col: 0,
                val: 1.0,
            },
            Triplet {
                row: 2,
                col: 0,
                val: 3.0,
            },
        ];
        let mat = CscMatrix::try_from_triplets(4, 1, &triplets).unwrap();
        assert_eq!(mat.nrows(), 4);
        assert_eq!(mat.ncols(), 1);
        assert!(!mat.is_square());
        assert_eq!(mat.nnz(), 2);
        assert_eq!(mat.col_ptrs(), &[0, 2]);
        assert_eq!(mat.row_indices(), &[0, 2]);
    }

    /// Rejects non-zero starting column pointer.
    #[test]
    fn rejects_nonzero_start_col_ptr() {
        let err = CscMatrix::try_new(2, 2, vec![1, 1, 2], vec![1], vec![1.0]).unwrap_err();
        assert!(matches!(err, SparseError::ColPtrMustStartAtZero { .. }));
    }

    /// Rejects non-monotonic column pointers.
    #[test]
    fn rejects_non_monotonic_col_ptrs() {
        let err = CscMatrix::try_new(2, 2, vec![0, 2, 1], vec![0, 1], vec![1.0, 2.0]).unwrap_err();
        assert!(matches!(err, SparseError::ColPtrNotMonotonic { .. }));
    }

    /// values_mut allows in-place modification.
    #[test]
    fn values_mut_modification() {
        let mut mat = CscMatrix::try_new(2, 2, vec![0, 1, 2], vec![0, 1], vec![1.0, 2.0]).unwrap();
        mat.values_mut()[0] = 99.0;
        assert_eq!(mat.values(), &[99.0, 2.0]);
    }
}
