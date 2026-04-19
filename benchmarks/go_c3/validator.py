#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Official GO Challenge 3 validator environment management."""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import textwrap

from .paths import default_cache_root


C3_DATA_UTILITIES_URL = "https://github.com/GOCompetition/C3DataUtilities.git"
C3_DATA_UTILITIES_REF = "bb5df337553b21ab8be89ae5f9106958541730d4"
GO3_DATA_MODEL_URL = "https://github.com/Smart-DS/GO-3-data-model.git"
GO3_DATA_MODEL_REF = "5472a2373f456cc7e9923cdd31be1d4345d9830f"
VALIDATOR_PYTHON_REQUIREMENTS = [
    "pydantic<2",
    # C3DataUtilities pinned at bb5df337 still calls numpy.reshape(..., newshape=...),
    # which numpy 2.x removed. Pin numpy<2 so the venv stays compatible with the
    # vendored validator until upstream is updated.
    "numpy<2",
    "pandas",
    "scipy",
    "networkx",
    "psutil",
]


_BUS_RESIDUAL_EXTRACT_SCRIPT = textwrap.dedent(
    """
    import json
    import sys
    import numpy
    from datamodel.input.data import InputDataFile
    from datamodel.output.data import OutputDataFile
    from datautilities import arraydata, evaluation, utils, validation


    def _signed_bus_p_shortfall(solution_evaluator):
        out = numpy.zeros(shape=(solution_evaluator.problem.num_bus, solution_evaluator.problem.num_t), dtype=float)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_sd_inj_mat, solution_evaluator.sd_t_p, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_sh_inj_mat, solution_evaluator.sh_t_p, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_acl_fr_inj_mat, solution_evaluator.acl_t_p_fr, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_acl_to_inj_mat, solution_evaluator.acl_t_p_to, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_dcl_fr_inj_mat, solution_evaluator.dcl_t_p, out=out)
        dcl_t_float = numpy.zeros_like(solution_evaluator.dcl_t_p)
        numpy.negative(solution_evaluator.dcl_t_p, out=dcl_t_float)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_dcl_to_inj_mat, dcl_t_float, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_xfr_fr_inj_mat, solution_evaluator.xfr_t_p_fr, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_xfr_to_inj_mat, solution_evaluator.xfr_t_p_to, out=out)
        numpy.negative(out, out=out)
        return out


    def _signed_bus_q_shortfall(solution_evaluator):
        out = numpy.zeros(shape=(solution_evaluator.problem.num_bus, solution_evaluator.problem.num_t), dtype=float)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_sd_inj_mat, solution_evaluator.sd_t_q, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_sh_inj_mat, solution_evaluator.sh_t_q, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_acl_fr_inj_mat, solution_evaluator.acl_t_q_fr, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_acl_to_inj_mat, solution_evaluator.acl_t_q_to, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_dcl_fr_inj_mat, solution_evaluator.dcl_t_q_fr, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_dcl_to_inj_mat, solution_evaluator.dcl_t_q_to, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_xfr_fr_inj_mat, solution_evaluator.xfr_t_q_fr, out=out)
        utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_xfr_to_inj_mat, solution_evaluator.xfr_t_q_to, out=out)
        numpy.negative(out, out=out)
        return out


    def _bus_term_matrix(solution_evaluator, term_key):
        out = numpy.zeros(shape=(solution_evaluator.problem.num_bus, solution_evaluator.problem.num_t), dtype=float)
        if term_key == "sd_p":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_sd_inj_mat, solution_evaluator.sd_t_p, out=out)
        elif term_key == "sh_p":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_sh_inj_mat, solution_evaluator.sh_t_p, out=out)
        elif term_key == "acl_fr_p":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_acl_fr_inj_mat, solution_evaluator.acl_t_p_fr, out=out)
        elif term_key == "acl_to_p":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_acl_to_inj_mat, solution_evaluator.acl_t_p_to, out=out)
        elif term_key == "dcl_fr_p":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_dcl_fr_inj_mat, solution_evaluator.dcl_t_p, out=out)
        elif term_key == "dcl_to_p":
            dcl_t_float = numpy.zeros_like(solution_evaluator.dcl_t_p)
            numpy.negative(solution_evaluator.dcl_t_p, out=dcl_t_float)
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_dcl_to_inj_mat, dcl_t_float, out=out)
        elif term_key == "xfr_fr_p":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_xfr_fr_inj_mat, solution_evaluator.xfr_t_p_fr, out=out)
        elif term_key == "xfr_to_p":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_xfr_to_inj_mat, solution_evaluator.xfr_t_p_to, out=out)
        elif term_key == "sd_q":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_sd_inj_mat, solution_evaluator.sd_t_q, out=out)
        elif term_key == "sh_q":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_sh_inj_mat, solution_evaluator.sh_t_q, out=out)
        elif term_key == "acl_fr_q":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_acl_fr_inj_mat, solution_evaluator.acl_t_q_fr, out=out)
        elif term_key == "acl_to_q":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_acl_to_inj_mat, solution_evaluator.acl_t_q_to, out=out)
        elif term_key == "dcl_fr_q":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_dcl_fr_inj_mat, solution_evaluator.dcl_t_q_fr, out=out)
        elif term_key == "dcl_to_q":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_dcl_to_inj_mat, solution_evaluator.dcl_t_q_to, out=out)
        elif term_key == "xfr_fr_q":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_xfr_fr_inj_mat, solution_evaluator.xfr_t_q_fr, out=out)
        elif term_key == "xfr_to_q":
            utils.csr_mat_vec_add_to_vec(solution_evaluator.bus_xfr_to_inj_mat, solution_evaluator.xfr_t_q_to, out=out)
        else:
            raise KeyError(term_key)
        return out


    def _per_bus_summary(problem_data_array, signed_shortfall):
        abs_shortfall = numpy.absolute(signed_shortfall)
        duration = numpy.reshape(problem_data_array.t_d, newshape=(1, problem_data_array.num_t))
        weighted_abs = abs_shortfall * duration
        per_bus = []
        for bus_idx, bus_uid in enumerate(problem_data_array.bus_uid):
            bus_abs = abs_shortfall[bus_idx]
            bus_weighted = weighted_abs[bus_idx]
            worst_period = int(bus_abs.argmax()) if bus_abs.size else 0
            worst_signed = float(signed_shortfall[bus_idx, worst_period]) if bus_abs.size else 0.0
            per_bus.append(
                {
                    "bus_uid": str(bus_uid),
                    "sum_abs_residual_pu": float(bus_abs.sum()),
                    "sum_abs_residual_pu_hours": float(bus_weighted.sum()),
                    "max_abs_residual_pu": float(bus_abs.max()) if bus_abs.size else 0.0,
                    "worst_period": worst_period,
                    "worst_signed_residual_pu": worst_signed,
                    "worst_abs_residual_pu": float(abs(worst_signed)),
                    "worst_abs_residual_pu_hours": float(bus_weighted[worst_period]) if bus_abs.size else 0.0,
                }
            )
        per_bus.sort(key=lambda item: (-item["sum_abs_residual_pu_hours"], item["bus_uid"]))
        return per_bus


    def _per_bus_detail(problem_data_array, solution_evaluator, signed_bus_p, signed_bus_q):
        term_mats = {
            "sd_p": _bus_term_matrix(solution_evaluator, "sd_p"),
            "sh_p": _bus_term_matrix(solution_evaluator, "sh_p"),
            "acl_fr_p": _bus_term_matrix(solution_evaluator, "acl_fr_p"),
            "acl_to_p": _bus_term_matrix(solution_evaluator, "acl_to_p"),
            "dcl_fr_p": _bus_term_matrix(solution_evaluator, "dcl_fr_p"),
            "dcl_to_p": _bus_term_matrix(solution_evaluator, "dcl_to_p"),
            "xfr_fr_p": _bus_term_matrix(solution_evaluator, "xfr_fr_p"),
            "xfr_to_p": _bus_term_matrix(solution_evaluator, "xfr_to_p"),
            "sd_q": _bus_term_matrix(solution_evaluator, "sd_q"),
            "sh_q": _bus_term_matrix(solution_evaluator, "sh_q"),
            "acl_fr_q": _bus_term_matrix(solution_evaluator, "acl_fr_q"),
            "acl_to_q": _bus_term_matrix(solution_evaluator, "acl_to_q"),
            "dcl_fr_q": _bus_term_matrix(solution_evaluator, "dcl_fr_q"),
            "dcl_to_q": _bus_term_matrix(solution_evaluator, "dcl_to_q"),
            "xfr_fr_q": _bus_term_matrix(solution_evaluator, "xfr_fr_q"),
            "xfr_to_q": _bus_term_matrix(solution_evaluator, "xfr_to_q"),
        }
        detail = {}
        for bus_idx, bus_uid in enumerate(problem_data_array.bus_uid):
            detail[str(bus_uid)] = {
                "vm_pu": [float(value) for value in solution_evaluator.bus_t_v[bus_idx].tolist()],
                "va_rad": [float(value) for value in solution_evaluator.bus_t_theta[bus_idx].tolist()],
                "p_shortfall_pu": [float(value) for value in signed_bus_p[bus_idx].tolist()],
                "q_shortfall_pu": [float(value) for value in signed_bus_q[bus_idx].tolist()],
                "p_terms_pu": {
                    "sd": [float(value) for value in term_mats["sd_p"][bus_idx].tolist()],
                    "sh": [float(value) for value in term_mats["sh_p"][bus_idx].tolist()],
                    "acl_fr": [float(value) for value in term_mats["acl_fr_p"][bus_idx].tolist()],
                    "acl_to": [float(value) for value in term_mats["acl_to_p"][bus_idx].tolist()],
                    "dcl_fr": [float(value) for value in term_mats["dcl_fr_p"][bus_idx].tolist()],
                    "dcl_to": [float(value) for value in term_mats["dcl_to_p"][bus_idx].tolist()],
                    "xfr_fr": [float(value) for value in term_mats["xfr_fr_p"][bus_idx].tolist()],
                    "xfr_to": [float(value) for value in term_mats["xfr_to_p"][bus_idx].tolist()],
                },
                "q_terms_pu": {
                    "sd": [float(value) for value in term_mats["sd_q"][bus_idx].tolist()],
                    "sh": [float(value) for value in term_mats["sh_q"][bus_idx].tolist()],
                    "acl_fr": [float(value) for value in term_mats["acl_fr_q"][bus_idx].tolist()],
                    "acl_to": [float(value) for value in term_mats["acl_to_q"][bus_idx].tolist()],
                    "dcl_fr": [float(value) for value in term_mats["dcl_fr_q"][bus_idx].tolist()],
                    "dcl_to": [float(value) for value in term_mats["dcl_to_q"][bus_idx].tolist()],
                    "xfr_fr": [float(value) for value in term_mats["xfr_fr_q"][bus_idx].tolist()],
                    "xfr_to": [float(value) for value in term_mats["xfr_to_q"][bus_idx].tolist()],
                },
            }
        return detail


    problem_path = sys.argv[1]
    solution_path = sys.argv[2]
    config_path = sys.argv[3]
    output_path = sys.argv[4]

    config = validation.read_config(config_path)
    problem_data_model = InputDataFile.load(problem_path)
    solution_data_model = OutputDataFile.load(solution_path)

    problem_data_array = arraydata.InputData()
    problem_data_array.set_from_data_model(problem_data_model)

    solution_data_array = arraydata.OutputData()
    solution_data_array.set_from_data_model(problem_data_array, solution_data_model)

    solution_evaluator = evaluation.SolutionEvaluator(problem_data_array, solution_data_array, config=config)
    solution_evaluator.run()

    signed_bus_p = _signed_bus_p_shortfall(solution_evaluator)
    signed_bus_q = _signed_bus_q_shortfall(solution_evaluator)

    payload = {
        "problem_path": problem_path,
        "solution_path": solution_path,
        "metrics": {
            "sum_bus_t_z_p": float(solution_evaluator.problem.c_p * numpy.sum(numpy.absolute(signed_bus_p) * numpy.reshape(problem_data_array.t_d, newshape=(1, problem_data_array.num_t)))),
            "sum_bus_t_z_q": float(solution_evaluator.problem.c_q * numpy.sum(numpy.absolute(signed_bus_q) * numpy.reshape(problem_data_array.t_d, newshape=(1, problem_data_array.num_t)))),
        },
        "bus_p": _per_bus_summary(problem_data_array, signed_bus_p),
        "bus_q": _per_bus_summary(problem_data_array, signed_bus_q),
        "bus_detail": _per_bus_detail(problem_data_array, solution_evaluator, signed_bus_p, signed_bus_q),
    }
    with open(output_path, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
    """
)


class _ValidatorRunLock:
    def __init__(self, path: Path) -> None:
        self._path = path
        self._handle = None

    def __enter__(self) -> "_ValidatorRunLock":
        self._path.parent.mkdir(parents=True, exist_ok=True)
        self._handle = self._path.open("w", encoding="utf-8")
        try:
            import fcntl  # type: ignore

            fcntl.flock(self._handle.fileno(), fcntl.LOCK_EX)
        except ImportError:
            # Non-POSIX fallback: best-effort exclusive creator spin.
            while True:
                try:
                    self._path.touch(exist_ok=False)
                    break
                except FileExistsError:
                    time.sleep(0.1)
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if self._handle is None:
            return
        try:
            import fcntl  # type: ignore

            fcntl.flock(self._handle.fileno(), fcntl.LOCK_UN)
        except ImportError:
            try:
                self._path.unlink(missing_ok=True)
            except OSError:
                pass
        self._handle.close()
        self._handle = None


@dataclass(frozen=True)
class ValidatorEnvironment:
    cache_root: Path
    source_root: Path
    venv_dir: Path
    python_executable: Path
    c3_data_utilities_dir: Path
    go3_data_model_dir: Path

    def metadata(self) -> dict[str, object]:
        return {
            "c3_data_utilities_ref": C3_DATA_UTILITIES_REF,
            "go3_data_model_ref": GO3_DATA_MODEL_REF,
            "python_requirements": VALIDATOR_PYTHON_REQUIREMENTS,
            "venv_dir": str(self.venv_dir),
        }


def _run(command: list[str], cwd: Path | None = None) -> None:
    subprocess.run(command, cwd=cwd, check=True)


def _ensure_checkout(parent: Path, name: str, url: str, ref: str) -> Path:
    path = parent / name
    if not path.is_dir():
        _run(["git", "clone", url, str(path)])
    _run(["git", "fetch", "--all", "--tags"], cwd=path)
    _run(["git", "checkout", ref], cwd=path)
    return path


def _expected_metadata(venv_dir: Path) -> dict[str, object]:
    return {
        "c3_data_utilities_ref": C3_DATA_UTILITIES_REF,
        "go3_data_model_ref": GO3_DATA_MODEL_REF,
        "python_requirements": VALIDATOR_PYTHON_REQUIREMENTS,
        "venv_dir": str(venv_dir),
    }


def _load_metadata(path: Path) -> dict[str, object] | None:
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None


def ensure_validator_environment(cache_root: Path | None = None) -> ValidatorEnvironment:
    cache_root = (cache_root or default_cache_root()).expanduser().resolve()
    source_root = cache_root / "validator-src"
    venv_dir = cache_root / "validator-venv"
    metadata_path = cache_root / "validator-metadata.json"
    cache_root.mkdir(parents=True, exist_ok=True)
    source_root.mkdir(parents=True, exist_ok=True)
    python_executable = venv_dir / ("Scripts" if sys.platform.startswith("win") else "bin") / "python"
    c3_dir = source_root / "C3DataUtilities"
    model_dir = source_root / "GO-3-data-model"

    expected_metadata = _expected_metadata(venv_dir)
    cached_metadata = _load_metadata(metadata_path)
    if (
        cached_metadata == expected_metadata
        and python_executable.exists()
        and c3_dir.is_dir()
        and model_dir.is_dir()
    ):
        return ValidatorEnvironment(
            cache_root=cache_root,
            source_root=source_root,
            venv_dir=venv_dir,
            python_executable=python_executable,
            c3_data_utilities_dir=c3_dir,
            go3_data_model_dir=model_dir,
        )

    c3_dir = _ensure_checkout(source_root, "C3DataUtilities", C3_DATA_UTILITIES_URL, C3_DATA_UTILITIES_REF)
    model_dir = _ensure_checkout(source_root, "GO-3-data-model", GO3_DATA_MODEL_URL, GO3_DATA_MODEL_REF)

    if not venv_dir.exists():
        _run([sys.executable, "-m", "venv", str(venv_dir)])
    pip_executable = [str(python_executable), "-m", "pip"]

    _run(pip_executable + ["install", "--upgrade", "pip", "setuptools", "wheel"])
    _run(pip_executable + ["install"] + VALIDATOR_PYTHON_REQUIREMENTS)
    _run(pip_executable + ["install", "-e", str(model_dir), "--no-deps"])
    _run(pip_executable + ["install", "-e", str(c3_dir), "--no-deps"])

    metadata_path.write_text(
        json.dumps(expected_metadata, indent=2, sort_keys=True)
        + "\n",
        encoding="utf-8",
    )

    return ValidatorEnvironment(
        cache_root=cache_root,
        source_root=source_root,
        venv_dir=venv_dir,
        python_executable=python_executable,
        c3_data_utilities_dir=c3_dir,
        go3_data_model_dir=model_dir,
    )


def validator_summary(summary: dict[str, object] | None) -> dict[str, object]:
    if not isinstance(summary, dict):
        return {}
    evaluation = summary.get("evaluation")
    if not isinstance(evaluation, dict):
        nested_summary = summary.get("summary")
        if isinstance(nested_summary, dict):
            evaluation = nested_summary.get("evaluation")
    if not isinstance(evaluation, dict):
        evaluation = {}
    solution = summary.get("solution")
    if not isinstance(solution, dict):
        nested_summary = summary.get("summary")
        if isinstance(nested_summary, dict):
            solution = nested_summary.get("solution")
    if not isinstance(solution, dict):
        solution = {}
    problem = summary.get("problem")
    if not isinstance(problem, dict):
        nested_summary = summary.get("summary")
        if isinstance(nested_summary, dict):
            problem = nested_summary.get("problem")
    if not isinstance(problem, dict):
        problem = {}
    compact: dict[str, object] = {}
    for key in (
        "feas",
        "infeas",
        "obj",
        "z",
        "z_base",
        "z_value",
        "z_cost",
        "z_penalty",
        "z_k_worst_case",
        "z_k_average_case",
        "phys_feas",
        "infeas_diagnostics",
        "error_diagnostics",
    ):
        if key in evaluation:
            compact[key] = evaluation[key]
    if "obj" not in compact and "z" in compact:
        compact["obj"] = compact["z"]
    if "surplus_total" in problem:
        compact["problem_surplus_total"] = problem["surplus_total"]
    if "pass" in problem:
        compact["problem_pass"] = problem["pass"]
    if "pass" in solution:
        compact["pass"] = solution["pass"]
    return compact


def _rank_bus_residual_deltas(
    lhs_rows: list[dict[str, object]],
    rhs_rows: list[dict[str, object]],
    *,
    top_k: int,
) -> dict[str, object]:
    lhs_by_bus = {
        str(row.get("bus_uid")): row
        for row in lhs_rows
        if isinstance(row, dict) and row.get("bus_uid") is not None
    }
    rhs_by_bus = {
        str(row.get("bus_uid")): row
        for row in rhs_rows
        if isinstance(row, dict) and row.get("bus_uid") is not None
    }
    deltas: list[dict[str, object]] = []
    for bus_uid in sorted(set(lhs_by_bus) | set(rhs_by_bus)):
        lhs = lhs_by_bus.get(bus_uid, {})
        rhs = rhs_by_bus.get(bus_uid, {})
        lhs_sum = float(lhs.get("sum_abs_residual_pu_hours", 0.0) or 0.0)
        rhs_sum = float(rhs.get("sum_abs_residual_pu_hours", 0.0) or 0.0)
        delta = lhs_sum - rhs_sum
        deltas.append(
            {
                "bus_uid": bus_uid,
                "lhs_sum_abs_residual_pu_hours": lhs_sum,
                "rhs_sum_abs_residual_pu_hours": rhs_sum,
                "lhs_minus_rhs_sum_abs_residual_pu_hours": delta,
                "lhs_max_abs_residual_pu": float(lhs.get("max_abs_residual_pu", 0.0) or 0.0),
                "rhs_max_abs_residual_pu": float(rhs.get("max_abs_residual_pu", 0.0) or 0.0),
                "lhs_worst_period": lhs.get("worst_period"),
                "rhs_worst_period": rhs.get("worst_period"),
            }
        )
    deltas.sort(
        key=lambda item: (
            -abs(float(item["lhs_minus_rhs_sum_abs_residual_pu_hours"])),
            str(item["bus_uid"]),
        )
    )
    worse = [
        item
        for item in deltas
        if float(item["lhs_minus_rhs_sum_abs_residual_pu_hours"]) > 1e-12
    ][:top_k]
    better = [
        item
        for item in sorted(
            deltas,
            key=lambda item: (
                float(item["lhs_minus_rhs_sum_abs_residual_pu_hours"]),
                str(item["bus_uid"]),
            ),
        )
        if float(item["lhs_minus_rhs_sum_abs_residual_pu_hours"]) < -1e-12
    ][:top_k]
    return {
        "all_buses": deltas,
        "top_lhs_worse": worse,
        "top_lhs_better": better,
    }


def _compare_bus_residual_payloads(
    lhs: dict[str, object],
    rhs: dict[str, object],
    *,
    top_k: int = 10,
) -> dict[str, object]:
    lhs_metrics = lhs.get("metrics", {})
    if not isinstance(lhs_metrics, dict):
        lhs_metrics = {}
    rhs_metrics = rhs.get("metrics", {})
    if not isinstance(rhs_metrics, dict):
        rhs_metrics = {}
    lhs_bus_p = lhs.get("bus_p", [])
    if not isinstance(lhs_bus_p, list):
        lhs_bus_p = []
    rhs_bus_p = rhs.get("bus_p", [])
    if not isinstance(rhs_bus_p, list):
        rhs_bus_p = []
    lhs_bus_q = lhs.get("bus_q", [])
    if not isinstance(lhs_bus_q, list):
        lhs_bus_q = []
    rhs_bus_q = rhs.get("bus_q", [])
    if not isinstance(rhs_bus_q, list):
        rhs_bus_q = []
    return {
        "metrics": {
            "lhs_sum_bus_t_z_p": float(lhs_metrics.get("sum_bus_t_z_p", 0.0) or 0.0),
            "rhs_sum_bus_t_z_p": float(rhs_metrics.get("sum_bus_t_z_p", 0.0) or 0.0),
            "lhs_minus_rhs_sum_bus_t_z_p": float(lhs_metrics.get("sum_bus_t_z_p", 0.0) or 0.0)
            - float(rhs_metrics.get("sum_bus_t_z_p", 0.0) or 0.0),
            "lhs_sum_bus_t_z_q": float(lhs_metrics.get("sum_bus_t_z_q", 0.0) or 0.0),
            "rhs_sum_bus_t_z_q": float(rhs_metrics.get("sum_bus_t_z_q", 0.0) or 0.0),
            "lhs_minus_rhs_sum_bus_t_z_q": float(lhs_metrics.get("sum_bus_t_z_q", 0.0) or 0.0)
            - float(rhs_metrics.get("sum_bus_t_z_q", 0.0) or 0.0),
        },
        "bus_p": _rank_bus_residual_deltas(lhs_bus_p, rhs_bus_p, top_k=top_k),
        "bus_q": _rank_bus_residual_deltas(lhs_bus_q, rhs_bus_q, top_k=top_k),
    }


_VALIDATOR_STDOUT_METRIC = re.compile(r"^(?P<key>obj|feas):\s*(?P<value>[-+0-9.eE]+)\s*$", re.MULTILINE)


def _parse_stdout_metrics(stdout: str) -> dict[str, object]:
    metrics: dict[str, object] = {}
    for match in _VALIDATOR_STDOUT_METRIC.finditer(stdout):
        key = match.group("key")
        raw_value = match.group("value")
        try:
            value = float(raw_value)
        except ValueError:
            continue
        metrics[key] = int(value) if value.is_integer() else value
    return metrics


def validate_with_official_tool(
    env: ValidatorEnvironment,
    problem_path: Path,
    *,
    solution_path: Path | None = None,
    workdir: Path,
    parameters: dict[str, object] | None = None,
) -> dict[str, object]:
    problem_path = problem_path.expanduser().resolve()
    solution_path = None if solution_path is None else solution_path.expanduser().resolve()
    workdir = workdir.expanduser().resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    validator_outputs = (
        "summary.json",
        "summary.csv",
        "data_errors.txt",
        "ignored_errors.txt",
        "solution_errors.txt",
    )
    command = [
        str(env.python_executable),
        "check_data.py",
        "--problem",
        str(problem_path),
    ]
    if solution_path is not None:
        command.extend(["--solution", str(solution_path)])
    if parameters:
        command.extend(["--parameters", json.dumps(parameters, sort_keys=True)])

    with _ValidatorRunLock(env.cache_root / "validator-run.lock"):
        # The official checker writes fixed output filenames into its working
        # directory. Clear those artifacts before each run so we never mistake
        # a stale prior summary for the current problem/solution pair.
        for filename in validator_outputs:
            stale = env.c3_data_utilities_dir / filename
            if stale.exists():
                stale.unlink()

        proc = subprocess.run(
            command,
            cwd=env.c3_data_utilities_dir,
            capture_output=True,
            text=True,
            check=False,
        )

        copied_outputs: dict[str, str] = {}
        for filename in validator_outputs:
            source = env.c3_data_utilities_dir / filename
            if source.exists():
                destination = workdir / filename
                shutil.copy2(source, destination)
                copied_outputs[filename] = str(destination)

    summary = None
    summary_path = workdir / "summary.json"
    if summary_path.exists():
        with summary_path.open("r", encoding="utf-8") as handle:
            summary = json.load(handle)

    summary_metrics = validator_summary(summary)
    stdout_metrics = _parse_stdout_metrics(proc.stdout)
    for key, value in stdout_metrics.items():
        summary_metrics.setdefault(key, value)
    if "obj" not in summary_metrics and "z" in summary_metrics:
        summary_metrics["obj"] = summary_metrics["z"]

    return {
        "command": command,
        "returncode": proc.returncode,
        "stdout": proc.stdout,
        "stderr": proc.stderr,
        "copied_outputs": copied_outputs,
        "summary": summary,
        "summary_metrics": summary_metrics,
    }


def extract_bus_residuals_with_official_tool(
    env: ValidatorEnvironment,
    problem_path: Path,
    *,
    solution_path: Path,
    workdir: Path,
    label: str = "solution",
) -> dict[str, object]:
    problem_path = problem_path.expanduser().resolve()
    solution_path = solution_path.expanduser().resolve()
    workdir = workdir.expanduser().resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    output_path = workdir / f"{label}-bus-residuals.json"
    config_path = env.c3_data_utilities_dir / "config.json"
    command = [
        str(env.python_executable),
        "-c",
        _BUS_RESIDUAL_EXTRACT_SCRIPT,
        str(problem_path),
        str(solution_path),
        str(config_path),
        str(output_path),
    ]
    proc = subprocess.run(
        command,
        cwd=env.c3_data_utilities_dir,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            "official bus residual extraction failed"
            + (f": {proc.stderr.strip()}" if proc.stderr.strip() else "")
        )
    if not output_path.exists():
        raise RuntimeError(f"official bus residual extraction did not produce {output_path}")
    payload = json.loads(output_path.read_text(encoding="utf-8"))
    payload["command"] = command
    payload["stdout"] = proc.stdout
    payload["stderr"] = proc.stderr
    payload["report_path"] = str(output_path)
    return payload


def compare_bus_residuals_with_official_tool(
    env: ValidatorEnvironment,
    problem_path: Path,
    *,
    lhs_solution_path: Path,
    rhs_solution_path: Path,
    workdir: Path,
    top_k: int = 10,
) -> dict[str, object]:
    workdir = workdir.expanduser().resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    lhs = extract_bus_residuals_with_official_tool(
        env,
        problem_path,
        solution_path=lhs_solution_path,
        workdir=workdir,
        label="lhs",
    )
    rhs = extract_bus_residuals_with_official_tool(
        env,
        problem_path,
        solution_path=rhs_solution_path,
        workdir=workdir,
        label="rhs",
    )
    comparison = _compare_bus_residual_payloads(lhs, rhs, top_k=top_k)
    report = {
        "problem_path": str(problem_path.expanduser().resolve()),
        "lhs_solution_path": str(lhs_solution_path.expanduser().resolve()),
        "rhs_solution_path": str(rhs_solution_path.expanduser().resolve()),
        "top_k": int(top_k),
        "lhs": lhs,
        "rhs": rhs,
        "comparison": comparison,
        "report_path": str(workdir / "bus-residual-comparison.json"),
    }
    Path(report["report_path"]).write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return report
