// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Experimental HVDC methods that are not part of the stable root API.

/// Experimental simultaneous AC/DC Newton solve.
pub mod simultaneous {
    pub use crate::solver::simultaneous::{
        SimultaneousAcDcSolverOptions, solve_simultaneous_ac_dc as solve,
    };
}
