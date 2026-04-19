#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Repository-relative paths for the GO C3 harness."""

from __future__ import annotations

import os
from pathlib import Path


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def benchmark_root() -> Path:
    return repo_root() / "research" / "benchmarks" / "go-c3"


def manifests_dir() -> Path:
    return Path(__file__).resolve().parent / "manifests"


def default_cache_root() -> Path:
    return repo_root() / "target" / "benchmarks" / "go-c3"


def source_python_root() -> Path:
    return repo_root() / "src" / "surge-py" / "python"


def default_results_root() -> Path:
    override = os.environ.get("SURGE_GO_C3_RESULTS_ROOT")
    if override:
        return Path(override).expanduser().resolve()
    return (Path.home() / "Documents" / "go-competition-results").resolve()
