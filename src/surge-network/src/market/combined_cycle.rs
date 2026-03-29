// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Combined cycle plant modeling.

use serde::{Deserialize, Serialize};

use crate::market::EnergyOffer;

/// A single combined cycle configuration (e.g. "1x0", "2x1", "2x1_duct").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedCycleConfig {
    /// Configuration name ("1x0", "2x1", "2x1_duct", etc.).
    pub name: String,
    /// Generator indices online in this config.
    pub gen_indices: Vec<usize>,
    /// Aggregate min output (MW).
    pub p_min_mw: f64,
    /// Aggregate max output (MW).
    pub p_max_mw: f64,
    /// Heat rate curve: (MW, BTU/MWh) segments.
    pub heat_rate_curve: Vec<(f64, f64)>,
    /// Energy offer for this config.
    pub energy_offer: Option<EnergyOffer>,
    /// Ramp-up curve for this config: (MW operating point, MW/min).
    pub ramp_up_curve: Vec<(f64, f64)>,
    /// Ramp-down curve for this config.
    pub ramp_down_curve: Vec<(f64, f64)>,
    /// No-load cost ($/hr) for this configuration. Zero by default;
    /// overridden by market offer `ConfigOffer.no_load_cost` at dispatch time.
    #[serde(default)]
    pub no_load_cost: f64,
    /// Minimum up time in this config (hours).
    pub min_up_time_hr: f64,
    /// Minimum down time after leaving this config (hours).
    pub min_down_time_hr: f64,
    /// Reserve offers keyed by product ID (generic reserve model).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserve_offers: Vec<crate::market::reserve::ReserveOffer>,
    /// Custom qualification flags for reserve products.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub qualifications: crate::market::reserve::QualificationMap,
}

/// A transition between two combined cycle configurations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedCycleTransition {
    /// Source configuration name.
    pub from_config: String,
    /// Destination configuration name.
    pub to_config: String,
    /// Transition time (minutes).
    pub transition_time_min: f64,
    /// Transition cost ($).
    pub transition_cost: f64,
    /// Can transition while serving load (hot) vs must go offline.
    pub online_transition: bool,
}

/// A combined cycle power plant with multiple configurations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedCyclePlant {
    /// Plant name.
    pub name: String,
    /// Available configurations.
    pub configs: Vec<CombinedCycleConfig>,
    /// Allowed transitions between configurations.
    pub transitions: Vec<CombinedCycleTransition>,
    /// Currently active configuration. None = all offline.
    pub active_config: Option<String>,
    /// Hours in current configuration.
    pub hours_in_config: f64,
    /// Informational — duct firing modeled as a separate config.
    pub duct_firing_capable: bool,
}
