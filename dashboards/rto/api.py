# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""RTO dashboard adapter: case registry, scaffold synthesizer, solve driver.

Every solve routes through :func:`surge.market.go_c3.solve_workflow`,
the canonical native two-stage SCUC → AC SCED pipeline (validator-
aligned bus-balance penalties, anchor-based dispatch pinning,
target-tracking feedback, reactive-support pin retry). The dashboard
loads a goc3 problem archive, translates the user's policy form into
:class:`surge.market.go_c3.MarketPolicy`, runs the native solve, then
flattens the resulting dispatch / settlement for the browser.

The dashboard's scenario JSON is the native save/load format — a
scenario round-tripped through ``/api/solve`` and back to the sidebar
re-solves with the same goc3 problem + same policy.
"""

from __future__ import annotations

import json
import logging
import re
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Generator

from .export import extract_settlement
from .policy import RtoPolicy

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

    ``goc3_problem_path`` is set when the case is a GO-C3 archive —
    the RTO workflow uses it to route the solve through the canonical
    :mod:`surge.market.go_c3` native pipeline so the AC SCED inherits
    go_c3's validator-aligned tuning.
    """

    network: Any
    period_durations_hours: list[float] | None = None
    load_forecast_mw: dict[int, list[float]] | None = None
    renewable_caps_mw: dict[str, list[float]] | None = None
    goc3_problem_path: Path | None = None


@dataclass
class BuiltinCase:
    """One case the UI case-selector can load directly."""

    id: str
    title: str
    size: str  # 'small' | 'medium' | 'large'
    family: str  # 'ieee' | 'goc3'
    source: str
    loader: Callable[[], CaseContext]


def _factory(name: str) -> Callable[[], CaseContext]:
    """Built-in IEEE case loader (``surge.case9()`` etc.)."""

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


def _decompress_goc3_problem(zst_path: Path) -> Path | None:
    """Decompress a ``.goc3-problem.json.zst`` to a stable cache path.

    The Rust :func:`surge_io.go_c3.load_problem` only consumes plain
    JSON; the dashboard ships ``.zst``-compressed problem files to
    keep the repo small. Cache the decompressed copy under
    ``target/dashboards/rto/goc3-problems/`` and reuse it across
    solves so we don't pay the (~50 ms) decompress on every solve.
    Returns ``None`` if zstandard isn't installed or the source file
    is missing.
    """
    if not zst_path.is_file():
        return None
    try:
        import zstandard as zstd  # type: ignore
    except ImportError:
        return None
    cache_dir = Path("target/dashboards/rto/goc3-problems")
    cache_dir.mkdir(parents=True, exist_ok=True)
    out = cache_dir / zst_path.name.removesuffix(".zst")
    if out.is_file() and out.stat().st_mtime >= zst_path.stat().st_mtime:
        return out
    with open(zst_path, "rb") as fh:
        raw = zstd.ZstdDecompressor().stream_reader(fh).read()
    out.write_bytes(raw)
    return out


def _load_goc3_context(
    network_path: Path, problem_path: Path
) -> CaseContext:
    """Load a GO-C3 pair: network + companion ``.goc3-problem.json.zst``.

    Pulls consumer devices' ``p_ub`` profiles out of
    ``time_series_input.simple_dispatchable_device`` and emits them as a
    per-bus load forecast in MW (multiplying by ``base_norm_mva``).

    Also carries the decompressed problem-file path back via
    ``CaseContext.goc3_problem_path`` so the RTO workflow can route
    through the canonical go_c3 native pipeline (validator-aligned
    bus-balance penalties, target-tracking feedback, reactive-pin
    retry, etc.).
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
    decompressed_path = _decompress_goc3_problem(problem_path)

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
        goc3_problem_path=decompressed_path,
    )


def _goc3_loader(network_path: Path) -> Callable[[], CaseContext]:
    problem_path = network_path.parent / network_path.name.replace(
        ".surge.json.zst", ".goc3-problem.json.zst"
    )
    def _load() -> CaseContext:
        return _load_goc3_context(network_path, problem_path)
    return _load


BUILTIN_CASES: tuple[BuiltinCase, ...] = (
    # GO-C3 cases route through the canonical ``surge.market.go_c3``
    # native pipeline — validator-aligned bus-balance penalties,
    # anchor-based dispatch pinning, target-tracking feedback,
    # reactive-support pin retry. These problem archives ship full
    # time-series + reserve / cost data so the native pipeline reads
    # everything it needs from the file. The first entry is the
    # default the dashboard lands on at startup.
    BuiltinCase(
        "goc3_73_d3_315",
        "GO-C3 73-bus (event4 D3 315)",
        "medium",
        "goc3",
        "examples/cases/go_c3_event4_73_d3_315_sw0/",
        _goc3_loader(
            CASES_DIR
            / "go_c3_event4_73_d3_315_sw0"
            / "go_c3_event4_73_d3_315_sw0.surge.json.zst"
        ),
    ),
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
        "goc3_73_d2_369",
        "GO-C3 73-bus (event4 D2 369)",
        "medium",
        "goc3",
        "examples/cases/go_c3_event4_73_d2_369_sw0/",
        _goc3_loader(
            CASES_DIR
            / "go_c3_event4_73_d2_369_sw0"
            / "go_c3_event4_73_d2_369_sw0.surge.json.zst"
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
    BuiltinCase(
        "goc3_617_d1_015",
        "GO-C3 617-bus (event4 D1 015)",
        "large",
        "goc3",
        "examples/cases/go_c3_event4_617_d1_015_sw0/",
        _goc3_loader(
            CASES_DIR
            / "go_c3_event4_617_d1_015_sw0"
            / "go_c3_event4_617_d1_015_sw0.surge.json.zst"
        ),
    ),
    BuiltinCase(
        "goc3_617_d2_045",
        "GO-C3 617-bus (event4 D2 045)",
        "large",
        "goc3",
        "examples/cases/go_c3_event4_617_d2_045_sw0/",
        _goc3_loader(
            CASES_DIR
            / "go_c3_event4_617_d2_045_sw0"
            / "go_c3_event4_617_d2_045_sw0.surge.json.zst"
        ),
    ),
    BuiltinCase(
        "goc3_617_d3_069",
        "GO-C3 617-bus (event4 D3 069)",
        "large",
        "goc3",
        "examples/cases/go_c3_event4_617_d3_069_sw0/",
        _goc3_loader(
            CASES_DIR
            / "go_c3_event4_617_d3_069_sw0"
            / "go_c3_event4_617_d3_069_sw0.surge.json.zst"
        ),
    ),
    BuiltinCase(
        "goc3_2000",
        "GO-C3 2000-bus (event4 D1 003)",
        "large",
        "goc3",
        "examples/cases/go_c3_event4_2000_d1_003_sw0/",
        _goc3_loader(
            CASES_DIR
            / "go_c3_event4_2000_d1_003_sw0"
            / "go_c3_event4_2000_d1_003_sw0.surge.json.zst"
        ),
    ),
    # IEEE cases — the dashboard synthesizes a load profile + offer
    # curves from the network's quadratic cost coeffs (or default
    # tiers when a generator has no cost), then routes the solve
    # through ``surge.solve_dispatch`` + ``redispatch_with_ac``.
    # These cases don't ship goc3 problem archives so the native
    # pipeline can't drive them; the dashboard's policy knobs that
    # map to DispatchRequest fields (loss_factors, security,
    # runtime.run_pricing, commitment) still take effect, while the
    # goc3-only knobs (reactive-pin factor, sced_ac_opf_*) are inert.
    BuiltinCase("case9", "IEEE 9-bus", "small", "ieee", "surge.case9()", _factory("case9")),
    BuiltinCase("case14", "IEEE 14-bus", "small", "ieee", "surge.case14()", _factory("case14")),
    BuiltinCase("case30", "IEEE 30-bus", "small", "ieee", "surge.case30()", _factory("case30")),
    BuiltinCase("case57", "IEEE 57-bus", "small", "ieee", "surge.case57()", _factory("case57")),
    BuiltinCase("case118", "IEEE 118-bus", "medium", "ieee", "surge.case118()", _factory("case118")),
    BuiltinCase("case300", "IEEE 300-bus", "large", "ieee", "surge.case300()", _factory("case300")),
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


_TOPOLOGY_LAYOUT_CACHE: dict[tuple, dict[str, Any]] = {}


def _topology_signature(buses: list, edges_raw: list[tuple[int, int]]) -> tuple:
    """Stable hashable signature for a (buses, edges) pair so the
    layout cache keys on the actual graph rather than the network
    object identity. Two scaffolds of the same case share the
    same signature."""
    bus_tuple = tuple(int(b.number) for b in buses)
    edge_tuple = tuple(sorted((min(a, b), max(a, b)) for a, b in edges_raw))
    return (bus_tuple, edge_tuple)


def _topology_layout(network: Any, *, iterations: int | None = None) -> dict[str, Any]:
    """Compute an (x, y) position per bus plus the branch adjacency list.

    Uses a Kamada-Kawai layout for small networks (n ≤ 800) and falls
    back to NetworkX's spring layout for larger ones. KK produces
    visibly nicer "natural" placement on transmission graphs than
    FR — nodes settle into distinguishable clusters instead of
    getting clamped at the unit-box boundary as the prior in-loop
    `[0,1]` clamp produced.

    Output is normalised to ``[0, 1] × [0, 1]`` (with a small inset
    margin) so the frontend can rescale to any viewport. Branches
    beyond ~2500 are dropped from the response to keep the payload
    small — the Grid tab degrades to bus dots only for very large
    cases. Layouts are memoised by graph signature so repeat
    scaffolds of the same case skip recomputation.
    """
    import networkx as nx

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

    sig = _topology_signature(buses, edges_raw)
    cached = _TOPOLOGY_LAYOUT_CACHE.get(sig)
    if cached is not None:
        return cached

    g = nx.Graph()
    g.add_nodes_from(range(n))
    g.add_edges_from(edges_raw)

    # Pick the algorithm by graph size. Kamada-Kawai produces
    # noticeably better-looking transmission graphs but its all-pairs
    # shortest-path build is O(n²) memory and O(n²·log·n) time —
    # cap it at 800 buses so the 1576-, 2000-, 4224-bus runs don't
    # spend tens of seconds laying out before the dashboard can
    # render. For larger graphs, NetworkX's spring_layout (FR) is
    # used. NetworkX ≥3.x routes both algorithms through
    # ``scipy.sparse``, so when scipy is unavailable we fall back
    # to a deterministic circular embedding — visibly worse but
    # keeps the dashboard's scaffold endpoint from hard-erroring
    # on an environment that ships networkx without scipy.
    pos: dict[int, tuple[float, float]]
    try:
        if n <= 800 and len(edges_raw) > 0:
            pos = nx.kamada_kawai_layout(g, scale=1.0)
        else:
            pos = nx.spring_layout(
                g,
                seed=0xD15A7C,
                iterations=iterations or 200,
                scale=1.0,
            )
    except (ImportError, ModuleNotFoundError):
        # Pure-Python circular fallback. Sorting by degree first puts
        # the highly-connected hubs near the center of the ring so the
        # picture is at least readable.
        import math
        degree = {i: len(list(g.neighbors(i))) for i in range(n)}
        order = sorted(range(n), key=lambda i: -degree[i])
        pos = {}
        for rank, node in enumerate(order):
            theta = 2.0 * math.pi * rank / max(1, n)
            pos[node] = (math.cos(theta), math.sin(theta))

    # Normalise to ``[margin, 1 − margin]`` on both axes. The 4 %
    # inset keeps the outermost nodes from sitting on the edge of
    # the SVG, which made the prior layout look squashed against
    # the canvas border.
    xs = [pos[i][0] for i in range(n)]
    ys = [pos[i][1] for i in range(n)]
    xmin, xmax = min(xs), max(xs)
    ymin, ymax = min(ys), max(ys)
    xspan = max(1e-6, xmax - xmin)
    yspan = max(1e-6, ymax - ymin)
    margin = 0.04
    inner = 1.0 - 2 * margin

    buses_out = []
    for i, b in enumerate(buses):
        buses_out.append({
            "number": bus_nums[i],
            "x": margin + ((pos[i][0] - xmin) / xspan) * inner,
            "y": margin + ((pos[i][1] - ymin) / yspan) * inner,
        })

    # For very large networks, drop the edge list to keep payload small;
    # the UI falls back to bus dots only.
    branches_out = [{"from": bus_nums[a], "to": bus_nums[b]} for a, b in edges_raw[:2500]]
    note = None if len(edges_raw) <= 2500 else f"{len(edges_raw) - 2500} branches omitted (truncated)"

    out = {"buses": buses_out, "branches": branches_out, "note": note}
    _TOPOLOGY_LAYOUT_CACHE[sig] = out
    return out


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
        if isinstance(loaded, CaseContext) and loaded.goc3_problem_path:
            # Carry the decompressed problem path so the scenario JSON
            # round-trips it back into ``_build_rto_problem``.
            source["goc3_problem_path"] = str(loaded.goc3_problem_path)
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
            # Workflow shape:
            #   "scuc"         — single SCUC pass (LMPs only on flowgate-bound buses)
            #   "scuc_ac_sced" — SCUC + per-period AC OPF reconcile (go_c3-style;
            #                    full LMPs on every bus from NLP duals — default)
            #
            # Default is scuc_ac_sced because the goc3 native pipeline's
            # SCUC-only stage doesn't surface per-bus LMPs (only the
            # binding flowgate's MCC term carries through). Switch to
            # ``"scuc"`` only if you want the faster MIP-only solve and
            # don't care about full LMP coverage.
            "solve_mode": "scuc_ac_sced",
            "commitment_mode": "optimize",  # "optimize" | "all_committed"
            "lp_solver": "highs",
            "nlp_solver": "ipopt",  # only consulted for scuc_ac_sced
            "mip_gap": 0.001,
            "time_limit_secs": None,
            "run_pricing": True,
            "voll_per_mwh": 9000.0,
            "thermal_overload_per_mwh": 5000.0,
            "reserve_shortfall_per_mwh": 1000.0,
            # SCUC loss handling. goc3 cases ship a canonical
            # PTDF-weighted load-pattern warm start (the
            # ``("load_pattern", 0.02)`` tuple from
            # GoC3Policy.scuc_loss_factor_warm_start); the
            # benchmark wins on this. IEEE cases default to
            # ``"disabled"`` since they don't carry the topology
            # for a meaningful loss-pattern seed.
            "loss_mode": (
                "load_pattern" if "goc3_problem_path" in source else "disabled"
            ),
            "loss_rate": 0.02,
            "loss_max_iterations": 0,
            # SCUC iterative N-1 security screening. goc3 cases ship
            # the canonical preseed=250 / max_iterations=10 /
            # max_cuts_per_iteration=2500 from GoC3Policy. IEEE cases
            # leave it off — their flowgate sets are too small to
            # justify outer-loop screening.
            "security_enabled": "goc3_problem_path" in source,
            "security_max_iterations": 10,
            "security_max_cuts_per_iteration": 2500,
            "security_preseed_count_per_period": 250,
            # AC SCED tuning — only consulted when the workflow
            # routes through the go_c3 native pipeline (i.e. the
            # case ships a goc3 problem archive).
            #
            # ``reactive_support_pin_factor`` defaults to ``0.2`` on
            # goc3 problems (the canonical fallback factor that
            # resolves Ipopt convergence-basin issues on 73-/617-bus
            # AC SCED) and ``0.0`` everywhere else.
            "reactive_support_pin_factor": (
                0.2 if "goc3_problem_path" in source else 0.0
            ),
            "sced_ac_opf_tolerance": None,
            "sced_ac_opf_max_iterations": None,
            "disable_sced_thermal_limits": False,
            "ac_relax_committed_pmin_to_zero": False,
        },
    }
    return scenario


# ---------------------------------------------------------------------------
# Scenario → solve inputs (goc3 problem path + dashboard view data)
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
        custom = override.get("custom_profile") or load_cfg.get("custom_profile")
        if custom and len(custom) > 0:
            vals = _resample([float(v) for v in custom], n)
        else:
            vals = [1.0] * n
    else:
        vals = _profile_values(shape_name, n)
    return [nominal_mw * v for v in vals]


def _synthesize_offer_curve(
    gen_meta: dict[str, Any], offers_cfg: dict[str, Any]
) -> list[tuple[float, float]]:
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


@dataclass
class _SolveInputs:
    """Everything the dashboard needs to drive a solve.

    Two solve paths share this struct:

    * **goc3 native** — when ``goc3_problem_path`` is set, the
      dashboard hands the path to ``GoC3Problem.load`` and routes
      through ``surge.market.go_c3.solve_workflow``.
    * **IEEE / surge_dispatch** — when ``goc3_problem_path`` is
      ``None``, the dashboard builds a canonical
      :class:`DispatchRequest` from the scaffolded scenario (loads,
      offer curves, reserves, policy) and calls
      :func:`surge.solve_dispatch` directly. Both paths fill in the
      ``network`` / ``load_forecast_mw`` fields for the view layer.
    """

    goc3_problem_path: Path | None
    period_durations_hours: list[float]
    network: Any
    load_forecast_mw: dict[int, list[float]]


def _resolve_solve_inputs(scenario: dict[str, Any]) -> _SolveInputs:
    """Pull the solve inputs out of a dashboard scenario dict.

    GO-C3 cases carry a goc3 problem archive (the case loader
    decompresses it and threads the path back); IEEE cases don't —
    those load a fresh ``surge.Network`` and the dashboard's solve
    path builds the request from the scaffold.
    """
    source = scenario.get("source") or {}
    case_id = source.get("case_id")
    if source.get("kind") != "builtin" or not case_id:
        raise ValueError(
            "scenario source must be a builtin case — pick one from "
            "the registry"
        )
    case = get_case(case_id)
    loaded = case.loader()
    if isinstance(loaded, CaseContext):
        network = loaded.network
        ctx_durations = loaded.period_durations_hours
        ctx_load_forecast = loaded.load_forecast_mw
        goc3_path = loaded.goc3_problem_path
    else:
        network = loaded
        ctx_durations = None
        ctx_load_forecast = None
        goc3_path = None

    # Time axis: goc3 cases honor the archive's own intervals; IEEE
    # cases honor the dashboard's ``time_axis`` knob.
    if ctx_durations and goc3_path is not None:
        durations = list(ctx_durations)
    else:
        t = scenario.get("time_axis") or {}
        resolution_min = int(t.get("resolution_minutes") or 60)
        periods_n = int(t.get("periods") or (
            int(t.get("horizon_minutes") or 1440) // resolution_min
        ))
        durations = [resolution_min / 60.0] * periods_n

    if ctx_load_forecast:
        load_forecast = dict(ctx_load_forecast)
    else:
        load_forecast = _ieee_load_forecast(scenario, network, durations)

    return _SolveInputs(
        goc3_problem_path=goc3_path,
        period_durations_hours=durations,
        network=network,
        load_forecast_mw=load_forecast,
    )


def _ieee_load_forecast(
    scenario: dict[str, Any],
    network: Any,
    durations: list[float],
) -> dict[int, list[float]]:
    """Build a per-bus load forecast for IEEE cases from the scaffold.

    Three sources, in priority order: scenario ``loads`` with
    ``profile_mw`` (already per-period), scenario ``loads`` with
    ``nominal_mw`` (scaled by the active load profile shape), or
    the network's bus loads as a last-ditch fallback.
    """
    periods = len(durations)
    load_cfg = scenario.get("load_config") or {}
    forecast: dict[int, list[float]] = {}
    scen_loads = scenario.get("loads") or []
    if scen_loads:
        for row in scen_loads:
            bus = int(row["bus"])
            prof = row.get("profile_mw")
            if prof:
                arr = [float(v) for v in prof]
                if len(arr) != periods:
                    arr = _resample(arr, periods) if arr else [0.0] * periods
                forecast[bus] = arr
            else:
                nominal = float(row.get("nominal_mw") or 0.0)
                forecast[bus] = _bus_load_profile(bus, nominal, periods, load_cfg)
        return forecast
    for bus, mw in _bus_nominal_load(network).items():
        forecast[int(bus)] = _bus_load_profile(bus, mw, periods, load_cfg)
    return forecast


def _policy_from_scenario(scenario: dict[str, Any]) -> RtoPolicy:
    p = scenario.get("policy") or {}
    return RtoPolicy(
        solve_mode=str(p.get("solve_mode") or "scuc_ac_sced"),
        lp_solver=str(p.get("lp_solver") or "highs"),
        nlp_solver=str(p.get("nlp_solver") or "ipopt"),
        mip_gap=float(p.get("mip_gap") or 1e-3),
        time_limit_secs=(float(p["time_limit_secs"]) if p.get("time_limit_secs") else None),
        commitment_mode=str(p.get("commitment_mode") or "optimize"),
        run_pricing=bool(p.get("run_pricing", True)),
        loss_mode=str(p.get("loss_mode") or "disabled"),
        loss_rate=float(p.get("loss_rate") if p.get("loss_rate") is not None else 0.02),
        loss_max_iterations=int(p.get("loss_max_iterations") or 0),
        security_enabled=bool(p.get("security_enabled", False)),
        security_max_iterations=int(p.get("security_max_iterations") or 10),
        security_max_cuts_per_iteration=int(p.get("security_max_cuts_per_iteration") or 2500),
        security_preseed_count_per_period=int(p.get("security_preseed_count_per_period") or 250),
        reactive_support_pin_factor=float(p.get("reactive_support_pin_factor") or 0.0),
        sced_ac_opf_tolerance=(
            float(p["sced_ac_opf_tolerance"])
            if p.get("sced_ac_opf_tolerance") is not None else None
        ),
        sced_ac_opf_max_iterations=(
            int(p["sced_ac_opf_max_iterations"])
            if p.get("sced_ac_opf_max_iterations") is not None else None
        ),
        disable_sced_thermal_limits=bool(p.get("disable_sced_thermal_limits", False)),
        ac_relax_committed_pmin_to_zero=bool(p.get("ac_relax_committed_pmin_to_zero", False)),
    )


def _lmp_source_label(policy: RtoPolicy) -> str:
    """Which solve pass produced the LMPs the dashboard is surfacing.

    The two goc3 native modes are SCUC-only (LMPs from the SCUC
    repricing LP) and the two-stage SCUC + AC SCED (LMPs from the
    AC NLP duals). The header label tracks ``solve_mode`` so the
    user can tell at a glance which pass priced the system.
    """
    return {
        "scuc": "DC SCUC",
        "scuc_ac_sced": "AC SCED",
    }.get(policy.solve_mode, policy.solve_mode)


# ---------------------------------------------------------------------------
# Solve + result flattening
# ---------------------------------------------------------------------------


class _LogCapture:
    """Capture solve log output for the dashboard's Log tab.

    Attaches a handler to the ``dashboards.rto`` and
    ``surge.market.go_c3`` Python loggers for the lifetime of a
    ``with`` block. The captured text is exposed via
    :meth:`text`; an optional ``Queue`` subscriber lets a
    streaming endpoint forward each formatted log line to the
    client in real time without re-parsing the StringIO.

    Earlier revisions wrapped ``surge.market.SolveLogger`` with
    ``capture_solver_log=True`` so Rust tracing + solver console
    spam (Gurobi MIP progress, Ipopt NLP iterations,
    ``surge_dispatch`` internals) all flowed into the log. That
    used ``os.dup2`` to redirect process-wide stdout/stderr to a
    pipe, which deadlocks under FastAPI's worker thread when a
    high-volume Rust tracing run (goc3_73 D3/315 — 42 periods ×
    250 preseed cuts/period) fills the 64 KB pipe buffer faster
    than the reader thread can drain. Symptom: solve hangs
    indefinitely with no output. Disable the fd-tee and accept
    Python-side logs only.
    """

    _LOGGER_NAMES: tuple[str, ...] = ("dashboards.rto", "surge.market.go_c3")

    def __init__(self, subscriber: Any | None = None) -> None:
        """``subscriber`` is anything with a ``put(str)`` method
        (typically a :class:`queue.Queue`); each formatted log
        record — Python-side and Rust-side — gets pushed to it as
        it's emitted."""
        import io as _io
        import threading

        self._buf = _io.StringIO()
        self._formatter = logging.Formatter(
            "%(asctime)s %(levelname)-5s %(name)s · %(message)s",
            datefmt="%H:%M:%S",
        )
        self._subscriber = subscriber
        self._buf_lock = threading.Lock()

        outer = self

        class _DashboardLogHandler(logging.Handler):
            def emit(self_inner, record: logging.LogRecord) -> None:  # noqa: N805
                try:
                    line = outer._formatter.format(record)
                except Exception:  # noqa: BLE001
                    return
                outer._emit(line)

        self._handler: logging.Handler = _DashboardLogHandler()
        self._handler.setLevel(logging.INFO)
        self._loggers = [logging.getLogger(n) for n in self._LOGGER_NAMES]
        self._prev_levels: list[int] = []

        # Rust tracing pump: a daemon thread reads each formatted
        # line from the in-process broadcast channel (no fd-tee) and
        # routes it through the same emit path as the Python logs.
        self._stream: Any = None
        self._pump_thread: threading.Thread | None = None
        self._pump_stop = threading.Event()

    def _emit(self, line: str) -> None:
        with self._buf_lock:
            self._buf.write(line + "\n")
        if self._subscriber is not None:
            try:
                self._subscriber.put(line)
            except Exception:  # noqa: BLE001
                pass

    def __enter__(self) -> "_LogCapture":
        for lg in self._loggers:
            self._prev_levels.append(lg.level)
            if lg.level > logging.INFO or lg.level == logging.NOTSET:
                lg.setLevel(logging.INFO)
            lg.addHandler(self._handler)
        # Subscribe to Rust tracing via the broadcast layer added in
        # surge.init_logging. Falls through silently if surge or the
        # LogStream class isn't available (older wheel).
        try:
            from surge.market import LogStream

            self._stream = LogStream(level="info")
            self._stream.__enter__()
            self._pump_stop.clear()

            import threading

            def _pump() -> None:
                while not self._pump_stop.is_set():
                    line = self._stream.recv(0.25)
                    if line is not None:
                        self._emit(line)

            self._pump_thread = threading.Thread(
                target=_pump, daemon=True, name="rto-rust-log-pump"
            )
            self._pump_thread.start()
        except Exception:  # noqa: BLE001
            self._stream = None
            self._pump_thread = None
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:
        for lg, level in zip(self._loggers, self._prev_levels):
            lg.removeHandler(self._handler)
            lg.setLevel(level)
        if self._pump_thread is not None:
            self._pump_stop.set()
            self._pump_thread.join(timeout=1.0)
            self._pump_thread = None
        if self._stream is not None:
            try:
                self._stream.__exit__(exc_type, exc_val, exc_tb)
            except Exception:  # noqa: BLE001
                pass
            self._stream = None

    def text(self) -> str:
        with self._buf_lock:
            return self._buf.getvalue()


def run_solve(scenario: dict[str, Any]) -> dict[str, Any]:
    """Run the dashboard solve and flatten its output for the browser.

    Two paths share this entry point:

    * **GO-C3 case** — routes through
      :func:`surge.market.go_c3.solve_workflow`, the canonical native
      two-stage SCUC → AC SCED with validator-aligned tuning.
    * **IEEE case** — builds a :class:`DispatchRequest` from the
      dashboard scaffold and calls :func:`surge.solve_dispatch`
      directly. AC SCED on this path uses
      :func:`surge.market.redispatch_with_ac` — fine on small IEEE
      networks but lacks the goc3 pipeline's reactive-pin /
      anchor-pinning / target-tracking machinery.
    """
    return _run_solve_with_capture(scenario, _LogCapture())


def run_solve_stream(
    scenario: dict[str, Any],
) -> "Generator[str, None, None]":
    """Stream the solve as Server-Sent Events.

    Yields ``event: log`` chunks as the Python loggers emit
    records, then a single ``event: result`` chunk carrying the
    final flattened-result JSON, then closes. Errors are surfaced
    via ``event: error`` (the result chunk is then suppressed).

    The actual solve runs in a worker thread so this generator
    can drain the log queue without blocking the FastAPI event
    loop. Heartbeats every 5 s (``: ping`` comments) keep the
    connection alive across reverse proxies / browsers that idle
    out long-lived streams.
    """
    import json as _json
    import queue as _queue
    import threading

    log_queue: _queue.Queue[str] = _queue.Queue()
    capture = _LogCapture(subscriber=log_queue)

    holder: dict[str, Any] = {}

    def _worker() -> None:
        try:
            holder["result"] = _run_solve_with_capture(scenario, capture)
        except Exception as exc:  # noqa: BLE001
            holder["error"] = str(exc)

    thread = threading.Thread(target=_worker, daemon=True, name="rto-solve")
    thread.start()

    def _sse(event: str, payload: str) -> str:
        # Each line of the payload gets its own ``data:`` prefix —
        # required by the SSE spec when the payload contains newlines.
        lines = payload.split("\n")
        body = "\n".join(f"data: {ln}" for ln in lines)
        return f"event: {event}\n{body}\n\n"

    last_heartbeat = time.monotonic()
    while True:
        try:
            line = log_queue.get(timeout=0.25)
            yield _sse("log", line)
        except _queue.Empty:
            now = time.monotonic()
            if not thread.is_alive():
                break
            if now - last_heartbeat > 5.0:
                yield ": ping\n\n"
                last_heartbeat = now

    # Drain anything that landed after the thread finished.
    while True:
        try:
            line = log_queue.get_nowait()
        except _queue.Empty:
            break
        yield _sse("log", line)

    if "error" in holder:
        yield _sse("error", holder["error"])
        return

    result = holder.get("result")
    if result is None:
        yield _sse("error", "solve produced no result payload")
        return
    yield _sse("result", _json.dumps(result))


def _run_solve_with_capture(
    scenario: dict[str, Any], capture: "_LogCapture"
) -> dict[str, Any]:
    """Solve body. Split out so the streaming endpoint can hand in a
    capture pre-bound to a Queue subscriber while the canonical
    JSON endpoint keeps the simple StringIO path."""
    inputs = _resolve_solve_inputs(scenario)
    policy = _policy_from_scenario(scenario)

    with capture as cap:
        logger.info(
            "RTO solve · case=%s · solve_mode=%s · commitment=%s · lp=%s · nlp=%s",
            (scenario.get("source") or {}).get("case_id", "?"),
            policy.solve_mode,
            policy.commitment_mode,
            policy.lp_solver,
            policy.nlp_solver,
        )
        t0 = time.perf_counter()
        try:
            if inputs.goc3_problem_path is not None:
                outcome = _solve_goc3(inputs, policy)
            else:
                outcome = _solve_ieee(inputs, scenario, policy)
        except Exception as err:  # noqa: BLE001
            elapsed = time.perf_counter() - t0
            return {
                "status": "error",
                "error": str(err),
                "elapsed_secs": elapsed,
                "settlement_summary": None,
                "settlement": None,
                "dispatch": None,
                "violations": [],
                "solve_log": cap.text(),
            }
        elapsed = time.perf_counter() - t0

    solve_log = cap.text()
    err = outcome.get("error")
    if err:
        return {
            "status": "error",
            "error": err,
            "elapsed_secs": elapsed,
            "settlement_summary": None,
            "settlement": None,
            "dispatch": None,
            "violations": [],
            "solve_log": solve_log,
        }

    final_solution = outcome["final_solution"]
    settlement = extract_settlement(final_solution, inputs.period_durations_hours)
    flat = _flatten_results(
        native_result=outcome.get("native_result") or {},
        elapsed_secs=elapsed,
        settlement=settlement,
        dispatch=final_solution,
        scuc_dispatch=outcome.get("scuc_solution"),
        inputs=inputs,
        scenario=scenario,
        policy=policy,
    )
    flat["solve_log"] = solve_log
    return flat


def _solve_goc3(inputs: _SolveInputs, policy: RtoPolicy) -> dict[str, Any]:
    """Run the GO-C3 native two-stage workflow."""
    from surge.market.go_c3 import (
        GoC3Problem,
        build_workflow as goc3_build_workflow,
        solve_workflow as goc3_solve_workflow,
    )

    market_policy = policy.to_market_policy()
    stop_after = "scuc" if policy.solve_mode == "scuc" else None

    problem = GoC3Problem.load(str(inputs.goc3_problem_path))
    workflow = goc3_build_workflow(problem, market_policy)
    native_result = goc3_solve_workflow(
        workflow,
        lp_solver=policy.lp_solver,
        nlp_solver=policy.nlp_solver,
        stop_after_stage=stop_after,
    )
    err = native_result.get("error") if isinstance(native_result, dict) else None
    if err:
        return {
            "error": (
                f"go_c3 native pipeline failed at stage {err.get('stage_id')!r}: "
                f"{err.get('error')}"
            )
        }
    stages = native_result.get("stages") or []
    if not stages:
        return {"error": "go_c3 native pipeline returned no solved stages"}
    final_solution = stages[-1].get("solution")
    if final_solution is None:
        return {
            "error": (
                f"go_c3 native stage {stages[-1].get('stage_id')!r} "
                "carried no solution payload"
            )
        }
    # The AC SCED stage strips zonal ``reserve_results`` and per-
    # resource ``reserve_awards`` — those data live on the SCUC
    # stage solution. Overlay them onto the AC dispatch so the
    # dashboard's AS Pricing + Reserves tabs (and the per-gen
    # AS revenue split) work on two-stage runs. Mirrors the
    # ``dc_reserve_source`` argument of ``surge.market.go_c3.export``.
    scuc_solution: dict[str, Any] | None = None
    if len(stages) > 1:
        scuc_solution = stages[-2].get("solution")
        if scuc_solution is not None:
            _overlay_scuc_reserves_on_ac(final_solution, scuc_solution)
    return {
        "final_solution": final_solution,
        "scuc_solution": scuc_solution,
        "native_result": native_result,
    }


def _overlay_scuc_reserves_on_ac(
    ac_dispatch: dict[str, Any],
    scuc_dispatch: dict[str, Any],
) -> None:
    """Overlay zonal ``reserve_results`` + per-resource ``reserve_awards``
    from the SCUC stage onto the AC SCED dispatch in-place.

    Active-power reserve clearing is fully decided by the SCUC; the
    AC reconcile pass only re-balances P/Q for the dispatch envelope
    around the SCUC commitment / awards. The native pipeline drops
    reserve metadata from the AC stage's solution, but the dashboard
    needs it on the final dispatch dict so the settlement extractor
    surfaces the AS price / award per zone × product × period.
    """
    ac_periods = ac_dispatch.get("periods") or []
    scuc_periods = scuc_dispatch.get("periods") or []
    for t, ac_period in enumerate(ac_periods):
        if t >= len(scuc_periods):
            break
        scuc_period = scuc_periods[t]
        # Period-level zonal / system reserve clearing.
        if scuc_period.get("reserve_results"):
            ac_period["reserve_results"] = list(scuc_period["reserve_results"])
        # Per-resource reserve awards.
        scuc_by_rid: dict[str, dict[str, Any]] = {
            r.get("resource_id"): r
            for r in (scuc_period.get("resource_results") or [])
            if r.get("resource_id")
        }
        for ac_resource in ac_period.get("resource_results") or []:
            rid = ac_resource.get("resource_id")
            scuc_resource = scuc_by_rid.get(rid)
            if scuc_resource is not None and scuc_resource.get("reserve_awards"):
                ac_resource["reserve_awards"] = scuc_resource["reserve_awards"]


def _solve_ieee(
    inputs: _SolveInputs,
    scenario: dict[str, Any],
    policy: RtoPolicy,
) -> dict[str, Any]:
    """Build a DispatchRequest from the scaffold and solve via surge."""
    import surge  # type: ignore

    request = _build_ieee_dispatch_request(inputs, scenario, policy)
    dc_result = surge.solve_dispatch(
        inputs.network, request, lp_solver=policy.lp_solver
    )
    dc_dict = dc_result.to_dict()

    if policy.solve_mode != "scuc_ac_sced":
        return {"final_solution": dc_dict}

    # AC SCED reconcile via the canonical Python wrapper. Goc3-native
    # tuning (reactive pin / anchor pinning / target tracking) isn't
    # available here — small IEEE networks usually converge fine
    # without those, but case14's regulated-bus voltage data trips
    # the AC OPF pre-solve check; users who hit that should drop to
    # scuc-only or use a goc3 case.
    from surge.market import redispatch_with_ac
    from surge.market.reconcile import extract_fixed_commitment

    fixed_resources = extract_fixed_commitment(dc_result, request)
    config = _ieee_market_config(policy, getattr(inputs.network, "base_mva", 100.0))
    ac_result, _ac_request, _ac_report = redispatch_with_ac(
        network_builder=lambda: (inputs.network, {}),
        request_builder=lambda: _build_ieee_dispatch_request(
            inputs, scenario, policy, formulation="ac"
        ),
        dispatch_result=dc_result,
        fixed_resources=fixed_resources,
        surge_module=surge,
        config=config,
        lp_solver=policy.lp_solver,
        nlp_solver=policy.nlp_solver,
        periods=len(inputs.period_durations_hours),
    )
    return {"final_solution": ac_result.to_dict(), "scuc_solution": dc_dict}


def _ieee_market_config(policy: RtoPolicy, base_mva: float) -> Any:
    """Default :class:`MarketConfig` for the IEEE solve path."""
    from dataclasses import replace
    from surge.market import LossFactorRules, MarketConfig

    cfg = MarketConfig.default(base_mva)
    enabled = policy.loss_mode != "disabled"
    rules = replace(
        cfg.network_rules,
        loss_factors=LossFactorRules(
            enabled=enabled,
            max_iterations=max(1, policy.loss_max_iterations) if enabled else 1,
            tolerance=1e-3,
        ),
    )
    return replace(cfg, network_rules=rules)


def _build_ieee_dispatch_request(
    inputs: _SolveInputs,
    scenario: dict[str, Any],
    policy: RtoPolicy,
    *,
    formulation: str = "dc",
) -> dict[str, Any]:
    """Assemble a canonical DispatchRequest for an IEEE-case solve.

    Threads the dashboard's load forecast, generator offer curves,
    zonal reserve requirements, and policy knobs onto the request
    via the typed :class:`DispatchRequestBuilder`. ``formulation``
    flips between DC SCUC and AC reconcile — the AC pass uses
    ``period_by_period`` coupling because the dispatch validator
    rejects multi-period AC.
    """
    from surge.market import (
        GeneratorOfferSchedule,
        NON_SPINNING,
        REG_DOWN,
        REG_UP,
        ReserveProductDef,
        SPINNING,
        ZonalRequirement,
        request as _request_builder,
    )

    n_periods = len(inputs.period_durations_hours)
    builder = _request_builder().timeline(
        periods=n_periods,
        hours_by_period=inputs.period_durations_hours,
    )
    builder.formulation(formulation)
    builder.coupling(
        "period_by_period" if formulation == "ac" or n_periods == 1 else "time_coupled"
    )
    builder.run_pricing(bool(policy.run_pricing))

    if policy.commitment_mode == "all_committed":
        builder.commitment_all_committed()
    elif policy.commitment_mode == "fixed_initial":
        # Pin every in-service generator on for every period —
        # there's no per-resource initial state in the IEEE case.
        resources = []
        for g in inputs.network.generators:
            committed = bool(getattr(g, "in_service", True))
            resources.append(
                {
                    "resource_id": g.resource_id,
                    "initial": committed,
                    "periods": [committed] * n_periods,
                }
            )
        builder.commitment_fixed(resources=resources)
    else:  # optimize
        builder.commitment_optimize(
            mip_rel_gap=policy.mip_gap,
            time_limit_secs=policy.time_limit_secs,
        )

    for bus, prof in inputs.load_forecast_mw.items():
        builder.load_profile(bus=int(bus), values=list(prof))

    # Reserve products — same 4-product AS set the dashboard's UI
    # shows; shortfall costs are a fixed default since the IEEE
    # scaffold doesn't expose them on the form.
    shortfall_default = 1_000.0
    reserve_products = [
        ReserveProductDef(
            id=p.id, name=p.name, direction=p.direction,
            qualification=p.qualification, energy_coupling=p.energy_coupling,
            shared_limit_products=p.shared_limit_products,
            balance_products=p.balance_products,
            deploy_secs=p.deploy_secs, kind=p.kind,
            shortfall_cost_per_mw=shortfall_default,
        )
        for p in (REG_UP, REG_DOWN, SPINNING, NON_SPINNING)
    ]
    builder.extend_market(
        reserve_products=[p.to_product_dict() for p in reserve_products]
    )

    # Zonal reserve requirements derived from the scaffold (peak-load %).
    res_cfg = scenario.get("reserves_config") or {}
    zone_id = int(res_cfg.get("zone_id") or 1)
    peak_load = max(
        (sum(inputs.load_forecast_mw[b][t] for b in inputs.load_forecast_mw)
         for t in range(n_periods)),
        default=0.0,
    ) if inputs.load_forecast_mw else 0.0
    requirements: list[ZonalRequirement] = []
    for pid, spec in (res_cfg.get("products") or {}).items():
        if not spec:
            continue
        abs_mw = spec.get("absolute_mw")
        req_mw = (
            float(abs_mw) if abs_mw is not None
            else peak_load * (float(spec.get("percent_of_peak") or 0.0) / 100.0)
        )
        if req_mw <= 1e-9:
            continue
        requirements.append(
            ZonalRequirement(
                zone_id=zone_id,
                product_id=str(pid),
                requirement_mw=req_mw,
                per_period_mw=[req_mw] * n_periods,
                shortfall_cost_per_unit=shortfall_default,
            )
        )
    if requirements:
        builder.zonal_reserves(requirements)

    # Generator offer schedules from the scaffold's offers config.
    offers_cfg = scenario.get("offers_config") or {}
    energy_offers: list[GeneratorOfferSchedule] = []
    for meta in scenario.get("generators") or []:
        rid = meta["resource_id"]
        segs = _per_gen_offer_segments(meta, offers_cfg)
        no_load = float(meta.get("cost_c0") or 0.0) if meta.get("has_cost") else 0.0
        energy_offers.append(
            GeneratorOfferSchedule(
                resource_id=rid,
                segments_by_period=[segs for _ in range(n_periods)],
                no_load_cost_by_period=[no_load for _ in range(n_periods)],
                startup_cost_tiers=[],
            )
        )
    if energy_offers:
        builder.generator_offers(energy_offers)

    config = _ieee_market_config(policy, getattr(inputs.network, "base_mva", 100.0))
    builder.market_config(config)
    return builder.build()


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


# Display label for each ``ObjectiveTermKind`` the dispatch result emits.
# Grouping by ``kind`` rather than ``bucket`` so commitment-decided
# costs (startup / shutdown / AS clearing) don't get merged into
# coarser buckets and disappear when AC SCED's narrower bucket set
# is mistaken for the full picture.
_OBJECTIVE_KIND_LABELS: dict[str, str] = {
    "generator_energy": "Generator energy",
    "dispatchable_load_energy": "Load energy (DR)",
    "hvdc_energy": "HVDC energy",
    "generator_no_load": "No-load",
    "generator_startup": "Startup",
    "generator_shutdown": "Shutdown",
    "reserve_procurement": "AS clearing",
    "reserve_shortfall": "AS shortfall",
    "thermal_limit_penalty": "Thermal penalty",
    "power_balance_penalty": "P-balance penalty",
    "reactive_balance_penalty": "Q-balance penalty",
    "voltage_bound_penalty": "Voltage penalty",
    "angle_bound_penalty": "Angle penalty",
    "ramp_penalty": "Ramp penalty",
    "flowgate_penalty": "Flowgate penalty",
    "interface_penalty": "Interface penalty",
    "headroom_penalty": "Headroom penalty",
    "footroom_penalty": "Footroom penalty",
    "energy_window_penalty": "Energy window penalty",
    "co2_cost": "CO₂",
    "other": "Other",
}


def _aggregate_objective_terms_by_kind(
    dispatch: dict[str, Any] | None,
    n_periods: int,
) -> dict[str, list[float]]:
    """Per-kind, per-period dollar totals from a single dispatch
    stage's ``objective_terms``."""
    out: dict[str, list[float]] = {}
    if dispatch is None:
        return out
    for t, period in enumerate(dispatch.get("periods") or []):
        if t >= n_periods:
            break
        for entry in (period.get("objective_terms") or []):
            kind = str(entry.get("kind") or "other")
            dollars = float(entry.get("dollars") or 0.0)
            arr = out.setdefault(kind, [0.0] * n_periods)
            arr[t] += dollars
    return out


def _objective_breakdown(
    dispatch: dict[str, Any],
    scuc_dispatch: dict[str, Any] | None,
    n_periods: int,
) -> dict[str, Any]:
    """Aggregate ``objective_terms`` from BOTH dispatch stages and
    surface a per-bucket × per-period breakdown for the UI.

    The AC SCED stage emits ``generator_energy``,
    ``dispatchable_load_energy``, and AC penalty kinds; commitment-
    decided costs (``generator_startup``, ``generator_shutdown``,
    ``reserve_procurement``, ``reserve_shortfall``) only land on the
    SCUC stage because AC SCED runs with commitment fixed. Without
    pulling from both stages, the user-visible Objective tab loses
    every commitment-decided bucket — so startup / AS appear as if
    they cost nothing.

    Merge rule: prefer AC SCED's per-period series for kinds that
    appear in both stages (it's the final delivered redispatch);
    fall back to SCUC for kinds that only the commitment stage
    emits. ``penalty_summary`` (a top-level dict with validator-
    aligned penalty totals) is taken from the AC SCED stage where
    it's most useful — the SCUC's penalty_summary covers the DC
    SCUC slacks, not the AC redispatch.
    """
    ac_kinds = _aggregate_objective_terms_by_kind(dispatch, n_periods)
    scuc_kinds = _aggregate_objective_terms_by_kind(scuc_dispatch, n_periods)

    merged: dict[str, list[float]] = {}
    # SCUC supplies kinds AC SCED doesn't have at all.
    for kind, series in scuc_kinds.items():
        if kind not in ac_kinds:
            merged[kind] = series
    # AC SCED wins for kinds present in both — these are the post-
    # redispatch realised costs.
    for kind, series in ac_kinds.items():
        merged[kind] = series

    # Drop any kind whose horizon total is essentially zero so the
    # cards / stacked area chart don't get cluttered with empty
    # entries (generator_shutdown is often 0 on no-shutdown horizons).
    per_period_by_bucket: dict[str, list[float]] = {}
    total_by_bucket: dict[str, float] = {}
    for kind, series in merged.items():
        total = sum(series)
        if abs(total) < 1e-3:
            continue
        label = _OBJECTIVE_KIND_LABELS.get(kind, kind)
        # Multiple raw kinds can map to the same display bucket
        # (none today, but keep the merge correct).
        arr = per_period_by_bucket.setdefault(label, [0.0] * n_periods)
        for t in range(n_periods):
            arr[t] += series[t]
        total_by_bucket[label] = total_by_bucket.get(label, 0.0) + total
    grand_total = sum(total_by_bucket.values())
    penalty_summary = dict(dispatch.get("penalty_summary") or {})
    return {
        "buckets": sorted(total_by_bucket.keys()),
        "total_by_bucket": total_by_bucket,
        "per_period_by_bucket": per_period_by_bucket,
        "grand_total_dollars": grand_total,
        "penalty_summary": penalty_summary,
    }


def _branch_shadow_prices_by_period(
    dispatch: dict[str, Any] | None,
    n_periods: int,
) -> list[dict[tuple[int, int], float]]:
    """Index per-period thermal shadow prices keyed by ``(from_bus, to_bus)``.

    The unified dispatch path emits per-branch thermal duals in
    ``constraint_results`` with ``kind="branch_thermal"`` and an id of
    ``"branch:<from>:<to>:<circuit>"`` (no direction suffix; that's
    reserved for slack-positive accounting entries). Multi-circuit
    branches between the same bus pair are summed — the dashboard view
    is keyed on bus pair, not circuit, since the Grid map draws one
    line between two buses.
    """
    out: list[dict[tuple[int, int], float]] = [dict() for _ in range(n_periods)]
    if dispatch is None:
        return out
    periods = dispatch.get("periods") or []
    for t, period in enumerate(periods):
        if t >= n_periods:
            break
        for c in (period.get("constraint_results") or []):
            if str(c.get("kind") or "") != "branch_thermal":
                continue
            cid = str(c.get("constraint_id") or "")
            parts = cid.split(":")
            # Shadow-price entries are 4 parts: branch:from:to:circuit.
            # Slack-only accounting entries (5 parts with reverse /
            # forward / ac_thermal_from / ac_thermal_to suffix) carry
            # only ``slack_mw`` and are filtered out here.
            if len(parts) != 4 or parts[0] != "branch":
                continue
            sp = c.get("shadow_price")
            if sp is None:
                continue
            try:
                fb = int(parts[1])
                tb = int(parts[2])
                price = float(sp)
            except (TypeError, ValueError):
                continue
            if abs(price) < 1e-6:
                continue
            key = (fb, tb)
            out[t][key] = out[t].get(key, 0.0) + price
    return out


def _branches_summary(
    dispatch: dict[str, Any],
    scuc_dispatch: dict[str, Any] | None,
    network: Any,
    branch_flows_by_period: list[list[dict[str, Any]]],
    threshold: float = 0.9,
) -> list[dict[str, Any]]:
    """Per-branch summary for the Branches tab.

    Returns the rows whose worst period utilisation reaches
    ``threshold`` (default 90 %). Each row carries the per-period
    flow (MW), utilisation, and shadow prices from both pricing
    stages (SCUC repricing LP + AC SCED NLP). The dashboard plots
    SCUC and SCED prices as separate series so the user can compare
    where the active-power dispatch sees congestion vs. where the AC
    redispatch sees it (the latter accounting for losses + voltage
    coupling).
    """
    rows: list[dict[str, Any]] = []
    if not branch_flows_by_period:
        return rows
    n_periods = len(branch_flows_by_period)
    sced_duals = _branch_shadow_prices_by_period(dispatch, n_periods)
    scuc_duals = _branch_shadow_prices_by_period(scuc_dispatch, n_periods)
    has_sced = any(any(p.values()) for p in sced_duals)
    has_scuc = any(any(p.values()) for p in scuc_duals)

    n_branches = len(branch_flows_by_period[0])
    for bi in range(n_branches):
        flows = [bp[bi]["flow_mw"] for bp in branch_flows_by_period]
        utils = [bp[bi].get("utilization") for bp in branch_flows_by_period]
        rating = float(branch_flows_by_period[0][bi].get("rating_mva") or 0.0)
        from_bus = int(branch_flows_by_period[0][bi].get("from") or 0)
        to_bus = int(branch_flows_by_period[0][bi].get("to") or 0)
        worst_util = max((u for u in utils if u is not None), default=None)
        if worst_util is None or worst_util < threshold:
            continue
        key = (from_bus, to_bus)
        scuc_series = [p.get(key) for p in scuc_duals]
        sced_series = [p.get(key) for p in sced_duals]
        # Back-compat single ``shadow_price`` field — prefer SCED when
        # both stages reported, fall back to SCUC. Per-stage series
        # below let the UI overlay both when they differ.
        combined: list[float | None] = []
        for sc, se in zip(scuc_series, sced_series):
            if se is not None:
                combined.append(se)
            elif sc is not None:
                combined.append(sc)
            else:
                combined.append(None)
        rows.append({
            "from_bus": from_bus,
            "to_bus": to_bus,
            "rating_mva": rating,
            "flow_mw": flows,
            "utilization": utils,
            "shadow_price": combined,
            "shadow_price_scuc": scuc_series if has_scuc else None,
            "shadow_price_sced": sced_series if has_sced else None,
            "worst_utilization": float(worst_util),
            "is_breached": bool(worst_util > 1.0 + 1e-6),
        })
    rows.sort(key=lambda r: -r["worst_utilization"])
    return rows


_N1_RE = re.compile(
    r"^N1_t(?P<period>\d+)_(?P<cf>\d+)_(?P<ct>\d+)_(?P<mf>\d+)_(?P<mt>\d+)$"
)
_HVDC_N1_RE = re.compile(
    r"^HVDC_N1_t(?P<period>\d+)_(?P<link>\d+)_(?P<mf>\d+)_(?P<mt>\d+)$"
)


def _parse_security_cut_id(cid: str) -> tuple[str, str] | None:
    """Parse the ``constraint_id`` of a SCUC security flowgate into a
    ``(outage, monitored)`` pair.

    The SCUC's iterative N-1 screening (``surge_dispatch::scuc::security``)
    builds flowgates whose names encode the contingency + monitored
    bus pair plus the period the cut was added in:

        ``N1_t{period}_{ctg_from}_{ctg_to}_{mon_from}_{mon_to}``
        ``HVDC_N1_t{period}_{link_idx}_{mon_from}_{mon_to}``

    The same ``(ctg, mon)`` may have multiple cuts across different
    periods; we strip the period prefix here so the dashboard groups
    by physical pair rather than per-period cut.
    """
    m = _N1_RE.match(cid)
    if m:
        return (
            f"{m.group('cf')}→{m.group('ct')}",
            f"{m.group('mf')}→{m.group('mt')}",
        )
    m = _HVDC_N1_RE.match(cid)
    if m:
        return (
            f"hvdc:{m.group('link')}",
            f"{m.group('mf')}→{m.group('mt')}",
        )
    return None


def _security_cuts_from_constraint_results(
    dispatch: dict[str, Any] | None,
    n_periods: int,
) -> dict[tuple[str, str], dict[str, list[float | None]]]:
    """Group flowgate ``constraint_results`` by (outage, monitored).

    Returns a dict keyed by ``(outage_label, monitored_label)`` with
    per-period ``shadow_price`` and ``slack_mw`` arrays. The same cut
    may be added in multiple periods (one flowgate per period); we
    take the entry from each period it appears in.
    """
    out: dict[tuple[str, str], dict[str, list[float | None]]] = {}
    if dispatch is None:
        return out
    periods = dispatch.get("periods") or []
    for t, per in enumerate(periods):
        if t >= n_periods:
            break
        for c in (per.get("constraint_results") or []):
            if str(c.get("kind") or "") != "flowgate":
                continue
            cid = str(c.get("constraint_id") or "")
            # Strip the slack-direction suffix (``flowgate:NAME:reverse|forward``)
            # the SCUC emits for slack-positive entries — peel back to the
            # underlying name so it matches the shadow-price entry's id.
            if cid.startswith("flowgate:") and (cid.endswith(":reverse") or cid.endswith(":forward")):
                cid = cid[len("flowgate:"):].rsplit(":", 1)[0]
            key = _parse_security_cut_id(cid)
            if key is None:
                continue
            row = out.setdefault(
                key,
                {
                    "shadow_price": [None] * n_periods,
                    "slack_mw": [None] * n_periods,
                },
            )
            sp = c.get("shadow_price")
            slack = c.get("slack_mw")
            if sp is not None:
                row["shadow_price"][t] = float(sp)
            if slack is not None:
                # Keep the larger of forward/reverse slacks per period.
                cur = row["slack_mw"][t]
                row["slack_mw"][t] = max(float(slack), cur or 0.0)
    return out


def _contingencies_summary(
    dispatch: dict[str, Any],
    scuc_dispatch: dict[str, Any] | None,
    n_periods: int,
    threshold: float = 0.95,
) -> list[dict[str, Any]]:
    """Per-contingency summary for the Contingencies tab.

    Pulls the SCUC's near-binding contingency report
    (``diagnostics.security.near_binding_contingencies``) — a list of
    ``(period, contingency, monitored, post_flow_mw, limit_mw,
    utilization)`` tuples for cuts whose post-contingency flow on
    the converged dispatch reaches the near-binding threshold (0.7
    in surge-dispatch). Each (outage, monitored) pair groups across
    periods into a per-pair row with per-period flow / utilisation
    series. Shadow prices come from the LP duals on those same cuts
    (filtered ``constraint_results`` with ``kind=branch_thermal`` or
    ``kind=flowgate`` and an ``N1_t...`` id).
    """
    src = scuc_dispatch if scuc_dispatch is not None else dispatch
    diag = (src.get("diagnostics") or {}) if src else {}
    sec = (diag.get("security") or {})
    near_binding = sec.get("near_binding_contingencies") or []

    # Group by (outage, monitored) bus pair. Multiple circuits between
    # the same two buses get folded together (the dashboard's pair-level
    # view is what users want; circuit detail is rarely actionable).
    grouped: dict[tuple[str, str], dict[str, Any]] = {}
    for r in near_binding:
        outage = f"{r['outage_from_bus']}→{r['outage_to_bus']}"
        monitored = f"{r['monitored_from_bus']}→{r['monitored_to_bus']}"
        key = (outage, monitored)
        row = grouped.setdefault(
            key,
            {
                "outage_branch": outage,
                "monitored_branch": monitored,
                "rating_mva": float(r["limit_mw"]),
                "flow_mw": [None] * n_periods,
                "utilization": [None] * n_periods,
                "shadow_price": [None] * n_periods,
                "shadow_price_scuc": [None] * n_periods,
                "shadow_price_sced": [None] * n_periods,
            },
        )
        t = int(r["period"])
        if t < 0 or t >= n_periods:
            continue
        # Take the worst case if multiple circuits report on the same
        # period × bus pair (max |flow|).
        existing = row["flow_mw"][t]
        flow = float(r["post_contingency_flow_mw"])
        if existing is None or abs(flow) > abs(existing):
            row["flow_mw"][t] = flow
            row["utilization"][t] = float(r["utilization"])

    if not grouped:
        return []

    # Layer in shadow prices from constraint_results — same parsing as
    # the SCUC pricing LP would use. Most goc3-native runs leave these
    # at zero (LP degeneracy), but the AC SCED branch_thermal duals
    # ARE meaningful when they show up.
    scuc_cuts = _security_cuts_from_constraint_results(scuc_dispatch, n_periods)
    sced_cuts = _security_cuts_from_constraint_results(dispatch, n_periods)
    for key, row in grouped.items():
        scuc_entry = scuc_cuts.get(key)
        sced_entry = sced_cuts.get(key)
        if scuc_entry is not None:
            row["shadow_price_scuc"] = scuc_entry["shadow_price"]
        if sced_entry is not None:
            row["shadow_price_sced"] = sced_entry["shadow_price"]
        # Combined: prefer SCED, fall back to SCUC.
        sced_sp = row["shadow_price_sced"]
        scuc_sp = row["shadow_price_scuc"]
        row["shadow_price"] = [
            (sced_sp[t] if sced_sp[t] is not None else scuc_sp[t])
            for t in range(n_periods)
        ]

    rows: list[dict[str, Any]] = []
    for row in grouped.values():
        utils = [u for u in row["utilization"] if u is not None]
        if not utils:
            continue
        worst = max(utils)
        if worst < threshold:
            continue
        max_abs_sp = max(
            (abs(x) for x in row["shadow_price"] if x is not None), default=0.0
        )
        is_breached = worst > 1.0 + 1e-6
        is_binding = max_abs_sp > 1e-6 or worst >= 0.999
        # Drop the per-stage series when neither stage produced any
        # signal — saves payload size and lets the JS hide the
        # SCUC/SCED legend entries.
        if not any(x is not None for x in row["shadow_price_scuc"]):
            row["shadow_price_scuc"] = None
        if not any(x is not None for x in row["shadow_price_sced"]):
            row["shadow_price_sced"] = None
        row["worst_utilization"] = float(worst)
        row["is_breached"] = is_breached
        row["is_binding"] = is_binding
        row["max_shadow_price"] = max_abs_sp
        rows.append(row)

    rows.sort(key=lambda r: -r["worst_utilization"])
    return rows


def _flatten_results(
    *,
    native_result: dict[str, Any],
    elapsed_secs: float,
    settlement: dict[str, Any],
    dispatch: dict[str, Any],
    scuc_dispatch: dict[str, Any] | None = None,
    inputs: _SolveInputs,
    scenario: dict[str, Any],
    policy: RtoPolicy | None = None,
) -> dict[str, Any]:
    """Collapse the raw goc3 native result + settlement + dispatch-result
    into a single frontend-shaped dict. Keeps the fields the dashboard
    needs and drops the rest so the JSON payload stays small."""
    n_periods = int(settlement.get("periods") or 0)
    totals = settlement["totals"]

    # LMPs per bus × period, plus mean / peak / min per period.
    # Reactive parallel: per-bus Q-LMP ($/MVAr-h) when AC SCED ran.
    lmps_by_bus: dict[int, list[float]] = {}
    q_lmps_by_bus: dict[int, list[float]] = {}
    voltages_by_bus: dict[int, list[float]] = {}
    lmp_period_mean: list[float] = []
    lmp_period_peak: list[float] = []
    lmp_period_min: list[float] = []
    q_lmp_period_mean: list[float] = []
    q_lmp_period_peak: list[float] = []
    q_lmp_period_min: list[float] = []
    has_q_lmp = False
    for per in settlement["lmps_per_period"]:
        ps: list[float] = []
        qs: list[float] = []
        for b in per["buses"]:
            bus_num = int(b["bus_number"])
            lmp = float(b.get("lmp") or 0.0)
            lmps_by_bus.setdefault(bus_num, []).append(lmp)
            ps.append(lmp)
            q_lmp_raw = b.get("q_lmp")
            q_lmp_val = float(q_lmp_raw) if q_lmp_raw is not None else 0.0
            if q_lmp_raw is not None:
                has_q_lmp = True
            q_lmps_by_bus.setdefault(bus_num, []).append(q_lmp_val)
            qs.append(q_lmp_val)
            v = b.get("voltage_pu")
            voltages_by_bus.setdefault(bus_num, []).append(
                float(v) if v is not None else 0.0
            )
        lmp_period_mean.append(sum(ps) / len(ps) if ps else 0.0)
        lmp_period_peak.append(max(ps) if ps else 0.0)
        lmp_period_min.append(min(ps) if ps else 0.0)
        q_lmp_period_mean.append(sum(qs) / len(qs) if qs else 0.0)
        q_lmp_period_peak.append(max(qs) if qs else 0.0)
        q_lmp_period_min.append(min(qs) if qs else 0.0)

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

    # Raw commitment + per-period Q (MVAr) from the native dispatch
    # result. The AC SCED stage stores reactive output under
    # ``resource_results[*].detail.q_mvar``; the SCUC-only path
    # leaves it ``None`` (no AC), in which case Q stays 0.
    n_periods_dispatch = len(dispatch.get("periods") or [])
    for g in gens_by_rid.values():
        g["q_mvar"] = [0.0] * n_periods_dispatch
    has_gen_q = False
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
                detail = res.get("detail") or {}
                q_mvar = detail.get("q_mvar")
                if q_mvar is not None:
                    gens_by_rid[rid]["q_mvar"][t] = float(q_mvar)
                    has_gen_q = True

    # The goc3 native pipeline models per-end HVDC reactive injection
    # by emitting two synthetic ``kind=generator`` resources per
    # link, named ``__dc_line_q__{link_id}__fr`` and
    # ``__dc_line_q__{link_id}__to``. They carry no real-power
    # dispatch (it's on ``period.hvdc_results[*].mw``) — only the
    # NLP's reactive support at each terminal. Surfacing them in the
    # generator list pollutes it with two zero-MW "gens" per link.
    # Pull them out and fold them into the HVDC tab data instead.
    HVDC_Q_PREFIX = "__dc_line_q__"
    hvdc_q_by_link: dict[str, dict[str, dict[str, Any]]] = {}
    real_gens_by_rid: dict[str, dict[str, Any]] = {}
    for rid, rec in gens_by_rid.items():
        if rid.startswith(HVDC_Q_PREFIX):
            tail = rid[len(HVDC_Q_PREFIX):]
            # Expected suffix: ``{link_id}__fr`` or ``{link_id}__to``.
            for end in ("__fr", "__to"):
                if tail.endswith(end):
                    link_id = tail[: -len(end)]
                    side = end.lstrip("_")
                    hvdc_q_by_link.setdefault(link_id, {})[side] = rec
                    break
            continue
        real_gens_by_rid[rid] = rec
    generators = list(real_gens_by_rid.values())

    # Layer per-resource AS awards onto each generator so the dashboard
    # can split revenue into Energy / AS columns and mark sparkline
    # periods where the unit was carrying any AS. The settlement's
    # ``resource_reserve_awards`` already filters out non-carrying
    # resources to keep the payload small; we just join here.
    rr_by_rid = {
        r["resource_id"]: r
        for r in (settlement.get("resource_reserve_awards") or [])
    }
    for g in generators:
        rec = rr_by_rid.get(g["resource_id"])
        if rec:
            g["reserve_awards_by_product"] = rec["by_product"]
            g["as_carrying_mask"] = rec["carrying_mask"]
            g["as_revenue_dollars"] = float(rec["total_revenue_dollars"])
        else:
            g["reserve_awards_by_product"] = {}
            g["as_carrying_mask"] = [False] * n_periods
            g["as_revenue_dollars"] = 0.0

    # Per-link HVDC time-series for the dedicated HVDC tab. Real-
    # power dispatch comes from ``period.hvdc_results`` (signed
    # from-end MW + delivered to-end MW after losses); reactive
    # support comes from the synthetic ``__dc_line_q__*`` pseudo-
    # gens we just stripped out of the generator list.
    hvdc_links_by_id: dict[str, dict[str, Any]] = {}
    for t, per in enumerate(dispatch.get("periods") or []):
        for entry in (per.get("hvdc_results") or []):
            link_id = str(entry.get("link_id") or "")
            if not link_id:
                continue
            link = hvdc_links_by_id.setdefault(
                link_id,
                {
                    "link_id": link_id,
                    "name": str(entry.get("name") or link_id),
                    "from_bus": None,
                    "to_bus": None,
                    "power_mw": [0.0] * n_periods,
                    "delivered_mw": [0.0] * n_periods,
                    "q_from_mvar": [0.0] * n_periods,
                    "q_to_mvar": [0.0] * n_periods,
                },
            )
            if t < n_periods:
                link["power_mw"][t] = float(entry.get("mw") or 0.0)
                link["delivered_mw"][t] = float(entry.get("delivered_mw") or 0.0)
    has_hvdc_q = False
    for link_id, sides in hvdc_q_by_link.items():
        link = hvdc_links_by_id.setdefault(
            link_id,
            {
                "link_id": link_id,
                "name": link_id,
                "from_bus": None,
                "to_bus": None,
                "power_mw": [0.0] * n_periods,
                "delivered_mw": [0.0] * n_periods,
                "q_from_mvar": [0.0] * n_periods,
                "q_to_mvar": [0.0] * n_periods,
            },
        )
        if "fr" in sides:
            link["from_bus"] = int(sides["fr"].get("bus") or 0) or link["from_bus"]
            q = sides["fr"].get("q_mvar") or []
            for t in range(min(n_periods, len(q))):
                link["q_from_mvar"][t] = float(q[t] or 0.0)
                if abs(q[t] or 0.0) > 1e-6:
                    has_hvdc_q = True
        if "to" in sides:
            link["to_bus"] = int(sides["to"].get("bus") or 0) or link["to_bus"]
            q = sides["to"].get("q_mvar") or []
            for t in range(min(n_periods, len(q))):
                link["q_to_mvar"][t] = float(q[t] or 0.0)
                if abs(q[t] or 0.0) > 1e-6:
                    has_hvdc_q = True
    hvdc_links = sorted(hvdc_links_by_id.values(), key=lambda l: l["link_id"])

    # Per-bus actual cleared load comes from ``bus_results.withdrawals_mw``
    # / ``withdrawals_mvar`` — the realised injection at each bus on
    # the converged dispatch. Goc3 cases run with dispatchable loads
    # (each ``sd_*`` resource carries its own bid curve) so the
    # cleared load may be less than the forecast when the LMP rises
    # above the load's bid; the dashboard's "served" trace reflects
    # that. Falls back to 0 on DC-only solves that don't surface
    # withdrawals.
    bus_p_by_period: dict[int, list[float]] = {}
    bus_q_by_period: dict[int, list[float]] = {}
    has_load_q = False
    has_load_served = False
    for t, per in enumerate(dispatch.get("periods") or []):
        for b in per.get("bus_results") or []:
            bus_num = int(b.get("bus_number") or 0)
            mw_arr = bus_p_by_period.setdefault(bus_num, [0.0] * n_periods)
            mw = b.get("withdrawals_mw")
            if mw is not None and t < len(mw_arr):
                mw_arr[t] = float(mw)
                has_load_served = True
            mvar_arr = bus_q_by_period.setdefault(bus_num, [0.0] * n_periods)
            mvar = b.get("withdrawals_mvar")
            if mvar is not None and t < len(mvar_arr):
                mvar_arr[t] = float(mvar)
                has_load_q = True

    # Detect whether the dispatch actually had dispatchable-load
    # resources. If so, surface that on the per-row "Handling"
    # column instead of the static "fixed" the scaffold defaults to.
    has_dispatchable_loads = any(
        res.get("kind") == "dispatchable_load"
        for per in (dispatch.get("periods") or [])
        for res in (per.get("resource_results") or [])
    )
    handling_label = "dispatchable" if has_dispatchable_loads else "fixed"

    loads = []
    load_forecast = inputs.load_forecast_mw
    for bus in sorted(load_forecast.keys()):
        forecast = load_forecast[bus]
        served_p = bus_p_by_period.get(int(bus))
        # Fall back to forecast when the dispatch didn't surface an
        # actual withdrawal (DC-only path on a non-goc3 case).
        if served_p is None or not has_load_served:
            served_p = list(forecast)
        served_q = bus_q_by_period.get(int(bus)) or [0.0] * n_periods
        # Shed = forecast − cleared. Dispatchable loads can clear at
        # less than forecast when their bid sits below the LMP; for
        # fixed loads this is always 0.
        shed = [
            max(0.0, (forecast[t] if t < len(forecast) else 0.0) - served_p[t])
            for t in range(n_periods)
        ]
        loads.append({
            "bus": int(bus),
            "nominal_mw": list(forecast),
            "served_mw": served_p,
            "served_mvar": served_q,
            "shed_mw": shed,
            "handling": handling_label,
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
    branch_flows_by_period = _compute_branch_flows(dispatch, inputs.network)
    peak_utilization = 0.0
    for rows in branch_flows_by_period:
        for r in rows:
            u = r.get("utilization")
            if u is not None and u > peak_utilization:
                peak_utilization = u

    # Objective + branches + contingencies for the new diagnostic
    # tabs. The server emits everything above a low floor (0.5)
    # and the dashboard's threshold knob filters client-side so
    # the user can sweep the threshold without re-solving.
    threshold = float(
        ((scenario.get("ui") or {}).get("branches_threshold") or 0.9)
    )
    server_floor = 0.5
    objective_breakdown = _objective_breakdown(dispatch, scuc_dispatch, n_periods)
    branches_summary = _branches_summary(
        dispatch,
        scuc_dispatch,
        inputs.network,
        branch_flows_by_period,
        threshold=server_floor,
    )
    contingencies_summary = _contingencies_summary(
        dispatch, scuc_dispatch, n_periods, threshold=server_floor
    )

    return {
        "status": "ok",
        "elapsed_secs": float(elapsed_secs or 0.0),
        "solve_mode": (policy.solve_mode if policy else "scuc_ac_sced"),
        "lmp_source": _lmp_source_label(policy or RtoPolicy()),
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
        # Reactive views — populated only when AC SCED ran (the
        # SCUC-only path leaves q_lmp empty and ``has_q_lmp = False``;
        # the dashboard hides reactive UI in that case).
        "has_reactive": bool(has_q_lmp),
        "q_lmps_by_bus": (
            {str(k): v for k, v in q_lmps_by_bus.items()} if has_q_lmp else {}
        ),
        "q_lmp_aggregates": {
            "per_period_mean": q_lmp_period_mean,
            "per_period_peak": q_lmp_period_peak,
            "per_period_min": q_lmp_period_min,
        },
        "voltages_by_bus": (
            {str(k): v for k, v in voltages_by_bus.items()}
            if any(any(v) for v in voltages_by_bus.values()) else {}
        ),
        "generators": generators,
        "hvdc_links": hvdc_links,
        "has_hvdc": bool(hvdc_links),
        "has_hvdc_reactive": has_hvdc_q,
        "loads": loads,
        "reserve_awards": reserve_awards,
        "violations": violations,
        "branch_flows_by_period": branch_flows_by_period,
        "objective_breakdown": objective_breakdown,
        "branches_summary": branches_summary,
        "contingencies_summary": contingencies_summary,
        "ui": {
            "branches_threshold": threshold,
        },
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
