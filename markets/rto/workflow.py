# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""MarketWorkflow construction for the RTO day-ahead market.

The DAM runs as a single SCUC stage whose internal
``runtime.run_pricing`` flag is what drives LMP extraction. A caller
who wants a distinct pricing stage (re-LP with commitment pinned)
can use :func:`build_workflow` and pass ``run_pricing_stage=True`` —
the second stage re-solves as an LP with the SCUC commitment fixed.
"""

from __future__ import annotations

import copy
import logging
import time
from typing import Any

from surge.market import (
    MarketWorkflow,
    WorkflowContext,
    WorkflowStage,
    WorkflowStageResult,
    WorkflowStageRole,
    extract_fixed_commitment,
)

from .config import default_config, default_reserve_products
from .policy import RtoPolicy
from .problem import RtoProblem

logger = logging.getLogger("markets.rto.workflow")


def build_workflow(
    problem: RtoProblem,
    policy: RtoPolicy,
    *,
    run_pricing_stage: bool = False,
) -> tuple[MarketWorkflow, dict[str, Any]]:
    """Build the day-ahead workflow and the base request dict.

    ``run_pricing_stage=True`` adds a second LP stage that re-solves
    with commitment pinned to the SCUC outcome. This is only useful
    when ``policy.commitment_mode == "optimize"`` — otherwise the SCUC
    is already an LP and a second pass is a no-op.
    """
    config = default_config(policy)
    reserve_products = [p.to_product_dict() for p in default_reserve_products(policy)]
    base_request = problem.build_request(
        config=config, policy=policy, reserve_products=reserve_products
    )

    stages: list[WorkflowStage] = [
        WorkflowStage(
            stage_id="dam_scuc",
            role=WorkflowStageRole.UNIT_COMMITMENT,
            execute=_make_scuc_executor(problem, policy, base_request),
            description="Day-ahead SCUC (MIP) with LMP repricing via runtime.run_pricing",
        )
    ]

    if (
        run_pricing_stage
        and policy.commitment_mode == "optimize"
    ):
        stages.append(
            WorkflowStage(
                stage_id="dam_pricing",
                role=WorkflowStageRole.PRICING,
                execute=_make_pricing_executor(problem, policy, base_request),
                description="Pricing LP: commitment fixed from dam_scuc",
            )
        )

    return MarketWorkflow(stages=stages), base_request


def _make_scuc_executor(problem: RtoProblem, policy: RtoPolicy, base_request: dict):
    def execute(context: WorkflowContext) -> WorkflowStageResult:
        t0 = time.perf_counter()
        request = copy.deepcopy(base_request)
        logger.info(
            "DAM SCUC: solving %d periods, commitment=%s, lp_solver=%s",
            problem.periods,
            policy.commitment_mode,
            policy.lp_solver,
        )
        result = context.surge_module.solve_dispatch(
            problem.network, request, lp_solver=policy.lp_solver
        )
        elapsed = time.perf_counter() - t0
        total_cost = result.summary.get("total_cost")
        logger.info("DAM SCUC: solved in %.2fs · total_cost=%s", elapsed, total_cost)
        return WorkflowStageResult(
            stage_id="dam_scuc",
            role=WorkflowStageRole.UNIT_COMMITMENT,
            result=result,
            request=request,
            network=problem.network,
            metadata={"elapsed_secs": elapsed},
        )

    return execute


def _make_pricing_executor(problem: RtoPolicy, policy: RtoPolicy, base_request: dict):
    """Build a pricing-stage executor that re-solves with commitment pinned."""

    def execute(context: WorkflowContext) -> WorkflowStageResult:
        t0 = time.perf_counter()
        scuc = context.stage("dam_scuc").result
        pinned_request = copy.deepcopy(base_request)
        pinned_request["commitment"] = {
            "fixed": {"resources": extract_fixed_commitment(scuc)}
        }
        # runtime.run_pricing already True; keep it.
        logger.info("DAM pricing: re-solving LP with commitment pinned from SCUC")
        result = context.surge_module.solve_dispatch(
            problem.network, pinned_request, lp_solver=policy.lp_solver
        )
        elapsed = time.perf_counter() - t0
        logger.info("DAM pricing: solved in %.2fs", elapsed)
        return WorkflowStageResult(
            stage_id="dam_pricing",
            role=WorkflowStageRole.PRICING,
            result=result,
            request=pinned_request,
            network=problem.network,
            metadata={"elapsed_secs": elapsed},
        )

    return execute


__all__ = ["build_workflow"]
