#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Dataset download, unpack, and scenario discovery helpers."""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
import re
from pathlib import Path
from urllib.parse import urlparse
from urllib.request import urlretrieve
import zipfile

from .manifests import DatasetResource
from .paths import default_cache_root


class ArchiveLayout(str, Enum):
    CHALLENGE3 = "challenge3"


@dataclass(frozen=True)
class DatasetArchive:
    resource: DatasetResource
    archive_path: Path
    unpacked_root: Path


@dataclass(frozen=True)
class ScenarioRecord:
    dataset_key: str
    division: str
    network_model: str
    scenario_id: int
    problem_path: Path
    pop_solution_path: Path | None
    pop_log_path: Path | None


_PROBLEM_NAME_RE = re.compile(r"scenario_(\d+)\.json$")
_POP_NAME_RE = re.compile(r"scenario_(\d+)\.json\.pop_solution\.json$")
_POP_LOG_RE = re.compile(r"scenario_(\d+)\.json\.popsolution\.log$")


def dataset_archive_path(resource: DatasetResource, cache_root: Path | None = None) -> Path:
    cache_dir = (cache_root or default_cache_root()) / "archives"
    filename = Path(urlparse(resource.url).path).name
    return cache_dir / filename


def ensure_dataset_archive(resource: DatasetResource, cache_root: Path | None = None) -> Path:
    archive_path = dataset_archive_path(resource, cache_root=cache_root)
    archive_path.parent.mkdir(parents=True, exist_ok=True)
    if archive_path.is_file():
        return archive_path
    urlretrieve(resource.url, archive_path)
    return archive_path


def ensure_dataset_unpacked(resource: DatasetResource, cache_root: Path | None = None) -> DatasetArchive:
    cache_dir = cache_root or default_cache_root()
    archive_path = ensure_dataset_archive(resource, cache_root=cache_dir)
    unpacked_root = cache_dir / "datasets" / resource.key
    sentinel = unpacked_root / ".unpacked"
    if sentinel.is_file():
        return DatasetArchive(resource=resource, archive_path=archive_path, unpacked_root=unpacked_root)

    unpacked_root.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive_path) as archive:
        archive.extractall(unpacked_root)
    sentinel.write_text(resource.url + "\n", encoding="utf-8")
    return DatasetArchive(resource=resource, archive_path=archive_path, unpacked_root=unpacked_root)


def discover_scenarios(unpacked_root: Path, dataset_key: str) -> list[ScenarioRecord]:
    by_key: dict[tuple[str, str, int], dict[str, Path | None]] = {}

    for path in sorted(unpacked_root.rglob("*")):
        if not path.is_file():
            continue
        rel_parts = path.relative_to(unpacked_root).parts
        division = "NA"
        network_model = rel_parts[0] if rel_parts else dataset_key
        if rel_parts and re.fullmatch(r"D\d", rel_parts[0]):
            division = rel_parts[0]
            if len(rel_parts) > 1:
                network_model = rel_parts[1]

        problem_match = _PROBLEM_NAME_RE.search(path.name)
        pop_match = _POP_NAME_RE.search(path.name)
        log_match = _POP_LOG_RE.search(path.name)

        scenario_id: int | None = None
        record_key: tuple[str, str, int] | None = None
        if problem_match:
            scenario_id = int(problem_match.group(1))
            record_key = (division, network_model, scenario_id)
            by_key.setdefault(record_key, {})["problem"] = path
        elif pop_match:
            scenario_id = int(pop_match.group(1))
            record_key = (division, network_model, scenario_id)
            by_key.setdefault(record_key, {})["pop_solution"] = path
        elif log_match:
            scenario_id = int(log_match.group(1))
            record_key = (division, network_model, scenario_id)
            by_key.setdefault(record_key, {})["pop_log"] = path

    scenarios = []
    for (division, network_model, scenario_id), paths in sorted(by_key.items()):
        problem_path = paths.get("problem")
        if problem_path is None:
            continue
        scenarios.append(
            ScenarioRecord(
                dataset_key=dataset_key,
                division=division,
                network_model=network_model,
                scenario_id=scenario_id,
                problem_path=problem_path,
                pop_solution_path=paths.get("pop_solution"),
                pop_log_path=paths.get("pop_log"),
            )
        )
    return scenarios
