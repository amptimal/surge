# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Batch solve and scenario automation helpers."""

from __future__ import annotations

import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

from . import _native

if TYPE_CHECKING:
    from ._surge import AcPfResult, DcPfResult, Network, OpfResult


SweepResult = _native.SweepResult
SweepResults = _native.SweepResults
parameter_sweep = _native.parameter_sweep


@dataclass
class ScenarioResult:
    """Result from solving a single case."""

    case_name: str
    network: Network | None = None
    solution: "AcPfResult | DcPfResult | OpfResult | None" = None
    error: str | None = None
    wall_time_s: float = 0.0


@dataclass
class BatchResults:
    """Collected results from a batch solve run."""

    results: list[ScenarioResult] = field(default_factory=list)

    def to_dataframe(self):
        """Summary DataFrame: case, status, gen_mw, load_mw, loss_mw, vm_range, time_ms."""
        import pandas as pd

        rows: list[dict[str, Any]] = []
        for r in self.results:
            row: dict[str, Any] = {
                "case": r.case_name,
                "status": "ok" if r.error is None else "error",
                "error": r.error,
                "time_ms": r.wall_time_s * 1000,
            }
            sol = r.solution
            net = r.network
            if sol is not None and net is not None:
                row["total_load_mw"] = net.total_load_mw
                row["total_gen_mw"] = net.total_generation_mw
                # DC solutions have no vm/va_rad/converged/iterations
                vm = getattr(sol, "vm", None)
                if vm is not None:
                    row["converged"] = sol.converged
                    row["iterations"] = sol.iterations
                    row["min_vm_pu"] = float(vm.min()) if len(vm) > 0 else None
                    row["max_vm_pu"] = float(vm.max()) if len(vm) > 0 else None
            rows.append(row)
        return pd.DataFrame(rows)

    def compare(self, metric: str = "vm"):
        """Side-by-side comparison DataFrame.

        Args:
            metric: ``"vm"`` (bus voltage magnitude), ``"va_deg"`` (angle degrees),
                or ``"theta"`` (DC bus angle in radians).

        Returns:
            DataFrame with rows = bus numbers, columns = case names.
        """
        import numpy as np
        import pandas as pd

        # Collect all bus numbers across cases.
        all_buses: set[int] = set()
        for r in self.results:
            if r.network is not None:
                all_buses.update(r.network.bus_numbers)
        bus_list = sorted(all_buses)

        data: dict[str, list[float | None]] = {}
        for r in self.results:
            col: list[float | None] = []
            if r.network is not None and r.solution is not None:
                bus_map = {b: i for i, b in enumerate(r.network.bus_numbers)}
                sol = r.solution
                if metric == "vm":
                    arr = getattr(sol, "vm", None)
                elif metric == "theta":
                    arr = getattr(sol, "theta", None)
                else:  # "va_deg"
                    va_deg = getattr(sol, "va_deg", None)
                    arr = va_deg
                for bus in bus_list:
                    idx = bus_map.get(bus)
                    if arr is not None and idx is not None:
                        col.append(float(arr[idx]))
                    else:
                        col.append(None)
            else:
                col = [None] * len(bus_list)
            data[r.case_name] = col

        return pd.DataFrame(data, index=bus_list)

    def violations(
        self,
        vmin: float = 0.95,
        vmax: float = 1.05,
        thermal_pct: float = 100.0,
    ):
        """Summary of voltage and thermal violations across all cases.

        Returns:
            DataFrame with columns: case, violation_type, element, value, limit.
        """
        import pandas as pd

        columns = ["case", "type", "element", "value", "limit"]
        rows: list[dict[str, Any]] = []
        for r in self.results:
            if r.solution is None or r.network is None:
                continue
            sol, net = r.solution, r.network
            # Voltage violations (AC solutions only — DC has no vm).
            vm = getattr(sol, "vm", None)
            if vm is not None:
                bus_nums = list(net.bus_numbers)
                for i in range(len(bus_nums)):
                    v = float(vm[i])
                    if v < vmin:
                        rows.append(
                            {
                                "case": r.case_name,
                                "type": "voltage_low",
                                "element": f"bus:{bus_nums[i]}",
                                "value": v,
                                "limit": vmin,
                            }
                        )
                    elif v > vmax:
                        rows.append(
                            {
                                "case": r.case_name,
                                "type": "voltage_high",
                                "element": f"bus:{bus_nums[i]}",
                                "value": v,
                                "limit": vmax,
                            }
                        )
            # Thermal violations (AC solutions only — DC has no branch_loading_pct).
            loading = getattr(sol, "branch_loading_pct", None)
            if loading is not None:
                if callable(loading):
                    loading = loading()
                from_buses = list(net.branch_from)
                to_buses = list(net.branch_to)
                circuits = list(net.branch_circuit)
                for i in range(len(from_buses)):
                    pct = float(loading[i])
                    if pct > thermal_pct:
                        rows.append(
                            {
                                "case": r.case_name,
                                "type": "thermal",
                                "element": f"branch:{from_buses[i]}→{to_buses[i]}({circuits[i]})",
                                "value": pct,
                                "limit": thermal_pct,
                            }
                        )
        return pd.DataFrame(rows, columns=columns)

    def __len__(self) -> int:
        return len(self.results)

    def __getitem__(self, idx: int) -> ScenarioResult:
        return self.results[idx]


def batch_solve(
    cases: list[str | Path | Any],
    solver: str = "acpf",
    parallel: bool = True,
    max_workers: int | None = None,
    **solver_kwargs: Any,
) -> BatchResults:
    """Solve multiple cases and collect results.

    Args:
        cases: List of file paths (str/Path) or ``Network`` objects.
        solver: Solver method — ``"acpf"``, ``"dcpf"``, ``"fdpf"``,
            ``"dc-opf"``, ``"ac-opf"``, or ``"scopf"``.
        parallel: Use thread pool for parallel execution (default True).
            Surge releases the GIL so threads give real parallelism.
        max_workers: Maximum threads (default: min(len(cases), 8)).
        **solver_kwargs: Passed to the solver function.

    Returns:
        :class:`BatchResults` with one :class:`ScenarioResult` per case.

    Raises:
        TypeError: If *cases* is a dict (pass a list instead).
    """
    if isinstance(cases, dict):
        raise TypeError(
            "batch_solve() expects a list of file paths or Network objects, "
            "not a dict. Use list(your_dict.values()) to pass the networks."
        )
    import surge

    def _solve_one(case: str | Path | Any) -> ScenarioResult:
        t0 = time.perf_counter()
        try:
            if isinstance(case, (str, Path)):
                name = Path(case).stem
                net = surge.load(str(case))
            else:
                net = case
                name = getattr(net, "name", None) or "unnamed"

            sol = _dispatch_solver(solver, net, solver_kwargs)
            dt = time.perf_counter() - t0
            return ScenarioResult(case_name=name, network=net, solution=sol, wall_time_s=dt)
        except Exception as exc:
            dt = time.perf_counter() - t0
            name = Path(case).stem if isinstance(case, (str, Path)) else "unnamed"
            return ScenarioResult(case_name=name, error=str(exc), wall_time_s=dt)

    if parallel and len(cases) > 1:
        workers = max_workers or min(len(cases), 8)
        results: list[ScenarioResult] = [None] * len(cases)  # type: ignore[list-item]
        with ThreadPoolExecutor(max_workers=workers) as pool:
            future_to_idx = {pool.submit(_solve_one, c): i for i, c in enumerate(cases)}
            for future in as_completed(future_to_idx):
                idx = future_to_idx[future]
                results[idx] = future.result()
    else:
        results = [_solve_one(c) for c in cases]

    return BatchResults(results=results)


def _dispatch_solver(solver: str, net: Any, kwargs: dict[str, Any]) -> Any:
    import surge

    s = solver.lower()

    # Power flow
    if s == "acpf":
        return surge.solve_ac_pf(
            net,
            _require_options("acpf", kwargs, surge.powerflow.AcPfOptions),
        )
    elif s == "dcpf":
        return surge.solve_dc_pf(
            net,
            _require_options("dcpf", kwargs, surge.powerflow.DcPfOptions),
        )
    elif s == "fdpf":
        return surge.powerflow.solve_fdpf(
            net,
            _require_options("fdpf", kwargs, surge.powerflow.FdpfOptions),
        )
    # OPF
    elif s == "dc-opf":
        _require_opf_batch_kwargs("dc-opf", kwargs)
        return surge.solve_dc_opf(
            net,
            options=kwargs.get("options"),
            runtime=kwargs.get("runtime"),
        )
    elif s == "ac-opf":
        _require_opf_batch_kwargs("ac-opf", kwargs)
        return surge.solve_ac_opf(
            net,
            options=kwargs.get("options"),
            runtime=kwargs.get("runtime"),
        )
    elif s == "scopf":
        _require_opf_batch_kwargs("scopf", kwargs)
        return surge.solve_scopf(
            net,
            options=kwargs.get("options"),
            runtime=kwargs.get("runtime"),
        )
    else:
        supported = "acpf, dcpf, fdpf, dc-opf, ac-opf, scopf"
        raise ValueError(f"Unknown solver {solver!r} — supported: {supported}")


def _require_opf_batch_kwargs(solver: str, kwargs: dict[str, Any]) -> None:
    unknown = sorted(set(kwargs) - {"options", "runtime"})
    if unknown:
        joined = ", ".join(unknown)
        raise TypeError(
            f"batch_solve(..., solver={solver!r}) accepts only 'options' and 'runtime' "
            f"for OPF studies; got {joined}"
        )


def _require_options(solver: str, kwargs: dict[str, Any], option_type: type[Any]) -> Any:
    unknown = sorted(set(kwargs) - {"options"})
    if unknown:
        joined = ", ".join(unknown)
        raise TypeError(
            f"batch_solve(..., solver={solver!r}) accepts only 'options' "
            f"for power-flow studies; got {joined}"
        )
    options = kwargs.get("options")
    if options is None:
        return option_type()
    if not isinstance(options, option_type):
        raise TypeError(
            f"batch_solve(..., solver={solver!r}) requires options={option_type.__name__}(...)"
        )
    return options


__all__ = [
    "BatchResults",
    "ScenarioResult",
    "SweepResult",
    "SweepResults",
    "batch_solve",
    "parameter_sweep",
]
