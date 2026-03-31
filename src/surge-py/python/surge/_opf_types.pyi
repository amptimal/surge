# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from enum import Enum
from typing import TYPE_CHECKING

from ._study_inputs import HvdcOpfLink, ParSetpoint, VirtualBid

if TYPE_CHECKING:
    from . import OpfResult
    from .opf import ScopfResult


class DcCostModel(str, Enum):
    QUADRATIC = "quadratic"
    PIECEWISE_LINEAR = "piecewise_linear"


class DcLossModel(str, Enum):
    IGNORE = "ignore"
    ITERATIVE = "iterative"


class GeneratorLimitMode(str, Enum):
    HARD = "hard"
    SOFT = "soft"


class DiscreteMode(str, Enum):
    CONTINUOUS = "continuous"
    ROUND_AND_CHECK = "round_and_check"


class HvdcMode(str, Enum):
    AUTO = "auto"
    ENABLED = "enabled"
    DISABLED = "disabled"


class AcAngleWarmStartMode(str, Enum):
    AUTO = "auto"
    DC_OPF = "dc_opf"
    DC_POWER_FLOW = "dc_power_flow"


class ScopfFormulation(str, Enum):
    DC = "dc"
    AC = "ac"


class ScopfMode(str, Enum):
    PREVENTIVE = "preventive"
    CORRECTIVE = "corrective"


class ThermalRating(str, Enum):
    RATE_A = "rate-a"
    RATE_B = "rate-b"
    RATE_C = "rate-c"


class TransmissionSwitchingFormulation(str, Enum):
    DC_MILP = "dc_milp"
    DC_RELAXED = "dc_relaxed"
    DC_ENUMERATE = "dc_enumerate"


class ReactiveDispatchObjective(str, Enum):
    LOSS = "loss"
    VOLTAGE = "voltage"
    COMBINED = "combined"


@dataclass(frozen=True, kw_only=True)
class ConstraintScreening:
    threshold_fraction: float = 0.9
    minimum_bus_count: int = 1000
    fallback_enabled: bool = False


@dataclass(frozen=True, kw_only=True)
class ScopfScreeningPolicy:
    enabled: bool = True
    threshold_fraction: float = 0.9
    max_initial_contingencies: int = 500


@dataclass(frozen=True, kw_only=True)
class DcOpfOptions:
    enforce_thermal_limits: bool = True
    minimum_branch_rating_a_mva: float = 1.0
    cost_model: DcCostModel = ...
    piecewise_linear_breakpoints: int = 20
    enforce_flowgates: bool = True
    par_setpoints: list[ParSetpoint] = ...
    hvdc_links: list[HvdcOpfLink] = ...
    generator_limit_mode: GeneratorLimitMode = ...
    generator_limit_penalty_per_mw: float | None = None
    virtual_bids: list[VirtualBid] = ...
    loss_model: DcLossModel = ...
    loss_iterations: int = 3
    loss_tolerance: float = 1e-3
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class DcOpfRuntime:
    tolerance: float = 1e-8
    max_iterations: int = 200
    lp_solver: str | None = None
    warm_start_theta: list[float] | None = None
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class AcOpfOptions:
    enforce_thermal_limits: bool = True
    minimum_branch_rating_a_mva: float = 1.0
    enforce_angle_limits: bool = False
    optimize_switched_shunts: bool = False
    optimize_taps: bool = False
    optimize_phase_shifters: bool = False
    optimize_svc: bool = False
    optimize_tcsc: bool = False
    hvdc_mode: HvdcMode = ...
    enforce_capability_curves: bool = True
    discrete_mode: DiscreteMode = ...
    storage_state_mwh_by_generator_id: Mapping[str, float] = ...
    interval_hours: float = 1.0
    enforce_flowgates: bool = False
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class AcOpfRuntime:
    tolerance: float = 1e-6
    max_iterations: int = 0
    exact_hessian: bool = True
    nlp_solver: str | None = None
    print_level: int = 0
    warm_start: OpfResult | None = None
    angle_warm_start: AcAngleWarmStartMode = ...
    constraint_screening: ConstraintScreening | None = None
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class ScopfOptions:
    formulation: ScopfFormulation = ...
    mode: ScopfMode = ...
    corrective_ramp_window_minutes: float = 10.0
    voltage_threshold_pu: float = 0.01
    contingency_rating: ThermalRating = ...
    enforce_flowgates: bool = True
    enforce_voltage_security: bool = True
    max_contingencies: int = 0
    minimum_branch_rating_a_mva: float = 1.0
    enforce_angle_limits: bool = True
    dc_opf: DcOpfOptions | None = None
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class ScopfRuntime:
    violation_tolerance_pu: float = 0.01
    max_iterations: int = 20
    max_cuts_per_iteration: int = 100
    lp_solver: str | None = None
    nlp_solver: str | None = None
    newton_max_iterations: int = 30
    newton_tolerance: float = 1e-6
    screening: ScopfScreeningPolicy = ...
    warm_start: ScopfResult | None = None
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class TransmissionSwitchingOptions:
    formulation: TransmissionSwitchingFormulation = ...
    maximum_open_switches: int | None = None
    switchable_branch_indices: list[int] | None = None
    switchable_rating_threshold_mva: float | None = None
    big_m: float | None = None
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class TransmissionSwitchingRuntime:
    tolerance: float = 1e-6
    lp_solver: str | None = None
    time_limit_secs: float = 300.0
    mip_gap: float = 0.01
    max_iterations: int = 1000
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class ReactiveDispatchOptions:
    objective: ReactiveDispatchObjective = ...
    voltage_target_pu: float = 1.0
    loss_weight: float = 1.0
    voltage_weight: float = 1.0
    fix_active_power: bool = True
    optimize_reactive_power: bool = True
    enforce_thermal_limits: bool = True
    minimum_branch_rating_a_mva: float = 1.0
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class ReactiveDispatchRuntime:
    tolerance: float = 1e-6
    max_iterations: int = 0
    exact_hessian: bool = True
    nlp_solver: str | None = None
    print_level: int = 0
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class ReconfigurationOptions:
    max_open_branches: int = 1
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


@dataclass(frozen=True, kw_only=True)
class ReconfigurationRuntime:
    tolerance: float = 1e-6
    max_iterations: int = 200
    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]: ...


__all__: list[str]
