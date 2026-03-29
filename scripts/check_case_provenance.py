#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Fail if tracked Surge case bundles do not have sibling provenance notes."""

from __future__ import annotations

import argparse
import pathlib
import sys


BUNDLE_SUFFIX = ".surge.json.zst"
PROVENANCE_FILE = "PROVENANCE.md"


def bundle_paths(root: pathlib.Path) -> list[pathlib.Path]:
    if root.is_file():
        return [root] if root.name.endswith(BUNDLE_SUFFIX) else []
    return sorted(path for path in root.rglob(f"*{BUNDLE_SUFFIX}") if path.is_file())


def missing_provenance(root: pathlib.Path) -> list[pathlib.Path]:
    missing: list[pathlib.Path] = []
    for bundle in bundle_paths(root):
        provenance = bundle.parent / PROVENANCE_FILE
        if not provenance.is_file():
            missing.append(bundle)
    return missing


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("paths", nargs="+", help="bundle file or directory to inspect")
    args = parser.parse_args()

    rc = 0
    for raw_path in args.paths:
        path = pathlib.Path(raw_path)
        if not path.exists():
            print(f"{path}: does not exist", file=sys.stderr)
            rc = 1
            continue

        missing = missing_provenance(path)
        if not missing:
            print(f"{path}: provenance ok")
            continue

        rc = 1
        print(f"{path}: missing sibling {PROVENANCE_FILE} for:")
        for bundle in missing:
            print(f"  {bundle}")
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
