#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Case-data assembly helpers for the GO C3 dashboard.

This module used to generate a self-contained HTML dashboard. The server
(``markets/go_c3/server``) is now the live frontend; this file keeps only
the data-building helpers it calls into.
"""
from __future__ import annotations

import html
import json
import math
from pathlib import Path
from typing import Any

from benchmarks.go_c3.datasets import ScenarioRecord
from benchmarks.go_c3.paths import default_cache_root
from benchmarks.go_c3.runner import baseline_output_dir
from benchmarks.go_c3.violations import (
    _branch_flow_pi,
    _build_acl_params,
    _build_xfr_params,
    _xfr_flow_pi,
    compute_solution_violations,
)


class _DashboardScenario:
    """ScenarioRecord plus the switching mode detected from the run directory."""
    __slots__ = ("record", "switching_mode")

    def __init__(self, record: ScenarioRecord, switching_mode: str):
        self.record = record
        self.switching_mode = switching_mode

    # Delegate common attributes for convenience.
    def __getattr__(self, name: str):
        return getattr(self.record, name)


def _discover_baseline_scenarios(cache_root: Path) -> list[_DashboardScenario]:
    """Scan runs/baseline/ for scenario dirs that have a run-report.json."""
    baseline_root = cache_root / "runs" / "baseline"
    if not baseline_root.exists():
        return []
    scenarios: list[_DashboardScenario] = []
    for run_report_path in sorted(baseline_root.rglob("run-report.json")):
        # Skip archived prior runs (scenario_NNN/archive/{ts}/run-report.json).
        if "archive" in run_report_path.parts:
            continue
        try:
            report = json.loads(run_report_path.read_text(encoding="utf-8"))
        except Exception:
            continue
        dataset_key = report.get("dataset_key")
        division = report.get("division")
        network_model = report.get("network_model", "")
        scenario_id = report.get("scenario_id")
        problem_path = report.get("problem_path")
        if not all([dataset_key, division, scenario_id is not None, problem_path]):
            continue
        pp = Path(problem_path)
        if not pp.exists():
            continue
        # Detect switching mode from directory structure (…/sw0/scenario_NNN or …/sw1/scenario_NNN).
        # Falls back to "sw0" for legacy layouts without the sw0/sw1 level.
        rel = run_report_path.relative_to(baseline_root)
        parts = rel.parts  # e.g. ("event4_73", "D2", "sw0", "scenario_911", "run-report.json")
        sw = "sw0"
        for p in parts:
            if p in ("sw0", "sw1"):
                sw = p
                break
        record = ScenarioRecord(
            dataset_key=dataset_key,
            division=division,
            network_model=network_model,
            scenario_id=scenario_id,
            problem_path=pp,
            pop_solution_path=None,
            pop_log_path=None,
        )
        scenarios.append(_DashboardScenario(record, sw))
    return scenarios


def _load_json(path: Path) -> dict | None:
    if path.exists():
        return json.loads(path.read_text(encoding="utf-8"))
    return None


_Z_BREAKDOWN_KEYS = [
    ("z", "Objective (z)"),
    ("z_base", "z_base (surplus)"),
    ("z_cost", "z_cost (energy+commit)"),
    ("z_penalty", "z_penalty (violations)"),
    ("z_value", "z_value (load served)"),
    ("z_k_worst_case", "z_k (worst contingency)"),
    ("z_k_average_case", "z_k (avg contingency)"),
    ("sum_sd_t_z_on", "No-load cost slack"),
    ("sum_sd_t_z_su", "Startup cost slack"),
    ("sum_pr_t_z_p", "Producer P slack"),
    ("sum_cs_t_z_p", "Consumer P slack"),
    ("sum_bus_t_z_p", "Bus P balance penalty"),
    ("sum_bus_t_z_q", "Bus Q balance penalty"),
    ("sum_acl_t_z_s", "AC line thermal slack"),
    ("sum_xfr_t_z_s", "Transformer thermal slack"),
    ("sum_prz_t_z_rgu", "Reserve up shortfall"),
    ("sum_prz_t_z_rgd", "Reserve down shortfall"),
    ("sum_prz_t_z_scr", "Spinning reserve shortfall"),
    ("sum_prz_t_z_nsc", "Non-spin reserve shortfall"),
    ("sum_prz_t_z_rru", "Ramp up reserve shortfall"),
    ("sum_prz_t_z_rrd", "Ramp down reserve shortfall"),
    ("sum_sd_t_z_rgd", "Device reserve down slack"),
    ("sum_sd_t_z_rgu", "Device reserve up slack"),
    ("feas", "Feasible"),
    ("phys_feas", "Physically Feasible"),
]


def _extract_z_breakdown(evaluation: dict) -> dict[str, Any]:
    """Extract z-score breakdown from a validator evaluation dict."""
    out = {}
    for key, _ in _Z_BREAKDOWN_KEYS:
        val = evaluation.get(key)
        if val is not None:
            out[key] = val
    return out


# Cost component buckets from dispatch result `summary.total_*` — shared
# between DC SCUC and AC SCED. Tuples are (display_bucket, summary_field).
_OBJECTIVE_BUCKETS: tuple[tuple[str, str], ...] = (
    ("energy", "total_energy_cost"),
    ("no_load", "total_no_load_cost"),
    ("startup", "total_startup_cost"),
    ("shutdown", "total_shutdown_cost"),
    ("reserve", "total_reserve_cost"),
    ("tracking", "total_tracking_cost"),
    ("adder", "total_adder_cost"),
    ("other", "total_other_cost"),
    ("penalty", "total_penalty_cost"),
)

# Taxonomy used to split the penalty bucket. Tuples are
# (display_key, cost_field, quantity_field, quantity_unit).
_PENALTY_CATEGORIES: tuple[tuple[str, str, str | None, str | None], ...] = (
    ("thermal", "thermal_total_cost", "thermal_total_mw", "MW"),
    ("reserve_shortfall", "reserve_shortfall_total_cost", "reserve_shortfall_total_mw", "MW"),
    ("power_balance_p", "power_balance_p_total_cost", "power_balance_p_total_mw", "MW"),
    ("power_balance_q", "power_balance_q_total_cost", "power_balance_q_total_mvar", "Mvar"),
    ("flowgate", "flowgate_total_cost", "flowgate_total_mw", "MW"),
    ("ramp", "ramp_total_cost", "ramp_total_mw", "MW"),
    ("voltage", "voltage_total_cost", "voltage_total_pu", "pu"),
    ("angle", "angle_total_cost", "angle_total_rad", "rad"),
    ("headroom_footroom", "headroom_footroom_total_cost", "headroom_footroom_total_mw", "MW"),
    ("energy_window", "energy_window_total_cost", None, None),
)


def _build_objective_breakdown(result: dict | None) -> dict[str, Any] | None:
    """Assemble the per-run objective breakdown for one solve stage.

    The dispatch result's ``summary`` gives us the nine cost buckets; the
    ``penalty_summary`` splits the penalty bucket into its ten physical
    categories with both dollars and physical quantities. Per-period values
    are re-aggregated by walking ``periods[i].objective_terms`` and summing
    dollars by ``bucket`` — this matches ``summary.total_*`` to machine
    precision on the cases we sampled.
    """
    if result is None:
        return None
    summary = result.get("summary") or {}
    penalty_summary = result.get("penalty_summary") or {}
    periods = result.get("periods") or []

    summary_out: dict[str, float | None] = {
        "total_cost": summary.get("total_cost"),
    }
    for bucket, field in _OBJECTIVE_BUCKETS:
        summary_out[bucket] = summary.get(field)

    penalty_out: list[dict[str, Any]] = []
    for key, cost_field, qty_field, qty_unit in _PENALTY_CATEGORIES:
        penalty_out.append({
            "key": key,
            "cost": penalty_summary.get(cost_field),
            "quantity": penalty_summary.get(qty_field) if qty_field else None,
            "quantity_unit": qty_unit,
        })

    per_period: list[dict[str, Any]] = []
    for p in periods:
        period_index = p.get("period_index")
        row: dict[str, Any] = {"period": period_index}
        # Initialize every bucket to 0.0 so downstream code doesn't see holes.
        for bucket, _ in _OBJECTIVE_BUCKETS:
            row[bucket] = 0.0
        for term in p.get("objective_terms", []) or []:
            bucket = term.get("bucket")
            if bucket in row:
                row[bucket] += float(term.get("dollars") or 0.0)
        row["total"] = p.get("total_cost") or sum(row[b] for b, _ in _OBJECTIVE_BUCKETS)
        per_period.append(row)

    return {
        "summary": summary_out,
        "penalty_summary": penalty_out,
        "per_period": per_period,
    }


def _compute_bus_layout(problem_raw: dict) -> dict[str, list[float]]:
    """Kamada-Kawai 2D layout of the bus+branch graph, normalized to [0, 1]².

    The GO C3 problem has no geographic coordinates, so we lay the graph out
    automatically. Kamada-Kawai (spring model with all-pairs shortest paths)
    is deterministic and produces good results for transmission-scale meshed
    networks up to ~1k buses. Above that it gets expensive; we still compute
    it (one-shot at case-build time, cached with the rest of the case JSON)
    but the frontend will need to swap renderers for very large grids.
    """
    try:
        import networkx as nx  # type: ignore
    except ImportError:
        return {}
    buses = problem_raw.get("network", {}).get("bus", [])
    if not buses:
        return {}
    graph = nx.Graph()
    for b in buses:
        uid = b.get("uid")
        if uid is not None:
            graph.add_node(uid)
    for section in ("ac_line", "two_winding_transformer", "dc_line"):
        for br in problem_raw.get("network", {}).get(section, []) or []:
            fr, to = br.get("fr_bus"), br.get("to_bus")
            if fr is not None and to is not None:
                graph.add_edge(fr, to)
    if graph.number_of_edges() == 0:
        # No connectivity to work with; fall back to a circular arrangement.
        pos = nx.circular_layout(graph)
    else:
        try:
            pos = nx.kamada_kawai_layout(graph)
        except Exception:
            # kamada_kawai requires a connected graph; fall back to spring layout
            # with a fixed seed for reproducibility.
            pos = nx.spring_layout(graph, seed=42, iterations=200)
    # Normalize to [0, 1]² with a 2% padding margin so nodes don't clip edges.
    xs = [p[0] for p in pos.values()]
    ys = [p[1] for p in pos.values()]
    xmin, xmax = (min(xs), max(xs)) if xs else (0.0, 1.0)
    ymin, ymax = (min(ys), max(ys)) if ys else (0.0, 1.0)
    xrange = max(xmax - xmin, 1e-9)
    yrange = max(ymax - ymin, 1e-9)
    padding = 0.02
    out: dict[str, list[float]] = {}
    for uid, (x, y) in pos.items():
        nx_ = padding + (1 - 2 * padding) * (x - xmin) / xrange
        ny_ = padding + (1 - 2 * padding) * (y - ymin) / yrange
        out[str(uid)] = [round(float(nx_), 5), round(float(ny_), 5)]
    return out


def _compute_grid_assets(problem_raw: dict) -> dict[str, dict[str, list[str]]]:
    """Roll up the UID lists of every asset attached to each bus.

    Lets the frontend render satellite glyphs around each bus node without
    re-walking the full device/shunt/transformer/dc_line arrays on every
    period change or color-mode switch.
    """
    network = problem_raw.get("network", {}) or {}
    rollup: dict[str, dict[str, list[str]]] = {}

    def ensure(bus_uid: str) -> dict[str, list[str]]:
        if bus_uid not in rollup:
            rollup[bus_uid] = {
                "producers": [],
                "consumers": [],
                "shunts": [],
                "transformer_ends": [],
                "hvdc_ends": [],
            }
        return rollup[bus_uid]

    for b in network.get("bus", []) or []:
        if (uid := b.get("uid")) is not None:
            ensure(str(uid))

    for d in network.get("simple_dispatchable_device", []) or []:
        bus_uid = d.get("bus")
        if bus_uid is None:
            continue
        slot = "producers" if d.get("device_type") == "producer" else "consumers"
        ensure(str(bus_uid))[slot].append(d.get("uid"))
    for s in network.get("shunt", []) or []:
        if (bus_uid := s.get("bus")) is not None:
            ensure(str(bus_uid))["shunts"].append(s.get("uid"))
    for xf in network.get("two_winding_transformer", []) or []:
        for side in ("fr_bus", "to_bus"):
            if (bus_uid := xf.get(side)) is not None:
                ensure(str(bus_uid))["transformer_ends"].append(xf.get("uid"))
    for dc in network.get("dc_line", []) or []:
        for side in ("fr_bus", "to_bus"):
            if (bus_uid := dc.get(side)) is not None:
                ensure(str(bus_uid))["hvdc_ends"].append(dc.get("uid"))
    return rollup


def _compute_shunt_catalog(problem_raw: dict, solution: dict | None = None) -> dict[str, dict[str, Any]]:
    """Static shunt parameters + per-period dispatched step for each shunt.

    Static fields come from the problem; the per-period ``step_series`` list
    is extracted from ``solution.time_series_output.shunt`` when present.
    """
    out: dict[str, dict[str, Any]] = {}
    for s in problem_raw.get("network", {}).get("shunt", []) or []:
        uid = s.get("uid")
        if uid is None:
            continue
        out[str(uid)] = {
            "uid": str(uid),
            "bus": s.get("bus"),
            "gs": s.get("gs"),
            "bs": s.get("bs"),
            "step_lb": s.get("step_lb"),
            "step_ub": s.get("step_ub"),
            "initial_status": s.get("initial_status"),
            "step_series": [],
        }
    for entry in (solution or {}).get("time_series_output", {}).get("shunt", []) or []:
        uid = str(entry.get("uid"))
        if uid in out:
            out[uid]["step_series"] = list(entry.get("step") or [])
    return out


def _compute_xfmr_catalog(problem_raw: dict, solution: dict | None = None) -> dict[str, dict[str, Any]]:
    """Static transformer parameters + per-period dispatched tap state.

    GO C3 doesn't have a distinct "phase shifter" type — it's a
    ``two_winding_transformer`` with ``ta_lb != ta_ub``. We flag those so
    the frontend can title the modal appropriately.
    """
    out: dict[str, dict[str, Any]] = {}
    for xf in problem_raw.get("network", {}).get("two_winding_transformer", []) or []:
        uid = xf.get("uid")
        if uid is None:
            continue
        tm_lb, tm_ub = xf.get("tm_lb"), xf.get("tm_ub")
        ta_lb, ta_ub = xf.get("ta_lb"), xf.get("ta_ub")
        out[str(uid)] = {
            "uid": str(uid),
            "fr_bus": xf.get("fr_bus"),
            "to_bus": xf.get("to_bus"),
            "tm_bounds": [tm_lb, tm_ub] if tm_lb is not None and tm_ub is not None else None,
            "ta_bounds": [ta_lb, ta_ub] if ta_lb is not None and ta_ub is not None else None,
            "is_phase_shifter": ta_lb is not None and ta_ub is not None and ta_lb != ta_ub,
            "has_tap_ratio": tm_lb is not None and tm_ub is not None and tm_lb != tm_ub,
            "initial_status": xf.get("initial_status"),
            "tm_series": [],
            "ta_series": [],
            "on_status_series": [],
        }
    for entry in (solution or {}).get("time_series_output", {}).get("two_winding_transformer", []) or []:
        uid = str(entry.get("uid"))
        if uid in out:
            out[uid]["tm_series"] = list(entry.get("tm") or [])
            out[uid]["ta_series"] = list(entry.get("ta") or [])
            out[uid]["on_status_series"] = list(entry.get("on_status") or [])
    return out


def _extract_dc_violations(dc_result: dict | None) -> dict[str, Any]:
    """Aggregate DC SCUC constraint violations from dispatch result."""
    if dc_result is None:
        return {}
    from collections import defaultdict
    by_kind: dict[str, dict[str, float]] = defaultdict(lambda: {"count": 0, "total_slack_mw": 0.0, "total_penalty": 0.0})
    pb_curtailment = 0.0
    pb_excess = 0.0
    per_period: list[dict] = []

    for period in dc_result.get("periods", []):
        pbv = period.get("power_balance_violation", {})
        pb_curtailment += pbv.get("curtailment_mw", 0.0)
        pb_excess += pbv.get("excess_mw", 0.0)
        period_violations: dict[str, dict[str, float]] = defaultdict(lambda: {"count": 0, "slack_mw": 0.0, "penalty": 0.0})
        for cr in period.get("constraint_results", []):
            kind = cr.get("kind", "unknown")
            by_kind[kind]["count"] += 1
            by_kind[kind]["total_slack_mw"] += abs(cr.get("slack_mw", 0.0))
            by_kind[kind]["total_penalty"] += abs(cr.get("penalty_cost", 0.0))
            period_violations[kind]["count"] += 1
            period_violations[kind]["slack_mw"] += abs(cr.get("slack_mw", 0.0))
            period_violations[kind]["penalty"] += abs(cr.get("penalty_cost", 0.0))
        per_period.append({
            "period_index": period.get("period_index", len(per_period)),
            "curtailment_mw": pbv.get("curtailment_mw", 0.0),
            "excess_mw": pbv.get("excess_mw", 0.0),
            "by_kind": dict(period_violations),
        })

    summary = {
        "curtailment_mw": round(pb_curtailment, 4),
        "excess_mw": round(pb_excess, 4),
        "by_kind": {k: {kk: round(vv, 4) for kk, vv in v.items()} for k, v in sorted(by_kind.items())},
        "total_violations": sum(v["count"] for v in by_kind.values()),
        "total_penalty": round(sum(v["total_penalty"] for v in by_kind.values()), 2),
    }
    return {"summary": summary, "periods": per_period}


def _compute_marginal_cost(
    dtype: str,
    ts: dict,
    ac_p: list[float],
    ac_ecost: list[float],
    base_mva: float,
    n_periods: int,
) -> list[float]:
    """Compute per-period marginal cost.

    For producers: energy_cost / power_mw ($/MWh).
    For consumers: bid price of the last served cost block ($/MWh).
    """
    cost_blocks = ts.get("cost", [])
    mc = [0.0] * n_periods
    for t in range(n_periods):
        p = ac_p[t] if t < len(ac_p) else 0.0
        if dtype == "producer":
            ecost = ac_ecost[t] if t < len(ac_ecost) else 0.0
            mc[t] = ecost / abs(p) if abs(p) > 0.1 else 0.0
        elif dtype == "consumer" and t < len(cost_blocks):
            # cost_blocks[t] = [[price_$/pu, block_qty_pu], ...]. GO C3 reports
            # block prices in $/(pu·h), so divide by base_mva to get $/MWh.
            blocks = cost_blocks[t] if isinstance(cost_blocks[t], list) else []
            served_pu = abs(p) / base_mva
            cumulative = 0.0
            block_price_pu = 0.0
            for block in blocks:
                if not isinstance(block, list) or len(block) < 2:
                    continue
                price, qty = block[0], block[1]
                cumulative += qty
                if served_pu > 1e-6:
                    block_price_pu = price
                if cumulative >= served_pu - 1e-8:
                    break
            mc[t] = block_price_pu / base_mva if base_mva > 0 else block_price_pu
    return mc


def _safe_float(v: Any, default: float = 0.0) -> float:
    try:
        return float(v)
    except (TypeError, ValueError):
        return default


def _extract_device_series(
    dispatch_result: dict | None,
    n_periods: int,
) -> dict[str, dict[str, list]]:
    """Extract per-resource P/Q/on/energy_cost/lmp series from a dispatch result.

    Block-decomposed consumer IDs (sd_XXX::blk:YY) are aggregated back to
    their parent UID (sd_XXX).  Consumer power_mw is negative (generator
    convention); we use detail.served_p_mw when available and negate to
    report positive consumption in MW.
    """
    out: dict[str, dict[str, list]] = {}
    if dispatch_result is None:
        return out
    periods = dispatch_result.get("periods", [])
    for t, period in enumerate(periods):
        # Build bus LMP lookup for this period
        bus_lmp: dict[int, float] = {}
        for br in period.get("bus_results", []):
            bus_lmp[br.get("bus_number", 0)] = _safe_float(br.get("lmp"))

        for rr in period.get("resource_results", []):
            rid = rr.get("resource_id", "")
            if rid.startswith("__"):
                continue
            parent_id = rid.split("::")[0] if "::" in rid else rid
            is_block = "::" in rid
            entry = out.setdefault(parent_id, {
                "p": [0.0] * n_periods, "q": [0.0] * n_periods,
                "on": [0] * n_periods, "energy_cost": [0.0] * n_periods,
                "lmp": [0.0] * n_periods, "bid_price": [0.0] * n_periods,
            })
            detail = rr.get("detail", {}) or {}
            ecost = _safe_float(rr.get("energy_cost"))
            bus_num = rr.get("bus_number", 0)

            if is_block:
                served = detail.get("served_p_mw")
                served_mw = _safe_float(served) if served is not None else abs(_safe_float(rr.get("power_mw")))
                entry["p"][t] += served_mw
                served_q = detail.get("served_q_mvar")
                if served_q is not None:
                    entry["q"][t] += _safe_float(served_q)
                entry["energy_cost"][t] += ecost
                if served_mw > 1e-6:
                    entry["on"][t] = 1
                    # Track bid price of the last served block (highest index with nonzero served)
                    block_lmp = detail.get("lmp_at_bus")
                    if block_lmp is not None:
                        entry["lmp"][t] = _safe_float(block_lmp)
            else:
                entry["p"][t] = _safe_float(rr.get("power_mw"))
                q_val = rr.get("q_mvar")
                if q_val is None:
                    q_val = detail.get("q_mvar")
                entry["q"][t] = _safe_float(q_val)
                entry["energy_cost"][t] = ecost
                entry["lmp"][t] = bus_lmp.get(bus_num, 0.0)
                entry["on"][t] = 1 if detail.get("commitment", False) else (1 if abs(entry["p"][t]) > 1e-6 else 0)
                # Reserve awards
                awards = rr.get("reserve_awards", {})
                if isinstance(awards, dict):
                    for prod, mw in awards.items():
                        entry.setdefault("res_" + prod, [0.0] * n_periods)[t] = _safe_float(mw)
    return out


def _extract_hvdc_series(dispatch_result: dict | None, n_periods: int) -> dict[str, dict[str, list]]:
    out: dict[str, dict[str, list]] = {}
    if dispatch_result is None:
        return out
    for t, period in enumerate(dispatch_result.get("periods", [])):
        for hr in period.get("hvdc_results", []):
            lid = str(hr.get("link_id", ""))
            entry = out.setdefault(lid, {"p": [0.0] * n_periods})
            entry["p"][t] = _safe_float(hr.get("mw"))
    return out


def _extract_bus_series(
    dispatch_result: dict | None,
    n_periods: int,
    bus_number_to_uid: dict[int, str],
) -> dict[str, dict[str, list]]:
    """Extract per-bus V, theta, net injection, withdrawals from a dispatch result."""
    out: dict[str, dict[str, list]] = {}
    if dispatch_result is None:
        return out
    for t, period in enumerate(dispatch_result.get("periods", [])):
        for br in period.get("bus_results", []):
            bnum = br.get("bus_number")
            uid = bus_number_to_uid.get(bnum, f"bus_{bnum}")
            entry = out.setdefault(uid, {
                "vm": [0.0] * n_periods, "va": [0.0] * n_periods,
                "p_inj": [0.0] * n_periods, "q_inj": [0.0] * n_periods,
                "p_wd": [0.0] * n_periods, "q_wd": [0.0] * n_periods,
                "lmp": [0.0] * n_periods,
                # MEC = marginal energy component ($/MWh), MCC = marginal
                # congestion, MLC = marginal loss. LMP ≡ MEC + MCC + MLC.
                "mec": [0.0] * n_periods,
                "mcc": [0.0] * n_periods,
                "mlc": [0.0] * n_periods,
            })
            entry["vm"][t] = _safe_float(br.get("voltage_pu"))
            entry["va"][t] = _safe_float(br.get("angle_rad"))
            entry["p_inj"][t] = _safe_float(br.get("net_injection_mw"))
            entry["q_inj"][t] = _safe_float(br.get("net_reactive_injection_mvar"))
            entry["p_wd"][t] = _safe_float(br.get("withdrawals_mw"))
            entry["q_wd"][t] = _safe_float(br.get("withdrawals_mvar"))
            entry["lmp"][t] = _safe_float(br.get("lmp"))
            entry["mec"][t] = _safe_float(br.get("mec"))
            entry["mcc"][t] = _safe_float(br.get("mcc"))
            entry["mlc"][t] = _safe_float(br.get("mlc"))
    return out


def _compute_dc_branch_flows(
    problem_raw: dict,
    dc_result: dict | None,
    n_periods: int,
) -> dict[str, list[float]]:
    """Compute DC branch flows from bus angles: P_ij = (theta_i - theta_j) / x_ij."""
    if dc_result is None:
        return {}
    net = problem_raw["network"]
    base_mva = net["general"]["base_norm_mva"]
    buses = net["bus"]
    bus_idx = {b["uid"]: i for i, b in enumerate(buses)}
    bus_num_to_idx = {i + 1: i for i in range(len(buses))}

    flows: dict[str, list[float]] = {}
    for t, period in enumerate(dc_result.get("periods", [])):
        bus_angle = [0.0] * len(buses)
        for br in period.get("bus_results", []):
            bi = bus_num_to_idx.get(br.get("bus_number"), -1)
            if bi >= 0:
                bus_angle[bi] = br.get("angle_rad", 0.0)

        for acl in net.get("ac_line", []):
            uid = acl["uid"]
            fi = bus_idx.get(acl["fr_bus"], -1)
            ti = bus_idx.get(acl["to_bus"], -1)
            if fi < 0 or ti < 0:
                continue
            x = acl.get("x", 1e6)
            if abs(x) < 1e-14:
                continue
            p_pu = (bus_angle[fi] - bus_angle[ti]) / x
            entry = flows.setdefault(uid, [0.0] * n_periods)
            entry[t] = round(abs(p_pu) * base_mva, 2)

        for xfr in net.get("two_winding_transformer", []):
            uid = xfr["uid"]
            fi = bus_idx.get(xfr["fr_bus"], -1)
            ti = bus_idx.get(xfr["to_bus"], -1)
            if fi < 0 or ti < 0:
                continue
            x = xfr.get("x", 1e6)
            tap = xfr.get("initial_status", {}).get("tm", 1.0) or 1.0
            if abs(x) < 1e-14:
                continue
            p_pu = (bus_angle[fi] - bus_angle[ti]) / x
            entry = flows.setdefault(uid, [0.0] * n_periods)
            entry[t] = round(abs(p_pu) * base_mva, 2)

    return flows


def _extract_dc_branch_violations(
    dc_result: dict | None,
    n_periods: int,
) -> dict[str, list[dict]]:
    """Extract per-branch, per-period slack/penalty from DC constraint_results.

    Returns {branch_uid: [{slack_mw, penalty_cost}, ...per period]}.
    The constraint_id format is 'branch:from:to:circuit:direction' where
    circuit matches the numeric suffix of the ac_line uid (acl_XXX -> XXX).
    """
    if dc_result is None:
        return {}
    out: dict[str, list[dict]] = {}
    for t, period in enumerate(dc_result.get("periods", [])):
        for cr in period.get("constraint_results", []):
            if cr.get("kind") != "branch_thermal":
                continue
            cid = cr.get("constraint_id", "")
            parts = cid.split(":")
            if len(parts) >= 4:
                circuit = parts[3]
                branch_uid = f"acl_{circuit}"
            else:
                continue
            entry = out.setdefault(branch_uid, [{"slack_mw": 0.0, "penalty": 0.0} for _ in range(n_periods)])
            if t < len(entry):
                entry[t]["slack_mw"] += abs(cr.get("slack_mw", 0.0))
                entry[t]["penalty"] += abs(cr.get("penalty_cost", 0.0))
    return out


def _extract_solution_bus_series(
    solution: dict,
    n_periods: int,
) -> dict[str, dict[str, list]]:
    """Extract V/theta from a GO C3 solution.json."""
    out: dict[str, dict[str, list]] = {}
    for b in solution.get("time_series_output", {}).get("bus", []):
        uid = b["uid"]
        out[uid] = {
            "vm": [_safe_float(v) for v in b.get("vm", [0.0] * n_periods)],
            "va": [_safe_float(v) for v in b.get("va", [0.0] * n_periods)],
        }
    return out


def _extract_reserve_system(
    dispatch_result: dict | None,
    n_periods: int,
) -> list[dict]:
    """Extract per-period zonal reserve results from dispatch result."""
    if dispatch_result is None:
        return []
    out = []
    for t, period in enumerate(dispatch_result.get("periods", [])):
        period_reserves = []
        for rr in period.get("reserve_results", []):
            period_reserves.append({
                "product_id": rr.get("product_id", ""),
                "zone_id": rr.get("zone_id", 0),
                "requirement_mw": _safe_float(rr.get("requirement_mw")),
                "provided_mw": _safe_float(rr.get("provided_mw")),
                "shortfall_mw": _safe_float(rr.get("shortfall_mw")),
                "clearing_price": _safe_float(rr.get("clearing_price")),
            })
        out.append(period_reserves)
    return out


def _compute_solution_bus_injections(
    problem_raw: dict,
    solution: dict,
    n_periods: int,
) -> dict[str, dict[str, list]]:
    """Compute per-bus P/Q injections from a GO C3 solution's device dispatch."""
    net = problem_raw["network"]
    base_mva = net["general"]["base_norm_mva"]
    sdd_info = {d["uid"]: d for d in net["simple_dispatchable_device"]}
    sol_sdd = {d["uid"]: d for d in solution.get("time_series_output", {}).get("simple_dispatchable_device", [])}
    sol_dc = {d["uid"]: d for d in solution.get("time_series_output", {}).get("dc_line", [])}

    out: dict[str, dict[str, list]] = {}
    for b in net["bus"]:
        out[b["uid"]] = {"p_inj": [0.0] * n_periods, "q_inj": [0.0] * n_periods}

    for uid, sd in sol_sdd.items():
        info = sdd_info.get(uid)
        if info is None:
            continue
        bus = info["bus"]
        dtype = info["device_type"]
        sign = 1.0 if dtype == "producer" else -1.0
        entry = out.get(bus)
        if entry is None:
            continue
        for t in range(min(n_periods, len(sd.get("p_on", [])))):
            on = sd.get("on_status", [0])[t] if t < len(sd.get("on_status", [])) else 0
            entry["p_inj"][t] += sign * _safe_float(sd["p_on"][t]) * on * base_mva
            q_val = sd.get("q", [0.0])[t] if t < len(sd.get("q", [])) else 0.0
            entry["q_inj"][t] += sign * _safe_float(q_val) * on * base_mva

    for dc_info in net.get("dc_line", []):
        dc_uid = dc_info["uid"]
        ds = sol_dc.get(dc_uid, {})
        for t in range(n_periods):
            pdc_fr = _safe_float(ds.get("pdc_fr", [0.0])[t] if t < len(ds.get("pdc_fr", [])) else 0.0) * base_mva
            fr_entry = out.get(dc_info["fr_bus"])
            to_entry = out.get(dc_info["to_bus"])
            if fr_entry:
                fr_entry["p_inj"][t] -= pdc_fr
            if to_entry:
                to_entry["p_inj"][t] += pdc_fr

    return out


def _extract_solution_device_series(
    solution: dict,
    base_mva: float,
    n_periods: int,
) -> dict[str, dict[str, list]]:
    """Extract P/Q/on from a GO C3 solution.json (per-unit -> MW/Mvar)."""
    out: dict[str, dict[str, list]] = {}
    for sd in solution.get("time_series_output", {}).get("simple_dispatchable_device", []):
        uid = sd["uid"]
        p_on = [_safe_float(v) * base_mva for v in sd.get("p_on", [0.0] * n_periods)]
        q = [_safe_float(v) * base_mva for v in sd.get("q", [0.0] * n_periods)]
        on = [int(bool(v)) for v in sd.get("on_status", [0] * n_periods)]
        out[uid] = {"p": p_on, "q": q, "on": on}
    return out


def _extract_solution_hvdc_series(
    solution: dict,
    base_mva: float,
    n_periods: int,
) -> dict[str, dict[str, list]]:
    out: dict[str, dict[str, list]] = {}
    for dc in solution.get("time_series_output", {}).get("dc_line", []):
        uid = dc["uid"]
        out[uid] = {
            "p": [_safe_float(v) * base_mva for v in dc.get("pdc_fr", [0.0] * n_periods)],
            "q_fr": [_safe_float(v) * base_mva for v in dc.get("qdc_fr", [0.0] * n_periods)],
            "q_to": [_safe_float(v) * base_mva for v in dc.get("qdc_to", [0.0] * n_periods)],
        }
    return out


def _compute_branch_flows(
    problem_raw: dict,
    solution: dict,
    n_periods: int,
) -> list[dict]:
    """Compute per-period branch flows from solution V/theta using pi-model."""
    net = problem_raw["network"]
    base_mva = net["general"]["base_norm_mva"]
    buses = net["bus"]
    bus_idx = {b["uid"]: i for i, b in enumerate(buses)}
    sol_bus = {b["uid"]: b for b in solution["time_series_output"]["bus"]}
    sol_acl = {a["uid"]: a for a in solution["time_series_output"].get("ac_line", [])}
    sol_xfr = {x["uid"]: x for x in solution["time_series_output"].get("two_winding_transformer", [])}

    acl_params = _build_acl_params(net, bus_idx)
    xfr_params = _build_xfr_params(net, bus_idx)

    branches: list[dict] = []
    for ap in acl_params:
        uid = ap["uid"]
        acl_data = next((a for a in net["ac_line"] if a["uid"] == uid), {})
        flows = []
        for t in range(n_periods):
            on_list = sol_acl.get(uid, {}).get("on_status", [1] * n_periods)
            on = on_list[t] if t < len(on_list) else 1
            if not on:
                flows.append(0.0)
                continue
            vm = [sol_bus[b["uid"]]["vm"][t] for b in buses]
            va = [sol_bus[b["uid"]]["va"][t] for b in buses]
            pf, qf, pt, qt = _branch_flow_pi(ap, vm, va)
            sf = math.sqrt(pf * pf + qf * qf) * base_mva
            st = math.sqrt(pt * pt + qt * qt) * base_mva
            flows.append(round(max(sf, st), 2))
        branches.append({
            "uid": uid, "type": "ac_line",
            "fr_bus": acl_data.get("fr_bus", ""),
            "to_bus": acl_data.get("to_bus", ""),
            "limit_mva": round(ap["s_max"] * base_mva, 2),
            "flow_mva": flows,
        })
    for xp in xfr_params:
        uid = xp["uid"]
        xfr_data = next((x for x in net.get("two_winding_transformer", []) if x["uid"] == uid), {})
        flows = []
        for t in range(n_periods):
            on_list = sol_xfr.get(uid, {}).get("on_status", [1] * n_periods)
            on = on_list[t] if t < len(on_list) else 1
            if not on:
                flows.append(0.0)
                continue
            vm = [sol_bus[b["uid"]]["vm"][t] for b in buses]
            va = [sol_bus[b["uid"]]["va"][t] for b in buses]
            xs = sol_xfr.get(uid, {})
            tau = xs.get("tm", [xp["tau0"]])[t] if t < len(xs.get("tm", [xp["tau0"]])) else xp["tau0"]
            phi = xs.get("ta", [xp["phi0"]])[t] if t < len(xs.get("ta", [xp["phi0"]])) else xp["phi0"]
            pf, qf, pt, qt = _xfr_flow_pi(xp, vm, va, tau, phi)
            sf = math.sqrt(pf * pf + qf * qf) * base_mva
            st = math.sqrt(pt * pt + qt * qt) * base_mva
            flows.append(round(max(sf, st), 2))
        branches.append({
            "uid": uid, "type": "transformer",
            "fr_bus": xfr_data.get("fr_bus", ""),
            "to_bus": xfr_data.get("to_bus", ""),
            "limit_mva": round(xp["s_max"] * base_mva, 2),
            "flow_mva": flows,
        })
    return branches


def _build_unsolved_case_data(
    scenario: ScenarioRecord,
    cache_root: Path,
    switching_mode: str = "SW0",
) -> dict[str, Any] | None:
    """Build minimal case data for a scenario that Surge hasn't solved yet.

    Includes problem info, winner/leaderboard data, and device bounds so
    the dashboard can show reference data and the competitive landscape.
    """
    try:
        problem_raw = json.loads(scenario.problem_path.read_text(encoding="utf-8"))
    except Exception:
        return None
    base_mva = problem_raw["network"]["general"]["base_norm_mva"]
    net = problem_raw["network"]
    n_periods = len(problem_raw.get("time_series_input", {}).get("simple_dispatchable_device", [{}])[0].get("p_ub", [0]))
    if n_periods == 0:
        n_periods = 1

    n_producers = sum(1 for d in net["simple_dispatchable_device"] if d["device_type"] == "producer")
    n_consumers = sum(1 for d in net["simple_dispatchable_device"] if d["device_type"] == "consumer")

    # Winner / leaderboard — try requested SW mode, fall back to other if empty
    leaderboard_entries: list[dict] = []
    winner_z: dict[str, Any] = {}
    leaderboard_source_sw: str = switching_mode
    other_sw = "SW1" if switching_mode.upper() == "SW0" else "SW0"
    try:
        from benchmarks.go_c3.references import load_scenario_leaderboard, ensure_reference_submission, select_reference_entries
        _, entries = load_scenario_leaderboard(
            scenario.dataset_key, scenario.division, scenario.network_model, scenario.scenario_id,
        )
        sw_entries = [e for e in entries if e.switching_mode == switching_mode]
        if not sw_entries:
            sw_entries = [e for e in entries if e.switching_mode == other_sw]
            if sw_entries:
                leaderboard_source_sw = other_sw
        for entry in sw_entries:
            leaderboard_entries.append({
                "rank": entry.rank, "team": entry.team,
                "objective": entry.objective, "runtime_seconds": entry.runtime_seconds,
                "switching_mode": entry.switching_mode,
            })
        top_entries = select_reference_entries(sw_entries, include_benchmark=False, top_k=1)
        if top_entries:
            ref = ensure_reference_submission(top_entries[0], cache_root=cache_root)
            if ref.archived_summary is not None:
                winner_z = _extract_z_breakdown(ref.archived_summary.get("evaluation", {}))
    except Exception:
        pass

    return {
        "solved": False,
        "leaderboard_source_sw": leaderboard_source_sw if leaderboard_entries and leaderboard_source_sw != switching_mode else None,
        "case_info": {
            "dataset": scenario.dataset_key,
            "division": scenario.division,
            "network_model": scenario.network_model,
            "scenario_id": scenario.scenario_id,
            "base_mva": base_mva,
            "n_buses": len(net["bus"]),
            "n_ac_lines": len(net.get("ac_line", [])),
            "n_transformers": len(net.get("two_winding_transformer", [])),
            "n_dc_lines": len(net.get("dc_line", [])),
            "n_producers": n_producers,
            "n_consumers": n_consumers,
            "n_shunts": len(net.get("shunt", [])),
            "n_periods": n_periods,
        },
        "dc_summary": {},
        "dc_violations": {},
        "ac_summary": {},
        "policy": {},
        "run_report": {},
        "violation_summary": {},
        "violation_periods": [],
        "leaderboard": leaderboard_entries,
        "our_validation": {},
        "our_z": {},
        "winner_z": winner_z,
        "reserve_system": {},
        "contingencies": [],
        "periods": {"count": n_periods, "devices": [], "hvdc": [], "branches": [], "buses": []},
        "solve_log": "Not solved yet",
    }


def _build_case_data(
    scenario: ScenarioRecord,
    cache_root: Path,
    switching_mode: str = "SW0",
) -> dict[str, Any] | None:
    from markets.go_c3 import GoC3Policy
    _sw_policy = GoC3Policy(allow_branch_switching=(switching_mode.upper() == "SW1"))
    workdir = baseline_output_dir(cache_root, scenario, policy=_sw_policy)
    run_report = _load_json(workdir / "run-report.json")
    if run_report is None or run_report.get("status") != "ok":
        return None

    solution = _load_json(workdir / "solution.json")
    if solution is None:
        return None

    problem_raw = json.loads(scenario.problem_path.read_text(encoding="utf-8"))
    base_mva = problem_raw["network"]["general"]["base_norm_mva"]
    n_periods = len(solution["time_series_output"]["bus"][0]["vm"])

    # Violation report
    violation_report = _load_json(workdir / "violation-report.json")
    if violation_report is None:
        violation_report = compute_solution_violations(problem_raw, solution)

    # DC and AC dispatch results. The native solve path (solve_baseline_scenario_native)
    # emits a single workflow-result.json with both stages inside instead of the
    # separate dc-dispatch-result.json / dispatch-result.json files the Python
    # orchestration writes. Load whichever is on disk.
    dc_result = _load_json(workdir / "dc-dispatch-result.json")
    ac_result = _load_json(workdir / "dispatch-result.json")
    if dc_result is None or ac_result is None:
        workflow = _load_json(workdir / "workflow-result.json")
        if workflow is not None:
            for stage in workflow.get("stages", []) or []:
                sid = stage.get("stage_id")
                sol = stage.get("solution")
                if sol is None:
                    continue
                if dc_result is None and sid == "scuc":
                    dc_result = sol
                elif ac_result is None and sid == "sced":
                    ac_result = sol

    dc_violations = _extract_dc_violations(dc_result)
    dc_branch_flows = _compute_dc_branch_flows(problem_raw, dc_result, n_periods)
    dc_branch_viols = _extract_dc_branch_violations(dc_result, n_periods)

    # Bus number -> UID mapping
    bus_number_to_uid = {i + 1: b["uid"] for i, b in enumerate(problem_raw["network"]["bus"])}

    dc_devices = _extract_device_series(dc_result, n_periods)
    ac_devices = _extract_device_series(ac_result, n_periods)
    dc_hvdc = _extract_hvdc_series(dc_result, n_periods)
    ac_hvdc_raw = _extract_hvdc_series(ac_result, n_periods)
    ac_sol_hvdc = _extract_solution_hvdc_series(solution, base_mva, n_periods)

    # Bus data
    dc_buses = _extract_bus_series(dc_result, n_periods, bus_number_to_uid)
    ac_buses = _extract_bus_series(ac_result, n_periods, bus_number_to_uid)

    # Winner solution (best-effort)
    winner_devices: dict[str, dict[str, list]] = {}
    winner_hvdc: dict[str, dict[str, list]] = {}
    winner_branches: list[dict] = []
    winner_buses: dict[str, dict[str, list]] = {}
    winner_bus_inj: dict[str, dict[str, list]] = {}
    winner_z: dict[str, Any] = {}
    leaderboard_entries: list[dict] = []
    leaderboard_source_sw: str = switching_mode
    other_sw = "SW1" if switching_mode.upper() == "SW0" else "SW0"
    try:
        from benchmarks.go_c3.references import load_scenario_leaderboard, ensure_reference_submission, select_reference_entries
        _, entries = load_scenario_leaderboard(
            scenario.dataset_key, scenario.division, scenario.network_model, scenario.scenario_id,
        )
        sw_entries = [e for e in entries if e.switching_mode == switching_mode]
        # Fall back to the other switching mode when the requested one has no
        # competition data (e.g. 73-bus SW0 scenarios have no SW0 leaderboard
        # entries because every competitor ran SW1). The yellow banner on the
        # Scores tab signals that the reference is from the other mode.
        if not sw_entries:
            sw_entries = [e for e in entries if e.switching_mode == other_sw]
            if sw_entries:
                leaderboard_source_sw = other_sw
        for entry in sw_entries:
            leaderboard_entries.append({
                "rank": entry.rank,
                "team": entry.team,
                "objective": entry.objective,
                "runtime_seconds": entry.runtime_seconds,
                "switching_mode": entry.switching_mode,
            })
        top_entries = select_reference_entries(sw_entries, include_benchmark=False, top_k=1)
        if top_entries:
            ref = ensure_reference_submission(top_entries[0], cache_root=cache_root)
            # Always load winner trajectory, even when it came from the other
            # switching mode. Empirically most SW1 winners never actually
            # toggled a branch (≈96% of rank-1 SW1 submissions had constant
            # on_status across all periods), so overlaying them on an SW0
            # case is meaningful rather than misleading. The few winners that
            # did switch will show up with time-varying on_status and can be
            # interpreted accordingly.
            winner_sol = json.loads(ref.solution_path.read_text(encoding="utf-8"))
            winner_devices = _extract_solution_device_series(winner_sol, base_mva, n_periods)
            winner_hvdc = _extract_solution_hvdc_series(winner_sol, base_mva, n_periods)
            winner_branches = _compute_branch_flows(problem_raw, winner_sol, n_periods)
            winner_buses = _extract_solution_bus_series(winner_sol, n_periods)
            winner_bus_inj = _compute_solution_bus_injections(problem_raw, winner_sol, n_periods)
            # Winner's validator summary from archived submission — safe to
            # copy across SW modes since the z-breakdown is the winner's own
            # score for its own solution and we only use it for comparison.
            if ref.archived_summary is not None:
                winner_eval = ref.archived_summary.get("evaluation", {})
                winner_z = _extract_z_breakdown(winner_eval)
    except Exception:
        pass

    # Assemble devices
    sdd_info = {d["uid"]: d for d in problem_raw["network"]["simple_dispatchable_device"]}
    sdd_ts = {d["uid"]: d for d in problem_raw.get("time_series_input", {}).get("simple_dispatchable_device", [])}
    all_uids = sorted(set(sdd_info.keys()))
    devices = []
    for uid in all_uids:
        info = sdd_info[uid]
        dtype = info["device_type"]
        bus = info["bus"]
        dc = dc_devices.get(uid, {})
        ac = ac_devices.get(uid, {})
        win = winner_devices.get(uid, {})
        ts = sdd_ts.get(uid, {})
        p_lb = [_safe_float(v) * base_mva for v in ts.get("p_lb", [0.0] * n_periods)]
        p_ub = [_safe_float(v) * base_mva for v in ts.get("p_ub", [0.0] * n_periods)]
        devices.append({
            "uid": uid, "bus": bus, "type": dtype,
            "p_lb": p_lb, "p_ub": p_ub,
            "dispatchable": any(ub - lb > 1e-6 for lb, ub in zip(p_lb, p_ub)),
            "dc_p": dc.get("p", [0.0] * n_periods),
            "ac_p": ac.get("p", [0.0] * n_periods),
            "ac_q": ac.get("q", [0.0] * n_periods),
            "ac_on": ac.get("on", [0] * n_periods),
            "dc_lmp": dc.get("lmp", [0.0] * n_periods),
            "ac_lmp": ac.get("lmp", [0.0] * n_periods),
            "dc_ecost": dc.get("energy_cost", [0.0] * n_periods),
            "ac_ecost": ac.get("energy_cost", [0.0] * n_periods),
            "mc": _compute_marginal_cost(dtype, ts, ac.get("p", [0.0] * n_periods), ac.get("energy_cost", [0.0] * n_periods), base_mva, n_periods),
            "cost_blocks": ts.get("cost", []),
            "winner_p": win.get("p", []),
            "winner_q": win.get("q", []),
            "winner_on": win.get("on", []),
            "reserves": {k[4:]: v for k, v in dc.items() if k.startswith("res_")},
        })

    # HVDC
    dc_lines = problem_raw["network"].get("dc_line", [])
    hvdc_list = []
    for dcl in dc_lines:
        uid = dcl["uid"]
        dc_h = dc_hvdc.get(uid, {})
        ac_h = ac_sol_hvdc.get(uid, {})
        win_h = winner_hvdc.get(uid, {})
        hvdc_list.append({
            "uid": uid,
            "dc_p": dc_h.get("p", [0.0] * n_periods),
            "ac_p": ac_h.get("p", [0.0] * n_periods),
            "ac_q_fr": ac_h.get("q_fr", [0.0] * n_periods),
            "ac_q_to": ac_h.get("q_to", [0.0] * n_periods),
            "winner_p": win_h.get("p", []),
            "winner_q_fr": win_h.get("q_fr", []),
            "winner_q_to": win_h.get("q_to", []),
        })

    # Branch flows — ours and winner's
    branches = _compute_branch_flows(problem_raw, solution, n_periods)
    winner_branch_lookup = {b["uid"]: b.get("flow_mva", []) for b in winner_branches}
    for b in branches:
        b["winner_flow_mva"] = winner_branch_lookup.get(b["uid"], [])
        b["dc_flow_mva"] = dc_branch_flows.get(b["uid"], [])
        viols = dc_branch_viols.get(b["uid"], [])
        b["dc_slack_mw"] = [v.get("slack_mw", 0.0) for v in viols] if viols else []
        b["dc_penalty"] = [v.get("penalty", 0.0) for v in viols] if viols else []

    # Our validation summary (violation-report based)
    our_validation = run_report.get("violation_summary", {})

    # Our full validator z-breakdown (from validator-baseline if available)
    our_z: dict[str, Any] = {}
    _sw_dir = "sw1" if switching_mode.upper() == "SW1" else "sw0"
    val_summary_path = cache_root / "runs" / "validator-baseline" / scenario.dataset_key / scenario.division / _sw_dir / f"scenario_{scenario.scenario_id:03d}" / "summary.json"
    if val_summary_path.exists():
        try:
            val_data = json.loads(val_summary_path.read_text(encoding="utf-8"))
            our_z = _extract_z_breakdown(val_data.get("evaluation", {}))
        except Exception:
            pass

    # Case info
    net = problem_raw["network"]
    n_producers = sum(1 for d in net["simple_dispatchable_device"] if d["device_type"] == "producer")
    n_consumers = sum(1 for d in net["simple_dispatchable_device"] if d["device_type"] == "consumer")
    case_info = {
        "dataset": scenario.dataset_key,
        "division": scenario.division,
        "network_model": scenario.network_model,
        "scenario_id": scenario.scenario_id,
        "base_mva": base_mva,
        "n_buses": len(net["bus"]),
        "n_ac_lines": len(net.get("ac_line", [])),
        "n_transformers": len(net.get("two_winding_transformer", [])),
        "n_dc_lines": len(net.get("dc_line", [])),
        "n_producers": n_producers,
        "n_consumers": n_consumers,
        "n_shunts": len(net.get("shunt", [])),
        "n_periods": n_periods,
    }

    # DC solve stats
    dc_summary = {}
    if dc_result is not None:
        ds = dc_result.get("summary", {})
        # DC solve time = total solve minus AC solve time
        ac_time = (run_report.get("ac_reconcile") or {}).get("solve_time_secs", 0.0) or 0.0
        total_time = run_report.get("solve_seconds", 0.0) or 0.0
        dc_solve_time = total_time - ac_time
        dc_summary = {
            "solve_time_secs": dc_solve_time,
            "total_cost": ds.get("total_cost"),
            "energy_cost": ds.get("total_energy_cost"),
            "no_load_cost": ds.get("total_no_load_cost"),
            "startup_cost": ds.get("total_startup_cost"),
            "reserve_cost": ds.get("total_reserve_cost"),
        }

    # AC solve stats
    ac_summary = {}
    ac_rec = run_report.get("ac_reconcile")
    if ac_rec is not None:
        ac_summary = {
            "mode": ac_rec.get("mode"),
            "nlp_solver": ac_rec.get("nlp_solver"),
            "solve_time_secs": ac_rec.get("solve_time_secs"),
            "total_cost": ac_rec.get("dispatch_total_cost"),
            "energy_cost": ac_rec.get("dispatch_total_energy_cost"),
            "no_load_cost": ac_rec.get("dispatch_total_no_load_cost"),
            "startup_cost": ac_rec.get("dispatch_total_startup_cost"),
            "thermal_slack_count": ac_rec.get("branch_thermal_slack_count"),
            "max_thermal_slack_mva": ac_rec.get("max_branch_thermal_slack_mva"),
            "commitment_refinement_iterations": ac_rec.get("commitment_refinement_iterations"),
        }

    policy = run_report.get("policy", {})

    # Solve log
    solve_log_path = workdir / "solve.log"
    solve_log = solve_log_path.read_text(encoding="utf-8") if solve_log_path.exists() else "Log not found"

    return {
        "solved": True,
        "leaderboard_source_sw": leaderboard_source_sw if leaderboard_entries and leaderboard_source_sw != switching_mode else None,
        "solve_log": solve_log,
        "case_info": case_info,
        "dc_summary": dc_summary,
        "dc_violations": dc_violations.get("summary", {}),
        "ac_summary": ac_summary,
        "policy": {
            "formulation": policy.get("formulation"),
            "lp_solver": policy.get("lp_solver"),
            "commitment_mode": policy.get("commitment_mode"),
            "ac_reconcile_mode": policy.get("ac_reconcile_mode"),
            "nlp_solver": policy.get("nlp_solver") or policy.get("ac_nlp_solver"),
            "allow_branch_switching": policy.get("allow_branch_switching", False),
        },
        "run_report": {
            "status": run_report.get("status"),
            "solve_seconds": run_report.get("solve_seconds"),
            "dispatch_summary": run_report.get("dispatch_summary", {}),
            "ac_reconcile": run_report.get("ac_reconcile"),
            "policy": run_report.get("policy", {}),
        },
        "violation_summary": violation_report.get("summary", {}),
        "violation_periods": violation_report.get("periods", []),
        "objective": {
            "dc": _build_objective_breakdown(dc_result),
            "ac": _build_objective_breakdown(ac_result),
        },
        "grid_layout": _compute_bus_layout(problem_raw),
        "grid_assets": _compute_grid_assets(problem_raw),
        "shunts": _compute_shunt_catalog(problem_raw, solution),
        "xfmrs": _compute_xfmr_catalog(problem_raw, solution),
        "leaderboard": leaderboard_entries,
        "our_validation": our_validation,
        "our_z": our_z,
        "winner_z": winner_z,
        "reserve_system": _extract_reserve_system(dc_result, n_periods),
        "contingencies": [
            {"uid": c["uid"], "components": c.get("components", [])}
            for c in problem_raw.get("reliability", problem_raw.get("network", {}).get("reliability", {})).get("contingency", [])
        ],
        "periods": {
            "count": n_periods,
            "devices": devices,
            "hvdc": hvdc_list,
            "branches": branches,
            "buses": [
                {
                    "uid": b["uid"],
                    "ac_vm": ac_buses.get(b["uid"], {}).get("vm", []),
                    "ac_va": ac_buses.get(b["uid"], {}).get("va", []),
                    "ac_p_inj": ac_buses.get(b["uid"], {}).get("p_inj", []),
                    "ac_q_inj": ac_buses.get(b["uid"], {}).get("q_inj", []),
                    "ac_p_wd": ac_buses.get(b["uid"], {}).get("p_wd", []),
                    "ac_lmp": ac_buses.get(b["uid"], {}).get("lmp", []),
                    "ac_mec": ac_buses.get(b["uid"], {}).get("mec", []),
                    "ac_mcc": ac_buses.get(b["uid"], {}).get("mcc", []),
                    "ac_mlc": ac_buses.get(b["uid"], {}).get("mlc", []),
                    "dc_lmp": dc_buses.get(b["uid"], {}).get("lmp", []),
                    "dc_mec": dc_buses.get(b["uid"], {}).get("mec", []),
                    "dc_mcc": dc_buses.get(b["uid"], {}).get("mcc", []),
                    "dc_mlc": dc_buses.get(b["uid"], {}).get("mlc", []),
                    "dc_p_inj": dc_buses.get(b["uid"], {}).get("p_inj", []),
                    "dc_p_wd": dc_buses.get(b["uid"], {}).get("p_wd", []),
                    "winner_vm": winner_buses.get(b["uid"], {}).get("vm", []),
                    "winner_va": winner_buses.get(b["uid"], {}).get("va", []),
                    "winner_p_inj": winner_bus_inj.get(b["uid"], {}).get("p_inj", []),
                    "winner_q_inj": winner_bus_inj.get(b["uid"], {}).get("q_inj", []),
                }
                for b in problem_raw["network"]["bus"]
            ],
        },
    }

