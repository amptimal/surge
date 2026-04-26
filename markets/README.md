# Markets

A **market** in this tree is the declarative spec for a dispatch
formulation: what problem data it consumes, what products are priced,
which workflow it runs, and what shape its output takes. Each market
lives in its own subpackage; this directory is where the *formulations*
live — not the dashboards.

| Layer | Lives in | Role |
|---|---|---|
| **Market spec** | `markets/<name>/` | What this market *is* — topology conventions, policy, config, solve, export. |
| **Framework** | `src/surge-py/python/surge/market/` + `src/surge-market/` | Reusable building blocks (MarketConfig, ReserveProductDef, MarketWorkflow, run_market_solve). |
| **Dashboard** | `dashboards/<name>/` | Interactive UI — per-case JSON + live re-solve. |

## Reference implementations

| Market | What it does | Topology | Clearing |
|---|---|---|---|
| [`markets/battery/`](battery/README.md) | Single-site price-taker BESS optimisation | 1-bus network built in-memory | Single-stage time-coupled LP |
| [`markets/go_c3/`](go_c3/README.md) | GO Competition Challenge 3 | GO C3 JSON → Rust adapter | SCUC MIP → AC SCED NLP |

Start a new market by copying [`markets/_template/`](_template/README.md).

## The contract

Every market module provides four required files. Copy the template to
start, then fill each in. Input and output **flow through native
Surge data types** — `surge.Network`, the canonical DispatchRequest
dict, `DispatchResult`, and `MarketConfig`. No custom JSON schemas,
no bespoke adapters.

The `Problem` dataclass must satisfy the
:class:`surge.market.MarketProblem` Protocol — two methods,
`build_network(policy)` and `build_request(policy)`. The request is
assembled with `surge.market.request()` (the typed
`DispatchRequestBuilder`) — not hand-rolled dict literals. Everything
else is market-specific.

### Required

| File | Required API | What you decide |
|---|---|---|
| `problem.py` | `@dataclass Problem` conforming to `surge.market.MarketProblem` — `build_network(policy) → surge.Network` + `build_request(policy) → DispatchRequest`. | How forecasts / requirements / site params map onto the canonical DispatchRequest. |
| `policy.py` | `@dataclass Policy` | Which LP/MIP solver, commitment mode, MIP gap, penalty multipliers. |
| `solve.py` | `solve(problem, workdir, *, policy: Policy \| None = None, label: str \| None = None) → dict` | Which workflow (single LP vs SCUC → pricing vs SCUC → AC SCED); writes market artifacts to `workdir`. |
| `__init__.py` | Re-exports the public API. First three lines expose `{Market}Policy`, `{Market}Problem`, `solve` in that order. | — |
| `README.md` | 1 page: what this market is, quick-start, policy knobs, scope. | — |

### Optional

| File | Purpose |
|---|---|
| `config.py` | `default_config(policy) → surge.market.MarketConfig` — provide when you have penalty / network-rule / reserve defaults worth locking in (RTO, GO C3). Skip when framework defaults are fine (battery). |
| `export.py` | Turn the native DispatchResult into a market-specific report (settlement.json for LMPs, revenue-report.json for batteries, solution.json for GO C3). |
| `workflow.py` | Multi-stage workflow builder if the market isn't a single LP call. |

### Environment

Markets expect `surge` to be importable — run `maturin develop` from
`src/surge-py/` or `uv pip install -e 'src/surge-py[dev]'` in your
environment before calling `solve`.

## Why native-Surge input / output

Problem data flows through typed Surge constructs from end to end:

```
caller                 markets/<name>/                    surge
──────                 ─────────────────                  ─────
  ↓                          ↓                              ↓
network ──(pre-built)──► Problem ──build_request──► DispatchRequest dict
  or                       │
  built via                └─►build_network──────► surge.Network
  surge.case118()
  + add_* / from_df                                          ↓
                                                      solve_dispatch
                                                             ↓
                                                      DispatchResult
                                                             ↓
                                                       export helpers
                                                             ↓
                                                 per-market report JSON
```

This means:

* No problem-file parser unless the market has a pre-existing
  on-disk schema it must consume (like GO C3's JSON).
* Problem loaders for forecasts / requirements can be as simple as
  CSV → dict → dataclass.
* Test fixtures construct tiny in-memory networks directly — no
  round-trip through disk or adapters (see `markets/battery/` and
  the 10-test suite under `tests/battery/`).
* Results are inspected through typed Surge result objects
  (`DispatchResult.periods[i].bus_results[j].lmp`, etc), not by
  parsing exported JSON.

## Building the DispatchRequest

Use :func:`surge.market.request` (the typed
:class:`DispatchRequestBuilder`) — every chain-able method is typed,
so IDE autocomplete surfaces the shape of the request and typos become
errors at call time, not runtime serde failures.

```python
from surge.market import MarketConfig, REG_UP, ZonalRequirement, request

cfg = MarketConfig.default(base_mva=100.0)

req = (
    request()
    .timeline(periods=24, hours_by_period=[1.0] * 24)
    .commitment_optimize(mip_rel_gap=1e-3)
    .coupling("time_coupled")
    .load_profile(bus=4, values=load_forecast)
    .generator_offers([energy_offer_schedule])
    .zonal_reserves([
        ZonalRequirement(zone_id=1, product_id=REG_UP.id,
                         requirement_mw=5.0, per_period_mw=[5.0] * 24),
    ])
    .market_config(cfg)            # fills missing penalty/network defaults
    .run_pricing(True)
    .build()                        # → DispatchRequest TypedDict
)
```

Key semantics:

* **`market_config(cfg)` preserves caller intent.** It fills missing
  `market.penalty_config` and `network.*` keys; anything the builder
  has already set is kept.
* **No scalar broadcasting on profile values.** Every `values=` / 
  `capacity_factors=` list must match `periods` exactly — guardrail
  against a scalar being silently broadcast as a fill value.
* **Accepts the existing typed dataclasses.**
  `GeneratorOfferSchedule`, `ZonalRequirement`, `ReserveProductDef`,
  and `DispatchableLoadSpec` render themselves; the builder calls
  their render methods internally.
* **Escape hatches** (`extend_market`, `extend_network`,
  `extend_state_initial`, `extend_runtime`, `raw_merge`) let you
  splice in fields the builder doesn't explicitly promote. The
  builder is designed to cover the 80% — rare fields stay accessible.

### Dispatchable loads

Declare a dispatchable-load resource with
:class:`surge.market.DispatchableLoadSpec` and override per-period
parameters with :class:`DispatchableLoadOfferSchedule`. Cost models
are built via small factories so you never type the
`{"LinearCurtailment": {"cost_per_mw": X}}` tagged-enum shape by
hand:

```python
from surge.market import (
    DispatchableLoadOfferSchedule,
    DispatchableLoadSpec,
    linear_curtailment,
    request,
)

dl = DispatchableLoadSpec(
    resource_id="dr_factory_42",
    bus=4,
    p_sched_pu=0.5,
    p_max_pu=0.5,
    cost_model=linear_curtailment(9000.0),  # $/MWh VOLL
    archetype="Curtailable",
)
schedule = DispatchableLoadOfferSchedule(
    resource_id="dr_factory_42",
    periods=[
        {"p_sched_pu": 0.5, "p_max_pu": 0.5,
         "cost_model": linear_curtailment(cost)}
        for cost in per_period_curtailment_costs
    ],
)
req = (
    request()
    .timeline(periods=24, hours_by_period=[1.0] * 24)
    .dispatchable_loads([dl])
    .dispatchable_load_offers([schedule])
    .build()
)
```

Other cost-model factories: :func:`quadratic_utility`,
:func:`piecewise_linear_utility`, :func:`interrupt_penalty`.

## Axes that vary between markets

A new market is a delta along these axes — nothing else:

| Axis | GO C3 | RTO day-ahead | Battery operator |
|---|---|---|---|
| Problem source | GO C3 JSON + Rust adapter | `surge.Network` + forecast arrays | In-memory `Network()` with `add_storage` + forecasts |
| Reserve products | All 10 incl. reactive | `REG_UP`, `REG_DOWN`, `SPINNING`, `NON_SPINNING` | Any caller-supplied subset |
| Objective | Min cost + penalties | Min cost + penalties | Price-taker surplus (via LMP-priced virtual resources) |
| Commitment | SCUC MIP | SCUC MIP or LP-only | LP-only (no MIP) |
| Workflow | SCUC → AC SCED | SCUC + optional pricing LP | Single-stage time-coupled LP |
| AC reconcile | `ac_dispatch` | `none` | `none` |
| Export | `solution.json` (GO C3) | `settlement.json` (LMPs, AS prices, payments) | `revenue-report.json` (P&L + SOC) |

The SCUC / SCED kernel, LP backend, reserve pricing, violation
assessment, MarketWorkflow runner, and AC reconcile helpers are all
framework code — same for every market.

## Reusable patterns

**Price-taker grid proxy.** A single-site market (battery, solar
operator, DR aggregator, VPP) models its exogenous price signal by
adding a *virtual grid-import generator* (per-period offer curve =
LMP forecast) and a *virtual grid-export dispatchable load*
(per-period curtailment cost = LMP forecast) at the POI bus. The LP's
objective reduces to the operator's net surplus — no special
"price-taker" mode is needed in the framework. See
[`markets/battery/problem.py`](battery/problem.py) for the reference
implementation (`_market_payload` + `build_network`).

## Public API a caller sees

```python
from markets.battery   import BatteryPolicy,  BatteryProblem,  solve
from markets.go_c3     import GoC3Policy,     GoC3Problem,     solve
```

That's the whole surface. Interactive per-case views live in
`dashboards/<name>/` and call into these.
