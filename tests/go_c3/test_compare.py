# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import json
from pathlib import Path
import sys


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.compare import _load_cached_reference_validation, aggregate_comparison_reports


def test_aggregate_comparison_reports_accumulates_reference_and_baseline_metrics() -> None:
    reports = [
        {
            "division": "D2",
            "scenario_track": {"track_key": "C3E4N00073D2:SW1"},
            "leaderboard_gap_pct_benchmark_to_winner": 0.1,
            "references": [
                {
                    "entry": {"team": "Winner"},
                    "local_validation": {"summary_metrics": {"feas": 1, "phys_feas": 1, "obj": 10.0}},
                    "objective_delta_local_minus_archived": 0.0,
                },
                {
                    "entry": {"team": "ARPA-e Benchmark"},
                    "local_validation": {"summary_metrics": {"feas": 1, "phys_feas": 0, "obj": 8.0}},
                    "objective_delta_local_minus_archived": 1.5,
                },
            ],
            "baseline_validation": {
                "validation_summary": {"feas": 1, "phys_feas": 0, "obj": -5.0, "z_penalty": 7.0}
            },
        },
        {
            "division": "D1",
            "scenario_track": {"track_key": "C3E4N00073D1:SW1"},
            "leaderboard_gap_pct_benchmark_to_winner": 0.2,
            "references": [
                {
                    "entry": {"team": "Winner"},
                    "local_validation": {"summary_metrics": {"feas": 1, "phys_feas": 0, "obj": 11.0}},
                    "objective_delta_local_minus_archived": -0.25,
                }
            ],
            "baseline_validation": {
                "validation_summary": {"feas": 1, "phys_feas": 0, "obj": -6.0, "z_penalty": 9.0}
            },
        },
    ]

    aggregates = aggregate_comparison_reports(reports)

    assert aggregates["scenario_count"] == 2
    assert aggregates["counts"]["top_reference"] == 2
    assert aggregates["counts"]["benchmark_reference"] == 1
    assert aggregates["counts"]["baseline"] == 2
    assert aggregates["top_reference"]["obj"] == 21.0
    assert aggregates["top_reference"]["phys_feas"] == 1.0
    assert aggregates["top_reference"]["objective_delta_local_minus_archived"] == -0.25
    assert aggregates["benchmark_reference"]["obj"] == 8.0
    assert aggregates["benchmark_reference"]["objective_delta_local_minus_archived"] == 1.5
    assert aggregates["baseline"]["z_penalty"] == 16.0
    assert aggregates["by_division"]["D2"]["scenario_count"] == 1
    assert aggregates["by_division"]["D1"]["counts"]["baseline"] == 1
    assert aggregates["by_track"]["C3E4N00073D2:SW1"]["leaderboard"]["benchmark_gap_pct_to_winner"] == 0.1


def test_load_cached_reference_validation_requires_matching_problem_and_solution(tmp_path: Path) -> None:
    workdir = tmp_path / "reference"
    workdir.mkdir()
    (workdir / "run-report.json").write_text(
        json.dumps(
            {
                "problem_path": "/tmp/problem.json",
                "solution_path": "/tmp/solution.json",
                "validation": {"summary_metrics": {"obj": 1.0}},
            }
        ),
        encoding="utf-8",
    )

    assert _load_cached_reference_validation(
        workdir,
        problem_path=Path("/tmp/problem.json"),
        solution_path=Path("/tmp/solution.json"),
    ) == {"summary_metrics": {"obj": 1.0}}
    assert _load_cached_reference_validation(
        workdir,
        problem_path=Path("/tmp/other-problem.json"),
        solution_path=Path("/tmp/solution.json"),
    ) is None
