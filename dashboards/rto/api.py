# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""RTO dashboard adapter: case registry, scaffold synthesizer, solve driver.

The frontend sends a single self-contained ``scenario`` dict (see the
``build_scaffold`` return shape for the full schema). This module:

1. Enumerates the built-in case library and unpacks a case on demand.
2. Synthesizes a default scenario for any ``surge.Network`` — picks a
   24-period horizon, applies a daily duck-curve load profile to each
   bus' nominal demand, derives offer curves from the network's
   quadratic cost coefficients (falls back to a flat 3-tier curve),
   and rolls up reserve requirements as a percentage of system peak.
3. Translates the scenario back into an :class:`RtoProblem` and calls
   :func:`markets.rto.solve`, then flattens the
   results (settlement + dispatch + violations) for the browser.

Scenario JSON is also the native save/load format — a scenario round-
tripped through ``/api/solve`` and back to the sidebar yields the same
market inputs byte-for-byte.
"""

from __future__ import annotations

import json
import logging
import re
import tempfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

from surge.market import ZonalRequirement

from markets.rto import (
    GeneratorOfferSchedule,
    RtoPolicy,
    RtoProblem,
    solve,
)
from markets.rto.config import default_reserve_products

logger = logging.getLogger("dashboards.rto.api")

REPO_ROOT = Path(__file__).resolve().parents[2]
CASES_DIR = REPO_ROOT / "examples" / "cases"


# ---------------------------------------------------------------------------
# Built-in case registry
# ---------------------------------------------------------------------------


@dataclass
class CaseContext:
    """Loaded case: the network plus optional timeline / load seed data.

    ``load_forecast_mw`` / ``renewable_caps_mw`` are only populated when
    the case file ships its own per-period profiles (GO-C3 problem
    archives do; the IEEE cases don't). The scaffold synthesizer falls
    back to its default profile shapes when these are absent.
    """

    network: Any
    period_durations_hours: list[float] | None = None
    load_forecast_mw: dict[int, list[float]] | None = None
    renewable_caps_mw: dict[str, list[float]] | None = None


@dataclass
class BuiltinCase:
    """One case the UI case-selector can load directly."""

    id: str
    title: str
    size: str  # 'small' | 'medium' | 'large'
    family: str  # 'ieee' | 'goc3'
    source: str
    loader: Callable[[], CaseContext]


def _load_surge_json_zst(path: Path) -> Callable[[], CaseContext]:
    def _load() -> CaseContext:
        import surge  # type: ignore
        if not path.is_file():
            raise FileNotFoundError(f"case file missing: {path}")
        return CaseContext(network=surge.load(str(path)))
    return _load


def _factory(name: str) -> Callable[[], CaseContext]:
    def _load() -> CaseContext:
        import surge  # type: ignore
        fn = getattr(surge, name)
        return CaseContext(network=fn())
    return _load


def _goc3_bus_name_to_number(bus_uid: str) -> int | None:
    """Map a GO-C3 bus identifier to Surge's 1-indexed bus number.

    GO-C3 problem files use 0-indexed names (``bus_00``, …, ``bus_72``)
    while the surge-native loader indexes 1-based (1 .. 73). So we add 1
    after parsing the digits out of the uid.
    """
    m = re.search(r"(\d+)", bus_uid)
    return (int(m.group(1)) + 1) if m else None


def _load_goc3_context(
    network_path: Path, problem_path: Path
) -> CaseContext:
    """Load a GO-C3 pair: network + companion ``.goc3-problem.json.zst``.

    Pulls consumer devices' ``p_ub`` profiles out of
    ``time_series_input.simple_dispatchable_device`` and emits them as a
    per-bus load forecast in MW (multiplying by ``base_norm_mva``).
    """
    import surge  # type: ignore
    try:
        import zstandard as zstd  # type: ignore
    except ImportError:
        logger.warning("zstandard not installed; GO-C3 problem data not loaded")
        return CaseContext(network=surge.load(str(network_path)))

    network = surge.load(str(network_path))
    if not problem_path.is_file():
        return CaseContext(network=network)

    with open(problem_path, "rb") as fh:
        raw = zstd.ZstdDecompressor().stream_reader(fh).read()
    doc = json.loads(raw)

    tsi = doc.get("time_series_input", {})
    net_block = doc.get("network", {})
    base_mva = float(net_block.get("general", {}).get("base_norm_mva") or 100.0)
    durations = list(tsi.get("general", {}).get("interval_duration") or [])
    tsi_by_uid = {d["uid"]: d for d in tsi.get("simple_dispatchable_device", [])}

    load_forecast: dict[int, list[float]] = {}
    for dev in net_block.get("simple_dispatchable_device", []):
        if dev.get("device_type") != "consumer":
            continue
        bus_num = _goc3_bus_name_to_number(str(dev.get("bus") or ""))
        if bus_num is None:
            continue
        tsi_entry = tsi_by_uid.get(dev["uid"])
        if not tsi_entry or "p_ub" not in tsi_entry:
            continue
        mw_series = [float(v) * base_mva for v in tsi_entry["p_ub"]]
        # Multiple consumers can sit on the same bus — sum them.
        existing = load_forecast.get(bus_num)
        if existing is None:
            load_forecast[bus_num] = mw_series
        else:
            for i in range(min(len(existing), len(mw_series))):
                existing[i] += mw_series[i]

    return CaseContext(
        network=network,
        period_durations_hours=durations if durations else None,
        load_forecast_mw=load_forecast or None,
    )


def _goc3_loader(network_path: Path) -> Callable[[], CaseContext]:
    problem_path = network_path.parent / network_path.name.replace(
        ".surge.json.zst", ".goc3-problem.json.zst"
    )
    def _load() -> CaseContext:
        return _load_goc3_context(network_path, problem_path)
    return _load


BUILTIN_CASES: tuple[BuiltinCase, ...] = (
    BuiltinCase("case9", "IEEE 9-bus", "small", "ieee", "surge.case9()", _factory("case9")),
    BuiltinCase("case14", "IEEE 14-bus", "small", "ieee", "surge.case14()", _factory("case14")),
    BuiltinCase("case30", "IEEE 30-bus", "small", "ieee", "surge.case30()", _factory("case30")),
    BuiltinCase("case57", "IEEE 57-bus", "small", "ieee", "surge.case57()", _factory("case57")),
    BuiltinCase("case118", "IEEE 118-bus", "medium", "ieee", "surge.case118()", _factory("case118")),
    BuiltinCase("case300", "IEEE 300-bus", "large", "ieee", "surge.case300()", _factory("case300")),
    BuiltinCase(
        "goc3_73",
        "GO-C3 73-bus (event4 D1 303)",
        "medium",
        "goc3",
        "examples/cases/go_c3_event4_73_d1_303_sw0/",
        _goc3_loader(
            CASES_DIR
            / "go_c3_event4_73_d1_303_sw0"
            / "go_c3_event4_73_d1_303_sw0.surge.json.zst"
        ),
    ),
    BuiltinCase(
        "goc3_617",
        "GO-C3 617-bus (event4 D1 921)",
        "large",
        "goc3",
        "examples/cases/go_c3_event4_617_d1_921_sw0/",
        _goc3_loader(
            CASES_DIR
            / "go_c3_event4_617_d1_921_sw0"
            / "go_c3_event4_617_d1_921_sw0.surge.json.zst"
        ),
    ),
)


def available_cases() -> list[dict[str, Any]]:
    """Serializable summary of the case registry for the ``/api/meta`` endpoint."""
    return [
        {
            "id": c.id,
            "title": c.title,
            "size": c.size,
            "family": c.family,
            "source": c.source,
        }
        for c in BUILTIN_CASES
    ]


def get_case(case_id: str) -> BuiltinCase:
    for c in BUILTIN_CASES:
        if c.id == case_id:
            return c
    raise KeyError(f"unknown case id: {case_id!r}")


# ---------------------------------------------------------------------------
# Profile shapes
# ---------------------------------------------------------------------------


#: 24-point per-hour normalized load shapes. Each is resampled to the
#: scenario's actual period count via nearest-neighbour.
LOAD_PROFILE_SHAPES: dict[str, list[float]] = {
    "flat": [1.0] * 24,
    "duck": [
        0.70, 0.65, 0.60, 0.55, 0.50, 0.50, 0.55, 0.65,
        0.75, 0.80, 0.80, 0.75, 0.70, 0.70, 0.72, 0.78,
        0.90, 1.00, 0.95, 0.85, 0.80, 0.75, 0.70, 0.68,
    ],
    "peak": [
        0.55, 0.50, 0.48, 0.47, 0.47, 0.50, 0.60, 0.80,
        0.85, 0.82, 0.78, 0.75, 0.72, 0.70, 0.70, 0.75,
        0.90, 1.00, 0.97, 0.88, 0.78, 0.68, 0.60, 0.55,
    ],
}

#: Per-period renewable availability shapes (solar rises in the morning
#: and peaks mid-day; wind peaks overnight). Normalized to [0, 1] where
#: 1.0 = full nameplate capacity.
RENEWABLE_PROFILE_SHAPES: dict[str, list[float]] = {
    "flat": [1.0] * 24,
    "solar": [
        0.00, 0.00, 0.00, 0.00, 0.00, 0.05, 0.15, 0.35,
        0.55, 0.75, 0.90, 0.98, 1.00, 0.98, 0.90, 0.75,
        0.55, 0.35, 0.15, 0.05, 0.00, 0.00, 0.00, 0.00,
    ],
    "wind": [
        0.85, 0.88, 0.90, 0.92, 0.90, 0.85, 0.75, 0.60,
        0.50, 0.45, 0.42, 0.40, 0.40, 0.42, 0.48, 0.55,
        0.60, 0.65, 0.72, 0.78, 0.82, 0.85, 0.88, 0.88,
    ],
}


def _resample(values: list[float], n: int) -> list[float]:
    if n <= 0 or not values:
        return []
    if len(values) == n:
        return list(values)
    out = [0.0] * n
    for i in range(n):
        src_idx = min(len(values) - 1, int(round(i * len(values) / n)))
        out[i] = float(values[src_idx])
    return out


def _profile_values(name: str, n: int, default: str = "flat") -> list[float]:
    """Return ``n`` per-period multipliers for shape ``name``."""
    shapes = LOAD_PROFILE_SHAPES
    if name not in shapes:
        name = default
    return _resample(shapes[name], n)


def _renewable_profile_values(name: str, n: int, default: str = "flat") -> list[float]:
    shapes = RENEWABLE_PROFILE_SHAPES
    if name not in shapes:
        name = default
    return _resample(shapes[name], n)


# ---------------------------------------------------------------------------
# Scaffold synthesis
# ---------------------------------------------------------------------------


def _is_renewable(gen: Any) -> bool:
    fuel = str(getattr(gen, "fuel_type", "") or "").lower()
    return any(k in fuel for k in ("solar", "wind", "pv"))


def _bus_nominal_load(network: Any) -> dict[int, float]:
    """Sum ``active_power_demand_mw`` per bus for the network's loads."""
    out: dict[int, float] = {}
    for load in network.loads:
        if not getattr(load, "in_service", True):
            continue
        bus = int(load.bus)
        out[bus] = out.get(bus, 0.0) + float(load.pd_mw)
    return out


def _topology_layout(network: Any, *, iterations: int | None = None) -> dict[str, Any]:
    """Compute an (x, y) position per bus plus the branch adjacency list.

    Uses a small deterministic Fruchterman-Reingold spring embedding so
    the layout is reproducible for the same network. Output is normalised
    to ``[0, 1] × [0, 1]`` so the frontend can rescale it to any viewport.
    Branches beyond ~2000 are dropped from the response to keep the
    payload small — the Grid tab degrades to bus dots only for very
    large cases.
    """
    import math
    import random

    buses = list(network.buses)
    bus_nums = [int(b.number) for b in buses]
    bus_idx = {bn: i for i, bn in enumerate(bus_nums)}
    edges_raw: list[tuple[int, int]] = []
    for br in network.branches:
        if not getattr(br, "in_service", True):
            continue
        fb, tb = int(br.from_bus), int(br.to_bus)
        if fb in bus_idx and tb in bus_idx and fb != tb:
            edges_raw.append((bus_idx[fb], bus_idx[tb]))

    n = len(buses)
    if n == 0:
        return {"buses": [], "branches": [], "note": "empty network"}

    # Deterministic seed so the same case always lays out identically.
    rng = random.Random(0xD15A7C)
    pos = [(rng.random(), rng.random()) for _ in range(n)]

    # Fruchterman-Reingold. Linear per iteration; ~150 iterations is
    # plenty for n ≤ a few hundred.
    if iterations is None:
        iterations = max(60, min(200, 2500 // max(1, n)))
    area = 1.0
    k = math.sqrt(area / max(1, n))
    t = 0.1
    cool = 0.95
    for _ in range(iterations):
        disp = [[0.0, 0.0] for _ in range(n)]
        # Repulsion
        for i in range(n):
            xi, yi = pos[i]
            for j in range(i + 1, n):
                dx = xi - pos[j][0]
                dy = yi - pos[j][1]
                d2 = dx * dx + dy * dy
                if d2 < 1e-9:
                    dx = rng.random() * 1e-3
                    dy = rng.random() * 1e-3
                    d2 = dx * dx + dy * dy
                d = math.sqrt(d2)
                force = (k * k) / d
                fx = (dx / d) * force
                fy = (dy / d) * force
                disp[i][0] += fx; disp[i][1] += fy
                disp[j][0] -= fx; disp[j][1] -= fy
        # Attraction along edges
        for a, b in edges_raw:
            dx = pos[a][0] - pos[b][0]
            dy = pos[a][1] - pos[b][1]
            d = math.sqrt(dx * dx + dy * dy) or 1e-9
            force = (d * d) / k
            fx = (dx / d) * force
            fy = (dy / d) * force
            disp[a][0] -= fx; disp[a][1] -= fy
            disp[b][0] += fx; disp[b][1] += fy
        # Apply displacement with temperature cap
        for i in range(n):
            dx, dy = disp[i]
            d = math.sqrt(dx * dx + dy * dy) or 1e-9
            step = min(d, t) / d
            nx = pos[i][0] + dx * step
            ny = pos[i][1] + dy * step
            # Keep in unit box
            pos[i] = (max(0.0, min(1.0, nx)), max(0.0, min(1.0, ny)))
        t *= cool

    # Normalise to full [0, 1] × [0, 1] extent.
    xs = [p[0] for p in pos]; ys = [p[1] for p in pos]
    xmin, xmax = min(xs), max(xs)
    ymin, ymax = min(ys), max(ys)
    xspan = max(1e-6, xmax - xmin)
    yspan = max(1e-6, ymax - ymin)
    buses_out = []
    for i, b in enumerate(buses):
        buses_out.append({
            "number": bus_nums[i],
            "x": (pos[i][0] - xmin) / xspan,
            "y": (pos[i][1] - ymin) / yspan,
        })

    # For very large networks, drop the edge list to keep payload small;
    # the UI falls back to bus dots only.
    branches_out = [{"from": bus_nums[a], "to": bus_nums[b]} for a, b in edges_raw[:2500]]
    note = None if len(edges_raw) <= 2500 else f"{len(edges_raw) - 2500} branches omitted (truncated)"

    return {"buses": buses_out, "branches": branches_out, "note": note}


def _network_summary(network: Any) -> dict[str, Any]:
    bus_count = len(network.buses)
    gens = list(network.generators)
    loads = list(network.loads)
    branches = list(network.branches)
    nom_load = _bus_nominal_load(network)
    return {
        "buses": bus_count,
        "generators": len(gens),
        "loads": len(loads),
        "branches": len(branches),
        "total_load_mw": float(sum(nom_load.values())),
        "total_capacity_mw": float(sum(getattr(g, "pmax_mw", 0.0) for g in gens)),
        "renewable_capacity_mw": float(
            sum(getattr(g, "pmax_mw", 0.0) for g in gens if _is_renewable(g))
        ),
    }


def build_scaffold(case_id: str | None = None, *, network: Any | None = None,
                   case_title: str | None = None) -> dict[str, Any]:
    """Synthesize a default scenario dict from a case id or loaded network.

    Called by ``GET /api/cases/{id}/scaffold``. The returned dict is
    what the frontend stores in ``state.scenario`` and POSTs back to
    ``/api/solve``. It contains every knob a user can tweak.
    """
    ctx_durations: list[float] | None = None
    ctx_load_forecast: dict[int, list[float]] | None = None
    if network is None:
        if case_id is None:
            raise ValueError("either case_id or network must be provided")
        case = get_case(case_id)
        loaded = case.loader()
        # Back-compat: legacy loaders returned a raw Network.
        if isinstance(loaded, CaseContext):
            network = loaded.network
            ctx_durations = loaded.period_durations_hours
            ctx_load_forecast = loaded.load_forecast_mw
        else:
            network = loaded
        case_title = case.title
        source = {"kind": "builtin", "case_id": case.id, "title": case.title,
                  "family": case.family, "size": case.size}
    else:
        source = {"kind": "custom", "case_id": case_id or "custom",
                  "title": case_title or "custom network"}

    # Default horizon is 24 × 60 min. If the case file shipped its own
    # durations (GO-C3 problem archives do, 18 × 0.25 hr), honor them.
    if ctx_durations:
        default_periods = len(ctx_durations)
        # Round to the nearest common resolution; fall back to 60.
        dur_hr = ctx_durations[0] if ctx_durations else 1.0
        default_resolution_min = max(5, int(round(dur_hr * 60)))
    else:
        default_periods = 24
        default_resolution_min = 60
    now = datetime.now(timezone.utc).replace(
        hour=0, minute=0, second=0, microsecond=0, tzinfo=None
    )
    start_iso = now.isoformat(timespec="minutes")

    gens = []
    for g in network.generators:
        rid = g.resource_id
        gens.append({
            "resource_id": rid,
            "bus": int(g.bus),
            "pmin_mw": float(getattr(g, "pmin_mw", 0.0) or 0.0),
            "pmax_mw": float(getattr(g, "pmax_mw", 0.0) or 0.0),
            "in_service": bool(getattr(g, "in_service", True)),
            "fuel_type": getattr(g, "fuel_type", None),
            "is_renewable": _is_renewable(g),
            "cost_c0": float(getattr(g, "cost_c0", 0.0) or 0.0),
            "cost_c1": float(getattr(g, "cost_c1", 0.0) or 0.0),
            "cost_c2": float(getattr(g, "cost_c2", 0.0) or 0.0),
            "has_cost": bool(getattr(g, "has_cost", False)),
        })

    # Prefer the case-shipped per-period profile when it's there
    # (GO-C3 cases); otherwise pull each bus' single-period nominal
    # load out of the network. Both shapes feed ``loads`` — the
    # solver-side builder picks the per-period profile when present.
    if ctx_load_forecast:
        loads = []
        for bus in sorted(ctx_load_forecast):
            prof = list(ctx_load_forecast[bus])
            loads.append({
                "bus": int(bus),
                "nominal_mw": max(prof) if prof else 0.0,
                "profile_mw": prof,
            })
    else:
        nom = _bus_nominal_load(network)
        loads = [{"bus": bus, "nominal_mw": mw} for bus, mw in sorted(nom.items())]

    summary = _network_summary(network)
    topology = _topology_layout(network)
    # When the case ships its own load profiles (GO-C3), rewrite the
    # total_load_mw to the peak across periods so the sidebar badge
    # doesn't read "0 MW load" on those networks.
    if ctx_load_forecast:
        peak = max(
            (sum(ctx_load_forecast[b][t] for b in ctx_load_forecast)
             for t in range(len(next(iter(ctx_load_forecast.values()))))),
            default=0.0,
        )
        summary["total_load_mw"] = float(peak)

    scenario = {
        "source": source,
        "network_summary": summary,
        "topology": topology,
        "generators": gens,
        "loads": loads,
        "time_axis": {
            "start_iso": start_iso,
            "periods": default_periods,
            "resolution_minutes": default_resolution_min,
            "horizon_minutes": default_periods * default_resolution_min,
        },
        "load_config": {
            # Global default. Per-bus entries override.
            "handling": "fixed",        # 'fixed' | 'dispatchable'
            "profile_shape": "duck",    # 'flat' | 'duck' | 'peak' | 'custom'
            "default_voll_per_mwh": 9000.0,
            "per_bus": {},              # {"<bus>": {"handling": ..., "voll_per_mwh": ..., "profile_shape": ..., "custom_profile": [...]}}
            "custom_profile": None,     # global custom per-period multipliers when profile_shape == 'custom'
        },
        "offers_config": {
            "synthesis": "from_cost_coeffs",  # 'from_cost_coeffs' | 'flat_tiered'
            "flat_prices": [20.0, 40.0, 80.0],
            "flat_fractions": [0.33, 0.67, 1.0],
            "per_gen": {},              # {"<rid>": [[mw, price], ...]}
        },
        "renewables_config": {
            "profile_shape": "solar",  # nearly all listed renewable gens are PV in the IEEE set
            "per_gen": {},              # {"<rid>": {"profile_shape": ..., "custom_profile": [...]}}
        },
        "reserves_config": {
            "zone_id": 1,  # system-wide single zone for MVP
            "products": {
                "reg_up":   {"percent_of_peak": 5.0, "absolute_mw": None},
                "reg_down": {"percent_of_peak": 5.0, "absolute_mw": None},
                "syn":      {"percent_of_peak": 3.0, "absolute_mw": None},
                "nsyn":     {"percent_of_peak": 2.0, "absolute_mw": None},
            },
        },
        "policy": {
            "commitment_mode": "optimize",
            "lp_solver": "highs",
            "mip_gap": 0.001,
            "time_limit_secs": None,
            "run_pricing": True,
            "voll_per_mwh": 9000.0,
            "thermal_overload_per_mwh": 5000.0,
            "reserve_shortfall_per_mwh": 1000.0,
        },
    }
    return scenario


# ---------------------------------------------------------------------------
# Scenario → RtoProblem translation
# ---------------------------------------------------------------------------


def _bus_load_profile(
    bus: int,
    nominal_mw: float,
    n: int,
    load_cfg: dict[str, Any],
) -> list[float]:
    """Per-period MW demand for one bus combining the global or per-bus shape."""
    per_bus = load_cfg.get("per_bus") or {}
    override = per_bus.get(str(bus), {})
    shape_name = override.get("profile_shape") or load_cfg.get("profile_shape") or "flat"
    if shape_name == "custom":
        # Custom per-period multiplier, sourced from the per-bus override first
        # then the global custom profile.
        custom = override.get("custom_profile") or load_cfg.get("custom_profile")
        if custom and len(custom) > 0:
            vals = _resample([float(v) for v in custom], n)
        else:
            vals = [1.0] * n
    else:
        vals = _profile_values(shape_name, n)
    return [nominal_mw * v for v in vals]


def _synthesize_offer_curve(gen_meta: dict[str, Any], offers_cfg: dict[str, Any]) -> list[tuple[float, float]]:
    """Build a 3-segment (cumulative MW, $/MWh) curve for one generator.

    ``from_cost_coeffs`` samples the marginal cost ``dC/dp = c1 + 2·c2·p``
    at three breakpoints across ``[pmin, pmax]``. ``flat_tiered`` ignores
    the network cost and slots three fixed price tiers at
    ``flat_fractions`` of pmax.
    """
    pmin = float(gen_meta.get("pmin_mw") or 0.0)
    pmax = float(gen_meta.get("pmax_mw") or 0.0)
    if pmax <= pmin:
        return [(max(pmax, 0.01), 100.0)]
    synthesis = offers_cfg.get("synthesis", "from_cost_coeffs")
    fractions = offers_cfg.get("flat_fractions") or [0.33, 0.67, 1.0]
    prices = offers_cfg.get("flat_prices") or [20.0, 40.0, 80.0]
    pairs: list[tuple[float, float]] = []
    if synthesis == "from_cost_coeffs" and gen_meta.get("has_cost"):
        c1 = float(gen_meta.get("cost_c1") or 0.0)
        c2 = float(gen_meta.get("cost_c2") or 0.0)
        for f in fractions:
            mw = pmin + f * (pmax - pmin)
            mc = max(0.0, c1 + 2.0 * c2 * mw)
            pairs.append((mw, mc))
    else:
        for f, price in zip(fractions, prices):
            mw = pmin + f * (pmax - pmin)
            pairs.append((mw, float(price)))
    # Sanity: strictly increasing MW breakpoints.
    cleaned: list[tuple[float, float]] = []
    last_mw = -1e-9
    for mw, pr in pairs:
        if mw <= last_mw:
            mw = last_mw + 1e-6
        cleaned.append((mw, pr))
        last_mw = mw
    return cleaned


def _per_gen_offer_segments(
    gen_meta: dict[str, Any],
    offers_cfg: dict[str, Any],
) -> list[tuple[float, float]]:
    """Per-gen override falls back to synthesized."""
    per_gen = offers_cfg.get("per_gen") or {}
    override = per_gen.get(gen_meta["resource_id"])
    if override and len(override) > 0:
        return [(float(mw), float(price)) for mw, price in override]
    return _synthesize_offer_curve(gen_meta, offers_cfg)


def _build_rto_problem(scenario: dict[str, Any]) -> tuple[RtoProblem, Any]:
    import surge  # type: ignore

    source = scenario.get("source", {})
    case_id = source.get("case_id")
    # Always rebuild the network fresh so each solve starts clean.
    case_ctx: CaseContext | None = None
    if source.get("kind") == "builtin" and case_id:
        loaded = get_case(case_id).loader()
        if isinstance(loaded, CaseContext):
            case_ctx = loaded
            network = loaded.network
        else:
            network = loaded
    elif source.get("kind") == "custom" and scenario.get("network_payload"):
        # Custom network uploaded as a serialized Surge JSON payload.
        with tempfile.NamedTemporaryFile(suffix=".surge.json", mode="w", delete=False) as fh:
            fh.write(scenario["network_payload"])
            tmp = Path(fh.name)
        network = surge.load(str(tmp))
        tmp.unlink(missing_ok=True)
    else:
        raise ValueError("scenario source missing — provide a builtin case id or network_payload")

    t = scenario.get("time_axis") or {}
    resolution_min = int(t.get("resolution_minutes") or 60)
    periods = int(t.get("periods") or (int(t.get("horizon_minutes") or 1440) // resolution_min))
    duration_hr = resolution_min / 60.0
    period_durations = [duration_hr] * periods

    # ── Load forecast ──
    # Three sources, in priority order:
    #   1) scenario['loads'] with ``profile_mw`` — already per-period
    #      (case-shipped from e.g. GO-C3 problem files).
    #   2) scenario['loads'] with ``nominal_mw`` — scale by the active
    #      load profile shape from ``load_config``.
    #   3) fall back to ``_bus_nominal_load(network)`` when neither is
    #      present (covers round-tripped scenarios where ``loads`` got
    #      stripped).
    load_cfg = scenario.get("load_config") or {}
    load_forecast: dict[int, list[float]] = {}
    scen_loads = scenario.get("loads") or []
    if scen_loads:
        for row in scen_loads:
            bus = int(row["bus"])
            prof = row.get("profile_mw")
            if prof:
                # Resize to requested horizon.
                arr = [float(v) for v in prof]
                if len(arr) != periods:
                    arr = _resample(arr, periods) if len(arr) > 0 else [0.0] * periods
                load_forecast[bus] = arr
            else:
                nominal_mw = float(row.get("nominal_mw") or 0.0)
                load_forecast[bus] = _bus_load_profile(bus, nominal_mw, periods, load_cfg)
    else:
        for bus, mw in _bus_nominal_load(network).items():
            load_forecast[int(bus)] = _bus_load_profile(bus, mw, periods, load_cfg)

    # ── Renewable caps ──
    ren_cfg = scenario.get("renewables_config") or {}
    renewable_caps: dict[str, list[float]] = {}
    gens_meta = {g["resource_id"]: g for g in (scenario.get("generators") or [])}
    # Re-derive from the fresh network because resource ids are stable.
    for g in network.generators:
        rid = g.resource_id
        meta = gens_meta.get(rid) or {
            "resource_id": rid,
            "pmax_mw": float(getattr(g, "pmax_mw", 0.0) or 0.0),
            "is_renewable": _is_renewable(g),
        }
        if not meta.get("is_renewable"):
            continue
        pmax = float(meta.get("pmax_mw") or 0.0)
        per_gen = (ren_cfg.get("per_gen") or {}).get(rid, {})
        shape = per_gen.get("profile_shape") or ren_cfg.get("profile_shape") or "flat"
        if shape == "custom":
            custom = per_gen.get("custom_profile") or []
            if custom:
                prof = _resample([float(v) for v in custom], periods)
            else:
                prof = [1.0] * periods
        else:
            prof = _renewable_profile_values(shape, periods)
        renewable_caps[rid] = [pmax * v for v in prof]

    # ── Reserve requirements ──
    res_cfg = scenario.get("reserves_config") or {}
    zone_id = int(res_cfg.get("zone_id") or 1)
    peak_load = max(
        (sum(load_forecast[b][t] for b in load_forecast) for t in range(periods)),
        default=0.0,
    ) if load_forecast else 0.0
    reserve_requirements: list[ZonalRequirement] = []
    for pid, spec in (res_cfg.get("products") or {}).items():
        if not spec:
            continue
        abs_mw = spec.get("absolute_mw")
        if abs_mw is not None:
            req_mw = float(abs_mw)
        else:
            req_mw = peak_load * (float(spec.get("percent_of_peak") or 0.0) / 100.0)
        if req_mw <= 1e-9:
            continue
        reserve_requirements.append(
            ZonalRequirement(
                zone_id=zone_id,
                product_id=str(pid),
                requirement_mw=req_mw,
                per_period_mw=[req_mw] * periods,
            )
        )

    # ── Energy offer schedules ──
    # Include ``no_load_cost`` explicitly (= c0 if the network has one,
    # else 0) — the Rust wire format requires the field whenever we
    # ship a generator_offer_schedule.
    offers_cfg = scenario.get("offers_config") or {}
    energy_offers: list[GeneratorOfferSchedule] = []
    for meta in scenario.get("generators") or []:
        rid = meta["resource_id"]
        segs = _per_gen_offer_segments(meta, offers_cfg)
        no_load = float(meta.get("cost_c0") or 0.0) if meta.get("has_cost") else 0.0
        energy_offers.append(
            GeneratorOfferSchedule(
                resource_id=rid,
                segments_by_period=[segs for _ in range(periods)],
                no_load_cost_by_period=[no_load for _ in range(periods)],
                startup_cost_tiers=[],
            )
        )

    problem = RtoProblem.from_dicts(
        network,
        period_durations_hours=period_durations,
        load_forecast_mw=load_forecast,
        renewable_caps_mw=renewable_caps,
        reserve_requirements=reserve_requirements,
        energy_offers=energy_offers,
    )
    return problem, network


def _policy_from_scenario(scenario: dict[str, Any]) -> RtoPolicy:
    p = scenario.get("policy") or {}
    return RtoPolicy(
        lp_solver=str(p.get("lp_solver") or "highs"),
        mip_gap=float(p.get("mip_gap") or 1e-3),
        time_limit_secs=(float(p["time_limit_secs"]) if p.get("time_limit_secs") else None),
        commitment_mode=str(p.get("commitment_mode") or "optimize"),
        run_pricing=bool(p.get("run_pricing", True)),
        voll_per_mwh=float(p.get("voll_per_mwh") or 9000.0),
        thermal_overload_cost_per_mwh=float(p.get("thermal_overload_per_mwh") or 5000.0),
        reserve_shortfall_cost_per_mwh=float(p.get("reserve_shortfall_per_mwh") or 1000.0),
    )


# ---------------------------------------------------------------------------
# Solve + result flattening
# ---------------------------------------------------------------------------


class _LogCapture:
    """Capture Python log messages emitted during a solve.

    Attaches a ``StringIO`` handler to the ``markets.rto`` + dashboards
    loggers for the lifetime of a ``with`` block, then returns the
    captured text so the frontend can display it in the Log tab. Keeps
    the global log-level untouched.
    """

    def __init__(self, logger_names: tuple[str, ...] = ("markets.rto", "dashboards.rto")):
        import io as _io
        self._buf = _io.StringIO()
        self._handler = logging.StreamHandler(self._buf)
        self._handler.setLevel(logging.INFO)
        self._handler.setFormatter(
            logging.Formatter("%(asctime)s %(levelname)-5s %(name)s · %(message)s",
                              datefmt="%H:%M:%S")
        )
        self._loggers = [logging.getLogger(n) for n in logger_names]
        self._prev_levels: list[int] = []

    def __enter__(self) -> "_LogCapture":
        for lg in self._loggers:
            self._prev_levels.append(lg.level)
            if lg.level > logging.INFO or lg.level == logging.NOTSET:
                lg.setLevel(logging.INFO)
            lg.addHandler(self._handler)
        return self

    def __exit__(self, *exc) -> None:
        for lg, level in zip(self._loggers, self._prev_levels):
            lg.removeHandler(self._handler)
            lg.setLevel(level)

    def text(self) -> str:
        return self._buf.getvalue()


def run_solve(scenario: dict[str, Any]) -> dict[str, Any]:
    """Build an RtoProblem from the scenario and run the day-ahead solve."""
    problem, network = _build_rto_problem(scenario)
    policy = _policy_from_scenario(scenario)
    with tempfile.TemporaryDirectory(prefix="surge-rto-dash-") as tmp:
        with _LogCapture() as cap:
            logger.info(
                "RTO solve · case=%s · periods=%d · commitment=%s · solver=%s",
                (scenario.get("source") or {}).get("case_id", "?"),
                problem.periods,
                policy.commitment_mode,
                policy.lp_solver,
            )
            report = solve(
                problem,
                Path(tmp),
                policy=policy,
                label="dashboard",
            )
        workdir = Path(tmp)
        status = report.get("status")
        solve_log = cap.text()
        if status != "ok":
            return {
                "status": status,
                "error": report.get("error"),
                "elapsed_secs": report.get("elapsed_secs"),
                "settlement_summary": None,
                "settlement": None,
                "dispatch": None,
                "violations": [],
                "solve_log": solve_log,
            }
        settlement = json.loads((workdir / "settlement.json").read_text(encoding="utf-8"))
        dispatch = json.loads((workdir / "dispatch-result.json").read_text(encoding="utf-8"))

    flat = _flatten_results(report, settlement, dispatch, problem, scenario)
    flat["solve_log"] = solve_log
    return flat


def _compute_branch_flows(
    dispatch: dict[str, Any], network: Any
) -> list[list[dict[str, Any]]]:
    """Derive per-period DC branch flows from the solved bus angles.

    The native dispatch result doesn't serialize per-branch flows, but
    for a DC-OPF clearing we can recover them from the bus angles the
    solver publishes:

        flow_pu = (θ_from − θ_to − phase_shift) / (x_pu × tap)
        flow_mw = flow_pu × base_mva

    Utilization is ``|flow_mw| / rate_a_mva`` when the branch ships a
    rating (GO-C3 / case30 do; most IEEE cases don't). ``rating_mva``
    defaults to 0 in that case, and the Grid tab falls back to
    coloring by normalized flow magnitude.
    """
    import math
    base_mva = float(getattr(network, "base_mva", 100.0) or 100.0)
    branches: list[tuple[int, int, float, float, float, float]] = []
    for br in network.branches:
        if not getattr(br, "in_service", True):
            continue
        x_pu = float(getattr(br, "x_pu", 0.0) or 0.0)
        if abs(x_pu) < 1e-12:
            continue
        fb = int(br.from_bus)
        tb = int(br.to_bus)
        tap = float(getattr(br, "tap", 1.0) or 1.0) or 1.0
        shift = math.radians(float(getattr(br, "shift_deg", 0.0) or 0.0))
        rating = float(getattr(br, "rate_a_mva", 0.0) or 0.0)
        branches.append((fb, tb, x_pu, tap, shift, rating))

    out: list[list[dict[str, Any]]] = []
    for per in dispatch.get("periods") or []:
        angles: dict[int, float] = {}
        for b in per.get("bus_results") or []:
            angles[int(b["bus_number"])] = float(b.get("angle_rad") or 0.0)
        rows: list[dict[str, Any]] = []
        for fb, tb, x_pu, tap, shift, rating in branches:
            theta_f = angles.get(fb, 0.0)
            theta_t = angles.get(tb, 0.0)
            flow_pu = (theta_f - theta_t - shift) / (x_pu * tap)
            flow_mw = flow_pu * base_mva
            util = (abs(flow_mw) / rating) if rating > 0 else None
            rows.append({
                "from": fb,
                "to": tb,
                "flow_mw": flow_mw,
                "rating_mva": rating,
                "utilization": util,
            })
        out.append(rows)
    return out


def _flatten_results(
    report: dict[str, Any],
    settlement: dict[str, Any],
    dispatch: dict[str, Any],
    problem: RtoProblem,
    scenario: dict[str, Any],
) -> dict[str, Any]:
    """Collapse the raw report + settlement + dispatch-result into a single
    frontend-shaped dict. Keeps the fields the dashboard needs and drops
    the rest so the JSON payload stays small."""
    n_periods = int((report.get("extras") or {}).get("periods") or 0)
    totals = settlement["totals"]

    # LMPs per bus × period, plus mean / peak / min per period.
    lmps_by_bus: dict[int, list[float]] = {}
    lmp_period_mean: list[float] = []
    lmp_period_peak: list[float] = []
    lmp_period_min: list[float] = []
    for per in settlement["lmps_per_period"]:
        ps: list[float] = []
        for b in per["buses"]:
            bus_num = int(b["bus_number"])
            lmp = float(b.get("lmp") or 0.0)
            lmps_by_bus.setdefault(bus_num, []).append(lmp)
            ps.append(lmp)
        lmp_period_mean.append(sum(ps) / len(ps) if ps else 0.0)
        lmp_period_peak.append(max(ps) if ps else 0.0)
        lmp_period_min.append(min(ps) if ps else 0.0)

    # Per-generator dispatch trace.
    gens_by_rid: dict[str, dict[str, Any]] = {}
    for meta in scenario.get("generators") or []:
        gens_by_rid[meta["resource_id"]] = {
            "resource_id": meta["resource_id"],
            "bus": int(meta["bus"]),
            "pmin_mw": float(meta.get("pmin_mw") or 0.0),
            "pmax_mw": float(meta.get("pmax_mw") or 0.0),
            "is_renewable": bool(meta.get("is_renewable")),
            "fuel_type": meta.get("fuel_type"),
            "power_mw": [0.0] * n_periods,
            "commitment": [False] * n_periods,
            "energy_cost_dollars": [0.0] * n_periods,
            "revenue_dollars": [0.0] * n_periods,
        }

    for row in settlement["energy_awards"]:
        rid = row["resource_id"]
        t = int(row["period"])
        rec = gens_by_rid.setdefault(
            rid,
            {
                "resource_id": rid,
                "bus": int(row.get("bus_number") or 0),
                "pmin_mw": 0.0,
                "pmax_mw": 0.0,
                "is_renewable": False,
                "fuel_type": None,
                "power_mw": [0.0] * n_periods,
                "commitment": [False] * n_periods,
                "energy_cost_dollars": [0.0] * n_periods,
                "revenue_dollars": [0.0] * n_periods,
            },
        )
        rec["power_mw"][t] = float(row.get("power_mw") or 0.0)
        rec["energy_cost_dollars"][t] = float(row.get("energy_cost_dollars") or 0.0)
        rec["revenue_dollars"][t] = float(row.get("payment_dollars") or 0.0)
        if rec["power_mw"][t] > 1e-6:
            rec["commitment"][t] = True

    # Raw commitment from the native dispatch result (catches idle committed gens too).
    for t, per in enumerate(dispatch.get("periods") or []):
        for res in per.get("resource_results") or []:
            if res.get("kind") != "generator":
                continue
            rid = res.get("resource_id")
            if rid in gens_by_rid:
                status = res.get("commitment_status")
                if status is not None:
                    gens_by_rid[rid]["commitment"][t] = str(status).lower() in {
                        "committed", "on", "up", "running", "true", "1"
                    }

    generators = list(gens_by_rid.values())

    # Per-bus load served: for now loads are always fixed = forecast.
    loads = []
    load_forecast = problem.load_forecast_mw
    for bus in sorted(load_forecast.keys()):
        forecast = load_forecast[bus]
        loads.append({
            "bus": int(bus),
            "nominal_mw": forecast,
            "served_mw": list(forecast),
            "shed_mw": [0.0] * n_periods,
            "handling": "fixed",
        })

    # Reserve awards collapsed to per-product × per-period.
    reserve_by_product: dict[str, dict[str, Any]] = {}
    for row in settlement["as_awards"]:
        pid = str(row["product_id"])
        rec = reserve_by_product.setdefault(pid, {
            "product_id": pid,
            "zone_id": int(row.get("zone_id") or 0),
            "requirement_mw": [0.0] * n_periods,
            "provided_mw": [0.0] * n_periods,
            "shortfall_mw": [0.0] * n_periods,
            "clearing_price": [0.0] * n_periods,
            "payment_dollars": [0.0] * n_periods,
        })
        t = int(row["period"])
        rec["requirement_mw"][t] = float(row.get("requirement_mw") or 0.0)
        rec["provided_mw"][t] = float(row.get("provided_mw") or 0.0)
        rec["shortfall_mw"][t] = float(row.get("shortfall_mw") or 0.0)
        rec["clearing_price"][t] = float(row.get("clearing_price") or 0.0)
        rec["payment_dollars"][t] = float(row.get("payment_dollars") or 0.0)
    reserve_awards = list(reserve_by_product.values())

    # Violations (thermal + bus-balance shortages) pulled from dispatch.
    violations: list[dict[str, Any]] = []
    for t, per in enumerate(dispatch.get("periods") or []):
        for br in per.get("branch_results") or []:
            overload = float(br.get("overload_mw") or 0.0)
            if abs(overload) > 1e-6:
                violations.append({
                    "period": t,
                    "kind": "thermal",
                    "element": f"{br.get('from_bus')}→{br.get('to_bus')}",
                    "severity_mw": overload,
                })
        for bus in per.get("bus_results") or []:
            shortfall = float(bus.get("p_shortfall_mw") or 0.0)
            overgen = float(bus.get("p_overgen_mw") or 0.0)
            if abs(shortfall) > 1e-6:
                violations.append({
                    "period": t,
                    "kind": "load_shed",
                    "element": f"bus {bus.get('bus_number')}",
                    "severity_mw": shortfall,
                })
            if abs(overgen) > 1e-6:
                violations.append({
                    "period": t,
                    "kind": "overgen",
                    "element": f"bus {bus.get('bus_number')}",
                    "severity_mw": overgen,
                })
    total_shortfall = sum(v["severity_mw"] for v in violations if v["kind"] == "load_shed")
    total_overload = sum(abs(v["severity_mw"]) for v in violations if v["kind"] == "thermal")

    # Top-level summary — the numbers the overview cards read.
    mean_lmp = sum(lmp_period_mean) / len(lmp_period_mean) if lmp_period_mean else 0.0
    peak_lmp = max(lmp_period_peak) if lmp_period_peak else 0.0

    # Per-period DC branch flows + utilization for the Grid tab to
    # highlight binding / near-binding / breached lines.
    branch_flows_by_period = _compute_branch_flows(dispatch, problem.network)
    peak_utilization = 0.0
    for rows in branch_flows_by_period:
        for r in rows:
            u = r.get("utilization")
            if u is not None and u > peak_utilization:
                peak_utilization = u

    return {
        "status": "ok",
        "elapsed_secs": float(report.get("elapsed_secs") or 0.0),
        "periods": n_periods,
        "period_durations_hours": settlement["period_durations_hours"],
        "summary": {
            "production_cost_dollars": totals["production_cost_dollars"],
            "energy_payment_dollars": totals["energy_payment_dollars"],
            "load_payment_dollars": totals["load_payment_dollars"],
            "as_payment_dollars": totals["as_payment_dollars"],
            "shortfall_penalty_dollars": totals["shortfall_penalty_dollars"],
            "congestion_rent_dollars": totals["congestion_rent_dollars"],
            "mean_system_lmp": mean_lmp,
            "peak_system_lmp": peak_lmp,
            "total_load_shed_mw": total_shortfall,
            "total_thermal_overload_mw": total_overload,
            "peak_branch_utilization": peak_utilization,
        },
        "lmps_by_bus": {str(k): v for k, v in lmps_by_bus.items()},
        "lmp_aggregates": {
            "per_period_mean": lmp_period_mean,
            "per_period_peak": lmp_period_peak,
            "per_period_min": lmp_period_min,
        },
        "generators": generators,
        "loads": loads,
        "reserve_awards": reserve_awards,
        "violations": violations,
        "branch_flows_by_period": branch_flows_by_period,
    }


__all__ = [
    "BuiltinCase",
    "available_cases",
    "build_scaffold",
    "get_case",
    "run_solve",
    "LOAD_PROFILE_SHAPES",
    "RENEWABLE_PROFILE_SHAPES",
]
