# `<your market name>`

One-paragraph market description:

* **Participants** — who trades (generators, loads, storage, ...).
* **Products** — energy + which reserves; how they're priced
  (pay-as-bid, LMP, price-taker).
* **Clearing** — single-stage LP? SCUC → SCED? Sequential real-time?
* **Settlement / export** — what the output represents (ISO
  settlement, competition submission, revenue P&L).

Concrete reference implementations:

* [`markets/go_c3/`](../go_c3/README.md) — multi-participant DAM
  with LMPs, AS, SCUC + AC SCED reconcile (the GO Competition
  Challenge 3 spec).
* [`markets/battery/`](../battery/README.md) — single-site
  price-taker with AS co-optimisation.

## Input shape

Problem data lives in native Surge types:

* :class:`surge.Network` — topology (bus, branch, generator,
  dispatchable load, storage). Caller supplies a pre-built network
  *or* the problem builds one itself (see
  :meth:`Problem.build_network`).
* The problem dataclass carries per-period forecasts + market
  requirements.

```python
from markets.<your_market> import Policy, Problem, solve

problem = Problem(
    period_durations_hours=[1.0] * 24,
    # TODO: fill in market-specific fields
)
report = solve(problem, workdir="out/run-1", policy=Policy())
print(report["status"])
```

## The canonical DispatchRequest

Everything a Surge solver consumes comes from one nested dict. Build
it with :func:`surge.market.request` — the typed chainable builder —
**not** hand-rolled dict literals. Each method has full IDE
autocomplete and typos become errors at call time, not runtime serde
failures.

Top-level keys the request exposes:

| Key | Builder method(s) | Purpose |
|---|---|---|
| `timeline` | `.timeline(periods=, hours_by_period=)` | Required. |
| `commitment` | `.commitment_all_committed()` / `.commitment_optimize()` / `.commitment_fixed()` | SCUC mode. |
| `coupling` | `.coupling("time_coupled" \| "period_by_period")` | Needed for SOC / ramp constraints. |
| `profiles` | `.load_profile()`, `.renewable_profile()`, `.generator_derate()`, `.branch_derate()` | Per-period overrides. |
| `state.initial` | `.previous_dispatch()`, `.storage_soc_overrides()` | Ramp init + SOC carryforward. |
| `market` | `.reserve_products()`, `.zonal_reserves()`, `.generator_offers()`, `.reserve_offers()`, `.dispatchable_loads()`, `.dispatchable_load_offers()`, `.penalty_config()` | Market payload. |
| `network` | `.market_config(cfg)` | Fills penalty / network-rule defaults from :class:`MarketConfig`. |
| `runtime` | `.run_pricing()` | LMP repricing LP toggle. |
| *(escape)* | `.extend_market()`, `.extend_network()`, `.extend_state_initial()`, `.extend_runtime()`, `.raw_merge()` | Splice in fields the builder doesn't promote. |

Finalize with `.build()` — returns the
:class:`surge._generated.DispatchRequest` TypedDict.

This template's `problem.py` is the minimum-viable example — a 1-bus
network with one generator and one load, built via the builder. Copy
it and fill in your market's topology + market payload.

If you ship a `config.py`, call `.market_config(cfg)` on the builder
to fill in sensible `market.penalty_config` + `network.*` defaults
from the canonical preset. `config.py` is optional — skip it when the
framework defaults are fine (see `markets/battery/`).

## Multi-stage workflows

For SCUC → pricing or SCUC → AC SCED, use
:class:`surge.market.MarketWorkflow` instead of a single
:func:`surge.solve_dispatch` call. Each stage is a
:class:`WorkflowStage` that receives a :class:`WorkflowContext` and
returns a :class:`WorkflowStageResult`. See
[`markets/rto/workflow.py`](../rto/workflow.py) for a minimal
two-stage setup.

## Native inputs worth knowing

| Helper | What it builds |
|---|---|
| :func:`surge.case9`, `case14`, `case30`, `case57`, `case118`, `case300` | Pre-compiled test networks. |
| :func:`surge.from_dataframes` | Construct a network from pandas DataFrames. |
| :meth:`Network.add_bus` / `add_branch` / `add_generator` / `add_load` / `add_dispatchable_load` / `add_storage` | Mutate a network. |
| :class:`surge.StorageParams` | BESS parameters (efficiency, SOC bounds, offer / bid curves, degradation). |
| :class:`surge.market.ReserveProductDef` | AS-product definition (direction, qualification, energy coupling). |
| :class:`surge.market.ZonalRequirement` | Zonal reserve requirement (fixed or endogenous). |
| :class:`surge.market.GeneratorOfferSchedule` | Columnar energy offer schedule dataclass (per-period segments). |
| :class:`surge.market.GeneratorReserveOfferSchedule` | Columnar reserve offer schedule dataclass. |
| :func:`surge.market.piecewise_linear_offer` | Build a generator offer schedule dict directly. |
| :func:`surge.market.reserve_offer_schedule` | Build a per-resource reserve offer schedule dict. |

## Axes you typically vary

| Axis | Example values |
|---|---|
| Problem source | pre-built `surge.Network` \| `from_dataframes` \| in-memory `Network()` + `add_*` |
| Reserve products | `[REG_UP, REG_DOWN, SPINNING, NON_SPINNING]` \| fewer \| add reactive |
| Clearing | single-stage LP \| SCUC MIP \| SCUC → pricing LP \| SCUC → AC SCED |
| Objective sense | cost minimisation (framework native) \| price-taker surplus (via LMP-priced virtual resources — see battery market) |
| Settlement export | per-bus LMPs + AS prices + payments \| revenue P&L + SOC \| validator submission format |
| Time coupling | `period_by_period` (no SOC) \| `time_coupled` (SOC + ramp across periods) |

## What lives next to each market

* **Dashboard** — `dashboards/<name>/` for interactive per-case views.

Optional. Small markets can ship with just `markets/<name>/`.
