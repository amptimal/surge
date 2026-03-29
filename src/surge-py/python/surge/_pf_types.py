# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Typed option helpers for the canonical power-flow API."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from ._opf_types import _compact_kwargs


@dataclass(kw_only=True)
class DcPfOptions:
    """Options for DC power flow.

    The DC power flow solves the linear B-theta system ``P = B' * theta``
    using the standard DC approximation (flat voltages, small angles,
    lossless branches).

    Attributes:
        headroom_slack: When True, distribute slack across all buses
            proportionally rather than using a single slack bus.
        headroom_slack_buses: Explicit list of bus numbers to use for
            headroom slack distribution. Ignored when headroom_slack
            is False.
        participation_factors: Explicit bus-number-to-weight map for
            distributed slack. Takes precedence over headroom slack.
        angle_reference: Output angle reference convention for reported
            bus angles. ``"preserve_initial"`` keeps each island's
            reference bus at its initialized angle, ``"zero"`` forces
            each reference bus to 0, and distributed modes shift angles
            so a weighted-average reference angle is zero.
    """

    headroom_slack: bool = False
    headroom_slack_buses: list[int] | None = None
    participation_factors: dict[int, float] | None = None
    angle_reference: str = "preserve_initial"

    def to_native_kwargs(self, network: Any | None = None) -> dict[str, Any]:
        return _compact_kwargs(self.__dict__, network)


@dataclass(kw_only=True)
class AcPfOptions:
    """Options for AC Newton-Raphson power flow.

    Controls convergence behavior, reactive limits, slack distribution,
    discrete controls, and initialization strategy for the nonlinear
    AC power flow solver.

    Attributes:
        tolerance: Convergence tolerance for the power mismatch in
            per-unit on the system MVA base. Default 1e-8.
        max_iterations: Maximum Newton-Raphson iterations. Default 100.
        flat_start: When True, initialize all buses at V=1.0 p.u. and
            theta=0 instead of using case-data voltages.
        oltc: Enable on-load tap changer (OLTC) outer-loop control.
            Adjusts transformer taps to regulate voltage. Default True.
        switched_shunts: Enable switched shunt outer-loop control.
            Steps shunt susceptance blocks to regulate voltage. Default True.
        oltc_max_iter: Maximum OLTC adjustment iterations. Default 20.
        distributed_slack: When True, distribute active power mismatch
            across participating generators instead of a single slack bus.
        slack_participation: Generator-index-to-weight mapping for
            distributed slack. None uses equal participation.
        enforce_interchange: Enforce area interchange MW targets from
            network area schedules. Default False.
        interchange_max_iter: Maximum area interchange adjustment
            iterations. Default 10.
        enforce_q_limits: Enable PV-to-PQ switching when generators
            reach reactive power limits (qmin/qmax). Default True.
        enforce_gen_p_limits: Clamp generator active power to
            [pmin, pmax] bounds. Default True.
        merge_zero_impedance: Merge zero-impedance branches before
            solving, reducing numerical difficulty. Default False.
        dc_warm_start: Initialize voltage angles from a DC power flow
            before the NR solve. Default True.
        startup_policy: Initialization strategy. "single" runs one
            solve attempt. "adaptive" escalates through fallbacks.
            "parallel_warm_and_flat" races warm and flat starts.
        q_sharing: Reactive power sharing among generators at the same
            bus. "capability" (default) shares proportional to Qmax-Qmin.
            "mbase" shares proportional to machine MVA base.
            "equal" shares equally.
        warm_start: Optional WarmStart object with explicit initial
            voltage magnitudes and angles. Overrides flat_start and
            dc_warm_start when provided.
        line_search: Enable step-size damping for improved robustness
            on difficult cases. Default True.
        detect_islands: Detect electrically disconnected islands and
            solve each independently. Default True.
        dc_line_model: Treatment of HVDC lines. "fixed_schedule"
            (default) uses the scheduled setpoint as constant P/Q
            injections. "sequential_ac_dc" runs full AC/DC iteration.
        record_convergence_history: Store per-iteration mismatch values
            in the result for diagnostic analysis. Default False.
        vm_min: Lower voltage magnitude clamp during iteration (p.u.).
            Prevents voltage collapse during early iterations. Default 0.5.
        vm_max: Upper voltage magnitude clamp during iteration (p.u.).
            Default 1.5.
        angle_reference: Output angle reference convention.
            "preserve_initial" (default) keeps the original slack angle.
            "zero" shifts all angles so the slack bus is at zero.
            ``"distributed"``, ``"distributed_load"``,
            ``"distributed_generation"``, and ``"distributed_inertia"``
            apply the corresponding distributed reference policy.
    """

    tolerance: float = 1e-8
    max_iterations: int = 100
    flat_start: bool = False
    oltc: bool = True
    switched_shunts: bool = True
    oltc_max_iter: int = 20
    distributed_slack: bool = False
    slack_participation: dict[int, float] | None = None
    enforce_interchange: bool = False
    interchange_max_iter: int = 10
    enforce_q_limits: bool = True
    enforce_gen_p_limits: bool = True
    merge_zero_impedance: bool = False
    dc_warm_start: bool = True
    startup_policy: str = "single"
    q_sharing: str = "capability"
    warm_start: Any | None = None
    line_search: bool = True
    detect_islands: bool = True
    dc_line_model: str = "fixed_schedule"
    record_convergence_history: bool = False
    vm_min: float = 0.5
    vm_max: float = 1.5
    angle_reference: str = "preserve_initial"

    def to_native_kwargs(self, network: Any | None = None) -> dict[str, Any]:
        return _compact_kwargs(self.__dict__, network)


@dataclass(kw_only=True)
class FdpfOptions:
    """Options for Fast Decoupled Power Flow (FDPF).

    FDPF exploits the approximate P-theta / Q-V decoupling in
    high-voltage transmission networks. Faster per iteration than
    Newton-Raphson but less robust and less exact.

    Attributes:
        tolerance: Convergence tolerance in per-unit. Default 1e-6.
        max_iterations: Maximum half-iterations. Default 100.
        flat_start: Initialize from flat start (V=1.0, theta=0).
            Default True.
        variant: Decoupling variant. "xb" (default) uses B'=-1/x for
            the P-theta subproblem, suitable for typical HV transmission.
            "bx" uses the full B matrix, more robust for higher R/X ratios.
        enforce_q_limits: Enable PV-to-PQ reactive limit switching.
            Default True.
    """

    tolerance: float = 1e-6
    max_iterations: int = 100
    flat_start: bool = True
    variant: str = "xb"
    enforce_q_limits: bool = True

    def to_native_kwargs(self, network: Any | None = None) -> dict[str, Any]:
        return _compact_kwargs(self.__dict__, network)


__all__ = ["AcPfOptions", "DcPfOptions", "FdpfOptions"]
