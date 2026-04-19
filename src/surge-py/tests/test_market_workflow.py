# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from surge.market import (
    MarketConfig,
    MarketWorkflow,
    WorkflowRunner,
    WorkflowStage,
    WorkflowStageResult,
    WorkflowStageRole,
)


def test_market_config_preset_uses_grouped_network_rules():
    config = MarketConfig.from_preset("goc3", e_vio_cost=25.0, base_mva=100.0)
    network = config.network_rules.to_dict()

    assert network["thermal_limits"]["enforce"] is True
    assert network["ramping"]["enforcement"] == "hard"
    assert network["energy_windows"]["penalty_per_puh"] == 0.25
    assert network["topology_control"]["mode"] == "fixed"


def test_workflow_runner_executes_stages_in_order():
    events: list[str] = []

    def stage_one(context):
        events.append("stage_one")
        return WorkflowStageResult(
            stage_id="stage_one",
            role=WorkflowStageRole.UNIT_COMMITMENT,
            metadata={"value": 1},
        )

    def stage_two(context):
        events.append(f"stage_two:{context.stage('stage_one').metadata['value']}")
        return WorkflowStageResult(
            stage_id="stage_two",
            role=WorkflowStageRole.ECONOMIC_DISPATCH,
        )

    workflow = MarketWorkflow(
        stages=[
            WorkflowStage("stage_one", WorkflowStageRole.UNIT_COMMITMENT, stage_one),
            WorkflowStage("stage_two", WorkflowStageRole.ECONOMIC_DISPATCH, stage_two),
        ],
    )

    result = WorkflowRunner().run(workflow, surge_module=object())

    assert events == ["stage_one", "stage_two:1"]
    assert result.final_stage.stage_id == "stage_two"
