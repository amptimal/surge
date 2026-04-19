# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Default :class:`surge.market.MarketConfig` for this market.

The :class:`MarketConfig` captures:

* **Penalty tensor** — thermal, voltage, power balance, ramp, angle,
  reserve (each a :class:`PenaltyCurve`).
* **Network rules** — energy windows, loss factors, thermal-limit
  enforcement, commitment transitions, ramping, topology control.
* **AC reconcile config** — only relevant if the market runs an
  AC-SCED stage.
* **Benders config** — AC-OPF Benders slack penalties.

Start from :meth:`MarketConfig.default` and override what's specific
to your market. See :mod:`markets.rto.config` for a worked example.
"""

from __future__ import annotations

from surge.market import MarketConfig

from .policy import Policy


def default_config(policy: Policy | None = None, *, base_mva: float = 100.0) -> MarketConfig:
    """Return the canonical :class:`MarketConfig` for this market."""
    _ = policy or Policy()
    return MarketConfig.default(base_mva)


__all__ = ["default_config"]
