# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Tests for score ledger."""

from __future__ import annotations

import json
from pathlib import Path
from unittest import mock
import sys

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.ledger import (
    LedgerDelta,
    LedgerEntry,
    compare_against_ledger,
    format_ledger_status,
    load_ledger,
    record_entry,
    save_ledger,
    scenario_key,
    update_ledger_from_run_reports,
)


def test_scenario_key_formatting() -> None:
    assert scenario_key("event4_73", "D2", 911) == "event4_73/D2/911"
    assert scenario_key("event4_73", "D1", 3) == "event4_73/D1/003"


def test_load_ledger_empty(tmp_path: Path) -> None:
    with mock.patch("benchmarks.go_c3.ledger.ledger_dir", return_value=tmp_path):
        result = load_ledger("sced_fixed")
    assert result == {}


def test_save_load_round_trip(tmp_path: Path) -> None:
    entries = {
        "event4_73/D2/911": {
            "scenario_key": "event4_73/D2/911",
            "mode": "sced_fixed",
            "our_obj": -6234.5,
            "ref_obj": -6200.0,
        }
    }
    with mock.patch("benchmarks.go_c3.ledger.ledger_dir", return_value=tmp_path):
        path = save_ledger("sced_fixed", entries)
        assert path.exists()
        loaded = load_ledger("sced_fixed")
    assert loaded["event4_73/D2/911"]["our_obj"] == -6234.5


def test_record_entry_creates_file(tmp_path: Path) -> None:
    with mock.patch("benchmarks.go_c3.ledger.ledger_dir", return_value=tmp_path):
        entry = record_entry(
            "sced_fixed",
            dataset_key="event4_73",
            division="D2",
            scenario_id=911,
            our_obj=-6234.5,
            ref_obj=-6200.0,
            ref_team="WinnerTeam",
        )
    assert entry.scenario_key == "event4_73/D2/911"
    assert entry.gap_pct is not None
    assert abs(entry.gap_pct - (-6234.5 - (-6200.0)) / abs(-6200.0) * 100.0) < 1e-10
    ledger_file = tmp_path / "sced_fixed.json"
    assert ledger_file.exists()


def test_record_entry_updates_existing(tmp_path: Path) -> None:
    with mock.patch("benchmarks.go_c3.ledger.ledger_dir", return_value=tmp_path):
        record_entry("sced_fixed", dataset_key="event4_73", division="D2", scenario_id=911, our_obj=-6000.0)
        record_entry("sced_fixed", dataset_key="event4_73", division="D2", scenario_id=911, our_obj=-6100.0)
        ledger = load_ledger("sced_fixed")
    assert ledger["event4_73/D2/911"]["our_obj"] == -6100.0


def test_compare_against_ledger_detects_improvement(tmp_path: Path) -> None:
    with mock.patch("benchmarks.go_c3.ledger.ledger_dir", return_value=tmp_path):
        save_ledger("sced_fixed", {
            "event4_73/D2/911": {"our_obj": -6000.0, "gap_pct": 1.0},
        })
        current = [LedgerEntry(
            scenario_key="event4_73/D2/911", mode="sced_fixed",
            our_obj=-6100.0, ref_obj=None, ref_team=None, gap_pct=None,
            our_feas=1, our_z_penalty=0.0, timestamp="", git_sha=None, notes="",
        )]
        deltas = compare_against_ledger("sced_fixed", current)
    assert len(deltas) == 1
    assert deltas[0].improved is True
    assert deltas[0].regressed is False


def test_compare_against_ledger_detects_regression(tmp_path: Path) -> None:
    with mock.patch("benchmarks.go_c3.ledger.ledger_dir", return_value=tmp_path):
        save_ledger("sced_fixed", {
            "event4_73/D2/911": {"our_obj": -6100.0, "gap_pct": 0.5},
        })
        current = [LedgerEntry(
            scenario_key="event4_73/D2/911", mode="sced_fixed",
            our_obj=-6000.0, ref_obj=None, ref_team=None, gap_pct=None,
            our_feas=1, our_z_penalty=0.0, timestamp="", git_sha=None, notes="",
        )]
        deltas = compare_against_ledger("sced_fixed", current)
    assert len(deltas) == 1
    assert deltas[0].improved is False
    assert deltas[0].regressed is True


def test_compare_against_ledger_detects_new(tmp_path: Path) -> None:
    with mock.patch("benchmarks.go_c3.ledger.ledger_dir", return_value=tmp_path):
        save_ledger("sced_fixed", {})
        current = [LedgerEntry(
            scenario_key="event4_73/D2/911", mode="sced_fixed",
            our_obj=-6000.0, ref_obj=None, ref_team=None, gap_pct=None,
            our_feas=1, our_z_penalty=0.0, timestamp="", git_sha=None, notes="",
        )]
        deltas = compare_against_ledger("sced_fixed", current)
    assert deltas[0].is_new is True


def test_format_ledger_status() -> None:
    deltas = [
        LedgerDelta("event4_73/D2/911", "sced_fixed", -6000.0, -6100.0, -100.0, 1.0, 0.5, True, False, False),
        LedgerDelta("event4_73/D2/912", "sced_fixed", -5000.0, -4900.0, 100.0, 0.5, 1.0, False, True, False),
        LedgerDelta("event4_73/D2/913", "sced_fixed", None, -5500.0, None, None, None, False, False, True),
    ]
    lines = format_ledger_status(deltas)
    assert any("Improved" in line for line in lines)
    assert any("Regressed" in line for line in lines)
    assert any("New" in line for line in lines)


def test_update_ledger_from_run_reports(tmp_path: Path) -> None:
    reports = [
        {
            "dataset_key": "event4_73",
            "division": "D2",
            "scenario_id": 911,
            "status": "ok",
            "dispatch_summary": {"total_cost": -6234.5},
        },
    ]
    with mock.patch("benchmarks.go_c3.ledger.ledger_dir", return_value=tmp_path):
        entries, deltas = update_ledger_from_run_reports(
            "sced_fixed",
            reports,
            ref_objectives={"event4_73/D2/911": -6200.0},
            notes="test run",
        )
    assert len(entries) == 1
    assert entries[0].our_obj == -6234.5
    assert deltas[0].is_new is True
    # Verify persisted
    with mock.patch("benchmarks.go_c3.ledger.ledger_dir", return_value=tmp_path):
        ledger = load_ledger("sced_fixed")
    assert "event4_73/D2/911" in ledger
