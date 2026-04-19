# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import math
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.violations import compute_solution_violations


def _simple_2bus_case() -> tuple[dict, dict]:
    """Two buses connected by one AC line.

    bus_0 has a producer injecting 1.0 pu.
    bus_1 has a consumer withdrawing 0.95 pu (losses ~ 0.05).
    V/theta are set so the pi-model flow balances exactly at bus_0
    and leaves a known residual at bus_1.
    """
    r, x, b = 0.01, 0.1, 0.02
    z_sq = r * r + x * x
    g_sr = r / z_sq
    b_sr = -x / z_sq

    vm0, va0 = 1.05, 0.0
    vm1, va1 = 1.00, -0.05

    th = va0 - va1
    cos_t = math.cos(th)
    sin_t = math.sin(th)
    vft = vm0 * vm1

    # Compute exact branch flows to construct a balanced problem
    g_ff = g_sr
    b_ff = b_sr + b / 2.0
    pf = g_ff * vm0 * vm0 - g_sr * vft * cos_t - b_sr * vft * sin_t
    qf = -b_ff * vm0 * vm0 + b_sr * vft * cos_t - g_sr * vft * sin_t

    g_tt = g_sr
    b_tt = b_sr + b / 2.0
    pt = g_tt * vm1 * vm1 - g_sr * vft * cos_t + b_sr * vft * sin_t
    qt = -b_tt * vm1 * vm1 + b_sr * vft * cos_t + g_sr * vft * sin_t

    problem = {
        "network": {
            "general": {"base_norm_mva": 100},
            "bus": [{"uid": "bus_0"}, {"uid": "bus_1"}],
            "ac_line": [{
                "uid": "acl_0",
                "fr_bus": "bus_0",
                "to_bus": "bus_1",
                "r": r, "x": x, "b": b,
                "additional_shunt": 0,
                "mva_ub_nom": 5.0,
                "initial_status": {"on_status": 1},
            }],
            "two_winding_transformer": [],
            "dc_line": [],
            "shunt": [],
            "simple_dispatchable_device": [
                {"uid": "gen_0", "bus": "bus_0", "device_type": "producer"},
                {"uid": "load_0", "bus": "bus_1", "device_type": "consumer"},
            ],
            "violation_cost": {
                "p_bus_vio_cost": 1_000_000,
                "q_bus_vio_cost": 1_000_000,
                "s_vio_cost": 500,
            },
        },
        "time_series_input": {
            "general": {"interval_duration": [1]},
            "simple_dispatchable_device": [],
        },
    }

    # Producer injects exactly pf (so bus_0 balances perfectly).
    # Consumer withdraws |pt| (so bus_1 balances perfectly).
    solution = {
        "time_series_output": {
            "bus": [
                {"uid": "bus_0", "vm": [vm0], "va": [va0]},
                {"uid": "bus_1", "vm": [vm1], "va": [va1]},
            ],
            "ac_line": [{"uid": "acl_0", "on_status": [1]}],
            "two_winding_transformer": [],
            "dc_line": [],
            "shunt": [],
            "simple_dispatchable_device": [
                {"uid": "gen_0", "p_on": [pf], "q": [qf], "on_status": [1]},
                {"uid": "load_0", "p_on": [-pt], "q": [-qt], "on_status": [1]},
            ],
        },
    }
    return problem, solution


def test_balanced_case_reports_zero_violations() -> None:
    problem, solution = _simple_2bus_case()
    report = compute_solution_violations(problem, solution)

    summary = report["summary"]
    assert summary["bus_p_balance"]["total_mismatch_mw"] == pytest.approx(0.0, abs=0.01)
    assert summary["bus_q_balance"]["total_mismatch_mvar"] == pytest.approx(0.0, abs=0.01)
    assert summary["branch_thermal"]["total_overload_mva"] == pytest.approx(0.0, abs=0.01)
    assert summary["total_penalty_cost"] == pytest.approx(0.0, abs=1.0)

    assert len(report["periods"]) == 1
    p0 = report["periods"][0]
    assert p0["bus_p_balance"]["total_abs_mismatch_mw"] == pytest.approx(0.0, abs=0.01)


def test_imbalanced_case_reports_violations() -> None:
    problem, solution = _simple_2bus_case()

    # Shift the producer's output by +0.1 pu → bus_0 has 10 MW excess injection
    sdd = solution["time_series_output"]["simple_dispatchable_device"]
    sdd[0]["p_on"] = [sdd[0]["p_on"][0] + 0.1]

    report = compute_solution_violations(problem, solution)

    p0 = report["periods"][0]
    # bus_0 should have ~0.1 pu mismatch
    assert p0["bus_p_balance"]["total_abs_mismatch_mw"] == pytest.approx(10.0, abs=0.5)
    assert p0["bus_p_balance"]["max_abs_mismatch_bus"] == "bus_0"
    assert p0["bus_p_balance"]["penalty_cost"] > 0
    assert "bus_0" in p0["bus_p_balance"]["buses"]

    summary = report["summary"]
    assert summary["bus_p_balance"]["total_mismatch_mw"] == pytest.approx(10.0, abs=0.5)
    assert summary["total_penalty_cost"] > 0


def test_thermal_overload_reported() -> None:
    problem, solution = _simple_2bus_case()

    # Set s_max very low to trigger thermal violation
    problem["network"]["ac_line"][0]["mva_ub_nom"] = 0.001

    report = compute_solution_violations(problem, solution)

    p0 = report["periods"][0]
    assert len(p0["branch_thermal"]["violations"]) == 1
    viol = p0["branch_thermal"]["violations"][0]
    assert viol["uid"] == "acl_0"
    assert viol["overload_mva"] > 0
    assert p0["branch_thermal"]["penalty_cost"] > 0


def test_report_structure() -> None:
    problem, solution = _simple_2bus_case()
    report = compute_solution_violations(problem, solution)

    assert "summary" in report
    assert "periods" in report
    assert len(report["periods"]) == 1

    summary = report["summary"]
    for key in ("bus_p_balance", "bus_q_balance", "branch_thermal"):
        assert key in summary
    assert "total_penalty_cost" in summary

    p0 = report["periods"][0]
    assert p0["period_index"] == 0
    for key in ("bus_p_balance", "bus_q_balance", "branch_thermal"):
        assert key in p0


def test_startup_trajectory_and_raw_q_are_counted_in_bus_balance() -> None:
    problem = {
        "network": {
            "general": {"base_norm_mva": 100},
            "bus": [{"uid": "bus_0"}],
            "ac_line": [],
            "two_winding_transformer": [],
            "dc_line": [],
            "shunt": [],
            "simple_dispatchable_device": [{
                "uid": "gen_0",
                "bus": "bus_0",
                "device_type": "producer",
                "initial_status": {"on_status": 0, "p": 0.0, "q": 0.0},
                "p_startup_ramp_ub": 1.0,
                "p_shutdown_ramp_ub": 2.0,
            }],
            "violation_cost": {
                "p_bus_vio_cost": 1_000_000,
                "q_bus_vio_cost": 1_000_000,
                "s_vio_cost": 500,
            },
        },
        "time_series_input": {
            "general": {"interval_duration": [1.0, 1.0, 1.0, 1.0]},
            "simple_dispatchable_device": [{
                "uid": "gen_0",
                "p_lb": [0.0, 0.0, 0.0, 4.0],
                "q_lb": [-10.0, -10.0, -10.0, -10.0],
                "q_ub": [10.0, 10.0, 10.0, 10.0],
            }],
        },
    }
    solution = {
        "time_series_output": {
            "bus": [{
                "uid": "bus_0",
                "vm": [1.0, 1.0, 1.0, 1.0],
                "va": [0.0, 0.0, 0.0, 0.0],
            }],
            "ac_line": [],
            "two_winding_transformer": [],
            "dc_line": [],
            "shunt": [],
            "simple_dispatchable_device": [{
                "uid": "gen_0",
                "on_status": [0, 0, 0, 1],
                "p_on": [0.0, 0.0, 0.0, 4.0],
                "q": [0.0, 0.3, 0.2, 0.1],
            }],
        },
    }

    report = compute_solution_violations(problem, solution)

    p0 = report["periods"][0]
    p1 = report["periods"][1]
    p2 = report["periods"][2]
    assert p0["bus_p_balance"]["total_abs_mismatch_mw"] == pytest.approx(100.0, abs=0.01)
    assert p1["bus_p_balance"]["total_abs_mismatch_mw"] == pytest.approx(200.0, abs=0.01)
    assert p2["bus_p_balance"]["total_abs_mismatch_mw"] == pytest.approx(300.0, abs=0.01)
    assert p1["bus_q_balance"]["total_abs_mismatch_mvar"] == pytest.approx(30.0, abs=0.01)
    assert p2["bus_q_balance"]["total_abs_mismatch_mvar"] == pytest.approx(20.0, abs=0.01)
