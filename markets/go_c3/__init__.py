# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""GO Competition Challenge 3 market design.

This package is the declarative spec for the GO C3 market. The entry
points are:

* :class:`GoC3Policy`  — solver / formulation knobs.
* :func:`default_config` — default :class:`surge.market.MarketConfig`.
* :class:`GoC3Problem` — GO C3 problem-file schema loader.
* :func:`solve` — canonical SCUC → AC-SCED solve for a problem file.

Example::

    from pathlib import Path
    from markets.go_c3 import GoC3Policy, solve

    report = solve(
        problem_path=Path("scenario.json"),
        workdir=Path("out/run-1"),
        policy=GoC3Policy(lp_solver="gurobi", ac_reconcile_mode="ac_dispatch"),
    )
    print(report["status"], report["artifacts"]["solution"])

Suite running, validator integration, and cross-scenario comparisons
live in :mod:`benchmarks.go_c3`. The interactive dashboard lives in
:mod:`dashboards.go_c3`.
"""

from .policy import DEFAULT_COMMITMENT_MIP_REL_GAP, GoC3Policy
from .problem import GoC3Problem
from .solve import solve
from .config import default_config

__all__ = [
    "GoC3Policy",
    "GoC3Problem",
    "solve",
    "DEFAULT_COMMITMENT_MIP_REL_GAP",
    "default_config",
]
