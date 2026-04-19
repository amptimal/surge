# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Interactive dashboard for the ``markets/rto/`` day-ahead market.

Two entry paths:

* **Built-in / network-only.** Pick an IEEE case (9, 14, 30, 57, 118,
  300) or a GO-C3 case (73, 617). The dashboard synthesizes every RTO
  market input — timeline, loads, offer curves, reserve requirements,
  renewable caps — from sensible defaults the user then tweaks.

* **Full case.** Load a pre-packaged RTO scenario (native JSON export
  from this dashboard). Everything is already populated; the user
  overrides policy and re-solves.

The dashboard calls :func:`markets.rto.solve` with the
assembled :class:`RtoProblem` and returns settlement, dispatch, and
violation data flattened for the browser.
"""

from .server.app import create_app

__all__ = ["create_app"]
