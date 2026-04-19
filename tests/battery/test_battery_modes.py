# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Tests for the four-mode matrix: dispatch_mode × period_coupling.

Matrix:

+---------------------+-------------------------------------+-------------------------------------+
|                     | coupled                             | sequential                          |
+=====================+=====================================+=====================================+
| optimal_foresight   | LP sees full forecast; extracts     | Myopic — each period's LP only      |
|                     | max arbitrage (revenue ceiling).    | sees its own LMP; SOC carries.      |
+---------------------+-------------------------------------+-------------------------------------+
| pwl_offers          | Bid / offer curves gate dispatch    | Same bids, one period at a time —   |
|                     | across the whole horizon.           | simulates RTM clearing.             |
+---------------------+-------------------------------------+-------------------------------------+
"""

from __future__ import annotations

from pathlib import Path

import pytest

from surge.market import REG_UP

from markets.battery import (
    AsProduct,
    BatteryPolicy,
    BatteryProblem,
    PwlBidStrategy,
    SiteSpec,
    solve,
)


def _basic_site(
    *,
    initial_soc: float = 0.0,
    energy_mwh: float = 100.0,
    efficiency: float = 1.0,
    degradation: float = 0.0,
) -> SiteSpec:
    leg = max(0.0, efficiency) ** 0.5
    return SiteSpec(
        poi_limit_mw=50.0,
        bess_power_charge_mw=25.0,
        bess_power_discharge_mw=25.0,
        bess_energy_mwh=energy_mwh,
        bess_charge_efficiency=leg,
        bess_discharge_efficiency=leg,
        bess_soc_min_fraction=0.0,
        bess_soc_max_fraction=1.0,
        bess_initial_soc_mwh=initial_soc,
        bess_degradation_cost_per_mwh=degradation,
    )


# ---------------------------------------------------------------------------
# Policy validation
# ---------------------------------------------------------------------------


def test_policy_rejects_invalid_dispatch_mode() -> None:
    with pytest.raises(ValueError, match="dispatch_mode"):
        BatteryPolicy(dispatch_mode="clairvoyant")


def test_policy_rejects_invalid_period_coupling() -> None:
    with pytest.raises(ValueError, match="period_coupling"):
        BatteryPolicy(period_coupling="parallel")


def test_pwl_offers_requires_strategy(tmp_path: Path) -> None:
    """pwl_offers without a PwlBidStrategy surfaces a clear error."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[20.0, 80.0],
        site=_basic_site(initial_soc=0.0),
    )
    report = solve(problem, tmp_path, policy=BatteryPolicy(dispatch_mode="pwl_offers"))
    assert report["status"] == "error"
    assert "PwlBidStrategy" in (report["error"] or "")


# ---------------------------------------------------------------------------
# optimal_foresight × sequential: myopia erases inter-period arbitrage
# ---------------------------------------------------------------------------


def test_optimal_foresight_coupled_sees_the_spread(tmp_path: Path) -> None:
    """Classic arb — LP with foresight charges at $20 / discharges at $80."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[20.0, 80.0],
        site=_basic_site(initial_soc=0.0),
    )
    report = solve(
        problem,
        tmp_path,
        policy=BatteryPolicy(
            dispatch_mode="optimal_foresight", period_coupling="coupled"
        ),
    )
    assert report["status"] == "ok"
    assert report["extras"]["revenue_summary"]["energy_revenue_dollars"] == pytest.approx(1_500.0, rel=1e-3)


def test_optimal_foresight_sequential_is_myopic(tmp_path: Path) -> None:
    """With zero-cost BESS and no forward knowledge, period 0 has no
    incentive to charge — it sits idle. Period 1 then has no SOC to
    discharge. Net revenue = $0."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[20.0, 80.0],
        site=_basic_site(initial_soc=0.0),
    )
    report = solve(
        problem,
        tmp_path,
        policy=BatteryPolicy(
            dispatch_mode="optimal_foresight", period_coupling="sequential"
        ),
    )
    assert report["status"] == "ok"
    assert report["extras"]["revenue_summary"]["energy_revenue_dollars"] == pytest.approx(0.0, abs=1.0)


# ---------------------------------------------------------------------------
# pwl_offers: bids hardwire behavior, both modes capture the same spread
# ---------------------------------------------------------------------------


def test_pwl_offers_coupled_honors_bids(tmp_path: Path) -> None:
    """With discharge offer $50 and charge bid $30, the LP:
    - period 0 (LMP $20 ≤ bid $30): charges
    - period 1 (LMP $80 ≥ offer $50): discharges
    """
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[20.0, 80.0],
        site=_basic_site(initial_soc=0.0),
        pwl_strategy=PwlBidStrategy.flat(
            discharge_capacity_mw=25.0, discharge_price=50.0,
            charge_capacity_mw=25.0, charge_price=30.0,
        ),
    )
    report = solve(
        problem,
        tmp_path,
        policy=BatteryPolicy(dispatch_mode="pwl_offers", period_coupling="coupled"),
    )
    assert report["status"] == "ok"
    assert report["extras"]["revenue_summary"]["energy_revenue_dollars"] == pytest.approx(1_500.0, rel=1e-3)


def test_pwl_offers_sequential_captures_sequential_spread(tmp_path: Path) -> None:
    """Key property of PWL bids: the battery's offer / bid curves act
    as a *self-commitment device* — they dictate behavior per period
    regardless of forward knowledge. Sequential clearing under PWL
    recovers the same revenue as coupled because the bids deterministically
    charge when LMP ≤ bid and discharge when LMP ≥ offer.
    """
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[20.0, 80.0],
        site=_basic_site(initial_soc=0.0),
        pwl_strategy=PwlBidStrategy.flat(
            discharge_capacity_mw=25.0, discharge_price=50.0,
            charge_capacity_mw=25.0, charge_price=30.0,
        ),
    )
    report = solve(
        problem,
        tmp_path,
        policy=BatteryPolicy(dispatch_mode="pwl_offers", period_coupling="sequential"),
    )
    assert report["status"] == "ok"
    assert report["extras"]["revenue_summary"]["energy_revenue_dollars"] == pytest.approx(1_500.0, rel=1e-3)


def test_pwl_bid_offer_thresholds_gate_dispatch(tmp_path: Path) -> None:
    """When LMP falls between the charge bid and the discharge offer,
    the battery stays idle — both directions are uneconomic at the
    submitted prices."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[40.0, 45.0],   # between bid $30 and offer $50
        site=_basic_site(initial_soc=50.0),
        pwl_strategy=PwlBidStrategy.flat(
            discharge_capacity_mw=25.0, discharge_price=50.0,
            charge_capacity_mw=25.0, charge_price=30.0,
        ),
    )
    report = solve(
        problem,
        tmp_path,
        policy=BatteryPolicy(dispatch_mode="pwl_offers", period_coupling="coupled"),
    )
    assert report["status"] == "ok"
    # No charge or discharge — LMP is in the dead band.
    assert report["extras"]["revenue_summary"]["energy_revenue_dollars"] == pytest.approx(0.0, abs=1.0)
    assert report["extras"]["revenue_summary"]["total_throughput_mwh"] == pytest.approx(0.0, abs=0.01)


# ---------------------------------------------------------------------------
# Sequential SOC carryforward
# ---------------------------------------------------------------------------


def test_sequential_soc_carries_forward(tmp_path: Path) -> None:
    """Starting at SOC 50 MWh, dumping 25 MW in period 0 should leave
    25 MWh for period 1. Sequential solve must observe that SOC."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[30.0, 80.0],
        site=_basic_site(initial_soc=50.0),
    )
    report = solve(
        problem,
        tmp_path,
        policy=BatteryPolicy(
            dispatch_mode="optimal_foresight", period_coupling="sequential"
        ),
    )
    assert report["status"] == "ok"
    # With optimal_foresight sequential starting at SOC 50:
    # - p0 (LMP $30): no forward knowledge → might dump or idle;
    #   because dumping pays $30/MW in this period with no alternative
    #   use, it will dump 25 MW. SOC goes 50 → 25.
    # - p1 (LMP $80): 25 MWh left → discharge 25 MW.
    import json
    revenue = json.loads((tmp_path / "revenue-report.json").read_text())
    soc_end_p0 = revenue["schedule"][0]["soc_mwh"]
    soc_end_p1 = revenue["schedule"][1]["soc_mwh"]
    # SOC monotonically decreases in a discharge-only sequential run.
    assert soc_end_p0 < 50.0
    assert soc_end_p1 < soc_end_p0


# ---------------------------------------------------------------------------
# AS co-optimisation respects PWL offer price
# ---------------------------------------------------------------------------


def test_pwl_as_offer_above_forecast_declines_clearing(tmp_path: Path) -> None:
    """If the BESS's AS offer price exceeds the forecast AS price,
    the LP prefers the shortfall penalty (= forecast price) over the
    battery's bid. No AS clears."""
    problem = BatteryProblem(
        period_durations_hours=[1.0],
        lmp_forecast_per_mwh=[5.0],
        site=_basic_site(initial_soc=50.0),
        as_products=[AsProduct(REG_UP, price_forecast_per_mwh=[20.0])],  # forecast $20
        pwl_strategy=PwlBidStrategy.flat(
            discharge_capacity_mw=25.0, discharge_price=100.0,
            charge_capacity_mw=25.0, charge_price=0.0,
            as_offer_prices_per_mwh={"reg_up": 50.0},   # bid $50 > forecast $20
        ),
    )
    report = solve(
        problem,
        tmp_path,
        policy=BatteryPolicy(dispatch_mode="pwl_offers", period_coupling="coupled"),
    )
    assert report["status"] == "ok"
    assert report["extras"]["revenue_summary"]["as_revenue_dollars"] == pytest.approx(0.0, abs=1.0)


def test_pwl_as_offer_below_forecast_clears(tmp_path: Path) -> None:
    """If the BESS's AS bid is below the forecast shortfall price, the
    LP clears the battery's offer and the operator earns
    award × forecast price (not bid price) — the standard market
    pay-as-clear rule."""
    problem = BatteryProblem(
        period_durations_hours=[1.0],
        lmp_forecast_per_mwh=[5.0],
        site=_basic_site(initial_soc=50.0),
        as_products=[AsProduct(REG_UP, price_forecast_per_mwh=[20.0])],
        pwl_strategy=PwlBidStrategy.flat(
            discharge_capacity_mw=25.0, discharge_price=100.0,
            charge_capacity_mw=25.0, charge_price=0.0,
            as_offer_prices_per_mwh={"reg_up": 5.0},   # bid $5 < forecast $20
        ),
    )
    report = solve(
        problem,
        tmp_path,
        policy=BatteryPolicy(dispatch_mode="pwl_offers", period_coupling="coupled"),
    )
    assert report["status"] == "ok"
    # 25 MW award × $20 forecast × 1h = $500
    assert report["extras"]["revenue_summary"]["as_revenue_dollars"] == pytest.approx(500.0, rel=1e-3)


# ---------------------------------------------------------------------------
# Shape-preserving artifacts across modes
# ---------------------------------------------------------------------------


def test_sequential_writes_list_of_dispatch_results(tmp_path: Path) -> None:
    """Sequential dispatch-result.json is a JSON list; coupled is a single object."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0, 1.0],
        lmp_forecast_per_mwh=[30.0, 40.0, 50.0],
        site=_basic_site(initial_soc=50.0),
    )
    import json
    report = solve(
        problem,
        tmp_path,
        policy=BatteryPolicy(period_coupling="sequential"),
    )
    assert report["status"] == "ok"
    payload = json.loads(Path(report["artifacts"]["dispatch_result"]).read_text())
    assert isinstance(payload, list)
    assert len(payload) == 3

    report2 = solve(
        problem,
        tmp_path / "coupled",
        policy=BatteryPolicy(period_coupling="coupled"),
    )
    payload2 = json.loads(Path(report2["artifacts"]["dispatch_result"]).read_text())
    assert isinstance(payload2, dict)
    assert payload2["study"]["periods"] == 3
