#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""ResultInspector — structured access to dispatch results for debugging.

Wraps a DispatchResult dict (from ``result.to_dict()``) and provides
tabular views that answer common debugging questions without manual
dict-walking.

Usage::

    from benchmarks.go_c3.inspector import ResultInspector

    result = surge.solve_dispatch(network, request)
    insp = ResultInspector(result.to_dict())

    insp.cost_waterfall()              # cost breakdown by component
    insp.commitment_schedule("gen_A")  # per-period on/off schedule
    insp.dispatch_schedule()           # per-period per-resource MW
    insp.binding_constraints(period=3) # constraints with nonzero shadow price
    insp.violations()                  # all penalty slack entries
"""

from __future__ import annotations

from typing import Any, Optional


class ResultInspector:
    """Structured inspector over a DispatchResult dict."""

    def __init__(self, result: dict[str, Any]) -> None:
        self._r = result
        self._periods: list[dict] = result.get("periods", [])
        self._resources: list[dict] = result.get("resources", [])
        self._buses: list[dict] = result.get("buses", [])
        self._summaries: list[dict] = result.get("resource_summaries", [])
        self._summary: dict = result.get("summary", {})
        self._diagnostics: dict = result.get("diagnostics", {})
        self._model_diags: list[dict] = result.get("model_diagnostics", [])

    # ── Cost waterfall ────────────────────────────────────────────────────

    def cost_waterfall(self) -> dict[str, float]:
        """Decompose total cost into components.

        Returns a dict with keys: energy, no_load, startup, reserve, co2,
        and penalty subtotals computed from constraint results.
        """
        s = self._summary
        wf: dict[str, float] = {
            "total_cost": s.get("total_cost", 0.0),
            "energy_cost": s.get("total_energy_cost", 0.0),
            "no_load_cost": s.get("total_no_load_cost", 0.0),
            "startup_cost": s.get("total_startup_cost", 0.0),
            "reserve_cost": s.get("total_reserve_cost", 0.0),
            "co2_cost": 0.0,
        }

        # Sum penalty costs from constraint results across all periods.
        penalty_by_kind: dict[str, float] = {}
        for period in self._periods:
            for cr in period.get("constraint_results", []):
                kind = cr.get("kind", "other")
                pc = cr.get("penalty_cost", 0.0)
                if abs(pc) > 1e-12:
                    penalty_by_kind[kind] = penalty_by_kind.get(kind, 0.0) + pc

        wf["penalty_costs"] = penalty_by_kind
        wf["total_penalty"] = sum(penalty_by_kind.values())
        return wf

    # ── Commitment schedule ───────────────────────────────────────────────

    def commitment_schedule(
        self, resource_id: Optional[str] = None
    ) -> list[dict[str, Any]]:
        """Per-period commitment status for generators.

        Returns a flat list of dicts with keys: resource_id, period,
        on_status, startup, shutdown, power_mw, energy_cost.
        """
        rows: list[dict[str, Any]] = []
        for period in self._periods:
            t = period.get("period_index", 0)
            for rr in period.get("resource_results", []):
                if resource_id and rr.get("resource_id") != resource_id:
                    continue
                detail = rr.get("detail", {})
                # Generator detail has commitment/startup/shutdown fields.
                gen_detail = detail.get("Generator") or detail.get("generator", {})
                if not gen_detail and not detail.get("Storage"):
                    continue
                sto_detail = detail.get("Storage") or detail.get("storage", {})
                d = gen_detail or sto_detail or {}
                rows.append({
                    "resource_id": rr.get("resource_id", ""),
                    "period": t,
                    "on_status": d.get("commitment", True),
                    "startup": d.get("startup", False),
                    "shutdown": d.get("shutdown", False),
                    "power_mw": rr.get("power_mw", 0.0),
                    "energy_cost": rr.get("energy_cost", 0.0),
                })
        return rows

    # ── Dispatch schedule ─────────────────────────────────────────────────

    def dispatch_schedule(
        self, resource_id: Optional[str] = None
    ) -> list[dict[str, Any]]:
        """Per-period per-resource dispatch table.

        Returns a flat list of dicts with keys: resource_id, kind, period,
        power_mw, energy_cost, no_load_cost, startup_cost, reserve_awards.
        """
        rows: list[dict[str, Any]] = []
        for period in self._periods:
            t = period.get("period_index", 0)
            for rr in period.get("resource_results", []):
                if resource_id and rr.get("resource_id") != resource_id:
                    continue
                rows.append({
                    "resource_id": rr.get("resource_id", ""),
                    "kind": rr.get("kind", ""),
                    "period": t,
                    "power_mw": rr.get("power_mw", 0.0),
                    "energy_cost": rr.get("energy_cost", 0.0),
                    "no_load_cost": rr.get("no_load_cost", 0.0),
                    "startup_cost": rr.get("startup_cost", 0.0),
                    "reserve_awards": rr.get("reserve_awards", {}),
                })
        return rows

    # ── Bus results ───────────────────────────────────────────────────────

    def bus_results(
        self,
        bus_number: Optional[int] = None,
        period: Optional[int] = None,
    ) -> list[dict[str, Any]]:
        """Per-period per-bus results (LMP, voltage, injections)."""
        rows: list[dict[str, Any]] = []
        for p in self._periods:
            t = p.get("period_index", 0)
            if period is not None and t != period:
                continue
            for br in p.get("bus_results", []):
                if bus_number is not None and br.get("bus_number") != bus_number:
                    continue
                rows.append({
                    "bus_number": br.get("bus_number", 0),
                    "period": t,
                    "lmp": br.get("lmp", 0.0),
                    "mec": br.get("mec", 0.0),
                    "mcc": br.get("mcc", 0.0),
                    "mlc": br.get("mlc", 0.0),
                    "vm_pu": br.get("voltage_pu"),
                    "angle_rad": br.get("angle_rad"),
                    "net_injection_mw": br.get("net_injection_mw", 0.0),
                })
        return rows

    # ── Binding constraints ───────────────────────────────────────────────

    def binding_constraints(
        self, period: Optional[int] = None, min_shadow_price: float = 0.0
    ) -> list[dict[str, Any]]:
        """Constraints with nonzero shadow price, ranked by magnitude."""
        rows: list[dict[str, Any]] = []
        for p in self._periods:
            t = p.get("period_index", 0)
            if period is not None and t != period:
                continue
            for cr in p.get("constraint_results", []):
                sp = abs(cr.get("shadow_price", 0.0))
                if sp < min_shadow_price:
                    continue
                rows.append({
                    "constraint_id": cr.get("constraint_id", ""),
                    "kind": cr.get("kind", ""),
                    "period": t,
                    "shadow_price": cr.get("shadow_price", 0.0),
                    "slack_mw": cr.get("slack_mw", 0.0),
                    "penalty_cost": cr.get("penalty_cost", 0.0),
                })
        rows.sort(key=lambda r: abs(r["shadow_price"]), reverse=True)
        return rows

    # ── Violations ────────────────────────────────────────────────────────

    def violations(self) -> list[dict[str, Any]]:
        """All constraint results with nonzero penalty cost."""
        rows: list[dict[str, Any]] = []
        for p in self._periods:
            t = p.get("period_index", 0)
            for cr in p.get("constraint_results", []):
                pc = cr.get("penalty_cost", 0.0)
                if abs(pc) > 1e-12:
                    rows.append({
                        "constraint_id": cr.get("constraint_id", ""),
                        "kind": cr.get("kind", ""),
                        "period": t,
                        "slack_mw": cr.get("slack_mw", 0.0),
                        "penalty_cost": pc,
                    })
        rows.sort(key=lambda r: abs(r["penalty_cost"]), reverse=True)
        return rows

    # ── Reserve summary ───────────────────────────────────────────────────

    def reserve_summary(
        self, period: Optional[int] = None
    ) -> list[dict[str, Any]]:
        """Per-period reserve market clearing results."""
        rows: list[dict[str, Any]] = []
        for p in self._periods:
            t = p.get("period_index", 0)
            if period is not None and t != period:
                continue
            for rr in p.get("reserve_results", []):
                rows.append({
                    "product_id": rr.get("product_id", ""),
                    "scope": rr.get("scope", ""),
                    "zone_id": rr.get("zone_id"),
                    "period": t,
                    "requirement_mw": rr.get("requirement_mw", 0.0),
                    "provided_mw": rr.get("provided_mw", 0.0),
                    "shortfall_mw": rr.get("shortfall_mw", 0.0),
                    "clearing_price": rr.get("clearing_price", 0.0),
                })
        return rows

    # ── Model diagnostics ─────────────────────────────────────────────────

    def model_diagnostic(self, stage: Optional[str] = None) -> Optional[dict]:
        """Return the model diagnostic for a given stage, or the first one."""
        for md in self._model_diags:
            if stage is None or md.get("stage") == stage:
                return md
        return None

    # ── Quick summary ─────────────────────────────────────────────────────

    def summary_table(self) -> str:
        """One-line-per-component cost summary for quick terminal output."""
        wf = self.cost_waterfall()
        lines = [
            f"{'Component':<25} {'Cost ($)':>15}",
            f"{'─' * 25} {'─' * 15}",
            f"{'Energy':.<25} {wf['energy_cost']:>15,.2f}",
            f"{'No-load':.<25} {wf['no_load_cost']:>15,.2f}",
            f"{'Startup':.<25} {wf['startup_cost']:>15,.2f}",
            f"{'Reserve':.<25} {wf['reserve_cost']:>15,.2f}",
        ]
        for kind, cost in sorted(
            wf.get("penalty_costs", {}).items(),
            key=lambda kv: abs(kv[1]),
            reverse=True,
        ):
            lines.append(f"{'  penalty: ' + kind:.<25} {cost:>15,.2f}")
        lines.append(f"{'─' * 25} {'─' * 15}")
        lines.append(f"{'TOTAL':.<25} {wf['total_cost']:>15,.2f}")
        return "\n".join(lines)
