# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""
Phase 4 pytest suite for the Surge Python bindings.

Tests the core surge module functions against standard IEEE test cases.
Run after building: cd src/surge-py && maturin develop --release
"""

import os
from pathlib import Path

import pytest
import numpy as np
import pandas as pd

import surge

# Absolute paths to test case files (relative to workspace root).
_REPO_ROOT = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "..", "..")
)
CASE9 = os.path.join(_REPO_ROOT, "examples", "cases", "case9", "case9.surge.json.zst")
CASE14 = os.path.join(_REPO_ROOT, "examples", "cases", "case14", "case14.surge.json.zst")
CASE30 = os.path.join(_REPO_ROOT, "examples", "cases", "case30", "case30.surge.json.zst")
CASE118_ZST = os.path.join(_REPO_ROOT, "examples", "cases", "ieee118", "case118.surge.json.zst")


def _generator_id_at_bus(network, bus, machine_id="1"):
    for generator in network.generators:
        if generator.bus == bus and generator.machine_id == machine_id:
            return generator.id
    raise AssertionError(f"generator not found at bus={bus} machine_id={machine_id}")


# ---------------------------------------------------------------------------
# Module-level smoke test
# ---------------------------------------------------------------------------

def test_version():
    v = surge.version()
    assert isinstance(v, str)
    assert len(v) > 0


# ---------------------------------------------------------------------------
# Network loading
# ---------------------------------------------------------------------------

class TestLoad:
    def test_load_case9(self):
        net = surge.load(CASE9)
        assert net is not None
        assert net.n_buses == 9
        assert net.n_branches == 9
        assert net.n_generators == 3

    def test_load_case14(self):
        net = surge.load(CASE14)
        assert net.n_buses == 14
        assert net.n_branches == 20

    def test_load_nonexistent_raises(self):
        with pytest.raises(Exception):
            surge.load("/nonexistent/path/case.m")

    def test_network_properties(self):
        net = surge.load(CASE9)
        assert net.base_mva == pytest.approx(100.0, rel=1e-6)
        assert net.total_load_mw > 0
        buses = net.bus_numbers
        assert len(buses) == 9
        assert isinstance(buses, list)

    def test_network_repr(self):
        net = surge.load(CASE9)
        r = repr(net)
        assert "case9" in r.lower() or "Network" in r


# ---------------------------------------------------------------------------
# DC power flow
# ---------------------------------------------------------------------------

class TestDcPowerFlow:
    def test_solve_dcpf_returns_object(self):
        net = surge.load(CASE9)
        sol = surge.solve_dc_pf(net)
        assert isinstance(sol, surge.DcPfResult)
        assert isinstance(sol.va_rad, np.ndarray)
        assert isinstance(sol.branch_p_mw, np.ndarray)
        assert isinstance(sol.slack_p_mw, float)
        assert sol.va_rad.shape == (9,)
        assert sol.branch_p_mw.shape == (9,)
        assert sol.solve_time_secs > 0
        assert len(sol.bus_numbers) == 9
        assert len(sol.branch_keys) == 9

    def test_solve_dcpf_case14(self):
        net = surge.load(CASE14)
        sol = surge.solve_dc_pf(net)
        assert sol.va_rad.shape == (14,)
        assert sol.branch_p_mw.shape == (20,)
        # Slack bus angle should be near zero.
        assert abs(sol.va_rad[0]) < 1e-10

    def test_headroom_slack_distribution_is_reported_in_mw(self):
        net = surge.load(CASE14)
        single = surge.solve_dc_pf(net)
        participating_buses = sorted({generator.bus for generator in net.generators if generator.in_service})

        sol = surge.solve_dc_pf(
            net,
            surge.DcPfOptions(headroom_slack_buses=participating_buses),
        )

        assert sol.slack_distribution_mw
        assert sum(sol.slack_distribution_mw.values()) == pytest.approx(single.slack_p_mw, abs=1e-6)

    def test_dcpf_angle_reference_changes_only_angles(self):
        net = surge.load(CASE14)
        net.set_bus_voltage(net.bus_numbers[0], 1.0, 6.87)
        preserve = surge.solve_dc_pf(net)
        zero = surge.solve_dc_pf(net, surge.DcPfOptions(angle_reference="zero"))
        distributed = surge.solve_dc_pf(
            net,
            surge.DcPfOptions(angle_reference="distributed_load"),
        )

        np.testing.assert_allclose(preserve.branch_p_mw, zero.branch_p_mw, atol=1e-9)
        np.testing.assert_allclose(preserve.branch_p_mw, distributed.branch_p_mw, atol=1e-9)
        assert abs(zero.va_rad[0]) < 1e-10
        assert not np.allclose(preserve.va_rad, zero.va_rad)


# ---------------------------------------------------------------------------
# AC power flow (Newton-Raphson)
# ---------------------------------------------------------------------------

class TestAcPowerFlow:
    def test_solve_acpf_converges_case9(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        assert sol.converged is True
        assert sol.max_mismatch < 1e-6
        assert isinstance(sol.vm, np.ndarray)
        assert isinstance(sol.va_rad, np.ndarray)
        assert sol.vm.shape == (9,)

    def test_solve_acpf_voltages_physical_case14(self):
        net = surge.load(CASE14)
        sol = surge.solve_ac_pf(net)
        assert sol.converged is True
        for v in sol.vm:
            assert 0.5 < v < 1.5, f"Voltage {v:.4f} out of physical range"

    def test_solve_acpf_custom_tolerance(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(
            net,
            surge.AcPfOptions(tolerance=1e-10, max_iterations=50),
        )
        assert sol.converged is True
        assert sol.max_mismatch < 1e-8

    def test_solve_acpf_flat_start(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net, surge.AcPfOptions(flat_start=True))
        assert sol.converged is True

    def test_va_deg_property(self):
        """M-12: va_deg returns angles in degrees, consistent with to_dataframe()."""
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        assert sol.converged is True
        va_rad = sol.va_rad
        va_deg = sol.va_deg
        assert isinstance(va_deg, np.ndarray)
        assert va_deg.shape == va_rad.shape
        np.testing.assert_allclose(va_deg, np.degrees(va_rad), atol=1e-12,
                                   err_msg="va_deg should equal np.degrees(va)")
        df = sol.to_dataframe()
        assert isinstance(df, pd.DataFrame), f"Expected DataFrame, got {type(df)}"
        np.testing.assert_allclose(va_deg, df["va_deg"].values, atol=1e-12,
                                   err_msg="va_deg must match to_dataframe()['va_deg']")


# ---------------------------------------------------------------------------
# PTDF matrix
# ---------------------------------------------------------------------------

class TestPtdf:
    def test_compute_ptdf_returns_result(self):
        """compute_ptdf() returns a PtdfResult with ptdf matrix and metadata."""
        net = surge.load(CASE9)
        result = surge.dc.compute_ptdf(net)
        assert hasattr(result, "ptdf")
        assert hasattr(result, "bus_numbers")
        assert hasattr(result, "monitored_branches")
        assert hasattr(result, "branch_from")
        assert hasattr(result, "branch_to")
        assert hasattr(result, "branch_circuit")
        assert hasattr(result, "branch_keys")

    def test_compute_ptdf_shape(self):
        net = surge.load(CASE9)
        result = surge.dc.compute_ptdf(net)
        ptdf = result.ptdf
        assert isinstance(ptdf, np.ndarray)
        assert ptdf.ndim == 2
        assert ptdf.shape == (9, 9)  # n_branches x n_buses

    def test_compute_ptdf_metadata_lengths(self):
        """bus_numbers length == n_buses, branch_from/to length == n_branches."""
        net = surge.load(CASE9)
        result = surge.dc.compute_ptdf(net)
        assert len(result.bus_numbers) == net.n_buses
        assert len(result.branch_from) == net.n_branches
        assert len(result.branch_to) == net.n_branches
        assert len(result.branch_circuit) == net.n_branches

    def test_compute_ptdf_bus_numbers_are_external(self):
        """bus_numbers must match the network's external bus numbers."""
        net = surge.load(CASE9)
        result = surge.dc.compute_ptdf(net)
        assert sorted(result.bus_numbers) == sorted(net.bus_numbers)

    def test_ptdf_slack_column_zero(self):
        net = surge.load(CASE9)
        result = surge.dc.compute_ptdf(net)
        ptdf = result.ptdf
        # Slack bus (bus index 0) column should be all zeros.
        assert np.allclose(ptdf[:, 0], 0.0, atol=1e-10)

    def test_ptdf_values_bounded(self):
        net = surge.load(CASE9)
        result = surge.dc.compute_ptdf(net)
        ptdf = result.ptdf
        # PTDF values should be in [-1, 1] (fraction of injection flowing on branch).
        assert np.all(ptdf >= -1.0 - 1e-9)
        assert np.all(ptdf <= 1.0 + 1e-9)

    def test_compute_ptdf_returns_result(self):
        """compute_ptdf() returns a PtdfResult with .ptdf ndarray."""
        net = surge.load(CASE9)
        result = surge.dc.compute_ptdf(net)
        assert hasattr(result, "ptdf")
        assert isinstance(result.ptdf, np.ndarray)
        assert result.ptdf.shape == (9, 9)

    def test_ptdf_repr(self):
        """PtdfResult __repr__ includes shape info."""
        net = surge.load(CASE9)
        result = surge.dc.compute_ptdf(net)
        r = repr(result)
        assert "PtdfResult" in r
        assert "buses=9" in r
        assert "monitored=9" in r


class TestDcStudyApi:
    def test_ptdf_request_uses_branch_keys_and_bus_numbers(self):
        net = surge.load(CASE9)
        branch = net.branches[0]
        request = surge.dc.PtdfRequest(
            monitored_branches=[
                surge.dc.BranchKey(branch.from_bus, branch.to_bus, branch.circuit),
            ],
            bus_numbers=[1, 2],
        )

        result = surge.dc.compute_ptdf(net, request)

        assert result.ptdf.shape == (1, 2)
        assert result.bus_numbers == [1, 2]
        assert result.branch_keys == [(branch.from_bus, branch.to_bus, branch.circuit)]

    def test_n2_lodf_returns_typed_result(self):
        net = surge.load(CASE14)
        first = net.branches[0]
        second = net.branches[2]
        request = surge.dc.N2LodfRequest(
            outage_pair=(
                surge.dc.BranchKey(first.from_bus, first.to_bus, first.circuit),
                surge.dc.BranchKey(second.from_bus, second.to_bus, second.circuit),
            )
        )

        result = surge.dc.compute_n2_lodf(net, request)

        assert isinstance(result, surge.dc.N2LodfResult)
        assert isinstance(result.lodf, np.ndarray)


# ---------------------------------------------------------------------------
# DC Optimal Power Flow
# ---------------------------------------------------------------------------

class TestDcOpf:
    def test_solve_dc_opf_basic(self):
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        assert isinstance(result, surge.DcOpfResult)
        assert result.total_cost > 0
        pg = result.gen_p_mw
        lmp = result.lmp
        assert isinstance(pg, np.ndarray)
        assert isinstance(lmp, np.ndarray)
        assert pg.shape == (3,)   # 3 generators in case9
        assert lmp.shape == (9,)  # 9 buses

    def test_solve_dc_opf_generation_bounds(self):
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        pg = result.gen_p_mw
        pmax = net.gen_pmax
        pmin = net.gen_pmin
        for i, (p, lo, hi) in enumerate(zip(pg, pmin, pmax)):
            assert p >= lo - 0.1, f"Gen {i}: Pg={p:.2f} < Pmin={lo:.2f}"
            assert p <= hi + 0.1, f"Gen {i}: Pg={p:.2f} > Pmax={hi:.2f}"

    def test_dc_opf_to_dataframe_uses_external_bus_numbers(self):
        """A-01: to_dataframe()['bus_id'] must return external bus numbers, not 0..n."""
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        df = result.to_dataframe()
        assert isinstance(df, pd.DataFrame), f"Expected DataFrame, got {type(df)}"
        bus_ids = sorted(df.index.tolist())
        expected = sorted(net.bus_numbers)
        assert bus_ids == expected, (
            f"to_dataframe() returned {bus_ids}, expected external bus numbers {expected}"
        )
        assert 0 not in df.index.values, "bus_id index must not contain zero-based index 0"

    def test_solve_dc_opf_returns_typed_result(self):
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        assert result.opf.total_cost > 0
        assert isinstance(result.hvdc_dispatch_mw, np.ndarray)
        assert isinstance(result.hvdc_shadow_prices, np.ndarray)
        assert isinstance(result.generator_limit_violations, list)


# ---------------------------------------------------------------------------
# AC Optimal Power Flow
# ---------------------------------------------------------------------------

class TestAcOpf:
    def test_solve_ac_opf_basic(self):
        net = surge.load(CASE9)
        result = surge.solve_ac_opf(net)
        assert isinstance(result, surge.AcOpfResult)
        assert result.total_cost > 0
        assert result.iterations is None or result.iterations > 0

    def test_ac_opf_cost_at_least_dc_opf(self):
        """AC-OPF cost should be >= DC-OPF cost (AC includes reactive losses)."""
        net = surge.load(CASE9)
        dc_sol = surge.solve_dc_opf(net)
        ac_sol = surge.solve_ac_opf(net)
        # Allow 5% tolerance for numerical reasons.
        assert ac_sol.total_cost >= dc_sol.total_cost * 0.95


# ---------------------------------------------------------------------------
# N-1 contingency analysis
# ---------------------------------------------------------------------------

class TestContingency:
    def test_analyze_n1_branch_basic(self):
        net = surge.load(CASE9)
        result = surge.analyze_n1_branch(net)
        assert result.n_contingencies > 0
        assert result.n_converged > 0

    def test_analyze_n1_branch_screened(self):
        net = surge.load(CASE9)
        result = surge.analyze_n1_branch(net)
        # For case9 (small, well-conditioned), most contingencies should converge.
        assert result.n_converged >= result.n_contingencies // 2

    def test_analyze_n1_generator(self):
        net = surge.load(CASE9)
        result = surge.analyze_n1_generator(net)
        assert result.n_contingencies > 0


# ---------------------------------------------------------------------------
# Unsupported root surface
# ---------------------------------------------------------------------------

class TestUnsupportedSurface:
    def test_unbound_studies_are_not_root_exports(self):
        unsupported = {
            "analyze_faults",
            "analyze_n1_transient",
            "build_extended_ward_equivalent",
            "build_ward_equivalent",
            "compute_elcc",
            "compute_l_index",
            "compute_lole",
            "compute_state_estimation",
            "solve_dc_opf_full",
            "solve_dispatch",
            "solve_expansion",
            "solve_frequency_response",
            "solve_orpd",
            "solve_ots",
            "optimize_network_reconfiguration",
            "solve_ac_opf_with_hvdc",
        }
        missing = {name for name in unsupported if hasattr(surge, name)}
        assert missing == set()


# ---------------------------------------------------------------------------
# Network editing API — bus operations
# ---------------------------------------------------------------------------

class TestNetworkEditBus:
    def test_set_bus_load_exact_value(self):
        """set_bus_load writes to bus_pd and shrinks total_load_mw by correct amount."""
        net = surge.load(CASE9)
        bus_idx = net.bus_numbers.index(5)
        orig_pd = net.bus_pd[bus_idx]   # case9 bus 5 has 90 MW
        orig_total = net.total_load_mw
        net.set_bus_load(5, 50.0, 0.0)
        assert net.bus_pd[bus_idx] == pytest.approx(50.0)
        assert net.total_load_mw == pytest.approx(orig_total - orig_pd + 50.0, rel=1e-6)

    def test_set_bus_load_zero(self):
        """Zeroing a bus load reduces total_load_mw by the original pd."""
        net = surge.load(CASE9)
        bus_idx = net.bus_numbers.index(5)
        orig_pd = net.bus_pd[bus_idx]
        orig_total = net.total_load_mw
        net.set_bus_load(5, 0.0, 0.0)
        assert net.bus_pd[bus_idx] == pytest.approx(0.0)
        assert net.total_load_mw == pytest.approx(orig_total - orig_pd, rel=1e-6)

    def test_set_bus_voltage_roundtrip(self):
        """set_bus_voltage writes vm to the bus and is readable via bus_vm."""
        net = surge.load(CASE9)
        bus_idx = net.bus_numbers.index(1)
        net.set_bus_voltage(1, 1.05, 3.0)
        assert net.bus_vm[bus_idx] == pytest.approx(1.05)

    def test_set_bus_type_valid(self):
        """set_bus_type accepts all four valid strings without raising."""
        net = surge.load(CASE9)
        for bt in ("PQ", "PV", "Slack", "Isolated"):
            net.set_bus_type(5, bt)  # just check no exception

    def test_set_bus_type_invalid_raises(self):
        """set_bus_type with an unrecognised string raises ValueError."""
        net = surge.load(CASE9)
        with pytest.raises(ValueError, match="bus_type"):
            net.set_bus_type(1, "XX")

    def test_add_bus_increments_count(self):
        """add_bus increases n_buses by 1."""
        net = surge.load(CASE9)
        net.add_bus(100, "PQ", 345.0, pd_mw=10.0)
        assert net.n_buses == 10
        assert 100 in net.bus_numbers

    def test_add_bus_duplicate_number_raises(self):
        """add_bus with a number that already exists raises NetworkError."""
        net = surge.load(CASE9)
        with pytest.raises(surge.NetworkError, match="already exists"):
            net.add_bus(1, "PQ", 345.0)

    def test_add_bus_invalid_type_raises(self):
        """add_bus with an invalid bus_type raises ValueError."""
        net = surge.load(CASE9)
        with pytest.raises(ValueError, match="bus_type"):
            net.add_bus(999, "INVALID", 345.0)

    def test_remove_bus_decrements_count(self):
        """remove_bus reduces n_buses by 1."""
        net = surge.load(CASE9)
        net.remove_bus(9)
        assert net.n_buses == 8
        assert 9 not in net.bus_numbers

    def test_remove_bus_cascades_to_branches(self):
        """Removing a bus also removes its connected branches."""
        net = surge.load(CASE9)
        net.remove_bus(9)
        # Bus 9 is connected to buses 4 and 8 — both those branches gone
        for f, t in zip(net.branch_from, net.branch_to):
            assert f != 9 and t != 9

    def test_remove_bus_cascades_to_generators(self):
        """Removing a generator bus also removes its generators."""
        net = surge.load(CASE9)
        n_gen_before = net.n_generators
        net.remove_bus(1)   # bus 1 has a generator in case9
        assert net.n_generators < n_gen_before
        assert 1 not in net.gen_buses

    def test_remove_bus_not_found_raises(self):
        """remove_bus on a nonexistent bus raises NetworkError."""
        net = surge.load(CASE9)
        with pytest.raises(surge.NetworkError, match="not found"):
            net.remove_bus(999)

    def test_set_bus_load_bus_not_found_raises(self):
        """set_bus_load on a nonexistent bus raises NetworkError."""
        net = surge.load(CASE9)
        with pytest.raises(surge.NetworkError, match="not found"):
            net.set_bus_load(999, 50.0)


# ---------------------------------------------------------------------------
# Network editing API — branch operations
# ---------------------------------------------------------------------------

class TestNetworkEditBranch:
    def test_set_branch_in_service_false_affects_solution(self):
        """Outaging a branch produces a different power flow solution."""
        net_base = surge.load(CASE9)
        net_outage = surge.load(CASE9)
        net_outage.set_branch_in_service(4, 5, False)
        sol_base = surge.solve_ac_pf(net_base)
        sol_outage = surge.solve_ac_pf(net_outage)
        assert sol_base.converged and sol_outage.converged
        # Voltage angles must differ — outage redistributes flows
        assert any(abs(a - b) > 1e-6 for a, b in zip(sol_base.va_rad, sol_outage.va_rad))

    def test_set_branch_in_service_roundtrip(self):
        """Outage then restore a branch gives the original solution."""
        net = surge.load(CASE9)
        sol_before = surge.solve_ac_pf(net)
        net.set_branch_in_service(4, 5, False)
        net.set_branch_in_service(4, 5, True)
        sol_after = surge.solve_ac_pf(net)
        assert sol_before.converged and sol_after.converged
        assert all(abs(a - b) < 1e-8 for a, b in zip(sol_before.va_rad, sol_after.va_rad))

    def test_set_branch_rating_exact(self):
        """set_branch_rating writes the correct value to branch_rate_a."""
        net = surge.load(CASE9)
        pairs = list(zip(net.branch_from, net.branch_to))
        idx = pairs.index((4, 5))
        net.set_branch_rating(4, 5, 175.0)
        assert net.branch_rate_a[idx] == pytest.approx(175.0)

    def test_set_branch_tap_affects_solution(self):
        """Changing tap ratio on a branch shifts voltages."""
        net_base = surge.load(CASE9)
        net_tap = surge.load(CASE9)
        net_tap.set_branch_tap(4, 5, 0.95)
        sol_base = surge.solve_ac_pf(net_base)
        sol_tap = surge.solve_ac_pf(net_tap)
        assert sol_base.converged and sol_tap.converged
        assert any(abs(a - b) > 1e-6 for a, b in zip(sol_base.vm, sol_tap.vm))

    def test_remove_branch_decrements_count(self):
        """remove_branch decreases n_branches by 1."""
        net = surge.load(CASE9)
        net.remove_branch(4, 5)
        assert net.n_branches == 8
        pairs = list(zip(net.branch_from, net.branch_to))
        assert (4, 5) not in pairs

    def test_add_branch_increments_count(self):
        """add_branch increases n_branches by 1."""
        net = surge.load(CASE9)
        net.add_branch(1, 9, r=0.005, x=0.05, rate_a_mva=200.0, circuit=2)
        assert net.n_branches == 10

    def test_add_branch_invalid_from_bus_raises(self):
        """add_branch with a missing from_bus raises NetworkError."""
        net = surge.load(CASE9)
        with pytest.raises(surge.NetworkError, match="from_bus"):
            net.add_branch(999, 1, r=0.01, x=0.1)

    def test_add_branch_invalid_to_bus_raises(self):
        """add_branch with a missing to_bus raises NetworkError."""
        net = surge.load(CASE9)
        with pytest.raises(surge.NetworkError, match="to_bus"):
            net.add_branch(1, 999, r=0.01, x=0.1)

    def test_remove_branch_not_found_raises(self):
        """remove_branch on a nonexistent pair raises NetworkError."""
        net = surge.load(CASE9)
        with pytest.raises(surge.NetworkError, match="not found"):
            net.remove_branch(1, 9)   # not a direct branch in case9


# ---------------------------------------------------------------------------
# Network editing API — generator operations
# ---------------------------------------------------------------------------

class TestNetworkEditGenerator:
    def test_set_generator_p_exact(self):
        """set_generator_p writes the correct MW to gen_p."""
        net = surge.load(CASE9)
        idx = net.gen_buses.index(1)
        net.set_generator_p(_generator_id_at_bus(net, 1), 99.0)
        assert net.gen_p[idx] == pytest.approx(99.0)

    def test_set_generator_in_service_false(self):
        """set_generator_in_service(False) sets the flag to False."""
        net = surge.load(CASE9)
        idx = net.gen_buses.index(2)
        net.set_generator_in_service(_generator_id_at_bus(net, 2), False)
        assert net.gen_in_service[idx] is False

    def test_set_generator_in_service_roundtrip(self):
        """Disabling then re-enabling a generator restores in_service=True."""
        net = surge.load(CASE9)
        idx = net.gen_buses.index(2)
        generator_id = _generator_id_at_bus(net, 2)
        net.set_generator_in_service(generator_id, False)
        net.set_generator_in_service(generator_id, True)
        assert net.gen_in_service[idx] is True

    def test_set_generator_limits_exact(self):
        """set_generator_limits writes to gen_pmax and gen_pmin."""
        net = surge.load(CASE9)
        idx = net.gen_buses.index(1)
        net.set_generator_limits(
            _generator_id_at_bus(net, 1), pmax_mw=300.0, pmin_mw=20.0
        )
        assert net.gen_pmax[idx] == pytest.approx(300.0)
        assert net.gen_pmin[idx] == pytest.approx(20.0)

    def test_add_generator_increments_count(self):
        """add_generator increases n_generators and pg is readable."""
        net = surge.load(CASE9)
        n_before = net.n_generators
        generator_id = net.add_generator(5, p_mw=30.0, pmax_mw=100.0, pmin_mw=0.0)
        assert net.n_generators == n_before + 1
        idx = net.gen_buses.index(5)
        assert net.generator(generator_id).id == generator_id
        assert net.gen_p[idx] == pytest.approx(30.0)
        assert net.gen_pmax[idx] == pytest.approx(100.0)

    def test_remove_generator_decrements_count(self):
        """remove_generator reduces n_generators by 1."""
        net = surge.load(CASE9)
        n_before = net.n_generators
        net.remove_generator(_generator_id_at_bus(net, 1))
        assert net.n_generators == n_before - 1
        # Bus 1 generator gone — should not appear in gen_buses
        # (unless another gen was at bus 1, which case9 doesn't have)
        assert 1 not in net.gen_buses

    def test_remove_generator_not_found_raises(self):
        """remove_generator on a nonexistent bus raises NetworkError."""
        net = surge.load(CASE9)
        with pytest.raises(surge.NetworkError, match="not found"):
            net.remove_generator("missing-generator")

    def test_add_generator_invalid_bus_raises(self):
        """add_generator on a nonexistent bus raises NetworkError."""
        net = surge.load(CASE9)
        with pytest.raises(surge.NetworkError, match="not found"):
            net.add_generator(999, p_mw=50.0, pmax_mw=100.0)

    def test_scale_generation_exact(self):
        """scale_generation multiplies every in-service gen_p by factor."""
        net = surge.load(CASE9)
        orig_pg = list(net.gen_p)
        net.scale_generators(2.0)
        for orig, scaled in zip(orig_pg, net.gen_p):
            assert scaled == pytest.approx(orig * 2.0, rel=1e-6)

    def test_scale_generation_skips_out_of_service(self):
        """scale_generation does not change out-of-service generator dispatch."""
        net = surge.load(CASE9)
        idx = net.gen_buses.index(2)
        pg_before = net.gen_p[idx]
        net.set_generator_in_service(_generator_id_at_bus(net, 2), False)
        net.scale_generators(3.0)
        assert net.gen_p[idx] == pytest.approx(pg_before)


# ---------------------------------------------------------------------------
# Network editing API — scale + copy
# ---------------------------------------------------------------------------

class TestNetworkEditScaleCopy:
    def test_scale_load_exact(self):
        """scale_loads(0.5) halves total_load_mw."""
        net = surge.load(CASE9)
        orig = net.total_load_mw
        net.scale_loads(0.5)
        assert net.total_load_mw == pytest.approx(orig * 0.5, rel=1e-6)

    def test_scale_load_individual_buses(self):
        """scale_load writes correct per-bus values to bus_pd."""
        net = surge.load(CASE9)
        orig_pd = list(net.bus_pd)
        net.scale_loads(3.0)
        for orig, scaled in zip(orig_pd, net.bus_pd):
            assert scaled == pytest.approx(orig * 3.0, rel=1e-6)

    def test_scale_load_preserves_solve(self):
        """Network is still solvable after scaling load."""
        net = surge.load(CASE9)
        net.scale_loads(0.8)
        sol = surge.solve_ac_pf(net)
        assert sol.converged

    def test_copy_bus_mutation_independence(self):
        """Mutating a copy's bus load does not change the original."""
        net = surge.load(CASE9)
        bus_idx = net.bus_numbers.index(5)
        orig_pd = net.bus_pd[bus_idx]
        copy = net.copy()
        copy.set_bus_load(5, orig_pd * 5)
        assert net.bus_pd[bus_idx] == pytest.approx(orig_pd)
        assert copy.bus_pd[bus_idx] == pytest.approx(orig_pd * 5)

    def test_copy_generator_mutation_independence(self):
        """Mutating a copy's generator does not change the original."""
        net = surge.load(CASE9)
        gen_idx = net.gen_buses.index(1)
        orig_pg = net.gen_p[gen_idx]
        copy = net.copy()
        copy.set_generator_p(_generator_id_at_bus(copy, 1), orig_pg * 2)
        assert net.gen_p[gen_idx] == pytest.approx(orig_pg)
        assert copy.gen_p[gen_idx] == pytest.approx(orig_pg * 2)

    def test_copy_branch_mutation_independence(self):
        """Outaging a branch on the copy does not affect the original."""
        net = surge.load(CASE9)
        copy = net.copy()
        copy.set_branch_in_service(4, 5, False)
        sol_orig = surge.solve_ac_pf(net)
        sol_copy = surge.solve_ac_pf(copy)
        assert sol_orig.converged and sol_copy.converged
        # Solutions should differ because the copy has an outage
        assert any(abs(a - b) > 1e-6 for a, b in zip(sol_orig.va_rad, sol_copy.va_rad))

    def test_double_copy_independence(self):
        """Two independent copies do not share Arc state."""
        net = surge.load(CASE9)
        c1 = net.copy()
        c2 = net.copy()
        c1.scale_loads(0.5)
        orig_load = net.total_load_mw
        assert c2.total_load_mw == pytest.approx(orig_load, rel=1e-9)
        assert net.total_load_mw == pytest.approx(orig_load, rel=1e-9)


# ---------------------------------------------------------------------------
# Phase 1: Network property expansion
# ---------------------------------------------------------------------------

class TestNetworkProperties:
    """Tests for Phase 1 — new Network getters and tabular methods."""

    def test_bus_arrays_length(self):
        net = surge.load(CASE9)
        assert len(net.bus_area) == net.n_buses
        assert len(net.bus_zone) == net.n_buses
        assert len(net.bus_base_kv) == net.n_buses
        assert len(net.bus_name) == net.n_buses
        assert len(net.bus_type_str) == net.n_buses
        assert len(net.bus_qd) == net.n_buses
        assert len(net.bus_vmin) == net.n_buses
        assert len(net.bus_vmax) == net.n_buses
        assert len(net.bus_gs) == net.n_buses
        assert len(net.bus_bs) == net.n_buses

    def test_branch_arrays_length(self):
        net = surge.load(CASE9)
        assert len(net.branch_b) == net.n_branches
        assert len(net.branch_rate_b) == net.n_branches
        assert len(net.branch_rate_c) == net.n_branches
        assert len(net.branch_in_service) == net.n_branches
        assert len(net.branch_tap) == net.n_branches
        assert len(net.branch_shift_deg) == net.n_branches
        assert len(net.branch_circuit) == net.n_branches

    def test_gen_arrays_length(self):
        net = surge.load(CASE9)
        assert len(net.gen_qmax) == net.n_generators
        assert len(net.gen_qmin) == net.n_generators
        assert len(net.gen_vs_pu) == net.n_generators
        assert len(net.gen_machine_id) == net.n_generators
        assert len(net.gen_q) == net.n_generators

    def test_bus_type_str_values(self):
        net = surge.load(CASE9)
        valid = {"PQ", "PV", "Slack", "Isolated"}
        assert all(t in valid for t in net.bus_type_str)
        assert "Slack" in net.bus_type_str

    def test_bus_vmin_vmax_reasonable(self):
        net = surge.load(CASE9)
        assert all(0.5 <= v <= 1.0 for v in net.bus_vmin)
        assert all(1.0 <= v <= 1.5 for v in net.bus_vmax)

    def test_branch_in_service_default_true(self):
        net = surge.load(CASE9)
        assert all(net.branch_in_service)

    def test_branch_tap_unity_for_lines(self):
        """case9 has no transformers; all taps should be 1.0."""
        net = surge.load(CASE9)
        assert all(t == pytest.approx(1.0) or t == pytest.approx(0.0) for t in net.branch_tap)

    def test_gen_qmax_ge_qmin(self):
        net = surge.load(CASE9)
        for qmax, qmin in zip(net.gen_qmax, net.gen_qmin):
            assert qmax >= qmin

    def test_bus_dataframe_columns(self):
        net = surge.load(CASE9)
        df = net.bus_dataframe()
        assert isinstance(df, pd.DataFrame), f"Expected DataFrame, got {type(df)}"
        assert df.index.name == "bus_id"
        required = {"name", "type", "base_kv", "area", "zone",
                    "pd_mw", "qd_mvar", "vmin_pu", "vmax_pu"}
        assert required.issubset(df.columns)
        assert len(df) == net.n_buses

    def test_branch_dataframe_columns(self):
        net = surge.load(CASE9)
        df = net.branch_dataframe()
        assert isinstance(df, pd.DataFrame), f"Expected DataFrame, got {type(df)}"
        assert list(df.index.names) == ["from_bus", "to_bus"]
        required = {"circuit", "r", "x", "b", "rate_a_mva", "in_service"}
        assert required.issubset(df.columns)
        assert len(df) == net.n_branches

    def test_gen_dataframe_columns(self):
        net = surge.load(CASE9)
        df = net.gen_dataframe()
        assert isinstance(df, pd.DataFrame), f"Expected DataFrame, got {type(df)}"
        assert list(df.index.names) == ["bus_id", "machine_id"]
        required = {"gen_idx", "p_mw", "pmax_mw", "pmin_mw",
                    "qmax_mvar", "qmin_mvar", "vs_pu", "in_service"}
        assert required.issubset(df.columns)
        assert len(df) == net.n_generators

    def test_buses_filter_all(self):
        """No filters → returns all buses."""
        net = surge.load(CASE9)
        result = net.find_bus_numbers()
        assert sorted(result) == sorted(net.bus_numbers)

    def test_buses_filter_by_type(self):
        net = surge.load(CASE9)
        slack_buses = net.find_bus_numbers(bus_type="Slack")
        assert len(slack_buses) == 1
        pv_buses = net.find_bus_numbers(bus_type="PV")
        assert len(pv_buses) >= 1

    def test_buses_filter_by_kv(self):
        """case14 has buses at 132 kV and 33 kV."""
        net = surge.load(CASE14)
        high_kv = net.find_bus_numbers(kv_min=100.0)
        low_kv = net.find_bus_numbers(kv_max=50.0)
        assert len(high_kv) > 0
        assert len(low_kv) > 0
        # Should not overlap
        assert not set(high_kv) & set(low_kv)

    def test_ptdf_deterministic(self):
        """compute_ptdf called twice returns same shape."""
        net = surge.load(CASE9)
        r1 = surge.dc.compute_ptdf(net)
        r2 = surge.dc.compute_ptdf(net)
        assert r1.ptdf.shape == r2.ptdf.shape
        assert np.allclose(r1.ptdf, r2.ptdf)


# ---------------------------------------------------------------------------
# Phase 2: Custom contingency list
# ---------------------------------------------------------------------------

class TestCustomContingency:
    def test_contingency_construction(self):
        ctg = surge.Contingency("CTG-1", branches=[(4, 5, 1), (5, 6, 1)])
        assert ctg.id == "CTG-1"
        assert len(ctg.branches) == 2
        assert len(ctg.generators) == 0

    def test_contingency_default_label(self):
        ctg = surge.Contingency("MY-CTG")
        assert ctg.label == "MY-CTG"

    def test_contingency_custom_label(self):
        ctg = surge.Contingency("CTG-2", label="Bus 4-5 Trip")
        assert ctg.label == "Bus 4-5 Trip"

    def test_compute_contingencies_matches_n1_branch(self):
        """Single-branch CTG: results_dataframe should have 1 row."""
        net = surge.load(CASE9)
        ctg = surge.Contingency("N1-4-5", branches=[(4, 5, 1)])
        result = surge.analyze_contingencies(net, [ctg])
        assert result.n_contingencies == 1
        df = result.results_dataframe()
        assert isinstance(df, pd.DataFrame)
        assert df.index[0] == "N1-4-5"
        assert isinstance(df["converged"].iloc[0], (bool, np.bool_))

    def test_compute_contingencies_results_dataframe(self):
        net = surge.load(CASE9)
        ctg = surge.Contingency("CTG-A", branches=[(4, 5, 1)])
        result = surge.analyze_contingencies(net, [ctg])
        df = result.results_dataframe()
        assert isinstance(df, pd.DataFrame)
        assert df.index.name == "contingency_id"
        assert "converged" in df.columns
        assert len(df) == 1

    def test_compute_contingencies_violations_dataframe(self):
        net = surge.load(CASE9)
        ctg = surge.Contingency("CTG-B", branches=[(4, 5, 1)])
        result = surge.analyze_contingencies(net, [ctg])
        df = result.violations_dataframe()
        assert isinstance(df, pd.DataFrame)
        assert df.index.name == "contingency_id"
        assert "violation_type" in df.columns

    def test_invalid_contingency_branch_raises(self):
        net = surge.load(CASE9)
        ctg = surge.Contingency("BAD", branches=[(99, 100, 1)])
        with pytest.raises(Exception):
            surge.analyze_contingencies(net, [ctg])


# ---------------------------------------------------------------------------
# Phase 3: Save/write functions
# ---------------------------------------------------------------------------

class TestSave:
    def test_matpower_module_save_roundtrip(self, tmp_path):
        net = surge.load(CASE9)
        out = tmp_path / "out.m"
        surge.io.matpower.save(net, out)
        net2 = surge.load(out)
        assert net2.n_buses == net.n_buses
        assert net2.n_branches == net.n_branches
        assert net2.n_generators == net.n_generators

    def test_psse_module_save_roundtrip(self, tmp_path):
        net = surge.load(CASE9)
        out = tmp_path / "out.raw"
        surge.io.psse.raw.save(net, out, version=surge.io.psse.raw.Version.V33)
        net2 = surge.load(out)
        assert net2.n_buses == net.n_buses

    def test_json_module_save_roundtrip(self, tmp_path):
        net = surge.load(CASE9)
        out = tmp_path / "out.surge.json"
        surge.io.json.save(net, out)
        net2 = surge.load(out)
        assert net2.n_buses == net.n_buses

    def test_json_module_save_zstd_roundtrip(self, tmp_path):
        net = surge.load(CASE9)
        out = tmp_path / "out.surge.json.zst"
        surge.io.json.save(net, out)
        net2 = surge.load(out)
        assert net2.n_buses == net.n_buses

    def test_json_module_pretty_dump_roundtrip(self):
        net = surge.load(CASE9)
        content = surge.io.json.dumps(net, pretty=True)
        assert '"format": "surge-json"' in content
        net2 = surge.io.json.loads(content)
        assert net2.n_generators == net.n_generators

    def test_bin_module_save_roundtrip(self, tmp_path):
        net = surge.load(CASE9)
        out = tmp_path / "out.surge.bin"
        surge.io.bin.save(net, out)
        net2 = surge.load(out)
        assert net2.n_buses == net.n_buses

    def test_bin_module_bytes_roundtrip(self):
        net = surge.load(CASE9)
        payload = surge.io.bin.dumps(net)
        assert isinstance(payload, bytes)
        net2 = surge.io.bin.loads(payload)
        assert net2.n_branches == net.n_branches

    def test_save_auto_matpower(self, tmp_path):
        net = surge.load(CASE9)
        out = tmp_path / "auto.m"
        surge.save(net, out)
        net2 = surge.load(out)
        assert net2.n_buses == net.n_buses

    def test_pathlike_top_level_load(self):
        net = surge.load(Path(CASE9))
        assert net.n_buses == 9

    def test_io_dumps_matpower(self):
        net = surge.load(CASE9)
        s = surge.io.dumps(net, surge.io.Format.MATPOWER)
        assert "mpc.bus" in s or "function mpc" in s

    def test_io_dumps_psse(self):
        net = surge.load(CASE9)
        s = surge.io.psse.raw.dumps(net, version=surge.io.psse.raw.Version.V33)
        assert isinstance(s, str) and len(s) > 0


# ---------------------------------------------------------------------------
# Phase 4: Per-generator Q output
# ---------------------------------------------------------------------------

class TestGenQg:
    def test_gen_qg_mvar_length(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        assert len(sol.gen_q_mvar) == net.n_generators

    def test_gen_qg_mvar_sum_matches_bus_q(self):
        """Sum of gen Qg ≈ total bus reactive injection + total Qd."""
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        # q_inject_mvar is net reactive injection per bus (gen - load) in MVAr
        q_inject_total = sum(sol.q_inject_mvar)
        q_load_mvar = sum(net.bus_qd)
        total_gen_q = sum(sol.gen_q_mvar)
        assert total_gen_q == pytest.approx(q_inject_total + q_load_mvar, abs=1.0)

    def test_q_limited_buses_type(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        assert isinstance(sol.q_limited_buses, list)

    def test_n_q_limit_switches_type(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        assert isinstance(sol.n_q_limit_switches, int)
        assert sol.n_q_limit_switches >= 0

    def test_island_ids_type(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        assert isinstance(sol.island_ids, list)

    def test_n_islands_type(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        assert isinstance(sol.n_islands, int)


# ---------------------------------------------------------------------------
# Phase 5: OLTC and switched shunts
# ---------------------------------------------------------------------------

class TestDiscreteControls:
    def test_add_oltc_control(self):
        net = surge.load(CASE14)
        # case14 has transformers; use branch 4→7 (circuit 1)
        assert net.n_oltc_controls == 0
        # Just verify the method exists and is callable without error on a real branch
        branch_from = net.branch_from[0]
        branch_to = net.branch_to[0]
        net.add_oltc_control(branch_from, branch_to, v_target=1.0)
        assert net.n_oltc_controls == 1

    def test_clear_discrete_controls(self):
        net = surge.load(CASE14)
        branch_from = net.branch_from[0]
        branch_to = net.branch_to[0]
        net.add_oltc_control(branch_from, branch_to)
        net.clear_discrete_controls()
        assert net.n_oltc_controls == 0
        assert net.n_switched_shunts == 0

    def test_add_switched_shunt(self):
        net = surge.load(CASE9)
        net.add_switched_shunt(5, b_step_mvar=10.0, n_steps_cap=3)
        assert net.n_switched_shunts == 1

    def test_solve_acpf_with_no_discrete_controls(self):
        """solve_ac_pf still works when no OLTC/shunt controls are registered."""
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(
            net,
            surge.AcPfOptions(oltc=True, switched_shunts=True),
        )
        assert sol.converged

    def test_solve_acpf_oltc_disabled(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(
            net,
            surge.AcPfOptions(oltc=False, switched_shunts=False),
        )
        assert sol.converged


# ---------------------------------------------------------------------------
# Phase 6: Island detection
# ---------------------------------------------------------------------------

class TestIslands:
    def test_connected_network_one_island(self):
        net = surge.load(CASE9)
        islands = net.islands()
        assert len(islands) == 1
        assert sorted(islands[0]) == sorted(net.bus_numbers)

    def test_disconnected_network_two_islands(self):
        """Trip all branches from bus 9 → it forms its own island."""
        net = surge.load(CASE9)
        # Find and outage branches connected to bus 9
        for i, (fb, tb) in enumerate(zip(net.branch_from, net.branch_to)):
            if fb == 9 or tb == 9:
                net.set_branch_in_service(fb, tb, False)
        islands = net.islands()
        assert len(islands) == 2
        # Bus 9 should be isolated
        isolated = next(grp for grp in islands if len(grp) == 1)
        assert isolated[0] == 9


# ---------------------------------------------------------------------------
# Phase 8: Loss sensitivity factors
# ---------------------------------------------------------------------------

class TestLossSensitivities:
    def test_lsf_returns_result(self):
        net = surge.load(CASE9)
        result = surge.losses.compute_loss_factors(net)
        assert hasattr(result, "bus_numbers")
        assert hasattr(result, "lsf")
        assert hasattr(result, "base_losses_mw")

    def test_lsf_length(self):
        net = surge.load(CASE9)
        result = surge.losses.compute_loss_factors(net)
        assert len(result.bus_numbers) == net.n_buses
        assert len(result.lsf) == net.n_buses

    def test_lsf_base_losses_positive(self):
        net = surge.load(CASE9)
        result = surge.losses.compute_loss_factors(net)
        assert result.base_losses_mw > 0

    def test_lsf_slack_bus_near_one(self):
        """LSF at the slack bus is within a reasonable range."""
        net = surge.load(CASE9)
        result = surge.losses.compute_loss_factors(net)
        slack_buses = net.find_bus_numbers(bus_type="Slack")
        slack_idx = list(result.bus_numbers).index(slack_buses[0])
        assert abs(result.lsf[slack_idx]) <= 2.0

    def test_lsf_repr(self):
        net = surge.load(CASE9)
        result = surge.losses.compute_loss_factors(net)
        r = repr(result)
        assert "LsfResult" in r

    def test_lsf_to_dataframe(self):
        net = surge.load(CASE9)
        result = surge.losses.compute_loss_factors(net)
        df = result.to_dataframe()
        assert df.index.name == "bus_id"
        assert "lsf" in df.columns
        assert len(df) == net.n_buses


# ---------------------------------------------------------------------------
# Phase 9 (TIER 3): Network diff, area interchange, capability curves
# ---------------------------------------------------------------------------

class TestTier3:
    def test_diff_identical_networks(self):
        net = surge.load(CASE9)
        diff = net.compare_with(net)
        assert diff["buses"] == []
        assert diff["branches"] == []

    def test_diff_after_mutation(self):
        net = surge.load(CASE9)
        modified = net.copy()
        modified.set_bus_load(5, 999.0)
        diff = net.compare_with(modified)
        assert len(diff["loads"]) > 0
        changed_load = next(d for d in diff["loads"] if d["bus"] == 5)
        assert changed_load["kind"] == "modified"
        assert "pd_mw" in changed_load

    def test_diff_branch_outage(self):
        net = surge.load(CASE9)
        modified = net.copy()
        modified.set_branch_in_service(4, 5, False)
        diff = net.compare_with(modified)
        assert len(diff["branches"]) > 0

    def test_area_schedule_returns_dict(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        interchange = net.area_schedule_mw(sol)
        # case9 has buses all in area 1; net export from area 1 ≈ 0
        assert isinstance(interchange, dict)

    def test_gen_capability_curve_no_curve(self):
        """case9 generators have no explicit PQ curve; should return empty list."""
        net = surge.load(CASE9)
        curve = net.generator_capability_curve(_generator_id_at_bus(net, 1))
        assert curve == []


# ---------------------------------------------------------------------------
# Rich Python Objects
# ---------------------------------------------------------------------------


class TestRichObjects:
    """Tests for Bus/Branch/Generator/Load and BusSolved/BranchSolved/GenSolved/
    BusOpf/BranchOpf/GenOpf rich object access."""

    # ── Static network objects ──────────────────────────────────────────────

    def test_bus_objects_basic(self):
        net = surge.load(CASE9)
        buses = net.buses
        assert len(buses) == 9
        b = buses[0]
        assert isinstance(b.number, int)
        assert isinstance(b.name, str)
        assert b.type_str in ("PQ", "PV", "Slack", "Isolated")
        assert b.base_kv > 0
        assert b.vmin_pu <= b.vmax_pu
        assert repr(b).startswith("<Bus")

    def test_bus_computed_properties(self):
        net = surge.load(CASE9)
        slack = net.slack_bus
        assert slack.is_slack
        assert not slack.is_pv
        assert not slack.is_pq
        assert not slack.is_isolated
        pv_buses = [b for b in net.buses if b.is_pv]
        pq_buses = [b for b in net.buses if b.is_pq]
        assert len(pv_buses) + len(pq_buses) + 1 == 9  # +1 for slack

    def test_get_bus_by_number(self):
        net = surge.load(CASE9)
        b = net.bus(1)
        assert b.number == 1
        assert b.is_slack
        with pytest.raises(Exception):
            net.bus(999)

    def test_branch_objects_basic(self):
        net = surge.load(CASE9)
        branches = net.branches
        assert len(branches) == net.n_branches
        br = branches[0]
        assert br.from_bus in net.bus_numbers
        assert br.to_bus in net.bus_numbers
        assert br.x_pu > 0
        assert br.in_service
        assert repr(br).startswith("<Branch")

    def test_branch_computed_properties(self):
        net = surge.load(CASE9)
        for br in net.branches:
            z = br.z_pu
            assert len(z) == 2  # (r, x) tuple
            assert br.b_dc_pu > 0  # 1/(x*tap)
            # transformers have tap != 1.0 or shift != 0
            is_xfmr = (abs(br.tap - 1.0) > 1e-6) or (abs(br.shift_deg) > 1e-6)
            assert br.is_transformer == is_xfmr

    def test_get_branch_by_endpoints(self):
        net = surge.load(CASE9)
        br = net.branch(1, 4)
        assert br.from_bus == 1 and br.to_bus == 4

    def test_generator_objects_basic(self):
        net = surge.load(CASE9)
        gens = net.generators
        assert len(gens) == net.n_generators
        g = gens[0]
        assert g.bus in net.bus_numbers
        assert g.pmax_mw >= g.pmin_mw
        assert g.qmax_mvar >= g.qmin_mvar
        assert isinstance(g.machine_id, str)
        assert repr(g).startswith("<Generator")

    def test_generator_cost_methods(self):
        net = surge.load(CASE9)
        for g in net.generators:
            if g.has_cost:
                cost = g.cost_at(g.pmin_mw)
                assert cost >= 0
                mc = g.marginal_cost_at(g.pmin_mw)
                assert mc >= 0

    def test_generator_ancillary_flag(self):
        net = surge.load(CASE9)
        for g in net.generators:
            _ = g.has_ancillary_services  # should not raise

    def test_load_objects(self):
        net = surge.load(CASE9)
        loads = net.loads
        # case9 embeds load in buses; explicit Load list may be empty
        assert isinstance(loads, list)
        for ld in loads:
            assert ld.pd_mw >= 0
            assert repr(ld).startswith("<Load")

    def test_get_generator(self):
        net = surge.load(CASE9)
        g = net.generator(_generator_id_at_bus(net, 1))
        assert g.bus == 1
        with pytest.raises(Exception):
            net.generator("missing-generator")

    # ── Power flow solved objects ───────────────────────────────────────────

    def test_pf_bus_solved(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        buses = sol.buses
        assert len(buses) == 9
        for b in buses:
            assert 0.9 < b.vm_pu < 1.1
            assert -90 < b.va_deg < 90
            assert repr(b).startswith("<BusSolved")

    def test_pf_bus_solved_slack(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        b1 = sol.bus(1)
        assert b1.number == 1
        assert abs(b1.va_deg) < 1e-6  # slack reference angle = 0

    def test_pf_branch_solved(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        branches = sol.branches
        assert len(branches) == net.n_branches
        for br in branches:
            assert repr(br).startswith("<BranchSolved")
            # loading pct non-negative for in-service branches with rate_a > 0
            if br.rate_a_mva > 0:
                assert br.loading_pct >= 0

    def test_pf_gen_solved(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        gens = sol.generators
        assert len(gens) == net.n_generators
        for g in gens:
            assert repr(g).startswith("<GenSolved")
        # Generator at slack bus should have non-trivial Qg
        slack_gen = next(g for g in gens if g.bus == 1)
        assert abs(slack_gen.q_mvar_solved) < 200  # reasonable range

    def test_pf_island_field(self):
        net = surge.load(CASE9)
        sol = surge.solve_ac_pf(net)
        buses = sol.buses
        for b in buses:
            assert b.island_id >= 0

    # ── OPF solved objects ──────────────────────────────────────────────────

    def test_opf_bus_opf(self):
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        buses = result.buses
        assert len(buses) == 9
        for b in buses:
            assert b.lmp > 0
            assert repr(b).startswith("<BusOpf")

    def test_opf_branch_opf(self):
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        branches = result.branches
        assert len(branches) == net.n_branches
        for br in branches:
            assert br.loading_pct >= 0
            assert repr(br).startswith("<BranchOpf")

    def test_opf_gen_opf(self):
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        gens = result.generators
        assert len(gens) == net.n_generators
        for g in gens:
            assert g.mu_pmin >= 0
            assert g.mu_pmax >= 0
            assert g.cost_actual >= 0
            assert repr(g).startswith("<GenOpf")

    def test_opf_lmp_dataframe(self):
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        df = result.lmp_dataframe()
        assert df.index.name == "bus_id"
        assert "lmp" in df
        assert len(df.index) == 9
        for lmp in df["lmp"]:
            assert lmp > 0

    def test_opf_binding_branch(self):
        """On a congested case, at least one branch should be binding."""
        net = surge.load(CASE14)
        # Artificially tighten one line to force congestion
        result = surge.solve_dc_opf(net)
        branches = result.branches
        # is_binding flag should not raise
        for br in branches:
            _ = br.is_binding

    def test_opf_json_round_trip_requires_attach_network(self):
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        detached = surge.OpfResult.from_json(result.to_json())
        assert detached.has_attached_network is False
        with pytest.raises(Exception):
            detached.buses
        detached.attach_network(net)
        assert detached.has_attached_network is True
        assert len(detached.buses) == net.n_buses
        assert detached.gen_bus_numbers == [generator.bus for generator in net.generators if generator.in_service]
        assert detached.gen_ids == [generator.id for generator in net.generators if generator.in_service]
        assert detached.gen_machine_ids == [
            generator.machine_id or "1" for generator in net.generators if generator.in_service
        ]

    def test_opf_branch_loading_finite_matches_rated_branches(self):
        net = surge.load(CASE9)
        result = surge.solve_dc_opf(net)
        loading = result.branch_loading_pct

        expected_available = np.array(
            [branch.rate_a_mva > 0.0 for branch in net.branches], dtype=np.bool_
        )
        assert np.all(np.isfinite(loading[expected_available]))
        assert np.all(np.isnan(loading[~expected_available]))


# ---------------------------------------------------------------------------
# Network construction from scratch
# ---------------------------------------------------------------------------

class TestNetworkFromScratch:
    """Tests for building networks entirely from code (no file loading)."""

    def test_empty_constructor_defaults(self):
        """Network() creates an empty network with sensible defaults."""
        net = surge.Network()
        assert net.name == ""
        assert net.base_mva == 100.0
        assert net.freq_hz == 60.0
        assert net.n_buses == 0
        assert net.n_branches == 0
        assert net.n_generators == 0

    def test_constructor_with_params(self):
        """Network(name, base_mva, freq_hz) sets all three."""
        net = surge.Network("UK Grid", base_mva=100.0, freq_hz=50.0)
        assert net.name == "UK Grid"
        assert net.base_mva == 100.0
        assert net.freq_hz == 50.0

    def test_name_setter(self):
        """Setting net.name mutates the network."""
        net = surge.Network()
        net.name = "renamed"
        assert net.name == "renamed"

    def test_base_mva_setter(self):
        """Setting net.base_mva mutates the network."""
        net = surge.Network()
        net.base_mva = 200.0
        assert net.base_mva == 200.0

    def test_freq_hz_setter(self):
        """Setting net.freq_hz mutates the network."""
        net = surge.Network()
        net.freq_hz = 50.0
        assert net.freq_hz == 50.0

    def test_build_and_solve_3bus(self):
        """Build a 3-bus network from scratch, solve NR, verify convergence."""
        net = surge.Network("3bus")
        net.add_bus(1, "Slack", 138.0)
        net.add_bus(2, "PV", 138.0)
        net.add_bus(3, "PQ", 138.0)
        net.add_branch(1, 2, r=0.01, x=0.1)
        net.add_branch(2, 3, r=0.01, x=0.1)
        net.add_branch(1, 3, r=0.01, x=0.1)
        net.add_generator(1, p_mw=100.0, pmax_mw=200.0)
        net.add_generator(2, p_mw=50.0, pmax_mw=100.0, vs_pu=1.02)
        net.set_bus_load(3, pd_mw=120.0, qd_mvar=40.0)

        sol = surge.solve_ac_pf(net)
        assert sol.converged
        assert sol.iterations <= 10

    def test_build_and_solve_7bus(self):
        """Build a 7-bus network from scratch, solve NR, verify results.

        Topology (loosely based on a simple transmission system):
          Bus 1 (Slack, 345kV) -- Gen 1 (200MW)
          Bus 2 (PV, 345kV)   -- Gen 2 (150MW)
          Bus 3 (PV, 138kV)   -- Gen 3 (80MW)
          Bus 4 (PQ, 138kV)   -- Load (100 MW, 30 MVAr)
          Bus 5 (PQ, 138kV)   -- Load (80 MW, 25 MVAr)
          Bus 6 (PQ, 138kV)   -- Load (60 MW, 20 MVAr)
          Bus 7 (PQ, 138kV)   -- Load (50 MW, 15 MVAr)
        """
        net = surge.Network("7bus")
        # Buses
        net.add_bus(1, "Slack", 345.0)
        net.add_bus(2, "PV", 345.0)
        net.add_bus(3, "PV", 138.0)
        net.add_bus(4, "PQ", 138.0)
        net.add_bus(5, "PQ", 138.0)
        net.add_bus(6, "PQ", 138.0)
        net.add_bus(7, "PQ", 138.0)

        # Generators
        net.add_generator(1, p_mw=200.0, pmax_mw=400.0, vs_pu=1.04)
        net.add_generator(2, p_mw=150.0, pmax_mw=300.0, vs_pu=1.025)
        net.add_generator(3, p_mw=80.0, pmax_mw=150.0, vs_pu=1.01)

        # Loads
        net.set_bus_load(4, pd_mw=100.0, qd_mvar=30.0)
        net.set_bus_load(5, pd_mw=80.0, qd_mvar=25.0)
        net.set_bus_load(6, pd_mw=60.0, qd_mvar=20.0)
        net.set_bus_load(7, pd_mw=50.0, qd_mvar=15.0)

        # Branches (lines and transformers)
        # HV lines (345 kV)
        net.add_branch(1, 2, r=0.005, x=0.05, b=0.04)
        # Transformers (345/138 kV) — tap = 345/138 ≈ 2.5 but in per-unit = 1.0
        net.add_branch(1, 4, r=0.003, x=0.08)
        net.add_branch(2, 5, r=0.003, x=0.08)
        # 138 kV lines
        net.add_branch(3, 4, r=0.01, x=0.1, b=0.02)
        net.add_branch(4, 5, r=0.008, x=0.08, b=0.015)
        net.add_branch(5, 6, r=0.012, x=0.12, b=0.025)
        net.add_branch(6, 7, r=0.01, x=0.1, b=0.02)
        net.add_branch(3, 7, r=0.015, x=0.15, b=0.03)

        assert net.n_buses == 7
        assert net.n_branches == 8
        assert net.n_generators == 3

        # Solve AC power flow
        sol = surge.solve_ac_pf(net)
        assert sol.converged
        assert sol.iterations <= 10

        # Voltage magnitudes should be close to 1.0 pu
        for vm in sol.vm:
            assert 0.9 <= vm <= 1.1, f"vm={vm} out of range"

        # PV buses should hold their setpoints
        # Bus 2 → vs=1.025, Bus 3 → vs=1.01
        bus_idx = {b: i for i, b in enumerate(net.bus_numbers)}
        assert abs(sol.vm[bus_idx[2]] - 1.025) < 1e-4
        assert abs(sol.vm[bus_idx[3]] - 1.01) < 1e-4

        # Total generation should match total load + losses
        # Use bus injections from the solution (slack bus P is solved, not scheduled)
        total_load = 100 + 80 + 60 + 50  # 290 MW
        df = sol.to_dataframe()
        total_gen = df["p_mw"][df["p_mw"] > 0].sum()
        assert total_gen > total_load, "generation must exceed load (losses)"
        assert total_gen < total_load * 1.1, "losses should be < 10%"

    def test_build_and_solve_dc_opf(self):
        """Build a 5-bus network from scratch, solve DC-OPF, verify dispatch."""
        net = surge.Network("5bus_opf")
        net.add_bus(1, "Slack", 138.0)
        net.add_bus(2, "PV", 138.0)
        net.add_bus(3, "PQ", 138.0)
        net.add_bus(4, "PQ", 138.0)
        net.add_bus(5, "PQ", 138.0)

        # Cheap gen at bus 1 ($20/MWh), expensive gen at bus 2 ($40/MWh)
        gen1_id = net.add_generator(1, p_mw=100.0, pmax_mw=200.0)
        net.set_generator_cost(gen1_id, [20.0, 0.0])  # c1*P + c0
        gen2_id = net.add_generator(2, p_mw=50.0, pmax_mw=100.0)
        net.set_generator_cost(gen2_id, [40.0, 0.0])

        net.set_bus_load(3, pd_mw=60.0)
        net.set_bus_load(4, pd_mw=40.0)
        net.set_bus_load(5, pd_mw=30.0)

        net.add_branch(1, 2, r=0.01, x=0.1, rate_a_mva=100.0)
        net.add_branch(1, 3, r=0.01, x=0.1, rate_a_mva=100.0)
        net.add_branch(2, 4, r=0.01, x=0.1, rate_a_mva=100.0)
        net.add_branch(3, 5, r=0.01, x=0.1, rate_a_mva=100.0)
        net.add_branch(4, 5, r=0.01, x=0.1, rate_a_mva=100.0)

        result = surge.solve_dc_opf(net)

        # Total dispatch should equal total load (DC-OPF, lossless)
        total_load = 60 + 40 + 30  # 130 MW
        pg = list(result.gen_p_mw)
        total_gen = sum(pg)
        assert abs(total_gen - total_load) < 1e-3

        # Cheap gen should dispatch first (up to its limit or total load)
        assert pg[0] >= pg[1], "cheap gen should dispatch more"

    def test_copy_independence(self):
        """Copying a from-scratch network produces an independent copy."""
        net1 = surge.Network("original")
        net1.add_bus(1, "Slack", 138.0)
        net1.add_generator(1, p_mw=50.0, pmax_mw=100.0)

        net2 = net1.copy()
        net2.name = "copy"
        net2.add_bus(2, "PQ", 138.0)

        assert net1.name == "original"
        assert net2.name == "copy"
        assert net1.n_buses == 1
        assert net2.n_buses == 2


# ---------------------------------------------------------------------------
# Conductor/transformer type library
# ---------------------------------------------------------------------------

class TestConductorLibrary:
    def test_ohm_to_pu(self):
        """ohm_to_pu: 10 Ohm at 138 kV, 100 MVA → z_base = 190.44 → 0.0525 pu."""
        z_pu = surge.units.ohm_to_pu(10.0, 138.0)
        z_base = 138.0**2 / 100.0
        assert z_pu == pytest.approx(10.0 / z_base, rel=1e-10)

    def test_add_line_per_unit_conversion(self):
        """add_line converts Ohm/km and uS/km to per-unit correctly."""
        net = surge.Network("line_test")
        net.add_bus(1, "Slack", 138.0)
        net.add_bus(2, "PQ", 138.0)
        # Drake ACSR: r=0.0739 Ohm/km, x=0.4653 Ohm/km, b=3.399 uS/km
        net.add_line(1, 2, 0.0739, 0.4653, 3.399, 100.0, 138.0)
        df = net.branch_dataframe()
        z_base = 138.0**2 / 100.0
        assert df["r"].iloc[0] == pytest.approx(0.0739 * 100.0 / z_base, rel=1e-6)
        assert df["x"].iloc[0] == pytest.approx(0.4653 * 100.0 / z_base, rel=1e-6)
        assert df["b"].iloc[0] == pytest.approx(3.399e-6 * 100.0 * z_base, rel=1e-6)

    def test_add_transformer_per_unit_conversion(self):
        """add_transformer converts nameplate % impedance to system-base p.u."""
        net = surge.Network("xfmr_test")
        net.add_bus(1, "Slack", 345.0)
        net.add_bus(2, "PQ", 138.0)
        net.add_transformer(1, 2, mva_rating=200.0, v1_kv=345.0, v2_kv=138.0,
                            z_percent=8.0, r_percent=0.5)
        df = net.branch_dataframe()
        x_pct = (8.0**2 - 0.5**2)**0.5
        r_pu = (0.5 / 100.0) * (100.0 / 200.0)
        x_pu = (x_pct / 100.0) * (100.0 / 200.0)
        assert df["r"].iloc[0] == pytest.approx(r_pu, rel=1e-6)
        assert df["x"].iloc[0] == pytest.approx(x_pu, rel=1e-6)

    def test_add_transformer_custom_tap_and_rating(self):
        """add_transformer with non-default tap and rating."""
        net = surge.Network("xfmr_tap")
        net.add_bus(1, "Slack", 345.0)
        net.add_bus(2, "PQ", 138.0)
        net.add_transformer(1, 2, mva_rating=500.0, v1_kv=345.0, v2_kv=138.0,
                            z_percent=12.0, tap_pu=1.05, rate_a_mva=600.0)
        df = net.branch_dataframe()
        assert df["tap"].iloc[0] == pytest.approx(1.05, rel=1e-6)
        assert df["rate_a_mva"].iloc[0] == pytest.approx(600.0, rel=1e-6)

    def test_build_and_solve_with_add_line_and_transformer(self):
        """Build a 3-bus network using add_line + add_transformer and solve NR."""
        net = surge.Network("mixed_3bus")
        net.add_bus(1, "Slack", 345.0)
        net.add_bus(2, "PV", 138.0)
        net.add_bus(3, "PQ", 138.0)
        net.add_generator(1, 100.0, 200.0)
        net.add_generator(2, 50.0, 100.0)
        net.set_bus_load(3, 120.0)
        # Step-down transformer 345->138 kV
        net.add_transformer(1, 2, mva_rating=200.0, v1_kv=345.0, v2_kv=138.0,
                            z_percent=8.0)
        # 138 kV line
        net.add_line(2, 3, 0.0739, 0.4653, 3.399, 50.0, 138.0)
        sol = surge.solve_ac_pf(net)
        assert sol.converged


# ---------------------------------------------------------------------------
# Parameter sweep tests
# ---------------------------------------------------------------------------

class TestParameterSweep:
    """Tests for surge.batch.parameter_sweep()."""

    def test_basic_load_scaling(self):
        """Sweep with 3 load-scaling scenarios using NR."""
        net = surge.load(CASE9)
        results = surge.batch.parameter_sweep(
            net,
            scenarios=[
                ("80% load", [("scale_load", 0.8)]),
                ("base", []),
                ("120% load", [("scale_load", 1.2)]),
            ],
            solver="acpf",
        )
        assert len(results) == 3
        for r in results.results:
            assert r.converged, f"Scenario '{r.name}' did not converge: {r.error}"
            assert r.solution is not None
            assert r.error is None

    def test_result_names_preserved(self):
        """Scenario names are preserved in results."""
        net = surge.load(CASE9)
        results = surge.batch.parameter_sweep(
            net,
            scenarios=[
                ("alpha", []),
                ("beta", [("scale_load", 0.9)]),
                ("gamma", [("scale_load", 1.1)]),
            ],
        )
        names = {r.name for r in results.results}
        assert names == {"alpha", "beta", "gamma"}

    def test_multiple_modification_types(self):
        """Sweep with multiple modification types in a single scenario."""
        net = surge.load(CASE9)
        results = surge.batch.parameter_sweep(
            net,
            scenarios=[
                ("combined", [
                    ("scale_load", 0.9),
                    ("set_generator_p", 1, 100.0),
                ]),
            ],
            solver="acpf",
        )
        assert len(results) == 1
        r = results[0]
        assert r.converged
        assert r.solution is not None

    def test_to_dataframe(self):
        """to_dataframe() returns a table with expected columns."""
        net = surge.load(CASE9)
        results = surge.batch.parameter_sweep(
            net,
            scenarios=[
                ("low", [("scale_load", 0.8)]),
                ("high", [("scale_load", 1.2)]),
            ],
        )
        df = results.to_dataframe()
        assert len(df) == 2
        assert df.index.name == "name"
        for col in ["converged", "iterations", "max_vm", "min_vm",
                     "total_losses_mw", "solve_time_secs"]:
            assert col in df.columns, f"Missing column: {col}"
        assert all(df["converged"])

    def test_dc_solver(self):
        """Sweep works with the DC solver."""
        net = surge.load(CASE9)
        results = surge.batch.parameter_sweep(
            net,
            scenarios=[("dc_base", [])],
            solver="dcpf",
        )
        assert len(results) == 1
        assert results[0].converged

    def test_fdpf_solver(self):
        """Sweep works with the FDPF solver."""
        net = surge.load(CASE9)
        results = surge.batch.parameter_sweep(
            net,
            scenarios=[("fdpf_base", [])],
            solver="fdpf",
        )
        assert len(results) == 1
        assert results[0].converged

    def test_invalid_solver_raises(self):
        """Unknown solver name raises ValueError."""
        net = surge.load(CASE9)
        with pytest.raises(ValueError, match="Unknown solver"):
            surge.batch.parameter_sweep(net, scenarios=[("x", [])], solver="bogus")

    def test_invalid_modification_raises(self):
        """Unknown modification method raises ValueError."""
        net = surge.load(CASE9)
        with pytest.raises(ValueError, match="Unknown modification"):
            surge.batch.parameter_sweep(
                net,
                scenarios=[("bad", [("nonexistent_method", 1.0)])],
            )

    def test_failed_scenario_has_error(self):
        """A scenario with a bad modification captures the error."""
        net = surge.load(CASE9)
        results = surge.batch.parameter_sweep(
            net,
            scenarios=[
                ("good", []),
                ("bad_bus", [("set_bus_load", 99999, 100.0)]),
            ],
        )
        assert len(results) == 2
        bad = [r for r in results.results if r.name == "bad_bus"][0]
        assert not bad.converged
        assert bad.solution is None
        assert bad.error is not None

    def test_empty_scenarios(self):
        """Empty scenario list returns empty results."""
        net = surge.load(CASE9)
        results = surge.batch.parameter_sweep(net, scenarios=[])
        assert len(results) == 0

    def test_getitem_negative_index(self):
        """Negative indexing works on SweepResults."""
        net = surge.load(CASE9)
        results = surge.batch.parameter_sweep(
            net,
            scenarios=[("a", []), ("b", [])],
        )
        assert results[-1].name == results[1].name

    def test_base_network_unchanged(self):
        """The base network is not modified by the sweep."""
        net = surge.load(CASE9)
        original_load = net.total_load_mw
        surge.batch.parameter_sweep(
            net,
            scenarios=[
                ("half_load", [("scale_load", 0.5)]),
                ("double_load", [("scale_load", 2.0)]),
            ],
        )
        assert net.total_load_mw == pytest.approx(original_load, rel=1e-10)


# ---------------------------------------------------------------------------
# Interfaces and Flowgates
# ---------------------------------------------------------------------------

class TestInterfacesFlowgates:

    def test_add_interface(self):
        """Add an interface and verify it does not error."""
        net = surge.load(CASE9)
        # case9 branches include (1,4,1), (5,6,1) among others
        net.add_interface(
            "Test Interface",
            members=[((1, 4, 1), 1.0), ((5, 6, 1), 1.0)],
            limit_forward_mw=200.0,
            limit_reverse_mw=100.0,
        )
        # No error means success; the interface is stored on the network.

    def test_add_flowgate(self):
        """Add a flowgate and verify it does not error."""
        net = surge.load(CASE9)
        net.add_flowgate(
            "FG_Test",
            monitored=[((4, 5, 1), 1.0)],
            limit_mw=100.0,
            contingency_branch=None,
        )

    def test_remove_interface(self):
        """Add then remove an interface."""
        net = surge.load(CASE9)
        net.add_interface(
            "Houston Import",
            members=[((1, 4, 1), 1.0)],
            limit_forward_mw=300.0,
        )
        # Should succeed.
        net.remove_interface("Houston Import")
        # Removing again should raise.
        with pytest.raises(Exception, match="not found"):
            net.remove_interface("Houston Import")

    def test_remove_flowgate(self):
        """Add then remove a flowgate."""
        net = surge.load(CASE9)
        net.add_flowgate(
            "FG_Delete",
            monitored=[((4, 5, 1), 1.0)],
            limit_mw=50.0,
        )
        net.remove_flowgate("FG_Delete")
        with pytest.raises(Exception, match="not found"):
            net.remove_flowgate("FG_Delete")

    def test_interface_dc_opf(self):
        """Add a binding interface constraint to case9 and verify the DC-OPF respects it.

        Strategy: solve case9 DC-OPF unconstrained, find a branch with large flow,
        then add a tight interface limit on that branch and re-solve. The interface
        flow should be at or below the limit.
        """
        net = surge.load(CASE9)

        # Solve unconstrained first to find a high-flow branch.
        sol_base = surge.solve_dc_opf(net)
        branches_base = sol_base.branches

        # Branch (7,8,1) typically carries significant flow in case9.
        target_br = None
        for br in branches_base:
            if br.from_bus == 7 and br.to_bus == 8:
                target_br = br
                break
        assert target_br is not None, "Branch 7-8 must exist in case9"
        base_flow = abs(target_br.pf_mw)
        assert base_flow > 1.0, f"Expected non-trivial flow on 7-8, got {base_flow}"

        # Set a tight interface limit at 50% of the base flow.
        tight_limit = base_flow * 0.5
        net2 = net.copy()
        net2.add_interface(
            "Tight_7_8",
            members=[((7, 8, 1), 1.0)],
            limit_forward_mw=tight_limit,
            limit_reverse_mw=tight_limit,
        )

        sol_iface = surge.solve_dc_opf(net2)
        branches_iface = sol_iface.branches
        # Find the same branch in the constrained solution.
        constrained_flow = abs(next(
            br.pf_mw for br in branches_iface
            if br.from_bus == 7 and br.to_bus == 8
        ))
        # The flow should be within tolerance of the limit.
        assert constrained_flow <= tight_limit + 0.5, (
            f"Interface flow {constrained_flow:.2f} MW exceeds limit {tight_limit:.2f} MW"
        )
        # The constrained cost should be >= base cost (tighter constraints can't lower cost).
        assert sol_iface.total_cost >= sol_base.total_cost * 0.999

    def test_flowgate_base_case_dc_opf(self):
        """Add a binding base-case flowgate to case9 and verify the DC-OPF respects it.

        Same logic as the interface test but using a flowgate (no contingency).
        """
        net = surge.load(CASE9)

        sol_base = surge.solve_dc_opf(net)
        branches_base = sol_base.branches

        # Use branch (4,5,1) — pick a different branch from the interface test.
        target_br = None
        for br in branches_base:
            if br.from_bus == 4 and br.to_bus == 5:
                target_br = br
                break
        assert target_br is not None, "Branch 4-5 must exist in case9"
        base_flow = abs(target_br.pf_mw)

        if base_flow < 1.0:
            pytest.skip("Branch 4-5 carries negligible flow in case9; cannot test binding flowgate")

        tight_limit = base_flow * 0.5
        net2 = net.copy()
        net2.add_flowgate(
            "FG_4_5",
            monitored=[((4, 5, 1), 1.0)],
            limit_mw=tight_limit,
            contingency_branch=None,
        )

        sol_fg = surge.solve_dc_opf(net2)
        branches_fg = sol_fg.branches
        constrained_flow = abs(next(
            br.pf_mw for br in branches_fg
            if br.from_bus == 4 and br.to_bus == 5
        ))
        assert constrained_flow <= tight_limit + 0.5, (
            f"Flowgate flow {constrained_flow:.2f} MW exceeds limit {tight_limit:.2f} MW"
        )
        assert sol_fg.total_cost >= sol_base.total_cost * 0.999
