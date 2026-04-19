# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Tests for commitment schedule extraction and injection."""

from __future__ import annotations

import json
from pathlib import Path
import sys

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.commitment import (
    BranchSchedule,
    BusSchedule,
    DeviceSchedule,
    HvdcSchedule,
    ReferenceSchedule,
    apply_fixed_commitment,
    apply_fixed_hvdc,
    apply_reference_schedule,
    extract_branch_schedules,
    extract_bus_schedules,
    extract_device_schedules,
    extract_hvdc_schedules,
    extract_reference_schedule,
    hvdc_dispatch_mw_by_link,
    on_status_by_resource,
)


def _minimal_solution(**overrides: object) -> dict:
    """Build a minimal GO C3 solution payload."""
    payload: dict = {
        "time_series_output": {
            "simple_dispatchable_device": [
                {
                    "uid": "sd_001",
                    "on_status": [1, 0, 1],
                    "p_on": [0.5, 0.0, 0.8],
                    "q": [0.1, 0.0, 0.2],
                    "p_reg_res_up": [0.01, 0.0, 0.02],
                    "p_reg_res_down": [0.01, 0.0, 0.02],
                    "p_syn_res": [0.0, 0.0, 0.0],
                    "p_nsyn_res": [0.0, 0.0, 0.0],
                    "p_ramp_res_up_online": [0.0, 0.0, 0.0],
                    "p_ramp_res_down_online": [0.0, 0.0, 0.0],
                    "p_ramp_res_up_offline": [0.0, 0.0, 0.0],
                    "p_ramp_res_down_offline": [0.0, 0.0, 0.0],
                    "q_res_up": [0.0, 0.0, 0.0],
                    "q_res_down": [0.0, 0.0, 0.0],
                },
                {
                    "uid": "sd_002",
                    "on_status": [1, 1, 1],
                    "p_on": [1.0, 0.9, 0.95],
                    "q": [0.3, 0.2, 0.25],
                },
            ],
            "bus": [
                {"uid": "bus_01", "va": [0.0, 0.01, 0.02], "vm": [1.0, 1.01, 0.99]},
                {"uid": "bus_02", "va": [0.05, 0.06, 0.07], "vm": [1.02, 1.0, 0.98]},
            ],
            "ac_line": [
                {"uid": "acl_001", "on_status": [1, 1, 0]},
            ],
            "two_winding_transformer": [
                {"uid": "xfmr_001", "on_status": [1, 1, 1]},
            ],
            "dc_line": [
                {
                    "uid": "dcl_001",
                    "pdc_fr": [0.5, 0.3, 0.4],
                    "qdc_fr": [0.1, 0.2, 0.3],
                    "qdc_to": [0.4, 0.5, 0.6],
                },
            ],
            "shunt": [],
        }
    }
    tso = payload["time_series_output"]
    for key, value in overrides.items():
        tso[key] = value
    return payload


def _minimal_request(periods: int = 3) -> dict:
    """Build a minimal dispatch request dict with commitment and market."""
    return {
        "timeline": {"periods": periods},
        "commitment": {
            "optimize": {
                "initial_conditions": [
                    {"resource_id": "sd_001", "committed": True},
                    {"resource_id": "sd_002", "committed": False},
                ],
            }
        },
        "market": {
            "generator_offer_schedules": [
                {"resource_id": "sd_001", "schedule": {}},
                {"resource_id": "sd_002", "schedule": {}},
            ]
        },
        "network": {
            "hvdc_links": [
                {"id": "dcl_001", "name": "dc1"},
            ]
        },
        "runtime": {},
    }


# ---------------------------------------------------------------------------
# Extraction tests
# ---------------------------------------------------------------------------


def test_extract_device_schedules_from_minimal_payload() -> None:
    payload = _minimal_solution()
    devices = extract_device_schedules(payload)
    assert set(devices.keys()) == {"sd_001", "sd_002"}
    assert devices["sd_001"].on_status == [True, False, True]
    assert devices["sd_001"].p_on == [0.5, 0.0, 0.8]
    assert devices["sd_001"].q == [0.1, 0.0, 0.2]
    assert devices["sd_001"].p_reg_res_up == [0.01, 0.0, 0.02]
    assert devices["sd_002"].on_status == [True, True, True]
    assert devices["sd_002"].p_on == [1.0, 0.9, 0.95]


def test_extract_device_schedules_empty_payload() -> None:
    devices = extract_device_schedules({"time_series_output": {}})
    assert devices == {}
    devices = extract_device_schedules({})
    assert devices == {}


def test_extract_device_schedules_missing_reserve_fields_default_to_zeros() -> None:
    payload = _minimal_solution()
    devices = extract_device_schedules(payload)
    # sd_002 has no explicit reserve fields — should default to zeros
    assert devices["sd_002"].p_syn_res == [0.0, 0.0, 0.0]
    assert devices["sd_002"].q_res_down == [0.0, 0.0, 0.0]


def test_extract_hvdc_schedules() -> None:
    payload = _minimal_solution()
    hvdc = extract_hvdc_schedules(payload)
    assert set(hvdc.keys()) == {"dcl_001"}
    assert hvdc["dcl_001"].pdc_fr == [0.5, 0.3, 0.4]
    assert hvdc["dcl_001"].qdc_fr == [0.1, 0.2, 0.3]
    assert hvdc["dcl_001"].qdc_to == [0.4, 0.5, 0.6]


def test_extract_hvdc_schedules_empty() -> None:
    hvdc = extract_hvdc_schedules({"time_series_output": {"dc_line": []}})
    assert hvdc == {}


def test_extract_branch_schedules() -> None:
    payload = _minimal_solution()
    branches = extract_branch_schedules(payload)
    assert set(branches.keys()) == {"acl_001", "xfmr_001"}
    assert branches["acl_001"].branch_type == "ac_line"
    assert branches["acl_001"].on_status == [True, True, False]
    assert branches["xfmr_001"].branch_type == "two_winding_transformer"
    assert branches["xfmr_001"].on_status == [True, True, True]


def test_extract_bus_schedules() -> None:
    payload = _minimal_solution()
    buses = extract_bus_schedules(payload)
    assert set(buses.keys()) == {"bus_01", "bus_02"}
    assert buses["bus_01"].va == [0.0, 0.01, 0.02]
    assert buses["bus_01"].vm == [1.0, 1.01, 0.99]


def test_extract_reference_schedule_from_file(tmp_path: Path) -> None:
    payload = _minimal_solution()
    path = tmp_path / "solution.json"
    path.write_text(json.dumps(payload), encoding="utf-8")
    schedule = extract_reference_schedule(path, label="test")
    assert schedule.source_label == "test"
    assert schedule.source_path == str(path)
    assert schedule.periods == 3
    assert len(schedule.devices) == 2
    assert len(schedule.hvdc_links) == 1
    assert len(schedule.branches) == 2
    assert len(schedule.buses) == 2


def test_extract_reference_schedule_default_label(tmp_path: Path) -> None:
    path = tmp_path / "winner.json"
    path.write_text(json.dumps(_minimal_solution()), encoding="utf-8")
    schedule = extract_reference_schedule(path)
    assert schedule.source_label == "winner.json"


# ---------------------------------------------------------------------------
# Compatibility helper tests
# ---------------------------------------------------------------------------


def test_on_status_by_resource_unfiltered() -> None:
    payload = _minimal_solution()
    path_stub = "/fake/path"
    devices = extract_device_schedules(payload)
    schedule = ReferenceSchedule(
        source_path=path_stub, source_label="test",
        devices=devices, hvdc_links={}, branches={}, buses={}, periods=3,
    )
    result = on_status_by_resource(schedule)
    assert result == {
        "sd_001": [True, False, True],
        "sd_002": [True, True, True],
    }


def test_on_status_by_resource_filtered() -> None:
    payload = _minimal_solution()
    devices = extract_device_schedules(payload)
    schedule = ReferenceSchedule(
        source_path=None, source_label="test",
        devices=devices, hvdc_links={}, branches={}, buses={}, periods=3,
    )
    result = on_status_by_resource(schedule, eligible_resource_ids={"sd_001"})
    assert set(result.keys()) == {"sd_001"}


def test_hvdc_dispatch_mw_converts_units() -> None:
    payload = _minimal_solution()
    hvdc = extract_hvdc_schedules(payload)
    schedule = ReferenceSchedule(
        source_path=None, source_label="test",
        devices={}, hvdc_links=hvdc, branches={}, buses={}, periods=3,
    )
    result = hvdc_dispatch_mw_by_link(schedule, base_mva=100.0)
    assert result == {"dcl_001": [50.0, 30.0, 40.0]}


def test_hvdc_dispatch_mw_filtered() -> None:
    payload = _minimal_solution()
    hvdc = extract_hvdc_schedules(payload)
    schedule = ReferenceSchedule(
        source_path=None, source_label="test",
        devices={}, hvdc_links=hvdc, branches={}, buses={}, periods=3,
    )
    result = hvdc_dispatch_mw_by_link(
        schedule, base_mva=100.0, eligible_link_ids={"nonexistent"},
    )
    assert result == {}


# ---------------------------------------------------------------------------
# Request mutation tests
# ---------------------------------------------------------------------------


def test_apply_fixed_commitment_mutates_request() -> None:
    payload = _minimal_solution()
    devices = extract_device_schedules(payload)
    schedule = ReferenceSchedule(
        source_path=None, source_label="test",
        devices=devices, hvdc_links={}, branches={}, buses={}, periods=3,
    )
    request = _minimal_request()
    apply_fixed_commitment(request, schedule, eligible_resource_ids={"sd_001", "sd_002"})
    fixed = request["commitment"]["fixed"]
    resources = {r["resource_id"]: r for r in fixed["resources"]}
    assert set(resources.keys()) == {"sd_001", "sd_002"}
    assert resources["sd_001"]["initial"] is True
    assert resources["sd_001"]["periods"] == [True, False, True]
    assert resources["sd_002"]["initial"] is False
    assert resources["sd_002"]["periods"] == [True, True, True]


def test_apply_fixed_commitment_missing_device_uses_false() -> None:
    schedule = ReferenceSchedule(
        source_path=None, source_label="test",
        devices={}, hvdc_links={}, branches={}, buses={}, periods=3,
    )
    request = _minimal_request()
    apply_fixed_commitment(request, schedule, eligible_resource_ids={"sd_001"})
    fixed = request["commitment"]["fixed"]
    resources = {r["resource_id"]: r for r in fixed["resources"]}
    assert resources["sd_001"]["periods"] == [False, False, False]


def test_apply_fixed_hvdc_mutates_request() -> None:
    payload = _minimal_solution()
    hvdc = extract_hvdc_schedules(payload)
    schedule = ReferenceSchedule(
        source_path=None, source_label="test",
        devices={}, hvdc_links=hvdc, branches={}, buses={}, periods=3,
    )
    request = _minimal_request()
    apply_fixed_hvdc(request, schedule, base_mva=100.0)
    fixed_dispatch = request["runtime"]["fixed_hvdc_dispatch"]
    assert len(fixed_dispatch) == 1
    assert fixed_dispatch[0]["link_id"] == "dcl_001"
    assert fixed_dispatch[0]["p_mw"] == [50.0, 30.0, 40.0]
    assert fixed_dispatch[0]["q_fr_mvar"] == [10.0, 20.0, 30.0]
    assert fixed_dispatch[0]["q_to_mvar"] == [40.0, 50.0, 60.0]


def test_apply_fixed_hvdc_no_eligible_links() -> None:
    payload = _minimal_solution()
    hvdc = extract_hvdc_schedules(payload)
    schedule = ReferenceSchedule(
        source_path=None, source_label="test",
        devices={}, hvdc_links=hvdc, branches={}, buses={}, periods=3,
    )
    request = _minimal_request()
    request["network"]["hvdc_links"] = []
    apply_fixed_hvdc(request, schedule, base_mva=100.0)
    assert "fixed_hvdc_dispatch" not in request.get("runtime", {})


def test_apply_reference_schedule_applies_both() -> None:
    payload = _minimal_solution()
    devices = extract_device_schedules(payload)
    hvdc = extract_hvdc_schedules(payload)
    schedule = ReferenceSchedule(
        source_path=None, source_label="test",
        devices=devices, hvdc_links=hvdc, branches={}, buses={}, periods=3,
    )
    request = _minimal_request()
    apply_reference_schedule(
        request, schedule,
        base_mva=100.0,
        eligible_resource_ids={"sd_001", "sd_002"},
    )
    assert "fixed" in request["commitment"]
    assert "fixed_hvdc_dispatch" in request["runtime"]


def test_apply_reference_schedule_commitment_only() -> None:
    payload = _minimal_solution()
    devices = extract_device_schedules(payload)
    hvdc = extract_hvdc_schedules(payload)
    schedule = ReferenceSchedule(
        source_path=None, source_label="test",
        devices=devices, hvdc_links=hvdc, branches={}, buses={}, periods=3,
    )
    request = _minimal_request()
    apply_reference_schedule(
        request, schedule,
        base_mva=100.0,
        eligible_resource_ids={"sd_001"},
        fix_hvdc=False,
    )
    assert "fixed" in request["commitment"]
    assert "fixed_hvdc_dispatch" not in request.get("runtime", {})
