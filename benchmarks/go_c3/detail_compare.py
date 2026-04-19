#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Detailed per-bus, per-asset, per-branch comparison of two GO C3 solutions."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .commitment import (
    extract_branch_schedules,
    extract_bus_schedules,
    extract_device_schedules,
    load_solution_payload,
)


# ---------------------------------------------------------------------------
# Data structures
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class PeriodDelta:
    """Delta for a single value in a single period."""

    period: int
    lhs: float
    rhs: float
    delta: float


@dataclass(frozen=True)
class DeviceComparison:
    """Per-device comparison across all periods."""

    uid: str
    on_status_diffs: list[int]
    p_deltas: list[PeriodDelta]
    q_deltas: list[PeriodDelta]
    max_abs_p_delta: float
    max_abs_q_delta: float
    commitment_matches: bool
    first_divergence_period: int | None


@dataclass(frozen=True)
class BusComparison:
    """Per-bus comparison across all periods."""

    uid: str
    va_deltas: list[PeriodDelta]
    vm_deltas: list[PeriodDelta]
    max_abs_va_delta: float
    max_abs_vm_delta: float


@dataclass(frozen=True)
class BranchComparison:
    """Per-branch comparison across all periods."""

    uid: str
    branch_type: str
    on_status_diffs: list[int]
    differently_switched: bool


@dataclass(frozen=True)
class SystemPeriodTotals:
    """Per-period system-level aggregates for one solution."""

    period: int
    total_gen_p: float
    total_load_p: float
    committed_count: int


@dataclass(frozen=True)
class CostWaterfallEntry:
    """Side-by-side cost component from validator summary."""

    component: str
    lhs_value: float
    rhs_value: float
    delta: float


@dataclass(frozen=True)
class DetailedComparison:
    """Complete detailed comparison between two solutions."""

    lhs_label: str
    rhs_label: str
    periods: int
    device_comparisons: list[DeviceComparison]
    bus_comparisons: list[BusComparison]
    branch_comparisons: list[BranchComparison]
    lhs_system_totals: list[SystemPeriodTotals]
    rhs_system_totals: list[SystemPeriodTotals]
    cost_waterfall: list[CostWaterfallEntry]
    first_divergence_period: int | None
    divergence_classification: str | None
    commitment_diff_summary: dict[str, Any]


# ---------------------------------------------------------------------------
# Comparison functions
# ---------------------------------------------------------------------------


def _period_deltas(lhs_series: list[float], rhs_series: list[float]) -> list[PeriodDelta]:
    length = min(len(lhs_series), len(rhs_series))
    return [
        PeriodDelta(period=i, lhs=lhs_series[i], rhs=rhs_series[i], delta=lhs_series[i] - rhs_series[i])
        for i in range(length)
    ]


def compare_device_schedules(
    lhs_payload: dict[str, Any],
    rhs_payload: dict[str, Any],
    *,
    base_mva: float = 1.0,
) -> list[DeviceComparison]:
    """Per-device comparison of P, Q, on_status."""
    lhs_devices = extract_device_schedules(lhs_payload)
    rhs_devices = extract_device_schedules(rhs_payload)
    all_uids = sorted(set(lhs_devices) | set(rhs_devices))
    comparisons: list[DeviceComparison] = []
    for uid in all_uids:
        lhs = lhs_devices.get(uid)
        rhs = rhs_devices.get(uid)
        if lhs is None or rhs is None:
            comparisons.append(DeviceComparison(
                uid=uid, on_status_diffs=[], p_deltas=[], q_deltas=[],
                max_abs_p_delta=0.0, max_abs_q_delta=0.0,
                commitment_matches=lhs is None and rhs is None,
                first_divergence_period=None,
            ))
            continue
        on_diffs = [i for i in range(min(len(lhs.on_status), len(rhs.on_status))) if lhs.on_status[i] != rhs.on_status[i]]
        p_lhs = [v * base_mva for v in lhs.p_on]
        p_rhs = [v * base_mva for v in rhs.p_on]
        q_lhs = [v * base_mva for v in lhs.q]
        q_rhs = [v * base_mva for v in rhs.q]
        p_deltas = _period_deltas(p_lhs, p_rhs)
        q_deltas = _period_deltas(q_lhs, q_rhs)
        max_abs_p = max((abs(d.delta) for d in p_deltas), default=0.0)
        max_abs_q = max((abs(d.delta) for d in q_deltas), default=0.0)
        first_div = None
        for i in range(min(len(lhs.on_status), len(rhs.on_status))):
            if lhs.on_status[i] != rhs.on_status[i]:
                first_div = i
                break
            if i < len(p_deltas) and abs(p_deltas[i].delta) > 0.01 * base_mva:
                first_div = i
                break
        comparisons.append(DeviceComparison(
            uid=uid,
            on_status_diffs=on_diffs,
            p_deltas=p_deltas,
            q_deltas=q_deltas,
            max_abs_p_delta=max_abs_p,
            max_abs_q_delta=max_abs_q,
            commitment_matches=len(on_diffs) == 0,
            first_divergence_period=first_div,
        ))
    return comparisons


def compare_bus_schedules(
    lhs_payload: dict[str, Any],
    rhs_payload: dict[str, Any],
) -> list[BusComparison]:
    """Per-bus comparison of Va, Vm."""
    lhs_buses = extract_bus_schedules(lhs_payload)
    rhs_buses = extract_bus_schedules(rhs_payload)
    all_uids = sorted(set(lhs_buses) | set(rhs_buses))
    comparisons: list[BusComparison] = []
    for uid in all_uids:
        lhs = lhs_buses.get(uid)
        rhs = rhs_buses.get(uid)
        if lhs is None or rhs is None:
            comparisons.append(BusComparison(
                uid=uid, va_deltas=[], vm_deltas=[],
                max_abs_va_delta=0.0, max_abs_vm_delta=0.0,
            ))
            continue
        va_deltas = _period_deltas(lhs.va, rhs.va)
        vm_deltas = _period_deltas(lhs.vm, rhs.vm)
        comparisons.append(BusComparison(
            uid=uid,
            va_deltas=va_deltas,
            vm_deltas=vm_deltas,
            max_abs_va_delta=max((abs(d.delta) for d in va_deltas), default=0.0),
            max_abs_vm_delta=max((abs(d.delta) for d in vm_deltas), default=0.0),
        ))
    return comparisons


def compare_branch_schedules(
    lhs_payload: dict[str, Any],
    rhs_payload: dict[str, Any],
) -> list[BranchComparison]:
    """Per-branch comparison of on_status (switching differences)."""
    lhs_branches = extract_branch_schedules(lhs_payload)
    rhs_branches = extract_branch_schedules(rhs_payload)
    all_uids = sorted(set(lhs_branches) | set(rhs_branches))
    comparisons: list[BranchComparison] = []
    for uid in all_uids:
        lhs = lhs_branches.get(uid)
        rhs = rhs_branches.get(uid)
        if lhs is None or rhs is None:
            comparisons.append(BranchComparison(
                uid=uid,
                branch_type=(lhs or rhs).branch_type if (lhs or rhs) else "unknown",
                on_status_diffs=[],
                differently_switched=False,
            ))
            continue
        on_diffs = [i for i in range(min(len(lhs.on_status), len(rhs.on_status))) if lhs.on_status[i] != rhs.on_status[i]]
        comparisons.append(BranchComparison(
            uid=uid,
            branch_type=lhs.branch_type,
            on_status_diffs=on_diffs,
            differently_switched=len(on_diffs) > 0,
        ))
    return comparisons


def compute_system_period_totals(
    payload: dict[str, Any],
    *,
    base_mva: float = 1.0,
) -> list[SystemPeriodTotals]:
    """Compute system-level totals for each period in a solution."""
    devices = extract_device_schedules(payload)
    if not devices:
        return []
    periods = max(len(d.on_status) for d in devices.values())
    totals: list[SystemPeriodTotals] = []
    for t in range(periods):
        gen_p = 0.0
        load_p = 0.0
        committed = 0
        for dev in devices.values():
            if t >= len(dev.on_status):
                continue
            p = dev.p_on[t] * base_mva if t < len(dev.p_on) else 0.0
            if dev.on_status[t]:
                committed += 1
            if p >= 0:
                gen_p += p
            else:
                load_p += p
        totals.append(SystemPeriodTotals(
            period=t, total_gen_p=gen_p, total_load_p=load_p, committed_count=committed,
        ))
    return totals


# ---------------------------------------------------------------------------
# Cost waterfall
# ---------------------------------------------------------------------------

_WATERFALL_COMPONENTS = (
    "z", "z_base", "z_cost", "z_penalty", "z_value",
    "z_k_worst_case", "z_k_average_case",
    "sum_bus_t_z_p", "sum_bus_t_z_q",
    "sum_sd_t_z_su", "sum_sd_t_z_sd", "sum_sd_t_z_on",
)


def build_cost_waterfall(
    lhs_summary: dict[str, Any],
    rhs_summary: dict[str, Any],
    *,
    components: tuple[str, ...] = _WATERFALL_COMPONENTS,
) -> list[CostWaterfallEntry]:
    """Decompose validator summary metrics side-by-side."""
    entries: list[CostWaterfallEntry] = []
    for comp in components:
        lhs_val = _numeric(lhs_summary.get(comp))
        rhs_val = _numeric(rhs_summary.get(comp))
        entries.append(CostWaterfallEntry(
            component=comp,
            lhs_value=lhs_val,
            rhs_value=rhs_val,
            delta=lhs_val - rhs_val,
        ))
    return entries


def _numeric(value: object) -> float:
    if isinstance(value, (int, float)):
        return float(value)
    return 0.0


# ---------------------------------------------------------------------------
# Divergence analysis
# ---------------------------------------------------------------------------


def find_first_divergence_period(
    device_comparisons: list[DeviceComparison],
    *,
    p_threshold: float = 0.01,
    on_status_matters: bool = True,
) -> int | None:
    """Find the earliest period where solutions meaningfully diverge."""
    earliest: int | None = None
    for dc in device_comparisons:
        if on_status_matters and dc.on_status_diffs:
            period = dc.on_status_diffs[0]
            if earliest is None or period < earliest:
                earliest = period
        for pd in dc.p_deltas:
            if abs(pd.delta) > p_threshold:
                if earliest is None or pd.period < earliest:
                    earliest = pd.period
                break
    return earliest


def classify_divergence(
    comparison: DetailedComparison,
) -> str | None:
    """Tag the comparison with a likely root-cause classification."""
    has_switching = any(bc.differently_switched for bc in comparison.branch_comparisons)
    if has_switching:
        return "switching"

    total_devices = len(comparison.device_comparisons)
    devices_with_commitment_diffs = sum(1 for dc in comparison.device_comparisons if not dc.commitment_matches)
    if total_devices > 0 and devices_with_commitment_diffs / total_devices > 0.3:
        return "commitment"

    z_penalty_delta = 0.0
    z_cost_delta = 0.0
    for entry in comparison.cost_waterfall:
        if entry.component == "z_penalty":
            z_penalty_delta = abs(entry.delta)
        elif entry.component == "z_cost":
            z_cost_delta = abs(entry.delta)
    if z_penalty_delta > 0 and z_penalty_delta > z_cost_delta * 2:
        return "penalty_dominated"

    max_vm_delta = max((bc.max_abs_vm_delta for bc in comparison.bus_comparisons), default=0.0)
    max_p_delta = max((dc.max_abs_p_delta for dc in comparison.device_comparisons), default=0.0)
    if max_vm_delta > 0.02 and max_p_delta < 1.0:
        return "voltage"

    if max_p_delta > 1.0:
        return "dispatch"

    return None


def build_commitment_diff_summary(
    lhs_payload: dict[str, Any],
    rhs_payload: dict[str, Any],
) -> dict[str, Any]:
    """Compare commitment schedules between two solutions."""
    lhs_devices = extract_device_schedules(lhs_payload)
    rhs_devices = extract_device_schedules(rhs_payload)
    all_uids = sorted(set(lhs_devices) | set(rhs_devices))
    devices_with_diffs = 0
    startup_diffs: list[dict[str, Any]] = []
    shutdown_diffs: list[dict[str, Any]] = []
    max_periods = 0
    for uid in all_uids:
        lhs = lhs_devices.get(uid)
        rhs = rhs_devices.get(uid)
        if lhs is None or rhs is None:
            devices_with_diffs += 1
            continue
        periods = min(len(lhs.on_status), len(rhs.on_status))
        max_periods = max(max_periods, periods)
        has_diff = False
        for t in range(periods):
            if lhs.on_status[t] != rhs.on_status[t]:
                has_diff = True
                if lhs.on_status[t] and not rhs.on_status[t]:
                    startup_diffs.append({"uid": uid, "period": t, "lhs_on": True, "rhs_on": False})
                else:
                    shutdown_diffs.append({"uid": uid, "period": t, "lhs_on": False, "rhs_on": True})
        if has_diff:
            devices_with_diffs += 1

    net_delta_by_period: list[int] = []
    for t in range(max_periods):
        lhs_on = sum(1 for d in lhs_devices.values() if t < len(d.on_status) and d.on_status[t])
        rhs_on = sum(1 for d in rhs_devices.values() if t < len(d.on_status) and d.on_status[t])
        net_delta_by_period.append(lhs_on - rhs_on)

    return {
        "total_devices": len(all_uids),
        "devices_with_diffs": devices_with_diffs,
        "startup_diffs": startup_diffs,
        "shutdown_diffs": shutdown_diffs,
        "net_commitment_delta_by_period": net_delta_by_period,
    }


# ---------------------------------------------------------------------------
# Top-level entry points
# ---------------------------------------------------------------------------


def compare_solutions(
    lhs_path: Path,
    rhs_path: Path,
    *,
    lhs_label: str = "lhs",
    rhs_label: str = "rhs",
    base_mva: float = 1.0,
    lhs_validator_summary: dict[str, Any] | None = None,
    rhs_validator_summary: dict[str, Any] | None = None,
) -> DetailedComparison:
    """Top-level entry point: load two solutions and produce a detailed comparison."""
    lhs_payload = load_solution_payload(lhs_path)
    rhs_payload = load_solution_payload(rhs_path)

    device_comparisons = compare_device_schedules(lhs_payload, rhs_payload, base_mva=base_mva)
    bus_comparisons = compare_bus_schedules(lhs_payload, rhs_payload)
    branch_comparisons = compare_branch_schedules(lhs_payload, rhs_payload)
    lhs_totals = compute_system_period_totals(lhs_payload, base_mva=base_mva)
    rhs_totals = compute_system_period_totals(rhs_payload, base_mva=base_mva)
    cost_waterfall = build_cost_waterfall(
        lhs_validator_summary or {},
        rhs_validator_summary or {},
    )
    first_div = find_first_divergence_period(device_comparisons, p_threshold=0.01 * base_mva)
    commitment_diff = build_commitment_diff_summary(lhs_payload, rhs_payload)

    periods = max(len(lhs_totals), len(rhs_totals))

    comparison = DetailedComparison(
        lhs_label=lhs_label,
        rhs_label=rhs_label,
        periods=periods,
        device_comparisons=device_comparisons,
        bus_comparisons=bus_comparisons,
        branch_comparisons=branch_comparisons,
        lhs_system_totals=lhs_totals,
        rhs_system_totals=rhs_totals,
        cost_waterfall=cost_waterfall,
        first_divergence_period=first_div,
        divergence_classification=None,
        commitment_diff_summary=commitment_diff,
    )
    classification = classify_divergence(comparison)
    return DetailedComparison(
        lhs_label=comparison.lhs_label,
        rhs_label=comparison.rhs_label,
        periods=comparison.periods,
        device_comparisons=comparison.device_comparisons,
        bus_comparisons=comparison.bus_comparisons,
        branch_comparisons=comparison.branch_comparisons,
        lhs_system_totals=comparison.lhs_system_totals,
        rhs_system_totals=comparison.rhs_system_totals,
        cost_waterfall=comparison.cost_waterfall,
        first_divergence_period=comparison.first_divergence_period,
        divergence_classification=classification,
        commitment_diff_summary=comparison.commitment_diff_summary,
    )


def comparison_to_dict(comparison: DetailedComparison) -> dict[str, Any]:
    """Serialize a DetailedComparison to a JSON-compatible dict."""
    return {
        "lhs_label": comparison.lhs_label,
        "rhs_label": comparison.rhs_label,
        "periods": comparison.periods,
        "first_divergence_period": comparison.first_divergence_period,
        "divergence_classification": comparison.divergence_classification,
        "commitment_diff_summary": comparison.commitment_diff_summary,
        "cost_waterfall": [
            {"component": e.component, "lhs": e.lhs_value, "rhs": e.rhs_value, "delta": e.delta}
            for e in comparison.cost_waterfall
        ],
        "device_comparisons": [
            {
                "uid": dc.uid,
                "commitment_matches": dc.commitment_matches,
                "on_status_diffs": dc.on_status_diffs,
                "max_abs_p_delta": dc.max_abs_p_delta,
                "max_abs_q_delta": dc.max_abs_q_delta,
                "first_divergence_period": dc.first_divergence_period,
            }
            for dc in comparison.device_comparisons
        ],
        "bus_comparisons": [
            {
                "uid": bc.uid,
                "max_abs_va_delta": bc.max_abs_va_delta,
                "max_abs_vm_delta": bc.max_abs_vm_delta,
            }
            for bc in comparison.bus_comparisons
        ],
        "branch_comparisons": [
            {
                "uid": bc.uid,
                "branch_type": bc.branch_type,
                "differently_switched": bc.differently_switched,
                "on_status_diffs": bc.on_status_diffs,
            }
            for bc in comparison.branch_comparisons
        ],
    }


def summarize_comparison(comparison: DetailedComparison) -> list[str]:
    """Produce concise human-readable summary lines for CLI output."""
    lines: list[str] = []
    lines.append(f"Comparison: {comparison.lhs_label} vs {comparison.rhs_label} ({comparison.periods} periods)")

    if comparison.divergence_classification:
        lines.append(f"Classification: {comparison.divergence_classification}")
    if comparison.first_divergence_period is not None:
        lines.append(f"First divergence: period {comparison.first_divergence_period}")

    # Commitment summary
    cs = comparison.commitment_diff_summary
    if cs.get("devices_with_diffs", 0) > 0:
        lines.append(
            f"Commitment: {cs['devices_with_diffs']}/{cs['total_devices']} devices differ"
        )

    # Top device P deltas
    sorted_devices = sorted(comparison.device_comparisons, key=lambda d: d.max_abs_p_delta, reverse=True)
    top_p = [d for d in sorted_devices[:5] if d.max_abs_p_delta > 0.01]
    if top_p:
        lines.append("Top P deltas (MW):")
        for dc in top_p:
            status = "" if dc.commitment_matches else " [commitment differs]"
            lines.append(f"  {dc.uid}: max |dP|={dc.max_abs_p_delta:.2f}{status}")

    # Top bus Vm deltas
    sorted_buses = sorted(comparison.bus_comparisons, key=lambda b: b.max_abs_vm_delta, reverse=True)
    top_vm = [b for b in sorted_buses[:5] if b.max_abs_vm_delta > 0.001]
    if top_vm:
        lines.append("Top Vm deltas (pu):")
        for bc in top_vm:
            lines.append(f"  {bc.uid}: max |dVm|={bc.max_abs_vm_delta:.4f}")

    # Switching
    switched = [bc for bc in comparison.branch_comparisons if bc.differently_switched]
    if switched:
        lines.append(f"Switching: {len(switched)} branches differ")

    # Cost waterfall
    interesting_costs = [e for e in comparison.cost_waterfall if abs(e.delta) > 0.01]
    if interesting_costs:
        lines.append("Cost waterfall:")
        for entry in interesting_costs:
            lines.append(f"  {entry.component}: {entry.lhs_value:.2f} vs {entry.rhs_value:.2f} (delta={entry.delta:+.2f})")

    return lines
