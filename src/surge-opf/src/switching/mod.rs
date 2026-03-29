// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Topology optimization: optimal transmission switching (OTS) and
//! optimal reactive power dispatch (ORPD).

pub mod orpd;
pub mod ots;

pub use orpd::{OrpdObjective, OrpdOptions, OrpdResult, solve_orpd};
pub use ots::{
    OtsFormulation, OtsOptions, OtsResult, OtsRuntime, SwitchableSet, solve_ots,
    solve_ots_with_runtime,
};
