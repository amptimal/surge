// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Area schedule data.
//!
//! Defines control areas, each with a slack bus, a scheduled net export,
//! and a tolerance. Used for inter-area power exchange tracking.
//! PSS/E RAW section: "AREA INTERCHANGE DATA".

use serde::{Deserialize, Serialize};

/// A control area and its scheduled power exchange.
///
/// In PSS/E, each bus belongs to an area. When `AcPfOptions::enforce_interchange`
/// is enabled, the NR outer loop adjusts regulating generators (those with
/// `agc_participation_factor > 0`) to drive actual net interchange toward `p_desired_mw`. Bilateral
/// transfers from `ScheduledAreaTransfer` records are added to the target.
/// When enforcement is disabled, these records are metadata only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaSchedule {
    /// Area number (ARNUM).
    pub number: u32,
    /// Area slack bus number (ISW). Bus that balances the area's generation.
    pub slack_bus: u32,
    /// Desired net real power export from the area in MW (PDES).
    pub p_desired_mw: f64,
    /// Interchange tolerance band in MW (PTOL).
    pub p_tolerance_mw: f64,
    /// Area name (up to 12 characters) (ARNAME).
    pub name: String,
}

impl Default for AreaSchedule {
    fn default() -> Self {
        Self {
            number: 1,
            slack_bus: 0,
            p_desired_mw: 0.0,
            p_tolerance_mw: 10.0,
            name: String::new(),
        }
    }
}
