# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Tests for detailed solution comparison."""

from __future__ import annotations

import json
from pathlib import Path
import sys

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.detail_compare import (
    BranchComparison,
    CostWaterfallEntry,
    DetailedComparison,
    DeviceComparison,
    build_commitment_diff_summary,
    build_cost_waterfall,
    classify_divergence,
    compare_branch_schedules,
    compare_bus_schedules,
    compare_device_schedules,
    compare_solutions,
    comparison_to_dict,
    compute_system_period_totals,
    find_first_divergence_period,
    summarize_comparison,
)


def _solution(devices=None, buses=None, ac_lines=None, xfmrs=None, dc_lines=None) -> dict:
    return {
        "time_series_output": {
            "simple_dispatchable_device": devices or [],
            "bus": buses or [],
            "ac_line": ac_lines or [],
            "two_winding_transformer": xfmrs or [],
            "dc_line": dc_lines or [],
            "shunt": [],
        }
    }


def _device(uid, on_status, p_on, q=None):
    return {
        "uid": uid,
        "on_status": on_status,
        "p_on": p_on,
        "q": q or [0.0] * len(p_on),
    }


# ---------------------------------------------------------------------------
# Device comparison
# ---------------------------------------------------------------------------


def test_compare_device_schedules_identical() -> None:
    lhs = _solution(devices=[_device("sd_001", [1, 1], [0.5, 0.6])])
    rhs = _solution(devices=[_device("sd_001", [1, 1], [0.5, 0.6])])
    result = compare_device_schedules(lhs, rhs, base_mva=100.0)
    assert len(result) == 1
    assert result[0].commitment_matches is True
    assert result[0].max_abs_p_delta == 0.0
    assert result[0].on_status_diffs == []
    assert result[0].first_divergence_period is None


def test_compare_device_schedules_detects_p_delta() -> None:
    lhs = _solution(devices=[_device("sd_001", [1, 1], [0.5, 0.6])])
    rhs = _solution(devices=[_device("sd_001", [1, 1], [0.5, 0.8])])
    result = compare_device_schedules(lhs, rhs, base_mva=100.0)
    assert result[0].max_abs_p_delta == 20.0  # (0.6 - 0.8) * 100
    assert result[0].commitment_matches is True
    assert result[0].first_divergence_period == 1


def test_compare_device_schedules_detects_commitment_diff() -> None:
    lhs = _solution(devices=[_device("sd_001", [1, 0], [0.5, 0.0])])
    rhs = _solution(devices=[_device("sd_001", [1, 1], [0.5, 0.6])])
    result = compare_device_schedules(lhs, rhs, base_mva=1.0)
    assert result[0].commitment_matches is False
    assert result[0].on_status_diffs == [1]
    assert result[0].first_divergence_period == 1


# ---------------------------------------------------------------------------
# Bus comparison
# ---------------------------------------------------------------------------


def test_compare_bus_schedules_detects_voltage_delta() -> None:
    lhs = _solution(buses=[{"uid": "bus_01", "va": [0.0, 0.01], "vm": [1.0, 1.05]}])
    rhs = _solution(buses=[{"uid": "bus_01", "va": [0.0, 0.02], "vm": [1.0, 1.0]}])
    result = compare_bus_schedules(lhs, rhs)
    assert len(result) == 1
    assert abs(result[0].max_abs_vm_delta - 0.05) < 1e-12
    assert abs(result[0].max_abs_va_delta - 0.01) < 1e-12


def test_compare_bus_schedules_identical() -> None:
    lhs = _solution(buses=[{"uid": "bus_01", "va": [0.0], "vm": [1.0]}])
    rhs = _solution(buses=[{"uid": "bus_01", "va": [0.0], "vm": [1.0]}])
    result = compare_bus_schedules(lhs, rhs)
    assert result[0].max_abs_vm_delta == 0.0


# ---------------------------------------------------------------------------
# Branch comparison
# ---------------------------------------------------------------------------


def test_compare_branch_schedules_detects_switching() -> None:
    lhs = _solution(ac_lines=[{"uid": "acl_001", "on_status": [1, 0]}])
    rhs = _solution(ac_lines=[{"uid": "acl_001", "on_status": [1, 1]}])
    result = compare_branch_schedules(lhs, rhs)
    assert len(result) == 1
    assert result[0].differently_switched is True
    assert result[0].on_status_diffs == [1]


def test_compare_branch_schedules_identical() -> None:
    lhs = _solution(ac_lines=[{"uid": "acl_001", "on_status": [1, 1]}])
    rhs = _solution(ac_lines=[{"uid": "acl_001", "on_status": [1, 1]}])
    result = compare_branch_schedules(lhs, rhs)
    assert result[0].differently_switched is False


# ---------------------------------------------------------------------------
# System totals
# ---------------------------------------------------------------------------


def test_compute_system_period_totals() -> None:
    payload = _solution(devices=[
        _device("gen_1", [1, 1], [0.5, 0.6]),
        _device("load_1", [1, 1], [-0.3, -0.4]),
    ])
    totals = compute_system_period_totals(payload, base_mva=100.0)
    assert len(totals) == 2
    assert totals[0].total_gen_p == 50.0
    assert totals[0].total_load_p == -30.0
    assert totals[0].committed_count == 2


# ---------------------------------------------------------------------------
# Cost waterfall
# ---------------------------------------------------------------------------


def test_build_cost_waterfall() -> None:
    lhs = {"z": -1000.0, "z_cost": -1200.0, "z_penalty": 200.0}
    rhs = {"z": -1100.0, "z_cost": -1100.0, "z_penalty": 0.0}
    entries = build_cost_waterfall(lhs, rhs)
    z_entry = next(e for e in entries if e.component == "z")
    assert z_entry.lhs_value == -1000.0
    assert z_entry.rhs_value == -1100.0
    assert z_entry.delta == 100.0


def test_build_cost_waterfall_missing_keys() -> None:
    entries = build_cost_waterfall({}, {"z": 5.0})
    z_entry = next(e for e in entries if e.component == "z")
    assert z_entry.lhs_value == 0.0
    assert z_entry.rhs_value == 5.0


# ---------------------------------------------------------------------------
# Divergence analysis
# ---------------------------------------------------------------------------


def test_find_first_divergence_period_no_divergence() -> None:
    dc = DeviceComparison(
        uid="sd_001", on_status_diffs=[], p_deltas=[], q_deltas=[],
        max_abs_p_delta=0.0, max_abs_q_delta=0.0,
        commitment_matches=True, first_divergence_period=None,
    )
    assert find_first_divergence_period([dc]) is None


def test_find_first_divergence_period_commitment_diff() -> None:
    dc = DeviceComparison(
        uid="sd_001", on_status_diffs=[3, 5], p_deltas=[], q_deltas=[],
        max_abs_p_delta=0.0, max_abs_q_delta=0.0,
        commitment_matches=False, first_divergence_period=3,
    )
    assert find_first_divergence_period([dc]) == 3


def test_classify_divergence_switching() -> None:
    comparison = DetailedComparison(
        lhs_label="a", rhs_label="b", periods=2,
        device_comparisons=[], bus_comparisons=[],
        branch_comparisons=[BranchComparison("acl_001", "ac_line", [1], True)],
        lhs_system_totals=[], rhs_system_totals=[],
        cost_waterfall=[], first_divergence_period=None,
        divergence_classification=None, commitment_diff_summary={},
    )
    assert classify_divergence(comparison) == "switching"


def test_classify_divergence_commitment() -> None:
    devices = [
        DeviceComparison(f"sd_{i:03d}", [0], [], [], 0.0, 0.0, False, 0)
        for i in range(10)
    ]
    comparison = DetailedComparison(
        lhs_label="a", rhs_label="b", periods=1,
        device_comparisons=devices, bus_comparisons=[],
        branch_comparisons=[], lhs_system_totals=[], rhs_system_totals=[],
        cost_waterfall=[], first_divergence_period=None,
        divergence_classification=None, commitment_diff_summary={},
    )
    assert classify_divergence(comparison) == "commitment"


def test_classify_divergence_penalty_dominated() -> None:
    comparison = DetailedComparison(
        lhs_label="a", rhs_label="b", periods=1,
        device_comparisons=[
            DeviceComparison("sd_001", [], [], [], 0.0, 0.0, True, None),
        ],
        bus_comparisons=[], branch_comparisons=[],
        lhs_system_totals=[], rhs_system_totals=[],
        cost_waterfall=[
            CostWaterfallEntry("z_penalty", 500.0, 0.0, 500.0),
            CostWaterfallEntry("z_cost", -1000.0, -1001.0, 1.0),
        ],
        first_divergence_period=None,
        divergence_classification=None, commitment_diff_summary={},
    )
    assert classify_divergence(comparison) == "penalty_dominated"


def test_classify_divergence_none() -> None:
    comparison = DetailedComparison(
        lhs_label="a", rhs_label="b", periods=1,
        device_comparisons=[
            DeviceComparison("sd_001", [], [], [], 0.0, 0.0, True, None),
        ],
        bus_comparisons=[], branch_comparisons=[],
        lhs_system_totals=[], rhs_system_totals=[],
        cost_waterfall=[], first_divergence_period=None,
        divergence_classification=None, commitment_diff_summary={},
    )
    assert classify_divergence(comparison) is None


# ---------------------------------------------------------------------------
# Commitment diff
# ---------------------------------------------------------------------------


def test_build_commitment_diff_summary() -> None:
    lhs = _solution(devices=[_device("sd_001", [1, 0], [0.5, 0.0])])
    rhs = _solution(devices=[_device("sd_001", [1, 1], [0.5, 0.6])])
    result = build_commitment_diff_summary(lhs, rhs)
    assert result["total_devices"] == 1
    assert result["devices_with_diffs"] == 1
    assert len(result["shutdown_diffs"]) == 1
    assert result["shutdown_diffs"][0]["uid"] == "sd_001"
    assert result["net_commitment_delta_by_period"] == [0, -1]


# ---------------------------------------------------------------------------
# End-to-end
# ---------------------------------------------------------------------------


def test_compare_solutions_end_to_end(tmp_path: Path) -> None:
    lhs = _solution(
        devices=[_device("sd_001", [1, 1], [0.5, 0.6])],
        buses=[{"uid": "bus_01", "va": [0.0, 0.01], "vm": [1.0, 1.01]}],
        ac_lines=[{"uid": "acl_001", "on_status": [1, 1]}],
    )
    rhs = _solution(
        devices=[_device("sd_001", [1, 1], [0.5, 0.8])],
        buses=[{"uid": "bus_01", "va": [0.0, 0.02], "vm": [1.0, 1.0]}],
        ac_lines=[{"uid": "acl_001", "on_status": [1, 1]}],
    )
    lhs_path = tmp_path / "lhs.json"
    rhs_path = tmp_path / "rhs.json"
    lhs_path.write_text(json.dumps(lhs), encoding="utf-8")
    rhs_path.write_text(json.dumps(rhs), encoding="utf-8")

    comparison = compare_solutions(lhs_path, rhs_path, lhs_label="ours", rhs_label="winner")
    assert comparison.lhs_label == "ours"
    assert comparison.rhs_label == "winner"
    assert comparison.periods == 2
    assert len(comparison.device_comparisons) == 1
    assert len(comparison.bus_comparisons) == 1


def test_comparison_to_dict_serializes(tmp_path: Path) -> None:
    lhs = _solution(devices=[_device("sd_001", [1], [0.5])])
    rhs = _solution(devices=[_device("sd_001", [1], [0.5])])
    lhs_path = tmp_path / "lhs.json"
    rhs_path = tmp_path / "rhs.json"
    lhs_path.write_text(json.dumps(lhs), encoding="utf-8")
    rhs_path.write_text(json.dumps(rhs), encoding="utf-8")

    comparison = compare_solutions(lhs_path, rhs_path)
    d = comparison_to_dict(comparison)
    # Verify it's JSON-serializable
    serialized = json.dumps(d)
    assert isinstance(json.loads(serialized), dict)


def test_summarize_comparison_produces_lines(tmp_path: Path) -> None:
    lhs = _solution(devices=[_device("sd_001", [1, 0], [0.5, 0.0])])
    rhs = _solution(devices=[_device("sd_001", [1, 1], [0.5, 0.6])])
    lhs_path = tmp_path / "lhs.json"
    rhs_path = tmp_path / "rhs.json"
    lhs_path.write_text(json.dumps(lhs), encoding="utf-8")
    rhs_path.write_text(json.dumps(rhs), encoding="utf-8")

    comparison = compare_solutions(lhs_path, rhs_path, lhs_label="ours", rhs_label="ref")
    lines = summarize_comparison(comparison)
    assert any("ours vs ref" in line for line in lines)
    assert len(lines) > 1
