// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Market rules and reserve zone types.

use serde::{Deserialize, Serialize};

/// System-wide market rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketRules {
    /// Value of Lost Load ($/MWh) — penalty for unserved energy.
    pub voll: f64,
    /// Generic reserve product definitions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserve_products: Vec<crate::market::reserve::ReserveProduct>,
    /// System-wide reserve requirements (generic).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system_reserve_requirements: Vec<crate::market::reserve::SystemReserveRequirement>,
}

impl Default for MarketRules {
    fn default() -> Self {
        Self {
            voll: 9000.0,
            reserve_products: Vec::new(),
            system_reserve_requirements: Vec::new(),
        }
    }
}

/// A reserve zone grouping buses with zonal AS requirements.
///
/// Buses reference their zone via `Bus.reserve_zone: Option<String>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReserveZone {
    /// Zone name (referenced by Bus.reserve_zone).
    pub name: String,
    /// Generic zonal reserve requirements.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zonal_requirements: Vec<crate::market::reserve::ZonalReserveRequirement>,
}
