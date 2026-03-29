# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Shared typed study inputs used across optimization and dispatch APIs."""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum


class VirtualBidDirection(str, Enum):
    INC = "inc"
    DEC = "dec"


@dataclass(frozen=True, kw_only=True)
class VirtualBid:
    """Virtual energy bid for day-ahead dispatch or DC-OPF studies."""

    position_id: str
    bus: int
    period: int
    mw_limit: float
    price_per_mwh: float
    direction: VirtualBidDirection = VirtualBidDirection.INC
    in_service: bool = True

    def to_native(self, _network: object | None = None) -> dict[str, object]:
        return {
            "position_id": self.position_id,
            "bus": self.bus,
            "period": self.period,
            "mw_limit": self.mw_limit,
            "price_per_mwh": self.price_per_mwh,
            "direction": self.direction.value,
            "in_service": self.in_service,
        }


@dataclass(frozen=True, kw_only=True)
class ParSetpoint:
    """Phase-angle regulator flow target for one branch."""

    from_bus: int
    to_bus: int
    circuit: str
    target_mw: float

    def to_native(self, _network: object | None = None) -> dict[str, object]:
        return {
            "from_bus": self.from_bus,
            "to_bus": self.to_bus,
            "circuit": self.circuit,
            "target_mw": self.target_mw,
        }


@dataclass(frozen=True, kw_only=True)
class HvdcOpfLink:
    """HVDC transfer corridor co-optimized inside DC-OPF studies."""

    from_bus: int
    to_bus: int
    p_dc_min_mw: float
    p_dc_max_mw: float
    name: str = ""
    loss_a_mw: float = 0.0
    loss_b_frac: float = 0.0

    def to_native(self, _network: object | None = None) -> dict[str, object]:
        return {
            "from_bus": self.from_bus,
            "to_bus": self.to_bus,
            "p_dc_min_mw": self.p_dc_min_mw,
            "p_dc_max_mw": self.p_dc_max_mw,
            "name": self.name,
            "loss_a_mw": self.loss_a_mw,
            "loss_b_frac": self.loss_b_frac,
        }


__all__ = [
    "HvdcOpfLink",
    "ParSetpoint",
    "VirtualBid",
    "VirtualBidDirection",
]
