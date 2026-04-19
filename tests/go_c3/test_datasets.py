# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from pathlib import Path
import sys


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.datasets import discover_scenarios


def test_discover_scenarios_handles_event4_layout(tmp_path):
    root = tmp_path / "event4_73"
    scenario_dir = root / "D2" / "C3E4N00073D2"
    scenario_dir.mkdir(parents=True)
    (scenario_dir / "scenario_303.json").write_text("{}", encoding="utf-8")
    (scenario_dir / "scenario_303.json.pop_solution.json").write_text("{}", encoding="utf-8")
    (scenario_dir / "scenario_303.json.popsolution.log").write_text("ok\n", encoding="utf-8")

    records = discover_scenarios(root, "event4_73")

    assert len(records) == 1
    record = records[0]
    assert record.dataset_key == "event4_73"
    assert record.division == "D2"
    assert record.network_model == "C3E4N00073D2"
    assert record.scenario_id == 303
    assert record.pop_solution_path is not None
    assert record.pop_log_path is not None


def test_discover_scenarios_ignores_orphan_pop_solution(tmp_path):
    root = tmp_path / "event4_73"
    scenario_dir = root / "D1" / "C3E4N00073D1"
    scenario_dir.mkdir(parents=True)
    (scenario_dir / "scenario_303.json.pop_solution.json").write_text("{}", encoding="utf-8")

    assert discover_scenarios(root, "event4_73") == []
