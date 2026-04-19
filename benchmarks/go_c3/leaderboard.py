#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Helpers for parsing the public GO Challenge 3 leaderboard workbook."""

from __future__ import annotations

from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
import re
import xml.etree.ElementTree as ET
import zipfile


_XLSX_NS = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
_REL_NS = "{http://schemas.openxmlformats.org/officeDocument/2006/relationships}"
_SWITCHING_MODE_RE = re.compile(r"(SW[01])", re.IGNORECASE)


@dataclass(frozen=True)
class LeaderboardEntry:
    rank: int | None
    team: str
    model: str
    scenario_id: int | None
    objective: float | None
    score: float | None
    runtime_seconds: float | None
    time_limit_seconds: float | None
    solution_bytes: int | None
    feasible: int | None
    infeasible: int | None
    exit_code: int | None
    state: str
    reruns: int | None
    scored: float | None
    switching_count: float | None
    language: str
    url: str

    @property
    def switching_mode(self) -> str | None:
        return infer_switching_mode(self.url, self.switching_count)


def _column_number(column_name: str) -> int:
    value = 0
    for char in column_name:
        if char.isalpha():
            value = value * 26 + ord(char.upper()) - 64
    return value


def _read_sheet_rows(path: Path, sheet_name: str) -> list[dict[str, str]]:
    with zipfile.ZipFile(path) as workbook_zip:
        shared_strings: list[str] = []
        if "xl/sharedStrings.xml" in workbook_zip.namelist():
            shared_strings_root = ET.fromstring(workbook_zip.read("xl/sharedStrings.xml"))
            for item in shared_strings_root.findall(f"{_XLSX_NS}si"):
                shared_strings.append("".join(text.text or "" for text in item.iter(f"{_XLSX_NS}t")))

        workbook_root = ET.fromstring(workbook_zip.read("xl/workbook.xml"))
        relationships_root = ET.fromstring(workbook_zip.read("xl/_rels/workbook.xml.rels"))
        relationship_targets = {
            relationship.attrib["Id"]: relationship.attrib["Target"]
            for relationship in relationships_root
        }

        target = None
        for sheet in workbook_root.find(f"{_XLSX_NS}sheets") or []:
            if sheet.attrib.get("name") != sheet_name:
                continue
            relationship_id = sheet.attrib[f"{_REL_NS}id"]
            target = "xl/" + relationship_targets[relationship_id]
            break
        if target is None:
            raise FileNotFoundError(f"sheet {sheet_name!r} not found in {path}")

        sheet_root = ET.fromstring(workbook_zip.read(target))
        rows: list[dict[str, str]] = []
        for row in sheet_root.iter(f"{_XLSX_NS}row"):
            values: dict[str, str] = {}
            for cell in row.findall(f"{_XLSX_NS}c"):
                cell_ref = cell.attrib.get("r", "")
                match = re.match(r"([A-Z]+)(\d+)", cell_ref)
                if match is None:
                    continue
                column_number = _column_number(match.group(1))
                cell_type = cell.attrib.get("t")
                value_node = cell.find(f"{_XLSX_NS}v")
                inline_string = cell.find(f"{_XLSX_NS}is")
                if inline_string is not None:
                    value = "".join(text.text or "" for text in inline_string.iter(f"{_XLSX_NS}t"))
                elif value_node is None:
                    value = ""
                else:
                    raw_value = value_node.text or ""
                    value = shared_strings[int(raw_value)] if cell_type == "s" and raw_value.isdigit() else raw_value
                values[str(column_number)] = value
            if values:
                rows.append(values)
        return rows


def _maybe_float(raw: str | None) -> float | None:
    if raw in (None, ""):
        return None
    try:
        return float(raw)
    except ValueError:
        return None


def _maybe_int(raw: str | None) -> int | None:
    value = _maybe_float(raw)
    if value is None:
        return None
    return int(value)


def parse_data2_rows(rows: list[dict[str, str]]) -> list[LeaderboardEntry]:
    if not rows:
        return []
    header = rows[0]
    columns = {header.get(column_id, ""): column_id for column_id in header}

    def cell(row: dict[str, str], name: str) -> str:
        column_id = columns.get(name)
        return row.get(column_id, "") if column_id is not None else ""

    parsed: list[LeaderboardEntry] = []
    for row in rows[1:]:
        parsed.append(
            LeaderboardEntry(
                rank=_maybe_int(cell(row, "rank")),
                team=cell(row, "team"),
                model=cell(row, "model"),
                scenario_id=_maybe_int(cell(row, "scenario")),
                objective=_maybe_float(cell(row, "objective")),
                score=_maybe_float(cell(row, "score")),
                runtime_seconds=_maybe_float(cell(row, "runtime")),
                time_limit_seconds=_maybe_float(cell(row, "timelimit")),
                solution_bytes=_maybe_int(cell(row, "soln bytes")),
                feasible=_maybe_int(cell(row, "feas")),
                infeasible=_maybe_int(cell(row, "infeas")),
                exit_code=_maybe_int(cell(row, "exitcode")),
                state=cell(row, "srun_state"),
                reruns=_maybe_int(cell(row, "reruns")),
                scored=_maybe_float(cell(row, "scored")),
                switching_count=_maybe_float(cell(row, "switches")),
                language=cell(row, "language"),
                url=cell(row, "url"),
            )
        )
    return parsed


@lru_cache(maxsize=32)
def _load_event_workbook_cached(resolved_path: str, mtime_ns: int, sheet_name: str) -> tuple[LeaderboardEntry, ...]:
    # Keyed by resolved absolute path + mtime so the cache auto-invalidates
    # when the underlying xlsx is rewritten on disk.
    return tuple(parse_data2_rows(_read_sheet_rows(Path(resolved_path), sheet_name)))


def load_event_workbook(path: Path, *, sheet_name: str = "data2") -> list[LeaderboardEntry]:
    # Cold-parse of the 30 MB competition xlsx takes ~13 s (3.5M regex calls).
    # The workbook is the same file for every scenario in an event, so cache
    # per server process. `list(...)` shim keeps the caller-visible contract.
    resolved = str(path.resolve())
    mtime_ns = path.stat().st_mtime_ns
    return list(_load_event_workbook_cached(resolved, mtime_ns, sheet_name))


def infer_switching_mode(url: str | None, switching_count: float | None = None) -> str | None:
    if url:
        match = _SWITCHING_MODE_RE.search(url)
        if match is not None:
            return match.group(1).upper()
    if switching_count is None:
        return None
    return "SW1" if float(switching_count) > 0.0 else "SW0"


def _normalize_switching_mode(switching_mode: str | None) -> str | None:
    if switching_mode is None:
        return None
    normalized = switching_mode.strip().upper()
    if normalized not in {"SW0", "SW1"}:
        raise ValueError(f"unsupported switching mode: {switching_mode!r}")
    return normalized


def track_key(model: str, switching_mode: str | None) -> str:
    return f"{model}:{_normalize_switching_mode(switching_mode) or 'unknown'}"


def scenario_leaderboard(
    entries: list[LeaderboardEntry],
    *,
    model: str,
    scenario_id: int,
    switching_mode: str | None = None,
) -> list[LeaderboardEntry]:
    normalized_switching_mode = _normalize_switching_mode(switching_mode)
    selected = [
        entry
        for entry in entries
        if entry.model == model and entry.scenario_id == scenario_id
        and (
            normalized_switching_mode is None
            or entry.switching_mode == normalized_switching_mode
        )
    ]
    return sorted(
        selected,
        key=lambda entry: (
            entry.rank is None,
            entry.rank if entry.rank is not None else 10**9,
            entry.team,
        ),
    )


def benchmark_entry(entries: list[LeaderboardEntry]) -> LeaderboardEntry | None:
    for entry in entries:
        if entry.team == "ARPA-e Benchmark":
            return entry
    return None
