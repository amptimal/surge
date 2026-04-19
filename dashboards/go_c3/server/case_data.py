# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Case index + on-demand case data loader backed by ``dashboard-cache/``."""
from __future__ import annotations

import json
import re
import threading
from pathlib import Path
from typing import Any

from benchmarks.go_c3.datasets import ScenarioRecord, discover_scenarios
from benchmarks.go_c3.runner import baseline_output_dir
from markets.go_c3 import GoC3Policy

from ..dashboard import (
    _build_case_data,
    _build_unsolved_case_data,
    _discover_baseline_scenarios,
)


_DATASET_BUS_COUNT_RE = re.compile(r"event\d+_(\d+)(?:_d\d)?$")


def _expected_network_model_prefix(dataset_key: str) -> str | None:
    """For ``event4_617`` return ``C3E4N00617``. ``None`` if the bus count
    can't be parsed (e.g. a non-standard dataset key).

    Used to filter out stray subdirectories that end up colocated with a
    dataset (e.g. a duplicate ``C3E4N00073D1/`` under ``event4_617/D1/``),
    which would otherwise leak unrelated scenarios into the sidebar.
    """
    m = _DATASET_BUS_COUNT_RE.match(dataset_key)
    if not m:
        return None
    return f"C3E4N{int(m.group(1)):05d}"


def _cache_dir(cache_root: Path) -> Path:
    d = cache_root / "runs" / "dashboard-cache"
    d.mkdir(parents=True, exist_ok=True)
    return d


def _cache_path(cache_root: Path, key: str) -> Path:
    return _cache_dir(cache_root) / f"{key.replace('/', '_')}.json"


def _parse_key(key: str) -> tuple[str, str, str, int]:
    """Split ``dataset/division/sw/id`` into its four components."""
    try:
        dataset, division, sw, raw_id = key.split("/")
        return dataset, division, sw, int(raw_id)
    except (ValueError, KeyError) as exc:
        raise KeyError(f"invalid case key: {key!r}") from exc


class CaseRegistry:
    """Discovers scenarios on disk and serves case data on demand.

    The existing ``generate_dashboard`` flow eagerly rebuilt every case's
    JSON into ``data/``. The server instead reads the cache file if fresh,
    otherwise rebuilds a single case lazily in response to a request.
    A per-key ``threading.Lock`` prevents two concurrent requests from
    duplicating the same expensive build.
    """

    def __init__(self, cache_root: Path):
        self._cache_root = cache_root
        self._build_locks: dict[str, threading.Lock] = {}
        self._registry_lock = threading.Lock()

    # ─── discovery ────────────────────────────────────────────────────────

    def index(self) -> dict[str, dict[str, Any]]:
        """Return ``{key: {solved, dataset, division, sw, scenario_id}}``."""
        entries: dict[str, dict[str, Any]] = {}

        for scenario in _discover_baseline_scenarios(self._cache_root):
            sw = scenario.switching_mode
            key = f"{scenario.dataset_key}/{scenario.division}/{sw}/{scenario.scenario_id}"
            entries[key] = {
                "solved": True,
                "dataset": scenario.dataset_key,
                "division": scenario.division,
                "sw": sw,
                "scenario_id": scenario.scenario_id,
                "network_model": scenario.record.network_model,
            }

        datasets_root = self._cache_root / "datasets"
        if datasets_root.exists():
            for dataset_dir in sorted(datasets_root.iterdir()):
                if not dataset_dir.is_dir():
                    continue
                expected_prefix = _expected_network_model_prefix(dataset_dir.name)
                try:
                    all_scenarios = discover_scenarios(dataset_dir, dataset_dir.name)
                except Exception:
                    continue
                for sc in all_scenarios:
                    # Drop stray scenarios whose network_model doesn't match
                    # the dataset's bus count (e.g. a duplicate
                    # C3E4N00073D1/ subdir accidentally left inside
                    # event4_617/). Unknown dataset keys skip the check.
                    if expected_prefix and not sc.network_model.startswith(expected_prefix):
                        continue
                    for sw in ("sw0", "sw1"):
                        key = f"{sc.dataset_key}/{sc.division}/{sw}/{sc.scenario_id}"
                        if key in entries:
                            continue
                        entries[key] = {
                            "solved": False,
                            "dataset": sc.dataset_key,
                            "division": sc.division,
                            "sw": sw,
                            "scenario_id": sc.scenario_id,
                            "network_model": sc.network_model,
                        }
        return entries

    # ─── case data ────────────────────────────────────────────────────────

    def _lock_for(self, key: str) -> threading.Lock:
        with self._registry_lock:
            lock = self._build_locks.get(key)
            if lock is None:
                lock = threading.Lock()
                self._build_locks[key] = lock
            return lock

    def _scenario_for(self, key: str) -> tuple[ScenarioRecord, str, bool]:
        """Resolve key → (record, switching_mode, is_solved).

        Prefers a discovered solved scenario (which gives us the live
        ``problem_path``); otherwise finds the scenario in the dataset tree.
        """
        dataset, division, sw, scenario_id = _parse_key(key)

        for scenario in _discover_baseline_scenarios(self._cache_root):
            if (
                scenario.dataset_key == dataset
                and scenario.division == division
                and scenario.switching_mode == sw
                and scenario.scenario_id == scenario_id
            ):
                return scenario.record, sw, True

        datasets_root = self._cache_root / "datasets" / dataset
        if not datasets_root.exists():
            raise KeyError(f"unknown dataset: {dataset}")
        for sc in discover_scenarios(datasets_root, dataset):
            if sc.division == division and sc.scenario_id == scenario_id:
                return sc, sw, False
        raise KeyError(f"scenario not found: {key}")

    def _cache_valid(
        self,
        cache_path: Path,
        record: ScenarioRecord,
        sw: str,
        is_solved: bool,
    ) -> bool:
        if not cache_path.exists():
            return False
        if not is_solved:
            return True
        policy = GoC3Policy(allow_branch_switching=(sw == "sw1"))
        run_report = baseline_output_dir(self._cache_root, record, policy=policy) / "run-report.json"
        if not run_report.exists():
            return True
        return cache_path.stat().st_mtime >= run_report.stat().st_mtime

    def _read_cache(self, cache_path: Path) -> dict[str, Any] | None:
        if not cache_path.exists():
            return None
        try:
            return json.loads(cache_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return None

    def load_case(self, key: str, *, force_rebuild: bool = False) -> dict[str, Any] | None:
        """Return the case blob, building from surge/validator artifacts on miss.

        Build strategy:
        1. If the cache is fresh, serve it.
        2. Otherwise rebuild via :func:`_build_case_data` (solved) or
           :func:`_build_unsolved_case_data` (no run-report).
        3. If the run-report exists but reports an error (e.g. SCED failed),
           ``_build_case_data`` returns ``None``; fall back to the
           unsolved-data builder so winner/leaderboard reference still
           renders with the failure surfaced as a banner.
        4. If both rebuild paths return ``None`` but a stale cache exists,
           prefer the stale cache over 404.
        """
        record, sw, is_solved = self._scenario_for(key)
        cache_path = _cache_path(self._cache_root, key)

        if not force_rebuild and self._cache_valid(cache_path, record, sw, is_solved):
            cached = self._read_cache(cache_path)
            if cached is not None:
                return cached

        with self._lock_for(key):
            if not force_rebuild and self._cache_valid(cache_path, record, sw, is_solved):
                cached = self._read_cache(cache_path)
                if cached is not None:
                    return cached

            data: dict[str, Any] | None = None
            if is_solved:
                data = _build_case_data(record, self._cache_root, switching_mode=sw.upper())
                if data is None:
                    # Solve errored — degrade to the unsolved view so the
                    # case still loads (winner/leaderboard reference + a
                    # surfaced failure banner). The error message comes
                    # from run-report.json.
                    data = _build_unsolved_case_data(
                        record, self._cache_root, switching_mode=sw.upper()
                    )
                    if data is not None:
                        err_msg = self._read_solve_error(record, sw)
                        if err_msg:
                            data["solve_error"] = err_msg
            else:
                data = _build_unsolved_case_data(record, self._cache_root, switching_mode=sw.upper())

            if data is not None:
                cache_path.write_text(
                    json.dumps(data, separators=(",", ":"), default=str),
                    encoding="utf-8",
                )
                return data

            # Both rebuild paths returned None — fall back to stale cache.
            stale = self._read_cache(cache_path)
            if stale is not None:
                return stale
            return None

    def _read_solve_error(self, record: ScenarioRecord, sw: str) -> str | None:
        """Pull the `error` field from run-report.json when the solve failed."""
        policy = GoC3Policy(allow_branch_switching=(sw == "sw1"))
        rr_path = baseline_output_dir(self._cache_root, record, policy=policy) / "run-report.json"
        if not rr_path.exists():
            return None
        try:
            rr = json.loads(rr_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return None
        return rr.get("error") or None

    def invalidate(self, key: str) -> None:
        """Drop the cache file for ``key`` so the next load rebuilds."""
        cache_path = _cache_path(self._cache_root, key)
        if cache_path.exists():
            cache_path.unlink()

    # ─── ancillary per-case artifacts ─────────────────────────────────────

    def workdir(self, key: str) -> Path:
        record, sw, _ = self._scenario_for(key)
        policy = GoC3Policy(allow_branch_switching=(sw == "sw1"))
        return baseline_output_dir(self._cache_root, record, policy=policy)

    def solve_log(self, key: str, *, max_bytes: int = 2_000_000) -> str | None:
        log_path = self.workdir(key) / "solve.log"
        if not log_path.exists():
            return None
        size = log_path.stat().st_size
        if size <= max_bytes:
            return log_path.read_text(encoding="utf-8", errors="replace")
        # tail only — dashboards care about the most recent log lines
        with log_path.open("rb") as fh:
            fh.seek(-max_bytes, 2)
            return fh.read().decode("utf-8", errors="replace")

    def list_archives(self, key: str) -> list[dict[str, Any]]:
        archive_root = self.workdir(key) / "archive"
        if not archive_root.exists():
            return []
        out: list[dict[str, Any]] = []
        for entry in sorted(archive_root.iterdir()):
            if not entry.is_dir():
                continue
            stat = entry.stat()
            report_path = entry / "run-report.json"
            status: str | None = None
            if report_path.exists():
                try:
                    status = json.loads(report_path.read_text(encoding="utf-8")).get("status")
                except (OSError, json.JSONDecodeError):
                    status = None
            out.append({
                "timestamp": entry.name,
                "path": str(entry),
                "mtime_unix": stat.st_mtime,
                "status": status,
            })
        return out

    def _archive_dir(self, key: str, timestamp: str) -> Path:
        archive_dir = self.workdir(key) / "archive" / timestamp
        if not archive_dir.is_dir():
            raise FileNotFoundError(f"archive not found: {timestamp}")
        return archive_dir

    def archive_run_report(self, key: str, timestamp: str) -> dict[str, Any]:
        report_path = self._archive_dir(key, timestamp) / "run-report.json"
        if not report_path.exists():
            raise FileNotFoundError(f"archive has no run-report.json: {timestamp}")
        return json.loads(report_path.read_text(encoding="utf-8"))

    def archive_solve_log(self, key: str, timestamp: str, *, max_bytes: int = 2_000_000) -> str | None:
        log_path = self._archive_dir(key, timestamp) / "solve.log"
        if not log_path.exists():
            return None
        size = log_path.stat().st_size
        if size <= max_bytes:
            return log_path.read_text(encoding="utf-8", errors="replace")
        with log_path.open("rb") as fh:
            fh.seek(-max_bytes, 2)
            return fh.read().decode("utf-8", errors="replace")
