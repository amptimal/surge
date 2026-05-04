# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Solve entry point for the datacenter market.

Both period-coupling modes route through ``surge.solve_dispatch`` with
``commitment_optimize`` (the SCUC MIP). Sequential mode chains
single-period SCUC solves with storage SOC carryforward and (when the
underlying request supports it) commitment-state carryforward.
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import surge
from surge.market import extract_storage_end_soc, run_market_solve

from .export import extract_pl_report, extract_pl_report_from_sequence
from .policy import DataCenterPolicy
from .problem import BESS_RESOURCE_ID, DataCenterProblem

logger = logging.getLogger("markets.datacenter.solve")


@dataclass
class _DataCenterRun:
    coupling: str
    coupled_result: Any | None = None
    sequential_results: list[Any] | None = None
    pl_report: dict[str, Any] | None = None
    periods_solved: int = 0


def solve(
    problem: DataCenterProblem,
    workdir: Path,
    *,
    policy: DataCenterPolicy | None = None,
    label: str | None = None,
) -> dict[str, Any]:
    """Solve the datacenter operator SCUC.

    Coupled mode runs one time-coupled SCUC over the entire horizon.
    Sequential mode runs N single-period SCUCs with state carryforward
    (storage SOC; commitment state on units with min-up/min-down can
    be lost between periods, so sequential mode is appropriate when
    the modelled units cycle freely or have horizon-local commitment).
    """
    policy = policy or DataCenterPolicy()

    def build() -> _DataCenterRun:
        run = _DataCenterRun(coupling=policy.period_coupling)
        if policy.period_coupling == "coupled":
            run.coupled_result = _solve_coupled(problem, policy)
            run.pl_report = extract_pl_report(run.coupled_result, problem)
            run.periods_solved = problem.periods
        else:
            run.sequential_results = _solve_sequential(problem, policy)
            run.pl_report = extract_pl_report_from_sequence(
                run.sequential_results, problem
            )
            run.periods_solved = len(run.sequential_results)
        return run

    def artifacts_from(run: _DataCenterRun, workdir: Path) -> dict[str, Path | None]:
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

        pl_path: Path | None = None
        if run.pl_report is not None:
            pl_path = workdir / "pnl-report.json"
            pl_path.write_text(
                json.dumps(run.pl_report, indent=2) + "\n", encoding="utf-8"
            )

        return {"dispatch_result": dispatch_path, "pl_report": pl_path}

    def extras_from(run: _DataCenterRun) -> dict[str, Any]:
        return {
            "periods": problem.periods,
            "periods_solved": run.periods_solved,
            "pl_summary": run.pl_report["totals"] if run.pl_report else None,
        }

    return run_market_solve(
        workdir,
        policy=policy,
        label=label,
        logger_name="markets.datacenter",
        build=build,
        artifacts_from=artifacts_from,
        extras_from=extras_from,
    )


def _solve_coupled(problem: DataCenterProblem, policy: DataCenterPolicy) -> Any:
    network = problem.build_network()
    request = problem.build_request(policy)
    logger.info(
        "datacenter coupled SCUC: %d periods, commitment=%s, lp_solver=%s",
        problem.periods,
        policy.commitment_mode,
        policy.lp_solver,
    )
    return surge.solve_dispatch(network, request, lp_solver=policy.lp_solver)


def _solve_sequential(
    problem: DataCenterProblem, policy: DataCenterPolicy
) -> list[Any]:
    """N single-period SCUC solves chained via storage SOC override.

    Each period solves an independent commitment problem — units may
    cycle freely between periods unless their min-up/min-down
    constraints make sequential solving infeasible. Use the
    ``coupled`` mode for fleets with binding inter-period commitment.
    """
    results: list[Any] = []
    soc = (
        problem.site.bess.initial_soc_mwh
        if problem.site.bess.initial_soc_mwh is not None
        else 0.5 * problem.site.bess.energy_mwh
    )
    for t in range(problem.periods):
        network = problem.build_network(initial_soc_override_mwh=soc)
        request = problem.build_request(policy, period_slice=slice(t, t + 1))
        logger.info(
            "datacenter sequential SCUC: period %d/%d (init_soc=%.2f MWh)",
            t + 1,
            problem.periods,
            soc,
        )
        result = surge.solve_dispatch(network, request, lp_solver=policy.lp_solver)
        results.append(result)
        soc = (
            extract_storage_end_soc(result, BESS_RESOURCE_ID, fallback=soc) or soc
        )
    return results


__all__ = ["solve"]
