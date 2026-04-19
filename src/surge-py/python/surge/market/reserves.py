# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Standard reserve product definitions and zonal requirement builders.

Provides ISO-standard reserve product templates derived from the
GO Competition Challenge 3 formulation (§4.6).  Users can use these
directly or define custom products.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class ReserveProductDef:
    """Definition of a reserve product for dispatch optimization.

    This is a template — it defines the *kind* of product (regulation up,
    spinning, etc.) and its qualification rules.  Zonal requirements and
    per-device offers are built separately using these templates.

    Attributes:
        id: Unique product identifier used in dispatch requests.
        name: Human-readable product name.
        direction: ``"Up"`` or ``"Down"``.
        qualification: Who can provide this reserve:
            ``"Committed"`` — online units only,
            ``"Synchronized"`` — online and synchronized units,
            ``"OfflineQuickStart"`` — offline fast-start units.
        energy_coupling: How reserve capacity couples to energy dispatch:
            ``"Headroom"`` — limited by ``pmax - p_dispatch``,
            ``"Footroom"`` — limited by ``p_dispatch - pmin``,
            ``"ReactiveHeadroom"`` — limited by ``qmax - q_dispatch``,
            ``"None"`` — no energy coupling.
        shared_limit_products: Product IDs sharing a device-level capacity
            limit with this product.  E.g., spinning + reg_up share the
            online headroom envelope.
        balance_products: Product IDs whose excess provision can satisfy
            this product's zonal requirement (waterfall substitution).
        deploy_secs: Deployment time in seconds.
        kind: ``"Active"`` (real-power reserve) or ``"Reactive"``.
    """

    id: str
    name: str
    direction: str
    qualification: str
    energy_coupling: str
    shared_limit_products: tuple[str, ...] = ()
    balance_products: tuple[str, ...] = ()
    deploy_secs: int = 600
    kind: str = "Real"
    #: Shortfall-demand curve (``$/MW``). The native request requires this
    #: on every product. A linear curve with ``cost_per_unit`` priced at
    #: the ISO's scarcity value is the common case.
    shortfall_cost_per_mw: float = 1_000.0

    def to_product_dict(self) -> dict[str, Any]:
        """Build a Surge ``ReserveProduct`` dict for a dispatch request.

        Field names and enum values match the native request schema:
        ``id`` (not ``product_id``), ``deploy_secs``, and ``kind`` is
        one of ``"Real" | "Reactive" | "ReactiveHeadroom"``.
        """
        kind = {
            "Active": "Real",
            "Real": "Real",
            "Reactive": "Reactive",
            "ReactiveHeadroom": "ReactiveHeadroom",
        }.get(self.kind, self.kind)
        d: dict[str, Any] = {
            "id": self.id,
            "name": self.name,
            "kind": kind,
            "direction": self.direction,
            "qualification": self.qualification,
            "energy_coupling": self.energy_coupling,
            "deploy_secs": float(self.deploy_secs),
            "demand_curve": {
                "type": "linear",
                "cost_per_unit": float(self.shortfall_cost_per_mw),
            },
        }
        if self.shared_limit_products:
            d["shared_limit_products"] = list(self.shared_limit_products)
        if self.balance_products:
            d["balance_products"] = list(self.balance_products)
        return d


# ---------------------------------------------------------------------------
# Standard reserve product templates
# ---------------------------------------------------------------------------

REG_UP = ReserveProductDef(
    id="reg_up",
    name="Regulation Up",
    direction="Up",
    qualification="Committed",
    energy_coupling="Headroom",
    deploy_secs=300,
)

REG_DOWN = ReserveProductDef(
    id="reg_down",
    name="Regulation Down",
    direction="Down",
    qualification="Committed",
    energy_coupling="Footroom",
    deploy_secs=300,
)

SPINNING = ReserveProductDef(
    id="syn",
    name="Synchronized Reserve",
    direction="Up",
    qualification="Synchronized",
    energy_coupling="Headroom",
    shared_limit_products=("reg_up",),
    balance_products=("reg_up",),
    deploy_secs=600,
)

NON_SPINNING = ReserveProductDef(
    id="nsyn",
    name="Non-Synchronized Reserve",
    direction="Up",
    qualification="OfflineQuickStart",
    energy_coupling="None",
    shared_limit_products=("reg_up", "syn"),
    balance_products=("reg_up", "syn"),
    deploy_secs=600,
)

RAMP_UP_ON = ReserveProductDef(
    id="ramp_up_on",
    name="Ramping Reserve Up (Online)",
    direction="Up",
    qualification="Committed",
    energy_coupling="Headroom",
    shared_limit_products=("reg_up", "syn"),
    balance_products=("ramp_up_off",),
    deploy_secs=900,
)

RAMP_UP_OFF = ReserveProductDef(
    id="ramp_up_off",
    name="Ramping Reserve Up (Offline)",
    direction="Up",
    qualification="OfflineQuickStart",
    energy_coupling="None",
    shared_limit_products=("nsyn",),
    balance_products=("ramp_up_on",),
    deploy_secs=900,
)

RAMP_DOWN_ON = ReserveProductDef(
    id="ramp_down_on",
    name="Ramping Reserve Down (Online)",
    direction="Down",
    qualification="Committed",
    energy_coupling="Footroom",
    shared_limit_products=("reg_down",),
    balance_products=("ramp_down_off",),
    deploy_secs=900,
)

RAMP_DOWN_OFF = ReserveProductDef(
    id="ramp_down_off",
    name="Ramping Reserve Down (Offline)",
    direction="Down",
    qualification="Committed",
    energy_coupling="Footroom",
    balance_products=("ramp_down_on",),
    deploy_secs=900,
)

REACTIVE_UP = ReserveProductDef(
    id="q_res_up",
    name="Reactive Reserve Up",
    direction="Up",
    qualification="Committed",
    energy_coupling="None",
    kind="Reactive",
    deploy_secs=600,
)

REACTIVE_DOWN = ReserveProductDef(
    id="q_res_down",
    name="Reactive Reserve Down",
    direction="Down",
    qualification="Committed",
    energy_coupling="None",
    kind="Reactive",
    deploy_secs=600,
)

#: All standard active-power reserve products (ordered by convention).
STANDARD_ACTIVE_PRODUCTS: tuple[ReserveProductDef, ...] = (
    REG_UP,
    REG_DOWN,
    SPINNING,
    NON_SPINNING,
    RAMP_UP_ON,
    RAMP_UP_OFF,
    RAMP_DOWN_ON,
    RAMP_DOWN_OFF,
)

#: All standard reserve products including reactive.
STANDARD_ALL_PRODUCTS: tuple[ReserveProductDef, ...] = (
    *STANDARD_ACTIVE_PRODUCTS,
    REACTIVE_UP,
    REACTIVE_DOWN,
)

#: Lookup by product ID.
PRODUCT_BY_ID: dict[str, ReserveProductDef] = {p.id: p for p in STANDARD_ALL_PRODUCTS}


# ---------------------------------------------------------------------------
# Zonal requirement builder
# ---------------------------------------------------------------------------

@dataclass
class ZonalRequirement:
    """A reserve requirement for a single zone and product.

    Supports both fixed per-period requirements and endogenous requirements
    that scale with dispatch decisions.
    """

    zone_id: int | str
    product_id: str
    shortfall_cost_per_unit: float = 0.0

    #: Fixed per-period requirement (MW).  Mutually exclusive with
    #: endogenous coefficients.
    per_period_mw: list[float] | None = None

    #: Scalar requirement (used when per_period_mw is None).
    requirement_mw: float = 0.0

    #: Endogenous scaling: fraction of served dispatchable load.
    served_dispatchable_load_coefficient: float | None = None

    #: Endogenous scaling: fraction of largest online generator dispatch.
    largest_generator_dispatch_coefficient: float | None = None

    #: Bus numbers participating in this zone.
    participant_bus_numbers: list[int] | None = None

    def to_dict(self) -> dict[str, Any]:
        """Emit a dict for ``market.zonal_reserve_requirements``."""
        d: dict[str, Any] = {
            "zone_id": int(self.zone_id) if isinstance(self.zone_id, int) else self.zone_id,
            "product_id": self.product_id,
            "requirement_mw": float(self.requirement_mw),
        }
        if self.per_period_mw is not None:
            d["per_period_mw"] = list(self.per_period_mw)
        if self.shortfall_cost_per_unit:
            d["shortfall_cost_per_unit"] = float(self.shortfall_cost_per_unit)
        if self.served_dispatchable_load_coefficient is not None:
            d["served_dispatchable_load_coefficient"] = float(self.served_dispatchable_load_coefficient)
        if self.largest_generator_dispatch_coefficient is not None:
            d["largest_generator_dispatch_coefficient"] = float(self.largest_generator_dispatch_coefficient)
        if self.participant_bus_numbers is not None:
            d["participant_bus_numbers"] = list(self.participant_bus_numbers)
        return d


def build_reserve_products_dict(
    products: list[ReserveProductDef] | tuple[ReserveProductDef, ...],
) -> list[dict[str, Any]]:
    """Convert a list of ``ReserveProductDef`` to dispatch request dicts."""
    return [p.to_product_dict() for p in products]
