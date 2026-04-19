# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Shared fixtures for markets/battery tests."""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))
