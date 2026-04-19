# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Generator and storage offer schedule builders.

Helpers for constructing the per-resource offer schedule dicts that the
Surge dispatch solver expects.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass
class GeneratorOfferSchedule:
    """Columnar per-resource energy offer schedule.

    Ergonomic Python input for the dispatch request's
    ``market.generator_offer_schedules`` field. Each entry in
    ``segments_by_period`` is the piecewise-linear offer curve for
    one period as a list of ``(cumulative_mw, cost_per_mwh)`` tuples.

    Convert to the request dict with :meth:`to_request_dict`.
    """

    resource_id: str
    segments_by_period: list[list[tuple[float, float]]]
    no_load_cost_by_period: list[float] | None = None
    startup_cost_tiers: list[dict[str, Any]] | None = None

    def to_request_dict(self, periods: int) -> dict[str, Any]:
        """Render into the request payload shape the solver consumes."""
        if len(self.segments_by_period) != periods:
            raise ValueError(
                f"generator {self.resource_id}: segments_by_period has "
                f"{len(self.segments_by_period)} entries, expected {periods}"
            )
        periods_list: list[dict[str, Any]] = []
        for t in range(periods):
            period_entry: dict[str, Any] = {
                "segments": [
                    [float(mw), float(cost)] for mw, cost in self.segments_by_period[t]
                ],
            }
            if self.no_load_cost_by_period is not None:
                period_entry["no_load_cost"] = float(self.no_load_cost_by_period[t])
            if self.startup_cost_tiers is not None:
                period_entry["startup_tiers"] = list(self.startup_cost_tiers)
            periods_list.append(period_entry)
        return {
            "resource_id": self.resource_id,
            "schedule": {"periods": periods_list},
        }


@dataclass
class GeneratorReserveOfferSchedule:
    """Columnar per-resource reserve offer schedule.

    Ergonomic Python input for the request's
    ``market.generator_reserve_offer_schedules`` field. Each entry in
    ``offers_by_period`` is the list of reserve-product offer dicts for
    one period (use :func:`reserve_offer` to build them).

    Convert to the request dict with :meth:`to_request_dict`.
    """

    resource_id: str
    offers_by_period: list[list[dict[str, Any]]]

    def to_request_dict(self, periods: int) -> dict[str, Any]:
        if len(self.offers_by_period) != periods:
            raise ValueError(
                f"generator {self.resource_id}: offers_by_period has "
                f"{len(self.offers_by_period)} entries, expected {periods}"
            )
        return {
            "resource_id": self.resource_id,
            "schedule": {
                "periods": [list(period_offers) for period_offers in self.offers_by_period]
            },
        }


def piecewise_linear_offer(
    resource_id: str,
    segments_by_period: list[list[tuple[float, float]]],
    *,
    no_load_cost: float | list[float] = 0.0,
    startup_tiers: list[dict[str, Any]] | None = None,
    startup_tiers_by_period: list[list[dict[str, Any]]] | None = None,
    tiebreak_epsilon: float = 0.0,
) -> dict[str, Any]:
    """Build a generator offer schedule dict.

    Args:
        resource_id: Generator resource ID (must match network).
        segments_by_period: Per-period list of ``(cumulative_mw, marginal_cost)``
            tuples defining the piecewise-linear cost curve.
        no_load_cost: No-load cost ($/hr), scalar or per-period list.
        startup_tiers: Startup cost tiers (applied to all periods).
            Each dict: ``{"max_offline_hours": float, "cost": float}``.
        startup_tiers_by_period: Per-period startup tiers (overrides ``startup_tiers``).
        tiebreak_epsilon: Small per-block cost increment to break LP degeneracy.

    Returns:
        A dict suitable for inclusion in ``market.generator_offer_schedules``.
    """
    periods_list: list[dict[str, Any]] = []
    n_periods = len(segments_by_period)

    for t in range(n_periods):
        segments = segments_by_period[t]
        if tiebreak_epsilon > 0:
            segments = [
                (mw, cost + i * tiebreak_epsilon)
                for i, (mw, cost) in enumerate(segments)
            ]

        period_entry: dict[str, Any] = {
            "segments": [[mw, cost] for mw, cost in segments],
        }

        nlc = no_load_cost[t] if isinstance(no_load_cost, list) else no_load_cost
        period_entry["no_load_cost"] = nlc

        if startup_tiers_by_period is not None and t < len(startup_tiers_by_period):
            period_entry["startup_tiers"] = startup_tiers_by_period[t]
        elif startup_tiers is not None:
            period_entry["startup_tiers"] = startup_tiers

        periods_list.append(period_entry)

    return {
        "resource_id": resource_id,
        "schedule": {"periods": periods_list},
    }


def cost_blocks_to_segments(
    blocks: list[tuple[float, float]],
    base_mva: float = 100.0,
    *,
    tiebreak_epsilon: float = 0.0,
) -> list[tuple[float, float]]:
    """Convert ``(marginal_cost_per_pu, block_size_pu)`` blocks to cumulative MW segments.

    This is the standard conversion from GO-style cost blocks
    ``[price_$/pu, qty_pu]`` to the ``(cumulative_mw, $/MWh)`` format
    that Surge expects.

    Args:
        blocks: List of ``(marginal_cost_per_pu, block_size_pu)`` tuples.
        base_mva: System MVA base for pu→MW conversion.
        tiebreak_epsilon: Per-block cost perturbation.

    Returns:
        List of ``(cumulative_mw, cost_per_mwh)`` tuples.
    """
    segments: list[tuple[float, float]] = []
    cumulative_mw = 0.0
    for i, (marginal_cost, block_size_pu) in enumerate(blocks):
        block_mw = float(block_size_pu) * base_mva
        if block_mw <= 1e-12:
            continue
        cumulative_mw += block_mw
        cost_per_mwh = float(marginal_cost) / base_mva if abs(base_mva) > 1e-12 else float(marginal_cost)
        segments.append((cumulative_mw, cost_per_mwh + i * tiebreak_epsilon))
    return segments


def reserve_offer(
    product_id: str,
    capacity_mw: float,
    cost_per_mwh: float = 0.0,
) -> dict[str, Any]:
    """Build a single reserve offer dict for a resource."""
    return {
        "product_id": product_id,
        "capacity_mw": capacity_mw,
        "cost_per_mwh": cost_per_mwh,
    }


def reserve_offer_schedule(
    resource_id: str,
    offers_by_period: list[list[dict[str, Any]]],
) -> dict[str, Any]:
    """Build a per-resource reserve offer schedule dict.

    Args:
        resource_id: Resource ID.
        offers_by_period: Per-period list of reserve offer dicts
            (from ``reserve_offer()``).
    """
    return {
        "resource_id": resource_id,
        "schedule": {"periods": offers_by_period},
    }
