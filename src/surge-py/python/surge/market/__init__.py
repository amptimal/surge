# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Market dispatch framework.

Provides the building blocks for building a power market: penalty/network
configuration (`MarketConfig`), reserve products, offer schedule helpers,
multi-stage workflow orchestration (`MarketWorkflow` + `WorkflowRunner`),
AC reconciliation, violation assessment, and a standard solve wrapper
(`run_market_solve`) that handles logging, timing, and `run-report.json`.

Quick start::

    from surge.market import run_market_solve

    def solve(problem, workdir, *, policy=None, label=None):
        policy = policy or MyPolicy()
        return run_market_solve(
            workdir,
            policy=policy,
            label=label,
            logger_name="markets.my_market",
            build=lambda: surge.solve_dispatch(
                problem.build_network(),
                problem.build_request(policy=policy),
                lp_solver=policy.lp_solver,
            ),
        )
"""

from .config import (
    AcReconcileConfig,
    BendersConfig,
    CommitmentTransitionRules,
    EnergyWindowRules,
    FlowgateRules,
    LossFactorRules,
    MarketConfig,
    NetworkRules,
    PenaltyConfig,
    PenaltyCurve,
    PenaltyCurveSegment,
    RampRules,
    ThermalLimitRules,
    TopologyControlRules,
)
from ..dispatch_request import DispatchRequest
from .loads import (
    DispatchableLoadOfferSchedule,
    DispatchableLoadSpec,
    LoadArchetype,
    build_dispatchable_load_blocks,
    interrupt_penalty,
    linear_curtailment,
    piecewise_linear_utility,
    quadratic_utility,
)
from .logging import LogStream, SolveLogger
from .offers import (
    GeneratorOfferSchedule,
    GeneratorReserveOfferSchedule,
    cost_blocks_to_segments,
    piecewise_linear_offer,
    reserve_offer,
    reserve_offer_schedule,
)
from .problem import MarketProblem
from .report import RunReport, write_run_report
from .request import DispatchRequestBuilder, request
from .runner import run_market_solve
from .reconcile import (
    build_all_committed_schedule,
    build_decommit_feedback,
    build_warm_start_schedule,
    extract_fixed_commitment,
    extract_storage_end_soc,
    identify_resources_at_pmin,
    pin_dispatch_bounds,
    redispatch_with_ac,
    thermal_slack_summary,
)
from .reserves import (
    ECRS,
    NON_SPINNING,
    PRODUCT_BY_ID,
    RAMP_DOWN_OFF,
    RAMP_DOWN_ON,
    RAMP_UP_OFF,
    RAMP_UP_ON,
    REACTIVE_DOWN,
    REACTIVE_UP,
    REG_DOWN,
    REG_UP,
    SPINNING,
    STANDARD_ACTIVE_PRODUCTS,
    STANDARD_ALL_PRODUCTS,
    ReserveProductDef,
    ZonalRequirement,
    build_reserve_products_dict,
)
from .violations import ViolationReport, assess_dispatch_violations_native, assess_violations
from .workflow import (
    MarketWorkflow,
    WorkflowContext,
    WorkflowResult,
    WorkflowRunner,
    WorkflowStage,
    WorkflowStageResult,
    WorkflowStageRole,
    build_dispatch_stage,
)

__all__ = [
    # Config
    "AcReconcileConfig",
    "BendersConfig",
    "CommitmentTransitionRules",
    "EnergyWindowRules",
    "FlowgateRules",
    "LossFactorRules",
    "MarketConfig",
    "NetworkRules",
    "PenaltyConfig",
    "PenaltyCurve",
    "PenaltyCurveSegment",
    "RampRules",
    "ThermalLimitRules",
    "TopologyControlRules",
    # Reserves
    "ReserveProductDef",
    "ZonalRequirement",
    "build_reserve_products_dict",
    "REG_UP",
    "REG_DOWN",
    "SPINNING",
    "ECRS",
    "NON_SPINNING",
    "RAMP_UP_ON",
    "RAMP_UP_OFF",
    "RAMP_DOWN_ON",
    "RAMP_DOWN_OFF",
    "REACTIVE_UP",
    "REACTIVE_DOWN",
    "STANDARD_ACTIVE_PRODUCTS",
    "STANDARD_ALL_PRODUCTS",
    "PRODUCT_BY_ID",
    # Offers
    "GeneratorOfferSchedule",
    "GeneratorReserveOfferSchedule",
    "piecewise_linear_offer",
    "cost_blocks_to_segments",
    "reserve_offer",
    "reserve_offer_schedule",
    # Loads
    "build_dispatchable_load_blocks",
    "DispatchableLoadOfferSchedule",
    "DispatchableLoadSpec",
    "LoadArchetype",
    "interrupt_penalty",
    "linear_curtailment",
    "piecewise_linear_utility",
    "quadratic_utility",
    # Logging
    "LogStream",
    "SolveLogger",
    # Problem contract
    "MarketProblem",
    # Request builder
    "DispatchRequestBuilder",
    "request",
    # Report
    "RunReport",
    "write_run_report",
    # Solve runner
    "run_market_solve",
    # DispatchRequest typed view
    "DispatchRequest",
    # Workflow
    "MarketWorkflow",
    "WorkflowContext",
    "WorkflowResult",
    "WorkflowRunner",
    "WorkflowStage",
    "WorkflowStageResult",
    "WorkflowStageRole",
    "build_dispatch_stage",
    # Reconcile
    "build_all_committed_schedule",
    "build_decommit_feedback",
    "build_warm_start_schedule",
    "extract_fixed_commitment",
    "extract_storage_end_soc",
    "identify_resources_at_pmin",
    "pin_dispatch_bounds",
    "redispatch_with_ac",
    "thermal_slack_summary",
    # Violations
    "ViolationReport",
    "assess_violations",
    "assess_dispatch_violations_native",
]
