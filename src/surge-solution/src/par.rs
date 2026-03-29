// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Phase-shifting transformer (PAR) flow-setpoint types for DC-OPF and SCED.

use serde::{Deserialize, Serialize};

/// A phase-shifting transformer (PAR) operating in flow-setpoint mode.
///
/// When included in `DcOpfOptions::par_setpoints` or `DispatchOptions::par_setpoints`,
/// the PAR branch is removed from the passive B matrix and replaced by fixed
/// scheduled injections at its terminal buses.  The solver then determines
/// the remaining network angles, and the implied shift angle is computed post-solve.
///
/// This is the standard RTO approach for modelling manually-controlled PARs
/// (also called Phase-Angle Regulators or TAPs) in DC market clearing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParSetpoint {
    /// From-bus number (external).
    pub from_bus: u32,
    /// To-bus number (external).
    pub to_bus: u32,
    /// Circuit identifier matching `Branch::circuit`.
    pub circuit: String,
    /// Target MW flow from `from_bus` to `to_bus` (positive = forward direction).
    pub target_mw: f64,
}

/// Post-solve PAR result: actual implied shift angle for a flow-setpoint PAR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParResult {
    /// From-bus number (external).
    pub from_bus: u32,
    /// To-bus number (external).
    pub to_bus: u32,
    /// Circuit identifier.
    pub circuit: String,
    /// Target MW flow requested.
    pub target_mw: f64,
    /// Implied shift angle in degrees: φ = (θ_from − θ_to − target/(base × b_dc)) × (180/π)
    ///
    /// This is the PST angle that would produce the requested flow given the
    /// post-optimization network angles.  Positive = from_bus leads to_bus.
    pub implied_shift_deg: f64,
    /// Whether the implied shift is within the branch control's radian bounds.
    ///
    /// `false` when the required angle exceeds the PAR's mechanical limits.
    pub within_limits: bool,
}
