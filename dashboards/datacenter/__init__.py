# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Interactive dashboard for the ``markets/datacenter/`` market.

Live single-page app: edit forecasts (LMP, AS prices, IT load,
renewable capacity factors), tune asset specs (BESS, solar, wind,
fuel cell, gas CT, diesel, optional nuclear), toggle 4-CP days, and
re-solve the SCUC MIP in real time. Renders dispatch stack, per-
resource P&L, BESS SOC + AS awards, sensitivity sliders, 4-CP
exposure, and a thermal-commitment Gantt.
"""

from .server.app import create_app

__all__ = ["create_app"]
