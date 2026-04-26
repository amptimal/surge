# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Canonical GO C3 market solve.

:func:`solve` takes a :class:`GoC3Problem` and a :class:`GoC3Policy`,
runs the canonical Rust workflow (SCUC → AC-SCED), and writes:

* ``{workdir}/solution.json``         — exported GO C3 solution
* ``{workdir}/workflow-result.json``  — stage-by-stage Rust workflow trace
* ``{workdir}/run-report.json``       — this function's status / timing report
* ``{workdir}/solve.log``             — Python + (optional) Rust / solver log

Nothing else. Archive rotation, downstream scoring (violation report,
load-value report), validator integration, retry heuristics, and
per-scenario path conventions are not market concerns — wrap
:func:`solve` in your own harness for those.
"""

from __future__ import annotations

import json
import logging
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from surge.market import run_market_solve

from .policy import GoC3Policy
from .problem import GoC3Problem

logger = logging.getLogger("go_c3.solve")


_SCUC_MIP_STAT_KEYS = (
    "n_vars",
    "n_bin_vars",
    "pre_fixed_bin_vars",
    "n_int_vars",
    "n_rows",
    "n_nonzeros",
    "node_count",
    "iter_count",
    "objective",
    "objective_bound",
    "final_gap",
    "final_time_secs",
    "time_limit_secs",
    "terminated_by",
)

_AC_OPF_STAT_KEYS = (
    "period_idx",
    "period_label",
    "solver_name",
    "solve_time_secs",
    "attempt_label",
    "n_vars",
    "n_constraints",
    "jac_nnz",
    "hess_nnz",
    "status_code",
    "status_label",
    "iterations",
    "objective",
    "final_primal_inf",
    "final_dual_inf",
    "final_mu",
    "converged",
)


@dataclass
class _GoC3Outcome:
    """What the build callable returns, success or partial-failure."""

    workflow_result: dict | None
    exported: dict | None
    step_timings: dict[str, float]
    error: str | None


def _extract_scuc_mip_stats(workflow_result: dict | None) -> dict | None:
    if not workflow_result:
        return None
    for stage in workflow_result.get("stages") or []:
        if stage.get("stage_id") != "scuc":
            continue
        solution = stage.get("solution") or {}
        diagnostics = solution.get("diagnostics") or {}
        trace = diagnostics.get("commitment_mip_trace")
        if not trace:
            return None
        return {key: trace.get(key) for key in _SCUC_MIP_STAT_KEYS if key in trace}
    return None


def _extract_scuc_security_report(workflow_result: dict | None) -> dict | None:
    """Per-iteration security-loop breakdown emitted by surge-dispatch.

    Returns ``None`` when the SCUC stage didn't attach security metadata
    (e.g. `security` was not configured, or an early-failure path). The
    returned dict has stable keys suitable for a run-report: ``setup_timings_secs``,
    ``per_iteration`` (list, each with ``inner_solve_secs`` / ``screen_secs`` /
    ``cut_build_secs`` / ``inner_mip_trace`` restricted to `_SCUC_MIP_STAT_KEYS`),
    and the aggregate counters (``iterations``, ``n_cuts``, ``converged`` …).
    """
    if not workflow_result:
        return None
    for stage in workflow_result.get("stages") or []:
        if stage.get("stage_id") != "scuc":
            continue
        solution = stage.get("solution") or {}
        diagnostics = solution.get("diagnostics") or {}
        security = diagnostics.get("security")
        if not security:
            return None
        per_iter_raw = security.get("per_iteration") or []
        per_iteration: list[dict[str, Any]] = []
        for entry in per_iter_raw:
            trace = entry.get("inner_mip_trace")
            trimmed_trace = (
                {key: trace.get(key) for key in _SCUC_MIP_STAT_KEYS if key in trace}
                if trace
                else None
            )
            per_iteration.append(
                {
                    "iter": entry.get("iter"),
                    "net_clone_secs": entry.get("net_clone_secs"),
                    "inner_solve_secs": entry.get("inner_solve_secs"),
                    "repair_theta_secs": entry.get("repair_theta_secs", 0.0),
                    "screen_secs": entry.get("screen_secs"),
                    "cut_build_secs": entry.get("cut_build_secs", 0.0),
                    "new_cuts": entry.get("new_cuts"),
                    "n_branch_violations": entry.get("n_branch_violations"),
                    "n_hvdc_violations": entry.get("n_hvdc_violations"),
                    "max_branch_violation_pu": entry.get("max_branch_violation_pu"),
                    "max_hvdc_violation_pu": entry.get("max_hvdc_violation_pu"),
                    "inner_mip_trace": trimmed_trace,
                    # System-row loss telemetry (None on `Static` mode or
                    # per-bus path). Surfaces realized vs blended total
                    # losses per period and per-bus LF distribution stats
                    # so we can A/B `scuc_loss_treatment` settings against
                    # AC-SCED slack penalty headroom.
                    "sys_row_loss_telemetry": entry.get("sys_row_loss_telemetry"),
                }
            )
        return {
            "iterations": security.get("iterations"),
            "n_cuts": security.get("n_cuts"),
            "converged": security.get("converged"),
            "last_branch_violations": security.get("last_branch_violations"),
            "last_hvdc_violations": security.get("last_hvdc_violations"),
            "max_branch_violation_pu": security.get("max_branch_violation_pu"),
            "max_hvdc_violation_pu": security.get("max_hvdc_violation_pu"),
            "n_preseed_cuts": security.get("n_preseed_cuts", 0),
            "setup_timings_secs": security.get("setup_timings_secs"),
            "per_iteration": per_iteration,
        }
    return None


def _extract_ac_opf_stats(workflow_result: dict | None) -> list[dict] | None:
    """Return per-period AC-OPF solver stats from the AC SCED stage.

    Walks ``workflow_result["stages"]`` looking for a stage whose
    ``solution.diagnostics.ac_opf_stats`` is populated. Returns a list
    of dicts (one per period) restricted to the known keys, or ``None``
    when the workflow ran no AC SCED stage or the backend didn't emit a
    trace (non-Ipopt NLP backends return ``None`` traces today).
    """
    if not workflow_result:
        return None
    for stage in workflow_result.get("stages") or []:
        solution = stage.get("solution") or {}
        diagnostics = solution.get("diagnostics") or {}
        stats = diagnostics.get("ac_opf_stats")
        if not stats:
            continue
        return [
            {key: entry.get(key) for key in _AC_OPF_STAT_KEYS if key in entry}
            for entry in stats
        ]
    return None


def _run_pass(
    *,
    problem: GoC3Problem,
    policy: GoC3Policy,
    go_c3_native,
) -> _GoC3Outcome:
    """One pass of the canonical workflow.

    Returns the outcome with either a populated workflow/exported pair
    (on success) or an ``error`` message (on failure).
    """
    timings: dict[str, float] = {}
    policy_dict = policy.to_dict()
    try:
        logger.info("Native solve: building canonical workflow")
        t0 = time.perf_counter()
        wf = go_c3_native.build_workflow(problem, policy_dict)
        timings["build_workflow"] = time.perf_counter() - t0
        logger.info("Native solve: workflow stages %s", wf.stages())

        t0 = time.perf_counter()
        stop_after = "scuc" if policy.scuc_only else None
        wr = go_c3_native.solve_workflow(
            wf,
            lp_solver=policy_dict.get("lp_solver"),
            nlp_solver=policy_dict.get("nlp_solver"),
            stop_after_stage=stop_after,
        )
        timings["solve_workflow"] = time.perf_counter() - t0

        stage_err = wr.get("error") if isinstance(wr, dict) else None
        if stage_err is not None:
            msg = (
                f"stage '{stage_err['stage_id']}' ({stage_err['role']}) "
                f"failed: {stage_err['error']}"
            )
            logger.warning(
                "Native solve failed in stage '%s' (%s): %s",
                stage_err["stage_id"],
                stage_err["role"],
                stage_err["error"],
            )
            return _GoC3Outcome(
                workflow_result=wr,
                exported=None,
                step_timings=timings,
                error=msg,
            )

        if stop_after == "scuc":
            final_solution = wr["stages"][0]["solution"]
            dc_res = None
        else:
            stage_idx = -1 if policy.ac_reconcile_mode == "ac_dispatch" else 0
            final_solution = wr["stages"][stage_idx]["solution"]
            dc_res = None
            if policy.ac_reconcile_mode == "ac_dispatch" and len(wr["stages"]) > 1:
                dc_res = wr["stages"][0]["solution"]

        logger.info("Native solve: exporting solution")
        t0 = time.perf_counter()
        exp = go_c3_native.export(
            problem,
            final_solution,
            dc_reserve_source=dc_res,
            allow_consumer_reserve_shedding=policy.allow_ac_consumer_reserve_shedding,
        )
        timings["export"] = time.perf_counter() - t0
        logger.info(
            "Native solve: step times (s) build_workflow=%.2f "
            "solve_workflow=%.2f export=%.2f",
            timings["build_workflow"],
            timings["solve_workflow"],
            timings["export"],
        )
        return _GoC3Outcome(
            workflow_result=wr,
            exported=exp,
            step_timings=timings,
            error=None,
        )
    except Exception as exc:  # noqa: BLE001
        logger.warning("Native solve failed: %s", exc)
        return _GoC3Outcome(
            workflow_result=None,
            exported=None,
            step_timings=timings,
            error=str(exc),
        )


def solve(
    problem: GoC3Problem,
    workdir: Path,
    *,
    policy: GoC3Policy | None = None,
    label: str | None = None,
) -> dict[str, Any]:
    """Solve a GO C3 scenario and write the market artifacts to *workdir*.

    Runs the canonical Rust workflow (SCUC → AC-SCED) via
    :mod:`surge.market.go_c3`. One pass, no retries — operational
    workarounds (e.g. retry with a reactive-support pin) live in
    :mod:`benchmarks.go_c3.runner`.

    Writes four files to *workdir*:

    * ``solution.json`` — the GO C3 competition solution (exported).
    * ``workflow-result.json`` — per-stage Rust workflow trace.
    * ``run-report.json`` — this function's status, timing, policy.
    * ``solve.log`` — timestamped log (Python logs; with Rust / solver
      console when ``policy.capture_solver_log`` is True).

    Returns the run-report dict. Callers that need scoring or
    dashboard artifacts should wrap this function.
    """
    import surge.market.go_c3 as go_c3_native

    policy = policy or GoC3Policy()

    def build() -> _GoC3Outcome:
        return _run_pass(problem=problem, policy=policy, go_c3_native=go_c3_native)

    def artifacts_from(outcome: _GoC3Outcome, workdir: Path) -> dict[str, Path | None]:
        artifacts: dict[str, Path | None] = {"solve_log": workdir / "solve.log"}
        if outcome.exported is not None:
            solution_path = workdir / "solution.json"
            solution_path.write_text(json.dumps(outcome.exported) + "\n", encoding="utf-8")
            artifacts["solution"] = solution_path
        if outcome.workflow_result is not None:
            wf_path = workdir / "workflow-result.json"
            wf_path.write_text(
                json.dumps(outcome.workflow_result) + "\n", encoding="utf-8"
            )
            artifacts["workflow_result"] = wf_path
        return artifacts

    def extras_from(outcome: _GoC3Outcome) -> dict[str, Any]:
        wr = outcome.workflow_result
        extras: dict[str, Any] = {
            "problem_path": str(problem.path),
            "solve_mode": "native_workflow",
            "step_timings_secs": outcome.step_timings,
            "workflow_stages": (
                [stage["stage_id"] for stage in wr["stages"]]
                if wr is not None
                else None
            ),
            "scuc_mip_stats": _extract_scuc_mip_stats(wr),
            "scuc_security_report": _extract_scuc_security_report(wr),
            "ac_opf_stats": _extract_ac_opf_stats(wr),
        }
        if wr is not None:
            extras["stage_timings_secs"] = [
                {"stage_id": stage["stage_id"], "timings": stage.get("timings")}
                for stage in wr["stages"]
            ]
            stage_err = wr.get("error")
            if stage_err is not None:
                extras["failed_stage_id"] = stage_err["stage_id"]
                extras["failed_stage_role"] = stage_err["role"]
                extras["failed_stage_error"] = stage_err["error"]
        return extras

    return run_market_solve(
        workdir,
        policy=policy,
        label=label,
        logger_name="go_c3",
        problem_path=problem.path,
        build=build,
        artifacts_from=artifacts_from,
        extras_from=extras_from,
        error_from=lambda outcome: outcome.error,
    )


__all__ = ["solve"]
