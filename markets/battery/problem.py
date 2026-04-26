# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Battery operator problem + in-memory surge.Network builder.

The battery operator problem is a single-site, price-taker
optimisation: given an exogenous LMP forecast (and optionally AS
price forecasts), schedule the BESS's charge / discharge /
AS-capacity awards to maximise net revenue subject to SOC dynamics
and the POI limit.

## How this is modelled in Surge's LP

The battery site is a 1-bus network. Three dispatchable resources
sit on that bus:

1. **BESS** — a storage generator with ``pmin = -charge_mw_max`` and
   ``pmax = +discharge_mw_max`` plus :class:`StorageParams` carrying
   SOC dynamics, efficiency, and degradation.
2. **Virtual grid-import gen** — a conventional generator with
   per-period offer curve equal to the LMP forecast. Supplies the
   bus whenever the site needs to pull power from the grid
   (BESS charging or site load).
3. **Virtual grid-export DL** — a curtailable dispatchable load
   with per-period linear-curtailment cost equal to the LMP
   forecast. Absorbs power the site exports (BESS discharge).

Because both the virtual-gen cost and the virtual-DL curtailment
cost equal the LMP at each period, the LP's objective reduces to::

    maximise  Σ LMP[t] × BESS_net_output[t] × duration[t]
              − Σ degradation × (charge[t] + discharge[t]) × duration[t]
              + AS revenue (if enabled)

which is exactly the battery-operator surplus.

Optional site fixtures:
* **Site fixed load** — any hourly consumption profile attached to
  the same bus. Affects the grid import/export net flow but not the
  BESS's own economics.
* **Site PV** — an on-site solar/wind resource; modelled as a
  zero-cost generator with a renewable capacity-factor profile.

AS co-optimization: each :class:`AsProduct` adds a reserve product
(``REG_UP``, ``SPINNING``, etc.) with a per-period offer from the
BESS priced at the NEGATIVE of the forecast AS price — the LP picks
up the offer up to the battery's headroom / footroom, and the
negative cost credits the operator at the forecast price. A zonal
requirement of 0 MW registers the zone without forcing any
shortfall penalty.
"""

from __future__ import annotations

import dataclasses
from dataclasses import dataclass, field
from typing import Any

from surge.market import (
    DispatchableLoadOfferSchedule,
    DispatchableLoadSpec,
    GeneratorOfferSchedule,
    GeneratorReserveOfferSchedule,
    ReserveProductDef,
    ZonalRequirement,
    linear_curtailment,
    request as _request_builder,
)


def _decouple_product(p: ReserveProductDef) -> ReserveProductDef:
    """Strip ``balance_products`` and ``shared_limit_products`` from a
    standard ISO reserve product definition.

    The ISO templates (e.g. SPINNING) declare that reg-up awards can
    substitute for spin's zonal requirement (``balance_products =
    ("reg_up",)``). In an ISO clearing engine that keeps the system
    whole at minimum penalty, this is correct — a single MW of reg-up
    is a higher-quality service that also covers spin demand. But for a
    single-asset battery operator's price-taker view, the
    substitution-ladder credits the same MW twice (it reduces *both*
    reg-up and spin slack penalties), so the LP picks reg-up over spin
    even when spin's per-product price is higher. The operator's
    actual revenue is per-product price × award, with no
    cross-coupling — so we strip the ladder before handing the products
    to the LP. Cross-product physical headroom is still enforced by the
    SCUC's cross-headroom row family (sums all Up-direction awards),
    which doesn't depend on ``shared_limit_products``.
    """
    return dataclasses.replace(p, balance_products=(), shared_limit_products=())


# ---------------------------------------------------------------------------
# Site / bid / AS dataclasses — the caller-facing problem inputs.
# ---------------------------------------------------------------------------


@dataclass
class SiteSpec:
    """Physical description of a battery site."""

    #: Point-of-interconnection limit in MW (same limit both directions).
    poi_limit_mw: float

    #: BESS charge rate limit (MW, positive).
    bess_power_charge_mw: float

    #: BESS discharge rate limit (MW, positive).
    bess_power_discharge_mw: float

    #: BESS usable energy capacity (MWh).
    bess_energy_mwh: float

    #: Charge-leg efficiency in (0, 1]: fraction of metered charge MW that
    #: reaches the SoC reservoir. Most of a lithium-ion battery's round-trip
    #: loss lives on this side (inverter + DC bus).
    bess_charge_efficiency: float = 0.90

    #: Discharge-leg efficiency in (0, 1]: fraction of SoC draw that reaches
    #: the grid. Typically higher than charge efficiency for modern inverters.
    bess_discharge_efficiency: float = 0.98

    #: SOC minimum as a fraction of energy capacity.
    bess_soc_min_fraction: float = 0.10

    #: SOC maximum as a fraction of energy capacity.
    bess_soc_max_fraction: float = 0.95

    #: Discharge-side power foldback threshold, as a fraction of energy
    #: capacity. Below this SOC the discharge MW cap derates linearly to
    #: zero at ``bess_soc_min_fraction``. ``None`` disables the cut.
    bess_discharge_foldback_fraction: float | None = None

    #: Charge-side power foldback threshold, as a fraction of energy
    #: capacity. Above this SOC the charge MW cap derates linearly to
    #: zero at ``bess_soc_max_fraction``. ``None`` disables the cut.
    bess_charge_foldback_fraction: float | None = None

    #: Initial SOC. ``None`` → 50 % of capacity.
    bess_initial_soc_mwh: float | None = None

    #: Degradation cost applied to every MWh of throughput
    #: (charge AND discharge). Sensible default 0 for a pure LMP
    #: arbitrage study; raise to e.g. 2–5 $/MWh for wear modelling.
    bess_degradation_cost_per_mwh: float = 0.0

    #: Maximum full equivalent cycles per 24-hour window. One FEC =
    #: full charge + full discharge of the rated energy capacity.
    #: ``None`` (the default) leaves throughput uncapped. The cap is
    #: enforced as a linear constraint inside the time-coupled SCUC
    #: build; period-by-period (sequential) solves cannot enforce it
    #: because they have no inter-period coupling.
    bess_daily_cycle_limit: float | None = None

    #: Optional site baseline load (MW, per period). Adds a fixed
    #: load at the POI bus — the grid still supplies / absorbs the net.
    site_load_mw: list[float] | None = None

    #: Optional on-site PV / wind capacity per period (MW). Creates a
    #: zero-cost generator with per-period derate matching the
    #: capacity factor. Spill beyond this max becomes curtailment.
    site_pv_mw: list[float] | None = None


@dataclass
class AsProduct:
    """One ancillary-services product the BESS bids into.

    ``price_forecast_per_mwh`` is the exogenous price forecast
    (what the market is expected to clear at). In both
    ``optimal_foresight`` and ``pwl_offers`` dispatch modes, the
    battery's AS revenue is reported as ``award × forecast_price``.

    In ``pwl_offers`` mode, the operator's own AS bid prices come
    from :class:`PwlBidStrategy.as_offer_prices_per_mwh` — the LP
    clears the battery's offer only when the forecast shortfall
    cost (= forecast price) exceeds the bid price.
    """

    product_def: ReserveProductDef
    price_forecast_per_mwh: list[float]


@dataclass
class PwlBidStrategy:
    """The battery operator's PWL bid strategy for ``pwl_offers`` mode.

    Models the bids the operator submits to the market. Energy bids
    are static over the horizon (Surge's storage offer curves are
    horizon-static; use :attr:`BatteryPolicy.period_coupling` =
    ``"sequential"`` to vary bids per period by running one-period
    solves with different strategies).

    Semantics:

    * ``discharge_offer_segments`` — cumulative (MW, $/MWh) segments.
      ``[(25.0, 50.0)]`` means "I'll discharge up to 25 MW at $50/MWh";
      ``[(10.0, 30.0), (25.0, 60.0)]`` offers the first 10 MW at $30
      and the next 15 MW at $60. Energy dispatches only when LMP ≥
      offer price for that segment.

    * ``charge_bid_segments`` — cumulative (MW, $/MWh) segments.
      ``[(25.0, 20.0)]`` means "I'll charge up to 25 MW at up to
      $20/MWh". Energy charges only when LMP ≤ bid price for that
      segment.

    * ``as_offer_prices_per_mwh`` — per-product scalar price the
      battery bids for each AS product. When the forecast AS price
      exceeds the bid, the LP awards the capacity.
    """

    discharge_offer_segments: list[tuple[float, float]]
    charge_bid_segments: list[tuple[float, float]]
    as_offer_prices_per_mwh: dict[str, float] = field(default_factory=dict)

    #: Optional per-period price overrides. Outer length = periods;
    #: each inner list length = number of segments. ``None`` at either
    #: level means "use the scalar baseline for that period/segment".
    #: Only honored in sequential mode (via
    #: ``build_network(period_index=...)``); MW breakpoints remain
    #: horizon-static.
    discharge_offer_price_per_period: list[list[float | None] | None] | None = None
    charge_bid_price_per_period: list[list[float | None] | None] | None = None

    @classmethod
    def flat(
        cls,
        *,
        discharge_capacity_mw: float,
        discharge_price: float,
        charge_capacity_mw: float,
        charge_price: float,
        as_offer_prices_per_mwh: dict[str, float] | None = None,
    ) -> "PwlBidStrategy":
        """Convenience constructor for a flat 1-segment bid strategy."""
        return cls(
            discharge_offer_segments=[(float(discharge_capacity_mw), float(discharge_price))],
            charge_bid_segments=[(float(charge_capacity_mw), float(charge_price))],
            as_offer_prices_per_mwh=dict(as_offer_prices_per_mwh or {}),
        )


# ---------------------------------------------------------------------------
# Problem: site + forecasts → (surge.Network, DispatchRequest dict).
# ---------------------------------------------------------------------------


@dataclass
class BatteryProblem:
    """Complete battery-operator problem: site + forecasts.

    Required for ``dispatch_mode="pwl_offers"``: supply
    :attr:`pwl_strategy` with the operator's discharge-offer,
    charge-bid, and AS-offer prices.
    """

    period_durations_hours: list[float]
    lmp_forecast_per_mwh: list[float]
    site: SiteSpec
    as_products: list[AsProduct] = field(default_factory=list)

    #: Required when :attr:`BatteryPolicy.dispatch_mode` is
    #: ``"pwl_offers"``. Ignored in ``"optimal_foresight"`` mode.
    pwl_strategy: PwlBidStrategy | None = None

    bus_number: int = 1
    base_kv: float = 138.0
    base_mva: float = 100.0

    @property
    def periods(self) -> int:
        return len(self.period_durations_hours)

    def __post_init__(self) -> None:
        periods = self.periods
        if periods <= 0:
            raise ValueError("period_durations_hours must be non-empty")
        if len(self.lmp_forecast_per_mwh) != periods:
            raise ValueError(
                f"lmp_forecast_per_mwh length ({len(self.lmp_forecast_per_mwh)}) "
                f"does not match periods ({periods})"
            )
        if self.site.site_load_mw is not None and len(self.site.site_load_mw) != periods:
            raise ValueError(
                f"site_load_mw length ({len(self.site.site_load_mw)}) "
                f"does not match periods ({periods})"
            )
        if self.site.site_pv_mw is not None and len(self.site.site_pv_mw) != periods:
            raise ValueError(
                f"site_pv_mw length ({len(self.site.site_pv_mw)}) "
                f"does not match periods ({periods})"
            )
        for ap in self.as_products:
            if len(ap.price_forecast_per_mwh) != periods:
                raise ValueError(
                    f"AS product {ap.product_def.id}: price forecast has "
                    f"{len(ap.price_forecast_per_mwh)} entries, expected {periods}"
                )

    # -----------------------------------------------------------------
    # Network & request construction
    # -----------------------------------------------------------------

    # Stable resource-id convention used by both the network and the
    # request. Keeps downstream bookkeeping (export, tests) trivial.
    BESS_RESOURCE_ID = "bess"
    GRID_IMPORT_RESOURCE_ID = "grid_import"
    GRID_EXPORT_RESOURCE_ID = "grid_export"
    SITE_PV_RESOURCE_ID = "site_pv"

    def _as_capacity_for(self, ap: "AsProduct") -> float:
        """Physical offer cap for one AS product.

        Up-direction products are capped by discharge power; down-direction
        by charge power. The per-period physical headroom / footroom is
        enforced by the solver via :class:`ReserveProductDef.energy_coupling`.
        """
        if ap.product_def.direction == "Up":
            return float(self.site.bess_power_discharge_mw)
        return float(self.site.bess_power_charge_mw)

    def build_network(
        self,
        *,
        dispatch_mode: str = "optimal_foresight",
        initial_soc_override_mwh: float | None = None,
        period_index: int | None = None,
    ) -> Any:
        """Construct the single-bus surge.Network for this site.

        ``dispatch_mode``:
          * ``"optimal_foresight"`` — BESS uses cost-minimisation
            mode with zero cost; LP extracts max arbitrage against
            the LMP forecast.
          * ``"pwl_offers"`` — BESS uses ``offer_curve`` mode with
            the static discharge-offer and charge-bid curves from
            :attr:`self.pwl_strategy`. The charge-bid is negated to
            give the LP a credit per MW charged (the standard bid
            semantics — "max willing to pay").

        ``initial_soc_override_mwh`` is used by sequential mode to
        carry SOC forward from the previous period.
        """
        import surge  # type: ignore
        from surge import StorageParams

        net = surge.Network(base_mva=self.base_mva)
        net.add_bus(number=self.bus_number, bus_type="Slack", base_kv=self.base_kv)

        # BESS parameters — the "offer" side depends on dispatch mode.
        initial_soc = (
            initial_soc_override_mwh
            if initial_soc_override_mwh is not None
            else (
                self.site.bess_initial_soc_mwh
                if self.site.bess_initial_soc_mwh is not None
                else 0.5 * self.site.bess_energy_mwh
            )
        )
        soc_min = self.site.bess_soc_min_fraction * self.site.bess_energy_mwh
        soc_max = self.site.bess_soc_max_fraction * self.site.bess_energy_mwh
        dis_foldback_mwh = (
            self.site.bess_discharge_foldback_fraction * self.site.bess_energy_mwh
            if self.site.bess_discharge_foldback_fraction is not None
            else None
        )
        ch_foldback_mwh = (
            self.site.bess_charge_foldback_fraction * self.site.bess_energy_mwh
            if self.site.bess_charge_foldback_fraction is not None
            else None
        )

        if dispatch_mode == "pwl_offers":
            if self.pwl_strategy is None:
                raise ValueError(
                    "dispatch_mode='pwl_offers' requires a PwlBidStrategy "
                    "on the BatteryProblem"
                )
            # The native StorageParams.charge_bid takes a cumulative
            # $-cost curve — positive = cost to charge, negative = credit.
            # For "max willing to pay $X/MWh" semantics we flip the sign.
            disc_segments = self.pwl_strategy.discharge_offer_segments
            chrg_segments = self.pwl_strategy.charge_bid_segments
            if period_index is not None:
                disc_segments = _apply_per_period_prices(
                    disc_segments,
                    self.pwl_strategy.discharge_offer_price_per_period,
                    period_index,
                )
                chrg_segments = _apply_per_period_prices(
                    chrg_segments,
                    self.pwl_strategy.charge_bid_price_per_period,
                    period_index,
                )
            discharge_curve = _cumulative_price_curve(disc_segments)
            charge_curve = _cumulative_price_curve(chrg_segments, credit=True)
            bess_params = StorageParams(
                energy_capacity_mwh=self.site.bess_energy_mwh,
                charge_efficiency=self.site.bess_charge_efficiency,
                discharge_efficiency=self.site.bess_discharge_efficiency,
                soc_initial_mwh=initial_soc,
                soc_min_mwh=soc_min,
                soc_max_mwh=soc_max,
                degradation_cost_per_mwh=self.site.bess_degradation_cost_per_mwh,
                dispatch_mode="offer_curve",
                discharge_offer=discharge_curve,
                charge_bid=charge_curve,
                discharge_foldback_soc_mwh=dis_foldback_mwh,
                charge_foldback_soc_mwh=ch_foldback_mwh,
                daily_cycle_limit=self.site.bess_daily_cycle_limit,
            )
        else:
            bess_params = StorageParams(
                energy_capacity_mwh=self.site.bess_energy_mwh,
                charge_efficiency=self.site.bess_charge_efficiency,
                discharge_efficiency=self.site.bess_discharge_efficiency,
                soc_initial_mwh=initial_soc,
                soc_min_mwh=soc_min,
                soc_max_mwh=soc_max,
                degradation_cost_per_mwh=self.site.bess_degradation_cost_per_mwh,
                discharge_foldback_soc_mwh=dis_foldback_mwh,
                charge_foldback_soc_mwh=ch_foldback_mwh,
                daily_cycle_limit=self.site.bess_daily_cycle_limit,
            )
        net.add_storage(
            bus=self.bus_number,
            charge_mw_max=self.site.bess_power_charge_mw,
            discharge_mw_max=self.site.bess_power_discharge_mw,
            params=bess_params,
            id=self.BESS_RESOURCE_ID,
        )
        net.set_generator_cost(self.BESS_RESOURCE_ID, coeffs=[0.0, 0.0, 0.0])

        # Qualify the BESS for every AS product it bids into. The
        # :class:`ReserveProductDef.qualification` is the RULE (Committed /
        # Synchronized / OfflineQuickStart); the per-resource flag here is
        # simply whether the resource opts in to that product.
        if self.as_products:
            net.set_generator_qualifications(
                self.BESS_RESOURCE_ID,
                {ap.product_def.id: True for ap in self.as_products},
            )

        # Virtual grid-import generator. pmax = POI limit; per-period
        # cost curve set via the request.
        net.add_generator(
            bus=self.bus_number,
            p_mw=0.0,
            pmax_mw=self.site.poi_limit_mw,
            pmin_mw=0.0,
            vs_pu=1.0,
            qmax_mvar=0.0,
            qmin_mvar=0.0,
            machine_id="grid",
            id=self.GRID_IMPORT_RESOURCE_ID,
        )
        net.set_generator_cost(self.GRID_IMPORT_RESOURCE_ID, coeffs=[0.0, 0.0, 0.0])

        # Optional on-site PV
        if self.site.site_pv_mw is not None:
            pv_max = max(self.site.site_pv_mw) if self.site.site_pv_mw else 0.0
            net.add_generator(
                bus=self.bus_number,
                p_mw=0.0,
                pmax_mw=max(pv_max, 1e-6),
                pmin_mw=0.0,
                vs_pu=1.0,
                qmax_mvar=0.0,
                qmin_mvar=0.0,
                machine_id="pv",
                id=self.SITE_PV_RESOURCE_ID,
            )
            net.set_generator_cost(self.SITE_PV_RESOURCE_ID, coeffs=[0.0, 0.0, 0.0])

        # Optional site baseline load (via load_profile in request).
        # The Network-level add_load just needs a placeholder.
        if self.site.site_load_mw is not None:
            net.add_load(bus=self.bus_number, pd_mw=0.0, qd_mvar=0.0)

        return net

    def build_request(
        self,
        policy: Any,
        *,
        period_slice: slice | None = None,
    ) -> dict[str, Any]:
        """Build the canonical :class:`DispatchRequest` dict.

        ``period_slice`` (used by sequential solving) limits the
        request to a subrange of periods. When given, the returned
        request's timeline, forecasts, and AS prices are all sliced
        to that range.
        """
        if period_slice is None:
            period_slice = slice(0, self.periods)
        start, stop, _ = period_slice.indices(self.periods)
        n = stop - start
        if n <= 0:
            raise ValueError(f"period_slice {period_slice!r} selects zero periods")

        builder = (
            _request_builder()
            .timeline(
                periods=n,
                hours_by_period=self.period_durations_hours[start:stop],
            )
            .commitment_all_committed()
            .coupling("time_coupled" if n > 1 else "period_by_period")
        )
        self._apply_profiles(builder, period_slice)
        self._apply_market(builder, policy, period_slice)
        return builder.build()

    # -----------------------------------------------------------------
    # Payload helpers
    # -----------------------------------------------------------------

    def _apply_profiles(self, builder: Any, period_slice: slice) -> None:
        start, stop, _ = period_slice.indices(self.periods)
        if self.site.site_load_mw is not None:
            builder.load_profile(
                bus=self.bus_number,
                values=self.site.site_load_mw[start:stop],
            )
        if self.site.site_pv_mw is not None:
            pv_max = max(self.site.site_pv_mw) if self.site.site_pv_mw else 0.0
            caps_full = [
                float(v) / pv_max if pv_max > 1e-12 else 0.0
                for v in self.site.site_pv_mw
            ]
            builder.renewable_profile(
                resource=self.SITE_PV_RESOURCE_ID,
                capacity_factors=caps_full[start:stop],
            )

    def _apply_market(self, builder: Any, policy: Any, period_slice: slice) -> None:
        start, stop, _ = period_slice.indices(self.periods)
        n = stop - start

        # Virtual grid-import generator per-period offer curve = LMP.
        grid_import_offer = GeneratorOfferSchedule(
            resource_id=self.GRID_IMPORT_RESOURCE_ID,
            segments_by_period=[
                [(float(self.site.poi_limit_mw), float(self.lmp_forecast_per_mwh[t]))]
                for t in range(start, stop)
            ],
            no_load_cost_by_period=[0.0] * n,
            startup_cost_tiers=[],
        )
        builder.generator_offers([grid_import_offer])

        # Virtual grid-export dispatchable load with per-period curtail
        # cost = LMP — the LP earns LMP × served MW per hour for power
        # exported from the site.
        p_sched_pu = self.site.poi_limit_mw / self.base_mva
        grid_export = DispatchableLoadSpec(
            resource_id=self.GRID_EXPORT_RESOURCE_ID,
            bus=self.bus_number,
            p_sched_pu=p_sched_pu,
            p_max_pu=p_sched_pu,
            archetype="Curtailable",
            cost_model=linear_curtailment(0.0),
        )
        grid_export_schedule = DispatchableLoadOfferSchedule(
            resource_id=self.GRID_EXPORT_RESOURCE_ID,
            periods=[
                {
                    "p_sched_pu": p_sched_pu,
                    "p_max_pu": p_sched_pu,
                    "cost_model": linear_curtailment(self.lmp_forecast_per_mwh[t]),
                }
                for t in range(start, stop)
            ],
        )
        builder.dispatchable_loads([grid_export])
        builder.dispatchable_load_offers([grid_export_schedule])

        # AS co-optimization. The shortfall mechanism gives the LP a
        # reward for clearing reserves equal to ``shortfall − offer``
        # per cleared MW. We pick the two so the net per-MW reward is
        # exactly the per-period forecast price:
        #
        #   shortfall = max(price[t])         (scalar; ZonalRequirement
        #                                      is zone-wide, not per-period)
        #   offer[t]  = shortfall − price[t]  (per-period BESS bid)
        #
        # Per-period requirement is gated on price[t] > 0, so periods
        # where the operator places no value on AS create no demand —
        # the LP leaves the BESS free for energy arbitrage.
        #
        # In ``pwl_offers`` mode the BESS offers at the strategy's bid
        # price; the LP clears only when shortfall > bid.
        if self.as_products:
            builder.reserve_products(
                [_decouple_product(ap.product_def) for ap in self.as_products]
            )
            requirements: list[ZonalRequirement] = []
            shortfall_by_product: dict[str, float] = {}
            for ap in self.as_products:
                cap = self._as_capacity_for(ap)
                prices_window = ap.price_forecast_per_mwh[start:stop]
                shortfall = float(max(prices_window))
                shortfall_by_product[ap.product_def.id] = shortfall
                per_period = [
                    cap if float(prices_window[t]) > 0.0 else 0.0
                    for t in range(n)
                ]
                requirements.append(
                    ZonalRequirement(
                        zone_id=1,
                        product_id=ap.product_def.id,
                        requirement_mw=cap if shortfall > 0.0 else 0.0,
                        per_period_mw=per_period,
                        shortfall_cost_per_unit=shortfall,
                    )
                )
            builder.zonal_reserves(requirements)

            offer_costs = self._as_offer_costs_per_period(
                policy, period_slice, shortfall_by_product
            )
            # Per-period offered capacity. We hard-zero the BESS's offer
            # for any (product, period) that has no demand
            # (``price[t] == 0``) so the LP can't park headroom in a
            # product with no buyer. Without this guard, when a product
            # has zero price across the horizon the offer cost
            # collapses to zero (``shortfall − price = 0``) and the LP
            # objective becomes degenerate — the simplex can return any
            # award up to capacity, including non-zero awards that
            # silently steal discharge headroom from products that
            # *do* have positive price (e.g. spinning reserve).
            ap_prices_window = {
                ap.product_def.id: ap.price_forecast_per_mwh[start:stop]
                for ap in self.as_products
            }
            bess_reserve_offers = GeneratorReserveOfferSchedule(
                resource_id=self.BESS_RESOURCE_ID,
                offers_by_period=[
                    [
                        {
                            "product_id": ap.product_def.id,
                            "capacity_mw": (
                                self._as_capacity_for(ap)
                                if float(ap_prices_window[ap.product_def.id][t]) > 0.0
                                else 0.0
                            ),
                            "cost_per_mwh": float(offer_costs[ap.product_def.id][t]),
                        }
                        for ap in self.as_products
                    ]
                    for t in range(n)
                ],
            )
            builder.reserve_offers([bess_reserve_offers])

            # SOC-headroom guard for AS awards. When the operator opts
            # in via ``BatteryPolicy.enforce_reserve_soc_capacity``, we
            # pass per-product impact factors that the LP turns into
            # per-period rows of the form
            #
            #   Σ_p∈Up   award[p] · dt / η_dis  ≤  SOC[t] − soc_min
            #   Σ_p∈Down award[p] · dt · η_ch  ≤  soc_max − SOC[t]
            #
            # so every cleared MW of an award is backed by enough
            # energy to survive a 100 %-deployment over the period.
            if getattr(policy, "enforce_reserve_soc_capacity", False):
                eta_ch = float(self.site.bess_charge_efficiency)
                eta_dis = float(self.site.bess_discharge_efficiency)
                impacts: list[dict[str, Any]] = []
                for ap in self.as_products:
                    if ap.product_def.direction == "Up":
                        # Discharging delivers ``award_mw`` to the grid
                        # but draws ``award_mw / η_dis`` from SOC, so
                        # the impact factor is +1/η_dis (positive).
                        factor = 1.0 / max(eta_dis, 1e-9)
                    else:
                        # Charging absorbs ``award_mw`` from the grid
                        # and adds ``award_mw · η_ch`` to SOC, so the
                        # impact factor is −η_ch (negative — consumes
                        # SOC headroom rather than SOC).
                        factor = -eta_ch
                    impacts.append(
                        {
                            "resource_id": self.BESS_RESOURCE_ID,
                            "product_id": ap.product_def.id,
                            "values_mwh_per_mw": [factor] * n,
                        }
                    )
                if impacts:
                    builder.storage_reserve_soc_impacts(impacts)

    def _as_offer_costs_per_period(
        self,
        policy: Any,
        period_slice: slice,
        shortfall_by_product: dict[str, float],
    ) -> dict[str, list[float]]:
        """Per-period BESS reserve-offer cost in the LP's objective.

        ``optimal_foresight``: ``shortfall − price[t]`` so the net per-MW
        reward when the LP clears is exactly the period's forecast price.
        ``pwl_offers``: flat per-product bid from the strategy.
        """
        start, stop, _ = period_slice.indices(self.periods)
        n = stop - start
        mode = getattr(policy, "dispatch_mode", "optimal_foresight")
        if mode == "pwl_offers" and self.pwl_strategy is not None:
            return {
                ap.product_def.id: [
                    float(
                        self.pwl_strategy.as_offer_prices_per_mwh.get(
                            ap.product_def.id, 0.0
                        )
                    )
                ]
                * n
                for ap in self.as_products
            }
        return {
            ap.product_def.id: [
                shortfall_by_product[ap.product_def.id]
                - float(ap.price_forecast_per_mwh[start + t])
                for t in range(n)
            ]
            for ap in self.as_products
        }


# ---------------------------------------------------------------------------
# PWL curve helpers (local to this module; see surge.market.offers for the
# generic generator offer helpers).
# ---------------------------------------------------------------------------


def _apply_per_period_prices(
    baseline_segments: list[tuple[float, float]],
    per_period_prices: list[list[float | None] | None] | None,
    period_index: int,
) -> list[tuple[float, float]]:
    """Substitute per-period segment prices into the baseline.

    Keeps the MW breakpoints of ``baseline_segments``. Per-segment values
    of ``None`` fall back to the baseline price for that segment, so the
    override matrix can be sparse.
    """
    if not per_period_prices or period_index >= len(per_period_prices):
        return baseline_segments
    prices_this_period = per_period_prices[period_index]
    if prices_this_period is None or len(prices_this_period) != len(baseline_segments):
        return baseline_segments
    out: list[tuple[float, float]] = []
    for i, (mw, baseline_price) in enumerate(baseline_segments):
        v = prices_this_period[i]
        price = float(baseline_price) if v is None else float(v)
        out.append((float(mw), price))
    return out


def _cumulative_price_curve(
    segments: list[tuple[float, float]],
    *,
    credit: bool = False,
) -> list[tuple[float, float]]:
    """Convert ``[(cum_mw_1, price_1), ...]`` to Surge's
    ``[(0, 0), (cum_mw_1, cum_cost_1), ...]`` cumulative-cost format.

    ``credit=True`` flips every cumulative cost's sign — used to
    turn a "max willing to pay" charge bid into the LP's credit
    per MW charged.
    """
    if not segments:
        raise ValueError("offer/bid segments must be non-empty")
    out: list[tuple[float, float]] = [(0.0, 0.0)]
    prev_mw = 0.0
    prev_cum_cost = 0.0
    for cum_mw, price in segments:
        cum_mw = float(cum_mw)
        price = float(price)
        delta_mw = cum_mw - prev_mw
        if delta_mw <= 1e-9:
            raise ValueError(
                f"segment MW breakpoints must be strictly increasing; got {cum_mw} after {prev_mw}"
            )
        cum_cost = prev_cum_cost + price * delta_mw
        out.append((cum_mw, -cum_cost if credit else cum_cost))
        prev_mw = cum_mw
        prev_cum_cost = cum_cost
    return out


__all__ = ["AsProduct", "BatteryProblem", "PwlBidStrategy", "SiteSpec"]
