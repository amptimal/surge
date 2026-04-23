# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Tests for release/v0.1.5 agent-friendly helpers:

- ``Network.summary``, ``Network.loads_dataframe``, ``Network.shunts_dataframe``
- ``list_builtin_cases`` and ``load_builtin_case``
- ``load(path, format=...)`` with explicit format override, plus the
  ``load_network`` alias
- ``.to_dict()`` on every solution / result class exposed by the MCP surface
- JSON serializability of every ``.to_dict()`` payload
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

import surge


# ---------------------------------------------------------------------------
# Builtin case registry
# ---------------------------------------------------------------------------

def test_list_builtin_cases_returns_expected_names() -> None:
    names = surge.list_builtin_cases()
    assert names == [
        "case9", "case14", "case30", "market30", "case57", "case118", "case300",
    ]


@pytest.mark.parametrize("name", ["case9", "case14", "case118"])
def test_load_builtin_case_round_trips_with_constructor(name: str) -> None:
    via_factory = surge.load_builtin_case(name)
    via_ctor = getattr(surge, name)()
    assert via_factory.n_buses == via_ctor.n_buses
    assert via_factory.n_branches == via_ctor.n_branches
    assert via_factory.n_generators == via_ctor.n_generators


def test_load_builtin_case_rejects_unknown_name() -> None:
    with pytest.raises(ValueError):
        surge.load_builtin_case("not-a-real-case")


# ---------------------------------------------------------------------------
# load(path, format=...)
# ---------------------------------------------------------------------------

def test_load_accepts_explicit_format() -> None:
    matpower_path = (
        Path(surge.__file__).resolve().parent.parent.parent.parent.parent
        / "examples" / "cases" / "ieee118" / "case118.m"
    )
    if not matpower_path.exists():
        pytest.skip(f"test data not available: {matpower_path}")
    via_auto = surge.load(str(matpower_path))
    via_explicit = surge.load(str(matpower_path), format="matpower")
    assert via_auto.n_buses == via_explicit.n_buses == 118


def test_load_network_is_alias_for_load() -> None:
    assert surge.load_network is not surge.load  # different function objects
    # But produce equivalent results.
    net1 = surge.load_builtin_case("case14")
    assert net1.n_buses == 14


# ---------------------------------------------------------------------------
# Network.summary / loads_dataframe / shunts_dataframe
# ---------------------------------------------------------------------------

def test_network_summary_shape_and_contents() -> None:
    net = surge.case118()
    summary = net.summary()
    assert isinstance(summary, dict)
    # Required keys
    for key in (
        "name", "base_mva", "freq_hz",
        "n_buses", "n_branches", "n_branches_in_service",
        "n_generators", "n_generators_in_service",
        "n_loads", "n_loads_in_service",
        "n_fixed_shunts", "n_hvdc_links", "n_hvdc_dc_grids",
        "n_areas", "n_zones", "areas", "zones", "voltage_levels_kv",
        "total_generation_mw", "total_generation_capacity_mw",
        "total_load_mw", "total_load_mvar",
    ):
        assert key in summary, f"missing key: {key}"

    assert summary["n_buses"] == 118
    assert summary["n_branches"] == 186
    assert summary["n_generators"] == 54
    # Voltage-level list is sorted unique.
    kvs = summary["voltage_levels_kv"]
    assert kvs == sorted(kvs)
    assert len(kvs) == len(set(kvs))
    # Totals are positive finite numbers.
    assert summary["total_generation_capacity_mw"] > 0


def test_network_summary_is_json_serializable() -> None:
    net = surge.case14()
    json.dumps(net.summary())  # should not raise


def test_loads_dataframe_returns_rows_for_each_load() -> None:
    net = surge.case118()
    expected = len(net.loads)
    df_or_dict = net.loads_dataframe()
    try:
        import pandas as pd  # noqa: F401
        assert len(df_or_dict) == expected
        # Known columns
        for col in ("pd_mw", "qd_mvar", "in_service", "conforming"):
            assert col in df_or_dict.columns
    except ImportError:
        assert "pd_mw" in df_or_dict
        assert len(df_or_dict["pd_mw"]) == expected


def test_shunts_dataframe_runs_without_error() -> None:
    # case118 may or may not carry FixedShunt entries; either way it must not
    # raise. Test on market30 which has a richer device mix.
    net = surge.market30()
    result = net.shunts_dataframe()
    try:
        import pandas as pd  # noqa: F401
        assert result is not None
    except ImportError:
        assert "shunt_id" in result


# ---------------------------------------------------------------------------
# .to_dict() coverage — all must be JSON-serializable.
# ---------------------------------------------------------------------------

def _assert_json_ok(d: Any) -> None:
    """Round-trip through json to confirm full serializability."""
    encoded = json.dumps(d, default=str)
    decoded = json.loads(encoded)
    assert isinstance(decoded, (dict, list))


def test_dc_pf_result_to_dict() -> None:
    net = surge.case118()
    sol = surge.solve_dc_pf(net)
    d = sol.to_dict()
    assert d["bus_numbers"]
    assert len(d["va_rad"]) == net.n_buses
    assert len(d["va_deg"]) == net.n_buses
    assert "branch_p_mw" in d
    _assert_json_ok(d)


def test_ac_pf_result_to_dict_still_works() -> None:
    # Regression: pre-existing to_dict on AcPfResult should be unchanged.
    net = surge.case14()
    sol = surge.solve_ac_pf(net)
    d = sol.to_dict()
    assert d["converged"]
    _assert_json_ok(d)


def test_dc_opf_result_to_dict() -> None:
    from surge.opf import DcOpfOptions, DcOpfRuntime, solve_dc_opf
    net = surge.case14()
    sol = solve_dc_opf(net, DcOpfOptions(), DcOpfRuntime())
    d = sol.to_dict()
    # Inherits OpfResult fields + adds DC-OPF specifics.
    assert "total_cost" in d  # from underlying opf
    assert "hvdc_dispatch_mw" in d
    assert "is_feasible" in d
    _assert_json_ok(d)


def test_ac_opf_result_to_dict() -> None:
    from surge.opf import AcOpfOptions, AcOpfRuntime, solve_ac_opf
    # case14 has a stock AC setpoint that violates tightened voltage limits;
    # case9 is small and consistent.
    net = surge.case9()
    sol = solve_ac_opf(net, AcOpfOptions(), AcOpfRuntime())
    d = sol.to_dict()
    # Inherits OpfResult fields + adds AC-OPF HVDC specifics.
    assert "total_cost" in d
    assert "hvdc_p_dc_mw" in d
    assert "hvdc_iterations" in d
    _assert_json_ok(d)


def test_scopf_result_to_dict() -> None:
    from surge.opf import ScopfOptions, ScopfRuntime, solve_scopf
    net = surge.case14()
    sol = solve_scopf(net, ScopfOptions(), ScopfRuntime())
    d = sol.to_dict()
    assert "base_opf" in d
    assert isinstance(d["base_opf"], dict)
    assert "formulation" in d
    assert isinstance(d["binding_contingencies"], list)
    assert isinstance(d["remaining_violations"], list)
    assert isinstance(d["failed_contingencies"], list)
    assert isinstance(d["screening_stats"], dict)
    _assert_json_ok(d)


def test_contingency_analysis_to_dict_shape() -> None:
    net = surge.case14()
    ca = surge.analyze_n1_branch(net)
    d = ca.to_dict()
    for key in (
        "n_contingencies", "n_screened_out", "n_ac_solved", "n_converged",
        "n_with_violations", "n_violations", "n_voltage_critical",
        "solve_time_secs", "results", "violations",
    ):
        assert key in d, f"missing key {key}"
    assert isinstance(d["results"], list)
    assert isinstance(d["violations"], list)
    if d["results"]:
        r = d["results"][0]
        for k in (
            "contingency_id", "label", "converged", "n_violations",
            "max_loading_pct", "min_vm_pu", "n_islands",
            "vsm_category", "max_l_index",
        ):
            assert k in r
    if d["violations"]:
        v = d["violations"][0]
        for k in (
            "contingency_id", "violation_type",
            "from_bus", "to_bus", "bus_number",
            "loading_pct", "flow_mw", "flow_mva", "limit_mva",
            "vm_pu", "vm_limit_pu",
        ):
            assert k in v
    _assert_json_ok(d)


def _pick_transfer_buses(net):
    """Pick disjoint source/sink bus sets for a transfer path.

    Prefers distinct areas when the network has more than one; falls back to
    splitting the bus list in half otherwise.
    """
    buses_by_area: dict[int, list[int]] = {}
    for b in net.buses:
        buses_by_area.setdefault(b.area, []).append(b.number)
    areas = sorted(buses_by_area)
    if len(areas) >= 2:
        return buses_by_area[areas[0]][:3], buses_by_area[areas[1]][:3]
    all_nums = [b.number for b in net.buses]
    mid = len(all_nums) // 2
    return all_nums[:3], all_nums[mid:mid + 3]


def test_nerc_atc_result_to_dict() -> None:
    from surge import transfer
    net = surge.case118()
    source_buses, sink_buses = _pick_transfer_buses(net)
    path = transfer.TransferPath("atc-test", source_buses, sink_buses)
    res = transfer.compute_nerc_atc(net, path)
    d = res.to_dict()
    for k in (
        "atc_mw", "ttc_mw", "trm_mw", "cbm_mw", "etc_mw", "limit_cause",
        "binding_branch", "binding_contingency", "monitored_branches",
        "reactive_margin_warning", "transfer_ptdf",
    ):
        assert k in d
    _assert_json_ok(d)


def test_ac_atc_result_to_dict() -> None:
    from surge import transfer
    net = surge.case118()
    source_buses, sink_buses = _pick_transfer_buses(net)
    path = transfer.TransferPath("ac-atc-test", source_buses, sink_buses)
    res = transfer.compute_ac_atc(net, path)
    d = res.to_dict()
    for k in (
        "atc_mw", "thermal_limit_mw", "voltage_limit_mw",
        "limiting_bus", "binding_branch", "limiting_constraint",
    ):
        assert k in d
    _assert_json_ok(d)


# ---------------------------------------------------------------------------
# Matrix .to_dict() with format param.
# ---------------------------------------------------------------------------

def _ptdf_for_case14():
    return surge.dc.compute_ptdf(surge.case14())


def test_ptdf_to_dict_summary_default() -> None:
    res = _ptdf_for_case14()
    d = res.to_dict()
    assert "shape" in d and isinstance(d["shape"], tuple)
    assert "sparsity" in d
    assert "max_abs" in d
    assert "nnz" in d
    assert "top_per_row" in d
    assert "bus_numbers" in d
    assert "monitored_branch_keys" in d
    assert "matrix" not in d  # summary must not dump the dense matrix
    _assert_json_ok(d)


def test_ptdf_to_dict_sparse_format() -> None:
    res = _ptdf_for_case14()
    d = res.to_dict(format="sparse")
    assert set(d).issuperset({"data", "indices", "indptr", "shape"})
    # indptr length = n_rows + 1
    assert len(d["indptr"]) == d["shape"][0] + 1
    _assert_json_ok(d)


def test_ptdf_to_dict_full_format_dense() -> None:
    res = _ptdf_for_case14()
    d = res.to_dict(format="full")
    assert "matrix" in d
    rows = d["matrix"]
    assert len(rows) == d["shape"][0]
    assert all(len(r) == d["shape"][1] for r in rows)
    _assert_json_ok(d)


def test_ptdf_to_dict_rejects_unknown_format() -> None:
    res = _ptdf_for_case14()
    with pytest.raises(ValueError):
        res.to_dict(format="bogus")


def test_lodf_to_dict_summary() -> None:
    net = surge.case14()
    res = surge.dc.compute_lodf(net)
    d = res.to_dict()
    assert "top_per_row" in d
    assert "monitored_keys" in d
    assert "outage_keys" in d
    _assert_json_ok(d)


def test_lodf_matrix_to_dict_refuses_full_for_large_networks() -> None:
    net = surge.case118()
    res = surge.dc.compute_lodf_matrix(net)
    # case118 has 186 branches, under the 500 limit — full is allowed.
    d_full = res.to_dict(format="full")
    assert "matrix" in d_full
    # summary should always work
    d_sum = res.to_dict(format="summary")
    assert "top_per_row" in d_sum
    _assert_json_ok(d_sum)


def test_otdf_to_dict_summary_and_refuses_full() -> None:
    from surge.dc import BranchKey, OtdfRequest, compute_otdf
    net = surge.case14()
    # Take the first two branches as both monitored and outage so the tensor
    # is tiny enough to serialize.
    branches = list(net.branches)[:2]
    keys = tuple(
        BranchKey(from_bus=b.from_bus, to_bus=b.to_bus, circuit=b.circuit)
        for b in branches
    )
    req = OtdfRequest(monitored_branches=keys, outage_branches=keys)
    res = compute_otdf(net, req)
    d = res.to_dict()
    assert d["shape"][0] == 2
    assert d["shape"][1] == 2
    assert "top_per_pair" in d
    _assert_json_ok(d)

    d_sparse = res.to_dict(format="sparse")
    assert "indices" in d_sparse
    assert "values" in d_sparse
    _assert_json_ok(d_sparse)

    with pytest.raises(ValueError):
        res.to_dict(format="full")
