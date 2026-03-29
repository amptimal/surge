# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import importlib.machinery
import sys
import types
from pathlib import Path

import pytest

import surge


def test_load_native_from_path_restores_previous_module_on_failure(monkeypatch):
    module_name = surge._native_module_name()
    sentinel = types.ModuleType(module_name)
    monkeypatch.setitem(sys.modules, module_name, sentinel)

    class FailingLoader:
        def exec_module(self, module):
            raise ImportError("boom")

    spec = importlib.machinery.ModuleSpec(module_name, FailingLoader())
    monkeypatch.setattr(
        surge.importlib.util,
        "spec_from_file_location",
        lambda *args, **kwargs: spec,
    )

    with pytest.raises(ImportError):
        surge._load_native_from_path(Path("/tmp/fake_surge_native.so"))

    assert sys.modules[module_name] is sentinel


def test_import_native_restores_previous_module_when_candidate_is_missing_exports(monkeypatch):
    module_name = surge._native_module_name()
    sentinel = types.ModuleType(module_name)
    partial = types.ModuleType(module_name)

    monkeypatch.setitem(sys.modules, module_name, sentinel)
    monkeypatch.setattr(surge, "_REQUIRED_NATIVE_EXPORTS", frozenset({"load"}))
    monkeypatch.setattr(surge, "_is_source_tree_package", lambda: (True, Path("/tmp/repo")))
    monkeypatch.setattr(surge, "_source_tree_native_candidate", lambda: Path("/tmp/candidate.so"))

    def fake_load_native_from_path(path):
        monkeypatch.setitem(sys.modules, module_name, partial)
        return partial

    monkeypatch.setattr(surge, "_load_native_from_path", fake_load_native_from_path)

    with pytest.raises(ImportError, match="missing required exports: load"):
        surge._import_native()

    assert sys.modules[module_name] is sentinel
