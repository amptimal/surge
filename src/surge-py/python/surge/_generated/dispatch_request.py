# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
# ============================================================================
# AUTO-GENERATED — DO NOT EDIT.
# Regenerate with `python3 scripts/codegen_dispatch_request.py`.
# Source of truth: surge_dispatch::DispatchRequest (see
# `src/surge-dispatch/src/request.rs`). The Rust types derive
# ``schemars::JsonSchema``; this module mirrors that schema as Python
# ``TypedDict`` / ``Literal`` aliases for IDE + type-checker support.
# ============================================================================
"""Typed view of the ``DispatchRequest`` dict accepted by ``surge.solve_dispatch``.

Fields whose Rust type lives in another crate (``surge_network::market::*``,
``surge_opf::AcOpfOptions``, ``surge_solution::ParSetpoint``) are typed as
``dict[str, Any]`` / ``list[dict[str, Any]]`` because the schema treats
them as opaque JSON values. Build those payloads with the helpers in
``surge.market`` (``GeneratorOfferSchedule``, ``ReserveProductDef``,
``build_reserve_products_dict``, ...).
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict, Union


class _AcBusLoadProfileRequired(TypedDict):
    bus_number: int

class AcBusLoadProfile(_AcBusLoadProfileRequired, total=False):
    """Optional AC-only bus load override profile.  This augments the standard
active-power-only [`BusLoadProfiles`] surface:  - `p_mw = Some(..)`, `q_mvar =
None`: override active load and preserve the base bus reactive power factor. -
`p_mw = None`, `q_mvar = Some(..)`: keep active load and override only
reactive demand. - `p_mw = Some(..)`, `q_mvar = Some(..)`: override both
active and reactive bus demand explicitly.
"""
    p_mw: Union[list[float], None]
    q_mvar: Union[list[float], None]


class AcBusLoadProfiles(TypedDict, total=False):
    """Collection of AC-only bus load overrides."""
    profiles: list[AcBusLoadProfile]


class AcDispatchTargetTrackingPair(TypedDict, total=False):
    """Asymmetric quadratic penalty pair for one generator or dispatchable load in
the AC-OPF target-tracking term.  The tracking objective applies
`upward_per_mw2 * max(0, p - target)² + downward_per_mw2 * max(0, target -
p)²`. Symmetric behaviour (the legacy case) can be encoded by setting both
fields to the same value.
"""
    upward_per_mw2: float
    downward_per_mw2: float


class AcDispatchTargetTracking(TypedDict, total=False):
    generator_p_penalty_per_mw2: float
    generator_p_coefficients_default: AcDispatchTargetTrackingPair
    generator_p_coefficients_overrides_by_id: dict[str, AcDispatchTargetTrackingPair]
    dispatchable_load_p_penalty_per_mw2: float
    dispatchable_load_p_coefficients_default: AcDispatchTargetTrackingPair
    dispatchable_load_p_coefficients_overrides_by_id: dict[str, AcDispatchTargetTrackingPair]


class BusPeriodVoltageSeries(TypedDict):
    bus_number: int
    vm_pu: list[float]
    va_rad: list[float]


class _HvdcPeriodPowerSeriesRequired(TypedDict):
    link_id: str
    p_mw: list[float]

class HvdcPeriodPowerSeries(_HvdcPeriodPowerSeriesRequired, total=False):
    q_fr_mvar: list[float]
    q_to_mvar: list[float]


class _ResourcePeriodPowerSeriesRequired(TypedDict):
    resource_id: str
    p_mw: list[float]

class ResourcePeriodPowerSeries(_ResourcePeriodPowerSeriesRequired, total=False):
    q_mvar: list[float]


class AcDispatchWarmStart(TypedDict, total=False):
    buses: list[BusPeriodVoltageSeries]
    generators: list[ResourcePeriodPowerSeries]
    dispatchable_loads: list[ResourcePeriodPowerSeries]
    hvdc_links: list[HvdcPeriodPowerSeries]


class _BranchDerateProfileRequired(TypedDict):
    derate_factors: list[float]
    from_bus: int
    to_bus: int

class BranchDerateProfile(_BranchDerateProfileRequired, total=False):
    """Branch derate profile keyed by a stable branch selector."""
    circuit: str


class BranchDerateProfiles(TypedDict, total=False):
    """Collection of branch derate profiles."""
    profiles: list[BranchDerateProfile]


class _BranchRefRequired(TypedDict):
    from_bus: int
    to_bus: int

class BranchRef(_BranchRefRequired, total=False):
    """Stable branch selector."""
    circuit: str


class BusAreaAssignment(TypedDict):
    """Assign an area id to one bus."""
    bus_number: int
    area_id: int


class BusLoadProfile(TypedDict):
    """Active-power load profile for one bus."""
    bus_number: int
    values_mw: list[float]


class BusLoadProfiles(TypedDict, total=False):
    """Collection of active-power bus load profiles."""
    profiles: list[BusLoadProfile]


class CarbonPrice(TypedDict):
    """Carbon price in $/tCO2.  Combined with [`EmissionProfile`] (or the inline
`co2_rate_t_per_mwh` generator field), this adds an emission surcharge to each
generator's dispatch cost:  ```text carbon_cost_g = pg_mw_g * hours * rate_g *
price_per_tonne ```
"""
    price_per_tonne: float


class CombinedCycleConfigOfferSchedule(TypedDict):
    """Combined-cycle config offer override."""
    plant_id: str
    config_name: str
    schedule: Any


class CommitmentTerm(TypedDict):
    """One linear commitment-cut term keyed by resource id."""
    resource_id: str
    coeff: float


class _CommitmentConstraintRequired(TypedDict):
    name: str
    period_idx: int
    terms: list[CommitmentTerm]
    lower_bound: float

class CommitmentConstraint(_CommitmentConstraintRequired, total=False):
    """Public commitment constraint keyed by resource ids."""
    penalty_cost: Union[float, None]


class CommitmentInitialCondition(TypedDict, total=False):
    """Initial commitment metadata for one resource."""
    resource_id: str
    committed: Union[bool, None]
    hours_on: Union[int, None]
    offline_hours: Union[float, None]
    starts_24h: Union[int, None]
    starts_168h: Union[int, None]
    energy_mwh_24h: Union[float, None]


class ResourcePeriodCommitment(TypedDict):
    """Minimum commitment floor for one resource across the solved periods."""
    resource_id: str
    periods: list[bool]


class CommitmentOptions(TypedDict, total=False):
    """Public commitment optimization controls."""
    initial_conditions: list[CommitmentInitialCondition]
    warm_start_commitment: list[ResourcePeriodCommitment]
    time_limit_secs: Union[float, None]
    mip_rel_gap: Union[float, None]
    mip_gap_schedule: Union[list[tuple[float, float]], None]
    disable_warm_start: bool


class _ResourceCommitmentScheduleRequired(TypedDict):
    resource_id: str
    initial: bool

class ResourceCommitmentSchedule(_ResourceCommitmentScheduleRequired, total=False):
    """Fixed commitment schedule for one resource."""
    periods: Union[list[bool], None]


class CommitmentSchedule(TypedDict, total=False):
    """Fixed commitment schedule for dispatch-only studies."""
    resources: list[ResourceCommitmentSchedule]


class CommitmentPolicy_Variant0(TypedDict):
    """Commitment is provided externally."""
    fixed: CommitmentSchedule
class CommitmentPolicy_Variant1(TypedDict):
    """Optimize commitment endogenously."""
    optimize: CommitmentOptions
class CommitmentPolicy_Variant2(TypedDict):
    """Lock day-ahead commitments on and optimize only additional units."""
    additional: dict[str, Any]
# Public commitment policy for dispatch studies.
CommitmentPolicy = Union[Literal['all_committed'], CommitmentPolicy_Variant0, CommitmentPolicy_Variant1, CommitmentPolicy_Variant2]


# How startup/shutdown output trajectories are modeled across intervals.
CommitmentTrajectoryMode = Literal['inline_deloading', 'offline_trajectory']


class CommitmentTransitionPolicy(TypedDict, total=False):
    """Commitment transition modeling policy."""
    shutdown_deloading: bool
    trajectory_mode: CommitmentTrajectoryMode


# Whether a constraint family is enforced as hard or soft.
ConstraintEnforcement = Literal['soft', 'hard']


class HvdcDispatchPoint(TypedDict):
    """Previous dispatch point for one HVDC link."""
    link_id: str
    mw: float


class ResourceDispatchPoint(TypedDict):
    """Previous dispatch point for one resource."""
    resource_id: str
    mw: float


class StorageSocOverride(TypedDict):
    """Initial storage state override for one storage resource."""
    resource_id: str
    soc_mwh: float


class DispatchInitialState(TypedDict, total=False):
    """Initial dispatch state for sequential or horizon-start solves."""
    previous_resource_dispatch: list[ResourceDispatchPoint]
    previous_hvdc_dispatch: list[HvdcDispatchPoint]
    storage_soc_overrides: list[StorageSocOverride]


class DispatchableLoadOfferSchedule(TypedDict):
    """Per-resource offer schedule override for a dispatchable load."""
    resource_id: str
    schedule: Any


class ReserveOfferSchedule(TypedDict):
    """Per-resource reserve offer override schedule."""
    periods: list[list[Any]]


class DispatchableLoadReserveOfferSchedule(TypedDict):
    """Per-resource reserve offer schedule override for a dispatchable load."""
    resource_id: str
    schedule: ReserveOfferSchedule


class ResourceEmissionRate(TypedDict):
    """Per-resource emission rate."""
    resource_id: str
    rate_tonnes_per_mwh: float


class EmissionProfile(TypedDict, total=False):
    """Public keyed emission profile."""
    resources: list[ResourceEmissionRate]


class FrequencySecurityOptions(TypedDict, total=False):
    """Frequency security constraints to embed in the SCED/SCUC LP.  Each field is
optional — `None` means "do not add this constraint." All `None` (the default)
produces zero additional LP rows.
"""
    min_inertia_mws: Union[float, None]
    max_rocof_hz_per_s: Union[float, None]
    min_pfr_mw: Union[float, None]
    generator_h_values: list[float]
    freq_event_mw: float
    min_nadir_hz: float
    largest_contingency_mw: float
    base_frequency_hz: float


class GeneratorCostModeling(TypedDict, total=False):
    """Generator cost approximation controls shared across dispatch formulations.
Explicit piecewise-linear curves always use their native epiograph
representation. These options control whether convex polynomial generator
costs should be outer-linearized into the same PWL form.
"""
    use_pwl_costs: bool
    pwl_cost_breakpoints: int


class GeneratorOfferSchedule(TypedDict):
    """Per-resource offer schedule override for a generator or storage resource."""
    resource_id: str
    schedule: Any


class GeneratorReserveOfferSchedule(TypedDict):
    """Per-resource reserve offer schedule override for a generator or storage resource."""
    resource_id: str
    schedule: ReserveOfferSchedule


class MustRunUnits(TypedDict, total=False):
    """Public keyed must-run floor list."""
    resource_ids: list[str]


class PowerBalancePenalty(TypedDict, total=False):
    """Stepped penalty curves for power balance slack variables."""
    curtailment: list[tuple[float, float]]
    excess: list[tuple[float, float]]


class ResourceAreaAssignment(TypedDict):
    """Assign an area id to one resource."""
    resource_id: str
    area_id: int


class ResourceEligibility(TypedDict):
    """Boolean eligibility override for one resource."""
    resource_id: str
    eligible: bool


class ResourceEnergyWindowLimit(TypedDict, total=False):
    """Absolute energy budget for one resource over a horizon window.  The window is
inclusive on both ends. Either or both bounds may be set.
"""
    resource_id: str
    start_period_idx: int
    end_period_idx: int
    min_energy_mwh: Union[float, None]
    max_energy_mwh: Union[float, None]


class ResourceStartupWindowLimit(TypedDict):
    """Absolute startup-count limit for one resource over a horizon window.  The
window is inclusive on both ends: `start_period_idx=0, end_period_idx=23`
constrains the first 24 solved periods.
"""
    resource_id: str
    start_period_idx: int
    end_period_idx: int
    max_startups: int


class StoragePowerSchedule(TypedDict):
    """Per-storage self schedule."""
    resource_id: str
    values_mw: list[float]


class StorageReserveSocImpact(TypedDict):
    """Per-storage reserve SOC impact profile."""
    resource_id: str
    product_id: str
    values_mwh_per_mw: list[float]


class TieLineLimits(TypedDict):
    """Inter-area transfer limits for multi-area dispatch.  When provided in a
canonical dispatch request, an additional LP constraint is added per
`(from_area, to_area)` pair per hour on the physical interchange across AC
branches and dispatchable HVDC links whose terminal buses lie in those two
areas:  ```text net_transfer[from → to] = Σ AC branch flow crossing from_area
to to_area + Σ dispatchable HVDC transfer from_area to to_area
net_transfer[from → to] ≤ limit_mw[(from_area, to_area)] ```  Area assignment
uses `load_area[b]` (index into bus list). Generator-area tags are not used
for these interface rows.  `limits_mw` is directional: a positive entry
`limits_mw[(0,1)] = 200.0` means area 0 may export at most 200 MW to area 1.
The reverse flow `limits_mw[(1,0)]` is a separate entry.
"""
    limits_mw: list[tuple[tuple[int, int], float]]


class DispatchMarket(TypedDict, total=False):
    """Market inputs and market-facing policy for dispatch."""
    reserve_products: list[Any]
    system_reserve_requirements: list[Any]
    zonal_reserve_requirements: list[Any]
    ramp_sharing: Any
    co2_cap_t: Union[float, None]
    co2_price_per_t: float
    emission_profile: Union[EmissionProfile, None]
    carbon_price: Union[CarbonPrice, None]
    storage_self_schedules: list[StoragePowerSchedule]
    storage_reserve_soc_impacts: list[StorageReserveSocImpact]
    generator_offer_schedules: list[GeneratorOfferSchedule]
    dispatchable_load_offer_schedules: list[DispatchableLoadOfferSchedule]
    generator_reserve_offer_schedules: list[GeneratorReserveOfferSchedule]
    dispatchable_load_reserve_offer_schedules: list[DispatchableLoadReserveOfferSchedule]
    combined_cycle_offer_schedules: list[CombinedCycleConfigOfferSchedule]
    tie_line_limits: Union[TieLineLimits, None]
    resource_area_assignments: list[ResourceAreaAssignment]
    bus_area_assignments: list[BusAreaAssignment]
    must_run_units: Union[MustRunUnits, None]
    frequency_security: FrequencySecurityOptions
    dispatchable_loads: list[Any]
    virtual_bids: list[Any]
    power_balance_penalty: PowerBalancePenalty
    penalty_config: Any
    generator_cost_modeling: Union[GeneratorCostModeling, None]
    regulation_eligibility: list[ResourceEligibility]
    startup_window_limits: list[ResourceStartupWindowLimit]
    energy_window_limits: list[ResourceEnergyWindowLimit]
    commitment_constraints: list[CommitmentConstraint]


class EnergyWindowPolicy(TypedDict, total=False):
    """Multi-interval energy-window enforcement policy."""
    enforcement: ConstraintEnforcement
    penalty_per_puh: float


class FlowgatePolicy(TypedDict, total=False):
    """Flowgate/interface enforcement policy."""
    enabled: bool
    max_nomogram_iterations: int


class ForbiddenZonePolicy(TypedDict, total=False):
    """Forbidden-operating-zone enforcement policy."""
    enabled: bool
    max_transit_periods: Union[int, None]


class HvdcBand(TypedDict):
    """A single dispatch band for multi-band HVDC control."""
    id: str
    p_min_mw: float
    p_max_mw: float
    cost_per_mwh: float
    loss_b_frac: float
    ramp_mw_per_min: float
    reserve_eligible_up: bool
    reserve_eligible_down: bool
    max_duration_hours: float


class _HvdcDispatchLinkRequired(TypedDict):
    name: str
    from_bus: int
    to_bus: int
    p_dc_min_mw: float
    p_dc_max_mw: float
    loss_a_mw: float
    loss_b_frac: float
    ramp_mw_per_min: float
    cost_per_mwh: float
    bands: list[HvdcBand]

class HvdcDispatchLink(_HvdcDispatchLinkRequired, total=False):
    """HVDC link description for dispatch co-optimization."""
    id: str


class LossFactorWarmStartMode_Variant0(TypedDict):
    """No cold-start warm-start on iter 0. The first MIP is solved lossless; the refinement LP corrects for losses after. Subsequent security iterations still warm-start from the prior iteration's `dloss_dp` if available."""
    mode: Literal['disabled']
class LossFactorWarmStartMode_Variant1(TypedDict):
    """Seed every bus's `dloss` to the same rate `rate ∈ [0, 0.5]` (typical `0.02` for 2%). `total_losses_mw = rate × total_load`. No per-bus variation; cheapest cold-start. Good when the network's losses are dominated by a roughly uniform background loss rate rather than strong per-bus asymmetries."""
    mode: Literal['uniform']
    rate: float
class LossFactorWarmStartMode_Variant2(TypedDict):
    """Seed `dloss` from a synthetic load-pattern DC PF plus sparse adjoint loss sensitivities, normalised so total weighted losses match `rate × total_load`. Captures per-bus variation from network topology + load pattern without materialising loss PTDFs."""
    mode: Literal['load_pattern']
    rate: float
class LossFactorWarmStartMode_Variant3(TypedDict):
    """Seed from a DC power flow on each hourly load pattern with pmax-balanced generation. Most accurate cold-start; costs one DC PF plus one adjoint solve per period. Falls back to `Uniform { rate: 0.02 }` if the DC PF fails."""
    mode: Literal['dc_pf']
# Cold-start strategy for the SCUC loss-factor warm-start on the first security iteration.
LossFactorWarmStartMode = Union[LossFactorWarmStartMode_Variant0, LossFactorWarmStartMode_Variant1, LossFactorWarmStartMode_Variant2, LossFactorWarmStartMode_Variant3]


class LossFactorPolicy(TypedDict, total=False):
    """Iterative DC loss-factor policy."""
    enabled: bool
    max_iterations: int
    tolerance: float
    warm_start_mode: LossFactorWarmStartMode


class PhHeadCurve(TypedDict):
    """Public pumped-hydro head curve keyed by resource id."""
    resource_id: str
    breakpoints: list[tuple[float, float]]


class _PhModeConstraintRequired(TypedDict):
    resource_id: str
    min_gen_run_periods: int
    min_pump_run_periods: int
    pump_to_gen_periods: int
    gen_to_pump_periods: int

class PhModeConstraint(_PhModeConstraintRequired, total=False):
    """Public pumped-hydro mode constraint keyed by resource id."""
    max_pump_starts: Union[int, None]


class RampMode_Variant0(TypedDict):
    """Incremental block decomposition."""
    block: dict[str, Any]
# How piecewise ramp curves are applied in dispatch LP formulations.
RampMode = Union[Literal['averaged'], Literal['interpolated'], RampMode_Variant0]


class RampPolicy(TypedDict, total=False):
    """Ramp-constraint modeling policy."""
    mode: RampMode
    enforcement: ConstraintEnforcement


class HvdcLinkRef(TypedDict):
    """Stable HVDC selector."""
    link_id: str


# How N-1 contingencies are embedded into DC time-coupled dispatch.
SecurityEmbedding = Literal['explicit_contingencies', 'iterative_screening']


# Method for ranking contingency pairs when pre-seeding iter 0 of iterative-screening SCUC.
SecurityPreseedMethod = Literal['none', 'max_lodf_topology']


class SecurityPolicy(TypedDict, total=False):
    """Optional N-1 security policy for DC time-coupled dispatch."""
    embedding: SecurityEmbedding
    max_iterations: int
    violation_tolerance_pu: float
    max_cuts_per_iteration: int
    branch_contingencies: list[BranchRef]
    hvdc_contingencies: list[HvdcLinkRef]
    preseed_count_per_period: int
    preseed_method: SecurityPreseedMethod
    near_binding_report: bool


class ThermalLimitPolicy(TypedDict, total=False):
    """Thermal-limit enforcement policy."""
    enforce: bool
    min_rate_a: float


# Network topology-control policy.
TopologyControlMode = Literal['fixed', 'switchable']


class TopologyControlPolicy(TypedDict, total=False):
    """Topology-control modeling policy."""
    mode: TopologyControlMode
    branch_switching_big_m_factor: float


class DispatchNetwork(TypedDict, total=False):
    """Network-facing study policy."""
    thermal_limits: ThermalLimitPolicy
    flowgates: FlowgatePolicy
    par_setpoints: list[Any]
    hvdc_links: list[HvdcDispatchLink]
    loss_factors: LossFactorPolicy
    forbidden_zones: ForbiddenZonePolicy
    commitment_transitions: CommitmentTransitionPolicy
    ramping: RampPolicy
    energy_windows: EnergyWindowPolicy
    topology_control: TopologyControlPolicy
    security: Union[SecurityPolicy, None]
    ph_head_curves: list[PhHeadCurve]
    ph_mode_constraints: list[PhModeConstraint]


class GeneratorDerateProfile(TypedDict):
    """Generator derate profile keyed by dispatch `resource_id`."""
    resource_id: str
    derate_factors: list[float]


class GeneratorDerateProfiles(TypedDict, total=False):
    """Collection of generator derate profiles."""
    profiles: list[GeneratorDerateProfile]


class _GeneratorDispatchBoundsProfileRequired(TypedDict):
    resource_id: str
    p_min_mw: list[float]
    p_max_mw: list[float]

class GeneratorDispatchBoundsProfile(_GeneratorDispatchBoundsProfileRequired, total=False):
    """Absolute generator dispatch bounds keyed by dispatch `resource_id`.  Unlike
derates, these bounds specify the per-period physical dispatch window directly
in MW and are applied before each SCED/SCUC network snapshot is built. This is
the right surface for resources whose availability floor and ceiling both vary
over time, such as fixed-profile renewable injections or externally supplied
must-take schedules.
"""
    q_min_mvar: Union[list[float], None]
    q_max_mvar: Union[list[float], None]


class GeneratorDispatchBoundsProfiles(TypedDict, total=False):
    """Collection of absolute generator dispatch bounds."""
    profiles: list[GeneratorDispatchBoundsProfile]


class HvdcDerateProfile(TypedDict):
    """HVDC derate profile keyed by dispatch `link_id`."""
    link_id: str
    derate_factors: list[float]


class HvdcDerateProfiles(TypedDict, total=False):
    """Collection of HVDC derate profiles."""
    profiles: list[HvdcDerateProfile]


class RenewableProfile(TypedDict):
    """Renewable capacity-factor profile keyed by dispatch `resource_id`."""
    resource_id: str
    capacity_factors: list[float]


class RenewableProfiles(TypedDict, total=False):
    """Collection of renewable capacity-factor profiles."""
    profiles: list[RenewableProfile]


class DispatchProfiles(TypedDict, total=False):
    """Time-series profiles and derates applied during the study."""
    load: BusLoadProfiles
    ac_bus_load: AcBusLoadProfiles
    renewable: RenewableProfiles
    generator_derates: GeneratorDerateProfiles
    generator_dispatch_bounds: GeneratorDispatchBoundsProfiles
    branch_derates: BranchDerateProfiles
    hvdc_derates: HvdcDerateProfiles


class _ScedAcBendersCutRequired(TypedDict):
    period: int
    coefficients_dollars_per_mw_per_hour: dict[str, float]
    rhs_dollars_per_hour: float

class ScedAcBendersCut(_ScedAcBendersCutRequired, total=False):
    """A single Benders optimality cut on the SCED LP from an AC-OPF subproblem.
Encodes a linear lower bound of the form  `eta[period] >= rhs_dollars_per_hour
+ Σ_g coefficient[gen] * Pg[gen, period]`  where `eta[period]` is a scalar
epigraph variable that the SCED LP will minimise (one per period). The cut is
generated by solving the AC-OPF subproblem with `Pg` fixed to a candidate
master schedule and reading the shadow prices on the bound constraints. See
`surge_opf::solve_ac_opf_subproblem` for the cut construction details and the
SCED-AC Benders module documentation for the convergence story.
"""
    iteration: int


class ScedAcBendersRunParams(TypedDict, total=False):
    """Orchestration parameters controlling the SCED-AC Benders master / subproblem
loop. When populated inside [`ScedAcBendersRuntime::orchestration`], the
dispatch solver runs the full decomposition internally (using
[`crate::sced_ac_benders::solve_sced_sequence_benders`]) rather than expecting
the caller to drive iterations from outside.  Every field has a sensible
default; callers typically populate only the fields they want to override.
"""
    max_iterations: int
    rel_tol: float
    abs_tol: float
    min_slack_dollars_per_hour: float
    marginal_trim_dollars_per_mw_per_hour: float
    trust_region_mw: Union[float, None]
    trust_region_expansion_factor: float
    trust_region_contraction_factor: float
    trust_region_min_mw: float
    max_cuts_per_period: Union[int, None]
    cut_dedup_marginal_tol: float
    stagnation_patience: int
    oscillation_patience: int
    ac_opf_thermal_slack_penalty_per_mva: float
    ac_opf_bus_active_power_balance_slack_penalty_per_mw: float
    ac_opf_bus_reactive_power_balance_slack_penalty_per_mvar: float


class ScedAcBendersRuntime(TypedDict, total=False):
    """Per-period configuration for SCED-AC Benders decomposition.  When
`period_eta_active` is true for a period, the SCED LP allocates a scalar
`eta[period]` variable with cost coefficient `+1.0` and adds a row for every
cut in `cuts` whose `period` matches. Periods not listed run the standard SCED
LP unchanged.
"""
    eta_periods: list[int]
    cuts: list[ScedAcBendersCut]
    orchestration: Union[ScedAcBendersRunParams, None]


class DispatchRuntime(TypedDict, total=False):
    """Runtime execution controls for dispatch."""
    tolerance: float
    run_pricing: bool
    ac_relax_committed_pmin_to_zero: bool
    ac_opf: Any
    fixed_hvdc_dispatch: list[HvdcPeriodPowerSeries]
    ac_dispatch_warm_start: AcDispatchWarmStart
    ac_target_tracking: AcDispatchTargetTracking
    sced_ac_benders: ScedAcBendersRuntime
    capture_model_diagnostics: bool
    scuc_firm_bus_balance_slacks: bool
    scuc_firm_branch_thermal_slacks: bool
    scuc_disable_bus_power_balance: bool
    ac_sced_period_concurrency: Union[int, None]


class DispatchState(TypedDict, total=False):
    """Initial state carried into the study."""
    initial: DispatchInitialState


class DispatchTimeline(TypedDict, total=False):
    """Study timeline."""
    periods: int
    interval_hours: float
    interval_hours_by_period: list[float]


# Power balance formulation.
Formulation = Literal['dc', 'ac']


# How dispatch intervals are coupled across the study timeline.
IntervalCoupling = Literal['period_by_period', 'time_coupled']


class DispatchRequest(TypedDict, total=False):
    """Public dispatch request consumed by surge_dispatch::solve_dispatch."""
    formulation: Formulation
    coupling: IntervalCoupling
    commitment: CommitmentPolicy
    timeline: DispatchTimeline
    profiles: DispatchProfiles
    state: DispatchState
    market: DispatchMarket
    network: DispatchNetwork
    runtime: DispatchRuntime


__all__ = [
    'AcBusLoadProfile',
    'AcBusLoadProfiles',
    'AcDispatchTargetTracking',
    'AcDispatchTargetTrackingPair',
    'AcDispatchWarmStart',
    'BranchDerateProfile',
    'BranchDerateProfiles',
    'BranchRef',
    'BusAreaAssignment',
    'BusLoadProfile',
    'BusLoadProfiles',
    'BusPeriodVoltageSeries',
    'CarbonPrice',
    'CombinedCycleConfigOfferSchedule',
    'CommitmentConstraint',
    'CommitmentInitialCondition',
    'CommitmentOptions',
    'CommitmentPolicy',
    'CommitmentSchedule',
    'CommitmentTerm',
    'CommitmentTrajectoryMode',
    'CommitmentTransitionPolicy',
    'ConstraintEnforcement',
    'DispatchInitialState',
    'DispatchMarket',
    'DispatchNetwork',
    'DispatchProfiles',
    'DispatchRequest',
    'DispatchRuntime',
    'DispatchState',
    'DispatchTimeline',
    'DispatchableLoadOfferSchedule',
    'DispatchableLoadReserveOfferSchedule',
    'EmissionProfile',
    'EnergyWindowPolicy',
    'FlowgatePolicy',
    'ForbiddenZonePolicy',
    'Formulation',
    'FrequencySecurityOptions',
    'GeneratorCostModeling',
    'GeneratorDerateProfile',
    'GeneratorDerateProfiles',
    'GeneratorDispatchBoundsProfile',
    'GeneratorDispatchBoundsProfiles',
    'GeneratorOfferSchedule',
    'GeneratorReserveOfferSchedule',
    'HvdcBand',
    'HvdcDerateProfile',
    'HvdcDerateProfiles',
    'HvdcDispatchLink',
    'HvdcDispatchPoint',
    'HvdcLinkRef',
    'HvdcPeriodPowerSeries',
    'IntervalCoupling',
    'LossFactorPolicy',
    'LossFactorWarmStartMode',
    'MustRunUnits',
    'PhHeadCurve',
    'PhModeConstraint',
    'PowerBalancePenalty',
    'RampMode',
    'RampPolicy',
    'RenewableProfile',
    'RenewableProfiles',
    'ReserveOfferSchedule',
    'ResourceAreaAssignment',
    'ResourceCommitmentSchedule',
    'ResourceDispatchPoint',
    'ResourceEligibility',
    'ResourceEmissionRate',
    'ResourceEnergyWindowLimit',
    'ResourcePeriodCommitment',
    'ResourcePeriodPowerSeries',
    'ResourceStartupWindowLimit',
    'ScedAcBendersCut',
    'ScedAcBendersRunParams',
    'ScedAcBendersRuntime',
    'SecurityEmbedding',
    'SecurityPolicy',
    'SecurityPreseedMethod',
    'StoragePowerSchedule',
    'StorageReserveSocImpact',
    'StorageSocOverride',
    'ThermalLimitPolicy',
    'TieLineLimits',
    'TopologyControlMode',
    'TopologyControlPolicy',
]
