// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Neutral-point grounding impedance types for zero-sequence network construction.

use serde::{Deserialize, Serialize};

/// A neutral-point grounding impedance entry from CGMES `Ground`,
/// `GroundingImpedance`, or `PetersenCoil` equipment.
///
/// - `Ground`: solid earthing — `x_ohm = 0`, no min/max.
/// - `GroundingImpedance`: fixed neutral reactor — `x_ohm` from `GroundingImpedance.x`.
/// - `PetersenCoil`: arc-suppression coil — `x_ohm` from `xGroundNominal`,
///   with optional `x_min_ohm`/`x_max_ohm` tuning range from `xGroundMin`/`xGroundMax`.
///
/// Not used in positive-sequence power flow — reserved for zero-seq / 3-phase solver.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroundingEntry {
    /// Bus number where the grounding equipment is connected.
    pub bus: u32,
    /// Grounding reactance in Ohm (0.0 for solid earth).
    pub x_ohm: f64,
    /// PetersenCoil minimum tuning reactance (Ohm). None for solid ground or fixed impedance.
    pub x_min_ohm: Option<f64>,
    /// PetersenCoil maximum tuning reactance (Ohm). None for solid ground or fixed impedance.
    pub x_max_ohm: Option<f64>,
}
