# Changelog

All notable changes to the public Surge release surface will be documented in
this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows Semantic Versioning intent.

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
