# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import pandas as pd
import pytest

import surge


def _happy_path_frames():
    buses = pd.DataFrame(
        [
            {
                "number": 1,
                "type": "Slack",
                "base_kv": 230.0,
                "name": "SWING",
                "vm_pu": 1.03,
            },
            {
                "number": 2,
                "type": "PV",
                "base_kv": 230.0,
                "name": "GEN",
                "vm_pu": 1.02,
            },
            {
                "number": 3,
                "type": "PQ",
                "base_kv": 230.0,
                "name": "LOAD",
                "pd_mw": 125.0,
                "qd_mvar": 35.0,
            },
        ]
    )
    branches = pd.DataFrame(
        [
            {
                "from_bus": 1,
                "to_bus": 2,
                "r": 0.01,
                "x": 0.08,
                "b": 0.02,
                "rate_a": 250.0,
                "tap": 1.0,
                "shift": 0.0,
                "circuit": 7,
            },
            {
                "from_bus": 2,
                "to_bus": 3,
                "r": 0.01,
                "x": 0.09,
                "rate_a": 250.0,
            },
            {
                "from_bus": 1,
                "to_bus": 3,
                "r": 0.02,
                "x": 0.12,
                "rate_a": 250.0,
            },
        ]
    )
    generators = pd.DataFrame(
        [
            {
                "bus": 1,
                "pg": 110.0,
                "pmax": 160.0,
                "pmin": 20.0,
                "qmax": 90.0,
                "qmin": -90.0,
                "vs": 1.03,
                "machine_id": "G1",
            },
            {
                "bus": 2,
                "pg": 40.0,
                "pmax": 70.0,
                "qmax": 60.0,
                "qmin": -60.0,
                "vs": 1.02,
            },
        ]
    )
    return buses, branches, generators


def test_from_dataframes_builds_network_and_solves():
    buses, branches, generators = _happy_path_frames()

    net = surge.construction.from_dataframes(
        buses,
        branches,
        generators,
        base_mva=125.0,
        name="pandas-grid",
    )

    assert net.name == "pandas-grid"
    assert net.base_mva == pytest.approx(125.0)
    assert net.bus_numbers == [1, 2, 3]
    assert net.bus_name == ["SWING", "GEN", "LOAD"]
    assert net.bus_type_str == ["Slack", "PV", "PQ"]
    assert net.bus_pd == pytest.approx([0.0, 0.0, 125.0])
    assert net.bus_qd == pytest.approx([0.0, 0.0, 35.0])
    assert net.branch_circuit == ["7", "1", "1"]
    assert net.branch_b == pytest.approx([0.02, 0.0, 0.0])
    assert net.branch_rate_a == pytest.approx([250.0, 250.0, 250.0])
    assert net.gen_machine_id == ["G1", "1"]
    assert net.gen_pmax == pytest.approx([160.0, 70.0])
    assert net.gen_qmax == pytest.approx([90.0, 60.0])
    assert net.gen_vs_pu == pytest.approx([1.03, 1.02])

    result = surge.solve_ac_pf(net)
    assert result.converged is True


def test_from_dataframes_applies_defaults_and_dtype_coercion():
    buses = pd.DataFrame(
        [
            {"number": "1", "base_kv": "230.0", "type": "Slack"},
            {"number": "2", "base_kv": 230, "pd_mw": 25},
        ]
    )
    branches = pd.DataFrame([{"from_bus": "1", "to_bus": 2, "r": "0.01", "x": 0.08}])
    generators = pd.DataFrame([{"bus": "1", "pg": "30.0"}])

    net = surge.construction.from_dataframes(buses, branches, generators)

    assert net.bus_type_str == ["Slack", "PQ"]
    assert net.bus_name == ["", ""]
    assert net.bus_pd == pytest.approx([0.0, 25.0])
    assert net.bus_qd == pytest.approx([0.0, 0.0])
    assert net.bus_vm == pytest.approx([1.0, 1.0])
    assert net.branch_b == pytest.approx([0.0])
    assert net.branch_rate_a == pytest.approx([0.0])
    assert net.branch_tap == pytest.approx([1.0])
    assert net.branch_shift_deg == pytest.approx([0.0])
    assert net.branch_circuit == ["1"]
    assert net.gen_pmax == pytest.approx([9999.0])
    assert net.gen_pmin == pytest.approx([0.0])
    assert net.gen_qmax == pytest.approx([9999.0])
    assert net.gen_qmin == pytest.approx([-9999.0])
    assert net.gen_vs_pu == pytest.approx([1.0])
    assert net.gen_machine_id == ["1"]


@pytest.mark.parametrize(
    ("section", "drop_column"),
    [
        ("buses", "number"),
        ("branches", "from_bus"),
        ("generators", "bus"),
    ],
)
def test_from_dataframes_requires_mandatory_columns(section, drop_column):
    buses, branches, generators = _happy_path_frames()
    frames = {"buses": buses, "branches": branches, "generators": generators}
    frames[section] = frames[section].drop(columns=[drop_column])

    with pytest.raises(KeyError, match=drop_column):
        surge.construction.from_dataframes(
            frames["buses"],
            frames["branches"],
            frames["generators"],
        )


def test_from_dataframes_rejects_duplicate_bus_numbers():
    buses, branches, generators = _happy_path_frames()
    duplicate = pd.concat([buses, buses.iloc[[0]]], ignore_index=True)

    with pytest.raises(surge.NetworkError, match="already exists"):
        surge.construction.from_dataframes(duplicate, branches, generators)


def test_from_dataframes_rejects_missing_bus_references():
    buses, branches, generators = _happy_path_frames()
    bad_branches = branches.copy()
    bad_branches.loc[0, "to_bus"] = 999

    with pytest.raises(surge.NetworkError, match="to_bus"):
        surge.construction.from_dataframes(buses, bad_branches, generators)

    bad_generators = generators.copy()
    bad_generators.loc[0, "bus"] = 999

    with pytest.raises(surge.NetworkError, match="not found"):
        surge.construction.from_dataframes(buses, branches, bad_generators)


def test_from_dataframes_rejects_nan_and_non_numeric_values():
    buses, branches, generators = _happy_path_frames()

    bad_buses = buses.copy()
    bad_buses.loc[0, "number"] = float("nan")
    with pytest.raises(ValueError):
        surge.construction.from_dataframes(bad_buses, branches, generators)

    bad_generators = generators.astype({"pg": object}).copy()
    bad_generators.loc[0, "pg"] = "not-a-number"
    with pytest.raises(ValueError):
        surge.construction.from_dataframes(buses, branches, bad_generators)
