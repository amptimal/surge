# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Type stubs for the curated public ``surge`` package surface."""

from __future__ import annotations

from collections.abc import Mapping
from os import PathLike
from typing import Any

from ._study_inputs import HvdcOpfLink as HvdcOpfLink
from ._study_inputs import ParSetpoint as ParSetpoint
from ._study_inputs import VirtualBid as VirtualBid
from ._surge import (
    AcPfResult as AcPfResult,
    AmbiguousTopologyError as AmbiguousTopologyError,
    AreaSchedule as AreaSchedule,
    Bay as Bay,
    BindingContingency as BindingContingency,
    Branch as Branch,
    BreakerRating as BreakerRating,
    Bus as Bus,
    BusbarSection as BusbarSection,
    CollapsedBranch as CollapsedBranch,
    CombinedCycleConfig as CombinedCycleConfig,
    CombinedCyclePlant as CombinedCyclePlant,
    CombinedCycleTransition as CombinedCycleTransition,
    ConnectivityNode as ConnectivityNode,
    Contingency as Contingency,
    ContingencyAnalysis as ContingencyAnalysis,
    ContingencyOptions as ContingencyOptions,
    ContingencyViolation as ContingencyViolation,
    ConvergenceError as ConvergenceError,
    DcBranch as DcBranch,
    DcBus as DcBus,
    DcConverter as DcConverter,
    DcPfResult as DcPfResult,
    DispatchResult as DispatchResult,
    DispatchableLoad as DispatchableLoad,
    FactsDevice as FactsDevice,
    FailedContingencyEvaluation as FailedContingencyEvaluation,
    FixedShunt as FixedShunt,
    Generator as Generator,
    Hvdc as Hvdc,
    HvdcDcBusSolution as HvdcDcBusSolution,
    HvdcLccDetail as HvdcLccDetail,
    HvdcResult as HvdcResult,
    HvdcStationSolution as HvdcStationSolution,
    InfeasibleError as InfeasibleError,
    LccHvdcLink as LccHvdcLink,
    Load as Load,
    MissingTopologyError as MissingTopologyError,
    Network as Network,
    NetworkError as NetworkError,
    NodeBreakerTopology as NodeBreakerTopology,
    OpfResult as OpfResult,
    OutageEntry as OutageEntry,
    PumpedHydroUnit as PumpedHydroUnit,
    ReserveZone as ReserveZone,
    ScopfScreeningStats as ScopfScreeningStats,
    StaleTopologyError as StaleTopologyError,
    StorageParams as StorageParams,
    Substation as Substation,
    SurgeError as SurgeError,
    SurgeIOError as SurgeIOError,
    UnsupportedFeatureError as UnsupportedFeatureError,
    TerminalConnection as TerminalConnection,
    TopologyBusMerge as TopologyBusMerge,
    TopologyBusSplit as TopologyBusSplit,
    TopologyError as TopologyError,
    TopologyIntegrityError as TopologyIntegrityError,
    TopologyMapping as TopologyMapping,
    TopologyRebuildResult as TopologyRebuildResult,
    TopologyReport as TopologyReport,
    TopologySwitch as TopologySwitch,
    VoltageLevel as VoltageLevel,
    VscHvdcLink as VscHvdcLink,
    analyze_contingencies as analyze_contingencies,
    analyze_n1_branch as analyze_n1_branch,
    analyze_n1_generator as analyze_n1_generator,
    LogReceiver as LogReceiver,
    analyze_n2_branch as analyze_n2_branch,
    attach_log_listener as attach_log_listener,
    case118 as case118,
    case14 as case14,
    case30 as case30,
    case300 as case300,
    case57 as case57,
    case9 as case9,
    detach_log_listener as detach_log_listener,
    get_max_threads as get_max_threads,
    init_logging as init_logging,
    market30 as market30,
    set_max_threads as set_max_threads,
    solve_hvdc as solve_hvdc,
    version as version,
)
from .opf import (
    AcAngleWarmStartMode as AcAngleWarmStartMode,
    AcOpfBendersSubproblemResult as AcOpfBendersSubproblemResult,
    AcOpfOptions as AcOpfOptions,
    AcOpfResult as AcOpfResult,
    AcOpfRuntime as AcOpfRuntime,
    ConstraintScreening as ConstraintScreening,
    DcCostModel as DcCostModel,
    DcLossModel as DcLossModel,
    DcOpfOptions as DcOpfOptions,
    DcOpfResult as DcOpfResult,
    DcOpfRuntime as DcOpfRuntime,
    DiscreteMode as DiscreteMode,
    GeneratorLimitMode as GeneratorLimitMode,
    HvdcMode as HvdcMode,
    ScopfFormulation as ScopfFormulation,
    ScopfMode as ScopfMode,
    ScopfOptions as ScopfOptions,
    ScopfResult as ScopfResult,
    ScopfRuntime as ScopfRuntime,
    ScopfScreeningPolicy as ScopfScreeningPolicy,
    ThermalRating as ThermalRating,
    solve_ac_opf_subproblem as solve_ac_opf_subproblem,
)
from .dispatch import solve_dispatch as solve_dispatch
from .dispatch_request import DispatchRequest as DispatchRequest
from .powerflow import AcPfOptions as AcPfOptions
from .powerflow import DcPfOptions as DcPfOptions
from . import audit as audit
from . import batch as batch
from . import compose as compose
from . import construction as construction
from . import contingency as contingency
from . import contingency_io as contingency_io
from . import dc as dc
from . import dispatch as dispatch
from . import io as io
from . import losses as losses
from . import market as market
from . import opf as opf
from . import powerflow as powerflow
from . import subsystem as subsystem
from . import transfer as transfer
from . import units as units


def load(
    path: str | PathLike[str],
    format: str | None = None,
) -> Network: ...


def load_network(
    path: str | PathLike[str],
    format: str | None = None,
) -> Network: ...


def save(network: Network, path: str | PathLike[str]) -> None: ...


def list_builtin_cases() -> list[str]: ...


def load_builtin_case(name: str) -> Network: ...


def builtin_case_rated_flags() -> list[tuple[str, bool]]: ...


def solve_dc_pf(
    network: Network,
    options: DcPfOptions | None = None,
    /,
) -> DcPfResult: ...


def solve_ac_pf(
    network: Network,
    options: AcPfOptions | None = None,
    /,
) -> AcPfResult: ...


def solve_dc_opf(
    network: Network,
    options: DcOpfOptions | None = None,
    runtime: DcOpfRuntime | None = None,
) -> DcOpfResult: ...


def solve_ac_opf(
    network: Network,
    options: AcOpfOptions | None = None,
    runtime: AcOpfRuntime | None = None,
) -> AcOpfResult: ...


def solve_scopf(
    network: Network,
    options: ScopfOptions | None = None,
    runtime: ScopfRuntime | None = None,
) -> ScopfResult: ...


def solve_dispatch(
    network: Network,
    request: Mapping[str, Any] | str | None = None,
    *,
    lp_solver: str | None = None,
) -> DispatchResult: ...
