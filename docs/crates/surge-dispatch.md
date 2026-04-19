# surge-dispatch

`surge-dispatch` is the unified economic-dispatch and unit-commitment kernel.
It exposes one typed request surface that covers Security-Constrained
Economic Dispatch (SCED), Security-Constrained Unit Commitment (SCUC),
time-coupled multi-period dispatch, reliability commitment, AC redispatch,
and the SCED-AC Benders decomposition — all routed through the same
[`DispatchModel`] / [`DispatchRequest`] / [`solve_dispatch`] API.

Higher-level market-layer composition (multi-stage workflows, reserve
catalogues, format adapters) lives one crate up in
[surge-market](surge-market.md).

## Canonical API

Every study uses the same four steps:

1. Prepare a [`DispatchModel`] from a raw [`surge_network::Network`].
2. Build a typed [`DispatchRequest`] with the builder.
3. Optionally preflight it with [`DispatchModel::prepare_request`].
4. Call [`solve_dispatch`] (or [`DispatchModel::solve`]).

```rust
use surge_dispatch::{DispatchModel, DispatchRequest, DispatchTimeline};

let model = DispatchModel::prepare(&network)?;
let request = DispatchRequest::builder()
    .dc()
    .time_coupled()
    .all_committed()
    .timeline(DispatchTimeline::hourly(24))
    .build();

model.validate_request(&request)?;
let solution = model.solve(&request)?;
# Ok::<(), surge_dispatch::DispatchError>(())
```

The result is always a [`DispatchSolution`] with per-period detail in
[`DispatchPeriodResult`] and an exact [`ObjectiveBucket`] ledger that
reconciles to `total_cost`.

## Study Axes

The request model exposes three orthogonal choices. Every supported
study is a point in this three-dimensional grid.

### Formulation

| Value | Description |
|---|---|
| `Dc` | Linearized B-θ power balance. Solved as an LP/QP/MIP via HiGHS or Gurobi. Fast, approximate, suitable for commitment and energy-only dispatch. |
| `Ac` | Full polar AC power balance. Solved as an NLP via Ipopt/COPT. Exact, slower, required for bus-voltage-aware redispatch and reactive reserves. |

### Interval Coupling

| Value | Description |
|---|---|
| `PeriodByPeriod` | Solve intervals one at a time; thread state (storage SoC, prior dispatch) between solves. Parallelisable for AC SCED. |
| `TimeCoupled` | Solve all intervals as one optimisation. Required for SCUC, multi-period ramp constraints, energy windows, Benders SCED. |

### Commitment Policy

| Value | Description |
|---|---|
| `AllCommitted` | Every in-service generator is online for every period. Standard SCED baseline. |
| `Fixed(schedule)` | Commitment is provided exogenously (e.g. handed down from a prior SCUC stage). |
| `Optimize(options)` | Commitment is a MIP decision variable with min up/down time, startup tiers, startup-window caps. Standard SCUC. |
| `Additional { minimum_commitment, options }` | Day-ahead commitments are locked on; the solver can only *add* more units. Reliability commitment. |

### Common Points On The Grid

| Study | Axes |
|---|---|
| DC period-by-period dispatch | `Dc + PeriodByPeriod + AllCommitted` |
| DC time-coupled dispatch | `Dc + TimeCoupled + Fixed` |
| DC SCUC | `Dc + TimeCoupled + Optimize` |
| Reliability commitment | `Dc + TimeCoupled + Additional` |
| AC period-by-period dispatch | `Ac + PeriodByPeriod + AllCommitted` (or `Fixed`) |
| AC SCED with Benders cuts | `Ac + TimeCoupled + Fixed` with `sced_ac_benders` populated |

Attach an N-1 security policy to any DC time-coupled study via
[`DispatchRequestBuilder::security`] — see the
[Security Screening](#security-screening) section.

## Request Structure

[`DispatchRequest`] groups inputs into seven domains so a large study
configuration stays navigable:

| Field | Type | Purpose |
|---|---|---|
| `timeline` | `DispatchTimeline` | Period count and per-period interval widths. |
| `profiles` | `DispatchProfiles` | Load, renewable, derate, and generator dispatch-bound time series. |
| `state` | `DispatchState` | Initial SoC, prior dispatch, commitment initial conditions. |
| `market` | `DispatchMarket` | Reserve products, offer schedules, emissions, virtual bids, penalties, dispatchable loads. |
| `network` | `DispatchNetwork` | Thermal, flowgate, loss, ramp, topology, security policies. |
| `runtime` | `DispatchRuntime` | Process-local knobs — tolerances, warm starts, diagnostics, concurrency. |
| `commitment` | `CommitmentPolicy` | Policy kind + fixed schedule or MIP options. |

All seven domains have sensible defaults; callers populate only what
their study exercises.

### Timeline

```rust
DispatchTimeline::hourly(24)                           // 24 one-hour periods
DispatchTimeline::variable(vec![0.5, 0.5, 1.0, 2.0])   // 4 periods, variable widths
```

### Profiles

| Profile | What it is |
|---|---|
| `load` | Per-bus MW load profile (DC side). |
| `ac_bus_load` | Per-bus `(P, Q)` load profile (AC side). |
| `renewable` | Per-resource capacity-factor profiles (0–1). |
| `generator_derates` | Per-resource derate multipliers on `P_max`. |
| `generator_dispatch_bounds` | Per-period `[P_min, P_max]` (and optional `[Q_min, Q_max]`) overrides used for stage-to-stage pinning. |
| `branch_derates` | Per-branch per-period thermal rating multipliers (≥1 uprates, <1 derates). |
| `hvdc_derates` | Per-HVDC-link per-period derate multipliers. |

### Market

[`DispatchMarket`] is the home of everything that looks like a market
product: reserve products, zonal/system reserve requirements,
generator and dispatchable-load offer schedules, virtual bids,
emissions (`co2_cap_t`, `co2_price_per_t`, `emission_profile`,
`carbon_price`), carbon-aware tie-line limits, combined-cycle config
offers, must-run units, startup-window and energy-window caps, and the
power-balance penalty curve.

Generator cost approximation is controlled by
`GeneratorCostModeling`:

| Field | Type | Default | Description |
|---|---|---|---|
| `use_pwl_costs` | bool | `false` | Convert convex polynomial costs to a PWL epigraph (LP-compatible). |
| `pwl_cost_breakpoints` | int | `20` | Tangent-line breakpoints per approximated generator. |

### Network

[`DispatchNetwork`] carries physical-network-facing policies:

| Policy | Controls |
|---|---|
| `thermal_limits` | `enforce` + `min_rate_a` floor. |
| `flowgates` | `enabled` + `max_nomogram_iterations` for interface constraints. |
| `loss_factors` | Iterative DC-loss penalty-factor loop (`enabled`, `max_iterations`, `tolerance`). |
| `ramping` | Ramp constraint mode (`Averaged`, `Interpolated`, `Block`) and `Soft`/`Hard` enforcement. |
| `energy_windows` | Multi-interval energy budget `enforcement` + `penalty_per_puh`. |
| `forbidden_zones` | Forbidden operating zone enforcement. |
| `commitment_transitions` | Shutdown deloading and `InlineDeloading` / `OfflineTrajectory` modes. |
| `topology_control` | `Fixed` or `Switchable` branch topology (optimal transmission switching when `Switchable`). |
| `security` | Optional [`SecurityPolicy`] (see below). |
| `par_setpoints` | Phase-angle regulator MW setpoints excluded from B_bus. |
| `hvdc_links` | HVDC dispatch links (bands, fixed, or free). |
| `ph_head_curves` / `ph_mode_constraints` | Pumped-hydro head curves and mode-transition constraints. |

### Runtime

[`DispatchRuntime`] holds per-process knobs: solver tolerance, pricing
pass toggle, AC warm-start series, target-tracking penalties,
SCED-AC Benders cuts, model-diagnostic capture, and AC SCED period
concurrency. See [`DispatchSolveOptions`] for `Arc<dyn LpSolver>` /
`Arc<dyn NlpSolver>` overrides that must not be serialised.

## Reserve Products

Reserve markets ride on three [`DispatchMarket`] fields:

- `reserve_products` — product catalogue (`ReserveKind`,
  `ReserveDirection`, `QualificationRule`, deploy time, energy coupling).
- `system_reserve_requirements` — system-wide MW requirements per
  product per period with optional demand curves.
- `zonal_reserve_requirements` — zonal MW requirements with area-based
  scoping.

The canonical reserve catalogue (regulation-up/down, synchronous,
non-synchronous, ramping, reactive-headroom) and the canonical
requirement constructors (`from_load_fraction`, `from_largest_unit`,
`from_series`) live one crate up in
[surge-market's `reserves` module](surge-market.md#reserve-products).

## Security Screening

Attach a [`SecurityPolicy`] to DC time-coupled requests to add N-1
contingency protection:

| Field | Type | Default | Description |
|---|---|---|---|
| `embedding` | enum | `ExplicitContingencies` | `ExplicitContingencies` (build the full contingency constraint set) or `IterativeScreening` (cutting-plane loop). |
| `max_iterations` | int | `10` | Maximum outer-loop iterations for `IterativeScreening`. |
| `violation_tolerance_pu` | float | `0.01` | Post-contingency flow violation threshold. |
| `max_cuts_per_iteration` | int | `50` | Cap on new cuts per iteration. |
| `branch_contingencies` | list | `[]` | Branches to monitor (empty = all). |
| `hvdc_contingencies` | list | `[]` | HVDC links to consider as contingencies. |
| `preseed_count_per_period` | int | `0` | Top-N structural pairs to pre-seed iteration 0 with. |
| `preseed_method` | enum | `None` | `None` or `MaxLodfTopology` (PTDF-based severity ranking). |

## SCED-AC Benders

For AC-feasible time-coupled dispatch on large horizons, the DC SCED
can be augmented with AC-OPF subproblems that generate optimality cuts.
[`ScedAcBendersRuntime`] controls the loop:

- `eta_periods` — period indices where the master LP allocates an `η`
  epigraph variable.
- `cuts` — current cut pool keyed by period (externally managed or
  produced by the internal orchestrator).
- `orchestration` — when `Some(ScedAcBendersRunParams)`, the dispatch
  solver drives the full master/subproblem loop internally with trust
  regions, cut dedup, stagnation and oscillation detection, and
  per-period AC-OPF slack penalties.

The default parameters match the Python reference orchestrator so
cross-implementation comparison is straightforward. Orchestration only
applies to AC sequential horizons; DC and AC time-coupled solves
ignore it.

## HVDC

[`HvdcDispatchLink`] lets HVDC point-to-point links participate in
dispatch as fixed, banded, or fully free P variables. Co-dispatch with
generation is automatic for both DC and AC formulations.
[`DispatchRuntime::fixed_hvdc_dispatch`] pins a per-period HVDC
trajectory on the AC side — used by the AC refinement runtime to flip
or zero HVDC flow when the DC anchor traps the NLP.

## Results

[`DispatchSolution`] is ledger-first. Every persisted dollar amount is
either an exact [`ObjectiveTerm`] contribution or a derived rollup of
those exact terms. The serialised `audit` block ([`SolutionAuditReport`])
records whether the solution passed exact reconciliation and lists the
mismatch rows if it did not. Export paths should treat
`audit.audit_passed = false` as a failed final artefact rather than a
soft warning.

The top-level surface:

| Field | Description |
|---|---|
| `study` | [`DispatchStudy`] — formulation, coupling, commitment kind, period count, workflow stage metadata. |
| `summary` | [`DispatchSummary`] — horizon totals (`total_cost`, `total_energy_cost`, `total_reserve_cost`, `total_startup_cost`, `total_penalty_cost`, exact `objective_terms`). |
| `resources` / `buses` | Keyed catalogs with stable ids. |
| `periods` | Per-period [`DispatchPeriodResult`] — bus results (LMPs), resource results (dispatch, reserve awards, SoC), reserve results, constraint results, penalty summary, diagnostics. |
| `resource_summaries` | Horizon rollups keyed by resource id. |
| `audit` | [`SolutionAuditReport`] — reconciliation status and mismatch rows. |
| `diagnostics` | Optional [`ModelDiagnostic`] snapshots when `runtime.capture_model_diagnostics = true`. |

## Violations

[`assess_dispatch_violations`] produces a [`ViolationAssessment`] with
per-period bus balance, reactive balance, and branch-thermal violations
plus their implied [`ViolationCosts`]. This is the same structure used
by the GO C3 validator integration.

## Entry Points

| Function | Purpose |
|---|---|
| [`solve_dispatch`] | Solve a request against a prepared model. |
| [`solve_dispatch_with_options`] | Same, with process-local [`DispatchSolveOptions`] (solver overrides). |
| [`DispatchModel::prepare`] | Canonicalise generator ids + validate a network. |
| [`DispatchModel::prepare_request`] | Validate a request and return a [`PreparedDispatchRequest`]. |
| [`DispatchModel::solve_prepared`] | Solve a previously prepared request (avoids re-validation). |
| [`DispatchModel::validate_request`] | Preflight without solving. |

## Feature Flags

| Feature | Enables |
|---|---|
| `parquet` | Parquet export helpers in `datasets` via the `parquet` + `arrow-array` crates. |

No Cargo features are required for solver backends — LP, MIP, QP, and
NLP backends are discovered at runtime through `surge-opf`.

## Examples

### DC SCUC (24-period commitment optimisation)

```rust
use surge_dispatch::{
    CommitmentOptions, DispatchModel, DispatchRequest, DispatchTimeline,
};

let model = DispatchModel::prepare(&network)?;
let request = DispatchRequest::builder()
    .dc()
    .time_coupled()
    .optimize_commitment(CommitmentOptions {
        time_limit_secs: Some(600.0),
        mip_rel_gap: Some(1.0e-4),
        ..Default::default()
    })
    .timeline(DispatchTimeline::hourly(24))
    .build();

let solution = model.solve(&request)?;
assert_eq!(solution.study().periods, 24);
# Ok::<(), surge_dispatch::DispatchError>(())
```

### AC Period-By-Period Dispatch With Warm Start

```rust
use surge_dispatch::{
    DispatchModel, DispatchRequest, DispatchSolveOptions, DispatchTimeline,
};

let model = DispatchModel::prepare(&network)?;
let request = DispatchRequest::builder()
    .ac()
    .period_by_period()
    .all_committed()
    .timeline(DispatchTimeline::hourly(4))
    .update_runtime(|rt| rt.ac_sced_period_concurrency = Some(4))
    .build();

let options = DispatchSolveOptions {
    nlp_solver: Some(surge_opf::backends::ac_opf_nlp_solver_from_str("ipopt")?),
    ..Default::default()
};

let solution = model.solve_with_options(&request, &options)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### DC Time-Coupled With N-1 Screening

```rust
use surge_dispatch::{
    DispatchModel, DispatchRequest, DispatchTimeline, SecurityEmbedding,
    SecurityPolicy,
};

let policy = SecurityPolicy {
    embedding: SecurityEmbedding::IterativeScreening,
    max_iterations: 10,
    violation_tolerance_pu: 0.005,
    ..Default::default()
};

let request = DispatchRequest::builder()
    .dc()
    .time_coupled()
    .all_committed()
    .timeline(DispatchTimeline::hourly(12))
    .security(policy)
    .build();

let solution = DispatchModel::prepare(&network)?.solve(&request)?;
# Ok::<(), surge_dispatch::DispatchError>(())
```

## Related Docs

- [surge-market](surge-market.md) — multi-stage workflows, reserve
  catalogues, AC SCED setup, retry/refinement runtime, format adapters.
- [surge-opf](surge-opf.md) — the AC-OPF and DC-OPF kernels that
  `surge-dispatch` reuses for AC dispatch and Benders subproblems.
- [surge-network](surge-network.md) — domain model consumed by
  [`DispatchModel::prepare`].
- [surge-solution](surge-solution.md) — shared
  [`ObjectiveTerm`] / [`SolutionAuditReport`] contracts.
- [Data Model And Conventions](../data-model.md) — cost curve, rating,
  and offer conventions.
- [References](../references.md#market-dispatch-and-commitment) —
  PNNL-35792 and arXiv:2411.12033, the ARPA-E GO Competition Challenge
  3 problem formulation and analysis paper that the constraint set
  here implements.
