#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Benchmark native format IO and validate solver-equivalent results."""

from __future__ import annotations

import argparse
import json
import math
import statistics
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any


DEFAULT_CASES = [
    "case_ACTIVSg10k",
    "pglib_opf_case9241_pegase",
    "pglib_opf_case10192_epigrids",
    "pglib_opf_case30000_goc",
]

FORMATS = [
    ("matpower", ".m"),
    ("surge_json", ".surge.json"),
    ("surge_json_zst", ".surge.json.zst"),
    ("surge_bin", ".surge.bin"),
]

VALIDATION_METHODS = {
    "acpf": "nr",
    "dcpf": "dc",
    "dcopf": "dc-opf",
    "acopf": "ac-opf",
}

VALIDATION_METHOD_FALLBACKS = {
    "acpf": ["nr", "nr-warm", "fdpf"],
    "dcpf": ["dc"],
    "dcopf": ["dc-opf"],
    "acopf": ["ac-opf"],
}

KNOWN_UNSUPPORTED_VALIDATION_METHODS = {
    (
        "pglib_opf_case10192_epigrids",
        "acpf",
    ): (
        "MATPOWER baseline does not converge under the current CLI ACPF methods "
        "(nr, nr-warm, fdpf); the case is multi-island and the main island diverges "
        "before the CLI fallback chain exhausts."
    ),
}

VOLATILE_KEYS = {"solve_time_secs"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Measure same-format read/write cycles using surge-solve --convert. "
            "Optionally validate that solving the native formats matches the "
            "MATPOWER baseline across PF and OPF methods."
        )
    )
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path("target/release/surge-solve"),
        help="Path to the surge-solve binary to benchmark and validate.",
    )
    parser.add_argument(
        "--cases-dir",
        type=Path,
        default=Path("examples/cases"),
        help="Directory containing packaged example case bundles.",
    )
    parser.add_argument(
        "--case",
        action="append",
        dest="cases",
        help="Case bundle name to process. Repeat to select multiple cases.",
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=4,
        help="Timed runs per case/format after one warmup run.",
    )
    parser.add_argument(
        "--skip-benchmarks",
        action="store_true",
        help="Skip the read/write cycle benchmark phase.",
    )
    parser.add_argument(
        "--validate-solutions",
        action="store_true",
        help=(
            "Solve the MATPOWER file as the baseline, then validate that "
            ".surge.json, .surge.json.zst, and .surge.bin produce matching "
            "results for acpf, dcpf, dcopf, and acopf."
        ),
    )
    parser.add_argument(
        "--validation-only",
        action="store_true",
        help="Run the cross-format solve validation phase without benchmarking.",
    )
    parser.add_argument(
        "--method",
        action="append",
        choices=sorted(VALIDATION_METHODS),
        dest="validation_methods",
        help=(
            "Validation method to run. Repeat to select a subset. "
            "Defaults to acpf, dcpf, dcopf, acopf when validation is enabled."
        ),
    )
    parser.add_argument(
        "--abs-tol",
        type=float,
        default=1e-8,
        help="Absolute tolerance for numeric validation comparisons.",
    )
    parser.add_argument(
        "--rel-tol",
        type=float,
        default=1e-8,
        help="Relative tolerance for numeric validation comparisons.",
    )
    parser.add_argument(
        "--dcopf-solver",
        default="copt",
        choices=["default", "highs", "gurobi", "cplex", "copt"],
        help="Solver backend to use for dcopf validation runs.",
    )
    parser.add_argument(
        "--acopf-solver",
        default="ipopt",
        choices=["default", "gurobi", "copt", "ipopt"],
        help="Solver backend to use for acopf validation runs.",
    )
    return parser.parse_args()


def discover_case_files(case_dir: Path) -> dict[str, Path]:
    matpower_files = sorted(case_dir.glob("*.m"))
    if not matpower_files:
        raise SystemExit(f"missing MATPOWER baseline in {case_dir}")
    if len(matpower_files) != 1:
        raise SystemExit(
            f"expected exactly one MATPOWER file in {case_dir}, found {len(matpower_files)}"
        )

    stem = matpower_files[0].stem
    files = {
        "matpower": matpower_files[0],
        "surge_json": case_dir / f"{stem}.surge.json",
        "surge_json_zst": case_dir / f"{stem}.surge.json.zst",
        "surge_bin": case_dir / f"{stem}.surge.bin",
    }
    for format_name, path in files.items():
        if not path.exists():
            raise SystemExit(f"missing packaged {format_name} file: {path}")
    return files


def benchmark_one(binary: Path, input_path: Path, runs: int) -> list[float]:
    with tempfile.TemporaryDirectory(prefix="surge-native-bench-") as tmpdir:
        output_path = Path(tmpdir) / f"out{''.join(input_path.suffixes)}"
        warmup = subprocess.run(
            [str(binary), str(input_path), "--convert", str(output_path)],
            capture_output=True,
            text=True,
            check=False,
        )
        if warmup.returncode != 0:
            raise RuntimeError(
                f"warmup failed for {input_path}: {warmup.stderr.strip() or warmup.stdout.strip()}"
            )

        timings: list[float] = []
        for _ in range(runs):
            if output_path.exists():
                output_path.unlink()
            start = time.perf_counter()
            proc = subprocess.run(
                [str(binary), str(input_path), "--convert", str(output_path)],
                capture_output=True,
                text=True,
                check=False,
            )
            elapsed = time.perf_counter() - start
            if proc.returncode != 0:
                raise RuntimeError(
                    f"timed run failed for {input_path}: {proc.stderr.strip() or proc.stdout.strip()}"
                )
            timings.append(elapsed)
    return timings


def build_method_command(
    binary: Path,
    input_path: Path,
    cli_method: str,
    dcopf_solver: str,
    acopf_solver: str,
) -> list[str]:
    command = [
        str(binary),
        str(input_path),
        "--method",
        cli_method,
        "--output",
        "json",
        "--quiet",
    ]
    if cli_method == "dc-opf":
        command.extend(["--solver", dcopf_solver])
    elif cli_method == "ac-opf":
        command.extend(["--solver", acopf_solver])
    return command


def run_method_json(
    binary: Path,
    input_path: Path,
    cli_method: str,
    dcopf_solver: str,
    acopf_solver: str,
) -> Any:
    command = build_method_command(binary, input_path, cli_method, dcopf_solver, acopf_solver)

    proc = subprocess.run(command, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        raise RuntimeError(
            f"{cli_method} failed for {input_path}: "
            f"{proc.stderr.strip() or proc.stdout.strip()}"
        )
    try:
        return parse_json_output(proc.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            f"{cli_method} returned invalid JSON for {input_path}: {exc}"
        ) from exc


def run_validation_baseline(
    binary: Path,
    input_path: Path,
    validation_method: str,
    dcopf_solver: str,
    acopf_solver: str,
) -> tuple[str, Any]:
    errors: list[str] = []
    for cli_method in VALIDATION_METHOD_FALLBACKS[validation_method]:
        try:
            result = run_method_json(
                binary=binary,
                input_path=input_path,
                cli_method=cli_method,
                dcopf_solver=dcopf_solver,
                acopf_solver=acopf_solver,
            )
            return cli_method, result
        except RuntimeError as exc:
            errors.append(str(exc))
    joined = "\n".join(errors)
    raise RuntimeError(
        f"{validation_method} failed for {input_path} across fallback methods:\n{joined}"
    )


def parse_json_output(output: str) -> Any:
    start = output.find("{")
    if start == -1:
        raise json.JSONDecodeError("no JSON object found", output, 0)
    return json.loads(output[start:])


def compare_values(
    baseline: Any,
    candidate: Any,
    path: str,
    abs_tol: float,
    rel_tol: float,
) -> str | None:
    if isinstance(baseline, dict) and isinstance(candidate, dict):
        baseline_keys = {key for key in baseline if key not in VOLATILE_KEYS}
        candidate_keys = {key for key in candidate if key not in VOLATILE_KEYS}
        if baseline_keys != candidate_keys:
            return (
                f"{path}: key mismatch baseline={sorted(baseline_keys)} "
                f"candidate={sorted(candidate_keys)}"
            )
        for key in sorted(baseline_keys):
            mismatch = compare_values(
                baseline[key],
                candidate[key],
                f"{path}.{key}",
                abs_tol,
                rel_tol,
            )
            if mismatch:
                return mismatch
        return None

    if isinstance(baseline, list) and isinstance(candidate, list):
        if len(baseline) != len(candidate):
            return (
                f"{path}: list length mismatch baseline={len(baseline)} "
                f"candidate={len(candidate)}"
            )
        for index, (lhs, rhs) in enumerate(zip(baseline, candidate)):
            mismatch = compare_values(lhs, rhs, f"{path}[{index}]", abs_tol, rel_tol)
            if mismatch:
                return mismatch
        return None

    if isinstance(baseline, bool) or isinstance(candidate, bool):
        if baseline != candidate:
            return f"{path}: bool mismatch baseline={baseline!r} candidate={candidate!r}"
        return None

    if isinstance(baseline, (int, float)) and isinstance(candidate, (int, float)):
        lhs = float(baseline)
        rhs = float(candidate)
        if math.isnan(lhs) or math.isnan(rhs):
            if not (math.isnan(lhs) and math.isnan(rhs)):
                return f"{path}: NaN mismatch baseline={baseline!r} candidate={candidate!r}"
            return None
        if math.isinf(lhs) or math.isinf(rhs):
            if lhs != rhs:
                return f"{path}: infinity mismatch baseline={baseline!r} candidate={candidate!r}"
            return None
        if not math.isclose(lhs, rhs, rel_tol=rel_tol, abs_tol=abs_tol):
            return f"{path}: numeric mismatch baseline={lhs:.16g} candidate={rhs:.16g}"
        return None

    if baseline != candidate:
        return f"{path}: value mismatch baseline={baseline!r} candidate={candidate!r}"
    return None


def run_validation(
    binary: Path,
    case_name: str,
    files: dict[str, Path],
    methods: list[str],
    abs_tol: float,
    rel_tol: float,
    dcopf_solver: str,
    acopf_solver: str,
) -> list[tuple[str, str, str, str, str, str]]:
    baseline_results: dict[str, tuple[str, Any]] = {}
    rows: list[tuple[str, str, str, str, str, str]] = []
    for method in methods:
        try:
            baseline_results[method] = run_validation_baseline(
                binary=binary,
                input_path=files["matpower"],
                validation_method=method,
                dcopf_solver=dcopf_solver,
                acopf_solver=acopf_solver,
            )
        except RuntimeError:
            reason = KNOWN_UNSUPPORTED_VALIDATION_METHODS.get((case_name, method))
            if reason is None:
                raise
            for format_name, _extension in FORMATS[1:]:
                rows.append((case_name, method, "-", format_name, "unsupported", reason))

    for method in methods:
        if method not in baseline_results:
            continue
        baseline_cli_method, baseline_result = baseline_results[method]
        for format_name, _extension in FORMATS[1:]:
            candidate = run_method_json(
                binary,
                files[format_name],
                baseline_cli_method,
                dcopf_solver,
                acopf_solver,
            )
            mismatch = compare_values(
                baseline_result, candidate, "$", abs_tol, rel_tol
            )
            if mismatch:
                raise RuntimeError(
                    f"validation failed for case={case_name} method={method} "
                    f"format={format_name}: {mismatch}"
                )
            rows.append((case_name, method, baseline_cli_method, format_name, "pass", ""))
    return rows


def main() -> int:
    args = parse_args()
    if args.validation_only:
        args.skip_benchmarks = True
        args.validate_solutions = True

    binary = args.binary.resolve()
    if not binary.exists():
        raise SystemExit(f"missing surge-solve binary: {binary}")

    cases_dir = args.cases_dir.resolve()
    cases = args.cases or DEFAULT_CASES
    validation_methods = args.validation_methods or list(VALIDATION_METHODS)

    if not args.skip_benchmarks:
        print(
            "\t".join(
                [
                    "case",
                    "format",
                    "size_mb",
                    "median_s",
                    "mean_s",
                    "min_s",
                    "max_s",
                    "runs_s",
                ]
            )
        )
        for case_name in cases:
            files = discover_case_files(cases_dir / case_name)
            for format_name, _extension in FORMATS:
                input_path = files[format_name]
                timings = benchmark_one(binary, input_path, args.runs)
                size_mb = input_path.stat().st_size / (1024 * 1024)
                print(
                    "\t".join(
                        [
                            case_name,
                            format_name,
                            f"{size_mb:.2f}",
                            f"{statistics.median(timings):.4f}",
                            f"{statistics.mean(timings):.4f}",
                            f"{min(timings):.4f}",
                            f"{max(timings):.4f}",
                            ",".join(f"{value:.4f}" for value in timings),
                        ]
                    )
                )

    if args.validate_solutions:
        if not args.skip_benchmarks:
            print()
        print("validation_case\tmethod\tcli_method\tformat\tstatus\tdetail")
        for case_name in cases:
            files = discover_case_files(cases_dir / case_name)
            rows = run_validation(
                binary=binary,
                case_name=case_name,
                files=files,
                methods=validation_methods,
                abs_tol=args.abs_tol,
                rel_tol=args.rel_tol,
                dcopf_solver=args.dcopf_solver,
                acopf_solver=args.acopf_solver,
            )
            for row in rows:
                print("\t".join(row))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
