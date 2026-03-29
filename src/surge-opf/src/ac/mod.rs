// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC-OPF family: nonlinear polar formulation with canonical NLP runtime selection.

pub mod hvdc;
pub mod loss_factors;
pub(crate) mod mapping;
mod nlp_impl;
pub(crate) mod pq_curve;
mod problem;
pub mod sensitivity;
pub mod solve;
mod sparsity;
pub mod types;

// Re-export primary public API
pub use loss_factors::compute_ac_marginal_loss_factors;
pub use sensitivity::BendersCut;
pub use solve::{solve_ac_opf, solve_ac_opf_with_runtime};
pub use types::{AcOpfError, AcOpfOptions, AcOpfRuntime, DiscreteMode, WarmStart};
