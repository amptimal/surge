#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Round-trip a GO C3 winner solution through surge's ACPF and export pipeline.

Takes a winner's GO C3 ``solution.json``, treats every generator/load/DC-line
flow as a fixed setpoint, and for each period:
  * Phase 2: constructs a minimal surge ``Network`` snapshot and runs
    ``surge.solve_ac_pf`` to check the winner's dispatch closes the AC
    power-flow equations.
  * Phase 3: emits our own GO C3 solution.json (using either the winner's
    ``vm``/``va`` or our ACPF's) and scores it with the official validator.

Intended as a correctness cross-check for our AC modeling and exporter.
"""

from __future__ import annotations

import argparse
import copy
import json
import math
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import surge

from benchmarks.go_c3.violations import _build_transition_injections


# ─── Data plumbing ─────────────────────────────────────────────────────────


@dataclass
class StaticData:
    """Immutable structural data lifted out of the GO C3 problem."""

    base_mva: float
    bus_uid_to_num: dict[str, int]
    bus_uids_in_order: list[str]
    buses: list[dict[str, Any]]
    ac_lines: list[dict[str, Any]]
    xfrs: list[dict[str, Any]]
    dc_lines: list[dict[str, Any]]
    shunts: list[dict[str, Any]]
    devices: list[dict[str, Any]]
    periods: int

    @classmethod
    def from_problem(cls, problem: dict[str, Any]) -> "StaticData":
        n = problem["network"]
        base = float(n["general"]["base_norm_mva"])
        periods = int(problem["time_series_input"]["general"]["time_periods"])
        buses = list(n["bus"])
        uid_to_num = {b["uid"]: i + 1 for i, b in enumerate(buses)}
        return cls(
            base_mva=base,
            bus_uid_to_num=uid_to_num,
            bus_uids_in_order=[b["uid"] for b in buses],
            buses=buses,
            ac_lines=list(n["ac_line"]),
            xfrs=list(n["two_winding_transformer"]),
            dc_lines=list(n["dc_line"]),
            shunts=list(n["shunt"]),
            devices=list(n["simple_dispatchable_device"]),
            periods=periods,
        )


@dataclass
class WinnerSolution:
    """Indexed views of the winner's ``solution.json`` (arrays per period)."""

    raw: dict[str, Any]
    sdd: dict[str, dict[str, Any]]
    bus: dict[str, dict[str, Any]]
    ac_line: dict[str, dict[str, Any]]
    xfr: dict[str, dict[str, Any]]
    dc_line: dict[str, dict[str, Any]]
    shunt: dict[str, dict[str, Any]]

    @classmethod
    def load(cls, path: Path) -> "WinnerSolution":
        raw = json.loads(path.read_text())
        tso = raw["time_series_output"]
        return cls(
            raw=raw,
            sdd={d["uid"]: d for d in tso["simple_dispatchable_device"]},
            bus={b["uid"]: b for b in tso["bus"]},
            ac_line={a["uid"]: a for a in tso["ac_line"]},
            xfr={x["uid"]: x for x in tso["two_winding_transformer"]},
            dc_line={d["uid"]: d for d in tso["dc_line"]},
            shunt={s["uid"]: s for s in tso["shunt"]},
        )


# ─── Per-period snapshot builder ───────────────────────────────────────────


def pick_slack_bus_for_period(
    sd: StaticData, win: WinnerSolution, t: int
) -> int:
    """Pick the bus (1-indexed) with the largest online producer at period t."""
    best_bus, best_p = None, -1.0
    for dev in sd.devices:
        if dev["device_type"] != "producer":
            continue
        sol = win.sdd[dev["uid"]]
        if not int(sol["on_status"][t]):
            continue
        p = abs(float(sol["p_on"][t]))
        if p > best_p:
            best_p = p
            best_bus = sd.bus_uid_to_num[dev["bus"]]
    if best_bus is None:
        raise RuntimeError(f"No online producer anywhere at period {t}")
    return best_bus


def build_snapshot(
    sd: StaticData,
    win: WinnerSolution,
    t: int,
    slack_t: int,
    transitions: dict[str, dict[str, list[float]]] | None = None,
) -> tuple[Any, dict[str, int]]:
    """Build a fresh surge Network baked to winner dispatch at period ``t``.

    Returns (network, device_uid_to_generator_machine_id_circuit_map).
    The bus number at ``slack_t`` is marked as Slack with a generator
    whose voltage setpoint = winner vm[t]; all other producer buses are PQ
    with gen.q fixed to winner's q[t].
    """
    base = sd.base_mva
    net = surge.Network(f"roundtrip_t{t}", base)

    # Buses. Use winner vm/va as warm-start initial vm/va.
    for b in sd.buses:
        bn = sd.bus_uid_to_num[b["uid"]]
        sol = win.bus[b["uid"]]
        vm = float(sol["vm"][t])
        va_deg = math.degrees(float(sol["va"][t]))
        net.add_bus(
            bn,
            "PQ",
            float(b["base_nom_volt"]),
            b["uid"],
            0.0,
            0.0,
            vm,
            va_deg,
        )

    # Aggregate per-bus loads + shunts from consumers, DC lines, shunts.
    n_bus = len(sd.buses)
    pd_mw = [0.0] * (n_bus + 1)  # 1-indexed
    qd_mvar = [0.0] * (n_bus + 1)
    gs_mw = [0.0] * (n_bus + 1)
    bs_mvar = [0.0] * (n_bus + 1)

    # Consumers → bus loads. Includes startup/shutdown ramp injection
    # (validator uses p = p_on + p_su + p_sd). Q is not gated by on_status.
    trans = transitions or {}
    for dev in sd.devices:
        if dev["device_type"] != "consumer":
            continue
        bn = sd.bus_uid_to_num[dev["bus"]]
        sdd_sol = win.sdd[dev["uid"]]
        tr = trans.get(dev["uid"], {})
        p_su = float(tr.get("p_su", [0.0] * sd.periods)[t])
        p_sd = float(tr.get("p_sd", [0.0] * sd.periods)[t])
        p_total = float(sdd_sol["p_on"][t]) + p_su + p_sd
        pd_mw[bn] += p_total * base
        qd_mvar[bn] += float(sdd_sol["q"][t]) * base

    # DC lines → synthetic PQ loads at both terminals.
    # Sign convention (matches markets/go_c3/violations.py):
    #   at fr bus: p_dev -= pdc_fr, q_dev -= qdc_fr  → load (+pdc_fr, +qdc_fr)
    #   at to bus: p_dev += pdc_fr, q_dev -= qdc_to  → load (-pdc_fr, +qdc_to)
    for dc in sd.dc_lines:
        fr = sd.bus_uid_to_num[dc["fr_bus"]]
        to = sd.bus_uid_to_num[dc["to_bus"]]
        dc_sol = win.dc_line[dc["uid"]]
        pdc = float(dc_sol["pdc_fr"][t]) * base
        qfr = float(dc_sol["qdc_fr"][t]) * base
        qto = float(dc_sol["qdc_to"][t]) * base
        pd_mw[fr] += pdc
        qd_mvar[fr] += qfr
        pd_mw[to] -= pdc
        qd_mvar[to] += qto

    # Shunts → per-bus shunt admittance at winner's step[t].
    for sh in sd.shunts:
        bn = sd.bus_uid_to_num[sh["bus"]]
        sh_sol = win.shunt[sh["uid"]]
        step = int(sh_sol["step"][t])
        gs_mw[bn] += float(sh["gs"]) * step * base
        bs_mvar[bn] += float(sh["bs"]) * step * base

    # Apply loads + shunts.
    for b in sd.buses:
        bn = sd.bus_uid_to_num[b["uid"]]
        net.set_bus_load(bn, pd_mw[bn], qd_mvar[bn])
        net.set_bus_shunt(bn, gs_mw[bn], bs_mvar[bn])

    # AC lines as branches. Assign unique circuit numbers per bus-pair
    # (surge requires a unique (from, to, circuit) key).
    ckt_by_pair: dict[tuple[int, int], int] = {}

    def _next_ckt(a: int, b: int) -> int:
        key = (min(a, b), max(a, b))
        ckt_by_pair[key] = ckt_by_pair.get(key, 0) + 1
        return ckt_by_pair[key]

    for acl in sd.ac_lines:
        fr = sd.bus_uid_to_num[acl["fr_bus"]]
        to = sd.bus_uid_to_num[acl["to_bus"]]
        sol = win.ac_line[acl["uid"]]
        on = int(sol["on_status"][t])
        rate = float(acl["mva_ub_nom"]) * base
        ckt = _next_ckt(fr, to)
        net.add_branch(
            fr,
            to,
            float(acl["r"]),
            float(acl["x"]),
            float(acl["b"]),
            rate,
            1.0,
            0.0,
            ckt,
            0.0,
            False,
        )
        if not on:
            net.set_branch_in_service(fr, to, ckt, False)

    # Transformers as branches with tap and phase shift.
    for xfr in sd.xfrs:
        fr = sd.bus_uid_to_num[xfr["fr_bus"]]
        to = sd.bus_uid_to_num[xfr["to_bus"]]
        sol = win.xfr[xfr["uid"]]
        on = int(sol["on_status"][t])
        tm = float(sol["tm"][t])
        ta_deg = math.degrees(float(sol["ta"][t]))
        rate = float(xfr["mva_ub_nom"]) * base
        ckt = _next_ckt(fr, to)
        net.add_branch(
            fr,
            to,
            float(xfr["r"]),
            float(xfr["x"]),
            float(xfr["b"]),
            rate,
            tm,
            ta_deg,
            ckt,
            0.0,
            False,
        )
        if not on:
            net.set_branch_in_service(fr, to, ckt, False)

    # Producers as generators. Pg/Qg fixed from winner (PQ). Except slack.
    # Producer output: p = p_on + p_su + p_sd (validator convention). Q
    # is used as-is regardless of on_status.
    slack_gen_id = None
    slack_vm = float(win.bus[sd.bus_uids_in_order[slack_t - 1]]["vm"][t])
    for dev in sd.devices:
        if dev["device_type"] != "producer":
            continue
        bn = sd.bus_uid_to_num[dev["bus"]]
        sdd_sol = win.sdd[dev["uid"]]
        on = int(sdd_sol["on_status"][t])
        tr = trans.get(dev["uid"], {})
        p_su = float(tr.get("p_su", [0.0] * sd.periods)[t])
        p_sd = float(tr.get("p_sd", [0.0] * sd.periods)[t])
        p_mw = (float(sdd_sol["p_on"][t]) + p_su + p_sd) * base
        q_mvar = float(sdd_sol["q"][t]) * base
        huge = 1.0e9
        gid = net.add_generator(
            bn,
            p_mw,
            huge,
            -huge,
            slack_vm if bn == slack_t else 1.0,
            huge,
            -huge,
            "1",
            dev["uid"],
        )
        net.set_generator_q(gid, q_mvar)
        # Leave in_service=True always. on_status gating is only for
        # dispatch / market cost — AC physics uses p_total (incl. ramp
        # contributions during transitions) and q[t] from the solution.
        if bn == slack_t and slack_gen_id is None and on:
            net.set_generator_voltage_regulated(gid, True)
            slack_gen_id = gid
        else:
            net.set_generator_voltage_regulated(gid, False)

    if slack_gen_id is None:
        raise RuntimeError(
            f"No online producer at slack bus {slack_t} for period {t}"
        )

    net.set_bus_type(slack_t, "Slack")

    # Build a reverse map of producer uid -> bus
    producer_uid_to_bus = {
        d["uid"]: sd.bus_uid_to_num[d["bus"]]
        for d in sd.devices
        if d["device_type"] == "producer"
    }
    return net, producer_uid_to_bus


# ─── Phase 2: ACPF per period ──────────────────────────────────────────────


@dataclass
class PeriodResult:
    period: int
    converged: bool
    iterations: int
    max_mismatch_pu: float
    vm_err_max: float
    va_err_max_rad: float
    slack_pg_winner_mw: float | None
    slack_pg_surge_mw: float | None
    slack_qg_winner_mvar: float | None
    slack_qg_surge_mvar: float | None
    vm_surge: list[float] = field(default_factory=list)
    va_rad_surge: list[float] = field(default_factory=list)


def run_phase2(
    sd: StaticData,
    win: WinnerSolution,
    transitions: dict[str, dict[str, list[float]]] | None = None,
    *,
    tolerance: float = 1e-8,
    max_iterations: int = 50,
) -> list[PeriodResult]:
    if transitions is None:
        transitions = {}

    results: list[PeriodResult] = []
    for t in range(sd.periods):
        slack_t = pick_slack_bus_for_period(sd, win, t)
        net, _producer_bus = build_snapshot(sd, win, t, slack_t, transitions)
        # Flat-start is tempting for robustness, but the winner's initial
        # vm/va are already baked into add_bus — ACPF uses those as warm
        # start. Force flat_start=False (default) to honor our warm start.
        opts = surge.AcPfOptions(
            tolerance=tolerance,
            max_iterations=max_iterations,
            flat_start=False,
            oltc=False,
            switched_shunts=False,
            enforce_q_limits=False,
            enforce_gen_p_limits=False,
            distributed_slack=False,
            dc_warm_start=False,
            dc_line_model="fixed_schedule",
            startup_policy="single",
            angle_reference="preserve_initial",
        )
        res = surge.solve_ac_pf(net, opts)

        # Compare vm/va to winner.
        vm_err = 0.0
        va_err = 0.0
        vm_surge: list[float] = []
        va_surge: list[float] = []
        for i, uid in enumerate(sd.bus_uids_in_order):
            v_win = float(win.bus[uid]["vm"][t])
            a_win = float(win.bus[uid]["va"][t])
            v_our = float(res.vm[i])
            a_our_rad = float(res.va_rad[i])
            vm_surge.append(v_our)
            va_surge.append(a_our_rad)
            vm_err = max(vm_err, abs(v_our - v_win))
            va_err = max(va_err, abs(a_our_rad - a_win))

        # Compare slack Pg/Qg: sum winner producer Pg/Qg at slack,
        # sum our p_inject/q_inject minus loads at slack.
        slack_idx = slack_t - 1
        slack_p_surge = float(res.p_inject_mw[slack_idx])
        slack_q_surge = float(res.q_inject_mvar[slack_idx])
        # Net injection at slack from surge = total gen P - total load P.
        slack_load_p = 0.0
        slack_load_q = 0.0
        for dev in sd.devices:
            if dev["device_type"] == "consumer" and sd.bus_uid_to_num[dev["bus"]] == slack_t:
                sdd_sol = win.sdd[dev["uid"]]
                if int(sdd_sol["on_status"][t]):
                    slack_load_p += float(sdd_sol["p_on"][t]) * sd.base_mva
                    slack_load_q += float(sdd_sol["q"][t]) * sd.base_mva
        # Also DC line & shunt at slack already in load totals (baked in
        # build_snapshot); surge's p_inject excludes them. So the surge Pg
        # at slack = net_inject + all loads we added = net_inject + load.
        # Just report raw net injection for transparency.
        slack_pg_surge = slack_p_surge
        slack_qg_surge = slack_q_surge
        slack_pg_winner = sum(
            float(win.sdd[d["uid"]]["p_on"][t]) * sd.base_mva
            for d in sd.devices
            if d["device_type"] == "producer"
            and sd.bus_uid_to_num[d["bus"]] == slack_t
            and int(win.sdd[d["uid"]]["on_status"][t])
        )
        slack_qg_winner = sum(
            float(win.sdd[d["uid"]]["q"][t]) * sd.base_mva
            for d in sd.devices
            if d["device_type"] == "producer"
            and sd.bus_uid_to_num[d["bus"]] == slack_t
            and int(win.sdd[d["uid"]]["on_status"][t])
        )

        results.append(
            PeriodResult(
                period=t,
                converged=bool(res.converged),
                iterations=int(res.iterations),
                max_mismatch_pu=float(res.max_mismatch),
                vm_err_max=vm_err,
                va_err_max_rad=va_err,
                slack_pg_winner_mw=slack_pg_winner,
                slack_pg_surge_mw=slack_pg_surge,
                slack_qg_winner_mvar=slack_qg_winner,
                slack_qg_surge_mvar=slack_qg_surge,
                vm_surge=vm_surge,
                va_rad_surge=va_surge,
            )
        )
    return results


# ─── Phase 3: round-trip export & validate ─────────────────────────────────


def build_solution_from_winner(
    win: WinnerSolution,
    *,
    surge_vm_by_period: list[list[float]] | None = None,
    surge_va_rad_by_period: list[list[float]] | None = None,
) -> dict[str, Any]:
    """Build a fresh GO C3 solution.json dict.

    If ``surge_vm_by_period`` / ``surge_va_rad_by_period`` are given, uses
    our ACPF's vm/va (Phase 3b). Otherwise passes through the winner's
    values (Phase 3a — tests only the serializer).
    """
    sol = copy.deepcopy(win.raw)
    if surge_vm_by_period is None:
        return sol
    tso = sol["time_series_output"]
    for i, bus_entry in enumerate(tso["bus"]):
        for t, vm_t in enumerate(surge_vm_by_period):
            bus_entry["vm"][t] = surge_vm_by_period[t][i]
            bus_entry["va"][t] = surge_va_rad_by_period[t][i]
    return sol


# ─── CLI ───────────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--problem",
        type=Path,
        default=Path(
            "/Users/drew/code/surge/target/benchmarks/go-c3/datasets/event4_73/D1/C3E4N00073D1/scenario_303.json"
        ),
    )
    parser.add_argument(
        "--winner-solution",
        type=Path,
        default=Path(
            "/Users/drew/code/surge/target/benchmarks/go-c3/reference-submissions/extracted/C3E4N00073D1/scenario_303/Gatorgar/solution.json"
        ),
    )
    parser.add_argument(
        "--workdir",
        type=Path,
        default=Path("/tmp/surge_roundtrip_303"),
    )
    parser.add_argument("--skip-phase2", action="store_true")
    parser.add_argument("--skip-phase3", action="store_true")
    args = parser.parse_args()

    args.workdir.mkdir(parents=True, exist_ok=True)

    print("Loading problem + winner solution...")
    problem = json.loads(args.problem.read_text())
    sd = StaticData.from_problem(problem)
    win = WinnerSolution.load(args.winner_solution)
    print(
        f"  buses={len(sd.buses)} devices={len(sd.devices)} "
        f"ac_lines={len(sd.ac_lines)} xfrs={len(sd.xfrs)} "
        f"dc_lines={len(sd.dc_lines)} periods={sd.periods}"
    )

    phase2_results: list[PeriodResult] = []
    if not args.skip_phase2:
        print("\n=== Phase 2: snapshot ACPF per period ===")
        transitions = _build_transition_injections(problem, win.raw)
        phase2_results = run_phase2(sd, win, transitions)
        print(
            f"{'t':>3} {'conv':>5} {'it':>3} {'|Δ|pu':>10} "
            f"{'|Δvm|':>10} {'|Δva|rad':>10}"
        )
        for r in phase2_results:
            print(
                f"{r.period:3d} {str(r.converged):>5} {r.iterations:3d} "
                f"{r.max_mismatch_pu:10.3e} {r.vm_err_max:10.3e} "
                f"{r.va_err_max_rad:10.3e}"
            )
        any_diverged = any(not r.converged for r in phase2_results)
        print(f"\nConverged: {not any_diverged} (all 18 periods)" if not any_diverged
              else "\nWARNING: at least one period did not converge!")

    if not args.skip_phase3:
        # Lazy import so Phase 2 can run without validator deps.
        from benchmarks.go_c3.paths import default_cache_root
        from benchmarks.go_c3.validator import (
            ensure_validator_environment,
            validate_with_official_tool,
        )

        print("\n=== Phase 3a: round-trip export (winner V/θ) ===")
        sol_3a = build_solution_from_winner(win)
        sol_3a_path = args.workdir / "solution_3a.json"
        sol_3a_path.write_text(json.dumps(sol_3a, separators=(",", ":")))

        cache_root = default_cache_root()
        env = ensure_validator_environment(cache_root=cache_root)
        res_3a = validate_with_official_tool(
            env, args.problem, solution_path=sol_3a_path,
            workdir=args.workdir / "validator_3a",
        )
        z_3a = res_3a["summary_metrics"].get("z")
        print(f"  z (3a, passthrough) = {z_3a}")

        if phase2_results:
            print("\n=== Phase 3b: round-trip with our ACPF V/θ ===")
            vm_by_period = [r.vm_surge for r in phase2_results]
            va_rad_by_period = [r.va_rad_surge for r in phase2_results]
            sol_3b = build_solution_from_winner(
                win,
                surge_vm_by_period=vm_by_period,
                surge_va_rad_by_period=va_rad_by_period,
            )
            sol_3b_path = args.workdir / "solution_3b.json"
            sol_3b_path.write_text(json.dumps(sol_3b, separators=(",", ":")))
            res_3b = validate_with_official_tool(
                env, args.problem, solution_path=sol_3b_path,
                workdir=args.workdir / "validator_3b",
            )
            z_3b = res_3b["summary_metrics"].get("z")
            print(f"  z (3b, our ACPF vm/va) = {z_3b}")

        # Archived winner z.
        winner_summary = json.loads(
            args.winner_solution.with_name("summary.json").read_text()
        )
        z_winner = winner_summary.get("evaluation", {}).get("z")
        print(f"\n  z (winner archived)      = {z_winner}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
