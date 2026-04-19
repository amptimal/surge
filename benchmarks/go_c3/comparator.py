#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""ResultComparator — compare a Surge dispatch result against a GO C3 reference.

Accepts a DispatchResult dict (``result.to_dict()``) and a GO C3 reference
solution dict (the ``time_series_output`` from a POP or winner submission),
plus the GO C3 problem for context (base_mva, device list, bus list).

Usage::

    from benchmarks.go_c3.comparator import ResultComparator

    cmp = ResultComparator(
        surge_result=result.to_dict(),
        reference_solution=ref_solution,   # GO C3 time_series_output dict
        problem=problem,                   # GoC3Problem instance
    )

    cmp.cost_waterfall_diff()    # side-by-side cost breakdown
    cmp.commitment_diff()        # resources with on_status mismatches
    cmp.dispatch_diff()          # MW deltas ranked by cost impact
    cmp.bus_diff()               # voltage/angle deltas ranked by magnitude
    cmp.first_divergence()       # earliest period where solutions differ
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Optional, Union


class ResultComparator:
    """Compare a Surge DispatchResult against a GO C3 reference solution."""

    def __init__(
        self,
        surge_result: dict[str, Any],
        reference_solution: dict[str, Any],
        problem: Any,
        tolerance: float = 1e-4,
    ) -> None:
        """
        Args:
            surge_result: DispatchResult.to_dict() output.
            reference_solution: GO C3 ``time_series_output`` dict from
                a reference/winner solution.
            problem: A GoC3Problem instance or raw problem dict (for base_mva,
                device UIDs, bus UIDs).
            tolerance: Absolute tolerance for considering values "different".
        """
        self._surge = surge_result
        self._tol = tolerance

        # Accept either a GoC3Problem or raw dict.
        if hasattr(problem, "raw"):
            self._problem = problem.raw
            self._base_mva = problem.base_norm_mva
        else:
            self._problem = problem
            self._base_mva = float(
                problem.get("network", {}).get("general", {}).get("base_norm_mva", 100.0)
            )

        # The reference may be wrapped in {"time_series_output": ...} or bare.
        if "time_series_output" in reference_solution:
            self._ref = reference_solution["time_series_output"]
        else:
            self._ref = reference_solution

        # Build UID indexes for the reference solution.
        self._ref_devices = {
            d["uid"]: d
            for d in self._ref.get("simple_dispatchable_device", [])
        }
        self._ref_buses = {
            b["uid"]: b for b in self._ref.get("bus", [])
        }
        self._ref_dc_lines = {
            d["uid"]: d for d in self._ref.get("dc_line", [])
        }

        # Build Surge resource index.
        self._surge_periods = self._surge.get("periods", [])
        self._surge_resources_by_id: dict[str, list[dict]] = {}
        for period in self._surge_periods:
            for rr in period.get("resource_results", []):
                rid = rr.get("resource_id", "")
                self._surge_resources_by_id.setdefault(rid, []).append(rr)

    @classmethod
    def from_files(
        cls,
        surge_result_path: Union[str, Path],
        reference_solution_path: Union[str, Path],
        problem_path: Union[str, Path],
        tolerance: float = 1e-4,
    ) -> "ResultComparator":
        """Convenience constructor from file paths."""
        with open(surge_result_path) as f:
            surge_result = json.load(f)
        with open(reference_solution_path) as f:
            reference_solution = json.load(f)
        with open(problem_path) as f:
            problem = json.load(f)
        return cls(surge_result, reference_solution, problem, tolerance)

    # ── Cost waterfall diff ───────────────────────────────────────────────

    def cost_waterfall_diff(self) -> dict[str, Any]:
        """Side-by-side cost breakdown (Surge summary vs reference objective).

        Note: The reference GO C3 solution doesn't carry a cost breakdown —
        only the validator produces the full objective. This returns the Surge
        decomposition so you can see which components dominate YOUR cost.
        """
        s = self._surge.get("summary", {})
        return {
            "surge": {
                "total_cost": s.get("total_cost", 0.0),
                "energy_cost": s.get("total_energy_cost", 0.0),
                "no_load_cost": s.get("total_no_load_cost", 0.0),
                "startup_cost": s.get("total_startup_cost", 0.0),
                "reserve_cost": s.get("total_reserve_cost", 0.0),
            },
        }

    # ── Commitment diff ───────────────────────────────────────────────────

    def commitment_diff(self) -> list[dict[str, Any]]:
        """Resources with on_status mismatches between Surge and reference.

        Returns rows ranked by number of periods with mismatched commitment.
        """
        diffs: list[dict[str, Any]] = []

        for ref_dev in self._ref.get("simple_dispatchable_device", []):
            uid = ref_dev["uid"]
            ref_on = ref_dev.get("on_status", [])
            n_periods = len(ref_on)

            # Find matching Surge resource.
            surge_on = self._extract_surge_commitment(uid, n_periods)
            if surge_on is None:
                diffs.append({
                    "resource_id": uid,
                    "issue": "missing_in_surge",
                    "n_mismatch": n_periods,
                    "mismatched_periods": list(range(n_periods)),
                })
                continue

            mismatched = []
            for t in range(n_periods):
                r_on = bool(ref_on[t]) if t < len(ref_on) else False
                s_on = bool(surge_on[t]) if t < len(surge_on) else False
                if r_on != s_on:
                    mismatched.append(t)

            if mismatched:
                diffs.append({
                    "resource_id": uid,
                    "issue": "commitment_mismatch",
                    "n_mismatch": len(mismatched),
                    "mismatched_periods": mismatched,
                    "surge_on": surge_on,
                    "ref_on": [bool(v) for v in ref_on],
                })

        diffs.sort(key=lambda r: r["n_mismatch"], reverse=True)
        return diffs

    # ── Dispatch diff ─────────────────────────────────────────────────────

    def dispatch_diff(self, top_n: int = 50) -> list[dict[str, Any]]:
        """Per-period per-resource MW deltas, ranked by |delta_mw|.

        Returns the top-N largest absolute MW differences.
        """
        rows: list[dict[str, Any]] = []

        for ref_dev in self._ref.get("simple_dispatchable_device", []):
            uid = ref_dev["uid"]
            ref_p_on = ref_dev.get("p_on", [])
            n_periods = len(ref_p_on)

            surge_mw = self._extract_surge_dispatch_mw(uid, n_periods)

            for t in range(n_periods):
                ref_mw = ref_p_on[t] * self._base_mva if t < len(ref_p_on) else 0.0
                s_mw = surge_mw[t] if t < len(surge_mw) else 0.0
                delta = s_mw - ref_mw
                if abs(delta) > self._tol:
                    rows.append({
                        "resource_id": uid,
                        "period": t,
                        "surge_mw": s_mw,
                        "ref_mw": ref_mw,
                        "delta_mw": delta,
                    })

        rows.sort(key=lambda r: abs(r["delta_mw"]), reverse=True)
        return rows[:top_n]

    # ── Bus diff ──────────────────────────────────────────────────────────

    def bus_diff(self, top_n: int = 50) -> list[dict[str, Any]]:
        """Per-period per-bus voltage/angle deltas, ranked by magnitude."""
        rows: list[dict[str, Any]] = []

        # Build Surge bus results index: {bus_name: [{period_data}, ...]}
        surge_bus_by_name: dict[str, dict[int, dict]] = {}
        bus_catalog = {
            b.get("bus_number"): b.get("name", str(b.get("bus_number", "")))
            for b in self._surge.get("buses", [])
        }
        for period in self._surge_periods:
            t = period.get("period_index", 0)
            for br in period.get("bus_results", []):
                bus_num = br.get("bus_number", 0)
                bus_name = bus_catalog.get(bus_num, str(bus_num))
                surge_bus_by_name.setdefault(bus_name, {})[t] = br

        for ref_bus in self._ref.get("bus", []):
            uid = ref_bus["uid"]
            ref_vm = ref_bus.get("vm", [])
            ref_va = ref_bus.get("va", [])
            n_periods = len(ref_vm)

            surge_periods = surge_bus_by_name.get(uid, {})
            for t in range(n_periods):
                s_bus = surge_periods.get(t, {})
                r_vm = ref_vm[t] if t < len(ref_vm) else 1.0
                r_va = ref_va[t] if t < len(ref_va) else 0.0
                s_vm = s_bus.get("voltage_pu") or 1.0
                s_va = s_bus.get("angle_rad") or 0.0

                vm_delta = s_vm - r_vm
                va_delta = s_va - r_va

                if abs(vm_delta) > self._tol or abs(va_delta) > self._tol:
                    rows.append({
                        "bus_uid": uid,
                        "period": t,
                        "surge_vm": s_vm,
                        "ref_vm": r_vm,
                        "delta_vm": vm_delta,
                        "surge_va": s_va,
                        "ref_va": r_va,
                        "delta_va": va_delta,
                    })

        rows.sort(
            key=lambda r: abs(r["delta_vm"]) + abs(r["delta_va"]),
            reverse=True,
        )
        return rows[:top_n]

    # ── First divergence ──────────────────────────────────────────────────

    def first_divergence(self, mw_tolerance: float = 0.1) -> Optional[dict[str, Any]]:
        """Find the earliest period where any resource's MW differs.

        Returns a dict with the first diverging resource, period, and delta,
        or None if solutions match within tolerance.
        """
        for ref_dev in self._ref.get("simple_dispatchable_device", []):
            uid = ref_dev["uid"]
            ref_p_on = ref_dev.get("p_on", [])
            n_periods = len(ref_p_on)
            surge_mw = self._extract_surge_dispatch_mw(uid, n_periods)

            for t in range(n_periods):
                ref_mw = ref_p_on[t] * self._base_mva if t < len(ref_p_on) else 0.0
                s_mw = surge_mw[t] if t < len(surge_mw) else 0.0
                if abs(s_mw - ref_mw) > mw_tolerance:
                    return {
                        "resource_id": uid,
                        "period": t,
                        "surge_mw": s_mw,
                        "ref_mw": ref_mw,
                        "delta_mw": s_mw - ref_mw,
                    }
        return None

    # ── Summary report ────────────────────────────────────────────────────

    def summary_report(self) -> str:
        """Quick terminal-friendly comparison summary."""
        lines: list[str] = []
        lines.append("=== Surge vs Reference Comparison ===")

        # Commitment
        commit_diffs = self.commitment_diff()
        n_commit_mismatch = sum(d["n_mismatch"] for d in commit_diffs)
        lines.append(
            f"Commitment: {len(commit_diffs)} resources with mismatches "
            f"({n_commit_mismatch} total period-mismatches)"
        )
        for d in commit_diffs[:5]:
            lines.append(
                f"  {d['resource_id']}: {d['n_mismatch']} periods "
                f"(first: t={d['mismatched_periods'][0]})"
            )

        # Dispatch
        dispatch_diffs = self.dispatch_diff(top_n=10)
        if dispatch_diffs:
            lines.append(
                f"\nDispatch: top {len(dispatch_diffs)} MW deltas"
            )
            for d in dispatch_diffs[:5]:
                lines.append(
                    f"  {d['resource_id']} t={d['period']}: "
                    f"surge={d['surge_mw']:.1f} ref={d['ref_mw']:.1f} "
                    f"delta={d['delta_mw']:+.1f} MW"
                )

        # First divergence
        first = self.first_divergence()
        if first:
            lines.append(
                f"\nFirst divergence: {first['resource_id']} at t={first['period']} "
                f"(delta={first['delta_mw']:+.1f} MW)"
            )
        else:
            lines.append("\nNo dispatch divergence found within tolerance.")

        return "\n".join(lines)

    # ── Internal helpers ──────────────────────────────────────────────────

    def _extract_surge_commitment(
        self, resource_uid: str, n_periods: int
    ) -> Optional[list[bool]]:
        """Extract per-period on_status for a resource from Surge results."""
        result: list[bool] = [False] * n_periods
        found = False
        for period in self._surge_periods:
            t = period.get("period_index", 0)
            if t >= n_periods:
                continue
            for rr in period.get("resource_results", []):
                if rr.get("resource_id") != resource_uid:
                    continue
                found = True
                detail = rr.get("detail", {})
                gen_d = detail.get("Generator") or detail.get("generator", {})
                sto_d = detail.get("Storage") or detail.get("storage", {})
                d = gen_d or sto_d or {}
                result[t] = d.get("commitment", True)
                break
        return result if found else None

    def _extract_surge_dispatch_mw(
        self, resource_uid: str, n_periods: int
    ) -> list[float]:
        """Extract per-period power_mw for a resource from Surge results."""
        result: list[float] = [0.0] * n_periods
        for period in self._surge_periods:
            t = period.get("period_index", 0)
            if t >= n_periods:
                continue
            for rr in period.get("resource_results", []):
                if rr.get("resource_id") == resource_uid:
                    result[t] = rr.get("power_mw", 0.0)
                    break
        return result
