# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import inspect
from pathlib import Path

import surge


CASE9 = Path(__file__).resolve().parents[3] / "examples" / "cases" / "case9" / "case9.surge.json.zst"


def test_root_dc_surface_is_curated():
    exports = set(surge.__all__)
    names = set(dir(surge))

    assert {"DcPfOptions", "DcPfResult", "solve_dc_pf", "dc"} <= exports

    assert "compute_ptdf" not in exports
    assert "prepare_dc_study" not in exports
    assert "compute_lodf" not in exports
    assert "compute_lodf_matrix" not in exports
    assert "compute_n2_lodf" not in exports
    assert "compute_n2_lodf_batch" not in exports
    assert "compute_otdf" not in exports
    assert "run_dc_analysis" not in exports
    assert "PreparedDcStudy" not in exports
    assert "PtdfResult" not in exports
    assert "LodfResult" not in exports
    assert "LodfMatrixResult" not in exports
    assert "N2LodfResult" not in exports
    assert "N2LodfBatchResult" not in exports
    assert "OtdfResult" not in exports
    assert "DcAnalysisResult" not in exports

    assert "PreparedDcStudy" not in names
    assert "compute_ptdf" not in names
    assert "prepare_dc_study" not in names
    assert "run_dc_analysis" not in names


def test_dc_namespace_is_canonical():
    exports = set(surge.dc.__all__)

    assert {
        "BranchKey",
        "SlackPolicy",
        "PtdfRequest",
        "LodfRequest",
        "LodfMatrixRequest",
        "OtdfRequest",
        "N2LodfRequest",
        "N2LodfBatchRequest",
        "DcAnalysisRequest",
        "DcAnalysisResult",
        "PreparedDcStudy",
        "prepare_study",
        "compute_ptdf",
        "compute_lodf",
        "compute_lodf_matrix",
        "compute_otdf",
        "compute_n2_lodf",
        "compute_n2_lodf_batch",
        "run_analysis",
    } <= exports


def test_dc_pf_options_expose_angle_reference_and_participation_factors():
    signature = inspect.signature(surge.DcPfOptions)
    assert "participation_factors" in signature.parameters
    assert "angle_reference" in signature.parameters


def test_public_lodf_and_n2_signatures_do_not_expose_slack_knobs():
    lodf_params = set(inspect.signature(surge.dc.compute_lodf).parameters)
    n2_params = set(inspect.signature(surge.dc.compute_n2_lodf).parameters)
    prepared_lodf_params = set(inspect.signature(surge.dc.PreparedDcStudy.compute_lodf).parameters)
    prepared_n2_params = set(
        inspect.signature(surge.dc.PreparedDcStudy.compute_n2_lodf).parameters
    )

    for params in (lodf_params, n2_params, prepared_lodf_params, prepared_n2_params):
        assert "slack_weights" not in params
        assert "headroom_slack" not in params
        assert "headroom_slack_buses" not in params


def test_prepare_study_is_namespaced():
    assert hasattr(surge, "dc")
    assert hasattr(surge.dc, "prepare_study")
    assert not hasattr(surge, "prepare_dc_study")
    assert not hasattr(surge, "PreparedDcStudy")


def test_prepared_dc_study_solve_pf_smoke():
    study = surge.dc.prepare_study(surge.load(CASE9))
    result = study.solve_pf()

    assert result.solve_time_secs > 0
    assert len(result.va_rad) > 0
    assert len(result.branch_p_mw) > 0


def test_native_binding_does_not_export_removed_dc_helpers():
    assert not hasattr(surge._native, "compute_lodf_pairs")
    assert not hasattr(surge._native, "run_dc_analysis")
