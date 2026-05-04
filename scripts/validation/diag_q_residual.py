"""Localize the per-bus Q (and P) residual in a GO C3 solution.

Loads scenario problem JSON + solution.json, runs the same pi-model
math as benchmarks/go_c3/violations.py, then prints:
  * per-bus residual (sorted by |Q residual|)
  * per-branch contribution split (line vs xfmr) at the leaking buses
  * sanity-check sum check: device_inj == sum(branch_flow_into_bus)
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path

import sys

# Reuse the validator's pi-model functions verbatim so any divergence
# we see is between (a) AC OPF internal balance and (b) the validator's
# external recomputation — not between the validator and a re-impl.
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from benchmarks.go_c3.violations import (  # noqa: E402
    _branch_flow_pi,
    _build_acl_params,
    _build_xfr_params,
    _xfr_flow_pi,
)


def _build_transition(problem, solution):
    sdd_info = {d["uid"]: d for d in problem["network"]["simple_dispatchable_device"]}
    out = {}
    tso = solution["time_series_output"]
    n_periods = len(problem["time_series_input"]["general"]["interval_duration"])
    for d in tso["simple_dispatchable_device"]:
        info = sdd_info[d["uid"]]
        on = d.get("on_status", [info["initial_status"]["on_status"]] * n_periods)
        p_su = [0.0] * n_periods
        p_sd = [0.0] * n_periods
        out[d["uid"]] = {"p_su": p_su, "p_sd": p_sd}
    return out


def main(problem_path: Path, solution_path: Path, periods: list[int] | None) -> None:
    with open(problem_path) as f:
        problem = json.load(f)
    with open(solution_path) as f:
        solution = json.load(f)

    net = problem["network"]
    base_mva = net["general"]["base_norm_mva"]
    durations = problem["time_series_input"]["general"]["interval_duration"]
    n_periods = len(durations)

    buses = net["bus"]
    n_bus = len(buses)
    bus_idx = {b["uid"]: i for i, b in enumerate(buses)}
    bus_uid = [b["uid"] for b in buses]

    acl_params = _build_acl_params(net, bus_idx)
    xfr_params = _build_xfr_params(net, bus_idx)

    tso = solution["time_series_output"]
    sol_bus = {b["uid"]: b for b in tso["bus"]}
    sol_acl = {a["uid"]: a for a in tso.get("ac_line", [])}
    sol_xfr = {x["uid"]: x for x in tso.get("two_winding_transformer", [])}
    sol_dc = {d["uid"]: d for d in tso.get("dc_line", [])}
    sol_sh = {s["uid"]: s for s in tso.get("shunt", [])}
    sol_sdd = {d["uid"]: d for d in tso["simple_dispatchable_device"]}

    sdd_info = {d["uid"]: d for d in net["simple_dispatchable_device"]}
    shunt_info = {s["uid"]: s for s in net.get("shunt", [])}

    target = list(range(n_periods)) if periods is None else periods

    for t in target:
        vm = [sol_bus[b["uid"]]["vm"][t] for b in buses]
        va = [sol_bus[b["uid"]]["va"][t] for b in buses]

        p_flow = [0.0] * n_bus
        q_flow = [0.0] * n_bus

        # Track per-bus contributions so we can attribute the residual.
        line_q_at = [[] for _ in range(n_bus)]
        xfr_q_at = [[] for _ in range(n_bus)]

        for ap in acl_params:
            on_seq = sol_acl.get(ap["uid"], {}).get("on_status", [1])
            on = on_seq[t] if t < len(on_seq) else 1
            if not on:
                continue
            pf, qf, pt, qt = _branch_flow_pi(ap, vm, va)
            p_flow[ap["fi"]] += pf
            q_flow[ap["fi"]] += qf
            p_flow[ap["ti"]] += pt
            q_flow[ap["ti"]] += qt
            line_q_at[ap["fi"]].append((ap["uid"], qf))
            line_q_at[ap["ti"]].append((ap["uid"], qt))

        for xp in xfr_params:
            on_seq = sol_xfr.get(xp["uid"], {}).get("on_status", [1])
            on = on_seq[t] if t < len(on_seq) else 1
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
            xfr_q_at[xp["fi"]].append((xp["uid"], qf, tau, phi))
            xfr_q_at[xp["ti"]].append((xp["uid"], qt, tau, phi))

        p_dev = [0.0] * n_bus
        q_dev = [0.0] * n_bus

        for uid, sd in sol_sdd.items():
            info = sdd_info[uid]
            bi = bus_idx[info["bus"]]
            dtype = info["device_type"]
            p_on = sd["p_on"][t] if t < len(sd["p_on"]) else 0.0
            q_val = sd["q"][t] if t < len(sd["q"]) else 0.0
            sign = 1.0 if dtype == "producer" else -1.0
            p_dev[bi] += sign * p_on
            q_dev[bi] += sign * q_val

        for dc_info in net.get("dc_line", []):
            ds = sol_dc.get(dc_info["uid"], {})
            pdc_fr = ds.get("pdc_fr", [0.0])[t] if t < len(ds.get("pdc_fr", [0.0])) else 0.0
            qdc_fr = ds.get("qdc_fr", [0.0])[t] if t < len(ds.get("qdc_fr", [0.0])) else 0.0
            qdc_to = ds.get("qdc_to", [0.0])[t] if t < len(ds.get("qdc_to", [0.0])) else 0.0
            fi = bus_idx[dc_info["fr_bus"]]
            ti = bus_idx[dc_info["to_bus"]]
            p_dev[fi] -= pdc_fr
            p_dev[ti] += pdc_fr
            q_dev[fi] -= qdc_fr
            q_dev[ti] -= qdc_to

        for uid, sh_sol in sol_sh.items():
            sh = shunt_info.get(uid)
            if sh is None:
                continue
            bi = bus_idx[sh["bus"]]
            step = sh_sol.get("step", [0])[t] if t < len(sh_sol.get("step", [0])) else 0
            gs = sh.get("gs", 0)
            bs = sh.get("bs", 0)
            gs = float(gs) * step if not isinstance(gs, list) else float(gs[step] if step < len(gs) else 0.0)
            bs = float(bs) * step if not isinstance(bs, list) else float(bs[step] if step < len(bs) else 0.0)
            v2 = vm[bi] * vm[bi]
            p_dev[bi] -= gs * v2
            q_dev[bi] += bs * v2

        # Per-bus residual = device_injection - branch_flow_out
        residuals = []
        for i in range(n_bus):
            dp = p_dev[i] - p_flow[i]
            dq = q_dev[i] - q_flow[i]
            residuals.append((bus_uid[i], dp, dq))

        residuals.sort(key=lambda r: abs(r[2]), reverse=True)
        sum_p = sum(abs(r[1]) for r in residuals)
        sum_q = sum(abs(r[2]) for r in residuals)

        print(f"\n=== period {t} (dt={durations[t]:.4f}h) ===")
        print(f"  total |dP|={sum_p:.3e} pu   total |dQ|={sum_q:.3e} pu")
        print(f"  top 8 buses by |dQ|:")
        for uid, dp, dq in residuals[:8]:
            print(f"    {uid:>10s}  dP={dp:+.3e}  dQ={dq:+.3e}")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--problem", required=True)
    ap.add_argument("--solution", required=True)
    ap.add_argument("--periods", type=lambda s: [int(x) for x in s.split(",")], default=None)
    args = ap.parse_args()
    main(Path(args.problem), Path(args.solution), args.periods)
