# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from __future__ import annotations

import ast
import inspect
from pathlib import Path

import pytest
import surge


STUB_PATH = Path(__file__).resolve().parents[1] / "python" / "surge" / "_surge.pyi"
INIT_STUB_PATH = Path(__file__).resolve().parents[1] / "python" / "surge" / "__init__.pyi"
CASE9 = Path(__file__).resolve().parents[3] / "examples" / "cases" / "case9" / "case9.surge.json.zst"


def _parameter_names(callable_obj) -> list[str]:
    try:
        return list(inspect.signature(callable_obj).parameters)
    except (TypeError, ValueError):
        text_signature = getattr(callable_obj, "__text_signature__", "") or ""
        names: list[str] = []
        for part in text_signature.strip().strip("()").split(","):
            token = part.strip()
            if not token or token in {"self", "/", "*"}:
                continue
            names.append(token.split(":")[0].split("=")[0].strip())
        return names


def test_stub_surface_tracks_public_python_contract():
    stub_text = STUB_PATH.read_text()

    required_snippets = [
        "def set_generator_p(",
        "def add_facts_device(",
        "def remove_facts_device(self, name: str)",
        "def gen_p_mw(self) -> NDArray[np.float64]:",
        "def gen_q_mvar(self) -> NDArray[np.float64]:",
        'Python creation/update currently supports ``"Curtailable"`` and',
    ]
    for snippet in required_snippets:
        assert snippet in stub_text

    forbidden_snippets = [
        "def set_generator_pg(",
        "def pg_mw(self)",
        "def qg_mvar(self)",
        "def qg_mvar_solved(self)",
        "class DynamicEquivResult:",
        "def build_dynamic_equivalents(",
    ]
    for snippet in forbidden_snippets:
        assert snippet not in stub_text


def test_native_stub_parses_and_exposes_expected_top_level_classes():
    tree = ast.parse(STUB_PATH.read_text())
    top_level_classes = {
        node.name for node in ast.iter_child_nodes(tree) if isinstance(node, ast.ClassDef)
    }

    required_classes = {
        "BindingContingency",
        "ContingencyViolation",
        "ScopfScreeningStats",
        "FailedContingencyEvaluation",
        "DcOpfResult",
        "ScopfResult",
        "AcOpfHvdcResult",
        "OtsResult",
        "OrpdResult",
        "ReconfigResult",
    }

    missing = required_classes - top_level_classes
    assert not missing, f"_surge.pyi is missing parsed top-level classes: {sorted(missing)}"


def test_native_stub_tracks_scopf_metadata_fields():
    stub_text = STUB_PATH.read_text()

    required_snippets = [
        "cut_kind: str",
        "outaged_generator_indices: list[int]",
        "contingency_label: str",
        "outaged_generators: list[int]",
    ]
    for snippet in required_snippets:
        assert snippet in stub_text


def test_runtime_surface_tracks_public_python_contract():
    add_generator_params = _parameter_names(surge.Network.add_generator)
    assert "p_mw" in add_generator_params
    assert "pg_mw" not in add_generator_params

    assert hasattr(surge.Network, "set_generator_p")
    assert not hasattr(surge.Network, "set_generator_pg")

    add_facts_params = _parameter_names(surge.Network.add_facts_device)
    assert "name" in add_facts_params
    assert "bus_from" in add_facts_params
    assert "bus_to" in add_facts_params
    assert "bus_i" not in add_facts_params
    assert "bus_j" not in add_facts_params

    generator = surge.Generator(1)
    assert hasattr(generator, "p_mw")
    assert hasattr(generator, "q_mvar")
    assert not hasattr(generator, "pg_mw")
    assert not hasattr(generator, "qg_mvar")

    facts = surge.FactsDevice("facts-1", 1)
    assert hasattr(facts, "bus_from")
    assert hasattr(facts, "bus_to")
    assert not hasattr(facts, "bus_i")
    assert not hasattr(facts, "bus_j")

    result = surge.solve_dc_opf(surge.load(str(CASE9)))
    assert hasattr(result, "gen_p_mw")
    assert hasattr(result, "gen_q_mvar")
    assert not hasattr(result, "p_mw")
    assert not hasattr(result, "q_mvar")

    opf_generator = result.generators[0]
    assert hasattr(opf_generator, "p_mw")
    assert hasattr(opf_generator, "q_mvar")
    assert not hasattr(opf_generator, "pg_mw")
    assert not hasattr(opf_generator, "qg_mvar")


def test_init_stub_covers_all_runtime_exports():
    """Every name in surge.__all__ must appear in __init__.pyi, and vice-versa."""
    tree = ast.parse(INIT_STUB_PATH.read_text())

    # Collect names that the stub re-exports from surge sub-modules,
    # plus any top-level function/class/assignment definitions.
    # Skip imports from non-surge modules (os, __future__, etc.).
    stub_names: set[str] = set()
    for node in ast.iter_child_nodes(tree):
        if isinstance(node, ast.ImportFrom):
            if not node.level:  # skip absolute imports (os, __future__, …)
                continue
            for alias in node.names:
                stub_names.add(alias.asname or alias.name)
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            stub_names.add(node.name)
        elif isinstance(node, ast.ClassDef):
            stub_names.add(node.name)
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            stub_names.add(node.target.id)
        elif isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name):
                    stub_names.add(target.id)

    runtime_all = set(surge.__all__)

    missing_from_stub = runtime_all - stub_names
    extra_in_stub = stub_names - runtime_all

    assert not missing_from_stub, (
        f"__init__.pyi is missing exports that exist in surge.__all__: {sorted(missing_from_stub)}"
    )
    assert not extra_in_stub, (
        f"__init__.pyi has exports not in surge.__all__: {sorted(extra_in_stub)}"
    )


def test_dispatchable_load_surface_rejects_unsupported_archetypes():
    load = surge.DispatchableLoad(1, archetype="Curtailable")
    assert load.archetype == "Curtailable"

    with pytest.raises(ValueError, match="Curtailable"):
        surge.DispatchableLoad(1, archetype="Elastic")

    net = surge.Network()
    net.add_bus(1, "PQ", 138.0)
    with pytest.raises(ValueError, match="Curtailable"):
        net.add_dispatchable_load(1, 10.0, archetype="IndependentPQ")
