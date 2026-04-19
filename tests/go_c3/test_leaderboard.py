# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from pathlib import Path
import sys


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.leaderboard import benchmark_entry, infer_switching_mode, parse_data2_rows, scenario_leaderboard, track_key


def test_parse_data2_rows_and_select_scenario_entries() -> None:
    rows = [
        {
            "1": "rank&team",
            "2": "rank",
            "3": "team",
            "4": "model",
            "5": "scenario",
            "7": "objective",
            "9": "score",
            "10": "runtime",
            "11": "timelimit",
            "12": "soln bytes",
            "13": "feas",
            "14": "infeas",
            "15": "exitcode",
            "16": "srun_state",
            "17": "reruns",
            "18": "scored",
            "19": "switches",
            "20": "url",
            "26": "language",
        },
        {
            "2": "2",
            "3": "ARPA-e Benchmark",
            "4": "C3E4N00617D2",
            "5": "2",
            "7": "163637410.323598",
            "9": "163637410.323598",
            "10": "592",
            "11": "7200",
            "12": "12973000",
            "13": "1",
            "14": "0",
            "15": "0",
            "16": "COMPLETED",
            "17": "1",
            "18": "45176.846777800929",
            "19": "1",
            "20": "https://example.test/benchmark",
            "26": "cpp",
        },
        {
            "2": "1",
            "3": "TIM-GO",
            "4": "C3E4N00617D2",
            "5": "2",
            "7": "163780452.926727",
            "9": "163780452.926727",
            "10": "189",
            "11": "7200",
            "12": "6197190",
            "13": "1",
            "14": "0",
            "15": "0",
            "16": "COMPLETED",
            "17": "1",
            "18": "45176.190524745369",
            "19": "1",
            "20": "https://example.test/tim-go",
            "26": "julia",
        },
    ]

    entries = parse_data2_rows(rows)
    selected = scenario_leaderboard(entries, model="C3E4N00617D2", scenario_id=2)

    assert [entry.team for entry in selected] == ["TIM-GO", "ARPA-e Benchmark"]
    assert selected[0].objective == 163780452.926727
    assert benchmark_entry(selected) is not None
    assert benchmark_entry(selected).team == "ARPA-e Benchmark"


def test_switching_mode_helpers_prefer_url_but_fall_back_to_switch_count() -> None:
    assert infer_switching_mode("https://example.test/C3E4_2_SW1_file.tar.gz", 0.0) == "SW1"
    assert infer_switching_mode("", 0.0) == "SW0"
    assert infer_switching_mode(None, 2.0) == "SW1"
    assert track_key("C3E4N00073D2", "SW1") == "C3E4N00073D2:SW1"


def test_scenario_leaderboard_filters_by_switching_mode() -> None:
    rows = [
        {
            "1": "rank&team",
            "2": "rank",
            "3": "team",
            "4": "model",
            "5": "scenario",
            "7": "objective",
            "19": "switches",
            "20": "url",
            "26": "language",
        },
        {
            "2": "1",
            "3": "Winner SW0",
            "4": "C3E4N00073D2",
            "5": "911",
            "7": "100",
            "19": "0",
            "20": "https://example.test/C3E4N00073D2_SW0.tar.gz",
            "26": "julia",
        },
        {
            "2": "1",
            "3": "Winner SW1",
            "4": "C3E4N00073D2",
            "5": "911",
            "7": "110",
            "19": "2",
            "20": "https://example.test/C3E4N00073D2_SW1.tar.gz",
            "26": "julia",
        },
    ]

    entries = parse_data2_rows(rows)
    selected = scenario_leaderboard(entries, model="C3E4N00073D2", scenario_id=911, switching_mode="SW0")

    assert [entry.team for entry in selected] == ["Winner SW0"]
