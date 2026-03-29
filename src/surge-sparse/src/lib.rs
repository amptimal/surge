// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Validated sparse matrix infrastructure for Surge solvers.

mod complex_klu;
mod csc;
mod error;
mod klu;

pub use complex_klu::ComplexKluSolver;
pub use csc::{CscMatrix, Triplet};
pub use error::{SparseError, SparseResult};
pub use klu::KluSolver;
