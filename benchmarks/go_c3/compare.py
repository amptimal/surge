#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Scenario and suite comparison helpers for GO Challenge 3."""

from __future__ import annotations

from pathlib import Path
from collections import defaultdict
from typing import Any

from .datasets import ScenarioRecord
from .leaderboard import benchmark_entry, track_key
from .references import (
    ensure_reference_submission,
    load_scenario_leaderboard,
    select_reference_entries,
)
from .runner import run_pop_validation, solve_baseline_scenario, validate_baseline_solution
from .validator import ensure_validator_environment, validate_with_official_tool


def comparison_output_dir(cache_root: Path, scenario: ScenarioRecord) -> Path:
    return (
        cache_root
        / "runs"
        / "comparisons"
        / scenario.dataset_key
        / scenario.division
        / f"scenario_{scenario.scenario_id:03d}"
    )


def _write_json(path: Path, payload: dict[str, Any]) -> None:
    path.write_text(__import__("json").dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _numeric_delta(lhs: object, rhs: object) -> float | None:
    if isinstance(lhs, (int, float)) and isinstance(rhs, (int, float)):
        return float(lhs) - float(rhs)
    return None


def _percent_gap(reference: object, baseline: object) -> float | None:
    if not isinstance(reference, (int, float)) or not isinstance(baseline, (int, float)):
        return None
    denom = abs(float(reference))
    if denom <= 0.0:
        return None
    return (float(reference) - float(baseline)) / denom * 100.0


def _reference_validation_report_path(workdir: Path) -> Path:
    return workdir / "run-report.json"


def _load_cached_reference_validation(
    workdir: Path,
    *,
    problem_path: Path,
    solution_path: Path,
) -> dict[str, object] | None:
    report_path = _reference_validation_report_path(workdir)
    if not report_path.exists():
        return None
    report = __import__("json").loads(report_path.read_text(encoding="utf-8"))
    if not isinstance(report, dict):
        return None
    if report.get("problem_path") != str(problem_path):
        return None
    if report.get("solution_path") != str(solution_path):
        return None
    validation = report.get("validation")
    return validation if isinstance(validation, dict) else None


def compare_scenario(
    scenario: ScenarioRecord,
    *,
    cache_root: Path,
    validator_env=None,
    results_root: Path | None = None,
    workbook_path: Path | None = None,
    include_benchmark: bool = True,
    top_k: int = 1,
    validate_pop: bool = True,
    validate_baseline: bool = True,
    solve_baseline_if_missing: bool = False,
    baseline_policy=None,
    switching_mode: str | None = None,
) -> dict[str, Any]:
    validator_env = validator_env or ensure_validator_environment(cache_root=cache_root)
    resolved_workbook, leaderboard_rows = load_scenario_leaderboard(
        scenario.dataset_key,
        scenario.division,
        scenario.network_model,
        scenario.scenario_id,
        results_root=results_root,
        workbook_path=workbook_path,
        switching_mode=switching_mode,
    )
    winner = leaderboard_rows[0] if leaderboard_rows else None
    benchmark = benchmark_entry(leaderboard_rows)
    scenario_switching_mode = switching_mode or next(
        (entry.switching_mode for entry in leaderboard_rows if entry.switching_mode is not None),
        None,
    )
    scenario_track = {
        "switching_mode": scenario_switching_mode,
        "track_key": track_key(scenario.network_model, scenario_switching_mode),
    }
    selected_reference_rows = select_reference_entries(
        leaderboard_rows,
        include_benchmark=include_benchmark,
        top_k=top_k,
    )

    workdir = comparison_output_dir(cache_root, scenario)
    workdir.mkdir(parents=True, exist_ok=True)

    references: list[dict[str, Any]] = []
    for entry in selected_reference_rows:
        reference_report: dict[str, Any] = {
            "entry": entry.__dict__,
            "switching_mode": entry.switching_mode,
            "is_benchmark": entry.team == "ARPA-e Benchmark",
            "is_winner": winner is not None and entry.team == winner.team and entry.rank == winner.rank,
            "workbook_gap_pct_to_winner": _percent_gap(winner.objective if winner is not None else None, entry.objective),
        }
        try:
            submission = ensure_reference_submission(entry, cache_root=cache_root)
            local_validation_dir = workdir / "references" / submission.entry.team.replace("/", "-")
            validation = _load_cached_reference_validation(
                local_validation_dir,
                problem_path=scenario.problem_path,
                solution_path=submission.solution_path,
            )
            if validation is None:
                validation = validate_with_official_tool(
                    validator_env,
                    scenario.problem_path,
                    solution_path=submission.solution_path,
                    workdir=local_validation_dir,
                )
                _write_json(
                    _reference_validation_report_path(local_validation_dir),
                    {
                        "problem_path": str(scenario.problem_path),
                        "solution_path": str(submission.solution_path),
                        "validation": validation,
                    },
                )
            reference_report.update(
                {
                    "archive_path": str(submission.archive_path),
                    "extracted_dir": str(submission.extracted_dir),
                    "solution_path": str(submission.solution_path),
                    "archived_summary_path": str(submission.summary_json_path) if submission.summary_json_path is not None else None,
                    "archived_summary_metrics": submission.archived_metrics,
                    "local_validation": validation,
                    "objective_delta_local_minus_archived": _numeric_delta(
                        validation.get("summary_metrics", {}).get("obj"),
                        submission.archived_metrics.get("obj"),
                    ),
                    "objective_delta_archived_minus_workbook": _numeric_delta(
                        submission.archived_metrics.get("obj"),
                        entry.objective,
                    ),
                    "objective_delta_local_minus_workbook": _numeric_delta(
                        validation.get("summary_metrics", {}).get("obj"),
                        entry.objective,
                    ),
                }
            )
        except Exception as exc:
            reference_report["error"] = str(exc)
        references.append(reference_report)

    pop_report = None
    if validate_pop and scenario.pop_solution_path is not None:
        pop_report = run_pop_validation(
            scenario,
            cache_root=cache_root,
            validator_env=validator_env,
        )

    baseline_report = None
    baseline_validation = None
    if validate_baseline:
        if solve_baseline_if_missing:
            baseline_report = solve_baseline_scenario(
                scenario,
                cache_root=cache_root,
                policy=baseline_policy,
            )
        try:
            baseline_validation = validate_baseline_solution(
                scenario,
                cache_root=cache_root,
                validator_env=validator_env,
            )
        except FileNotFoundError:
            baseline_validation = None

    baseline_workbook_comparison = None
    if isinstance(baseline_validation, dict):
        summary = baseline_validation.get("validation_summary", {})
        if isinstance(summary, dict):
            baseline_workbook_comparison = {
                "objective_delta_local_minus_winner_workbook": _numeric_delta(
                    summary.get("obj"),
                    winner.objective if winner is not None else None,
                ),
                "objective_delta_local_minus_benchmark_workbook": _numeric_delta(
                    summary.get("obj"),
                    benchmark.objective if benchmark is not None else None,
                ),
                "objective_gap_pct_to_winner_workbook": _percent_gap(
                    winner.objective if winner is not None else None,
                    summary.get("obj"),
                ),
                "objective_gap_pct_to_benchmark_workbook": _percent_gap(
                    benchmark.objective if benchmark is not None else None,
                    summary.get("obj"),
                ),
            }

    report = {
        "dataset_key": scenario.dataset_key,
        "division": scenario.division,
        "network_model": scenario.network_model,
        "scenario_id": scenario.scenario_id,
        "problem_path": str(scenario.problem_path),
        "workbook_path": str(resolved_workbook),
        "scenario_track": scenario_track,
        "leaderboard_winner": winner.__dict__ if winner is not None else None,
        "leaderboard_benchmark": benchmark.__dict__ if benchmark is not None else None,
        "leaderboard_gap_pct_benchmark_to_winner": _percent_gap(
            winner.objective if winner is not None else None,
            benchmark.objective if benchmark is not None else None,
        ),
        "leaderboard_rows": [entry.__dict__ for entry in leaderboard_rows],
        "references": references,
        "pop_validation": pop_report,
        "baseline_run": baseline_report,
        "baseline_validation": baseline_validation,
        "baseline_workbook_comparison": baseline_workbook_comparison,
        "comparison_report_path": str(workdir / "comparison-report.json"),
    }
    _write_json(workdir / "comparison-report.json", report)
    return report


def compare_suite(
    scenarios: list[ScenarioRecord],
    *,
    cache_root: Path,
    results_root: Path | None = None,
    workbook_path: Path | None = None,
    include_benchmark: bool = True,
    top_k: int = 1,
    validate_pop: bool = True,
    validate_baseline: bool = True,
    solve_baseline_if_missing: bool = False,
    baseline_policy=None,
    switching_mode: str | None = None,
) -> list[dict[str, Any]]:
    validator_env = ensure_validator_environment(cache_root=cache_root)
    return [
        compare_scenario(
            scenario,
            cache_root=cache_root,
            validator_env=validator_env,
            results_root=results_root,
            workbook_path=workbook_path,
            include_benchmark=include_benchmark,
            top_k=top_k,
            validate_pop=validate_pop,
            validate_baseline=validate_baseline,
            solve_baseline_if_missing=solve_baseline_if_missing,
            baseline_policy=baseline_policy,
            switching_mode=switching_mode,
        )
        for scenario in scenarios
    ]


def aggregate_comparison_reports(reports: list[dict[str, Any]]) -> dict[str, Any]:
    aggregates: dict[str, Any] = {
        "scenario_count": len(reports),
        "top_reference": defaultdict(float),
        "benchmark_reference": defaultdict(float),
        "baseline": defaultdict(float),
        "counts": defaultdict(int),
        "by_division": defaultdict(lambda: {"scenario_count": 0, "counts": defaultdict(int), "leaderboard": defaultdict(float)}),
        "by_track": defaultdict(lambda: {"scenario_count": 0, "counts": defaultdict(int), "leaderboard": defaultdict(float)}),
    }
    for report in reports:
        division = report.get("division")
        track = report.get("scenario_track", {}).get("track_key") if isinstance(report.get("scenario_track"), dict) else None
        benchmark_gap = report.get("leaderboard_gap_pct_benchmark_to_winner")
        if isinstance(division, str):
            division_bucket = aggregates["by_division"][division]
            division_bucket["scenario_count"] += 1
            if isinstance(benchmark_gap, (int, float)):
                division_bucket["leaderboard"]["benchmark_gap_pct_to_winner"] += float(benchmark_gap)
        if isinstance(track, str):
            track_bucket = aggregates["by_track"][track]
            track_bucket["scenario_count"] += 1
            if isinstance(benchmark_gap, (int, float)):
                track_bucket["leaderboard"]["benchmark_gap_pct_to_winner"] += float(benchmark_gap)

        references = report.get("references", [])
        if isinstance(references, list) and references:
            first = references[0] if isinstance(references[0], dict) else None
            if isinstance(first, dict):
                local_summary = first.get("local_validation", {}).get("summary_metrics", {})
                if isinstance(local_summary, dict):
                    aggregates["counts"]["top_reference"] += 1
                    if isinstance(division, str):
                        aggregates["by_division"][division]["counts"]["top_reference"] += 1
                    if isinstance(track, str):
                        aggregates["by_track"][track]["counts"]["top_reference"] += 1
                    for key, value in local_summary.items():
                        if isinstance(value, (int, float)):
                            aggregates["top_reference"][key] += float(value)
                delta = first.get("objective_delta_local_minus_archived")
                if isinstance(delta, (int, float)):
                    aggregates["top_reference"]["objective_delta_local_minus_archived"] += float(delta)
            for reference in references:
                if not isinstance(reference, dict):
                    continue
                entry = reference.get("entry", {})
                if not isinstance(entry, dict) or entry.get("team") != "ARPA-e Benchmark":
                    continue
                local_summary = reference.get("local_validation", {}).get("summary_metrics", {})
                if isinstance(local_summary, dict):
                    aggregates["counts"]["benchmark_reference"] += 1
                    if isinstance(division, str):
                        aggregates["by_division"][division]["counts"]["benchmark_reference"] += 1
                    if isinstance(track, str):
                        aggregates["by_track"][track]["counts"]["benchmark_reference"] += 1
                    for key, value in local_summary.items():
                        if isinstance(value, (int, float)):
                            aggregates["benchmark_reference"][key] += float(value)
                delta = reference.get("objective_delta_local_minus_archived")
                if isinstance(delta, (int, float)):
                    aggregates["benchmark_reference"]["objective_delta_local_minus_archived"] += float(delta)
                break

        baseline = report.get("baseline_validation")
        if isinstance(baseline, dict):
            validation_summary = baseline.get("validation_summary", {})
            if isinstance(validation_summary, dict):
                aggregates["counts"]["baseline"] += 1
                if isinstance(division, str):
                    aggregates["by_division"][division]["counts"]["baseline"] += 1
                if isinstance(track, str):
                    aggregates["by_track"][track]["counts"]["baseline"] += 1
                for key, value in validation_summary.items():
                    if isinstance(value, (int, float)):
                        aggregates["baseline"][key] += float(value)

    return {
        "scenario_count": aggregates["scenario_count"],
        "counts": dict(aggregates["counts"]),
        "top_reference": dict(aggregates["top_reference"]),
        "benchmark_reference": dict(aggregates["benchmark_reference"]),
        "baseline": dict(aggregates["baseline"]),
        "by_division": {
            key: {
                "scenario_count": value["scenario_count"],
                "counts": dict(value["counts"]),
                "leaderboard": dict(value["leaderboard"]),
            }
            for key, value in aggregates["by_division"].items()
        },
        "by_track": {
            key: {
                "scenario_count": value["scenario_count"],
                "counts": dict(value["counts"]),
                "leaderboard": dict(value["leaderboard"]),
            }
            for key, value in aggregates["by_track"].items()
        },
    }
