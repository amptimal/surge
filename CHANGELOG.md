# Changelog

All notable changes to the public Surge release surface will be documented in
this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows Semantic Versioning intent.

## [0.1.9] — 2026-05-04

New **Datacenter Operator** market — behind-the-meter SCUC for a
microgridded datacenter — and three SCUC primitives that support it.

### Added (`markets`, `dashboards`)

- `markets/datacenter/` + `dashboards/datacenter/`: optimises
  commitment + dispatch + AS awards across IT-load tiers (must-serve +
  curtailable VOLL), BESS, solar, wind, fuel cell, gas CT, diesel,
  optional must-run nuclear, and (optional) coincident-peak
  transmission charges, against an exogenous LMP forecast and AS price
  forecasts. Forecasts and asset specs editable live; re-solves in
  seconds.

### Added (`surge-dispatch`)

- `peak_demand_charges`: SCUC primitive for coincident-peak demand
  charges (e.g. ERCOT 4-CP). Adds an auxiliary `peak_mw` variable
  bounded below by the resource's dispatch on the flagged periods,
  with a linear `charge_per_mw × peak_mw` objective term.

### Added (`surge.market`)

- `generator_dispatch_bounds`: pin a resource's per-period dispatch
  window directly in MW (set `p_min == p_max` for must-take fixed
  output).
- `must_run_units`: force `u[t]=1` for the listed resources. Paired
  with `generator_dispatch_bounds` this removes both commitment and
  dispatch freedom — the canonical pin for baseload nuclear /
  must-take PPAs / fixed-output IPP contracts.
- `ECRS` canonical reserve product.

### Changed (defaults)

- `markets.go_c3` default `lp_solver` flipped from `gurobi` → `highs`.
  Default runs no longer require a commercial license.

## [0.1.8] — 2026-05-01

`surge-dispatch` SCUC security loop: lower memory, faster solves, and
adjoint loss sensitivities. Default policy retuned.

### Added (`surge-dispatch`)

- Lazy PTDF caching on the SCUC security path — caches build only
  when a contingency actually binds, rather than eagerly per period.
- Adjoint-based DC loss sensitivities replace the explicit per-branch
  Jacobian build in `surge-opf`/`surge-dispatch`, simplifying the
  loss-factor pipeline.

### Fixed

- `surge-dispatch`: SCUC security loop now defers early exit by one
  iteration when sys-row loss treatment is active but no solve has
  yet consumed realized loss factors. Previously both
  `ScalarFeedback` and `PenaltyFactors` could silently no-op on
  scenarios with a clean contingency profile.

### Performance (`surge-dispatch`)

- Lower SCUC security memory footprint at scale: per-period state is
  released after use rather than retained for the full horizon.
- Faster security wall time via tightened PTDF tolerance, lazy cache
  construction, and adapter-side request reuse.

### Changed (defaults)

- `scuc_loss_treatment` defaulted to `penalty_factors` (was
  `scalar_feedback`), paired with the security-loop fix above.
- `scuc_thermal_penalty_multiplier` defaulted to `1.25` (was `10.0`)
  to keep SCUC thermal penalties closer to the configured slack rate.

## [0.1.7] — 2026-04-26

`surge-dispatch` polish release: end-to-end shadow prices, Q-LMPs,
faster N-1 screening, and SCUC correctness fixes. New in-process
Rust→Python tracing broadcast on `surge-py`. GO C3 defaults updated.

### Added (`surge-dispatch`)

- Per-branch and per-contingency shadow prices flow from AC SCED
  through to `BranchThermal` constraint results; security-loop
  flowgates retain their `N1_t{period}_…` names. SCUC pricing
  extraction emits per-constraint duals whenever the dual vector is
  full-length, while LMPs still gate on optimal pricing-LP status.
- Per-bus Q-LMP on AC SCED dispatch results.
- Per-iteration security SCUC timings in `run-report.json`.
- Optional per-iteration scalar loss-feedback pass in SCUC.

### Added (`surge-py`)

- In-process Rust→Python tracing broadcast (replaces fd-tee, which
  deadlocked under load); typed `.pyi` stubs.

### Fixed

- `surge-dispatch`: PTDF-form security cuts now bind dispatch when
  `scuc_disable_bus_power_balance=true` (previously absorbed by free
  per-bus slacks).
- `surge-dispatch`: AC SCED no-storage path now threads `dt_hours`
  (was hard-coded to 1 h, miscosting sub-hourly markets).
- `surge-dispatch`: SCUC PF system-row RHS sign — drop the loss
  double-count that hung AC SCED on loss-feedback runs.
- `surge-dispatch`: sparse-aware reserve extraction preserves
  storage SoC coupling across the SCUC/SCED handoff.
- `surge-dispatch`: zonal / system reserve duals preserved through
  the pricing LP so AS clearing prices match.
- `surge-opf`: AC-OPF Ipopt `constr_viol_tol` bound to `tol` so
  unscaled bus balance tracks the requested tolerance.

### Performance (`surge-dispatch`)

- ~45–65× faster N-1 security screening: per-period parallelism via
  `rayon` and flat per-branch state in `HourlySecurityContext`
  remove the inner-loop `HashMap` rebuild on 6049-bus and larger.
- 16× sparser PTDF security cuts on the
  `scuc_disable_bus_power_balance` path: active-period gating moved
  above PTDF row construction, `bus_load_p_mw_with_map` hoisted out
  of the per-row loop, per-row `HashMap` allocation eliminated.
  617-bus D1 SCUC: 118 M NZ / 20.9 s → ~7 M / sub-second.

### Changed

- GO C3 exporter: consumer reserve shedding
  (`ExportOptions::allow_consumer_reserve_shedding`, default on)
  caps per-consumer up/down reserve awards to the available room
  after `ac_dispatch` curtailment, fixing spurious validator
  `viol_cs_t_p_on_*` flags on physically valid solutions.
- GO C3 defaults: `scuc_loss_treatment="scalar_feedback"` (was
  static), `scuc_security_preseed_count_per_period=0` (was 1000).

### Build

- Dashboard Docker image installs `zstandard` for compressed
  network blob round-trips.

Dashboards (`dashboards/rto`, `dashboards/battery`) saw substantial
work this cycle but are not part of the published release surface.

## [0.1.6] — 2026-04-24

`surge-dispatch` release: SCUC LP tightening, large-network
performance, and a fix for unbounded thermal-slack relaxations. GO C3
adapter gains diagnostic knobs for isolating large-network bottlenecks.

### Fixed

- **Bounded thermal-slack relaxation in SCUC.** Per-branch thermal
  slack columns (`branch_lower_slack`, `branch_upper_slack`) were
  allocated with `col_upper = +∞`, letting the LP relaxation
  hallucinate unbounded virtual capacity on degenerate networks.
  `col_upper` is now capped at 10× rating; slack rates are unchanged,
  so the economic tradeoff is preserved. On 1576-bus D1 s003 this
  takes the SCUC MIP from `time_limit` at 3637s with a −$1.7e14 dual
  bound to `optimal` at 66s with a 1.92% gap; commitment decisions
  unchanged.

### Added

- **Sparse reserve-product participation in SCUC.** Reserve LP columns
  are now emitted only for `(product, resource)` pairs that can
  qualify under some commitment state AND have a nonzero offer
  capacity in some period. Applies to both generators and
  dispatchable loads. On 617-bus D2 the pre-presolve LP shrinks by
  ~97k columns.
- **Consumer-level DL reserve aggregation.** Dispatchable loads that
  share a `reserve_group` (the GO C3 pattern of price-decomposed
  consumer blocks) now share a single reserve variable per product,
  bounded by total offer and coupled to total served. Removes a
  spurious per-block pro-rata constraint that was over-restricting
  consumer reserve when block served-levels were uneven.
- **Sparse reserve row families.** Cross-headroom / cross-footroom,
  shared-limit, and energy-coupling rows are now emitted only for
  participating resources. On 73-bus D3 s303 pre-presolve rows drop
  37% and nonzeros 17%.
- **SW0 branch-binary strip.** When `allow_branch_switching=false`,
  `branch_commitment`/`startup`/`shutdown` columns and their
  state-evolution rows are omitted from the LP entirely instead of
  allocated and pinned. On 617-bus D2 this removes ~123k cols and
  ~82k rows up-front.
- **GO C3 SCUC diagnostic knobs** on `GoC3Policy` / `DispatchRuntime`:
  - `scuc_disable_bus_power_balance` — drop per-bus KCL rows and
    `pb_*` slack cols; replace with a single system-balance row plus
    a post-solve DC-PF theta repair before N-1 screening. Defaults to
    `true` for GO C3. On 6049-bus D1 s015 this takes SCUC from
    unsolved at 300s to optimal in 8s.
  - `scuc_copperplate` — zero the power-balance penalty so per-bus
    rows become trivially satisfied via free slack (for isolating
    whether MIP cost lives in UC or in network coupling).
  - `scuc_firm_bus_balance_slacks`, `scuc_firm_branch_thermal_slacks`,
    `disable_scuc_thermal_limits` — per-family slack-firming probes.

### Performance (`surge-dispatch`)

- **O(N²) hoists in `attach_keyed_period_views`.** Branch / flowgate
  shadow-price lookups and zonal reserve participant matching no
  longer re-scan the network per period. On 4224-bus D1 s014 this
  function drops from 108s to sub-second.
- **Hoisted `network.bus_index_map()` rebuilds** out of the
  `build_capacity_logic_reserve_rows` zone loops. Per-product,
  per-period cost drops ~1000× on 4224-bus D1 s014.
- **Cached zonal participant sets** on `ActiveZonalRequirement`,
  eliminating an O(N_bus) HashMap rebuild per DL per zonal
  requirement per period in SCUC bounds construction.

## [0.1.5] — 2026-04-22

Python-side release: agent / MCP integration helpers, a PyPSA netCDF
bridge, and accessor consistency fixes. No Rust crate API changes.

### Added

- **Agent-friendly MCP helpers on `surge-py`.** `.to_dict()` now exists
  on every solver result (`AcPfResult`, `DcPfResult`, `DcOpfResult`,
  `ScopfResult`, `AcOpfHvdcResult`, `ContingencyAnalysis`,
  `AcAtcResult`, `PtdfResult`, `LodfResult`, `LodfMatrixResult`,
  `OtdfResult`, and the nested contingency / screening types) so MCP
  hosts and tool-calling agents can serialize results with one call.
  Matrix results accept `format={"summary","sparse","full"}` and a
  `top_k_per_branch` knob.
- **Network convenience accessors.** `Network.summary()`,
  `Network.loads_dataframe()`, `Network.shunts_dataframe()` round out
  the existing generator / bus / branch DataFrame surface.
- **Built-in case helpers.** `surge.list_builtin_cases()` and
  `surge.load_builtin_case(name)` enumerate the packaged IEEE / market
  cases by string name. `surge.builtin_case_rated_flags()` reports
  which built-ins ship with branch thermal ratings (relevant for
  transfer-capability studies).
- **Explicit format override on load.** `surge.load(path, format=...)`
  and the `surge.load_network` alias let MCP hosts pass an explicit
  format when the extension is ambiguous or missing.
- **PyPSA netCDF bridge.** New `surge.io.pypsa_nc.load(path)` reads
  PyPSA netCDF directly into a Surge `Network`, preserving per-bus
  `v_mag_pu_set` that the MATPOWER round-trip path cannot always
  carry through. Requires the optional `pypsa` package.
- **Format interop guide.** New [`docs/format-interop.md`](docs/format-interop.md)
  documents per-format round-trip caveats and when to prefer the
  PyPSA bridge over a MATPOWER hop.

### Changed

- **Accessor consistency on `AcPfResult`.** `branch_apparent_power`
  and `branch_loading_pct` are now properties, matching the rest of
  the result surface. **Breaking for callers using `()`** — drop the
  parentheses: `result.branch_loading_pct` (not
  `result.branch_loading_pct()`).
- **Branch type auto-detection on `Network.add_branch`.** An
  off-nominal tap (|tap − 1| > 1e-6) or non-zero phase shift now
  tags the branch as a `Transformer`, matching the MATPOWER reader
  convention. Previously Python-built networks landed as `Line`
  regardless.
- **Strict-JSON-safe matrix serialization.** PTDF / LODF / OTDF
  `to_dict` now filters non-finite entries (NaN / ±∞ from radial /
  islanding outages) from `nnz`, `max_abs`, and top-k lists, surfaces
  `nan_count` / `inf_count` separately, and emits Python `None` for
  non-finite cells in `format="full"` so the payload round-trips
  through strict JSON encoders.

### Build

- **Docker image builds HiGHS 1.14.0 from source** rather than relying
  on the Debian `libhighs-dev` package, which lagged behind the
  workspace's vendored HiGHS.

## [0.1.4] — 2026-04-20

### Added

- **Dispatch observability.** `DispatchSolution` now carries stage-failure
  diagnostics and per-period AC-OPF statistics, making multi-stage workflow
  failures and AC-SCED iteration costs inspectable without re-running the
  solve.
- **SCUC loss-factor warm start** and **per-period load-pattern sensitivity**
  in `surge-dispatch` SCUC — reduces MIP root relaxation time on
  loss-aware formulations. Loss-factor coefficient writes now use a 1e-4
  cutoff to keep the LP sparse.
- **Flowgate directional slack** on `surge-network::Flowgate` — lets
  flowgate limits be relaxed in one direction without disabling the
  constraint.
- **HiGHS MIP trace.** `MipTrace` is now populated unconditionally on every
  HiGHS MIP solve (previously gated); primal bound recovery falls back to
  `objective_function_value` when `mip_primal_bound` is NaN.

### Fixed

- GO C3 adapter: new market-extras fields are now forwarded into
  `run-report.json`.

## [0.1.3] — 2026-04-19

### Changed

- **HiGHS MIP backend tuning.** Presolve is now left on by default even when a
  primal-start hint is supplied (previously forced off). `simplex_scale_strategy=4`
  is now the default for MIP LP solves. HiGHS MIP verbose logging is now gated by
  `SURGE_HIGHS_VERBOSE` (previously LP/QP only).

### Added

- `SURGE_HIGHS_*` environment variables for tuning HiGHS without a rebuild:
  `THREADS`, `PARALLEL`, `RANDOM_SEED`, `SIMPLEX_STRAT`, `SCALE_STRAT`,
  `PRIMAL_FEAS_TOL`, `DUAL_FEAS_TOL`, `CROSSOVER`, `MIP_HEURISTIC`,
  `MIP_DETECT_SYM`, `MIP_FEAS_TOL`, `MIP_REL_GAP`.

## [0.1.2] — 2026-04-18

### Added

#### New crates

- `surge-dispatch` — unified economic dispatch and unit commitment kernel.
  Typed `DispatchModel` / `DispatchRequest` / `solve_dispatch` API covers DC
  and AC SCED, DC SCUC, time-coupled multi-period dispatch, reliability
  commitment, AC redispatch, and SCED-AC Benders decomposition through one
  request surface with three orthogonal study axes (Formulation × Interval
  Coupling × Commitment Policy). Includes reserve-product modeling, N-1
  security screening (explicit contingencies or iterative screening),
  HVDC co-dispatch, emissions and carbon pricing, and a ledger-first
  `DispatchSolution` with an exact `ObjectiveTerm` audit.
- `surge-market` — canonical market-formulation layer on top of
  `surge-dispatch`. Provides standard reserve-product constructors
  (regulation, synchronized, non-synchronized, ramping, reactive
  headroom) and zonal-requirement builders, commitment helpers,
  piecewise offer-curve construction, per-bus load aggregation,
  startup/shutdown trajectory derivation, and time-window translators.
  Adds a typed multi-stage workflow runner (`MarketStage`,
  `MarketWorkflow`, `solve_market_workflow`) with commitment handoff
  and dispatch pinning, the canonical two-stage DC SCUC → AC SCED
  workflow, the AC SCED setup combinator (reactive-reserve filter,
  commitment augmentation, bandable-subset producer pinning, AC warm
  start, Q-bound overrides), and the AC refinement runtime
  (`RetryPolicy` nested grid of OPF / band / NLP / HVDC attempts with
  feedback providers and commitment probes). Includes the GO Competition
  Challenge 3 format adapter as the reference implementation.

#### Python

- New `surge.dispatch` namespace exposing the canonical dispatch API —
  `DispatchRequest`, `DispatchSolution`, study-axis enums, timeline
  helpers, and reserve/market/network configuration builders.
- New `surge.market` namespace with `MarketConfig`, `MarketWorkflow`,
  `WorkflowRunner`, `run_market_solve`, reserve catalog constants,
  penalty-curve builders, AC reconciliation helpers, and
  violation-assessment utilities.
- New `surge.market.go_c3` namespace with a one-call `load` /
  `build_workflow` / `solve_workflow` / `export` / `save` recipe for the
  GO C3 adapter.
- Typed `.pyi` stubs for dispatch and market namespaces; `surge.opf`
  namespace module added.
- New `solve_sced` binding.

#### Optimization (`surge-opf`)

- AC-OPF Benders subproblem support that produces the optimality cuts
  consumed by `surge-dispatch`'s SCED-AC Benders loop.
- Canonical reactive-reserve modeling in AC-OPF with per-product
  headroom/footroom constraints and deliverability caps.
- HVDC co-optimization inside AC-OPF, including converter-terminal Q
  constraints and per-link dispatch bands.
- Generator P-Q capability curves, piecewise cost epigraph support, and
  improved tap / phase-shifter / switched-shunt / SVC / TCSC handling.
- Pre-solve model-reduction backend (`backends::reduce`) that removes
  bound-implied-zero columns and duplicate rows before handing the LP
  to the chosen backend.
- Canonical MIP gap schedule / progress monitor API so commitment
  solves can target time-varying gap thresholds.
- Expanded Gurobi and HiGHS backend coverage (MIP callbacks, incumbent
  tracking, Benders-compatible LP resolves); improved COPT backend for
  AC-OPF NLPs.
- AC-OPF result envelope now carries the full objective-ledger audit,
  and the `surge-solve` CLI fails closed when the ledger audit fails.

#### Network Model (`surge-network`)

- First-class `DispatchableLoad` with offer schedules and reserve
  participation.
- Reserve market primitives: `ReserveProduct`, `ReserveDirection`,
  `ReserveKind`, `QualificationRule`, `EnergyCoupling`,
  `ZonalReserveRequirement`, `SystemReserveRequirement`.
- Generator extensions for reserve capability (regulation, spinning,
  non-spinning) and startup tiers keyed by offline hours.
- Flowgate and interface refinements, penalty-curve types, and power-
  balance penalty configuration.

#### Shared Solution Types (`surge-solution`)

- New `economics` module defining the exact `ObjectiveBucket` /
  `ObjectiveTerm` / `ObjectiveLedgerMismatch` / `SolutionAuditReport`
  contracts used by `surge-dispatch` for ledger-first cost reporting.
- New `ids` module with canonical resource-id helpers
  (`generator_resource_id`, `dispatchable_load_resource_id`,
  `combined_cycle_plant_id`, `default_machine_id`).

#### CLI (`surge-bindings`)

- New `refresh_activsg_psse` helper binary for regenerating the
  ACTIVSg2000 case from upstream PSS/E data used in the dispatch
  tutorial.

#### Documentation

- New per-crate docs: [`surge-dispatch`](docs/crates/surge-dispatch.md)
  and [`surge-market`](docs/crates/surge-market.md).
- New Tutorial 12 — DC dispatch on ACTIVSg with LMP heat maps, with a
  companion notebook.
- Expanded generated Python namespace surface to include `surge.dispatch`,
  `surge.market`, and `surge.market.go_c3`.
- Refreshed architecture, support matrix, crate index, and release
  process to cover the new dispatch and market crates.

### Changed

- `surge-bindings` binaries published to crates.io now include the new
  objective-ledger audit enforcement on AC-OPF outputs.
- Workspace member count updated — `surge-dispatch` and `surge-market`
  join the crates.io publication list immediately before
  `surge-bindings`.

## [0.1.1] — 2026-03-31

### Fixed

- Corrected several DC-SCOPF issues affecting angle-limit handling, HVDC and
  MTDC power balance, piecewise-linear cost passthrough, corrective Hessian
  sizing, loss-factor outputs, and HVDC contingency accounting.

### Added

- Added co-optimized variable HVDC dispatch, PAR scheduled-interchange
  treatment, soft generator limits, iterative loss-factor support, and the
  related CLI and Python SCOPF options.

### Changed

- DC-SCOPF now defaults to LP costs in Rust and Python for more robust HiGHS
  behavior on large cases.
- `surge-bindings` is now published on crates.io, and installation guidance now
  leads with `cargo install surge-bindings` and `pip install surge-py`.

### Documentation

- Refreshed the quickstart, support matrix, SCOPF tutorial, CLI reference,
  notebook, and crate docs to match the new defaults and release packaging.

## [0.1.0] — 2026-03-29

Initial public release of the Surge power systems analysis engine.

### Power Flow (`surge-ac`, `surge-dc`)

- AC Newton-Raphson solver with sparse KLU factorization and reactive power
  limit enforcement.
- AC Newton-Raphson warm-start variant with DC-initialized voltage angles.
- Fast Decoupled Power Flow (FDPF) with B-prime / B-double-prime splitting.
- DC power flow (B-theta) with sparse KLU factorization.
- Linear sensitivity matrices: PTDF, LODF, OTDF, BLDF, GSF, and N-2 LODF.

### HVDC (`surge-hvdc`)

- LCC and VSC HVDC link modeling with sequential, block-coupled, and hybrid
  AC/DC iteration strategies.
- Multi-terminal DC (MTDC) network solver with converter loss modeling.

### Security and Contingency (`surge-contingency`)

- N-1 branch and generator contingency analysis with parallel execution via
  rayon.
- N-2 branch-pair contingency analysis.
- LODF-based fast screening with configurable thresholds.
- P4 and P6 post-contingency post-dispatch workflow support.
- Local voltage-stress screening for voltage stability assessment.
- Corrective action modeling with topology and redispatch remediation.

### Optimization (`surge-opf`)

- DC-OPF via sparse B-theta formulation with LMP extraction from power balance
  duals.
- AC-OPF via Ipopt NLP with exact analytical Hessian and LMP decomposition
  (energy, congestion, loss components).
- Security-Constrained OPF (SCOPF) with iterative constraint generation
  (cutting-plane) and penalty slack formulation.
- Optimal Transmission Switching (OTS) and Optimal Reactive Power Dispatch
  (ORPD).
- SOCP and SDP relaxation workflows.
- Pluggable solver backends: HiGHS (bundled), Gurobi, COPT, CPLEX (runtime
  detected), Ipopt (link-time).

### Transfer (`surge-transfer`)

- NERC-style ATC (Available Transfer Capability) workflows.
- AFC (Available Flowgate Capability) and multi-transfer studies via DFAX.
- AC transfer capability with thermal, voltage, and transient stability limits.
- TPL-001-5.1 compliance report generation (P1-P7 categories).

### Network Model (`surge-network`)

- Comprehensive power system network model: buses, branches, generators,
  loads, shunts, HVDC links, transformers (2- and 3-winding), switched shunts,
  FACTS devices, storage (unified as generators with `StorageParams`).
- Area, zone, and owner metadata for regional analysis.
- Contingency definition with branch, generator, and HVDC outage types plus
  modification actions (tap, load, generation, shunt adjustments).
- Flowgate and interface constraint definitions.
- Versioned native JSON schema (`surge-network-json` v0.1.0) with Zstandard
  compression and compact binary variants.

### Topology (`surge-topology`)

- Node-breaker to bus-branch topology projection.
- Island detection and connectivity analysis.
- Topology rebuild workflows for retained switching studies.

### File Formats (`surge-io`)

- MATPOWER `.m` reader and writer.
- PSS/E RAW reader and writer (v30-v36) with RAWX support.
- PSS/E DYR dynamics data reader (130+ model types).
- PSS/E sequence data reader (zero/positive/negative sequence impedances).
- CGMES 2.4.15 and CGMES 3.0 (CIM100) reader (29 import waves).
- XIIDM (PowSyBl) reader and writer including phase tap changers.
- UCTE `.uct` reader.
- IEEE Common Data Format (CDF) reader.
- OpenDSS `.dss` reader for 3-phase distribution models.
- COMTRADE reader for oscillography data.
- Surge native JSON, compressed JSON (Zstandard), and binary format
  reader/writer.

### Sparse Infrastructure (`surge-sparse`)

- Compressed Sparse Column (CSC) matrix with COO-to-CSC assembly.
- KLU sparse LU factorization with symbolic reuse and numeric refactor.
- Complex KLU solver for Y-bus admittance matrix operations.

### Solution Types (`surge-solution`)

- Shared result contracts for power flow and OPF outputs.
- Replay-friendly solved state snapshots.

### CLI (`surge-bindings`)

- `surge-solve` binary with solver methods: `acpf`, `acpf-warm`, `fdpf`,
  `dcpf`, `dc-opf`, `ac-opf`, `socp-opf`, `scopf`, `ots`, `orpd`,
  `contingency`, `n-2`, `hvdc`, `injection-capability`, `nerc-atc`.
- Format auto-detection from file extension.
- JSON, text, and binary output modes.
- Solver backend selection via `--solver`.

### Python Bindings (`surge-py`)

- `surge` Python package with typed stubs (`.pyi`) and `py.typed` marker.
- Root-level entry points: `solve_ac_pf`, `solve_dc_pf`, `solve_dc_opf`,
  `solve_ac_opf`, `solve_scopf`, `analyze_n1_branch`,
  `analyze_n1_generator`, `analyze_n2_branch`, `load`, `save`.
- Namespaced APIs: `surge.powerflow`, `surge.optimization`,
  `surge.contingency`, `surge.transfer`, `surge.dc`, `surge.io`, `surge.batch`.
- NumPy interop for voltage, angle, and sensitivity arrays.
- Parameter sweep with parallel scenario execution.
- Custom exception hierarchy (`SurgeError` base with solver-specific
  subclasses).
- Python 3.10 through 3.14 support.

### Packaging and Build

- Rust workspace with 13 member crates, edition 2024, MSRV 1.87.
- Vendored HiGHS 1.13.1 for reproducible LP/QP builds.
- SuiteSparse/KLU linked for sparse factorization.
- Release profile: `opt-level=3`, fat LTO, single codegen unit.
- Generic public wheel builds: Linux x86_64, Linux aarch64, macOS aarch64,
  Windows x86_64.
- Optional targeted GitHub Release wheel artifacts for `x86-64-v4`.
- PolyForm Noncommercial 1.0.0 license with commercial dual-license option.

### Documentation

- 8 user tutorials with Jupyter notebook companions.
- 15 per-crate reference guides.
- Architecture, validation evidence, and method fidelity documentation.
- CLI reference, Python API surface guide, and quickstart.
- Packaged example cases (IEEE 118-bus, ACTIVSg10k, pglib cases) in native
  format with provenance records.
