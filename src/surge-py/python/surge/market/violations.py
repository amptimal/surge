# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Post-solve violation assessment using AC pi-model power flow.

Evaluates a dispatch solution against the standard AC pi-model power
flow equations and reports bus P/Q balance, branch thermal, and reserve
shortfall violations with quantities and penalty costs.

Two assessment paths are available:

- ``assess_dispatch_violations_native()``: Rust-native assessment using the
  Surge network model and dispatch result.  Fast and exact.
- ``assess_violations()``: Python assessment from raw problem/solution dicts
  (matching the GO C3 JSON format).  Supports reserve shortfall computation.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Any


# ---------------------------------------------------------------------------
# Violation report types
# ---------------------------------------------------------------------------

@dataclass
class BusViolation:
    bus_uid: str
    mismatch_pu: float
    period: int


@dataclass
class ThermalViolation:
    branch_uid: str
    flow_mva: float
    limit_mva: float
    overload_mva: float
    period: int


@dataclass
class ReserveShortfall:
    product_id: str
    zone: str
    shortfall_mw: float
    penalty: float
    period: int


@dataclass
class ViolationReport:
    """Summary of post-solve violations across all periods."""

    bus_p_total_mismatch_mw: float = 0.0
    bus_p_total_penalty: float = 0.0
    bus_q_total_mismatch_mvar: float = 0.0
    bus_q_total_penalty: float = 0.0
    thermal_total_overload_mva: float = 0.0
    thermal_total_penalty: float = 0.0
    reserve_total_shortfall_mw: float = 0.0
    reserve_total_penalty: float = 0.0
    periods: list[dict[str, Any]] = field(default_factory=list)

    @property
    def total_penalty_cost(self) -> float:
        return (
            self.bus_p_total_penalty
            + self.bus_q_total_penalty
            + self.thermal_total_penalty
            + self.reserve_total_penalty
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "summary": {
                "bus_p_balance": {
                    "total_mismatch_mw": round(self.bus_p_total_mismatch_mw, 4),
                    "total_penalty_cost": round(self.bus_p_total_penalty, 2),
                },
                "bus_q_balance": {
                    "total_mismatch_mvar": round(self.bus_q_total_mismatch_mvar, 4),
                    "total_penalty_cost": round(self.bus_q_total_penalty, 2),
                },
                "branch_thermal": {
                    "total_overload_mva": round(self.thermal_total_overload_mva, 4),
                    "total_penalty_cost": round(self.thermal_total_penalty, 2),
                },
                "reserve": {
                    "total_shortfall_mw": round(self.reserve_total_shortfall_mw, 4),
                    "total_penalty_cost": round(self.reserve_total_penalty, 2),
                },
                "total_penalty_cost": round(self.total_penalty_cost, 2),
            },
            "periods": self.periods,
        }


# ---------------------------------------------------------------------------
# Pi-model branch flow calculations
# ---------------------------------------------------------------------------

def _build_acl_params(
    ac_lines: list[dict[str, Any]],
    bus_idx: dict[str, int],
) -> list[dict[str, Any]]:
    params = []
    for acl in ac_lines:
        r = float(acl["r"])
        x = float(acl["x"])
        b_ch = float(acl.get("b", 0.0))
        z_sq = r * r + x * x
        if z_sq < 1e-14:
            continue
        g_sr = r / z_sq
        b_sr = -x / z_sq
        has_extra = acl.get("additional_shunt") == 1
        params.append({
            "uid": acl["uid"],
            "fi": bus_idx[acl["fr_bus"]],
            "ti": bus_idx[acl["to_bus"]],
            "g_sr": g_sr, "b_sr": b_sr, "b_ch": b_ch,
            "g_fr": float(acl.get("g_fr", 0.0)) if has_extra else 0.0,
            "b_fr": float(acl.get("b_fr", 0.0)) if has_extra else 0.0,
            "g_to": float(acl.get("g_to", 0.0)) if has_extra else 0.0,
            "b_to": float(acl.get("b_to", 0.0)) if has_extra else 0.0,
            "s_max": float(acl.get("mva_ub_nom", 1e6)),
        })
    return params


def _build_xfr_params(
    transformers: list[dict[str, Any]],
    bus_idx: dict[str, int],
) -> list[dict[str, Any]]:
    params = []
    for xfr in transformers:
        r = float(xfr["r"])
        x = float(xfr["x"])
        b_ch = float(xfr.get("b", 0.0))
        z_sq = r * r + x * x
        if z_sq < 1e-14:
            continue
        g_sr = r / z_sq
        b_sr = -x / z_sq
        has_extra = xfr.get("additional_shunt") == 1
        init = xfr.get("initial_status", {})
        params.append({
            "uid": xfr["uid"],
            "fi": bus_idx[xfr["fr_bus"]],
            "ti": bus_idx[xfr["to_bus"]],
            "g_sr": g_sr, "b_sr": b_sr, "b_ch": b_ch,
            "g_fr": float(xfr.get("g_fr", 0.0)) if has_extra else 0.0,
            "b_fr": float(xfr.get("b_fr", 0.0)) if has_extra else 0.0,
            "g_to": float(xfr.get("g_to", 0.0)) if has_extra else 0.0,
            "b_to": float(xfr.get("b_to", 0.0)) if has_extra else 0.0,
            "tau0": float(init.get("tm", 1.0) or 1.0),
            "phi0": float(init.get("ta", 0.0) or 0.0),
            "s_max": float(xfr.get("mva_ub_nom", 1e6)),
        })
    return params


def _branch_flow_pi(ap: dict, vm: list, va: list) -> tuple[float, float, float, float]:
    """Standard pi-model branch flow for AC lines (tap=1, shift=0)."""
    fi, ti = ap["fi"], ap["ti"]
    g_sr, b_sr, b_ch = ap["g_sr"], ap["b_sr"], ap["b_ch"]
    g_fr, b_fr, g_to, b_to = ap["g_fr"], ap["b_fr"], ap["g_to"], ap["b_to"]

    vf, vt = vm[fi], vm[ti]
    th = va[fi] - va[ti]
    cos_t, sin_t = math.cos(th), math.sin(th)
    vft = vf * vt

    g_ff = g_sr + g_fr
    b_ff = b_sr + b_fr + b_ch / 2.0
    g_tt = g_sr + g_to
    b_tt = b_sr + b_to + b_ch / 2.0

    pf = g_ff * vf * vf - g_sr * vft * cos_t - b_sr * vft * sin_t
    qf = -b_ff * vf * vf + b_sr * vft * cos_t - g_sr * vft * sin_t
    pt = g_tt * vt * vt - g_sr * vft * cos_t + b_sr * vft * sin_t
    qt = -b_tt * vt * vt + b_sr * vft * cos_t + g_sr * vft * sin_t

    return pf, qf, pt, qt


def _xfr_flow_pi(
    xp: dict, vm: list, va: list, tau: float, phi: float,
) -> tuple[float, float, float, float]:
    """Standard pi-model branch flow for transformers with tap and phase shift."""
    fi, ti = xp["fi"], xp["ti"]
    g_sr, b_sr, b_ch = xp["g_sr"], xp["b_sr"], xp["b_ch"]
    g_fr, b_fr, g_to, b_to = xp["g_fr"], xp["b_fr"], xp["g_to"], xp["b_to"]

    vf, vt = vm[fi], vm[ti]
    th = va[fi] - va[ti] - phi
    cos_t, sin_t = math.cos(th), math.sin(th)

    tau_sq = tau * tau
    vf2_tau2 = vf * vf / tau_sq
    vft_tau = vf * vt / tau

    g_ff = g_sr + g_fr
    b_ff = b_sr + b_fr + b_ch / 2.0
    g_tt = g_sr + g_to
    b_tt = b_sr + b_to + b_ch / 2.0

    pf = g_ff * vf2_tau2 - g_sr * vft_tau * cos_t - b_sr * vft_tau * sin_t
    qf = -b_ff * vf2_tau2 + b_sr * vft_tau * cos_t - g_sr * vft_tau * sin_t
    pt = g_tt * vt * vt - g_sr * vft_tau * cos_t + b_sr * vft_tau * sin_t
    qt = -b_tt * vt * vt + b_sr * vft_tau * cos_t + g_sr * vft_tau * sin_t

    return pf, qf, pt, qt


# ---------------------------------------------------------------------------
# Reserve penalty computation
# ---------------------------------------------------------------------------

_RESERVE_PRODUCT_VIO_COST_MAP = {
    "reg_up": "REG_UP_vio_cost",
    "reg_down": "REG_DOWN_vio_cost",
    "syn": "SYN_vio_cost",
    "nsyn": "NSYN_vio_cost",
    "ramp_up_on": "RAMPING_RESERVE_UP_vio_cost",
    "ramp_up_off": "RAMPING_RESERVE_UP_vio_cost",
    "ramp_down_on": "RAMPING_RESERVE_DOWN_vio_cost",
    "ramp_down_off": "RAMPING_RESERVE_DOWN_vio_cost",
}


def _build_transition_injections(
    problem: dict[str, Any],
    solution: dict[str, Any],
) -> dict[str, dict[str, list[float]]]:
    """Mirror the official GO validator's startup/shutdown MW accounting."""

    net = problem["network"]
    ts_input = problem.get("time_series_input", {})
    tso = solution["time_series_output"]
    durations = ts_input.get("general", {}).get("interval_duration", [])
    sd_ts_map = {
        d["uid"]: d
        for d in ts_input.get("simple_dispatchable_device", [])
    }
    sd_sol_map = {
        d["uid"]: d
        for d in tso.get("simple_dispatchable_device", [])
    }

    t_end: list[float] = []
    elapsed = 0.0
    for dt in durations:
        elapsed += float(dt)
        t_end.append(elapsed)

    def startup_points(
        p_min: list[float],
        p_ru_su: float,
        start_t: int,
    ) -> list[tuple[int, float]]:
        points: list[tuple[int, float]] = []
        p_start = float(p_min[start_t])
        t_new = start_t
        while t_new > 0:
            t_new -= 1
            p_new = p_start - p_ru_su * (t_end[start_t] - t_end[t_new])
            if p_new <= 1e-8:
                break
            points.append((t_new, p_new))
        return points

    def shutdown_points(
        p_min: list[float],
        p_0: float,
        p_rd_sd: float,
        stop_t: int,
        num_t: int,
    ) -> list[tuple[int, float]]:
        points: list[tuple[int, float]] = []
        p_start = p_0 if stop_t == 0 else float(p_min[stop_t - 1])
        t_new = stop_t
        t_start = 0.0 if stop_t == 0 else t_end[stop_t - 1]
        while t_new < num_t:
            p_new = p_start - p_rd_sd * (t_end[t_new] - t_start)
            if p_new <= 1e-8:
                break
            points.append((t_new, p_new))
            t_new += 1
        return points

    transition: dict[str, dict[str, list[float]]] = {}
    for sd in net.get("simple_dispatchable_device", []):
        uid = sd["uid"]
        sol = sd_sol_map.get(uid, {})
        on_status = [int(v) for v in sol.get("on_status", [])]
        num_t = len(on_status)
        p_su = [0.0] * num_t
        p_sd = [0.0] * num_t
        ts = sd_ts_map.get(uid, {})
        p_min = [float(v) for v in ts.get("p_lb", [0.0] * num_t)]
        initial_on = int(sd.get("initial_status", {}).get("on_status", 0) or 0)
        p_init = float(sd.get("initial_status", {}).get("p", 0.0) or 0.0)
        p_ru_su = float(sd.get("p_startup_ramp_ub", 0.0) or 0.0)
        p_rd_sd = float(sd.get("p_shutdown_ramp_ub", 0.0) or 0.0)

        prev = initial_on
        for t, on in enumerate(on_status):
            diff = on - prev
            if diff > 0:
                for tp, p in startup_points(p_min, p_ru_su, t):
                    p_su[tp] = p
            elif diff < 0:
                for tp, p in shutdown_points(p_min, p_init, p_rd_sd, t, num_t):
                    p_sd[tp] = p
            prev = on

        transition[uid] = {"p_su": p_su, "p_sd": p_sd}

    return transition


def _compute_reserve_penalties(
    problem: dict[str, Any],
    dc_dispatch_result: dict[str, Any] | None,
    durations: list,
    base_mva: float,
) -> tuple[list[dict], dict]:
    """Compute per-period reserve shortfall penalties from DC dispatch result."""
    if dc_dispatch_result is None:
        return [], {}

    net = problem["network"]
    zone_costs: dict[str, dict[str, float]] = {}
    for zone in net.get("active_zonal_reserve", []):
        uid = zone.get("uid", "")
        zone_costs[uid] = {k: float(v) for k, v in zone.items() if k.endswith("_vio_cost")}

    zone_id_to_uid: dict[int, str] = {}
    for i, zone in enumerate(net.get("active_zonal_reserve", [])):
        zone_id_to_uid[i] = zone.get("uid", f"zone_{i}")
        zone_id_to_uid[i + 1] = zone.get("uid", f"zone_{i}")

    per_period: list[dict] = []
    sum_shortfall_mw = 0.0
    sum_penalty = 0.0

    for t, period in enumerate(dc_dispatch_result.get("periods", [])):
        dt = float(durations[t]) if t < len(durations) else 1.0
        period_products: list[dict] = []
        period_penalty = 0.0

        for rr in period.get("reserve_results", []):
            product_id = rr.get("product_id", "")
            zone_id = rr.get("zone_id", 0)
            shortfall = float(rr.get("shortfall_mw", 0.0))
            if shortfall < 1e-6:
                continue
            zone_uid = zone_id_to_uid.get(zone_id, "")
            vio_cost_key = _RESERVE_PRODUCT_VIO_COST_MAP.get(product_id, "")
            vio_cost_per_pu = zone_costs.get(zone_uid, {}).get(vio_cost_key, 0.0)
            penalty = (shortfall / base_mva) * vio_cost_per_pu * dt
            period_products.append({
                "product_id": product_id,
                "zone": zone_uid,
                "shortfall_mw": round(shortfall, 4),
                "penalty": round(penalty, 2),
            })
            period_penalty += penalty
            sum_shortfall_mw += shortfall

        per_period.append({
            "products": period_products,
            "total_penalty": round(period_penalty, 2),
        })
        sum_penalty += period_penalty

    return per_period, {
        "total_shortfall_mw": round(sum_shortfall_mw, 4),
        "total_penalty": round(sum_penalty, 2),
    }


# ---------------------------------------------------------------------------
# Main violation assessment
# ---------------------------------------------------------------------------

def assess_violations(
    problem: dict[str, Any],
    solution: dict[str, Any],
    dc_dispatch_result: dict[str, Any] | None = None,
) -> ViolationReport:
    """Compute bus P/Q balance, branch thermal, and reserve violations.

    Uses the standard AC pi-model power flow equations (matching the
    GO C3 validator) to evaluate the solution against the network model.

    Args:
        problem: GO C3 problem dict (or any dict with ``network``,
            ``time_series_input``, and ``time_series_output`` structure).
        solution: Solution dict with ``time_series_output`` containing
            per-device and per-bus time series.
        dc_dispatch_result: Optional DC dispatch result dict for reserve
            shortfall computation.

    Returns:
        A ``ViolationReport`` with per-period and summary violations.
    """
    net = problem["network"]
    base_mva = net["general"]["base_norm_mva"]
    tso = solution["time_series_output"]
    viol_cost = net.get("violation_cost", {})
    c_p = float(viol_cost.get("p_bus_vio_cost", 0.0))
    c_q = float(viol_cost.get("q_bus_vio_cost", 0.0))
    c_s = float(viol_cost.get("s_vio_cost", 0.0))

    ts_input = problem.get("time_series_input", {})
    durations = ts_input.get("general", {}).get("interval_duration", [])

    buses = net["bus"]
    n_bus = len(buses)
    bus_idx = {b["uid"]: i for i, b in enumerate(buses)}

    sol_bus = {b["uid"]: b for b in tso["bus"]}
    n_periods = len(sol_bus[buses[0]["uid"]]["vm"])

    acl_params = _build_acl_params(net.get("ac_line", []), bus_idx)
    xfr_params = _build_xfr_params(net.get("two_winding_transformer", []), bus_idx)

    sol_acl = {a["uid"]: a for a in tso.get("ac_line", [])}
    sol_xfr = {x["uid"]: x for x in tso.get("two_winding_transformer", [])}
    sol_dc = {d["uid"]: d for d in tso.get("dc_line", [])}
    sol_sh = {s["uid"]: s for s in tso.get("shunt", [])}
    sol_sdd = {d["uid"]: d for d in tso["simple_dispatchable_device"]}

    sdd_info = {d["uid"]: d for d in net["simple_dispatchable_device"]}
    shunt_info = {s["uid"]: s for s in net.get("shunt", [])}
    transition = _build_transition_injections(problem, solution)

    report = ViolationReport()

    for t in range(n_periods):
        dt = float(durations[t]) if t < len(durations) else 1.0
        vm = [sol_bus[b["uid"]]["vm"][t] for b in buses]
        va = [sol_bus[b["uid"]]["va"][t] for b in buses]

        p_flow = [0.0] * n_bus
        q_flow = [0.0] * n_bus
        acl_s_violations: list[dict[str, Any]] = []
        xfr_s_violations: list[dict[str, Any]] = []

        # AC lines
        for ap in acl_params:
            on_series = sol_acl.get(ap["uid"], {}).get("on_status", [1])
            on = on_series[t] if t < len(on_series) else 1
            if not on:
                continue
            pf, qf, pt, qt = _branch_flow_pi(ap, vm, va)
            p_flow[ap["fi"]] += pf
            q_flow[ap["fi"]] += qf
            p_flow[ap["ti"]] += pt
            q_flow[ap["ti"]] += qt

            sf = math.sqrt(pf * pf + qf * qf)
            st = math.sqrt(pt * pt + qt * qt)
            overload = max(sf, st) - ap["s_max"]
            if overload > 1e-6:
                acl_s_violations.append({
                    "uid": ap["uid"],
                    "flow_mva": max(sf, st) * base_mva,
                    "limit_mva": ap["s_max"] * base_mva,
                    "overload_mva": overload * base_mva,
                })

        # Transformers
        for xp in xfr_params:
            on_series = sol_xfr.get(xp["uid"], {}).get("on_status", [1])
            on = on_series[t] if t < len(on_series) else 1
            if not on:
                continue
            xs = sol_xfr.get(xp["uid"], {})
            tau_series = xs.get("tm", [xp["tau0"]])
            phi_series = xs.get("ta", [xp["phi0"]])
            tau = tau_series[t] if t < len(tau_series) else xp["tau0"]
            phi = phi_series[t] if t < len(phi_series) else xp["phi0"]
            pf, qf, pt, qt = _xfr_flow_pi(xp, vm, va, tau, phi)
            p_flow[xp["fi"]] += pf
            q_flow[xp["fi"]] += qf
            p_flow[xp["ti"]] += pt
            q_flow[xp["ti"]] += qt

            sf = math.sqrt(pf * pf + qf * qf)
            st = math.sqrt(pt * pt + qt * qt)
            overload = max(sf, st) - xp["s_max"]
            if overload > 1e-6:
                xfr_s_violations.append({
                    "uid": xp["uid"],
                    "flow_mva": max(sf, st) * base_mva,
                    "limit_mva": xp["s_max"] * base_mva,
                    "overload_mva": overload * base_mva,
                })

        # Device injections
        p_dev = [0.0] * n_bus
        q_dev = [0.0] * n_bus

        for uid, sd in sol_sdd.items():
            info = sdd_info[uid]
            bi = bus_idx[info["bus"]]
            dtype = info["device_type"]
            p_on = sd["p_on"][t] if t < len(sd["p_on"]) else 0.0
            q_val = sd["q"][t] if t < len(sd["q"]) else 0.0
            sign = 1.0 if dtype == "producer" else -1.0
            p_su = transition.get(uid, {}).get("p_su", [])
            p_sd = transition.get(uid, {}).get("p_sd", [])
            p_total = (
                p_on
                + (p_su[t] if t < len(p_su) else 0.0)
                + (p_sd[t] if t < len(p_sd) else 0.0)
            )
            p_dev[bi] += sign * p_total
            q_dev[bi] += sign * q_val

        # HVDC
        for dc_info in net.get("dc_line", []):
            dc_uid = dc_info["uid"]
            ds = sol_dc.get(dc_uid, {})
            pdc_fr_s = ds.get("pdc_fr", [0.0])
            qdc_fr_s = ds.get("qdc_fr", [0.0])
            qdc_to_s = ds.get("qdc_to", [0.0])
            pdc_fr = pdc_fr_s[t] if t < len(pdc_fr_s) else 0.0
            qdc_fr = qdc_fr_s[t] if t < len(qdc_fr_s) else 0.0
            qdc_to = qdc_to_s[t] if t < len(qdc_to_s) else 0.0
            fi = bus_idx[dc_info["fr_bus"]]
            ti = bus_idx[dc_info["to_bus"]]
            p_dev[fi] -= pdc_fr
            p_dev[ti] += pdc_fr
            q_dev[fi] -= qdc_fr
            q_dev[ti] -= qdc_to

        # Shunts
        for uid, sh_sol in sol_sh.items():
            sh = shunt_info.get(uid)
            if sh is None:
                continue
            bi = bus_idx[sh["bus"]]
            step_s = sh_sol.get("step", [0])
            step = step_s[t] if t < len(step_s) else 0
            gs = sh.get("gs", 0)
            bs = sh.get("bs", 0)
            if isinstance(gs, list):
                gs = gs[step] if step < len(gs) else 0.0
            if isinstance(bs, list):
                bs = bs[step] if step < len(bs) else 0.0
            v2 = vm[bi] * vm[bi]
            p_dev[bi] -= float(gs) * v2
            q_dev[bi] += float(bs) * v2

        # Bus balance mismatch
        bus_p_mismatch: dict[str, float] = {}
        period_p_abs = 0.0
        period_q_abs = 0.0
        max_p_abs = 0.0
        max_p_bus = ""
        max_q_abs = 0.0
        max_q_bus = ""

        for i, b in enumerate(buses):
            uid = b["uid"]
            dp = p_dev[i] - p_flow[i]
            dq = q_dev[i] - q_flow[i]
            if abs(dp) > 1e-6:
                bus_p_mismatch[uid] = dp
            adp = abs(dp)
            adq = abs(dq)
            period_p_abs += adp
            period_q_abs += adq
            if adp > max_p_abs:
                max_p_abs = adp
                max_p_bus = uid
            if adq > max_q_abs:
                max_q_abs = adq
                max_q_bus = uid

        period_thermal = sum(v["overload_mva"] for v in acl_s_violations) + \
            sum(v["overload_mva"] for v in xfr_s_violations)

        p_penalty = c_p * period_p_abs * dt
        q_penalty = c_q * period_q_abs * dt
        thermal_penalty = c_s * (period_thermal / base_mva) * dt

        report.bus_p_total_mismatch_mw += period_p_abs * base_mva
        report.bus_q_total_mismatch_mvar += period_q_abs * base_mva
        report.bus_p_total_penalty += p_penalty
        report.bus_q_total_penalty += q_penalty
        report.thermal_total_overload_mva += period_thermal
        report.thermal_total_penalty += thermal_penalty

        report.periods.append({
            "period_index": t,
            "bus_p_balance": {
                "total_abs_mismatch_pu": round(period_p_abs, 8),
                "total_abs_mismatch_mw": round(period_p_abs * base_mva, 4),
                "penalty_cost": round(p_penalty, 2),
                "max_abs_mismatch_pu": round(max_p_abs, 8),
                "max_abs_mismatch_bus": max_p_bus,
                "buses": {
                    uid: round(v, 8)
                    for uid, v in sorted(bus_p_mismatch.items(), key=lambda x: -abs(x[1]))
                },
            },
            "bus_q_balance": {
                "total_abs_mismatch_pu": round(period_q_abs, 8),
                "total_abs_mismatch_mvar": round(period_q_abs * base_mva, 4),
                "penalty_cost": round(q_penalty, 2),
                "max_abs_mismatch_pu": round(max_q_abs, 8),
                "max_abs_mismatch_bus": max_q_bus,
            },
            "branch_thermal": {
                "total_overload_mva": round(period_thermal, 4),
                "penalty_cost": round(thermal_penalty, 2),
                "violations": acl_s_violations + xfr_s_violations,
            },
        })

    # Reserve penalties
    reserve_periods, reserve_summary = _compute_reserve_penalties(
        problem, dc_dispatch_result, durations, base_mva,
    )
    report.reserve_total_shortfall_mw = reserve_summary.get("total_shortfall_mw", 0.0)
    report.reserve_total_penalty = reserve_summary.get("total_penalty", 0.0)

    for i, pr in enumerate(report.periods):
        rp = reserve_periods[i] if i < len(reserve_periods) else {"products": [], "total_penalty": 0.0}
        pr["reserve"] = rp

    return report


# ---------------------------------------------------------------------------
# Rust-native violation assessment
# ---------------------------------------------------------------------------

def assess_dispatch_violations_native(
    network,
    result,
    *,
    p_bus_vio_cost: float = 1_000_000.0,
    q_bus_vio_cost: float = 1_000_000.0,
    s_vio_cost: float = 500.0,
    interval_hours: list[float] | None = None,
) -> dict[str, Any]:
    """Assess AC pi-model violations using the Rust-native implementation.

    This is faster than the pure-Python ``assess_violations()`` and uses
    the exact same pi-model branch flow equations from the Surge core.

    Args:
        network: Surge Network object.
        result: Surge DispatchResult object.
        p_bus_vio_cost: Active power bus balance violation cost ($/pu/hr).
        q_bus_vio_cost: Reactive power bus balance violation cost ($/pu/hr).
        s_vio_cost: Branch thermal violation cost ($/pu/hr).
        interval_hours: Per-period interval durations (hours).

    Returns:
        Dict with violation summary and per-period details.
    """
    from surge.dispatch import assess_dispatch_violations as _native_assess

    return _native_assess(
        network, result,
        p_bus_vio_cost=p_bus_vio_cost,
        q_bus_vio_cost=q_bus_vio_cost,
        s_vio_cost=s_vio_cost,
        interval_hours=interval_hours,
    )
