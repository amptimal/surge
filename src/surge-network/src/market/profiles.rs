// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Time-series profile types — hourly load and renewable capacity factor data.
//!
//! Used by `surge-io` profile readers and the Python bindings.
//!
//! ## Stable identifiers
//!
//! All profile types use **stable external identifiers** rather than internal
//! array indices.  Generator-targeted profiles use canonical generator IDs;
//! branch-targeted profiles use `(from_bus, to_bus, circuit)`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Load profiles
// ---------------------------------------------------------------------------

/// Hourly load profile for a single bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadProfile {
    /// External bus number (matches `Bus::number`).
    pub bus: u32,
    /// MW load for each hour.
    pub load_mw: Vec<f64>,
}

/// Collection of load profiles across buses.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadProfiles {
    pub profiles: Vec<LoadProfile>,
    pub n_timesteps: usize,
}

// ---------------------------------------------------------------------------
// Renewable profiles
// ---------------------------------------------------------------------------

/// Hourly capacity factor profile for a renewable generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewableProfile {
    /// Canonical generator ID (matches `Generator::id`).
    pub generator_id: String,
    /// Capacity factor \[0, 1\] for each hour.
    pub capacity_factors: Vec<f64>,
}

/// Collection of renewable profiles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RenewableProfiles {
    pub profiles: Vec<RenewableProfile>,
    pub n_timesteps: usize,
}

// ---------------------------------------------------------------------------
// Generator derate profiles
// ---------------------------------------------------------------------------

/// Per-interval derate factor for a single generator.
///
/// A derate factor of `1.0` means full nameplate capacity is available.
/// A derate factor of `0.5` means 50% of nameplate is available (partial outage).
/// A derate factor of `0.0` means the unit is fully offline (forced outage).
///
/// Applied *before* renewable capacity factors so that
/// `pmax_effective = pmax_nameplate × derate × cf`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorDerateProfile {
    /// Canonical generator ID (matches `Generator::id`).
    pub generator_id: String,
    /// Derate factor [0, 1] for each interval.
    pub derate_factors: Vec<f64>,
}

/// Collection of generator derate profiles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneratorDerateProfiles {
    pub profiles: Vec<GeneratorDerateProfile>,
    pub n_timesteps: usize,
}

// ---------------------------------------------------------------------------
// Branch derate profiles
// ---------------------------------------------------------------------------

/// Per-interval derate factor for a single branch.
///
/// A derate factor of `1.0` leaves the thermal rating (`rate_a`) unchanged.
/// A derate factor in `(0, 1)` tightens the thermal limit proportionally
/// (e.g., seasonal ambient-adjusted ratings).
/// A derate factor of `0.0` marks the branch as out of service for that interval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchDerateProfile {
    /// From-bus number (matches `Branch::from_bus`).
    pub from_bus: u32,
    /// To-bus number (matches `Branch::to_bus`).
    pub to_bus: u32,
    /// Circuit identifier (matches `Branch::circuit`).
    pub circuit: String,
    /// Derate factor [0, 1] for each interval.
    pub derate_factors: Vec<f64>,
}

/// Collection of branch derate profiles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BranchDerateProfiles {
    pub profiles: Vec<BranchDerateProfile>,
    pub n_timesteps: usize,
}

// ---------------------------------------------------------------------------
// HVDC derate profiles
// ---------------------------------------------------------------------------

/// Per-interval derate factor for a single HVDC link.
///
/// A derate factor of `1.0` leaves the scheduled setpoint unchanged.
/// A derate factor in `(0, 1)` reduces the transfer capacity proportionally.
/// A derate factor of `0.0` takes the link fully out of service for that interval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HvdcDerateProfile {
    /// HVDC link name (matches `HvdcLink::name()`).
    pub name: String,
    /// Derate factor [0, 1] for each interval.
    pub derate_factors: Vec<f64>,
}

/// Collection of HVDC derate profiles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HvdcDerateProfiles {
    pub profiles: Vec<HvdcDerateProfile>,
    pub n_timesteps: usize,
}
