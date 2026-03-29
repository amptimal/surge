// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Multi-section line grouping data.
//!
//! A multi-section line is a transmission line modeled as multiple series
//! sections with dummy (intermediate) buses. The grouping record identifies
//! the terminal buses and the dummy buses that connect the sections.
//! PSS/E RAW section: "MULTI-SECTION LINE DATA".

use serde::{Deserialize, Serialize};

/// A multi-section line grouping.
///
/// The individual line sections are stored as separate branches in
/// `Network::branches`. This struct records the grouping metadata:
/// which terminal buses the overall line connects, and which dummy
/// buses are the internal section boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiSectionLineGroup {
    /// From-bus number (I) — first terminal.
    pub from_bus: u32,
    /// To-bus number (J) — second terminal.
    pub to_bus: u32,
    /// Line identifier (ID).
    pub id: String,
    /// Metered end: 1 = from bus, 2 = to bus (MET).
    pub metered_end: u32,
    /// Dummy (intermediate) bus numbers connecting sections.
    /// The actual line has `dummy_buses.len() + 1` sections.
    pub dummy_buses: Vec<u32>,
}

impl Default for MultiSectionLineGroup {
    fn default() -> Self {
        Self {
            from_bus: 0,
            to_bus: 0,
            id: "1".to_string(),
            metered_end: 1,
            dummy_buses: Vec::new(),
        }
    }
}
