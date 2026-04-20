// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Network model — the complete power system representation.

use std::collections::{HashMap, HashSet};

use tracing::debug;

use crate::market::{
    AmbientConditions, CombinedCyclePlant, DispatchableLoad, EmissionPolicy, MarketRules,
    OutageEntry, PumpedHydroUnit, ReserveZone,
};
use crate::network::asset::AssetCatalog;
use crate::network::boundary::BoundaryData;
use crate::network::breaker::BreakerRating;
use crate::network::cgmes_roundtrip::CgmesRoundtripData;
use crate::network::grounding::GroundingEntry;
use crate::network::impedance_correction::ImpedanceCorrectionTable;
use crate::network::induction_machine::InductionMachine;
use crate::network::market_data::MarketData;
use crate::network::measurement::CimMeasurement;
use crate::network::multi_section_line::MultiSectionLineGroup;
use crate::network::net_ops::NetworkOperationsData;
use crate::network::op_limits::OperationalLimits;
use crate::network::protection::ProtectionData;
use crate::network::scheduled_area_transfer::ScheduledAreaTransfer;
use crate::network::switching_device_rating::SwitchingDeviceRatingSet;
use crate::network::types::DEFAULT_BASE_MVA;
use crate::network::voltage_droop_control::VoltageDroopControl;
use crate::network::{
    AreaSchedule, Branch, Bus, BusType, FactsDevice, FixedShunt, Flowgate, Generator, HvdcModel,
    Interface, Load, NodeBreakerTopology, OltcSpec, Owner, ParSpec, PowerInjection, Region,
    SwitchedShunt, SwitchedShuntOpf, TopologyMappingState, generator::StorageValidationError,
};
use serde::{Deserialize, Serialize};

/// Structured error type returned by [`Network::validate`].
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    /// The network has no buses.
    #[error("network has no buses")]
    EmptyNetwork,

    /// `base_mva` is not positive or not finite.
    #[error("base_mva must be positive and finite, got {0}")]
    InvalidBaseMva(f64),

    /// Two or more buses share the same external bus number.
    #[error("duplicate bus number {0}")]
    DuplicateBusNumber(u32),

    /// No bus has `BusType::Slack` (every network needs at least one angle reference).
    #[error("network has no slack bus")]
    NoSlackBus,

    /// A branch references a bus number that does not exist in the bus list.
    #[error("branch ({branch_from}-{branch_to}) references missing bus {missing_bus}")]
    InvalidBranchEndpoint {
        branch_from: u32,
        branch_to: u32,
        missing_bus: u32,
    },

    /// A branch has the same bus on both ends.
    #[error("branch ({0}-{0}) is a self-loop")]
    SelfLoopBranch(u32),

    /// A generator references a bus number not present in the bus list.
    #[error("generator references missing bus {0}")]
    InvalidGeneratorBus(u32),

    /// A generator references a remote regulated bus number not present in the bus list.
    #[error("generator at bus {bus} references missing regulated bus {reg_bus}")]
    InvalidGeneratorRegulatedBus { bus: u32, reg_bus: u32 },

    /// A load references a bus number not present in the bus list.
    #[error("load references missing bus {0}")]
    InvalidLoadBus(u32),

    /// A power injection references a bus number not present in the bus list.
    #[error("power injection references missing bus {0}")]
    InvalidPowerInjectionBus(u32),

    /// A fixed shunt references a bus number not present in the bus list.
    #[error("fixed shunt references missing bus {0}")]
    InvalidFixedShuntBus(u32),

    /// A dispatchable load references a bus number not present in the bus list.
    #[error("dispatchable load references missing bus {0}")]
    InvalidDispatchableLoadBus(u32),

    /// Two generators share the same canonical ID (after whitespace trimming).
    #[error("duplicate canonical generator id `{id}`")]
    DuplicateGeneratorId { id: String },

    /// A switched shunt references a missing host bus.
    #[error("switched shunt `{id}` references missing host bus {bus}")]
    InvalidSwitchedShuntBus { id: String, bus: u32 },

    /// A switched shunt references a missing regulated bus.
    #[error("switched shunt `{id}` references missing regulated bus {bus}")]
    InvalidSwitchedShuntRegulatedBus { id: String, bus: u32 },

    /// A switched-shunt OPF relaxation references a missing host bus.
    #[error("switched shunt OPF `{id}` references missing host bus {bus}")]
    InvalidSwitchedShuntOpfBus { id: String, bus: u32 },

    /// A generator has `pmin > pmax`.
    #[error("generator at bus {bus} has pmin > pmax")]
    InvalidGeneratorLimits { bus: u32 },

    /// A generator has `qmin > qmax`.
    #[error("generator at bus {bus} has qmin > qmax")]
    InvalidGeneratorReactiveLimits { bus: u32 },

    /// A storage-capable generator has invalid `StorageParams` (e.g. negative capacity).
    #[error("generator at bus {bus} has invalid storage parameters: {source}")]
    InvalidStorageParameters {
        bus: u32,
        #[source]
        source: StorageValidationError,
    },

    /// A bus field required for solve readiness is not finite or out of range.
    #[error("bus {bus} field `{field}` is invalid: {value}")]
    InvalidBusField {
        bus: u32,
        field: &'static str,
        value: f64,
    },

    /// A load field required for solve readiness is not finite or out of range.
    #[error("load at bus {bus} field `{field}` is invalid: {value}")]
    InvalidLoadField {
        bus: u32,
        field: &'static str,
        value: f64,
    },

    /// A fixed shunt field required for solve readiness is not finite or out of range.
    #[error("fixed shunt at bus {bus} field `{field}` is invalid: {value}")]
    InvalidFixedShuntField {
        bus: u32,
        field: &'static str,
        value: f64,
    },

    /// A power injection field required for solve readiness is not finite or out of range.
    #[error("power injection at bus {bus} field `{field}` is invalid: {value}")]
    InvalidPowerInjectionField {
        bus: u32,
        field: &'static str,
        value: f64,
    },

    /// A generator field required for solve readiness is not finite or out of range.
    #[error("generator at bus {bus} field `{field}` is invalid: {value}")]
    InvalidGeneratorField {
        bus: u32,
        field: &'static str,
        value: f64,
    },

    /// A branch field required for solve readiness is not finite or out of range.
    #[error("branch ({from_bus}-{to_bus}) field `{field}` is invalid: {value}")]
    InvalidBranchField {
        from_bus: u32,
        to_bus: u32,
        field: &'static str,
        value: f64,
    },

    /// Branch angle-difference limits are finite but inverted.
    #[error(
        "branch ({from_bus}-{to_bus}) has invalid angle bounds (min={min_rad:?}, max={max_rad:?})"
    )]
    InvalidBranchAngleBounds {
        from_bus: u32,
        to_bus: u32,
        min_rad: Option<f64>,
        max_rad: Option<f64>,
    },

    /// A bus component does not have exactly one slack bus.
    #[error(
        "connected component with buses {buses:?} has slack buses {slack_buses:?}; expected exactly one slack bus"
    )]
    InvalidSlackPlacement {
        buses: Vec<u32>,
        slack_buses: Vec<u32>,
    },

    /// An isolated bus is still connected by an in-service branch.
    #[error("bus {bus} is marked isolated but still has in-service connectivity")]
    InvalidIsolatedBusConnectivity { bus: u32 },

    /// A bus has NaN or Inf in its voltage magnitude or angle initial condition.
    #[error("bus {0} has non-finite voltage initial condition (vm or va is NaN/Inf)")]
    NonFiniteBusVoltage(u32),

    /// A bus has non-finite or inverted voltage bounds (`vmin > vmax`).
    #[error(
        "bus {0} has invalid voltage bounds (vmin={1}, vmax={2}): must be finite with vmin <= vmax"
    )]
    InvalidBusVoltageBounds(u32, f64, f64),

    /// A branch has NaN or Inf in `r`, `x`, `b`, or `tap`.
    #[error("branch ({0}-{1}) has non-finite impedance parameter (r, x, b, or tap is NaN/Inf)")]
    NonFiniteBranchImpedance(u32, u32),

    /// Two or more branches share the same `(from_bus, to_bus, circuit)` key.
    #[error("duplicate branch key ({from_bus}-{to_bus} ckt {circuit})")]
    DuplicateBranchKey {
        from_bus: u32,
        to_bus: u32,
        circuit: String,
    },

    /// Node-breaker topology is present but no bus-branch mapping has been built yet.
    #[error("network has node-breaker topology but no mapped bus-branch view yet")]
    MissingTopologyMapping,

    /// Node-breaker topology mapping is stale (switches changed since last rebuild).
    #[error("network node-breaker topology is stale; call rebuild_topology() before solving")]
    StaleNodeBreakerTopology,

    /// An interface uses mismatched or invalid branch/coefficient metadata.
    #[error("interface `{name}` is invalid: {detail}")]
    InvalidInterfaceDefinition { name: String, detail: String },

    /// A flowgate uses mismatched or invalid monitored branch/coefficient metadata.
    #[error("flowgate `{name}` is invalid: {detail}")]
    InvalidFlowgateDefinition { name: String, detail: String },

    /// Two or more area schedules share the same area number.
    #[error("duplicate area schedule number {0}")]
    DuplicateAreaScheduleNumber(u32),

    /// An area schedule references a missing or invalid slack bus.
    #[error("area {area} references invalid slack bus {slack_bus}")]
    InvalidAreaScheduleSlackBus { area: u32, slack_bus: u32 },

    /// An area-schedule field required for runtime correctness is invalid.
    #[error("area {area} field `{field}` is invalid: {value}")]
    InvalidAreaScheduleField {
        area: u32,
        field: &'static str,
        value: f64,
    },

    /// Two or more point-to-point HVDC links share the same stable name.
    #[error("duplicate HVDC link name `{name}`")]
    DuplicateHvdcLinkName { name: String },

    /// A point-to-point HVDC link references a missing AC bus.
    #[error("HVDC link `{name}` references missing AC bus {bus}")]
    InvalidHvdcLinkEndpoint { name: String, bus: u32 },

    /// Two explicit DC grids share the same canonical grid id.
    #[error("duplicate explicit DC grid id {id}")]
    DuplicateDcGridId { id: u32 },

    /// Two buses inside the same explicit DC grid share the same bus id.
    #[error("explicit DC grid {grid_id} has duplicate DC bus id {bus_id}")]
    DuplicateDcBusId { grid_id: u32, bus_id: u32 },

    /// An explicit DC-grid converter references a missing AC bus.
    #[error("explicit DC grid {grid_id} converter references missing AC bus {ac_bus}")]
    InvalidDcConverterAcBus { grid_id: u32, ac_bus: u32 },

    /// An explicit DC-grid converter references a missing DC bus.
    #[error("explicit DC grid {grid_id} converter references missing DC bus {dc_bus}")]
    InvalidDcConverterDcBus { grid_id: u32, dc_bus: u32 },

    /// An explicit DC-grid branch references a missing DC bus.
    #[error(
        "explicit DC grid {grid_id} branch ({from_bus}-{to_bus}) references missing DC bus {missing_bus}"
    )]
    InvalidDcBranchEndpoint {
        grid_id: u32,
        from_bus: u32,
        to_bus: u32,
        missing_bus: u32,
    },

    /// A solve-ready network mixes the two canonical HVDC representations.
    #[error(
        "network mixes point-to-point HVDC links with explicit DC-network topology; choose one canonical HVDC representation per solve"
    )]
    MixedHvdcRepresentation,
}

fn is_valid_lower_bound(value: f64) -> bool {
    value.is_finite() || value == f64::NEG_INFINITY
}

fn is_valid_upper_bound(value: f64) -> bool {
    value.is_finite() || value == f64::INFINITY
}

/// Stable branch identity for metadata that must survive branch reordering.
///
/// Conditional ratings come from equipment-level data (for example CGMES
/// `ConditionalLimit` objects), so they must follow the physical branch
/// across topology rebuilds and vector compaction. The identity is therefore
/// keyed by the undirected terminal pair plus circuit identifier rather than
/// an ephemeral branch array index.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BranchEquipmentKey {
    pub bus_a: u32,
    pub bus_b: u32,
    pub circuit: String,
}

impl BranchEquipmentKey {
    pub fn new(from_bus: u32, to_bus: u32, circuit: impl Into<String>) -> Self {
        let circuit = circuit.into();
        if from_bus <= to_bus {
            Self {
                bus_a: from_bus,
                bus_b: to_bus,
                circuit,
            }
        } else {
            Self {
                bus_a: to_bus,
                bus_b: from_bus,
                circuit,
            }
        }
    }

    pub fn from_branch(branch: &Branch) -> Self {
        Self::new(branch.from_bus, branch.to_bus, branch.circuit.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BranchConditionalRatingEntry {
    branch: BranchEquipmentKey,
    ratings: Vec<ConditionalRating>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BranchThermalRatingSnapshot {
    branch: BranchEquipmentKey,
    rating_a_mva: f64,
    rating_c_mva: f64,
}

/// A branch-identity-indexed collection of conditional thermal ratings.
///
/// The cached base-ratings snapshot remains internal so callers cannot
/// invalidate reset behaviour by mutating a separate public side table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BranchConditionalRatings {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entries: Vec<BranchConditionalRatingEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    base_thermal_ratings: Vec<BranchThermalRatingSnapshot>,
}

impl BranchConditionalRatings {
    /// Returns `true` if no branches have conditional ratings.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of branches that have conditional ratings.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Iterate over `(branch_key, ratings)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&BranchEquipmentKey, &Vec<ConditionalRating>)> {
        self.entries
            .iter()
            .map(|entry| (&entry.branch, &entry.ratings))
    }

    /// Iterate over the rating vectors (without branch indices).
    pub fn values(&self) -> impl Iterator<Item = &Vec<ConditionalRating>> {
        self.entries.iter().map(|entry| &entry.ratings)
    }

    /// Look up conditional ratings for a specific stable branch key.
    pub fn get(&self, branch: &BranchEquipmentKey) -> Option<&[ConditionalRating]> {
        self.entries
            .iter()
            .find(|entry| entry.branch == *branch)
            .map(|entry| entry.ratings.as_slice())
    }

    /// Look up conditional ratings for a branch value.
    pub fn get_for_branch(&self, branch: &Branch) -> Option<&[ConditionalRating]> {
        self.get(&BranchEquipmentKey::from_branch(branch))
    }

    /// Insert or replace conditional ratings for a stable branch key.
    pub fn insert(&mut self, branch: BranchEquipmentKey, ratings: Vec<ConditionalRating>) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.branch == branch) {
            entry.ratings = ratings;
        } else {
            self.entries
                .push(BranchConditionalRatingEntry { branch, ratings });
        }
    }

    /// Insert or replace conditional ratings for a branch value.
    pub fn insert_for_branch(&mut self, branch: &Branch, ratings: Vec<ConditionalRating>) {
        self.insert(BranchEquipmentKey::from_branch(branch), ratings);
    }

    /// Remove all conditional ratings.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Merge another set of conditional ratings into this one.
    pub fn extend(&mut self, other: Self) {
        for entry in other.entries {
            self.insert(entry.branch, entry.ratings);
        }
        for snapshot in other.base_thermal_ratings {
            if !self
                .base_thermal_ratings
                .iter()
                .any(|existing| existing.branch == snapshot.branch)
            {
                self.base_thermal_ratings.push(snapshot);
            }
        }
    }

    fn apply_to(&mut self, branches: &mut [Branch], active_conditions: &[String]) {
        if self.entries.is_empty() {
            return;
        }
        let branch_positions: HashMap<BranchEquipmentKey, usize> = branches
            .iter()
            .enumerate()
            .map(|(idx, branch)| (BranchEquipmentKey::from_branch(branch), idx))
            .collect();
        // Snapshot base ratings for any branches we haven't seen yet.
        for entry in &self.entries {
            if let Some(&br_idx) = branch_positions.get(&entry.branch)
                && let Some(branch) = branches.get(br_idx)
                && !self
                    .base_thermal_ratings
                    .iter()
                    .any(|snapshot| snapshot.branch == entry.branch)
            {
                self.base_thermal_ratings.push(BranchThermalRatingSnapshot {
                    branch: entry.branch.clone(),
                    rating_a_mva: branch.rating_a_mva,
                    rating_c_mva: branch.rating_c_mva,
                });
            }
        }
        // Reset all to base ratings before applying conditions.
        for snapshot in &self.base_thermal_ratings {
            if let Some(&br_idx) = branch_positions.get(&snapshot.branch)
                && let Some(branch) = branches.get_mut(br_idx)
            {
                branch.rating_a_mva = snapshot.rating_a_mva;
                branch.rating_c_mva = snapshot.rating_c_mva;
            }
        }
        // Apply the most restrictive matching conditional rating.
        for entry in &self.entries {
            let Some(&br_idx) = branch_positions.get(&entry.branch) else {
                continue;
            };
            let Some(branch) = branches.get_mut(br_idx) else {
                continue;
            };
            let matching: Vec<&ConditionalRating> = entry
                .ratings
                .iter()
                .filter(|cr| active_conditions.iter().any(|c| c == &cr.condition_id))
                .collect();
            if matching.is_empty() {
                continue;
            }
            if let Some(min_a) = matching
                .iter()
                .filter(|cr| cr.rating_a_mva > 0.0)
                .map(|cr| cr.rating_a_mva)
                .reduce(f64::min)
            {
                branch.rating_a_mva = min_a;
            }
            if let Some(min_c) = matching
                .iter()
                .filter(|cr| cr.rating_c_mva > 0.0)
                .map(|cr| cr.rating_c_mva)
                .reduce(f64::min)
            {
                branch.rating_c_mva = min_c;
            }
        }
    }

    fn reset_on(&mut self, branches: &mut [Branch]) {
        let branch_positions: HashMap<BranchEquipmentKey, usize> = branches
            .iter()
            .enumerate()
            .map(|(idx, branch)| (BranchEquipmentKey::from_branch(branch), idx))
            .collect();
        for snapshot in &self.base_thermal_ratings {
            if let Some(&br_idx) = branch_positions.get(&snapshot.branch)
                && let Some(branch) = branches.get_mut(br_idx)
            {
                branch.rating_a_mva = snapshot.rating_a_mva;
                branch.rating_c_mva = snapshot.rating_c_mva;
            }
        }
        self.base_thermal_ratings.clear();
    }
}

/// Per-phase impedance entry from CGMES `PhaseImpedanceData`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseImpedanceEntry {
    /// Matrix row index (0-based phase index).
    pub row: u8,
    /// Matrix column index (0-based phase index).
    pub col: u8,
    /// Series resistance (ohm/m).
    pub r: f64,
    /// Series reactance (ohm/m).
    pub x: f64,
    /// Shunt susceptance (S/m).
    pub b: f64,
}

/// Mutual coupling between two line segments from CGMES `MutualCoupling`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutualCoupling {
    /// mRID of the first coupled line segment terminal.
    pub line1_id: String,
    /// mRID of the second coupled line segment terminal.
    pub line2_id: String,
    /// Mutual zero-sequence resistance (pu, system base).
    pub r: f64,
    /// Mutual zero-sequence reactance (pu, system base).
    pub x: f64,
}

/// A geographic coordinate point (WGS84).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GeoPoint {
    /// Longitude in decimal degrees.
    pub x: f64,
    /// Latitude in decimal degrees.
    pub y: f64,
}

/// CIM/CGMES supplementary data grouped for clarity.
///
/// Contains metadata, assets, measurements, protection, grounding, geographic
/// locations, and operational data imported from CIM/CGMES profiles. Not used
/// by power flow or OPF solvers. Serialized flat (via `#[serde(flatten)]`) so
/// JSON backward compatibility is preserved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkCimData {
    /// Per-phase impedance data from CGMES `PerLengthPhaseImpedance` + `PhaseImpedanceData`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_length_phase_impedances: HashMap<String, Vec<PhaseImpedanceEntry>>,

    /// Mutual coupling pairs from CGMES `MutualCoupling`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutual_couplings: Vec<MutualCoupling>,

    /// Neutral-point grounding impedances from CGMES `Ground`, `GroundingImpedance`,
    /// and `PetersenCoil`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grounding_impedances: Vec<GroundingEntry>,

    /// Geographic positions of network equipment (CGMES GL profile).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub geo_locations: HashMap<String, Vec<GeoPoint>>,

    /// CIM-aligned measurements from the CGMES Measurement profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measurements: Vec<CimMeasurement>,

    /// Physical asset and wire information from CGMES Asset/WireInfo profiles.
    #[serde(default, skip_serializing_if = "AssetCatalog::is_empty")]
    pub asset_catalog: AssetCatalog,

    /// IEC 61970-302 Operational Limits — full CIM-aligned limit hierarchy.
    #[serde(default, skip_serializing_if = "OperationalLimits::is_empty")]
    pub operational_limits: OperationalLimits,

    /// ENTSO-E boundary profile data (EQBD/BD).
    #[serde(default, skip_serializing_if = "BoundaryData::is_empty")]
    pub boundary_data: BoundaryData,

    /// Original CGMES source objects preserved for faithful export of lowered
    /// classes such as `EquivalentInjection`, `ExternalNetworkInjection`, and
    /// `DanglingLine`.
    #[serde(default, skip_serializing_if = "CgmesRoundtripData::is_empty")]
    pub cgmes_roundtrip: CgmesRoundtripData,

    /// IEC 61970-302 Protection equipment.
    #[serde(default, skip_serializing_if = "ProtectionData::is_empty")]
    pub protection_data: ProtectionData,

    /// IEC 62325 Energy Market Data.
    #[serde(default, skip_serializing_if = "MarketData::is_empty")]
    pub market_data: MarketData,

    /// IEC 61968 Network Operations — switching plans, outage records, crew dispatch.
    #[serde(default, skip_serializing_if = "NetworkOperationsData::is_empty")]
    pub network_operations: NetworkOperationsData,
}

/// Supplementary metadata: regions, owners, impedance corrections, and other
/// reference data imported from PSS/E, CDF, or CGMES.
///
/// Not used directly by power flow or OPF solvers. Populated by parsers and
/// preserved for round-tripping, reporting, and specialized analysis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkMetadata {
    /// Region (zone) name lookup table from PSS/E RAW "ZONE DATA".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<Region>,
    /// Owner name lookup table from PSS/E RAW "OWNER DATA".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<Owner>,
    /// Voltage droop control records from PSS/E v36 "VOLTAGE DROOP CONTROL DATA".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voltage_droop_controls: Vec<VoltageDroopControl>,
    /// Switching device rating sets from PSS/E v36 "SWITCHING DEVICE RATING SET DATA".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switching_device_rating_sets: Vec<SwitchingDeviceRatingSet>,
    /// Scheduled inter-area power transfers from PSS/E RAW "INTER-AREA TRANSFER DATA".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scheduled_area_transfers: Vec<ScheduledAreaTransfer>,
    /// Impedance correction tables from PSS/E RAW "IMPEDANCE CORRECTION DATA".
    /// Referenced by transformer `tab` field for tap-dependent R/X scaling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub impedance_corrections: Vec<ImpedanceCorrectionTable>,
    /// Multi-section line groupings from PSS/E RAW "MULTI-SECTION LINE DATA".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub multi_section_line_groups: Vec<MultiSectionLineGroup>,
}

/// Market and dispatch data: dispatchable loads, reserves, combined-cycle plants,
/// outage schedules, and system-wide policies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkMarketData {
    /// Dispatchable loads (demand-response resources).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatchable_loads: Vec<DispatchableLoad>,
    /// Pumped hydro storage units (synchronous machine overlay).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pumped_hydro_units: Vec<PumpedHydroUnit>,
    /// Combined cycle power plants with configuration-based commitment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub combined_cycle_plants: Vec<CombinedCyclePlant>,
    /// Outage / derate schedule for planning and dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outage_schedule: Vec<OutageEntry>,
    /// Reserve zones defining zonal AS requirements.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserve_zones: Vec<ReserveZone>,
    /// System-wide ambient conditions fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambient: Option<AmbientConditions>,
    /// System-wide emission constraints and carbon pricing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emission_policy: Option<EmissionPolicy>,
    /// Market rules (VOLL, AS requirements).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_rules: Option<MarketRules>,
}

/// Discrete voltage control devices: switched shunts, OLTCs, and PARs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkControlData {
    /// Discrete switched shunt banks for voltage control in the NR outer loop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switched_shunts: Vec<SwitchedShunt>,
    /// OPF-relaxed switched shunts for AC-OPF optimization.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switched_shunts_opf: Vec<SwitchedShuntOpf>,
    /// OLTC transformer control specifications.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oltc_specs: Vec<OltcSpec>,
    /// Phase Angle Regulator (PAR) specifications.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub par_specs: Vec<ParSpec>,
}

/// A complete power system network model.
///
/// Holds all electrical equipment (buses, branches, generators, loads) and
/// supplementary data (HVDC links, FACTS devices, flowgates, topology, etc.)
/// needed to run power flow, OPF, contingency analysis, and dispatch.
///
/// Typically constructed by a parser in `surge-io` (MATPOWER, PSS/E RAW,
/// CGMES, XIIDM) rather than built by hand. After parsing, call
/// [`validate()`](Self::validate) to check invariants before passing the
/// network to any solver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    /// System name.
    pub name: String,
    /// System base power in MVA.
    pub base_mva: f64,
    /// Nominal system frequency in Hz.  Defaults to 60 Hz (North America / Korea).
    /// Set to 50 Hz for Europe / most of Asia.
    #[serde(default = "Network::default_freq_hz")]
    pub freq_hz: f64,
    /// All buses in the network.
    pub buses: Vec<Bus>,
    /// All branches (lines and transformers) in the network.
    pub branches: Vec<Branch>,
    /// All generators in the network.
    pub generators: Vec<Generator>,
    /// All loads in the network.
    pub loads: Vec<Load>,
    /// Discrete voltage control devices: switched shunts, OLTCs, and PARs.
    #[serde(default)]
    pub controls: NetworkControlData,

    /// Canonical HVDC namespace: point-to-point links and explicit DC grids.
    #[serde(default, skip_serializing_if = "HvdcModel::is_empty")]
    pub hvdc: HvdcModel,

    /// Area interchange records parsed from PSS/E RAW "AREA INTERCHANGE DATA".
    ///
    /// Metadata only — does not affect the Newton-Raphson solve directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub area_schedules: Vec<AreaSchedule>,

    /// FACTS device records parsed from PSS/E RAW "FACTS DEVICE DATA".
    ///
    /// Processed by `surge_ac::facts_expansion::expand_facts()` before solving
    /// to convert them into Generator and Branch modifications.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts_devices: Vec<FactsDevice>,

    /// Supplementary metadata: regions, owners, impedance corrections, and other
    /// reference data.
    #[serde(default)]
    pub metadata: NetworkMetadata,

    /// CIM/CGMES supplementary data — metadata, assets, measurements, protection,
    /// grounding, geographic locations, and operational data.
    ///
    /// Not used by power flow or OPF solvers. Populated by the CGMES parser and
    /// preserved for round-tripping and specialized analysis tools.
    #[serde(default)]
    pub cim: NetworkCimData,

    /// Transmission interfaces — sets of branches defining flow boundaries
    /// between areas.  Enforced in DC-OPF as linear constraints on bus angles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<Interface>,

    /// Flowgates — monitored elements under specific contingencies.
    /// All in-service flowgates are enforced in DC-OPF/SCED/SCUC as linear
    /// constraints on base-case monitored-element flow.  Contingency flowgates
    /// carry pre-computed OTDF-adjusted limits in `limit_mw`.  Dynamic OTDF
    /// constraint generation belongs in SCOPF.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flowgates: Vec<Flowgate>,

    /// Operating nomograms: piecewise-linear inter-flowgate limit dependencies.
    ///
    /// Each nomogram restricts the MW limit of one flowgate based on the
    /// real-time flow measured on a second "index" flowgate.  Applied
    /// iteratively in SCED/SCUC (see `DispatchOptions::max_nomogram_iter`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nomograms: Vec<crate::network::flowgate::OperatingNomogram>,

    /// Physical node-breaker topology model.
    ///
    /// When present, the network was built from a node-breaker source (CGMES,
    /// XIIDM node-breaker).  The model retains the full physical hierarchy
    /// (substations, voltage levels, bays, connectivity nodes, switches) and
    /// the mapping from connectivity nodes to bus-branch buses.
    ///
    /// When absent, the network is purely bus-branch (MATPOWER, PSS/E, XIIDM
    /// bus-breaker).  All existing workflows are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology: Option<NodeBreakerTopology>,

    /// Induction machine (motor load) records from PSS/E RAW v35+ "INDUCTION MACHINE DATA".
    ///
    /// Stores steady-state equivalent-circuit parameters for each motor load.
    /// Empty for MATPOWER, IEEE-CDF, and PSS/E v33 cases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub induction_machines: Vec<InductionMachine>,

    /// Conditional thermal limits from CGMES `ConditionalLimit` objects.
    ///
    /// Keyed by branch index.  Each entry holds one or more condition-dependent
    /// ratings that override `rate_a`/`rate_c` when the user activates the
    /// matching condition via [`Network::apply_conditional_limits`].
    #[serde(default, skip_serializing_if = "BranchConditionalRatings::is_empty")]
    pub conditional_limits: BranchConditionalRatings,

    /// Circuit breaker ratings for fault duty comparison.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breaker_ratings: Vec<BreakerRating>,
    /// Fixed shunt equipment (preserves identity lost when baked into Bus.shunt_susceptance_mvar).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed_shunts: Vec<FixedShunt>,
    /// Explicit fixed P/Q injections that must survive topology remap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub power_injections: Vec<PowerInjection>,

    /// Market and dispatch data: dispatchable loads, reserves, combined-cycle plants,
    /// outage schedules, and system-wide policies.
    #[serde(default)]
    pub market_data: NetworkMarketData,
}

/// A condition-dependent thermal rating for a branch.
///
/// From CGMES `ConditionalLimit`: when the named condition is active,
/// the branch's `rate_a` and/or `rate_c` should be replaced with the
/// values in this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalRating {
    /// Condition identifier (CGMES mRID of the condition reference).
    pub condition_id: String,
    /// Normal (PATL) rating under this condition (MVA).  0.0 = no override.
    pub rating_a_mva: f64,
    /// Emergency (TATL) rating under this condition (MVA).  0.0 = no override.
    pub rating_c_mva: f64,
}

/// Trait for equipment types that carry a canonicalizable string ID.
pub(crate) trait HasCanonicalId {
    fn canonical_id(&self) -> &str;
    fn set_canonical_id(&mut self, id: String);
    fn bus_number(&self) -> u32;
}

impl HasCanonicalId for Generator {
    fn canonical_id(&self) -> &str {
        &self.id
    }
    fn set_canonical_id(&mut self, id: String) {
        self.id = id;
    }
    fn bus_number(&self) -> u32 {
        self.bus
    }
}

impl HasCanonicalId for Load {
    fn canonical_id(&self) -> &str {
        &self.id
    }
    fn set_canonical_id(&mut self, id: String) {
        self.id = id;
    }
    fn bus_number(&self) -> u32 {
        self.bus
    }
}

impl HasCanonicalId for FixedShunt {
    fn canonical_id(&self) -> &str {
        &self.id
    }
    fn set_canonical_id(&mut self, id: String) {
        self.id = id;
    }
    fn bus_number(&self) -> u32 {
        self.bus
    }
}

impl HasCanonicalId for SwitchedShunt {
    fn canonical_id(&self) -> &str {
        &self.id
    }
    fn set_canonical_id(&mut self, id: String) {
        self.id = id;
    }
    fn bus_number(&self) -> u32 {
        self.bus
    }
}

impl HasCanonicalId for SwitchedShuntOpf {
    fn canonical_id(&self) -> &str {
        &self.id
    }
    fn set_canonical_id(&mut self, id: String) {
        self.id = id;
    }
    fn bus_number(&self) -> u32 {
        self.bus
    }
}

/// Fill missing IDs on a slice of equipment with deterministic network-local IDs.
///
/// Existing non-empty IDs are preserved after trimming surrounding whitespace.
/// Generated IDs use the format `"{prefix}_{bus}_{ordinal}"` and are stable for
/// a fixed ordering.
fn canonicalize_ids(items: &mut [impl HasCanonicalId], prefix: &str) {
    let mut used_ids = HashSet::new();
    for item in items.iter_mut() {
        let trimmed = item.canonical_id().trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if item.canonical_id() != trimmed {
            item.set_canonical_id(trimmed.clone());
        }
        used_ids.insert(trimmed);
    }

    let mut ordinal_by_bus: HashMap<u32, usize> = HashMap::new();
    for item in items.iter_mut() {
        let bus = item.bus_number();
        let ordinal = ordinal_by_bus.entry(bus).or_insert(0);
        *ordinal += 1;

        if !item.canonical_id().trim().is_empty() {
            continue;
        }

        let base = format!("{prefix}_{bus}_{ordinal}");
        let mut candidate = base.clone();
        let mut collision = 2usize;
        while used_ids.contains(&candidate) {
            candidate = format!("{base}_{collision}");
            collision += 1;
        }

        used_ids.insert(candidate.clone());
        item.set_canonical_id(candidate);
    }
}

impl Default for Network {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_mva: DEFAULT_BASE_MVA,
            freq_hz: 60.0,
            buses: Vec::new(),
            branches: Vec::new(),
            generators: Vec::new(),
            loads: Vec::new(),
            controls: NetworkControlData::default(),
            hvdc: HvdcModel::default(),
            area_schedules: Vec::new(),
            facts_devices: Vec::new(),
            metadata: NetworkMetadata::default(),
            cim: NetworkCimData::default(),
            interfaces: Vec::new(),
            flowgates: Vec::new(),
            nomograms: Vec::new(),
            topology: None,
            induction_machines: Vec::new(),
            conditional_limits: BranchConditionalRatings::default(),
            breaker_ratings: Vec::new(),
            fixed_shunts: Vec::new(),
            power_injections: Vec::new(),
            market_data: NetworkMarketData::default(),
        }
    }
}

impl Network {
    /// Default nominal frequency: 60 Hz (North America).
    fn default_freq_hz() -> f64 {
        60.0
    }

    /// Create an empty network with the given name and default settings
    /// (`base_mva = 100`, `freq_hz = 60`).
    pub fn new(name: &str) -> Self {
        debug!(name, "creating new network");
        Self {
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// Apply conditional thermal limits for the given active conditions.
    ///
    /// For each branch that has conditional ratings matching any of the
    /// `active_conditions`, overwrites `rate_a`/`rate_c` with the most
    /// restrictive matching conditional value.  Snapshots original ratings
    /// on first call so `reset_conditional_limits` can restore them.
    pub fn apply_conditional_limits(&mut self, active_conditions: &[String]) {
        self.conditional_limits
            .apply_to(&mut self.branches, active_conditions);
    }

    /// Reset all conditional limits back to base thermal ratings.
    pub fn reset_conditional_limits(&mut self) {
        self.conditional_limits.reset_on(&mut self.branches);
    }

    /// Iterator over generators that have storage capability.
    pub fn storage_generators(&self) -> impl Iterator<Item = (usize, &Generator)> {
        self.generators
            .iter()
            .enumerate()
            .filter(|(_, g)| g.is_storage())
    }

    /// Number of buses in the network.
    pub fn n_buses(&self) -> usize {
        self.buses.len()
    }

    /// Number of branches in the network.
    pub fn n_branches(&self) -> usize {
        self.branches.len()
    }

    /// Maximum external bus number in the network, or 0 if empty.
    pub fn max_bus_number(&self) -> u32 {
        self.buses.iter().map(|b| b.number).max().unwrap_or(0)
    }

    /// Number of generators in the network (total, including out-of-service).
    pub fn n_generators(&self) -> usize {
        self.generators.len()
    }

    /// Number of in-service generators.
    pub fn n_generators_in_service(&self) -> usize {
        self.generators.iter().filter(|g| g.in_service).count()
    }

    /// Validate the structural integrity of the network graph and identity
    /// references.
    ///
    /// This checks that the model is internally connected the way the solver
    /// code expects, but does not try to prove solve-time numeric readiness.
    pub fn validate_structure(&self) -> Result<(), NetworkError> {
        // 1. Non-empty buses
        if self.buses.is_empty() {
            return Err(NetworkError::EmptyNetwork);
        }

        // 2. base_mva > 0 and finite
        if !self.base_mva.is_finite() || self.base_mva <= 0.0 {
            return Err(NetworkError::InvalidBaseMva(self.base_mva));
        }

        // 3. Build bus number set and detect duplicates
        let mut bus_numbers = std::collections::HashSet::new();
        for bus in &self.buses {
            if !bus_numbers.insert(bus.number) {
                return Err(NetworkError::DuplicateBusNumber(bus.number));
            }
        }

        // 3b. If the network retains node-breaker topology, the mapped bus-branch
        // view must be present and current before any solve-time validation can
        // trust the bus-indexed equipment arrays.
        if let Some(topology) = &self.topology {
            match topology.status() {
                TopologyMappingState::Missing => return Err(NetworkError::MissingTopologyMapping),
                TopologyMappingState::Stale => return Err(NetworkError::StaleNodeBreakerTopology),
                TopologyMappingState::Current => {}
            }
        }

        // 4. Branch endpoints reference valid buses; no self-loops.
        for branch in &self.branches {
            if !bus_numbers.contains(&branch.from_bus) {
                return Err(NetworkError::InvalidBranchEndpoint {
                    branch_from: branch.from_bus,
                    branch_to: branch.to_bus,
                    missing_bus: branch.from_bus,
                });
            }
            if !bus_numbers.contains(&branch.to_bus) {
                return Err(NetworkError::InvalidBranchEndpoint {
                    branch_from: branch.from_bus,
                    branch_to: branch.to_bus,
                    missing_bus: branch.to_bus,
                });
            }
            if branch.from_bus == branch.to_bus {
                return Err(NetworkError::SelfLoopBranch(branch.from_bus));
            }
        }

        // 5. No duplicate branch keys (from_bus, to_bus, circuit).
        let mut branch_keys = std::collections::HashSet::new();
        for branch in &self.branches {
            let key = BranchEquipmentKey::from_branch(branch);
            if !branch_keys.insert(key) {
                return Err(NetworkError::DuplicateBranchKey {
                    from_bus: branch.from_bus.min(branch.to_bus),
                    to_bus: branch.from_bus.max(branch.to_bus),
                    circuit: branch.circuit.clone(),
                });
            }
        }

        self.validate_interface_definitions()?;
        self.validate_internal_control_indices()?;
        self.validate_hvdc_structure(&bus_numbers)?;

        // 6. Validate all remaining bus-backed equipment arrays so imported
        // networks cannot silently drop demand, shunts, or DR state at solve time.
        for load in &self.loads {
            if !bus_numbers.contains(&load.bus) {
                return Err(NetworkError::InvalidLoadBus(load.bus));
            }
        }
        for injection in &self.power_injections {
            if !bus_numbers.contains(&injection.bus) {
                return Err(NetworkError::InvalidPowerInjectionBus(injection.bus));
            }
        }
        for shunt in &self.fixed_shunts {
            if !bus_numbers.contains(&shunt.bus) {
                return Err(NetworkError::InvalidFixedShuntBus(shunt.bus));
            }
        }
        for resource in &self.market_data.dispatchable_loads {
            if !bus_numbers.contains(&resource.bus) {
                return Err(NetworkError::InvalidDispatchableLoadBus(resource.bus));
            }
        }

        // 7. Generator bus references valid buses; explicit canonical IDs are
        //    unique after trimming surrounding whitespace; pmin <= pmax.
        // Missing IDs are allowed here and can be synthesized later via
        // canonicalize_generator_ids() at runtime boundaries.
        let mut generator_ids = std::collections::HashSet::new();
        for g in &self.generators {
            if !bus_numbers.contains(&g.bus) {
                return Err(NetworkError::InvalidGeneratorBus(g.bus));
            }
            if let Some(reg_bus) = g.reg_bus
                && !bus_numbers.contains(&reg_bus)
            {
                return Err(NetworkError::InvalidGeneratorRegulatedBus {
                    bus: g.bus,
                    reg_bus,
                });
            }
            let canonical_id = g.id.trim();
            if !canonical_id.is_empty() && !generator_ids.insert(canonical_id.to_string()) {
                return Err(NetworkError::DuplicateGeneratorId {
                    id: canonical_id.to_string(),
                });
            }
            if g.pmin > g.pmax {
                return Err(NetworkError::InvalidGeneratorLimits { bus: g.bus });
            }
        }

        // 8. Storage parameters on storage-capable generators.
        for g in &self.generators {
            if let Some(storage) = &g.storage {
                storage
                    .validate()
                    .map_err(|source| NetworkError::InvalidStorageParameters {
                        bus: g.bus,
                        source,
                    })?;
            }
        }

        Ok(())
    }

    /// Validate the network for solver readiness.
    ///
    /// This is the release-grade contract that callers should rely on before
    /// invoking power flow, OPF, or contingency analysis. Explicit unbounded
    /// optimization limits are permitted where the runtime already models them
    /// as open-ended bounds.
    pub fn validate_for_solve(&self) -> Result<(), NetworkError> {
        self.validate_structure()?;
        self.validate_hvdc_solve_contract()?;
        self.validate_area_schedules()?;
        self.validate_component_slack()?;
        self.validate_numerics_for_solve()?;
        Ok(())
    }

    /// Validate the network for DC solver readiness.
    ///
    /// DC studies do not consume the full AC voltage/reactive-control state, so
    /// this contract intentionally checks only the structural and numeric fields
    /// that the DC formulation actually uses.
    pub fn validate_for_dc_solve(&self) -> Result<(), NetworkError> {
        self.validate_structure()?;
        self.validate_hvdc_solve_contract()?;
        self.validate_area_schedules()?;
        self.validate_component_slack()?;
        self.validate_numerics_for_dc_solve()?;
        Ok(())
    }

    /// Validate network invariants required before any solver is invoked.
    ///
    /// This now means "solve-ready" rather than merely structurally coherent.
    pub fn validate(&self) -> Result<(), NetworkError> {
        self.validate_for_solve()
    }

    fn validate_component_slack(&self) -> Result<(), NetworkError> {
        let bus_index = self.bus_index_map();
        let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); self.buses.len()];
        let mut electrically_active = vec![false; self.buses.len()];

        for (idx, bus) in self.buses.iter().enumerate() {
            electrically_active[idx] =
                bus.shunt_conductance_mw != 0.0 || bus.shunt_susceptance_mvar != 0.0;
        }
        let mark_active_bus_number = |bus_number: u32, active: &mut [bool]| {
            if let Some(&idx) = bus_index.get(&bus_number) {
                active[idx] = true;
            }
        };
        for load in self.loads.iter().filter(|load| load.in_service) {
            mark_active_bus_number(load.bus, &mut electrically_active);
        }
        for generator in self
            .generators
            .iter()
            .filter(|generator| generator.in_service)
        {
            mark_active_bus_number(generator.bus, &mut electrically_active);
        }
        let regulating_targets: HashMap<u32, usize> = self
            .generators
            .iter()
            .filter(|generator| generator.can_voltage_regulate())
            .fold(HashMap::new(), |mut counts, generator| {
                let target_bus = generator.reg_bus.unwrap_or(generator.bus);
                *counts.entry(target_bus).or_insert(0) += 1;
                counts
            });
        for injection in self
            .power_injections
            .iter()
            .filter(|injection| injection.in_service)
        {
            mark_active_bus_number(injection.bus, &mut electrically_active);
        }
        for shunt in self.fixed_shunts.iter().filter(|shunt| shunt.in_service) {
            mark_active_bus_number(shunt.bus, &mut electrically_active);
        }
        for shunt in &self.controls.switched_shunts {
            if shunt.b_injected() != 0.0
                && let Some(&idx) = bus_index.get(&shunt.bus)
                && let Some(active) = electrically_active.get_mut(idx)
            {
                *active = true;
            }
        }
        for shunt in &self.controls.switched_shunts_opf {
            if shunt.b_init_pu != 0.0
                && let Some(&idx) = bus_index.get(&shunt.bus)
                && let Some(active) = electrically_active.get_mut(idx)
            {
                *active = true;
            }
        }

        for branch in self.branches.iter().filter(|br| br.in_service) {
            let Some(&from_idx) = bus_index.get(&branch.from_bus) else {
                continue;
            };
            let Some(&to_idx) = bus_index.get(&branch.to_bus) else {
                continue;
            };
            adjacency[from_idx].push(to_idx);
            adjacency[to_idx].push(from_idx);
        }

        let mut visited = vec![false; self.buses.len()];
        for start_idx in 0..self.buses.len() {
            if visited[start_idx] {
                continue;
            }

            if self.buses[start_idx].bus_type == BusType::Isolated {
                visited[start_idx] = true;
                let bus_number = self.buses[start_idx].number;
                if !adjacency[start_idx].is_empty() || electrically_active[start_idx] {
                    return Err(NetworkError::InvalidIsolatedBusConnectivity { bus: bus_number });
                }
                continue;
            }

            let mut stack = vec![start_idx];
            let mut component = Vec::new();
            while let Some(idx) = stack.pop() {
                if visited[idx] {
                    continue;
                }
                if self.buses[idx].bus_type == BusType::Isolated {
                    return Err(NetworkError::InvalidIsolatedBusConnectivity {
                        bus: self.buses[idx].number,
                    });
                }
                visited[idx] = true;
                component.push(idx);
                for &next in &adjacency[idx] {
                    if !visited[next] {
                        stack.push(next);
                    }
                }
            }

            if component.is_empty() {
                continue;
            }

            let buses: Vec<u32> = component
                .iter()
                .map(|&idx| self.buses[idx].number)
                .collect();
            let slack_buses: Vec<u32> = component
                .iter()
                .filter(|&&idx| self.buses[idx].bus_type == BusType::Slack)
                .map(|&idx| self.buses[idx].number)
                .collect();
            if slack_buses.len() != 1 {
                return Err(NetworkError::InvalidSlackPlacement { buses, slack_buses });
            }
            for &idx in &component {
                let bus = &self.buses[idx];
                let reg_count = regulating_targets.get(&bus.number).copied().unwrap_or(0);
                match bus.bus_type {
                    BusType::Slack if reg_count == 0 => {
                        return Err(NetworkError::InvalidSlackPlacement { buses, slack_buses });
                    }
                    BusType::PV if reg_count == 0 => {
                        return Err(NetworkError::InvalidGeneratorField {
                            bus: bus.number,
                            field: "voltage_regulated",
                            value: 0.0,
                        });
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    fn validate_area_schedules(&self) -> Result<(), NetworkError> {
        let bus_index = self.bus_index_map();
        let mut seen_areas = HashSet::new();

        for area in &self.area_schedules {
            if !seen_areas.insert(area.number) {
                return Err(NetworkError::DuplicateAreaScheduleNumber(area.number));
            }
            if area.slack_bus == 0 || !bus_index.contains_key(&area.slack_bus) {
                return Err(NetworkError::InvalidAreaScheduleSlackBus {
                    area: area.number,
                    slack_bus: area.slack_bus,
                });
            }
            if !area.p_desired_mw.is_finite() {
                return Err(NetworkError::InvalidAreaScheduleField {
                    area: area.number,
                    field: "p_desired_mw",
                    value: area.p_desired_mw,
                });
            }
            if !area.p_tolerance_mw.is_finite() || area.p_tolerance_mw < 0.0 {
                return Err(NetworkError::InvalidAreaScheduleField {
                    area: area.number,
                    field: "p_tolerance_mw",
                    value: area.p_tolerance_mw,
                });
            }
        }

        Ok(())
    }

    fn validate_numerics_for_solve(&self) -> Result<(), NetworkError> {
        for bus in &self.buses {
            if !bus.shunt_conductance_mw.is_finite() {
                return Err(NetworkError::InvalidBusField {
                    bus: bus.number,
                    field: "shunt_conductance_mw",
                    value: bus.shunt_conductance_mw,
                });
            }
            if !bus.shunt_susceptance_mvar.is_finite() {
                return Err(NetworkError::InvalidBusField {
                    bus: bus.number,
                    field: "shunt_susceptance_mvar",
                    value: bus.shunt_susceptance_mvar,
                });
            }
            if !bus.voltage_magnitude_pu.is_finite() || !bus.voltage_angle_rad.is_finite() {
                return Err(NetworkError::NonFiniteBusVoltage(bus.number));
            }
            if !bus.voltage_min_pu.is_finite()
                || !bus.voltage_max_pu.is_finite()
                || bus.voltage_min_pu > bus.voltage_max_pu
            {
                return Err(NetworkError::InvalidBusVoltageBounds(
                    bus.number,
                    bus.voltage_min_pu,
                    bus.voltage_max_pu,
                ));
            }
        }

        for load in &self.loads {
            for (field, value) in [
                ("active_power_demand_mw", load.active_power_demand_mw),
                (
                    "reactive_power_demand_mvar",
                    load.reactive_power_demand_mvar,
                ),
                ("zip_p_impedance_frac", load.zip_p_impedance_frac),
                ("zip_p_current_frac", load.zip_p_current_frac),
                ("zip_p_power_frac", load.zip_p_power_frac),
                ("zip_q_impedance_frac", load.zip_q_impedance_frac),
                ("zip_q_current_frac", load.zip_q_current_frac),
                ("zip_q_power_frac", load.zip_q_power_frac),
                (
                    "freq_sensitivity_p_pct_per_hz",
                    load.freq_sensitivity_p_pct_per_hz,
                ),
                (
                    "freq_sensitivity_q_pct_per_hz",
                    load.freq_sensitivity_q_pct_per_hz,
                ),
                ("frac_motor_a", load.frac_motor_a),
                ("frac_motor_b", load.frac_motor_b),
                ("frac_motor_c", load.frac_motor_c),
                ("frac_motor_d", load.frac_motor_d),
                ("frac_electronic", load.frac_electronic),
                ("frac_static", load.frac_static),
            ] {
                if !value.is_finite() || (field.ends_with("_frac") && !(0.0..=1.0).contains(&value))
                {
                    return Err(NetworkError::InvalidLoadField {
                        bus: load.bus,
                        field,
                        value,
                    });
                }
            }
        }

        for injection in &self.power_injections {
            for (field, value) in [
                (
                    "active_power_injection_mw",
                    injection.active_power_injection_mw,
                ),
                (
                    "reactive_power_injection_mvar",
                    injection.reactive_power_injection_mvar,
                ),
            ] {
                if !value.is_finite() {
                    return Err(NetworkError::InvalidPowerInjectionField {
                        bus: injection.bus,
                        field,
                        value,
                    });
                }
            }
        }

        for shunt in &self.fixed_shunts {
            for (field, value) in [("g_mw", shunt.g_mw), ("b_mvar", shunt.b_mvar)] {
                if !value.is_finite() {
                    return Err(NetworkError::InvalidFixedShuntField {
                        bus: shunt.bus,
                        field,
                        value,
                    });
                }
            }
            if let Some(rated_kv) = shunt.rated_kv {
                if !rated_kv.is_finite() {
                    return Err(NetworkError::InvalidFixedShuntField {
                        bus: shunt.bus,
                        field: "rated_kv",
                        value: rated_kv,
                    });
                }
            }
            if let Some(rated_mvar) = shunt.rated_mvar {
                if !rated_mvar.is_finite() {
                    return Err(NetworkError::InvalidFixedShuntField {
                        bus: shunt.bus,
                        field: "rated_mvar",
                        value: rated_mvar,
                    });
                }
            }
        }

        for g in &self.generators {
            for (field, value) in [
                ("p", g.p),
                ("q", g.q),
                ("voltage_setpoint_pu", g.voltage_setpoint_pu),
                ("machine_base_mva", g.machine_base_mva),
            ] {
                if !value.is_finite() {
                    return Err(NetworkError::InvalidGeneratorField {
                        bus: g.bus,
                        field,
                        value,
                    });
                }
            }
            if g.machine_base_mva <= 0.0 {
                return Err(NetworkError::InvalidGeneratorField {
                    bus: g.bus,
                    field: "machine_base_mva",
                    value: g.machine_base_mva,
                });
            }
            if g.voltage_setpoint_pu <= 0.0 {
                return Err(NetworkError::InvalidGeneratorField {
                    bus: g.bus,
                    field: "voltage_setpoint_pu",
                    value: g.voltage_setpoint_pu,
                });
            }
            for (field, value) in [("pmin", g.pmin), ("qmin", g.qmin)] {
                if !is_valid_lower_bound(value) {
                    return Err(NetworkError::InvalidGeneratorField {
                        bus: g.bus,
                        field,
                        value,
                    });
                }
            }
            for (field, value) in [("pmax", g.pmax), ("qmax", g.qmax)] {
                if !is_valid_upper_bound(value) {
                    return Err(NetworkError::InvalidGeneratorField {
                        bus: g.bus,
                        field,
                        value,
                    });
                }
            }
            if g.pmin > g.pmax {
                return Err(NetworkError::InvalidGeneratorLimits { bus: g.bus });
            }
            if g.qmin > g.qmax {
                return Err(NetworkError::InvalidGeneratorReactiveLimits { bus: g.bus });
            }
            if let Some(apf) = g.agc_participation_factor {
                if !apf.is_finite() || apf < 0.0 {
                    return Err(NetworkError::InvalidGeneratorField {
                        bus: g.bus,
                        field: "agc_participation_factor",
                        value: apf,
                    });
                }
            }
            if let Some(forced_outage_rate) = g.forced_outage_rate {
                if !forced_outage_rate.is_finite() || !(0.0..=1.0).contains(&forced_outage_rate) {
                    return Err(NetworkError::InvalidGeneratorField {
                        bus: g.bus,
                        field: "forced_outage_rate",
                        value: forced_outage_rate,
                    });
                }
            }
        }

        for branch in &self.branches {
            if branch.tap < 0.0 {
                return Err(NetworkError::InvalidBranchField {
                    from_bus: branch.from_bus,
                    to_bus: branch.to_bus,
                    field: "tap",
                    value: branch.tap,
                });
            }
            for (field, value) in [
                ("r", branch.r),
                ("x", branch.x),
                ("b", branch.b),
                ("g_pi", branch.g_pi),
                ("tap", branch.tap),
                ("phase_shift_rad", branch.phase_shift_rad),
                ("g_mag", branch.g_mag),
                ("b_mag", branch.b_mag),
            ] {
                if !value.is_finite() {
                    return Err(NetworkError::InvalidBranchField {
                        from_bus: branch.from_bus,
                        to_bus: branch.to_bus,
                        field,
                        value,
                    });
                }
            }
            for (field, value) in [
                ("rating_a_mva", branch.rating_a_mva),
                ("rating_b_mva", branch.rating_b_mva),
                ("rating_c_mva", branch.rating_c_mva),
            ] {
                if !is_valid_upper_bound(value) {
                    return Err(NetworkError::InvalidBranchField {
                        from_bus: branch.from_bus,
                        to_bus: branch.to_bus,
                        field,
                        value,
                    });
                }
            }
            if let (Some(min), Some(max)) = (branch.angle_diff_min_rad, branch.angle_diff_max_rad) {
                if !is_valid_lower_bound(min) || !is_valid_upper_bound(max) || min > max {
                    return Err(NetworkError::InvalidBranchAngleBounds {
                        from_bus: branch.from_bus,
                        to_bus: branch.to_bus,
                        min_rad: branch.angle_diff_min_rad,
                        max_rad: branch.angle_diff_max_rad,
                    });
                }
            }
            if let Some(min) = branch.angle_diff_min_rad {
                if !is_valid_lower_bound(min) {
                    return Err(NetworkError::InvalidBranchField {
                        from_bus: branch.from_bus,
                        to_bus: branch.to_bus,
                        field: "angle_diff_min_rad",
                        value: min,
                    });
                }
            }
            if let Some(max) = branch.angle_diff_max_rad {
                if !is_valid_upper_bound(max) {
                    return Err(NetworkError::InvalidBranchField {
                        from_bus: branch.from_bus,
                        to_bus: branch.to_bus,
                        field: "angle_diff_max_rad",
                        value: max,
                    });
                }
            }
        }

        Ok(())
    }

    fn validate_numerics_for_dc_solve(&self) -> Result<(), NetworkError> {
        for bus in &self.buses {
            for (field, value) in [
                ("shunt_conductance_mw", bus.shunt_conductance_mw),
                ("shunt_susceptance_mvar", bus.shunt_susceptance_mvar),
            ] {
                if !value.is_finite() {
                    return Err(NetworkError::InvalidBusField {
                        bus: bus.number,
                        field,
                        value,
                    });
                }
            }
        }

        for load in &self.loads {
            for (field, value) in [
                ("active_power_demand_mw", load.active_power_demand_mw),
                (
                    "reactive_power_demand_mvar",
                    load.reactive_power_demand_mvar,
                ),
            ] {
                if !value.is_finite() {
                    return Err(NetworkError::InvalidLoadField {
                        bus: load.bus,
                        field,
                        value,
                    });
                }
            }
        }

        for injection in &self.power_injections {
            for (field, value) in [
                (
                    "active_power_injection_mw",
                    injection.active_power_injection_mw,
                ),
                (
                    "reactive_power_injection_mvar",
                    injection.reactive_power_injection_mvar,
                ),
            ] {
                if !value.is_finite() {
                    return Err(NetworkError::InvalidPowerInjectionField {
                        bus: injection.bus,
                        field,
                        value,
                    });
                }
            }
        }

        for g in &self.generators {
            if !g.p.is_finite() {
                return Err(NetworkError::InvalidGeneratorField {
                    bus: g.bus,
                    field: "p",
                    value: g.p,
                });
            }
            for (field, value) in [("pmin", g.pmin)] {
                if !is_valid_lower_bound(value) {
                    return Err(NetworkError::InvalidGeneratorField {
                        bus: g.bus,
                        field,
                        value,
                    });
                }
            }
            for (field, value) in [("pmax", g.pmax)] {
                if !is_valid_upper_bound(value) {
                    return Err(NetworkError::InvalidGeneratorField {
                        bus: g.bus,
                        field,
                        value,
                    });
                }
            }
            if g.pmin > g.pmax {
                return Err(NetworkError::InvalidGeneratorLimits { bus: g.bus });
            }
            if let Some(apf) = g.agc_participation_factor {
                if !apf.is_finite() || apf < 0.0 {
                    return Err(NetworkError::InvalidGeneratorField {
                        bus: g.bus,
                        field: "agc_participation_factor",
                        value: apf,
                    });
                }
            }
        }

        for branch in &self.branches {
            if branch.tap < 0.0 {
                return Err(NetworkError::InvalidBranchField {
                    from_bus: branch.from_bus,
                    to_bus: branch.to_bus,
                    field: "tap",
                    value: branch.tap,
                });
            }
            for (field, value) in [
                ("x", branch.x),
                ("tap", branch.tap),
                ("phase_shift_rad", branch.phase_shift_rad),
            ] {
                if !value.is_finite() {
                    return Err(NetworkError::InvalidBranchField {
                        from_bus: branch.from_bus,
                        to_bus: branch.to_bus,
                        field,
                        value,
                    });
                }
            }
            for (field, value) in [
                ("rating_a_mva", branch.rating_a_mva),
                ("rating_b_mva", branch.rating_b_mva),
                ("rating_c_mva", branch.rating_c_mva),
            ] {
                if !is_valid_upper_bound(value) {
                    return Err(NetworkError::InvalidBranchField {
                        from_bus: branch.from_bus,
                        to_bus: branch.to_bus,
                        field,
                        value,
                    });
                }
            }
        }

        Ok(())
    }

    /// Fill missing canonical generator IDs with deterministic network-local IDs.
    ///
    /// Existing non-empty IDs are preserved (after trimming surrounding
    /// whitespace). Only missing IDs are synthesized, and the generated values
    /// are stable for a fixed generator ordering.
    pub fn canonicalize_branch_circuit_ids(&mut self) {
        let mut used = HashSet::new();
        let mut next_suffix_by_key: HashMap<(u32, u32, String), usize> = HashMap::new();

        for branch in &mut self.branches {
            let base_circuit = {
                let trimmed = branch.circuit.trim();
                if trimmed.is_empty() {
                    "1".to_string()
                } else {
                    trimmed.to_string()
                }
            };
            let base_key =
                BranchEquipmentKey::new(branch.from_bus, branch.to_bus, base_circuit.clone());
            let counter_key = (base_key.bus_a, base_key.bus_b, base_circuit.clone());

            if used.insert(base_key) {
                branch.circuit = base_circuit;
                next_suffix_by_key.entry(counter_key).or_insert(2);
                continue;
            }

            let next_suffix = next_suffix_by_key.entry(counter_key).or_insert(2);
            loop {
                let candidate = format!("{base_circuit}#{}", *next_suffix);
                *next_suffix += 1;
                let candidate_key =
                    BranchEquipmentKey::new(branch.from_bus, branch.to_bus, candidate.clone());
                if used.insert(candidate_key) {
                    branch.circuit = candidate;
                    break;
                }
            }
        }
    }

    pub fn canonicalize_generator_ids(&mut self) {
        canonicalize_ids(&mut self.generators, "gen");
    }

    /// Fill missing load identifiers with deterministic network-local IDs.
    ///
    /// Existing non-empty IDs are preserved after trimming surrounding
    /// whitespace. Generated IDs are stable for a fixed load ordering.
    pub fn canonicalize_load_ids(&mut self) {
        canonicalize_ids(&mut self.loads, "load");
    }

    /// Fill missing fixed-shunt identifiers with deterministic network-local IDs.
    ///
    /// Existing non-empty IDs are preserved after trimming surrounding
    /// whitespace. Generated IDs are stable for a fixed shunt ordering.
    pub fn canonicalize_shunt_ids(&mut self) {
        canonicalize_ids(&mut self.fixed_shunts, "shunt");
    }

    /// Fill missing switched-shunt identifiers with deterministic network-local IDs.
    pub fn canonicalize_switched_shunt_ids(&mut self) {
        canonicalize_ids(&mut self.controls.switched_shunts, "switched_shunt");
        canonicalize_ids(&mut self.controls.switched_shunts_opf, "switched_shunt_opf");
    }

    /// Fill missing explicit-DC converter identifiers with deterministic grid-local IDs.
    pub fn canonicalize_hvdc_converter_ids(&mut self) {
        self.hvdc.canonicalize_converter_ids();
    }

    /// Fill missing dispatchable-load identifiers with deterministic network-local IDs.
    ///
    /// Existing non-empty IDs are preserved after trimming surrounding
    /// whitespace. Generated IDs are stable for a fixed resource ordering and
    /// use the external bus number.
    pub fn canonicalize_dispatchable_load_ids(&mut self) {
        struct DlWrapper<'a> {
            inner: &'a mut DispatchableLoad,
        }
        impl HasCanonicalId for DlWrapper<'_> {
            fn canonical_id(&self) -> &str {
                &self.inner.resource_id
            }
            fn set_canonical_id(&mut self, id: String) {
                self.inner.resource_id = id;
            }
            fn bus_number(&self) -> u32 {
                self.inner.bus
            }
        }

        let mut wrappers: Vec<DlWrapper> = self
            .market_data
            .dispatchable_loads
            .iter_mut()
            .map(|dl| DlWrapper { inner: dl })
            .collect();
        canonicalize_ids(&mut wrappers, "dispatchable_load");
    }

    /// Canonicalize source-facing identities that must be unambiguous at runtime.
    ///
    /// This keeps solver and study code agnostic to source-format quirks such
    /// as reverse-direction duplicate branch records, stale PV bus tags on
    /// buses without an in-service regulating generator, or missing explicit
    /// generator / DC-converter identifiers.
    pub fn canonicalize_runtime_identities(&mut self) {
        self.canonicalize_runtime_bus_types();
        self.canonicalize_branch_circuit_ids();
        self.canonicalize_generator_ids();
        self.canonicalize_switched_shunt_ids();
        self.canonicalize_hvdc_converter_ids();
    }

    fn canonicalize_runtime_bus_types(&mut self) {
        let regulating_targets: HashSet<u32> = self
            .generators
            .iter()
            .filter(|generator| generator.can_voltage_regulate())
            .map(|generator| generator.reg_bus.unwrap_or(generator.bus))
            .collect();

        for bus in &mut self.buses {
            if bus.bus_type == BusType::PV && !regulating_targets.contains(&bus.number) {
                bus.bus_type = BusType::PQ;
            }
        }
    }

    /// Build a mapping from external bus number to internal 0-based index.
    pub fn bus_index_map(&self) -> HashMap<u32, usize> {
        self.buses
            .iter()
            .enumerate()
            .map(|(i, b)| (b.number, i))
            .collect()
    }

    /// Build a mapping from canonical generator ID to generator array index.
    ///
    /// Duplicate IDs are deduplicated: only the first occurrence is kept.
    pub fn gen_index_by_id(&self) -> HashMap<String, usize> {
        let mut map = HashMap::new();
        for (i, g) in self.generators.iter().enumerate() {
            map.entry(g.id.trim().to_string()).or_insert(i);
        }
        map
    }

    /// Find the internal array index of the generator matching a canonical ID.
    pub fn find_gen_index_by_id(&self, id: &str) -> Option<usize> {
        let canonical = id.trim();
        self.generators
            .iter()
            .position(|g| g.id.trim() == canonical)
    }

    /// Build a mapping from `(bus, machine_id)` to generator array index.
    ///
    /// This is a source-format convenience lookup, not the canonical generator
    /// identity contract. Multiple generators at the same bus with the same
    /// machine ID are deduplicated: only the first occurrence is kept.
    pub fn gen_index_map(&self) -> HashMap<(u32, Option<String>), usize> {
        let mut map = HashMap::new();
        for (i, g) in self.generators.iter().enumerate() {
            map.entry((g.bus, g.machine_id.clone())).or_insert(i);
        }
        map
    }

    /// Find the internal array index of the generator matching `(bus, machine_id)`.
    ///
    /// This is a source-format convenience lookup. When `machine_id` is `None`,
    /// the first generator (in array order) at that bus is returned regardless
    /// of its `machine_id` field.
    pub fn find_gen_index(&self, bus: u32, machine_id: Option<&str>) -> Option<usize> {
        match machine_id {
            Some(mid) => self
                .generators
                .iter()
                .position(|g| g.bus == bus && g.machine_id.as_deref().unwrap_or("1") == mid),
            None => self.generators.iter().position(|g| g.bus == bus),
        }
    }

    /// Build a mapping from `(from_bus, to_bus, circuit)` to branch array index.
    ///
    /// Parallel branches (same terminal buses, different circuit IDs) are all
    /// included.  The first occurrence wins for duplicate keys.
    pub fn branch_index_map(&self) -> HashMap<(u32, u32, String), usize> {
        let mut map = HashMap::new();
        for (i, b) in self.branches.iter().enumerate() {
            map.entry((b.from_bus, b.to_bus, b.circuit.clone()))
                .or_insert(i);
        }
        map
    }

    /// Find the internal array index of the branch matching `(from, to, circuit)`.
    ///
    /// Matches either direction, preserving the circuit identifier.
    pub fn find_branch_index(&self, from_bus: u32, to_bus: u32, circuit: &str) -> Option<usize> {
        self.branches.iter().position(|branch| {
            let same_direction = branch.from_bus == from_bus && branch.to_bus == to_bus;
            let reverse_direction = branch.from_bus == to_bus && branch.to_bus == from_bus;
            (same_direction || reverse_direction) && branch.circuit == circuit
        })
    }

    /// Build a mapping from canonical load ID to load array index.
    ///
    /// Duplicate IDs are deduplicated: only the first occurrence is kept.
    pub fn load_index_by_id(&self) -> HashMap<String, usize> {
        let mut map = HashMap::new();
        for (i, load) in self.loads.iter().enumerate() {
            map.entry(load.id.clone()).or_insert(i);
        }
        map
    }

    /// Find the internal array index of the load matching a canonical ID.
    pub fn find_load_index_by_id(&self, id: &str) -> Option<usize> {
        self.loads.iter().position(|load| load.id == id)
    }

    /// Find the internal array index of the first load matching `(bus, id)`.
    ///
    /// When `id` is `None`, returns the first load at the bus.
    pub fn find_load_index(&self, bus: u32, id: Option<&str>) -> Option<usize> {
        match id {
            Some(load_id) => self
                .loads
                .iter()
                .position(|load| load.bus == bus && load.id == load_id),
            None => self.loads.iter().position(|load| load.bus == bus),
        }
    }

    /// Build a mapping from canonical fixed-shunt ID to shunt array index.
    ///
    /// Duplicate IDs are deduplicated: only the first occurrence is kept.
    pub fn shunt_index_by_id(&self) -> HashMap<String, usize> {
        let mut map = HashMap::new();
        for (i, shunt) in self.fixed_shunts.iter().enumerate() {
            map.entry(shunt.id.clone()).or_insert(i);
        }
        map
    }

    /// Find the internal array index of the fixed shunt matching a canonical ID.
    pub fn find_shunt_index_by_id(&self, id: &str) -> Option<usize> {
        self.fixed_shunts.iter().position(|shunt| shunt.id == id)
    }

    /// Find the internal array index of the first fixed shunt matching `(bus, id)`.
    ///
    /// When `id` is `None`, returns the first shunt at the bus.
    pub fn find_shunt_index(&self, bus: u32, id: Option<&str>) -> Option<usize> {
        match id {
            Some(shunt_id) => self
                .fixed_shunts
                .iter()
                .position(|shunt| shunt.bus == bus && shunt.id == shunt_id),
            None => self.fixed_shunts.iter().position(|shunt| shunt.bus == bus),
        }
    }

    /// Find the internal array index of the HVDC link matching its stable name.
    pub fn find_hvdc_link_index_by_name(&self, name: &str) -> Option<usize> {
        self.hvdc.links.iter().position(|link| link.name() == name)
    }

    fn validate_interface_definitions(&self) -> Result<(), NetworkError> {
        let existing_branches: HashSet<_> = self
            .branches
            .iter()
            .map(crate::network::BranchRef::from)
            .collect();
        for interface in &self.interfaces {
            if interface.members.is_empty() {
                return Err(NetworkError::InvalidInterfaceDefinition {
                    name: interface.name.clone(),
                    detail: "interface has no weighted branch members".to_string(),
                });
            }
            for member in &interface.members {
                if !member.coefficient.is_finite() {
                    return Err(NetworkError::InvalidInterfaceDefinition {
                        name: interface.name.clone(),
                        detail: format!(
                            "interface member ({}, {}, {}) has non-finite coefficient {}",
                            member.branch.from_bus,
                            member.branch.to_bus,
                            member.branch.circuit,
                            member.coefficient
                        ),
                    });
                }
                if !existing_branches.contains(&member.branch) {
                    return Err(NetworkError::InvalidInterfaceDefinition {
                        name: interface.name.clone(),
                        detail: format!(
                            "interface references missing branch ({}, {}, {})",
                            member.branch.from_bus, member.branch.to_bus, member.branch.circuit
                        ),
                    });
                }
            }
        }

        for flowgate in &self.flowgates {
            if flowgate.monitored.is_empty() && flowgate.contingency_branch.is_none() {
                return Err(NetworkError::InvalidFlowgateDefinition {
                    name: flowgate.name.clone(),
                    detail: "flowgate has neither monitored members nor a contingency branch"
                        .to_string(),
                });
            }
            for member in &flowgate.monitored {
                if !member.coefficient.is_finite() {
                    return Err(NetworkError::InvalidFlowgateDefinition {
                        name: flowgate.name.clone(),
                        detail: format!(
                            "flowgate monitored branch ({}, {}, {}) has non-finite coefficient {}",
                            member.branch.from_bus,
                            member.branch.to_bus,
                            member.branch.circuit,
                            member.coefficient
                        ),
                    });
                }
                if !existing_branches.contains(&member.branch) {
                    return Err(NetworkError::InvalidFlowgateDefinition {
                        name: flowgate.name.clone(),
                        detail: format!(
                            "flowgate references missing monitored branch ({}, {}, {})",
                            member.branch.from_bus, member.branch.to_bus, member.branch.circuit
                        ),
                    });
                }
            }
            if let Some(branch) = &flowgate.contingency_branch
                && !existing_branches.contains(branch)
            {
                return Err(NetworkError::InvalidFlowgateDefinition {
                    name: flowgate.name.clone(),
                    detail: format!(
                        "flowgate references missing contingency branch ({}, {}, {})",
                        branch.from_bus, branch.to_bus, branch.circuit
                    ),
                });
            }
        }

        Ok(())
    }

    fn validate_internal_control_indices(&self) -> Result<(), NetworkError> {
        let bus_numbers: HashSet<u32> = self.buses.iter().map(|bus| bus.number).collect();

        for shunt in &self.controls.switched_shunts {
            if !bus_numbers.contains(&shunt.bus) {
                return Err(NetworkError::InvalidSwitchedShuntBus {
                    id: shunt.id.clone(),
                    bus: shunt.bus,
                });
            }
            if !bus_numbers.contains(&shunt.bus_regulated) {
                return Err(NetworkError::InvalidSwitchedShuntRegulatedBus {
                    id: shunt.id.clone(),
                    bus: shunt.bus_regulated,
                });
            }
        }

        for shunt in &self.controls.switched_shunts_opf {
            if !bus_numbers.contains(&shunt.bus) {
                return Err(NetworkError::InvalidSwitchedShuntOpfBus {
                    id: shunt.id.clone(),
                    bus: shunt.bus,
                });
            }
        }

        Ok(())
    }

    fn validate_hvdc_structure(&self, bus_numbers: &HashSet<u32>) -> Result<(), NetworkError> {
        let mut link_names = HashSet::new();
        for link in &self.hvdc.links {
            let name = link.name().trim().to_string();
            if !name.is_empty() && !link_names.insert(name.clone()) {
                return Err(NetworkError::DuplicateHvdcLinkName { name });
            }

            match link {
                crate::network::HvdcLink::Lcc(link) => {
                    for terminal in [&link.rectifier, &link.inverter] {
                        if !bus_numbers.contains(&terminal.bus) {
                            return Err(NetworkError::InvalidHvdcLinkEndpoint {
                                name: link.name.clone(),
                                bus: terminal.bus,
                            });
                        }
                    }
                }
                crate::network::HvdcLink::Vsc(link) => {
                    for terminal in [&link.converter1, &link.converter2] {
                        if !bus_numbers.contains(&terminal.bus) {
                            return Err(NetworkError::InvalidHvdcLinkEndpoint {
                                name: link.name.clone(),
                                bus: terminal.bus,
                            });
                        }
                    }
                }
            }
        }

        let mut grid_ids = HashSet::new();
        for grid in self.hvdc.dc_grids.iter().filter(|grid| !grid.is_empty()) {
            if !grid_ids.insert(grid.id) {
                return Err(NetworkError::DuplicateDcGridId { id: grid.id });
            }

            let mut dc_bus_ids = HashSet::new();
            for bus in &grid.buses {
                if !dc_bus_ids.insert(bus.bus_id) {
                    return Err(NetworkError::DuplicateDcBusId {
                        grid_id: grid.id,
                        bus_id: bus.bus_id,
                    });
                }
            }

            for converter in &grid.converters {
                if !bus_numbers.contains(&converter.ac_bus()) {
                    return Err(NetworkError::InvalidDcConverterAcBus {
                        grid_id: grid.id,
                        ac_bus: converter.ac_bus(),
                    });
                }
                if !dc_bus_ids.contains(&converter.dc_bus()) {
                    return Err(NetworkError::InvalidDcConverterDcBus {
                        grid_id: grid.id,
                        dc_bus: converter.dc_bus(),
                    });
                }
            }

            for branch in &grid.branches {
                if !dc_bus_ids.contains(&branch.from_bus) {
                    return Err(NetworkError::InvalidDcBranchEndpoint {
                        grid_id: grid.id,
                        from_bus: branch.from_bus,
                        to_bus: branch.to_bus,
                        missing_bus: branch.from_bus,
                    });
                }
                if !dc_bus_ids.contains(&branch.to_bus) {
                    return Err(NetworkError::InvalidDcBranchEndpoint {
                        grid_id: grid.id,
                        from_bus: branch.from_bus,
                        to_bus: branch.to_bus,
                        missing_bus: branch.to_bus,
                    });
                }
            }
        }

        Ok(())
    }

    fn validate_hvdc_solve_contract(&self) -> Result<(), NetworkError> {
        if self.hvdc.has_point_to_point_links() && self.hvdc.has_explicit_dc_topology() {
            return Err(NetworkError::MixedHvdcRepresentation);
        }
        Ok(())
    }

    /// Find the internal array index of the pumped-hydro unit matching its stable name.
    pub fn find_pumped_hydro_index_by_name(&self, name: &str) -> Option<usize> {
        self.market_data
            .pumped_hydro_units
            .iter()
            .position(|unit| unit.name == name)
    }

    /// Find the internal array index of the dispatchable load matching `(resource_id, bus)`.
    pub fn find_dispatchable_load_index(
        &self,
        resource_id: &str,
        bus: Option<u32>,
    ) -> Option<usize> {
        self.market_data
            .dispatchable_loads
            .iter()
            .enumerate()
            .find_map(|(index, resource)| {
                if resource.resource_id != resource_id {
                    return None;
                }
                if bus.is_none_or(|value| resource.bus == value) {
                    Some(index)
                } else {
                    None
                }
            })
    }

    /// Find the internal array index of the combined-cycle plant matching its stable name.
    pub fn find_combined_cycle_index_by_name(&self, name: &str) -> Option<usize> {
        self.market_data
            .combined_cycle_plants
            .iter()
            .position(|plant| plant.name == name)
    }

    /// Find the internal index of the first slack bus in bus-array order.
    ///
    /// All DC/PTDF/OPF solvers use this as the single angle reference — only
    /// the first slack bus is removed from the B' matrix.  AC NR excludes ALL
    /// slack buses from the Jacobian.  Use `slack_buses()` to enumerate all of
    /// them; call [`validate_for_solve`](Self::validate_for_solve) to enforce
    /// one slack bus per connected component.
    pub fn slack_bus_index(&self) -> Option<usize> {
        self.buses.iter().position(|b| b.bus_type == BusType::Slack)
    }

    /// Return references to all slack buses in the network.
    ///
    /// For systems with distributed slack or multiple slack buses, this returns
    /// all of them. For single-slack systems, the returned `Vec` has one element.
    /// Use [`slack_bus_index`](Self::slack_bus_index) when only the first (reference) bus is needed.
    pub fn slack_buses(&self) -> Vec<&Bus> {
        self.buses
            .iter()
            .filter(|b| b.bus_type == BusType::Slack)
            .collect()
    }

    /// Aggregate generator AGC participation factors by internal bus index.
    ///
    /// Returns `(internal_bus_index, total_participation_factor)` pairs for
    /// buses that have at least one in-service generator with a positive
    /// `agc_participation_factor`. Weights are **not** normalized (the caller
    /// should normalize to sum to 1.0 if needed).
    pub fn agc_participation_by_bus(&self) -> Vec<(usize, f64)> {
        let bus_map = self.bus_index_map();
        let mut by_bus = vec![0.0f64; self.n_buses()];
        for g in self.generators.iter().filter(|g| g.in_service) {
            if let Some(apf) = g.agc_participation_factor {
                if apf > 0.0 && apf.is_finite() {
                    if let Some(&idx) = bus_map.get(&g.bus) {
                        by_bus[idx] += apf;
                    }
                }
            }
        }
        by_bus
            .into_iter()
            .enumerate()
            .filter(|&(_, w)| w > 0.0)
            .collect()
    }

    /// Compute per-bus real power demand (MW) by summing in-service Load objects
    /// and subtracting in-service PowerInjection objects.
    pub fn bus_load_p_mw(&self) -> Vec<f64> {
        self.bus_load_p_mw_with_map(&self.bus_index_map())
    }

    /// Compute per-bus reactive power demand (MVAr) by summing in-service Load objects
    /// and subtracting in-service PowerInjection objects.
    pub fn bus_load_q_mvar(&self) -> Vec<f64> {
        self.bus_load_q_mvar_with_map(&self.bus_index_map())
    }

    /// Compute per-bus real power demand (MW) with a pre-built bus index map.
    pub fn bus_load_p_mw_with_map(&self, bus_map: &HashMap<u32, usize>) -> Vec<f64> {
        let mut demand = vec![0.0; self.buses.len()];
        for load in &self.loads {
            if load.in_service {
                if let Some(&idx) = bus_map.get(&load.bus) {
                    demand[idx] += load.active_power_demand_mw;
                }
            }
        }
        for injection in &self.power_injections {
            if injection.in_service {
                if let Some(&idx) = bus_map.get(&injection.bus) {
                    demand[idx] -= injection.active_power_injection_mw;
                }
            }
        }
        demand
    }

    /// Compute per-bus reactive power demand (MVAr) with a pre-built bus index map.
    pub fn bus_load_q_mvar_with_map(&self, bus_map: &HashMap<u32, usize>) -> Vec<f64> {
        let mut demand = vec![0.0; self.buses.len()];
        for load in &self.loads {
            if load.in_service {
                if let Some(&idx) = bus_map.get(&load.bus) {
                    demand[idx] += load.reactive_power_demand_mvar;
                }
            }
        }
        for injection in &self.power_injections {
            if injection.in_service {
                if let Some(&idx) = bus_map.get(&injection.bus) {
                    demand[idx] -= injection.reactive_power_injection_mvar;
                }
            }
        }
        demand
    }

    /// Compute real power injection at each bus in per-unit.
    /// P_inject\[i\] = (sum of Pg at bus i - Pd at bus i) / base_mva
    ///
    /// Demand is computed from in-service `Load` objects (and `PowerInjection`
    /// objects, which act as negative load). Generator output is summed from
    /// in-service generators.
    pub fn bus_p_injection_pu(&self) -> Vec<f64> {
        self.bus_p_injection_pu_with_map(&self.bus_index_map())
    }

    /// Compute real power injection at each bus in per-unit with a pre-built bus index map.
    pub fn bus_p_injection_pu_with_map(&self, bus_map: &HashMap<u32, usize>) -> Vec<f64> {
        let n = self.buses.len();
        let demand = self.bus_load_p_mw_with_map(bus_map);
        let mut p_inj = vec![0.0; n];

        for (i, d) in demand.iter().enumerate() {
            p_inj[i] -= d / self.base_mva;
        }

        for g in &self.generators {
            if g.in_service
                && let Some(&idx) = bus_map.get(&g.bus)
            {
                p_inj[idx] += g.p / self.base_mva;
            }
        }

        p_inj
    }

    /// Compute reactive power injection at each bus in per-unit.
    ///
    /// Demand is computed from in-service `Load` objects (and `PowerInjection`
    /// objects, which act as negative load). Generator output is summed from
    /// in-service generators.
    pub fn bus_q_injection_pu(&self) -> Vec<f64> {
        self.bus_q_injection_pu_with_map(&self.bus_index_map())
    }

    /// Compute reactive power injection at each bus in per-unit with a pre-built bus index map.
    pub fn bus_q_injection_pu_with_map(&self, bus_map: &HashMap<u32, usize>) -> Vec<f64> {
        let n = self.buses.len();
        let demand = self.bus_load_q_mvar_with_map(bus_map);
        let mut q_inj = vec![0.0; n];

        for (i, d) in demand.iter().enumerate() {
            q_inj[i] -= d / self.base_mva;
        }

        for g in &self.generators {
            if g.in_service
                && let Some(&idx) = bus_map.get(&g.bus)
            {
                q_inj[idx] += g.q / self.base_mva;
            }
        }

        q_inj
    }

    /// Total real power generation (MW).
    pub fn total_generation_mw(&self) -> f64 {
        self.generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.p)
            .sum()
    }

    /// Total real power demand (MW) from in-service Load objects.
    pub fn total_load_mw(&self) -> f64 {
        self.loads
            .iter()
            .filter(|l| l.in_service)
            .map(|l| l.active_power_demand_mw)
            .sum()
    }

    /// Rebuild bus-level fixed shunt aggregates from explicit equipment.
    ///
    /// This is the canonical path for node-breaker-backed networks where
    /// topology-sensitive state must survive bus splits and merges. Dynamic
    /// control overlays such as switched shunts or AC/DC controller injections
    /// are intentionally excluded; they are applied by solver/control loops.
    ///
    /// Load demand is not stored on Bus; it lives exclusively on Load objects
    /// and is computed at solve time via [`bus_load_p_mw`](Self::bus_load_p_mw).
    pub fn rebuild_bus_state_from_explicit_equipment(&mut self) {
        self.rebuild_bus_state_from_explicit_equipment_with_map(&self.bus_index_map());
    }

    /// Rebuild bus-level fixed shunt aggregates with a pre-built bus index map.
    pub fn rebuild_bus_state_from_explicit_equipment_with_map(
        &mut self,
        bus_map: &HashMap<u32, usize>,
    ) {
        for bus in &mut self.buses {
            bus.shunt_conductance_mw = 0.0;
            bus.shunt_susceptance_mvar = 0.0;
        }

        for shunt in &self.fixed_shunts {
            if shunt.in_service
                && let Some(&idx) = bus_map.get(&shunt.bus)
            {
                self.buses[idx].shunt_conductance_mw += shunt.g_mw;
                self.buses[idx].shunt_susceptance_mvar += shunt.b_mvar;
            }
        }
    }

    /// Scale all loads by a factor. If `area` is `Some`, only loads in that area
    /// are affected.
    ///
    /// Scales Load objects only. Real and reactive power are both multiplied by `factor`.
    pub fn scale_loads(&mut self, factor: f64, area: Option<u32>) {
        self.scale_loads_with_map(factor, area, &self.bus_index_map());
    }

    /// Scale all loads by a factor with a pre-built bus index map.
    pub fn scale_loads_with_map(
        &mut self,
        factor: f64,
        area: Option<u32>,
        bus_map: &HashMap<u32, usize>,
    ) {
        let bus_area: Vec<u32> = self.buses.iter().map(|b| b.area).collect();
        for load in &mut self.loads {
            if let Some(a) = area {
                if let Some(&idx) = bus_map.get(&load.bus) {
                    if bus_area.get(idx).copied() != Some(a) {
                        continue;
                    }
                }
            }
            load.active_power_demand_mw *= factor;
            load.reactive_power_demand_mvar *= factor;
        }
    }

    /// Scale all in-service generator real power output by a factor.
    ///
    /// Each generator's `p` is multiplied by `factor` and then clamped to
    /// `[pmin, pmax]`. If `area` is `Some`, only generators connected to buses
    /// in that area are affected.
    pub fn scale_generation(&mut self, factor: f64, area: Option<u32>) {
        self.scale_generation_with_map(factor, area, &self.bus_index_map());
    }

    /// Scale all in-service generator real power output with a pre-built bus index map.
    pub fn scale_generation_with_map(
        &mut self,
        factor: f64,
        area: Option<u32>,
        bus_map: &HashMap<u32, usize>,
    ) {
        let bus_area: Vec<u32> = self.buses.iter().map(|b| b.area).collect();
        for g in &mut self.generators {
            if !g.in_service {
                continue;
            }
            if let Some(a) = area {
                if let Some(&idx) = bus_map.get(&g.bus) {
                    if bus_area.get(idx).copied() != Some(a) {
                        continue;
                    }
                }
            }
            g.p = (g.p * factor).clamp(g.pmin, g.pmax);
        }
    }

    /// Set zero-sequence impedance for a branch identified by (from, to, circuit).
    ///
    /// Returns `true` if the branch was found and updated, `false` otherwise.
    pub fn set_branch_sequence(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        circuit: &str,
        r0: f64,
        x0: f64,
        b0: f64,
    ) -> bool {
        use crate::network::ZeroSeqData;
        for br in &mut self.branches {
            let matched = (br.from_bus == from_bus && br.to_bus == to_bus && br.circuit == circuit)
                || (br.from_bus == to_bus && br.to_bus == from_bus && br.circuit == circuit);
            if matched {
                let zs = br.zero_seq.get_or_insert_with(ZeroSeqData::default);
                zs.r0 = r0;
                zs.x0 = x0;
                zs.b0 = b0;
                return true;
            }
        }
        false
    }

    /// Get zero-sequence impedance for a branch identified by (from, to, circuit).
    ///
    /// Returns `Some((r0, x0, b0))` if the branch exists and has zero-sequence
    /// data, `None` otherwise.
    pub fn get_branch_sequence(
        &self,
        from_bus: u32,
        to_bus: u32,
        circuit: &str,
    ) -> Option<(f64, f64, f64)> {
        for br in &self.branches {
            let matched = (br.from_bus == from_bus && br.to_bus == to_bus && br.circuit == circuit)
                || (br.from_bus == to_bus && br.to_bus == from_bus && br.circuit == circuit);
            if matched {
                return br.zero_seq.as_ref().map(|zs| (zs.r0, zs.x0, zs.b0));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{
        Branch, Bus, BusType, FixedShunt, Generator, GeneratorRef, Load, StorageParams,
    };

    /// Build a minimal 3-bus network for testing:
    ///   Bus 1 (Slack, Pd=0)  -- Branch --> Bus 2 (PQ, Pd=50)
    ///   Bus 2 (PQ, Pd=50)    -- Branch --> Bus 3 (PV, Pd=30)
    ///   Generator on bus 1: Pg=90 MW, in_service
    ///   Generator on bus 3: Pg=20 MW, in_service
    fn make_3bus_network() -> Network {
        let mut net = Network::new("test-3bus");

        let bus1 = Bus::new(1, BusType::Slack, 138.0);
        let bus2 = Bus::new(2, BusType::PQ, 138.0);
        let bus3 = Bus::new(3, BusType::PV, 138.0);

        net.buses.push(bus1);
        net.buses.push(bus2);
        net.buses.push(bus3);

        // Load demand lives on Load objects, not Bus fields.
        net.loads.push(Load::new(2, 50.0, 0.0));
        net.loads.push(Load::new(3, 30.0, 0.0));

        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
        net.branches.push(Branch::new_line(2, 3, 0.02, 0.2, 0.04));

        net.generators.push(Generator::new(1, 90.0, 1.0));
        net.generators.push(Generator::new(3, 20.0, 1.0));
        net.canonicalize_generator_ids();

        net
    }

    // -----------------------------------------------------------------------
    // Count accessors
    // -----------------------------------------------------------------------

    #[test]
    fn test_n_buses() {
        let net = make_3bus_network();
        assert_eq!(net.n_buses(), 3);
    }

    #[test]
    fn test_n_branches() {
        let net = make_3bus_network();
        assert_eq!(net.n_branches(), 2);
    }

    #[test]
    fn test_n_generators() {
        let net = make_3bus_network();
        assert_eq!(net.n_generators(), 2);
    }

    #[test]
    fn test_n_generators_in_service_excludes_out_of_service() {
        let mut net = make_3bus_network();
        // Take the second generator offline
        net.generators[1].in_service = false;
        assert_eq!(net.n_generators(), 2, "n_generators returns total count");
        assert_eq!(
            net.n_generators_in_service(),
            1,
            "n_generators_in_service should only count in-service generators"
        );
    }

    // -----------------------------------------------------------------------
    // bus_index_map
    // -----------------------------------------------------------------------

    #[test]
    fn test_bus_index_map_correctness() {
        let net = make_3bus_network();
        let map = net.bus_index_map();
        assert_eq!(map.len(), 3);
        assert_eq!(map[&1], 0);
        assert_eq!(map[&2], 1);
        assert_eq!(map[&3], 2);
    }

    #[test]
    fn test_bus_index_map_non_contiguous_bus_numbers() {
        let mut net = Network::new("non-contiguous");
        net.buses.push(Bus::new(10, BusType::Slack, 138.0));
        net.buses.push(Bus::new(50, BusType::PQ, 138.0));
        net.buses.push(Bus::new(99, BusType::PQ, 138.0));

        let map = net.bus_index_map();
        assert_eq!(map.len(), 3);
        assert_eq!(map[&10], 0);
        assert_eq!(map[&50], 1);
        assert_eq!(map[&99], 2);
    }

    // -----------------------------------------------------------------------
    // slack_bus_index
    // -----------------------------------------------------------------------

    #[test]
    fn test_slack_bus_index() {
        let net = make_3bus_network();
        assert_eq!(
            net.slack_bus_index(),
            Some(0),
            "Bus 1 is Slack and at index 0"
        );
    }

    #[test]
    fn test_slack_bus_index_no_slack() {
        let mut net = Network::new("no-slack");
        net.buses.push(Bus::new(1, BusType::PQ, 138.0));
        net.buses.push(Bus::new(2, BusType::PV, 138.0));
        assert_eq!(
            net.slack_bus_index(),
            None,
            "No slack bus should return None"
        );
    }

    #[test]
    fn test_slack_bus_index_slack_not_first() {
        let mut net = Network::new("slack-last");
        net.buses.push(Bus::new(1, BusType::PQ, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses.push(Bus::new(3, BusType::Slack, 138.0));
        assert_eq!(
            net.slack_bus_index(),
            Some(2),
            "Slack bus at the end should return index 2"
        );
    }

    // -----------------------------------------------------------------------
    // total_generation_mw / total_load_mw
    // -----------------------------------------------------------------------

    #[test]
    fn test_total_generation_mw() {
        let net = make_3bus_network();
        // Gen 1: 90 MW + Gen 2: 20 MW = 110 MW
        assert!(
            (net.total_generation_mw() - 110.0).abs() < 1e-10,
            "total generation should be 110 MW; got {}",
            net.total_generation_mw()
        );
    }

    #[test]
    fn test_total_generation_mw_excludes_offline() {
        let mut net = make_3bus_network();
        net.generators[0].in_service = false;
        assert!(
            (net.total_generation_mw() - 20.0).abs() < 1e-10,
            "offline gen should not count; got {}",
            net.total_generation_mw()
        );
    }

    #[test]
    fn test_total_load_mw() {
        let net = make_3bus_network();
        // Bus 1: Pd=0, Bus 2: Pd=50, Bus 3: Pd=30 => total = 80 MW
        assert!(
            (net.total_load_mw() - 80.0).abs() < 1e-10,
            "total load should be 80 MW; got {}",
            net.total_load_mw()
        );
    }

    // -----------------------------------------------------------------------
    // Empty network edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_network() {
        let net = Network::new("empty");
        assert_eq!(net.n_buses(), 0);
        assert_eq!(net.n_branches(), 0);
        assert_eq!(net.n_generators(), 0);
        assert_eq!(net.slack_bus_index(), None);
        assert!((net.total_generation_mw()).abs() < 1e-10);
        assert!((net.total_load_mw()).abs() < 1e-10);
        assert!(net.bus_index_map().is_empty());
    }

    #[test]
    fn test_empty_network_bus_p_injection() {
        let net = Network::new("empty");
        let p_inj = net.bus_p_injection_pu();
        assert!(
            p_inj.is_empty(),
            "empty network should have empty injection vector"
        );
    }

    #[test]
    fn test_validate_rejects_invalid_storage_parameters() {
        let mut net = make_3bus_network();
        net.generators[0].storage = Some(StorageParams {
            charge_efficiency: 1.2,
            ..StorageParams::with_energy_capacity_mwh(50.0)
        });

        let err = net.validate().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidStorageParameters { bus: 1, .. }
        ));
    }

    #[test]
    fn test_validate_rejects_load_on_missing_bus() {
        let mut net = make_3bus_network();
        net.loads.push(Load::new(99, 10.0, 2.0));

        let err = net.validate().unwrap_err();
        assert!(matches!(err, NetworkError::InvalidLoadBus(99)));
    }

    #[test]
    fn test_validate_rejects_power_injection_on_missing_bus() {
        let mut net = make_3bus_network();
        net.power_injections.push(PowerInjection {
            bus: 99,
            id: "inj_missing".into(),
            kind: crate::network::power_injection::PowerInjectionKind::Other,
            active_power_injection_mw: 5.0,
            reactive_power_injection_mvar: 1.0,
            in_service: true,
        });

        let err = net.validate().unwrap_err();
        assert!(matches!(err, NetworkError::InvalidPowerInjectionBus(99)));
    }

    #[test]
    fn test_validate_rejects_fixed_shunt_on_missing_bus() {
        let mut net = make_3bus_network();
        net.fixed_shunts.push(FixedShunt {
            bus: 99,
            id: "sh_missing".into(),
            shunt_type: crate::network::ShuntType::Capacitor,
            g_mw: 0.0,
            b_mvar: 1.0,
            in_service: true,
            rated_kv: None,
            rated_mvar: None,
        });

        let err = net.validate().unwrap_err();
        assert!(matches!(err, NetworkError::InvalidFixedShuntBus(99)));
    }

    #[test]
    fn test_validate_rejects_dispatchable_load_on_missing_bus() {
        let mut net = make_3bus_network();
        net.market_data
            .dispatchable_loads
            .push(DispatchableLoad::curtailable(
                99,
                10.0,
                2.0,
                0.0,
                100.0,
                net.base_mva,
            ));

        let err = net.validate().unwrap_err();
        assert!(matches!(err, NetworkError::InvalidDispatchableLoadBus(99)));
    }

    // -----------------------------------------------------------------------
    // bus_p_injection_pu
    // -----------------------------------------------------------------------

    #[test]
    fn test_bus_p_injection_pu() {
        let net = make_3bus_network();
        let p_inj = net.bus_p_injection_pu();
        // Bus 1 (idx 0): Gen=90, Pd=0  =>  90/100 = 0.9
        // Bus 2 (idx 1): Gen=0,  Pd=50 => -50/100 = -0.5
        // Bus 3 (idx 2): Gen=20, Pd=30 => (20-30)/100 = -0.1
        assert_eq!(p_inj.len(), 3);
        assert!(
            (p_inj[0] - 0.9).abs() < 1e-10,
            "bus 1 p_inj: expected 0.9, got {}",
            p_inj[0]
        );
        assert!(
            (p_inj[1] - (-0.5)).abs() < 1e-10,
            "bus 2 p_inj: expected -0.5, got {}",
            p_inj[1]
        );
        assert!(
            (p_inj[2] - (-0.1)).abs() < 1e-10,
            "bus 3 p_inj: expected -0.1, got {}",
            p_inj[2]
        );
    }

    // -----------------------------------------------------------------------
    // Network::new defaults
    // -----------------------------------------------------------------------

    #[test]
    fn test_network_new_defaults() {
        let net = Network::new("test-defaults");
        assert_eq!(net.name, "test-defaults");
        assert!(
            (net.base_mva - 100.0).abs() < 1e-10,
            "default base_mva should be 100"
        );
        assert!(net.buses.is_empty());
        assert!(net.branches.is_empty());
        assert!(net.generators.is_empty());
        assert!(net.loads.is_empty());
    }

    // -----------------------------------------------------------------------
    // scale_loads / scale_generation
    // -----------------------------------------------------------------------

    #[test]
    fn test_scale_loads_all() {
        let mut net = make_3bus_network();
        // Add more explicit Load records
        net.loads.push(Load::new(2, 10.0, 5.0));
        net.loads.push(Load::new(3, 20.0, 10.0));

        let old_total: f64 = net
            .loads
            .iter()
            .map(|l| l.active_power_demand_mw)
            .sum::<f64>();

        net.scale_loads(1.5, None);

        let new_total: f64 = net
            .loads
            .iter()
            .map(|l| l.active_power_demand_mw)
            .sum::<f64>();

        assert!(
            (new_total - old_total * 1.5).abs() < 1e-10,
            "total Pd should scale by 1.5: old={} new={} expected={}",
            old_total,
            new_total,
            old_total * 1.5
        );
    }

    #[test]
    fn test_scale_generation_clamped() {
        let mut net = make_3bus_network();
        // Gen 0: Pg=90, Pmax=100 (default from Generator::new)
        // Gen 1: Pg=20, Pmax=100

        // Scale by 10x — should clamp to Pmax
        net.scale_generation(10.0, None);

        for g in &net.generators {
            assert!(
                g.p <= g.pmax,
                "Pg={} should be <= Pmax={} after 10x scaling",
                g.p,
                g.pmax
            );
        }
    }

    // -----------------------------------------------------------------------
    // set/get_branch_sequence
    // -----------------------------------------------------------------------

    #[test]
    fn test_set_branch_sequence_roundtrip() {
        let mut net = make_3bus_network();
        // Branch 0: from_bus=1, to_bus=2, circuit=1
        let (from, to, ckt) = {
            let br = &net.branches[0];
            (br.from_bus, br.to_bus, br.circuit.clone())
        };
        assert!(net.get_branch_sequence(from, to, &ckt).is_none());

        assert!(net.set_branch_sequence(from, to, &ckt, 0.15, 0.45, 0.02));
        let (r0, x0, b0) = net.get_branch_sequence(from, to, &ckt).unwrap();
        assert!((r0 - 0.15).abs() < 1e-10);
        assert!((x0 - 0.45).abs() < 1e-10);
        assert!((b0 - 0.02).abs() < 1e-10);

        // Reversed direction should also work
        let (r0r, x0r, b0r) = net.get_branch_sequence(to, from, &ckt).unwrap();
        assert!((r0r - 0.15).abs() < 1e-10);
        assert!((x0r - 0.45).abs() < 1e-10);
        assert!((b0r - 0.02).abs() < 1e-10);
    }

    #[test]
    fn test_set_branch_sequence_not_found() {
        let mut net = make_3bus_network();
        assert!(!net.set_branch_sequence(99, 100, "1", 0.1, 0.2, 0.0));
    }

    // -----------------------------------------------------------------------
    // Negative tap ratio solve-readiness check.
    // -----------------------------------------------------------------------

    #[test]
    fn test_negative_tap_ratio_rejected_by_validate_for_solve() {
        let mut net = make_3bus_network();
        net.branches[0].tap = -0.95;
        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidBranchField { field: "tap", .. }
        ));
    }

    #[test]
    fn validate_for_solve_rejects_missing_slack_in_island() {
        let mut net = Network::new("component-slack");
        net.buses = vec![
            Bus::new(1, BusType::Slack, 138.0),
            Bus::new(2, BusType::PQ, 138.0),
            Bus::new(3, BusType::PQ, 138.0),
        ];
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
        net.generators.push(Generator::new(1, 50.0, 1.0));
        net.canonicalize_generator_ids();

        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidSlackPlacement { buses, slack_buses }
            if buses.contains(&3) && slack_buses.is_empty()
        ));
    }

    #[test]
    fn validate_for_solve_rejects_isolated_bus_with_active_equipment() {
        let mut net = Network::new("isolated-bus");
        net.buses = vec![Bus::new(1, BusType::Isolated, 138.0)];
        net.loads.push(Load::new(1, 5.0, 1.0));

        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidIsolatedBusConnectivity { bus: 1 }
        ));
    }

    #[test]
    fn validate_for_solve_rejects_isolated_bus_with_bus_shunt() {
        let mut net = Network::new("isolated-bus-shunt");
        let mut bus = Bus::new(1, BusType::Isolated, 138.0);
        bus.shunt_susceptance_mvar = 25.0;
        net.buses = vec![bus];

        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidIsolatedBusConnectivity { bus: 1 }
        ));
    }

    #[test]
    fn validate_for_solve_rejects_duplicate_area_schedule_numbers() {
        let mut net = make_3bus_network();
        net.area_schedules.push(AreaSchedule {
            number: 1,
            slack_bus: 1,
            p_desired_mw: 10.0,
            p_tolerance_mw: 5.0,
            name: "A".to_string(),
        });
        net.area_schedules.push(AreaSchedule {
            number: 1,
            slack_bus: 1,
            p_desired_mw: 20.0,
            p_tolerance_mw: 5.0,
            name: "B".to_string(),
        });

        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(err, NetworkError::DuplicateAreaScheduleNumber(1)));
    }

    #[test]
    fn validate_for_solve_rejects_invalid_area_schedule_slack_bus() {
        let mut net = make_3bus_network();
        net.area_schedules.push(AreaSchedule {
            number: 7,
            slack_bus: 99,
            p_desired_mw: 10.0,
            p_tolerance_mw: 5.0,
            name: "bad".to_string(),
        });

        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidAreaScheduleSlackBus {
                area: 7,
                slack_bus: 99
            }
        ));
    }

    #[test]
    fn validate_for_solve_allows_explicitly_unbounded_generator_limits() {
        let mut net = make_3bus_network();
        net.generators[0].qmin = f64::NEG_INFINITY;
        net.generators[0].qmax = f64::INFINITY;
        net.generators[0].pmax = f64::INFINITY;

        net.validate_for_solve()
            .expect("unbounded OPF-style generator limits should be allowed");
    }

    #[test]
    fn validate_for_solve_allows_explicitly_unbounded_branch_limits() {
        let mut net = make_3bus_network();
        net.branches[0].rating_a_mva = f64::INFINITY;
        net.branches[0].angle_diff_min_rad = Some(f64::NEG_INFINITY);
        net.branches[0].angle_diff_max_rad = Some(f64::INFINITY);

        net.validate_for_solve()
            .expect("unbounded thermal and angle limits should be allowed");
    }

    #[test]
    fn validate_for_solve_rejects_wrong_sided_infinite_limits() {
        let mut net = make_3bus_network();
        net.generators[0].qmax = f64::NEG_INFINITY;
        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidGeneratorField {
                field: "qmax",
                value,
                ..
            } if value == f64::NEG_INFINITY
        ));
    }

    #[test]
    fn validate_for_solve_allows_raw_agc_weights_above_one() {
        let mut net = make_3bus_network();
        net.generators[0].agc_participation_factor = Some(2.0);

        net.validate_for_solve()
            .expect("AGC participation factors are raw weights and may exceed 1.0");
    }

    #[test]
    fn validate_accepts_network_after_generator_ids_are_canonicalized() {
        let mut net = make_3bus_network();
        net.generators[0].id.clear();

        net.canonicalize_generator_ids();
        net.validate()
            .expect("validate should succeed after canonicalization");
        assert!(
            !net.generators[0].id.is_empty(),
            "canonicalization should have auto-assigned a canonical id"
        );
        assert!(
            net.generators[0].id.starts_with("gen_"),
            "canonical id should follow gen_{{bus}}_{{ordinal}} format, got: {}",
            net.generators[0].id
        );
    }

    #[test]
    fn canonicalize_runtime_identities_demotes_pv_bus_without_active_regulator() {
        let mut net = Network::new("runtime-bus-type-normalization");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PV, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));

        let mut slack_gen = Generator::new(1, 50.0, 1.0);
        slack_gen.id = "g1".into();
        net.generators.push(slack_gen);

        let mut offline_pv_gen = Generator::new(2, 10.0, 1.02);
        offline_pv_gen.id = "g2".into();
        offline_pv_gen.in_service = false;
        net.generators.push(offline_pv_gen);

        net.canonicalize_runtime_identities();

        assert_eq!(net.buses[1].bus_type, BusType::PQ);
        net.validate()
            .expect("runtime canonicalization should produce a solve-ready network");
    }

    #[test]
    fn validate_rejects_duplicate_generator_id() {
        let mut net = make_3bus_network();
        let duplicate_id = net.generators[0].id.clone();
        net.generators[1].id = duplicate_id.clone();

        let err = net.validate().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::DuplicateGeneratorId { id } if id == duplicate_id
        ));
    }

    #[test]
    fn canonicalize_generator_ids_fills_missing_ids_deterministically() {
        let mut net = Network::new("canonicalize-generator-ids");
        net.generators
            .push(Generator::with_id(" explicit-a ", 10, 0.0, 1.0));
        net.generators.push(Generator::new(10, 0.0, 1.0));
        net.generators.push(Generator::new(10, 0.0, 1.0));
        net.generators
            .push(Generator::with_id("gen_10_2", 10, 0.0, 1.0));
        net.generators.push(Generator::new(20, 0.0, 1.0));

        let mut clone = net.clone();
        net.canonicalize_generator_ids();
        clone.canonicalize_generator_ids();

        let ids: Vec<&str> = net
            .generators
            .iter()
            .map(|generator| generator.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "explicit-a",
                "gen_10_2_2",
                "gen_10_3",
                "gen_10_2",
                "gen_20_1"
            ]
        );
        assert_eq!(
            ids,
            clone
                .generators
                .iter()
                .map(|generator| generator.id.as_str())
                .collect::<Vec<_>>()
        );
        let unique_ids: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique_ids.len(), ids.len());
    }

    #[test]
    fn canonicalize_branch_circuit_ids_disambiguates_reverse_duplicates() {
        let mut net = Network::new("canonicalize-branch-circuits");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(2, 1, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));

        net.canonicalize_branch_circuit_ids();

        let circuits: Vec<&str> = net
            .branches
            .iter()
            .map(|branch| branch.circuit.as_str())
            .collect();
        assert_eq!(circuits, vec!["1", "1#2", "1#3"]);
        net.validate_structure()
            .expect("canonicalized branch circuits should be structurally valid");
    }

    #[test]
    fn canonicalize_load_and_shunt_ids_fills_missing_ids_deterministically() {
        let mut net = Network::new("canonicalize-load-shunt-ids");
        net.loads.push(Load {
            bus: 3,
            id: String::new(),
            ..Default::default()
        });
        net.loads.push(Load {
            bus: 3,
            id: " load_existing ".into(),
            ..Default::default()
        });
        net.fixed_shunts.push(FixedShunt {
            bus: 7,
            id: String::new(),
            shunt_type: crate::network::ShuntType::Capacitor,
            g_mw: 0.0,
            b_mvar: 0.0,
            in_service: true,
            rated_kv: None,
            rated_mvar: None,
        });
        net.fixed_shunts.push(FixedShunt {
            bus: 7,
            id: " sh_existing ".into(),
            shunt_type: crate::network::ShuntType::Reactor,
            g_mw: 0.0,
            b_mvar: 0.0,
            in_service: true,
            rated_kv: None,
            rated_mvar: None,
        });

        net.canonicalize_load_ids();
        net.canonicalize_shunt_ids();

        assert_eq!(net.loads[0].id, "load_3_1");
        assert_eq!(net.loads[1].id, "load_existing");
        assert_eq!(net.fixed_shunts[0].id, "shunt_7_1");
        assert_eq!(net.fixed_shunts[1].id, "sh_existing");
    }

    #[test]
    fn canonicalize_dispatchable_load_ids_fills_missing_ids_deterministically() {
        let mut net = make_3bus_network();
        net.market_data
            .dispatchable_loads
            .push(DispatchableLoad::curtailable(
                2, 150.0, 30.0, 50.0, 40.0, 100.0,
            ));
        net.market_data
            .dispatchable_loads
            .push(DispatchableLoad::curtailable(
                2, 100.0, 20.0, 0.0, 50.0, 100.0,
            ));
        net.market_data.dispatchable_loads[1].resource_id = " dr_existing ".into();

        net.canonicalize_dispatchable_load_ids();

        assert_eq!(
            net.market_data.dispatchable_loads[0].resource_id,
            "dispatchable_load_2_1"
        );
        assert_eq!(
            net.market_data.dispatchable_loads[1].resource_id,
            "dr_existing"
        );
    }

    #[test]
    fn find_stable_asset_indices_by_identity() {
        let mut net = make_3bus_network();
        net.loads.push(Load {
            bus: 2,
            id: "load_2_a".into(),
            ..Default::default()
        });
        net.fixed_shunts.push(FixedShunt {
            bus: 2,
            id: "shunt_2_a".into(),
            shunt_type: crate::network::ShuntType::Capacitor,
            g_mw: 1.0,
            b_mvar: 2.0,
            in_service: true,
            rated_kv: None,
            rated_mvar: None,
        });
        net.hvdc
            .links
            .push(crate::network::HvdcLink::Vsc(crate::network::VscHvdcLink {
                name: "HVDC_A".into(),
                converter1: crate::network::VscConverterTerminal {
                    bus: 1,
                    ..Default::default()
                },
                converter2: crate::network::VscConverterTerminal {
                    bus: 3,
                    ..Default::default()
                },
                ..Default::default()
            }));
        net.market_data
            .pumped_hydro_units
            .push(PumpedHydroUnit::new(
                "PH_A".into(),
                GeneratorRef {
                    bus: 1,
                    id: "GEN_A".into(),
                },
                100.0,
            ));
        net.market_data
            .dispatchable_loads
            .push(DispatchableLoad::curtailable(
                2, 20.0, 5.0, 0.0, 100.0, 100.0,
            ));
        net.market_data.dispatchable_loads[0].resource_id = "dr_a".into();
        net.market_data
            .combined_cycle_plants
            .push(CombinedCyclePlant {
                id: String::new(),
                name: "CC_A".into(),
                configs: Vec::new(),
                transitions: Vec::new(),
                active_config: None,
                hours_in_config: 0.0,
                duct_firing_capable: false,
            });

        assert_eq!(net.find_branch_index(1, 2, "1"), Some(0));
        assert_eq!(net.find_branch_index(2, 1, "1"), Some(0));
        assert_eq!(net.find_load_index_by_id("load_2_a"), Some(2));
        assert_eq!(net.find_load_index(2, Some("load_2_a")), Some(2));
        assert_eq!(net.find_shunt_index_by_id("shunt_2_a"), Some(0));
        assert_eq!(net.find_shunt_index(2, Some("shunt_2_a")), Some(0));
        assert_eq!(net.find_hvdc_link_index_by_name("HVDC_A"), Some(0));
        assert_eq!(net.find_pumped_hydro_index_by_name("PH_A"), Some(0));
        assert_eq!(net.find_dispatchable_load_index("dr_a", Some(2)), Some(0));
        assert_eq!(net.find_combined_cycle_index_by_name("CC_A"), Some(0));
    }

    #[test]
    fn conditional_limits_apply_and_reset() {
        let mut net = Network::new("test");
        // Add two branches with known ratings.
        let mut br0 = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        br0.rating_a_mva = 100.0;
        br0.rating_c_mva = 150.0;
        let mut br1 = Branch::new_line(2, 3, 0.01, 0.1, 0.0);
        br1.rating_a_mva = 200.0;
        br1.rating_c_mva = 250.0;
        net.branches.push(br0);
        net.branches.push(br1);

        // Register conditional limits: branch 0 has summer + winter conditions.
        net.conditional_limits.insert_for_branch(
            &net.branches[0],
            vec![
                ConditionalRating {
                    condition_id: "summer".to_string(),
                    rating_a_mva: 80.0,
                    rating_c_mva: 0.0,
                },
                ConditionalRating {
                    condition_id: "winter".to_string(),
                    rating_a_mva: 120.0,
                    rating_c_mva: 0.0,
                },
            ],
        );

        // Apply summer: branch 0 rate_a should become 80 (more restrictive).
        net.apply_conditional_limits(&["summer".to_string()]);
        assert!(
            (net.branches[0].rating_a_mva - 80.0).abs() < 1e-6,
            "Branch 0 rate_a should be 80 after summer, got {}",
            net.branches[0].rating_a_mva
        );
        assert!(
            (net.branches[1].rating_a_mva - 200.0).abs() < 1e-6,
            "Branch 1 should be unchanged"
        );

        // Apply winter (without reset): branch 0 rate_a should become 120.
        net.apply_conditional_limits(&["winter".to_string()]);
        assert!(
            (net.branches[0].rating_a_mva - 120.0).abs() < 1e-6,
            "Branch 0 rate_a should be 120 after winter, got {}",
            net.branches[0].rating_a_mva
        );

        // Reset: branch 0 should go back to original 100.
        net.reset_conditional_limits();
        assert!(
            (net.branches[0].rating_a_mva - 100.0).abs() < 1e-6,
            "Branch 0 rate_a should be 100 after reset, got {}",
            net.branches[0].rating_a_mva
        );
    }

    #[test]
    fn conditional_limits_clear_preserves_reset_state() {
        let mut net = Network::new("test");
        let mut branch = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        branch.rating_a_mva = 100.0;
        branch.rating_c_mva = 150.0;
        net.branches.push(branch);
        net.conditional_limits.insert_for_branch(
            &net.branches[0],
            vec![ConditionalRating {
                condition_id: "summer".to_string(),
                rating_a_mva: 80.0,
                rating_c_mva: 120.0,
            }],
        );

        net.apply_conditional_limits(&["summer".to_string()]);
        assert!((net.branches[0].rating_a_mva - 80.0).abs() < 1e-6);

        net.conditional_limits.clear();
        net.reset_conditional_limits();

        assert!((net.branches[0].rating_a_mva - 100.0).abs() < 1e-6);
        assert!((net.branches[0].rating_c_mva - 150.0).abs() < 1e-6);
    }

    #[test]
    fn conditional_limits_empty_conditions_reset_to_base() {
        let mut net = Network::new("conditional-empty");
        let mut branch = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        branch.rating_a_mva = 100.0;
        branch.rating_c_mva = 150.0;
        net.branches.push(branch);
        net.conditional_limits.insert_for_branch(
            &net.branches[0],
            vec![ConditionalRating {
                condition_id: "summer".to_string(),
                rating_a_mva: 80.0,
                rating_c_mva: 120.0,
            }],
        );

        net.apply_conditional_limits(&["summer".to_string()]);
        assert!((net.branches[0].rating_a_mva - 80.0).abs() < 1e-6);

        net.apply_conditional_limits(&[]);
        assert!((net.branches[0].rating_a_mva - 100.0).abs() < 1e-6);
        assert!((net.branches[0].rating_c_mva - 150.0).abs() < 1e-6);
    }

    #[test]
    fn conditional_limits_serde_roundtrip_preserves_reset_state() {
        let mut net = Network::new("test");
        let mut branch = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        branch.rating_a_mva = 100.0;
        branch.rating_c_mva = 150.0;
        net.branches.push(branch);
        net.conditional_limits.insert_for_branch(
            &net.branches[0],
            vec![ConditionalRating {
                condition_id: "summer".to_string(),
                rating_a_mva: 80.0,
                rating_c_mva: 120.0,
            }],
        );

        net.apply_conditional_limits(&["summer".to_string()]);
        let json = serde_json::to_string(&net).unwrap();
        let mut roundtripped: Network = serde_json::from_str(&json).unwrap();

        roundtripped.reset_conditional_limits();

        assert!((roundtripped.branches[0].rating_a_mva - 100.0).abs() < 1e-6);
        assert!((roundtripped.branches[0].rating_c_mva - 150.0).abs() < 1e-6);
    }

    #[test]
    fn validate_structure_rejects_reverse_direction_duplicate_branch() {
        let mut net = Network::new("dup-branch");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
        let mut reverse = Branch::new_line(2, 1, 0.01, 0.1, 0.0);
        reverse.circuit = "1".to_string();
        net.branches.push(reverse);

        let err = net.validate_structure().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::DuplicateBranchKey {
                from_bus: 1,
                to_bus: 2,
                ..
            }
        ));
    }

    #[test]
    fn generator_lookup_uses_trimmed_canonical_id() {
        let mut net = Network::new("trimmed-generator-id");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        let mut generator = Generator::new(1, 50.0, 1.0);
        generator.id = "  GEN_A  ".to_string();
        net.generators.push(generator);

        assert_eq!(net.find_gen_index_by_id("GEN_A"), Some(0));
        assert_eq!(net.find_gen_index_by_id("  GEN_A "), Some(0));
        assert_eq!(net.gen_index_by_id().get("GEN_A"), Some(&0));
    }

    #[test]
    fn validate_for_solve_rejects_invalid_switched_shunt_indices() {
        let mut net = Network::new("switched-shunt-index");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.controls.switched_shunts.push(SwitchedShunt {
            id: "ssh_1".into(),
            bus: 1,
            bus_regulated: 0,
            b_step: 0.01,
            n_steps_cap: 1,
            n_steps_react: 0,
            v_target: 1.0,
            v_band: 0.02,
            n_active_steps: 0,
        });

        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidSwitchedShuntRegulatedBus { id, bus }
                if id == "ssh_1" && bus == 0
        ));
    }

    #[test]
    fn validate_for_solve_rejects_slack_without_regulating_generator() {
        let mut net = Network::new("slack-no-reg");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        let mut generator = Generator::new(1, 50.0, 1.0);
        generator.voltage_regulated = false;
        net.generators.push(generator);

        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidSlackPlacement { slack_buses, .. } if slack_buses == vec![1]
        ));
    }

    #[test]
    fn validate_for_solve_rejects_pv_without_regulating_generator() {
        let mut net = Network::new("pv-no-reg");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PV, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
        net.generators.push(Generator::new(1, 60.0, 1.0));
        let mut generator = Generator::new(2, 40.0, 1.0);
        generator.voltage_regulated = false;
        net.generators.push(generator);

        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidGeneratorField {
                bus,
                field: "voltage_regulated",
                value
            } if bus == 2 && value == 0.0
        ));
    }

    #[test]
    fn validate_for_solve_accepts_remote_regulated_pv_bus() {
        let mut net = Network::new("pv-remote-reg");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PV, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));

        let mut slack_generator = Generator::new(1, 60.0, 1.0);
        slack_generator.reg_bus = Some(1);
        net.generators.push(slack_generator);

        let mut remote_generator = Generator::new(1, 20.0, 1.0);
        remote_generator.reg_bus = Some(2);
        net.generators.push(remote_generator);

        assert!(net.validate_for_solve().is_ok());
    }

    #[test]
    fn validate_structure_rejects_missing_remote_regulated_bus() {
        let mut net = Network::new("missing-reg-bus");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        let mut generator = Generator::new(1, 50.0, 1.0);
        generator.reg_bus = Some(2);
        net.generators.push(generator);

        let err = net.validate_structure().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidGeneratorRegulatedBus { bus, reg_bus }
                if bus == 1 && reg_bus == 2
        ));
    }

    #[test]
    fn validate_structure_rejects_empty_interface_members() {
        let mut net = Network::new("interface-mismatch");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.interfaces.push(Interface {
            name: "IF_A".to_string(),
            members: vec![],
            limit_forward_mw: 100.0,
            limit_reverse_mw: 100.0,
            in_service: true,
            limit_forward_mw_schedule: vec![],
            limit_reverse_mw_schedule: vec![],
        });

        let err = net.validate_structure().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidInterfaceDefinition { name, .. } if name == "IF_A"
        ));
    }

    #[test]
    fn validate_structure_rejects_missing_interface_branch_reference() {
        let mut net = Network::new("interface-missing-branch");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.interfaces.push(Interface {
            name: "IF_A".to_string(),
            members: vec![crate::network::WeightedBranchRef::new(1, 2, "1", 1.0)],
            limit_forward_mw: 100.0,
            limit_reverse_mw: 100.0,
            in_service: true,
            limit_forward_mw_schedule: vec![],
            limit_reverse_mw_schedule: vec![],
        });

        let err = net.validate_structure().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidInterfaceDefinition { name, .. } if name == "IF_A"
        ));
    }

    #[test]
    fn validate_structure_rejects_non_finite_flowgate_coefficients() {
        let mut net = Network::new("flowgate-nan");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
        net.flowgates.push(Flowgate {
            name: "FG_A".to_string(),
            monitored: vec![crate::network::WeightedBranchRef::new(1, 2, "1", f64::NAN)],
            contingency_branch: None,
            limit_mw: 100.0,
            limit_reverse_mw: 100.0,
            in_service: true,
            limit_mw_schedule: vec![],
            limit_reverse_mw_schedule: vec![],
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            limit_mw_active_period: None,
            breach_sides: crate::network::FlowgateBreachSides::Both,
        });

        let err = net.validate_structure().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidFlowgateDefinition { name, .. } if name == "FG_A"
        ));
    }

    #[test]
    fn validate_structure_rejects_missing_flowgate_contingency_branch() {
        let mut net = Network::new("flowgate-missing-ctg");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
        net.flowgates.push(Flowgate {
            name: "FG_A".to_string(),
            monitored: vec![crate::network::WeightedBranchRef::new(1, 2, "1", 1.0)],
            contingency_branch: Some(crate::network::BranchRef::new(2, 1, "1")),
            limit_mw: 100.0,
            limit_reverse_mw: 100.0,
            in_service: true,
            limit_mw_schedule: vec![],
            limit_reverse_mw_schedule: vec![],
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            limit_mw_active_period: None,
            breach_sides: crate::network::FlowgateBreachSides::Both,
        });

        let err = net.validate_structure().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::InvalidFlowgateDefinition { name, .. } if name == "FG_A"
        ));
    }

    #[test]
    fn validate_structure_rejects_duplicate_hvdc_link_names() {
        let mut net = Network::new("dup-hvdc-name");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));

        let mut link_a = crate::network::LccHvdcLink {
            name: "HVDC_A".to_string(),
            ..Default::default()
        };
        link_a.rectifier.bus = 1;
        link_a.inverter.bus = 2;
        let mut link_b = crate::network::LccHvdcLink {
            name: "HVDC_A".to_string(),
            ..Default::default()
        };
        link_b.rectifier.bus = 1;
        link_b.inverter.bus = 2;
        net.hvdc.push_lcc_link(link_a);
        net.hvdc.push_lcc_link(link_b);

        let err = net.validate_structure().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::DuplicateHvdcLinkName { name } if name == "HVDC_A"
        ));
    }

    #[test]
    fn validate_for_solve_rejects_mixed_hvdc_representations() {
        let mut net = Network::new("mixed-hvdc");
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));

        let mut link = crate::network::LccHvdcLink {
            name: "HVDC_A".to_string(),
            ..Default::default()
        };
        link.rectifier.bus = 1;
        link.inverter.bus = 2;
        net.hvdc.push_lcc_link(link);

        let grid = net.hvdc.ensure_dc_grid(1, Some("grid".to_string()));
        grid.buses.push(crate::network::DcBus {
            bus_id: 101,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 320.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.converters.push(crate::network::DcConverter::Vsc(
            crate::network::DcConverterStation {
                id: String::new(),
                dc_bus: 101,
                ac_bus: 1,
                control_type_dc: 2,
                control_type_ac: 1,
                active_power_mw: 0.0,
                reactive_power_mvar: 0.0,
                is_lcc: false,
                voltage_setpoint_pu: 1.0,
                transformer_r_pu: 0.0,
                transformer_x_pu: 0.0,
                transformer: false,
                tap_ratio: 1.0,
                filter_susceptance_pu: 0.0,
                filter: false,
                reactor_r_pu: 0.0,
                reactor_x_pu: 0.0,
                reactor: false,
                base_kv_ac: 230.0,
                voltage_max_pu: 1.1,
                voltage_min_pu: 0.9,
                current_max_pu: 2.0,
                status: true,
                loss_constant_mw: 0.0,
                loss_linear: 0.0,
                loss_quadratic_rectifier: 0.0,
                loss_quadratic_inverter: 0.0,
                droop: 0.0,
                power_dc_setpoint_mw: 0.0,
                voltage_dc_setpoint_pu: 1.0,
                active_power_ac_max_mw: 10.0,
                active_power_ac_min_mw: -10.0,
                reactive_power_ac_max_mvar: 10.0,
                reactive_power_ac_min_mvar: -10.0,
            },
        ));

        let err = net.validate_for_solve().unwrap_err();
        assert!(matches!(err, NetworkError::MixedHvdcRepresentation));
    }
}
