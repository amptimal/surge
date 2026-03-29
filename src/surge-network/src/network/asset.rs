// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Physical asset and wire information types from CIM IEC 61968-11 Asset package.
//!
//! These types store conductor properties, cable construction details, tower
//! geometry, transformer nameplate data, and general asset metadata parsed from
//! CGMES WireInfo / CableInfo / WireSpacingInfo / TransformerTankInfo classes.
//!
//! Future use: Carson's equation / Ametani impedance calculation from wire geometry.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Physical conductor properties from CIM WireInfo / OverheadWireInfo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireProperties {
    /// Human-readable name (from IdentifiedObject.name).
    pub name: String,
    /// AC resistance at 75 deg C (Ohm/km).
    pub r_ac75_ohm_per_km: Option<f64>,
    /// DC resistance at 20 deg C (Ohm/km).
    pub r_dc20_ohm_per_km: Option<f64>,
    /// Geometric Mean Radius (m).
    pub gmr_m: Option<f64>,
    /// Conductor radius (m).
    pub radius_m: Option<f64>,
    /// Size designation (e.g., "795 kcmil", "Drake").
    pub size_description: Option<String>,
    /// WireMaterialKind: copper, aluminum, steel, acsr, etc.
    pub material: Option<String>,
    /// Number of strands.
    pub strand_count: Option<u32>,
    /// Number of steel core strands.
    pub core_strand_count: Option<u32>,
    /// Ampacity (A).
    pub rated_current_a: Option<f64>,
}

/// Cable-specific properties from CIM CableInfo / ConcentricNeutralCableInfo / TapeShieldCableInfo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CableProperties {
    /// Base wire properties inherited from WireInfo.
    pub wire: WireProperties,
    /// Rated temperature (deg C).
    pub nominal_temperature_c: Option<f64>,
    /// Insulation material (CableConstructionKind).
    pub insulation_material: Option<String>,
    /// Insulation thickness (mm).
    pub insulation_thickness_mm: Option<f64>,
    /// Outer jacket thickness (mm).
    pub outer_jacket_thickness_mm: Option<f64>,
    /// Shield material (CableShieldMaterialKind).
    pub shield_material: Option<String>,
    /// Diameter over insulation (mm).
    pub diameter_over_insulation_mm: Option<f64>,
    /// Diameter over jacket (mm).
    pub diameter_over_jacket_mm: Option<f64>,
    /// Diameter over screen (mm).
    pub diameter_over_screen_mm: Option<f64>,
    /// Whether strand fill is used.
    pub is_strand_fill: Option<bool>,
    // -- Concentric neutral cable fields --
    /// Number of neutral strands (concentric neutral cable).
    pub neutral_strand_count: Option<u32>,
    /// GMR of neutral strand in meters (concentric neutral cable).
    pub neutral_strand_gmr_m: Option<f64>,
    /// Radius of neutral strand in meters (concentric neutral cable).
    pub neutral_strand_radius_m: Option<f64>,
    /// DC resistance of neutral at 20 deg C in Ohm/km (concentric neutral cable).
    pub neutral_strand_rdc20_ohm_per_km: Option<f64>,
    // -- Tape shield cable fields --
    /// Tape thickness in mm (tape shield cable).
    pub tape_thickness_mm: Option<f64>,
    /// Tape lap percent overlap (tape shield cable).
    pub tape_lap_percent: Option<f64>,
}

/// A single conductor position within a tower/spacing configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WirePosition {
    /// Horizontal position from tower center (m).
    pub x_m: f64,
    /// Height above ground (m).
    pub y_m: f64,
    /// Phase assignment (A, B, C, N, s1, s2).
    pub phase: Option<String>,
    /// Ordering within the spacing.
    pub sequence_number: u32,
}

/// Tower/spacing geometry for overhead or underground lines from CIM WireSpacingInfo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireSpacing {
    /// Human-readable name.
    pub name: String,
    /// Whether this is an underground cable spacing (vs overhead).
    pub is_cable: bool,
    /// Number of conductors per phase (bundle count).
    pub phase_wire_count: u32,
    /// Bundle spacing in meters.
    pub phase_wire_spacing_m: Option<f64>,
    /// Conductor positions (phase and neutral).
    pub positions: Vec<WirePosition>,
}

/// Transformer nameplate data from CIM TransformerTankInfo / PowerTransformerInfo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransformerInfoData {
    /// Human-readable name.
    pub name: String,
    /// Winding information (one per TransformerEndInfo).
    pub windings: Vec<TransformerWindingInfo>,
    /// No-load (iron) loss in watts.
    pub no_load_loss_w: Option<f64>,
    /// Exciting current as percentage of rated current.
    pub exciting_current_pct: Option<f64>,
    /// Short-circuit (copper) loss in watts.
    pub short_circuit_loss_w: Option<f64>,
    /// Leakage impedance as percentage.
    pub leakage_impedance_pct: Option<f64>,
}

/// Per-winding data from CIM TransformerEndInfo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransformerWindingInfo {
    /// Winding number (1, 2, 3).
    pub end_number: u32,
    /// Nameplate MVA.
    pub rated_s_mva: Option<f64>,
    /// Rated voltage (kV).
    pub rated_u_kv: Option<f64>,
    /// Winding resistance (Ohm).
    pub r_ohm: Option<f64>,
    /// Connection kind (Y, D, Yn, etc.).
    pub connection_kind: Option<String>,
    /// Insulation voltage (kV).
    pub insulation_u_kv: Option<f64>,
    /// Short-term emergency rating (MVA).
    pub short_term_s_mva: Option<f64>,
}

/// General asset metadata from CIM Asset / ProductAssetModel.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetMetadata {
    /// Equipment mRID this asset record is associated with.
    pub equipment_mrid: String,
    /// Serial number.
    pub serial_number: Option<String>,
    /// Manufacturer name (from ProductAssetModel).
    pub manufacturer: Option<String>,
    /// Model number.
    pub model_number: Option<String>,
    /// Date of manufacture.
    pub manufactured_date: Option<DateTime<Utc>>,
    /// Installation date.
    pub installation_date: Option<DateTime<Utc>>,
    /// Retirement date.
    pub retired_date: Option<DateTime<Utc>>,
}

/// Complete asset information container for the network.
///
/// Populated by the CGMES parser when Asset/WireInfo profile data is present.
/// Keyed by CIM mRID for cross-reference with equipment objects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetCatalog {
    /// Wire/conductor definitions keyed by CIM mRID.
    pub wire_infos: HashMap<String, WireProperties>,
    /// Cable definitions keyed by CIM mRID.
    pub cable_infos: HashMap<String, CableProperties>,
    /// Wire spacing/tower geometry keyed by CIM mRID.
    pub wire_spacings: HashMap<String, WireSpacing>,
    /// Transformer info keyed by CIM mRID.
    pub transformer_infos: HashMap<String, TransformerInfoData>,
    /// Asset metadata keyed by equipment mRID.
    pub asset_metadata: HashMap<String, AssetMetadata>,
}

impl AssetCatalog {
    /// Returns true if no asset data has been populated.
    pub fn is_empty(&self) -> bool {
        self.wire_infos.is_empty()
            && self.cable_infos.is_empty()
            && self.wire_spacings.is_empty()
            && self.transformer_infos.is_empty()
            && self.asset_metadata.is_empty()
    }
}
