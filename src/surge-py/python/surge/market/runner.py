# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Standard solve wrapper used by every market's ``solve()`` entry point.

Wraps the canonical scaffolding — ``SolveLogger`` context, wall-clock
timing, ``status``/``error`` capture on exception, and
``write_run_report`` — so a market's ``solve.py`` only has to express
the unique build-and-solve callable and the per-market extras /
artifacts.

Typical usage::

    from surge.market import run_market_solve

    def solve(problem, workdir, *, policy=None, label=None):
        policy = policy or MyPolicy()

        def build():
            network = problem.build_network()
            request = problem.build_request(policy=policy)
            return surge.solve_dispatch(network, request, lp_solver=policy.lp_solver)

        return run_market_solve(
            workdir,
            policy=policy,
            label=label,
            logger_name="markets.my_market",
            build=build,
            extras_from=lambda result: {"total_cost": result.summary.get("total_cost")},
        )
"""

from __future__ import annotations

import logging
import time
from pathlib import Path
from typing import Any, Callable, Mapping

import surge

from .logging import SolveLogger
from .report import RunReport, write_run_report


def run_market_solve(
    workdir: Path,
    *,
    policy: Any,
    build: Callable[[], Any],
    logger_name: str,
    label: str | None = None,
    problem_path: Path | None = None,
    log_level: str | None = None,
    capture_solver_log: bool | None = None,
    extras_from: Callable[[Any], Mapping[str, Any]] | None = None,
    artifacts_from: Callable[[Any, Path], Mapping[str, Path | None]] | None = None,
    error_from: Callable[[Any], str | None] | None = None,
    extra_extras: Mapping[str, Any] | None = None,
    extra_artifacts: Mapping[str, Path | None] | None = None,
    surge_module: Any = surge,
) -> RunReport:
    """Run a market solve with the canonical scaffolding.

    Parameters
    ----------
    workdir
        Directory artifacts are written into. Created if missing.
    policy
        The market's policy dataclass. Stored under ``"policy"`` in
        the run report.
    build
        Callable returning the result of the solve (typically a
        ``DispatchResult`` or a workflow result). Any exception raised
        is caught, logged, and surfaced as ``status="error"`` with the
        exception message in ``error``.
    logger_name
        Logger hierarchy attached for the duration of the solve.
    label, problem_path
        Optional metadata threaded through to ``SolveLogger`` and the
        run report.
    log_level, capture_solver_log
        Optional overrides; default to ``policy.log_level`` /
        ``policy.capture_solver_log`` if those attributes exist,
        otherwise ``"info"`` / ``False``.
    extras_from, artifacts_from
        Per-market hooks invoked on the solve result. ``extras_from``
        returns the ``extras`` mapping; ``artifacts_from`` returns the
        ``artifacts`` mapping (and may write files into ``workdir``
        as a side effect). Both receive the build result. ``artifacts_from``
        also receives ``workdir``. Always invoked when ``build`` returned
        a non-None result, including when ``error_from`` flags an error
        — hooks decide for themselves what to emit on partial state.
    error_from
        Optional callable that inspects the build result and returns
        an error message (or ``None`` if the solve succeeded). Lets a
        market signal a soft failure — one where ``build`` returned
        partial state instead of raising. Status flips to ``"error"``
        but ``extras_from`` / ``artifacts_from`` still run.
    extra_extras, extra_artifacts
        Additional entries merged into the corresponding mapping. Useful
        for fields known before the solve (e.g. ``problem_path``).
    surge_module
        Override the imported ``surge`` module — defaults to the live one.

    Returns
    -------
    The dict written to ``{workdir}/run-report.json``.
    """
    workdir = Path(workdir)
    workdir.mkdir(parents=True, exist_ok=True)

    resolved_log_level = log_level or getattr(policy, "log_level", "info")
    resolved_capture = (
        capture_solver_log
        if capture_solver_log is not None
        else bool(getattr(policy, "capture_solver_log", False))
    )

    log = logging.getLogger(logger_name)
    result: Any = None
    status = "ok"
    error: str | None = None

    with SolveLogger(
        workdir,
        logger_name=logger_name,
        policy=policy,
        label=label,
        problem_path=problem_path,
        surge_module=surge_module,
        log_level=resolved_log_level,
        capture_solver_log=resolved_capture,
    ):
        started_at = time.perf_counter()
        try:
            result = build()
        except Exception as exc:  # noqa: BLE001
            log.exception("solve failed: %s", exc)
            status = "error"
            error = str(exc)
        elapsed_secs = time.perf_counter() - started_at

    extras: dict[str, Any] = {}
    artifacts: dict[str, Path | None] = {}
    if extra_extras:
        extras.update(extra_extras)
    if extra_artifacts:
        artifacts.update(extra_artifacts)

    if status == "ok" and result is not None and error_from is not None:
        soft_error = error_from(result)
        if soft_error is not None:
            status = "error"
            error = soft_error

    if result is not None:
        if artifacts_from is not None:
            artifacts.update(artifacts_from(result, workdir))
        if extras_from is not None:
            extras.update(extras_from(result))

    return write_run_report(
        workdir,
        status=status,
        elapsed_secs=elapsed_secs,
        policy=policy,
        label=label,
        error=error,
        artifacts=artifacts,
        extras=extras,
    )


__all__ = ["run_market_solve"]
