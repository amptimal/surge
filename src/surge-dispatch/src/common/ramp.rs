// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared ramp-rate helpers for SCED and SCUC.

use surge_network::network::Generator;

use crate::request::RampMode;

/// Compute ramp-up rate (MW/min) for a generator based on the active RampMode.
pub(crate) fn ramp_up_for_mode(g: &Generator, prev_mw: f64, mode: &RampMode) -> f64 {
    match mode {
        RampMode::Averaged => g.ramp_up_avg_mw_per_min().unwrap_or(f64::MAX),
        RampMode::Interpolated => g.ramp_up_at_mw(prev_mw).unwrap_or(f64::MAX),
        RampMode::Block { .. } => f64::MAX, // Per-block ramp bounds handle this
    }
}

/// Compute ramp-down rate (MW/min) for a generator based on the active RampMode.
pub(crate) fn ramp_dn_for_mode(g: &Generator, prev_mw: f64, mode: &RampMode) -> f64 {
    match mode {
        RampMode::Averaged => g.ramp_down_avg_mw_per_min().unwrap_or(f64::MAX),
        RampMode::Interpolated => g.ramp_down_at_mw(prev_mw).unwrap_or(f64::MAX),
        RampMode::Block { .. } => f64::MAX, // Per-block ramp bounds handle this
    }
}
