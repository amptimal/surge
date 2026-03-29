// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use thiserror::Error;

pub type SparseResult<T> = Result<T, SparseError>;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum SparseError {
    #[error("matrix must be square, found {nrows}x{ncols}")]
    MatrixNotSquare { nrows: usize, ncols: usize },
    #[error("empty matrices are not supported by this operation")]
    EmptyMatrix,
    #[error("CSC column pointer length must be ncols + 1 = {expected}, found {found}")]
    InvalidColPtrLen { expected: usize, found: usize },
    #[error("CSC column pointers must start at 0, found {found}")]
    ColPtrMustStartAtZero { found: usize },
    #[error(
        "CSC column pointers must be nondecreasing, but col_ptrs[{index}]={current} < col_ptrs[{prev_index}]={previous}"
    )]
    ColPtrNotMonotonic {
        index: usize,
        current: usize,
        prev_index: usize,
        previous: usize,
    },
    #[error("CSC final column pointer {found} must equal nnz {expected}")]
    InvalidColPtrEnd { expected: usize, found: usize },
    #[error("CSC row index {row} at position {index} is out of bounds for {nrows} rows")]
    RowIndexOutOfBounds {
        index: usize,
        row: usize,
        nrows: usize,
    },
    #[error(
        "CSC row indices must be strictly increasing within each column; column {col} has {previous} followed by {current}"
    )]
    RowIndicesNotStrictlyIncreasing {
        col: usize,
        previous: usize,
        current: usize,
    },
    #[error("triplet ({row}, {col}) is out of bounds for matrix {nrows}x{ncols}")]
    TripletOutOfBounds {
        row: usize,
        col: usize,
        nrows: usize,
        ncols: usize,
    },
    #[error("{what} {value} exceeds i32::MAX required by SuiteSparse KLU")]
    IndexTooLarge { what: &'static str, value: usize },
    #[error("value buffer length {found} does not match expected nnz {expected}")]
    ValueCountMismatch { expected: usize, found: usize },
    #[error("RHS length {found} does not match matrix dimension {expected}")]
    RhsLengthMismatch { expected: usize, found: usize },
    #[error("KLU factorization has not been computed yet")]
    NotFactorized,
    #[error("matrix pattern does not match the solver's analyzed sparsity pattern")]
    PatternMismatch,
    #[error("size overflow while computing {what}")]
    SizeOverflow { what: &'static str },
    #[error("SuiteSparse KLU symbolic analysis failed")]
    KluAnalyzeFailed,
    #[error("SuiteSparse KLU numeric factorization failed")]
    KluFactorFailed,
    #[error("SuiteSparse KLU numeric refactorization failed")]
    KluRefactorFailed,
    #[error("SuiteSparse KLU reciprocal condition estimate failed")]
    KluRcondFailed,
    #[error(
        "SuiteSparse KLU factorization is ill-conditioned: rcond={rcond:.3e} below strict threshold {threshold:.3e}"
    )]
    KluIllConditioned { rcond: f64, threshold: f64 },
    #[error("SuiteSparse KLU solve failed")]
    KluSolveFailed,
    #[error("Y-bus is missing diagonal entry ({index}, {index})")]
    MissingDiagonal { index: usize },
}
