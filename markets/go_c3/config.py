# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Market configuration preset for GO Competition Challenge 3.

The GO C3 market's penalty tensor, network rules, and AC-reconcile
settings are expressed as a :class:`surge.market.MarketConfig`.
Most callers do not need to instantiate this directly — the canonical
solve path (:mod:`markets.go_c3.solve`) delegates to Rust which
applies the preset internally. It is exposed here so new markets can
see the full shape of what a market declares.
"""

from __future__ import annotations

from surge.market import MarketConfig


def default_config(
    base_mva: float = 100.0,
    *,
    s_vio_cost: float = 500.0,
    p_bus_vio_cost: float = 1_000_000.0,
    q_bus_vio_cost: float | None = None,
    e_vio_cost: float = 0.0,
    max_bid_cost: float = 0.0,
) -> MarketConfig:
    """Return the canonical GO C3 market configuration.

    Thin wrapper around :meth:`surge.market.MarketConfig.default` with
    the GO C3 penalty tensor (thermal, voltage, bus balance, ramp,
    angle, reserve), benders slack costs, and network rules.
    """
    return MarketConfig.default(
        base_mva,
        s_vio_cost=s_vio_cost,
        p_bus_vio_cost=p_bus_vio_cost,
        q_bus_vio_cost=q_bus_vio_cost,
        e_vio_cost=e_vio_cost,
        max_bid_cost=max_bid_cost,
    )


__all__ = ["default_config"]
