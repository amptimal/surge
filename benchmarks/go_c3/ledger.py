#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Score ledger for tracking GO Challenge 3 regression progress."""

from __future__ import annotations

from dataclasses import dataclass
import datetime
import json
from pathlib import Path
import subprocess
from typing import Any

from .paths import benchmark_root


# ---------------------------------------------------------------------------
# Data structures
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class LedgerEntry:
    """One recorded score for a scenario in a specific mode."""

    scenario_key: str
    mode: str
    our_obj: float | None
    ref_obj: float | None
    ref_team: str | None
    gap_pct: float | None
    our_feas: int | None
    our_z_penalty: float | None
    timestamp: str
    git_sha: str | None
    notes: str


@dataclass(frozen=True)
class LedgerDelta:
    """Change between current run and ledger for one scenario."""

    scenario_key: str
    mode: str
    previous_obj: float | None
    current_obj: float | None
    obj_delta: float | None
    previous_gap_pct: float | None
    current_gap_pct: float | None
    improved: bool
    regressed: bool
    is_new: bool


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def ledger_dir() -> Path:
    """Return the ledger directory path."""
    return benchmark_root() / "ledger"


def scenario_key(dataset_key: str, division: str, scenario_id: int) -> str:
    """Build a canonical scenario key string."""
    return f"{dataset_key}/{division}/{scenario_id:03d}"


def _current_git_sha() -> str | None:
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
            cwd=str(benchmark_root()),
        )
        return result.stdout.strip() or None
    except Exception:
        return None


def _gap_pct(our_obj: float | None, ref_obj: float | None) -> float | None:
    if our_obj is None or ref_obj is None:
        return None
    denom = abs(ref_obj)
    if denom <= 0.0:
        return None
    return (our_obj - ref_obj) / denom * 100.0


def _entry_to_dict(entry: LedgerEntry) -> dict[str, Any]:
    return {
        "scenario_key": entry.scenario_key,
        "mode": entry.mode,
        "our_obj": entry.our_obj,
        "ref_obj": entry.ref_obj,
        "ref_team": entry.ref_team,
        "gap_pct": entry.gap_pct,
        "our_feas": entry.our_feas,
        "our_z_penalty": entry.our_z_penalty,
        "timestamp": entry.timestamp,
        "git_sha": entry.git_sha,
        "notes": entry.notes,
    }


# ---------------------------------------------------------------------------
# Persistence
# ---------------------------------------------------------------------------


def ledger_path(mode: str) -> Path:
    """Return the JSON file path for a given mode's ledger."""
    return ledger_dir() / f"{mode}.json"


def load_ledger(mode: str) -> dict[str, dict[str, Any]]:
    """Load the ledger for a given mode. Returns {scenario_key: entry_dict}."""
    path = ledger_path(mode)
    if not path.exists():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def save_ledger(mode: str, entries: dict[str, dict[str, Any]]) -> Path:
    """Write the ledger for a given mode. Returns the written path."""
    path = ledger_path(mode)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(entries, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return path


# ---------------------------------------------------------------------------
# Recording
# ---------------------------------------------------------------------------


def record_entry(
    mode: str,
    *,
    dataset_key: str,
    division: str,
    scenario_id: int,
    our_obj: float | None,
    ref_obj: float | None = None,
    ref_team: str | None = None,
    our_feas: int | None = None,
    our_z_penalty: float | None = None,
    notes: str = "",
) -> LedgerEntry:
    """Build a LedgerEntry and persist it to the ledger file."""
    key = scenario_key(dataset_key, division, scenario_id)
    entry = LedgerEntry(
        scenario_key=key,
        mode=mode,
        our_obj=our_obj,
        ref_obj=ref_obj,
        ref_team=ref_team,
        gap_pct=_gap_pct(our_obj, ref_obj),
        our_feas=our_feas,
        our_z_penalty=our_z_penalty,
        timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
        git_sha=_current_git_sha(),
        notes=notes,
    )
    ledger = load_ledger(mode)
    ledger[key] = _entry_to_dict(entry)
    save_ledger(mode, ledger)
    return entry


# ---------------------------------------------------------------------------
# Comparison
# ---------------------------------------------------------------------------


def compare_against_ledger(
    mode: str,
    current_entries: list[LedgerEntry],
) -> list[LedgerDelta]:
    """Compare a list of current entries against the saved ledger."""
    ledger = load_ledger(mode)
    deltas: list[LedgerDelta] = []
    for entry in current_entries:
        previous = ledger.get(entry.scenario_key)
        if previous is None:
            deltas.append(LedgerDelta(
                scenario_key=entry.scenario_key,
                mode=mode,
                previous_obj=None,
                current_obj=entry.our_obj,
                obj_delta=None,
                previous_gap_pct=None,
                current_gap_pct=entry.gap_pct,
                improved=False,
                regressed=False,
                is_new=True,
            ))
            continue
        prev_obj = previous.get("our_obj")
        obj_delta = None
        improved = False
        regressed = False
        if entry.our_obj is not None and isinstance(prev_obj, (int, float)):
            obj_delta = entry.our_obj - float(prev_obj)
            # For GO C3, more negative obj is better (cost minimization)
            # But some objectives are positive — use sign of reference
            # Simple rule: lower is better
            if obj_delta < -1e-6:
                improved = True
            elif obj_delta > 1e-6:
                regressed = True
        deltas.append(LedgerDelta(
            scenario_key=entry.scenario_key,
            mode=mode,
            previous_obj=float(prev_obj) if isinstance(prev_obj, (int, float)) else None,
            current_obj=entry.our_obj,
            obj_delta=obj_delta,
            previous_gap_pct=float(previous.get("gap_pct")) if isinstance(previous.get("gap_pct"), (int, float)) else None,
            current_gap_pct=entry.gap_pct,
            improved=improved,
            regressed=regressed,
            is_new=False,
        ))
    return deltas


def format_ledger_status(deltas: list[LedgerDelta]) -> list[str]:
    """Format ledger deltas as human-readable lines for CLI output."""
    lines: list[str] = []
    improved = [d for d in deltas if d.improved]
    regressed = [d for d in deltas if d.regressed]
    new = [d for d in deltas if d.is_new]
    unchanged = [d for d in deltas if not d.improved and not d.regressed and not d.is_new]

    if improved:
        lines.append(f"Improved ({len(improved)}):")
        for d in improved:
            lines.append(f"  {d.scenario_key}: {d.previous_obj:.2f} -> {d.current_obj:.2f} ({d.obj_delta:+.2f})")
    if regressed:
        lines.append(f"Regressed ({len(regressed)}):")
        for d in regressed:
            lines.append(f"  {d.scenario_key}: {d.previous_obj:.2f} -> {d.current_obj:.2f} ({d.obj_delta:+.2f})")
    if new:
        lines.append(f"New ({len(new)}):")
        for d in new:
            obj_str = f"{d.current_obj:.2f}" if d.current_obj is not None else "N/A"
            lines.append(f"  {d.scenario_key}: {obj_str}")
    if unchanged:
        lines.append(f"Unchanged ({len(unchanged)})")

    return lines


def update_ledger_from_run_reports(
    mode: str,
    run_reports: list[dict[str, Any]],
    *,
    ref_objectives: dict[str, float] | None = None,
    ref_teams: dict[str, str] | None = None,
    notes: str = "",
) -> tuple[list[LedgerEntry], list[LedgerDelta]]:
    """Batch update: ingest multiple run-reports, compare, persist."""
    ref_objectives = ref_objectives or {}
    ref_teams = ref_teams or {}
    timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
    git_sha = _current_git_sha()

    entries: list[LedgerEntry] = []
    for report in run_reports:
        key = scenario_key(
            report.get("dataset_key", ""),
            report.get("division", ""),
            int(report.get("scenario_id", 0)),
        )
        summary = report.get("dispatch_summary", {})
        our_obj = summary.get("total_cost") if isinstance(summary, dict) else None
        entry = LedgerEntry(
            scenario_key=key,
            mode=mode,
            our_obj=our_obj,
            ref_obj=ref_objectives.get(key),
            ref_team=ref_teams.get(key),
            gap_pct=_gap_pct(our_obj, ref_objectives.get(key)),
            our_feas=1 if report.get("status") == "ok" else 0,
            our_z_penalty=None,
            timestamp=timestamp,
            git_sha=git_sha,
            notes=notes,
        )
        entries.append(entry)

    deltas = compare_against_ledger(mode, entries)

    ledger = load_ledger(mode)
    for entry in entries:
        ledger[entry.scenario_key] = _entry_to_dict(entry)
    save_ledger(mode, ledger)

    return entries, deltas
