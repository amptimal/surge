// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use serde::{Deserialize, Serialize};

/// Switching device alternate rating set (PSS/E v36).
///
/// Provides one named rating set for a switching device identified by
/// (from\_bus, to\_bus, circuit). Multiple rating sets per device are
/// supported via different `rating_set` values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchingDeviceRatingSet {
    /// From bus number.
    pub from_bus: u32,
    /// To bus number.
    pub to_bus: u32,
    /// Circuit identifier.
    pub circuit: String,
    /// Rating set number.
    pub rating_set: u32,
    /// Normal rating (MVA).
    pub rate1: f64,
    /// Short-term rating (MVA).
    pub rate2: f64,
    /// Emergency rating (MVA).
    pub rate3: f64,
    /// Additional ratings (rate4 through rate12).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_rates: Vec<f64>,
}
