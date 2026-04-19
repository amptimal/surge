# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Problem dataclass + canonical DispatchRequest builder.

A market problem's job is to:

1. **Own the scenario input** — any forecasts, requirements, or site
   parameters the dispatch needs. Store these as typed fields, not
   opaque dicts.
2. **Build (or take) a** :class:`surge.Network` — the topology the
   dispatch solves on. Three common patterns:

   * RTO-style: caller passes in a pre-built network (e.g.
     ``surge.case118()``). The problem reads it read-only.
   * Battery-style: the problem constructs a small in-memory
     network (``surge.Network(base_mva=100.0)`` + ``add_bus`` /
     ``add_generator`` / ``add_storage``).
   * Hybrid: caller passes a base network, problem mutates it.

3. **Assemble the canonical** :class:`DispatchRequest` dict
   (``build_request``) — the native Surge input that
   :func:`surge.solve_dispatch` consumes.

The `Problem` class below is a *runnable* minimal market: a 1-bus
network with one generator and one load. Copy-paste it and fill in
your market's topology, forecasts, and offer schedule. The template
smoke test (``tests/test_template_smoke.py``) exercises this
skeleton end-to-end to catch contract drift.

See :class:`markets.rto.problem.RtoProblem` for a rich-input example
and :class:`markets.battery.problem.BatteryProblem` for an in-memory
network builder.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from surge.market import request

from .config import default_config


@dataclass
class Problem:
    """Scenario input for this market.

    Required fields should cover every input the solve needs. Keep
    types concrete — no ``dict`` catch-alls.
    """

    # Time axis: required for every dispatch request.
    period_durations_hours: list[float] = field(default_factory=lambda: [1.0, 1.0])

    #: Per-period load (MW) at the single bus. Replace with your
    #: market's forecast shape (per-bus dict, DataFrame, etc.).
    load_mw_by_period: list[float] = field(default_factory=lambda: [50.0, 60.0])

    #: Caller-supplied :class:`surge.Network`. If ``None``, the
    #: problem builds a minimal 1-bus / 1-gen network itself.
    network: Any = None

    @property
    def periods(self) -> int:
        return len(self.period_durations_hours)

    def __post_init__(self) -> None:
        if self.periods <= 0:
            raise ValueError("period_durations_hours must be non-empty")
        if len(self.load_mw_by_period) != self.periods:
            raise ValueError(
                f"load_mw_by_period has {len(self.load_mw_by_period)} entries, "
                f"expected {self.periods}"
            )

    # -- Network construction --------------------------------------------

    def build_network(self, policy: Any = None) -> Any:
        """Build the :class:`surge.Network` for this problem."""
        if self.network is not None:
            return self.network
        import surge  # type: ignore

        net = surge.Network(base_mva=100.0)
        net.add_bus(number=1, bus_type="Slack", base_kv=138.0)
        net.add_generator(
            bus=1,
            p_mw=0.0,
            pmax_mw=200.0,
            pmin_mw=0.0,
            vs_pu=1.0,
            qmax_mvar=100.0,
            qmin_mvar=-100.0,
            machine_id="1",
            id="gen_1",
        )
        net.set_generator_cost("gen_1", coeffs=[0.0, 30.0, 0.0])
        net.add_load(bus=1, pd_mw=0.0, qd_mvar=0.0)
        return net

    # -- Request builder -------------------------------------------------

    def build_request(self, policy: Any) -> dict[str, Any]:
        """Assemble the canonical :class:`DispatchRequest` dict.

        Built via :func:`surge.market.request` — a chainable typed
        builder. ``market_config(cfg)`` fills missing penalty /
        network-rule defaults without clobbering any fields the
        builder has already set.
        """
        return (
            request()
            .timeline(
                periods=self.periods,
                hours_by_period=self.period_durations_hours,
            )
            .commitment_all_committed()
            .coupling("period_by_period")
            .load_profile(bus=1, values=self.load_mw_by_period)
            .market_config(default_config(policy))
            .build()
        )


__all__ = ["Problem"]
