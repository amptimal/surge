# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass(kw_only=True)
class DcPfOptions:
    headroom_slack: bool = False
    headroom_slack_buses: list[int] | None = None
    participation_factors: dict[int, float] | None = None
    angle_reference: str = "preserve_initial"
    def to_native_kwargs(self, network: Any | None = None) -> dict[str, Any]: ...


@dataclass(kw_only=True)
class AcPfOptions:
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
    def to_native_kwargs(self, network: Any | None = None) -> dict[str, Any]: ...


@dataclass(kw_only=True)
class FdpfOptions:
    tolerance: float = 1e-6
    max_iterations: int = 100
    flat_start: bool = True
    variant: str = "xb"
    enforce_q_limits: bool = True
    def to_native_kwargs(self, network: Any | None = None) -> dict[str, Any]: ...


__all__: list[str]
