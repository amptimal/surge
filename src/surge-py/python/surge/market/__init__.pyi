# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Type stubs for the curated ``surge.market`` package surface.

The sub-modules ship with inline annotations (PEP 561 via the top-level
``py.typed`` marker). This stub exists to make the re-export surface
visible to type checkers and IDEs with a single import.
"""

from __future__ import annotations

from ..dispatch_request import DispatchRequest as DispatchRequest
from .config import (
    AcReconcileConfig as AcReconcileConfig,
    BendersConfig as BendersConfig,
    CommitmentTransitionRules as CommitmentTransitionRules,
    EnergyWindowRules as EnergyWindowRules,
    FlowgateRules as FlowgateRules,
    LossFactorRules as LossFactorRules,
    MarketConfig as MarketConfig,
    NetworkRules as NetworkRules,
    PenaltyConfig as PenaltyConfig,
    PenaltyCurve as PenaltyCurve,
    PenaltyCurveSegment as PenaltyCurveSegment,
    RampRules as RampRules,
    ThermalLimitRules as ThermalLimitRules,
    TopologyControlRules as TopologyControlRules,
)
from .loads import (
    DispatchableLoadOfferSchedule as DispatchableLoadOfferSchedule,
    DispatchableLoadSpec as DispatchableLoadSpec,
    LoadArchetype as LoadArchetype,
    build_dispatchable_load_blocks as build_dispatchable_load_blocks,
    interrupt_penalty as interrupt_penalty,
    linear_curtailment as linear_curtailment,
    piecewise_linear_utility as piecewise_linear_utility,
    quadratic_utility as quadratic_utility,
)
from .logging import SolveLogger as SolveLogger
from .offers import (
    GeneratorOfferSchedule as GeneratorOfferSchedule,
    GeneratorReserveOfferSchedule as GeneratorReserveOfferSchedule,
    cost_blocks_to_segments as cost_blocks_to_segments,
    piecewise_linear_offer as piecewise_linear_offer,
    reserve_offer as reserve_offer,
    reserve_offer_schedule as reserve_offer_schedule,
)
from .problem import MarketProblem as MarketProblem
from .reconcile import (
    build_all_committed_schedule as build_all_committed_schedule,
    build_decommit_feedback as build_decommit_feedback,
    build_warm_start_schedule as build_warm_start_schedule,
    extract_fixed_commitment as extract_fixed_commitment,
    extract_storage_end_soc as extract_storage_end_soc,
    identify_resources_at_pmin as identify_resources_at_pmin,
    pin_dispatch_bounds as pin_dispatch_bounds,
    redispatch_with_ac as redispatch_with_ac,
    thermal_slack_summary as thermal_slack_summary,
)
from .report import (
    RunReport as RunReport,
    write_run_report as write_run_report,
)
from .request import (
    DispatchRequestBuilder as DispatchRequestBuilder,
    request as request,
)
from .reserves import (
    NON_SPINNING as NON_SPINNING,
    PRODUCT_BY_ID as PRODUCT_BY_ID,
    RAMP_DOWN_OFF as RAMP_DOWN_OFF,
    RAMP_DOWN_ON as RAMP_DOWN_ON,
    RAMP_UP_OFF as RAMP_UP_OFF,
    RAMP_UP_ON as RAMP_UP_ON,
    REACTIVE_DOWN as REACTIVE_DOWN,
    REACTIVE_UP as REACTIVE_UP,
    REG_DOWN as REG_DOWN,
    REG_UP as REG_UP,
    SPINNING as SPINNING,
    STANDARD_ACTIVE_PRODUCTS as STANDARD_ACTIVE_PRODUCTS,
    STANDARD_ALL_PRODUCTS as STANDARD_ALL_PRODUCTS,
    ReserveProductDef as ReserveProductDef,
    ZonalRequirement as ZonalRequirement,
    build_reserve_products_dict as build_reserve_products_dict,
)
from .runner import run_market_solve as run_market_solve
from .violations import (
    ViolationReport as ViolationReport,
    assess_dispatch_violations_native as assess_dispatch_violations_native,
    assess_violations as assess_violations,
)
from .workflow import (
    MarketWorkflow as MarketWorkflow,
    WorkflowContext as WorkflowContext,
    WorkflowResult as WorkflowResult,
    WorkflowRunner as WorkflowRunner,
    WorkflowStage as WorkflowStage,
    WorkflowStageResult as WorkflowStageResult,
    WorkflowStageRole as WorkflowStageRole,
    build_dispatch_stage as build_dispatch_stage,
)
