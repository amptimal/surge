#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Round-trip a GO C3 winner through our DC SCUC + AC SCED pipeline.

Pins commitment (and optionally Pg, Qg) to the winner's solution and runs
the canonical two-stage market workflow. The premise: if our SCUC had
arrived at exactly the winner's settings, the rest of our pipeline
(SCED → export → validator) should reproduce the winner's z-score.

Variants:
  * A — commitment pinned only; Pg/Qg/reserves optimized
  * B — commitment + Pg pinned; Qg/reserves optimized
  * C — commitment + Pg + Qg pinned; reserves optimized
  * D — commitment + Pg + Qg + HVDC terminal Q pinned (tightest)
"""

from __future__ import annotations

import argparse
import copy
import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from surge.market import go_c3 as gx

from benchmarks.go_c3.paths import default_cache_root
from benchmarks.go_c3.validator import (
    ensure_validator_environment,
    validate_with_official_tool,
)
from benchmarks.go_c3.violations import _build_transition_injections


# ─── Winner data access ────────────────────────────────────────────────────


@dataclass
class Winner:
    raw: dict[str, Any]
    sdd: dict[str, dict[str, Any]]
    dc_line: dict[str, dict[str, Any]]
    base_mva: float
    periods: int

    @classmethod
    def load(cls, path: Path, problem_path: Path) -> "Winner":
        raw = json.loads(path.read_text())
        tso = raw["time_series_output"]
        prob = json.loads(problem_path.read_text())
        return cls(
            raw=raw,
            sdd={d["uid"]: d for d in tso["simple_dispatchable_device"]},
            dc_line={d["uid"]: d for d in tso["dc_line"]},
            base_mva=float(prob["network"]["general"]["base_norm_mva"]),
            periods=int(prob["time_series_input"]["general"]["time_periods"]),
        )


# ─── Request mutators ──────────────────────────────────────────────────────


def commit_schedule_from_winner(
    winner: Winner, committable_ids: set[str]
) -> dict[str, list[bool]]:
    """Per-device per-period commitment (True = on).

    Filtered to ``committable_ids`` — the Rust commitment schedule
    rejects entries for resources that aren't in its tracked set (e.g.,
    always-on producers, consumers, HVDC synth gens).
    """
    return {
        uid: [bool(int(s)) for s in d["on_status"]]
        for uid, d in winner.sdd.items()
        if uid in committable_ids
    }


def committable_resource_ids_from_workflow(workflow, stage_idx: int = 1) -> set[str]:
    """Extract the set of resource_ids the stage's commitment schedule tracks."""
    req = workflow.stage_request(stage_idx)
    fx = req.get("commitment", {}).get("fixed", {})
    return {r["resource_id"] for r in fx.get("resources", [])}


def pin_producer_bounds_in_request(
    request: dict[str, Any],
    winner: Winner,
    transitions: dict[str, dict[str, list[float]]],
    *,
    pin_p: bool,
    pin_q: bool,
    band_mw: float = 0.0,
) -> None:
    """Pin per-period Pg and/or Qg bounds in generator_dispatch_bounds.

    ``band_mw`` adds a symmetric tolerance around winner's value
    (≥ 0). 0.0 means pin to a point; any small positive band helps the
    NLP avoid infeasibility on marginal cases.
    """
    base = winner.base_mva
    profiles = request["profiles"]["generator_dispatch_bounds"]["profiles"]
    for prof in profiles:
        rid = prof["resource_id"]
        wd = winner.sdd.get(rid)
        if wd is None:
            continue
        tr = transitions.get(rid, {})
        periods = winner.periods
        p_su = tr.get("p_su", [0.0] * periods)
        p_sd = tr.get("p_sd", [0.0] * periods)
        if pin_p:
            p_mw = [
                (float(wd["p_on"][t]) + float(p_su[t]) + float(p_sd[t])) * base
                for t in range(periods)
            ]
            prof["p_min_mw"] = [p - band_mw for p in p_mw]
            prof["p_max_mw"] = [p + band_mw for p in p_mw]
        if pin_q:
            q_mvar = [float(wd["q"][t]) * base for t in range(periods)]
            prof["q_min_mvar"] = [q - band_mw for q in q_mvar]
            prof["q_max_mvar"] = [q + band_mw for q in q_mvar]


def pin_dispatchable_loads_in_request(
    request: dict[str, Any],
    winner: Winner,
    transitions: dict[str, dict[str, list[float]]],
) -> None:
    """Pin each consumer's per-period per-block dispatch to winner's p_on.

    Each GO C3 consumer becomes N blocks (e.g. sd_XXX::blk:00..04) with
    per-period offers. Winner's p_on total must be distributed across
    these blocks; we fill them greedily in block order until winner's
    total is served, then zero out the rest. Q is similarly distributed
    in proportion to the block's P share.
    """
    base = winner.base_mva
    schedules = request["market"]["dispatchable_load_offer_schedules"]

    # Group block IDs by consumer uid. Block id format: "sd_XXX::blk:YY".
    blocks_by_consumer: dict[str, list[dict[str, Any]]] = {}
    for sch in schedules:
        rid = sch["resource_id"]
        if "::blk:" not in rid:
            continue
        consumer_uid = rid.split("::blk:")[0]
        blocks_by_consumer.setdefault(consumer_uid, []).append(sch)

    # Sort blocks by block index for stable greedy fill order.
    for lst in blocks_by_consumer.values():
        lst.sort(key=lambda s: int(s["resource_id"].split("::blk:")[1]))

    periods = winner.periods
    for consumer_uid, blocks in blocks_by_consumer.items():
        wd = winner.sdd.get(consumer_uid)
        if wd is None:
            continue
        tr = transitions.get(consumer_uid, {})
        p_su = tr.get("p_su", [0.0] * periods)
        p_sd = tr.get("p_sd", [0.0] * periods)
        for t in range(periods):
            p_target_pu = float(wd["p_on"][t]) + float(p_su[t]) + float(p_sd[t])
            q_target_pu = float(wd["q"][t])
            remaining = p_target_pu
            # Distribute across blocks greedily.
            for sch in blocks:
                periods_arr = sch["schedule"]["periods"]
                period = periods_arr[t]
                original_max = float(period["p_max_pu"])
                alloc = min(remaining, original_max) if remaining > 0 else 0.0
                q_share = q_target_pu * (alloc / p_target_pu) if p_target_pu > 0 else 0.0
                period["p_max_pu"] = alloc
                period["p_sched_pu"] = alloc
                period["q_sched_pu"] = q_share
                period["q_min_pu"] = q_share
                period["q_max_pu"] = q_share
                remaining -= alloc

    # Also update top-level dispatchable_loads p_min_pu / p_max_pu if
    # they're enforced as global bounds (defensive — they might be
    # per-block level but we set them conservatively).


def pin_hvdc_synth_bounds_in_request(
    request: dict[str, Any],
    ctx: dict[str, Any],
    winner: Winner,
    *,
    band_mvar: float = 0.0,
) -> None:
    """Pin HVDC synthetic generator Q bounds to winner's qdc_fr / qdc_to.

    These synth gens carry the DC-line reactive terminal injections. The
    surge network's synth-gen Q convention is the NEGATIVE of GO C3's
    `qdc_fr` / `qdc_to` (see `surge-io/src/go_c3/hvdc_q.rs:144`
    `terminal_generator_q_bounds_mvar`, which negates `q_lb_pu` / `q_ub_pu`
    when populating the gen's qmin/qmax). Pinning to `+qdc_fr` instead of
    `-qdc_fr` adds ~200 MVAr of double-count residual at the HVDC terminal
    buses and makes winner's feasible operating point appear infeasible to
    the AC NLP.
    """
    base = winner.base_mva
    resource_to_output = ctx.get("dc_line_reactive_support_resource_to_output", {})
    profiles = request["profiles"]["generator_dispatch_bounds"]["profiles"]
    by_id = {p["resource_id"]: p for p in profiles}
    for synth_id, (dc_uid, field) in resource_to_output.items():
        prof = by_id.get(synth_id)
        if prof is None:
            continue
        dc = winner.dc_line.get(dc_uid)
        if dc is None:
            continue
        # Negate: surge synth-gen Q = −(GO C3 qdc_fr / qdc_to).
        q = [-float(dc[field][t]) * base for t in range(winner.periods)]
        prof["q_min_mvar"] = [qi - band_mvar for qi in q]
        prof["q_max_mvar"] = [qi + band_mvar for qi in q]
        # Synth gens have p_min = p_max = 0 already; leave untouched.


# ─── Driver ────────────────────────────────────────────────────────────────


@dataclass
class Variant:
    name: str
    pin_p: bool
    pin_q: bool
    pin_hvdc_q: bool
    # Experiment knobs
    ac_opf_tolerance: float | None = None  # E1: override Ipopt `tol`
    warm_start_from_winner: bool = False  # E2: seed V/θ from winner
    reactive_cost_multiplier: float = 1.0  # scale Reactive product shortfall cost
    syn_cost_multiplier: float = 1.0  # E4-syn: scale SYN shortfall cost
    disable_syn_balance: bool = False  # E6: remove SYN balance_products substitution
    bus_balance_penalty_multiplier: float = 1.0  # E7: scale bus P/Q balance slack penalty

    @property
    def label(self) -> str:
        pieces = ["commit"]
        if self.pin_p:
            pieces.append("Pg")
        if self.pin_q:
            pieces.append("Qg")
        if self.pin_hvdc_q:
            pieces.append("HVDCq")
        if self.ac_opf_tolerance is not None:
            pieces.append(f"tol={self.ac_opf_tolerance:g}")
        if self.warm_start_from_winner:
            pieces.append("warm")
        if self.reactive_cost_multiplier != 1.0:
            pieces.append(f"qx{self.reactive_cost_multiplier:g}")
        if self.syn_cost_multiplier != 1.0:
            pieces.append(f"synx{self.syn_cost_multiplier:g}")
        if self.disable_syn_balance:
            pieces.append("noBal")
        if self.bus_balance_penalty_multiplier != 1.0:
            pieces.append(f"busPenx{self.bus_balance_penalty_multiplier:g}")
        return "+".join(pieces)


def apply_warm_start_from_winner(
    request: dict[str, Any],
    ctx: dict[str, Any],
    winner: Winner,
) -> None:
    """Populate ``request.runtime.ac_dispatch_warm_start.buses`` from winner."""
    bus_uid_to_number = ctx.get("bus_uid_to_number", {})
    tso = winner.raw["time_series_output"]
    bus_sol_by_uid = {b["uid"]: b for b in tso["bus"]}
    buses_ws = []
    for uid, bn in bus_uid_to_number.items():
        b = bus_sol_by_uid.get(uid)
        if b is None:
            continue
        buses_ws.append(
            {
                "bus_number": int(bn),
                "vm_pu": [float(v) for v in b["vm"]],
                "va_rad": [float(v) for v in b["va"]],
            }
        )
    rt = request.setdefault("runtime", {})
    rt["ac_dispatch_warm_start"] = {"buses": buses_ws}


def set_ac_opf_tolerance(request: dict[str, Any], tolerance: float) -> None:
    """Set runtime.ac_opf.tolerance (the Ipopt `tol` value)."""
    rt = request.setdefault("runtime", {})
    ac_opf = rt.setdefault("ac_opf", {})
    ac_opf["tolerance"] = float(tolerance)


def scale_ac_opf_bus_balance_penalties(
    request: dict[str, Any], multiplier: float
) -> None:
    """Multiply AC OPF's bus P/Q balance slack penalties by ``multiplier``.

    Bumps how strongly the AC SCED's NLP objective penalizes nonzero
    bus-balance slack (the slack that lets Ipopt converge at a
    near-feasible-but-not-exact point). A higher multiplier forces the
    NLP to drive residual |ΔP| / |ΔQ| smaller, at the cost of longer
    Ipopt runs and potential convergence pain. Reads the current
    value from ``runtime.ac_opf`` (populated by the workflow builder
    from GO C3 validator-aligned defaults) and multiplies in place.
    """
    rt = request.setdefault("runtime", {})
    ac_opf = rt.setdefault("ac_opf", {})
    p_key = "bus_active_power_balance_slack_penalty_per_mw"
    q_key = "bus_reactive_power_balance_slack_penalty_per_mvar"
    if p_key in ac_opf:
        ac_opf[p_key] = float(ac_opf[p_key]) * float(multiplier)
    if q_key in ac_opf:
        ac_opf[q_key] = float(ac_opf[q_key]) * float(multiplier)


def scale_reactive_shortfall_cost(
    request: dict[str, Any], multiplier: float, kinds: tuple[str, ...] = ("Reactive",)
) -> None:
    """Multiply demand-curve cost of reserve products of given kinds.

    Also bumps each ``zonal_reserve_requirements`` entry's
    ``shortfall_cost_per_unit`` so the SCUC LP and AC SCED NLP both
    see the higher shortfall penalty (the LP uses zone-specific
    shortfall_cost_per_unit when present, falling back to the demand
    curve only if it's None).
    """
    # Scale demand curve cost on the products themselves
    matching_ids: set[str] = set()
    for p in request["market"]["reserve_products"]:
        if p.get("kind") in kinds:
            matching_ids.add(p.get("id", ""))
            dc = p.get("demand_curve", {})
            if "cost_per_unit" in dc:
                dc["cost_per_unit"] = float(dc["cost_per_unit"]) * float(multiplier)
    # Scale zonal requirement shortfall costs for those products
    for req in request["market"].get("zonal_reserve_requirements", []):
        if req.get("product_id") in matching_ids:
            sc = req.get("shortfall_cost_per_unit")
            if sc is not None:
                req["shortfall_cost_per_unit"] = float(sc) * float(multiplier)


def scale_active_syn_shortfall_cost(
    request: dict[str, Any], multiplier: float
) -> None:
    """Crank the active synchronized-contingency reserve (SYN) shortfall cost.

    Targets the 'syn' reserve product specifically — the one driving
    the $307 SCR shortfall penalty in variant A.
    """
    for p in request["market"]["reserve_products"]:
        if p.get("id") == "syn":
            dc = p.get("demand_curve", {})
            if "cost_per_unit" in dc:
                dc["cost_per_unit"] = float(dc["cost_per_unit"]) * float(multiplier)
    for req in request["market"].get("zonal_reserve_requirements", []):
        if req.get("product_id") == "syn":
            sc = req.get("shortfall_cost_per_unit")
            if sc is not None:
                req["shortfall_cost_per_unit"] = float(sc) * float(multiplier)


def disable_syn_balance_substitution(request: dict[str, Any]) -> None:
    """Remove SYN's ``balance_products`` so REG_UP awards no longer substitute.

    Our LP currently models SYN demand as satisfiable by sum(SYN + REG_UP)
    awards via the ``balance_products`` mechanism. The GO C3 validator
    instead uses a *cascade*: REG_UP shortfall (positive remainder) bleeds
    into SCR demand, but REG_UP over-provision is only a "leftover" — only
    p_scr awards directly reduce SCR demand. The two formulations diverge
    when REG_UP is just barely met (no leftover) and SCR awards alone are
    short of SCR demand: LP says feasible, validator says shortfall.
    Removing ``balance_products`` forces the LP to cover SCR demand with
    SYN awards alone, matching the validator's strict accounting.
    """
    for p in request["market"]["reserve_products"]:
        if p.get("id") == "syn":
            p["balance_products"] = []


def run_variant(
    variant: Variant,
    *,
    problem_path: Path,
    winner_path: Path,
    workdir: Path,
    lp_solver: str,
    nlp_solver: str,
    band_mw: float = 0.0,
) -> dict[str, Any]:
    workdir.mkdir(parents=True, exist_ok=True)
    print(f"\n=== Variant {variant.name}: {variant.label} ===")

    problem = gx.load(problem_path)
    winner = Winner.load(winner_path, problem_path)
    transitions = _build_transition_injections(
        json.loads(problem_path.read_text()), winner.raw
    )

    policy = gx.MarketPolicy(
        formulation="dc",
        ac_reconcile_mode="ac_dispatch",
        commitment_mode="optimize",  # set_stage_commitment overrides this
        consumer_mode="dispatchable",
        lp_solver=lp_solver,
        nlp_solver=nlp_solver,
    )

    # Build workflow (DC SCUC → AC SCED). Also need the context for HVDC
    # synth-gen resource IDs.
    _, ctx = gx.build_network(problem, policy)
    workflow = gx.build_workflow(problem, policy)
    assert workflow.n_stages() == 2, f"expected 2 stages, got {workflow.n_stages()}"

    # Pin commitment in both stages to winner's per-period on_status.
    # Filter to the committable subset (producers that actually have
    # startup/shutdown decisions; skips always-on producers, consumers,
    # and HVDC synth gens).
    committable = committable_resource_ids_from_workflow(workflow, 1)
    commit = commit_schedule_from_winner(winner, committable)
    workflow.set_stage_commitment(0, commit)
    workflow.set_stage_commitment(1, commit)

    # Pin Pg (and consumer dispatch) at BOTH stages so DC SCUC allocates
    # reserves against winner's dispatch and AC SCED refines Q/V/θ over
    # the same dispatch. Qg and HVDC-Q only affect AC SCED, so pin only
    # at stage 1.
    for stage_idx in (0, 1):
        req = workflow.stage_request(stage_idx)
        pin_producer_bounds_in_request(
            req,
            winner,
            transitions,
            pin_p=variant.pin_p,
            pin_q=variant.pin_q and stage_idx == 1,
            band_mw=band_mw,
        )
        if variant.pin_p:
            pin_dispatchable_loads_in_request(req, winner, transitions)
        if variant.pin_hvdc_q and stage_idx == 1:
            pin_hvdc_synth_bounds_in_request(req, ctx, winner, band_mvar=band_mw)
        # Experiment knobs — stage 1 (AC SCED) only
        if stage_idx == 1:
            if variant.ac_opf_tolerance is not None:
                set_ac_opf_tolerance(req, variant.ac_opf_tolerance)
            if variant.warm_start_from_winner:
                apply_warm_start_from_winner(req, ctx, winner)
            if variant.bus_balance_penalty_multiplier != 1.0:
                scale_ac_opf_bus_balance_penalties(
                    req, variant.bus_balance_penalty_multiplier
                )
        if variant.reactive_cost_multiplier != 1.0:
            scale_reactive_shortfall_cost(req, variant.reactive_cost_multiplier)
        if variant.syn_cost_multiplier != 1.0:
            scale_active_syn_shortfall_cost(req, variant.syn_cost_multiplier)
        if variant.disable_syn_balance:
            disable_syn_balance_substitution(req)
        workflow.set_stage_request(stage_idx, req)

    # Solve.
    print(f"  solving (lp={lp_solver}, nlp={nlp_solver})...")
    result = gx.solve_workflow(workflow, lp_solver=lp_solver, nlp_solver=nlp_solver)
    stages = result["stages"]
    print(
        f"  stage 0 ({stages[0]['stage_id']}): status={stages[0].get('status', 'ok')}"
    )
    print(
        f"  stage 1 ({stages[1]['stage_id']}): status={stages[1].get('status', 'ok')}"
    )

    # Export.
    ac_sol = stages[1]["solution"]
    dc_sol = stages[0]["solution"]
    exported = gx.export(problem, ac_sol, dc_reserve_source=dc_sol)
    sol_path = workdir / f"solution_{variant.name}.json"
    gx.save(exported, sol_path)

    # Capture AC SCED's internal view of bus-balance/thermal/etc slack
    # penalties (the NLP's own objective tally) so we can compare
    # against the validator's post-hoc scoring.
    ac_penalty_summary = ac_sol.get("penalty_summary", {}) or {}
    sced_p_mw = float(ac_penalty_summary.get("power_balance_p_total_mw", 0.0))
    sced_q_mvar = float(ac_penalty_summary.get("power_balance_q_total_mvar", 0.0))
    sced_p_cost = float(ac_penalty_summary.get("power_balance_p_total_cost", 0.0))
    sced_q_cost = float(ac_penalty_summary.get("power_balance_q_total_cost", 0.0))
    print(
        f"  SCED internal bus balance: |ΔP|_sum={sced_p_mw:.4e} MW  "
        f"|ΔQ|_sum={sced_q_mvar:.4e} MVAr  "
        f"P_cost={sced_p_cost:.2f}  Q_cost={sced_q_cost:.2f}"
    )

    # Validate.
    env = ensure_validator_environment(cache_root=default_cache_root())
    vr = validate_with_official_tool(
        env, problem_path, solution_path=sol_path, workdir=workdir / f"validator_{variant.name}"
    )
    metrics = vr["summary_metrics"]
    z = metrics.get("z")
    # Pull full penalty breakdown from the validator's detailed summary JSON.
    summary_full = vr.get("summary") or {}
    ev = summary_full.get("evaluation", {}) if isinstance(summary_full, dict) else {}
    breakdown = {
        k: ev.get(k, 0.0)
        for k in (
            "sum_bus_t_z_p",
            "sum_bus_t_z_q",
            "sum_prz_t_z_scr",
            "sum_prz_t_z_rgu",
            "sum_prz_t_z_rgd",
            "sum_sd_t_z_rgu",
            "sum_sd_t_z_rgd",
        )
    }
    print(
        f"  z = {z}   (z_cost={metrics.get('z_cost')} "
        f"z_penalty={metrics.get('z_penalty')} feas={metrics.get('feas')} infeas={metrics.get('infeas')})"
    )
    print(
        "  penalty breakdown: "
        + ", ".join(f"{k.replace('sum_','').replace('_t','')}={v:.2f}" for k, v in breakdown.items())
    )
    return {
        "variant": variant.label,
        "z": z,
        "z_cost": metrics.get("z_cost"),
        "z_penalty": metrics.get("z_penalty"),
        "metrics": metrics,
        "breakdown": breakdown,
        "sced_internal": {
            "p_mw": sced_p_mw,
            "q_mvar": sced_q_mvar,
            "p_cost": sced_p_cost,
            "q_cost": sced_q_cost,
        },
    }


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
        default=Path("/tmp/surge_sced_roundtrip_303"),
    )
    parser.add_argument("--lp-solver", default="gurobi")
    parser.add_argument("--nlp-solver", default="ipopt")
    parser.add_argument(
        "--variants",
        nargs="+",
        default=["A", "B", "C", "D"],
        choices=["A", "B", "C", "D", "E1", "E2", "E3", "E4",
                 "E4_syn10", "E4_syn100", "E4_syn1000", "E6", "E7", "E8"],
    )
    parser.add_argument(
        "--band-mw",
        type=float,
        default=0.1,
        help="Symmetric tolerance (MW / MVAr) around winner values when pinning.",
    )
    args = parser.parse_args()

    variants = {
        "A": Variant(name="A", pin_p=False, pin_q=False, pin_hvdc_q=False),
        "B": Variant(name="B", pin_p=True, pin_q=False, pin_hvdc_q=False),
        "C": Variant(name="C", pin_p=True, pin_q=True, pin_hvdc_q=False),
        "D": Variant(name="D", pin_p=True, pin_q=True, pin_hvdc_q=True),
        # E-series: variant A baseline + one NLP-level remediation
        "E1": Variant(
            name="E1",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            ac_opf_tolerance=1e-9,
        ),
        "E2": Variant(
            name="E2",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            warm_start_from_winner=True,
        ),
        "E3": Variant(
            name="E3",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            ac_opf_tolerance=1e-9,
            warm_start_from_winner=True,
        ),
        "E4": Variant(
            # E2/E3 showed the AC SCED "warm start" mechanism is V/θ
            # target-tracking, not a true initial-guess warm start, so
            # E4 uses tolerance + cost multiplier only (no warm-start).
            name="E4",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            ac_opf_tolerance=1e-9,
            warm_start_from_winner=False,
            reactive_cost_multiplier=10.0,
        ),
        "E4_syn10": Variant(
            # SYN cost ×10 — does cranking the synchronized-reserve
            # shortfall cost incentivize our SCUC to award more SYN?
            name="E4_syn10",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            syn_cost_multiplier=10.0,
        ),
        "E4_syn100": Variant(
            name="E4_syn100",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            syn_cost_multiplier=100.0,
        ),
        "E4_syn1000": Variant(
            name="E4_syn1000",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            syn_cost_multiplier=1000.0,
        ),
        "E6": Variant(
            # Validator-aligned cascade: SCR demand met by SCR awards
            # alone. Superseded by the adapter default as of the
            # cascade-alignment commit; kept as a flag for regression
            # testing. No behavior change vs A post-fix.
            name="E6",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            disable_syn_balance=True,
        ),
        "E7": Variant(
            # Bus P/Q balance penalty × 10 — pushes the AC SCED NLP to
            # drive residual bus P/Q imbalance tighter. Attacks the
            # ~$133 bus_p + bus_q validator penalty seen at the local
            # NLP optimum (notably the bus_17 Q residual ~2e-6 pu).
            name="E7",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            bus_balance_penalty_multiplier=10.0,
        ),
        "E8": Variant(
            # × 100, for sensitivity — does penalty eventually drive
            # the residual to NLP tolerance?
            name="E8",
            pin_p=False, pin_q=False, pin_hvdc_q=False,
            bus_balance_penalty_multiplier=100.0,
        ),
    }

    args.workdir.mkdir(parents=True, exist_ok=True)
    results = []
    for key in args.variants:
        r = run_variant(
            variants[key],
            problem_path=args.problem,
            winner_path=args.winner_solution,
            workdir=args.workdir,
            lp_solver=args.lp_solver,
            nlp_solver=args.nlp_solver,
            band_mw=args.band_mw,
        )
        results.append(r)

    winner_summary = json.loads(
        args.winner_solution.with_name("summary.json").read_text()
    )
    z_winner = winner_summary.get("evaluation", {}).get("z")

    print("\n=== Summary ===")
    print(
        f"{'variant':<40} {'z':>22} {'Δ z':>14} {'z_cost':>14} "
        f"{'z_penalty':>12} {'bus_q':>8} {'bus_p':>8} {'prz_scr':>10}"
    )
    for r in results:
        z = r["z"]
        d = (z - z_winner) if (z is not None and z_winner is not None) else None
        dstr = f"{d:.3e}" if d is not None else "n/a"
        b = r.get("breakdown", {})
        print(
            f"{r['variant']:<40} {z:>22.4f} {dstr:>14} "
            f"{r.get('z_cost', 0):>14.2f} {r.get('z_penalty', 0):>12.2f} "
            f"{b.get('sum_bus_t_z_q', 0):>8.2f} {b.get('sum_bus_t_z_p', 0):>8.2f} "
            f"{b.get('sum_prz_t_z_scr', 0):>10.2f}"
        )
    print(
        f"{'winner (archived)':<40} {z_winner:>22.4f}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
