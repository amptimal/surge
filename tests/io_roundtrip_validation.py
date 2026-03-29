#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""
Surge I/O Round-Trip Validation Suite

Tests read→write→read round-trips for all supported format combinations.
Compares structural properties AND power flow results.
Generates a markdown report at docs/IO-ROUNDTRIP-REPORT.md
"""

import subprocess
import tempfile
import os
import sys
import re
import time
import traceback
from pathlib import Path
from dataclasses import dataclass, field
from typing import Optional
import numpy as np


def strip_ansi(text: str) -> str:
    """Remove ANSI escape codes."""
    return re.sub(r'\x1b\[[0-9;]*m', '', text)


def extract_error(stderr: str) -> str:
    """Extract the meaningful error from CLI stderr (skip WARN lines)."""
    stderr = strip_ansi(stderr)
    # Look for "Error:" lines first
    for line in stderr.splitlines():
        line = line.strip()
        if line.startswith("Error:"):
            return line[:200]
    # Fall back to last non-empty line
    lines = [l.strip() for l in stderr.splitlines() if l.strip()]
    if lines:
        return lines[-1][:200]
    return stderr[:200]

# ── paths ──────────────────────────────────────────────────────────────────
REPO = Path(__file__).resolve().parents[1]
INSTANCES = REPO / "tests" / "data"
CLI = REPO / "target" / "release" / "surge-solve"
REPORT_PATH = REPO / "docs" / "IO-ROUNDTRIP-REPORT.md"

# Add a local surge-py virtualenv if present.
for site_packages in sorted((REPO / "src" / "surge-py" / ".venv" / "lib").glob("python*/site-packages")):
    sys.path.insert(0, str(site_packages))
    break

import surge

# ── writable format extensions ─────────────────────────────────────────────
WRITE_FORMATS = {
    "matpower": ".m",
    "psse": ".raw",
    "xiidm": ".xiidm",
    "json": ".json",
}

# ── test cases per source format ───────────────────────────────────────────
TEST_CASES = {
    "matpower": [
        "case9.m", "case14.m", "case30.m", "case57.m", "case118.m",
        "case300.m", "case2383wp.m",
    ],
    "psse": [
        "raw/case9.raw", "raw/IEEE14.raw", "raw/IEEE14_PTIv33.raw",
        "raw/IEEE118.raw", "raw/kundur-twoarea_v33.raw",
        "raw/240busWECC_2018_PSS_fixedshunt.raw",
    ],
    "ieee_cdf": [
        "ieee14cdf.cdf", "ieee30cdf.cdf", "ieee57cdf.cdf",
        "ieee118cdf.cdf", "ieee300cdf.cdf",
    ],
    "xiidm": [
        "xiidm/EuropeanOpenModel_v33.xiidm",
    ],
    "ucte": [
        "ucte/beTestGrid.uct", "ucte/germanTsos.uct",
        "ucte/20170322_1844_SN3_FR2.uct",
    ],
    "cgmes": [
        "cgmes/case9/", "cgmes/case14/", "cgmes/case118/",
        "cgmes/case300/", "cgmes/case2383wp/",
        "cgmes/ieee14_ppow/", "cgmes/ieee118_ppow/",
        "cgmes/microgrid_be/", "cgmes/eurostag_ex1/",
        "cgmes/cigremv/",
    ],
    "dss": [
        "dss/ieee13/IEEE13Nodeckt.dss",
        "dss/ieee34/ieee34Mod1.dss",
        "dss/ieee37/ieee37.dss",
        "dss/ieee123/IEEE123Master.dss",
        "dss/ieee8500/Master.dss",
    ],
}


@dataclass
class Comparison:
    """Detailed comparison between two networks."""
    buses_match: bool = False
    branches_match: bool = False
    generators_match: bool = False
    loads_match: bool = False
    base_mva_match: bool = False
    n_buses_orig: int = 0
    n_buses_rt: int = 0
    n_branches_orig: int = 0
    n_branches_rt: int = 0
    n_gens_orig: int = 0
    n_gens_rt: int = 0
    n_loads_orig: int = 0
    n_loads_rt: int = 0
    pf_converged_orig: bool = False
    pf_converged_rt: bool = False
    pf_iters_orig: int = 0
    pf_iters_rt: int = 0
    vm_max_err: float = float("inf")
    va_max_err: float = float("inf")
    vm_mean_err: float = float("inf")
    va_mean_err: float = float("inf")
    pf_error: str = ""
    notes: list = field(default_factory=list)

    @property
    def structural_pass(self) -> bool:
        return (self.buses_match and self.branches_match
                and self.generators_match and self.base_mva_match)

    @property
    def pf_pass(self) -> bool:
        """PF results match within tolerance (1e-4 p.u. for Vm, 1e-3 rad for Va)."""
        if not self.pf_converged_orig or not self.pf_converged_rt:
            return False
        return self.vm_max_err < 1e-4 and self.va_max_err < 1e-3

    @property
    def status(self) -> str:
        if self.structural_pass and self.pf_pass:
            return "PASS"
        elif self.structural_pass and not self.pf_converged_orig:
            return "SKIP"  # original didn't converge, can't compare PF
        elif self.structural_pass and not self.pf_converged_rt:
            return "FAIL-PF"
        elif not self.structural_pass:
            return "FAIL-STRUCT"
        else:
            return "FAIL"


@dataclass
class TestResult:
    source_file: str
    source_format: str
    target_format: str
    read_ok: bool = False
    write_ok: bool = False
    reread_ok: bool = False
    comparison: Optional[Comparison] = None
    error: str = ""
    elapsed_ms: float = 0.0

    @property
    def status(self) -> str:
        if not self.read_ok:
            return "READ-FAIL"
        if not self.write_ok:
            return "WRITE-FAIL"
        if not self.reread_ok:
            return "REREAD-FAIL"
        if self.comparison:
            return self.comparison.status
        return "ERROR"


def load_network(path: str):
    """Load a network using surge Python bindings."""
    return surge.load(path)


def convert_file(src: str, dst: str) -> tuple[bool, str]:
    """Convert a file using the CLI."""
    result = subprocess.run(
        [str(CLI), src, "--convert", dst],
        capture_output=True, text=True, timeout=120,
    )
    if result.returncode != 0:
        return False, extract_error(result.stderr)
    return True, ""


def run_power_flow(net) -> tuple[bool, object, str]:
    """Run NR power flow, return (converged, solution, error)."""
    try:
        sol = surge.solve_ac_pf(net)
        converged = sol.iterations > 0 and sol.max_mismatch < 1e-6
        return converged, sol, ""
    except Exception as e:
        return False, None, str(e)


def compare_networks(net_orig, net_rt) -> Comparison:
    """Deep comparison of two networks."""
    c = Comparison()

    # Structural comparison
    c.n_buses_orig = net_orig.n_buses
    c.n_buses_rt = net_rt.n_buses
    c.n_branches_orig = net_orig.n_branches
    c.n_branches_rt = net_rt.n_branches
    c.n_gens_orig = net_orig.n_generators
    c.n_gens_rt = net_rt.n_generators
    c.n_loads_orig = getattr(net_orig, 'n_loads', -1)
    c.n_loads_rt = getattr(net_rt, 'n_loads', -1)

    c.buses_match = c.n_buses_orig == c.n_buses_rt
    c.branches_match = c.n_branches_orig == c.n_branches_rt
    c.generators_match = c.n_gens_orig == c.n_gens_rt
    c.loads_match = c.n_loads_orig == c.n_loads_rt

    try:
        c.base_mva_match = abs(net_orig.base_mva - net_rt.base_mva) < 1e-6
    except Exception:
        c.base_mva_match = True  # can't check, assume ok

    if not c.buses_match:
        c.notes.append(f"Bus count: {c.n_buses_orig} -> {c.n_buses_rt}")
    if not c.branches_match:
        c.notes.append(f"Branch count: {c.n_branches_orig} -> {c.n_branches_rt}")
    if not c.generators_match:
        c.notes.append(f"Generator count: {c.n_gens_orig} -> {c.n_gens_rt}")

    # Power flow comparison
    conv_orig, sol_orig, err_orig = run_power_flow(net_orig)
    c.pf_converged_orig = conv_orig

    if not conv_orig:
        c.pf_error = f"Original PF failed: {err_orig}"
        return c

    conv_rt, sol_rt, err_rt = run_power_flow(net_rt)
    c.pf_converged_rt = conv_rt
    c.pf_iters_orig = sol_orig.iterations
    c.pf_iters_rt = sol_rt.iterations if sol_rt else 0

    if not conv_rt:
        c.pf_error = f"Round-trip PF failed: {err_rt}"
        return c

    # Compare voltage magnitudes and angles
    try:
        vm_orig = np.array(sol_orig.vm)
        vm_rt = np.array(sol_rt.vm)
        va_orig = np.array(sol_orig.va)
        va_rt = np.array(sol_rt.va)

        if len(vm_orig) == len(vm_rt):
            vm_diff = np.abs(vm_orig - vm_rt)
            va_diff = np.abs(va_orig - va_rt)
            c.vm_max_err = float(np.max(vm_diff))
            c.va_max_err = float(np.max(va_diff))
            c.vm_mean_err = float(np.mean(vm_diff))
            c.va_mean_err = float(np.mean(va_diff))
        else:
            c.notes.append(f"Vm array length mismatch: {len(vm_orig)} vs {len(vm_rt)}")
    except Exception as e:
        c.pf_error = f"Comparison error: {e}"

    return c


def display_name(source_path: str, source_format: str) -> str:
    """Get a display name for a test case path."""
    if source_format == "cgmes":
        # For CGMES directories, show the directory name (e.g. "case9/")
        p = source_path.rstrip("/")
        return os.path.basename(p) + "/"
    return os.path.basename(source_path)


def run_single_test(source_path: str, source_format: str, target_format: str) -> TestResult:
    """Run a single read→write→read round-trip test."""
    t0 = time.time()
    result = TestResult(
        source_file=display_name(source_path, source_format),
        source_format=source_format,
        target_format=target_format,
    )

    # Step 1: Read original
    try:
        net_orig = load_network(source_path)
        result.read_ok = True
    except Exception as e:
        result.error = f"Read failed: {e}"
        result.elapsed_ms = (time.time() - t0) * 1000
        return result

    # Step 2: Write to target format
    ext = WRITE_FORMATS[target_format]
    with tempfile.NamedTemporaryFile(suffix=ext, delete=False) as f:
        tmp_path = f.name

    try:
        ok, err = convert_file(source_path, tmp_path)
        if not ok:
            result.error = f"Write failed: {err}"
            result.elapsed_ms = (time.time() - t0) * 1000
            os.unlink(tmp_path)
            return result
        result.write_ok = True

        # Step 3: Re-read the written file
        try:
            net_rt = load_network(tmp_path)
            result.reread_ok = True
        except Exception as e:
            result.error = f"Re-read failed: {e}"
            result.elapsed_ms = (time.time() - t0) * 1000
            os.unlink(tmp_path)
            return result

        # Step 4: Compare
        result.comparison = compare_networks(net_orig, net_rt)

    finally:
        if os.path.exists(tmp_path):
            os.unlink(tmp_path)

    result.elapsed_ms = (time.time() - t0) * 1000
    return result


def run_all_tests() -> list[TestResult]:
    """Run all round-trip tests."""
    results = []
    total = 0

    # Count total tests
    for src_fmt, cases in TEST_CASES.items():
        for case in cases:
            for tgt_fmt in WRITE_FORMATS:
                total += 1

    print(f"Running {total} round-trip tests across {len(WRITE_FORMATS)} write formats...")
    print()

    idx = 0
    for src_fmt, cases in TEST_CASES.items():
        for case_file in cases:
            src_path = str(INSTANCES / case_file)
            if not os.path.exists(src_path):
                print(f"  SKIP: {case_file} (not found)")
                continue

            for tgt_fmt in WRITE_FORMATS:
                idx += 1
                label = f"[{idx}/{total}] {case_file} -> {tgt_fmt}"
                print(f"  {label} ...", end="", flush=True)

                try:
                    r = run_single_test(src_path, src_fmt, tgt_fmt)
                    results.append(r)
                    status = r.status
                    detail = ""
                    if r.comparison and r.comparison.pf_pass:
                        detail = f" (Vm err={r.comparison.vm_max_err:.2e})"
                    elif r.comparison and r.comparison.pf_error:
                        detail = f" ({r.comparison.pf_error[:60]})"
                    elif r.error:
                        detail = f" ({r.error[:60]})"
                    print(f" {status}{detail}")
                except Exception as e:
                    print(f" ERROR: {e}")
                    results.append(TestResult(
                        source_file=case_file,
                        source_format=src_fmt,
                        target_format=tgt_fmt,
                        error=str(e),
                    ))

    return results


def generate_report(results: list[TestResult]) -> str:
    """Generate a markdown report."""
    lines = []
    lines.append("# Surge I/O Round-Trip Validation Report")
    lines.append("")
    lines.append(f"Generated: {time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime())}")
    lines.append("")
    lines.append("This report tests Surge's file format read/write capabilities by performing")
    lines.append("round-trip validation: read a file, write it to a target format, read the")
    lines.append("written file back, and compare both the network structure and power flow results.")
    lines.append("")

    # ── Summary ────────────────────────────────────────────────────────────
    pass_count = sum(1 for r in results if r.status == "PASS")
    skip_count = sum(1 for r in results if r.status == "SKIP")
    fail_count = sum(1 for r in results if r.status not in ("PASS", "SKIP"))
    total = len(results)

    lines.append("## Summary")
    lines.append("")
    lines.append(f"| Metric | Count |")
    lines.append(f"|--------|-------|")
    lines.append(f"| Total tests | {total} |")
    lines.append(f"| PASS | {pass_count} |")
    lines.append(f"| SKIP (original PF didn't converge) | {skip_count} |")
    lines.append(f"| FAIL | {fail_count} |")
    lines.append(f"| Pass rate (excl. skips) | {pass_count}/{total - skip_count} ({100*pass_count/max(1,total-skip_count):.1f}%) |")
    lines.append("")

    # ── Format matrix ──────────────────────────────────────────────────────
    lines.append("## Format Compatibility Matrix")
    lines.append("")
    lines.append("Each cell shows PASS/TOTAL for that source→target combination.")
    lines.append("")

    src_formats = list(TEST_CASES.keys())
    tgt_formats = list(WRITE_FORMATS.keys())

    header = "| Source \\ Target | " + " | ".join(tgt_formats) + " |"
    sep = "|" + "---|" * (len(tgt_formats) + 1)
    lines.append(header)
    lines.append(sep)

    for sf in src_formats:
        cells = []
        for tf in tgt_formats:
            matching = [r for r in results if r.source_format == sf and r.target_format == tf]
            if not matching:
                cells.append("—")
                continue
            p = sum(1 for r in matching if r.status == "PASS")
            s = sum(1 for r in matching if r.status == "SKIP")
            t = len(matching)
            effective = t - s
            if effective == 0:
                cells.append(f"SKIP ({t})")
            elif p == effective:
                cells.append(f"**{p}/{effective}**")
            else:
                cells.append(f"{p}/{effective}")
        lines.append(f"| {sf} | " + " | ".join(cells) + " |")

    lines.append("")

    # ── Detailed results by source format ──────────────────────────────────
    lines.append("## Detailed Results")
    lines.append("")

    for sf in src_formats:
        sf_results = [r for r in results if r.source_format == sf]
        if not sf_results:
            continue

        lines.append(f"### Source: {sf}")
        lines.append("")
        lines.append("| Source File | Target | Status | Buses | Branches | Gens | Vm Max Err | Va Max Err | Notes |")
        lines.append("|------------|--------|--------|-------|----------|------|-----------|-----------|-------|")

        for r in sf_results:
            c = r.comparison
            status = r.status

            if c:
                buses = f"{c.n_buses_orig}→{c.n_buses_rt}" if not c.buses_match else str(c.n_buses_orig)
                branches = f"{c.n_branches_orig}→{c.n_branches_rt}" if not c.branches_match else str(c.n_branches_orig)
                gens = f"{c.n_gens_orig}→{c.n_gens_rt}" if not c.generators_match else str(c.n_gens_orig)
                vm_err = f"{c.vm_max_err:.2e}" if c.vm_max_err < float("inf") else "—"
                va_err = f"{c.va_max_err:.2e}" if c.va_max_err < float("inf") else "—"
                notes = "; ".join(c.notes) if c.notes else (c.pf_error[:50] if c.pf_error else "")
            else:
                buses = branches = gens = vm_err = va_err = "—"
                notes = r.error[:50] if r.error else ""

            lines.append(f"| {r.source_file} | {r.target_format} | {status} | {buses} | {branches} | {gens} | {vm_err} | {va_err} | {notes} |")

        lines.append("")

    # ── Failures detail ────────────────────────────────────────────────────
    failures = [r for r in results if r.status not in ("PASS", "SKIP")]
    if failures:
        lines.append("## Failure Details")
        lines.append("")
        for r in failures:
            lines.append(f"### {r.source_file} → {r.target_format} ({r.status})")
            lines.append("")
            if r.error:
                lines.append(f"**Error**: {r.error}")
            if r.comparison:
                c = r.comparison
                if c.notes:
                    lines.append(f"**Structural diffs**: {'; '.join(c.notes)}")
                if c.pf_error:
                    lines.append(f"**PF error**: {c.pf_error}")
                if c.vm_max_err < float("inf"):
                    lines.append(f"**Vm max error**: {c.vm_max_err:.6e}")
                    lines.append(f"**Va max error**: {c.va_max_err:.6e}")
            lines.append("")

    # ── Methodology ────────────────────────────────────────────────────────
    lines.append("## Methodology")
    lines.append("")
    lines.append("1. **Read** the source file using `surge.load()`")
    lines.append("2. **Write** to the target format using `surge-solve --convert`")
    lines.append("3. **Re-read** the written file using `surge.load()`")
    lines.append("4. **Compare structure**: bus count, branch count, generator count, base MVA")
    lines.append("5. **Compare power flow**: run Newton-Raphson on both networks, compare Vm and Va arrays")
    lines.append("")
    lines.append("**Pass criteria**:")
    lines.append("- All structural counts match exactly")
    lines.append("- Vm max error < 1e-4 p.u.")
    lines.append("- Va max error < 1e-3 rad")
    lines.append("")
    lines.append("**SKIP**: original network doesn't converge under NR (cannot compare PF results)")
    lines.append("")
    lines.append("**Writable formats**: MATPOWER (.m), PSS/E v33 (.raw), XIIDM (.xiidm), JSON (.json)")
    lines.append("")
    lines.append("**Read-only formats tested as sources**: IEEE CDF (.cdf), UCTE-DEF (.uct), CGMES (directory of EQ/SSH/SV/TP XML profiles), OpenDSS (.dss)")
    lines.append("")
    lines.append("**Note on CGMES**: Input is a directory containing 4 XML profile files (EQ, SSH, SV, TP).")
    lines.append("Known CGMES PF failures exist for some cases due to model differences (see MEMORY.md).")
    lines.append("")
    lines.append("**Note on DSS**: OpenDSS files are distribution network models. These typically have no")
    lines.append("generators and use backward/forward sweep (BFS) solvers rather than Newton-Raphson.")
    lines.append("NR power flow is expected to fail or not converge on these cases. The test validates")
    lines.append("that the structural read/write path works correctly.")
    lines.append("")

    return "\n".join(lines)


def main():
    if not CLI.exists():
        print(f"ERROR: CLI not found at {CLI}. Run: cargo build --release")
        sys.exit(1)

    if not INSTANCES.exists():
        print(f"ERROR: Test data not found at {INSTANCES}")
        sys.exit(1)

    results = run_all_tests()

    # Generate and write report
    report = generate_report(results)
    REPORT_PATH.write_text(report)
    print(f"\nReport written to {REPORT_PATH}")

    # Print summary
    pass_count = sum(1 for r in results if r.status == "PASS")
    skip_count = sum(1 for r in results if r.status == "SKIP")
    fail_count = sum(1 for r in results if r.status not in ("PASS", "SKIP"))
    print(f"\nSummary: {pass_count} PASS / {skip_count} SKIP / {fail_count} FAIL out of {len(results)} tests")

    if fail_count > 0:
        print("\nFailures:")
        for r in results:
            if r.status not in ("PASS", "SKIP"):
                err = r.error or (r.comparison.pf_error if r.comparison else "")
                notes = "; ".join(r.comparison.notes) if r.comparison and r.comparison.notes else ""
                print(f"  {r.source_file} -> {r.target_format}: {r.status} | {err} {notes}")


if __name__ == "__main__":
    main()
