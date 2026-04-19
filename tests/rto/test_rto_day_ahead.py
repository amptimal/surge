# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""End-to-end and unit tests for markets.rto day-ahead."""

from __future__ import annotations

import json
import math
from pathlib import Path

import pytest

import surge
from surge.market import ZonalRequirement

from markets.rto import (
    GeneratorReserveOfferSchedule,
    RtoPolicy,
    RtoProblem,
    build_workflow,
    default_reserve_products,
    solve,
)
from markets.rto.export import extract_settlement


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _case14_problem(
    periods: int = 6,
    bus4_load: list[float] | None = None,
    reserve_req_mw: float = 0.0,
) -> RtoProblem:
    if bus4_load is None:
        bus4_load = [35.0, 45.0, 55.0, 70.0, 55.0, 40.0][:periods]
    reqs = []
    if reserve_req_mw > 0:
        reqs.append(
            ZonalRequirement(
                zone_id=1,
                product_id="reg_up",
                requirement_mw=reserve_req_mw,
                per_period_mw=[reserve_req_mw] * periods,
            )
        )
    return RtoProblem.from_dicts(
        surge.case14(),
        period_durations_hours=[1.0] * periods,
        load_forecast_mw={4: bus4_load},
        reserve_requirements=reqs,
    )


# ---------------------------------------------------------------------------
# End-to-end
# ---------------------------------------------------------------------------


def test_solve_end_to_end(tmp_path: Path) -> None:
    problem = _case14_problem(periods=6, reserve_req_mw=3.0)
    report = solve(
        problem,
        tmp_path,
        policy=RtoPolicy(lp_solver="highs", commitment_mode="all_committed"),
        label="case14-6p",
    )

    assert report["status"] == "ok"
    assert report["extras"]["periods"] == 6
    assert report["extras"]["stages"] == ["dam_scuc"]
    assert (
        report["extras"]["total_cost"] is not None
        and report["extras"]["total_cost"] > 0
    )
    assert Path(report["run_report_path"]).is_file()
    assert Path(report["artifacts"]["settlement"]).is_file()
    assert Path(report["artifacts"]["dispatch_result"]).is_file()

    settlement = json.loads(Path(report["artifacts"]["settlement"]).read_text())
    assert len(settlement["lmps_per_period"]) == 6
    # Every bus must have an LMP reported.
    for p in settlement["lmps_per_period"]:
        assert len(p["buses"]) == 14
        for b in p["buses"]:
            assert b["lmp"] is not None


def test_settlement_identity_lossless(tmp_path: Path) -> None:
    """On a lossless DC clearing, load_payment == gen_revenue + congestion_rent."""
    problem = _case14_problem(periods=3)
    report = solve(
        problem,
        tmp_path,
        policy=RtoPolicy(lp_solver="highs", commitment_mode="all_committed"),
    )
    settlement = json.loads(Path(report["artifacts"]["settlement"]).read_text())
    totals = settlement["totals"]
    lhs = totals["load_payment_dollars"]
    rhs = totals["energy_payment_dollars"] + totals["congestion_rent_dollars"]
    assert lhs == pytest.approx(rhs, rel=1e-6)


def test_lmps_rise_with_load(tmp_path: Path) -> None:
    """LMP at a demand-bearing bus monotonically rises with load."""
    # Reset the network each time to avoid residual state from previous solves.
    loads = sorted([30.0, 45.0, 60.0, 80.0])
    prev_lmp = 0.0
    for i, load in enumerate(loads):
        problem = _case14_problem(periods=1, bus4_load=[load])
        report = solve(
            problem,
            tmp_path / f"p{i}",
            policy=RtoPolicy(lp_solver="highs", commitment_mode="all_committed"),
        )
        settlement = json.loads(Path(report["artifacts"]["settlement"]).read_text())
        b4 = next(b for b in settlement["lmps_per_period"][0]["buses"] if b["bus_number"] == 4)
        lmp = b4["lmp"]
        assert lmp >= prev_lmp - 1e-6, (
            f"LMP should rise with load; got {lmp} at load={load}, prev={prev_lmp}"
        )
        prev_lmp = lmp


def test_reserve_shortfall_penalty(tmp_path: Path) -> None:
    """Without any reserve offers, a 5 MW reg-up requirement fully shorts."""
    problem = _case14_problem(periods=2, reserve_req_mw=5.0)
    report = solve(
        problem,
        tmp_path,
        policy=RtoPolicy(
            lp_solver="highs",
            commitment_mode="all_committed",
            reserve_shortfall_cost_per_mwh=2000.0,
        ),
    )
    # 5 MW × 2000 × 2 periods = $20 000 penalty (plus $5 shortfall-price
    # kicker from the solver's ε-break, hence "≈").
    settlement = json.loads(Path(report["artifacts"]["settlement"]).read_text())
    shortfall = settlement["totals"]["shortfall_penalty_dollars"]
    assert shortfall == pytest.approx(20_000.0, abs=50.0)


def test_reserve_awards_clear_with_offer(tmp_path: Path) -> None:
    """When a generator supplies a reg-up offer, the requirement clears at the offer price."""
    # Build a problem with a reg-up offer on gen_1_1 at $3/MWh.
    problem = _case14_problem(periods=1, reserve_req_mw=5.0)
    problem.reserve_offers = [
        GeneratorReserveOfferSchedule(
            resource_id="gen_1_1",
            offers_by_period=[
                [
                    {"product_id": "reg_up", "capacity_mw": 50.0, "cost_per_mwh": 3.0},
                ],
            ],
        )
    ]
    report = solve(
        problem,
        tmp_path,
        policy=RtoPolicy(lp_solver="highs", commitment_mode="all_committed"),
    )
    settlement = json.loads(Path(report["artifacts"]["settlement"]).read_text())
    # No shortfall.
    assert settlement["totals"]["shortfall_penalty_dollars"] == pytest.approx(0.0, abs=1.0)
    # AS clearing price should be ≈ $3/MWh (the marginal offer), within solver tolerance.
    as_rows = settlement["as_awards"]
    assert len(as_rows) == 1
    reg_up = as_rows[0]
    assert reg_up["product_id"] == "reg_up"
    assert reg_up["provided_mw"] == pytest.approx(5.0, abs=0.01)
    assert 2.0 <= reg_up["clearing_price"] <= 10.0


# ---------------------------------------------------------------------------
# build_request / workflow smoke
# ---------------------------------------------------------------------------


def test_build_request_shape() -> None:
    problem = _case14_problem(periods=3)
    workflow, request = build_workflow(problem, RtoPolicy(commitment_mode="all_committed"))
    assert workflow.stages[0].stage_id == "dam_scuc"
    assert request["timeline"] == {"periods": 3, "interval_hours_by_period": [1.0, 1.0, 1.0]}
    assert request["commitment"] == "all_committed"
    assert request["runtime"]["run_pricing"] is True
    assert request["profiles"]["load"]["profiles"][0]["bus_number"] == 4
    assert len(request["market"]["reserve_products"]) == 4


def test_build_request_rejects_misaligned_load_length() -> None:
    # Construct a problem by hand where load len != periods.
    problem = RtoProblem(
        network=surge.case14(),
        period_durations_hours=[1.0, 1.0, 1.0],
        load_forecast_mw={4: [30.0, 40.0]},  # 2 values, but 3 periods
    )
    with pytest.raises(ValueError, match="has 2 values but problem has 3 periods"):
        build_workflow(problem, RtoPolicy(commitment_mode="all_committed"))


# ---------------------------------------------------------------------------
# CSV loader
# ---------------------------------------------------------------------------


def test_from_csvs_roundtrip(tmp_path: Path) -> None:
    load_csv = tmp_path / "load.csv"
    load_csv.write_text(
        "bus_number,period,value_mw\n"
        "4,0,30.0\n4,1,45.0\n4,2,60.0\n"
        "5,0,7.6\n5,1,7.6\n5,2,7.6\n",
        encoding="utf-8",
    )
    reserves_csv = tmp_path / "reserves.csv"
    reserves_csv.write_text(
        "zone_id,product_id,period,requirement_mw\n"
        "1,reg_up,0,5.0\n1,reg_up,1,5.0\n1,reg_up,2,5.0\n",
        encoding="utf-8",
    )
    problem = RtoProblem.from_csvs(
        surge.case14(),
        load_csv=load_csv,
        reserves_csv=reserves_csv,
    )
    assert problem.periods == 3
    assert problem.load_forecast_mw[4] == [30.0, 45.0, 60.0]
    assert problem.load_forecast_mw[5] == [7.6, 7.6, 7.6]
    assert len(problem.reserve_requirements) == 1
    r = problem.reserve_requirements[0]
    assert r.zone_id == 1
    assert r.product_id == "reg_up"
    assert r.per_period_mw == [5.0, 5.0, 5.0]
