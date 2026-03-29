// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical HVDC solver options.
//!
//! A single [`HvdcOptions`] struct controls all stable solver paths
//! (sequential, block-coupled, hybrid MTDC). Previously these were separate options types;
//! they are now consolidated here.

use crate::result::HvdcMethod;

/// Options for the HVDC power flow solver.
///
/// Controls all solver paths via [`HvdcMethod`]. Fields that only apply to
/// specific solvers are documented as such and ignored by other paths.
#[derive(Debug, Clone)]
pub struct HvdcOptions {
    /// Solution method (default: [`HvdcMethod::Auto`]).
    pub method: HvdcMethod,

    /// Convergence tolerance for the outer AC-DC loop (default: 1e-6 pu).
    ///
    /// - Sequential: maximum P/Q change between iterations in MW.
    /// - Block-coupled: coupled residual infinity norm in pu.
    pub tol: f64,

    /// Maximum number of outer/Newton iterations (default: 50).
    pub max_iter: u32,

    /// AC power flow convergence tolerance in per-unit (default: 1e-8).
    ///
    /// Used by Sequential, BlockCoupled, and Hybrid for the inner AC NR solve.
    pub ac_tol: f64,

    /// Maximum NR iterations for each inner AC power flow solve (default: 100).
    ///
    /// Used by Sequential, BlockCoupled, and Hybrid.
    pub max_ac_iter: u32,

    /// Inner DC solver convergence tolerance in per-unit (default: 1e-8).
    ///
    /// Only used by explicit DC-network methods (`BlockCoupled` and `Hybrid`).
    pub dc_tol: f64,

    /// Maximum inner DC solver iterations (default: 50).
    ///
    /// Only used by explicit DC-network methods (`BlockCoupled` and `Hybrid`).
    pub max_dc_iter: u32,

    /// If true, use a flat start (V=1∠0°) for each inner AC power flow.
    ///
    /// Used by Sequential, BlockCoupled, and Hybrid.
    pub flat_start: bool,

    /// Enable cross-coupling sensitivity corrections in the block-coupled
    /// solver (default: true).
    ///
    /// When true, the block-coupled solver computes `dP_ac/dV_dc` and
    /// `dP_dc/dV_ac` corrections to improve convergence on weak AC systems.
    /// When false, the solver degrades to plain alternating AC/DC iteration.
    ///
    /// Only used by BlockCoupled. Ignored by Sequential and Hybrid.
    pub coupling_sensitivities: bool,

    /// Enable coordinated multi-station droop (default: true).
    ///
    /// When true, PVdcDroop stations redistribute DC power imbalance
    /// proportional to their droop gains between Newton iterations.
    /// Only used by BlockCoupled. Ignored by Sequential and Hybrid.
    pub coordinated_droop: bool,
}

impl Default for HvdcOptions {
    fn default() -> Self {
        Self {
            method: HvdcMethod::Auto,
            tol: 1e-6,
            max_iter: 50,
            ac_tol: 1e-8,
            max_ac_iter: 100,
            dc_tol: 1e-8,
            max_dc_iter: 50,
            flat_start: true,
            coupling_sensitivities: true,
            coordinated_droop: true,
        }
    }
}
