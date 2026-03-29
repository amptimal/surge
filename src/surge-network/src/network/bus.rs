// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Bus (node) representation in the power system network.

use serde::{Deserialize, Serialize};

use crate::market::AmbientConditions;

/// Bus type classification for power flow analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusType {
    /// PQ bus — real and reactive power specified (load bus).
    PQ = 1,
    /// PV bus — real power and voltage magnitude specified (generator bus).
    PV = 2,
    /// Slack (reference) bus — voltage magnitude and angle specified.
    Slack = 3,
    /// Isolated bus — disconnected from the network.
    Isolated = 4,
}

/// A bus (node) in the power system network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bus {
    /// Bus number (unique identifier).
    pub number: u32,
    /// Bus name.
    pub name: String,
    /// Bus type (PQ, PV, Slack, Isolated).
    pub bus_type: BusType,
    /// Shunt conductance (MW demanded at V = 1.0 p.u.).
    pub shunt_conductance_mw: f64,
    /// Shunt susceptance (MVAr injected at V = 1.0 p.u.).
    pub shunt_susceptance_mvar: f64,
    /// Area number.
    pub area: u32,
    /// Voltage magnitude in per-unit.
    pub voltage_magnitude_pu: f64,
    /// Voltage angle in radians.
    pub voltage_angle_rad: f64,
    /// Base voltage in kV.
    pub base_kv: f64,
    /// Zone number.
    pub zone: u32,
    /// Maximum voltage magnitude in per-unit.
    pub voltage_max_pu: f64,
    /// Minimum voltage magnitude in per-unit.
    pub voltage_min_pu: f64,
    /// Connected-component island ID (0 = largest island).
    /// Populated by CGMES importer; other importers default to 0.
    /// The NR/DC solvers perform their own island detection at solve time
    /// using in-service branches (which may differ from import topology).
    pub island_id: u32,
    /// Latitude in decimal degrees (WGS84). None if unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    /// Longitude in decimal degrees (WGS84). None if unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// Bus frequency (Hz). None = nominal (Network.freq_hz).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_hz: Option<f64>,
    /// Ambient conditions at this location. None = use Network.market_data.ambient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambient: Option<AmbientConditions>,
    /// Reserve zone name. References a ReserveZone on Network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_zone: Option<String>,
    /// Ownership entries (PSS/E OWNER field). Single-owner for buses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<super::owner::OwnershipEntry>,
}

impl Default for Bus {
    fn default() -> Self {
        Self {
            number: 0,
            name: String::new(),
            bus_type: BusType::PQ,
            shunt_conductance_mw: 0.0,
            shunt_susceptance_mvar: 0.0,
            base_kv: 0.0,
            voltage_magnitude_pu: 1.0,
            voltage_angle_rad: 0.0,
            area: 1,
            zone: 1,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            island_id: 0,
            latitude: None,
            longitude: None,
            freq_hz: None,
            ambient: None,
            reserve_zone: None,
            owners: Vec::new(),
        }
    }
}

impl Bus {
    pub fn new(number: u32, bus_type: BusType, base_kv: f64) -> Self {
        Self {
            number,
            bus_type,
            base_kv,
            ..Default::default()
        }
    }

    /// True if this bus is the slack (reference) bus.
    pub fn is_slack(&self) -> bool {
        self.bus_type == BusType::Slack
    }

    /// True if this bus is a PV (generator) bus.
    pub fn is_pv(&self) -> bool {
        self.bus_type == BusType::PV
    }

    /// True if this bus is a PQ (load) bus.
    pub fn is_pq(&self) -> bool {
        self.bus_type == BusType::PQ
    }
}
