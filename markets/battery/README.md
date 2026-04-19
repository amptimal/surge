# Battery Operator Market

Single-site, price-taker optimisation of a BESS against an exogenous
LMP forecast and (optionally) AS price forecasts. Input and output
are native Surge types: :class:`surge.Network` + dispatch request in,
:class:`DispatchResult` + derived revenue report out.

## What this market declares

| Aspect | Value |
|---|---|
| **Problem schema** | `SiteSpec` (POI, BESS power/energy/efficiency/SOC, optional site load and PV) + LMP forecast + AS price forecasts + optional `PwlBidStrategy`. |
| **Topology** | One bus with BESS + virtual grid-import gen + virtual grid-export dispatchable load. |
| **Formulation** | DC LP. Time coupling depends on policy; commitment is always "all committed". |
| **Dispatch modes** | `optimal_foresight` (zero-cost BESS) or `pwl_offers` (submitted bid/offer curves). |
| **Period coupling** | `coupled` (single time-coupled LP) or `sequential` (N one-period LPs with SOC carryforward). |
| **AS products** | Any `ReserveProductDef` the caller supplies. Per-product per-period forecast price; optional per-product bid price. |
| **Export** | `revenue-report.json` with per-period charge / discharge / SOC / energy revenue / AS awards / degradation cost. |
| **MarketConfig** | None — a single-site price-taker study needs no penalty / network-rule overrides, so this module omits `config.py` and uses the framework `MarketConfig` defaults. See `markets/README.md` for the full contract. |

## The four-mode matrix

The two policy flags `dispatch_mode` × `period_coupling` give four
distinct interpretations of a battery operator study:

| | `coupled` | `sequential` |
|---|---|---|
| **`optimal_foresight`** | **Revenue ceiling.** LP sees the full LMP forecast and extracts maximum arbitrage. Zero-cost BESS, SOC linked across periods. | **Myopic baseline.** Each period's LP only sees its own LMP; SOC carries forward from the prior result. The battery can't foresee later peaks — typically leaves money on the table. |
| **`pwl_offers`** | **DA clearing against your bids.** The battery's discharge-offer and charge-bid curves gate dispatch across the whole horizon. Answers "what would my submitted bid strategy clear in a DAM?". | **RTM clearing against your bids.** Same bids, one period at a time. Simulates an ISO's sequential real-time market clearing — bids act as a self-commitment device, so revenue often matches the coupled case even without foresight. |

**Use `optimal_foresight / coupled` for**: the revenue ceiling — what a
perfectly informed battery could earn. Great as a benchmark.

**Use `pwl_offers / sequential` for**: the most realistic RTM
simulation. What your battery *actually earns* in a sequentially
cleared market given the specific offer strategy you submit.

**Use `pwl_offers / coupled` for**: day-ahead bidding studies where
the ISO co-optimises all 24 hours against your bids.

**Use `optimal_foresight / sequential` for**: sensitivity on forecast
value — measures the cost of not knowing the future when the battery
has no self-commitment device.

## How the LP math works

The site is a 1-bus network with three resources (four in
`pwl_offers` mode, where the BESS's own offer/bid curves are active):

1. **BESS** — a storage generator with `pmin = −charge_mw_max`,
   `pmax = +discharge_mw_max`, and :class:`StorageParams` for SOC
   dynamics.
2. **Virtual grid-import gen** — conventional gen with
   `pmax = POI_limit` and per-period offer curve equal to
   `LMP_forecast[t]`.
3. **Virtual grid-export DL** — curtailable dispatchable load with
   per-period `LinearCurtailment { cost_per_mw = LMP_forecast[t] }`.

In **`optimal_foresight`** mode the BESS has zero cost, so the LP
treats it as a perfectly flexible resource and arbitrages the LMP
spread up to the SOC envelope.

In **`pwl_offers`** mode the BESS runs in `offer_curve` dispatch
mode with the strategy's `discharge_offer_segments` and
`charge_bid_segments` applied statically over the horizon:

* **Discharge** incurs offer-curve cost per MW in the LP objective.
  Since the grid-export DL credits LMP per MW served, the LP
  discharges iff `LMP > offer_price`.
* **Charge** receives a per-MW credit equal to the bid price (via
  a sign-flipped `charge_bid` curve in `StorageParams`). Since the
  grid-import gen costs LMP per MW supplied, the LP charges iff
  `LMP < bid_price`.

## Quick start

### Revenue ceiling (default)

```python
from pathlib import Path
from markets.battery import BatteryPolicy, BatteryProblem, SiteSpec, solve

lmp = [25, 22, 20, 18, 20, 25, 30, 40, 55, 60,
       65, 70, 70, 68, 65, 60, 55, 50, 60, 75,
       80, 70, 50, 35]

problem = BatteryProblem(
    period_durations_hours=[1.0] * 24,
    lmp_forecast_per_mwh=lmp,
    site=SiteSpec(
        poi_limit_mw=50.0,
        bess_power_charge_mw=25.0,
        bess_power_discharge_mw=25.0,
        bess_energy_mwh=100.0,
        bess_charge_efficiency=0.90,
        bess_discharge_efficiency=0.98,
        bess_initial_soc_mwh=50.0,
    ),
)
report = solve(problem, Path("out/ceiling"), policy=BatteryPolicy())
```

### PWL bidding, sequential clearing

```python
from markets.battery import (
    BatteryPolicy, BatteryProblem, SiteSpec, PwlBidStrategy, solve,
)

problem = BatteryProblem(
    period_durations_hours=[1.0] * 24,
    lmp_forecast_per_mwh=lmp,
    site=SiteSpec(
        poi_limit_mw=50.0,
        bess_power_charge_mw=25.0,
        bess_power_discharge_mw=25.0,
        bess_energy_mwh=100.0,
        bess_charge_efficiency=0.90,
        bess_discharge_efficiency=0.98,
        bess_initial_soc_mwh=50.0,
    ),
    pwl_strategy=PwlBidStrategy.flat(
        discharge_capacity_mw=25.0, discharge_price=55.0,
        charge_capacity_mw=25.0, charge_price=30.0,
    ),
)
report = solve(
    problem, Path("out/rtm-bids"),
    policy=BatteryPolicy(
        dispatch_mode="pwl_offers",
        period_coupling="sequential",
    ),
)
```

### Multi-segment PWL curve

```python
pwl_strategy=PwlBidStrategy(
    # Discharge up to 10 MW at $40/MWh, next 15 MW at $60/MWh
    discharge_offer_segments=[(10.0, 40.0), (25.0, 60.0)],
    # Charge up to 10 MW at $35/MWh, next 15 MW at $25/MWh
    charge_bid_segments=[(10.0, 35.0), (25.0, 25.0)],
    # AS bid: $3/MW for reg-up
    as_offer_prices_per_mwh={"reg_up": 3.0},
)
```

## Policy knobs (`BatteryPolicy`)

| Field | Default | Purpose |
|---|---|---|
| `dispatch_mode` | `"optimal_foresight"` | `"optimal_foresight"` — zero-cost BESS, perfect-forecast arbitrage. `"pwl_offers"` — use the submitted PWL bid/offer curves. |
| `period_coupling` | `"coupled"` | `"coupled"` — single time-coupled LP. `"sequential"` — N one-period LPs, SOC carried forward. |
| `lp_solver` | `"highs"` | LP backend. |
| `log_level` | `"info"` | Python logging verbosity. |

## Input shape

```python
SiteSpec(
    poi_limit_mw=50.0,
    bess_power_charge_mw=25.0,       # MW (positive)
    bess_power_discharge_mw=25.0,    # MW (positive)
    bess_energy_mwh=100.0,
    bess_charge_efficiency=0.90,
    bess_discharge_efficiency=0.98,
    bess_soc_min_fraction=0.10,
    bess_soc_max_fraction=0.95,
    bess_initial_soc_mwh=None,       # None → 50 % of capacity
    bess_degradation_cost_per_mwh=0.0,
    site_load_mw=None,
    site_pv_mw=None,
)

AsProduct(
    product_def=REG_UP,              # any surge.market.ReserveProductDef
    price_forecast_per_mwh=[8.0, 9.0, ...],  # length = periods
)

# For dispatch_mode="pwl_offers":
PwlBidStrategy(
    discharge_offer_segments=[(25.0, 50.0)],        # (cum_mw, price) segments
    charge_bid_segments=[(25.0, 30.0)],
    as_offer_prices_per_mwh={"reg_up": 2.0},
)
# or
PwlBidStrategy.flat(
    discharge_capacity_mw=25.0, discharge_price=50.0,
    charge_capacity_mw=25.0, charge_price=30.0,
    as_offer_prices_per_mwh={"reg_up": 2.0},
)
```

## Output shape

Each solve writes three files to *workdir*:

| File | Content |
|---|---|
| `run-report.json` | Status, timing, policy, `revenue_summary` top-line totals. |
| `revenue-report.json` | Per-period schedule (charge / discharge / SOC / net export / LMP / energy revenue), per-product AS awards, totals. |
| `dispatch-result.json` | Native `DispatchResult.to_dict()`. A single object in `coupled` mode; a list of per-period objects in `sequential` mode. |

The top-line `revenue_summary`:

```json
{
  "energy_revenue_dollars": 1500.0,
  "as_revenue_dollars": 3750.0,
  "degradation_cost_dollars": 100.0,
  "net_revenue_dollars": 5150.0,
  "total_charge_mwh": 25.0,
  "total_discharge_mwh": 25.0,
  "total_throughput_mwh": 50.0,
  "full_equivalent_cycles": 0.25
}
```

## Known limitations

* **`pwl_offers / coupled` uses static offer/bid curves over the
  horizon.** Per-period variation in the energy bids requires
  `sequential` mode or multiple runs. This is a surge-storage-layer
  constraint; `generator_offer_schedules` in the request does not
  override a storage resource's offer curves.
* **AS bids are a single scalar per product.** Multi-segment AS
  offer curves aren't yet supported — a common simplification that
  matches most ISO practice.
* **Single site only.** Fleet optimisation is out of scope.

## What lives next to this market

- Dashboard (future): `dashboards/battery/` will show SOC
  trajectories, revenue P&L, cycle counts, four-mode side-by-side
  comparisons.
- Harness (future): `benchmarks/battery/` will carry reference
  LMP traces and analytic-optimum comparisons.
