# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Background job queue for solve / validate actions.

In-memory and single-process by design — the server is a local dev tool. Each
job captures its own log buffer so SSE listeners can tail it. If the server
restarts mid-job the artifacts remain on disk; only the in-memory job handle
is lost, which is fine because solve outputs are self-persisted.
"""
from __future__ import annotations

import asyncio
import dataclasses
import io
import logging
import threading
import time
import traceback
import uuid
from concurrent.futures import Future, ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

DEFAULT_MAX_CONCURRENCY = 4


@dataclass
class Job:
    id: str
    kind: str  # "solve" | "validate"
    case_key: str
    status: str = "queued"  # queued | running | succeeded | failed
    created_at: float = field(default_factory=time.time)
    started_at: float | None = None
    finished_at: float | None = None
    error: str | None = None
    result: dict[str, Any] | None = None
    policy: dict[str, Any] | None = None
    _log: list[str] = field(default_factory=list)
    _future: Future | None = None
    _listeners: set[asyncio.Queue[str]] = field(default_factory=set)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "kind": self.kind,
            "case_key": self.case_key,
            "status": self.status,
            "created_at": self.created_at,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "error": self.error,
            "policy": self.policy,
        }


class _JobLogHandler(logging.Handler):
    """Fan out Python log records into a Job's buffer and all SSE listeners."""

    def __init__(self, job: Job, bus: "JobBus"):
        super().__init__(level=logging.INFO)
        self._job = job
        self._bus = bus
        self.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))

    def emit(self, record: logging.LogRecord) -> None:
        try:
            line = self.format(record)
        except Exception:  # noqa: BLE001
            line = record.getMessage()
        self._bus.append_log(self._job, line)


class JobBus:
    """Manages jobs and broadcasts log lines to SSE listeners."""

    def __init__(self, max_workers: int = DEFAULT_MAX_CONCURRENCY):
        self._executor = ThreadPoolExecutor(max_workers=max_workers, thread_name_prefix="go_c3_job")
        self._jobs: dict[str, Job] = {}
        self._scenario_locks: dict[str, threading.Lock] = {}
        self._lock = threading.Lock()
        self._loop: asyncio.AbstractEventLoop | None = None

    def bind_loop(self, loop: asyncio.AbstractEventLoop) -> None:
        """Attach the asyncio loop used for pushing to SSE queues."""
        self._loop = loop

    def shutdown(self) -> None:
        self._executor.shutdown(wait=False, cancel_futures=True)

    # ─── job lifecycle ────────────────────────────────────────────────────

    def _scenario_lock(self, case_key: str) -> threading.Lock:
        with self._lock:
            lock = self._scenario_locks.get(case_key)
            if lock is None:
                lock = threading.Lock()
                self._scenario_locks[case_key] = lock
            return lock

    def submit(
        self,
        *,
        kind: str,
        case_key: str,
        target: Callable[[Job], dict[str, Any]],
        policy: dict[str, Any] | None = None,
    ) -> Job:
        job = Job(id=str(uuid.uuid4()), kind=kind, case_key=case_key, policy=policy)
        with self._lock:
            self._jobs[job.id] = job

        def runner() -> None:
            scenario_lock = self._scenario_lock(case_key)
            acquired = scenario_lock.acquire(blocking=False)
            if not acquired:
                self.append_log(job, "[queue] another job is already running for this scenario — waiting")
                scenario_lock.acquire()
            try:
                job.status = "running"
                job.started_at = time.time()
                self._broadcast_status(job)

                # Route surge + runner logs into this job's buffer.
                handler = _JobLogHandler(job, self)
                targets = [logging.getLogger("go_c3.runner"), logging.getLogger("go_c3.server")]
                for t in targets:
                    t.addHandler(handler)
                try:
                    job.result = target(job)
                    job.status = "succeeded"
                except Exception as exc:  # noqa: BLE001
                    job.status = "failed"
                    job.error = str(exc)
                    self.append_log(job, "[error] " + str(exc))
                    self.append_log(job, traceback.format_exc())
                finally:
                    for t in targets:
                        t.removeHandler(handler)
                    job.finished_at = time.time()
                    self._broadcast_status(job)
            finally:
                scenario_lock.release()

        job._future = self._executor.submit(runner)
        return job

    def get(self, job_id: str) -> Job | None:
        return self._jobs.get(job_id)

    def list_recent(self, limit: int = 50) -> list[Job]:
        with self._lock:
            items = sorted(self._jobs.values(), key=lambda j: j.created_at, reverse=True)
            return items[:limit]

    # ─── log + sse plumbing ───────────────────────────────────────────────

    def append_log(self, job: Job, line: str) -> None:
        job._log.append(line)
        if len(job._log) > 5000:
            del job._log[: len(job._log) - 5000]
        self._push_to_listeners(job, "log", line)

    def _broadcast_status(self, job: Job) -> None:
        self._push_to_listeners(job, "status", job.status)

    def _push_to_listeners(self, job: Job, event: str, data: str) -> None:
        if not job._listeners or self._loop is None:
            return
        payload = f"event: {event}\ndata: {data}\n\n"
        for q in list(job._listeners):
            try:
                self._loop.call_soon_threadsafe(q.put_nowait, payload)
            except Exception:  # noqa: BLE001
                pass

    async def subscribe(self, job: Job, *, replay_log: bool = True) -> asyncio.Queue[str]:
        q: asyncio.Queue[str] = asyncio.Queue()
        job._listeners.add(q)
        if replay_log:
            for line in list(job._log):
                await q.put(f"event: log\ndata: {line}\n\n")
            await q.put(f"event: status\ndata: {job.status}\n\n")
        return q

    def unsubscribe(self, job: Job, q: asyncio.Queue[str]) -> None:
        job._listeners.discard(q)

    def log_text(self, job: Job) -> str:
        return "\n".join(job._log)


# ─── targets: solve + validate ───────────────────────────────────────────


def make_solve_target(
    registry: "Any",
    case_key: str,
    policy_overrides: dict[str, Any] | None,
    bus: "JobBus",
) -> Callable[[Job], dict[str, Any]]:
    """Build the callable that executes a solve for ``case_key``."""
    from benchmarks.go_c3.runner import solve_baseline_scenario
    from markets.go_c3 import GoC3Policy

    def target(job: Job) -> dict[str, Any]:
        record, sw, _ = registry._scenario_for(case_key)
        fields = {
            "allow_branch_switching": (sw == "sw1"),
            "capture_solver_log": True,
        }
        if policy_overrides:
            overrides = dict(policy_overrides)
            if "ac_nlp_solver" in overrides and "nlp_solver" not in overrides:
                overrides["nlp_solver"] = overrides.pop("ac_nlp_solver")
            if "fixed_consumer_mode" in overrides and "consumer_mode" not in overrides:
                overrides["consumer_mode"] = overrides.pop("fixed_consumer_mode")
            fields.update(overrides)
        policy = GoC3Policy(**fields)
        job.policy = {k: v for k, v in dataclasses.asdict(policy).items() if not k.startswith("_")}
        bus.append_log(job, f"[solve] starting {case_key}")
        bus.append_log(job, f"[solve] policy: lp_solver={policy.lp_solver} mode={policy.commitment_mode} reconcile={policy.ac_reconcile_mode}")
        report = solve_baseline_scenario(record, cache_root=registry._cache_root, policy=policy)
        status = report.get("status")
        bus.append_log(job, f"[solve] complete · status={status}")
        if status == "ok":
            bus.append_log(job, f"[solve] dc_cost={report.get('dc_dispatch_summary', {}).get('total_cost')}")
        elif err := report.get("error"):
            bus.append_log(job, f"[solve] error: {str(err)[:400]}")
        registry.invalidate(case_key)
        return report

    return target


def make_validate_target(
    registry: "Any",
    case_key: str,
    bus: "JobBus",
) -> Callable[[Job], dict[str, Any]]:
    from benchmarks.go_c3.runner import validate_baseline_solution
    from benchmarks.go_c3.validator import ensure_validator_environment
    from markets.go_c3 import GoC3Policy

    def target(job: Job) -> dict[str, Any]:
        record, sw, _ = registry._scenario_for(case_key)
        policy = GoC3Policy(allow_branch_switching=(sw == "sw1"))
        bus.append_log(job, f"[validate] ensuring validator environment")
        env = ensure_validator_environment(cache_root=registry._cache_root)
        bus.append_log(job, f"[validate] running official validator for {case_key}")
        report = validate_baseline_solution(
            record,
            cache_root=registry._cache_root,
            validator_env=env,
            policy=policy,
        )
        summary = report.get("validation_summary") or {}
        if summary:
            bus.append_log(job, "[validate] summary:")
            for k, v in summary.items():
                bus.append_log(job, f"  {k}: {v}")
        else:
            bus.append_log(job, "[validate] no summary_metrics returned")
        registry.invalidate(case_key)
        return report

    return target
