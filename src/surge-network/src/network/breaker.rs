// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Circuit breaker rating data for fault duty comparison.

use serde::{Deserialize, Serialize};

/// Circuit breaker rating at a bus.
///
/// Used by surge-fault to compare computed fault duties against breaker
/// capabilities, flagging buses where duties exceed interrupting ratings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakerRating {
    /// Bus number where the breaker is installed.
    pub bus: u32,
    /// Breaker name / identifier.
    pub name: String,
    /// Rated voltage (kV).
    pub rated_kv: f64,
    /// Symmetrical interrupting capability (kA).
    pub interrupting_ka: f64,
    /// Close-and-latch asymmetrical peak (kA).
    pub momentary_ka: Option<f64>,
    /// Rated interrupting time (cycles at system frequency).
    pub clearing_time_cycles: f64,
    /// In-service status.
    pub in_service: bool,
}
