// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Single-period Security-Constrained Economic Dispatch (SCED).
//!
//! Extends the DC-OPF sparse formulation with:
//! - Ramp rate constraints (as tightened variable bounds)
//! - Reserve co-optimization via generic `reserve_lp` module
//!
//! Variables: x = [θ | Pg | P_hvdc | e_g | sto_ch | sto_dis | sto_epi_dis | sto_epi_ch | DL | vbid | reserve_block]

pub(crate) mod ac;
pub(super) mod bounds;
pub(super) mod extract;
pub(super) mod frequency;
pub(super) mod layout;
pub(super) mod objective;
pub(super) mod plan;
pub(super) mod problem;
pub(super) mod rows;
pub(crate) mod security;
pub(super) mod solve;

#[cfg(test)]
pub(crate) use crate::hvdc::HvdcBand;
#[cfg(test)]
pub(crate) use crate::hvdc::HvdcDispatchLink;
#[cfg(test)]
pub(crate) use solve::solve_sced;

// Re-export items needed by the test module (use super::* in tests.rs)
#[cfg(test)]
pub(crate) use crate::legacy::DispatchOptions;
#[cfg(test)]
pub(crate) use crate::request::RampMode;
#[cfg(test)]
pub(crate) use surge_network::Network;

#[cfg(test)]
mod tests;
