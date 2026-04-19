# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Template smoke test — the copy-me skeleton at ``markets/_template/``
must produce a working market out of the box.

This guards against contract drift. If we change the ``MarketProblem``
protocol, the ``MarketConfig`` defaults, or ``run_market_solve``'s
signature, this test catches it by actually running the template's
solve end-to-end.
"""

from __future__ import annotations

from pathlib import Path

from surge.market import MarketProblem

from markets._template import Policy, Problem, solve, default_config


def test_template_problem_conforms_to_market_problem_protocol() -> None:
    """The template's Problem must satisfy :class:`MarketProblem`."""
    problem = Problem()
    assert isinstance(problem, MarketProblem)


def test_template_default_config_builds() -> None:
    """``default_config`` returns a valid :class:`MarketConfig`."""
    cfg = default_config()
    assert cfg.penalties is not None
    # ``apply_defaults_to_request`` should be idempotent-compatible
    # with an empty request.
    resolved = cfg.apply_defaults_to_request({})
    assert "market" in resolved
    assert "network" in resolved


def test_template_solve_runs(tmp_path: Path) -> None:
    """End-to-end: template's solve produces a valid run report + dispatch result."""
    problem = Problem(
        period_durations_hours=[1.0, 1.0],
        load_mw_by_period=[50.0, 60.0],
    )
    report = solve(problem, tmp_path, policy=Policy())

    assert report["status"] == "ok", report.get("error")
    assert (tmp_path / "run-report.json").exists()
    assert (tmp_path / "dispatch-result.json").exists()

    extras = report.get("extras") or {}
    assert extras.get("periods") == 2
    assert extras.get("total_cost") is not None
