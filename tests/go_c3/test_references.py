# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from pathlib import Path
import io
import json
import sys
import tarfile


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.leaderboard import LeaderboardEntry
from benchmarks.go_c3.references import (
    ensure_reference_submission,
    event_index_from_dataset_key,
    event_index_from_model,
    reference_archive_path,
    select_reference_entries,
)


def _entry(*, team: str = "TIM-GO", url: str = "https://example.test/sample.tar.gz") -> LeaderboardEntry:
    return LeaderboardEntry(
        rank=1,
        team=team,
        model="C3E4N00073D2",
        scenario_id=303,
        objective=147785503.054286,
        score=147785503.054286,
        runtime_seconds=40.0,
        time_limit_seconds=7200.0,
        solution_bytes=397165,
        feasible=1,
        infeasible=0,
        exit_code=0,
        state="COMPLETED",
        reruns=1,
        scored=45176.0,
        switching_count=0.0,
        language="cpp",
        url=url,
    )


def _write_member(archive: tarfile.TarFile, name: str, payload: bytes) -> None:
    info = tarfile.TarInfo(name=name)
    info.size = len(payload)
    archive.addfile(info, io.BytesIO(payload))


def test_event_index_helpers() -> None:
    assert event_index_from_dataset_key("event4_73") == 4
    assert event_index_from_model("C3E4N00073D2") == 4


def test_reference_archive_path_uses_url_filename(tmp_path: Path) -> None:
    path = reference_archive_path(_entry(url="https://example.test/foo/bar/sample-name.tar.gz"), cache_root=tmp_path)
    assert path == tmp_path / "reference-submissions" / "event4" / "sample-name.tar.gz"


def test_select_reference_entries_keeps_top_k_and_benchmark() -> None:
    selected = select_reference_entries(
        [
            _entry(team="Winner"),
            _entry(team="RunnerUp"),
            _entry(team="ARPA-e Benchmark"),
        ],
        include_benchmark=True,
        top_k=1,
    )
    assert [entry.team for entry in selected] == ["Winner", "ARPA-e Benchmark"]


def test_ensure_reference_submission_extracts_solution_and_summary(tmp_path: Path) -> None:
    archive_path = reference_archive_path(_entry(), cache_root=tmp_path)
    archive_path.parent.mkdir(parents=True, exist_ok=True)
    summary = {
        "problem": {"surplus_total": 149185153.7025907, "pass": 1},
        "solution": {"pass": 1},
        "evaluation": {"z": 147785503.05428633, "feas": 1, "phys_feas": 1},
    }
    with tarfile.open(archive_path, "w:gz") as archive:
        _write_member(archive, "solution.json", json.dumps({"time_series_output": {}}).encode("utf-8"))
        _write_member(archive, "summary.json", json.dumps(summary).encode("utf-8"))
        _write_member(archive, "summary.csv", b"objective\n147785503.05428633\n")

    submission = ensure_reference_submission(_entry(), cache_root=tmp_path)

    assert submission.solution_path.exists()
    assert submission.summary_json_path is not None
    assert submission.archived_metrics["obj"] == 147785503.05428633
    assert submission.archived_metrics["problem_surplus_total"] == 149185153.7025907
