// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Owner data.
//!
//! An owner is a named entity (utility, company) that owns equipment in the
//! network. PSS/E devices (generators, branches, loads) carry owner numbers.
//! PSS/E RAW section: "OWNER DATA".

use serde::{Deserialize, Serialize};

/// A named owner of power system equipment.
///
/// The owner number is referenced by generators, branches, loads, and other
/// devices. This struct provides the name lookup table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Owner {
    /// Owner number (OWNUM in PSS/E).
    pub number: u32,
    /// Owner name, up to 12 characters (OWNAME in PSS/E).
    pub name: String,
}

/// A single (owner_number, fraction) pair from PSS/E multi-owner records.
///
/// Generators, branches, and transformers support up to 4 co-owners with
/// fractional ownership. Buses and loads carry a single entry (fraction = 1.0).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnershipEntry {
    /// Owner number, referencing an entry in `Network.owners`.
    pub owner: u32,
    /// Fractional ownership (0.0..=1.0). Defaults to 1.0 for single-owner devices.
    pub fraction: f64,
}

impl Default for Owner {
    fn default() -> Self {
        Self {
            number: 1,
            name: String::new(),
        }
    }
}
