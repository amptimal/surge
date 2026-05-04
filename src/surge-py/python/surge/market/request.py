# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Typed builder for the canonical :class:`DispatchRequest`.

Motivation
----------

A dispatch request is a deeply-nested dict with ~40 top-level
possibilities spread across ``timeline``, ``commitment``, ``coupling``,
``profiles``, ``market``, ``network``, ``state.initial``, and ``runtime``.
Hand-assembling the dict — as every ``markets/*/problem.py`` used to
do — is dict-soup: string keys, no IDE autocomplete, no type checking,
and typos that only surface when the Rust solver deserialises.

:class:`DispatchRequestBuilder` replaces the dict literals with a
chainable, typed API. Each method mutates the builder in place and
returns ``self``, so the common case reads top-to-bottom:

.. code-block:: python

    from surge.market import request, REG_UP, ZonalRequirement

    req = (
        request()
        .timeline(periods=24, hours_by_period=[1.0] * 24)
        .commitment_optimize(mip_rel_gap=1e-3)
        .coupling("time_coupled")
        .load_profile(bus=4, values=load_forecast)
        .generator_offers([offer_schedule])
        .zonal_reserves([ZonalRequirement(zone_id=1, product_id="reg_up",
                                          requirement_mw=5, per_period_mw=[5]*24)])
        .market_config(cfg)   # fills in penalty/network defaults
        .run_pricing(True)
        .build()              # → surge._generated.DispatchRequest (TypedDict)
    )

Design notes
------------

* **Not every Rust field is promoted.** The schema has ~80 leaf fields.
  The builder promotes the 20-ish that cover 80% of the markets we've
  written. Anything not promoted flows through the
  ``extend_*`` / :meth:`raw_merge` escape hatches; the Rust solver still
  accepts the request. When two callers need a new field, add a method.

* **Accepts the existing typed dataclasses.**
  :class:`GeneratorOfferSchedule`, :class:`ZonalRequirement`, and
  :class:`ReserveProductDef` render themselves via ``to_request_dict`` /
  ``to_product_dict`` / ``to_dict`` — the builder takes them as-is
  without a second layer of typed wrappers.

* **Merge semantics preserve caller intent.** :meth:`market_config` only
  fills missing keys (``apply_defaults_to_request`` semantics), so a
  builder call that explicitly set ``penalty_config`` isn't clobbered
  by a later ``market_config(cfg)`` call.

* **No scalar broadcasting on profiles.** ``.load_profile(values=...)``
  requires ``values`` be a list whose length matches ``periods``. Forcing
  explicit length is a guardrail against a scalar sneaking through as a
  "fill" value.
"""

from __future__ import annotations

import copy
from dataclasses import dataclass, field
from typing import Any, Literal, Mapping, Sequence

from .._generated.dispatch_request import DispatchRequest
from .loads import DispatchableLoadOfferSchedule, DispatchableLoadSpec
from .offers import GeneratorOfferSchedule, GeneratorReserveOfferSchedule
from .reserves import ReserveProductDef, ZonalRequirement


# Type aliases used in method signatures. Kept local to this module to
# avoid polluting surge.market's public namespace.
_Coupling = Literal["period_by_period", "time_coupled"]
_Formulation = Literal["dc", "ac"]


def _merge_missing(target: dict[str, Any], defaults: dict[str, Any]) -> dict[str, Any]:
    """Recursive merge — caller-set keys in ``target`` win over ``defaults``."""
    merged = copy.deepcopy(target)
    for key, value in defaults.items():
        if key not in merged:
            merged[key] = copy.deepcopy(value)
        elif isinstance(merged[key], dict) and isinstance(value, dict):
            merged[key] = _merge_missing(merged[key], value)
    return merged


@dataclass
class DispatchRequestBuilder:
    """Chainable builder for the canonical :class:`DispatchRequest`.

    Construct via :func:`surge.market.request`; each mutating method
    returns ``self`` so calls compose top-to-bottom. Call :meth:`build`
    to produce the final ``DispatchRequest`` dict.
    """

    _periods: int = 0
    _timeline: dict[str, Any] = field(default_factory=dict)
    _commitment: Any = "all_committed"
    _coupling: _Coupling | None = None
    _formulation: _Formulation | None = None
    _profiles: dict[str, Any] = field(default_factory=dict)
    _market: dict[str, Any] = field(default_factory=dict)
    _network: dict[str, Any] = field(default_factory=dict)
    _state_initial: dict[str, Any] = field(default_factory=dict)
    _runtime: dict[str, Any] = field(default_factory=dict)

    # ----- timeline -----------------------------------------------------

    def timeline(
        self,
        *,
        periods: int,
        hours_by_period: Sequence[float],
    ) -> "DispatchRequestBuilder":
        """Set the study timeline.

        ``hours_by_period`` must have exactly ``periods`` entries. Pass
        ``[h] * periods`` explicitly if all intervals share a duration —
        the builder does not broadcast a scalar (see module docstring).
        """
        periods = int(periods)
        if periods <= 0:
            raise ValueError(f"periods must be > 0, got {periods}")
        hours = [float(h) for h in hours_by_period]
        if len(hours) != periods:
            raise ValueError(
                f"hours_by_period has {len(hours)} entries, expected {periods}"
            )
        self._periods = periods
        self._timeline = {
            "periods": periods,
            "interval_hours_by_period": hours,
        }
        return self

    # ----- formulation + coupling --------------------------------------

    def formulation(self, value: _Formulation) -> "DispatchRequestBuilder":
        """Select ``"dc"`` or ``"ac"`` formulation for the dispatch."""
        if value not in ("dc", "ac"):
            raise ValueError(f"formulation must be 'dc' or 'ac', got {value!r}")
        self._formulation = value
        return self

    def coupling(self, value: _Coupling) -> "DispatchRequestBuilder":
        """Select ``"period_by_period"`` or ``"time_coupled"`` coupling."""
        if value not in ("period_by_period", "time_coupled"):
            raise ValueError(
                f"coupling must be 'period_by_period' or 'time_coupled', got {value!r}"
            )
        self._coupling = value
        return self

    # ----- commitment --------------------------------------------------

    def commitment_all_committed(self) -> "DispatchRequestBuilder":
        """Every resource is online for every period (LP-friendly)."""
        self._commitment = "all_committed"
        return self

    def commitment_optimize(
        self,
        *,
        mip_rel_gap: float | None = None,
        time_limit_secs: float | None = None,
        mip_gap_schedule: Sequence[tuple[float, float]] | None = None,
        disable_warm_start: bool = False,
        initial_conditions: Sequence[dict[str, Any]] | None = None,
    ) -> "DispatchRequestBuilder":
        """SCUC: solve commitment endogenously as a MIP."""
        options: dict[str, Any] = {}
        if mip_rel_gap is not None:
            options["mip_rel_gap"] = float(mip_rel_gap)
        if time_limit_secs is not None:
            options["time_limit_secs"] = float(time_limit_secs)
        if mip_gap_schedule is not None:
            options["mip_gap_schedule"] = [
                (float(t), float(g)) for t, g in mip_gap_schedule
            ]
        if disable_warm_start:
            options["disable_warm_start"] = True
        if initial_conditions is not None:
            options["initial_conditions"] = [dict(ic) for ic in initial_conditions]
        self._commitment = {"optimize": options}
        return self

    def commitment_fixed(
        self,
        *,
        resources: Sequence[dict[str, Any]],
    ) -> "DispatchRequestBuilder":
        """Pin commitment to a caller-supplied schedule.

        ``resources`` is a list of
        ``{"resource_id": ..., "initial": bool, "periods": [bool, ...]}``.
        See :class:`~surge._generated.dispatch_request.ResourceCommitmentSchedule`.
        """
        self._commitment = {"fixed": {"resources": [dict(r) for r in resources]}}
        return self

    # ----- profiles ----------------------------------------------------

    def load_profile(
        self,
        *,
        bus: int,
        values: Sequence[float],
    ) -> "DispatchRequestBuilder":
        """Add one per-bus active-power load forecast (MW per period)."""
        self._require_periods("load_profile")
        values = [float(v) for v in values]
        if len(values) != self._periods:
            raise ValueError(
                f"load_profile(bus={bus}): values has {len(values)} entries, "
                f"expected {self._periods}"
            )
        entry = {"bus_number": int(bus), "values_mw": values}
        self._profiles.setdefault("load", {}).setdefault("profiles", []).append(entry)
        return self

    def renewable_profile(
        self,
        *,
        resource: str,
        capacity_factors: Sequence[float],
    ) -> "DispatchRequestBuilder":
        """Add a per-resource capacity-factor profile for a renewable."""
        self._require_periods("renewable_profile")
        caps = [float(v) for v in capacity_factors]
        if len(caps) != self._periods:
            raise ValueError(
                f"renewable_profile({resource!r}): capacity_factors has {len(caps)} "
                f"entries, expected {self._periods}"
            )
        entry = {"resource_id": str(resource), "capacity_factors": caps}
        self._profiles.setdefault("renewable", {}).setdefault("profiles", []).append(entry)
        return self

    def generator_derate(
        self,
        *,
        resource: str,
        derate_factors: Sequence[float],
    ) -> "DispatchRequestBuilder":
        """Add a per-resource availability profile (0-1 fraction of pmax)."""
        self._require_periods("generator_derate")
        factors = [float(v) for v in derate_factors]
        if len(factors) != self._periods:
            raise ValueError(
                f"generator_derate({resource!r}): derate_factors has {len(factors)} "
                f"entries, expected {self._periods}"
            )
        entry = {"resource_id": str(resource), "derate_factors": factors}
        self._profiles.setdefault("generator_derates", {}).setdefault(
            "profiles", []
        ).append(entry)
        return self

    def generator_dispatch_bounds(
        self,
        *,
        resource: str,
        p_min_mw: Sequence[float],
        p_max_mw: Sequence[float],
    ) -> "DispatchRequestBuilder":
        """Pin a resource's per-period dispatch window directly in MW.

        Use for must-take / fixed-output resources (e.g. baseload nuclear)
        where both the floor and ceiling are determined by the availability
        schedule rather than the LP. Setting ``p_min_mw == p_max_mw`` pins
        output exactly.
        """
        self._require_periods("generator_dispatch_bounds")
        pmin = [float(v) for v in p_min_mw]
        pmax = [float(v) for v in p_max_mw]
        if len(pmin) != self._periods or len(pmax) != self._periods:
            raise ValueError(
                f"generator_dispatch_bounds({resource!r}): p_min_mw / p_max_mw "
                f"must have {self._periods} entries"
            )
        entry = {
            "resource_id": str(resource),
            "p_min_mw": pmin,
            "p_max_mw": pmax,
        }
        self._profiles.setdefault("generator_dispatch_bounds", {}).setdefault(
            "profiles", []
        ).append(entry)
        return self

    def branch_derate(
        self,
        *,
        branch_id: str,
        derates: Sequence[float],
    ) -> "DispatchRequestBuilder":
        """Add a per-branch derate profile (fraction in ``[0, 1]``)."""
        self._require_periods("branch_derate")
        values = [float(v) for v in derates]
        if len(values) != self._periods:
            raise ValueError(
                f"branch_derate({branch_id!r}): derates has {len(values)} "
                f"entries, expected {self._periods}"
            )
        entry = {"branch_id": str(branch_id), "values": values}
        self._profiles.setdefault("branch_derates", {}).setdefault(
            "profiles", []
        ).append(entry)
        return self

    # ----- market payload ----------------------------------------------

    def reserve_products(
        self, products: Sequence[ReserveProductDef]
    ) -> "DispatchRequestBuilder":
        """Register the AS product catalog for the study."""
        self._market["reserve_products"] = [p.to_product_dict() for p in products]
        return self

    def zonal_reserves(
        self, requirements: Sequence[ZonalRequirement]
    ) -> "DispatchRequestBuilder":
        """Set the zonal reserve requirements."""
        self._market["zonal_reserve_requirements"] = [r.to_dict() for r in requirements]
        return self

    def generator_offers(
        self, schedules: Sequence[GeneratorOfferSchedule]
    ) -> "DispatchRequestBuilder":
        """Per-resource generator energy-offer schedules."""
        self._require_periods("generator_offers")
        self._market["generator_offer_schedules"] = [
            s.to_request_dict(self._periods) for s in schedules
        ]
        return self

    def reserve_offers(
        self, schedules: Sequence[GeneratorReserveOfferSchedule]
    ) -> "DispatchRequestBuilder":
        """Per-resource generator reserve-offer schedules."""
        self._require_periods("reserve_offers")
        self._market["generator_reserve_offer_schedules"] = [
            s.to_request_dict(self._periods) for s in schedules
        ]
        return self

    def must_run_units(
        self, resource_ids: Sequence[str]
    ) -> "DispatchRequestBuilder":
        """Pin the listed generators to be committed in every period.

        The SCUC MIP forces ``u[t]=1`` for each listed resource at all
        periods, so when paired with ``generator_dispatch_bounds`` that
        also pins ``p_min == p_max``, the LP has no commitment or
        dispatch freedom on the resource — exactly what's needed for
        baseload nuclear / must-take PPAs / fixed-output IPP contracts.
        """
        ids = [str(r) for r in resource_ids]
        self._market["must_run_units"] = {"resource_ids": ids}
        return self

    def peak_demand_charges(
        self,
        charges: Sequence[Mapping[str, Any]],
    ) -> "DispatchRequestBuilder":
        """Per-resource coincident-peak demand charges.

        Each entry is a mapping with the keys ``name`` (caller-supplied
        identifier), ``resource_id`` (the generator whose MW dispatch
        feeds the peak — typically a virtual grid-import generator at
        the POI bus), ``period_indices`` (list of integer period
        indices to include in the peak set, e.g. the four expected
        4-CP intervals), and ``charge_per_mw`` (linear cost coefficient
        in ``$ / MW`` applied to the auxiliary peak variable).

        The SCUC LP allocates one auxiliary ``peak_mw[i] ≥ 0`` column
        per entry, emits ``peak_mw[i] ≥ pg[t][resource]`` for each
        ``t ∈ period_indices``, and adds ``charge_per_mw * peak_mw[i]``
        to the objective. This is the canonical formulation for
        transmission demand charges (4-CP, NYISO ICAP coincident peak,
        industrial tariff demand charges) — it minimises the *maximum*
        dispatch across the flagged periods at the given $/MW rate.
        """
        rendered: list[dict[str, Any]] = []
        for entry in charges:
            rendered.append(
                {
                    "name": str(entry["name"]),
                    "resource_id": str(entry["resource_id"]),
                    "period_indices": [int(p) for p in entry["period_indices"]],
                    "charge_per_mw": float(entry["charge_per_mw"]),
                }
            )
        self._market["peak_demand_charges"] = rendered
        return self

    def storage_reserve_soc_impacts(
        self, impacts: Sequence[Mapping[str, Any]]
    ) -> "DispatchRequestBuilder":
        """Per-storage per-product SOC impact factors used by the LP's
        reserve-SOC-headroom rows.

        Each entry is a mapping with the keys
        ``resource_id``, ``product_id``, and
        ``values_mwh_per_mw`` (length = horizon periods). A positive
        value means "deployment uses SOC" (up-direction reserves); a
        negative value means "deployment fills SOC" (down-direction).
        Magnitude of ``1 / η_dis`` (up) or ``η_ch`` (down) gives the
        full-period 100 %-deployment cap.
        """
        rendered: list[dict[str, Any]] = []
        for entry in impacts:
            rendered.append(
                {
                    "resource_id": str(entry["resource_id"]),
                    "product_id": str(entry["product_id"]),
                    "values_mwh_per_mw": [float(v) for v in entry["values_mwh_per_mw"]],
                }
            )
        self._market["storage_reserve_soc_impacts"] = rendered
        return self

    def dispatchable_loads(
        self,
        loads: Sequence[DispatchableLoadSpec | dict[str, Any]],
    ) -> "DispatchRequestBuilder":
        """Declare dispatchable-load resources.

        Accepts :class:`DispatchableLoadSpec` (preferred) or a raw dict
        matching the Rust ``DispatchableLoad`` shape.
        """
        self._market["dispatchable_loads"] = [
            spec.to_request_dict() if isinstance(spec, DispatchableLoadSpec) else dict(spec)
            for spec in loads
        ]
        return self

    def dispatchable_load_offers(
        self,
        schedules: Sequence[DispatchableLoadOfferSchedule | dict[str, Any]],
    ) -> "DispatchRequestBuilder":
        """Per-resource offer schedules for dispatchable loads.

        Accepts :class:`DispatchableLoadOfferSchedule` (preferred) or a
        raw dict already shaped for the request.
        """
        self._require_periods("dispatchable_load_offers")
        rendered: list[dict[str, Any]] = []
        for s in schedules:
            if isinstance(s, DispatchableLoadOfferSchedule):
                rendered.append(s.to_request_dict(self._periods))
            else:
                rendered.append(dict(s))
        self._market["dispatchable_load_offer_schedules"] = rendered
        return self

    def penalty_config(self, cfg: dict[str, Any]) -> "DispatchRequestBuilder":
        """Set the penalty tensor directly (e.g. from ``MarketConfig.to_penalty_dict()``)."""
        self._market["penalty_config"] = copy.deepcopy(cfg)
        return self

    def market_config(self, cfg: Any) -> "DispatchRequestBuilder":
        """Fill missing ``market`` and ``network`` defaults from a :class:`MarketConfig`.

        Caller-set keys are preserved — this is a fill-in-the-blanks
        merge, not an override. Uses
        :meth:`MarketConfig.apply_defaults_to_request` semantics.
        """
        self._market = _merge_missing(self._market, {"penalty_config": cfg.to_penalty_dict()})
        self._network = _merge_missing(self._network, cfg.network_rules.to_dict())
        return self

    # ----- initial state -----------------------------------------------

    def previous_dispatch(
        self, mapping: dict[str, float]
    ) -> "DispatchRequestBuilder":
        """Set per-resource previous-period MW for ramp initialisation."""
        self._state_initial["previous_resource_dispatch"] = [
            {"resource_id": str(rid), "mw": float(mw)}
            for rid, mw in mapping.items()
        ]
        return self

    def storage_soc_overrides(
        self, mapping: dict[str, float]
    ) -> "DispatchRequestBuilder":
        """Set per-resource initial SOC (MWh) for storage resources."""
        self._state_initial["storage_soc_overrides"] = [
            {"resource_id": str(rid), "soc_mwh": float(v)}
            for rid, v in mapping.items()
        ]
        return self

    # ----- runtime -----------------------------------------------------

    def run_pricing(self, enabled: bool = True) -> "DispatchRequestBuilder":
        """Toggle the LMP-pricing re-solve."""
        self._runtime["run_pricing"] = bool(enabled)
        return self

    # ----- escape hatches ----------------------------------------------

    def extend_market(self, **kwargs: Any) -> "DispatchRequestBuilder":
        """Merge raw keys into the ``market`` payload.

        For fields the builder doesn't promote (e.g. ``system_reserve_requirements``,
        ``virtual_bids``, ``ramp_sharing``). Values are deep-copied.
        """
        for key, value in kwargs.items():
            self._market[key] = copy.deepcopy(value)
        return self

    def extend_network(self, **kwargs: Any) -> "DispatchRequestBuilder":
        """Merge raw keys into the ``network`` payload (e.g. ``flowgates``)."""
        for key, value in kwargs.items():
            self._network[key] = copy.deepcopy(value)
        return self

    def extend_state_initial(self, **kwargs: Any) -> "DispatchRequestBuilder":
        """Merge raw keys into ``state.initial``."""
        for key, value in kwargs.items():
            self._state_initial[key] = copy.deepcopy(value)
        return self

    def extend_runtime(self, **kwargs: Any) -> "DispatchRequestBuilder":
        """Merge raw keys into ``runtime``."""
        for key, value in kwargs.items():
            self._runtime[key] = copy.deepcopy(value)
        return self

    def raw_merge(self, request: dict[str, Any]) -> "DispatchRequestBuilder":
        """Deep-merge a raw request dict into the builder's current state.

        Caller's current state wins on conflicts — this is
        ``apply_defaults_to_request`` semantics, used when you want to
        splice in a subtree without overriding fields you've already set.
        """
        # Build a shadow request from current state, merge the raw dict as
        # defaults, then re-project back into the builder fields.
        current = self._build_dict()
        merged = _merge_missing(current, request)
        self._reset_from_dict(merged)
        return self

    # ----- output ------------------------------------------------------

    def build(self) -> DispatchRequest:
        """Materialise the final :class:`DispatchRequest` dict."""
        if not self._timeline:
            raise ValueError(
                "DispatchRequestBuilder.build(): timeline(...) must be set "
                "before build()"
            )
        return self._build_dict()  # type: ignore[return-value]

    # ----- internals ---------------------------------------------------

    def _require_periods(self, method_name: str) -> None:
        if self._periods <= 0:
            raise ValueError(
                f"DispatchRequestBuilder.{method_name}(): timeline(...) must be "
                f"called first so periods is known"
            )

    def _build_dict(self) -> dict[str, Any]:
        out: dict[str, Any] = {}
        if self._formulation is not None:
            out["formulation"] = self._formulation
        if self._timeline:
            out["timeline"] = copy.deepcopy(self._timeline)
        out["commitment"] = copy.deepcopy(self._commitment)
        if self._coupling is not None:
            out["coupling"] = self._coupling
        if self._profiles:
            out["profiles"] = copy.deepcopy(self._profiles)
        if self._market:
            out["market"] = copy.deepcopy(self._market)
        if self._network:
            out["network"] = copy.deepcopy(self._network)
        if self._state_initial:
            out["state"] = {"initial": copy.deepcopy(self._state_initial)}
        if self._runtime:
            out["runtime"] = copy.deepcopy(self._runtime)
        return out

    def _reset_from_dict(self, request: dict[str, Any]) -> None:
        self._timeline = dict(request.get("timeline", {}))
        self._periods = int(self._timeline.get("periods", 0))
        self._commitment = request.get("commitment", "all_committed")
        self._coupling = request.get("coupling")
        self._formulation = request.get("formulation")
        self._profiles = dict(request.get("profiles", {}))
        self._market = dict(request.get("market", {}))
        self._network = dict(request.get("network", {}))
        state = request.get("state", {})
        self._state_initial = dict(state.get("initial", {})) if isinstance(state, dict) else {}
        self._runtime = dict(request.get("runtime", {}))


def request() -> DispatchRequestBuilder:
    """Open a fresh :class:`DispatchRequestBuilder`."""
    return DispatchRequestBuilder()


__all__ = ["DispatchRequestBuilder", "request"]
