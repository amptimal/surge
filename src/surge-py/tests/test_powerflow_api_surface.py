# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from pathlib import Path

import surge


REPO_ROOT = Path(__file__).resolve().parents[3]
CASE9 = REPO_ROOT / "examples" / "cases" / "case9" / "case9.surge.json.zst"
TEST_DATA = Path(__file__).resolve().parent / "data" / "profiles"


def test_powerflow_namespace_is_canonical():
    assert hasattr(surge, "powerflow")
    assert hasattr(surge, "io")
    assert hasattr(surge.io, "profiles")
    assert not hasattr(surge, "solve_timeseries")
    assert not hasattr(surge, "solve_fdpf")
    assert not hasattr(surge, "plot")
    assert hasattr(surge.powerflow, "solve_fdpf")
    assert not hasattr(surge, "solve_dispatch")
    assert not hasattr(surge, "dispatch")
    assert not hasattr(surge.powerflow, "replay_dispatch")


def test_powerflow_solver_surface():
    net = surge.load(CASE9)

    dc = surge.solve_dc_pf(net)
    ac = surge.solve_ac_pf(net)
    fdpf = surge.powerflow.solve_fdpf(net)

    assert dc.va_rad.shape == (net.n_buses,)
    assert dc.branch_p_mw.shape == (net.n_branches,)
    assert dc.solve_time_secs > 0
    assert ac.converged
    assert fdpf.converged


def test_profile_io_csv_readers():
    load_profiles = surge.io.profiles.read_load_profiles_csv(
        TEST_DATA / "load_24h.csv"
    )
    renewable_profiles = surge.io.profiles.read_renewable_profiles_csv(
        TEST_DATA / "renewable_24h.csv"
    )

    assert 101 in load_profiles
    assert len(load_profiles[101]) == 24
    assert "gen_101" in renewable_profiles
    assert len(renewable_profiles["gen_101"]) == 24
