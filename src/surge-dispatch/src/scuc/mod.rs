// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Security-Constrained Unit Commitment (SCUC).
//!
//! Monolithic multi-hour MILP formulation with 3-binary commitment variables:
//! - u[g,t] = 1 if generator g is on in hour t
//! - v[g,t] = 1 if generator g starts up in hour t
//! - w[g,t] = 1 if generator g shuts down in hour t

pub(super) mod bounds;
pub(crate) mod connectivity;
pub(super) mod cuts;
pub(super) mod extract;
pub(super) mod layout;
pub(super) mod losses;
pub(super) mod metadata;
pub(super) mod objective;
pub(super) mod penalty_factors;
pub(super) mod plan;
pub(super) mod pricing;
pub(super) mod problem;
pub(super) mod rows;
pub(super) mod security;
pub(crate) mod snapshot;
pub(super) mod solve;
pub(super) mod types;

pub(crate) use solve::solve_scuc_with_problem_spec;

#[cfg(test)]
mod tests;
