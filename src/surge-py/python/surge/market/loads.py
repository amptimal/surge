# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Dispatchable load declarations + the block-builder for flexible consumers.

Two public surfaces:

* :class:`DispatchableLoadSpec` — a typed dataclass that declares one
  dispatchable-load resource.  Replaces the hand-rolled dict literal
  that battery / RTO markets used to embed in their ``_market_payload``.
* :class:`DispatchableLoadOfferSchedule` — per-period override schedule
  for a declared DL, analogous to
  :class:`~surge.market.GeneratorOfferSchedule`.
* :func:`build_dispatchable_load_blocks` — still here for splitting a
  flexible consumer (`[p_lb, p_ub]` with a stacked cost curve) into
  ordered LP tranches.  Used by the GO C3 adapter.

Cost models are built via small factory helpers
(:func:`linear_curtailment`, :func:`quadratic_utility`,
:func:`piecewise_linear_utility`, :func:`interrupt_penalty`) so callers
don't need to know the tagged-enum shape Rust expects.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal, Sequence


# ---------------------------------------------------------------------------
# Cost-model factories (match the Rust ``LoadCostModel`` tagged enum)
# ---------------------------------------------------------------------------


def linear_curtailment(cost_per_mw: float) -> dict[str, Any]:
    """Value-of-lost-load / interruptible contract price ($/MWh).

    Objective contribution: ``cost_per_mw * (p_sched - p_served)``.
    The LP curtails when ``LMP > cost_per_mw``.
    """
    return {"LinearCurtailment": {"cost_per_mw": float(cost_per_mw)}}


def interrupt_penalty(cost_per_mw: float) -> dict[str, Any]:
    """Per-event interrupt payment — same math as ``linear_curtailment``,
    different semantic."""
    return {"InterruptPenalty": {"cost_per_mw": float(cost_per_mw)}}


def quadratic_utility(*, a: float, b: float) -> dict[str, Any]:
    """Quadratic utility: ``U(P) = a·P − b·P²/2``. ``a`` is the choke
    price ($/MWh); ``b`` is the demand-curve slope ($/MW²·h)."""
    return {"QuadraticUtility": {"a": float(a), "b": float(b)}}


def piecewise_linear_utility(
    points: Sequence[tuple[float, float]],
) -> dict[str, Any]:
    """Piecewise-linear utility from ``(P_MW, marginal_utility_$/MWh)``
    breakpoints sorted by P."""
    return {
        "PiecewiseLinear": {
            "points": [(float(p), float(mu)) for p, mu in points],
        }
    }


# ---------------------------------------------------------------------------
# DispatchableLoadSpec — typed declaration for a single DL resource
# ---------------------------------------------------------------------------


_LOAD_ARCHETYPES = ("Curtailable", "Elastic", "Interruptible", "IndependentPQ")
LoadArchetype = Literal["Curtailable", "Elastic", "Interruptible", "IndependentPQ"]


@dataclass
class DispatchableLoadSpec:
    """Typed declaration of one dispatchable-load resource.

    The field names mirror the Rust
    :class:`surge_network::market::DispatchableLoad` struct.  Cost
    models are built via the module-level factories
    (:func:`linear_curtailment`, :func:`quadratic_utility`, ...) so
    the caller never has to know the tagged-enum shape.

    Example::

        dl = DispatchableLoadSpec(
            resource_id="grid_export",
            bus=1,
            p_sched_pu=0.5,
            p_max_pu=0.5,
            archetype="Curtailable",
            cost_model=linear_curtailment(cost_per_mw=0.0),
        )
    """

    resource_id: str
    bus: int
    p_sched_pu: float
    p_max_pu: float
    cost_model: dict[str, Any]
    archetype: LoadArchetype = "Curtailable"

    q_sched_pu: float = 0.0
    p_min_pu: float = 0.0
    q_min_pu: float = 0.0
    q_max_pu: float = 0.0
    fixed_power_factor: bool = True
    in_service: bool = True

    reserve_offers: list[dict[str, Any]] = field(default_factory=list)
    qualifications: dict[str, bool] = field(default_factory=dict)

    #: Optional ramp group — resources sharing a group are ramp-coupled.
    ramp_group: str | None = None
    ramp_up_pu_per_hr: float | None = None
    ramp_down_pu_per_hr: float | None = None
    initial_p_pu: float | None = None

    def __post_init__(self) -> None:
        if self.archetype not in _LOAD_ARCHETYPES:
            raise ValueError(
                f"archetype must be one of {_LOAD_ARCHETYPES!r}, got {self.archetype!r}"
            )

    def to_request_dict(self) -> dict[str, Any]:
        """Render as the dict the dispatch request's
        ``market.dispatchable_loads`` list consumes."""
        out: dict[str, Any] = {
            "resource_id": str(self.resource_id),
            "bus": int(self.bus),
            "p_sched_pu": float(self.p_sched_pu),
            "q_sched_pu": float(self.q_sched_pu),
            "p_min_pu": float(self.p_min_pu),
            "p_max_pu": float(self.p_max_pu),
            "q_min_pu": float(self.q_min_pu),
            "q_max_pu": float(self.q_max_pu),
            "archetype": self.archetype,
            "cost_model": dict(self.cost_model),
            "fixed_power_factor": bool(self.fixed_power_factor),
            "in_service": bool(self.in_service),
            "reserve_offers": [dict(r) for r in self.reserve_offers],
            "qualifications": dict(self.qualifications),
        }
        if self.ramp_group is not None:
            out["ramp_group"] = str(self.ramp_group)
        if self.ramp_up_pu_per_hr is not None:
            out["ramp_up_pu_per_hr"] = float(self.ramp_up_pu_per_hr)
        if self.ramp_down_pu_per_hr is not None:
            out["ramp_down_pu_per_hr"] = float(self.ramp_down_pu_per_hr)
        if self.initial_p_pu is not None:
            out["initial_p_pu"] = float(self.initial_p_pu)
        return out


@dataclass
class DispatchableLoadOfferSchedule:
    """Per-period offer-schedule override for a declared :class:`DispatchableLoadSpec`.

    Each entry in ``periods`` overrides the base declaration's
    ``p_sched_pu`` / ``p_max_pu`` / ``q_*`` / ``cost_model`` for that
    period.  Omit any field to inherit from the base declaration.
    """

    resource_id: str
    periods: list[dict[str, Any]]

    def to_request_dict(self, n_periods: int) -> dict[str, Any]:
        if len(self.periods) != n_periods:
            raise ValueError(
                f"DispatchableLoadOfferSchedule({self.resource_id!r}): "
                f"periods has {len(self.periods)} entries, expected {n_periods}"
            )
        return {
            "resource_id": str(self.resource_id),
            "schedule": {"periods": [dict(p) for p in self.periods]},
        }


# ---------------------------------------------------------------------------
# Legacy helper — splits a flexible consumer into LP tranches (GO C3 adapter)
# ---------------------------------------------------------------------------


def build_dispatchable_load_blocks(
    load_id: str,
    bus: int,
    *,
    p_floor_by_period: list[float],
    p_ceiling_by_period: list[float],
    cost_blocks_by_period: list[list[tuple[float, float]]],
    q_floor_by_period: list[float] | None = None,
    q_ceiling_by_period: list[float] | None = None,
    initial_q_pu: float = 0.0,
    reserve_offers: list[dict[str, Any]] | None = None,
    reserve_offers_by_period: list[list[dict[str, Any]]] | None = None,
    ramp_up_pu_per_hr: float | None = None,
    ramp_down_pu_per_hr: float | None = None,
    initial_served_pu: float = 0.0,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]]]:
    """Split a flexible consumer into ordered dispatchable-load blocks.

    Each cost block ``(cost_per_mwh, size_pu)`` represents a tranche of
    consumption.  The LP serves blocks from highest willingness-to-pay
    first (highest curtailment cost = highest priority to serve).

    The ``p_lb`` (floor) portion of the consumer's demand is already baked
    into the bus load profile.  Cost blocks are walked starting at the
    floor, so blocks below the floor are skipped — matching the GO C3
    validator's ``eval_convex_cost_function`` semantics.

    Args:
        load_id: Consumer/load identifier (used as prefix for block resource IDs).
        bus: Bus number where the load connects.
        p_floor_by_period: Minimum consumption per period (pu).
        p_ceiling_by_period: Maximum consumption per period (pu).
        cost_blocks_by_period: Per-period list of ``(cost_per_mwh, size_pu)``
            tuples, ordered from highest to lowest willingness-to-pay.
        q_floor_by_period: Reactive power floor per period (pu).
        q_ceiling_by_period: Reactive power ceiling per period (pu).
        initial_q_pu: Initial reactive power (pu).
        reserve_offers: Static reserve offers (same for all periods/blocks).
        reserve_offers_by_period: Per-period reserve offers.
        ramp_up_pu_per_hr: Consumer-level ramp-up limit (pu/hr).
        ramp_down_pu_per_hr: Consumer-level ramp-down limit (pu/hr).
        initial_served_pu: Prior-horizon served real power (pu).

    Returns:
        Tuple of ``(dispatchable_loads, dl_offer_schedules, dl_reserve_schedules)``.
    """
    n_periods = len(p_floor_by_period)
    q_floor = q_floor_by_period or [0.0] * n_periods
    q_ceil = q_ceiling_by_period or [0.0] * n_periods

    flex_by_period = [
        max(ub - lb, 0.0)
        for lb, ub in zip(p_floor_by_period, p_ceiling_by_period)
    ]

    # Walk cost blocks per period, skipping the floor portion
    block_specs_by_period: list[list[tuple[float, float]]] = []
    max_blocks = 0
    for t in range(n_periods):
        flex_total = flex_by_period[t]
        raw_blocks = cost_blocks_by_period[t] if t < len(cost_blocks_by_period) else []
        blocks = _walk_cost_blocks(raw_blocks, p_floor_by_period[t], flex_total)
        block_specs_by_period.append(blocks)
        max_blocks = max(max_blocks, len(blocks))

    if max_blocks == 0:
        return [], [], []

    block_resource_ids = [f"{load_id}::blk:{i:02d}" for i in range(max_blocks)]

    # Compute max block sizes for reserve sharing
    block_max_size_pu = [0.0] * max_blocks
    for period_blocks in block_specs_by_period:
        for i, (_cost, size) in enumerate(period_blocks):
            if i < max_blocks and size > block_max_size_pu[i]:
                block_max_size_pu[i] = size
    total_block_capacity = sum(block_max_size_pu)

    ramp_group = load_id if (ramp_up_pu_per_hr is not None or ramp_down_pu_per_hr is not None) else None

    dispatchable_loads: list[dict[str, Any]] = []
    dl_schedules: list[dict[str, Any]] = []
    dl_reserve_schedules: list[dict[str, Any]] = []

    for block_idx, resource_id in enumerate(block_resource_ids):
        initial_block = (
            block_specs_by_period[0][block_idx]
            if block_specs_by_period and block_idx < len(block_specs_by_period[0])
            else (0.0, 0.0)
        )
        initial_cost, initial_size = initial_block
        iq_sched, iq_min, iq_max = _block_q_params(
            0, initial_size, flex_by_period, q_floor, q_ceil, initial_q_pu,
        )

        dl: dict[str, Any] = {
            "resource_id": resource_id,
            "bus": bus,
            "p_sched_pu": initial_size,
            "q_sched_pu": iq_sched,
            "p_min_pu": 0.0,
            "p_max_pu": initial_size,
            "q_min_pu": iq_min,
            "q_max_pu": iq_max,
            "archetype": "IndependentPQ",
            "cost_model": {"LinearCurtailment": {"cost_per_mw": initial_cost}},
            "fixed_power_factor": False,
            "in_service": True,
            "reserve_group": load_id,
        }

        if ramp_group is not None:
            dl["ramp_group"] = ramp_group
            dl["initial_p_pu"] = initial_served_pu
        if ramp_up_pu_per_hr is not None:
            dl["ramp_up_pu_per_hr"] = ramp_up_pu_per_hr
        if ramp_down_pu_per_hr is not None:
            dl["ramp_down_pu_per_hr"] = ramp_down_pu_per_hr

        # Static reserve offers (shared proportionally across blocks)
        if reserve_offers and total_block_capacity > 1e-12:
            share = block_max_size_pu[block_idx] / total_block_capacity
            dl["reserve_offers"] = [
                {
                    "product_id": ro["product_id"],
                    "capacity_mw": ro["capacity_mw"] * share,
                    "cost_per_mwh": ro["cost_per_mwh"],
                }
                for ro in reserve_offers
            ]

        dispatchable_loads.append(dl)

        # Per-period schedule
        dl_schedules.append({
            "resource_id": resource_id,
            "schedule": {
                "periods": [
                    _build_block_period(
                        t, block_idx, block_specs_by_period, flex_by_period,
                        q_floor, q_ceil, initial_q_pu,
                    )
                    for t in range(n_periods)
                ],
            },
        })

        # Per-period reserve offer schedule
        if reserve_offers_by_period and total_block_capacity > 1e-12:
            share = block_max_size_pu[block_idx] / total_block_capacity
            period_offers: list[list[dict[str, Any]]] = []
            for t in range(n_periods):
                t_offers = reserve_offers_by_period[t] if t < len(reserve_offers_by_period) else []
                period_offers.append([
                    {
                        "product_id": ro["product_id"],
                        "capacity_mw": ro["capacity_mw"] * share,
                        "cost_per_mwh": ro["cost_per_mwh"],
                    }
                    for ro in t_offers
                ])
            if any(po for po in period_offers):
                dl_reserve_schedules.append({
                    "resource_id": resource_id,
                    "schedule": {"periods": period_offers},
                })

    return dispatchable_loads, dl_schedules, dl_reserve_schedules


def _walk_cost_blocks(
    raw_blocks: list[tuple[float, float]],
    p_floor: float,
    flex_total: float,
) -> list[tuple[float, float]]:
    """Walk cost blocks starting after the floor, returning flex blocks."""
    if flex_total <= 1e-12 or not raw_blocks:
        return [(0.0, flex_total)] if flex_total > 1e-12 else []

    blocks: list[tuple[float, float]] = []
    skip_remaining = max(p_floor, 0.0)
    flex_remaining = flex_total

    for cost_per_mwh, block_size_pu in raw_blocks:
        if flex_remaining <= 1e-12:
            break
        size = block_size_pu
        if skip_remaining > 1e-12:
            skip_here = min(size, skip_remaining)
            size -= skip_here
            skip_remaining -= skip_here
        if size <= 1e-12:
            continue
        usable = min(size, flex_remaining)
        if usable > 1e-12:
            blocks.append((cost_per_mwh, usable))
            flex_remaining -= usable

    if flex_remaining > 1e-9:
        blocks.append((0.0, flex_remaining))
    if not blocks:
        blocks = [(0.0, flex_total)]

    return blocks


def _block_q_params(
    t: int,
    block_size: float,
    flex_by_period: list[float],
    q_floor: list[float],
    q_ceil: list[float],
    initial_q: float,
) -> tuple[float, float, float]:
    share = block_size / flex_by_period[t] if flex_by_period[t] > 1e-12 else 0.0
    q_min = q_floor[t] * share if t < len(q_floor) else 0.0
    q_max = q_ceil[t] * share if t < len(q_ceil) else 0.0
    q_sched = max(min(initial_q * share, q_max), q_min)
    return q_sched, q_min, q_max


def _build_block_period(
    t: int,
    block_idx: int,
    block_specs_by_period: list[list[tuple[float, float]]],
    flex_by_period: list[float],
    q_floor: list[float],
    q_ceil: list[float],
    initial_q: float,
) -> dict[str, Any]:
    if block_idx < len(block_specs_by_period[t]):
        cost, size = block_specs_by_period[t][block_idx]
    else:
        cost, size = 0.0, 0.0

    q_sched, q_min, q_max = _block_q_params(
        t, size, flex_by_period, q_floor, q_ceil, initial_q,
    )
    return {
        "p_sched_pu": size,
        "p_max_pu": size,
        "q_sched_pu": q_sched,
        "q_min_pu": q_min,
        "q_max_pu": q_max,
        "cost_model": {"LinearCurtailment": {"cost_per_mw": cost}},
    }
