#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Reference commitment schedule extraction and injection for GO Challenge 3."""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path
from typing import Any


_RESERVE_FIELDS = (
    "p_reg_res_up",
    "p_reg_res_down",
    "p_syn_res",
    "p_nsyn_res",
    "p_ramp_res_up_online",
    "p_ramp_res_down_online",
    "p_ramp_res_up_offline",
    "p_ramp_res_down_offline",
    "q_res_up",
    "q_res_down",
)


@dataclass(frozen=True)
class DeviceSchedule:
    """Per-device extracted schedule from a GO C3 solution."""

    uid: str
    on_status: list[bool]
    p_on: list[float]
    q: list[float]
    p_reg_res_up: list[float]
    p_reg_res_down: list[float]
    p_syn_res: list[float]
    p_nsyn_res: list[float]
    p_ramp_res_up_online: list[float]
    p_ramp_res_down_online: list[float]
    p_ramp_res_up_offline: list[float]
    p_ramp_res_down_offline: list[float]
    q_res_up: list[float]
    q_res_down: list[float]


@dataclass(frozen=True)
class HvdcSchedule:
    """Per-HVDC-link extracted schedule from a GO C3 solution."""

    uid: str
    pdc_fr: list[float]
    qdc_fr: list[float]
    qdc_to: list[float]


@dataclass(frozen=True)
class BranchSchedule:
    """Per-branch switching status from a GO C3 solution."""

    uid: str
    branch_type: str
    on_status: list[bool]


@dataclass(frozen=True)
class BusSchedule:
    """Per-bus voltage/angle from a GO C3 solution."""

    uid: str
    va: list[float]
    vm: list[float]


@dataclass(frozen=True)
class ReferenceSchedule:
    """Complete schedule extracted from a GO C3 solution."""

    source_path: str | None
    source_label: str
    devices: dict[str, DeviceSchedule]
    hvdc_links: dict[str, HvdcSchedule]
    branches: dict[str, BranchSchedule]
    buses: dict[str, BusSchedule]
    periods: int


# ---------------------------------------------------------------------------
# Extraction
# ---------------------------------------------------------------------------


def load_solution_payload(path: Path) -> dict[str, Any]:
    """Load a GO C3 solution JSON file."""
    return json.loads(path.read_text(encoding="utf-8"))


def extract_device_schedules(
    payload: dict[str, Any],
) -> dict[str, DeviceSchedule]:
    """Extract all device schedules from a solution payload."""
    devices = payload.get("time_series_output", {}).get("simple_dispatchable_device", [])
    result: dict[str, DeviceSchedule] = {}
    for device in devices:
        uid = str(device.get("uid", ""))
        if not uid:
            continue
        periods = len(device.get("on_status", []))
        result[uid] = DeviceSchedule(
            uid=uid,
            on_status=[bool(v) for v in device.get("on_status", [])],
            p_on=[float(v) for v in device.get("p_on", [])],
            q=[float(v) for v in device.get("q", [])],
            **{
                field: [float(v) for v in device.get(field, [])]
                or [0.0] * periods
                for field in _RESERVE_FIELDS
            },
        )
    return result


def extract_hvdc_schedules(
    payload: dict[str, Any],
) -> dict[str, HvdcSchedule]:
    """Extract HVDC link schedules from a solution payload."""
    dc_lines = payload.get("time_series_output", {}).get("dc_line", [])
    result: dict[str, HvdcSchedule] = {}
    for dc_line in dc_lines:
        uid = str(dc_line.get("uid", ""))
        if not uid:
            continue
        pdc_fr = dc_line.get("pdc_fr", [])
        if not isinstance(pdc_fr, list):
            continue
        qdc_fr = dc_line.get("qdc_fr", [])
        qdc_to = dc_line.get("qdc_to", [])
        result[uid] = HvdcSchedule(
            uid=uid,
            pdc_fr=[float(v) for v in pdc_fr],
            qdc_fr=[float(v) for v in qdc_fr] if isinstance(qdc_fr, list) else [],
            qdc_to=[float(v) for v in qdc_to] if isinstance(qdc_to, list) else [],
        )
    return result


def extract_branch_schedules(
    payload: dict[str, Any],
) -> dict[str, BranchSchedule]:
    """Extract branch switching schedules from a solution payload."""
    tso = payload.get("time_series_output", {})
    result: dict[str, BranchSchedule] = {}
    for ac_line in tso.get("ac_line", []):
        uid = str(ac_line.get("uid", ""))
        if not uid:
            continue
        result[uid] = BranchSchedule(
            uid=uid,
            branch_type="ac_line",
            on_status=[bool(v) for v in ac_line.get("on_status", [])],
        )
    for xfmr in tso.get("two_winding_transformer", []):
        uid = str(xfmr.get("uid", ""))
        if not uid:
            continue
        result[uid] = BranchSchedule(
            uid=uid,
            branch_type="two_winding_transformer",
            on_status=[bool(v) for v in xfmr.get("on_status", [])],
        )
    return result


def extract_bus_schedules(
    payload: dict[str, Any],
) -> dict[str, BusSchedule]:
    """Extract bus voltage/angle schedules from a solution payload."""
    buses = payload.get("time_series_output", {}).get("bus", [])
    result: dict[str, BusSchedule] = {}
    for bus in buses:
        uid = str(bus.get("uid", ""))
        if not uid:
            continue
        result[uid] = BusSchedule(
            uid=uid,
            va=[float(v) for v in bus.get("va", [])],
            vm=[float(v) for v in bus.get("vm", [])],
        )
    return result


def extract_reference_schedule(
    path: Path,
    *,
    label: str = "",
) -> ReferenceSchedule:
    """Load a solution file and extract the complete reference schedule."""
    payload = load_solution_payload(path)
    devices = extract_device_schedules(payload)
    hvdc_links = extract_hvdc_schedules(payload)
    branches = extract_branch_schedules(payload)
    buses = extract_bus_schedules(payload)
    periods = max(
        (len(dev.on_status) for dev in devices.values()),
        default=0,
    )
    return ReferenceSchedule(
        source_path=str(path),
        source_label=label or str(path.name),
        devices=devices,
        hvdc_links=hvdc_links,
        branches=branches,
        buses=buses,
        periods=periods,
    )


# ---------------------------------------------------------------------------
# Compatibility helpers (match runner._load_* return shapes)
# ---------------------------------------------------------------------------


def on_status_by_resource(
    schedule: ReferenceSchedule,
    *,
    eligible_resource_ids: set[str] | None = None,
) -> dict[str, list[bool]]:
    """Extract on/off status for each device, optionally filtered."""
    result: dict[str, list[bool]] = {}
    for uid, device in schedule.devices.items():
        if eligible_resource_ids is not None and uid not in eligible_resource_ids:
            continue
        if device.on_status:
            result[uid] = list(device.on_status)
    return result


def hvdc_dispatch_mw_by_link(
    schedule: ReferenceSchedule,
    *,
    base_mva: float,
    eligible_link_ids: set[str] | None = None,
) -> dict[str, list[float]]:
    """Extract HVDC dispatch in MW (converts from per-unit)."""
    result: dict[str, list[float]] = {}
    for uid, link in schedule.hvdc_links.items():
        if eligible_link_ids is not None and uid not in eligible_link_ids:
            continue
        if link.pdc_fr:
            result[uid] = [v * base_mva for v in link.pdc_fr]
    return result


# ---------------------------------------------------------------------------
# Request mutation
# ---------------------------------------------------------------------------


def _extract_initial_commitment_map(request: dict[str, Any]) -> dict[str, bool]:
    """Extract initial commitment booleans from a dispatch request."""
    commitment = request.get("commitment", {})
    options = None
    if isinstance(commitment, dict):
        if isinstance(commitment.get("additional"), dict):
            options = commitment["additional"].get("options")
        elif isinstance(commitment.get("optimize"), dict):
            options = commitment["optimize"]
    initial_map: dict[str, bool] = {}
    if not isinstance(options, dict):
        return initial_map
    for item in options.get("initial_conditions", []):
        if not isinstance(item, dict):
            continue
        resource_id = item.get("resource_id")
        committed = item.get("committed")
        if isinstance(resource_id, str) and isinstance(committed, bool):
            initial_map[resource_id] = committed
    return initial_map


def apply_fixed_commitment(
    request: dict[str, Any],
    schedule: ReferenceSchedule,
    *,
    eligible_resource_ids: set[str],
) -> None:
    """Mutate request to use fixed commitment from a reference schedule."""
    if not eligible_resource_ids:
        return
    initial_map = _extract_initial_commitment_map(request)
    periods = int(request.get("timeline", {}).get("periods", 0))
    request["commitment"] = {
        "fixed": {
            "resources": [
                {
                    "resource_id": resource_id,
                    "initial": initial_map.get(resource_id, False),
                    "periods": list(schedule.devices[resource_id].on_status)
                    if resource_id in schedule.devices
                    else [False] * periods,
                }
                for resource_id in sorted(eligible_resource_ids)
            ]
        }
    }


def apply_fixed_hvdc(
    request: dict[str, Any],
    schedule: ReferenceSchedule,
    *,
    base_mva: float,
) -> None:
    """Mutate request to use fixed HVDC dispatch from a reference schedule."""
    eligible_link_ids = {
        str(item.get("id"))
        for item in request.get("network", {}).get("hvdc_links", [])
        if isinstance(item, dict) and item.get("id")
    }
    if not eligible_link_ids:
        return
    fixed_schedule = []
    for uid, link in schedule.hvdc_links.items():
        if uid not in eligible_link_ids:
            continue
        if not link.pdc_fr:
            continue
        entry = {
            "link_id": uid,
            "p_mw": [v * base_mva for v in link.pdc_fr],
        }
        if link.qdc_fr:
            entry["q_fr_mvar"] = [v * base_mva for v in link.qdc_fr]
        if link.qdc_to:
            entry["q_to_mvar"] = [v * base_mva for v in link.qdc_to]
        fixed_schedule.append(entry)
    if not fixed_schedule:
        return
    runtime = request.setdefault("runtime", {})
    if not isinstance(runtime, dict):
        raise TypeError("request.runtime must be a mapping when applying fixed HVDC schedules")
    runtime["fixed_hvdc_dispatch"] = sorted(fixed_schedule, key=lambda item: item["link_id"])


def apply_reference_schedule(
    request: dict[str, Any],
    schedule: ReferenceSchedule,
    *,
    base_mva: float,
    eligible_resource_ids: set[str],
    fix_commitment: bool = True,
    fix_hvdc: bool = True,
) -> None:
    """Apply both commitment and HVDC from a reference schedule to a request."""
    if fix_commitment:
        apply_fixed_commitment(request, schedule, eligible_resource_ids=eligible_resource_ids)
    if fix_hvdc:
        apply_fixed_hvdc(request, schedule, base_mva=base_mva)
