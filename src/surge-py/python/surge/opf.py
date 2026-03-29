# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Canonical mainstream OPF facade."""

from __future__ import annotations

from . import _native
from ._opf_types import (
    AcAngleWarmStartMode,
    AcOpfOptions,
    AcOpfRuntime,
    ConstraintScreening,
    DcCostModel,
    DcLossModel,
    DcOpfOptions,
    DcOpfRuntime,
    DiscreteMode,
    GeneratorLimitMode,
    HvdcMode,
    ScopfFormulation,
    ScopfMode,
    ScopfOptions,
    ScopfRuntime,
    ScopfScreeningPolicy,
    ThermalRating,
)


def _require_instance(name: str, value: object | None, expected_type: type[object]) -> object:
    if value is None:
        return expected_type()
    if not isinstance(value, expected_type):
        raise TypeError(f"{name} expected {expected_type.__name__}, got {type(value).__name__}")
    return value


def _unwrap_opf_solution(warm_start: object | None) -> object | None:
    if warm_start is None:
        return None
    if hasattr(warm_start, "opf"):
        return warm_start.opf  # type: ignore[no-any-return]
    return warm_start


def _unwrap_scopf_result(warm_start: object | None) -> object | None:
    if warm_start is None:
        return None
    if isinstance(warm_start, ScopfResult):
        return warm_start._native_result
    return warm_start


class DcOpfResult:
    """Canonical DC-OPF result."""

    __slots__ = ("_native_result",)

    def __init__(self, native_result) -> None:
        self._native_result = native_result

    @property
    def opf(self):
        return self._native_result.opf

    @property
    def hvdc_dispatch_mw(self):
        return self._native_result.hvdc_dispatch_mw

    @property
    def hvdc_shadow_prices(self):
        return self._native_result.hvdc_shadow_prices

    @property
    def generator_limit_violations(self):
        return self._native_result.gen_limit_violations

    @property
    def feasible(self) -> bool:
        return self._native_result.is_feasible

    def __getattr__(self, name: str):
        return getattr(self.opf, name)

    def __repr__(self) -> str:
        return repr(self._native_result)


class AcOpfResult:
    """Canonical AC-OPF result."""

    __slots__ = ("_native_result",)

    def __init__(self, native_result) -> None:
        self._native_result = native_result

    @property
    def opf(self):
        return self._native_result.opf

    @property
    def hvdc_dispatch_mw(self):
        return self._native_result.hvdc_p_dc_mw

    @property
    def hvdc_losses_mw(self):
        return self._native_result.hvdc_p_loss_mw

    @property
    def hvdc_iterations(self) -> int:
        return self._native_result.hvdc_iterations

    @property
    def feasible(self) -> bool:
        discrete = self.opf.discrete_feasible
        if discrete is not None:
            return bool(discrete)
        return bool(self.opf.converged)

    def __getattr__(self, name: str):
        return getattr(self.opf, name)

    def __repr__(self) -> str:
        return repr(self._native_result)


class ScopfResult:
    """Canonical SCOPF result."""

    __slots__ = ("_native_result",)

    def __init__(self, native_result) -> None:
        self._native_result = native_result

    @property
    def opf(self):
        return self._native_result.base_opf

    @property
    def base_opf(self):
        return self._native_result.base_opf

    @property
    def formulation(self) -> str:
        return self._native_result.formulation

    @property
    def mode(self) -> str:
        return self._native_result.mode

    @property
    def iterations(self) -> int:
        return self._native_result.iterations

    @property
    def converged(self) -> bool:
        return self._native_result.converged

    @property
    def total_contingencies_evaluated(self) -> int:
        return self._native_result.total_contingencies_evaluated

    @property
    def total_contingency_constraints(self) -> int:
        return self._native_result.total_contingency_constraints

    @property
    def binding_contingencies(self):
        return self._native_result.binding_contingencies

    @property
    def lmp_contingency_congestion(self):
        return self._native_result.lmp_contingency_congestion

    @property
    def remaining_violations(self):
        return self._native_result.remaining_violations

    @property
    def failed_contingencies(self):
        return self._native_result.failed_contingencies

    @property
    def screening_stats(self):
        return self._native_result.screening_stats

    @property
    def solve_time_secs(self) -> float:
        return self._native_result.solve_time_secs

    def __getattr__(self, name: str):
        return getattr(self.opf, name)

    def __repr__(self) -> str:
        return repr(self._native_result)


def solve_dc_opf(
    network,
    options: DcOpfOptions | None = None,
    runtime: DcOpfRuntime | None = None,
) -> DcOpfResult:
    resolved_options = _require_instance("solve_dc_opf.options", options, DcOpfOptions)
    resolved_runtime = _require_instance("solve_dc_opf.runtime", runtime, DcOpfRuntime)
    kwargs = resolved_options.to_native_kwargs(network)
    kwargs.update(resolved_runtime.to_native_kwargs(network))
    return DcOpfResult(_native.solve_dc_opf_full(network, **kwargs))


def solve_ac_opf(
    network,
    options: AcOpfOptions | None = None,
    runtime: AcOpfRuntime | None = None,
) -> AcOpfResult:
    resolved_options = _require_instance("solve_ac_opf.options", options, AcOpfOptions)
    resolved_runtime = _require_instance("solve_ac_opf.runtime", runtime, AcOpfRuntime)
    kwargs = resolved_options.to_native_kwargs(network)
    kwargs.update(resolved_runtime.to_native_kwargs(network))
    kwargs["warm_start"] = _unwrap_opf_solution(kwargs.get("warm_start"))
    return AcOpfResult(_native.solve_ac_opf(network, **kwargs))


def solve_scopf(
    network,
    options: ScopfOptions | None = None,
    runtime: ScopfRuntime | None = None,
) -> ScopfResult:
    resolved_options = _require_instance("solve_scopf.options", options, ScopfOptions)
    resolved_runtime = _require_instance("solve_scopf.runtime", runtime, ScopfRuntime)
    kwargs = resolved_options.to_native_kwargs(network)
    kwargs.update(resolved_runtime.to_native_kwargs(network))
    kwargs["warm_start"] = _unwrap_scopf_result(kwargs.get("warm_start"))
    return ScopfResult(_native.solve_scopf(network, **kwargs))


__all__ = [
    "AcAngleWarmStartMode",
    "AcOpfResult",
    "AcOpfRuntime",
    "AcOpfOptions",
    "ConstraintScreening",
    "DcCostModel",
    "DcLossModel",
    "DcOpfResult",
    "DcOpfRuntime",
    "DcOpfOptions",
    "DiscreteMode",
    "GeneratorLimitMode",
    "HvdcMode",
    "ScopfFormulation",
    "ScopfMode",
    "ScopfResult",
    "ScopfRuntime",
    "ScopfScreeningPolicy",
    "ScopfOptions",
    "ThermalRating",
    "solve_ac_opf",
    "solve_dc_opf",
    "solve_scopf",
]
