# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import json
from pathlib import Path
import sys


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.validator import (
    ValidatorEnvironment,
    _compare_bus_residual_payloads,
    _expected_metadata,
    _parse_stdout_metrics,
    ensure_validator_environment,
    validate_with_official_tool,
    validator_summary,
)


def test_validator_summary_promotes_z_to_obj_and_keeps_score_components() -> None:
    summary = {
        "problem": {
            "surplus_total": 123.0,
            "pass": 1,
        },
        "solution": {
            "pass": 1,
        },
        "evaluation": {
            "z": -45.5,
            "z_base": -40.0,
            "z_penalty": 5.5,
            "phys_feas": 0,
            "feas": 1,
            "infeas": 0,
        },
    }

    compact = validator_summary(summary)

    assert compact["obj"] == -45.5
    assert compact["z"] == -45.5
    assert compact["z_base"] == -40.0
    assert compact["z_penalty"] == 5.5
    assert compact["phys_feas"] == 0
    assert compact["problem_surplus_total"] == 123.0
    assert compact["problem_pass"] == 1
    assert compact["pass"] == 1


def test_parse_stdout_metrics_extracts_obj_and_feas() -> None:
    stdout = "\n".join(
        [
            "some log line",
            "feas: 1",
            "obj: -6740549193.701818",
            "",
        ]
    )

    assert _parse_stdout_metrics(stdout) == {
        "feas": 1,
        "obj": -6740549193.701818,
    }


def test_compare_bus_residual_payloads_ranks_lhs_worse_buses() -> None:
    lhs = {
        "metrics": {
            "sum_bus_t_z_p": 12.0,
            "sum_bus_t_z_q": 8.0,
        },
        "bus_p": [
            {"bus_uid": "bus_02", "sum_abs_residual_pu_hours": 5.0, "max_abs_residual_pu": 2.0, "worst_period": 1},
            {"bus_uid": "bus_29", "sum_abs_residual_pu_hours": 3.0, "max_abs_residual_pu": 1.0, "worst_period": 2},
        ],
        "bus_q": [
            {"bus_uid": "bus_02", "sum_abs_residual_pu_hours": 4.0, "max_abs_residual_pu": 1.5, "worst_period": 1},
        ],
    }
    rhs = {
        "metrics": {
            "sum_bus_t_z_p": 4.0,
            "sum_bus_t_z_q": 2.0,
        },
        "bus_p": [
            {"bus_uid": "bus_02", "sum_abs_residual_pu_hours": 1.0, "max_abs_residual_pu": 0.5, "worst_period": 3},
            {"bus_uid": "bus_29", "sum_abs_residual_pu_hours": 6.0, "max_abs_residual_pu": 2.5, "worst_period": 4},
        ],
        "bus_q": [
            {"bus_uid": "bus_02", "sum_abs_residual_pu_hours": 0.5, "max_abs_residual_pu": 0.25, "worst_period": 2},
        ],
    }

    comparison = _compare_bus_residual_payloads(lhs, rhs, top_k=2)

    assert comparison["metrics"]["lhs_minus_rhs_sum_bus_t_z_p"] == 8.0
    assert comparison["metrics"]["lhs_minus_rhs_sum_bus_t_z_q"] == 6.0
    assert comparison["bus_p"]["top_lhs_worse"][0]["bus_uid"] == "bus_02"
    assert comparison["bus_p"]["top_lhs_worse"][0]["lhs_minus_rhs_sum_abs_residual_pu_hours"] == 4.0
    assert comparison["bus_p"]["top_lhs_better"][0]["bus_uid"] == "bus_29"
    assert comparison["bus_q"]["top_lhs_worse"][0]["bus_uid"] == "bus_02"


def test_ensure_validator_environment_resolves_relative_cache_root(tmp_path, monkeypatch) -> None:
    monkeypatch.chdir(tmp_path)
    cache_root = tmp_path / "cache"
    source_root = cache_root / "validator-src"
    venv_dir = cache_root / "validator-venv"
    c3_dir = source_root / "C3DataUtilities"
    model_dir = source_root / "GO-3-data-model"
    python_executable = venv_dir / ("Scripts" if sys.platform.startswith("win") else "bin") / "python"

    python_executable.parent.mkdir(parents=True, exist_ok=True)
    python_executable.write_text("", encoding="utf-8")
    c3_dir.mkdir(parents=True, exist_ok=True)
    model_dir.mkdir(parents=True, exist_ok=True)
    (cache_root / "validator-metadata.json").write_text(
        json.dumps(_expected_metadata(venv_dir), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    env = ensure_validator_environment(Path("cache"))

    assert env.cache_root == cache_root.resolve()
    assert env.python_executable == python_executable.resolve()


def test_validate_with_official_tool_uses_absolute_problem_and_solution_paths(tmp_path, monkeypatch) -> None:
    cache_root = tmp_path / "cache"
    c3_dir = cache_root / "validator-src" / "C3DataUtilities"
    model_dir = cache_root / "validator-src" / "GO-3-data-model"
    python_executable = cache_root / "validator-venv" / ("Scripts" if sys.platform.startswith("win") else "bin") / "python"
    c3_dir.mkdir(parents=True, exist_ok=True)
    model_dir.mkdir(parents=True, exist_ok=True)
    python_executable.parent.mkdir(parents=True, exist_ok=True)
    python_executable.write_text("", encoding="utf-8")
    env = ValidatorEnvironment(
        cache_root=cache_root,
        source_root=cache_root / "validator-src",
        venv_dir=cache_root / "validator-venv",
        python_executable=python_executable,
        c3_data_utilities_dir=c3_dir,
        go3_data_model_dir=model_dir,
    )
    problem_path = tmp_path / "problem.json"
    solution_path = tmp_path / "solution.json"
    problem_path.write_text("{}", encoding="utf-8")
    solution_path.write_text("{}", encoding="utf-8")
    captured = {}

    def fake_run(command, cwd=None, capture_output=None, text=None, check=None):
        captured["command"] = command
        captured["cwd"] = cwd
        return type("Proc", (), {"returncode": 0, "stdout": "", "stderr": ""})()

    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr("benchmarks.go_c3.validator.subprocess.run", fake_run)

    report = validate_with_official_tool(
        env,
        Path("problem.json"),
        solution_path=Path("solution.json"),
        workdir=Path("workdir"),
    )

    assert report["command"][3] == str(problem_path.resolve())
    assert report["command"][5] == str(solution_path.resolve())
