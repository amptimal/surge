#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Benchmark-harness runner for GO Challenge 3.

Wraps :func:`markets.go_c3.solve` with the suite-harness machinery:
per-scenario workdir conventions (``runs/baseline/{dataset}/.../
scenario_NNN/``), artifact archiving across re-solves, validator
integration, and the scoring/dashboard artifacts (pi-model violation
report, consumer-value report, DC-vs-AC dispatch payloads).

Public API:

* :func:`solve_baseline_scenario` — solve + write dashboard artifacts.
* :func:`validate_baseline_solution` — run the official validator.
* :func:`run_pop_validation` — validate a pop solution directly.
* :func:`run_suite` — batch over a :class:`Suite` of scenarios.
* :func:`solve_sced_fixed` — SCED with commitment pinned to a
  reference schedule.
* :func:`baseline_output_dir` / :func:`baseline_validation_dir` /
  :func:`sced_fixed_output_dir` — path conventions.
"""

from __future__ import annotations

import contextlib
import copy
import dataclasses
import json
import logging
import re
import shutil
import sys
import threading
import time
from collections import defaultdict
from datetime import datetime
from pathlib import Path
from typing import Any

from markets.go_c3 import GoC3Policy, GoC3Problem, solve
from surge.market import SolveLogger

from .commitment import ReferenceSchedule, extract_reference_schedule
from .datasets import ScenarioRecord, discover_scenarios, ensure_dataset_unpacked
from .manifests import DatasetManifest, DatasetResource, Suite
from .paths import default_cache_root, default_results_root, source_python_root
from .references import ensure_reference_submission, load_scenario_leaderboard, select_reference_entries
from .validator import ensure_validator_environment, validate_with_official_tool
from .violations import compute_solution_violations

logger = logging.getLogger("go_c3.runner")




def _import_surge():
    """Import the ``surge`` extension module, injecting the source tree on sys.path."""
    python_root = source_python_root()
    if str(python_root) not in sys.path:
        sys.path.insert(0, str(python_root))
    import surge  # type: ignore

    return surge


# ---------------------------------------------------------------------------
# Path conventions
# ---------------------------------------------------------------------------


def baseline_output_dir(
    cache_root: Path,
    scenario: ScenarioRecord,
    *,
    policy: GoC3Policy | None = None,
) -> Path:
    sw = "sw1" if (policy is not None and policy.allow_branch_switching) else "sw0"
    base = (
        cache_root
        / "runs"
        / "baseline"
        / scenario.dataset_key
        / scenario.division
        / sw
    )
    if policy is not None and getattr(policy, "scuc_only", False):
        base = base / "scuc_only"
    return base / f"scenario_{scenario.scenario_id:03d}"


def baseline_validation_dir(
    cache_root: Path,
    scenario: ScenarioRecord,
    *,
    policy: GoC3Policy | None = None,
) -> Path:
    sw = "sw1" if (policy is not None and policy.allow_branch_switching) else "sw0"
    return (
        cache_root
        / "runs"
        / "validator-baseline"
        / scenario.dataset_key
        / scenario.division
        / sw
        / f"scenario_{scenario.scenario_id:03d}"
    )


def _baseline_run_report_path(
    cache_root: Path,
    scenario: ScenarioRecord,
    *,
    policy: GoC3Policy | None = None,
) -> Path:
    return baseline_output_dir(cache_root, scenario, policy=policy) / "run-report.json"


def sced_fixed_output_dir(
    cache_root: Path,
    scenario: ScenarioRecord,
    *,
    ac_reconcile_mode: str | None = None,
) -> Path:
    run_name = "sced-fixed-exact" if ac_reconcile_mode == "ac_exact_replay" else "sced-fixed"
    return (
        cache_root
        / "runs"
        / run_name
        / scenario.dataset_key
        / scenario.division
        / f"scenario_{scenario.scenario_id:03d}"
    )


# ---------------------------------------------------------------------------
# Archive rotation — keep the newest ``keep_last`` prior runs per scenario.
# ---------------------------------------------------------------------------


DEFAULT_RUN_ARCHIVE_KEEP_LAST = 10


def _archive_existing_baseline_artifacts(
    workdir: Path,
    *,
    keep_last: int = DEFAULT_RUN_ARCHIVE_KEEP_LAST,
) -> Path | None:
    """Move prior run artifacts into ``workdir / "archive" / {iso_timestamp}``."""
    if not workdir.exists():
        return None
    entries = [p for p in workdir.iterdir() if p.name != "archive"]
    if not entries:
        return None

    report_path = workdir / "run-report.json"
    src_mtime = (report_path if report_path.exists() else workdir).stat().st_mtime
    base_ts = datetime.fromtimestamp(src_mtime).strftime("%Y-%m-%dT%H-%M-%S")

    archive_root = workdir / "archive"
    archive_root.mkdir(exist_ok=True)
    target = archive_root / base_ts
    suffix = 1
    while target.exists():
        target = archive_root / f"{base_ts}_{suffix}"
        suffix += 1
    target.mkdir()

    for entry in entries:
        shutil.move(str(entry), str(target / entry.name))

    prune_run_archives(workdir, keep_last=keep_last)
    return target


def prune_run_archives(workdir: Path, *, keep_last: int = DEFAULT_RUN_ARCHIVE_KEEP_LAST) -> list[Path]:
    if keep_last <= 0:
        return []
    archive_root = workdir / "archive"
    if not archive_root.exists():
        return []
    archives = sorted(p for p in archive_root.iterdir() if p.is_dir())
    if len(archives) <= keep_last:
        return []
    removed: list[Path] = []
    for stale in archives[:-keep_last]:
        shutil.rmtree(stale)
        removed.append(stale)
    return removed


# ---------------------------------------------------------------------------
# Objective breakdown — regroups the Rust dispatch summary's objective_terms
# into the buckets the dashboard renders.
# ---------------------------------------------------------------------------


_OBJECTIVE_KIND_GROUPS: dict[str, tuple[str, str]] = {
    "generator_energy": ("generators", "energy"),
    "generator_no_load": ("generators", "no_load"),
    "generator_startup": ("generators", "startup"),
    "generator_shutdown": ("generators", "shutdown"),
    "generator_target_tracking": ("generators", "target_tracking"),
    "dispatchable_load_target_tracking": ("loads", "target_tracking"),
    "combined_cycle_dispatch": ("generators", "combined_cycle_dispatch"),
    "combined_cycle_no_load": ("generators", "combined_cycle_no_load"),
    "combined_cycle_transition": ("generators", "combined_cycle_transition"),
    "power_balance_penalty": ("penalties", "power_balance"),
    "reactive_balance_penalty": ("penalties", "reactive_balance"),
    "voltage_penalty": ("penalties", "voltage"),
    "angle_difference_penalty": ("penalties", "angle_difference"),
    "thermal_limit_penalty": ("penalties", "thermal_limit"),
    "flowgate_penalty": ("penalties", "flowgate"),
    "interface_penalty": ("penalties", "interface"),
    "ramp_penalty": ("penalties", "ramp"),
    "reserve_shortfall": ("penalties", "reserve_shortfall"),
    "reactive_reserve_shortfall": ("penalties", "reactive_reserve_shortfall"),
    "commitment_capacity_penalty": ("penalties", "commitment_capacity"),
    "energy_window_penalty": ("penalties", "energy_window"),
    "reserve_procurement": ("reserves", "procurement"),
    "reactive_reserve_procurement": ("reserves", "reactive_procurement"),
    "storage_energy": ("other", "storage_energy"),
    "storage_offer_epigraph": ("other", "storage_offer_epigraph"),
    "hvdc_energy": ("other", "hvdc_energy"),
    "virtual_bid": ("other", "virtual_bid"),
    "carbon_adder": ("other", "carbon_adder"),
    "branch_switching_startup": ("other", "branch_switching_startup"),
    "branch_switching_shutdown": ("other", "branch_switching_shutdown"),
    "explicit_contingency_worst_case": ("other", "explicit_contingency_worst_case"),
    "explicit_contingency_average_case": ("other", "explicit_contingency_average_case"),
    "benders_eta": ("other", "benders_eta"),
    "other": ("other", "other"),
}


def _dispatchable_load_objective_group(term: dict[str, Any]) -> tuple[str, str]:
    dollars = float(term.get("dollars", 0.0) or 0.0)
    if dollars < -1e-9:
        return ("loads", "served_value")
    if dollars > 1e-9:
        return ("loads", "curtailment_cost")
    return ("loads", "energy")


def _group_dispatch_objective(dispatch_summary: dict[str, Any]) -> dict[str, Any]:
    objective_terms = dispatch_summary.get("objective_terms")
    if not isinstance(objective_terms, list):
        return {}

    by_bucket: dict[str, float] = defaultdict(float)
    by_kind: dict[str, float] = defaultdict(float)
    grouped: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    uncategorized: dict[str, float] = defaultdict(float)

    for term in objective_terms:
        if not isinstance(term, dict):
            continue
        raw_dollars = term.get("dollars")
        if not isinstance(raw_dollars, (int, float)):
            continue
        dollars = float(raw_dollars)
        bucket = str(term.get("bucket", "unknown"))
        kind = str(term.get("kind", "unknown"))
        by_bucket[bucket] += dollars
        by_kind[kind] += dollars
        if kind == "dispatchable_load_energy":
            group_target = _dispatchable_load_objective_group(term)
        else:
            group_target = _OBJECTIVE_KIND_GROUPS.get(kind)
        if group_target is None:
            uncategorized[kind] += dollars
            continue
        group_name, subgroup_name = group_target
        grouped[group_name][subgroup_name] += dollars

    objective_terms_total = float(sum(by_kind.values()))
    return {
        "objective_term_count": len(objective_terms),
        "objective_terms_total_dollars": objective_terms_total,
        "reported_total_cost": float(dispatch_summary.get("total_cost", 0.0) or 0.0),
        "by_bucket": dict(sorted(by_bucket.items())),
        "by_kind": dict(sorted(by_kind.items())),
        "group_totals": {
            group_name: dict(sorted(group_values.items()))
            for group_name, group_values in sorted(grouped.items())
            if group_values
        },
        "uncategorized": dict(sorted(uncategorized.items())),
    }


# ---------------------------------------------------------------------------
# Consumer-served value reporting (for the dashboard "Loads" tab).
# ---------------------------------------------------------------------------


def _normalized_series(values: Any, length: int) -> list[float]:
    series = [0.0] * length
    if not isinstance(values, list):
        return series
    for idx, value in enumerate(values[:length]):
        try:
            series[idx] = float(value or 0.0)
        except (TypeError, ValueError):
            series[idx] = 0.0
    return series


def _consumer_value_blocks_mw(
    problem: "GoC3Problem",
    device_ts: dict[str, Any],
    period_idx: int,
) -> list[tuple[float, float]]:
    raw_cost = device_ts.get("cost", [])
    if not isinstance(raw_cost, list) or period_idx >= len(raw_cost):
        return []
    period_blocks = raw_cost[period_idx]
    if not isinstance(period_blocks, list):
        return []
    blocks: list[tuple[float, float]] = []
    for block in period_blocks:
        if not isinstance(block, (list, tuple)) or len(block) != 2:
            continue
        try:
            value_per_mwh = float(block[0]) / problem.base_norm_mva
            block_size_pu = float(block[1])
        except (TypeError, ValueError, ZeroDivisionError):
            continue
        if block_size_pu <= 1e-12:
            continue
        blocks.append((value_per_mwh, block_size_pu * problem.base_norm_mva))
    blocks.sort(key=lambda item: item[0], reverse=True)
    return blocks


def _consumer_period_value_dollars(
    blocks_mw: list[tuple[float, float]],
    served_mw: float,
    duration_hours: float,
) -> tuple[float, float]:
    remaining_mw = max(float(served_mw), 0.0)
    value_dollars = 0.0
    for value_per_mwh, block_mw in blocks_mw:
        if remaining_mw <= 1e-12:
            break
        cleared_mw = min(remaining_mw, block_mw)
        value_dollars += value_per_mwh * cleared_mw
        remaining_mw -= cleared_mw
    return value_dollars * duration_hours, remaining_mw * duration_hours


def _consumer_served_pu_from_solution(
    problem: "GoC3Problem",
    solution_payload: dict[str, Any],
) -> dict[str, list[float]]:
    output = solution_payload.get("time_series_output")
    if not isinstance(output, dict):
        return {}
    rows = output.get("simple_dispatchable_device")
    if not isinstance(rows, list):
        return {}
    by_uid = {
        str(row.get("uid")): _normalized_series(row.get("p_on"), problem.periods)
        for row in rows
        if isinstance(row, dict) and row.get("uid") is not None
    }
    return {
        str(device.get("uid")): by_uid.get(str(device.get("uid")), [0.0] * problem.periods)
        for device in problem.devices
        if device.get("device_type") == "consumer"
    }


def _period_result_detail(resource_result: dict[str, Any]) -> dict[str, Any]:
    detail = resource_result.get("detail")
    return detail if isinstance(detail, dict) else {}


def _consumer_served_pu_from_dispatch_result(
    problem: "GoC3Problem",
    context: Any,
    dispatch_result_payload: dict[str, Any],
) -> dict[str, list[float]]:
    periods = dispatch_result_payload.get("periods")
    if not isinstance(periods, list):
        return {}
    served_pu_by_uid = {
        str(device.get("uid")): [0.0] * problem.periods
        for device in problem.devices
        if device.get("device_type") == "consumer"
    }
    period_lookup: list[dict[str, dict[str, Any]]] = []
    for period in periods[:problem.periods]:
        if not isinstance(period, dict):
            period_lookup.append({})
            continue
        lookup = {
            str(resource_result.get("resource_id")): resource_result
            for resource_result in period.get("resource_results", [])
            if isinstance(resource_result, dict) and resource_result.get("resource_id") is not None
        }
        period_lookup.append(lookup)
    for uid in list(served_pu_by_uid):
        fixed_floor = _normalized_series(
            getattr(context, "device_fixed_p_series_pu", {}).get(uid),
            problem.periods,
        )
        served_pu_by_uid[uid] = fixed_floor
        for resource_id in getattr(context, "consumer_dispatchable_resource_ids_by_uid", {}).get(uid, []):
            for period_idx in range(min(problem.periods, len(period_lookup))):
                resource_result = period_lookup[period_idx].get(resource_id, {})
                detail = _period_result_detail(resource_result)
                served_mw = detail.get("served_p_mw")
                if served_mw is None:
                    try:
                        served_mw = max(-float(resource_result.get("power_mw", 0.0) or 0.0), 0.0)
                    except (TypeError, ValueError):
                        served_mw = 0.0
                served_pu_by_uid[uid][period_idx] += float(served_mw) / problem.base_norm_mva
    return served_pu_by_uid


def _build_consumer_value_report(
    problem: "GoC3Problem",
    served_pu_by_uid: dict[str, list[float]],
    *,
    source: str,
) -> dict[str, Any]:
    periods = problem.periods
    durations = problem.interval_durations
    consumers: list[dict[str, Any]] = []
    total_served_mwh = 0.0
    total_value_dollars = 0.0
    total_unvalued_served_mwh = 0.0

    for device in problem.devices:
        if device.get("device_type") != "consumer":
            continue
        uid = str(device.get("uid"))
        device_ts = problem.device_time_series_by_uid.get(uid, {})
        served_series = _normalized_series(served_pu_by_uid.get(uid), periods)
        consumer_served_mwh = 0.0
        consumer_value_dollars = 0.0
        consumer_unvalued_served_mwh = 0.0
        period_rows: list[dict[str, Any]] = []
        for period_idx in range(periods):
            duration_hours = durations[period_idx] if period_idx < len(durations) else 0.0
            served_pu = max(served_series[period_idx], 0.0)
            served_mw = served_pu * problem.base_norm_mva
            served_mwh = served_mw * duration_hours
            value_dollars, unvalued_served_mwh = _consumer_period_value_dollars(
                _consumer_value_blocks_mw(problem, device_ts, period_idx),
                served_mw,
                duration_hours,
            )
            consumer_served_mwh += served_mwh
            consumer_value_dollars += value_dollars
            consumer_unvalued_served_mwh += unvalued_served_mwh
            period_rows.append(
                {
                    "period": period_idx,
                    "duration_hours": duration_hours,
                    "served_pu": served_pu,
                    "served_mw": served_mw,
                    "served_mwh": served_mwh,
                    "value_dollars": value_dollars,
                    "unvalued_served_mwh": unvalued_served_mwh,
                }
            )
        total_served_mwh += consumer_served_mwh
        total_value_dollars += consumer_value_dollars
        total_unvalued_served_mwh += consumer_unvalued_served_mwh
        consumers.append(
            {
                "uid": uid,
                "served_mwh": consumer_served_mwh,
                "value_dollars": consumer_value_dollars,
                "unvalued_served_mwh": consumer_unvalued_served_mwh,
                "periods": period_rows,
            }
        )

    consumers.sort(key=lambda row: row["uid"])
    return {
        "source": source,
        "consumer_count": len(consumers),
        "total_served_mwh": total_served_mwh,
        "total_value_dollars": total_value_dollars,
        "total_unvalued_served_mwh": total_unvalued_served_mwh,
        "consumers": consumers,
    }


def _consumer_value_summary(report: dict[str, Any] | None) -> dict[str, Any] | None:
    if not isinstance(report, dict):
        return None
    return {
        "source": report.get("source"),
        "consumer_count": int(report.get("consumer_count", 0) or 0),
        "total_served_mwh": float(report.get("total_served_mwh", 0.0) or 0.0),
        "total_value_dollars": float(report.get("total_value_dollars", 0.0) or 0.0),
        "total_unvalued_served_mwh": float(report.get("total_unvalued_served_mwh", 0.0) or 0.0),
    }


def _write_json(path: Path, payload: dict[str, Any]) -> None:
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


# ---------------------------------------------------------------------------
# Validator entry points
# ---------------------------------------------------------------------------


def run_pop_validation(
    scenario: ScenarioRecord,
    *,
    cache_root: Path | None = None,
    validator_env=None,
    parameters: dict[str, object] | None = None,
) -> dict[str, Any]:
    cache_root = cache_root or default_cache_root()
    if scenario.pop_solution_path is None:
        raise FileNotFoundError(f"{scenario.problem_path} has no public pop solution")
    env = validator_env or ensure_validator_environment(cache_root=cache_root)
    workdir = (
        cache_root
        / "runs"
        / "validator-pop"
        / scenario.dataset_key
        / scenario.division
        / f"scenario_{scenario.scenario_id:03d}"
    )
    result = validate_with_official_tool(
        env,
        scenario.problem_path,
        solution_path=scenario.pop_solution_path,
        workdir=workdir,
        parameters=parameters,
    )
    report = {
        "dataset_key": scenario.dataset_key,
        "division": scenario.division,
        "network_model": scenario.network_model,
        "scenario_id": scenario.scenario_id,
        "problem_path": str(scenario.problem_path),
        "pop_solution_path": str(scenario.pop_solution_path),
        "validation": result,
        "validation_summary": result.get("summary_metrics", {}),
        "run_report_path": str(workdir / "run-report.json"),
    }
    _write_json(workdir / "run-report.json", report)
    return report


def validate_baseline_solution(
    scenario: ScenarioRecord,
    *,
    cache_root: Path | None = None,
    validator_env=None,
    solution_path: Path | None = None,
    parameters: dict[str, object] | None = None,
    policy: GoC3Policy | None = None,
) -> dict[str, Any]:
    cache_root = cache_root or default_cache_root()
    env = validator_env or ensure_validator_environment(cache_root=cache_root)
    workdir = baseline_validation_dir(cache_root, scenario, policy=policy)
    resolved_solution_path = solution_path or _resolve_baseline_solution_path(
        cache_root, scenario, policy=policy
    )
    result = validate_with_official_tool(
        env,
        scenario.problem_path,
        solution_path=resolved_solution_path,
        workdir=workdir,
        parameters=parameters,
    )
    report = {
        "dataset_key": scenario.dataset_key,
        "division": scenario.division,
        "network_model": scenario.network_model,
        "scenario_id": scenario.scenario_id,
        "problem_path": str(scenario.problem_path),
        "solution_path": str(resolved_solution_path),
        "validation": result,
        "validation_summary": result.get("summary_metrics", {}),
        "run_report_path": str(workdir / "run-report.json"),
    }
    _write_json(workdir / "run-report.json", report)
    return report


# ---------------------------------------------------------------------------
# Baseline solve + dashboard artifacts
# ---------------------------------------------------------------------------


def solve_baseline_scenario(
    scenario: ScenarioRecord,
    *,
    cache_root: Path | None = None,
    policy: GoC3Policy | None = None,
) -> dict[str, Any]:
    """Solve a scenario and write the full set of dashboard artifacts.

    Thin wrapper over :func:`markets.go_c3.solve` that handles the
    suite conventions: resolves the per-scenario workdir, archives
    prior artifacts, then adds the downstream reports the dashboard
    and the validator consume.
    """
    cache_root = cache_root or default_cache_root()
    policy = policy or GoC3Policy()
    workdir = baseline_output_dir(cache_root, scenario, policy=policy)
    workdir.mkdir(parents=True, exist_ok=True)
    _archive_existing_baseline_artifacts(workdir)

    # Ensure the surge dylib is importable — the market solve loads it
    # lazily, but keeping this explicit here also warms it for the
    # downstream consumer-value reports.
    _import_surge()

    label = (
        f"{scenario.dataset_key} {scenario.division} "
        f"#{scenario.scenario_id}"
    )
    problem_handle = GoC3Problem.load(scenario.problem_path)

    # First pass: standard solve with no reactive-support pin. If it
    # fails and the policy authorizes a pin-factor retry, try again
    # with the pin applied — a known mitigation for NLP degeneracy on
    # tap/phase/reactive-heavy AC SCED cases.
    pin_factor = policy.reactive_support_pin_factor
    first_policy = policy.with_pin_factor(0.0) if pin_factor > 0 else policy
    market_report = solve(
        problem_handle, workdir, policy=first_policy, label=label,
    )
    used_reactive_pin = False
    if market_report.get("status") != "ok" and pin_factor > 0:
        logger.info(
            "solve_baseline_scenario: retrying %s with reactive_support_pin_factor=%.3f",
            label,
            pin_factor,
        )
        market_report = solve(
            problem_handle, workdir, policy=policy, label=label,
        )
        used_reactive_pin = market_report.get("status") == "ok"

    # --- Derive the dashboard artifacts from the workflow output ------
    workflow_result_path = workdir / "workflow-result.json"
    exported_path = workdir / "solution.json"
    solution_dict: dict[str, Any] | None = None
    if exported_path.exists():
        solution_dict = json.loads(exported_path.read_text(encoding="utf-8"))
    workflow_result: dict[str, Any] | None = None
    if workflow_result_path.exists():
        workflow_result = json.loads(workflow_result_path.read_text(encoding="utf-8"))

    dispatch_result_payload: dict[str, Any] | None = None
    dc_dispatch_result_payload: dict[str, Any] | None = None
    if workflow_result is not None:
        stages = workflow_result.get("stages", [])
        if stages:
            dispatch_result_payload = stages[-1].get("solution")
            if len(stages) > 1:
                dc_dispatch_result_payload = stages[0].get("solution")
        if dispatch_result_payload is not None:
            (workdir / "dispatch-result.json").write_text(
                json.dumps(dispatch_result_payload) + "\n", encoding="utf-8"
            )
        if dc_dispatch_result_payload is not None:
            (workdir / "dc-dispatch-result.json").write_text(
                json.dumps(dc_dispatch_result_payload) + "\n", encoding="utf-8"
            )

    dispatch_summary = (
        dict(dispatch_result_payload.get("summary", {}))
        if isinstance(dispatch_result_payload, dict)
        else {}
    )
    dc_dispatch_summary = (
        dict(dc_dispatch_result_payload.get("summary", {}))
        if isinstance(dc_dispatch_result_payload, dict)
        else {}
    )

    # Pi-model violation report — scoring replica.
    violation_report: dict[str, Any] | None = None
    if solution_dict is not None:
        try:
            raw_problem = json.loads(scenario.problem_path.read_text(encoding="utf-8"))
            violation_report = compute_solution_violations(
                raw_problem,
                solution_dict,
                dc_dispatch_result=dc_dispatch_result_payload,
            )
            _write_json(workdir / "violation-report.json", violation_report)
        except Exception as exc:  # pragma: no cover
            violation_report = {"error": str(exc)}

    # Consumer served-value reports (Loads tab in the dashboard).
    problem = GoC3Problem.load(scenario.problem_path)
    dispatch_load_value_report = (
        _build_consumer_value_report(
            problem,
            _consumer_served_pu_from_solution(problem, solution_dict),
            source="exported_solution",
        )
        if isinstance(solution_dict, dict)
        else None
    )
    if dispatch_load_value_report is not None:
        _write_json(
            workdir / "dispatch-load-value-report.json",
            dispatch_load_value_report,
        )
    dc_dispatch_load_value_report = (
        _build_consumer_value_report(
            problem,
            _consumer_served_pu_from_dispatch_result(
                problem, None, dc_dispatch_result_payload
            ),
            source="dispatch_result",
        )
        if isinstance(dc_dispatch_result_payload, dict)
        else None
    )
    if dc_dispatch_load_value_report is not None:
        _write_json(
            workdir / "dc-dispatch-load-value-report.json",
            dc_dispatch_load_value_report,
        )

    market_extras = market_report.get("extras") or {}
    market_artifacts = market_report.get("artifacts") or {}
    report = {
        "dataset_key": scenario.dataset_key,
        "division": scenario.division,
        "network_model": scenario.network_model,
        "scenario_id": scenario.scenario_id,
        "problem_path": str(scenario.problem_path),
        "policy": dataclasses.asdict(policy),
        "status": market_report["status"],
        "error": market_report.get("error"),
        "solve_seconds": market_report["elapsed_secs"],
        "step_timings_secs": market_extras.get("step_timings_secs") or {},
        "used_reactive_pin": used_reactive_pin,
        "scuc_mip_stats": market_extras.get("scuc_mip_stats"),
        "workflow_stages": market_extras.get("workflow_stages"),
        "dispatch_summary": dispatch_summary,
        "objective_breakdown": _group_dispatch_objective(dispatch_summary),
        "dispatch_penalty_summary": (
            dispatch_result_payload.get("penalty_summary")
            if isinstance(dispatch_result_payload, dict)
            else None
        ),
        "dispatch_load_value_summary": _consumer_value_summary(
            dispatch_load_value_report
        ),
        "dc_dispatch_summary": dc_dispatch_summary,
        "dc_objective_breakdown": _group_dispatch_objective(dc_dispatch_summary),
        "dc_dispatch_penalty_summary": (
            dc_dispatch_result_payload.get("penalty_summary")
            if isinstance(dc_dispatch_result_payload, dict)
            else None
        ),
        "dc_dispatch_load_value_summary": _consumer_value_summary(
            dc_dispatch_load_value_report
        ),
        "dispatch_load_value_report_path": (
            str(workdir / "dispatch-load-value-report.json")
            if dispatch_load_value_report is not None
            else None
        ),
        "dc_dispatch_load_value_report_path": (
            str(workdir / "dc-dispatch-load-value-report.json")
            if dc_dispatch_load_value_report is not None
            else None
        ),
        "solution_path": market_artifacts.get("solution"),
        "dc_dispatch_result_path": (
            str(workdir / "dc-dispatch-result.json")
            if dc_dispatch_result_payload is not None
            else None
        ),
        "violation_summary": (
            violation_report.get("summary")
            if isinstance(violation_report, dict)
            else None
        ),
        "run_report_path": str(workdir / "run-report.json"),
        "solve_log_path": str(workdir / "solve.log"),
    }
    _write_json(workdir / "run-report.json", report)
    return report


def _scenario_matches(
    scenario: ScenarioRecord,
    divisions: tuple[str, ...],
    scenario_ids: tuple[int, ...],
    *,
    network_model_prefix: str | None = None,
) -> bool:
    if divisions and scenario.division not in divisions:
        return False
    if scenario_ids and scenario.scenario_id not in scenario_ids:
        return False
    if network_model_prefix and not scenario.network_model.startswith(network_model_prefix):
        return False
    return True


def run_suite(
    suite: Suite,
    dataset_manifest: DatasetManifest,
    *,
    cache_root: Path | None = None,
    policy: GoC3Policy | None = None,
    with_validator: bool = False,
    network_model_prefix: str | None = None,
) -> list[dict[str, Any]]:
    cache_root = cache_root or default_cache_root()
    dataset_by_key = dataset_manifest.by_key()
    policy = policy or GoC3Policy()
    validator_env = ensure_validator_environment(cache_root=cache_root) if with_validator else None
    reports: list[dict[str, Any]] = []
    for target in suite.targets:
        resource: DatasetResource = dataset_by_key[target.dataset]
        unpacked = ensure_dataset_unpacked(resource, cache_root=cache_root)
        scenarios = discover_scenarios(unpacked.unpacked_root, resource.key)
        selected = [
            scenario
            for scenario in scenarios
            if _scenario_matches(
                scenario,
                target.divisions,
                target.scenario_ids,
                network_model_prefix=network_model_prefix,
            )
        ]
        for scenario in selected:
            scenario_report: dict[str, Any] = {
                "dataset_key": scenario.dataset_key,
                "division": scenario.division,
                "network_model": scenario.network_model,
                "scenario_id": scenario.scenario_id,
            }
            if target.validate_pop and validator_env is not None and scenario.pop_solution_path is not None:
                scenario_report["pop_validation"] = run_pop_validation(
                    scenario,
                    cache_root=cache_root,
                    validator_env=validator_env,
                )
            if target.solve_baseline:
                baseline_report = solve_baseline_scenario(
                    scenario,
                    cache_root=cache_root,
                    policy=policy,
                )
                scenario_report["baseline"] = baseline_report
                if (
                    with_validator
                    and validator_env is not None
                    and baseline_report.get("status") == "ok"
                    and baseline_report.get("solution_path") is not None
                ):
                    scenario_report["baseline_validation"] = validate_baseline_solution(
                        scenario,
                        cache_root=cache_root,
                        validator_env=validator_env,
                        solution_path=Path(str(baseline_report["solution_path"])),
                        policy=policy,
                    )
            reports.append(scenario_report)
    return reports


def solve_sced_fixed(
    scenario: ScenarioRecord,
    *,
    cache_root: Path | None = None,
    policy: GoC3Policy | None = None,
    commitment_source: str = "reference_winner",
    commitment_schedule: ReferenceSchedule | None = None,
    results_root: Path | None = None,  # noqa: ARG001 — back-compat
    switching_mode: str | None = None,  # noqa: ARG001 — back-compat
) -> dict[str, Any]:
    """Solve SCED with commitment fixed to a reference schedule.

    Builds the canonical two-stage workflow, then overrides stage 1's
    commitment with ``commitment_schedule`` so the DC LP honours the
    reference commitment instead of optimising commitment binaries.
    Used to isolate SCED correctness from SCUC quality.
    """
    import surge.market.go_c3 as go_c3_native

    if commitment_schedule is None:
        raise ValueError(
            "solve_sced_fixed: commitment_schedule must be provided. Load "
            "one via benchmarks.go_c3.commitment.extract_reference_schedule."
        )

    cache_root = cache_root or default_cache_root()
    policy = policy or GoC3Policy()
    surge = _import_surge()

    workdir = sced_fixed_output_dir(
        cache_root, scenario, ac_reconcile_mode=policy.ac_reconcile_mode
    )
    workdir.mkdir(parents=True, exist_ok=True)
    _archive_existing_baseline_artifacts(workdir)

    log_level = policy.log_level or "info"
    capture_solver_log = bool(policy.capture_solver_log)
    label = (
        f"sced-fixed {scenario.dataset_key} {scenario.division} #{scenario.scenario_id}"
    )
    with SolveLogger(
        workdir,
        logger_name="go_c3",
        policy=policy,
        label=label,
        problem_path=scenario.problem_path,
        surge_module=surge,
        log_level=log_level,
        capture_solver_log=capture_solver_log,
    ):
        native_policy = go_c3_native.MarketPolicy(
            formulation=policy.formulation,
            ac_reconcile_mode=policy.ac_reconcile_mode,
            consumer_mode=policy.consumer_mode,
            commitment_mode="fixed_initial",
            allow_branch_switching=False,
            lp_solver=policy.lp_solver,
            nlp_solver=policy.nlp_solver or "ipopt",
            commitment_mip_rel_gap=policy.commitment_mip_rel_gap,
            commitment_time_limit_secs=policy.commitment_time_limit_secs,
        )

        started_at = time.perf_counter()
        workflow_result: dict[str, Any] | None = None
        exported: dict[str, Any] | None = None
        status = "error"
        error_message: str | None = None
        try:
            logger.info("solve_sced_fixed: building canonical workflow")
            problem_handle = go_c3_native.load(scenario.problem_path)
            workflow = go_c3_native.build_workflow(problem_handle, native_policy)

            problem_obj = GoC3Problem.load(scenario.problem_path)
            producer_uids = {
                str(d.get("uid"))
                for d in problem_obj.devices
                if d.get("device_type") == "producer"
            }
            commitment_dict = {
                uid: list(device.on_status)
                for uid, device in commitment_schedule.devices.items()
                if device.on_status and uid in producer_uids
            }
            logger.info(
                "solve_sced_fixed: pinning stage-1 commitment to reference "
                "(%d producer resources, label=%s)",
                len(commitment_dict),
                commitment_schedule.source_label,
            )
            workflow.set_stage_commitment(0, commitment_dict)

            logger.info("solve_sced_fixed: solving workflow")
            workflow_result = go_c3_native.solve_workflow(
                workflow,
                lp_solver=native_policy.lp_solver,
                nlp_solver=native_policy.nlp_solver,
            )

            stage_idx = -1 if policy.ac_reconcile_mode == "ac_dispatch" else 0
            final_solution = workflow_result["stages"][stage_idx]["solution"]
            dc_reserve_source = None
            if (
                policy.ac_reconcile_mode == "ac_dispatch"
                and len(workflow_result["stages"]) > 1
            ):
                dc_reserve_source = workflow_result["stages"][0]["solution"]
            exported = go_c3_native.export(
                problem_handle,
                final_solution,
                dc_reserve_source=dc_reserve_source,
            )
            status = "ok"
            error_message = None
        except Exception as exc:  # noqa: BLE001
            logger.exception("solve_sced_fixed: failed: %s", exc)
            status = "error"
            error_message = str(exc)
        elapsed = time.perf_counter() - started_at

    exported_path: Path | None = None
    if exported is not None:
        exported_path = workdir / "solution.json"
        exported_path.write_text(json.dumps(exported) + "\n", encoding="utf-8")
    if workflow_result is not None:
        (workdir / "workflow-result.json").write_text(
            json.dumps(workflow_result) + "\n", encoding="utf-8"
        )

    dispatch_summary: dict[str, Any] = {}
    if workflow_result is not None and workflow_result.get("stages"):
        final = workflow_result["stages"][-1].get("solution") or {}
        dispatch_summary = dict(final.get("summary", {}) or {})

    report = {
        "dataset_key": scenario.dataset_key,
        "division": scenario.division,
        "network_model": scenario.network_model,
        "scenario_id": scenario.scenario_id,
        "problem_path": str(scenario.problem_path),
        "policy": dataclasses.asdict(policy),
        "commitment_source": commitment_source,
        "commitment_label": commitment_schedule.source_label,
        "status": status,
        "error": error_message,
        "solve_seconds": elapsed,
        "dispatch_summary": dispatch_summary,
        "solution_path": str(exported_path) if exported_path is not None else None,
        "run_report_path": str(workdir / "run-report.json"),
        "solve_log_path": str(workdir / "solve.log"),
    }
    _write_json(workdir / "run-report.json", report)
    return report


def _resolve_baseline_solution_path(
    cache_root: Path,
    scenario: ScenarioRecord,
    *,
    policy: GoC3Policy | None = None,
) -> Path:
    """Locate the most recent baseline-solve solution.json for a scenario."""
    run_report_path = _baseline_run_report_path(cache_root, scenario, policy=policy)
    if run_report_path.exists():
        report = json.loads(run_report_path.read_text(encoding="utf-8"))
        if report.get("status") != "ok":
            error = report.get("error")
            detail = f": {error}" if error else ""
            raise FileNotFoundError(
                f"latest baseline solve for {scenario.dataset_key} "
                f"{scenario.division} scenario_{scenario.scenario_id:03d} did "
                f"not succeed{detail}"
            )
        solution_path = report.get("solution_path")
        if isinstance(solution_path, str) and solution_path:
            resolved = Path(solution_path)
            if resolved.exists():
                return resolved
    fallback = baseline_output_dir(cache_root, scenario, policy=policy) / "solution.json"
    if not fallback.exists():
        raise FileNotFoundError(f"baseline solution not found at {fallback}")
    return fallback


__all__ = [
    "DEFAULT_RUN_ARCHIVE_KEEP_LAST",
    "baseline_output_dir",
    "baseline_validation_dir",
    "prune_run_archives",
    "run_pop_validation",
    "run_suite",
    "sced_fixed_output_dir",
    "solve_baseline_scenario",
    "solve_sced_fixed",
    "validate_baseline_solution",
]
