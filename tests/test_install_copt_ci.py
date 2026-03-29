# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import importlib.util
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "install_copt_ci.py"
SPEC = importlib.util.spec_from_file_location("install_copt_ci", SCRIPT_PATH)
assert SPEC is not None
assert SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def test_find_copt_home_locates_extracted_tree(tmp_path):
    copt_home = tmp_path / "CardinalOptimizer-8.0.3"
    (copt_home / "include" / "coptcpp_inc").mkdir(parents=True)
    (copt_home / "lib").mkdir()

    assert MODULE.find_copt_home(tmp_path) == copt_home.resolve()
