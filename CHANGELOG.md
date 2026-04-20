# Changelog

All notable changes to the public Surge release surface will be documented in
this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows Semantic Versioning intent.

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
