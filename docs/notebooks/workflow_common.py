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


def find_activsg_case(case: str = "2000") -> Path | None:
    root = repo_root()
    normalized = case.lower()
    filename = {
        "2000": "case_ACTIVSg2000.surge.json.zst",
        "activsg2000": "case_ACTIVSg2000.surge.json.zst",
        "10k": "case_ACTIVSg10k.surge.json.zst",
        "10000": "case_ACTIVSg10k.surge.json.zst",
        "activsg10k": "case_ACTIVSg10k.surge.json.zst",
    }.get(normalized)
    if filename is None:
        return None

    candidates = [
        root / "examples" / "cases" / filename.removesuffix(".surge.json.zst") / filename,
    ]
    for path in candidates:
        if path.exists():
            return path
    return None


def find_activsg_time_series_root() -> Path | None:
    root = repo_root()
    candidates = [
        root / "research" / "test-cases" / "data" / "ACTIVSg_Time_Series",
    ]
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
