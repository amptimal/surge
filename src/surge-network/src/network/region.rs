// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Region (zone) data.
//!
//! A region is a named grouping of buses used for administrative and
//! reporting purposes. Each bus carries a region number (`bus.zone`).
//! PSS/E RAW section: "ZONE DATA".

use serde::{Deserialize, Serialize};

/// A named region (PSS/E zone) — a grouping of buses.
///
/// The region number is carried on each bus as `bus.zone`. This struct
/// provides the name lookup table so that zone numbers can be resolved
/// to human-readable names during export and reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    /// Region number (ZONUM in PSS/E).
    pub number: u32,
    /// Region name, up to 12 characters (ZONAME in PSS/E).
    pub name: String,
}

impl Default for Region {
    fn default() -> Self {
        Self {
            number: 1,
            name: String::new(),
        }
    }
}
