# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Introspection of the loaded surge dylib for the top-nav build badge."""
from __future__ import annotations

import os
import subprocess
from datetime import datetime
from pathlib import Path
from typing import Any

from benchmarks.go_c3.paths import repo_root, source_python_root


def _dylib_path() -> Path | None:
    """Locate the pyo3 extension module currently available in the Python tree."""
    pkg_dir = source_python_root() / "surge"
    for candidate in pkg_dir.glob("_surge*.so"):
        return candidate
    for candidate in pkg_dir.glob("_surge*.dylib"):
        return candidate
    return None


def _infer_profile(so_path: Path) -> str:
    """Heuristically classify the .so as debug or release by file size.

    Debug builds are ~5x larger than release. No exact line between them,
    so we threshold at 40MB — works for the surge-py crate in practice.
    """
    try:
        size_mb = so_path.stat().st_size / (1024 * 1024)
    except OSError:
        return "unknown"
    return "debug" if size_mb >= 40 else "release"


def _git_sha() -> str | None:
    try:
        out = subprocess.run(
            ["git", "-C", str(repo_root()), "rev-parse", "--short=8", "HEAD"],
            capture_output=True,
            text=True,
            timeout=2,
        )
        if out.returncode == 0:
            return out.stdout.strip() or None
    except (OSError, subprocess.SubprocessError):
        pass
    return None


def _git_dirty() -> bool | None:
    try:
        out = subprocess.run(
            ["git", "-C", str(repo_root()), "status", "--porcelain"],
            capture_output=True,
            text=True,
            timeout=2,
        )
        if out.returncode == 0:
            return bool(out.stdout.strip())
    except (OSError, subprocess.SubprocessError):
        pass
    return None


def health_snapshot() -> dict[str, Any]:
    """Return a JSON-serializable view of the currently loaded surge build."""
    so = _dylib_path()
    if so is None:
        return {
            "surge_loaded": False,
            "build": None,
            "so_path": None,
            "so_mtime": None,
            "so_size_bytes": None,
            "git_sha": _git_sha(),
            "git_dirty": _git_dirty(),
        }
    stat = so.stat()
    return {
        "surge_loaded": True,
        "build": _infer_profile(so),
        "so_path": str(so),
        "so_mtime": datetime.fromtimestamp(stat.st_mtime).isoformat(timespec="seconds"),
        "so_mtime_unix": stat.st_mtime,
        "so_size_bytes": stat.st_size,
        "git_sha": _git_sha(),
        "git_dirty": _git_dirty(),
        "pid": os.getpid(),
    }
