# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Entry point: solve a day-ahead RTO market problem."""

from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Any

import surge
from surge.market import WorkflowRunner, run_market_solve

from .export import extract_settlement
from .policy import RtoPolicy
from .problem import RtoProblem
from .workflow import build_workflow

logger = logging.getLogger("markets.rto.solve")


def solve(
    problem: RtoProblem,
    workdir: Path,
    *,
    policy: RtoPolicy | None = None,
    run_pricing_stage: bool = False,
    label: str | None = None,
) -> dict[str, Any]:
    """Clear a day-ahead RTO market and write artifacts to *workdir*.

    Writes:

    * ``run-report.json`` — status, timing, policy, per-period summary.
    * ``settlement.json`` — LMPs, AS prices, energy / AS awards and payments.
    * ``dispatch-result.json`` — native ``DispatchResult.to_dict()`` for
      downstream tools (violation reports, dashboards).

    Returns the run-report dict.
    """
    policy = policy or RtoPolicy()
    workflow, _ = build_workflow(problem, policy, run_pricing_stage=run_pricing_stage)

    def build() -> Any:
        return WorkflowRunner().run(workflow, surge_module=surge)

    def artifacts_from(workflow_result: Any, workdir: Path) -> dict[str, Path | None]:
        final_stage = workflow_result.stages[-1]
        dispatch_result = final_stage.result

        dispatch_path: Path | None = None
        if hasattr(dispatch_result, "to_dict"):
            dispatch_path = workdir / "dispatch-result.json"
            dispatch_path.write_text(
                json.dumps(dispatch_result.to_dict()) + "\n", encoding="utf-8"
            )

        settlement_path: Path | None = None
        if dispatch_result is not None:
            settlement = extract_settlement(dispatch_result, problem)
            settlement_path = workdir / "settlement.json"
            settlement_path.write_text(
                json.dumps(settlement, indent=2) + "\n", encoding="utf-8"
            )

        return {
            "dispatch_result": dispatch_path,
            "settlement": settlement_path,
        }

    def extras_from(workflow_result: Any) -> dict[str, Any]:
        final_stage = workflow_result.stages[-1]
        dispatch_result = final_stage.result
        settlement = (
            extract_settlement(dispatch_result, problem)
            if dispatch_result is not None
            else None
        )
        return {
            "periods": problem.periods,
            "stages": [s.stage_id for s in workflow_result.stages],
            "total_cost": (
                dispatch_result.summary.get("total_cost")
                if dispatch_result is not None
                else None
            ),
            "settlement_summary": (
                {
                    "energy_payment_total": settlement["totals"]["energy_payment_dollars"],
                    "as_payment_total": settlement["totals"]["as_payment_dollars"],
                    "congestion_rent_total": settlement["totals"]["congestion_rent_dollars"],
                    "shortfall_penalty_total": settlement["totals"]["shortfall_penalty_dollars"],
                }
                if settlement is not None
                else None
            ),
        }

    return run_market_solve(
        workdir,
        policy=policy,
        label=label,
        logger_name="markets.rto",
        build=build,
        artifacts_from=artifacts_from,
        extras_from=extras_from,
    )


__all__ = ["solve"]
