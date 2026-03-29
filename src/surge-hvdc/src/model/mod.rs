// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! HVDC converter data models, control modes, and physics.

pub mod commutation;
pub mod control;
pub mod lcc;
pub mod link;
pub mod vsc;

// Re-export key types at module level.
pub use commutation::{CommutationCheck, check_commutation_failure};
pub use control::{LccHvdcControlMode, VscHvdcControlMode, VscStationState};
pub use lcc::{LccOperatingPoint, TapControl, compute_lcc_operating_point, lcc_converter_results};
pub use link::{HvdcLink, LccHvdcLink, VscHvdcLink};
pub use vsc::{
    VscInjections, vsc_converter_results, vsc_converter_results_with_mode, vsc_injections,
    vsc_losses_mw,
};
