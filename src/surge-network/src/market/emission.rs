// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Emission rates and policy types.

use serde::{Deserialize, Serialize};

/// Per-generator multi-pollutant emission rates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmissionRates {
    /// CO2 (tonnes/MWh).
    pub co2: f64,
    /// NOx (tonnes/MWh).
    pub nox: f64,
    /// SO2 (tonnes/MWh).
    pub so2: f64,
    /// PM2.5 (tonnes/MWh).
    pub pm25: f64,
}

/// System-wide emission constraints and carbon pricing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmissionPolicy {
    /// Carbon price ($/tonne CO2). Added to dispatch cost.
    pub carbon_price: f64,
    /// System-wide CO2 cap per period (tonnes). None = uncapped.
    pub co2_cap: Option<f64>,
    /// NOx cap per period (tonnes).
    pub nox_cap: Option<f64>,
    /// SO2 cap per period (tonnes).
    pub so2_cap: Option<f64>,
    /// PM2.5 cap per period (tonnes).
    pub pm25_cap: Option<f64>,
    /// CO2 allowance price ($/tonne) — cap-and-trade markets.
    pub co2_allowance_price: Option<f64>,
}

impl Default for EmissionPolicy {
    fn default() -> Self {
        Self {
            carbon_price: 0.0,
            co2_cap: None,
            nox_cap: None,
            so2_cap: None,
            pm25_cap: None,
            co2_allowance_price: None,
        }
    }
}
