# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Canonical dispatch facade."""

from __future__ import annotations

from collections.abc import Mapping
import json
import os
from pathlib import Path
import shutil
import subprocess
from typing import Any

from . import _native

ActivsgTimeSeries = _native.ActivsgTimeSeries
DispatchResult = _native.DispatchResult


def dumps_request(
    request: Mapping[str, Any] | str | bytes | None,
    *,
    indent: int = 2,
    sort_keys: bool = True,
) -> str:
    """Serialize a dispatch request to canonical JSON text."""

    if request is None:
        payload: Any = {}
    elif isinstance(request, bytes):
        payload = json.loads(request.decode("utf-8"))
    elif isinstance(request, str):
        payload = json.loads(request)
    else:
        payload = dict(request)
    return json.dumps(payload, indent=indent, sort_keys=sort_keys) + "\n"


def loads_request(payload: str | bytes) -> dict[str, Any]:
    """Parse a canonical dispatch request from JSON text or bytes."""

    text = payload.decode("utf-8") if isinstance(payload, bytes) else payload
    value = json.loads(text)
    if not isinstance(value, dict):
        raise TypeError("dispatch request payload must deserialize to a JSON object")
    return value


def _zstd_binary() -> str:
    binary = shutil.which("zstd")
    if binary is None:
        raise RuntimeError("zstd binary not found on PATH")
    return binary


def _is_zstd_path(path: Path) -> bool:
    return path.suffix == ".zst"


def save_request(
    request: Mapping[str, Any] | str | bytes | None,
    path,
    *,
    indent: int = 2,
    sort_keys: bool = True,
) -> None:
    """Save a canonical dispatch request as `.json` or `.json.zst`."""

    resolved = Path(path)
    resolved.parent.mkdir(parents=True, exist_ok=True)
    payload = dumps_request(request, indent=indent, sort_keys=sort_keys).encode("utf-8")
    if _is_zstd_path(resolved):
        subprocess.run(
            [_zstd_binary(), "--quiet", "--force", "-19", "-o", os.fspath(resolved), "-"],
            input=payload,
            check=True,
        )
        return
    resolved.write_bytes(payload)


def load_request(path) -> dict[str, Any]:
    """Load a canonical dispatch request from `.json` or `.json.zst`."""

    resolved = Path(path)
    if _is_zstd_path(resolved):
        proc = subprocess.run(
            [_zstd_binary(), "--quiet", "--decompress", "--stdout", os.fspath(resolved)],
            capture_output=True,
            check=True,
        )
        return loads_request(proc.stdout)
    return loads_request(resolved.read_text(encoding="utf-8"))


def read_tamu_activsg_time_series(network, root, case: str = "2000") -> ActivsgTimeSeries:
    """Read the public TAMU ACTIVSg time-series package for a Surge network.

    Args:
        network: Refreshed ACTIVSg network model.
        root: Directory containing the TAMU CSV bundle.
        case: ``"2000"`` or ``"10k"``.

    Returns:
        ActivsgTimeSeries helper with report metadata, truncated profile helpers,
        and a nameplate-override method for the passed network.
    """

    return _native.read_tamu_activsg_time_series(network, os.fspath(root), case)


def solve_dispatch(
    network,
    request: Mapping[str, Any] | str | None = None,
    *,
    lp_solver: str | None = None,
    nlp_solver: str | None = None,
) -> DispatchResult:
    """Solve a canonical dispatch study.

    Args:
        network: Power system network.
        request: Canonical dispatch request as a nested dict/list structure or
            a JSON string. ``None`` uses the default one-period DC
            all-committed dispatch request. Unknown keys are rejected instead
            of being silently ignored.
        lp_solver: Optional LP backend name for DC dispatch formulations.
            Accepted values match the Rust backend selector, such as
            ``"default"``, ``"highs"``, ``"gurobi"``, ``"copt"``, and
            ``"cplex"``.
        nlp_solver: Optional NLP backend name for AC dispatch formulations.
            Accepted values match the AC-OPF backend selector, such as
            ``"default"``, ``"ipopt"``, ``"copt"``, and ``"gurobi"``.

    Returns:
        DispatchResult with study metadata, per-period outcomes, and keyed
        resource/bus detail.
    """

    resolved_request = {} if request is None else request
    if lp_solver is not None and not isinstance(lp_solver, str):
        raise TypeError("solve_dispatch() expected str | None for lp_solver")
    if nlp_solver is not None and not isinstance(nlp_solver, str):
        raise TypeError("solve_dispatch() expected str | None for nlp_solver")
    return _native.solve_dispatch(
        network,
        resolved_request,
        lp_solver=lp_solver,
        nlp_solver=nlp_solver,
    )


def assess_dispatch_violations(
    network,
    result: DispatchResult,
    *,
    p_bus_vio_cost: float = 1_000_000.0,
    q_bus_vio_cost: float = 1_000_000.0,
    s_vio_cost: float = 500.0,
    interval_hours: list[float] | None = None,
) -> dict:
    """Assess AC pi-model violations for a solved dispatch result.

    Computes bus P/Q balance mismatches and branch thermal overloads
    using the exact pi-model power flow equations.  Returns a dict with
    per-period and summary violation data including penalty costs.

    Args:
        network: Power system network used for the dispatch.
        result: Solved dispatch result.
        p_bus_vio_cost: Active power bus balance violation cost ($/pu/hr).
        q_bus_vio_cost: Reactive power bus balance violation cost ($/pu/hr).
        s_vio_cost: Branch thermal violation cost ($/pu/hr).
        interval_hours: Per-period interval durations (hours).
            Defaults to 1.0 for each period.

    Returns:
        Dict with ``bus_p_total_mismatch_mw``, ``bus_q_total_mismatch_mvar``,
        ``thermal_total_overload_mva``, ``total_penalty``, and per-period
        ``periods`` list.
    """
    return _native.assess_dispatch_violations(
        network,
        result,
        p_bus_vio_cost=p_bus_vio_cost,
        q_bus_vio_cost=q_bus_vio_cost,
        s_vio_cost=s_vio_cost,
        interval_hours=interval_hours,
    )


__all__ = [
    "ActivsgTimeSeries",
    "DispatchResult",
    "assess_dispatch_violations",
    "dumps_request",
    "load_request",
    "loads_request",
    "read_tamu_activsg_time_series",
    "save_request",
    "solve_dispatch",
]
