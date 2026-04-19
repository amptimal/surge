# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Policy knobs for a battery price-taker optimisation.

Two orthogonal flags control the solve shape:

* :attr:`dispatch_mode` — how the battery decides when to move.
* :attr:`period_coupling` — how periods are linked.

Four valid combinations:

+---------------------+-------------------------------------+-------------------------------------+
|                     | coupled                             | sequential                          |
+=====================+=====================================+=====================================+
| optimal_foresight   | One LP over all periods; BESS is    | N single-period LPs; BESS is        |
|                     | zero-cost, SOC dynamics link        | zero-cost. SOC carries forward.     |
|                     | periods. Extracts max arbitrage     | Myopic — misses inter-period        |
|                     | with perfect forecast.              | arbitrage the coupled LP captures.  |
+---------------------+-------------------------------------+-------------------------------------+
| pwl_offers          | BESS's PWL discharge offer / charge | Same bids, but one period at a      |
|                     | bid curves constrain dispatch       | time. Simulates RTO/ISO real-time   |
|                     | against the LMP forecast over the   | myopic clearing — each interval     |
|                     | whole horizon.                      | sees only its own LMP.              |
+---------------------+-------------------------------------+-------------------------------------+

Use ``optimal_foresight / coupled`` for the theoretical revenue
ceiling; ``pwl_offers / sequential`` for the most realistic
simulation of how a bid-submitting operator fares in a sequentially
cleared RTM.
"""

from __future__ import annotations

from dataclasses import dataclass


#: Dispatch modes — how the battery decides to move.
DISPATCH_MODES = ("optimal_foresight", "pwl_offers")

#: Period-coupling modes — how periods are linked.
PERIOD_COUPLINGS = ("coupled", "sequential")


@dataclass(frozen=True)
class BatteryPolicy:
    """Solver / formulation knobs for a battery operator solve.

    Energy and AS prices are always exogenous forecasts supplied via
    :class:`BatteryProblem`. What varies across modes is *how the
    battery responds* to those prices.
    """

    #: ``"optimal_foresight"`` (default) — zero-cost BESS, LP extracts
    #: maximum arbitrage value against the LMP forecast. Use this for
    #: the revenue ceiling a perfectly-informed battery could earn.
    #:
    #: ``"pwl_offers"`` — BESS uses the static discharge-offer and
    #: charge-bid curves supplied via
    #: :attr:`BatteryProblem.pwl_strategy`. Energy dispatch gates on
    #: the LMP vs the offer / bid thresholds. Use this to simulate
    #: what the battery would earn under its submitted bid strategy.
    dispatch_mode: str = "optimal_foresight"

    #: ``"coupled"`` (default) — single time-coupled LP over all
    #: periods. Full intertemporal optimisation; SOC is a shared
    #: decision variable across periods.
    #:
    #: ``"sequential"`` — N single-period LPs chained via storage
    #: SOC overrides. Each interval's solve sees only its own LMP
    #: and AS prices. Simulates RTM myopic clearing.
    period_coupling: str = "coupled"

    #: LP solver backend. HiGHS handles the LP well; ``"default"``
    #: and ``"gurobi"`` also work.
    lp_solver: str = "highs"

    #: Bus-balance penalty multiplier (not currently wired through
    #: to the request — Surge's default 1e7 / 1e5 is already extreme
    #: enough for a 1-bus site; retained for future tuning).
    bus_balance_penalty_cost_per_mwh: float = 9_000.0

    #: Logging verbosity — "error" | "warn" | "info" | "debug".
    log_level: str = "info"

    def __post_init__(self) -> None:
        if self.dispatch_mode not in DISPATCH_MODES:
            raise ValueError(
                f"dispatch_mode must be one of {DISPATCH_MODES!r}, got "
                f"{self.dispatch_mode!r}"
            )
        if self.period_coupling not in PERIOD_COUPLINGS:
            raise ValueError(
                f"period_coupling must be one of {PERIOD_COUPLINGS!r}, got "
                f"{self.period_coupling!r}"
            )


__all__ = ["BatteryPolicy", "DISPATCH_MODES", "PERIOD_COUPLINGS"]
