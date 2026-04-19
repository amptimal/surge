# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Dispatch API surface."""

from __future__ import annotations

from collections.abc import Mapping
from os import PathLike
from typing import Any

from ._surge import DispatchResult as DispatchResult
from ._surge import Network as Network


class ActivsgTimeSeries:
    @property
    def case(self) -> str: ...
    @property
    def periods(self) -> int: ...
    @property
    def timestamps(self) -> list[str]: ...
    @property
    def report(self) -> dict[str, Any]: ...
    @property
    def generator_pmax_overrides(self) -> dict[str, float]: ...
    def timeline(self, periods: int | None = None) -> dict[str, Any]: ...
    def dc_dispatch_profiles(self, periods: int | None = None) -> dict[str, Any]: ...
    def ac_dispatch_profiles(self, periods: int | None = None) -> dict[str, Any]: ...
    def network_with_nameplate_overrides(self, network: Network) -> Network: ...


def read_tamu_activsg_time_series(
    network: Network,
    root: str | PathLike[str],
    case: str = "2000",
) -> ActivsgTimeSeries: ...


def solve_dispatch(
    network: Network,
    request: Mapping[str, Any] | str | None = None,
    *,
    lp_solver: str | None = None,
) -> DispatchResult: ...


__all__ = ["ActivsgTimeSeries", "DispatchResult", "read_tamu_activsg_time_series", "solve_dispatch"]
