// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Market-facing request configuration.

use schemars::JsonSchema;
use surge_network::market::{
    DispatchableLoad, DlOfferSchedule, OfferSchedule, PenaltyConfig, ReserveOffer, VirtualBid,
};

use crate::config::emissions::{CarbonPrice, TieLineLimits};
use crate::config::frequency::FrequencySecurityOptions;
use crate::ids::AreaId;
use crate::request::{CommitmentConstraint, PowerBalancePenalty};

/// Per-resource emission rate.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceEmissionRate {
    pub resource_id: String,
    pub rate_tonnes_per_mwh: f64,
}

/// Public keyed emission profile.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct EmissionProfile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceEmissionRate>,
}

/// Public keyed must-run floor list.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct MustRunUnits {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_ids: Vec<String>,
}

/// Per-resource offer schedule override for a generator or storage resource.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GeneratorOfferSchedule {
    pub resource_id: String,
    /// External `surge_network::market::OfferSchedule`; treated as opaque JSON.
    #[schemars(with = "serde_json::Value")]
    pub schedule: OfferSchedule,
}

/// Per-resource offer schedule override for a dispatchable load.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DispatchableLoadOfferSchedule {
    pub resource_id: String,
    /// External `surge_network::market::DlOfferSchedule`; treated as opaque JSON.
    #[schemars(with = "serde_json::Value")]
    pub schedule: DlOfferSchedule,
}

/// Per-resource reserve offer override schedule.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReserveOfferSchedule {
    /// External `surge_network::market::ReserveOffer` per `[period][offer]`;
    /// treated as opaque JSON.
    #[schemars(with = "Vec<Vec<serde_json::Value>>")]
    pub periods: Vec<Vec<ReserveOffer>>,
}

/// Per-resource reserve offer schedule override for a generator or storage resource.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GeneratorReserveOfferSchedule {
    pub resource_id: String,
    pub schedule: ReserveOfferSchedule,
}

/// Per-resource reserve offer schedule override for a dispatchable load.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DispatchableLoadReserveOfferSchedule {
    pub resource_id: String,
    pub schedule: ReserveOfferSchedule,
}

/// Per-storage self schedule.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoragePowerSchedule {
    pub resource_id: String,
    pub values_mw: Vec<f64>,
}

/// Per-storage reserve SOC impact profile.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageReserveSocImpact {
    pub resource_id: String,
    pub product_id: String,
    pub values_mwh_per_mw: Vec<f64>,
}

/// Combined-cycle config offer override.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CombinedCycleConfigOfferSchedule {
    pub plant_id: String,
    pub config_name: String,
    /// External `surge_network::market::OfferSchedule`; treated as opaque JSON.
    #[schemars(with = "serde_json::Value")]
    pub schedule: OfferSchedule,
}

/// Assign an area id to one resource.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceAreaAssignment {
    pub resource_id: String,
    pub area_id: AreaId,
}

/// Assign an area id to one bus.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusAreaAssignment {
    pub bus_number: u32,
    pub area_id: AreaId,
}

/// Boolean eligibility override for one resource.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceEligibility {
    pub resource_id: String,
    pub eligible: bool,
}

/// Absolute startup-count limit for one resource over a horizon window.
///
/// The window is inclusive on both ends: `start_period_idx=0, end_period_idx=23`
/// constrains the first 24 solved periods.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceStartupWindowLimit {
    pub resource_id: String,
    pub start_period_idx: usize,
    pub end_period_idx: usize,
    pub max_startups: u32,
}

/// Absolute energy budget for one resource over a horizon window.
///
/// The window is inclusive on both ends. Either or both bounds may be set.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct ResourceEnergyWindowLimit {
    pub resource_id: String,
    pub start_period_idx: usize,
    pub end_period_idx: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_energy_mwh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_energy_mwh: Option<f64>,
}

/// Generator cost approximation controls shared across dispatch formulations.
///
/// Explicit piecewise-linear curves always use their native epiograph
/// representation. These options control whether convex polynomial generator
/// costs should be outer-linearized into the same PWL form.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct GeneratorCostModeling {
    /// Convert convex polynomial generator costs to a PWL epigraph.
    pub use_pwl_costs: bool,
    /// Number of tangent-line breakpoints per approximated generator.
    pub pwl_cost_breakpoints: usize,
}

impl Default for GeneratorCostModeling {
    fn default() -> Self {
        Self {
            use_pwl_costs: false,
            pwl_cost_breakpoints: 20,
        }
    }
}

/// Market inputs and market-facing policy for dispatch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct DispatchMarket {
    /// External `surge_network::market::ReserveProduct`; opaque JSON.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub reserve_products: Vec<surge_network::market::ReserveProduct>,
    /// External `surge_network::market::SystemReserveRequirement`; opaque JSON.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub system_reserve_requirements: Vec<surge_network::market::SystemReserveRequirement>,
    /// External `surge_network::market::ZonalReserveRequirement`; opaque JSON.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub zonal_reserve_requirements: Vec<surge_network::market::ZonalReserveRequirement>,
    /// External `surge_network::market::RampSharingConfig`; opaque JSON.
    #[schemars(with = "serde_json::Value")]
    pub ramp_sharing: surge_network::market::RampSharingConfig,
    pub co2_cap_t: Option<f64>,
    pub co2_price_per_t: f64,
    pub emission_profile: Option<EmissionProfile>,
    pub carbon_price: Option<CarbonPrice>,
    pub storage_self_schedules: Vec<StoragePowerSchedule>,
    pub storage_reserve_soc_impacts: Vec<StorageReserveSocImpact>,
    pub generator_offer_schedules: Vec<GeneratorOfferSchedule>,
    pub dispatchable_load_offer_schedules: Vec<DispatchableLoadOfferSchedule>,
    pub generator_reserve_offer_schedules: Vec<GeneratorReserveOfferSchedule>,
    pub dispatchable_load_reserve_offer_schedules: Vec<DispatchableLoadReserveOfferSchedule>,
    pub combined_cycle_offer_schedules: Vec<CombinedCycleConfigOfferSchedule>,
    pub tie_line_limits: Option<TieLineLimits>,
    pub resource_area_assignments: Vec<ResourceAreaAssignment>,
    pub bus_area_assignments: Vec<BusAreaAssignment>,
    pub must_run_units: Option<MustRunUnits>,
    pub frequency_security: FrequencySecurityOptions,
    /// External `surge_network::market::DispatchableLoad`; opaque JSON.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub dispatchable_loads: Vec<DispatchableLoad>,
    /// External `surge_network::market::VirtualBid`; opaque JSON.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub virtual_bids: Vec<VirtualBid>,
    pub power_balance_penalty: PowerBalancePenalty,
    /// External `surge_network::market::PenaltyConfig`; opaque JSON.
    #[schemars(with = "serde_json::Value")]
    pub penalty_config: PenaltyConfig,
    /// Canonical generator-cost approximation controls for SCED/SCUC.
    ///
    /// When omitted, generators retain their native cost representation.
    pub generator_cost_modeling: Option<GeneratorCostModeling>,
    pub regulation_eligibility: Vec<ResourceEligibility>,
    pub startup_window_limits: Vec<ResourceStartupWindowLimit>,
    pub energy_window_limits: Vec<ResourceEnergyWindowLimit>,
    pub commitment_constraints: Vec<CommitmentConstraint>,
}

impl Default for DispatchMarket {
    fn default() -> Self {
        Self {
            reserve_products: Vec::new(),
            system_reserve_requirements: Vec::new(),
            zonal_reserve_requirements: Vec::new(),
            ramp_sharing: surge_network::market::RampSharingConfig::default(),
            co2_cap_t: None,
            co2_price_per_t: 0.0,
            emission_profile: None,
            carbon_price: None,
            storage_self_schedules: Vec::new(),
            storage_reserve_soc_impacts: Vec::new(),
            generator_offer_schedules: Vec::new(),
            dispatchable_load_offer_schedules: Vec::new(),
            generator_reserve_offer_schedules: Vec::new(),
            dispatchable_load_reserve_offer_schedules: Vec::new(),
            combined_cycle_offer_schedules: Vec::new(),
            tie_line_limits: None,
            resource_area_assignments: Vec::new(),
            bus_area_assignments: Vec::new(),
            must_run_units: None,
            frequency_security: FrequencySecurityOptions::default(),
            dispatchable_loads: Vec::new(),
            virtual_bids: Vec::new(),
            power_balance_penalty: PowerBalancePenalty::default(),
            penalty_config: PenaltyConfig::default(),
            generator_cost_modeling: None,
            regulation_eligibility: Vec::new(),
            startup_window_limits: Vec::new(),
            energy_window_limits: Vec::new(),
            commitment_constraints: Vec::new(),
        }
    }
}
