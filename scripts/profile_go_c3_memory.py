#!/usr/bin/env python3
"""Phase-by-phase RSS profiler for a single GO C3 solve.

Measures peak + current RSS before/after each stage of
solve_baseline_scenario so we can see which phase balloons memory.

Usage:
    uv run python scripts/profile_go_c3_memory.py \
        --dataset event4_617 --division D1 --scenario 2

Samples the resident set at:
    - process start
    - after `import surge`
    - after go_c3.load()
    - after go_c3.build_workflow() (no solve yet)
    - after go_c3.solve_workflow() stage 1 (DC SCUC)
    - after go_c3.solve_workflow() full result
    - after export

A background sampler thread also records peak RSS every 200 ms
so transient allocations between checkpoints aren't missed.
"""

from __future__ import annotations

import argparse
import gc
import os
import resource
import threading
import time
from pathlib import Path


def rss_mb() -> float:
    """Current RSS in MB (macOS: ru_maxrss is bytes; Linux: kB)."""
    usage = resource.getrusage(resource.RUSAGE_SELF)
    # macOS returns bytes, Linux returns KB. Guess based on platform.
    import sys
    if sys.platform == "darwin":
        return usage.ru_maxrss / (1024 * 1024)
    return usage.ru_maxrss / 1024


def current_rss_mb() -> float:
    """Instantaneous RSS via ps (more accurate than ru_maxrss for current)."""
    import subprocess
    out = subprocess.check_output(
        ["ps", "-o", "rss=", "-p", str(os.getpid())], text=True
    ).strip()
    return int(out) / 1024  # KB -> MB


class PeakSampler:
    """Background thread that samples current RSS every `interval_s`."""

    def __init__(self, interval_s: float = 0.2):
        self.interval_s = interval_s
        self.peak_mb = 0.0
        self.samples: list[tuple[float, float]] = []
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._start_time = 0.0

    def start(self):
        self._start_time = time.perf_counter()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def _run(self):
        while not self._stop.is_set():
            try:
                rss = current_rss_mb()
                self.peak_mb = max(self.peak_mb, rss)
                self.samples.append((time.perf_counter() - self._start_time, rss))
            except Exception:
                pass
            self._stop.wait(self.interval_s)

    def stop(self):
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=2.0)

    def mark(self, label: str) -> tuple[float, float]:
        t = time.perf_counter() - self._start_time
        rss = current_rss_mb()
        self.peak_mb = max(self.peak_mb, rss)
        return t, rss


def mark(sampler: PeakSampler, label: str, last_rss: float) -> float:
    gc.collect()
    t, rss = sampler.mark(label)
    delta = rss - last_rss
    sign = "+" if delta >= 0 else ""
    print(
        f"[{t:7.2f}s] {label:<40s} RSS={rss:10,.1f} MB  "
        f"Δ={sign}{delta:10,.1f} MB  peak={sampler.peak_mb:10,.1f} MB",
        flush=True,
    )
    return rss


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", default="event4_617")
    ap.add_argument("--division", default="D1")
    ap.add_argument("--scenario", type=int, default=2)
    ap.add_argument("--ac-reconcile", default="none",
                    help="'none' (SCUC only) or 'ac_dispatch' (full two-stage)")
    ap.add_argument("--skip-solve", action="store_true",
                    help="Build workflow only; do not call solve_workflow")
    ap.add_argument("--skip-export", action="store_true")
    args = ap.parse_args()

    sampler = PeakSampler(interval_s=0.1)
    sampler.start()

    # Drop pid to a well-known path so an out-of-process sampler can find us.
    pidfile = Path("/tmp/profile_go_c3_memory.pid")
    pidfile.write_text(str(os.getpid()))
    print(f"PID={os.getpid()}  dataset={args.dataset}  division={args.division}  "
          f"scenario={args.scenario}  ac_reconcile={args.ac_reconcile}",
          flush=True)
    print("=" * 110, flush=True)

    last = mark(sampler, "start", 0.0)

    # Locate scenario
    import sys
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from benchmarks.go_c3.cli import _locate_scenario, default_cache_root
    cache_root = default_cache_root()
    _, scenario = _locate_scenario(
        args.dataset, args.division, args.scenario, cache_root=cache_root
    )
    last = mark(sampler, "after _locate_scenario", last)

    print(f"problem_path={scenario.problem_path}  "
          f"file_size={scenario.problem_path.stat().st_size / 1024 / 1024:.1f} MB",
          flush=True)

    # Import surge
    import surge  # noqa: F401
    last = mark(sampler, "after import surge", last)

    import surge.market.go_c3 as go_c3
    last = mark(sampler, "after import surge.market.go_c3", last)

    # Policy
    from markets.go_c3 import GoC3Policy
    policy = GoC3Policy(
        lp_solver="gurobi",
        commitment_mode="optimize",
        commitment_mip_rel_gap=0.001,
        commitment_time_limit_secs=600.0,
        formulation="dc",
        ac_reconcile_mode=args.ac_reconcile,
        nlp_solver="ipopt",
    )
    native_policy = go_c3.MarketPolicy(
        formulation=policy.formulation,
        ac_reconcile_mode=policy.ac_reconcile_mode,
        consumer_mode=policy.consumer_mode,
        commitment_mode=policy.commitment_mode,
        allow_branch_switching=policy.allow_branch_switching,
        lp_solver=policy.lp_solver,
        nlp_solver=policy.nlp_solver or "ipopt",
        commitment_mip_rel_gap=policy.commitment_mip_rel_gap,
        commitment_time_limit_secs=policy.commitment_time_limit_secs,
    )
    last = mark(sampler, "after policy construction", last)

    # Load
    t0 = time.perf_counter()
    ph = go_c3.load(scenario.problem_path)
    print(f"          go_c3.load took {time.perf_counter() - t0:.2f}s", flush=True)
    last = mark(sampler, "after go_c3.load", last)

    # Build workflow
    t0 = time.perf_counter()
    wf = go_c3.build_workflow(ph, native_policy)
    print(f"          go_c3.build_workflow took {time.perf_counter() - t0:.2f}s",
          flush=True)
    last = mark(sampler, "after go_c3.build_workflow", last)
    print(f"          workflow stages: {wf.stages()}", flush=True)

    if args.skip_solve:
        print("skip-solve set; stopping.", flush=True)
    else:
        t0 = time.perf_counter()
        wr = go_c3.solve_workflow(
            wf,
            lp_solver=native_policy.lp_solver,
            nlp_solver=native_policy.nlp_solver,
        )
        print(f"          go_c3.solve_workflow took {time.perf_counter() - t0:.2f}s",
              flush=True)
        last = mark(sampler, "after go_c3.solve_workflow", last)

        if not args.skip_export:
            stage_idx = -1 if policy.ac_reconcile_mode == "ac_dispatch" else 0
            final_solution = wr["stages"][stage_idx]["solution"]
            dc_res = None
            if policy.ac_reconcile_mode == "ac_dispatch" and len(wr["stages"]) > 1:
                dc_res = wr["stages"][0]["solution"]
            t0 = time.perf_counter()
            exp = go_c3.export(ph, final_solution, dc_reserve_source=dc_res)
            print(f"          go_c3.export took {time.perf_counter() - t0:.2f}s",
                  flush=True)
            last = mark(sampler, "after go_c3.export", last)
            del exp

        del wr

    del wf
    del ph

    last = mark(sampler, "after freeing everything (gc)", last)

    sampler.stop()
    print("=" * 110, flush=True)
    print(f"PEAK RSS over full run: {sampler.peak_mb:,.1f} MB "
          f"({sampler.peak_mb / 1024:.2f} GB)", flush=True)

    # Dump time-series to file for charting
    out = Path("/tmp") / f"rss_profile_{args.dataset}_{args.division}_{args.scenario}.csv"
    with out.open("w") as f:
        f.write("t_s,rss_mb\n")
        for t, rss in sampler.samples:
            f.write(f"{t:.3f},{rss:.1f}\n")
    print(f"Sampler trace written to {out}", flush=True)


if __name__ == "__main__":
    main()
