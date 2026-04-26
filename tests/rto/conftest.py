# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Shared fixtures for the RTO dashboard tests.

Tests tagged ``@pytest.mark.slow`` are skipped by default. Pass
``--run-slow`` (e.g. ``uv run pytest tests/rto/ --run-slow``) to
include them. The marker is reserved for tests that decompress /
parse large case archives (the 2000-bus bundle takes ~15 s on
first load) — they're worth keeping in the suite for CI but
shouldn't block iteration loops.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--run-slow",
        action="store_true",
        default=False,
        help="run tests tagged @pytest.mark.slow (e.g. 2000-bus scaffold load)",
    )


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers", "slow: tests that need >5 s wall — skipped unless --run-slow"
    )


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    if config.getoption("--run-slow"):
        return
    skip_slow = pytest.mark.skip(reason="needs --run-slow")
    for item in items:
        if "slow" in item.keywords:
            item.add_marker(skip_slow)
