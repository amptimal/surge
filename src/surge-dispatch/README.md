# surge-dispatch

Unified dispatch and commitment workflows for Surge.

This crate exposes the typed dispatch request/result surface used for SCED,
time-coupled dispatch, commitment optimization, and related secure dispatch
workflows across the workspace.

## Public API

`DispatchModel::prepare` plus `solve_dispatch` is the canonical workflow.
Requests are described in terms of three explicit study axes:

- formulation: DC or AC
- coupling: period-by-period or time-coupled
- commitment policy: all committed, fixed, optimize, or additional

Results are keyed-first. The main output is [`DispatchSolution`], which carries:

- `resources` and `buses` catalogs with stable ids
- per-period `resource_results`, `bus_results`, `reserve_results`, and `constraint_results`
- keyed horizon summaries in `resource_summaries`
- exact `objective_terms` ledgers plus a persisted `audit` block that must reconcile to `total_cost`

```rust
use surge_dispatch::{
    DispatchModel, DispatchRequest, DispatchTimeline, solve_dispatch,
};

let model = DispatchModel::prepare(&network)?;
let request = DispatchRequest::builder()
    .dc()
    .period_by_period()
    .all_committed()
    .timeline(DispatchTimeline::hourly(1))
    .build();

model.validate_request(&request)?;

let solution = solve_dispatch(&model, &request)?;
let period = &solution.periods()[0];
let top_bus_lmp = period.bus_results()[0].lmp;
let first_resource = &period.resource_results()[0];
# Ok::<(), surge_dispatch::DispatchError>(())
```

The crate still contains raw solver-shaped internals for SCED and SCUC, but
those are implementation details rather than the product API.

## Audit Contract

`DispatchSolution` is now ledger-first. Every persisted dollar amount is either:

- an exact `objective_terms` contribution
- a derived rollup of those exact terms

The serialized `audit` block records whether the solution passed exact
reconciliation, whether any residual terms remain, and the concrete mismatch
rows when it did not. Export paths should treat `audit.audit_passed = false`
as a failed final artifact rather than a soft warning.

## Attribution

The variables, constraints, and objective structure implemented here
follow the problem formulation published for the ARPA-E Grid Optimization
(GO) Competition Challenge 3. See [NOTICE](../../NOTICE) and the
[References](../../docs/references.md#market-dispatch-and-commitment)
page for the full citations (PNNL-35792 / arXiv:2411.12033).
