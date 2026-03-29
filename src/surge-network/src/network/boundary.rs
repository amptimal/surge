// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! ENTSO-E boundary profile types — TSO-TSO interconnection points,
//! model authority sets, and external network equivalents (EQBD/BD).

use serde::{Deserialize, Serialize};

/// A TSO-TSO boundary point from the ENTSO-E EQBD profile.
///
/// Represents an interconnection point between two control areas (typically
/// national TSOs). The `connectivity_node_mrid` links to the CIM
/// `ConnectivityNode` at the boundary; the resolved `bus` is filled in
/// during network building when the CN can be mapped to a topological bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryPoint {
    /// CIM mRID of the BoundaryPoint object.
    pub mrid: String,
    /// mRID of the ConnectivityNode at the boundary.
    pub connectivity_node_mrid: Option<String>,
    /// ISO 3166 country code of one side (e.g., "DE").
    pub from_end_iso_code: Option<String>,
    /// ISO 3166 country code of the other side (e.g., "FR").
    pub to_end_iso_code: Option<String>,
    /// Human-readable name of the "from" end TSO.
    pub from_end_name: Option<String>,
    /// Human-readable name of the "to" end TSO.
    pub to_end_name: Option<String>,
    /// Short TSO name for the "from" end.
    pub from_end_name_tso: Option<String>,
    /// Short TSO name for the "to" end.
    pub to_end_name_tso: Option<String>,
    /// True if this is a DC interconnection (HVDC tie).
    pub is_direct_current: bool,
    /// True if this boundary point is excluded from area interchange accounting.
    pub is_excluded_from_area_interchange: bool,
    /// Resolved bus number (set when the CN maps to a topological bus).
    pub bus: Option<u32>,
}

/// Model authority set — TSO ownership of CIM equipment.
///
/// Each `ModelAuthoritySet` identifies a TSO or model authority and lists
/// the equipment mRIDs that belong to it. Used for multi-TSO model merging
/// (CGM assembly) to track which authority owns which equipment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAuthoritySet {
    /// CIM mRID of the ModelAuthoritySet.
    pub mrid: String,
    /// TSO identifier / short name.
    pub name: String,
    /// Full description of the authority.
    pub description: Option<String>,
    /// Equipment mRIDs owned by this authority (reverse-lookup from
    /// `IdentifiedObject.ModelAuthoritySet` references).
    pub members: Vec<String>,
}

/// External network equivalent — represents a reduced model of a
/// neighbouring control area or external system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquivalentNetworkData {
    /// CIM mRID.
    pub mrid: String,
    /// Name of the equivalent network.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// mRID of the `SubGeographicalRegion` / `GeographicalRegion` this
    /// equivalent represents.
    pub region_mrid: Option<String>,
}

/// External network branch equivalent — a pi-section branch representing
/// the impedance of a reduced external network.
///
/// Impedances are stored in physical ohms (as in the CIM source); downstream
/// code converts to per-unit when wiring into the admittance matrix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquivalentBranchData {
    /// CIM mRID.
    pub mrid: String,
    /// Parent `EquivalentNetwork` mRID.
    pub network_mrid: Option<String>,
    /// Positive-sequence resistance (Ohm).
    pub r_ohm: f64,
    /// Positive-sequence reactance (Ohm).
    pub x_ohm: f64,
    /// Zero-sequence resistance (Ohm), if available.
    pub r0_ohm: Option<f64>,
    /// Zero-sequence reactance (Ohm), if available.
    pub x0_ohm: Option<f64>,
    /// Negative-sequence resistance (Ohm), if available.
    pub r2_ohm: Option<f64>,
    /// Negative-sequence reactance (Ohm), if available.
    pub x2_ohm: Option<f64>,
    /// Resolved from-bus number.
    pub from_bus: Option<u32>,
    /// Resolved to-bus number.
    pub to_bus: Option<u32>,
}

/// External network shunt equivalent — a constant-admittance shunt
/// representing the reduced external network contribution at a boundary bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquivalentShuntData {
    /// CIM mRID.
    pub mrid: String,
    /// Parent `EquivalentNetwork` mRID.
    pub network_mrid: Option<String>,
    /// Conductance (Siemens).
    pub g_s: f64,
    /// Susceptance (Siemens).
    pub b_s: f64,
    /// Resolved bus number.
    pub bus: Option<u32>,
}

/// Container for all boundary and external equivalent data from the
/// ENTSO-E EQBD/BD profile.
///
/// Stored on `Network::boundary_data`.
/// Empty by default for non-CGMES cases.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoundaryData {
    /// TSO-TSO boundary points.
    pub boundary_points: Vec<BoundaryPoint>,
    /// Model authority sets (TSO ownership).
    pub model_authority_sets: Vec<ModelAuthoritySet>,
    /// External equivalent networks.
    pub equivalent_networks: Vec<EquivalentNetworkData>,
    /// External equivalent branches.
    pub equivalent_branches: Vec<EquivalentBranchData>,
    /// External equivalent shunts.
    pub equivalent_shunts: Vec<EquivalentShuntData>,
}

impl BoundaryData {
    /// Returns `true` if no boundary data has been populated.
    pub fn is_empty(&self) -> bool {
        self.boundary_points.is_empty()
            && self.model_authority_sets.is_empty()
            && self.equivalent_networks.is_empty()
            && self.equivalent_branches.is_empty()
            && self.equivalent_shunts.is_empty()
    }
}
