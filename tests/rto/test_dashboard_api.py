# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Smoke tests for the RTO dashboard adapter (``dashboards.rto.api``)."""

from __future__ import annotations

import pytest

from dashboards.rto import api as rto_api


def test_case_registry_exposes_expected_cases() -> None:
    ids = {c["id"] for c in rto_api.available_cases()}
    # Built-in IEEE set + the two GO-C3 references the dashboard advertises.
    for expected in ("case9", "case14", "case30", "case57", "case118", "goc3_73"):
        assert expected in ids, f"case {expected} missing from registry"


def test_scaffold_case14_has_synthesized_defaults() -> None:
    scen = rto_api.build_scaffold("case14")
    assert scen["source"]["case_id"] == "case14"
    # 24-period horizon at 60-min resolution by default
    assert scen["time_axis"]["periods"] == 24
    assert scen["time_axis"]["resolution_minutes"] == 60
    # All four reserve products should be present with sane defaults
    assert set(scen["reserves_config"]["products"].keys()) == {"reg_up", "reg_down", "syn", "nsyn"}
    # Generators populated with Pmax > 0
    assert scen["generators"]
    assert any(g["pmax_mw"] > 0 for g in scen["generators"])
    # Loads populated from the case (case14 has loads at ~11 buses)
    assert len(scen["loads"]) >= 10
    # Policy default is "optimize" — the MIP path
    assert scen["policy"]["commitment_mode"] == "optimize"


def test_solve_case14_returns_settlement() -> None:
    scen = rto_api.build_scaffold("case14")
    result = rto_api.run_solve(scen)
    assert result["status"] == "ok", result.get("error")
    summary = result["summary"]
    # Case14 with positive loads should produce positive energy payment
    assert summary["energy_payment_dollars"] > 0
    assert summary["load_payment_dollars"] > 0
    assert summary["mean_system_lmp"] >= 0
    # Per-period LMPs for every bus
    assert len(result["lmps_by_bus"]) == 14
    for bus_series in result["lmps_by_bus"].values():
        assert len(bus_series) == scen["time_axis"]["periods"]
    # Four reserve products should show up in the settlement (default 5/5/3/2 %)
    product_ids = {r["product_id"] for r in result["reserve_awards"]}
    assert product_ids >= {"reg_up", "reg_down", "syn", "nsyn"}


def test_scenario_roundtrip_solves_identically(tmp_path) -> None:
    """Serialize a scenario to JSON and back — result should match."""
    import json
    scen = rto_api.build_scaffold("case9")
    first = rto_api.run_solve(scen)
    # Round-trip
    path = tmp_path / "scen.json"
    path.write_text(json.dumps(scen))
    reloaded = json.loads(path.read_text())
    second = rto_api.run_solve(reloaded)
    assert first["status"] == "ok" and second["status"] == "ok"
    assert first["summary"]["production_cost_dollars"] == pytest.approx(
        second["summary"]["production_cost_dollars"], rel=1e-6
    )


def test_reserve_requirement_scaled_by_percent() -> None:
    """Reserve requirement = %-of-peak × peak load across periods."""
    scen = rto_api.build_scaffold("case14")
    # Tweak reg_up to 10% — should roughly double from the 5% default
    scen["reserves_config"]["products"]["reg_up"] = {"percent_of_peak": 10.0, "absolute_mw": None}
    scen["reserves_config"]["products"]["reg_down"] = {"percent_of_peak": 0.0, "absolute_mw": None}
    scen["reserves_config"]["products"]["syn"] = {"percent_of_peak": 0.0, "absolute_mw": None}
    scen["reserves_config"]["products"]["nsyn"] = {"percent_of_peak": 0.0, "absolute_mw": None}
    result = rto_api.run_solve(scen)
    assert result["status"] == "ok"
    # Only reg_up has a requirement
    reg_up = next(r for r in result["reserve_awards"] if r["product_id"] == "reg_up")
    # Peak load on case14 with duck profile: ~259 MW × 1.0 ≈ 259 MW peak
    # so 10% = ~26 MW requirement at peak hour.
    assert max(reg_up["requirement_mw"]) > 20.0


def test_scaffold_topology_present_for_builtins() -> None:
    """Every built-in case should come with a bus/edge layout."""
    for case_id in ("case9", "case14", "case30"):
        scen = rto_api.build_scaffold(case_id)
        topo = scen["topology"]
        bus_count = scen["network_summary"]["buses"]
        assert len(topo["buses"]) == bus_count
        # Positions normalised to the unit box.
        for b in topo["buses"]:
            assert 0.0 <= b["x"] <= 1.0
            assert 0.0 <= b["y"] <= 1.0
        # Edge endpoints refer to known bus numbers.
        numbers = {b["number"] for b in topo["buses"]}
        for br in topo["branches"]:
            assert br["from"] in numbers and br["to"] in numbers


def test_goc3_case_pulls_load_profile_from_problem_file() -> None:
    """GO-C3 73-bus should have load profiles sourced from its
    companion ``.goc3-problem.json.zst`` — not the empty network."""
    scen = rto_api.build_scaffold("goc3_73")
    # 18 × 15-min intervals from the problem archive.
    assert scen["time_axis"]["periods"] == 18
    assert scen["time_axis"]["resolution_minutes"] == 15
    # 51 consumer devices → 51 bus loads.
    assert len(scen["loads"]) >= 50
    peak = scen["network_summary"]["total_load_mw"]
    assert peak > 1000.0, f"GO-73 peak load should be ~7 GW, got {peak}"
    # Each load carries a per-period profile.
    assert all("profile_mw" in l for l in scen["loads"])


def test_load_shape_changes_profile() -> None:
    """Switching from duck to flat changes per-period load totals."""
    scen = rto_api.build_scaffold("case14")
    scen["load_config"]["profile_shape"] = "duck"
    result_duck = rto_api.run_solve(scen)
    scen["load_config"]["profile_shape"] = "flat"
    result_flat = rto_api.run_solve(scen)
    assert result_duck["status"] == "ok" and result_flat["status"] == "ok"
    # Flat profile has same load every period; duck has wider variance.
    def period_totals(r):
        return [sum(l["served_mw"][t] for l in r["loads"]) for t in range(r["periods"])]
    dv = period_totals(result_duck)
    fv = period_totals(result_flat)
    assert max(dv) - min(dv) > max(fv) - min(fv) * 1.1
