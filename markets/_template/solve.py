# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Single-call solve entry point.

The canonical pattern is:

1. Build (or accept) the :class:`surge.Network`.
2. Build the :class:`DispatchRequest` dict.
3. Call :func:`surge.solve_dispatch` OR run a multi-stage
   :class:`surge.market.MarketWorkflow` (for SCUC → pricing, SCUC →
   AC SCED, etc).
4. Wrap it all in :func:`surge.market.run_market_solve`, which handles
   ``SolveLogger``, timing, error capture, and ``run-report.json``.

For a single-stage market, :func:`surge.solve_dispatch` is all you
need. For multi-stage markets, see
:func:`surge.market.build_dispatch_stage` + :class:`MarketWorkflow`.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import surge
from surge.market import run_market_solve

from .policy import Policy
from .problem import Problem


def solve(
    problem: Problem,
    workdir: Path,
    *,
    policy: Policy | None = None,
    label: str | None = None,
) -> dict[str, Any]:
    """Solve this market problem and write artifacts to *workdir*.

    Writes at minimum:

    * ``run-report.json`` — status, timing, policy, paths to other
      artifacts.
    * ``dispatch-result.json`` — native :class:`DispatchResult` dump.
    """
    policy = policy or Policy()

    def build() -> Any:
        network = problem.build_network(policy)
        request = problem.build_request(policy)
        return surge.solve_dispatch(network, request, lp_solver=policy.lp_solver)

    def artifacts_from(result: Any, workdir: Path) -> dict[str, Path]:
        dispatch_path = workdir / "dispatch-result.json"
        dispatch_path.write_text(json.dumps(result.to_dict()) + "\n", encoding="utf-8")
        return {"dispatch_result": dispatch_path}

    return run_market_solve(
        workdir,
        policy=policy,
        label=label,
        logger_name="markets._template",
        build=build,
        artifacts_from=artifacts_from,
        extras_from=lambda result: {
            "periods": problem.periods,
            "total_cost": result.summary.get("total_cost"),
        },
    )


__all__ = ["solve"]
