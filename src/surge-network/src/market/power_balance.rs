// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Power balance violation types for dispatch solutions.

use serde::{Deserialize, Serialize};

/// Power balance violation for a single dispatch period.
///
/// When the LP cannot fully balance generation and load even with all
/// available resources dispatched, penalty slack variables absorb the
/// imbalance.  This struct records how much slack was used.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PowerBalanceViolation {
    /// Load curtailment: MW of demand that could not be served (generation < load).
    pub curtailment_mw: f64,
    /// Excess generation: MW of supply that could not be absorbed (generation > load).
    pub excess_mw: f64,
}
