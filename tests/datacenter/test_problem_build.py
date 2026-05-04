# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Schema + builder tests — make sure the network and request shapes
land what the rest of the stack expects."""

from __future__ import annotations

import pytest

from surge.market import REG_UP

from markets.datacenter import (
    AsProduct,
    BessSpec,
    CurtailableLoadTier,
    DataCenterPolicy,
    DataCenterProblem,
    FourCpSpec,
    ItLoadSpec,
    SiteSpec,
    SolarSpec,
    ThermalSpec,
    WindSpec,
)
from markets.datacenter.problem import (
    BESS_RESOURCE_ID,
    GRID_EXPORT_RESOURCE_ID,
    GRID_IMPORT_RESOURCE_ID,
    SOLAR_RESOURCE_ID,
    WIND_RESOURCE_ID,
)


def _minimal_problem(periods: int = 4) -> DataCenterProblem:
    return DataCenterProblem(
        period_durations_hours=[1.0] * periods,
        lmp_forecast_per_mwh=[40.0] * periods,
        site=SiteSpec(
            poi_limit_mw=1000.0,
            it_load=ItLoadSpec(
                must_serve_mw=[700.0] * periods,
                tiers=[CurtailableLoadTier("training", 200.0, voll_per_mwh=40.0)],
            ),
            bess=BessSpec(
                power_charge_mw=200.0, power_discharge_mw=200.0, energy_mwh=800.0
            ),
        ),
    )


def test_minimum_network_has_expected_resources() -> None:
    problem = _minimal_problem()
    net = problem.build_network()
    gen_ids = {g.id for g in net.generators}
    assert BESS_RESOURCE_ID in gen_ids
    assert GRID_IMPORT_RESOURCE_ID in gen_ids


def test_full_stack_network_exposes_each_asset() -> None:
    problem = DataCenterProblem(
        period_durations_hours=[1.0] * 4,
        lmp_forecast_per_mwh=[40.0] * 4,
        site=SiteSpec(
            poi_limit_mw=1000.0,
            it_load=ItLoadSpec(must_serve_mw=[700.0] * 4),
            bess=BessSpec(
                power_charge_mw=200.0, power_discharge_mw=200.0, energy_mwh=800.0
            ),
            solar=SolarSpec(nameplate_mw=100.0, capacity_factors=[0.5] * 4),
            wind=WindSpec(nameplate_mw=80.0, capacity_factors=[0.4] * 4),
            fuel_cell=ThermalSpec(
                resource_id="fc",
                nameplate_mw=120.0,
                pmin_mw=10.0,
                fuel_price_per_mmbtu=8.0,
                heat_rate_btu_per_kwh=6500.0,
                min_up_h=2.0,
                min_down_h=1.0,
            ),
            gas_ct=ThermalSpec(
                resource_id="ct",
                nameplate_mw=300.0,
                pmin_mw=40.0,
                fuel_price_per_mmbtu=4.0,
                heat_rate_btu_per_kwh=9500.0,
                min_up_h=4.0,
                min_down_h=4.0,
            ),
        ),
    )
    net = problem.build_network()
    gen_ids = {g.id for g in net.generators}
    assert {SOLAR_RESOURCE_ID, WIND_RESOURCE_ID, "fc", "ct"} <= gen_ids


def test_thermal_commitment_attrs_persist_on_generator() -> None:
    problem = _minimal_problem()
    problem.site.fuel_cell = ThermalSpec(
        resource_id="fc",
        nameplate_mw=100.0,
        pmin_mw=10.0,
        fuel_price_per_mmbtu=8.0,
        heat_rate_btu_per_kwh=6500.0,
        min_up_h=3.0,
        min_down_h=2.0,
        startup_cost_tiers=[{"max_offline_hours": 8.0, "cost": 2000.0}],
        ramp_up_mw_per_min=5.0,
        ramp_down_mw_per_min=5.0,
    )
    net = problem.build_network()
    fc = net.generator("fc")
    assert fc.min_up_time_hr == pytest.approx(3.0)
    assert fc.min_down_time_hr == pytest.approx(2.0)
    assert fc.startup_cost_tiers == [(8.0, 2000.0, 0.0)]
    assert fc.ramp_up_curve and fc.ramp_up_curve[0][1] == pytest.approx(5.0)


def test_request_payload_includes_load_profile_and_offers() -> None:
    problem = _minimal_problem(periods=4)
    req = problem.build_request(DataCenterPolicy())
    profiles = req["profiles"]["load"]["profiles"]
    assert profiles[0]["bus_number"] == 1
    assert profiles[0]["values_mw"] == [700.0, 700.0, 700.0, 700.0]

    market = req["market"]
    offer_ids = {s["resource_id"] for s in market["generator_offer_schedules"]}
    assert GRID_IMPORT_RESOURCE_ID in offer_ids

    dl_ids = {s["resource_id"] for s in market["dispatchable_loads"]}
    assert GRID_EXPORT_RESOURCE_ID in dl_ids
    assert "it_load::training" in dl_ids


def test_request_payload_renewable_profiles_match_periods() -> None:
    problem = DataCenterProblem(
        period_durations_hours=[1.0] * 6,
        lmp_forecast_per_mwh=[40.0] * 6,
        site=SiteSpec(
            poi_limit_mw=500.0,
            it_load=ItLoadSpec(must_serve_mw=[300.0] * 6),
            bess=BessSpec(
                power_charge_mw=100.0, power_discharge_mw=100.0, energy_mwh=400.0
            ),
            solar=SolarSpec(
                nameplate_mw=200.0, capacity_factors=[0.0, 0.2, 0.5, 0.8, 0.5, 0.2]
            ),
        ),
    )
    req = problem.build_request(DataCenterPolicy())
    rp = req["profiles"]["renewable"]["profiles"]
    assert any(
        e["resource_id"] == SOLAR_RESOURCE_ID
        and e["capacity_factors"] == [0.0, 0.2, 0.5, 0.8, 0.5, 0.2]
        for e in rp
    )


def test_four_cp_renders_through_to_market_payload() -> None:
    problem = _minimal_problem()
    problem.site.four_cp = FourCpSpec(period_indices=[1, 2], charge_per_mw=25_000.0)
    req = problem.build_request(DataCenterPolicy())
    charges = req["market"]["peak_demand_charges"]
    assert len(charges) == 1
    assert charges[0]["resource_id"] == GRID_IMPORT_RESOURCE_ID
    assert charges[0]["period_indices"] == [1, 2]
    assert charges[0]["charge_per_mw"] == pytest.approx(25_000.0)


def test_as_products_decoupled_to_avoid_double_credit() -> None:
    """When the user adds AS products with substitution ladders (ECRS,
    SPINNING, etc.), the operator-side problem strips the ladder so
    each cleared MW earns its own forecast price."""
    from surge.market import ECRS

    problem = _minimal_problem()
    problem.as_products = [AsProduct(REG_UP, [10.0] * 4), AsProduct(ECRS, [5.0] * 4)]
    req = problem.build_request(DataCenterPolicy())
    products = req["market"]["reserve_products"]
    for prod in products:
        assert prod.get("balance_products", []) == []
        assert prod.get("shared_limit_products", []) == []


def test_problem_validates_profile_lengths() -> None:
    with pytest.raises(ValueError):
        DataCenterProblem(
            period_durations_hours=[1.0, 1.0, 1.0],
            lmp_forecast_per_mwh=[40.0] * 4,  # too long
            site=SiteSpec(
                poi_limit_mw=100.0,
                it_load=ItLoadSpec(must_serve_mw=[50.0, 50.0, 50.0]),
                bess=BessSpec(
                    power_charge_mw=20.0, power_discharge_mw=20.0, energy_mwh=80.0
                ),
            ),
        )


def test_problem_validates_four_cp_period_bounds() -> None:
    with pytest.raises(ValueError):
        DataCenterProblem(
            period_durations_hours=[1.0] * 4,
            lmp_forecast_per_mwh=[40.0] * 4,
            site=SiteSpec(
                poi_limit_mw=100.0,
                it_load=ItLoadSpec(must_serve_mw=[50.0] * 4),
                bess=BessSpec(
                    power_charge_mw=20.0, power_discharge_mw=20.0, energy_mwh=80.0
                ),
                four_cp=FourCpSpec(period_indices=[7], charge_per_mw=1.0),  # OOB
            ),
        )
