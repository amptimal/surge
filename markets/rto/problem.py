# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Day-ahead RTO problem: network + forecasts + reserve requirements.

An :class:`RtoProblem` holds everything a day-ahead clearing needs
except the solver policy:

* The canonical :class:`surge.Network` (built from MATPOWER, PSS/E, or
  an in-memory case factory).
* Per-period interval durations (24 × 1 hr for a standard DAM).
* Per-bus load forecast (MW).
* Per-resource renewable availability cap (MW), for wind / solar.
* :class:`ZonalRequirement` list for the AS zones.
* Initial commitment + previous-dispatch state for ramp initialisation.
* Per-resource energy offer schedule + reserve offer schedule.

:meth:`RtoProblem.build_request` produces the canonical dispatch-request
dict a :class:`surge.Network` + :func:`surge.solve_dispatch` call consumes.
"""

from __future__ import annotations

import csv
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Mapping

from surge.market import (
    GeneratorOfferSchedule,
    GeneratorReserveOfferSchedule,
    MarketConfig,
    ZonalRequirement,
    request as _request_builder,
)

from .policy import RtoPolicy


@dataclass
class RtoProblem:
    """Day-ahead RTO problem.

    Construct directly, or use :meth:`from_dicts` / :meth:`from_csvs`.
    """

    network: Any  # surge.Network
    period_durations_hours: list[float]

    #: Bus-number → per-period MW demand forecast.
    load_forecast_mw: dict[int, list[float]] = field(default_factory=dict)

    #: Resource-id → per-period MW capacity cap (for wind / solar / hydro).
    renewable_caps_mw: dict[str, list[float]] = field(default_factory=dict)

    #: Zonal reserve requirements.
    reserve_requirements: list[ZonalRequirement] = field(default_factory=list)

    #: Resource-id → initial online status (``True``/``False``). Resources
    #: not listed inherit the network's :attr:`Generator.in_service`.
    initial_commitment: dict[str, bool] = field(default_factory=dict)

    #: Resource-id → previous-period dispatch MW (for ramp init).
    previous_dispatch_mw: dict[str, float] = field(default_factory=dict)

    #: Per-resource energy offer schedules. When a resource has no
    #: entry, the network's generator cost coefficients (c2, c1, c0)
    #: are used automatically by the solver.
    energy_offers: list[GeneratorOfferSchedule] = field(default_factory=list)

    #: Per-resource reserve offer schedules. Resources absent here
    #: cannot provide reserves.
    reserve_offers: list[GeneratorReserveOfferSchedule] = field(default_factory=list)

    @property
    def periods(self) -> int:
        return len(self.period_durations_hours)

    # -- builders --------------------------------------------------------

    def build_request(
        self,
        *,
        config: MarketConfig,
        policy: RtoPolicy,
        reserve_products: list[dict[str, Any]],
    ) -> dict[str, Any]:
        """Assemble the canonical Surge dispatch-request dict."""
        periods = self.periods
        if periods <= 0:
            raise ValueError("period_durations_hours must be non-empty")

        builder = _request_builder().timeline(
            periods=periods,
            hours_by_period=self.period_durations_hours,
        )

        # Commitment optimization and any ramp / time-coupled constraint
        # require the solver to see the periods as a single LP; a single-
        # period run can stay in the simpler ``period_by_period`` coupling.
        builder.coupling("time_coupled" if periods > 1 else "period_by_period")
        builder.run_pricing(bool(policy.run_pricing))

        self._apply_commitment(builder, policy)
        self._apply_profiles(builder, periods)
        self._apply_state(builder)
        self._apply_market(builder, reserve_products)

        # Fill in MarketConfig defaults last — preserves any market /
        # network fields the caller already set.
        builder.market_config(config)

        return builder.build()

    # -- internal payload helpers ---------------------------------------

    def _apply_commitment(self, builder: Any, policy: RtoPolicy) -> None:
        if policy.commitment_mode == "all_committed":
            builder.commitment_all_committed()
            return
        if policy.commitment_mode == "fixed_initial":
            resources = []
            for g in self.network.generators:
                rid = g.resource_id
                committed = bool(
                    self.initial_commitment.get(rid, bool(getattr(g, "in_service", True)))
                )
                resources.append(
                    {
                        "resource_id": rid,
                        "initial": committed,
                        "periods": [committed] * self.periods,
                    }
                )
            builder.commitment_fixed(resources=resources)
            return
        if policy.commitment_mode == "optimize":
            initial_conditions = (
                [
                    {"resource_id": rid, "committed": bool(on)}
                    for rid, on in self.initial_commitment.items()
                ]
                if self.initial_commitment
                else None
            )
            builder.commitment_optimize(
                mip_rel_gap=policy.mip_gap,
                time_limit_secs=policy.time_limit_secs,
                initial_conditions=initial_conditions,
            )
            return
        raise ValueError(f"unsupported commitment_mode: {policy.commitment_mode!r}")

    def _apply_profiles(self, builder: Any, periods: int) -> None:
        for bus_number, values in self.load_forecast_mw.items():
            if len(values) != periods:
                raise ValueError(
                    f"load forecast for bus {bus_number} has {len(values)} "
                    f"values but problem has {periods} periods"
                )
            builder.load_profile(bus=int(bus_number), values=values)

        if self.renewable_caps_mw:
            pmax_by_rid = {g.resource_id: float(g.pmax_mw) for g in self.network.generators}
            for resource_id, caps in self.renewable_caps_mw.items():
                if len(caps) != periods:
                    raise ValueError(
                        f"renewable cap for {resource_id} has {len(caps)} values "
                        f"but problem has {periods} periods"
                    )
                pmax = pmax_by_rid.get(str(resource_id))
                if pmax is None:
                    raise ValueError(
                        f"renewable_caps_mw references resource_id {resource_id!r} "
                        "that is not in the network"
                    )
                if pmax <= 0.0:
                    raise ValueError(
                        f"renewable resource {resource_id!r} has pmax_mw={pmax}; "
                        "cannot normalise MW caps to capacity factors"
                    )
                factors = [float(c) / pmax for c in caps]
                builder.renewable_profile(resource=str(resource_id), capacity_factors=factors)

    def _apply_state(self, builder: Any) -> None:
        if self.previous_dispatch_mw:
            builder.previous_dispatch(dict(self.previous_dispatch_mw))

    def _apply_market(
        self, builder: Any, reserve_products: list[dict[str, Any]]
    ) -> None:
        if reserve_products:
            builder.extend_market(reserve_products=[dict(p) for p in reserve_products])
        if self.reserve_requirements:
            builder.zonal_reserves(self.reserve_requirements)
        if self.energy_offers:
            builder.generator_offers(self.energy_offers)
        if self.reserve_offers:
            builder.reserve_offers(self.reserve_offers)

    # -- loaders ---------------------------------------------------------

    @classmethod
    def from_dicts(
        cls,
        network: Any,
        *,
        period_durations_hours: list[float],
        load_forecast_mw: Mapping[int, list[float]] | None = None,
        renewable_caps_mw: Mapping[str, list[float]] | None = None,
        reserve_requirements: list[ZonalRequirement] | None = None,
        initial_commitment: Mapping[str, bool] | None = None,
        previous_dispatch_mw: Mapping[str, float] | None = None,
        energy_offers: list[GeneratorOfferSchedule] | None = None,
        reserve_offers: list[GeneratorReserveOfferSchedule] | None = None,
    ) -> "RtoProblem":
        """Construct from raw Python containers (tests / notebooks)."""
        return cls(
            network=network,
            period_durations_hours=list(period_durations_hours),
            load_forecast_mw=dict(load_forecast_mw or {}),
            renewable_caps_mw=dict(renewable_caps_mw or {}),
            reserve_requirements=list(reserve_requirements or []),
            initial_commitment=dict(initial_commitment or {}),
            previous_dispatch_mw=dict(previous_dispatch_mw or {}),
            energy_offers=list(energy_offers or []),
            reserve_offers=list(reserve_offers or []),
        )

    @classmethod
    def from_csvs(
        cls,
        network: Any,
        *,
        load_csv: Path | str,
        reserves_csv: Path | str | None = None,
        renewable_csv: Path | str | None = None,
        period_durations_hours: list[float] | None = None,
    ) -> "RtoProblem":
        """Construct from disk CSVs.

        ``load_csv`` schema (header required): ``bus_number, period, value_mw``.
        ``reserves_csv``: ``zone_id, product_id, period, requirement_mw``
        (plus optional ``shortfall_cost_per_unit``).
        ``renewable_csv``: ``resource_id, period, cap_mw``.

        Period indexing is 0-based. ``period_durations_hours`` defaults
        to 1 hour × max-period+1.
        """
        load_csv = Path(load_csv)
        load_forecast = _load_per_bus_timeseries(load_csv, "bus_number", "value_mw")
        periods = max(len(v) for v in load_forecast.values()) if load_forecast else 0
        durations = list(period_durations_hours) if period_durations_hours else [1.0] * periods
        if len(durations) != periods and periods > 0:
            raise ValueError(
                f"period_durations_hours length ({len(durations)}) does not "
                f"match inferred periods ({periods})"
            )

        renewables: dict[str, list[float]] = {}
        if renewable_csv is not None:
            renewables = _load_per_resource_timeseries(
                Path(renewable_csv), "resource_id", "cap_mw"
            )

        reserves: list[ZonalRequirement] = []
        if reserves_csv is not None:
            reserves = _load_zonal_reserves(Path(reserves_csv), periods)

        return cls(
            network=network,
            period_durations_hours=durations,
            load_forecast_mw=load_forecast,
            renewable_caps_mw=renewables,
            reserve_requirements=reserves,
        )


# ---------------------------------------------------------------------------
# CSV helpers (kept free of pandas — a stdlib csv reader is all we need)
# ---------------------------------------------------------------------------


def _load_per_bus_timeseries(
    path: Path,
    key_col: str,
    value_col: str,
) -> dict[int, list[float]]:
    rows: dict[int, list[tuple[int, float]]] = {}
    with path.open("r", encoding="utf-8", newline="") as fh:
        reader = csv.DictReader(fh)
        for r in reader:
            bus = int(r[key_col])
            period = int(r["period"])
            value = float(r[value_col])
            rows.setdefault(bus, []).append((period, value))
    out: dict[int, list[float]] = {}
    for bus, pairs in rows.items():
        pairs.sort(key=lambda p: p[0])
        out[bus] = [v for _, v in pairs]
    return out


def _load_per_resource_timeseries(
    path: Path,
    key_col: str,
    value_col: str,
) -> dict[str, list[float]]:
    rows: dict[str, list[tuple[int, float]]] = {}
    with path.open("r", encoding="utf-8", newline="") as fh:
        reader = csv.DictReader(fh)
        for r in reader:
            rid = str(r[key_col])
            period = int(r["period"])
            value = float(r[value_col])
            rows.setdefault(rid, []).append((period, value))
    out: dict[str, list[float]] = {}
    for rid, pairs in rows.items():
        pairs.sort(key=lambda p: p[0])
        out[rid] = [v for _, v in pairs]
    return out


def _load_zonal_reserves(path: Path, periods: int) -> list[ZonalRequirement]:
    by_key: dict[tuple[int, str], dict[int, float]] = {}
    shortfall_by_key: dict[tuple[int, str], float] = {}
    with path.open("r", encoding="utf-8", newline="") as fh:
        reader = csv.DictReader(fh)
        for r in reader:
            key = (int(r["zone_id"]), str(r["product_id"]))
            period = int(r["period"])
            mw = float(r["requirement_mw"])
            by_key.setdefault(key, {})[period] = mw
            if "shortfall_cost_per_unit" in r and r["shortfall_cost_per_unit"]:
                shortfall_by_key[key] = float(r["shortfall_cost_per_unit"])
    requirements: list[ZonalRequirement] = []
    for (zone_id, product_id), per_period in by_key.items():
        mw = [per_period.get(t, 0.0) for t in range(periods)]
        requirements.append(
            ZonalRequirement(
                zone_id=zone_id,
                product_id=product_id,
                requirement_mw=mw[0] if mw else 0.0,
                per_period_mw=mw,
                shortfall_cost_per_unit=shortfall_by_key.get((zone_id, product_id), 0.0),
            )
        )
    return requirements


__all__ = [
    "GeneratorOfferSchedule",
    "GeneratorReserveOfferSchedule",
    "RtoProblem",
]
