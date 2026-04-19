// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Public HVDC dispatch configuration types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A single dispatch band for multi-band HVDC control.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HvdcBand {
    /// Band identifier (e.g. "base", "economic", "emergency").
    pub id: String,
    /// Minimum transfer for this band (MW).
    pub p_min_mw: f64,
    /// Maximum transfer for this band (MW).
    pub p_max_mw: f64,
    /// Marginal cost for this band ($/MWh).
    pub cost_per_mwh: f64,
    /// Linear loss fraction for this band.
    pub loss_b_frac: f64,
    /// Per-band ramp rate (MW/min). 0 = inherits link-level ramp.
    pub ramp_mw_per_min: f64,
    /// Whether headroom in this band can provide upward reserve.
    pub reserve_eligible_up: bool,
    /// Whether reduction from this band can provide downward reserve.
    pub reserve_eligible_down: bool,
    /// Maximum continuous duration (hours). 0 = unlimited.
    pub max_duration_hours: f64,
}

/// HVDC link description for dispatch co-optimization.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HvdcDispatchLink {
    /// Stable public identifier. Falls back to `name` and then a synthesized id when empty.
    #[serde(default)]
    pub id: String,
    /// Human-readable name (e.g. "HVDC_North_South").
    pub name: String,
    /// AC bus number at the rectifier (power source) end.
    pub from_bus: u32,
    /// AC bus number at the inverter (power sink) end.
    pub to_bus: u32,
    /// Minimum DC power transfer (MW). Can be 0 or negative for bidirectional.
    pub p_dc_min_mw: f64,
    /// Maximum DC power transfer (MW).
    pub p_dc_max_mw: f64,
    /// Constant loss coefficient `a` (MW): P_loss = a + b * P_dc.
    pub loss_a_mw: f64,
    /// Linear loss coefficient `b` (fraction): P_loss = a + b * P_dc.
    pub loss_b_frac: f64,
    /// Ramp rate limit (MW/min). 0 = unlimited.
    pub ramp_mw_per_min: f64,
    /// Dispatch cost per MWh (usually 0 for HVDC).
    pub cost_per_mwh: f64,
    /// Multi-band control segments. Empty = single-band legacy mode using
    /// the flat fields above.
    pub bands: Vec<HvdcBand>,
}

impl HvdcDispatchLink {
    /// Returns `true` if this link uses multi-band dispatch.
    pub fn is_banded(&self) -> bool {
        !self.bands.is_empty()
    }

    /// Number of LP variables needed for this link.
    pub fn n_vars(&self) -> usize {
        if self.is_banded() {
            self.bands.len()
        } else {
            1
        }
    }
}
