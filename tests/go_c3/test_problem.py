# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import json
from pathlib import Path
import sys


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from markets.go_c3.problem import GoC3Problem


def _write_problem(path: Path, interval_duration: list[float]) -> Path:
    payload = {
        "network": {
            "general": {"base_norm_mva": 100.0},
            "violation_cost": {},
            "bus": [
                {
                    "uid": "bus_a",
                    "vm_lb": 0.95,
                    "vm_ub": 1.05,
                    "active_reserve_uids": [],
                    "reactive_reserve_uids": [],
                    "base_nom_volt": 138.0,
                    "initial_status": {"vm": 1.0, "va": 0.0},
                }
            ],
            "shunt": [],
            "simple_dispatchable_device": [],
            "ac_line": [],
            "two_winding_transformer": [],
            "dc_line": [],
            "active_zonal_reserve": [],
            "reactive_zonal_reserve": [],
        },
        "time_series_input": {
            "general": {"time_periods": len(interval_duration), "interval_duration": interval_duration},
            "simple_dispatchable_device": [],
            "active_zonal_reserve": [],
            "reactive_zonal_reserve": [],
        },
        "reliability": {"contingency": []},
    }
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


def test_problem_summary_reports_uniform_intervals(tmp_path):
    problem = GoC3Problem.load(_write_problem(tmp_path / "scenario.json", [1.0, 1.0, 1.0]))

    summary = problem.summary()

    assert summary["periods"] == 3
    assert summary["uniform_intervals"] is True
    assert problem.representative_interval_hours == 1.0


def test_problem_summary_reports_average_for_non_uniform_intervals(tmp_path):
    problem = GoC3Problem.load(_write_problem(tmp_path / "scenario.json", [0.25, 0.25, 1.0]))

    assert problem.has_uniform_intervals is False
    assert problem.representative_interval_hours == (0.25 + 0.25 + 1.0) / 3.0
