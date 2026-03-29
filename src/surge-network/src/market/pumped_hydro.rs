// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Pumped hydro storage unit model.

use serde::{Deserialize, Serialize};

use crate::network::GeneratorRef;

/// Pumped hydro storage unit — a synchronous machine overlay.
///
/// The solver sees the underlying Generator via a stable generator reference.
/// This struct
/// carries the hydro-specific constraints that the dispatch engine needs:
/// reservoir dynamics, mode transitions, head dependence, penstock limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PumpedHydroUnit {
    /// Unit name.
    pub name: String,
    /// Stable reference to the synchronous machine the solver sees.
    pub generator: GeneratorRef,

    // --- operating modes ---
    /// Fixed-speed (pump is constant-P) vs variable-speed (pump is dispatchable).
    pub variable_speed: bool,
    /// Fixed-speed pump draw (MW). Ignored if variable_speed.
    pub pump_mw_fixed: f64,
    /// Variable-speed min pump power (MW).
    pub pump_mw_min: Option<f64>,
    /// Variable-speed max pump power (MW).
    pub pump_mw_max: Option<f64>,
    /// Minutes to switch between generate and pump mode (through zero).
    pub mode_transition_min: f64,
    /// Can operate as synchronous condenser (P=0, Q available).
    pub condenser_capable: bool,
    /// (low_mw, high_mw) around zero where operation is impossible.
    pub forbidden_zone: Option<(f64, f64)>,

    // --- reservoir / energy ---
    /// Upper reservoir capacity in energy-equivalent MWh.
    pub upper_reservoir_mwh: f64,
    /// Lower reservoir capacity (f64::MAX if river/ocean).
    pub lower_reservoir_mwh: f64,
    /// Initial state of charge (MWh).
    pub soc_initial_mwh: f64,
    /// Minimum state of charge (MWh).
    pub soc_min_mwh: f64,
    /// Maximum state of charge (MWh).
    pub soc_max_mwh: f64,
    /// Generation-mode efficiency (typically 0.87-0.93).
    pub efficiency_generate: f64,
    /// Pump-mode efficiency (typically 0.85-0.92).
    pub efficiency_pump: f64,

    // --- head dependence ---
    /// Piecewise-linear (soc_mwh, pmax_mw) — capacity vs reservoir level.
    pub head_curve: Vec<(f64, f64)>,

    // --- hydraulic constraints ---
    /// Number of reversible units at this plant.
    pub n_units: u32,
    /// Total hydraulic throughput limit across all units (MW).
    pub shared_penstock_mw_max: Option<f64>,
    /// Environmental minimum generation (MW, downstream flow).
    pub min_release_mw: f64,
    /// Hydraulic ramp limit (MW/min, fish/flood protection).
    pub ramp_rate_mw_per_min: Option<f64>,

    // --- startup ---
    /// Minutes to synchronize in generate mode.
    pub startup_time_gen_min: f64,
    /// Minutes to synchronize in pump mode.
    pub startup_time_pump_min: f64,
    /// Cost per start ($).
    pub startup_cost: f64,

    // --- market ---
    /// Reserve offers keyed by product ID (generic reserve model).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserve_offers: Vec<crate::market::reserve::ReserveOffer>,
    /// Custom qualification flags for reserve products.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub qualifications: crate::market::reserve::QualificationMap,
}

impl PumpedHydroUnit {
    /// Construct a pumped hydro unit with sensible defaults.
    pub fn new(name: String, generator: GeneratorRef, capacity_mwh: f64) -> Self {
        Self {
            name,
            generator,
            variable_speed: false,
            pump_mw_fixed: 0.0,
            pump_mw_min: None,
            pump_mw_max: None,
            mode_transition_min: 5.0,
            condenser_capable: false,
            forbidden_zone: None,
            upper_reservoir_mwh: capacity_mwh,
            lower_reservoir_mwh: f64::MAX,
            soc_initial_mwh: 0.5 * capacity_mwh,
            soc_min_mwh: 0.0,
            soc_max_mwh: capacity_mwh,
            efficiency_generate: 0.90,
            efficiency_pump: 0.87,
            head_curve: Vec::new(),
            n_units: 1,
            shared_penstock_mw_max: None,
            min_release_mw: 0.0,
            ramp_rate_mw_per_min: None,
            startup_time_gen_min: 5.0,
            startup_time_pump_min: 10.0,
            startup_cost: 0.0,
            reserve_offers: Vec::new(),
            qualifications: std::collections::HashMap::new(),
        }
    }
}
