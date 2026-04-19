# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Battery operator market — single-site price-taker optimisation.

Given an LMP forecast and optional AS price forecasts, schedule a
battery's charge / discharge / ancillary-services awards to maximise
net revenue (energy + AS − degradation) subject to SOC dynamics.

Quick start::

    from pathlib import Path
    from markets.battery import (
        AsProduct, BatteryPolicy, BatteryProblem, SiteSpec, solve,
    )
    from surge.market import REG_UP, SPINNING

    problem = BatteryProblem(
        period_durations_hours=[1.0] * 24,
        lmp_forecast_per_mwh=[25, 22, 20, 18, 20, 25, 30, 40, 55, 60,
                              65, 70, 70, 68, 65, 60, 55, 50, 60, 75,
                              80, 70, 50, 35],
        site=SiteSpec(
            poi_limit_mw=50.0,
            bess_power_charge_mw=25.0,
            bess_power_discharge_mw=25.0,
            bess_energy_mwh=100.0,
            bess_charge_efficiency=0.90,
            bess_discharge_efficiency=0.98,
            bess_degradation_cost_per_mwh=2.0,
        ),
        as_products=[
            AsProduct(REG_UP, price_forecast_per_mwh=[8.0] * 24),
            AsProduct(SPINNING, price_forecast_per_mwh=[3.0] * 24),
        ],
    )
    report = solve(problem, Path("out/battery"), policy=BatteryPolicy())
    print(report["extras"]["revenue_summary"])

The market builds a 1-bus Surge network with the BESS, a virtual
grid-import generator, and a virtual grid-export dispatchable load.
Both virtual resources are priced at the LMP forecast per period, so
the LP's net objective equals the operator's surplus — no special
"price-taker" mode is needed in the framework.

See :mod:`markets.battery.problem` for the full modelling details.
"""

from .policy import DISPATCH_MODES, PERIOD_COUPLINGS, BatteryPolicy
from .problem import AsProduct, BatteryProblem, PwlBidStrategy, SiteSpec
from .solve import solve
from .export import extract_revenue_report, extract_revenue_report_from_sequence

__all__ = [
    "BatteryPolicy",
    "BatteryProblem",
    "solve",
    "AsProduct",
    "DISPATCH_MODES",
    "PERIOD_COUPLINGS",
    "PwlBidStrategy",
    "SiteSpec",
    "extract_revenue_report",
    "extract_revenue_report_from_sequence",
]
