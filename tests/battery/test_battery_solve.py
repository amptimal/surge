# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Battery operator solve tests — arbitrage, AS co-optimisation, degradation."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from surge.market import REG_UP, SPINNING

from markets.battery import (
    AsProduct,
    BatteryPolicy,
    BatteryProblem,
    SiteSpec,
    solve,
)


def _basic_site(
    *,
    initial_soc: float = 0.0,
    degradation: float = 0.0,
    charge_mw: float = 25.0,
    discharge_mw: float = 25.0,
    energy_mwh: float = 100.0,
    efficiency: float = 1.0,
) -> SiteSpec:
    # Test helper still takes a single round-trip scalar for terseness; split
    # symmetrically into sqrt per leg so the pre-existing test expectations
    # (computed against the old single-field model) continue to hold.
    leg = max(0.0, efficiency) ** 0.5
    return SiteSpec(
        poi_limit_mw=50.0,
        bess_power_charge_mw=charge_mw,
        bess_power_discharge_mw=discharge_mw,
        bess_energy_mwh=energy_mwh,
        bess_charge_efficiency=leg,
        bess_discharge_efficiency=leg,
        bess_soc_min_fraction=0.0,
        bess_soc_max_fraction=1.0,
        bess_initial_soc_mwh=initial_soc,
        bess_degradation_cost_per_mwh=degradation,
    )


# ---------------------------------------------------------------------------
# Arbitrage
# ---------------------------------------------------------------------------


def test_two_period_arbitrage_empty_battery(tmp_path: Path) -> None:
    """Textbook arbitrage: charge at $20, discharge at $80 → $1 500 on 25 MW × 1h spread."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[20.0, 80.0],
        site=_basic_site(initial_soc=0.0),
    )
    report = solve(problem, tmp_path, policy=BatteryPolicy())
    assert report["status"] == "ok"
    summary = report["extras"]["revenue_summary"]
    assert summary["energy_revenue_dollars"] == pytest.approx(1_500.0, rel=1e-3)
    assert summary["as_revenue_dollars"] == 0.0
    assert summary["degradation_cost_dollars"] == 0.0

    rev = json.loads((tmp_path / "revenue-report.json").read_text())
    assert rev["schedule"][0]["charge_mw"] == pytest.approx(25.0, rel=1e-3)
    assert rev["schedule"][0]["discharge_mw"] == pytest.approx(0.0, abs=1e-3)
    assert rev["schedule"][1]["charge_mw"] == pytest.approx(0.0, abs=1e-3)
    assert rev["schedule"][1]["discharge_mw"] == pytest.approx(25.0, rel=1e-3)


def test_dump_soc_when_rising_price(tmp_path: Path) -> None:
    """Full initial SOC + rising LMP → dump all energy at the highest price."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0, 1.0, 1.0],
        lmp_forecast_per_mwh=[10.0, 30.0, 60.0, 90.0],
        site=_basic_site(initial_soc=100.0, energy_mwh=100.0),
    )
    report = solve(problem, tmp_path, policy=BatteryPolicy())
    assert report["status"] == "ok"
    rev = json.loads((tmp_path / "revenue-report.json").read_text())
    # All discharge, no charging.
    total_charge = sum(r["charge_mw"] for r in rev["schedule"])
    total_discharge = sum(r["discharge_mw"] for r in rev["schedule"])
    assert total_charge == pytest.approx(0.0, abs=1e-3)
    # 100 MWh total, 25 MW/h cap → minimum 4 hours. All 4 periods.
    assert total_discharge == pytest.approx(100.0, rel=1e-3)
    # Most discharge must happen at higher LMPs (periods 2, 3 carry
    # more MWh than periods 0, 1 if efficiency=1).
    total_revenue = sum(r["energy_revenue_dollars"] for r in rev["schedule"])
    assert total_revenue == pytest.approx(
        (25.0 * 10.0 + 25.0 * 30.0 + 25.0 * 60.0 + 25.0 * 90.0),
        rel=1e-2,
    )


def test_efficiency_reduces_arbitrage_profit(tmp_path: Path) -> None:
    """With 80 % round-trip efficiency and a $60 spread, arbitrage shrinks."""
    # Lossless: 25 MW × $60 × 1h = $1500
    lossless = solve(
        BatteryProblem(
            period_durations_hours=[1.0, 1.0],
            lmp_forecast_per_mwh=[20.0, 80.0],
            site=_basic_site(initial_soc=0.0, efficiency=1.0),
        ),
        tmp_path / "lossless",
        policy=BatteryPolicy(),
    )
    lossy = solve(
        BatteryProblem(
            period_durations_hours=[1.0, 1.0],
            lmp_forecast_per_mwh=[20.0, 80.0],
            site=_basic_site(initial_soc=0.0, efficiency=0.64),  # √0.64 = 0.8 per leg
        ),
        tmp_path / "lossy",
        policy=BatteryPolicy(),
    )
    assert lossless["status"] == "ok" and lossy["status"] == "ok"
    assert lossy["extras"]["revenue_summary"]["energy_revenue_dollars"] < lossless["extras"]["revenue_summary"]["energy_revenue_dollars"]


# ---------------------------------------------------------------------------
# AS co-optimization
# ---------------------------------------------------------------------------


def test_as_pays_more_than_energy_reserves_all_capacity(tmp_path: Path) -> None:
    """Flat LMP $5, reg-up $50 → battery reserves all capacity for AS."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0, 1.0],
        lmp_forecast_per_mwh=[5.0, 5.0, 5.0],
        site=_basic_site(initial_soc=50.0),
        as_products=[AsProduct(REG_UP, price_forecast_per_mwh=[50.0] * 3)],
    )
    report = solve(problem, tmp_path, policy=BatteryPolicy())
    assert report["status"] == "ok"
    summary = report["extras"]["revenue_summary"]
    assert summary["energy_revenue_dollars"] == pytest.approx(0.0, abs=1.0)
    # 25 MW × $50 × 3 hours = $3 750
    assert summary["as_revenue_dollars"] == pytest.approx(3_750.0, rel=1e-3)


def test_energy_beats_cheap_as(tmp_path: Path) -> None:
    """With LMP spread $60 vs reg-up at $1/MW, battery arbitrages energy."""
    problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[20.0, 80.0],
        site=_basic_site(initial_soc=0.0),
        as_products=[AsProduct(REG_UP, price_forecast_per_mwh=[1.0] * 2)],
    )
    report = solve(problem, tmp_path, policy=BatteryPolicy())
    assert report["status"] == "ok"
    summary = report["extras"]["revenue_summary"]
    # Energy-dominant: battery fully charges then fully discharges.
    assert summary["energy_revenue_dollars"] == pytest.approx(1_500.0, rel=1e-3)


def test_multiple_as_products_cooptimize(tmp_path: Path) -> None:
    """With 2 up-direction AS products sharing the same headroom limit,
    the battery's capacity is split across them according to price.

    ``SPINNING`` and ``REG_UP`` share a limit by framework convention
    (``SPINNING.shared_limit_products = (reg_up,)``), so total headroom
    awarded is capped at the battery's discharge rate.
    """
    problem = BatteryProblem(
        period_durations_hours=[1.0],
        lmp_forecast_per_mwh=[5.0],
        site=_basic_site(initial_soc=50.0),
        as_products=[
            AsProduct(REG_UP, price_forecast_per_mwh=[30.0]),
            AsProduct(SPINNING, price_forecast_per_mwh=[20.0]),
        ],
    )
    report = solve(problem, tmp_path, policy=BatteryPolicy())
    assert report["status"] == "ok"
    rev = json.loads((tmp_path / "revenue-report.json").read_text())
    # Battery should earn something on at least one product.
    assert rev["totals"]["as_revenue_dollars"] > 0


# ---------------------------------------------------------------------------
# Degradation
# ---------------------------------------------------------------------------


def test_high_degradation_shrinks_arbitrage(tmp_path: Path) -> None:
    """A degradation cost of $50/MWh throughput wipes out the $60 spread."""
    spread_problem = BatteryProblem(
        period_durations_hours=[1.0, 1.0],
        lmp_forecast_per_mwh=[20.0, 80.0],
        site=_basic_site(initial_soc=0.0, degradation=50.0),
    )
    report = solve(spread_problem, tmp_path, policy=BatteryPolicy())
    assert report["status"] == "ok"
    summary = report["extras"]["revenue_summary"]
    # Net = energy_rev - degradation_cost. With deg=$50 and 2×25 MWh
    # throughput = $2500 > $1500 revenue → battery should stay idle.
    assert summary["energy_revenue_dollars"] == pytest.approx(0.0, abs=1e-3)
    assert summary["degradation_cost_dollars"] == pytest.approx(0.0, abs=1e-3)


# ---------------------------------------------------------------------------
# Problem validation
# ---------------------------------------------------------------------------


def test_problem_rejects_forecast_length_mismatch() -> None:
    with pytest.raises(ValueError, match="lmp_forecast_per_mwh length"):
        BatteryProblem(
            period_durations_hours=[1.0, 1.0],
            lmp_forecast_per_mwh=[30.0],  # short
            site=_basic_site(),
        )


def test_problem_rejects_as_forecast_mismatch() -> None:
    with pytest.raises(ValueError, match="AS product reg_up"):
        BatteryProblem(
            period_durations_hours=[1.0, 1.0],
            lmp_forecast_per_mwh=[30.0, 40.0],
            site=_basic_site(),
            as_products=[AsProduct(REG_UP, price_forecast_per_mwh=[8.0])],  # short
        )


# ---------------------------------------------------------------------------
# Full 24-period realistic solve
# ---------------------------------------------------------------------------


def test_realistic_24_period_solve(tmp_path: Path) -> None:
    """Daily LMP shape with morning + evening peaks, BESS + reg_up +
    spinning. Sanity-check that every artifact is produced and the
    revenue totals pass common-sense bounds."""
    lmp = [
        25, 22, 20, 18, 20, 25, 30, 40, 55, 60,
        65, 70, 70, 68, 65, 60, 55, 50, 60, 75,
        80, 70, 50, 35,
    ]
    problem = BatteryProblem(
        period_durations_hours=[1.0] * 24,
        lmp_forecast_per_mwh=[float(x) for x in lmp],
        site=_basic_site(
            initial_soc=50.0,
            degradation=1.0,
            efficiency=0.88,
        ),
        as_products=[
            AsProduct(REG_UP, price_forecast_per_mwh=[8.0] * 24),
            AsProduct(SPINNING, price_forecast_per_mwh=[3.0] * 24),
        ],
    )
    report = solve(problem, tmp_path, policy=BatteryPolicy())
    assert report["status"] == "ok"

    for filename in ("run-report.json", "revenue-report.json", "dispatch-result.json"):
        assert (tmp_path / filename).is_file(), f"missing {filename}"

    summary = report["extras"]["revenue_summary"]
    # Bounds: positive net, reasonable cycle count.
    assert summary["net_revenue_dollars"] > 0
    assert 0 < summary["full_equivalent_cycles"] <= 3.0, summary
