# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Tests for the optional PyPSA netCDF bridge.

The bridge is only active when the ``pypsa`` package is installed.
Tests are skipped cleanly otherwise.
"""
from __future__ import annotations

from pathlib import Path

import pytest

import surge


pytest.importorskip("pypsa")

from surge.io import pypsa_nc


REPO_ROOT = Path(__file__).resolve().parents[3]
PAB_NC = (
    REPO_ROOT.parent
    / "Power-Agent"
    / "PowerAgentBench"
    / "cases"
    / "case39"
    / "pypsa"
    / "case39.nc"
)


@pytest.fixture(scope="module")
def pab_case_network():
    if not PAB_NC.exists():
        pytest.skip(f"PyPSA test case not available at {PAB_NC}")
    return pypsa_nc.load(str(PAB_NC))


def test_bridge_returns_surge_network(pab_case_network):
    assert isinstance(pab_case_network, surge.Network)


def test_bridge_preserves_counts(pab_case_network):
    s = pab_case_network.summary()
    assert s["n_buses"] == 39
    assert s["n_branches"] == 46  # 35 lines + 11 transformers
    assert s["n_generators"] == 10
    assert s["n_loads"] == 21


def test_bridge_preserves_voltage_setpoints(pab_case_network):
    """The whole point of this bridge: v_mag_pu_set survives the import.

    The MATPOWER path folds per-bus voltage setpoints into each
    generator's Vg and loses the direct bus-level setpoint. This bridge
    puts them on the surge bus directly.
    """
    import pypsa

    pypsa_net = pypsa.Network(str(PAB_NC))
    # Check a handful of bus voltage setpoints match.
    for bus_name in ["0", "5", "10", "20", "30"]:
        expected = float(pypsa_net.buses.at[bus_name, "v_mag_pu_set"])
        # Find surge bus by number.
        bus_num = int(bus_name)
        idx = list(pab_case_network.bus_numbers).index(bus_num)
        actual = list(pab_case_network.bus_vm)[idx]
        assert abs(actual - expected) < 1e-9, (
            f"bus {bus_name}: expected vm_pu_set={expected}, got {actual}"
        )


def test_bridge_assigns_slack_and_pv_buses(pab_case_network):
    """Generator-carrying buses should be PV / Slack, not all PQ.

    PyPSA leaves per-bus `control` as PQ even when generators are
    attached; the bridge derives bus types from the generator's
    `control` column (Slack / PV).
    """
    import pypsa

    pypsa_net = pypsa.Network(str(PAB_NC))
    slack_buses = set()
    pv_buses = set()
    for _, row in pypsa_net.generators.iterrows():
        ctrl = str(row.get("control", "")).lower()
        bus_num = int(row["bus"])
        if ctrl == "slack":
            slack_buses.add(bus_num)
        elif ctrl == "pv":
            pv_buses.add(bus_num)

    # Surge exposes bus_type_str.
    bus_types = dict(zip(pab_case_network.bus_numbers, pab_case_network.bus_type_str))
    for bus_num in slack_buses:
        assert bus_types[bus_num] == "Slack", (
            f"bus {bus_num} should be Slack but is {bus_types[bus_num]}"
        )
    for bus_num in pv_buses:
        assert bus_types[bus_num] == "PV", (
            f"bus {bus_num} should be PV but is {bus_types[bus_num]}"
        )


def test_bridge_line_impedance_rebased_to_system_base(pab_case_network):
    """PyPSA lines are in ohms/siemens; surge expects per-unit.

    Spot-check: L6 in case39 connects bus 3↔4 at v_nom=345 kV.
    PyPSA stores r=0.9522 Ω; at z_base=345²/100=1190.25 Ω the expected
    per-unit is r=0.0008.
    """
    # Find branch from=3 to=4
    froms = list(pab_case_network.branch_from)
    tos = list(pab_case_network.branch_to)
    r_values = list(pab_case_network.branch_r)
    for i, (f, t) in enumerate(zip(froms, tos)):
        if f == 3 and t == 4:
            assert abs(r_values[i] - 0.0008) < 1e-5, (
                f"L6 (bus 3→4) r_pu expected ~0.0008, got {r_values[i]}"
            )
            return
    pytest.fail("Could not find branch 3→4 in bridged network")


def test_bridge_transformer_impedance_rebased_to_system_base(pab_case_network):
    """PyPSA transformers store r, x in per-unit on s_nom base; surge
    expects per-unit on the system (100 MVA) base.

    Spot-check: T0 has x=0.1629 on s_nom=900 base; rebased to 100 MVA
    it should be x=0.1629 * 100/900 = 0.0181.
    """
    froms = list(pab_case_network.branch_from)
    tos = list(pab_case_network.branch_to)
    taps = list(pab_case_network.branch_tap)
    x_values = list(pab_case_network.branch_x)
    for i, (f, t, tap) in enumerate(zip(froms, tos, taps)):
        # T0 is bus 1 → bus 29 with tap_ratio 1.025
        if f == 1 and t == 29 and abs(tap - 1.025) < 1e-6:
            assert abs(x_values[i] - 0.0181) < 1e-3, (
                f"T0 (bus 1→29, tap=1.025) x_pu expected ~0.0181, got {x_values[i]}"
            )
            return
    pytest.fail("Could not find transformer T0 (bus 1→29, tap 1.025) in bridged network")


def test_bridge_ac_pf_converges(pab_case_network):
    """Bridge produces a network that surge's AC solver can solve.

    We verify convergence, not bit-identical values to PyPSA (expected
    solver drift — documented in format-interop.md).
    """
    sol = surge.solve_ac_pf(pab_case_network)
    assert sol.converged
    assert sol.max_mismatch < 1e-6


def test_bridge_missing_pypsa_raises_clear_error(tmp_path, monkeypatch):
    """If pypsa isn't installed, load() should raise ImportError with a
    hint to install it."""
    import builtins

    real_import = builtins.__import__

    def _no_pypsa(name, *args, **kwargs):
        if name == "pypsa":
            raise ImportError("No module named 'pypsa'")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", _no_pypsa)

    with pytest.raises(ImportError, match="pip install pypsa"):
        pypsa_nc.load(str(tmp_path / "nonexistent.nc"))
