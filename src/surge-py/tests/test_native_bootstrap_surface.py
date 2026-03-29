# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import ast
from pathlib import Path

import surge


def _module_level_native_uses(path: Path) -> set[str]:
    tree = ast.parse(path.read_text())
    names: set[str] = set()

    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Attribute)
            and isinstance(node.value, ast.Name)
            and node.value.id == "_native"
        ):
            names.add(node.attr)

    return names


def test_required_native_exports_cover_direct_package_usage():
    package_root = Path(surge.__file__).resolve().parent
    direct_native_uses: set[str] = set()

    for path in package_root.rglob("*.py"):
        direct_native_uses.update(_module_level_native_uses(path))

    direct_native_uses.discard("__doc__")
    missing = sorted(direct_native_uses - surge._REQUIRED_NATIVE_EXPORTS)
    assert not missing, (
        "surge package uses native exports that source-tree bootstrap does not "
        f"validate: {missing}"
    )
