# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Tests for :class:`DispatchableLoadSpec` + cost-model factories."""

from __future__ import annotations

import pytest

from surge.market import (
    DispatchableLoadOfferSchedule,
    DispatchableLoadSpec,
    interrupt_penalty,
    linear_curtailment,
    piecewise_linear_utility,
    quadratic_utility,
    request,
)


def test_cost_model_factories_produce_tagged_enum_shape() -> None:
    assert linear_curtailment(50.0) == {"LinearCurtailment": {"cost_per_mw": 50.0}}
    assert interrupt_penalty(60.0) == {"InterruptPenalty": {"cost_per_mw": 60.0}}
    assert quadratic_utility(a=100.0, b=0.5) == {
        "QuadraticUtility": {"a": 100.0, "b": 0.5}
    }
    assert piecewise_linear_utility([(0.0, 100.0), (50.0, 40.0)]) == {
        "PiecewiseLinear": {"points": [(0.0, 100.0), (50.0, 40.0)]}
    }


def test_dispatchable_load_spec_minimum() -> None:
    spec = DispatchableLoadSpec(
        resource_id="dr_1",
        bus=4,
        p_sched_pu=0.5,
        p_max_pu=0.5,
        cost_model=linear_curtailment(9000.0),
    )
    d = spec.to_request_dict()
    assert d["resource_id"] == "dr_1"
    assert d["bus"] == 4
    assert d["archetype"] == "Curtailable"
    assert d["cost_model"] == {"LinearCurtailment": {"cost_per_mw": 9000.0}}
    # Optional ramp fields omitted when unset.
    assert "ramp_group" not in d
    assert "initial_p_pu" not in d


def test_dispatchable_load_spec_with_ramp_group() -> None:
    spec = DispatchableLoadSpec(
        resource_id="dr_1",
        bus=4,
        p_sched_pu=0.5,
        p_max_pu=0.5,
        cost_model=linear_curtailment(9000.0),
        ramp_group="factory_42",
        ramp_up_pu_per_hr=0.2,
        initial_p_pu=0.3,
    )
    d = spec.to_request_dict()
    assert d["ramp_group"] == "factory_42"
    assert d["ramp_up_pu_per_hr"] == 0.2
    assert d["initial_p_pu"] == 0.3


def test_dispatchable_load_spec_validates_archetype() -> None:
    with pytest.raises(ValueError, match="archetype"):
        DispatchableLoadSpec(
            resource_id="x",
            bus=1,
            p_sched_pu=0.0,
            p_max_pu=0.0,
            cost_model=linear_curtailment(0.0),
            archetype="Nope",  # type: ignore[arg-type]
        )


def test_dispatchable_load_offer_schedule() -> None:
    sched = DispatchableLoadOfferSchedule(
        resource_id="grid_export",
        periods=[
            {"p_sched_pu": 0.5, "p_max_pu": 0.5,
             "cost_model": linear_curtailment(25.0)},
            {"p_sched_pu": 0.5, "p_max_pu": 0.5,
             "cost_model": linear_curtailment(30.0)},
        ],
    )
    d = sched.to_request_dict(2)
    assert d["resource_id"] == "grid_export"
    assert len(d["schedule"]["periods"]) == 2
    assert d["schedule"]["periods"][1]["cost_model"]["LinearCurtailment"]["cost_per_mw"] == 30.0


def test_dispatchable_load_offer_schedule_length_mismatch() -> None:
    sched = DispatchableLoadOfferSchedule(resource_id="x", periods=[{}])
    with pytest.raises(ValueError, match="expected 2"):
        sched.to_request_dict(2)


def test_builder_accepts_typed_dl_and_offer_schedule() -> None:
    spec = DispatchableLoadSpec(
        resource_id="dr_1",
        bus=4,
        p_sched_pu=0.3,
        p_max_pu=0.3,
        cost_model=linear_curtailment(9000.0),
    )
    sched = DispatchableLoadOfferSchedule(
        resource_id="dr_1",
        periods=[
            {"p_sched_pu": 0.3, "p_max_pu": 0.3,
             "cost_model": linear_curtailment(8500.0)},
            {"p_sched_pu": 0.3, "p_max_pu": 0.3,
             "cost_model": linear_curtailment(9000.0)},
        ],
    )
    req = (
        request()
        .timeline(periods=2, hours_by_period=[1.0, 1.0])
        .dispatchable_loads([spec])
        .dispatchable_load_offers([sched])
        .build()
    )
    assert req["market"]["dispatchable_loads"][0]["resource_id"] == "dr_1"
    assert len(req["market"]["dispatchable_load_offer_schedules"][0]["schedule"]["periods"]) == 2


def test_builder_dl_offers_require_timeline() -> None:
    sched = DispatchableLoadOfferSchedule(resource_id="x", periods=[{}])
    with pytest.raises(ValueError, match="timeline"):
        request().dispatchable_load_offers([sched])
