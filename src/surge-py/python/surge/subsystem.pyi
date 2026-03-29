# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._surge import Network


class Subsystem:
    name: str
    def __init__(
        self,
        network: Network,
        name: str = "",
        *,
        areas: list[int] | None = None,
        zones: list[int] | None = None,
        kv_min: float | None = None,
        kv_max: float | None = None,
        buses: list[int] | None = None,
        bus_type: str | None = None,
    ) -> None: ...
    @property
    def network(self) -> Network: ...
    @property
    def bus_numbers(self) -> list[int]: ...
    @property
    def branches(self) -> list[tuple[int, int, int]]: ...
    @property
    def tie_branches(self) -> list[tuple[int, int, int]]: ...
    @property
    def generators(self) -> list[tuple[int, str]]: ...
    @property
    def loads(self) -> list[int]: ...
    @property
    def total_load_mw(self) -> float: ...
    @property
    def total_generation_mw(self) -> float: ...
    def __len__(self) -> int: ...
    def __contains__(self, bus: int) -> bool: ...
