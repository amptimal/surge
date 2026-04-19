# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""TEMPLATE — copy this directory to ``markets/<your_market>/`` and fill in.

Re-export the public API so callers can write::

    from markets.<your_market> import Policy, Problem, solve
    from markets.<your_market> import extract_report     # if you add one

Two reference implementations to model against:

* ``markets/rto/``     — multi-participant ISO clearing with LMPs.
* ``markets/battery/`` — single-site price-taker optimisation.

Both follow the same four-required-file pattern this template demonstrates.
``config.py`` is provided here as an example — delete it for markets
whose framework-default :class:`MarketConfig` is fine (battery does this).
"""

from .policy import Policy
from .problem import Problem
from .solve import solve
from .config import default_config

__all__ = ["Policy", "Problem", "solve", "default_config"]
