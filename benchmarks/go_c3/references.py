#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Reference leaderboard workbook and submission archive helpers."""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path
import re
import tarfile
from urllib.parse import urlparse
from urllib.request import urlretrieve

from .leaderboard import LeaderboardEntry, benchmark_entry, load_event_workbook, scenario_leaderboard
from .paths import default_cache_root, default_results_root
from .validator import validator_summary


_EVENT_FROM_DATASET_RE = re.compile(r"event(\d+)_", re.IGNORECASE)
_EVENT_FROM_MODEL_RE = re.compile(r"^C3E(\d+)", re.IGNORECASE)
_WORKBOOK_NAME_RE = re.compile(r"^E(\d+)LB_.*\.xlsx$", re.IGNORECASE)


@dataclass(frozen=True)
class ReferenceSubmission:
    entry: LeaderboardEntry
    archive_path: Path
    extracted_dir: Path
    solution_path: Path
    summary_json_path: Path | None
    summary_csv_path: Path | None
    archived_summary: dict[str, object] | None
    archived_metrics: dict[str, object]


def event_index_from_dataset_key(dataset_key: str) -> int:
    match = _EVENT_FROM_DATASET_RE.match(dataset_key)
    if match is None:
        raise ValueError(f"dataset key {dataset_key!r} does not encode an event number")
    return int(match.group(1))


def event_index_from_model(model: str) -> int:
    match = _EVENT_FROM_MODEL_RE.match(model)
    if match is None:
        raise ValueError(f"model {model!r} does not encode an event number")
    return int(match.group(1))


def find_event_workbook(
    event_index: int,
    *,
    results_root: Path | None = None,
) -> Path:
    search_root = results_root or default_results_root()
    candidates = []
    for path in sorted(search_root.glob(f"E{event_index}LB*.xlsx")):
        match = _WORKBOOK_NAME_RE.match(path.name)
        if match and int(match.group(1)) == event_index:
            candidates.append(path)
    if not candidates:
        raise FileNotFoundError(f"no workbook found for event {event_index} under {search_root}")
    return max(candidates, key=lambda path: (path.stat().st_mtime, path.name))


def preload_leaderboard_workbooks(cache_root: Path | None = None) -> list[Path]:
    """Warm the leaderboard workbook LRU cache for every event we can discover.

    Called at server startup in a background thread so the first case request
    doesn't pay the ~5s Excel parse cost. Returns the paths that were parsed.
    Silently skips events without a downloaded workbook.
    """
    datasets_root = (cache_root or default_cache_root()) / "datasets"
    if not datasets_root.exists():
        return []
    # Discover events from dataset directory names (e.g. event4_73 → event 4).
    event_indices = set()
    for entry in datasets_root.iterdir():
        if not entry.is_dir():
            continue
        try:
            event_indices.add(event_index_from_dataset_key(entry.name))
        except ValueError:
            continue
    warmed: list[Path] = []
    for event_index in sorted(event_indices):
        try:
            path = find_event_workbook(event_index)
        except FileNotFoundError:
            continue
        # Calling load_event_workbook populates the module-level lru_cache.
        load_event_workbook(path)
        warmed.append(path)
    return warmed


def load_scenario_leaderboard(
    dataset_key: str,
    division: str,
    network_model: str,
    scenario_id: int,
    *,
    results_root: Path | None = None,
    workbook_path: Path | None = None,
    switching_mode: str | None = None,
) -> tuple[Path, list[LeaderboardEntry]]:
    event_index = event_index_from_dataset_key(dataset_key)
    resolved_workbook = workbook_path or find_event_workbook(event_index, results_root=results_root)
    entries = load_event_workbook(resolved_workbook)
    selected = scenario_leaderboard(
        entries,
        model=network_model,
        scenario_id=scenario_id,
        switching_mode=switching_mode,
    )
    filtered = [entry for entry in selected if entry.model == network_model and entry.scenario_id == scenario_id]
    return resolved_workbook, filtered


def _submission_slug(entry: LeaderboardEntry) -> str:
    if entry.url:
        parsed = urlparse(entry.url)
        name = Path(parsed.path).name
        if name:
            return name
    team_slug = re.sub(r"[^A-Za-z0-9._-]+", "-", entry.team).strip("-") or "submission"
    model_slug = entry.model or "model"
    scenario_slug = f"scenario_{entry.scenario_id:03d}" if entry.scenario_id is not None else "scenario"
    return f"{team_slug}_{model_slug}_{scenario_slug}.tar.gz"


def reference_archive_path(
    entry: LeaderboardEntry,
    *,
    cache_root: Path | None = None,
) -> Path:
    root = cache_root or default_cache_root()
    subdir = f"event{event_index_from_model(entry.model)}"
    return root / "reference-submissions" / subdir / _submission_slug(entry)


def ensure_reference_archive(
    entry: LeaderboardEntry,
    *,
    cache_root: Path | None = None,
) -> Path:
    if not entry.url:
        raise FileNotFoundError(f"leaderboard entry for {entry.team} {entry.model} scenario {entry.scenario_id} has no archive URL")
    archive_path = reference_archive_path(entry, cache_root=cache_root)
    archive_path.parent.mkdir(parents=True, exist_ok=True)
    if archive_path.is_file():
        return archive_path
    urlretrieve(entry.url, archive_path)
    return archive_path


def _extracted_submission_dir(
    entry: LeaderboardEntry,
    *,
    cache_root: Path | None = None,
) -> Path:
    root = cache_root or default_cache_root()
    team_slug = re.sub(r"[^A-Za-z0-9._-]+", "-", entry.team).strip("-") or "team"
    scenario_slug = f"scenario_{entry.scenario_id:03d}" if entry.scenario_id is not None else "scenario"
    return root / "reference-submissions" / "extracted" / entry.model / scenario_slug / team_slug


def _load_summary(path: Path | None) -> tuple[dict[str, object] | None, dict[str, object]]:
    if path is None or not path.exists():
        return None, {}
    summary = json.loads(path.read_text(encoding="utf-8"))
    return summary, validator_summary(summary)


def ensure_reference_submission(
    entry: LeaderboardEntry,
    *,
    cache_root: Path | None = None,
) -> ReferenceSubmission:
    archive_path = ensure_reference_archive(entry, cache_root=cache_root)
    extracted_dir = _extracted_submission_dir(entry, cache_root=cache_root)
    sentinel = extracted_dir / ".extracted"
    if not sentinel.exists():
        extracted_dir.mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive_path, "r:*") as archive:
            archive.extractall(extracted_dir, filter="data")
        sentinel.write_text(str(archive_path) + "\n", encoding="utf-8")

    solution_path = extracted_dir / "solution.json"
    if not solution_path.exists():
        matches = sorted(extracted_dir.rglob("solution.json"))
        if not matches:
            raise FileNotFoundError(f"no solution.json found in {archive_path}")
        solution_path = matches[0]
    summary_json_path = extracted_dir / "summary.json"
    if not summary_json_path.exists():
        matches = sorted(extracted_dir.rglob("summary.json"))
        summary_json_path = matches[0] if matches else None
    summary_csv_path = extracted_dir / "summary.csv"
    if not summary_csv_path.exists():
        matches = sorted(extracted_dir.rglob("summary.csv"))
        summary_csv_path = matches[0] if matches else None
    archived_summary, archived_metrics = _load_summary(summary_json_path)
    return ReferenceSubmission(
        entry=entry,
        archive_path=archive_path,
        extracted_dir=extracted_dir,
        solution_path=solution_path,
        summary_json_path=summary_json_path,
        summary_csv_path=summary_csv_path,
        archived_summary=archived_summary,
        archived_metrics=archived_metrics,
    )


def select_reference_entries(
    entries: list[LeaderboardEntry],
    *,
    include_benchmark: bool = True,
    top_k: int = 1,
) -> list[LeaderboardEntry]:
    selected: list[LeaderboardEntry] = []
    seen: set[tuple[str, int | None]] = set()
    for entry in entries:
        if top_k <= 0:
            break
        key = (entry.team, entry.scenario_id)
        if key in seen:
            continue
        selected.append(entry)
        seen.add(key)
        top_k -= 1
    if include_benchmark:
        benchmark = benchmark_entry(entries)
        if benchmark is not None and (benchmark.team, benchmark.scenario_id) not in seen:
            selected.append(benchmark)
    return selected
