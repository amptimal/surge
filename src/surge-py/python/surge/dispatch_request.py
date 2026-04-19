# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Typed view of the ``DispatchRequest`` dict accepted by :func:`surge.solve_dispatch`.

This module re-exports the auto-generated TypedDicts from
:mod:`surge._generated.dispatch_request`, which mirrors the Rust
``DispatchRequest`` schema 1:1. Regenerate after Rust-side changes
with ``python3 scripts/codegen_dispatch_request.py``.

The Rust struct lives in ``src/surge-dispatch/src/request.rs`` and
derives ``schemars::JsonSchema``. Nine top-level keys, all optional —
the Rust side applies ``serde(default)`` on every field, so an empty
``{}`` is a legal request:

==================  =============================================
``formulation``     ``"dc"`` (default) or ``"ac"``
``coupling``        ``"period_by_period"`` (default) or ``"time_coupled"``
``commitment``      ``"all_committed"`` | ``{"fixed": ...}`` | ``{"optimize": ...}`` | ``{"additional": ...}``
``timeline``        period count + interval hours
``profiles``        per-period load, renewable, derate, dispatch-bounds profiles
``state``           initial conditions (previous dispatch, storage SOC)
``market``          reserves, offers, penalties, requirements
``network``         thermal/flowgate/loss/topology/security options
``runtime``         solver knobs (tolerance, AC warm-start, Benders, ...)
==================  =============================================

Fields whose Rust type lives in another crate (``surge_network::market::*``,
``surge_opf::AcOpfOptions``, ``surge_solution::ParSetpoint``) are typed
as ``dict[str, Any]`` / ``list[dict[str, Any]]`` because the schema
treats them as opaque JSON values. Build those payloads with the
helpers in :mod:`surge.market` (``GeneratorOfferSchedule``,
``ReserveProductDef``, ``build_reserve_products_dict``, ...).

Example::

    from surge import DispatchRequest

    request: DispatchRequest = {
        "timeline": {"periods": 24, "interval_hours": 1.0},
        "commitment": "all_committed",
        "coupling": "time_coupled",
        "profiles": {
            "load": {"profiles": [{"bus_number": 1, "values_mw": [...]}]},
        },
        "market": {"reserve_products": [...]},
    }
    result = surge.solve_dispatch(network, request)
"""

from __future__ import annotations

# Re-export every generated type. The generated module is the source of
# truth — adding/removing a Rust type flows through here automatically.
from ._generated.dispatch_request import (
    AcBusLoadProfile,
    AcBusLoadProfiles,
    AcDispatchTargetTracking,
    AcDispatchTargetTrackingPair,
    AcDispatchWarmStart,
    BranchDerateProfile,
    BranchDerateProfiles,
    BranchRef,
    BusAreaAssignment,
    BusLoadProfile,
    BusLoadProfiles,
    BusPeriodVoltageSeries,
    CarbonPrice,
    CombinedCycleConfigOfferSchedule,
    CommitmentConstraint,
    CommitmentInitialCondition,
    CommitmentOptions,
    CommitmentPolicy,
    CommitmentSchedule,
    CommitmentTerm,
    CommitmentTrajectoryMode,
    CommitmentTransitionPolicy,
    ConstraintEnforcement,
    DispatchableLoadOfferSchedule,
    DispatchableLoadReserveOfferSchedule,
    DispatchInitialState,
    DispatchMarket,
    DispatchNetwork,
    DispatchProfiles,
    DispatchRequest,
    DispatchRuntime,
    DispatchState,
    DispatchTimeline,
    EmissionProfile,
    EnergyWindowPolicy,
    FlowgatePolicy,
    ForbiddenZonePolicy,
    Formulation,
    FrequencySecurityOptions,
    GeneratorCostModeling,
    GeneratorDerateProfile,
    GeneratorDerateProfiles,
    GeneratorDispatchBoundsProfile,
    GeneratorDispatchBoundsProfiles,
    GeneratorOfferSchedule,
    GeneratorReserveOfferSchedule,
    HvdcBand,
    HvdcDerateProfile,
    HvdcDerateProfiles,
    HvdcDispatchLink,
    HvdcDispatchPoint,
    HvdcLinkRef,
    HvdcPeriodPowerSeries,
    IntervalCoupling,
    LossFactorPolicy,
    MustRunUnits,
    PhHeadCurve,
    PhModeConstraint,
    PowerBalancePenalty,
    RampMode,
    RampPolicy,
    RenewableProfile,
    RenewableProfiles,
    ReserveOfferSchedule,
    ResourceAreaAssignment,
    ResourceCommitmentSchedule,
    ResourceDispatchPoint,
    ResourceEligibility,
    ResourceEmissionRate,
    ResourceEnergyWindowLimit,
    ResourcePeriodCommitment,
    ResourcePeriodPowerSeries,
    ResourceStartupWindowLimit,
    ScedAcBendersCut,
    ScedAcBendersRunParams,
    ScedAcBendersRuntime,
    SecurityEmbedding,
    SecurityPolicy,
    SecurityPreseedMethod,
    StoragePowerSchedule,
    StorageReserveSocImpact,
    StorageSocOverride,
    ThermalLimitPolicy,
    TieLineLimits,
    TopologyControlMode,
    TopologyControlPolicy,
)


# ---------------------------------------------------------------------------
# Back-compat aliases for the v0.1.x manual TypedDict surface.
# The generated module uses the canonical Rust names; older code may
# import the historical ``*Spec`` / ``LoadProfiles`` aliases. Keep these
# pointing at the canonical types so old imports keep working without
# any divergence.
# ---------------------------------------------------------------------------
TimelineSpec = DispatchTimeline
CommitmentSpec = CommitmentPolicy
ProfilesSpec = DispatchProfiles
StateSpec = DispatchState
MarketSpec = DispatchMarket
NetworkSpec = DispatchNetwork
RuntimeSpec = DispatchRuntime
LoadProfiles = BusLoadProfiles
ThermalLimitsSpec = ThermalLimitPolicy
FlowgatesSpec = FlowgatePolicy
LossFactorsSpec = LossFactorPolicy
ForbiddenZonesSpec = ForbiddenZonePolicy
CommitmentTransitionsSpec = CommitmentTransitionPolicy
RampingSpec = RampPolicy
EnergyWindowsSpec = EnergyWindowPolicy
TopologyControlSpec = TopologyControlPolicy
SecurityPolicySpec = SecurityPolicy
PowerBalancePenaltySpec = PowerBalancePenalty


__all__ = [
    # Top-level
    "DispatchRequest",
    # Axes
    "Formulation",
    "IntervalCoupling",
    "CommitmentPolicy",
    # Timeline
    "DispatchTimeline",
    "TimelineSpec",
    # Commitment
    "CommitmentInitialCondition",
    "CommitmentOptions",
    "CommitmentSchedule",
    "ResourceCommitmentSchedule",
    "ResourcePeriodCommitment",
    "CommitmentTerm",
    "CommitmentConstraint",
    "CommitmentSpec",
    # Profiles
    "DispatchProfiles",
    "ProfilesSpec",
    "BusLoadProfile",
    "BusLoadProfiles",
    "LoadProfiles",
    "AcBusLoadProfile",
    "AcBusLoadProfiles",
    "RenewableProfile",
    "RenewableProfiles",
    "GeneratorDerateProfile",
    "GeneratorDerateProfiles",
    "GeneratorDispatchBoundsProfile",
    "GeneratorDispatchBoundsProfiles",
    "BranchDerateProfile",
    "BranchDerateProfiles",
    "HvdcDerateProfile",
    "HvdcDerateProfiles",
    # State
    "DispatchState",
    "StateSpec",
    "DispatchInitialState",
    "ResourceDispatchPoint",
    "HvdcDispatchPoint",
    "StorageSocOverride",
    # Market
    "DispatchMarket",
    "MarketSpec",
    "ResourceEmissionRate",
    "EmissionProfile",
    "MustRunUnits",
    "GeneratorOfferSchedule",
    "DispatchableLoadOfferSchedule",
    "ReserveOfferSchedule",
    "GeneratorReserveOfferSchedule",
    "DispatchableLoadReserveOfferSchedule",
    "StoragePowerSchedule",
    "StorageReserveSocImpact",
    "CombinedCycleConfigOfferSchedule",
    "ResourceAreaAssignment",
    "BusAreaAssignment",
    "ResourceEligibility",
    "ResourceStartupWindowLimit",
    "ResourceEnergyWindowLimit",
    "GeneratorCostModeling",
    "PowerBalancePenalty",
    "PowerBalancePenaltySpec",
    "CarbonPrice",
    "TieLineLimits",
    "FrequencySecurityOptions",
    # Network
    "DispatchNetwork",
    "NetworkSpec",
    "BranchRef",
    "HvdcLinkRef",
    "ThermalLimitPolicy",
    "ThermalLimitsSpec",
    "FlowgatePolicy",
    "FlowgatesSpec",
    "LossFactorPolicy",
    "LossFactorsSpec",
    "ForbiddenZonePolicy",
    "ForbiddenZonesSpec",
    "CommitmentTransitionPolicy",
    "CommitmentTransitionsSpec",
    "CommitmentTrajectoryMode",
    "RampMode",
    "RampPolicy",
    "RampingSpec",
    "ConstraintEnforcement",
    "EnergyWindowPolicy",
    "EnergyWindowsSpec",
    "TopologyControlMode",
    "TopologyControlPolicy",
    "TopologyControlSpec",
    "SecurityEmbedding",
    "SecurityPreseedMethod",
    "SecurityPolicy",
    "SecurityPolicySpec",
    "PhHeadCurve",
    "PhModeConstraint",
    "HvdcDispatchLink",
    "HvdcBand",
    # Runtime
    "DispatchRuntime",
    "RuntimeSpec",
    "AcDispatchWarmStart",
    "AcDispatchTargetTracking",
    "AcDispatchTargetTrackingPair",
    "BusPeriodVoltageSeries",
    "ResourcePeriodPowerSeries",
    "HvdcPeriodPowerSeries",
    "ScedAcBendersCut",
    "ScedAcBendersRunParams",
    "ScedAcBendersRuntime",
]
