# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Shared pytest fixtures for GO Challenge 3 tests."""

from __future__ import annotations

from pathlib import Path
import sys

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from benchmarks.go_c3.paths import default_cache_root, default_results_root
from benchmarks.go_c3.datasets import discover_scenarios


def _dataset_available(dataset_key: str, cache_root: Path) -> bool:
    """Check if a dataset is unpacked and scenarios are discoverable."""
    unpacked_root = cache_root / "datasets" / dataset_key
    sentinel = unpacked_root / ".unpacked"
    return sentinel.is_file()


def _results_available() -> bool:
    """Check if competition results directory exists."""
    return default_results_root().is_dir()


@pytest.fixture(scope="session")
def go_c3_cache_root() -> Path:
    return default_cache_root()


@pytest.fixture(scope="session")
def go_c3_results_root() -> Path:
    return default_results_root()


@pytest.fixture(scope="session")
def go_c3_event4_73_available(go_c3_cache_root: Path) -> bool:
    return _dataset_available("event4_73", go_c3_cache_root)


@pytest.fixture(scope="session")
def go_c3_results_available() -> bool:
    return _results_available()


@pytest.fixture(scope="session")
def go_c3_validator_env(go_c3_cache_root: Path):
    """Lazily ensure the validator environment. Returns None if setup fails."""
    try:
        from benchmarks.go_c3.validator import ensure_validator_environment

        return ensure_validator_environment(cache_root=go_c3_cache_root)
    except Exception:
        return None


@pytest.fixture(scope="session")
def go_c3_event4_73_scenarios(go_c3_cache_root: Path, go_c3_event4_73_available: bool):
    """Return all discovered scenarios for event4_73, or empty list."""
    if not go_c3_event4_73_available:
        return []
    unpacked_root = go_c3_cache_root / "datasets" / "event4_73"
    return discover_scenarios(unpacked_root, "event4_73")
