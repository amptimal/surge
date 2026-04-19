// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Borrowed immutable dispatch problem data shared across setup/build paths.

use std::collections::HashMap;
use std::sync::Arc;

use surge_network::market::{
    BranchDerateProfiles, DispatchableLoad, DlOfferSchedule, GeneratorDerateProfiles,
    HvdcDerateProfiles, LoadProfiles, OfferSchedule, PenaltyCurve, RampSharingConfig,
    RenewableProfiles, VirtualBid,
};
use surge_network::market::{ReserveProduct, SystemReserveRequirement, ZonalReserveRequirement};
use surge_opf::backends::{LpSolver, try_default_lp_solver};
use surge_solution::ParSetpoint;

use crate::config::emissions::MustRunUnits;
use crate::config::emissions::{CarbonPrice, EmissionProfile, TieLineLimits};
use crate::config::frequency::FrequencySecurityOptions;
use crate::dispatch::IndexedDispatchInitialState;
use crate::dispatch::{
    CommitmentMode, IndexedCommitmentConstraint, IndexedEnergyWindowLimit, IndexedPhHeadCurve,
    IndexedPhModeConstraint, IndexedStartupWindowLimit,
};
use crate::hvdc::HvdcDispatchLink;
#[cfg(test)]
use crate::legacy::DispatchOptions;
use crate::request::{
    AcBusLoadProfiles, AcDispatchTargetTracking, DispatchInput, GeneratorCostModeling,
    GeneratorDispatchBoundsProfiles, PowerBalancePenalty, RampMode, ReserveOfferSchedule,
    ScedAcBendersRuntime,
};

/// Shared solve clock for period-count and hour conversions.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DispatchClock<'a> {
    pub interval_hours: f64,
    pub period_hours: &'a [f64],
    pub period_hour_prefix: &'a [f64],
}

impl<'a> DispatchClock<'a> {
    pub fn new(
        interval_hours: f64,
        period_hours: &'a [f64],
        period_hour_prefix: &'a [f64],
    ) -> Self {
        Self {
            interval_hours,
            period_hours,
            period_hour_prefix,
        }
    }

    pub fn hours_to_periods_ceil(&self, hours: f64) -> usize {
        self.hours_to_periods_ceil_from(0, hours)
    }

    #[allow(dead_code)]
    pub fn hours_to_periods_ceil_uncapped(&self, hours: f64) -> usize {
        self.hours_to_periods_ceil_from_uncapped(0, hours)
    }

    pub fn hours_to_periods_ceil_from(&self, start_period: usize, hours: f64) -> usize {
        if !(hours.is_finite() && hours > 0.0) {
            return 0;
        }
        if self.period_hours.is_empty() {
            return (hours / self.interval_hours).ceil() as usize;
        }
        let mut covered = 0.0;
        for (offset, period_hours) in self.period_hours.iter().enumerate().skip(start_period) {
            covered += *period_hours;
            if covered + 1e-9 >= hours {
                return offset - start_period + 1;
            }
        }
        self.period_hours.len().saturating_sub(start_period)
    }

    pub fn hours_to_periods_ceil_from_uncapped(&self, start_period: usize, hours: f64) -> usize {
        if !(hours.is_finite() && hours > 0.0) {
            return 0;
        }
        if self.period_hours.is_empty() {
            return (hours / self.interval_hours).ceil() as usize;
        }
        let mut covered = 0.0;
        let mut periods = 0usize;
        for period_hours in self.period_hours.iter().skip(start_period) {
            covered += *period_hours;
            periods += 1;
            if covered + 1e-9 >= hours {
                return periods;
            }
        }
        let fallback_hours = self
            .period_hours
            .last()
            .copied()
            .unwrap_or(self.interval_hours)
            .max(1e-9);
        periods + ((hours - covered).max(0.0) / fallback_hours).ceil() as usize
    }

    pub fn period_hours(&self, period: usize) -> f64 {
        self.period_hours
            .get(period)
            .copied()
            .unwrap_or(self.interval_hours)
    }

    pub fn hours_between(&self, start_period: usize, end_period_exclusive: usize) -> f64 {
        if self.period_hour_prefix.is_empty() {
            return end_period_exclusive.saturating_sub(start_period) as f64 * self.interval_hours;
        }
        let start = start_period.min(self.period_hour_prefix.len().saturating_sub(1));
        let end = end_period_exclusive.min(self.period_hour_prefix.len().saturating_sub(1));
        (self.period_hour_prefix[end] - self.period_hour_prefix[start]).max(0.0)
    }

    pub fn period_start_hours(&self, period: usize) -> f64 {
        if self.period_hour_prefix.is_empty() {
            return period as f64 * self.interval_hours;
        }
        let idx = period.min(self.period_hour_prefix.len().saturating_sub(1));
        self.period_hour_prefix[idx]
    }

    pub fn period_end_hours(&self, period: usize) -> f64 {
        if self.period_hour_prefix.is_empty() {
            return (period + 1) as f64 * self.interval_hours;
        }
        let idx = (period + 1).min(self.period_hour_prefix.len().saturating_sub(1));
        self.period_hour_prefix[idx]
    }

    pub fn lookback_periods_covering(&self, end_period_inclusive: usize, hours: f64) -> usize {
        if !(hours.is_finite() && hours > 0.0) {
            return 0;
        }
        if self.period_hours.is_empty() {
            return (hours / self.interval_hours).ceil() as usize;
        }
        let mut covered = 0.0;
        let mut periods = 0usize;
        for period in
            (0..=end_period_inclusive.min(self.period_hours.len().saturating_sub(1))).rev()
        {
            covered += self.period_hours[period];
            periods += 1;
            if covered + 1e-9 >= hours {
                return periods;
            }
        }
        periods
    }
}

/// Period-indexed contingency case used by the explicit DC SCUC formulation.
///
/// These are internal bookkeeping objects that let the SCUC plan attach the
/// paper's post-contingency objective accounting (`z_tk`, worst-case, average)
/// to the explicit contingency flowgate rows generated by `scuc::security`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExplicitContingencyElement {
    Branch(usize),
    Hvdc(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExplicitContingencyCase {
    pub period: usize,
    pub element: ExplicitContingencyElement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExplicitContingencyFlowgate {
    pub case_index: usize,
    pub flowgate_idx: usize,
}

/// Narrow immutable view of the canonical dispatch problem.
///
/// This intentionally carries only the fields needed by shared pre-solve
/// setup and related helpers, so internal code can stop depending on the
/// entire test-only compatibility `DispatchOptions` bag.
#[derive(Clone, Copy)]
pub(crate) struct DispatchProblemSpec<'a> {
    pub commitment: &'a CommitmentMode,
    pub ramp_mode: &'a RampMode,
    /// When `true`, SCUC ramp inequality rows enforce ramp limits as
    /// hard constraints by pinning the slack columns to zero. The
    /// slack columns are still allocated (so the LP layout indices
    /// don't shift) but their upper bound becomes 0. Adapters that
    /// require hard ramps set this flag.
    pub ramp_constraints_hard: bool,
    /// When `true`, multi-interval energy-window slacks are pinned to
    /// zero so the window rows are enforced as hard constraints.
    pub energy_window_constraints_hard: bool,
    /// Penalty coefficient on multi-interval energy constraint
    /// violations in $/pu-h. When zero (the default), the slack
    /// columns absorb any violation for free. Adapters that want soft
    /// energy-window enforcement set this to the market's per-pu-hour
    /// violation price.
    pub energy_window_violation_per_puh: f64,
    /// When `true`, AC branch on/off binaries `u^on_jt` are free in
    /// `{0, 1}` and the security loop adds connectivity cuts against
    /// any solved switching pattern that disconnects the bus-branch
    /// graph. When `false` (the default), the branch_commitment
    /// columns are pinned to the network's static `in_service` flag
    /// and the LP behaves identically to the non-switching
    /// formulation.
    pub allow_branch_switching: bool,
    /// Accumulated bus-branch connectivity cuts from the SCUC
    /// security loop. Each entry forces
    /// `Σ branch_commitment[period, j] ≥ 1` over the cut set for
    /// the specified period. Empty by default; populated iteratively
    /// by `crate::scuc::security::solve_security_dispatch` when
    /// `allow_branch_switching = true` and a solved switching pattern
    /// disconnects the graph. Threaded through the next solve via
    /// [`Self::with_connectivity_cuts`], bounded at 5 rounds.
    pub connectivity_cuts: &'a [crate::scuc::connectivity::IndexedConnectivityCut],
    /// Period-indexed contingency cases for the explicit DC contingency
    /// objective. Empty unless the solve path has already expanded the
    /// contingency set into explicit SCUC flowgates.
    pub explicit_contingency_cases: &'a [ExplicitContingencyCase],
    /// Mapping from explicit contingency cases to the synthetic flowgates
    /// injected into `network.flowgates`. Empty unless explicit contingency
    /// mode is active.
    pub explicit_contingency_flowgates: &'a [ExplicitContingencyFlowgate],
    /// Compact post-contingency flow constraints (Option C path).
    /// When non-empty, the SCUC LP emits one row per entry directly,
    /// with its own post-hourly slack-column block — bypassing the
    /// ~500-byte-per-entry `surge_network::network::Flowgate` route.
    /// Populated by `solve_explicit_security_dispatch`. Empty when
    /// the caller is not doing explicit N-1, or when the old Flowgate
    /// path is being used.
    pub contingency_cuts: &'a [crate::common::contingency::ContingencyCut],
    /// Auxiliary array for `HvdcBanded` cuts in `contingency_cuts`:
    /// each such cut's `hvdc_band_range` indexes into this slice for
    /// its per-band (band_idx, coefficient) pairs. Empty when no
    /// cuts are `HvdcBanded`.
    pub contingency_cut_hvdc_band_coefs: &'a [(u32, f64)],
    /// Big-M factor used to linearize the switchable-branch flow
    /// definition `pf_l - b·Δθ ∈ [-M(1-u^on), M(1-u^on)]` in SCUC when
    /// `allow_branch_switching = true`. The effective Big-M per branch
    /// is `factor × fmax_branch` (in per-unit). Higher factors give the
    /// LP more freedom to move flow on OFF branches (where the bound
    /// must not bind the angles) but widen the LP relaxation and
    /// weaken the MIP. The default `10.0` matches the production OTS
    /// practice used by PowerWorld / GE PSLF for the same row family.
    pub branch_switching_big_m_factor: f64,
    pub clock: DispatchClock<'a>,
    pub n_periods: usize,
    pub dt_hours: f64,
    pub lp_solver: Option<&'a Arc<dyn LpSolver>>,
    pub tolerance: f64,
    pub run_pricing: bool,
    pub ac_relax_committed_pmin_to_zero: bool,
    pub enforce_thermal_limits: bool,
    pub min_rate_a: f64,
    pub enforce_shutdown_deloading: bool,
    pub offline_commitment_trajectories: bool,
    pub enforce_flowgates: bool,
    pub use_loss_factors: bool,
    pub max_nomogram_iter: usize,
    pub max_loss_factor_iters: usize,
    pub loss_factor_tol: f64,
    pub enforce_forbidden_zones: bool,
    pub foz_max_transit_periods: Option<usize>,
    pub load_profiles: &'a LoadProfiles,
    pub ac_bus_load_profiles: &'a AcBusLoadProfiles,
    pub renewable_profiles: &'a RenewableProfiles,
    pub gen_derate_profiles: &'a GeneratorDerateProfiles,
    pub generator_dispatch_bounds: &'a GeneratorDispatchBoundsProfiles,
    pub branch_derate_profiles: &'a BranchDerateProfiles,
    pub hvdc_derate_profiles: &'a HvdcDerateProfiles,
    pub storage_self_schedules: Option<&'a HashMap<usize, Vec<f64>>>,
    pub storage_reserve_soc_impact: &'a HashMap<usize, HashMap<String, Vec<f64>>>,
    pub par_setpoints: &'a [ParSetpoint],
    pub offer_schedules: &'a HashMap<usize, OfferSchedule>,
    pub dl_offer_schedules: &'a HashMap<usize, DlOfferSchedule>,
    pub gen_reserve_offer_schedules: &'a HashMap<usize, ReserveOfferSchedule>,
    pub dl_reserve_offer_schedules: &'a HashMap<usize, ReserveOfferSchedule>,
    pub cc_config_offers: &'a [Vec<OfferSchedule>],
    pub hvdc_links: &'a [HvdcDispatchLink],
    pub ph_head_curves: &'a [IndexedPhHeadCurve],
    pub ph_mode_constraints: &'a [IndexedPhModeConstraint],
    pub startup_window_limits: &'a [IndexedStartupWindowLimit],
    pub energy_window_limits: &'a [IndexedEnergyWindowLimit],
    pub commitment_constraints: &'a [IndexedCommitmentConstraint],
    pub dispatchable_loads: &'a [DispatchableLoad],
    pub virtual_bids: &'a [VirtualBid],
    pub ac_generator_warm_start_p_mw: &'a HashMap<usize, Vec<f64>>,
    pub ac_generator_warm_start_q_mvar: &'a HashMap<usize, Vec<f64>>,
    pub ac_bus_warm_start_vm_pu: &'a HashMap<usize, Vec<f64>>,
    pub ac_bus_warm_start_va_rad: &'a HashMap<usize, Vec<f64>>,
    pub ac_dispatchable_load_warm_start_p_mw: &'a HashMap<usize, Vec<f64>>,
    pub ac_dispatchable_load_warm_start_q_mvar: &'a HashMap<usize, Vec<f64>>,
    pub fixed_hvdc_dispatch_mw: &'a HashMap<usize, Vec<f64>>,
    pub fixed_hvdc_dispatch_q_fr_mvar: &'a HashMap<usize, Vec<f64>>,
    pub fixed_hvdc_dispatch_q_to_mvar: &'a HashMap<usize, Vec<f64>>,
    pub ac_hvdc_warm_start_p_mw: &'a HashMap<usize, Vec<f64>>,
    pub ac_hvdc_warm_start_q_fr_mvar: &'a HashMap<usize, Vec<f64>>,
    pub ac_hvdc_warm_start_q_to_mvar: &'a HashMap<usize, Vec<f64>>,
    pub ac_target_tracking: &'a AcDispatchTargetTracking,
    /// SCED-AC Benders decomposition runtime: per-period eta variable
    /// activation flags and the accumulated optimality cut pool. The SCED
    /// row builder reads `cuts_for_period(t)` and adds one LP row per cut,
    /// plus an `eta[t]` epigraph variable in the column layout when the
    /// period is in `eta_periods`. Default-constructed (empty) for callers
    /// that do not opt in to Benders.
    pub sced_ac_benders: &'a ScedAcBendersRuntime,
    pub tie_line_limits: Option<&'a TieLineLimits>,
    pub frequency_security: &'a FrequencySecurityOptions,
    pub reserve_products: &'a [ReserveProduct],
    pub system_reserve_requirements: &'a [SystemReserveRequirement],
    pub zonal_reserve_requirements: &'a [ZonalReserveRequirement],
    pub must_run_units: Option<&'a MustRunUnits>,
    pub regulation_eligible: Option<&'a [bool]>,
    pub ramp_sharing: &'a RampSharingConfig,
    pub generator_area: &'a [usize],
    pub load_area: &'a [usize],
    pub thermal_penalty_curve: &'a PenaltyCurve,
    pub reserve_penalty_curve: &'a PenaltyCurve,
    pub ramp_penalty_curve: &'a PenaltyCurve,
    pub angle_penalty_curve: &'a PenaltyCurve,
    pub power_balance_penalty: &'a PowerBalancePenalty,
    pub co2_cap_t: Option<f64>,
    pub emission_profile: Option<&'a EmissionProfile>,
    pub carbon_price: Option<CarbonPrice>,
    pub co2_price_per_t: f64,
    pub generator_cost_modeling: Option<&'a GeneratorCostModeling>,
    pub initial_state: &'a IndexedDispatchInitialState,
    /// When true, capture a [`crate::model_diagnostic::ModelDiagnostic`]
    /// snapshot after each LP/MIP solve.
    pub capture_model_diagnostics: bool,
    /// Per-period AC SCED concurrency. See
    /// [`crate::request::DispatchRuntime::ac_sced_period_concurrency`] for
    /// semantics.
    pub ac_sced_period_concurrency: Option<usize>,
}

/// Period-local resolved view over a horizon-wide [`DispatchProblemSpec`].
#[derive(Clone, Copy)]
pub(crate) struct DispatchPeriodSpec<'a> {
    spec: &'a DispatchProblemSpec<'a>,
    pub period: usize,
}

impl<'a> DispatchProblemSpec<'a> {
    #[cfg(test)]
    pub fn from_options(options: &'a DispatchOptions) -> Self {
        let dt_hours = commitment_step_size_hours(&options.commitment).unwrap_or(options.dt_hours);
        Self::from_request_parts(
            &options.commitment,
            &options.ramp_mode,
            options.ramp_constraints_hard,
            options.energy_window_constraints_hard,
            options.energy_window_violation_per_puh,
            options.allow_branch_switching,
            options.branch_switching_big_m_factor,
            &[],
            &[],
            &[],
            options.n_periods,
            dt_hours,
            &[],
            &[],
            options.lp_solver.as_ref(),
            options.tolerance,
            options.run_pricing,
            options.ac_relax_committed_pmin_to_zero,
            options.enforce_thermal_limits,
            options.min_rate_a,
            options.enforce_shutdown_deloading,
            options.offline_commitment_trajectories,
            options.enforce_flowgates,
            options.use_loss_factors,
            options.max_nomogram_iter,
            options.max_loss_factor_iters,
            options.loss_factor_tol,
            options.enforce_forbidden_zones,
            options.foz_max_transit_periods,
            &options.load_profiles,
            &options.ac_bus_load_profiles,
            &options.renewable_profiles,
            &options.gen_derate_profiles,
            &options.generator_dispatch_bounds,
            &options.branch_derate_profiles,
            &options.hvdc_derate_profiles,
            options.storage_self_schedules.as_ref(),
            &options.storage_reserve_soc_impact,
            &options.par_setpoints,
            &options.offer_schedules,
            &options.dl_offer_schedules,
            &options.gen_reserve_offer_schedules,
            &options.dl_reserve_offer_schedules,
            &options.cc_config_offers,
            &options.hvdc_links,
            &options.ph_head_curves,
            &options.ph_mode_constraints,
            &options.startup_window_limits,
            &options.energy_window_limits,
            &options.commitment_constraints,
            &options.dispatchable_loads,
            &options.virtual_bids,
            &options.ac_generator_warm_start_p_mw,
            &options.ac_generator_warm_start_q_mvar,
            &options.ac_bus_warm_start_vm_pu,
            &options.ac_bus_warm_start_va_rad,
            &options.ac_dispatchable_load_warm_start_p_mw,
            &options.ac_dispatchable_load_warm_start_q_mvar,
            &options.fixed_hvdc_dispatch_mw,
            &options.fixed_hvdc_dispatch_q_fr_mvar,
            &options.fixed_hvdc_dispatch_q_to_mvar,
            &options.ac_hvdc_warm_start_p_mw,
            &options.ac_hvdc_warm_start_q_fr_mvar,
            &options.ac_hvdc_warm_start_q_to_mvar,
            &options.ac_target_tracking,
            &options.sced_ac_benders,
            options.tie_line_limits.as_ref(),
            &options.frequency_security,
            &options.reserve_products,
            &options.system_reserve_requirements,
            &options.zonal_reserve_requirements,
            options.must_run_units.as_ref(),
            options.regulation_eligible.as_deref(),
            &options.ramp_sharing,
            &options.generator_area,
            &options.load_area,
            &options.penalty_config.thermal,
            &options.penalty_config.reserve,
            &options.penalty_config.ramp,
            &options.penalty_config.angle,
            &options.power_balance_penalty,
            options.co2_cap_t,
            options.emission_profile.as_ref(),
            options.carbon_price,
            options.co2_price_per_t,
            options.generator_cost_modeling.as_ref(),
            &options.initial_state,
            false,
            None,
        )
    }

    pub fn from_request(input: &'a DispatchInput, commitment: &'a CommitmentMode) -> Self {
        Self::from_request_parts(
            commitment,
            &input.ramp_mode,
            input.ramp_constraints_hard,
            input.energy_window_constraints_hard,
            input.energy_window_violation_per_puh,
            input.allow_branch_switching,
            input.branch_switching_big_m_factor,
            &[],
            &[],
            &[],
            input.n_periods,
            input.dt_hours,
            &input.period_hours,
            &input.period_hour_prefix,
            input.lp_solver.as_ref(),
            input.tolerance,
            input.run_pricing,
            input.ac_relax_committed_pmin_to_zero,
            input.enforce_thermal_limits,
            input.min_rate_a,
            input.enforce_shutdown_deloading,
            input.offline_commitment_trajectories,
            input.enforce_flowgates,
            input.use_loss_factors,
            input.max_nomogram_iter,
            input.max_loss_factor_iters,
            input.loss_factor_tol,
            input.enforce_forbidden_zones,
            input.foz_max_transit_periods,
            &input.load_profiles,
            &input.ac_bus_load_profiles,
            &input.renewable_profiles,
            &input.gen_derate_profiles,
            &input.generator_dispatch_bounds,
            &input.branch_derate_profiles,
            &input.hvdc_derate_profiles,
            input.storage_self_schedules.as_ref(),
            &input.storage_reserve_soc_impact,
            &input.par_setpoints,
            &input.offer_schedules,
            &input.dl_offer_schedules,
            &input.gen_reserve_offer_schedules,
            &input.dl_reserve_offer_schedules,
            &input.cc_config_offers,
            &input.hvdc_links,
            &input.ph_head_curves,
            &input.ph_mode_constraints,
            &input.startup_window_limits,
            &input.energy_window_limits,
            &input.commitment_constraints,
            &input.dispatchable_loads,
            &input.virtual_bids,
            &input.ac_generator_warm_start_p_mw,
            &input.ac_generator_warm_start_q_mvar,
            &input.ac_bus_warm_start_vm_pu,
            &input.ac_bus_warm_start_va_rad,
            &input.ac_dispatchable_load_warm_start_p_mw,
            &input.ac_dispatchable_load_warm_start_q_mvar,
            &input.fixed_hvdc_dispatch_mw,
            &input.fixed_hvdc_dispatch_q_fr_mvar,
            &input.fixed_hvdc_dispatch_q_to_mvar,
            &input.ac_hvdc_warm_start_p_mw,
            &input.ac_hvdc_warm_start_q_fr_mvar,
            &input.ac_hvdc_warm_start_q_to_mvar,
            &input.ac_target_tracking,
            &input.sced_ac_benders,
            input.tie_line_limits.as_ref(),
            &input.frequency_security,
            &input.reserve_products,
            &input.system_reserve_requirements,
            &input.zonal_reserve_requirements,
            input.must_run_units.as_ref(),
            input.regulation_eligible.as_deref(),
            &input.ramp_sharing,
            &input.generator_area,
            &input.load_area,
            &input.penalty_config.thermal,
            &input.penalty_config.reserve,
            &input.penalty_config.ramp,
            &input.penalty_config.angle,
            &input.power_balance_penalty,
            input.co2_cap_t,
            input.emission_profile.as_ref(),
            input.carbon_price,
            input.co2_price_per_t,
            input.generator_cost_modeling.as_ref(),
            &input.initial_state,
            input.capture_model_diagnostics,
            input.ac_sced_period_concurrency,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_request_parts(
        commitment: &'a CommitmentMode,
        ramp_mode: &'a RampMode,
        ramp_constraints_hard: bool,
        energy_window_constraints_hard: bool,
        energy_window_violation_per_puh: f64,
        allow_branch_switching: bool,
        branch_switching_big_m_factor: f64,
        connectivity_cuts: &'a [crate::scuc::connectivity::IndexedConnectivityCut],
        explicit_contingency_cases: &'a [ExplicitContingencyCase],
        explicit_contingency_flowgates: &'a [ExplicitContingencyFlowgate],
        n_periods: usize,
        dt_hours: f64,
        period_hours: &'a [f64],
        period_hour_prefix: &'a [f64],
        lp_solver: Option<&'a Arc<dyn LpSolver>>,
        tolerance: f64,
        run_pricing: bool,
        ac_relax_committed_pmin_to_zero: bool,
        enforce_thermal_limits: bool,
        min_rate_a: f64,
        enforce_shutdown_deloading: bool,
        offline_commitment_trajectories: bool,
        enforce_flowgates: bool,
        use_loss_factors: bool,
        max_nomogram_iter: usize,
        max_loss_factor_iters: usize,
        loss_factor_tol: f64,
        enforce_forbidden_zones: bool,
        foz_max_transit_periods: Option<usize>,
        load_profiles: &'a LoadProfiles,
        ac_bus_load_profiles: &'a AcBusLoadProfiles,
        renewable_profiles: &'a RenewableProfiles,
        gen_derate_profiles: &'a GeneratorDerateProfiles,
        generator_dispatch_bounds: &'a GeneratorDispatchBoundsProfiles,
        branch_derate_profiles: &'a BranchDerateProfiles,
        hvdc_derate_profiles: &'a HvdcDerateProfiles,
        storage_self_schedules: Option<&'a HashMap<usize, Vec<f64>>>,
        storage_reserve_soc_impact: &'a HashMap<usize, HashMap<String, Vec<f64>>>,
        par_setpoints: &'a [ParSetpoint],
        offer_schedules: &'a HashMap<usize, OfferSchedule>,
        dl_offer_schedules: &'a HashMap<usize, DlOfferSchedule>,
        gen_reserve_offer_schedules: &'a HashMap<usize, ReserveOfferSchedule>,
        dl_reserve_offer_schedules: &'a HashMap<usize, ReserveOfferSchedule>,
        cc_config_offers: &'a [Vec<OfferSchedule>],
        hvdc_links: &'a [HvdcDispatchLink],
        ph_head_curves: &'a [IndexedPhHeadCurve],
        ph_mode_constraints: &'a [IndexedPhModeConstraint],
        startup_window_limits: &'a [IndexedStartupWindowLimit],
        energy_window_limits: &'a [IndexedEnergyWindowLimit],
        commitment_constraints: &'a [IndexedCommitmentConstraint],
        dispatchable_loads: &'a [DispatchableLoad],
        virtual_bids: &'a [VirtualBid],
        ac_generator_warm_start_p_mw: &'a HashMap<usize, Vec<f64>>,
        ac_generator_warm_start_q_mvar: &'a HashMap<usize, Vec<f64>>,
        ac_bus_warm_start_vm_pu: &'a HashMap<usize, Vec<f64>>,
        ac_bus_warm_start_va_rad: &'a HashMap<usize, Vec<f64>>,
        ac_dispatchable_load_warm_start_p_mw: &'a HashMap<usize, Vec<f64>>,
        ac_dispatchable_load_warm_start_q_mvar: &'a HashMap<usize, Vec<f64>>,
        fixed_hvdc_dispatch_mw: &'a HashMap<usize, Vec<f64>>,
        fixed_hvdc_dispatch_q_fr_mvar: &'a HashMap<usize, Vec<f64>>,
        fixed_hvdc_dispatch_q_to_mvar: &'a HashMap<usize, Vec<f64>>,
        ac_hvdc_warm_start_p_mw: &'a HashMap<usize, Vec<f64>>,
        ac_hvdc_warm_start_q_fr_mvar: &'a HashMap<usize, Vec<f64>>,
        ac_hvdc_warm_start_q_to_mvar: &'a HashMap<usize, Vec<f64>>,
        ac_target_tracking: &'a AcDispatchTargetTracking,
        sced_ac_benders: &'a ScedAcBendersRuntime,
        tie_line_limits: Option<&'a TieLineLimits>,
        frequency_security: &'a FrequencySecurityOptions,
        reserve_products: &'a [ReserveProduct],
        system_reserve_requirements: &'a [SystemReserveRequirement],
        zonal_reserve_requirements: &'a [ZonalReserveRequirement],
        must_run_units: Option<&'a MustRunUnits>,
        regulation_eligible: Option<&'a [bool]>,
        ramp_sharing: &'a RampSharingConfig,
        generator_area: &'a [usize],
        load_area: &'a [usize],
        thermal_penalty_curve: &'a PenaltyCurve,
        reserve_penalty_curve: &'a PenaltyCurve,
        ramp_penalty_curve: &'a PenaltyCurve,
        angle_penalty_curve: &'a PenaltyCurve,
        power_balance_penalty: &'a PowerBalancePenalty,
        co2_cap_t: Option<f64>,
        emission_profile: Option<&'a EmissionProfile>,
        carbon_price: Option<CarbonPrice>,
        co2_price_per_t: f64,
        generator_cost_modeling: Option<&'a GeneratorCostModeling>,
        initial_state: &'a IndexedDispatchInitialState,
        capture_model_diagnostics: bool,
        ac_sced_period_concurrency: Option<usize>,
    ) -> Self {
        let clock = DispatchClock::new(dt_hours, period_hours, period_hour_prefix);
        Self {
            commitment,
            ramp_mode,
            ramp_constraints_hard,
            energy_window_constraints_hard,
            energy_window_violation_per_puh,
            allow_branch_switching,
            connectivity_cuts,
            explicit_contingency_cases,
            explicit_contingency_flowgates,
            contingency_cuts: &[],
            contingency_cut_hvdc_band_coefs: &[],
            branch_switching_big_m_factor,
            clock,
            n_periods,
            dt_hours,
            lp_solver,
            tolerance,
            run_pricing,
            ac_relax_committed_pmin_to_zero,
            enforce_thermal_limits,
            min_rate_a,
            enforce_shutdown_deloading,
            offline_commitment_trajectories,
            enforce_flowgates,
            use_loss_factors,
            max_nomogram_iter,
            max_loss_factor_iters,
            loss_factor_tol,
            enforce_forbidden_zones,
            foz_max_transit_periods,
            load_profiles,
            ac_bus_load_profiles,
            renewable_profiles,
            gen_derate_profiles,
            generator_dispatch_bounds,
            branch_derate_profiles,
            hvdc_derate_profiles,
            storage_self_schedules,
            storage_reserve_soc_impact,
            par_setpoints,
            offer_schedules,
            dl_offer_schedules,
            gen_reserve_offer_schedules,
            dl_reserve_offer_schedules,
            cc_config_offers,
            hvdc_links,
            ph_head_curves,
            ph_mode_constraints,
            startup_window_limits,
            energy_window_limits,
            commitment_constraints,
            dispatchable_loads,
            virtual_bids,
            ac_generator_warm_start_p_mw,
            ac_generator_warm_start_q_mvar,
            ac_bus_warm_start_vm_pu,
            ac_bus_warm_start_va_rad,
            ac_dispatchable_load_warm_start_p_mw,
            ac_dispatchable_load_warm_start_q_mvar,
            fixed_hvdc_dispatch_mw,
            fixed_hvdc_dispatch_q_fr_mvar,
            fixed_hvdc_dispatch_q_to_mvar,
            ac_hvdc_warm_start_p_mw,
            ac_hvdc_warm_start_q_fr_mvar,
            ac_hvdc_warm_start_q_to_mvar,
            ac_target_tracking,
            sced_ac_benders,
            tie_line_limits,
            frequency_security,
            reserve_products,
            system_reserve_requirements,
            zonal_reserve_requirements,
            must_run_units,
            regulation_eligible,
            ramp_sharing,
            generator_area,
            load_area,
            thermal_penalty_curve,
            reserve_penalty_curve,
            ramp_penalty_curve,
            angle_penalty_curve,
            power_balance_penalty,
            co2_cap_t,
            emission_profile,
            carbon_price,
            co2_price_per_t,
            generator_cost_modeling,
            initial_state,
            capture_model_diagnostics,
            ac_sced_period_concurrency,
        }
    }

    pub fn is_block_mode(&self) -> bool {
        matches!(self.ramp_mode, RampMode::Block { .. })
    }

    pub fn has_per_block_reserves(&self) -> bool {
        matches!(
            self.ramp_mode,
            RampMode::Block {
                per_block_reserves: true
            }
        )
    }

    pub fn period(&'a self, period: usize) -> DispatchPeriodSpec<'a> {
        DispatchPeriodSpec { spec: self, period }
    }

    pub fn with_commitment<'b>(&'b self, commitment: &'b CommitmentMode) -> DispatchProblemSpec<'b>
    where
        'a: 'b,
    {
        DispatchProblemSpec {
            commitment,
            ramp_mode: self.ramp_mode,
            ramp_constraints_hard: self.ramp_constraints_hard,
            energy_window_constraints_hard: self.energy_window_constraints_hard,
            energy_window_violation_per_puh: self.energy_window_violation_per_puh,
            allow_branch_switching: self.allow_branch_switching,
            connectivity_cuts: self.connectivity_cuts,
            explicit_contingency_cases: self.explicit_contingency_cases,
            explicit_contingency_flowgates: self.explicit_contingency_flowgates,
            contingency_cuts: self.contingency_cuts,
            contingency_cut_hvdc_band_coefs: self.contingency_cut_hvdc_band_coefs,
            branch_switching_big_m_factor: self.branch_switching_big_m_factor,
            clock: self.clock,
            n_periods: self.n_periods,
            dt_hours: self.dt_hours,
            lp_solver: self.lp_solver,
            tolerance: self.tolerance,
            run_pricing: self.run_pricing,
            ac_relax_committed_pmin_to_zero: self.ac_relax_committed_pmin_to_zero,
            enforce_thermal_limits: self.enforce_thermal_limits,
            min_rate_a: self.min_rate_a,
            enforce_shutdown_deloading: self.enforce_shutdown_deloading,
            offline_commitment_trajectories: self.offline_commitment_trajectories,
            enforce_flowgates: self.enforce_flowgates,
            use_loss_factors: self.use_loss_factors,
            max_nomogram_iter: self.max_nomogram_iter,
            max_loss_factor_iters: self.max_loss_factor_iters,
            loss_factor_tol: self.loss_factor_tol,
            enforce_forbidden_zones: self.enforce_forbidden_zones,
            foz_max_transit_periods: self.foz_max_transit_periods,
            load_profiles: self.load_profiles,
            ac_bus_load_profiles: self.ac_bus_load_profiles,
            renewable_profiles: self.renewable_profiles,
            gen_derate_profiles: self.gen_derate_profiles,
            generator_dispatch_bounds: self.generator_dispatch_bounds,
            branch_derate_profiles: self.branch_derate_profiles,
            hvdc_derate_profiles: self.hvdc_derate_profiles,
            storage_self_schedules: self.storage_self_schedules,
            storage_reserve_soc_impact: self.storage_reserve_soc_impact,
            par_setpoints: self.par_setpoints,
            offer_schedules: self.offer_schedules,
            dl_offer_schedules: self.dl_offer_schedules,
            gen_reserve_offer_schedules: self.gen_reserve_offer_schedules,
            dl_reserve_offer_schedules: self.dl_reserve_offer_schedules,
            cc_config_offers: self.cc_config_offers,
            hvdc_links: self.hvdc_links,
            ph_head_curves: self.ph_head_curves,
            ph_mode_constraints: self.ph_mode_constraints,
            startup_window_limits: self.startup_window_limits,
            energy_window_limits: self.energy_window_limits,
            commitment_constraints: self.commitment_constraints,
            dispatchable_loads: self.dispatchable_loads,
            virtual_bids: self.virtual_bids,
            ac_generator_warm_start_p_mw: self.ac_generator_warm_start_p_mw,
            ac_generator_warm_start_q_mvar: self.ac_generator_warm_start_q_mvar,
            ac_bus_warm_start_vm_pu: self.ac_bus_warm_start_vm_pu,
            ac_bus_warm_start_va_rad: self.ac_bus_warm_start_va_rad,
            ac_dispatchable_load_warm_start_p_mw: self.ac_dispatchable_load_warm_start_p_mw,
            ac_dispatchable_load_warm_start_q_mvar: self.ac_dispatchable_load_warm_start_q_mvar,
            fixed_hvdc_dispatch_mw: self.fixed_hvdc_dispatch_mw,
            fixed_hvdc_dispatch_q_fr_mvar: self.fixed_hvdc_dispatch_q_fr_mvar,
            fixed_hvdc_dispatch_q_to_mvar: self.fixed_hvdc_dispatch_q_to_mvar,
            ac_hvdc_warm_start_p_mw: self.ac_hvdc_warm_start_p_mw,
            ac_hvdc_warm_start_q_fr_mvar: self.ac_hvdc_warm_start_q_fr_mvar,
            ac_hvdc_warm_start_q_to_mvar: self.ac_hvdc_warm_start_q_to_mvar,
            ac_target_tracking: self.ac_target_tracking,
            sced_ac_benders: self.sced_ac_benders,
            tie_line_limits: self.tie_line_limits,
            frequency_security: self.frequency_security,
            reserve_products: self.reserve_products,
            system_reserve_requirements: self.system_reserve_requirements,
            zonal_reserve_requirements: self.zonal_reserve_requirements,
            must_run_units: self.must_run_units,
            regulation_eligible: self.regulation_eligible,
            ramp_sharing: self.ramp_sharing,
            generator_area: self.generator_area,
            load_area: self.load_area,
            thermal_penalty_curve: self.thermal_penalty_curve,
            reserve_penalty_curve: self.reserve_penalty_curve,
            ramp_penalty_curve: self.ramp_penalty_curve,
            angle_penalty_curve: self.angle_penalty_curve,
            power_balance_penalty: self.power_balance_penalty,
            co2_cap_t: self.co2_cap_t,
            emission_profile: self.emission_profile,
            carbon_price: self.carbon_price,
            co2_price_per_t: self.co2_price_per_t,
            generator_cost_modeling: self.generator_cost_modeling,
            initial_state: self.initial_state,
            capture_model_diagnostics: self.capture_model_diagnostics,
            ac_sced_period_concurrency: self.ac_sced_period_concurrency,
        }
    }

    /// Return a new `DispatchProblemSpec` that references the supplied
    /// `sced_ac_benders` runtime in place of `self.sced_ac_benders`. All
    /// other fields are preserved verbatim. Used by the SCED-AC Benders
    /// orchestration loop to thread an evolving cut pool into each
    /// successive master solve without cloning the entire spec (which
    /// would defeat the purpose of `DispatchProblemSpec` being a cheap
    /// borrowed view over the input request).
    /// Return a new `DispatchProblemSpec` that references `cuts` as
    /// the connectivity cut pool. Used by the SCUC security loop to
    /// thread an evolving cut accumulator through each successive MIP
    /// re-solve without cloning the entire spec.
    pub fn with_connectivity_cuts<'b>(
        &'b self,
        cuts: &'b [crate::scuc::connectivity::IndexedConnectivityCut],
    ) -> DispatchProblemSpec<'b>
    where
        'a: 'b,
    {
        let mut next = DispatchProblemSpec { ..*self };
        next.connectivity_cuts = cuts;
        next
    }

    /// Return a new `DispatchProblemSpec` that references the supplied
    /// explicit contingency metadata. This is used by the explicit-security
    /// SCUC path after it materializes the contingency flowgates on the
    /// runtime network.
    pub fn with_explicit_contingencies<'b>(
        &'b self,
        cases: &'b [ExplicitContingencyCase],
        flowgates: &'b [ExplicitContingencyFlowgate],
    ) -> DispatchProblemSpec<'b>
    where
        'a: 'b,
    {
        let mut next = DispatchProblemSpec { ..*self };
        next.explicit_contingency_cases = cases;
        next.explicit_contingency_flowgates = flowgates;
        next
    }

    pub fn with_sced_ac_benders<'b>(
        &'b self,
        sced_ac_benders: &'b crate::request::ScedAcBendersRuntime,
    ) -> DispatchProblemSpec<'b>
    where
        'a: 'b,
    {
        DispatchProblemSpec {
            commitment: self.commitment,
            ramp_mode: self.ramp_mode,
            ramp_constraints_hard: self.ramp_constraints_hard,
            energy_window_constraints_hard: self.energy_window_constraints_hard,
            energy_window_violation_per_puh: self.energy_window_violation_per_puh,
            allow_branch_switching: self.allow_branch_switching,
            connectivity_cuts: self.connectivity_cuts,
            explicit_contingency_cases: self.explicit_contingency_cases,
            explicit_contingency_flowgates: self.explicit_contingency_flowgates,
            contingency_cuts: self.contingency_cuts,
            contingency_cut_hvdc_band_coefs: self.contingency_cut_hvdc_band_coefs,
            branch_switching_big_m_factor: self.branch_switching_big_m_factor,
            clock: self.clock,
            n_periods: self.n_periods,
            dt_hours: self.dt_hours,
            lp_solver: self.lp_solver,
            tolerance: self.tolerance,
            run_pricing: self.run_pricing,
            ac_relax_committed_pmin_to_zero: self.ac_relax_committed_pmin_to_zero,
            enforce_thermal_limits: self.enforce_thermal_limits,
            min_rate_a: self.min_rate_a,
            enforce_shutdown_deloading: self.enforce_shutdown_deloading,
            offline_commitment_trajectories: self.offline_commitment_trajectories,
            enforce_flowgates: self.enforce_flowgates,
            use_loss_factors: self.use_loss_factors,
            max_nomogram_iter: self.max_nomogram_iter,
            max_loss_factor_iters: self.max_loss_factor_iters,
            loss_factor_tol: self.loss_factor_tol,
            enforce_forbidden_zones: self.enforce_forbidden_zones,
            foz_max_transit_periods: self.foz_max_transit_periods,
            load_profiles: self.load_profiles,
            ac_bus_load_profiles: self.ac_bus_load_profiles,
            renewable_profiles: self.renewable_profiles,
            gen_derate_profiles: self.gen_derate_profiles,
            generator_dispatch_bounds: self.generator_dispatch_bounds,
            branch_derate_profiles: self.branch_derate_profiles,
            hvdc_derate_profiles: self.hvdc_derate_profiles,
            storage_self_schedules: self.storage_self_schedules,
            storage_reserve_soc_impact: self.storage_reserve_soc_impact,
            par_setpoints: self.par_setpoints,
            offer_schedules: self.offer_schedules,
            dl_offer_schedules: self.dl_offer_schedules,
            gen_reserve_offer_schedules: self.gen_reserve_offer_schedules,
            dl_reserve_offer_schedules: self.dl_reserve_offer_schedules,
            cc_config_offers: self.cc_config_offers,
            hvdc_links: self.hvdc_links,
            ph_head_curves: self.ph_head_curves,
            ph_mode_constraints: self.ph_mode_constraints,
            startup_window_limits: self.startup_window_limits,
            energy_window_limits: self.energy_window_limits,
            commitment_constraints: self.commitment_constraints,
            dispatchable_loads: self.dispatchable_loads,
            virtual_bids: self.virtual_bids,
            ac_generator_warm_start_p_mw: self.ac_generator_warm_start_p_mw,
            ac_generator_warm_start_q_mvar: self.ac_generator_warm_start_q_mvar,
            ac_bus_warm_start_vm_pu: self.ac_bus_warm_start_vm_pu,
            ac_bus_warm_start_va_rad: self.ac_bus_warm_start_va_rad,
            ac_dispatchable_load_warm_start_p_mw: self.ac_dispatchable_load_warm_start_p_mw,
            ac_dispatchable_load_warm_start_q_mvar: self.ac_dispatchable_load_warm_start_q_mvar,
            fixed_hvdc_dispatch_mw: self.fixed_hvdc_dispatch_mw,
            fixed_hvdc_dispatch_q_fr_mvar: self.fixed_hvdc_dispatch_q_fr_mvar,
            fixed_hvdc_dispatch_q_to_mvar: self.fixed_hvdc_dispatch_q_to_mvar,
            ac_hvdc_warm_start_p_mw: self.ac_hvdc_warm_start_p_mw,
            ac_hvdc_warm_start_q_fr_mvar: self.ac_hvdc_warm_start_q_fr_mvar,
            ac_hvdc_warm_start_q_to_mvar: self.ac_hvdc_warm_start_q_to_mvar,
            ac_target_tracking: self.ac_target_tracking,
            sced_ac_benders,
            tie_line_limits: self.tie_line_limits,
            frequency_security: self.frequency_security,
            reserve_products: self.reserve_products,
            system_reserve_requirements: self.system_reserve_requirements,
            zonal_reserve_requirements: self.zonal_reserve_requirements,
            must_run_units: self.must_run_units,
            regulation_eligible: self.regulation_eligible,
            ramp_sharing: self.ramp_sharing,
            generator_area: self.generator_area,
            load_area: self.load_area,
            thermal_penalty_curve: self.thermal_penalty_curve,
            reserve_penalty_curve: self.reserve_penalty_curve,
            ramp_penalty_curve: self.ramp_penalty_curve,
            angle_penalty_curve: self.angle_penalty_curve,
            power_balance_penalty: self.power_balance_penalty,
            co2_cap_t: self.co2_cap_t,
            emission_profile: self.emission_profile,
            carbon_price: self.carbon_price,
            co2_price_per_t: self.co2_price_per_t,
            generator_cost_modeling: self.generator_cost_modeling,
            initial_state: self.initial_state,
            capture_model_diagnostics: self.capture_model_diagnostics,
            ac_sced_period_concurrency: self.ac_sced_period_concurrency,
        }
    }

    #[allow(dead_code)]
    pub fn initial_commitment(&self) -> Option<&'a [bool]> {
        match self.commitment {
            CommitmentMode::Fixed { commitment, .. } => Some(commitment),
            CommitmentMode::Optimize(options) => options.initial_commitment.as_deref(),
            CommitmentMode::Additional { options, .. } => options.initial_commitment.as_deref(),
            CommitmentMode::AllCommitted => None,
        }
    }

    pub fn initial_commitment_at(&self, gen_idx: usize) -> Option<bool> {
        match self.commitment {
            CommitmentMode::Fixed { commitment, .. } => commitment.get(gen_idx).copied(),
            CommitmentMode::Optimize(options) => options.initial_commitment_at(gen_idx),
            CommitmentMode::Additional { options, .. } => options.initial_commitment_at(gen_idx),
            CommitmentMode::AllCommitted => None,
        }
    }

    #[allow(dead_code)]
    pub fn initial_offline_hours(&self) -> Option<&'a [f64]> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_offline_hours.as_deref(),
            CommitmentMode::Additional { options, .. } => options.initial_offline_hours.as_deref(),
            _ => None,
        }
    }

    pub fn initial_offline_hours_at(&self, gen_idx: usize) -> Option<f64> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_offline_hours_at(gen_idx),
            CommitmentMode::Additional { options, .. } => options.initial_offline_hours_at(gen_idx),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn initial_hours_on(&self) -> Option<&'a [i32]> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_hours_on.as_deref(),
            CommitmentMode::Additional { options, .. } => options.initial_hours_on.as_deref(),
            _ => None,
        }
    }

    pub fn initial_hours_on_at(&self, gen_idx: usize) -> Option<i32> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_hours_on_at(gen_idx),
            CommitmentMode::Additional { options, .. } => options.initial_hours_on_at(gen_idx),
            _ => None,
        }
    }

    pub fn warm_start_commitment_at(&self, period: usize, gen_idx: usize) -> Option<bool> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.warm_start_commitment_at(period, gen_idx),
            CommitmentMode::Additional { options, .. } => {
                options.warm_start_commitment_at(period, gen_idx)
            }
            _ => None,
        }
    }

    pub fn initial_online_hours_at(&self, gen_idx: usize) -> Option<f64> {
        self.initial_hours_on_at(gen_idx)
            .map(|hours| hours.max(0) as f64)
    }

    pub fn additional_commitment_at(&self, period: usize, gen_idx: usize) -> bool {
        match self.commitment {
            CommitmentMode::Additional { da_commitment, .. } => da_commitment
                .get(period)
                .and_then(|schedule| schedule.get(gen_idx))
                .copied()
                .unwrap_or(false),
            _ => false,
        }
    }

    pub fn additional_commitment_prefix_through(&self, period: usize, gen_idx: usize) -> bool {
        if self.initial_commitment_at(gen_idx) != Some(true) {
            return false;
        }
        (0..=period).all(|hour| self.additional_commitment_at(hour, gen_idx))
    }

    #[allow(dead_code)]
    pub fn initial_starts_24h(&self) -> Option<&'a [u32]> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_starts_24h.as_deref(),
            CommitmentMode::Additional { options, .. } => options.initial_starts_24h.as_deref(),
            _ => None,
        }
    }

    pub fn initial_starts_24h_at(&self, gen_idx: usize) -> Option<u32> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_starts_24h_at(gen_idx),
            CommitmentMode::Additional { options, .. } => options.initial_starts_24h_at(gen_idx),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn initial_starts_168h(&self) -> Option<&'a [u32]> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_starts_168h.as_deref(),
            CommitmentMode::Additional { options, .. } => options.initial_starts_168h.as_deref(),
            _ => None,
        }
    }

    pub fn initial_starts_168h_at(&self, gen_idx: usize) -> Option<u32> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_starts_168h_at(gen_idx),
            CommitmentMode::Additional { options, .. } => options.initial_starts_168h_at(gen_idx),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn initial_energy_mwh_24h(&self) -> Option<&'a [f64]> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_energy_mwh_24h.as_deref(),
            CommitmentMode::Additional { options, .. } => options.initial_energy_mwh_24h.as_deref(),
            _ => None,
        }
    }

    pub fn initial_energy_mwh_24h_at(&self, gen_idx: usize) -> Option<f64> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.initial_energy_mwh_24h_at(gen_idx),
            CommitmentMode::Additional { options, .. } => {
                options.initial_energy_mwh_24h_at(gen_idx)
            }
            _ => None,
        }
    }

    pub fn time_limit_secs(&self) -> Option<f64> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.time_limit_secs,
            CommitmentMode::Additional { options, .. } => options.time_limit_secs,
            _ => None,
        }
    }

    pub fn mip_rel_gap(&self) -> Option<f64> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.mip_rel_gap,
            CommitmentMode::Additional { options, .. } => options.mip_rel_gap,
            _ => None,
        }
    }

    pub fn mip_gap_schedule(&self) -> Option<&[(f64, f64)]> {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.mip_gap_schedule.as_deref(),
            CommitmentMode::Additional { options, .. } => options.mip_gap_schedule.as_deref(),
            _ => None,
        }
    }

    pub fn disable_warm_start(&self) -> bool {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.disable_warm_start,
            CommitmentMode::Additional { options, .. } => options.disable_warm_start,
            _ => false,
        }
    }

    #[cfg(test)]
    pub fn n_cost_segments(&self) -> usize {
        match self.commitment {
            CommitmentMode::Optimize(options) => options.n_cost_segments,
            CommitmentMode::Additional { options, .. } => options.n_cost_segments,
            _ => 0,
        }
    }

    pub fn generator_pwl_cost_breakpoints(&self) -> Option<usize> {
        if let Some(modeling) = self.generator_cost_modeling {
            return modeling
                .use_pwl_costs
                .then_some(modeling.pwl_cost_breakpoints.max(2));
        }

        #[cfg(not(test))]
        {
            None
        }
        #[cfg(test)]
        let legacy_segments = self.n_cost_segments();
        #[cfg(test)]
        {
            (legacy_segments > 0).then_some((legacy_segments + 1).max(2))
        }
    }

    pub fn use_pwl_generator_costs(&self) -> bool {
        self.generator_pwl_cost_breakpoints().is_some()
    }

    #[allow(dead_code)]
    pub fn interval_hours(&self) -> f64 {
        self.clock.interval_hours
    }

    pub fn period_hours(&self, period: usize) -> f64 {
        self.clock.period_hours(period)
    }

    pub fn hours_to_periods_ceil(&self, hours: f64) -> usize {
        self.clock.hours_to_periods_ceil(hours)
    }

    #[allow(dead_code)]
    pub fn hours_to_periods_ceil_uncapped(&self, hours: f64) -> usize {
        self.clock.hours_to_periods_ceil_uncapped(hours)
    }

    pub fn hours_to_periods_ceil_from(&self, start_period: usize, hours: f64) -> usize {
        self.clock.hours_to_periods_ceil_from(start_period, hours)
    }

    pub fn hours_to_periods_ceil_from_uncapped(&self, start_period: usize, hours: f64) -> usize {
        self.clock
            .hours_to_periods_ceil_from_uncapped(start_period, hours)
    }

    pub fn hours_between(&self, start_period: usize, end_period_exclusive: usize) -> f64 {
        self.clock.hours_between(start_period, end_period_exclusive)
    }

    pub fn period_start_hours(&self, period: usize) -> f64 {
        self.clock.period_start_hours(period)
    }

    pub fn period_end_hours(&self, period: usize) -> f64 {
        self.clock.period_end_hours(period)
    }

    pub fn lookback_periods_covering(&self, end_period_inclusive: usize, hours: f64) -> usize {
        self.clock
            .lookback_periods_covering(end_period_inclusive, hours)
    }

    pub fn resolve_lp_solver(&self) -> Arc<dyn LpSolver> {
        self.lp_solver
            .cloned()
            .unwrap_or_else(|| try_default_lp_solver().expect("no LP solver available"))
    }

    pub fn prev_dispatch_mw(&self) -> Option<&'a [f64]> {
        self.initial_state.prev_dispatch_mw.as_deref()
    }

    pub fn has_prev_dispatch(&self) -> bool {
        self.initial_state.has_prev_dispatch()
    }

    #[allow(dead_code)]
    pub fn prev_dispatch_mw_at(&self, gen_idx: usize) -> Option<f64> {
        self.initial_state.prev_dispatch_at(gen_idx)
    }

    #[allow(dead_code)]
    pub fn prev_hvdc_dispatch_mw_at(&self, link_idx: usize) -> Option<f64> {
        self.initial_state.prev_hvdc_dispatch_at(link_idx)
    }

    pub fn ac_generator_warm_start_p_mw_at(&self, period: usize, gen_idx: usize) -> Option<f64> {
        self.ac_generator_warm_start_p_mw
            .get(&gen_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    pub fn ac_generator_warm_start_q_mvar_at(&self, period: usize, gen_idx: usize) -> Option<f64> {
        self.ac_generator_warm_start_q_mvar
            .get(&gen_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    pub fn ac_bus_warm_start_vm_pu_at(&self, period: usize, bus_idx: usize) -> Option<f64> {
        self.ac_bus_warm_start_vm_pu
            .get(&bus_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    pub fn ac_bus_warm_start_va_rad_at(&self, period: usize, bus_idx: usize) -> Option<f64> {
        self.ac_bus_warm_start_va_rad
            .get(&bus_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    pub fn ac_dispatchable_load_warm_start_p_mw_at(
        &self,
        period: usize,
        dl_idx: usize,
    ) -> Option<f64> {
        self.ac_dispatchable_load_warm_start_p_mw
            .get(&dl_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    pub fn ac_dispatchable_load_warm_start_q_mvar_at(
        &self,
        period: usize,
        dl_idx: usize,
    ) -> Option<f64> {
        self.ac_dispatchable_load_warm_start_q_mvar
            .get(&dl_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    pub fn ac_hvdc_warm_start_p_mw_at(&self, period: usize, link_idx: usize) -> Option<f64> {
        self.ac_hvdc_warm_start_p_mw
            .get(&link_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    #[allow(dead_code)]
    pub fn ac_hvdc_warm_start_q_fr_mvar_at(&self, period: usize, link_idx: usize) -> Option<f64> {
        self.ac_hvdc_warm_start_q_fr_mvar
            .get(&link_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    #[allow(dead_code)]
    pub fn ac_hvdc_warm_start_q_to_mvar_at(&self, period: usize, link_idx: usize) -> Option<f64> {
        self.ac_hvdc_warm_start_q_to_mvar
            .get(&link_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    pub fn fixed_hvdc_dispatch_mw_at(&self, period: usize, link_idx: usize) -> Option<f64> {
        self.fixed_hvdc_dispatch_mw
            .get(&link_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    pub fn fixed_hvdc_dispatch_q_fr_mvar_at(&self, period: usize, link_idx: usize) -> Option<f64> {
        self.fixed_hvdc_dispatch_q_fr_mvar
            .get(&link_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }

    pub fn fixed_hvdc_dispatch_q_to_mvar_at(&self, period: usize, link_idx: usize) -> Option<f64> {
        self.fixed_hvdc_dispatch_q_to_mvar
            .get(&link_idx)
            .and_then(|periods| periods.get(period).or_else(|| periods.last()).copied())
    }
}

#[cfg(test)]
fn commitment_step_size_hours(commitment: &CommitmentMode) -> Option<f64> {
    match commitment {
        CommitmentMode::Optimize(options) => options.step_size_hours,
        CommitmentMode::Additional { options, .. } => options.step_size_hours,
        _ => None,
    }
}

impl<'a> DispatchPeriodSpec<'a> {
    pub fn interval_hours(&self) -> f64 {
        self.spec.period_hours(self.period)
    }

    pub fn fixed_commitment(&self) -> Option<&'a [bool]> {
        match self.spec.commitment {
            CommitmentMode::Fixed {
                commitment,
                per_period,
            } => Some(
                per_period
                    .as_ref()
                    .and_then(|rows| rows.get(self.period))
                    .map(Vec::as_slice)
                    .unwrap_or(commitment.as_slice()),
            ),
            _ => None,
        }
    }

    pub fn is_committed(&self, gen_idx: usize) -> bool {
        self.fixed_commitment()
            .and_then(|commitment| commitment.get(gen_idx))
            .copied()
            .unwrap_or(true)
    }

    pub fn next_fixed_commitment(&self) -> Option<&'a [bool]> {
        match self.spec.commitment {
            CommitmentMode::Fixed {
                commitment,
                per_period,
            } if self.period + 1 < self.spec.n_periods => Some(
                per_period
                    .as_ref()
                    .and_then(|rows| rows.get(self.period + 1))
                    .map(Vec::as_slice)
                    .unwrap_or(commitment.as_slice()),
            ),
            _ => None,
        }
    }

    pub fn storage_self_schedule_mw(&self, gen_index: usize) -> Option<f64> {
        self.spec
            .storage_self_schedules
            .and_then(|schedules| schedules.get(&gen_index))
            .and_then(|periods| periods.get(self.period).or_else(|| periods.last()).copied())
    }

    pub fn storage_reserve_soc_impact(&self, gen_index: usize, product_id: &str) -> f64 {
        self.spec
            .storage_reserve_soc_impact
            .get(&gen_index)
            .and_then(|products| products.get(product_id))
            .and_then(|periods| periods.get(self.period).or_else(|| periods.last()).copied())
            .unwrap_or(0.0)
    }

    #[allow(dead_code)]
    pub fn system_requirement_mw(&self, requirement: &SystemReserveRequirement) -> f64 {
        requirement.requirement_mw_for_period(self.period)
    }
}
