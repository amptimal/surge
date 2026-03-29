# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from pathlib import Path

import pytest

import surge


REPO_ROOT = Path(__file__).resolve().parents[3]
CASE9 = REPO_ROOT / "examples" / "cases" / "case9" / "case9.surge.json.zst"


def test_io_namespace_is_canonical():
    assert hasattr(surge, "io")
    assert hasattr(surge.io, "bin")
    assert hasattr(surge.io, "psse")
    assert hasattr(surge.io.psse, "raw")
    assert hasattr(surge.io.psse, "rawx")
    assert hasattr(surge.io.psse, "sequence")
    assert hasattr(surge.io.psse, "dyr")

    assert not hasattr(surge, "save_matpower")
    assert not hasattr(surge, "save_psse")
    assert not hasattr(surge, "save_json")
    assert not hasattr(surge, "save_cgmes3")
    assert not hasattr(surge, "network_to_string")
    assert not hasattr(surge, "load_dyr")
    assert not hasattr(surge, "full_modal_analysis")
    assert not hasattr(surge, "compute_cct_batch")
    assert not hasattr(surge, "pss_tune")
    assert not hasattr(surge.io, "raw")
    assert not hasattr(surge.io, "rawx")
    assert not hasattr(surge.io, "sequence")
    assert not hasattr(surge.io, "dyr")


def test_io_format_enum_matches_dumpable_surface():
    assert {member.name for member in surge.io.Format} == {
        "MATPOWER",
        "PSSE_RAW",
        "XIIDM",
        "UCTE",
        "SURGE_JSON",
        "DSS",
        "EPC",
    }
    assert not hasattr(surge.io.Format, "RAWX")
    assert not hasattr(surge.io.Format, "IEEE_CDF")
    assert not hasattr(surge.io.Format, "CGMES")


def test_top_level_public_modules_are_exported():
    exports = set(surge.__all__)
    assert {
        "batch",
        "compose",
        "construction",
        "contingency",
        "io",
        "losses",
        "subsystem",
        "transfer",
        "units",
    } <= exports

    removed = {"frequency", "interop", "protection", "ridethrough", "tpl"}
    assert removed.isdisjoint(exports)


def test_pathlike_and_string_io_roundtrip(tmp_path):
    net = surge.load(CASE9)

    json_path = tmp_path / "case9.surge.json"
    surge.save(net, json_path)
    json_net = surge.load(json_path)
    assert json_net.n_buses == net.n_buses

    zst_path = tmp_path / "case9.surge.json.zst"
    surge.save(net, zst_path)
    zst_net = surge.load(zst_path)
    assert zst_net.n_buses == net.n_buses

    bin_path = tmp_path / "case9.surge.bin"
    surge.save(net, bin_path)
    bin_net = surge.load(bin_path)
    assert bin_net.n_buses == net.n_buses

    matpower_text = surge.io.dumps(net, surge.io.Format.MATPOWER)
    matpower_net = surge.io.loads(matpower_text, surge.io.Format.MATPOWER)
    assert matpower_net.n_branches == net.n_branches

    surge_json_text = surge.io.json.dumps(net)
    surge_json_net = surge.io.loads(surge_json_text, surge.io.Format.SURGE_JSON)
    assert surge_json_net.n_buses == net.n_buses

    surge_bin_bytes = surge.io.bin.dumps(net)
    surge_bin_net = surge.io.bin.loads(surge_bin_bytes)
    assert surge_bin_net.n_generators == net.n_generators

    raw_text = surge.io.psse.raw.dumps(net, version=surge.io.psse.raw.Version.V33)
    raw_net = surge.io.psse.raw.loads(raw_text)
    assert raw_net.n_generators == net.n_generators

    with pytest.raises(ValueError, match="unsupported dump format"):
        surge.io.dumps(net, "rawx")


def test_cgmes_export_is_explicit(tmp_path):
    net = surge.load(CASE9)
    with pytest.raises(Exception):
        surge.save(net, tmp_path / "case9.xml")


def test_editable_market_objects_use_stable_refs():
    pumped_hydro = surge.PumpedHydroUnit("PH1", 101, "G1", 250.0)
    assert pumped_hydro.generator_bus == 101
    assert pumped_hydro.generator_id == "G1"

    branch_outage = surge.OutageEntry(
        "Branch",
        1.0,
        2.5,
        from_bus=4,
        to_bus=5,
        circuit="1",
    )
    assert branch_outage.category == "Branch"
    assert branch_outage.from_bus == 4
    assert branch_outage.to_bus == 5
    assert branch_outage.circuit == "1"
