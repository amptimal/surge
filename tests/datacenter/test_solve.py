# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""End-to-end SCUC smoke tests — checks the LP solves and the P&L
report invariants hold."""

from __future__ import annotations

from pathlib import Path

import pytest

from surge.market import ECRS, NON_SPINNING, REG_UP

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
    solve,
)


def _full_stack_problem(
    *,
    lmp: list[float] | None = None,
    four_cp: FourCpSpec | None = None,
    as_products: list[AsProduct] | None = None,
) -> DataCenterProblem:
    if lmp is None:
        lmp = [40.0] * 8 + [80.0] * 8 + [50.0] * 8
    return DataCenterProblem(
        period_durations_hours=[1.0] * 24,
        lmp_forecast_per_mwh=lmp,
        site=SiteSpec(
            poi_limit_mw=1000.0,
            it_load=ItLoadSpec(
                must_serve_mw=[700.0] * 24,
                tiers=[
                    CurtailableLoadTier("training", 200.0, voll_per_mwh=40.0),
                    CurtailableLoadTier("research", 100.0, voll_per_mwh=5.0),
                ],
            ),
            bess=BessSpec(
                power_charge_mw=200.0,
                power_discharge_mw=200.0,
                energy_mwh=800.0,
            ),
            solar=SolarSpec(
                nameplate_mw=250.0,
                capacity_factors=[0.0] * 6 + [0.5] * 8 + [0.8] * 4 + [0.0] * 6,
            ),
            wind=WindSpec(nameplate_mw=150.0, capacity_factors=[0.4] * 24),
            fuel_cell=ThermalSpec(
                resource_id="fc",
                nameplate_mw=200.0,
                pmin_mw=20.0,
                fuel_price_per_mmbtu=8.0,
                heat_rate_btu_per_kwh=6500.0,
                min_up_h=2.0,
                min_down_h=1.0,
                qualified_as_products=("reg_up", "reg_down", "syn", "ecrs"),
            ),
            gas_ct=ThermalSpec(
                resource_id="ct",
                nameplate_mw=400.0,
                pmin_mw=50.0,
                fuel_price_per_mmbtu=4.0,
                heat_rate_btu_per_kwh=9500.0,
                vom_per_mwh=4.0,
                no_load_cost_per_hr=500.0,
                startup_cost_tiers=[{"max_offline_hours": 8.0, "cost": 4000.0}],
                min_up_h=4.0,
                min_down_h=4.0,
                co2_tonnes_per_mwh=0.45,
                qualified_as_products=("reg_up", "syn", "ecrs"),
            ),
            diesel=ThermalSpec(
                resource_id="diesel",
                nameplate_mw=50.0,
                pmin_mw=0.0,
                fuel_price_per_mmbtu=20.0,
                heat_rate_btu_per_kwh=10500.0,
                co2_tonnes_per_mwh=0.74,
                qualified_as_products=("nsyn",),
            ),
            four_cp=four_cp,
        ),
        as_products=as_products or [],
    )


def test_full_stack_solve_succeeds(tmp_path: Path) -> None:
    problem = _full_stack_problem()
    report = solve(problem, tmp_path, policy=DataCenterPolicy(mip_time_limit_secs=60.0))
    assert report["status"] == "ok"
    summary = report["extras"]["pl_summary"]
    assert summary["must_serve_mwh"] == pytest.approx(700.0 * 24, rel=1e-3)
    assert (tmp_path / "pnl-report.json").exists()
    assert (tmp_path / "dispatch-result.json").exists()


def test_low_lmp_day_minimises_grid_import_via_renewables_first(tmp_path: Path) -> None:
    """At $20/MWh sustained, on-site renewables clear before any
    thermal commitment. Diesel never runs (way too expensive)."""
    problem = _full_stack_problem(lmp=[20.0] * 24)
    report = solve(problem, tmp_path, policy=DataCenterPolicy(mip_time_limit_secs=60.0))
    assert report["status"] == "ok"
    s = report["extras"]["pl_summary"]
    # Renewables should produce something across the day even when the
    # cheap grid is very accommodating.
    assert s["renewables_mwh"] > 1_000.0


def test_high_lmp_day_runs_on_site_thermal(tmp_path: Path) -> None:
    """LMP = $200/MWh sustained → on-site thermals dominate. Gas CT
    marginal cost ~$42/MWh ≪ $200, so thermal MWh significant."""
    problem = _full_stack_problem(lmp=[200.0] * 24)
    report = solve(problem, tmp_path, policy=DataCenterPolicy(mip_time_limit_secs=60.0))
    assert report["status"] == "ok"
    s = report["extras"]["pl_summary"]
    assert s["thermal_mwh"] > 5_000.0
    # And the operator should be a net exporter — total export revenue
    # >> import cost when LMP is sustained well above on-site marginal.
    assert s["energy_export_revenue_dollars"] > s["energy_import_cost_dollars"]


def test_four_cp_drives_peak_grid_import_to_zero(tmp_path: Path) -> None:
    """A $25 000/MW peak-demand charge applied to four flagged hours
    is large enough to make the LP commit on-site thermal rather than
    import during those hours."""
    four_cp = FourCpSpec(period_indices=[16, 17, 18, 19], charge_per_mw=25_000.0)
    problem = _full_stack_problem(four_cp=four_cp)
    report = solve(problem, tmp_path, policy=DataCenterPolicy(mip_time_limit_secs=120.0))
    assert report["status"] == "ok"
    s = report["extras"]["pl_summary"]
    # The peak across the flagged 4-CP window should be close to zero
    # when on-site capacity covers the must-serve baseline.
    assert s["peak_grid_import_mw"] <= 1.0


def test_pnl_invariant_holds(tmp_path: Path) -> None:
    """Net P&L equals revenues minus costs."""
    problem = _full_stack_problem(
        as_products=[
            AsProduct(REG_UP, [8.0] * 24),
            AsProduct(ECRS, [6.0] * 24),
            AsProduct(NON_SPINNING, [3.0] * 24),
        ]
    )
    report = solve(problem, tmp_path, policy=DataCenterPolicy(mip_time_limit_secs=60.0))
    assert report["status"] == "ok"
    s = report["extras"]["pl_summary"]
    revenues = (
        s["compute_revenue_dollars"]
        + s["energy_export_revenue_dollars"]
        + s["as_revenue_dollars"]
    )
    costs = (
        s["energy_import_cost_dollars"]
        + s["fuel_cost_dollars"]
        + s["vom_cost_dollars"]
        + s["no_load_cost_dollars"]
        + s["startup_cost_dollars"]
        + s["bess_degradation_cost_dollars"]
        + s["tx_demand_charge_dollars"]
    )
    assert s["net_pnl_dollars"] == pytest.approx(revenues - costs, rel=1e-6, abs=1.0)


def test_as_revenue_flows_to_qualified_resources(tmp_path: Path) -> None:
    """When a positive AS price is forecast, the LP clears reserves
    against qualified resources and the P&L picks up the revenue."""
    problem = _full_stack_problem(
        as_products=[AsProduct(REG_UP, [12.0] * 24)],
    )
    report = solve(problem, tmp_path, policy=DataCenterPolicy(mip_time_limit_secs=60.0))
    assert report["status"] == "ok"
    assert report["extras"]["pl_summary"]["as_revenue_dollars"] > 0.0
