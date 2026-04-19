#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Manifest loading for public GO Challenge 3 datasets, suites, and native cases."""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path
from typing import Any

from .paths import manifests_dir


@dataclass(frozen=True)
class DatasetResource:
    key: str
    title: str
    url: str
    stage: str
    family: str
    size_hint_buses: int | None
    division: str | None
    released_on: str
    format: str = "zip"
    notes: str = ""


@dataclass(frozen=True)
class DatasetManifest:
    version: int
    generated_on: str
    source: str
    datasets: tuple[DatasetResource, ...]

    def by_key(self) -> dict[str, DatasetResource]:
        return {dataset.key: dataset for dataset in self.datasets}


@dataclass(frozen=True)
class SuiteTarget:
    dataset: str
    divisions: tuple[str, ...]
    scenario_ids: tuple[int, ...]
    validate_pop: bool
    solve_baseline: bool
    switching_mode: str | None = None


@dataclass(frozen=True)
class Suite:
    name: str
    description: str
    targets: tuple[SuiteTarget, ...]


@dataclass(frozen=True)
class SuiteManifest:
    version: int
    suites: tuple[Suite, ...]

    def by_name(self) -> dict[str, Suite]:
        return {suite.name: suite for suite in self.suites}


@dataclass(frozen=True)
class NativeCaseDefinition:
    name: str
    description: str
    dataset: str
    division: str
    network_model: str
    scenario_id: int
    switching_mode: str
    variants: tuple[str, ...]
    tier: str
    track_examples: bool = False
    notes: str = ""


@dataclass(frozen=True)
class NativeCaseManifest:
    version: int
    cases: tuple[NativeCaseDefinition, ...]

    def by_name(self) -> dict[str, NativeCaseDefinition]:
        return {case.name: case for case in self.cases}


def _read_json(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def load_dataset_manifest(path: Path | None = None) -> DatasetManifest:
    raw = _read_json(path or manifests_dir() / "public-datasets.json")
    datasets = tuple(
        DatasetResource(
            key=item["key"],
            title=item["title"],
            url=item["url"],
            stage=item["stage"],
            family=item["family"],
            size_hint_buses=item.get("size_hint_buses"),
            division=item.get("division"),
            released_on=item["released_on"],
            format=item.get("format", "zip"),
            notes=item.get("notes", ""),
        )
        for item in raw["datasets"]
    )
    return DatasetManifest(
        version=raw["version"],
        generated_on=raw["generated_on"],
        source=raw["source"],
        datasets=datasets,
    )


def load_suite_manifest(path: Path | None = None) -> SuiteManifest:
    raw = _read_json(path or manifests_dir() / "suites.json")
    suites = []
    for suite_raw in raw["suites"]:
        targets = tuple(
            SuiteTarget(
                dataset=item["dataset"],
                divisions=tuple(item.get("divisions", [])),
                scenario_ids=tuple(item.get("scenario_ids", [])),
                validate_pop=bool(item.get("validate_pop", True)),
                solve_baseline=bool(item.get("solve_baseline", False)),
                switching_mode=item.get("switching_mode"),
            )
            for item in suite_raw["targets"]
        )
        suites.append(
            Suite(
                name=suite_raw["name"],
                description=suite_raw["description"],
                targets=targets,
            )
        )
    return SuiteManifest(version=raw["version"], suites=tuple(suites))


def load_native_case_manifest(path: Path | None = None) -> NativeCaseManifest:
    raw = _read_json(path or manifests_dir() / "native-cases.json")
    cases = tuple(
        NativeCaseDefinition(
            name=item["name"],
            description=item["description"],
            dataset=item["dataset"],
            division=item["division"],
            network_model=item["network_model"],
            scenario_id=int(item["scenario_id"]),
            switching_mode=str(item["switching_mode"]).upper(),
            variants=tuple(str(value) for value in item.get("variants", ("full", "t00"))),
            tier=item["tier"],
            track_examples=bool(item.get("track_examples", False)),
            notes=item.get("notes", ""),
        )
        for item in raw["cases"]
    )
    return NativeCaseManifest(version=raw["version"], cases=cases)
