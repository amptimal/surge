// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Physical power system model — buses, branches, generators, loads, and all equipment types.

pub(crate) mod serde_defaults;

pub mod angle_reference;
pub mod area_schedule;
pub mod asset;
pub mod boundary;
pub mod branch;
pub mod breaker;
pub mod bus;
pub mod case_diff;
pub mod cgmes_roundtrip;
pub mod contingency;
pub mod dc_line;
pub mod dc_network_types;
pub mod discrete_control;
pub mod facts;
pub mod fixed_shunt;
pub mod flowgate;
pub mod generator;
pub mod grounding;
pub mod hvdc;
pub mod impedance_correction;
pub mod induction_machine;
pub mod load;
pub mod market_data;
pub mod measurement;
pub mod model;
pub mod multi_section_line;
pub mod net_ops;
pub mod op_limits;
pub mod owner;
pub mod power_injection;
pub mod protection;
pub mod refs;
pub mod region;
pub mod scheduled_area_transfer;
pub mod switching_device_rating;
pub mod time_utils;
pub mod topology;
pub mod transformer_db;
pub mod types;
pub mod units;
pub mod voltage_droop_control;
pub mod vsc_dc_line;

// ── Core equipment ──────────────────────────────────────────────────────
pub use branch::{
    Branch, BranchOpfControl, BranchPiAdmittance, BranchPowerFlowsPu, BranchRatingCondition,
    BranchType, HarmonicData, LineData, LineType, PhaseMode, SeriesCompData, TapMode,
    TransformerConnection, TransformerData, WindingConnection, ZeroSeqData,
};
pub use bus::{Bus, BusType};
pub use fixed_shunt::{FixedShunt, ShuntType};
pub use generator::{
    CommitmentParams, CommitmentStatus, FuelParams, FuelSupply, GenFaultData, GenType, Generator,
    GeneratorTechnology, InverterParams, MarketParams, PqLinearLink, RampingParams,
    ReactiveCapability, StorageDispatchMode, StorageParams, StorageValidationError,
};
pub use load::{Load, LoadClass, LoadConnection};

// ── Network model ───────────────────────────────────────────────────────
pub use cgmes_roundtrip::{
    CgmesDanglingLineSource, CgmesEquivalentInjectionSource, CgmesExternalNetworkInjectionSource,
    CgmesRoundtripData,
};
pub use model::{
    BranchConditionalRatings, BranchEquipmentKey, ConditionalRating, GeoPoint, MutualCoupling,
    Network, NetworkCimData, NetworkControlData, NetworkError, NetworkMarketData, NetworkMetadata,
    PhaseImpedanceEntry,
};

// ── Controls & shunts ───────────────────────────────────────────────────
pub use discrete_control::{OltcSpec, ParSpec, SwitchedShunt, SwitchedShuntOpf};
pub use facts::{FactsDevice, FactsMode};

// ── HVDC ────────────────────────────────────────────────────────────────
pub use hvdc::{
    DcBranch, DcBus, DcConverter, DcConverterStation, DcGrid, HvdcLink, HvdcModel,
    LccConverterTerminal, LccDcConverter, LccDcConverterRole, LccHvdcControlMode, LccHvdcLink,
    VscConverterAcControlMode, VscConverterTerminal, VscHvdcControlMode, VscHvdcLink,
};

// ── Contingency ─────────────────────────────────────────────────────────
pub use contingency::{
    Contingency, ContingencyModification, TplCategory, apply_contingency_modifications,
    generate_breaker_contingencies, generate_n1_branch_contingencies,
    generate_p4_stuck_breaker_contingencies, generate_p5_contingencies,
    generate_p6_parallel_contingencies, generate_p6_user_pairs,
};

// ── Topology ────────────────────────────────────────────────────────────
pub use topology::{NodeBreakerTopology, SwitchDevice, SwitchType, TopologyMappingState};

// ── Supplementary ───────────────────────────────────────────────────────
pub use angle_reference::{
    AngleReference, DistributedAngleWeight, apply_angle_reference, apply_angle_reference_subset,
};
pub use area_schedule::AreaSchedule;
pub use flowgate::{
    Flowgate, FlowgateBreachSides, INACTIVE_FLOWGATE_LIMIT_MW, Interface, OperatingNomogram,
};
pub use owner::{Owner, OwnershipEntry};
pub use power_injection::PowerInjection;
pub use refs::{
    BranchRef, BreakerRef, DcBranchRef, DcConverterRef, DcGridRef, EquipmentRef, FactsDeviceRef,
    FixedShuntRef, GeneratorRef, HvdcLinkRef, InductionMachineRef, LccConverterTerminalRef,
    LccTerminalSide, LoadRef, SwitchedShuntRef, WeightedBranchRef,
};
pub use region::Region;
pub use types::{Complex, DEFAULT_BASE_MVA};
pub use units::{ohm_to_pu, pu_to_ohm, pu_to_y, y_to_pu, z_base_ohm};
