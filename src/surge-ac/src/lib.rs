// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge AC — Full AC power flow solver
//!
//! Implements multiple solution methods for the nonlinear AC power flow equations:
//!
//! - **Newton-Raphson (NR)**: Quadratic convergence, gold standard for well-conditioned systems
//! - **Fast Decoupled (FDPF)**: P-theta/Q-V decoupling, faster per iteration for HV networks
//!
//! The Jacobian is assembled in sparse CSC format and factored via LU decomposition.

// ── Module tree ──────────────────────────────────────────────────────────
pub mod control;
pub mod matrix;
pub(crate) mod solver;
pub mod topology;

pub mod ac_dc;

#[cfg(test)]
pub(crate) mod test_cases;

// ── Public re-exports ────────────────────────────────────────────────────

pub use control::facts_expansion::expand_facts;
pub use solver::fast_decoupled::{FdpfFactors, FdpfOptions, FdpfResult, FdpfVariant, solve_fdpf};
pub use solver::newton_raphson::{
    AcPfError, AcPfOptions, AngleReference, DcLineModel, DistributedAngleWeight, PreparedAcPf,
    PreparedStart, QSharingMode, SlackAttributionMode, StartupPolicy, WarmStart, solve_ac_pf,
    solve_ac_pf_kernel,
};
pub use solver::nr_kernel::{NrKernelOptions, NrState, NrWorkspace, PreparedNrModel, run_nr_inner};
pub use topology::zero_impedance::{MergedNetwork, expand_pf_solution, merge_zero_impedance};
