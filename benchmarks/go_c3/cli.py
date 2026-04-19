#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""CLI entry point for the GO Challenge 3 harness."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from markets.go_c3 import GoC3Policy, GoC3Problem
from markets.go_c3.policy import DEFAULT_COMMITMENT_MIP_REL_GAP

from .compare import aggregate_comparison_reports, compare_scenario, compare_suite
from .datasets import discover_scenarios, ensure_dataset_unpacked
from .leaderboard import benchmark_entry, load_event_workbook, scenario_leaderboard
from .manifests import load_dataset_manifest, load_suite_manifest
from .paths import default_cache_root, default_results_root
from .runner import (
    _resolve_baseline_solution_path,
    _scenario_matches,
    run_pop_validation,
    run_suite,
    sced_fixed_output_dir,
    solve_baseline_scenario,
    validate_baseline_solution,
)
from .validator import compare_bus_residuals_with_official_tool, ensure_validator_environment


def _sced_fixed_solution_path(cache_root: Path, scenario) -> Path:
    """Resolve the most recent surge sced-fixed exported solution for a scenario."""
    return sced_fixed_output_dir(cache_root, scenario) / "solution.json"


def _print_json(data) -> None:
    print(json.dumps(data, indent=2, sort_keys=True))


def _print_lines(lines: list[str]) -> None:
    print("\n".join(lines))


def _parse_json_object_argument(raw: str | None, *, flag: str) -> dict[str, Any] | None:
    if raw is None:
        return None
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise SystemExit(f"{flag} must be valid JSON: {exc}") from exc
    if not isinstance(payload, dict):
        raise SystemExit(f"{flag} must decode to a JSON object")
    return payload


def _top_diagnostics(summary: dict[str, object], limit: int = 3) -> list[str]:
    diagnostics = summary.get("infeas_diagnostics")
    if not isinstance(diagnostics, dict) or not diagnostics:
        return []
    ranked: list[tuple[str, float]] = []
    for code, detail in diagnostics.items():
        score = 0.0
        if isinstance(detail, dict):
            raw = detail.get("val")
            if isinstance(raw, (int, float)):
                score = float(raw)
        ranked.append((str(code), score))
    ranked.sort(key=lambda item: (-item[1], item[0]))
    return [
        f"{code}={score:g}" if score else code
        for code, score in ranked[:limit]
    ]


def _format_metric(value: object) -> str:
    if isinstance(value, float):
        return f"{value:.6f}"
    return str(value)


def _slugify(value: str | None) -> str:
    if not value:
        return "all"
    return "".join(char if char.isalnum() or char in ("-", "_", ".") else "_" for char in value)


def _summarize_validation(report: dict[str, object], *, label: str, solution_key: str) -> list[str]:
    summary = report.get("validation_summary", {})
    if not isinstance(summary, dict):
        summary = {}
    lines = [
        f"{label}: {report['dataset_key']} {report['division']} {report.get('network_model', '?')} scenario_{int(report['scenario_id']):03d}",
        " ".join(
            [
                f"feasible={summary.get('feas', '?')}",
                f"phys_feas={summary.get('phys_feas', '?')}",
                f"infeasible={summary.get('infeas', '?')}",
                f"pass={summary.get('pass', '?')}",
                f"objective={_format_metric(summary.get('obj', '?'))}",
                f"z_penalty={_format_metric(summary.get('z_penalty', '?'))}",
            ]
        ),
    ]
    diagnostics = _top_diagnostics(summary)
    if diagnostics:
        lines.append(f"top_diagnostics: {', '.join(diagnostics)}")
    if "problem_surplus_total" in summary:
        lines.append(f"problem_surplus_total: {_format_metric(summary['problem_surplus_total'])}")
    lines.append(f"problem: {report['problem_path']}")
    lines.append(f"{solution_key}: {report[solution_key]}")
    lines.append(f"report: {report['run_report_path']}")
    return lines


def _summarize_baseline(report: dict[str, object]) -> list[str]:
    lines = [
        f"baseline: {report['dataset_key']} {report['division']} scenario_{int(report['scenario_id']):03d}",
        f"status={report['status']} solver={report['policy']['lp_solver']} ac_nlp={report['policy'].get('nlp_solver') or report['policy'].get('ac_nlp_solver') or 'ipopt'} solve_seconds={float(report.get('solve_seconds', 0.0)):.2f}",
    ]
    dispatch_summary = report.get("dispatch_summary", {})
    if isinstance(dispatch_summary, dict) and dispatch_summary:
        pieces = []
        for key in ("total_cost", "total_energy_cost", "total_no_load_cost", "total_startup_cost"):
            value = dispatch_summary.get(key)
            if isinstance(value, (int, float)):
                pieces.append(f"{key}={value:.3f}")
        if pieces:
            lines.append("dispatch: " + " ".join(pieces))
    ac_reconcile = report.get("ac_reconcile", {})
    if isinstance(ac_reconcile, dict) and ac_reconcile:
        pieces = []
        mode = ac_reconcile.get("mode")
        if isinstance(mode, str):
            pieces.append(f"mode={mode}")
        for key in ("converged_periods", "periods", "total_iterations", "max_mismatch", "solve_time_secs"):
            value = ac_reconcile.get(key)
            if isinstance(value, (int, float)):
                pieces.append(f"{key}={value:.6f}" if key == "max_mismatch" else f"{key}={value}")
        if pieces:
            lines.append("ac_reconcile: " + " ".join(pieces))
    issue_counts = report.get("adapter_issue_counts", {})
    if isinstance(issue_counts, dict):
        lines.append(
            "adapter_issues: "
            + " ".join(f"{name}={issue_counts.get(name, 0)}" for name in ("error", "warning", "info"))
        )
    if report.get("error"):
        lines.append(f"error: {report['error']}")
    lines.append(f"request: {report['request_path']}")
    lines.append(f"solution: {report.get('solution_path')}")
    lines.append(f"adapter_report: {report['adapter_report_path']}")
    lines.append(f"report: {report['run_report_path']}")
    return lines


def _summarize_suite(reports: list[dict[str, object]]) -> list[str]:
    lines = [f"suite results: {len(reports)} scenario(s)"]
    for entry in reports:
        prefix = f"{entry['dataset_key']} {entry['division']} scenario_{int(entry['scenario_id']):03d}"
        parts = [prefix]
        pop = entry.get("pop_validation")
        if isinstance(pop, dict):
            summary = pop.get("validation_summary", {})
            if isinstance(summary, dict):
                parts.append(f"pop_feas={summary.get('feas', '?')}")
        baseline = entry.get("baseline")
        if isinstance(baseline, dict):
            parts.append(f"baseline={baseline.get('status', '?')}")
            dispatch_summary = baseline.get("dispatch_summary", {})
            if isinstance(dispatch_summary, dict):
                total_cost = dispatch_summary.get("total_cost")
                if isinstance(total_cost, (int, float)):
                    parts.append(f"cost={total_cost:.3f}")
        baseline_validation = entry.get("baseline_validation")
        if isinstance(baseline_validation, dict):
            summary = baseline_validation.get("validation_summary", {})
            if isinstance(summary, dict):
                parts.append(f"baseline_feas={summary.get('feas', '?')}")
                diagnostics = _top_diagnostics(summary, limit=2)
                if diagnostics:
                    parts.append("diag=" + ",".join(diagnostics))
        lines.append(" | ".join(parts))
    return lines


def _summarize_comparison(report: dict[str, object]) -> list[str]:
    lines = [
        f"comparison: {report['dataset_key']} {report['division']} {report['network_model']} scenario_{int(report['scenario_id']):03d}",
        f"workbook: {report['workbook_path']}",
    ]
    scenario_track = report.get("scenario_track", {})
    if isinstance(scenario_track, dict):
        lines.append(
            "track: "
            + " ".join(
                [
                    f"switching={scenario_track.get('switching_mode', '?')}",
                    f"track_key={scenario_track.get('track_key', '?')}",
                ]
            )
        )
    winner = report.get("leaderboard_winner", {})
    benchmark = report.get("leaderboard_benchmark", {})
    if isinstance(winner, dict) and winner:
        lines.append(
            "leaderboard_winner: "
            + " ".join(
                [
                    f"team={winner.get('team', '?')}",
                    f"rank={winner.get('rank', '?')}",
                    f"objective={_format_metric(winner.get('objective', '?'))}",
                    f"runtime={_format_metric(winner.get('runtime_seconds', '?'))}",
                ]
            )
        )
    if isinstance(benchmark, dict) and benchmark:
        lines.append(
            "leaderboard_benchmark: "
            + " ".join(
                [
                    f"rank={benchmark.get('rank', '?')}",
                    f"objective={_format_metric(benchmark.get('objective', '?'))}",
                    f"gap_pct_to_winner={_format_metric(report.get('leaderboard_gap_pct_benchmark_to_winner', '?'))}",
                ]
            )
        )
    references = report.get("references", [])
    if isinstance(references, list) and references:
        lines.append(f"references: {len(references)}")
        for item in references:
            if not isinstance(item, dict):
                continue
            entry = item.get("entry", {})
            archived = item.get("archived_summary_metrics", {})
            local = item.get("local_validation", {}).get("summary_metrics", {})
            if not isinstance(entry, dict):
                entry = {}
            if not isinstance(archived, dict):
                archived = {}
            if not isinstance(local, dict):
                local = {}
            pieces = [
                f"team={entry.get('team', '?')}",
                f"rank={entry.get('rank', '?')}",
                f"workbook_obj={_format_metric(entry.get('objective', '?'))}",
                f"archived_obj={_format_metric(archived.get('obj', '?'))}",
                f"local_obj={_format_metric(local.get('obj', '?'))}",
                f"local_feas={local.get('feas', '?')}",
                f"local_phys_feas={local.get('phys_feas', '?')}",
                f"delta_local_archived={_format_metric(item.get('objective_delta_local_minus_archived', '?'))}",
                f"delta_archived_workbook={_format_metric(item.get('objective_delta_archived_minus_workbook', '?'))}",
            ]
            lines.append("  " + " ".join(pieces))
    pop = report.get("pop_validation")
    if isinstance(pop, dict):
        lines.extend(_summarize_validation(pop, label="pop-validation", solution_key="pop_solution_path"))
    baseline = report.get("baseline_validation")
    if isinstance(baseline, dict):
        lines.extend(_summarize_validation(baseline, label="baseline-validation", solution_key="solution_path"))
    return lines


def _summarize_comparison_suite(reports: list[dict[str, object]]) -> list[str]:
    lines = [f"comparison suite: {len(reports)} scenario(s)"]
    for report in reports:
        prefix = f"{report['dataset_key']} {report['division']} {report['network_model']} scenario_{int(report['scenario_id']):03d}"
        parts = [prefix]
        scenario_track = report.get("scenario_track", {})
        if isinstance(scenario_track, dict):
            parts.append(f"track={scenario_track.get('switching_mode', '?')}")
        references = report.get("references", [])
        if isinstance(references, list) and references:
            ref0 = references[0] if isinstance(references[0], dict) else {}
            entry = ref0.get("entry", {}) if isinstance(ref0, dict) else {}
            archived = ref0.get("archived_summary_metrics", {}) if isinstance(ref0, dict) else {}
            local = ref0.get("local_validation", {}).get("summary_metrics", {}) if isinstance(ref0, dict) else {}
            if isinstance(entry, dict):
                parts.append(f"top_team={entry.get('team', '?')}")
                parts.append(f"leaderboard_obj={_format_metric(entry.get('objective', '?'))}")
            if isinstance(archived, dict):
                parts.append(f"archived_obj={_format_metric(archived.get('obj', '?'))}")
            if isinstance(local, dict):
                parts.append(f"local_ref_feas={local.get('feas', '?')}")
                parts.append(f"local_ref_phys={local.get('phys_feas', '?')}")
        baseline = report.get("baseline_validation")
        if isinstance(baseline, dict):
            summary = baseline.get("validation_summary", {})
            if isinstance(summary, dict):
                parts.append(f"baseline_feas={summary.get('feas', '?')}")
                parts.append(f"baseline_phys={summary.get('phys_feas', '?')}")
                parts.append(f"baseline_obj={_format_metric(summary.get('obj', '?'))}")
        lines.append(" | ".join(parts))
    return lines


def _summarize_bus_residual_comparison(report: dict[str, object]) -> list[str]:
    comparison = report.get("comparison", {})
    if not isinstance(comparison, dict):
        comparison = {}
    metrics = comparison.get("metrics", {})
    if not isinstance(metrics, dict):
        metrics = {}
    lines = [
        "bus-residual-comparison:",
        f"problem: {report.get('problem_path')}",
        f"lhs_solution: {report.get('lhs_solution_path')}",
        f"rhs_solution: {report.get('rhs_solution_path')}",
        "bus_p: "
        + " ".join(
            [
                f"lhs={_format_metric(metrics.get('lhs_sum_bus_t_z_p', '?'))}",
                f"rhs={_format_metric(metrics.get('rhs_sum_bus_t_z_p', '?'))}",
                f"lhs_minus_rhs={_format_metric(metrics.get('lhs_minus_rhs_sum_bus_t_z_p', '?'))}",
            ]
        ),
        "bus_q: "
        + " ".join(
            [
                f"lhs={_format_metric(metrics.get('lhs_sum_bus_t_z_q', '?'))}",
                f"rhs={_format_metric(metrics.get('rhs_sum_bus_t_z_q', '?'))}",
                f"lhs_minus_rhs={_format_metric(metrics.get('lhs_minus_rhs_sum_bus_t_z_q', '?'))}",
            ]
        ),
    ]
    for residual_key in ("bus_p", "bus_q"):
        bucket = comparison.get(residual_key, {})
        if not isinstance(bucket, dict):
            continue
        worse = bucket.get("top_lhs_worse", [])
        if isinstance(worse, list) and worse:
            lines.append(f"{residual_key}_top_lhs_worse:")
            for item in worse:
                if not isinstance(item, dict):
                    continue
                lines.append(
                    "  "
                    + " ".join(
                        [
                            f"bus={item.get('bus_uid', '?')}",
                            f"delta={_format_metric(item.get('lhs_minus_rhs_sum_abs_residual_pu_hours', '?'))}",
                            f"lhs={_format_metric(item.get('lhs_sum_abs_residual_pu_hours', '?'))}",
                            f"rhs={_format_metric(item.get('rhs_sum_abs_residual_pu_hours', '?'))}",
                        ]
                    )
                )
    lines.append(f"report: {report.get('report_path')}")
    return lines


def _dataset_by_key(key: str):
    manifest = load_dataset_manifest()
    try:
        return manifest, manifest.by_key()[key]
    except KeyError as exc:
        raise SystemExit(f"unknown dataset key: {key}") from exc


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="GO Competition Challenge 3 harness")
    parser.add_argument("--json", action="store_true", help="Print raw JSON instead of concise summaries")
    subparsers = parser.add_subparsers(dest="command", required=True)

    subparsers.add_parser("list-datasets", help="List public GO C3 dataset manifest entries")
    subparsers.add_parser("list-suites", help="List benchmark suite manifest entries")

    fetch = subparsers.add_parser("fetch", help="Download and unpack one dataset")
    fetch.add_argument("dataset_key")
    fetch.add_argument("--cache-root", type=Path, default=default_cache_root())

    discover = subparsers.add_parser("discover", help="List scenarios discovered in an unpacked dataset")
    discover.add_argument("dataset_key")
    discover.add_argument("--cache-root", type=Path, default=default_cache_root())
    discover.add_argument("--network-model-prefix")

    inspect = subparsers.add_parser("inspect", help="Summarize one GO C3 problem file")
    inspect.add_argument("problem_path", type=Path)

    validator = subparsers.add_parser("ensure-validator", help="Create/update the pinned official validator environment")
    validator.add_argument("--cache-root", type=Path, default=default_cache_root())
    validator.add_argument("--results-root", type=Path, default=default_results_root())

    compare = subparsers.add_parser(
        "compare-workbook",
        help="Compare a public scenario against workbook leaderboard rows and local validation artifacts",
    )
    compare.add_argument("workbook_path", type=Path)
    compare.add_argument("dataset_key")
    compare.add_argument("division")
    compare.add_argument("scenario_id", type=int)
    compare.add_argument("--cache-root", type=Path, default=default_cache_root())
    compare.add_argument("--top", type=int, default=5)
    compare.add_argument("--switching-mode", choices=["SW0", "SW1"])

    compare_scenario_cmd = subparsers.add_parser(
        "compare-scenario",
        help="Compare one scenario against leaderboard reference submissions and local validation artifacts",
    )
    compare_scenario_cmd.add_argument("dataset_key")
    compare_scenario_cmd.add_argument("division")
    compare_scenario_cmd.add_argument("scenario_id", type=int)
    compare_scenario_cmd.add_argument("--cache-root", type=Path, default=default_cache_root())
    compare_scenario_cmd.add_argument("--results-root", type=Path, default=default_results_root())
    compare_scenario_cmd.add_argument("--workbook-path", type=Path)
    compare_scenario_cmd.add_argument("--top-k", type=int, default=1)
    compare_scenario_cmd.add_argument("--switching-mode", choices=["SW0", "SW1"])
    compare_scenario_cmd.add_argument("--no-benchmark", action="store_true")
    compare_scenario_cmd.add_argument("--no-pop", action="store_true")
    compare_scenario_cmd.add_argument("--no-baseline", action="store_true")
    compare_scenario_cmd.add_argument("--solve-baseline-if-missing", action="store_true")
    compare_scenario_cmd.add_argument("--consumer-mode", default="dispatchable", choices=["dispatchable", "upper_bound", "initial_status"])
    compare_scenario_cmd.add_argument("--time-limit-secs", type=float)
    compare_scenario_cmd.add_argument("--lp-solver", default="gurobi")
    compare_scenario_cmd.add_argument("--ac-nlp-solver", default="ipopt")
    compare_scenario_cmd.add_argument("--with-pricing", action="store_true")
    compare_scenario_cmd.add_argument("--ac-reconcile", default="ac_dispatch", choices=["none", "acpf", "ac_dispatch"])

    compare_suite_cmd = subparsers.add_parser(
        "compare-suite",
        help="Compare every scenario in a suite against leaderboard reference submissions and local validation artifacts",
    )
    compare_suite_cmd.add_argument("suite_name")
    compare_suite_cmd.add_argument("--cache-root", type=Path, default=default_cache_root())
    compare_suite_cmd.add_argument("--results-root", type=Path, default=default_results_root())
    compare_suite_cmd.add_argument("--workbook-path", type=Path)
    compare_suite_cmd.add_argument("--top-k", type=int, default=1)
    compare_suite_cmd.add_argument("--switching-mode", choices=["SW0", "SW1"])
    compare_suite_cmd.add_argument("--no-benchmark", action="store_true")
    compare_suite_cmd.add_argument("--no-pop", action="store_true")
    compare_suite_cmd.add_argument("--no-baseline", action="store_true")
    compare_suite_cmd.add_argument("--solve-baseline-if-missing", action="store_true")
    compare_suite_cmd.add_argument("--network-model-prefix")
    compare_suite_cmd.add_argument("--consumer-mode", default="dispatchable", choices=["dispatchable", "upper_bound", "initial_status"])
    compare_suite_cmd.add_argument("--time-limit-secs", type=float)
    compare_suite_cmd.add_argument("--lp-solver", default="gurobi")
    compare_suite_cmd.add_argument("--ac-nlp-solver", default="ipopt")
    compare_suite_cmd.add_argument("--with-pricing", action="store_true")
    compare_suite_cmd.add_argument("--ac-reconcile", default="ac_dispatch", choices=["none", "acpf", "ac_dispatch"])

    pop = subparsers.add_parser("validate-pop", help="Validate a public POP solution for one dataset/scenario")
    pop.add_argument("dataset_key")
    pop.add_argument("division")
    pop.add_argument("scenario_id", type=int)
    pop.add_argument("--cache-root", type=Path, default=default_cache_root())

    baseline = subparsers.add_parser("solve-baseline", help="Run the baseline GO->Surge adapter on one scenario")
    baseline.add_argument("dataset_key")
    baseline.add_argument("division")
    baseline.add_argument("scenario_id", type=int)
    baseline.add_argument("--cache-root", type=Path, default=default_cache_root())
    baseline.add_argument("--consumer-mode", default="dispatchable", choices=["dispatchable", "upper_bound", "initial_status"])
    baseline.add_argument("--time-limit-secs", type=float)
    baseline.add_argument(
        "--mip-gap",
        type=float,
        default=DEFAULT_COMMITMENT_MIP_REL_GAP,
        help=(
            "Relative MIP optimality gap "
            f"(default: {DEFAULT_COMMITMENT_MIP_REL_GAP:g}; e.g. 0.01 = 1%%)"
        ),
    )
    baseline.add_argument("--lp-solver", default="gurobi")
    baseline.add_argument("--ac-nlp-solver", default="ipopt")
    baseline.add_argument("--with-pricing", action="store_true")
    baseline.add_argument("--ac-reconcile", default="ac_dispatch", choices=["none", "acpf", "ac_dispatch"])

    validate_baseline = subparsers.add_parser("validate-baseline", help="Validate an exported baseline solution for one scenario")
    validate_baseline.add_argument("dataset_key")
    validate_baseline.add_argument("division")
    validate_baseline.add_argument("scenario_id", type=int)
    validate_baseline.add_argument("--cache-root", type=Path, default=default_cache_root())

    compare_bus_residuals = subparsers.add_parser(
        "compare-bus-residuals",
        help="Compare official-checker bus P/Q residuals between two solutions for one scenario",
    )
    compare_bus_residuals.add_argument("dataset_key")
    compare_bus_residuals.add_argument("division")
    compare_bus_residuals.add_argument("scenario_id", type=int)
    compare_bus_residuals.add_argument("--cache-root", type=Path, default=default_cache_root())
    compare_bus_residuals.add_argument("--solution-path", type=Path)
    compare_bus_residuals.add_argument("--reference-solution-path", type=Path, required=True)
    compare_bus_residuals.add_argument("--top-k", type=int, default=10)

    q_term_diff_parser = subparsers.add_parser(
        "q-term-diff",
        help=(
            "Diff per-bus per-period Q (and P) injection terms between two "
            "validator bus_detail payloads (lhs is the probe, rhs the reference)"
        ),
    )
    q_term_diff_parser.add_argument("dataset_key")
    q_term_diff_parser.add_argument("division")
    q_term_diff_parser.add_argument("scenario_id", type=int)
    q_term_diff_parser.add_argument("--cache-root", type=Path, default=default_cache_root())
    q_term_diff_parser.add_argument(
        "--solution-path",
        type=Path,
        help="LHS solution (probe). Default: latest sced-fixed solution.json under cache.",
    )
    q_term_diff_parser.add_argument(
        "--reference-solution-path",
        type=Path,
        required=True,
        help="RHS solution (reference, e.g. winner pop_solution.json).",
    )
    q_term_diff_parser.add_argument("--top-k", type=int, default=20)
    q_term_diff_parser.add_argument(
        "--workdir",
        type=Path,
        help="Override output directory (default: cache/runs/q-term-diffs/<scenario>).",
    )

    suite = subparsers.add_parser("run-suite", help="Run one benchmark suite")
    suite.add_argument("suite_name")
    suite.add_argument("--cache-root", type=Path, default=default_cache_root())
    suite.add_argument("--with-validator", action="store_true")
    suite.add_argument("--consumer-mode", default="dispatchable", choices=["dispatchable", "upper_bound", "initial_status"])
    suite.add_argument("--time-limit-secs", type=float)
    suite.add_argument("--lp-solver", default="gurobi")
    suite.add_argument("--ac-nlp-solver", default="ipopt")
    suite.add_argument("--with-pricing", action="store_true")
    suite.add_argument("--ac-reconcile", default="ac_dispatch", choices=["none", "acpf", "ac_dispatch"])
    suite.add_argument("--network-model-prefix")

    # --- Validation harness subcommands ---

    extract_commit = subparsers.add_parser(
        "extract-commitment",
        help="Extract commitment schedule from a GO C3 solution file",
    )
    extract_commit.add_argument("solution_path", type=Path)
    extract_commit.add_argument("--label", default="")

    sced_fixed = subparsers.add_parser(
        "solve-sced-fixed",
        help="Solve a scenario with fixed reference commitment (SCED only)",
    )
    sced_fixed.add_argument("dataset_key")
    sced_fixed.add_argument("division")
    sced_fixed.add_argument("scenario_id", type=int)
    sced_fixed.add_argument("--cache-root", type=Path, default=default_cache_root())
    sced_fixed.add_argument("--results-root", type=Path, default=default_results_root())
    sced_fixed.add_argument(
        "--commitment-source",
        default="reference_winner",
        choices=["self", "reference_winner", "reference_benchmark", "pop"],
    )
    sced_fixed.add_argument("--commitment-solution-path", type=Path, help="Explicit solution file to use as commitment source")
    sced_fixed.add_argument("--switching-mode", choices=["SW0", "SW1"])
    sced_fixed.add_argument("--lp-solver", default="gurobi")
    sced_fixed.add_argument("--ac-nlp-solver", default="ipopt")
    sced_fixed.add_argument("--with-pricing", action="store_true")
    sced_fixed.add_argument(
        "--ac-reconcile",
        default="ac_dispatch",
        choices=["none", "acpf", "ac_dispatch", "ac_exact_replay"],
    )

    detail_cmp = subparsers.add_parser(
        "detail-compare",
        help="Detailed comparison between two GO C3 solutions",
    )
    detail_cmp.add_argument("lhs_solution", type=Path)
    detail_cmp.add_argument("rhs_solution", type=Path)
    detail_cmp.add_argument("--lhs-label", default="lhs")
    detail_cmp.add_argument("--rhs-label", default="rhs")
    detail_cmp.add_argument("--base-mva", type=float, default=100.0)
    detail_cmp.add_argument("--lhs-validator-summary", type=Path, help="Path to validator summary.json for LHS")
    detail_cmp.add_argument("--rhs-validator-summary", type=Path, help="Path to validator summary.json for RHS")

    ledger_update = subparsers.add_parser(
        "ledger-update",
        help="Record current scores to the ledger from run reports",
    )
    ledger_update.add_argument("mode", choices=["sced_fixed", "scuc", "end_to_end"])
    ledger_update.add_argument("--cache-root", type=Path, default=default_cache_root())
    ledger_update.add_argument("--suite-name", default="d2_sw0_73")
    ledger_update.add_argument("--notes", default="")

    ledger_status = subparsers.add_parser(
        "ledger-status",
        help="Show current scores from the ledger",
    )
    ledger_status.add_argument("mode", choices=["sced_fixed", "scuc", "end_to_end"])

    return parser


def _locate_scenario(dataset_key: str, division: str, scenario_id: int, cache_root: Path):
    manifest, resource = _dataset_by_key(dataset_key)
    unpacked = ensure_dataset_unpacked(resource, cache_root=cache_root)
    for scenario in discover_scenarios(unpacked.unpacked_root, resource.key):
        if scenario.division == division and scenario.scenario_id == scenario_id:
            return manifest, scenario
    raise SystemExit(f"scenario not found for {dataset_key} {division} {scenario_id}")


def _select_suite_scenarios(suite_name: str, cache_root: Path, *, network_model_prefix: str | None = None):
    dataset_manifest = load_dataset_manifest()
    suite_manifest = load_suite_manifest()
    suite = suite_manifest.by_name().get(suite_name)
    if suite is None:
        raise SystemExit(f"unknown suite: {suite_name}")
    dataset_by_key = dataset_manifest.by_key()
    selected = []
    for target in suite.targets:
        resource = dataset_by_key[target.dataset]
        unpacked = ensure_dataset_unpacked(resource, cache_root=cache_root)
        scenarios = [
            scenario
            for scenario in discover_scenarios(unpacked.unpacked_root, resource.key)
            if _scenario_matches(
                scenario,
                target.divisions,
                target.scenario_ids,
                network_model_prefix=network_model_prefix,
            )
        ]
        selected.extend(scenarios)
    return suite, selected


def _default_suite_switching_mode(suite) -> str | None:
    modes = sorted(
        {
            str(target.switching_mode).upper()
            for target in getattr(suite, "targets", [])
            if getattr(target, "switching_mode", None)
        }
    )
    if len(modes) == 1:
        return modes[0]
    return None


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    if args.command == "list-datasets":
        manifest = load_dataset_manifest()
        payload = {
            "version": manifest.version,
            "generated_on": manifest.generated_on,
            "datasets": [dataset.__dict__ for dataset in manifest.datasets],
        }
        if args.json:
            _print_json(payload)
        else:
            lines = [f"datasets: {len(manifest.datasets)} (manifest {manifest.version}, generated {manifest.generated_on})"]
            for dataset in manifest.datasets:
                lines.append(
                    f"{dataset.key}: {dataset.name} [{dataset.division}] buses={dataset.bus_count} public={dataset.public}"
                )
            _print_lines(lines)
        return 0

    if args.command == "list-suites":
        manifest = load_suite_manifest()
        payload = {
            "version": manifest.version,
            "suites": [
                {
                    "name": suite.name,
                    "description": suite.description,
                    "targets": [target.__dict__ for target in suite.targets],
                }
                for suite in manifest.suites
            ],
        }
        if args.json:
            _print_json(payload)
        else:
            lines = [f"suites: {len(manifest.suites)} (manifest {manifest.version})"]
            for suite in manifest.suites:
                lines.append(f"{suite.name}: {suite.description}")
            _print_lines(lines)
        return 0

    if args.command == "fetch":
        _, resource = _dataset_by_key(args.dataset_key)
        unpacked = ensure_dataset_unpacked(resource, cache_root=args.cache_root)
        payload = {
            "dataset_key": resource.key,
            "archive_path": str(unpacked.archive_path),
            "unpacked_root": str(unpacked.unpacked_root),
        }
        if args.json:
            _print_json(payload)
        else:
            _print_lines(
                [
                    f"fetched: {resource.key}",
                    f"archive: {unpacked.archive_path}",
                    f"unpacked: {unpacked.unpacked_root}",
                ]
            )
        return 0

    if args.command == "discover":
        _, resource = _dataset_by_key(args.dataset_key)
        unpacked = ensure_dataset_unpacked(resource, cache_root=args.cache_root)
        scenarios = discover_scenarios(unpacked.unpacked_root, resource.key)
        if args.network_model_prefix:
            scenarios = [
                scenario
                for scenario in scenarios
                if scenario.network_model.startswith(args.network_model_prefix)
            ]
        payload = [
            scenario.__dict__
            | {
                "problem_path": str(scenario.problem_path),
                "pop_solution_path": str(scenario.pop_solution_path) if scenario.pop_solution_path else None,
                "pop_log_path": str(scenario.pop_log_path) if scenario.pop_log_path else None,
            }
            for scenario in scenarios
        ]
        if args.json:
            _print_json(payload)
        else:
            lines = [f"scenarios: {resource.key} ({len(scenarios)})"]
            for scenario in scenarios:
                lines.append(
                    f"{scenario.division} {scenario.network_model} scenario_{scenario.scenario_id:03d}: {scenario.problem_path.name}"
                )
            _print_lines(lines)
        return 0

    if args.command == "inspect":
        problem = GoC3Problem.load(args.problem_path)
        payload = problem.summary()
        if args.json:
            _print_json(payload)
        else:
            _print_lines([f"{key}: {value}" for key, value in payload.items()])
        return 0

    if args.command == "ensure-validator":
        env = ensure_validator_environment(cache_root=args.cache_root)
        payload = env.metadata()
        if args.json:
            _print_json(payload)
        else:
            _print_lines(
                [
                    "validator environment ready",
                    f"c3_data_utilities_ref: {payload['c3_data_utilities_ref']}",
                    f"go3_data_model_ref: {payload['go3_data_model_ref']}",
                    f"venv: {payload['venv_dir']}",
                ]
            )
        return 0

    if args.command == "compare-scenario":
        _, scenario = _locate_scenario(args.dataset_key, args.division, args.scenario_id, args.cache_root)
        policy = GoC3Policy(
            consumer_mode=args.consumer_mode,
            commitment_time_limit_secs=args.time_limit_secs,
            lp_solver=args.lp_solver,
            nlp_solver=args.ac_nlp_solver,
            run_pricing=args.with_pricing,
            ac_reconcile_mode=args.ac_reconcile,
        )
        payload = compare_scenario(
            scenario,
            cache_root=args.cache_root,
            results_root=args.results_root,
            workbook_path=args.workbook_path,
            include_benchmark=not args.no_benchmark,
            top_k=args.top_k,
            validate_pop=not args.no_pop,
            validate_baseline=not args.no_baseline,
            solve_baseline_if_missing=args.solve_baseline_if_missing,
            baseline_policy=policy,
            switching_mode=args.switching_mode,
        )
        if args.json:
            _print_json(payload)
        else:
            _print_lines(_summarize_comparison(payload))
        return 0

    if args.command == "compare-suite":
        suite, scenarios = _select_suite_scenarios(
            args.suite_name,
            args.cache_root,
            network_model_prefix=args.network_model_prefix,
        )
        switching_mode = args.switching_mode or _default_suite_switching_mode(suite)
        policy = GoC3Policy(
            consumer_mode=args.consumer_mode,
            commitment_time_limit_secs=args.time_limit_secs,
            lp_solver=args.lp_solver,
            nlp_solver=args.ac_nlp_solver,
            run_pricing=args.with_pricing,
            ac_reconcile_mode=args.ac_reconcile,
        )
        reports = compare_suite(
            scenarios,
            cache_root=args.cache_root,
            results_root=args.results_root,
            workbook_path=args.workbook_path,
            include_benchmark=not args.no_benchmark,
            top_k=args.top_k,
            validate_pop=not args.no_pop,
            validate_baseline=not args.no_baseline,
            solve_baseline_if_missing=args.solve_baseline_if_missing,
            baseline_policy=policy,
            switching_mode=switching_mode,
        )
        aggregates = aggregate_comparison_reports(reports)
        summary_payload = {
            "suite": suite.name,
            "description": suite.description,
            "results_root": str(args.results_root),
            "workbook_path": str(args.workbook_path) if args.workbook_path is not None else None,
            "network_model_prefix": args.network_model_prefix,
            "switching_mode": switching_mode,
            "aggregates": aggregates,
            "reports": reports,
        }
        summary_dir = args.cache_root / "runs" / "comparisons" / "suite-summaries"
        summary_dir.mkdir(parents=True, exist_ok=True)
        summary_name = "__".join(
            [
                suite.name,
                _slugify(args.network_model_prefix),
                f"top{args.top_k}",
                "benchmark" if not args.no_benchmark else "nobenchmark",
            ]
        )
        summary_path = summary_dir / f"{summary_name}.json"
        summary_path.write_text(json.dumps(summary_payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        if args.json:
            _print_json(summary_payload | {"summary_path": str(summary_path)})
        else:
            lines = _summarize_comparison_suite(reports)
            lines.append(f"summary: {summary_path}")
            by_track = aggregates.get("by_track", {})
            if isinstance(by_track, dict) and by_track:
                lines.append("tracks:")
                for key in sorted(by_track):
                    bucket = by_track[key] if isinstance(by_track[key], dict) else {}
                    counts = bucket.get("counts", {}) if isinstance(bucket, dict) else {}
                    leaderboard = bucket.get("leaderboard", {}) if isinstance(bucket, dict) else {}
                    lines.append(
                        "  "
                        + " ".join(
                            [
                                f"{key}",
                                f"scenarios={bucket.get('scenario_count', '?')}",
                                f"top_refs={counts.get('top_reference', 0)}",
                                f"bench_refs={counts.get('benchmark_reference', 0)}",
                                f"baseline={counts.get('baseline', 0)}",
                                f"benchmark_gap_pct_sum={_format_metric(leaderboard.get('benchmark_gap_pct_to_winner', '?'))}",
                            ]
                        )
                    )
            _print_lines(lines)
        return 0

    if args.command == "compare-workbook":
        _, scenario = _locate_scenario(args.dataset_key, args.division, args.scenario_id, args.cache_root)
        validator_env = ensure_validator_environment(cache_root=args.cache_root)
        workbook_entries = load_event_workbook(args.workbook_path)
        leaderboard_rows = scenario_leaderboard(
            workbook_entries,
            model=scenario.network_model,
            scenario_id=scenario.scenario_id,
            switching_mode=args.switching_mode,
        )
        pop_payload = run_pop_validation(
            scenario,
            cache_root=args.cache_root,
            validator_env=validator_env,
        )
        try:
            baseline_payload = validate_baseline_solution(
                scenario,
                cache_root=args.cache_root,
                validator_env=validator_env,
            )
        except FileNotFoundError:
            baseline_payload = None

        benchmark = benchmark_entry(leaderboard_rows)
        payload = {
            "scenario": {
                "dataset_key": scenario.dataset_key,
                "division": scenario.division,
                "network_model": scenario.network_model,
                "scenario_id": scenario.scenario_id,
            },
            "leaderboard": [entry.__dict__ for entry in leaderboard_rows[: args.top]],
            "leaderboard_benchmark": benchmark.__dict__ if benchmark is not None else None,
            "pop_validation": pop_payload,
            "baseline_validation": baseline_payload,
        }
        if args.json:
            _print_json(payload)
        else:
            lines = [
                f"workbook: {args.workbook_path}",
                f"scenario: {scenario.dataset_key} {scenario.division} {scenario.network_model} scenario_{scenario.scenario_id:03d}",
            ]
            if leaderboard_rows:
                lines.append("leaderboard:")
                for entry in leaderboard_rows[: args.top]:
                    lines.append(
                        "  "
                        + " ".join(
                            [
                                f"rank={entry.rank if entry.rank is not None else '?'}",
                                f"team={entry.team}",
                                f"objective={_format_metric(entry.objective)}",
                                f"runtime={_format_metric(entry.runtime_seconds)}",
                                f"feas={entry.feasible if entry.feasible is not None else '?'}",
                                f"state={entry.state or '?'}",
                            ]
                        )
                    )
                if benchmark is not None:
                    lines.append(
                        "workbook_benchmark: "
                        + " ".join(
                            [
                                f"rank={benchmark.rank if benchmark.rank is not None else '?'}",
                                f"objective={_format_metric(benchmark.objective)}",
                                f"runtime={_format_metric(benchmark.runtime_seconds)}",
                            ]
                        )
                    )
            else:
                lines.append("leaderboard: no matching workbook rows found")
            lines.extend(_summarize_validation(pop_payload, label="pop-validation", solution_key="pop_solution_path"))
            if baseline_payload is not None:
                lines.extend(_summarize_validation(baseline_payload, label="baseline-validation", solution_key="solution_path"))
                pop_summary = pop_payload.get("validation_summary", {})
                base_summary = baseline_payload.get("validation_summary", {})
                if isinstance(pop_summary, dict) and isinstance(base_summary, dict):
                    pop_obj = pop_summary.get("obj")
                    base_obj = base_summary.get("obj")
                    if isinstance(pop_obj, (int, float)) and isinstance(base_obj, (int, float)):
                        lines.append(f"baseline_vs_pop_obj_delta: {base_obj - pop_obj:.6f}")
            else:
                lines.append("baseline-validation: no local baseline solution found to validate")
            lines.append(
                "note: workbook objective values come from the competition leaderboard; local validation artifacts here expose checker z-components and feasibility, which may not be directly leaderboard-comparable yet."
            )
            _print_lines(lines)
        return 0

    if args.command == "validate-pop":
        _, scenario = _locate_scenario(args.dataset_key, args.division, args.scenario_id, args.cache_root)
        payload = run_pop_validation(scenario, cache_root=args.cache_root)
        if args.json:
            _print_json(payload)
        else:
            _print_lines(_summarize_validation(payload, label="pop-validation", solution_key="pop_solution_path"))
        return 0

    if args.command == "solve-baseline":
        _, scenario = _locate_scenario(args.dataset_key, args.division, args.scenario_id, args.cache_root)
        policy = GoC3Policy(
            consumer_mode=args.consumer_mode,
            commitment_time_limit_secs=args.time_limit_secs,
            commitment_mip_rel_gap=getattr(args, "mip_gap", None),
            lp_solver=args.lp_solver,
            nlp_solver=args.ac_nlp_solver,
            run_pricing=args.with_pricing,
            ac_reconcile_mode=args.ac_reconcile,
        )
        payload = solve_baseline_scenario(scenario, cache_root=args.cache_root, policy=policy)
        if args.json:
            _print_json(payload)
        else:
            _print_lines(_summarize_baseline(payload))
        return 0

    if args.command == "validate-baseline":
        _, scenario = _locate_scenario(args.dataset_key, args.division, args.scenario_id, args.cache_root)
        payload = validate_baseline_solution(scenario, cache_root=args.cache_root)
        if args.json:
            _print_json(payload)
        else:
            _print_lines(_summarize_validation(payload, label="baseline-validation", solution_key="solution_path"))
        return 0

    if args.command == "compare-bus-residuals":
        _, scenario = _locate_scenario(args.dataset_key, args.division, args.scenario_id, args.cache_root)
        env = ensure_validator_environment(cache_root=args.cache_root)
        solution_path = (
            args.solution_path.resolve()
            if args.solution_path is not None
            else _resolve_baseline_solution_path(args.cache_root, scenario)
        )
        workdir = (
            args.cache_root
            / "runs"
            / "bus-residual-comparisons"
            / scenario.dataset_key
            / scenario.division
            / f"scenario_{scenario.scenario_id:03d}"
        )
        payload = compare_bus_residuals_with_official_tool(
            env,
            scenario.problem_path,
            lhs_solution_path=solution_path,
            rhs_solution_path=args.reference_solution_path.resolve(),
            workdir=workdir,
            top_k=args.top_k,
        )
        if args.json:
            _print_json(payload)
        else:
            _print_lines(_summarize_bus_residual_comparison(payload))
        return 0

    if args.command == "q-term-diff":
        from .q_term_diff import diff_bus_residuals, format_diff_report, write_diff_csv
        from .validator import extract_bus_residuals_with_official_tool

        _, scenario = _locate_scenario(args.dataset_key, args.division, args.scenario_id, args.cache_root)
        env = ensure_validator_environment(cache_root=args.cache_root)
        solution_path = (
            args.solution_path.resolve()
            if args.solution_path is not None
            else _sced_fixed_solution_path(args.cache_root, scenario)
        )
        if not solution_path.exists():
            raise SystemExit(f"surge solution not found: {solution_path}")
        reference_solution_path = args.reference_solution_path.resolve()
        if not reference_solution_path.exists():
            raise SystemExit(f"reference solution not found: {reference_solution_path}")
        workdir = (
            args.workdir.resolve()
            if args.workdir is not None
            else (
                args.cache_root
                / "runs"
                / "q-term-diffs"
                / scenario.dataset_key
                / scenario.division
                / f"scenario_{scenario.scenario_id:03d}"
            )
        )
        workdir.mkdir(parents=True, exist_ok=True)
        lhs_payload = extract_bus_residuals_with_official_tool(
            env,
            scenario.problem_path,
            solution_path=solution_path,
            workdir=workdir,
            label="surge",
        )
        rhs_payload = extract_bus_residuals_with_official_tool(
            env,
            scenario.problem_path,
            solution_path=reference_solution_path,
            workdir=workdir,
            label="reference",
        )
        report = diff_bus_residuals(
            Path(lhs_payload["report_path"]),
            Path(rhs_payload["report_path"]),
            top_k=args.top_k,
        )
        report["scenario"] = {
            "dataset_key": scenario.dataset_key,
            "division": scenario.division,
            "scenario_id": scenario.scenario_id,
            "problem_path": str(scenario.problem_path),
        }
        report["lhs_solution_path"] = str(solution_path)
        report["rhs_solution_path"] = str(reference_solution_path)
        report_path = workdir / "q-term-diff.json"
        report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        write_diff_csv(report, workdir / "q-term-diff.csv")
        report["report_path"] = str(report_path)
        if args.json:
            _print_json(report)
        else:
            _print_lines(format_diff_report(report, top_k=args.top_k))
            print(f"report: {report_path}")
        return 0

    if args.command == "run-suite":
        dataset_manifest = load_dataset_manifest()
        suite_manifest = load_suite_manifest()
        suite = suite_manifest.by_name().get(args.suite_name)
        if suite is None:
            raise SystemExit(f"unknown suite: {args.suite_name}")
        policy = GoC3Policy(
            consumer_mode=args.consumer_mode,
            commitment_time_limit_secs=args.time_limit_secs,
            lp_solver=args.lp_solver,
            nlp_solver=args.ac_nlp_solver,
            run_pricing=args.with_pricing,
            ac_reconcile_mode=args.ac_reconcile,
        )
        reports = run_suite(
            suite,
            dataset_manifest,
            cache_root=args.cache_root,
            policy=policy,
            with_validator=args.with_validator,
            network_model_prefix=args.network_model_prefix,
        )
        if args.json:
            _print_json(reports)
        else:
            _print_lines(_summarize_suite(reports))
        return 0

    if args.command == "extract-commitment":
        from .commitment import extract_reference_schedule

        schedule = extract_reference_schedule(args.solution_path.resolve(), label=args.label)
        payload = {
            "source_path": schedule.source_path,
            "source_label": schedule.source_label,
            "periods": schedule.periods,
            "device_count": len(schedule.devices),
            "hvdc_link_count": len(schedule.hvdc_links),
            "branch_count": len(schedule.branches),
            "bus_count": len(schedule.buses),
            "committed_device_periods": sum(
                sum(1 for p in dev.on_status if p)
                for dev in schedule.devices.values()
            ),
        }
        if args.json:
            _print_json(payload)
        else:
            _print_lines([f"{k}: {v}" for k, v in payload.items()])
        return 0

    if args.command == "solve-sced-fixed":
        from .commitment import extract_reference_schedule
        from .runner import solve_sced_fixed

        _, scenario = _locate_scenario(args.dataset_key, args.division, args.scenario_id, args.cache_root)
        policy = GoC3Policy(
            lp_solver=args.lp_solver,
            nlp_solver=args.ac_nlp_solver,
            run_pricing=args.with_pricing,
            ac_reconcile_mode=args.ac_reconcile,
        )
        commitment_schedule = None
        if args.commitment_solution_path:
            commitment_schedule = extract_reference_schedule(
                args.commitment_solution_path.resolve(), label="explicit",
            )
        report = solve_sced_fixed(
            scenario,
            cache_root=args.cache_root,
            policy=policy,
            commitment_source=args.commitment_source,
            commitment_schedule=commitment_schedule,
            results_root=args.results_root,
            switching_mode=args.switching_mode,
        )
        if args.json:
            _print_json(report)
        else:
            lines = [
                f"status: {report['status']}",
                f"commitment: {report.get('commitment_label', 'unknown')}",
                f"solve_seconds: {report.get('solve_seconds', 0):.2f}",
            ]
            summary = report.get("dispatch_summary", {})
            if summary:
                lines.append(f"total_cost: {summary.get('total_cost', 'N/A')}")
            if report.get("solution_path"):
                lines.append(f"solution: {report['solution_path']}")
            if report.get("error"):
                lines.append(f"error: {report['error']}")
            _print_lines(lines)
        return 0

    if args.command == "detail-compare":
        from .detail_compare import compare_solutions, comparison_to_dict, summarize_comparison

        lhs_val = None
        rhs_val = None
        if args.lhs_validator_summary:
            lhs_val = json.loads(args.lhs_validator_summary.resolve().read_text(encoding="utf-8"))
        if args.rhs_validator_summary:
            rhs_val = json.loads(args.rhs_validator_summary.resolve().read_text(encoding="utf-8"))
        comparison = compare_solutions(
            args.lhs_solution.resolve(),
            args.rhs_solution.resolve(),
            lhs_label=args.lhs_label,
            rhs_label=args.rhs_label,
            base_mva=args.base_mva,
            lhs_validator_summary=lhs_val,
            rhs_validator_summary=rhs_val,
        )
        if args.json:
            _print_json(comparison_to_dict(comparison))
        else:
            _print_lines(summarize_comparison(comparison))
        return 0

    if args.command == "ledger-update":
        from .ledger import update_ledger_from_run_reports, format_ledger_status

        run_dir = args.cache_root / "runs" / ("sced-fixed" if args.mode == "sced_fixed" else "baseline")
        run_reports = []
        if run_dir.exists():
            for report_path in sorted(run_dir.rglob("run-report.json")):
                report = json.loads(report_path.read_text(encoding="utf-8"))
                if report.get("status") == "ok":
                    run_reports.append(report)
        entries, deltas = update_ledger_from_run_reports(args.mode, run_reports, notes=args.notes)
        if args.json:
            _print_json({"entries": len(entries), "deltas": [d.__dict__ for d in deltas]})
        else:
            _print_lines(format_ledger_status(deltas) or [f"Recorded {len(entries)} entries"])
        return 0

    if args.command == "ledger-status":
        from .ledger import load_ledger

        ledger = load_ledger(args.mode)
        if args.json:
            _print_json(ledger)
        else:
            if not ledger:
                _print_lines([f"No entries in {args.mode} ledger"])
            else:
                lines = [f"{args.mode} ledger ({len(ledger)} entries):"]
                for key, entry in sorted(ledger.items()):
                    obj = entry.get("our_obj")
                    gap = entry.get("gap_pct")
                    feas = entry.get("our_feas")
                    obj_str = f"{obj:.2f}" if isinstance(obj, (int, float)) else "N/A"
                    gap_str = f"{gap:+.3f}%" if isinstance(gap, (int, float)) else ""
                    feas_str = "feas" if feas == 1 else "infeas" if feas == 0 else ""
                    lines.append(f"  {key}: obj={obj_str} {gap_str} {feas_str}".rstrip())
                _print_lines(lines)
        return 0

    raise SystemExit(f"unsupported command: {args.command}")


if __name__ == "__main__":
    raise SystemExit(main())
