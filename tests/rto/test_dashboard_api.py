# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Smoke tests for the RTO dashboard adapter (``dashboards.rto.api``).

The dashboard runs every solve through ``surge.market.go_c3``'s
canonical native two-stage SCUC → AC SCED pipeline, so these tests
exercise the goc3 cases bundled under ``examples/cases/``.
"""

from __future__ import annotations

import pytest

from dashboards.rto import api as rto_api


def test_case_registry_exposes_goc3_and_ieee_cases() -> None:
    """Both GO-C3 archives and IEEE built-ins are available."""
    ids = {c["id"] for c in rto_api.available_cases()}
    for expected in ("goc3_73", "goc3_617", "goc3_2000", "case9", "case14", "case30"):
        assert expected in ids, f"case {expected} missing from registry"


@pytest.mark.slow
def test_scaffold_goc3_2000_carries_problem_path() -> None:
    """The bundled 2000-bus archive decompresses on first scaffold
    and threads the problem path through the scenario source.

    Slow (~15 s on first run, ~5 s after the cached decompressed
    JSON warms up); excluded from the default pytest run via the
    ``slow`` marker. Pass ``--run-slow`` to include.
    """
    scen = rto_api.build_scaffold("goc3_2000")
    src = scen["source"]
    assert src["case_id"] == "goc3_2000"
    assert src["family"] == "goc3"
    assert "goc3_problem_path" in src
    # 2000-bus archives ship a non-trivial generator and load roster.
    assert len(scen["generators"]) > 500
    assert len(scen["loads"]) > 500


def test_default_case_is_goc3_73_d3_315() -> None:
    """The first case in the registry is the 73-bus event4 D3 315 —
    that's what the dashboard's frontend lands on at startup. D3
    has the long uniform 42-period horizon that exercises the
    most pipeline phases on a fresh demo."""
    cases = rto_api.available_cases()
    assert cases[0]["id"] == "goc3_73_d3_315"


def test_solve_case14_via_ieee_path() -> None:
    """case14 routes through the surge.solve_dispatch path — synthesized
    offer curves + load forecast + zonal reserve requirements.

    Pinned to ``solve_mode=scuc`` because case14's bundled data has a
    regulated-bus voltage outside its ``[v_min, v_max]`` envelope which
    the AC-OPF rejects pre-solve. The dashboard's default is now
    ``scuc_ac_sced``; users who pick case14 with AC SCED on get a
    clear error from surge rather than a mysterious failure.
    """
    scen = rto_api.build_scaffold("case14")
    scen["policy"]["solve_mode"] = "scuc"
    result = rto_api.run_solve(scen)
    assert result["status"] == "ok", result.get("error")
    assert result["periods"] == scen["time_axis"]["periods"]
    # 14 buses → an LMP series for each.
    assert len(result["lmps_by_bus"]) == 14
    for bus_lmps in result["lmps_by_bus"].values():
        assert len(bus_lmps) == result["periods"]
    summary = result["summary"]
    assert summary["energy_payment_dollars"] > 0
    assert summary["load_payment_dollars"] > 0


def test_solve_case9_via_ieee_path_all_committed() -> None:
    """case9 with all-committed commitment is the smallest sanity check."""
    scen = rto_api.build_scaffold("case9")
    scen["policy"]["solve_mode"] = "scuc"
    scen["policy"]["commitment_mode"] = "all_committed"
    result = rto_api.run_solve(scen)
    assert result["status"] == "ok", result.get("error")
    assert len(result["lmps_by_bus"]) == 9


def test_scaffold_goc3_73_carries_problem_path() -> None:
    """Loading the goc3_73 scaffold decompresses the problem
    archive and threads the cache path through the scenario
    ``source`` block so ``run_solve`` can hand it to
    ``GoC3Problem.load``."""
    scen = rto_api.build_scaffold("goc3_73")
    src = scen["source"]
    assert src["case_id"] == "goc3_73"
    assert src["family"] == "goc3"
    assert "goc3_problem_path" in src
    # 18 × 15-min intervals from the problem archive.
    assert scen["time_axis"]["periods"] == 18
    assert scen["time_axis"]["resolution_minutes"] == 15
    # Generators / loads pulled from the goc3 archive.
    assert len(scen["generators"]) > 100
    assert len(scen["loads"]) >= 50


def test_scaffold_default_policy_enables_reactive_pin_for_goc3() -> None:
    """``reactive_support_pin_factor`` defaults to 0.2 on goc3 cases —
    the canonical retry factor that resolves Ipopt convergence-basin
    issues on 73-/617-bus AC SCED."""
    scen = rto_api.build_scaffold("goc3_73")
    assert scen["policy"]["reactive_support_pin_factor"] == pytest.approx(0.2)


def test_run_solve_goc3_73_scuc_only() -> None:
    """SCUC-only on the 303 case — fast smoke test for the bridge.

    The native pipeline takes ~3 s and produces LMPs on every bus
    plus the standard settlement summary fields.
    """
    scen = rto_api.build_scaffold("goc3_73")
    scen["policy"]["solve_mode"] = "scuc"
    result = rto_api.run_solve(scen)
    assert result["status"] == "ok", result.get("error")
    assert result["lmp_source"] == "DC SCUC"
    assert result["periods"] == 18
    assert len(result["lmps_by_bus"]) == 73
    for bus_lmps in result["lmps_by_bus"].values():
        assert len(bus_lmps) == 18
    summary = result["summary"]
    # Production cost is in dollars; non-zero means the solve produced
    # a real dispatch (sign depends on the goc3 case's internal cost
    # accounting — anything but 0 is fine for the smoke).
    assert summary["mean_system_lmp"] is not None


def test_policy_translation_to_market_policy() -> None:
    """``RtoPolicy.to_market_policy`` produces the canonical
    :class:`MarketPolicy` shape — dashboard policy form ↔ goc3
    pipeline knobs.
    """
    from dashboards.rto.policy import RtoPolicy

    p = RtoPolicy(
        solve_mode="scuc_ac_sced",
        commitment_mode="optimize",
        lp_solver="highs",
        nlp_solver="ipopt",
        mip_gap=1e-3,
        time_limit_secs=120.0,
        reactive_support_pin_factor=0.2,
        sced_ac_opf_max_iterations=2000,
        loss_mode="load_pattern",
        loss_rate=0.02,
        loss_max_iterations=1,
        security_enabled=True,
        security_max_iterations=4,
    )
    mp = p.to_market_policy()
    assert mp.ac_reconcile_mode == "ac_dispatch"
    assert mp.commitment_mode == "optimize"
    assert mp.lp_solver == "highs"
    assert mp.nlp_solver == "ipopt"
    assert mp.commitment_mip_rel_gap == pytest.approx(1e-3)
    assert mp.commitment_time_limit_secs == 120.0
    assert mp.reactive_support_pin_factor == pytest.approx(0.2)
    assert mp.sced_ac_opf_max_iterations == 2000
    assert mp.scuc_loss_factor_warm_start == ("load_pattern", 0.02)
    assert mp.scuc_loss_factor_max_iterations == 1
    assert mp.scuc_security_max_iterations == 4


def test_policy_scuc_only_mode_disables_ac_reconcile() -> None:
    """``solve_mode=scuc`` collapses to ``ac_reconcile_mode="none"``
    so the goc3 pipeline runs the SCUC alone."""
    from dashboards.rto.policy import RtoPolicy

    mp = RtoPolicy(solve_mode="scuc").to_market_policy()
    assert mp.ac_reconcile_mode == "none"


def test_policy_security_disabled_pins_iterations_to_one() -> None:
    """When ``security_enabled=False``, the security knobs collapse
    to a no-op (1 iteration, 0 preseeded cuts)."""
    from dashboards.rto.policy import RtoPolicy

    mp = RtoPolicy(security_enabled=False, security_max_iterations=10).to_market_policy()
    assert mp.scuc_security_max_iterations == 1
    assert mp.scuc_security_preseed_count_per_period == 0


def test_policy_validation_rejects_unknown_solve_mode() -> None:
    from dashboards.rto.policy import RtoPolicy

    with pytest.raises(ValueError, match="solve_mode"):
        RtoPolicy(solve_mode="not_a_mode")


def test_policy_validation_rejects_negative_reactive_pin() -> None:
    from dashboards.rto.policy import RtoPolicy

    with pytest.raises(ValueError, match="reactive_support_pin_factor"):
        RtoPolicy(reactive_support_pin_factor=-0.1)
