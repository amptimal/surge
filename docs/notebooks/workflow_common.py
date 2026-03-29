# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from pathlib import Path
import os


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def find_case(filename: str = "case118.surge.json.zst") -> Path | None:
    root = repo_root()
    candidates = [
        root / "examples" / "cases" / "ieee118" / filename,
        root / "tests" / "data" / filename,
    ]

    env_dir = os.environ.get("SURGE_TEST_DATA")
    if env_dir:
        candidates.insert(0, Path(env_dir) / filename)

    for path in candidates:
        if path.exists():
            return path
    return None


def find_cli() -> Path | None:
    root = repo_root()
    candidates = [
        root / "target" / "release" / "surge-solve",
        root / "target" / "debug" / "surge-solve",
    ]
    for path in candidates:
        if path.exists():
            return path
    return None
