# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Per-solve log capture for market solves.

A :class:`SolveLogger` context manager writes ``{workdir}/solve.log``:

1. Python :mod:`logging` records from the configured ``logger_name``
   hierarchy (default ``"surge.market"``).
2. Optional Rust tracing output and solver console output (Gurobi MIP
   progress, Ipopt NLP iterations), via fd-level tee of stdout and
   stderr. Opt in with ``capture_solver_log=True`` — produces much
   larger logs (~3000+ lines for GO C3 solves), useful for debugging
   convergence or infeasibility issues.

Example::

    from surge.market import SolveLogger

    with SolveLogger(workdir, policy=policy, label=label,
                     surge_module=surge, logger_name="markets.rto"):
        result = solve_dispatch(network, request)
"""

from __future__ import annotations

import json
import logging
import os
import re
import sys
import threading
import time
from dataclasses import asdict, is_dataclass
from datetime import datetime
from pathlib import Path


_SURGE_LOGGING_INITIALIZED = False
_ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
_RUST_TRACING_LINE_RE = re.compile(
    r"^(?P<timestamp>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z)\s+"
    r"(?P<level>TRACE|DEBUG|INFO|WARN|ERROR)\s+"
    r"(?P<message>.*)$"
)
_RUST_SPAN_PREFIX_RE = re.compile(r"^(?P<name>[a-z][\w:.-]*): (?P<message>.+)$")


def _ensure_surge_logging(surge_module, level: str = "info") -> None:
    global _SURGE_LOGGING_INITIALIZED
    if _SURGE_LOGGING_INITIALIZED:
        return
    if hasattr(surge_module, "init_logging"):
        surge_module.init_logging(level)
    _SURGE_LOGGING_INITIALIZED = True


def _format_local_log_timestamp(timestamp: str) -> str:
    try:
        dt = datetime.fromisoformat(timestamp.replace("Z", "+00:00")).astimezone()
    except ValueError:
        return timestamp
    return dt.strftime("%Y-%m-%d %H:%M:%S,%f")[:-3]


def _normalize_captured_log_line(line: str) -> str:
    clean = _ANSI_ESCAPE_RE.sub("", line)
    if not clean.strip():
        return clean
    rust_match = _RUST_TRACING_LINE_RE.match(clean.rstrip("\n"))
    if rust_match is None:
        return clean
    timestamp = _format_local_log_timestamp(rust_match.group("timestamp"))
    level = {"WARN": "WARNING"}.get(rust_match.group("level"), rust_match.group("level"))
    message = rust_match.group("message")
    logger_name = "surge.rust"
    span_match = _RUST_SPAN_PREFIX_RE.match(message)
    if span_match is not None:
        logger_name = f"surge.{span_match.group('name').replace('::', '.')}"
        message = span_match.group("message")
    return f"{timestamp} {level:<7} [{logger_name}] {message}\n"


class SolveLogger:
    """Context manager that writes a per-solve log file.

    Parameters
    ----------
    workdir
        Directory that will contain ``solve.log``. Created if missing.
    logger_name
        Python logger hierarchy to attach to. Defaults to
        ``"surge.market"``; markets typically pass their own name
        (e.g. ``"go_c3"``, ``"markets.rto"``).
    policy
        Optional policy dataclass (or any mapping-like object). Written
        to the log header as JSON for reproducibility.
    label
        Optional human-readable label for the run.
    problem_path
        Optional input problem path, written to the log header.
    surge_module
        The imported ``surge`` module. Required when
        ``capture_solver_log=True`` to initialize Rust tracing.
    log_level
        Python log level name. Default ``"info"``.
    capture_solver_log
        When True, also tee fd 1 and 2 so Rust tracing and solver
        console output land in ``solve.log``. Default False.
    """

    def __init__(
        self,
        workdir: Path,
        *,
        logger_name: str = "surge.market",
        policy=None,
        label: str | None = None,
        problem_path: Path | None = None,
        surge_module=None,
        log_level: str = "info",
        capture_solver_log: bool = False,
    ):
        self._workdir = workdir
        self._logger_name = logger_name
        self._policy = policy
        self._label = label
        self._problem_path = problem_path
        self._surge_module = surge_module
        self._log_level = log_level
        self._capture_solver_log = capture_solver_log
        self._log_path = workdir / "solve.log"
        self._log_file = None
        self._file_handler: logging.FileHandler | None = None
        self._stream_handler: logging.StreamHandler | None = None
        self._tee_fds: list[dict] = []

    def __enter__(self):
        self._workdir.mkdir(parents=True, exist_ok=True)
        self._log_file = open(self._log_path, "w", encoding="utf-8")

        if self._surge_module is not None and self._capture_solver_log:
            _ensure_surge_logging(self._surge_module, self._log_level)
        if self._capture_solver_log:
            self._start_fd_tee(1)
            self._start_fd_tee(2)

        py_level = getattr(logging, self._log_level.upper(), logging.INFO)
        root = logging.getLogger(self._logger_name)
        if self._capture_solver_log:
            self._stream_handler = logging.StreamHandler(sys.stderr)
            self._stream_handler.setLevel(py_level)
            self._stream_handler.setFormatter(
                logging.Formatter("%(asctime)s %(levelname)-5s [%(name)s] %(message)s")
            )
            root.addHandler(self._stream_handler)
        else:
            self._file_handler = logging.FileHandler(self._log_path, mode="a", encoding="utf-8")
            self._file_handler.setLevel(py_level)
            self._file_handler.setFormatter(
                logging.Formatter("%(asctime)s %(levelname)-5s [%(name)s] %(message)s")
            )
            root.addHandler(self._file_handler)
        root.setLevel(min(root.level or py_level, py_level))

        self._write_header()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        footer = (
            f"\n=== SOLVE FAILED: {exc_type.__name__}: {exc_val} ===\n"
            if exc_val is not None
            else "\n=== SOLVE COMPLETE ===\n"
        )
        if self._capture_solver_log:
            self._write_to_stderr(footer)
        else:
            self._write_to_file(footer)

        root = logging.getLogger(self._logger_name)
        if self._stream_handler is not None:
            root.removeHandler(self._stream_handler)
            self._stream_handler.close()
            self._stream_handler = None
        if self._file_handler is not None:
            root.removeHandler(self._file_handler)
            self._file_handler.close()
            self._file_handler = None

        self._stop_all_fd_tees()

        if self._log_file is not None:
            self._log_file.close()
            self._log_file = None
        return False

    def _write_to_stderr(self, text: str) -> None:
        try:
            sys.stderr.write(text)
            sys.stderr.flush()
        except (OSError, ValueError):
            pass

    def _write_to_file(self, text: str) -> None:
        if self._log_file is not None and not self._log_file.closed:
            self._log_file.seek(0, 2)
            self._log_file.write(text)
            self._log_file.flush()

    def _write_header(self) -> None:
        lines = ["=== SURGE SOLVE LOG ===\n"]
        lines.append(f"timestamp: {time.strftime('%Y-%m-%dT%H:%M:%S%z')}\n")
        if self._label:
            lines.append(f"label: {self._label}\n")
        if self._problem_path is not None:
            lines.append(f"problem: {self._problem_path}\n")
        if self._policy is not None:
            policy_dict = asdict(self._policy) if is_dataclass(self._policy) else self._policy
            try:
                lines.append(f"policy: {json.dumps(policy_dict, sort_keys=True, default=str)}\n")
            except (TypeError, ValueError):
                lines.append(f"policy: {policy_dict!r}\n")
        lines.append("\n")
        header = "".join(lines)
        if self._capture_solver_log:
            self._write_to_stderr(header)
        else:
            self._write_to_file(header)

    def _start_fd_tee(self, target_fd: int) -> None:
        try:
            saved_fd = os.dup(target_fd)
            pipe_r, pipe_w = os.pipe()
            os.dup2(pipe_w, target_fd)
            os.close(pipe_w)
            log_file = self._log_file

            def _tee():
                reader = os.fdopen(pipe_r, "r", encoding="utf-8", errors="replace")
                try:
                    for line in reader:
                        try:
                            os.write(saved_fd, line.encode("utf-8", errors="replace"))
                        except OSError:
                            pass
                        try:
                            if log_file and not log_file.closed:
                                log_file.write(_normalize_captured_log_line(line))
                                log_file.flush()
                        except (OSError, ValueError):
                            pass
                except (OSError, ValueError):
                    pass
                finally:
                    try:
                        reader.close()
                    except OSError:
                        pass

            thread = threading.Thread(target=_tee, daemon=True)
            thread.start()
            self._tee_fds.append({
                "target_fd": target_fd,
                "saved_fd": saved_fd,
                "thread": thread,
            })
        except OSError:
            pass

    def _stop_all_fd_tees(self) -> None:
        try:
            sys.stdout.flush()
        except (OSError, ValueError):
            pass
        try:
            sys.stderr.flush()
        except (OSError, ValueError):
            pass
        for entry in self._tee_fds:
            try:
                os.dup2(entry["saved_fd"], entry["target_fd"])
                os.close(entry["saved_fd"])
            except OSError:
                pass
        for entry in self._tee_fds:
            entry["thread"].join(timeout=5.0)
        self._tee_fds.clear()

    @property
    def log_path(self) -> Path:
        return self._log_path


__all__ = ["SolveLogger"]
