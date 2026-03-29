# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import inspect
from pathlib import Path

import surge


CASE9 = Path(__file__).resolve().parents[3] / "examples" / "cases" / "case9" / "case9.surge.json.zst"


def test_root_transfer_and_contingency_surface_is_curated():
    exports = set(surge.__all__)

    assert {"transfer", "contingency"} <= exports

    hidden = {
        "compute_bldf",
        "compute_gsf",
        "compute_injection_capability",
        "compute_nerc_atc",
        "compute_ac_atc",
        "compute_afc",
        "compute_multi_transfer",
        "prepare_transfer_study",
        "TransferPath",
        "Flowgate",
        "AtcOptions",
        "n1_branch_study",
        "n1_generator_study",
        "n2_branch_study",
        "prepare_corrective_dispatch_study",
        "solve_corrective_dispatch",
        "rank_contingencies",
        "compute_voltage_stress",
        "analyze_branch_eens",
    }
    assert hidden.isdisjoint(exports)


def test_transfer_namespace_is_canonical():
    exports = set(surge.transfer.__all__)

    assert {
        "TransferStudy",
        "TransferPath",
        "Flowgate",
        "AtcOptions",
        "compute_bldf",
        "compute_gsf",
        "compute_injection_capability",
        "compute_nerc_atc",
        "compute_ac_atc",
        "compute_afc",
        "compute_multi_transfer",
        "prepare_transfer_study",
    } <= exports


def test_transfer_injection_capability_signature_matches_native_surface():
    params = inspect.signature(surge.transfer.compute_injection_capability).parameters

    assert "slack_weights" in params
    assert "monitored_branches" in params
    assert "contingency_branches" in params

    net = surge.load(str(CASE9))
    result = surge.transfer.compute_injection_capability(
        net,
        slack_weights=[(1, 1.0)],
    )
    assert hasattr(result, "by_bus")
    assert hasattr(result, "failed_contingencies")
    assert len(result.by_bus) == net.n_buses


def test_contingency_namespace_holds_reusable_study_workflows():
    exports = set(surge.contingency.__all__)

    assert {
        "ContingencyStudy",
        "PreparedCorrectiveDispatchStudy",
        "n1_branch_study",
        "n1_generator_study",
        "n2_branch_study",
        "prepare_corrective_dispatch_study",
        "solve_corrective_dispatch",
        "rank_contingencies",
        "compute_voltage_stress",
        "analyze_branch_eens",
    } <= exports
