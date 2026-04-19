# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Compute GO C3 solution violations from exported solution and problem data.

Evaluates the exported solution against the standard AC pi-model power flow
equations (the same model the official GO C3 validator uses) and reports
per-period violations with quantities and penalty costs.
"""
from __future__ import annotations

import math
from typing import Any


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
    """Reconstruct GO validator startup/shutdown active-power injections.

    The official validator evaluates bus P balance using
    ``p = p_on + p_su + p_sd`` while Q comes directly from the exported
    solution series and is not gated by ``on_status``. Mirror that here so
    local violation reports line up with official ``z_penalty`` terms.
    """

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

    # Map zone_id (int) to zone uid
    zone_id_to_uid: dict[int, str] = {}
    for i, zone in enumerate(net.get("active_zonal_reserve", [])):
        zone_id_to_uid[i] = zone.get("uid", f"zone_{i}")
        zone_id_to_uid[i + 1] = zone.get("uid", f"zone_{i}")  # 1-indexed fallback

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
            # GO C3 vio_cost is $/pu/hr; shortfall is in MW
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

    summary = {
        "total_shortfall_mw": round(sum_shortfall_mw, 4),
        "total_penalty": round(sum_penalty, 2),
    }
    return per_period, summary


def compute_solution_violations(
    problem: dict[str, Any],
    solution: dict[str, Any],
    dc_dispatch_result: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Compute bus P/Q balance, branch thermal, and reserve violations.

    Returns a report dict with per-period and summary violation data.
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

    # Precompute branch parameters
    acl_params = _build_acl_params(net, bus_idx)
    xfr_params = _build_xfr_params(net, bus_idx)

    # Solution lookups
    sol_acl = {a["uid"]: a for a in tso.get("ac_line", [])}
    sol_xfr = {x["uid"]: x for x in tso.get("two_winding_transformer", [])}
    sol_dc = {d["uid"]: d for d in tso.get("dc_line", [])}
    sol_sh = {s["uid"]: s for s in tso.get("shunt", [])}
    sol_sdd = {d["uid"]: d for d in tso["simple_dispatchable_device"]}

    sdd_info = {d["uid"]: d for d in net["simple_dispatchable_device"]}
    shunt_info = {s["uid"]: s for s in net.get("shunt", [])}
    transition = _build_transition_injections(problem, solution)

    period_reports: list[dict[str, Any]] = []

    sum_bus_p_mismatch = 0.0
    sum_bus_q_mismatch = 0.0
    sum_bus_p_penalty = 0.0
    sum_bus_q_penalty = 0.0
    sum_thermal_overload = 0.0
    sum_thermal_penalty = 0.0

    for t in range(n_periods):
        dt = float(durations[t]) if t < len(durations) else 1.0

        # Extract V, theta for this period
        vm = [sol_bus[b["uid"]]["vm"][t] for b in buses]
        va = [sol_bus[b["uid"]]["va"][t] for b in buses]

        # ---- Compute power flow injections from Y-bus (P_calc, Q_calc) ----
        p_flow = [0.0] * n_bus
        q_flow = [0.0] * n_bus

        acl_s_violations: list[dict[str, Any]] = []
        xfr_s_violations: list[dict[str, Any]] = []

        # AC lines
        for ap in acl_params:
            on = sol_acl.get(ap["uid"], {}).get("on_status", [1])[t] if t < len(sol_acl.get(ap["uid"], {}).get("on_status", [1])) else 1
            if not on:
                continue
            pf, qf, pt, qt = _branch_flow_pi(ap, vm, va)
            p_flow[ap["fi"]] += pf
            q_flow[ap["fi"]] += qf
            p_flow[ap["ti"]] += pt
            q_flow[ap["ti"]] += qt

            sf = math.sqrt(pf * pf + qf * qf)
            st = math.sqrt(pt * pt + qt * qt)
            s_max = ap["s_max"]
            overload = max(sf, st) - s_max
            if overload > 1e-6:
                acl_s_violations.append({
                    "uid": ap["uid"],
                    "flow_mva": max(sf, st) * base_mva,
                    "limit_mva": s_max * base_mva,
                    "overload_mva": overload * base_mva,
                })

        # Transformers
        for xp in xfr_params:
            on = sol_xfr.get(xp["uid"], {}).get("on_status", [1])[t] if t < len(sol_xfr.get(xp["uid"], {}).get("on_status", [1])) else 1
            if not on:
                continue
            xs = sol_xfr.get(xp["uid"], {})
            tau = xs.get("tm", [xp["tau0"]])[t] if t < len(xs.get("tm", [xp["tau0"]])) else xp["tau0"]
            phi = xs.get("ta", [xp["phi0"]])[t] if t < len(xs.get("ta", [xp["phi0"]])) else xp["phi0"]
            pf, qf, pt, qt = _xfr_flow_pi(xp, vm, va, tau, phi)
            p_flow[xp["fi"]] += pf
            q_flow[xp["fi"]] += qf
            p_flow[xp["ti"]] += pt
            q_flow[xp["ti"]] += qt

            sf = math.sqrt(pf * pf + qf * qf)
            st = math.sqrt(pt * pt + qt * qt)
            s_max = xp["s_max"]
            overload = max(sf, st) - s_max
            if overload > 1e-6:
                xfr_s_violations.append({
                    "uid": xp["uid"],
                    "flow_mva": max(sf, st) * base_mva,
                    "limit_mva": s_max * base_mva,
                    "overload_mva": overload * base_mva,
                })

        # ---- Compute device injections ----
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
            pdc_fr = ds.get("pdc_fr", [0.0])[t] if t < len(ds.get("pdc_fr", [0.0])) else 0.0
            qdc_fr = ds.get("qdc_fr", [0.0])[t] if t < len(ds.get("qdc_fr", [0.0])) else 0.0
            qdc_to = ds.get("qdc_to", [0.0])[t] if t < len(ds.get("qdc_to", [0.0])) else 0.0
            fi = bus_idx[dc_info["fr_bus"]]
            ti = bus_idx[dc_info["to_bus"]]
            # pdc_fr = power from fr_bus into DC line (positive = leaving fr_bus)
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
            step = sh_sol.get("step", [0])[t] if t < len(sh_sol.get("step", [0])) else 0
            gs = sh.get("gs", 0)
            bs = sh.get("bs", 0)
            if isinstance(gs, list):
                gs = gs[step] if step < len(gs) else 0.0
            else:
                gs = float(gs) * step
            if isinstance(bs, list):
                bs = bs[step] if step < len(bs) else 0.0
            else:
                bs = float(bs) * step
            v2 = vm[bi] * vm[bi]
            p_dev[bi] -= float(gs) * v2
            q_dev[bi] += float(bs) * v2

        # ---- Bus P/Q balance: mismatch = device_injection - power_flow ----
        bus_p_mismatch: dict[str, float] = {}
        bus_q_mismatch: dict[str, float] = {}
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
            if abs(dq) > 1e-6:
                bus_q_mismatch[uid] = dq
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

        period_thermal_overload = sum(v["overload_mva"] for v in acl_s_violations) + sum(v["overload_mva"] for v in xfr_s_violations)

        p_penalty = c_p * period_p_abs * dt
        q_penalty = c_q * period_q_abs * dt
        thermal_penalty = c_s * (period_thermal_overload / base_mva) * dt

        sum_bus_p_mismatch += period_p_abs * base_mva
        sum_bus_q_mismatch += period_q_abs * base_mva
        sum_bus_p_penalty += p_penalty
        sum_bus_q_penalty += q_penalty
        sum_thermal_overload += period_thermal_overload
        sum_thermal_penalty += thermal_penalty

        period_reports.append({
            "period_index": t,
            "bus_p_balance": {
                "total_abs_mismatch_pu": round(period_p_abs, 8),
                "total_abs_mismatch_mw": round(period_p_abs * base_mva, 4),
                "penalty_cost": round(p_penalty, 2),
                "max_abs_mismatch_pu": round(max_p_abs, 8),
                "max_abs_mismatch_bus": max_p_bus,
                "buses": {uid: round(v, 8) for uid, v in sorted(bus_p_mismatch.items(), key=lambda x: -abs(x[1]))},
            },
            "bus_q_balance": {
                "total_abs_mismatch_pu": round(period_q_abs, 8),
                "total_abs_mismatch_mvar": round(period_q_abs * base_mva, 4),
                "penalty_cost": round(q_penalty, 2),
                "max_abs_mismatch_pu": round(max_q_abs, 8),
                "max_abs_mismatch_bus": max_q_bus,
            },
            "branch_thermal": {
                "total_overload_mva": round(period_thermal_overload, 4),
                "penalty_cost": round(thermal_penalty, 2),
                "violations": acl_s_violations + xfr_s_violations,
            },
        })

    # Reserve penalties from DC dispatch result
    reserve_periods, reserve_summary = _compute_reserve_penalties(
        problem, dc_dispatch_result, durations, base_mva,
    )
    sum_reserve_penalty = reserve_summary.get("total_penalty", 0.0)

    # Merge reserve data into period reports
    for i, pr in enumerate(period_reports):
        rp = reserve_periods[i] if i < len(reserve_periods) else {"products": [], "total_penalty": 0.0}
        pr["reserve"] = rp

    return {
        "summary": {
            "bus_p_balance": {
                "total_mismatch_mw": round(sum_bus_p_mismatch, 4),
                "total_penalty_cost": round(sum_bus_p_penalty, 2),
            },
            "bus_q_balance": {
                "total_mismatch_mvar": round(sum_bus_q_mismatch, 4),
                "total_penalty_cost": round(sum_bus_q_penalty, 2),
            },
            "branch_thermal": {
                "total_overload_mva": round(sum_thermal_overload, 4),
                "total_penalty_cost": round(sum_thermal_penalty, 2),
            },
            "reserve": reserve_summary,
            "total_penalty_cost": round(sum_bus_p_penalty + sum_bus_q_penalty + sum_thermal_penalty + sum_reserve_penalty, 2),
        },
        "periods": period_reports,
    }


def _build_acl_params(net: dict, bus_idx: dict) -> list[dict]:
    params = []
    for acl in net.get("ac_line", []):
        r = float(acl["r"])
        x = float(acl["x"])
        b_ch = float(acl.get("b", 0.0))
        z_sq = r * r + x * x
        if z_sq < 1e-14:
            continue
        g_sr = r / z_sq
        b_sr = -x / z_sq
        g_fr = float(acl.get("g_fr", 0.0)) if acl.get("additional_shunt") == 1 else 0.0
        b_fr = float(acl.get("b_fr", 0.0)) if acl.get("additional_shunt") == 1 else 0.0
        g_to = float(acl.get("g_to", 0.0)) if acl.get("additional_shunt") == 1 else 0.0
        b_to = float(acl.get("b_to", 0.0)) if acl.get("additional_shunt") == 1 else 0.0
        params.append({
            "uid": acl["uid"],
            "fi": bus_idx[acl["fr_bus"]],
            "ti": bus_idx[acl["to_bus"]],
            "g_sr": g_sr, "b_sr": b_sr, "b_ch": b_ch,
            "g_fr": g_fr, "b_fr": b_fr, "g_to": g_to, "b_to": b_to,
            "s_max": float(acl.get("mva_ub_nom", 1e6)),
        })
    return params


def _build_xfr_params(net: dict, bus_idx: dict) -> list[dict]:
    params = []
    for xfr in net.get("two_winding_transformer", []):
        r = float(xfr["r"])
        x = float(xfr["x"])
        b_ch = float(xfr.get("b", 0.0))
        z_sq = r * r + x * x
        if z_sq < 1e-14:
            continue
        g_sr = r / z_sq
        b_sr = -x / z_sq
        g_fr = float(xfr.get("g_fr", 0.0)) if xfr.get("additional_shunt") == 1 else 0.0
        b_fr = float(xfr.get("b_fr", 0.0)) if xfr.get("additional_shunt") == 1 else 0.0
        g_to = float(xfr.get("g_to", 0.0)) if xfr.get("additional_shunt") == 1 else 0.0
        b_to = float(xfr.get("b_to", 0.0)) if xfr.get("additional_shunt") == 1 else 0.0
        init = xfr.get("initial_status", {})
        params.append({
            "uid": xfr["uid"],
            "fi": bus_idx[xfr["fr_bus"]],
            "ti": bus_idx[xfr["to_bus"]],
            "g_sr": g_sr, "b_sr": b_sr, "b_ch": b_ch,
            "g_fr": g_fr, "b_fr": b_fr, "g_to": g_to, "b_to": b_to,
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
