# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from pathlib import Path

import pytest
import surge


CASE9 = Path(__file__).resolve().parents[3] / "examples" / "cases" / "case9" / "case9.surge.json.zst"
ACTIVSG2000 = (
    Path(__file__).resolve().parents[3]
    / "examples"
    / "cases"
    / "case_ACTIVSg2000"
    / "case_ACTIVSg2000.surge.json.zst"
)
ACTIVSG_TS = (
    Path(__file__).resolve().parents[3]
    / "research"
    / "test-cases"
    / "data"
    / "ACTIVSg_Time_Series"
)


def test_dispatch_surface_smoke_and_roundtrip():
    net = surge.load(CASE9)

    result = surge.solve_dispatch(
        net,
        {
            "timeline": {"periods": 1, "interval_hours": 1.0},
            "network": {"thermal_limits": {"enforce": True}},
        },
    )

    assert isinstance(result, surge.DispatchResult)
    assert result.study["periods"] == 1
    assert result.study["formulation"] == "dc"
    assert len(result.periods) == 1
    assert result.summary["total_cost"] >= 0.0

    via_namespace = surge.dispatch.solve_dispatch(net)
    assert isinstance(via_namespace, surge.DispatchResult)
    assert via_namespace.study["periods"] == 1

    roundtrip = surge.DispatchResult.from_json(result.to_json())
    assert roundtrip.summary["total_cost"] == pytest.approx(result.summary["total_cost"])
    assert roundtrip.periods[0]["total_cost"] == pytest.approx(result.periods[0]["total_cost"])


def test_dispatch_rejects_runtime_lp_solver_inside_request_payload():
    net = surge.load(CASE9)

    with pytest.raises(ValueError, match="lp_solver"):
        surge.solve_dispatch(net, {"runtime": {"lp_solver": "highs"}})


def test_dispatch_rejects_runtime_nlp_solver_inside_request_payload():
    net = surge.load(CASE9)

    with pytest.raises(ValueError, match="nlp_solver"):
        surge.solve_dispatch(net, {"runtime": {"nlp_solver": "ipopt"}})


def test_dispatch_rejects_unknown_top_level_request_fields():
    net = surge.load(CASE9)

    with pytest.raises(ValueError, match="timelnie"):
        surge.solve_dispatch(net, {"timelnie": {"periods": 3}})


def test_dispatch_rejects_unknown_nested_request_fields():
    net = surge.load(CASE9)

    with pytest.raises(ValueError, match="perids"):
        surge.solve_dispatch(net, {"timeline": {"perids": 3}})


@pytest.mark.skipif(
    not (ACTIVSG2000.exists() and ACTIVSG_TS.exists()),
    reason="ACTIVSg case bundle or time-series package not present",
)
def test_dispatch_namespace_exposes_activsg_time_series_helper():
    net = surge.load(ACTIVSG2000)

    imported = surge.dispatch.read_tamu_activsg_time_series(net, ACTIVSG_TS, case="2000")

    assert imported.case == "ACTIVSg2000"
    assert imported.periods == 8784
    assert imported.timeline(24)["periods"] == 24

    profiles = imported.dc_dispatch_profiles(24)
    first_load_profile = profiles["load"]["profiles"][0]
    first_renewable_profile = profiles["renewable"]["profiles"][0]
    assert len(first_load_profile["values_mw"]) == 24
    assert len(first_renewable_profile["capacity_factors"]) == 24

    adjusted = imported.network_with_nameplate_overrides(net)
    assert isinstance(adjusted, surge.Network)
    assert adjusted.n_buses == net.n_buses
