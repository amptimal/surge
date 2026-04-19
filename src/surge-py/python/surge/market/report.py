# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Canonical ``run-report.json`` writer for market solves.

Every market's :func:`solve` writes a ``run-report.json`` to ``workdir``.
:func:`write_run_report` is the single shared writer.

Shape::

    {
      "schema_version": 2,
      "label": "...",
      "status": "ok" | "error",
      "error": null | "...",
      "elapsed_secs": 12.3,
      "policy": { ... dataclasses.asdict(policy) ... },
      "artifacts": {
        "dispatch_result": "/abs/path/dispatch-result.json",
        "settlement":      "/abs/path/settlement.json",
        ...
      },
      "extras": {
        "settlement_summary": {...},   # market-specific
        ...
      },
      "run_report_path": "/abs/path/run-report.json"
    }

Read paths via ``report["artifacts"]["<name>"]`` and per-market summary
fields via ``report["extras"]["<name>"]``.
"""

from __future__ import annotations

import dataclasses
import json
from pathlib import Path
from typing import Any, Literal, Mapping, TypedDict

SCHEMA_VERSION = 2


class RunReport(TypedDict, total=False):
    """Canonical run-report dict returned by :func:`write_run_report`."""

    schema_version: int
    label: str | None
    status: Literal["ok", "error"]
    error: str | None
    elapsed_secs: float
    policy: dict[str, Any]
    artifacts: dict[str, str]
    extras: dict[str, Any]
    run_report_path: str


def write_run_report(
    workdir: Path,
    *,
    status: Literal["ok", "error"],
    elapsed_secs: float,
    policy: Any,
    label: str | None = None,
    error: str | None = None,
    artifacts: Mapping[str, Path | str | None] | None = None,
    extras: Mapping[str, Any] | None = None,
    filename: str = "run-report.json",
) -> RunReport:
    """Write ``{workdir}/{filename}`` with the canonical schema.

    Parameters
    ----------
    workdir
        Target directory (created if missing).
    status
        ``"ok"`` or ``"error"``.
    elapsed_secs
        Wall-clock solve duration.
    policy
        Any dataclass (converted via :func:`dataclasses.asdict`) or a
        mapping. Stored under ``"policy"``.
    label, error
        Optional human-readable label and error message.
    artifacts
        Mapping of artifact name → path (or ``None``). ``None`` entries
        are dropped. Paths are normalized to absolute strings.
    extras
        Market-specific summary fields (settlement totals, MIP stats,
        revenue summary, ...).
    filename
        Override the output filename. Defaults to ``"run-report.json"``.

    Returns
    -------
    The written report dict.
    """
    workdir = Path(workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    out_path = workdir / filename

    if dataclasses.is_dataclass(policy):
        policy_dict = dataclasses.asdict(policy)
    elif isinstance(policy, Mapping):
        policy_dict = dict(policy)
    else:
        policy_dict = {"value": policy}

    clean_artifacts: dict[str, str] = {}
    if artifacts:
        for name, path in artifacts.items():
            if path is None:
                continue
            clean_artifacts[name] = str(Path(path))

    extras_dict: dict[str, Any] = dict(extras) if extras else {}

    report: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "label": label,
        "status": status,
        "error": error,
        "elapsed_secs": elapsed_secs,
        "policy": policy_dict,
        "artifacts": clean_artifacts,
        "extras": extras_dict,
        "run_report_path": str(out_path),
    }

    out_path.write_text(
        json.dumps(report, indent=2, default=str) + "\n", encoding="utf-8"
    )
    return report  # type: ignore[return-value]


__all__ = ["RunReport", "SCHEMA_VERSION", "write_run_report"]
