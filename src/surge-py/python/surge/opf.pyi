# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._opf_types import (
    AcAngleWarmStartMode as AcAngleWarmStartMode,
    AcOpfOptions as AcOpfOptions,
    AcOpfRuntime as AcOpfRuntime,
    ConstraintScreening as ConstraintScreening,
    DcCostModel as DcCostModel,
    DcLossModel as DcLossModel,
    DcOpfOptions as DcOpfOptions,
    DcOpfRuntime as DcOpfRuntime,
    DiscreteMode as DiscreteMode,
    GeneratorLimitMode as GeneratorLimitMode,
    HvdcMode as HvdcMode,
    ScopfFormulation as ScopfFormulation,
    ScopfMode as ScopfMode,
    ScopfOptions as ScopfOptions,
    ScopfRuntime as ScopfRuntime,
    ScopfScreeningPolicy as ScopfScreeningPolicy,
    ThermalRating as ThermalRating,
)
from ._surge import (
    BindingContingency,
    ContingencyViolation,
    FailedContingencyEvaluation,
    OpfResult,
    ScopfScreeningStats,
)


class DcOpfResult:
    @property
    def opf(self) -> OpfResult: ...
    @property
    def hvdc_dispatch_mw(self) -> list[float] | None: ...
    @property
    def hvdc_shadow_prices(self) -> list[float] | None: ...
    @property
    def generator_limit_violations(self) -> list[float] | None: ...
    @property
    def feasible(self) -> bool: ...
    def __getattr__(self, name: str) -> object: ...


class AcOpfResult:
    @property
    def opf(self) -> OpfResult: ...
    @property
    def hvdc_dispatch_mw(self) -> list[float] | None: ...
    @property
    def hvdc_losses_mw(self) -> list[float] | None: ...
    @property
    def hvdc_iterations(self) -> int: ...
    @property
    def feasible(self) -> bool: ...
    def __getattr__(self, name: str) -> object: ...


class ScopfResult:
    @property
    def opf(self) -> OpfResult: ...
    @property
    def base_opf(self) -> OpfResult: ...
    @property
    def formulation(self) -> str: ...
    @property
    def mode(self) -> str: ...
    @property
    def iterations(self) -> int: ...
    @property
    def converged(self) -> bool: ...
    @property
    def total_contingencies_evaluated(self) -> int: ...
    @property
    def total_contingency_constraints(self) -> int: ...
    @property
    def binding_contingencies(self) -> list[BindingContingency]: ...
    @property
    def lmp_contingency_congestion(self) -> object: ...
    @property
    def remaining_violations(self) -> list[ContingencyViolation]: ...
    @property
    def failed_contingencies(self) -> list[FailedContingencyEvaluation]: ...
    @property
    def screening_stats(self) -> ScopfScreeningStats: ...
    @property
    def solve_time_secs(self) -> float: ...
    def __getattr__(self, name: str) -> object: ...


def solve_dc_opf(
    network: object,
    options: DcOpfOptions | None = None,
    runtime: DcOpfRuntime | None = None,
) -> DcOpfResult: ...


def solve_ac_opf(
    network: object,
    options: AcOpfOptions | None = None,
    runtime: AcOpfRuntime | None = None,
) -> AcOpfResult: ...


def solve_scopf(
    network: object,
    options: ScopfOptions | None = None,
    runtime: ScopfRuntime | None = None,
) -> ScopfResult: ...


__all__: list[str]
