// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Lower-level HVDC solve methods for advanced workflows.
//!
//! This module is intentionally separate from the canonical root API. Prefer
//! [`crate::solve_hvdc`] unless you need direct access to a specific solver path.

/// Sequential point-to-point AC/HVDC outer iteration.
pub mod sequential {
    pub use crate::solver::sequential::solve_sequential as solve_hvdc_links;
}

/// Block-coupled AC/DC MTDC solve methods and supporting types.
pub mod block_coupled {
    pub use crate::dc_network::{DcBranch, DcNetwork, DcPfResult};
    pub use crate::solver::block_coupled::{
        AcDcMethod, AcDcSolverMode, BlockCoupledAcDcResult, BlockCoupledAcDcSolverOptions,
        VscStation, VscStationResult, solve_block_coupled_ac_dc as solve,
    };
}

/// Hybrid LCC/VSC MTDC solve methods and supporting types.
pub mod hybrid {
    pub use crate::dc_network::{DcBranch, DcNetwork};
    pub use crate::solver::hybrid_mtdc::{
        HybridMtdcNetwork, HybridMtdcResult, HybridVscConverter, LccConverter, LccConverterResult,
        VscConverterResult, solve_hybrid_mtdc as solve,
    };
}
