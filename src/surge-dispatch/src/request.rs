// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Typed dispatch request surface.
//!
//! The public API centers one [`DispatchRequest`] with explicit study axes and
//! grouped configuration domains. Solver-specific routing remains internal.

mod axes;
mod commitment;
mod market;
mod network;
mod profiles;
mod resolve;
mod runtime;
mod state;
mod timeline;

use std::collections::HashMap;
use std::sync::Arc;

use surge_network::Network;
use surge_network::market::{
    DispatchableLoad, DlOfferSchedule, OfferSchedule, PenaltyConfig, VirtualBid,
};
use surge_opf::backends::LpSolver;
use surge_solution::ParSetpoint;

use crate::common::spec::DispatchProblemSpec;
use crate::config::emissions::{
    CarbonPrice, EmissionProfile as IndexedEmissionProfile, MustRunUnits as IndexedMustRunUnits,
    TieLineLimits,
};
use crate::config::frequency::FrequencySecurityOptions;
use crate::dispatch::{
    CommitmentMode, Horizon, IndexedCommitmentConstraint, IndexedDispatchInitialState,
    IndexedEnergyWindowLimit, IndexedPhHeadCurve, IndexedPhModeConstraint,
    IndexedStartupWindowLimit,
};
use crate::error::{DispatchError, ScedError};
use crate::hvdc::HvdcDispatchLink;
use crate::scuc::types::SecurityDispatchSpec;

pub use axes::*;
pub use commitment::*;
pub use market::*;
pub use network::*;
pub use profiles::*;
pub use runtime::*;
pub use state::*;
pub use timeline::*;

/// Public dispatch request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "DispatchRequest",
    description = "Public dispatch request consumed by surge_dispatch::solve_dispatch."
)]
pub struct DispatchRequest {
    pub(crate) formulation: Formulation,
    pub(crate) coupling: IntervalCoupling,
    pub(crate) commitment: CommitmentPolicy,
    pub(crate) timeline: DispatchTimeline,
    pub(crate) profiles: DispatchProfiles,
    pub(crate) state: DispatchState,
    pub(crate) market: DispatchMarket,
    pub(crate) network: DispatchNetwork,
    pub(crate) runtime: DispatchRuntime,
}

/// Axis-first builder for [`DispatchRequest`].
#[derive(Debug, Clone, Default)]
pub struct DispatchRequestBuilder {
    request: DispatchRequest,
}

impl Default for DispatchRequest {
    fn default() -> Self {
        Self {
            formulation: Formulation::Dc,
            coupling: IntervalCoupling::PeriodByPeriod,
            commitment: CommitmentPolicy::AllCommitted,
            timeline: DispatchTimeline::default(),
            profiles: DispatchProfiles::default(),
            state: DispatchState::default(),
            market: DispatchMarket::default(),
            network: DispatchNetwork::default(),
            runtime: DispatchRuntime::default(),
        }
    }
}

impl DispatchRequestBuilder {
    pub fn formulation(mut self, formulation: Formulation) -> Self {
        self.request.formulation = formulation;
        self
    }

    pub fn dc(self) -> Self {
        self.formulation(Formulation::Dc)
    }

    pub fn ac(self) -> Self {
        self.formulation(Formulation::Ac)
    }

    pub fn coupling(mut self, coupling: IntervalCoupling) -> Self {
        self.request.coupling = coupling;
        self
    }

    pub fn period_by_period(self) -> Self {
        self.coupling(IntervalCoupling::PeriodByPeriod)
    }

    pub fn time_coupled(self) -> Self {
        self.coupling(IntervalCoupling::TimeCoupled)
    }

    pub fn commitment(mut self, commitment: CommitmentPolicy) -> Self {
        self.request.commitment = commitment;
        self
    }

    pub fn all_committed(self) -> Self {
        self.commitment(CommitmentPolicy::AllCommitted)
    }

    pub fn fixed_commitment(self, schedule: CommitmentSchedule) -> Self {
        self.commitment(CommitmentPolicy::Fixed(schedule))
    }

    pub fn optimize_commitment(self, options: CommitmentOptions) -> Self {
        self.commitment(CommitmentPolicy::Optimize(options))
    }

    pub fn additional_commitment(
        self,
        minimum_commitment: Vec<ResourcePeriodCommitment>,
        options: CommitmentOptions,
    ) -> Self {
        self.commitment(CommitmentPolicy::Additional {
            minimum_commitment,
            options,
        })
    }

    pub fn timeline(mut self, timeline: DispatchTimeline) -> Self {
        self.request.timeline = timeline;
        self
    }

    pub fn hourly(mut self, periods: usize) -> Self {
        self.request.timeline = DispatchTimeline::hourly(periods);
        self
    }

    pub fn intervals(mut self, periods: usize, interval_hours: f64) -> Self {
        self.request.timeline = DispatchTimeline {
            periods,
            interval_hours,
            interval_hours_by_period: Vec::new(),
        };
        self
    }

    pub fn intervals_by_period(mut self, interval_hours_by_period: Vec<f64>) -> Self {
        self.request.timeline = DispatchTimeline::variable(interval_hours_by_period);
        self
    }

    pub fn profiles(mut self, profiles: DispatchProfiles) -> Self {
        self.request.profiles = profiles;
        self
    }

    pub fn update_profiles(mut self, update: impl FnOnce(&mut DispatchProfiles)) -> Self {
        update(&mut self.request.profiles);
        self
    }

    pub fn state(mut self, state: DispatchState) -> Self {
        self.request.state = state;
        self
    }

    pub fn update_state(mut self, update: impl FnOnce(&mut DispatchState)) -> Self {
        update(&mut self.request.state);
        self
    }

    pub fn market(mut self, market: DispatchMarket) -> Self {
        self.request.market = market;
        self
    }

    pub fn update_market(mut self, update: impl FnOnce(&mut DispatchMarket)) -> Self {
        update(&mut self.request.market);
        self
    }

    pub fn network(mut self, network: DispatchNetwork) -> Self {
        self.request.network = network;
        self
    }

    pub fn update_network(mut self, update: impl FnOnce(&mut DispatchNetwork)) -> Self {
        update(&mut self.request.network);
        self
    }

    pub fn runtime(mut self, runtime: DispatchRuntime) -> Self {
        self.request.runtime = runtime;
        self
    }

    pub fn run_pricing(mut self, run_pricing: bool) -> Self {
        self.request.runtime.run_pricing = run_pricing;
        self
    }

    pub fn without_pricing(self) -> Self {
        self.run_pricing(false)
    }

    pub fn ac_relax_committed_pmin_to_zero(mut self, enabled: bool) -> Self {
        self.request.runtime.ac_relax_committed_pmin_to_zero = enabled;
        self
    }

    pub fn update_runtime(mut self, update: impl FnOnce(&mut DispatchRuntime)) -> Self {
        update(&mut self.request.runtime);
        self
    }

    pub fn security(mut self, security: SecurityPolicy) -> Self {
        self.request.network.security = Some(security);
        self
    }

    pub fn ac_opf(mut self, ac_opf: surge_opf::AcOpfOptions) -> Self {
        self.request.runtime.ac_opf = Some(ac_opf);
        self
    }

    pub fn add_load_profile(mut self, bus_number: u32, values_mw: Vec<f64>) -> Self {
        self.request.profiles.load.profiles.push(BusLoadProfile {
            bus_number,
            values_mw,
        });
        self
    }

    pub fn add_ac_bus_load_profile(mut self, profile: AcBusLoadProfile) -> Self {
        self.request.profiles.ac_bus_load.profiles.push(profile);
        self
    }

    pub fn add_ac_bus_voltage_warm_start(
        mut self,
        bus_number: u32,
        vm_pu: Vec<f64>,
        va_rad: Vec<f64>,
    ) -> Self {
        self.request
            .runtime
            .ac_dispatch_warm_start
            .buses
            .push(BusPeriodVoltageSeries {
                bus_number,
                vm_pu,
                va_rad,
            });
        self
    }

    pub fn add_renewable_profile(
        mut self,
        resource_id: impl Into<String>,
        capacity_factors: Vec<f64>,
    ) -> Self {
        self.request
            .profiles
            .renewable
            .profiles
            .push(RenewableProfile {
                resource_id: resource_id.into(),
                capacity_factors,
            });
        self
    }

    pub fn add_generator_derate_profile(
        mut self,
        resource_id: impl Into<String>,
        derate_factors: Vec<f64>,
    ) -> Self {
        self.request
            .profiles
            .generator_derates
            .profiles
            .push(GeneratorDerateProfile {
                resource_id: resource_id.into(),
                derate_factors,
            });
        self
    }

    pub fn add_generator_dispatch_bounds_profile(
        mut self,
        resource_id: impl Into<String>,
        p_min_mw: Vec<f64>,
        p_max_mw: Vec<f64>,
    ) -> Self {
        self.request
            .profiles
            .generator_dispatch_bounds
            .profiles
            .push(GeneratorDispatchBoundsProfile {
                resource_id: resource_id.into(),
                p_min_mw,
                p_max_mw,
                q_min_mvar: None,
                q_max_mvar: None,
            });
        self
    }

    pub fn add_generator_dispatch_bounds_profile_with_q(
        mut self,
        resource_id: impl Into<String>,
        p_min_mw: Vec<f64>,
        p_max_mw: Vec<f64>,
        q_min_mvar: Vec<f64>,
        q_max_mvar: Vec<f64>,
    ) -> Self {
        self.request
            .profiles
            .generator_dispatch_bounds
            .profiles
            .push(GeneratorDispatchBoundsProfile {
                resource_id: resource_id.into(),
                p_min_mw,
                p_max_mw,
                q_min_mvar: Some(q_min_mvar),
                q_max_mvar: Some(q_max_mvar),
            });
        self
    }

    pub fn add_branch_derate_profile(
        mut self,
        branch: BranchRef,
        derate_factors: Vec<f64>,
    ) -> Self {
        self.request
            .profiles
            .branch_derates
            .profiles
            .push(crate::request::BranchDerateProfile {
                branch,
                derate_factors,
            });
        self
    }

    pub fn add_hvdc_derate_profile(
        mut self,
        link_id: impl Into<String>,
        derate_factors: Vec<f64>,
    ) -> Self {
        self.request
            .profiles
            .hvdc_derates
            .profiles
            .push(crate::request::HvdcDerateProfile {
                link_id: link_id.into(),
                derate_factors,
            });
        self
    }

    pub fn add_generator_offer_schedule(mut self, schedule: GeneratorOfferSchedule) -> Self {
        self.request.market.generator_offer_schedules.push(schedule);
        self
    }

    pub fn add_dispatchable_load_offer_schedule(
        mut self,
        schedule: DispatchableLoadOfferSchedule,
    ) -> Self {
        self.request
            .market
            .dispatchable_load_offer_schedules
            .push(schedule);
        self
    }

    pub fn add_storage_power_schedule(mut self, schedule: StoragePowerSchedule) -> Self {
        self.request.market.storage_self_schedules.push(schedule);
        self
    }

    pub fn add_hvdc_link(mut self, link: HvdcDispatchLink) -> Self {
        self.request.network.hvdc_links.push(link);
        self
    }

    pub fn add_previous_resource_dispatch(
        mut self,
        resource_id: impl Into<String>,
        mw: f64,
    ) -> Self {
        self.request
            .state
            .initial
            .previous_resource_dispatch
            .push(ResourceDispatchPoint {
                resource_id: resource_id.into(),
                mw,
            });
        self
    }

    pub fn add_previous_hvdc_dispatch(mut self, link_id: impl Into<String>, mw: f64) -> Self {
        self.request
            .state
            .initial
            .previous_hvdc_dispatch
            .push(HvdcDispatchPoint {
                link_id: link_id.into(),
                mw,
            });
        self
    }

    pub fn add_storage_soc_override(
        mut self,
        resource_id: impl Into<String>,
        soc_mwh: f64,
    ) -> Self {
        self.request
            .state
            .initial
            .storage_soc_overrides
            .push(StorageSocOverride {
                resource_id: resource_id.into(),
                soc_mwh,
            });
        self
    }

    pub fn build(self) -> DispatchRequest {
        self.request
    }
}

/// Model-bound dispatch study prepared for execution.
#[derive(Debug, Clone)]
pub struct PreparedDispatchRequest {
    pub(crate) request: DispatchRequest,
    pub(crate) normalized: NormalizedDispatchRequest,
}

impl PreparedDispatchRequest {
    /// Access the original portable request spec.
    pub fn request(&self) -> &DispatchRequest {
        &self.request
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedDispatchRequest {
    pub input: DispatchInput,
    pub formulation: Formulation,
    pub coupling: IntervalCoupling,
    pub horizon: Horizon,
    pub commitment: CommitmentMode,
    pub ac_opf: surge_opf::AcOpfOptions,
    pub ac_opf_runtime: surge_opf::AcOpfRuntime,
    pub security: Option<ResolvedSecurityScreening>,
}

impl NormalizedDispatchRequest {
    pub(crate) fn problem_spec(&self) -> DispatchProblemSpec<'_> {
        DispatchProblemSpec::from_request(&self.input, &self.commitment)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedSecurityScreening {
    pub embedding: SecurityEmbedding,
    pub max_iterations: usize,
    pub violation_tolerance_pu: f64,
    pub max_cuts_per_iteration: usize,
    pub contingency_branches: Vec<usize>,
    pub hvdc_contingency_indices: Vec<usize>,
    pub preseed_count_per_period: usize,
    pub preseed_method: SecurityPreseedMethod,
}

impl ResolvedSecurityScreening {
    pub(crate) fn into_security_dispatch_spec(
        self,
        input: DispatchInput,
        commitment: CommitmentMode,
    ) -> SecurityDispatchSpec {
        SecurityDispatchSpec {
            input,
            commitment,
            max_iterations: self.max_iterations,
            violation_tolerance_pu: self.violation_tolerance_pu,
            max_cuts_per_iteration: self.max_cuts_per_iteration,
            contingency_branches: self.contingency_branches,
            hvdc_contingency_indices: self.hvdc_contingency_indices,
            preseed_count_per_period: self.preseed_count_per_period,
            preseed_method: self.preseed_method,
        }
    }
}

/// Internal flat dispatch input consumed by the solver engines.
#[derive(Debug, Clone)]
pub(crate) struct DispatchInput {
    pub n_periods: usize,
    pub dt_hours: f64,
    pub period_hours: Vec<f64>,
    pub period_hour_prefix: Vec<f64>,
    pub load_profiles: surge_network::market::LoadProfiles,
    pub ac_bus_load_profiles: AcBusLoadProfiles,
    pub renewable_profiles: surge_network::market::RenewableProfiles,
    pub gen_derate_profiles: surge_network::market::GeneratorDerateProfiles,
    pub generator_dispatch_bounds: GeneratorDispatchBoundsProfiles,
    pub branch_derate_profiles: surge_network::market::BranchDerateProfiles,
    pub hvdc_derate_profiles: surge_network::market::HvdcDerateProfiles,
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
    pub emission_profile: Option<IndexedEmissionProfile>,
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
    pub must_run_units: Option<IndexedMustRunUnits>,
    pub frequency_security: FrequencySecurityOptions,
    pub dispatchable_loads: Vec<DispatchableLoad>,
    pub virtual_bids: Vec<VirtualBid>,
    pub power_balance_penalty: PowerBalancePenalty,
    pub penalty_config: PenaltyConfig,
    pub generator_cost_modeling: Option<GeneratorCostModeling>,
    pub use_loss_factors: bool,
    pub max_loss_factor_iters: usize,
    pub loss_factor_tol: f64,
    pub enforce_forbidden_zones: bool,
    pub foz_max_transit_periods: Option<usize>,
    pub enforce_shutdown_deloading: bool,
    pub offline_commitment_trajectories: bool,
    pub ramp_mode: RampMode,
    /// When `true`, the SCUC ramp inequality rows are enforced as hard
    /// constraints (slack columns pinned to zero). See `DispatchNetwork::
    /// ramp_constraints_hard` for the rationale; this is the resolved
    /// counterpart that surge-dispatch reads at LP build time.
    pub ramp_constraints_hard: bool,
    /// Penalty coefficient on multi-interval energy window violations,
    /// in $/pu-h. Resolved from
    /// `DispatchNetwork::energy_windows.penalty_per_puh`.
    pub energy_window_constraints_hard: bool,
    pub energy_window_violation_per_puh: f64,
    /// When `true`, AC branch on/off binaries are free in `{0, 1}` and
    /// connectivity cuts are added by the security loop.
    /// See `DispatchNetwork::allow_branch_switching` for the rationale.
    pub allow_branch_switching: bool,
    /// Big-M factor (`× fmax`) for the switchable-branch flow
    /// definition rows. Resolved from
    /// `DispatchNetwork::branch_switching_big_m_factor`.
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
    /// SCED-AC Benders decomposition state: per-period eta activation flags
    /// and the accumulated optimality cut pool. Empty by default; the
    /// orchestration loop populates this between iterations.
    pub sced_ac_benders: ScedAcBendersRuntime,
    pub run_pricing: bool,
    pub ac_relax_committed_pmin_to_zero: bool,
    pub lp_solver: Option<Arc<dyn LpSolver>>,
    pub capture_model_diagnostics: bool,
    /// Per-period AC SCED concurrency. See
    /// [`crate::request::DispatchRuntime::ac_sced_period_concurrency`] for
    /// semantics.
    pub ac_sced_period_concurrency: Option<usize>,
}

impl Default for DispatchInput {
    fn default() -> Self {
        Self {
            n_periods: 1,
            dt_hours: 1.0,
            period_hours: vec![],
            period_hour_prefix: vec![0.0, 1.0],
            load_profiles: surge_network::market::LoadProfiles::default(),
            ac_bus_load_profiles: AcBusLoadProfiles::default(),
            renewable_profiles: surge_network::market::RenewableProfiles::default(),
            gen_derate_profiles: surge_network::market::GeneratorDerateProfiles::default(),
            generator_dispatch_bounds: GeneratorDispatchBoundsProfiles::default(),
            branch_derate_profiles: surge_network::market::BranchDerateProfiles::default(),
            hvdc_derate_profiles: surge_network::market::HvdcDerateProfiles::default(),
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
            enforce_forbidden_zones: false,
            foz_max_transit_periods: None,
            enforce_shutdown_deloading: false,
            offline_commitment_trajectories: false,
            ramp_mode: RampMode::default(),
            ramp_constraints_hard: false,
            energy_window_constraints_hard: false,
            energy_window_violation_per_puh: 0.0,
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
            capture_model_diagnostics: false,
            ac_sced_period_concurrency: None,
        }
    }
}

impl DispatchRequest {
    /// Start building an axis-first dispatch request.
    pub fn builder() -> DispatchRequestBuilder {
        DispatchRequestBuilder::default()
    }

    /// Network formulation.
    pub fn formulation(&self) -> Formulation {
        self.formulation
    }

    /// Interval coupling policy.
    pub fn coupling(&self) -> IntervalCoupling {
        self.coupling
    }

    /// Commitment policy.
    pub fn commitment(&self) -> &CommitmentPolicy {
        &self.commitment
    }

    /// Study timeline.
    pub fn timeline(&self) -> &DispatchTimeline {
        &self.timeline
    }

    /// Time-series profiles applied during the study.
    pub fn profiles(&self) -> &DispatchProfiles {
        &self.profiles
    }

    /// Initial dispatch state.
    pub fn state(&self) -> &DispatchState {
        &self.state
    }

    /// Market-facing study configuration.
    pub fn market(&self) -> &DispatchMarket {
        &self.market
    }

    /// Network-facing study configuration.
    pub fn network(&self) -> &DispatchNetwork {
        &self.network
    }

    /// Runtime execution controls.
    pub fn runtime(&self) -> &DispatchRuntime {
        &self.runtime
    }

    /// Optional N-1 security configuration.
    pub fn security(&self) -> Option<&SecurityPolicy> {
        self.network.security.as_ref()
    }

    /// Optional AC OPF options.
    pub fn ac_opf(&self) -> Option<&surge_opf::AcOpfOptions> {
        self.runtime.ac_opf.as_ref()
    }

    /// Override the network formulation. Consumed by market workflow
    /// builders that reuse a base request across multiple stages with
    /// different formulations (DC SCUC → AC SCED, etc.).
    pub fn set_formulation(&mut self, formulation: Formulation) {
        self.formulation = formulation;
    }

    /// Override the commitment policy. Used by the canonical
    /// two-stage workflow to promote stage 1's solved commitment
    /// into stage 2's fixed schedule.
    pub fn set_commitment(&mut self, commitment: CommitmentPolicy) {
        self.commitment = commitment;
    }

    /// Override the interval coupling policy.
    pub fn set_coupling(&mut self, coupling: IntervalCoupling) {
        self.coupling = coupling;
    }

    /// Mutable access to the per-period profiles. Used by market
    /// workflow executors that need to pin generator dispatch bounds
    /// to a prior stage's solved dispatch.
    pub fn profiles_mut(&mut self) -> &mut DispatchProfiles {
        &mut self.profiles
    }

    /// Mutable access to runtime execution controls. Used by market
    /// workflow executors that apply per-attempt overrides
    /// (`ac_opf`, `ac_relax_committed_pmin_to_zero`,
    /// `ac_target_tracking`, etc.) to a cloned request.
    pub fn runtime_mut(&mut self) -> &mut DispatchRuntime {
        &mut self.runtime
    }

    /// Mutable access to the market configuration. Used by market
    /// workflow executors that apply per-stage filters (e.g. drop
    /// active reserve products on the AC SCED stage).
    pub fn market_mut(&mut self) -> &mut DispatchMarket {
        &mut self.market
    }

    /// Replace the market configuration wholesale.
    pub fn set_market(&mut self, market: DispatchMarket) {
        self.market = market;
    }

    /// Validate request shape that does not depend on a specific dispatch model.
    pub fn validate(&self) -> Result<(), DispatchError> {
        self.validate_shape()
    }

    /// Validate request shape that does not depend on a specific dispatch model.
    pub fn validate_shape(&self) -> Result<(), DispatchError> {
        self.validate_request()
    }

    fn validate_request(&self) -> Result<(), ScedError> {
        self.timeline.validate()?;

        if self.formulation != Formulation::Ac && !self.profiles.ac_bus_load.profiles.is_empty() {
            return Err(ScedError::InvalidInput(
                "AC bus load profiles are only valid for AC dispatch".to_string(),
            ));
        }

        if self.formulation == Formulation::Ac && self.coupling != IntervalCoupling::PeriodByPeriod
        {
            return Err(ScedError::InvalidInput(
                "AC dispatch only supports coupling=PeriodByPeriod".to_string(),
            ));
        }

        if self.network.security.is_some()
            && (self.formulation != Formulation::Dc
                || self.coupling != IntervalCoupling::TimeCoupled)
        {
            return Err(ScedError::InvalidInput(
                "security requires formulation=DC and coupling=TimeCoupled".to_string(),
            ));
        }

        if self.formulation != Formulation::Ac && self.runtime.ac_opf.is_some() {
            return Err(ScedError::InvalidInput(
                "AC OPF settings are only valid for AC dispatch".to_string(),
            ));
        }

        if self.formulation != Formulation::Ac && !self.runtime.ac_dispatch_warm_start.is_empty() {
            return Err(ScedError::InvalidInput(
                "AC dispatch warm-start schedules are only valid for AC dispatch".to_string(),
            ));
        }

        if self.runtime.ac_target_tracking.generator_p_penalty_per_mw2 < 0.0
            || self
                .runtime
                .ac_target_tracking
                .dispatchable_load_p_penalty_per_mw2
                < 0.0
        {
            return Err(ScedError::InvalidInput(
                "runtime.ac_target_tracking penalties must be non-negative".to_string(),
            ));
        }

        if self.formulation != Formulation::Ac && !self.runtime.ac_target_tracking.is_disabled() {
            return Err(ScedError::InvalidInput(
                "runtime.ac_target_tracking is only valid for AC dispatch".to_string(),
            ));
        }

        if self.formulation != Formulation::Ac && self.runtime.ac_relax_committed_pmin_to_zero {
            return Err(ScedError::InvalidInput(
                "runtime.ac_relax_committed_pmin_to_zero is only valid for AC dispatch".to_string(),
            ));
        }

        match &self.commitment {
            CommitmentPolicy::AllCommitted | CommitmentPolicy::Fixed(_) => {}
            CommitmentPolicy::Optimize(_) | CommitmentPolicy::Additional { options: _, .. } => {
                if self.coupling != IntervalCoupling::TimeCoupled {
                    return Err(ScedError::InvalidInput(
                        "commitment optimization requires coupling=TimeCoupled".to_string(),
                    ));
                }
                if self.formulation == Formulation::Ac {
                    return Err(ScedError::InvalidInput(
                        "AC dispatch does not support commitment optimization".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn normalize(&self) -> Result<NormalizedDispatchRequest, ScedError> {
        self.normalize_with_options(&DispatchSolveOptions::default())
    }

    #[cfg(test)]
    pub(crate) fn normalize_with_options(
        &self,
        solve_options: &DispatchSolveOptions,
    ) -> Result<NormalizedDispatchRequest, ScedError> {
        self.validate_request()?;
        let commitment =
            strip_request_legacy_aliases(resolve::resolve_commitment(self, None, None)?);

        Ok(NormalizedDispatchRequest {
            input: resolve::build_input(self, None, None, solve_options)?,
            formulation: self.formulation,
            coupling: self.coupling,
            horizon: self.coupling.into(),
            commitment,
            ac_opf: self.runtime.ac_opf.clone().unwrap_or_default(),
            ac_opf_runtime: surge_opf::AcOpfRuntime {
                nlp_solver: solve_options.nlp_solver.clone(),
                ..Default::default()
            },
            security: resolve::resolve_security(self, None, None)?,
        })
    }

    pub(crate) fn resolve_with_options(
        &self,
        network: &Network,
        solve_options: &DispatchSolveOptions,
    ) -> Result<NormalizedDispatchRequest, ScedError> {
        self.validate_request()?;
        let catalog = resolve::ResolveCatalog::from_request(self, network)?;
        let commitment = strip_request_legacy_aliases(resolve::resolve_commitment(
            self,
            Some(network),
            Some(&catalog),
        )?);
        Ok(NormalizedDispatchRequest {
            input: resolve::build_input(self, Some(network), Some(&catalog), solve_options)?,
            formulation: self.formulation,
            coupling: self.coupling,
            horizon: self.coupling.into(),
            commitment,
            ac_opf: self.runtime.ac_opf.clone().unwrap_or_default(),
            ac_opf_runtime: surge_opf::AcOpfRuntime {
                nlp_solver: solve_options.nlp_solver.clone(),
                ..Default::default()
            },
            security: resolve::resolve_security(self, Some(network), Some(&catalog))?,
        })
    }
}

#[cfg(test)]
fn strip_request_legacy_aliases(commitment: CommitmentMode) -> CommitmentMode {
    match commitment {
        CommitmentMode::Optimize(mut options) => {
            options.n_cost_segments = 0;
            options.step_size_hours = None;
            CommitmentMode::Optimize(options)
        }
        CommitmentMode::Additional {
            da_commitment,
            mut options,
        } => {
            options.n_cost_segments = 0;
            options.step_size_hours = None;
            CommitmentMode::Additional {
                da_commitment,
                options,
            }
        }
        other => other,
    }
}

#[cfg(not(test))]
fn strip_request_legacy_aliases(commitment: CommitmentMode) -> CommitmentMode {
    commitment
}

impl From<IntervalCoupling> for Horizon {
    fn from(value: IntervalCoupling) -> Self {
        match value {
            IntervalCoupling::PeriodByPeriod => Horizon::Sequential,
            IntervalCoupling::TimeCoupled => Horizon::TimeCoupled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::{DispatchableLoad, LoadArchetype, LoadCostModel};
    use surge_network::network::{Bus, BusType, Generator};

    fn single_generator_network() -> Network {
        let mut network = Network::default();
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        network.generators.push(Generator::new(1, 0.0, 1.0));
        network
    }

    fn two_generator_network() -> Network {
        let mut network = Network::default();
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        let mut generator_a = Generator::new(1, 0.0, 1.0);
        generator_a.id = "gen_a".to_string();
        let mut generator_b = Generator::new(1, 0.0, 1.0);
        generator_b.id = "gen_b".to_string();
        network.generators.push(generator_a);
        network.generators.push(generator_b);
        network
    }

    #[test]
    fn request_defaults_to_period_by_period_dc_dispatch() {
        let request = DispatchRequest::default();
        let normalized = request.normalize().expect("normalize request");
        assert_eq!(normalized.formulation, Formulation::Dc);
        assert_eq!(normalized.coupling, IntervalCoupling::PeriodByPeriod);
        assert!(matches!(
            normalized.commitment,
            CommitmentMode::AllCommitted
        ));
        assert!(normalized.security.is_none());
    }

    #[test]
    fn timeline_hourly_sets_hour_width() {
        let timeline = DispatchTimeline::hourly(24);
        assert_eq!(timeline.periods, 24);
        assert_eq!(timeline.interval_hours, 1.0);
        assert!(timeline.interval_hours_by_period.is_empty());
    }

    #[test]
    fn timeline_variable_sets_average_width_and_period_values() {
        let timeline = DispatchTimeline::variable(vec![0.25, 0.5, 1.0]);
        assert_eq!(timeline.periods, 3);
        assert_eq!(timeline.interval_hours, (0.25 + 0.5 + 1.0) / 3.0);
        assert_eq!(timeline.interval_hours_by_period, vec![0.25, 0.5, 1.0]);
    }

    #[test]
    fn request_builder_sets_expected_study_axes() {
        let dc_sced = DispatchRequest::builder()
            .dc()
            .period_by_period()
            .all_committed()
            .build();
        assert_eq!(dc_sced.formulation, Formulation::Dc);
        assert_eq!(dc_sced.coupling, IntervalCoupling::PeriodByPeriod);
        assert!(matches!(dc_sced.commitment, CommitmentPolicy::AllCommitted));

        let dc_sced_tc = DispatchRequest::builder()
            .dc()
            .time_coupled()
            .all_committed()
            .build();
        assert_eq!(dc_sced_tc.formulation, Formulation::Dc);
        assert_eq!(dc_sced_tc.coupling, IntervalCoupling::TimeCoupled);
        assert!(matches!(
            dc_sced_tc.commitment,
            CommitmentPolicy::AllCommitted
        ));

        let dc_scuc = DispatchRequest::builder()
            .dc()
            .time_coupled()
            .optimize_commitment(CommitmentOptions::default())
            .build();
        assert_eq!(dc_scuc.formulation, Formulation::Dc);
        assert_eq!(dc_scuc.coupling, IntervalCoupling::TimeCoupled);
        assert!(matches!(dc_scuc.commitment, CommitmentPolicy::Optimize(_)));

        let ac_sced = DispatchRequest::builder()
            .ac()
            .period_by_period()
            .all_committed()
            .build();
        assert_eq!(ac_sced.formulation, Formulation::Ac);
        assert_eq!(ac_sced.coupling, IntervalCoupling::PeriodByPeriod);
        assert!(matches!(ac_sced.commitment, CommitmentPolicy::AllCommitted));
    }

    #[test]
    fn request_validate_shape_rejects_invalid_axis_combinations() {
        let request = DispatchRequest::builder()
            .dc()
            .period_by_period()
            .optimize_commitment(CommitmentOptions::default())
            .build()
            .validate_shape()
            .expect_err("expected invalid time-coupling requirement");

        assert!(matches!(request, ScedError::InvalidInput(_)));
    }

    #[test]
    fn request_serializes_workflow_axes_in_snake_case() {
        let value = serde_json::to_value(
            DispatchRequest::builder()
                .dc()
                .time_coupled()
                .optimize_commitment(CommitmentOptions::default())
                .build(),
        )
        .expect("serialize dispatch request");

        assert_eq!(value["formulation"], "dc");
        assert_eq!(value["coupling"], "time_coupled");
        assert!(value["commitment"]["optimize"].is_object());
    }

    #[test]
    fn request_deserializes_with_defaults_for_missing_sections() {
        let request: DispatchRequest =
            serde_json::from_str("{}").expect("deserialize default dispatch request");

        assert_eq!(request.timeline.periods, 1);
        assert_eq!(request.timeline.interval_hours, 1.0);
        assert!(request.timeline.interval_hours_by_period.is_empty());
        assert_eq!(request.runtime.tolerance, 1e-8);
        assert!(matches!(request.commitment, CommitmentPolicy::AllCommitted));
        assert!(request.market.generator_offer_schedules.is_empty());
        assert!(request.market.generator_cost_modeling.is_none());
        assert!(request.network.thermal_limits.enforce);
    }

    #[test]
    fn request_deserializes_generator_cost_modeling() {
        let request: DispatchRequest = serde_json::from_str(
            r#"{
                "market": {
                    "generator_cost_modeling": {
                        "use_pwl_costs": true,
                        "pwl_cost_breakpoints": 12
                    }
                }
            }"#,
        )
        .expect("deserialize generator cost modeling");

        let modeling = request
            .market
            .generator_cost_modeling
            .expect("generator cost modeling");
        assert!(modeling.use_pwl_costs);
        assert_eq!(modeling.pwl_cost_breakpoints, 12);
    }

    #[test]
    fn fixed_schedule_validates_period_count() {
        let network = single_generator_network();
        let request = DispatchRequest {
            coupling: IntervalCoupling::TimeCoupled,
            commitment: CommitmentPolicy::Fixed(CommitmentSchedule {
                resources: vec![ResourceCommitmentSchedule {
                    resource_id: "gen:1:1".to_string(),
                    initial: true,
                    periods: Some(vec![true]),
                }],
            }),
            timeline: DispatchTimeline {
                periods: 2,
                ..DispatchTimeline::default()
            },
            ..DispatchRequest::default()
        };
        let err = request
            .resolve_with_options(&network, &DispatchSolveOptions::default())
            .expect_err("expected invalid schedule");
        assert!(matches!(err, ScedError::InvalidInput(_)));
    }

    #[test]
    fn request_rejects_unknown_fields() {
        let err = serde_json::from_str::<DispatchRequest>(
            r#"{"timelnie":{"periods":3},"timeline":{"perids":3}}"#,
        )
        .expect_err("expected unknown fields to be rejected");

        assert!(
            err.to_string().contains("unknown field"),
            "expected unknown field error, got {err}"
        );
    }

    #[test]
    fn ac_dispatch_warm_start_schedules_resolve_to_indexed_maps() {
        let network = single_generator_network();
        let request = DispatchRequest {
            formulation: Formulation::Ac,
            runtime: DispatchRuntime {
                ac_dispatch_warm_start: AcDispatchWarmStart {
                    buses: vec![BusPeriodVoltageSeries {
                        bus_number: 1,
                        vm_pu: vec![1.01, 1.02],
                        va_rad: vec![0.02, 0.03],
                    }],
                    generators: vec![ResourcePeriodPowerSeries {
                        resource_id: "gen:1:1".to_string(),
                        p_mw: vec![12.0, 15.0],
                        q_mvar: vec![3.0, 4.0],
                    }],
                    dispatchable_loads: vec![ResourcePeriodPowerSeries {
                        resource_id: "load_0".to_string(),
                        p_mw: vec![8.0, 6.0],
                        q_mvar: vec![2.0, 1.5],
                    }],
                    hvdc_links: Vec::new(),
                },
                ..DispatchRuntime::default()
            },
            timeline: DispatchTimeline::hourly(2),
            market: DispatchMarket {
                dispatchable_loads: vec![DispatchableLoad {
                    bus: 1,
                    p_sched_pu: 0.1,
                    q_sched_pu: 0.02,
                    p_min_pu: 0.0,
                    p_max_pu: 0.2,
                    q_min_pu: -0.1,
                    q_max_pu: 0.1,
                    archetype: LoadArchetype::IndependentPQ,
                    cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 500.0 },
                    fixed_power_factor: false,
                    in_service: true,
                    resource_id: "load_0".to_string(),
                    product_type: None,
                    dispatch_notification_minutes: 0.0,
                    min_duration_hours: 0.0,
                    baseline_mw: None,
                    rebound_fraction: 0.0,
                    rebound_periods: 0,
                    ramp_up_pu_per_hr: None,
                    ramp_down_pu_per_hr: None,
                    initial_p_pu: None,
                    ramp_group: None,
                    energy_offer: None,
                    reserve_offers: vec![],
                    reserve_group: None,
                    qualifications: Default::default(),
                    pq_linear_equality: None,
                    pq_linear_upper: None,
                    pq_linear_lower: None,
                }],
                ..DispatchMarket::default()
            },
            ..DispatchRequest::default()
        };

        let normalized = request
            .resolve_with_options(&network, &DispatchSolveOptions::default())
            .expect("resolve AC request with warm-start schedules");
        assert_eq!(
            normalized.input.ac_generator_warm_start_p_mw.get(&0),
            Some(&vec![12.0, 15.0])
        );
        assert_eq!(
            normalized.input.ac_generator_warm_start_q_mvar.get(&0),
            Some(&vec![3.0, 4.0])
        );
        assert_eq!(
            normalized.input.ac_bus_warm_start_vm_pu.get(&0),
            Some(&vec![1.01, 1.02])
        );
        assert_eq!(
            normalized.input.ac_bus_warm_start_va_rad.get(&0),
            Some(&vec![0.02, 0.03])
        );
        assert_eq!(
            normalized
                .input
                .ac_dispatchable_load_warm_start_p_mw
                .get(&0),
            Some(&vec![8.0, 6.0])
        );
        assert_eq!(
            normalized
                .input
                .ac_dispatchable_load_warm_start_q_mvar
                .get(&0),
            Some(&vec![2.0, 1.5])
        );
    }

    #[test]
    fn ac_runtime_relaxed_committed_pmin_normalizes_for_ac_requests() {
        let network = single_generator_network();
        let request = DispatchRequest {
            formulation: Formulation::Ac,
            runtime: DispatchRuntime {
                ac_relax_committed_pmin_to_zero: true,
                ..DispatchRuntime::default()
            },
            ..DispatchRequest::default()
        };

        let normalized = request
            .resolve_with_options(&network, &DispatchSolveOptions::default())
            .expect("resolve AC request with relaxed committed pmin runtime");
        assert!(normalized.input.ac_relax_committed_pmin_to_zero);
    }

    #[test]
    fn request_accepts_dispatchable_load_schedule_q_overrides() {
        let json = r#"{
            "market": {
                "dispatchable_loads": [
                    {
                        "resource_id": "load_0",
                        "bus": 1,
                        "p_sched_pu": 0.3,
                        "q_sched_pu": 0.06,
                        "p_min_pu": 0.0,
                        "p_max_pu": 0.3,
                        "q_min_pu": 0.0,
                        "q_max_pu": 0.2,
                        "archetype": "IndependentPQ",
                        "cost_model": {"LinearCurtailment": {"cost_per_mw": 500.0}},
                        "fixed_power_factor": false,
                        "in_service": true
                    }
                ],
                "dispatchable_load_offer_schedules": [
                    {
                        "resource_id": "load_0",
                        "schedule": {
                            "periods": [
                                {
                                    "p_sched_pu": 0.3,
                                    "p_max_pu": 0.3,
                                    "q_sched_pu": 0.06,
                                    "q_min_pu": 0.0,
                                    "q_max_pu": 0.2,
                                    "pq_linear_equality": {"q_at_p_zero_pu": 0.04, "beta": 0.3},
                                    "cost_model": {"LinearCurtailment": {"cost_per_mw": 500.0}}
                                }
                            ]
                        }
                    }
                ]
            }
        }"#;

        let request: DispatchRequest = serde_json::from_str(json)
            .expect("deserialize request with dispatchable-load q overrides");
        let period = request.market.dispatchable_load_offer_schedules[0]
            .schedule
            .periods[0]
            .as_ref()
            .expect("period params");
        assert_eq!(period.q_sched_pu, Some(0.06));
        assert_eq!(period.q_min_pu, Some(0.0));
        assert_eq!(period.q_max_pu, Some(0.2));
        let pq_link = period
            .pq_linear_equality
            .expect("dispatchable-load period override should accept pq link data");
        assert!((pq_link.q_at_p_zero_pu - 0.04).abs() < 1e-9);
        assert!((pq_link.beta - 0.3).abs() < 1e-9);
    }

    #[test]
    fn request_rejects_mismatched_interval_vector_length() {
        let err = DispatchRequest {
            timeline: DispatchTimeline {
                periods: 2,
                interval_hours: 1.0,
                interval_hours_by_period: vec![0.5],
            },
            ..DispatchRequest::default()
        }
        .normalize()
        .expect_err("expected invalid timeline");
        assert!(matches!(err, ScedError::InvalidInput(_)));
    }

    #[test]
    fn security_requires_dc_time_coupled() {
        let err = DispatchRequest {
            network: DispatchNetwork {
                security: Some(SecurityPolicy::default()),
                ..DispatchNetwork::default()
            },
            ..DispatchRequest::default()
        }
        .normalize()
        .expect_err("expected invalid security configuration");
        assert!(matches!(err, ScedError::InvalidInput(_)));
    }

    #[test]
    fn optimize_requires_time_coupled() {
        let err = DispatchRequest {
            commitment: CommitmentPolicy::Optimize(CommitmentOptions::default()),
            ..DispatchRequest::default()
        }
        .normalize()
        .expect_err("expected invalid optimize configuration");
        assert!(matches!(err, ScedError::InvalidInput(_)));
    }

    #[test]
    fn request_level_generator_cost_modeling_drives_scuc_pwl_configuration() {
        let default_request = DispatchRequest {
            coupling: IntervalCoupling::TimeCoupled,
            commitment: CommitmentPolicy::Optimize(CommitmentOptions::default()),
            ..DispatchRequest::default()
        };
        let default_normalized = default_request
            .normalize()
            .expect("normalize default scuc request");
        let default_spec = default_normalized.problem_spec();
        assert_eq!(default_spec.generator_pwl_cost_breakpoints(), None);

        let explicit_request = DispatchRequest {
            coupling: IntervalCoupling::TimeCoupled,
            commitment: CommitmentPolicy::Optimize(CommitmentOptions::default()),
            market: DispatchMarket {
                generator_cost_modeling: Some(GeneratorCostModeling {
                    use_pwl_costs: false,
                    pwl_cost_breakpoints: 12,
                }),
                ..DispatchMarket::default()
            },
            ..DispatchRequest::default()
        };
        let explicit_normalized = explicit_request
            .normalize()
            .expect("normalize explicit scuc request");
        let explicit_spec = explicit_normalized.problem_spec();
        assert_eq!(explicit_spec.generator_pwl_cost_breakpoints(), None);
        assert!(!explicit_spec.use_pwl_generator_costs());
    }

    #[test]
    fn sparse_previous_dispatch_preserves_unset_resources() {
        let network = two_generator_network();
        let request = DispatchRequest {
            state: DispatchState {
                initial: DispatchInitialState {
                    previous_resource_dispatch: vec![ResourceDispatchPoint {
                        resource_id: "gen_a".to_string(),
                        mw: 42.0,
                    }],
                    ..DispatchInitialState::default()
                },
            },
            ..DispatchRequest::default()
        };

        let normalized = request
            .resolve_with_options(&network, &DispatchSolveOptions::default())
            .expect("resolve sparse initial dispatch");

        assert!(normalized.input.initial_state.has_prev_dispatch());
        assert_eq!(
            normalized.input.initial_state.prev_dispatch_at(0),
            Some(42.0)
        );
        assert_eq!(normalized.input.initial_state.prev_dispatch_at(1), None);
    }

    #[test]
    fn sparse_commitment_initial_conditions_preserve_unset_resources() {
        let network = two_generator_network();
        let request = DispatchRequest {
            coupling: IntervalCoupling::TimeCoupled,
            commitment: CommitmentPolicy::Optimize(CommitmentOptions {
                initial_conditions: vec![CommitmentInitialCondition {
                    resource_id: "gen_a".to_string(),
                    committed: Some(false),
                    ..CommitmentInitialCondition::default()
                }],
                ..CommitmentOptions::default()
            }),
            ..DispatchRequest::default()
        };

        let normalized = request
            .resolve_with_options(&network, &DispatchSolveOptions::default())
            .expect("resolve sparse commitment state");

        let CommitmentMode::Optimize(options) = normalized.commitment else {
            panic!("expected optimize commitment mode");
        };
        assert_eq!(options.initial_commitment_at(0), Some(false));
        assert_eq!(options.initial_commitment_at(1), None);
        assert_eq!(options.initial_hours_on_at(1), None);
    }

    #[test]
    fn sparse_commitment_warm_start_preserves_unset_resources() {
        let network = two_generator_network();
        let request = DispatchRequest {
            coupling: IntervalCoupling::TimeCoupled,
            commitment: CommitmentPolicy::Optimize(CommitmentOptions {
                warm_start_commitment: vec![ResourcePeriodCommitment {
                    resource_id: "gen_a".to_string(),
                    periods: vec![false, true],
                }],
                ..CommitmentOptions::default()
            }),
            timeline: DispatchTimeline {
                periods: 2,
                ..DispatchTimeline::default()
            },
            ..DispatchRequest::default()
        };

        let normalized = request
            .resolve_with_options(&network, &DispatchSolveOptions::default())
            .expect("resolve sparse commitment warm start");

        let CommitmentMode::Optimize(options) = normalized.commitment else {
            panic!("expected optimize commitment mode");
        };
        assert_eq!(options.warm_start_commitment_at(0, 0), Some(false));
        assert_eq!(options.warm_start_commitment_at(1, 0), Some(true));
        assert_eq!(options.warm_start_commitment_at(0, 1), None);
        assert_eq!(options.warm_start_commitment_at(1, 1), None);
    }

    #[test]
    fn duplicate_previous_dispatch_is_rejected() {
        let network = single_generator_network();
        let request = DispatchRequest {
            state: DispatchState {
                initial: DispatchInitialState {
                    previous_resource_dispatch: vec![
                        ResourceDispatchPoint {
                            resource_id: "gen:1:1".to_string(),
                            mw: 10.0,
                        },
                        ResourceDispatchPoint {
                            resource_id: "gen:1:1".to_string(),
                            mw: 20.0,
                        },
                    ],
                    ..DispatchInitialState::default()
                },
            },
            ..DispatchRequest::default()
        };

        let err = request
            .resolve_with_options(&network, &DispatchSolveOptions::default())
            .expect_err("expected duplicate previous dispatch to fail");
        assert!(
            matches!(err, ScedError::InvalidInput(message) if message.contains("duplicate resource gen:1:1"))
        );
    }

    #[test]
    fn duplicate_commitment_initial_conditions_are_rejected() {
        let network = single_generator_network();
        let request = DispatchRequest {
            coupling: IntervalCoupling::TimeCoupled,
            commitment: CommitmentPolicy::Optimize(CommitmentOptions {
                initial_conditions: vec![
                    CommitmentInitialCondition {
                        resource_id: "gen:1:1".to_string(),
                        committed: Some(true),
                        ..CommitmentInitialCondition::default()
                    },
                    CommitmentInitialCondition {
                        resource_id: "gen:1:1".to_string(),
                        committed: Some(false),
                        ..CommitmentInitialCondition::default()
                    },
                ],
                ..CommitmentOptions::default()
            }),
            ..DispatchRequest::default()
        };

        let err = request
            .resolve_with_options(&network, &DispatchSolveOptions::default())
            .expect_err("expected duplicate commitment initial conditions to fail");
        assert!(
            matches!(err, ScedError::InvalidInput(message) if message.contains("duplicate resource gen:1:1"))
        );
    }

    #[test]
    fn duplicate_commitment_warm_start_resources_are_rejected() {
        let network = single_generator_network();
        let request = DispatchRequest {
            coupling: IntervalCoupling::TimeCoupled,
            commitment: CommitmentPolicy::Optimize(CommitmentOptions {
                warm_start_commitment: vec![
                    ResourcePeriodCommitment {
                        resource_id: "gen:1:1".to_string(),
                        periods: vec![true, true],
                    },
                    ResourcePeriodCommitment {
                        resource_id: "gen:1:1".to_string(),
                        periods: vec![false, false],
                    },
                ],
                ..CommitmentOptions::default()
            }),
            timeline: DispatchTimeline {
                periods: 2,
                ..DispatchTimeline::default()
            },
            ..DispatchRequest::default()
        };

        let err = request
            .resolve_with_options(&network, &DispatchSolveOptions::default())
            .expect_err("expected duplicate commitment warm start to fail");
        assert!(
            matches!(err, ScedError::InvalidInput(message) if message.contains("duplicate resource gen:1:1"))
        );
    }
}
