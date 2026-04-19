# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Tests for :class:`surge.market.DispatchRequestBuilder`."""

from __future__ import annotations

import pytest

from surge.market import (
    GeneratorOfferSchedule,
    GeneratorReserveOfferSchedule,
    MarketConfig,
    REG_UP,
    SPINNING,
    ZonalRequirement,
    request,
)


def test_minimal_request_requires_only_timeline() -> None:
    req = request().timeline(periods=2, hours_by_period=[1.0, 1.0]).build()
    assert req["timeline"] == {
        "periods": 2,
        "interval_hours_by_period": [1.0, 1.0],
    }
    assert req["commitment"] == "all_committed"
    assert "coupling" not in req
    assert "profiles" not in req


def test_build_without_timeline_raises() -> None:
    with pytest.raises(ValueError, match="timeline"):
        request().build()


def test_load_profile_requires_length_match() -> None:
    builder = request().timeline(periods=3, hours_by_period=[1.0, 1.0, 1.0])
    with pytest.raises(ValueError, match="expected 3"):
        builder.load_profile(bus=1, values=[10.0, 20.0])  # 2 != 3


def test_load_and_renewable_profiles_accumulate() -> None:
    req = (
        request()
        .timeline(periods=2, hours_by_period=[1.0, 1.0])
        .load_profile(bus=1, values=[10.0, 20.0])
        .load_profile(bus=4, values=[30.0, 40.0])
        .renewable_profile(resource="wind_1", capacity_factors=[0.5, 0.6])
        .build()
    )
    loads = req["profiles"]["load"]["profiles"]
    renewables = req["profiles"]["renewable"]["profiles"]
    assert [p["bus_number"] for p in loads] == [1, 4]
    assert renewables[0]["resource_id"] == "wind_1"
    assert renewables[0]["capacity_factors"] == [0.5, 0.6]


def test_commitment_optimize_options() -> None:
    req = (
        request()
        .timeline(periods=1, hours_by_period=[1.0])
        .commitment_optimize(mip_rel_gap=1e-4, time_limit_secs=30.0,
                             disable_warm_start=True)
        .build()
    )
    opts = req["commitment"]["optimize"]
    assert opts == {
        "mip_rel_gap": 1e-4,
        "time_limit_secs": 30.0,
        "disable_warm_start": True,
    }


def test_commitment_fixed_schedule() -> None:
    req = (
        request()
        .timeline(periods=2, hours_by_period=[1.0, 1.0])
        .commitment_fixed(resources=[
            {"resource_id": "gen_1_1", "initial": True, "periods": [True, True]},
        ])
        .build()
    )
    assert req["commitment"]["fixed"]["resources"][0]["resource_id"] == "gen_1_1"


def test_market_config_fills_without_clobbering_caller_keys() -> None:
    """Caller's explicit penalty_config must survive a later market_config merge."""
    caller_penalties = {"thermal": {"type": "linear", "cost_per_unit": 42.0}}
    cfg = MarketConfig.default(100.0)

    req = (
        request()
        .timeline(periods=1, hours_by_period=[1.0])
        .penalty_config(caller_penalties)
        .market_config(cfg)
        .build()
    )
    # Caller's explicit value wins — not overwritten by default.
    assert req["market"]["penalty_config"]["thermal"]["cost_per_unit"] == 42.0


def test_market_config_fills_when_caller_hasnt_set() -> None:
    cfg = MarketConfig.default(100.0)
    req = (
        request()
        .timeline(periods=1, hours_by_period=[1.0])
        .market_config(cfg)
        .build()
    )
    # Framework defaults land.
    assert "thermal" in req["market"]["penalty_config"]
    assert "thermal_limits" in req["network"]


def test_reserve_products_and_zonal_reserves() -> None:
    req = (
        request()
        .timeline(periods=2, hours_by_period=[1.0, 1.0])
        .reserve_products([REG_UP, SPINNING])
        .zonal_reserves([
            ZonalRequirement(zone_id=1, product_id="reg_up",
                             requirement_mw=5.0, per_period_mw=[5.0, 5.0]),
        ])
        .build()
    )
    ids = {p["id"] for p in req["market"]["reserve_products"]}
    assert ids == {REG_UP.id, SPINNING.id}
    assert req["market"]["zonal_reserve_requirements"][0]["product_id"] == "reg_up"


def test_generator_offers_render_per_period() -> None:
    sched = GeneratorOfferSchedule(
        resource_id="gen_1_1",
        segments_by_period=[[(100.0, 30.0)], [(100.0, 35.0)]],
    )
    req = (
        request()
        .timeline(periods=2, hours_by_period=[1.0, 1.0])
        .generator_offers([sched])
        .build()
    )
    offers = req["market"]["generator_offer_schedules"][0]
    assert offers["resource_id"] == "gen_1_1"
    assert len(offers["schedule"]["periods"]) == 2


def test_generator_offers_errors_without_timeline() -> None:
    sched = GeneratorOfferSchedule(
        resource_id="gen_1_1",
        segments_by_period=[[(100.0, 30.0)]],
    )
    with pytest.raises(ValueError, match="timeline"):
        request().generator_offers([sched])


def test_reserve_offers() -> None:
    sched = GeneratorReserveOfferSchedule(
        resource_id="gen_1_1",
        offers_by_period=[
            [{"product_id": "reg_up", "capacity_mw": 10.0, "cost_per_mwh": 3.0}],
            [{"product_id": "reg_up", "capacity_mw": 10.0, "cost_per_mwh": 3.0}],
        ],
    )
    req = (
        request()
        .timeline(periods=2, hours_by_period=[1.0, 1.0])
        .reserve_offers([sched])
        .build()
    )
    rs = req["market"]["generator_reserve_offer_schedules"][0]
    assert rs["resource_id"] == "gen_1_1"


def test_previous_dispatch_and_soc_overrides() -> None:
    req = (
        request()
        .timeline(periods=1, hours_by_period=[1.0])
        .previous_dispatch({"gen_1_1": 40.0, "gen_2_1": 25.5})
        .storage_soc_overrides({"bess": 50.0})
        .build()
    )
    prev = req["state"]["initial"]["previous_resource_dispatch"]
    soc = req["state"]["initial"]["storage_soc_overrides"]
    assert {p["resource_id"]: p["mw"] for p in prev} == {"gen_1_1": 40.0, "gen_2_1": 25.5}
    assert soc[0]["soc_mwh"] == 50.0


def test_coupling_and_formulation() -> None:
    req = (
        request()
        .timeline(periods=2, hours_by_period=[1.0, 1.0])
        .coupling("time_coupled")
        .formulation("dc")
        .build()
    )
    assert req["coupling"] == "time_coupled"
    assert req["formulation"] == "dc"


def test_invalid_coupling_rejected() -> None:
    b = request().timeline(periods=1, hours_by_period=[1.0])
    with pytest.raises(ValueError, match="coupling"):
        b.coupling("sometimes")  # type: ignore[arg-type]


def test_run_pricing_toggle() -> None:
    req = (
        request()
        .timeline(periods=1, hours_by_period=[1.0])
        .run_pricing(True)
        .build()
    )
    assert req["runtime"]["run_pricing"] is True


def test_extend_escape_hatches() -> None:
    req = (
        request()
        .timeline(periods=1, hours_by_period=[1.0])
        .extend_market(virtual_bids=[{"bus": 1, "mw": 5.0}])
        .extend_network(flowgates={"enabled": True})
        .extend_state_initial(custom_key=[1, 2, 3])
        .extend_runtime(capture_model_diagnostics=True)
        .build()
    )
    assert req["market"]["virtual_bids"][0]["mw"] == 5.0
    assert req["network"]["flowgates"] == {"enabled": True}
    assert req["state"]["initial"]["custom_key"] == [1, 2, 3]
    assert req["runtime"]["capture_model_diagnostics"] is True


def test_raw_merge_preserves_caller_intent() -> None:
    req = (
        request()
        .timeline(periods=1, hours_by_period=[1.0])
        .run_pricing(True)
        # raw_merge should fill runtime.tolerance but NOT overwrite run_pricing.
        .raw_merge({"runtime": {"run_pricing": False, "tolerance": 1e-6}})
        .build()
    )
    assert req["runtime"]["run_pricing"] is True
    assert req["runtime"]["tolerance"] == 1e-6


def test_returns_self_for_chaining() -> None:
    b = request()
    assert b.timeline(periods=1, hours_by_period=[1.0]) is b
    assert b.coupling("period_by_period") is b
    assert b.commitment_all_committed() is b


def test_solves_end_to_end_on_case14() -> None:
    """The builder's output really is what surge.solve_dispatch consumes."""
    import surge

    net = surge.case14()
    req = (
        request()
        .timeline(periods=1, hours_by_period=[1.0])
        .commitment_all_committed()
        .market_config(MarketConfig.default(net.base_mva))
        .build()
    )
    result = surge.solve_dispatch(net, req, lp_solver="highs")
    assert result.summary.get("total_cost") is not None
