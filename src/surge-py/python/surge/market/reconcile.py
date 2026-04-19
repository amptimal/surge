# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""AC reconciliation helpers for the dispatch pipeline.

Provides warm-start construction, dispatch bounds pinning, and the
core ``redispatch_with_ac()`` function that bridges the DC SCUC
and AC SCED stages.
"""

from __future__ import annotations

import json
import logging
import os
import time
from pathlib import Path
from typing import Any

from .config import AcReconcileConfig, MarketConfig

logger = logging.getLogger("surge.market.reconcile")


# ---------------------------------------------------------------------------
# Result access helpers
# ---------------------------------------------------------------------------

def _period_records(dispatch_result: Any) -> list[dict[str, Any]]:
    return list(getattr(dispatch_result, "periods", []))


def _resource_period_lookup(dispatch_result: Any) -> list[dict[str, dict[str, Any]]]:
    result: list[dict[str, dict[str, Any]]] = []
    for period in _period_records(dispatch_result):
        by_id: dict[str, dict[str, Any]] = {}
        for rr in period.get("resource_results", []):
            rid = rr.get("resource_id", "")
            if rid:
                by_id[rid] = rr
        result.append(by_id)
    return result


def _resource_result_lookup_for_period(
    dispatch_result: Any, period_idx: int,
) -> dict[str, dict[str, Any]]:
    lookup = _resource_period_lookup(dispatch_result)
    return lookup[period_idx] if period_idx < len(lookup) else {}


def _bus_voltage_warm_start_for_period(
    network: Any,
    dispatch_result: Any,
    period_idx: int,
) -> tuple[list[float], list[float]] | None:
    periods = _period_records(dispatch_result)
    if period_idx >= len(periods):
        return None
    period = periods[period_idx]
    bus_results = period.get("bus_results", [])
    if not bus_results:
        return None

    bus_by_number: dict[int, dict[str, Any]] = {}
    for br in bus_results:
        bn = br.get("bus_number")
        if bn is not None:
            bus_by_number[int(bn)] = br

    buses = list(getattr(network, "buses", []))
    if not buses:
        return None

    vm_list: list[float] = []
    va_list: list[float] = []
    for bus in buses:
        bn = int(getattr(bus, "number", 0))
        br = bus_by_number.get(bn, {})
        vm_list.append(float(br.get("voltage_pu", 1.0)))
        va_list.append(float(br.get("angle_rad", 0.0)))
    return vm_list, va_list


# ---------------------------------------------------------------------------
# Warm start schedule construction
# ---------------------------------------------------------------------------

def build_warm_start_schedule(
    dispatch_result: Any,
    network: Any,
    *,
    periods: int | None = None,
    generator_resource_ids: set[str] | None = None,
    dispatchable_load_resource_ids: set[str] | None = None,
) -> dict[str, Any]:
    """Build an ``ac_dispatch_warm_start`` schedule from a DC dispatch result.

    Extracts bus voltages/angles and generator P/Q dispatch from the DC
    result to seed the AC OPF warm start.

    Args:
        dispatch_result: DC SCUC dispatch result.
        network: Surge network object.
        periods: Number of periods (defaults to all).
        generator_resource_ids: Generator resource IDs in the network.
        dispatchable_load_resource_ids: DL resource IDs in the network.

    Returns:
        Dict suitable for ``runtime.ac_dispatch_warm_start``.
    """
    period_records = _period_records(dispatch_result)
    n_periods = len(period_records) if periods is None else min(periods, len(period_records))

    if generator_resource_ids is None:
        generator_resource_ids = {
            str(getattr(g, "id", "")).strip()
            for g in getattr(network, "generators", [])
            if str(getattr(g, "id", "")).strip()
        }
    if dispatchable_load_resource_ids is None:
        dispatchable_load_resource_ids = set()

    # Bus warm starts
    bus_entries: list[dict[str, Any]] = []
    buses = list(getattr(network, "buses", []))
    if buses and period_records:
        bus_vm: dict[int, list[float]] = {}
        bus_va: dict[int, list[float]] = {}
        for bus in buses:
            bn = int(getattr(bus, "number", 0))
            bus_vm[bn] = []
            bus_va[bn] = []

        for t in range(n_periods):
            period = period_records[t] if t < len(period_records) else {}
            bus_results_by_number: dict[int, dict[str, Any]] = {}
            for br in period.get("bus_results", []):
                bn = br.get("bus_number")
                if bn is not None:
                    bus_results_by_number[int(bn)] = br
            for bus in buses:
                bn = int(getattr(bus, "number", 0))
                br = bus_results_by_number.get(bn, {})
                bus_vm[bn].append(float(br.get("voltage_pu", 1.0)))
                bus_va[bn].append(float(br.get("angle_rad", 0.0)))

        for bus in buses:
            bn = int(getattr(bus, "number", 0))
            bus_entries.append({
                "bus_number": bn,
                "vm_pu": bus_vm[bn],
                "va_rad": bus_va[bn],
            })

    # Generator warm starts
    gen_entries: list[dict[str, Any]] = []
    period_lookup = _resource_period_lookup(dispatch_result)
    for rid in sorted(generator_resource_ids):
        p_series: list[float] = []
        q_series: list[float] = []
        for t in range(n_periods):
            rr = period_lookup[t].get(rid, {}) if t < len(period_lookup) else {}
            p_series.append(float(rr.get("power_mw", 0.0) or 0.0))
            q_series.append(float(rr.get("reactive_power_mvar", 0.0) or 0.0))
        gen_entries.append({
            "resource_id": rid,
            "p_mw": p_series,
            "q_mvar": q_series,
        })

    # DL warm starts
    dl_entries: list[dict[str, Any]] = []
    for rid in sorted(dispatchable_load_resource_ids):
        p_series = []
        q_series = []
        for t in range(n_periods):
            rr = period_lookup[t].get(rid, {}) if t < len(period_lookup) else {}
            p_series.append(float(rr.get("power_mw", 0.0) or 0.0))
            q_series.append(float(rr.get("reactive_power_mvar", 0.0) or 0.0))
        dl_entries.append({
            "resource_id": rid,
            "p_mw": p_series,
            "q_mvar": q_series,
        })

    result: dict[str, Any] = {"buses": bus_entries, "generators": gen_entries}
    if dl_entries:
        result["dispatchable_loads"] = dl_entries
    return result


# ---------------------------------------------------------------------------
# Dispatch bounds pinning
# ---------------------------------------------------------------------------

# Producer active-reserve product IDs whose awards eat into the upper P band
# (headroom) for committed generators. Mirrors `_GO_RESERVE_PRODUCT_MAP` in
# `markets/go_c3/adapter.py`; listed here because this helper is used by
# callers outside the GO C3 pipeline too. Offline-only products (`nsyn`,
# `ramp_up_off`) are intentionally excluded because they don't bind the
# committed-gen upper headroom.
_UPWARD_RESERVE_PRODUCT_IDS: frozenset[str] = frozenset(
    {"reg_up", "syn", "ramp_up_on"}
)
# Same list for downward reserves (footroom).
_DOWNWARD_RESERVE_PRODUCT_IDS: frozenset[str] = frozenset(
    {"reg_down", "ramp_down_on"}
)


def pin_dispatch_bounds(
    request: dict[str, Any],
    network: Any,
    dispatch_result: Any,
    *,
    periods: int,
    device_kind_by_uid: dict[str, str] | None = None,
    relax_pmin: bool = False,
    relax_pmin_for_resources: set[str] | None = None,
    producer_band_fraction: float = 0.05,
    producer_band_floor_mw: float = 1.0,
) -> None:
    """Band generator dispatch around the DC target for AC redispatch.

    Every committed producer gets a symmetric band of
    ``max(producer_band_floor_mw, |target| * producer_band_fraction)`` MW
    around its DC dispatch, clipped to the physical per-period envelope the
    adapter wrote into ``profiles.generator_dispatch_bounds``. The band lets
    every generator participate in the AC redispatch rather than pinning P
    to a single slack-bus generator.

    Down-headroom for active reserve awards: if the DC SCED cleared active
    reserves (``reg_up``/``syn``/``ramp_up_on``/``reg_down``/``ramp_down_on``)
    on a generator, the upper band is shrunk by the sum of up-awards and the
    lower band is raised by the sum of down-awards so the committed reserve
    headroom is preserved through the AC pass.

    .. TODO(proper)::

        The long-term fix is to stop stripping active reserves in
        ``markets/go_c3/reconcile.py::_filter_ac_market_to_reactive_reserves``
        and let the AC SCED re-clear them against the distributed dispatch.
        That requires NLP-level support for active reserve variables in
        ``surge-opf`` (currently only reactive reserves are first-class).

    Args:
        request: AC dispatch request dict (modified in place).
        network: Surge network object.
        dispatch_result: DC SCUC dispatch result.
        periods: Number of periods.
        device_kind_by_uid: Map of resource ID → device kind. Producers and
            ``producer_static`` (renewables/DC-terminal Q support) are handled;
            all other kinds are skipped.
        relax_pmin: If True, floor the lower band at 0 instead of the physical
            per-period p_min (used by the second-pass fallback when the tight
            band can't close an AC NLP feasibility gap).
        relax_pmin_for_resources: Specific resources to relax pmin for.
        producer_band_fraction: Symmetric band width as a fraction of |target|.
        producer_band_floor_mw: Minimum symmetric band width (MW).
    """
    profiles_block = request.setdefault("profiles", {})
    bounds_block = profiles_block.setdefault("generator_dispatch_bounds", {})
    profile_list = bounds_block.setdefault("profiles", [])

    # The adapter writes per-period physical bounds into each profile. We
    # treat those as the envelope and clip the band to stay inside it, so
    # per-period derating, forced-offline periods, and commitment-fixed zero
    # windows all survive the reconcile pass unchanged.
    existing: dict[str, dict[str, Any]] = {}
    for entry in profile_list:
        rid = str(entry.get("resource_id", "")).strip()
        if rid:
            existing[rid] = entry

    period_lookup = _resource_period_lookup(dispatch_result)
    relaxed_ids = {str(r).strip() for r in (relax_pmin_for_resources or set())}

    if device_kind_by_uid is None:
        device_kind_by_uid = {}

    for generator in getattr(network, "generators", []):
        resource_id = str(getattr(generator, "id", "")).strip()
        if not resource_id:
            continue
        kind = device_kind_by_uid.get(resource_id, "producer")
        if kind not in {"producer", "producer_static"}:
            continue

        do_relax = relax_pmin or resource_id in relaxed_ids

        # Scalar generator limits as a conservative fallback when the profile
        # is missing (should not happen for GO C3, but `pin_dispatch_bounds`
        # is a public helper that may be called against hand-built requests).
        scalar_pmin = float(getattr(generator, "pmin_mw", getattr(generator, "pmin", 0.0)) or 0.0)
        scalar_pmax = float(getattr(generator, "pmax_mw", getattr(generator, "pmax", 1e9)) or 1e9)

        profile = existing.get(resource_id)
        existing_p_min: list[float] | None = None
        existing_p_max: list[float] | None = None
        if profile is not None:
            raw_min = profile.get("p_min_mw")
            raw_max = profile.get("p_max_mw")
            if isinstance(raw_min, list):
                existing_p_min = [float(v) for v in raw_min]
            if isinstance(raw_max, list):
                existing_p_max = [float(v) for v in raw_max]

        banded_pmin: list[float] = []
        banded_pmax: list[float] = []

        for t in range(periods):
            rr = period_lookup[t].get(resource_id, {}) if t < len(period_lookup) else {}

            # Physical per-period envelope from the adapter. Fall back to the
            # scalar generator limits if the profile didn't carry them.
            if existing_p_min is not None and t < len(existing_p_min):
                physical_lb = existing_p_min[t]
            else:
                physical_lb = scalar_pmin
            if existing_p_max is not None and t < len(existing_p_max):
                physical_ub = existing_p_max[t]
            else:
                physical_ub = scalar_pmax
            if physical_ub < physical_lb:
                physical_ub = physical_lb

            if kind == "producer_static":
                # Renewables modeled as static producers are fixed at their
                # forecast (GO C3 wires them in via the load-injection path),
                # so they stay hard-pinned at zero dispatch.
                banded_pmin.append(0.0)
                banded_pmax.append(0.0)
                continue

            target = max(float(rr.get("power_mw", 0.0) or 0.0), 0.0)
            detail = rr.get("detail") if isinstance(rr, dict) else None
            startup_or_shutdown = isinstance(detail, dict) and bool(
                detail.get("startup") or detail.get("shutdown")
            )

            if startup_or_shutdown:
                floor = 0.0 if do_relax else physical_lb
                lower = max(target, floor)
                upper = min(target, physical_ub)
                if upper < lower:
                    upper = lower
                banded_pmin.append(float(lower))
                banded_pmax.append(float(upper))
                continue

            band = max(producer_band_floor_mw, abs(target) * producer_band_fraction)

            floor = 0.0 if do_relax else physical_lb
            lower = max(target - band, floor)
            upper = min(target + band, physical_ub)

            # Reserve headroom shrink — preserve committed DC reserve awards.
            reserve_awards = rr.get("reserve_awards") if isinstance(rr, dict) else None
            if isinstance(reserve_awards, dict):
                up_award = 0.0
                down_award = 0.0
                for product_id, award_mw in reserve_awards.items():
                    try:
                        mw = float(award_mw)
                    except (TypeError, ValueError):
                        continue
                    if mw <= 0.0:
                        continue
                    if product_id in _UPWARD_RESERVE_PRODUCT_IDS:
                        up_award += mw
                    elif product_id in _DOWNWARD_RESERVE_PRODUCT_IDS:
                        down_award += mw
                if up_award > 0.0:
                    upper = min(upper, physical_ub - up_award)
                if down_award > 0.0:
                    lower = max(lower, physical_lb + down_award)

            # Final clip to the physical envelope and enforce lower ≤ upper
            # (reserve shrink can invert the band on tight gens — prefer the
            # upper side to keep committed-pmax reservation intact).
            lower = max(lower, physical_lb if not do_relax else 0.0)
            upper = min(upper, physical_ub)
            if upper < lower:
                upper = lower

            banded_pmin.append(float(lower))
            banded_pmax.append(float(upper))

        if profile is None:
            profile = {"resource_id": resource_id}
            profile_list.append(profile)
            existing[resource_id] = profile
        profile["p_min_mw"] = banded_pmin
        profile["p_max_mw"] = banded_pmax


# ---------------------------------------------------------------------------
# AC redispatch entry point
# ---------------------------------------------------------------------------

def redispatch_with_ac(
    network_builder,
    request_builder,
    *,
    dispatch_result: Any,
    fixed_resources: list[dict[str, Any]],
    surge_module: Any,
    config: MarketConfig,
    lp_solver: str | None = None,
    nlp_solver: str | None = None,
    periods: int | None = None,
    relax_pmin: bool = False,
    relax_pmin_for_resources: set[str] | None = None,
    fixed_hvdc_dispatch: list[dict[str, Any]] | None = None,
    use_sced_ac_benders: bool = False,
    device_kind_by_uid: dict[str, str] | None = None,
    slack_bus_numbers: set[int] | None = None,
) -> tuple[Any, dict[str, Any], dict[str, Any]]:
    """Run AC OPF redispatch with fixed commitment from the DC stage.

    This is the core AC reconciliation function.  It:

    1. Builds a fresh AC network and request via the provided builders.
    2. Pins commitment to the DC result.
    3. Pins active-power dispatch within a slack band of the DC result.
    4. Constructs warm starts from the DC result.
    5. Applies target-tracking penalties.
    6. Solves the AC OPF.

    Args:
        network_builder: Callable returning ``(network, context_dict)``
            for an AC formulation.
        request_builder: Callable returning a dispatch request dict
            for an AC formulation.
        dispatch_result: DC SCUC dispatch result.
        fixed_resources: Commitment schedule (per-resource, per-period).
        surge_module: The ``surge`` module (for ``solve_dispatch``).
        config: Market configuration with AC reconciliation settings.
        lp_solver: LP solver backend name.
        nlp_solver: NLP solver backend name.
        periods: Number of periods (defaults to all).
        relax_pmin: Allow generators to dispatch to zero.
        relax_pmin_for_resources: Specific resources to relax.
        fixed_hvdc_dispatch: Fixed HVDC dispatch schedule.
        use_sced_ac_benders: Use Benders decomposition instead of target tracking.
        device_kind_by_uid: Resource ID → device kind mapping.
        slack_bus_numbers: Slack bus numbers.

    Returns:
        Tuple of ``(ac_result, ac_request, ac_report)``.
    """
    ac_network, ac_context = network_builder()
    ac_request = config.apply_defaults_to_request(request_builder())

    effective_periods = periods or len(_period_records(dispatch_result))

    # Fix commitment
    ac_request["commitment"] = {
        "fixed": {"resources": fixed_resources},
    }

    # Remove security constraints for AC pass
    network_options = ac_request.get("network")
    if isinstance(network_options, dict):
        network_options.pop("security", None)

    # Pin dispatch bounds
    pin_dispatch_bounds(
        ac_request, ac_network, dispatch_result,
        periods=effective_periods,
        device_kind_by_uid=device_kind_by_uid,
        relax_pmin=relax_pmin,
        relax_pmin_for_resources=relax_pmin_for_resources,
        producer_band_fraction=config.ac_reconcile.producer_band_fraction,
        producer_band_floor_mw=config.ac_reconcile.producer_band_floor_mw,
    )

    # Set up runtime
    runtime = ac_request.setdefault("runtime", {})
    if relax_pmin or relax_pmin_for_resources:
        runtime["ac_relax_committed_pmin_to_zero"] = True

    # Warm start
    gen_ids = {
        str(getattr(g, "id", "")).strip()
        for g in getattr(ac_network, "generators", [])
        if str(getattr(g, "id", "")).strip()
    }
    dl_ids = {
        str(e.get("resource_id", "")).strip()
        for e in ac_request.get("market", {}).get("dispatchable_loads", [])
        if str(e.get("resource_id", "")).strip()
    }
    runtime["ac_dispatch_warm_start"] = build_warm_start_schedule(
        dispatch_result, ac_network,
        periods=effective_periods,
        generator_resource_ids=gen_ids,
        dispatchable_load_resource_ids=dl_ids,
    )

    # Target tracking
    if not use_sced_ac_benders:
        runtime["ac_target_tracking"] = config.ac_reconcile.to_target_tracking_dict()

    # AC OPF overrides
    runtime["ac_opf"] = config.ac_reconcile.to_opf_overrides_dict()

    # Benders
    if use_sced_ac_benders:
        runtime["sced_ac_benders"] = {
            "eta_periods": list(range(effective_periods)),
            "cuts": [],
            "orchestration": config.benders.to_orchestration_dict(),
        }

    # Fixed HVDC dispatch
    if fixed_hvdc_dispatch:
        runtime["fixed_hvdc_dispatch"] = [
            {
                **entry,
                "p_mw": entry["p_mw"][:effective_periods],
                **(
                    {"q_fr_mvar": entry["q_fr_mvar"][:effective_periods]}
                    if isinstance(entry.get("q_fr_mvar"), list) else {}
                ),
                **(
                    {"q_to_mvar": entry["q_to_mvar"][:effective_periods]}
                    if isinstance(entry.get("q_to_mvar"), list) else {}
                ),
            }
            for entry in fixed_hvdc_dispatch
        ]

    # Set initial generator dispatch on the network
    first_period = _resource_result_lookup_for_period(dispatch_result, 0)
    for generator in getattr(ac_network, "generators", []):
        resource_id = str(getattr(generator, "id", "")).strip()
        if not resource_id:
            continue
        kind = (device_kind_by_uid or {}).get(resource_id, "producer")
        if kind not in {"producer", "producer_static"}:
            continue
        pmax = float(getattr(generator, "pmax_mw", 0.0))
        real_pmin = float(getattr(generator, "pmin_mw", 0.0))
        do_relax = relax_pmin or (
            relax_pmin_for_resources is not None and resource_id in relax_pmin_for_resources
        )
        pmin = 0.0 if do_relax else real_pmin
        target = 0.0
        if kind == "producer":
            target = float(first_period.get(resource_id, {}).get("power_mw", 0.0) or 0.0)
        ac_network.set_generator_p(resource_id, target)
        ac_network.set_generator_limits(resource_id, pmax, pmin)

    # Debug dump
    dump_path = os.environ.get("SURGE_DUMP_AC_REQUEST")
    if dump_path:
        try:
            p = Path(dump_path).expanduser().resolve()
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps(ac_request, indent=2, sort_keys=True) + "\n")
        except Exception:
            pass

    # Solve
    logger.info("AC redispatch: calling solve_dispatch (lp=%s, nlp=%s)", lp_solver, nlp_solver)
    started = time.perf_counter()
    result = surge_module.solve_dispatch(
        ac_network, ac_request,
        lp_solver=lp_solver, nlp_solver=nlp_solver,
    )
    elapsed = time.perf_counter() - started

    summary = dict(getattr(result, "summary", {}))
    result_periods = _period_records(result)
    report: dict[str, Any] = {
        "mode": "ac_dispatch",
        "summary": {
            "periods": len(result_periods),
            "solve_time_secs": elapsed,
            "dispatch_total_cost": summary.get("total_cost"),
            "dispatch_total_energy_cost": summary.get("total_energy_cost"),
            "dispatch_total_no_load_cost": summary.get("total_no_load_cost"),
            "dispatch_total_startup_cost": summary.get("total_startup_cost"),
            "nlp_solver": nlp_solver or "default",
        },
    }
    logger.info(
        "AC redispatch complete: %d periods, cost=$%.2f, %.2fs",
        len(result_periods),
        summary.get("total_cost", 0.0) or 0.0,
        elapsed,
    )
    return result, ac_request, report


# ---------------------------------------------------------------------------
# Commitment extraction
# ---------------------------------------------------------------------------

def extract_fixed_commitment(
    dispatch_result: Any,
    request: dict[str, Any] | None = None,
    *,
    include_initial: bool = True,
) -> list[dict[str, Any]]:
    """Extract per-resource commitment schedule from a dispatch result.

    Returns a list of ``{"resource_id": str, "initial": bool,
    "periods": [bool, ...]}`` dicts suitable for
    ``request.commitment.fixed.resources``. ``initial`` defaults to the
    first period's status; pass ``include_initial=False`` to omit it.
    """
    period_records = _period_records(dispatch_result)
    resource_status: dict[str, list[bool]] = {}

    for t, period in enumerate(period_records):
        for rr in period.get("resource_results", []):
            rid = rr.get("resource_id", "")
            if not rid:
                continue
            on = bool(rr.get("on_status", False))
            if rid not in resource_status:
                resource_status[rid] = []
            while len(resource_status[rid]) < t:
                resource_status[rid].append(False)
            resource_status[rid].append(on)

    out: list[dict[str, Any]] = []
    for rid, statuses in sorted(resource_status.items()):
        entry: dict[str, Any] = {"resource_id": rid, "periods": statuses}
        if include_initial:
            entry["initial"] = statuses[0] if statuses else False
        out.append(entry)
    return out


def extract_storage_end_soc(
    dispatch_result: Any,
    resource_id: str,
    *,
    fallback: float | None = None,
) -> float | None:
    """Read a storage resource's end-of-horizon SOC (MWh) from a result.

    Looks up ``resource_results`` for the last period and returns the
    ``detail.soc_mwh`` field. Returns *fallback* if the resource is
    not present or the field is missing.
    """
    periods = _period_records(dispatch_result)
    if not periods:
        return fallback
    for rr in periods[-1].get("resource_results", []):
        if rr.get("resource_id") != resource_id:
            continue
        detail = rr.get("detail") or {}
        if "soc_mwh" in detail:
            return float(detail["soc_mwh"])
    return fallback


def build_all_committed_schedule(
    network: Any,
    periods: int,
) -> list[dict[str, Any]]:
    """Build a commitment schedule with all generators committed."""
    resources: list[dict[str, Any]] = []
    for generator in getattr(network, "generators", []):
        rid = str(getattr(generator, "id", "")).strip()
        if not rid:
            continue
        in_service = bool(getattr(generator, "in_service", True))
        resources.append({
            "resource_id": rid,
            "periods": [in_service] * periods,
        })
    return resources


# ---------------------------------------------------------------------------
# Decommit probes
# ---------------------------------------------------------------------------

def identify_resources_at_pmin(
    dispatch_result: Any,
    request: dict[str, Any],
    *,
    pmin_tolerance_mw: float = 1.0,
) -> set[str]:
    """Find committed resources dispatched at or near pmin.

    These are candidates for decommitment probes.
    """
    at_pmin: set[str] = set()
    period_lookup = _resource_period_lookup(dispatch_result)

    # Get pmin bounds from request
    bounds_profiles = (
        request.get("profiles", {})
        .get("generator_dispatch_bounds", {})
        .get("profiles", [])
    )
    pmin_by_resource: dict[str, list[float]] = {}
    for entry in bounds_profiles:
        rid = str(entry.get("resource_id", ""))
        if rid and "p_min_mw" in entry:
            pmin_by_resource[rid] = entry["p_min_mw"]

    for t, period_resources in enumerate(period_lookup):
        for rid, rr in period_resources.items():
            if not rr.get("on_status", False):
                continue
            p_mw = float(rr.get("power_mw", 0.0) or 0.0)
            pmin_series = pmin_by_resource.get(rid, [])
            pmin_mw = pmin_series[t] if t < len(pmin_series) else 0.0
            if p_mw <= pmin_mw + pmin_tolerance_mw:
                at_pmin.add(rid)

    return at_pmin


def build_decommit_feedback(
    dc_dispatch_result: Any,
    ac_probe_result: Any,
    *,
    idle_p_threshold_mw: float = 1.0,
    idle_q_threshold_mvar: float = 1.0,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Compare DC and AC probe results to identify decommit candidates.

    Runs after an AC probe where pmin is relaxed to zero.  Resources
    that the AC solve idles (P≈0, Q≈0) are candidates for decommitment.

    Returns:
        Tuple of ``(min_commitment_adds, decommit_cuts)``.
    """
    dc_periods = _resource_period_lookup(dc_dispatch_result)
    ac_periods = _resource_period_lookup(ac_probe_result)

    min_commits: list[dict[str, Any]] = []
    decommit_cuts: list[dict[str, Any]] = []

    seen_resources: set[str] = set()
    for t, dc_period in enumerate(dc_periods):
        ac_period = ac_periods[t] if t < len(ac_periods) else {}
        for rid, dc_rr in dc_period.items():
            if not dc_rr.get("on_status", False):
                continue
            if rid in seen_resources:
                continue

            ac_rr = ac_period.get(rid, {})
            ac_p = abs(float(ac_rr.get("power_mw", 0.0) or 0.0))
            ac_q = abs(float(ac_rr.get("reactive_power_mvar", 0.0) or 0.0))

            if ac_p < idle_p_threshold_mw and ac_q < idle_q_threshold_mvar:
                decommit_cuts.append({
                    "resource_id": rid,
                    "periods": [t],
                    "reason": f"AC probe idle: P={ac_p:.1f}MW, Q={ac_q:.1f}MVar",
                })
                seen_resources.add(rid)
            else:
                min_commits.append({
                    "resource_id": rid,
                    "periods": [t],
                })
                seen_resources.add(rid)

    return min_commits, decommit_cuts


# ---------------------------------------------------------------------------
# Thermal slack summary
# ---------------------------------------------------------------------------

def thermal_slack_summary(dispatch_result: Any) -> dict[str, Any]:
    """Extract thermal slack summary from a dispatch result."""
    max_mva = 0.0
    total_mva = 0.0
    count = 0
    for period in _period_records(dispatch_result):
        for cr in period.get("constraint_results", []):
            if cr.get("kind") == "ThermalLimit":
                slack = abs(float(cr.get("slack_mw", 0.0) or 0.0))
                if slack > 1e-6:
                    total_mva += slack
                    max_mva = max(max_mva, slack)
                    count += 1
    return {
        "thermal_slack_max_mva": round(max_mva, 4),
        "thermal_slack_total_mva": round(total_mva, 4),
        "thermal_slack_count": count,
    }
