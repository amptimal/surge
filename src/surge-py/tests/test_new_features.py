# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from pathlib import Path

import pytest

import surge


REPO_ROOT = Path(__file__).resolve().parents[3]
CASE9 = REPO_ROOT / "examples" / "cases" / "case9" / "case9.surge.json.zst"
CASE14 = REPO_ROOT / "examples" / "cases" / "case14" / "case14.surge.json.zst"


def test_curated_root_surface_excludes_unbound_studies():
    unsupported = {
        "analyze_faults",
        "analyze_n1_transient",
        "analyze_arcflash",
        "build_dynamic_equivalents",
        "build_extended_ward_equivalent",
        "build_ward_equivalent",
        "compute_elcc",
        "compute_gic",
        "compute_l_index",
        "compute_lole",
        "compute_lole_monte_carlo",
        "compute_state_estimation",
        "solve_distribution",
        "solve_expansion",
        "solve_frequency_response",
    }
    assert {name for name in unsupported if hasattr(surge, name)} == set()


def test_losses_module_uses_canonical_name():
    net = surge.load(CASE9)

    assert hasattr(surge, "losses")
    assert hasattr(surge.losses, "compute_loss_factors")
    assert not hasattr(surge.losses, "compute_factors")

    result = surge.losses.compute_loss_factors(net)
    assert result.base_losses_mw > 0.0


def test_network_uses_plural_scaling_names():
    net = surge.load(CASE9)
    assert hasattr(net, "scale_loads")
    assert hasattr(net, "scale_generators")
    assert not hasattr(net, "scale_load")
    assert not hasattr(net, "scale_generation")


def test_root_surface_excludes_legacy_solver_aliases():
    legacy = {
        "solve_acpf",
        "solve_dcpf",
        "solve_acopf",
        "solve_dcopf",
    }
    assert {name for name in legacy if hasattr(surge, name)} == set()


def test_voltage_stress_smoke():
    net = surge.load(CASE14)
    result = surge.contingency.compute_voltage_stress(net)

    assert result.max_l_index is not None
    assert len(result.per_bus) == net.n_buses


def test_contingency_helpers_round_trip(tmp_path):
    net = surge.load(CASE9)
    contingencies = surge.contingency_io.generate_n1_branch(net)
    path = tmp_path / "n1.json"

    surge.contingency_io.save_contingencies(contingencies, path, format="json")
    loaded = surge.contingency_io.load_contingencies(path)

    assert len(contingencies) == net.n_branches
    assert len(loaded) == len(contingencies)
    assert loaded[0].id == contingencies[0].id


def test_prepared_contingency_and_corrective_dispatch_smoke():
    net = surge.load(CASE9)
    study = surge.contingency.n1_branch_study(
        net,
        surge.ContingencyOptions(screening="lodf"),
    )

    analysis = study.analyze()
    corrective = study.solve_corrective_dispatch()

    assert analysis.n_contingencies > 0
    assert isinstance(corrective, list)


def test_transfer_study_smoke():
    net = surge.load(CASE9)
    path = surge.transfer.TransferPath("west_to_east", [1], [9])
    study = surge.transfer.prepare_transfer_study(net)

    atc = study.compute_nerc_atc(path)
    multi = surge.transfer.compute_multi_transfer(net, [path])

    assert atc.atc_mw >= 0.0
    assert len(multi.transfer_mw) == 1


def test_batch_solver_rejects_removed_solver_routes():
    results = surge.batch.batch_solve([CASE9], solver="cpf", parallel=False)

    assert len(results) == 1
    assert results[0].error is not None
    assert "Unknown solver" in results[0].error


def test_audit_surface_smoke():
    net = surge.load(CASE9)
    issues = surge.audit.audit_model(net)

    assert isinstance(issues, list)
