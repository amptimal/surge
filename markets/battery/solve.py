# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Solve entry point for the battery operator market.

Routes to :func:`_solve_coupled` or :func:`_solve_sequential` based
on :attr:`BatteryPolicy.period_coupling`. Both paths ultimately
produce the same three artifacts in *workdir*:

* ``run-report.json``
* ``revenue-report.json``
* ``dispatch-result.json`` (a list for sequential, a single object
  for coupled)
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import surge
from surge.market import extract_storage_end_soc, run_market_solve

from .export import (
    extract_revenue_report,
    extract_revenue_report_from_sequence,
)
from .policy import BatteryPolicy
from .problem import BatteryProblem

logger = logging.getLogger("markets.battery.solve")


@dataclass
class _BatteryRun:
    """Internal container threaded between build → artifacts/extras."""

    coupling: str
    coupled_result: Any | None = None
    sequential_results: list[Any] | None = None
    revenue_report: dict[str, Any] | None = None
    periods_solved: int = 0


def solve(
    problem: BatteryProblem,
    workdir: Path,
    *,
    policy: BatteryPolicy | None = None,
    label: str | None = None,
) -> dict[str, Any]:
    """Solve the battery operator problem.

    Dispatches on :attr:`BatteryPolicy.period_coupling` to either a
    single coupled LP over all periods, or N sequential one-period
    LPs chained via the storage SOC override.
    """
    policy = policy or BatteryPolicy()

    def build() -> _BatteryRun:
        run = _BatteryRun(coupling=policy.period_coupling)
        if policy.period_coupling == "coupled":
            run.coupled_result = _solve_coupled(problem, policy)
            run.revenue_report = extract_revenue_report(run.coupled_result, problem)
            run.periods_solved = problem.periods
        else:
            run.sequential_results = _solve_sequential(problem, policy)
            run.revenue_report = extract_revenue_report_from_sequence(
                run.sequential_results, problem
            )
            run.periods_solved = len(run.sequential_results)
        return run

    def artifacts_from(run: _BatteryRun, workdir: Path) -> dict[str, Path | None]:
        dispatch_path = workdir / "dispatch-result.json"
        if run.coupling == "coupled":
            payload = (
                run.coupled_result.to_dict()
                if hasattr(run.coupled_result, "to_dict")
                else {}
            )
        else:
            payload = [
                r.to_dict() if hasattr(r, "to_dict") else {}
                for r in (run.sequential_results or [])
            ]
        dispatch_path.write_text(json.dumps(payload) + "\n", encoding="utf-8")

        revenue_path: Path | None = None
        if run.revenue_report is not None:
            revenue_path = workdir / "revenue-report.json"
            revenue_path.write_text(
                json.dumps(run.revenue_report, indent=2) + "\n", encoding="utf-8"
            )

        return {"dispatch_result": dispatch_path, "revenue_report": revenue_path}

    def extras_from(run: _BatteryRun) -> dict[str, Any]:
        return {
            "periods": problem.periods,
            "periods_solved": run.periods_solved,
            "revenue_summary": (
                run.revenue_report["totals"] if run.revenue_report is not None else None
            ),
        }

    return run_market_solve(
        workdir,
        policy=policy,
        label=label,
        logger_name="markets.battery",
        build=build,
        artifacts_from=artifacts_from,
        extras_from=extras_from,
    )


# ---------------------------------------------------------------------------
# Coupled + sequential solvers
# ---------------------------------------------------------------------------


def _solve_coupled(problem: BatteryProblem, policy: BatteryPolicy) -> Any:
    """Single time-coupled LP over all periods."""
    network = problem.build_network(dispatch_mode=policy.dispatch_mode)
    request = problem.build_request(policy)
    logger.info(
        "battery coupled solve: %d periods, dispatch_mode=%s",
        problem.periods,
        policy.dispatch_mode,
    )
    return surge.solve_dispatch(network, request, lp_solver=policy.lp_solver)


def _solve_sequential(problem: BatteryProblem, policy: BatteryPolicy) -> list[Any]:
    """N one-period LPs chained via storage SOC override.

    Each period's LP sees only its own LMP and AS prices. The SOC
    from period ``t`` is read from the result's ``detail.soc_mwh``
    and passed as ``state.initial.storage_soc_overrides`` to period
    ``t + 1``.
    """
    results: list[Any] = []
    soc = (
        problem.site.bess_initial_soc_mwh
        if problem.site.bess_initial_soc_mwh is not None
        else 0.5 * problem.site.bess_energy_mwh
    )
    for t in range(problem.periods):
        network = problem.build_network(
            dispatch_mode=policy.dispatch_mode,
            initial_soc_override_mwh=soc,
            period_index=t,
        )
        request = problem.build_request(policy, period_slice=slice(t, t + 1))
        logger.info(
            "battery sequential solve: period %d/%d (init_soc=%.2f MWh)",
            t + 1,
            problem.periods,
            soc,
        )
        result = surge.solve_dispatch(network, request, lp_solver=policy.lp_solver)
        results.append(result)
        soc = extract_storage_end_soc(
            result, problem.BESS_RESOURCE_ID, fallback=soc
        ) or soc
    return results


__all__ = ["solve"]
