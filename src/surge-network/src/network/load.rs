// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Load representation.

use serde::{Deserialize, Serialize};

/// Load class for planning and demand categorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadClass {
    /// Residential customer load.
    Residential,
    /// Commercial customer load.
    Commercial,
    /// Industrial customer load.
    Industrial,
    /// Agricultural customer load (irrigation, processing).
    Agricultural,
    /// Data center load (high power factor, constant demand).
    DataCenter,
    /// Electric vehicle charging load.
    EvCharging,
    /// Uncategorized load.
    Other,
}

/// Winding connection type for fault and unbalanced analysis.
///
/// Determines how the load's zero-sequence impedance participates in
/// short-circuit calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LoadConnection {
    /// Wye-connected with grounded neutral (zero-sequence current path exists).
    #[default]
    WyeGrounded,
    /// Wye-connected with ungrounded (floating) neutral.
    WyeUngrounded,
    /// Delta-connected (no zero-sequence current path).
    Delta,
}

/// A load connected to a bus in the transmission network.
///
/// Supports ZIP (constant impedance / current / power) voltage dependence,
/// frequency sensitivity, and composite load modeling (CMPLDW motor fractions).
/// All power quantities are in MW/MVAr.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Load {
    // --- identity ---
    /// Bus number where the load is connected.
    pub bus: u32,
    /// Optional load identifier.
    #[serde(default)]
    pub id: String,
    /// Load status (true = in service).
    pub in_service: bool,
    /// Whether this load conforms to system-wide scaling forecasts.
    #[serde(default = "default_true")]
    pub conforming: bool,

    // --- steady-state injection ---
    /// Real power demand in MW.
    pub active_power_demand_mw: f64,
    /// Reactive power demand in MVAr.
    pub reactive_power_demand_mvar: f64,

    // --- voltage dependence (ZIP) ---
    /// Constant-impedance P fraction \[0,1\]. Default 0.
    #[serde(default)]
    pub zip_p_impedance_frac: f64,
    /// Constant-current P fraction \[0,1\]. Default 0.
    #[serde(default)]
    pub zip_p_current_frac: f64,
    /// Constant-power P fraction \[0,1\]. Default 1.
    #[serde(default = "default_one")]
    pub zip_p_power_frac: f64,
    /// Constant-impedance Q fraction \[0,1\]. Default 0.
    #[serde(default)]
    pub zip_q_impedance_frac: f64,
    /// Constant-current Q fraction \[0,1\]. Default 0.
    #[serde(default)]
    pub zip_q_current_frac: f64,
    /// Constant-power Q fraction \[0,1\]. Default 1.
    #[serde(default = "default_one")]
    pub zip_q_power_frac: f64,

    // --- frequency dependence ---
    /// Active power frequency sensitivity (%P per Hz). Default 0.
    #[serde(default)]
    pub freq_sensitivity_p_pct_per_hz: f64,
    /// Reactive power frequency sensitivity (%Q per Hz). Default 0.
    #[serde(default)]
    pub freq_sensitivity_q_pct_per_hz: f64,

    // --- composition (CMPLDW bridge) ---
    /// 3-phase large industrial motor fraction \[0,1\]. Default 0.
    #[serde(default)]
    pub frac_motor_a: f64,
    /// 3-phase commercial motor fraction \[0,1\]. Default 0.
    #[serde(default)]
    pub frac_motor_b: f64,
    /// 1-phase A/C compressor motor fraction \[0,1\]. Default 0.
    #[serde(default)]
    pub frac_motor_c: f64,
    /// 1-phase other motor fraction \[0,1\]. Default 0.
    #[serde(default)]
    pub frac_motor_d: f64,
    /// Power electronic load fraction \[0,1\]. Default 0.
    #[serde(default)]
    pub frac_electronic: f64,
    /// Static (ZIP) load fraction \[0,1\]. Default 1.
    #[serde(default = "default_one")]
    pub frac_static: f64,

    // --- classification ---
    /// Load class for planning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_class: Option<LoadClass>,
    /// Winding connection for fault/unbalanced analysis. Default WyeGrounded.
    #[serde(default)]
    pub connection: LoadConnection,
    /// UFLS/UVLS shedding tier (1 = first shed, higher = later).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shedding_priority: Option<u32>,
    /// Ownership entries (PSS/E OWNER field). Single-owner for loads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<super::owner::OwnershipEntry>,
}

use crate::network::serde_defaults::{default_one, default_true};

impl Default for Load {
    fn default() -> Self {
        Self {
            bus: 0,
            active_power_demand_mw: 0.0,
            reactive_power_demand_mvar: 0.0,
            in_service: true,
            conforming: true,
            id: String::new(),
            zip_p_impedance_frac: 0.0,
            zip_p_current_frac: 0.0,
            zip_p_power_frac: 1.0,
            zip_q_impedance_frac: 0.0,
            zip_q_current_frac: 0.0,
            zip_q_power_frac: 1.0,
            freq_sensitivity_p_pct_per_hz: 0.0,
            freq_sensitivity_q_pct_per_hz: 0.0,
            frac_motor_a: 0.0,
            frac_motor_b: 0.0,
            frac_motor_c: 0.0,
            frac_motor_d: 0.0,
            frac_electronic: 0.0,
            frac_static: 1.0,
            load_class: None,
            connection: LoadConnection::WyeGrounded,
            shedding_priority: None,
            owners: Vec::new(),
        }
    }
}

impl Load {
    /// Create a load with the given bus, active power (MW), and reactive power (MVAr).
    pub fn new(bus: u32, active_power_demand_mw: f64, reactive_power_demand_mvar: f64) -> Self {
        Self {
            bus,
            active_power_demand_mw,
            reactive_power_demand_mvar,
            ..Default::default()
        }
    }
}
