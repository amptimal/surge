// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

use std::collections::HashMap;
use std::sync::Arc;

use surge_network::market::{
    BranchDerateProfiles, DispatchableLoad, DlOfferSchedule, GeneratorDerateProfiles,
    HvdcDerateProfiles, LoadProfiles, OfferSchedule, PenaltyConfig, RenewableProfiles, VirtualBid,
};
use surge_opf::AcOpfOptions;
use surge_opf::backends::LpSolver;
use surge_solution::ParSetpoint;

use crate::config::emissions::{CarbonPrice, EmissionProfile, MustRunUnits, TieLineLimits};
use crate::config::frequency::FrequencySecurityOptions;
use crate::dispatch::{
    CommitmentMode, Horizon, IndexedCommitmentConstraint, IndexedDispatchInitialState,
    IndexedEnergyWindowLimit, IndexedPhHeadCurve, IndexedPhModeConstraint,
    IndexedStartupWindowLimit,
};
use crate::hvdc::HvdcDispatchLink;
use crate::request::{
    AcBusLoadProfiles, AcDispatchTargetTracking, Formulation, GeneratorCostModeling,
    GeneratorDispatchBoundsProfiles, PowerBalancePenalty, RampMode, ReserveOfferSchedule,
    ScedAcBendersRuntime,
};

/// Normalized dispatch options controlling formulation, horizon, commitment,
/// and all constraint/feature knobs.
///
/// Prefer [`crate::request::DispatchRequest`] for new code. This test-only type
/// remains only as local test compatibility scaffolding.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct DispatchOptions {
    pub formulation: Formulation,
    pub horizon: Horizon,
    pub commitment: CommitmentMode,
    pub n_periods: usize,
    pub dt_hours: f64,
    pub load_profiles: LoadProfiles,
    pub ac_bus_load_profiles: AcBusLoadProfiles,
    pub renewable_profiles: RenewableProfiles,
    pub gen_derate_profiles: GeneratorDerateProfiles,
    pub generator_dispatch_bounds: GeneratorDispatchBoundsProfiles,
    pub branch_derate_profiles: BranchDerateProfiles,
    pub hvdc_derate_profiles: HvdcDerateProfiles,
    pub initial_state: IndexedDispatchInitialState,
    pub tolerance: f64,
    pub enforce_thermal_limits: bool,
    pub min_rate_a: f64,
    pub enforce_flowgates: bool,
    pub max_nomogram_iter: usize,
    pub par_setpoints: Vec<ParSetpoint>,
    pub reserve_products: Vec<surge_network::market::ReserveProduct>,
    pub system_reserve_requirements: Vec<surge_network::market::SystemReserveRequirement>,
    pub zonal_reserve_requirements: Vec<surge_network::market::ZonalReserveRequirement>,
    pub ramp_sharing: surge_network::market::RampSharingConfig,
    pub co2_cap_t: Option<f64>,
    pub co2_price_per_t: f64,
    pub emission_profile: Option<EmissionProfile>,
    pub carbon_price: Option<CarbonPrice>,
    pub storage_self_schedules: Option<HashMap<usize, Vec<f64>>>,
    pub storage_reserve_soc_impact: HashMap<usize, HashMap<String, Vec<f64>>>,
    pub offer_schedules: HashMap<usize, OfferSchedule>,
    pub dl_offer_schedules: HashMap<usize, DlOfferSchedule>,
    pub gen_reserve_offer_schedules: HashMap<usize, ReserveOfferSchedule>,
    pub dl_reserve_offer_schedules: HashMap<usize, ReserveOfferSchedule>,
    pub cc_config_offers: Vec<Vec<OfferSchedule>>,
    pub hvdc_links: Vec<HvdcDispatchLink>,
    pub tie_line_limits: Option<TieLineLimits>,
    pub generator_area: Vec<usize>,
    pub load_area: Vec<usize>,
    pub must_run_units: Option<MustRunUnits>,
    pub frequency_security: FrequencySecurityOptions,
    pub dispatchable_loads: Vec<DispatchableLoad>,
    pub virtual_bids: Vec<VirtualBid>,
    pub power_balance_penalty: PowerBalancePenalty,
    pub penalty_config: PenaltyConfig,
    pub generator_cost_modeling: Option<GeneratorCostModeling>,
    pub use_loss_factors: bool,
    pub max_loss_factor_iters: usize,
    pub loss_factor_tol: f64,
    pub ac_opf: AcOpfOptions,
    pub enforce_forbidden_zones: bool,
    pub foz_max_transit_periods: Option<usize>,
    pub enforce_shutdown_deloading: bool,
    pub offline_commitment_trajectories: bool,
    pub ramp_mode: RampMode,
    /// When `true`, SCUC ramp inequality rows are enforced as hard
    /// constraints (slack pinned to zero). Defaults to `false`. See
    /// `DispatchInput::ramp_constraints_hard` for the rationale.
    pub ramp_constraints_hard: bool,
    /// Penalty coefficient on multi-interval energy window violations,
    /// in $/pu-h. Defaults to 0.0.
    pub energy_window_violation_per_puh: f64,
    /// When `true`, energy-window limits are enforced as hard
    /// constraints (slack pinned to zero). Test-only compatibility
    /// scaffolding for `DispatchProblemSpec::from_options`.
    pub energy_window_constraints_hard: bool,
    /// When `true`, AC branch on/off binaries are decision variables.
    /// Defaults to `false`.
    pub allow_branch_switching: bool,
    /// Big-M factor for switchable-branch flow definition rows.
    /// Defaults to `10.0`.
    pub branch_switching_big_m_factor: f64,
    pub regulation_eligible: Option<Vec<bool>>,
    pub startup_window_limits: Vec<IndexedStartupWindowLimit>,
    pub energy_window_limits: Vec<IndexedEnergyWindowLimit>,
    pub commitment_constraints: Vec<IndexedCommitmentConstraint>,
    pub ph_head_curves: Vec<IndexedPhHeadCurve>,
    pub ph_mode_constraints: Vec<IndexedPhModeConstraint>,
    pub ac_generator_warm_start_p_mw: HashMap<usize, Vec<f64>>,
    pub ac_generator_warm_start_q_mvar: HashMap<usize, Vec<f64>>,
    pub ac_bus_warm_start_vm_pu: HashMap<usize, Vec<f64>>,
    pub ac_bus_warm_start_va_rad: HashMap<usize, Vec<f64>>,
    pub ac_dispatchable_load_warm_start_p_mw: HashMap<usize, Vec<f64>>,
    pub ac_dispatchable_load_warm_start_q_mvar: HashMap<usize, Vec<f64>>,
    pub fixed_hvdc_dispatch_mw: HashMap<usize, Vec<f64>>,
    pub fixed_hvdc_dispatch_q_fr_mvar: HashMap<usize, Vec<f64>>,
    pub fixed_hvdc_dispatch_q_to_mvar: HashMap<usize, Vec<f64>>,
    pub ac_hvdc_warm_start_p_mw: HashMap<usize, Vec<f64>>,
    pub ac_hvdc_warm_start_q_fr_mvar: HashMap<usize, Vec<f64>>,
    pub ac_hvdc_warm_start_q_to_mvar: HashMap<usize, Vec<f64>>,
    pub ac_target_tracking: AcDispatchTargetTracking,
    pub sced_ac_benders: ScedAcBendersRuntime,
    pub run_pricing: bool,
    pub ac_relax_committed_pmin_to_zero: bool,
    pub lp_solver: Option<Arc<dyn LpSolver>>,
}

impl Default for DispatchOptions {
    fn default() -> Self {
        Self {
            formulation: Formulation::Dc,
            horizon: Horizon::Sequential,
            commitment: CommitmentMode::AllCommitted,
            n_periods: 1,
            dt_hours: 1.0,
            load_profiles: LoadProfiles::default(),
            ac_bus_load_profiles: AcBusLoadProfiles::default(),
            renewable_profiles: RenewableProfiles::default(),
            gen_derate_profiles: GeneratorDerateProfiles::default(),
            generator_dispatch_bounds: GeneratorDispatchBoundsProfiles::default(),
            branch_derate_profiles: BranchDerateProfiles::default(),
            hvdc_derate_profiles: HvdcDerateProfiles::default(),
            initial_state: IndexedDispatchInitialState::default(),
            tolerance: 1e-8,
            enforce_thermal_limits: true,
            min_rate_a: 1.0,
            enforce_flowgates: true,
            max_nomogram_iter: 10,
            par_setpoints: Vec::new(),
            reserve_products: Vec::new(),
            system_reserve_requirements: Vec::new(),
            zonal_reserve_requirements: Vec::new(),
            ramp_sharing: surge_network::market::RampSharingConfig::default(),
            co2_cap_t: None,
            co2_price_per_t: 0.0,
            emission_profile: None,
            carbon_price: None,
            storage_self_schedules: None,
            storage_reserve_soc_impact: HashMap::new(),
            offer_schedules: HashMap::new(),
            dl_offer_schedules: HashMap::new(),
            gen_reserve_offer_schedules: HashMap::new(),
            dl_reserve_offer_schedules: HashMap::new(),
            cc_config_offers: Vec::new(),
            hvdc_links: Vec::new(),
            tie_line_limits: None,
            generator_area: Vec::new(),
            load_area: Vec::new(),
            must_run_units: None,
            frequency_security: FrequencySecurityOptions::default(),
            dispatchable_loads: Vec::new(),
            virtual_bids: Vec::new(),
            power_balance_penalty: PowerBalancePenalty::default(),
            penalty_config: PenaltyConfig::default(),
            generator_cost_modeling: None,
            use_loss_factors: false,
            max_loss_factor_iters: 3,
            loss_factor_tol: 1e-3,
            ac_opf: AcOpfOptions::default(),
            enforce_forbidden_zones: false,
            foz_max_transit_periods: None,
            enforce_shutdown_deloading: false,
            offline_commitment_trajectories: false,
            ramp_mode: RampMode::default(),
            ramp_constraints_hard: false,
            energy_window_violation_per_puh: 0.0,
            energy_window_constraints_hard: false,
            allow_branch_switching: false,
            branch_switching_big_m_factor: 10.0,
            regulation_eligible: None,
            startup_window_limits: Vec::new(),
            energy_window_limits: Vec::new(),
            commitment_constraints: Vec::new(),
            ph_head_curves: Vec::new(),
            ph_mode_constraints: Vec::new(),
            ac_generator_warm_start_p_mw: HashMap::new(),
            ac_generator_warm_start_q_mvar: HashMap::new(),
            ac_bus_warm_start_vm_pu: HashMap::new(),
            ac_bus_warm_start_va_rad: HashMap::new(),
            ac_dispatchable_load_warm_start_p_mw: HashMap::new(),
            ac_dispatchable_load_warm_start_q_mvar: HashMap::new(),
            fixed_hvdc_dispatch_mw: HashMap::new(),
            fixed_hvdc_dispatch_q_fr_mvar: HashMap::new(),
            fixed_hvdc_dispatch_q_to_mvar: HashMap::new(),
            ac_hvdc_warm_start_p_mw: HashMap::new(),
            ac_hvdc_warm_start_q_fr_mvar: HashMap::new(),
            ac_hvdc_warm_start_q_to_mvar: HashMap::new(),
            ac_target_tracking: AcDispatchTargetTracking::default(),
            sced_ac_benders: ScedAcBendersRuntime::default(),
            run_pricing: true,
            ac_relax_committed_pmin_to_zero: false,
            lp_solver: None,
        }
    }
}
