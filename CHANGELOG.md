# Changelog

All notable changes to the public Surge release surface will be documented in
this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows Semantic Versioning intent.

## [0.1.1] — 2026-03-31

### Fixed

- **SCOPF angle constraints** — angle-difference constraints now use penalty
  slack variables instead of hard constraints, fixing infeasibility on cases
  with tight angle limits (e.g. ACTIVSg2000).
- **SCOPF HVDC power balance** — fixed HVDC and MTDC grid injections are now
  included in the power balance RHS for both preventive and corrective modes.
  Previously omitted, causing incorrect dispatch on networks with DC links.
- **SCOPF PWL cost passthrough** — `use_pwl_costs` and
  `quadratic_pwl_local_indices` now read from `DcOpfOptions` instead of being
  hardcoded to `false`.
- **Corrective mode Hessian** — Hessian column count updated to account for
  HVDC and generator-limit slack variables.
- **SCOPF loss factor results** — `total_losses_mw` and `lmp_loss` are now
  computed from the final theta solution when loss factors are active.
  Previously hardcoded to zero.
- **SCOPF contingency count** — `total_contingencies_evaluated` now includes
  HVDC contingencies.

### Added

- **SCOPF PAR setpoints** — PAR branches are excluded from the B-bus matrix
  and replaced by scheduled-interchange injections, matching DC-OPF behavior.
- **SCOPF variable HVDC dispatch** — `DcOpfOptions::hvdc_links` with variable
  P_dc bounds adds co-optimized HVDC decision variables with linear loss
  modeling.
- **SCOPF generator limit slacks** — soft Pmin/Pmax constraints via
  `--gen-limit-penalty` (CLI) or `DcOpfOptions(generator_limit_mode=Soft)`.
- **SCOPF loss factor iteration** — iterative loss compensation wrapping the
  cutting-plane loop, enabled via `--use-loss-factors` (CLI) or
  `DcOpfOptions(loss_model=Iterative)`. Available in preventive and
  corrective DC-SCOPF.
- **`--no-angle-limits`** CLI flag and `enforce_angle_limits` option to disable
  SCOPF angle-difference constraints entirely.
- **`--gen-limit-penalty`**, **`--use-loss-factors`**, **`--loss-iterations`**,
  **`--loss-tolerance`** CLI flags for DC-OPF and SCOPF.
- **SCOPF defaults to LP costs** — PWL (piecewise-linear) cost formulation is
  now the default for DC-SCOPF in both Rust and Python, avoiding HiGHS QP
  numerical issues on large cases.
- **Gurobi pip discovery** — `gurobipy/.libs/` in Python site-packages is now
  searched when looking for `libgurobi130.so`.
- **Python `ScopfOptions.cost_model` and `ScopfOptions.dc_opf`** fields for
  selecting the SCOPF cost formulation and passing DC sub-options such as
  gen-limit penalty, loss factors, and PWL breakpoints through to SCOPF.

### Changed

- **README quick start** — Python section now leads with `pip install surge-py`
  instead of build-from-source. CLI section includes Rust installation
  instructions.
- **Solver error messages** — HiGHS and Ipopt "not found" errors now
  explicitly note that `pip install highspy` / `pip install cyipopt` do not
  provide the C shared libraries Surge needs.
- **`surge-bindings` now published to crates.io** — users can install the
  CLI via `cargo install surge-bindings`.

### Documentation

- Added pip shared library warnings to quickstart, support-compatibility, and
  surge-py README.
- Updated SCOPF tutorial, CLI reference, notebook, and surge-opf crate docs
  with new options and defaults.

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
