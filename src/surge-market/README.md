# surge-market

Canonical market formulation crate for [Surge](https://github.com/ampowerinc/surge).

`surge-market` sits one layer above [`surge-dispatch`](../surge-dispatch):

- `surge-dispatch` is the canonical single-stage solver kernel (DC / AC
  SCED, DC SCUC, reserve LP, AC OPF reconciliation, etc.)
- `surge-market` hosts the canonical *market-layer* formulation that
  composes those single-stage solves into real markets — standard
  reserve-product constructors, commitment helpers, offer curves,
  startup / shutdown trajectories, the canonical multi-stage market
  workflow, and the canonical AC refinement runtime.

## Canonical market formulation

The crate's top-level modules implement the canonical pieces that are
shared by any properly-scoped power market:

| Module | What it provides |
|---|---|
| [`reserves`] | Standard reserve product shapes (`regulation_up`, `synchronized`, `ramping_*`, `reactive_headroom`, …) and requirement constructors (`zonal_requirement_from_load_fraction`, `zonal_requirement_from_largest_unit`, `zonal_requirement_from_series`). |
| [`commitment`] | Initial-condition derivation from accumulated on/off times, startup-tier construction from piecewise costs. |
| [`offers`] | Piecewise cost → cumulative-MW / marginal-cost segments; full `OfferCurve` assembly. |
| [`profiles`] | Per-bus load aggregation, generator dispatch-bound profile wrapping. |
| [`trajectory`] | Startup / shutdown ramp profile derivation, online-status inference from solved MW. |
| [`windows`] | Hour-window → period-index translators (startup windows, energy windows). |
| [`workflow`] | `MarketStage`, `MarketWorkflow`, `solve_market_workflow`, commitment handoff between stages, per-stage generator dispatch-bound pinning. |
| [`canonical_workflow`] | `canonical_two_stage_workflow` — standard DC SCUC → AC SCED recipe. |
| [`ac_reconcile`] | Canonical AC SCED setup — reactive reserve filter, commitment augmentation, bandable-subset producer pinning, AC warm start, Q-bound overrides, target-tracking feedback providers. |
| [`ac_refinement`] | Canonical refinement runtime — retry grid, feedback-provider and commitment-probe extension points. |

None of these modules reference any specific market format. They take
primitives and build typed [`surge_dispatch`] structures.

## Format adapters

Format-specific *adapters* live in submodules. They read a data
source's field names, assemble calls to the canonical constructors
above, and present a clean entry point.

Today there is one production adapter:

- [`go_c3`] — GO Competition Challenge 3 adapter. Entry points
  [`go_c3::build_dispatch_request`], [`go_c3::export_go_c3_solution`],
  and [`go_c3::build_canonical_workflow`]. Sub-modules handle consumer
  block decomposition, HVDC link + reactive synthetic generators,
  penalty configuration, reserve catalog, AC OPF defaults, AC SCED
  setup (bandable-producer selection, reactive-support commitment
  schedule).

Writing a new adapter (ERCOT, MISO, CAISO, …) is the same shape:
translate the source's field names into calls to the canonical
modules, return a typed `DispatchRequest` (and optionally a
`MarketWorkflow`).

## Canonical two-stage workflow

The standard day-ahead recipe:

```rust
use surge_dispatch::DispatchModel;
use surge_market::{
    canonical_workflow::{canonical_two_stage_workflow, CanonicalWorkflowOptions},
    solve_market_workflow,
};

// `uc_request` is DC + Optimize commitment.
// `ed_request` is AC + placeholder Fixed commitment (the executor
// overrides it with stage-1's solved schedule at solve time).
let model = DispatchModel::prepare(&network)?;
let workflow = canonical_two_stage_workflow(
    model,
    uc_request,
    ed_request,
    CanonicalWorkflowOptions::default(),
);

let result = solve_market_workflow(&workflow)?;
let uc_solution = &result.stages[0].solution;
let ed_solution = &result.stages[1].solution;
```

The workflow executor applies these handoffs between stages:

- **Commitment handoff** — extract the solved commitment schedule from
  stage 1, pin it into stage 2 as a `CommitmentPolicy::Fixed`.
- **Dispatch pinning** — narrow stage 2's generator `p_min_mw`/`p_max_mw`
  profiles around stage 1's solved dispatch (configurable band).
- **AC SCED setup** — when a stage carries an `ac_sced_setup`,
  the executor applies reactive-reserve filtering, voltage-support
  commitment augmentation, bandable-subset producer pinning, and
  per-period AC warm start built from the source solution.

## AC SCED setup

The AC SCED stage needs considerable pre-solve enrichment beyond the
bare request out of an adapter's `build_dispatch_request`. Without
these pieces, the AC NLP starts from flat voltages and zero Q and
routinely runs out of iterations:

```rust
use surge_market::{
    AcScedSetup, AcWarmStartConfig, ProducerDispatchPinning,
};

let setup = AcScedSetup {
    source_stage_id: "scuc".to_string(),
    reactive_reserve_product_ids: Some(["q_res_up", "q_res_down"].into()),
    commitment_augmentation: voltage_support_schedules,
    dispatch_pinning: Some(ProducerDispatchPinning {
        // format-provided device classification
        producer_resource_ids,
        producer_static_resource_ids,
        // bandable subset — slack + top-Q-range selection
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

let stage = MarketStage::new(...).with_ac_sced_setup(setup);
```

## AC refinement runtime

For stages that need retries — different OPF attempts, different
NLP solvers, wider P bands when the default dispatch leaves too much
penalty — attach a [`RetryPolicy`]:

```rust
use surge_market::{RetryPolicy, OpfAttempt, BandAttempt};

let policy = RetryPolicy {
    relax_pmin_sweep: vec![false, true],
    opf_attempts: vec![
        OpfAttempt::new("soft", Some(soft_opf)),
        OpfAttempt::new("strict_bus_balance", Some(strict_opf)),
        OpfAttempt::new("no_thermal_limits", Some(no_thermal_opf)),
    ],
    nlp_solver_candidates: vec![None],
    band_attempts: vec![
        BandAttempt::default_band(),
        BandAttempt::wide_band_retry(0.35, 1.0, 1.0e9, 1_000_000_000),
    ],
    wide_band_penalty_threshold_dollars: 1.0e6,
    ..RetryPolicy::noop()
};

let stage = MarketStage::new(...).with_retry_policy(policy);
```

### Feedback providers

Feedback providers run before each retry attempt and can mutate the
request based on the prior stage's solution. Two canonical
implementations ship in `ac_reconcile`:

- [`DcReducedCostTargetTracking`] — reads LP bound shadow prices
  (`pg_lower:<rid>` / `pg_upper:<rid>`) from the source stage and
  derives asymmetric P-tracking penalties.
- [`LmpMarginalCostTargetTracking`] — derives penalties from the
  economic arbitrage `LMP_at_bus − marginal_cost(Pg)`. Works for any
  LP formulation where column duals may be noisy.

Both attach via `RetryPolicy::with_feedback` and take `Arc<dyn
FeedbackProvider>`. Caller-supplied overrides in the request win
over provider-computed ones.

### Commitment probes

Between-iteration probes can rewrite the request (e.g. the pmin=0
decommit probe that adjusts the source stage's commitment based on a
pmin-relaxed AC probe). Scaffolding is in place via the
`CommitmentProbe` trait and `RetryPolicy::with_commitment_probe`;
specific probe implementations are future work.

## Python bindings

The crate's canonical entry points are exposed through
[`surge-py`](../surge-py) under `surge.market` / `surge.market.go_c3`.
See [`surge.market.go_c3`](../surge-py/python/surge/market/go_c3.py)
for the one-call recipe:

```python
import surge.market.go_c3 as go_c3

problem = go_c3.load("scenario_911.json")
policy = go_c3.Policy(
    formulation="dc",
    ac_reconcile_mode="ac_dispatch",
    lp_solver="gurobi",
    nlp_solver="ipopt",
)
workflow = go_c3.build_workflow(problem, policy)
result = go_c3.solve_workflow(workflow, lp_solver=policy.lp_solver, nlp_solver=policy.nlp_solver)
dc = result["stages"][0]["solution"]
ac = result["stages"][-1]["solution"]
solution = go_c3.export(problem, ac, dc_reserve_source=dc)
go_c3.save(solution, "solution.json")
```

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

## Attribution

The canonical market formulation composed by this crate — reserve
products, multi-stage SCUC/SCED handoff, AC reconcile with reactive
reserve coupling, soft energy windows, linearized N-1 security — and
the `surge_market::go_c3` format adapter follow the problem
formulation published for the ARPA-E Grid Optimization (GO)
Competition Challenge 3. See [NOTICE](../../NOTICE) and the
[References](../../docs/references.md#market-dispatch-and-commitment)
page for the full citations (PNNL-35792 / arXiv:2411.12033).
