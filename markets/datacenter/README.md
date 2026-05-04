# Datacenter Operator Market

Single-site, behind-the-meter SCUC for a microgridded datacenter load.
Optimises commitment + dispatch + AS awards across a portfolio of
on-site generation, storage, and curtailable IT load against an
exogenous LMP forecast and (optional) AS price forecasts. Coincident-
peak transmission charges (e.g. ERCOT 4-CP) are first-class — the LP
allocates an auxiliary peak variable and minimises peak grid import on
flagged periods.

## What this market declares

| Aspect | Value |
|---|---|
| **Problem schema** | `SiteSpec` (POI, IT load tiers, BESS, solar, wind, fuel cell, gas CT, diesel, optional nuclear, optional 4-CP charge) + LMP forecast + optional AS price forecasts. |
| **Topology** | One bus with the IT load (must-serve + tiered curtailable), each on-site resource, and virtual grid-import + grid-export. |
| **Formulation** | DC SCUC MIP (commitment endogenous). Time-coupled by default; sequential mode chains single-period SCUCs with SOC carryforward. |
| **AS products** | Any caller-supplied `ReserveProductDef` set (typically the five RTC+B products: Reg-Up, Reg-Down, RRS, ECRS, Non-Spin). Per-resource physical qualifications. |
| **4-CP modeling** | Flagged-period peak-demand charge applied to grid imports via the framework's `peak_demand_charges` primitive. Mathematically exact for any flagged-period set. |
| **Export** | `pnl-report.json` with per-period schedule + per-resource P&L breakdown + top-line totals (compute revenue, energy import / export, AS, fuel, CO₂, demand charge, net P&L). |

## How the SCUC math works

The site is a 1-bus network with all resources at the POI. Two
virtual resources turn the LP's cost-min objective into operator
surplus:

1. **Virtual grid-import gen** — `pmax = poi_limit`, per-period
   offer = LMP forecast.
2. **Virtual grid-export DL** — `Curtailable`, per-period curtailment
   cost = LMP forecast (so served MW is credited at LMP).

On-site generation:

* **Solar / wind** — zero-cost gens with capacity-factor profiles.
* **BESS** — `add_storage` with full SOC dynamics, efficiencies,
  foldback, optional FEC cap. Bids into AS via `qualified_as_products`.
* **Fuel cell, gas CT, diesel** — PWL energy-offer schedules with
  marginal cost = heat-rate × fuel-price + VOM, plus no-load cost,
  startup-cost tiers, min-up/min-down, and ramp curves (set on the
  Generator object). Each is a real MIP commitment decision.
* **Nuclear** (optional) — must-run baseload pinned to
  `availability × nameplate` via `generator_dispatch_bounds`
  (LP has no dispatch freedom; planned outages are modelled by
  zeroing the per-period availability factor).

IT load:

* **Must-serve** portion enters as a fixed load profile (no
  curtailment).
* **Curtailable tiers** are `DispatchableLoadSpec` Curtailable
  resources, each with a per-tier VOLL. The LP curtails the
  lowest-VOLL tier first when on-site supply + grid import exceed
  the tier's value.

## Quick start

```python
from pathlib import Path
from markets.datacenter import (
    AsProduct, BessSpec, CurtailableLoadTier, DataCenterPolicy,
    DataCenterProblem, FourCpSpec, ItLoadSpec, SiteSpec, SolarSpec,
    ThermalSpec, WindSpec, solve,
)
from surge.market import REG_UP, ECRS, NON_SPINNING

problem = DataCenterProblem(
    period_durations_hours=[1.0] * 24,
    lmp_forecast_per_mwh=lmp_24h,
    site=SiteSpec(
        poi_limit_mw=1000.0,
        it_load=ItLoadSpec(
            must_serve_mw=[700.0] * 24,
            tiers=[
                CurtailableLoadTier("training", 200.0, voll_per_mwh=40.0),
                CurtailableLoadTier("research", 100.0, voll_per_mwh=5.0),
            ],
        ),
        bess=BessSpec(
            power_charge_mw=200.0, power_discharge_mw=200.0,
            energy_mwh=800.0,
        ),
        solar=SolarSpec(nameplate_mw=250.0, capacity_factors=solar_cf_24h),
        wind=WindSpec(nameplate_mw=150.0, capacity_factors=wind_cf_24h),
        fuel_cell=ThermalSpec(
            resource_id="fc", nameplate_mw=200.0, pmin_mw=20.0,
            fuel_price_per_mmbtu=8.0, heat_rate_btu_per_kwh=6500.0,
            min_up_h=2.0, min_down_h=1.0,
            qualified_as_products=("reg_up", "reg_down", "syn", "ecrs"),
        ),
        gas_ct=ThermalSpec(
            resource_id="ct", nameplate_mw=400.0, pmin_mw=50.0,
            fuel_price_per_mmbtu=4.0, heat_rate_btu_per_kwh=9500.0,
            vom_per_mwh=4.0, no_load_cost_per_hr=500.0,
            startup_cost_tiers=[{"max_offline_hours": 8.0, "cost": 4000.0}],
            min_up_h=4.0, min_down_h=4.0,
            co2_tonnes_per_mwh=0.45,
            qualified_as_products=("reg_up", "syn", "ecrs"),
        ),
        diesel=ThermalSpec(
            resource_id="diesel", nameplate_mw=50.0, pmin_mw=0.0,
            fuel_price_per_mmbtu=20.0, heat_rate_btu_per_kwh=10500.0,
            co2_tonnes_per_mwh=0.74,
            qualified_as_products=("nsyn",),
        ),
        four_cp=FourCpSpec(
            period_indices=[16, 17, 18, 19],
            charge_per_mw=10000.0,
        ),
    ),
    as_products=[
        AsProduct(REG_UP, [8.0] * 24),
        AsProduct(ECRS, [6.0] * 24),
        AsProduct(NON_SPINNING, [3.0] * 24),
    ],
)
report = solve(problem, Path("out/datacenter"), policy=DataCenterPolicy())
print(report["extras"]["pl_summary"])
```

## Policy knobs (`DataCenterPolicy`)

| Field | Default | Purpose |
|---|---|---|
| `commitment_mode` | `"optimize"` | `"optimize"` (SCUC MIP) vs `"fixed"` (replay). |
| `period_coupling` | `"coupled"` | `"coupled"` (one time-coupled SCUC) vs `"sequential"` (N one-period SCUCs). |
| `lp_solver` | `"highs"` | LP/MIP backend. |
| `mip_rel_gap` | `1e-3` | MIP optimality gap tolerance. |
| `mip_time_limit_secs` | `600.0` | Solver wall-clock cap. |
| `enforce_reserve_capacity` | `False` | When on, every cleared MW of AS award must be backed by SOC + ramp headroom. |

## Output shape

Each solve writes three files to *workdir*:

| File | Content |
|---|---|
| `run-report.json` | Status, timing, policy, top-line `pl_summary`. |
| `pnl-report.json` | Per-period schedule + per-resource P&L + totals (compute revenue, energy import/export, AS, fuel, CO₂, demand charge, net P&L). |
| `dispatch-result.json` | Native `DispatchResult.to_dict()`. |

## Known limitations

* The SCUC's `commitment_mode="fixed"` path is wired in `policy.py`
  but not yet in `solve.py` (use `"optimize"` for v1; add a fixed
  schedule entry point when there's a need for replay studies).
* Reactive headroom is not co-optimised — the formulation is DC.
* Curtailable IT load tiers don't yet bid into AS; CLR-style
  participation will land via per-tier `qualified_as_products`.
