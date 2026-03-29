// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Ambient environmental conditions for dynamic line rating and equipment derating.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Snapshot of environmental state for a location.
///
/// Used on Bus (per-site) and Network (system-wide fallback). Drives
/// dynamic line rating, temperature-dependent resistance, generator derating,
/// and BESS derating.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmbientConditions {
    /// Ambient air temperature (degrees C).
    pub temperature_c: f64,
    /// Wind speed at conductor height (m/s). None = calm / unknown.
    pub wind_speed_m_s: Option<f64>,
    /// Wind angle relative to conductor (degrees, 90 = perpendicular).
    pub wind_angle_deg: Option<f64>,
    /// Global horizontal solar irradiance (W/m^2).
    pub solar_irradiance_w_m2: Option<f64>,
    /// Timestamp for this snapshot.
    pub timestamp: Option<DateTime<Utc>>,
}

impl Default for AmbientConditions {
    fn default() -> Self {
        Self {
            temperature_c: 25.0,
            wind_speed_m_s: None,
            wind_angle_deg: None,
            solar_irradiance_w_m2: None,
            timestamp: None,
        }
    }
}
