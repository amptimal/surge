# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Interactive dashboard for the ``markets/battery/`` market.

Live single-page app: edit prices, asset parameters, and bid strategy
with sliders and draggable charts; solve in <1 s; see revenue /
SOC / dispatch / AS awards instantly.
"""

from .server.app import create_app

__all__ = ["create_app"]
