// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Economic and dispatch layer — costs, offers, reserves, penalties, storage, and operational data.

pub mod ambient;
pub mod combined_cycle;
pub mod cost;
pub mod dispatchable_load;
pub mod emission;
pub mod energy_offer;
pub mod outage;
pub mod penalty;
pub mod power_balance;
pub mod profiles;
pub mod pumped_hydro;
pub mod reserve;
pub mod rules;
pub mod virtual_bid;

// ── Explicit re-exports ──────────────────────────────────────────────────
pub use ambient::AmbientConditions;
pub use combined_cycle::{CombinedCycleConfig, CombinedCyclePlant, CombinedCycleTransition};
pub use cost::CostCurve;
pub use dispatchable_load::{
    DemandResponseResults, DispatchableLoad, DlOfferSchedule, DlPeriodParams, LoadArchetype,
    LoadCostModel, LoadDispatchResult,
};
pub use emission::{EmissionPolicy, EmissionRates};
pub use energy_offer::{EnergyOffer, OfferCurve, OfferSchedule, StartupTier};
pub use outage::{EquipmentCategory, OutageEntry, OutageType};
pub use penalty::{PenaltyConfig, PenaltyCurve, PenaltySegment};
pub use power_balance::PowerBalanceViolation;
pub use profiles::{
    BranchDerateProfile, BranchDerateProfiles, GeneratorDerateProfile, GeneratorDerateProfiles,
    HvdcDerateProfile, HvdcDerateProfiles, LoadProfile, LoadProfiles, RenewableProfile,
    RenewableProfiles,
};
pub use pumped_hydro::PumpedHydroUnit;
pub use reserve::{
    EnergyCoupling, QualificationMap, QualificationRule, RampSharingConfig, ReserveDirection,
    ReserveKind, ReserveOffer, ReserveProduct, SystemReserveRequirement, ZonalReserveRequirement,
    qualifications_can_overlap, qualifies_for,
};
pub use rules::{MarketRules, ReserveZone};
pub use virtual_bid::{VirtualBid, VirtualBidDirection, VirtualBidResult};
