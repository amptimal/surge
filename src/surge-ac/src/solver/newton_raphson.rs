// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Newton-Raphson AC power flow solver.
//!
//! Solves the nonlinear AC power flow equations using Newton's method
//! with quadratic convergence. This is the gold standard for well-conditioned
//! power systems.
//!
//! This module is a facade that re-exports types and functions from its
//! sub-modules:
//! - `nr_options` — solver options, enums, and error types
//! - `nr_q_limits` — reactive power limit enforcement
//! - `nr_bus_setup` — bus classification and initialization helpers
//! - `nr_prepared` — prepared fixed-pattern NR solve
//! - `nr_interchange` — area interchange enforcement
//! - `nr_solve` — main solve logic, kernel, multi-island

use surge_network::Network;
use surge_solution::PfSolution;

// Used by tests via `use super::*`
#[cfg(test)]
use crate::matrix::ybus::build_ybus;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use surge_network::network::BusType;
#[cfg(test)]
use surge_solution::SolveStatus;

// ── Re-exports from sub-modules ────────────────────────────────────────

// Options, enums, error types
pub use super::nr_options::{
    AcPfError, AcPfOptions, DcLineModel, QSharingMode, SlackAttributionMode, StartupPolicy,
    WarmStart,
};
pub use surge_network::{AngleReference, DistributedAngleWeight};

// Prepared solve
pub use super::nr_prepared::{PreparedAcPf, PreparedStart};

// Main kernel entry point
pub use super::nr_solve::solve_ac_pf_kernel;

// Items used by other crate modules (pub(crate))
pub(crate) use super::nr_q_limits::collect_q_limits;

// Re-export for tests that use `super::build_island_network`
#[cfg(test)]
pub(crate) use super::nr_solve::build_island_network;

// Items used by tests via `use super::*`
#[cfg(test)]
pub(crate) use super::nr_bus_setup::apply_generator_p_limit_demotions;
#[cfg(test)]
use super::nr_kernel::ZipBusData;
#[cfg(test)]
use super::nr_kernel::populate_state_dependent_specs;

/// Solve AC power flow using Newton-Raphson method.
///
/// This is the primary entry point for AC power flow.  It routes through
/// `solve_ac_pf_with_dc_lines`, which handles FACTS
/// expansion and HVDC line injections before invoking the inner KLU-based
/// Newton-Raphson solver.  Networks without DC lines or FACTS devices incur
/// no overhead — the fast path calls [`solve_ac_pf_kernel`] directly.
///
/// Outer loops handled here:
///
/// - **Topology reduction** (`options.auto_reduce_topology`): reduces
///   node-breaker to bus-branch before solving.
/// - **DC line model** (`options.dc_line_model`): honored via `solve_ac_pf_with_dc_lines`.
/// - **Area interchange enforcement** (`options.enforce_interchange`): APF-
///   weighted generation redistributed per area until tie-line flows match
///   scheduled interchange within tolerance.
///
/// For the inner KLU-based Newton-Raphson solver without any outer loops, see
/// [`solve_ac_pf_kernel`].
///
/// # Example
///
/// ```ignore
/// use surge_ac::{AcPfOptions, solve_ac_pf};
///
/// let net = todo!("construct or load a Network in your application");
/// let sol = solve_ac_pf(&net, &AcPfOptions::default()).unwrap();
/// assert_eq!(sol.status, surge_solution::SolveStatus::Converged);
/// println!("iterations={}, mismatch={:.2e}", sol.iterations, sol.max_mismatch);
/// ```
pub fn solve_ac_pf(network: &Network, options: &AcPfOptions) -> Result<PfSolution, AcPfError> {
    network
        .validate()
        .map_err(|e| AcPfError::InvalidNetwork(e.to_string()))?;
    solve_ac_pf_validated(network, options)
}

fn solve_ac_pf_validated(
    network: &Network,
    options: &AcPfOptions,
) -> Result<PfSolution, AcPfError> {
    // Auto-reduce node-breaker topology to bus-branch before solving.
    if options.auto_reduce_topology && network.topology.is_some() {
        let reduced = surge_topology::rebuild_topology(network)
            .map_err(|e| AcPfError::InvalidNetwork(e.to_string()))?;
        let mut opts = options.clone();
        opts.auto_reduce_topology = false; // prevent infinite recursion
        return solve_ac_pf_validated(&reduced, &opts);
    }

    if !options.enforce_interchange || network.area_schedules.is_empty() {
        return crate::ac_dc::solve_ac_pf_with_dc_lines(network, options);
    }

    super::nr_interchange::solve_ac_pf_with_interchange(network, options)
}

#[cfg(test)]
#[path = "nr_tests.rs"]
mod tests;
