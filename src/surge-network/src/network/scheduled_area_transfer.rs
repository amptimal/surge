// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Scheduled area transfer data.
//!
//! Defines directional power transfer agreements between control areas.
//! PSS/E RAW section: "INTER-AREA TRANSFER DATA".

use serde::{Deserialize, Serialize};

/// A scheduled inter-area power transfer.
///
/// Represents a bilateral agreement for MW flow from one control area to
/// another. Used for inter-area transfer accounting and area regulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledAreaTransfer {
    /// From-area number (ARFROM in PSS/E).
    pub from_area: u32,
    /// To-area number (ARTO in PSS/E).
    pub to_area: u32,
    /// Transfer identifier (TRID in PSS/E). Allows multiple transfers
    /// between the same area pair.
    pub id: u32,
    /// Scheduled transfer in MW (PTRAN in PSS/E).
    pub p_transfer_mw: f64,
}

impl Default for ScheduledAreaTransfer {
    fn default() -> Self {
        Self {
            from_area: 1,
            to_area: 2,
            id: 1,
            p_transfer_mw: 0.0,
        }
    }
}
