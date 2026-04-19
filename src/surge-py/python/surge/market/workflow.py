# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Typed multi-stage market workflow helpers."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable


class WorkflowStageRole(str, Enum):
    """Semantic role for one stage in a market workflow."""

    UNIT_COMMITMENT = "unit_commitment"
    ECONOMIC_DISPATCH = "economic_dispatch"
    PRICING = "pricing"
    RELIABILITY_COMMITMENT = "reliability_commitment"
    AC_REDISPATCH = "ac_redispatch"
    CUSTOM = "custom"


@dataclass
class WorkflowStageResult:
    """Outputs produced by one executed workflow stage."""

    stage_id: str
    role: WorkflowStageRole | str
    result: Any = None
    request: dict[str, Any] | None = None
    network: Any = None
    status: str = "solved"
    metadata: dict[str, Any] = field(default_factory=dict)


@dataclass
class WorkflowContext:
    """Shared workflow execution context."""

    surge_module: Any
    inputs: dict[str, Any] = field(default_factory=dict)
    shared: dict[str, Any] = field(default_factory=dict)
    stage_results: dict[str, WorkflowStageResult] = field(default_factory=dict)
    stage_order: list[str] = field(default_factory=list)

    def stage(self, stage_id: str) -> WorkflowStageResult:
        return self.stage_results[stage_id]

    @property
    def final_stage(self) -> WorkflowStageResult | None:
        if not self.stage_order:
            return None
        return self.stage_results[self.stage_order[-1]]


StageExecutor = Callable[[WorkflowContext], WorkflowStageResult]
StageEnabled = bool | Callable[[WorkflowContext], bool]


@dataclass
class WorkflowStage:
    """One executable workflow stage."""

    stage_id: str
    role: WorkflowStageRole | str
    execute: StageExecutor
    enabled: StageEnabled = True
    description: str | None = None

    def is_enabled(self, context: WorkflowContext) -> bool:
        if callable(self.enabled):
            return bool(self.enabled(context))
        return bool(self.enabled)


@dataclass
class MarketWorkflow:
    """Ordered stage list for a higher-level market workflow."""

    stages: list[WorkflowStage]

    def validate(self) -> None:
        if not self.stages:
            raise ValueError("market workflow requires at least one stage")
        seen: set[str] = set()
        for stage in self.stages:
            stage_id = stage.stage_id.strip()
            if not stage_id:
                raise ValueError("market workflow stage_id must be non-empty")
            if stage_id in seen:
                raise ValueError(f"duplicate market workflow stage_id {stage_id!r}")
            seen.add(stage_id)


@dataclass
class WorkflowResult:
    """Result of a full market workflow execution."""

    stages: list[WorkflowStageResult]
    context: WorkflowContext

    def stage(self, stage_id: str) -> WorkflowStageResult:
        return self.context.stage(stage_id)

    @property
    def final_stage(self) -> WorkflowStageResult | None:
        return self.context.final_stage


class WorkflowRunner:
    """Sequential workflow runner."""

    def run(
        self,
        workflow: MarketWorkflow,
        surge_module: Any,
        *,
        inputs: dict[str, Any] | None = None,
        shared: dict[str, Any] | None = None,
    ) -> WorkflowResult:
        workflow.validate()
        context = WorkflowContext(
            surge_module=surge_module,
            inputs=dict(inputs or {}),
            shared=shared if shared is not None else {},
        )
        results: list[WorkflowStageResult] = []
        for stage in workflow.stages:
            if not stage.is_enabled(context):
                continue
            stage_result = stage.execute(context)
            if not isinstance(stage_result, WorkflowStageResult):
                raise TypeError(
                    f"workflow stage {stage.stage_id!r} returned "
                    f"{type(stage_result).__name__}, expected WorkflowStageResult"
                )
            if not stage_result.stage_id:
                stage_result.stage_id = stage.stage_id
            if not stage_result.role:
                stage_result.role = stage.role
            context.stage_results[stage.stage_id] = stage_result
            context.stage_order.append(stage.stage_id)
            results.append(stage_result)
        return WorkflowResult(stages=results, context=context)


def build_dispatch_stage(
    stage_id: str,
    role: WorkflowStageRole | str,
    *,
    network_builder: Callable[[WorkflowContext], Any],
    request_builder: Callable[[WorkflowContext], dict[str, Any]],
    solver_kwargs: dict[str, Any] | Callable[[WorkflowContext], dict[str, Any]] | None = None,
    enabled: StageEnabled = True,
    description: str | None = None,
) -> WorkflowStage:
    """Build a workflow stage that solves one dispatch request."""

    def execute(context: WorkflowContext) -> WorkflowStageResult:
        network = network_builder(context)
        request = request_builder(context)
        resolved_solver_kwargs = (
            solver_kwargs(context)
            if callable(solver_kwargs) else dict(solver_kwargs or {})
        )
        result = context.surge_module.solve_dispatch(network, request, **resolved_solver_kwargs)
        return WorkflowStageResult(
            stage_id=stage_id,
            role=role,
            result=result,
            request=request,
            network=network,
            metadata={"solver_kwargs": resolved_solver_kwargs},
        )

    return WorkflowStage(
        stage_id=stage_id,
        role=role,
        execute=execute,
        enabled=enabled,
        description=description,
    )


__all__ = [
    "MarketWorkflow",
    "WorkflowContext",
    "WorkflowResult",
    "WorkflowRunner",
    "WorkflowStage",
    "WorkflowStageResult",
    "WorkflowStageRole",
    "build_dispatch_stage",
]
