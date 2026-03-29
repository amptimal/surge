# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import importlib.util
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "check_optional_solver_linkage.py"
SPEC = importlib.util.spec_from_file_location("check_optional_solver_linkage", SCRIPT_PATH)
assert SPEC is not None
assert SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def test_core_extension_still_rejects_copt_runtime_linkage():
    allow = MODULE.allowed_patterns_for_member("surge/_surge.cpython-313-darwin.so")
    assert MODULE.forbidden(["libcopt_cpp.dylib"], allow_patterns=allow) == ["libcopt_cpp.dylib"]


def test_packaged_copt_shim_allows_copt_runtime_linkage():
    allow = MODULE.allowed_patterns_for_member("surge/libsurge_copt_nlp.dylib")
    assert MODULE.forbidden(["libcopt_cpp.dylib"], allow_patterns=allow) == []


def test_packaged_copt_shim_still_rejects_other_optional_solvers():
    allow = MODULE.allowed_patterns_for_member("surge/libsurge_copt_nlp.dylib")
    assert MODULE.forbidden(
        ["libipopt.dylib", "libgurobi130.dylib", "libcopt_cpp.dylib"],
        allow_patterns=allow,
    ) == ["libipopt.dylib", "libgurobi130.dylib"]
