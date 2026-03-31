# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from pathlib import Path

import pytest

import surge


REPO_ROOT = Path(__file__).resolve().parents[3]
CASE9 = REPO_ROOT / "examples" / "cases" / "case9" / "case9.surge.json.zst"


def test_root_opf_surface_is_curated():
    exports = set(surge.__all__)

    assert {
        "DcOpfOptions",
        "DcOpfRuntime",
        "DcOpfResult",
        "AcOpfOptions",
        "AcOpfRuntime",
        "AcOpfResult",
        "ScopfOptions",
        "ScopfRuntime",
        "ScopfResult",
    } <= exports

    assert "solve_dc_opf_full" not in exports
    assert "solve_ac_opf_with_hvdc" not in exports
    assert "solve_ots" not in exports
    assert "solve_orpd" not in exports
    assert "optimize_network_reconfiguration" not in exports


def test_removed_specialist_optimization_studies_are_not_exported():
    assert not hasattr(surge, "optimization")
    assert not hasattr(surge, "solve_transmission_switching")
    assert not hasattr(surge, "solve_reactive_dispatch")
    assert not hasattr(surge, "solve_reconfiguration")


def test_opf_functions_reject_legacy_kwargs_overrides():
    net = surge.load(CASE9)

    with pytest.raises(TypeError, match="unexpected keyword argument"):
        surge.solve_dc_opf(net, tolerance=1e-6)


def test_batch_opf_requires_options_and_runtime_keywords():
    net = surge.load(CASE9)
    result = surge.batch.batch_solve([net], solver="dc-opf", tolerance=1e-6).results[0]
    assert "accepts only 'options' and 'runtime'" in result.error


def test_dc_opf_options_require_explicit_soft_limit_penalty():
    with pytest.raises((TypeError, ValueError), match="generator_limit_penalty_per_mw"):
        surge.DcOpfOptions(generator_limit_mode=surge.GeneratorLimitMode.SOFT)


def test_ac_opf_exposes_feasible_for_surface_symmetry():
    net = surge.load(CASE9)
    result = surge.solve_ac_opf(net)

    assert hasattr(result, "feasible")
    assert result.feasible is True


def test_batch_violations_reads_branch_loading_property():
    net = surge.load(CASE9)
    results = surge.batch.batch_solve([net], solver="acpf", parallel=False)

    violations = results.violations()

    assert list(violations.columns) == ["case", "type", "element", "value", "limit"]


def test_scopf_wrapper_uses_screening_stats_name():
    assert hasattr(surge.opf.ScopfResult, "screening_stats")
    assert not hasattr(surge.opf.ScopfResult, "screening")


def test_scopf_options_keep_pwl_default_with_dc_suboptions():
    kwargs = surge.ScopfOptions(
        dc_opf=surge.DcOpfOptions(loss_model=surge.DcLossModel.ITERATIVE),
    ).to_native_kwargs()

    assert kwargs["use_pwl_costs"] is True
    assert kwargs["use_loss_factors"] is True


def test_scopf_options_use_top_level_cost_model_override():
    kwargs = surge.ScopfOptions(
        cost_model=surge.DcCostModel.QUADRATIC,
        dc_opf=surge.DcOpfOptions(
            loss_model=surge.DcLossModel.ITERATIVE,
            piecewise_linear_breakpoints=33,
        ),
    ).to_native_kwargs()

    assert kwargs["use_pwl_costs"] is False
    assert kwargs["pwl_cost_breakpoints"] == 33
    assert kwargs["use_loss_factors"] is True
