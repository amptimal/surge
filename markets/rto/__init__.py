# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Day-ahead RTO market.

Single-zone or multi-zone ISO-style day-ahead energy + ancillary-services
clearing. Four-product AS (reg-up, reg-down, spinning, non-spinning),
DC-only, LMP pricing via repricing LP. Input and output are native Surge
types: :class:`surge.Network` + dispatch request dict in, native
:class:`DispatchResult` + derived settlement dict out.

Quick start::

    from pathlib import Path
    import surge
    from markets.rto import RtoPolicy, RtoProblem, solve
    from surge.market import ZonalRequirement

    network = surge.case14()
    problem = RtoProblem.from_dicts(
        network,
        period_durations_hours=[1.0] * 24,
        load_forecast_mw={4: [35.0 + 10 * (t / 23) for t in range(24)]},
        reserve_requirements=[
            ZonalRequirement(zone_id=1, product_id="reg_up",
                             requirement_mw=5.0, per_period_mw=[5.0] * 24),
        ],
    )
    report = solve(problem, Path("out/dam"), policy=RtoPolicy())
    print(report["status"], report["extras"]["settlement_summary"])
"""

from .policy import (
    DEFAULT_RESERVE_SHORTFALL_COST_PER_MWH,
    DEFAULT_THERMAL_OVERLOAD_COST_PER_MWH,
    DEFAULT_VOLL_PER_MWH,
    RtoPolicy,
)
from .problem import (
    GeneratorOfferSchedule,
    GeneratorReserveOfferSchedule,
    RtoProblem,
)
from .solve import solve
from .config import (
    RTO_DEFAULT_RESERVE_PRODUCTS,
    default_config,
    default_reserve_products,
)
from .export import extract_settlement
from .workflow import build_workflow

__all__ = [
    "RtoPolicy",
    "RtoProblem",
    "solve",
    "DEFAULT_RESERVE_SHORTFALL_COST_PER_MWH",
    "DEFAULT_THERMAL_OVERLOAD_COST_PER_MWH",
    "DEFAULT_VOLL_PER_MWH",
    "GeneratorOfferSchedule",
    "GeneratorReserveOfferSchedule",
    "RTO_DEFAULT_RESERVE_PRODUCTS",
    "build_workflow",
    "default_config",
    "default_reserve_products",
    "extract_settlement",
]
