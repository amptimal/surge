# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Datacenter operator problem + in-memory surge.Network builder.

A datacenter operator runs a 1-bus microgrid behind a single point of
interconnection (POI). The asset stack is the canonical hyperscale-DC
mix:

* IT load split into a *must-serve* tier (latency-sensitive inference,
  storage, network) and one or more *curtailable* tiers, each with
  its own value-of-lost-load — high-VOLL inference, mid-VOLL training,
  low-VOLL batch / research.
* Solar PV and onshore wind, modelled as zero-cost generators with
  capacity-factor profiles.
* A battery (BESS) with full SOC dynamics, efficiency, foldback, and
  optional cycle-life cap.
* Thermal generation: a fuel cell, gas combustion turbine, and diesel
  backup, each with PWL heat-rate curves, fuel prices, VOM, no-load
  cost, startup-cost tiers, min up/down, and ramp constraints. All
  three thermal classes commit endogenously in the SCUC.
* Optional nuclear baseload — must-run with low marginal cost and
  (optional) planned-outage availability.

Plus two virtual resources at the POI bus that turn the LP's
cost-minimisation objective into the operator's surplus:

* A ``grid_import`` generator with per-period offer curve = LMP
  forecast (so importing power costs the LP exactly the LMP).
* A ``grid_export`` curtailable dispatchable load with per-period
  curtailment cost = LMP forecast (so exporting power credits the LP
  at the LMP).

Reserves co-optimise with energy via the canonical RTC+B product set
— Reg-Up, Reg-Down, RRS (synchronous responsive), ECRS, Non-Spin —
with per-resource physical qualifications. Coincident-peak
transmission charges (e.g. ERCOT 4-CP) flow through the framework's
:class:`PeakDemandCharge` primitive applied to grid imports.

The whole formulation flows through :class:`surge.market.MarketWorkflow`
SCUC, so commitment of CT / fuel cell / diesel is a real MIP decision
— not LP relaxation, not "all committed".
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
    """Strip ``balance_products`` and ``shared_limit_products`` from an
    ISO-template reserve product.

    The standard product templates assume ISO clearing semantics where
    a higher-quality product (reg-up) can substitute for a lower one
    (RRS, ECRS, non-spin) on the system requirement. In the price-taker
    operator's model each cleared MW earns its own forecast price, so
    the substitution ladder credits a single MW twice and biases the
    LP toward the highest-priced product. Decouple to keep per-product
    revenue accounting clean.
    """
    return dataclasses.replace(p, balance_products=(), shared_limit_products=())


# ---------------------------------------------------------------------------
# Asset specs
# ---------------------------------------------------------------------------


@dataclass
class CurtailableLoadTier:
    """One tier of curtailable IT load.

    A 1 GW datacenter might split as 700 MW must-serve (handled
    separately via :attr:`ItLoadSpec.must_serve_mw`), 200 MW Tier-1
    inference at $200/MWh VOLL, and 100 MW Tier-2 batch training at
    $40/MWh — the LP curtails low-tier work first when grid import
    plus on-site marginal cost exceeds the tier's value.
    """

    #: Stable identifier — used in result extraction and the dashboard.
    tier_id: str
    #: Maximum MW served at full utilisation (per period).
    capacity_mw: float
    #: Value-of-lost-load ($/MWh). The LP curtails the tier when its
    #: marginal cost of supply exceeds this. Inference / latency-bound
    #: serving is typically $100–500/MWh; training $20–80; research
    #: $0–10.
    voll_per_mwh: float
    #: Optional per-period capacity profile (MW). When ``None`` the
    #: tier is at full ``capacity_mw`` every period.
    capacity_per_period_mw: list[float] | None = None


@dataclass
class ItLoadSpec:
    """The datacenter's IT load — must-serve baseline plus tiers."""

    #: Must-serve MW per period. Goes onto the bus as a fixed load —
    #: the LP cannot curtail it. Use the `tiers` for any portion of the
    #: load that has a finite VOLL.
    must_serve_mw: list[float]
    #: One or more curtailable tiers, sorted from highest VOLL to
    #: lowest. The LP curtails low-VOLL tiers first when on-site
    #: supply is short or grid import is too expensive.
    tiers: list[CurtailableLoadTier] = field(default_factory=list)


@dataclass
class SolarSpec:
    """On-site solar PV.

    Modelled as a generator with a capacity-factor profile and an
    offer cost equal to ``-rec_value_per_mwh`` (the REC credit acts
    as a negative marginal cost, so the LP dispatches solar whenever
    its REC credit + LMP > 0; curtailment costs exactly REC per MWh
    curtailed because the operator forgoes the credit).
    """

    nameplate_mw: float
    #: Per-period capacity factor in [0, 1].
    capacity_factors: list[float]
    #: Renewable Energy Credit value ($/MWh). Used as the negative
    #: offer cost in the SCUC, so each MWh generated credits the LP
    #: by this amount and each MWh curtailed effectively forfeits it.
    rec_value_per_mwh: float = 0.0
    #: Optional reserve product qualifications. Solar can technically
    #: provide curtailment-down regulation; default is no AS bidding.
    qualified_as_products: tuple[str, ...] = ()


@dataclass
class WindSpec:
    """On-site wind. Same shape as :class:`SolarSpec`."""

    nameplate_mw: float
    capacity_factors: list[float]
    #: REC value ($/MWh) — see :attr:`SolarSpec.rec_value_per_mwh`.
    rec_value_per_mwh: float = 0.0
    qualified_as_products: tuple[str, ...] = ()


@dataclass
class BessSpec:
    """Battery energy storage system."""

    power_charge_mw: float
    power_discharge_mw: float
    energy_mwh: float
    charge_efficiency: float = 0.92
    discharge_efficiency: float = 0.96
    soc_min_fraction: float = 0.10
    soc_max_fraction: float = 0.95
    initial_soc_mwh: float | None = None
    degradation_cost_per_mwh: float = 2.0
    #: Cap on full equivalent cycles per 24 hours (None = uncapped).
    daily_cycle_limit: float | None = None
    #: Reserve products the BESS bids into; defaults to all five
    #: RTC+B products since storage qualifies for every category.
    qualified_as_products: tuple[str, ...] = (
        "reg_up",
        "reg_down",
        "syn",
        "ecrs",
        "nsyn",
    )


@dataclass
class ThermalSpec:
    """Common thermal-resource shape: fuel cell, gas CT, diesel.

    Marginal cost is computed as ``heat_rate × fuel_price + vom``.
    The heat rate can vary with output via ``heat_rate_segments``
    (cumulative MW → btu/MWh) — when omitted the unit uses a flat
    heat rate at all output levels.

    Commitment is always optimised by the SCUC (when the ambient
    policy is ``commitment_mode="optimize"``); ``min_up_h`` and
    ``min_down_h`` enforce realistic cycling. Diesel backup typically
    has ``min_up_h = 0.0`` and a high marginal cost so it only runs
    when called.
    """

    #: Stable resource id used by the dashboard + reports.
    resource_id: str
    nameplate_mw: float
    pmin_mw: float
    fuel_price_per_mmbtu: float
    heat_rate_btu_per_kwh: float
    #: Optional per-period fuel-price override ($/MMBtu, length =
    #: problem.periods). When set, takes precedence over the scalar
    #: ``fuel_price_per_mmbtu`` so different periods can carry
    #: different gas prices (e.g. shaped daily-cycle gas forecast for
    #: a CT + fuel cell sharing one natural-gas pipeline).
    fuel_price_per_period_per_mmbtu: list[float] | None = None
    #: Variable O&M ($/MWh).
    vom_per_mwh: float = 0.0
    #: No-load cost ($/hr) — paid every hour the unit is committed.
    no_load_cost_per_hr: float = 0.0
    #: Startup-cost tiers — list of ``{"max_offline_hours", "cost"}``
    #: dicts. Empty disables startup costs.
    startup_cost_tiers: list[dict[str, float]] = field(default_factory=list)
    min_up_h: float = 0.0
    min_down_h: float = 0.0
    ramp_up_mw_per_min: float | None = None
    ramp_down_mw_per_min: float | None = None
    #: CO₂ emissions (tonnes/MWh) — tracked for reporting and (later)
    #: carbon pricing studies.
    co2_tonnes_per_mwh: float = 0.0
    #: Reserve products this resource is physically qualified to
    #: provide. Synchronous units (CT, fuel cell) typically qualify
    #: for everything except non-spin; diesel quick-start qualifies
    #: only for non-spin. Override via the spec.
    qualified_as_products: tuple[str, ...] = ()


@dataclass
class NuclearSpec:
    """Optional baseload nuclear resource.

    Modelled as must-run at ``availability_factor × nameplate_mw``
    every period (set ``availability_factor`` per period to model
    planned outages). Marginal cost is low and constant.
    """

    resource_id: str
    nameplate_mw: float
    marginal_cost_per_mwh: float = 8.0
    #: Per-period availability fraction in [0, 1]. ``None`` → fully
    #: available every period.
    availability_per_period: list[float] | None = None
    qualified_as_products: tuple[str, ...] = ()


@dataclass
class FourCpSpec:
    """Coincident-peak transmission demand charge.

    Maps to :class:`surge_dispatch::PeakDemandCharge` — adds an
    auxiliary ``peak_grid_import_mw`` variable bounded below by grid
    import on every flagged period, with a linear cost
    ``charge_per_mw × peak``.

    For ERCOT 4-CP modelling: flag the periods within each user-marked
    "4-CP day" and pass ``charge_per_mw = annual_$_per_mw / 4`` (one
    of the four annual intervals lives in the simulated horizon).
    """

    name: str = "tx_4cp"
    #: Period indices to include in the peak set.
    period_indices: list[int] = field(default_factory=list)
    #: Linear cost coefficient in $/MW applied to the peak variable.
    #: Set to ``annual_$_per_MW / 4`` for one expected 4-CP interval.
    charge_per_mw: float = 0.0


@dataclass
class AsProduct:
    """One ancillary-services product the datacenter participates in.

    The forecast price drives both the LP's reward-for-clearing
    (via the same shortfall-trick the battery market uses) and the
    revenue accounting in the export report.
    """

    product_def: ReserveProductDef
    price_forecast_per_mwh: list[float]


@dataclass
class SiteSpec:
    """Full physical description of the datacenter microgrid."""

    poi_limit_mw: float
    it_load: ItLoadSpec
    bess: BessSpec
    solar: SolarSpec | None = None
    wind: WindSpec | None = None
    fuel_cell: ThermalSpec | None = None
    gas_ct: ThermalSpec | None = None
    diesel: ThermalSpec | None = None
    nuclear: NuclearSpec | None = None
    four_cp: FourCpSpec | None = None


# ---------------------------------------------------------------------------
# Stable resource ID convention
# ---------------------------------------------------------------------------


GRID_IMPORT_RESOURCE_ID = "grid_import"
GRID_EXPORT_RESOURCE_ID = "grid_export"
SOLAR_RESOURCE_ID = "site_solar"
WIND_RESOURCE_ID = "site_wind"
BESS_RESOURCE_ID = "site_bess"
NUCLEAR_DEFAULT_RESOURCE_ID = "site_nuclear"
CURTAILABLE_LOAD_PREFIX = "it_load"


# ---------------------------------------------------------------------------
# Problem dataclass
# ---------------------------------------------------------------------------


@dataclass
class DataCenterProblem:
    """Datacenter operator problem.

    Forecasts:

    * ``lmp_forecast_per_mwh`` — exogenous LMP at the POI ($/MWh).
    * ``as_products`` — zero or more ancillary-service products with
      per-period price forecasts. Each product is registered in the
      market and BESS / qualifying thermals offer into it.

    Solver requirements:

    * Periods, period durations, LMP, load profile, capacity-factor
      profiles, AS price forecasts must all have length =
      ``len(period_durations_hours)``.
    """

    period_durations_hours: list[float]
    lmp_forecast_per_mwh: list[float]
    site: SiteSpec
    as_products: list[AsProduct] = field(default_factory=list)

    bus_number: int = 1
    base_kv: float = 138.0
    base_mva: float = 100.0

    @property
    def periods(self) -> int:
        return len(self.period_durations_hours)

    def __post_init__(self) -> None:
        n = self.periods
        if n <= 0:
            raise ValueError("period_durations_hours must be non-empty")
        if len(self.lmp_forecast_per_mwh) != n:
            raise ValueError(
                f"lmp_forecast_per_mwh length ({len(self.lmp_forecast_per_mwh)}) "
                f"does not match periods ({n})"
            )
        if len(self.site.it_load.must_serve_mw) != n:
            raise ValueError(
                f"it_load.must_serve_mw length "
                f"({len(self.site.it_load.must_serve_mw)}) does not match periods ({n})"
            )
        for tier in self.site.it_load.tiers:
            if tier.capacity_per_period_mw is not None and len(tier.capacity_per_period_mw) != n:
                raise ValueError(
                    f"tier {tier.tier_id!r}: capacity_per_period_mw length "
                    f"({len(tier.capacity_per_period_mw)}) does not match periods ({n})"
                )
        for renewable, name in (
            (self.site.solar, "solar"),
            (self.site.wind, "wind"),
        ):
            if renewable is not None and len(renewable.capacity_factors) != n:
                raise ValueError(
                    f"{name}.capacity_factors length "
                    f"({len(renewable.capacity_factors)}) does not match periods ({n})"
                )
        if self.site.nuclear is not None:
            avail = self.site.nuclear.availability_per_period
            if avail is not None and len(avail) != n:
                raise ValueError(
                    f"nuclear.availability_per_period length "
                    f"({len(avail)}) does not match periods ({n})"
                )
        for ap in self.as_products:
            if len(ap.price_forecast_per_mwh) != n:
                raise ValueError(
                    f"AS product {ap.product_def.id!r}: price forecast length "
                    f"({len(ap.price_forecast_per_mwh)}) does not match periods ({n})"
                )
        if self.site.four_cp is not None:
            for p in self.site.four_cp.period_indices:
                if not (0 <= p < n):
                    raise ValueError(
                        f"four_cp period_indices contains {p} outside [0, {n})"
                    )

    # ------------------------------------------------------------------
    # Network builder
    # ------------------------------------------------------------------

    def thermal_specs(self) -> list[tuple[str, "ThermalSpec | None"]]:
        """All thermal slots in canonical order, including absent ones.

        Used by the solve / export path to iterate uniformly over the
        thermal stack.
        """
        return [
            ("fuel_cell", self.site.fuel_cell),
            ("gas_ct", self.site.gas_ct),
            ("diesel", self.site.diesel),
        ]

    def curtailable_load_resource_id(self, tier_id: str) -> str:
        return f"{CURTAILABLE_LOAD_PREFIX}::{tier_id}"

    def build_network(
        self,
        *,
        initial_soc_override_mwh: float | None = None,
    ) -> Any:
        import surge  # type: ignore
        from surge import StorageParams

        net = surge.Network(base_mva=self.base_mva)
        net.add_bus(number=self.bus_number, bus_type="Slack", base_kv=self.base_kv)

        # Must-serve IT load — Network-level placeholder, the per-period
        # MW comes from the request's load_profile.
        net.add_load(bus=self.bus_number, pd_mw=0.0, qd_mvar=0.0)

        # ----- BESS ---------------------------------------------------
        bess = self.site.bess
        initial_soc = (
            initial_soc_override_mwh
            if initial_soc_override_mwh is not None
            else (
                bess.initial_soc_mwh
                if bess.initial_soc_mwh is not None
                else 0.5 * bess.energy_mwh
            )
        )
        soc_min = bess.soc_min_fraction * bess.energy_mwh
        soc_max = bess.soc_max_fraction * bess.energy_mwh
        bess_params = StorageParams(
            energy_capacity_mwh=bess.energy_mwh,
            charge_efficiency=bess.charge_efficiency,
            discharge_efficiency=bess.discharge_efficiency,
            soc_initial_mwh=initial_soc,
            soc_min_mwh=soc_min,
            soc_max_mwh=soc_max,
            degradation_cost_per_mwh=bess.degradation_cost_per_mwh,
            daily_cycle_limit=bess.daily_cycle_limit,
        )
        net.add_storage(
            bus=self.bus_number,
            charge_mw_max=bess.power_charge_mw,
            discharge_mw_max=bess.power_discharge_mw,
            params=bess_params,
            id=BESS_RESOURCE_ID,
        )
        net.set_generator_cost(BESS_RESOURCE_ID, coeffs=[0.0, 0.0, 0.0])
        if bess.qualified_as_products:
            net.set_generator_qualifications(
                BESS_RESOURCE_ID,
                {pid: True for pid in bess.qualified_as_products},
            )

        # ----- Solar / Wind ------------------------------------------
        if self.site.solar is not None:
            self._add_renewable(
                net,
                resource_id=SOLAR_RESOURCE_ID,
                nameplate_mw=self.site.solar.nameplate_mw,
                qualified=self.site.solar.qualified_as_products,
                machine_id="pv",
            )
        if self.site.wind is not None:
            self._add_renewable(
                net,
                resource_id=WIND_RESOURCE_ID,
                nameplate_mw=self.site.wind.nameplate_mw,
                qualified=self.site.wind.qualified_as_products,
                machine_id="wind",
            )

        # ----- Thermals ----------------------------------------------
        for _slot, thermal in self.thermal_specs():
            if thermal is None:
                continue
            net.add_generator(
                bus=self.bus_number,
                p_mw=0.0,
                pmax_mw=thermal.nameplate_mw,
                pmin_mw=thermal.pmin_mw,
                vs_pu=1.0,
                qmax_mvar=0.0,
                qmin_mvar=0.0,
                machine_id=thermal.resource_id,
                id=thermal.resource_id,
            )
            net.set_generator_cost(thermal.resource_id, coeffs=[0.0, 0.0, 0.0])
            self._apply_thermal_commitment_attrs(net, thermal)
            if thermal.qualified_as_products:
                net.set_generator_qualifications(
                    thermal.resource_id,
                    {pid: True for pid in thermal.qualified_as_products},
                )

        # ----- Nuclear (must-run baseload) ----------------------------
        if self.site.nuclear is not None:
            nuc = self.site.nuclear
            net.add_generator(
                bus=self.bus_number,
                p_mw=nuc.nameplate_mw,
                pmax_mw=nuc.nameplate_mw,
                pmin_mw=0.0,
                vs_pu=1.0,
                qmax_mvar=0.0,
                qmin_mvar=0.0,
                machine_id=nuc.resource_id,
                id=nuc.resource_id,
            )
            # Polynomial cost: linear $/MWh × MW.
            net.set_generator_cost(
                nuc.resource_id, coeffs=[float(nuc.marginal_cost_per_mwh), 0.0]
            )
            if nuc.qualified_as_products:
                net.set_generator_qualifications(
                    nuc.resource_id,
                    {pid: True for pid in nuc.qualified_as_products},
                )

        # ----- Virtual grid-import generator -------------------------
        net.add_generator(
            bus=self.bus_number,
            p_mw=0.0,
            pmax_mw=self.site.poi_limit_mw,
            pmin_mw=0.0,
            vs_pu=1.0,
            qmax_mvar=0.0,
            qmin_mvar=0.0,
            machine_id="grid",
            id=GRID_IMPORT_RESOURCE_ID,
        )
        net.set_generator_cost(GRID_IMPORT_RESOURCE_ID, coeffs=[0.0, 0.0, 0.0])

        # ----- Curtailable IT load tiers -----------------------------
        # A placeholder add_dispatchable_load with zero schedule; the
        # request's dispatchable_load_offer_schedules carry the real
        # per-period spec.
        for tier in self.site.it_load.tiers:
            net.add_dispatchable_load(
                bus=self.bus_number,
                p_sched_mw=0.0,
                cost_per_mwh=tier.voll_per_mwh,
                archetype="Curtailable",
            )

        # ----- Virtual grid-export DL placeholder --------------------
        net.add_dispatchable_load(
            bus=self.bus_number,
            p_sched_mw=0.0,
            cost_per_mwh=0.0,
            archetype="Curtailable",
        )

        return net

    @staticmethod
    def _add_renewable(
        net: Any,
        *,
        resource_id: str,
        nameplate_mw: float,
        qualified: tuple[str, ...],
        machine_id: str,
    ) -> None:
        net.add_generator(
            bus=1,
            p_mw=0.0,
            pmax_mw=max(nameplate_mw, 1e-6),
            pmin_mw=0.0,
            vs_pu=1.0,
            qmax_mvar=0.0,
            qmin_mvar=0.0,
            machine_id=machine_id,
            id=resource_id,
        )
        net.set_generator_cost(resource_id, coeffs=[0.0, 0.0, 0.0])
        if qualified:
            net.set_generator_qualifications(
                resource_id, {pid: True for pid in qualified}
            )

    @staticmethod
    def _apply_thermal_commitment_attrs(net: Any, thermal: ThermalSpec) -> None:
        """Set min-up / min-down / ramp / startup attributes on the generator.

        Mutates the editable :class:`Generator` in-place via
        ``net.update_generator_object``.
        """
        gen = net.generator(thermal.resource_id)
        if thermal.min_up_h:
            gen.min_up_time_hr = float(thermal.min_up_h)
        if thermal.min_down_h:
            gen.min_down_time_hr = float(thermal.min_down_h)
        if thermal.startup_cost_tiers:
            gen.startup_cost_tiers = [
                (
                    float(t.get("max_offline_hours", 0.0)),
                    float(t.get("cost", 0.0)),
                    float(t.get("sync_time_min", 0.0)),
                )
                for t in thermal.startup_cost_tiers
            ]
        if thermal.ramp_up_mw_per_min is not None:
            gen.ramp_up_curve = [(float(thermal.nameplate_mw), float(thermal.ramp_up_mw_per_min))]
        if thermal.ramp_down_mw_per_min is not None:
            gen.ramp_down_curve = [
                (float(thermal.nameplate_mw), float(thermal.ramp_down_mw_per_min))
            ]
        net.update_generator_object(gen)

    # ------------------------------------------------------------------
    # Request builder
    # ------------------------------------------------------------------

    def build_request(
        self,
        policy: Any,
        *,
        period_slice: slice | None = None,
    ) -> dict[str, Any]:
        if period_slice is None:
            period_slice = slice(0, self.periods)
        start, stop, _ = period_slice.indices(self.periods)
        n = stop - start
        if n <= 0:
            raise ValueError(f"period_slice {period_slice!r} selects zero periods")

        builder = (
            _request_builder()
            .timeline(periods=n, hours_by_period=self.period_durations_hours[start:stop])
            .coupling("time_coupled" if n > 1 else "period_by_period")
        )

        if policy.commitment_mode == "optimize":
            builder.commitment_optimize(
                mip_rel_gap=policy.mip_rel_gap,
                time_limit_secs=policy.mip_time_limit_secs,
            )
        else:
            # commitment_fixed: caller supplies a schedule via extension.
            # The market wraps this in a separate solve flow.
            builder.commitment_all_committed()

        self._apply_profiles(builder, period_slice)
        self._apply_market(builder, policy, period_slice)
        self._apply_peak_demand_charges(builder, period_slice)
        return builder.build()

    # ------------------------------------------------------------------
    # Payload helpers
    # ------------------------------------------------------------------

    def _apply_profiles(self, builder: Any, period_slice: slice) -> None:
        start, stop, _ = period_slice.indices(self.periods)
        # Must-serve IT load profile.
        builder.load_profile(
            bus=self.bus_number,
            values=list(self.site.it_load.must_serve_mw[start:stop]),
        )
        # Renewable capacity-factor profiles. The Network has the
        # nameplate; the request's renewable_profile gates output at
        # cf × nameplate.
        if self.site.solar is not None:
            builder.renewable_profile(
                resource=SOLAR_RESOURCE_ID,
                capacity_factors=list(self.site.solar.capacity_factors[start:stop]),
            )
        if self.site.wind is not None:
            builder.renewable_profile(
                resource=WIND_RESOURCE_ID,
                capacity_factors=list(self.site.wind.capacity_factors[start:stop]),
            )
        # Nuclear is must-run baseload pinned to availability × nameplate.
        # Set p_min == p_max so the LP has no dispatch freedom.
        if self.site.nuclear is not None:
            avail = (
                self.site.nuclear.availability_per_period[start:stop]
                if self.site.nuclear.availability_per_period is not None
                else [1.0] * (stop - start)
            )
            pinned_mw = [
                float(self.site.nuclear.nameplate_mw) * float(a) for a in avail
            ]
            builder.generator_dispatch_bounds(
                resource=self.site.nuclear.resource_id,
                p_min_mw=pinned_mw,
                p_max_mw=pinned_mw,
            )

    def _apply_market(self, builder: Any, policy: Any, period_slice: slice) -> None:
        start, stop, _ = period_slice.indices(self.periods)
        n = stop - start

        gen_offers: list[GeneratorOfferSchedule] = []

        # Virtual grid-import: per-period offer = LMP forecast.
        gen_offers.append(
            GeneratorOfferSchedule(
                resource_id=GRID_IMPORT_RESOURCE_ID,
                segments_by_period=[
                    [(float(self.site.poi_limit_mw), float(self.lmp_forecast_per_mwh[t]))]
                    for t in range(start, stop)
                ],
                no_load_cost_by_period=[0.0] * n,
                startup_cost_tiers=[],
            )
        )

        # Thermal offer schedules — marginal cost = HR × fuel + VOM.
        # Each thermal can carry a per-period fuel-price array for
        # gas-fed assets sharing one pipeline (e.g. CT + fuel cell off
        # natural gas).
        for _slot, thermal in self.thermal_specs():
            if thermal is None:
                continue
            gen_offers.append(self._thermal_offer_schedule(thermal, period_slice))

        # Renewable offer schedules — REC value as negative marginal
        # cost. Each MWh dispatched credits the LP by the REC amount;
        # each MWh curtailed (cap_factor × nameplate − dispatched)
        # forgoes that credit. With REC = 0 we omit the schedule and
        # let the network's [0,0,0] polynomial cost stand.
        if self.site.solar and self.site.solar.rec_value_per_mwh:
            gen_offers.append(self._renewable_offer_schedule(
                SOLAR_RESOURCE_ID,
                self.site.solar.nameplate_mw,
                self.site.solar.rec_value_per_mwh,
                n,
            ))
        if self.site.wind and self.site.wind.rec_value_per_mwh:
            gen_offers.append(self._renewable_offer_schedule(
                WIND_RESOURCE_ID,
                self.site.wind.nameplate_mw,
                self.site.wind.rec_value_per_mwh,
                n,
            ))

        builder.generator_offers(gen_offers)

        # Nuclear is always committed — paired with the dispatch_bounds
        # pin (p_min == p_max) this removes both commitment and dispatch
        # freedom from the LP for the resource.
        if self.site.nuclear is not None:
            builder.must_run_units([self.site.nuclear.resource_id])

        # Curtailable IT load tiers via DispatchableLoadSpec /
        # DispatchableLoadOfferSchedule. Each tier is one resource.
        dl_specs: list[DispatchableLoadSpec] = []
        dl_schedules: list[DispatchableLoadOfferSchedule] = []
        for tier in self.site.it_load.tiers:
            cap_per_period = (
                tier.capacity_per_period_mw
                if tier.capacity_per_period_mw is not None
                else [tier.capacity_mw] * self.periods
            )[start:stop]
            p_max_pu_default = tier.capacity_mw / self.base_mva
            rid = self.curtailable_load_resource_id(tier.tier_id)
            dl_specs.append(
                DispatchableLoadSpec(
                    resource_id=rid,
                    bus=self.bus_number,
                    p_sched_pu=p_max_pu_default,
                    p_max_pu=p_max_pu_default,
                    archetype="Curtailable",
                    cost_model=linear_curtailment(tier.voll_per_mwh),
                )
            )
            dl_schedules.append(
                DispatchableLoadOfferSchedule(
                    resource_id=rid,
                    periods=[
                        {
                            "p_sched_pu": float(cap_per_period[t]) / self.base_mva,
                            "p_max_pu": float(cap_per_period[t]) / self.base_mva,
                            "cost_model": linear_curtailment(tier.voll_per_mwh),
                        }
                        for t in range(n)
                    ],
                )
            )

        # Virtual grid-export DL — per-period curtailment cost = LMP.
        export_p = self.site.poi_limit_mw / self.base_mva
        dl_specs.append(
            DispatchableLoadSpec(
                resource_id=GRID_EXPORT_RESOURCE_ID,
                bus=self.bus_number,
                p_sched_pu=export_p,
                p_max_pu=export_p,
                archetype="Curtailable",
                cost_model=linear_curtailment(0.0),
            )
        )
        dl_schedules.append(
            DispatchableLoadOfferSchedule(
                resource_id=GRID_EXPORT_RESOURCE_ID,
                periods=[
                    {
                        "p_sched_pu": export_p,
                        "p_max_pu": export_p,
                        "cost_model": linear_curtailment(
                            float(self.lmp_forecast_per_mwh[t])
                        ),
                    }
                    for t in range(start, stop)
                ],
            )
        )
        builder.dispatchable_loads(dl_specs)
        builder.dispatchable_load_offers(dl_schedules)

        # AS co-optimisation. Same shortfall trick the battery market
        # uses: zonal requirement at capacity with shortfall_cost = max
        # forecast price; per-period offer = (shortfall − price[t]) so
        # net per-MW reward equals price[t]. A zero-price period
        # creates no demand on its own.
        if self.as_products:
            builder.reserve_products(
                [_decouple_product(ap.product_def) for ap in self.as_products]
            )
            zonal_reqs: list[ZonalRequirement] = []
            shortfall_by_product: dict[str, float] = {}
            for ap in self.as_products:
                prices_window = ap.price_forecast_per_mwh[start:stop]
                shortfall = float(max(prices_window))
                shortfall_by_product[ap.product_def.id] = shortfall
                cap = self._as_capacity_for(ap.product_def.id)
                per_period = [
                    cap if float(prices_window[t]) > 0.0 else 0.0
                    for t in range(n)
                ]
                zonal_reqs.append(
                    ZonalRequirement(
                        zone_id=1,
                        product_id=ap.product_def.id,
                        requirement_mw=cap if shortfall > 0.0 else 0.0,
                        per_period_mw=per_period,
                        shortfall_cost_per_unit=shortfall,
                    )
                )
            builder.zonal_reserves(zonal_reqs)
            builder.reserve_offers(
                self._reserve_offer_schedules(period_slice, shortfall_by_product)
            )

    @staticmethod
    def _renewable_offer_schedule(
        resource_id: str, nameplate_mw: float, rec_value: float, n: int,
    ) -> GeneratorOfferSchedule:
        """Constant-segment offer at -REC for each period.

        The negative marginal cost makes the LP credit the operator
        for every MWh dispatched; the equivalent of "curtailment
        offer = -REC" (curtailing forfeits the credit) falls out of
        the same offer without a separate row family.
        """
        offer = -float(rec_value)
        return GeneratorOfferSchedule(
            resource_id=resource_id,
            segments_by_period=[
                [(float(nameplate_mw), offer)] for _ in range(n)
            ],
            no_load_cost_by_period=[0.0] * n,
            startup_cost_tiers=[],
        )

    def _thermal_offer_schedule(
        self, thermal: ThermalSpec, period_slice: slice
    ) -> GeneratorOfferSchedule:
        """Build the offer schedule for one thermal resource.

        Marginal cost per period = heat_rate (MMBtu/MWh) × fuel_price[t]
        + VOM. The per-period fuel price comes from
        ``fuel_price_per_period_per_mmbtu`` if set; otherwise the scalar
        ``fuel_price_per_mmbtu`` is broadcast.
        """
        start, stop, _ = period_slice.indices(self.periods)
        n = stop - start
        hr_mmbtu_per_mwh = thermal.heat_rate_btu_per_kwh / 1000.0
        if thermal.fuel_price_per_period_per_mmbtu is not None:
            if len(thermal.fuel_price_per_period_per_mmbtu) != self.periods:
                raise ValueError(
                    f"thermal {thermal.resource_id!r}: "
                    f"fuel_price_per_period_per_mmbtu length "
                    f"({len(thermal.fuel_price_per_period_per_mmbtu)}) does not "
                    f"match problem periods ({self.periods})"
                )
            fuel_prices = thermal.fuel_price_per_period_per_mmbtu[start:stop]
        else:
            fuel_prices = [thermal.fuel_price_per_mmbtu] * n
        startup_tiers = [
            {
                "max_offline_hours": float(t.get("max_offline_hours", 0.0)),
                "cost": float(t.get("cost", 0.0)),
                "sync_time_min": float(t.get("sync_time_min", 0.0)),
            }
            for t in thermal.startup_cost_tiers
        ]
        return GeneratorOfferSchedule(
            resource_id=thermal.resource_id,
            segments_by_period=[
                [(
                    float(thermal.nameplate_mw),
                    float(hr_mmbtu_per_mwh * fuel_prices[i] + thermal.vom_per_mwh),
                )]
                for i in range(n)
            ],
            no_load_cost_by_period=[float(thermal.no_load_cost_per_hr)] * n,
            startup_cost_tiers=startup_tiers,
        )

    def _as_capacity_for(self, product_id: str) -> float:
        """Sum across all qualified resources of their physical headroom
        for one AS product. Used as the zonal ``requirement_mw``.
        """
        cap = 0.0
        if product_id in self.site.bess.qualified_as_products:
            cap += float(
                self.site.bess.power_discharge_mw
                if product_id != "reg_down"
                else self.site.bess.power_charge_mw
            )
        for _slot, thermal in self.thermal_specs():
            if thermal is None:
                continue
            if product_id in thermal.qualified_as_products:
                cap += float(thermal.nameplate_mw - thermal.pmin_mw)
        for tier in self.site.it_load.tiers:
            # Curtailable load can provide ECRS / Non-Spin / RRS via
            # CLR-style participation. We don't yet plumb tier-level
            # AS qualifications — extend the spec when needed.
            del tier
        return cap

    def _reserve_offer_schedules(
        self, period_slice: slice, shortfall_by_product: dict[str, float]
    ) -> list[GeneratorReserveOfferSchedule]:
        """Build per-resource reserve offer schedules.

        Each qualified resource offers its physical capacity for each
        period, priced at ``shortfall − forecast_price`` so the net
        award reward equals the forecast price.
        """
        start, stop, _ = period_slice.indices(self.periods)
        n = stop - start
        prices_by_product = {
            ap.product_def.id: ap.price_forecast_per_mwh[start:stop]
            for ap in self.as_products
        }

        schedules: list[GeneratorReserveOfferSchedule] = []

        # BESS
        bess_offers_by_period: list[list[dict[str, Any]]] = []
        for t in range(n):
            row: list[dict[str, Any]] = []
            for ap in self.as_products:
                pid = ap.product_def.id
                if pid not in self.site.bess.qualified_as_products:
                    continue
                cap = (
                    self.site.bess.power_discharge_mw
                    if pid != "reg_down"
                    else self.site.bess.power_charge_mw
                )
                price_t = float(prices_by_product[pid][t])
                row.append(
                    {
                        "product_id": pid,
                        "capacity_mw": float(cap if price_t > 0.0 else 0.0),
                        "cost_per_mwh": float(shortfall_by_product[pid] - price_t),
                    }
                )
            bess_offers_by_period.append(row)
        if any(bess_offers_by_period):
            schedules.append(
                GeneratorReserveOfferSchedule(
                    resource_id=BESS_RESOURCE_ID,
                    offers_by_period=bess_offers_by_period,
                )
            )

        # Thermals
        for _slot, thermal in self.thermal_specs():
            if thermal is None or not thermal.qualified_as_products:
                continue
            cap = float(thermal.nameplate_mw - thermal.pmin_mw)
            offers_by_period: list[list[dict[str, Any]]] = []
            for t in range(n):
                row = []
                for ap in self.as_products:
                    pid = ap.product_def.id
                    if pid not in thermal.qualified_as_products:
                        continue
                    price_t = float(prices_by_product[pid][t])
                    row.append(
                        {
                            "product_id": pid,
                            "capacity_mw": float(cap if price_t > 0.0 else 0.0),
                            "cost_per_mwh": float(
                                shortfall_by_product[pid] - price_t
                            ),
                        }
                    )
                offers_by_period.append(row)
            if any(offers_by_period):
                schedules.append(
                    GeneratorReserveOfferSchedule(
                        resource_id=thermal.resource_id,
                        offers_by_period=offers_by_period,
                    )
                )

        return schedules

    def _apply_peak_demand_charges(self, builder: Any, period_slice: slice) -> None:
        if self.site.four_cp is None or not self.site.four_cp.period_indices:
            return
        start, stop, _ = period_slice.indices(self.periods)
        # Translate horizon-absolute period indices into the slice's
        # local indexing so a sequential solve sees only the local
        # flagged periods. Drop periods outside the slice.
        local: list[int] = []
        for p in self.site.four_cp.period_indices:
            if start <= p < stop:
                local.append(p - start)
        if not local:
            return
        builder.peak_demand_charges(
            [
                {
                    "name": self.site.four_cp.name,
                    "resource_id": GRID_IMPORT_RESOURCE_ID,
                    "period_indices": local,
                    "charge_per_mw": float(self.site.four_cp.charge_per_mw),
                }
            ]
        )


__all__ = [
    "AsProduct",
    "BessSpec",
    "BESS_RESOURCE_ID",
    "CurtailableLoadTier",
    "DataCenterProblem",
    "FourCpSpec",
    "GRID_EXPORT_RESOURCE_ID",
    "GRID_IMPORT_RESOURCE_ID",
    "ItLoadSpec",
    "NUCLEAR_DEFAULT_RESOURCE_ID",
    "NuclearSpec",
    "SOLAR_RESOURCE_ID",
    "SiteSpec",
    "SolarSpec",
    "ThermalSpec",
    "WIND_RESOURCE_ID",
    "WindSpec",
]
