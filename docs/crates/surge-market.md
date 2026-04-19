# surge-market

`surge-market` is the canonical market-formulation layer in the Surge
workspace. It sits one crate above [surge-dispatch](surge-dispatch.md):

- `surge-dispatch` is the single-stage solver kernel (DC/AC SCED, DC
  SCUC, reserve LP, AC OPF reconciliation).
- `surge-market` composes those single-stage solves into real markets
  — standard reserve-product catalogues, commitment helpers, offer
  curves, startup/shutdown trajectories, the canonical multi-stage
  workflow runner, and the canonical AC refinement runtime.

Format-specific adapters (today: GO Competition Challenge 3) translate
raw market data into calls to these canonical modules and return a
typed [`DispatchRequest`] or [`MarketWorkflow`].

## Why A Separate Crate?

A properly-scoped day-ahead / real-time power market clears in two
stages: a DC SCUC MIP that commits units and a downstream AC SCED NLP
that redispatches with fixed commitment against the full network. The
handoff between them is load-bearing — the AC NLP needs commitment
augmentation, reserve-aware dispatch pinning, AC warm starts, and
often multi-attempt retry logic to converge on stressed scenarios.

`surge-dispatch` deliberately stays single-stage. Everything that
composes multiple stages, caches state between them, or codifies
recipes that generalise across ISOs lives in this crate.

## Module Map

| Module | Provides |
|---|---|
| [`reserves`] | Standard reserve product shapes (`regulation_up`, `synchronized`, `ramping_up_on`, `reactive_headroom`, …) and requirement constructors (`zonal_requirement_from_load_fraction`, `from_largest_unit`, `from_series`). |
| [`commitment`] | Initial-condition derivation from accumulated on/off times; startup-tier construction from piecewise offline-hour costs. |
| [`offers`] | Piecewise cost → cumulative-MW / marginal-cost segments; full [`OfferCurve`] assembly. |
| [`profiles`] | Per-bus load aggregation; typed generator dispatch-bound profile wrapping. |
| [`trajectory`] | Startup / shutdown ramp-power derivation; online-status inference from solved MW. |
| [`windows`] | Hour-window → period-index translators (interval-start and interval-midpoint rules). |
| [`penalties`] | Canonical penalty-curve constructors for power-balance slack, reserve shortfall, and window violations. |
| [`heuristics`] | Resource classification helpers (producers / consumers / static resources) shared by adapters. |
| [`workflow`] | [`MarketStage`], [`MarketWorkflow`], [`solve_market_workflow`] — typed multi-stage workflow runner with commitment handoff and dispatch pinning. |
| [`canonical_workflow`] | [`canonical_two_stage_workflow`] — the standard DC SCUC → AC SCED recipe. |
| [`two_stage`] | Lower-level helpers for two-stage commitment extraction and schedule handoff. |
| [`ac_sced_setup`] | Combinator that wires adapter primitives into a canonical [`AcScedSetup`] config bag. |
| [`ac_reconcile`] | Canonical AC SCED setup implementation — reactive-reserve filter, commitment augmentation, bandable-subset producer pinning, AC warm start, Q-bound overrides, target-tracking feedback providers. |
| [`ac_refinement`] | Canonical refinement runtime — retry grid, feedback providers, commitment probes. |
| [`ac_opf_presets`] | Common [`AcOpfOptions`] overlays used by retry attempts (soft bus balance, strict, no-thermal, GO-validator-matched costs). |
| [`go_c3`] | GO Competition Challenge 3 format adapter — entry points [`build_dispatch_request`], [`export_go_c3_solution`], [`build_canonical_workflow`]. |

None of the modules above — except `go_c3` — reference any specific
market format. They take primitives and build typed
[`surge_dispatch`] structures.

## Canonical Two-Stage Workflow

The standard day-ahead recipe:

1. **Stage 1 — DC SCUC** (`Dc + TimeCoupled + Optimize`). Commits units,
   sets startup/shutdown schedules, produces the initial energy +
   reserve dispatch.
2. **Stage 2 — AC SCED** (`Ac + PeriodByPeriod + Fixed`). Redispatches
   against the full AC network with commitment pinned from stage 1.

```rust
use surge_dispatch::DispatchModel;
use surge_market::{
    canonical_workflow::{canonical_two_stage_workflow, CanonicalWorkflowOptions},
    solve_market_workflow,
};

let model = DispatchModel::prepare(&network)?;
let workflow = canonical_two_stage_workflow(
    model,
    uc_request,    // Dc + TimeCoupled + Optimize
    ed_request,    // Ac + PeriodByPeriod + Fixed (placeholder)
    CanonicalWorkflowOptions::default(),
);

let result = solve_market_workflow(&workflow)?;
let uc_solution = &result.stages[0].solution;
let ed_solution = &result.stages[1].solution;
# Ok::<(), surge_dispatch::DispatchError>(())
```

Stage ids are fixed constants: `CANONICAL_UC_STAGE_ID = "scuc"` and
`CANONICAL_ED_STAGE_ID = "sced"`.

### `CanonicalWorkflowOptions`

| Field | Type | Default | Description |
|---|---|---|---|
| `uc_options` | `DispatchSolveOptions` | default | Solve-time options for stage 1 (LP/MIP backend overrides). |
| `ed_options` | `DispatchSolveOptions` | default | Solve-time options for stage 2 (NLP backend overrides). |
| `ed_band_fraction` | float | `0.05` | Fractional P-band around each stage-1 dispatch target (±5 %). |
| `ed_band_floor_mw` | float | `1.0` | Minimum absolute band width in MW. |
| `ed_band_cap_mw` | float | `1.0e9` | Maximum absolute band width in MW. |

### Executor Handoffs

The workflow runner applies these handoffs between stages:

- **Commitment handoff** — extract the solved commitment schedule from
  the upstream stage, pin it into the downstream stage as
  `CommitmentPolicy::Fixed`.
- **Dispatch pinning** — narrow the downstream stage's generator
  `p_min_mw` / `p_max_mw` profiles around the upstream dispatch using
  the configured band.
- **AC SCED setup** — when a stage carries an `ac_sced_setup`, apply
  reactive-reserve filtering, voltage-support commitment augmentation,
  bandable-subset producer pinning, and per-period AC warm start built
  from the source solution.
- **Branch thermal relaxation** — when a stage carries
  `branch_relax_from_dc_slack`, inject per-period branch derate factors
  >1 from the source stage's `branch_thermal` constraint slacks so the
  AC NLP isn't asked to satisfy infeasible DC-leftover flows.

## Reserve Products

Modern day-ahead / real-time markets share a common reserve taxonomy.
The [`reserves`] module exposes it as typed constructors so adapters
do not reinvent the wheel:

| Product | Direction | Deploy | Qualification | Energy coupling |
|---|---|---|---|---|
| `regulation_up` | Up | 5 min | Committed | Headroom |
| `regulation_down` | Down | 5 min | Committed | Footroom |
| `synchronized` | Up | 10 min | Committed | Headroom |
| `non_synchronized` | Up | 10 min | Offline quick-start | Headroom |
| `ramping_up_on` / `ramping_up_off` | Up | 15 min | Online / Offline | Headroom |
| `ramping_down_on` / `ramping_down_off` | Down | 15 min | Online / Offline | Footroom |
| `reactive_up` / `reactive_down` | MVAr | — | Committed | MVAr-side |

Requirement constructors cover the three common zonal patterns:

- `zonal_requirement_from_load_fraction` — fraction of served load
  (regulation-type products).
- `zonal_requirement_from_largest_unit` — fraction of the largest
  dispatched producer (contingency-sized products).
- `zonal_requirement_from_series` — exogenous time-series requirement
  (market-operator-set ramping / reactive reserves).

The synthetic [`reactive_headroom_product`] constructor covers the
rare case where an aggregate MVAr requirement has no exogenous zone.

## AC SCED Setup

The AC SCED stage needs considerable pre-solve enrichment beyond the
bare request out of an adapter's `build_dispatch_request`. Without
these pieces, the AC NLP starts from flat voltages and zero Q and
routinely runs out of iterations.

[`AcScedSetup`] carries six canonical handoffs:

1. **Commitment handoff** — performed by the workflow executor from
   the source stage's solution.
2. **Commitment augmentation** — merge extra must-run schedules
   (voltage-support generators that must stay online on the AC stage)
   onto the pinned commitment.
3. **Reactive-reserves-only market filter** — strip active reserve
   products and awards that the AC stage cannot re-clear
   ([`apply_reactive_reserve_filter`]).
4. **Bandable-subset dispatch pinning** — narrow most generators' P
   bounds to their stage-1 dispatch, but give a designated subset
   (slack-bus gens, top-Q-range gens) a wider band so the NLP has
   headroom to close reactive corners
   ([`apply_producer_dispatch_pinning`]).
5. **Warm start** — populate `runtime.ac_dispatch_warm_start` with
   per-bus V/θ and per-resource P/Q seeds read from the upstream
   solution ([`build_ac_dispatch_warm_start`]).
6. **Q locks / fixes** — zero-bound Q for synthetic HVDC terminal
   support generators or fix their Q to an external schedule.

Typical setup:

```rust
use surge_market::{
    AcScedSetup, AcWarmStartConfig, MarketStage, ProducerDispatchPinning,
};

let setup = AcScedSetup {
    source_stage_id: "scuc".into(),
    reactive_reserve_product_ids: Some(["q_res_up", "q_res_down"].into()),
    commitment_augmentation: voltage_support_schedules,
    dispatch_pinning: Some(ProducerDispatchPinning {
        producer_resource_ids,
        producer_static_resource_ids,
        bandable_producer_resource_ids,
        band_fraction: 0.05,
        band_floor_mw: 1.0,
        band_cap_mw: 1.0e9,
        up_reserve_product_ids,
        down_reserve_product_ids,
        apply_reserve_shrink: true,
        relax_pmin: false,
        relax_pmin_for_resources: Default::default(),
    }),
    warm_start: Some(AcWarmStartConfig {
        bus_uid_to_number,
        producer_resource_ids,
        dispatchable_load_resource_ids,
        consumer_block_resource_ids_by_uid,
        consumer_q_to_p_ratio_by_uid,
    }),
    ..Default::default()
};

let stage = MarketStage::new("sced", role, model, request).with_ac_sced_setup(setup);
```

## AC Refinement Runtime

For stages that need retries — different OPF overlays, different NLP
solvers, wider P bands when the default dispatch leaves too much
penalty — attach a [`RetryPolicy`]. When present, the workflow executor
dispatches the stage through [`RefinementRuntime`] instead of calling
the single-shot [`DispatchModel::solve_with_options`].

### The Retry Grid

The runtime drives a nested grid:

```
relax_pmin_sweep
  × opf_attempts
    × nlp_solver_candidates
      × band_attempts
        × hvdc_attempts
```

Each cell clones the stage's request, applies the attempt's overrides,
solves, and records a [`RefinementAttemptReport`]. The first
successful cell returns immediately unless its penalty cost exceeds
`wide_band_penalty_threshold_dollars`, in which case subsequent band
attempts run and the lowest-penalty solution wins.

### Attempt Types

| Attempt | Purpose |
|---|---|
| [`OpfAttempt`] | Named [`AcOpfOptions`] patch (e.g. `strict_bus_balance`, `no_thermal_limits`). |
| [`BandAttempt`] | Dispatch-pinning band + bandable-producer count + anchor set. |
| [`HvdcAttempt`] | Override `runtime.fixed_hvdc_dispatch` with `Default`, `Flipped`, or `Neutral` strategy. |

### HVDC Strategy

The AC SCED's HVDC direction is not pinned by the canonical workflow,
but the DC warm-start bus voltages effectively anchor Ipopt near the
DC stage's HVDC choice. When the DC LP is degenerate (many zero-cost
renewables with no binding transmission → flat LMPs), the DC solver
picks an arbitrary HVDC direction that may land AC in an infeasibility
basin. [`HvdcStrategy`] drives the fallback:

| Strategy | What it does |
|---|---|
| `Default` | No override. Let AC NLP start from its natural equilibrium. |
| `Flipped` | Pin every HVDC link's per-period `(P, Q_fr, Q_to)` to the **negation** of the upstream dispatch. |
| `Neutral` | Pin every HVDC link's per-period `(P, Q_fr, Q_to)` to **zero**. |

### `RetryPolicy`

| Field | Type | Description |
|---|---|---|
| `relax_pmin_sweep` | `Vec<bool>` | Outer sweep over `runtime.ac_relax_committed_pmin_to_zero`. |
| `opf_attempts` | `Vec<OpfAttempt>` | Named AC-OPF patches tried in order. |
| `nlp_solver_candidates` | `Vec<Option<String>>` | NLP solver names tried in order (`None` = default). |
| `band_attempts` | `Vec<BandAttempt>` | Dispatch-pinning band attempts. |
| `wide_band_penalty_threshold_dollars` | float | Penalty threshold above which the next band attempt runs. |
| `hvdc_attempts` | `Vec<HvdcAttempt>` | HVDC-override fallbacks. |
| `hvdc_retry_bus_slack_threshold_mw` | float | Bus-slack threshold that triggers the next HVDC attempt. |
| `hard_fail_first_attempt` | bool | If true, the first attempt's exception propagates immediately (debug escape hatch). |
| `feedback_providers` | `Vec<Arc<dyn FeedbackProvider>>` | Pre-solve hooks. |
| `commitment_probes` | `Vec<Arc<dyn CommitmentProbe>>` | Between-iteration mutators. |
| `max_iterations` | int | Maximum outer refinement iterations (0 disables probe-driven iteration). |

Two presets are provided: [`RetryPolicy::noop`] (single solve, no
retries) and [`RetryPolicy::goc3_default`] (relax-pmin sweep, three
OPF overlays, default→wide band, full HVDC fallback).

### Feedback Providers

[`FeedbackProvider`] is a pre-solve hook that inspects the prior
stage's solution and mutates the current stage's request. Two
canonical implementations ship in [`ac_reconcile`]:

- [`DcReducedCostTargetTracking`] — reads LP bound shadow prices
  (`pg_lower:<rid>` / `pg_upper:<rid>`) from the source stage and
  derives asymmetric P-tracking penalties.
- [`LmpMarginalCostTargetTracking`] — derives penalties from the
  economic arbitrage `LMP_at_bus − marginal_cost(P_g)`. Works for any
  LP where column duals may be noisy.

Both attach via `RetryPolicy::with_feedback` and take
`Arc<dyn FeedbackProvider>`. Caller-supplied overrides in the request
win over provider-computed ones.

### Commitment Probes

[`CommitmentProbe`] is a between-iteration hook that may rewrite the
request (e.g. the pmin=0 decommit probe that adjusts the source
stage's commitment based on a pmin-relaxed AC probe). Scaffolding is
in place via the [`CommitmentProbe`] trait and
`RetryPolicy::with_commitment_probe`; specific probe implementations
are future work.

## Format Adapters

### GO Competition Challenge 3

[`go_c3`] is the reference implementation of a format adapter. Three
public entry points:

| Function | Purpose |
|---|---|
| [`go_c3::build_dispatch_request`] | GO C3 problem JSON → [`DispatchRequest`]. |
| [`go_c3::export_go_c3_solution`] | [`DispatchSolution`] → GO C3 solution JSON. |
| [`go_c3::build_canonical_workflow`] | Canonical two-stage DC SCUC → AC SCED [`MarketWorkflow`]. |

Internal sub-modules handle consumer block decomposition, HVDC-link
and reactive synthetic generators, penalty configuration, reserve
catalogue, AC OPF defaults, and AC SCED setup (bandable-producer
selection, reactive-support commitment schedule).

### Writing A New Adapter

Translate the source format's field names into calls to the canonical
modules above, then return a typed [`DispatchRequest`] (and optionally
a [`MarketWorkflow`]). The shape of a new adapter (ERCOT, MISO, CAISO,
etc.) is identical to `go_c3`.

The canonical `tests/canonical_workflow.rs` and
`tests/canonical_refinement.rs` integration tests run on a synthetic
three-bus network with no GO C3 dependency — a sanity check a new
adapter can run first.

## Python Bindings

The crate's canonical entry points are exposed through
[surge-py](surge-py.md) as the `surge.market` package. The headline
primitives for building a market:

| Import | Purpose |
|---|---|
| `surge.market.request` | Typed `DispatchRequestBuilder` factory — the canonical way to assemble a `DispatchRequest` dict. |
| `surge.market.MarketProblem` | Protocol every `markets/<name>/` problem dataclass satisfies (`build_network` + `build_request`). |
| `surge.market.MarketConfig` | Penalty tensor, network rules, AC-reconcile config, Benders config. `MarketConfig.default()` is the canonical starting preset. |
| `surge.market.MarketWorkflow` / `WorkflowRunner` | Multi-stage workflow (SCUC → pricing → AC SCED). |
| `surge.market.run_market_solve` | Wraps a market's solve with `SolveLogger` + timing + `run-report.json`. |
| `surge.market.DispatchableLoadSpec` | Typed dispatchable-load resource declaration. Cost models built via `linear_curtailment` / `quadratic_utility` / `piecewise_linear_utility` / `interrupt_penalty`. |
| `surge.market.ReserveProductDef` / `ZonalRequirement` | Typed AS product definitions + zonal requirements. |
| `surge.market.GeneratorOfferSchedule` / `GeneratorReserveOfferSchedule` | Typed per-resource energy and reserve offer schedules. |

### Building a DispatchRequest

```python
from surge.market import MarketConfig, REG_UP, ZonalRequirement, request

cfg = MarketConfig.default(base_mva=100.0)

req = (
    request()
    .timeline(periods=24, hours_by_period=[1.0] * 24)
    .commitment_optimize(mip_rel_gap=1e-3)
    .coupling("time_coupled")
    .load_profile(bus=4, values=load_forecast)
    .zonal_reserves([
        ZonalRequirement(zone_id=1, product_id=REG_UP.id,
                         requirement_mw=5.0, per_period_mw=[5.0] * 24),
    ])
    .market_config(cfg)            # fills missing penalty/network defaults
    .run_pricing(True)
    .build()                        # → surge._generated.DispatchRequest
)
```

### GO Competition Challenge 3 adapter

```python
import surge.market.go_c3 as go_c3

problem = go_c3.GoC3Problem.load("scenario_911.json")
policy = go_c3.MarketPolicy(
    formulation="dc",
    ac_reconcile_mode="ac_dispatch",
    lp_solver="gurobi",
    nlp_solver="ipopt",
)
# GoC3Problem satisfies MarketProblem — it exposes .build_network(policy)
# and .build_request(policy) methods like any other market.
workflow = go_c3.build_workflow(problem, policy)
result = go_c3.solve_workflow(
    workflow, lp_solver=policy.lp_solver, nlp_solver=policy.nlp_solver,
)
dc = result["stages"][0]["solution"]
ac = result["stages"][-1]["solution"]
solution = go_c3.export(problem, ac, dc_reserve_source=dc)
go_c3.save(solution, "solution.json")
```

The higher-level Python framework source lives in
`src/surge-py/python/surge/market/`. See
[markets/README.md](../../markets/README.md) for the full contract a
market module provides, and [markets/_template/](../../markets/_template/)
for a copy-me skeleton.

## Tests

```bash
cargo test -p surge-market --lib --tests
```

Key integration tests:

- `tests/canonical_workflow.rs` — two-stage commitment + pin handoff
  on a synthetic three-bus network.
- `tests/canonical_refinement.rs` — retry grid + feedback provider +
  producer-pinning composition on the same synthetic workflow. No GO
  C3 dependency; a sanity check a new market adapter can run first.

## Related Docs

- [surge-dispatch](surge-dispatch.md) — the single-stage kernel this
  crate composes.
- [surge-opf](surge-opf.md) — the AC-OPF implementation whose options
  the [`ac_opf_presets`] retry attempts patch.
- [surge-py](surge-py.md) — Python surface for market workflows.
- [Markets directory](../../markets/README.md) — market specs that
  consume this framework.
- [Data Model And Conventions](../data-model.md) — offer curves,
  ratings, and reserve conventions.
- [References](../references.md#market-dispatch-and-commitment) —
  PNNL-35792 and arXiv:2411.12033, the ARPA-E GO Competition Challenge
  3 problem formulation and analysis paper that the canonical market
  formulation here implements.
