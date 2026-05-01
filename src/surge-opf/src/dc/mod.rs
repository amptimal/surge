// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DC-OPF family: linear/QP formulations, LMP decomposition, loss factors.

pub(crate) mod costs;
pub mod island_lmp;
pub mod loss_factors;
pub mod opf;
pub mod opf_lp;

pub use island_lmp::{
    IslandRefs, decompose_lmp_lossless, decompose_lmp_with_losses, detect_island_refs,
    find_split_ref_bus, fix_island_theta_bounds,
};
pub use loss_factors::{
    compute_dc_loss_sensitivities, compute_dc_loss_sensitivities_adjoint, compute_total_dc_losses,
};
pub use opf::{
    DcOpfError, DcOpfOptions, DcOpfResult, DcOpfRuntime, HvdcOpfLink, solve_dc_opf,
    solve_dc_opf_with_runtime,
};
pub use opf_lp::{solve_dc_opf_lp, solve_dc_opf_lp_with_runtime, triplets_to_csc};
