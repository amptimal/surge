# Surge Architecture

This document is a high-level map of the current workspace. If this summary and
the manifests disagree, use the root `Cargo.toml` and the crate manifests.

## Workspace Shape

A useful mental model is:

- Foundation: `surge-network`, `surge-solution`, `surge-sparse`, `surge-io`, `surge-topology`
- Steady-state and transfer solvers: `surge-dc`, `surge-ac`, `surge-transfer`, `surge-hvdc`
- Optimization: `surge-opf`
- Security: `surge-contingency`
- Interfaces: `surge-bindings`, `surge-py`

## Core Dependency Rules

- `surge-network` is the shared domain-model layer.
- `surge-solution` is the shared result and replay layer.
- `surge-sparse` is a utility crate and is not the universal solver base; several solver crates also use their own direct numerical dependencies.
- `surge-io` handles parse and write boundaries and depends on `surge-network` and `surge-topology`.
- Solver crates generally depend downward into shared model or lower-level solver crates; interface crates sit at the leaves.

## Interface Crates

### `surge-bindings`

`surge-bindings` produces the `surge-solve` binary and a small supporting `rlib`. Its current direct workspace dependencies are:

- `surge-network`
- `surge-solution`
- `surge-dc`
- `surge-ac`
- `surge-io`
- `surge-contingency`
- `surge-opf`
- `surge-hvdc`
- `surge-transfer`

The CLI contract is defined in `src/surge-bindings/src/main.rs` and should be treated as authoritative over secondary prose.

### `surge-py`

`surge-py` builds the native `_surge` extension consumed by the `surge` Python package wrapper. Its current direct workspace dependencies are:

- `surge-network`
- `surge-solution`
- `surge-dc`
- `surge-ac`
- `surge-io`
- `surge-contingency`
- `surge-opf`
- `surge-hvdc`
- `surge-topology`
- `surge-transfer`

The Python contract is defined by the binding source in `src/surge-py/src/`
and the package-level stubs in `src/surge-py/python/surge/`, especially
`__init__.pyi`, `io/__init__.pyi`, and `io/psse/*.pyi`.

## Typical Data Flow

1. Parse or construct a `surge_network::Network`.
2. Run one or more analysis crates against that model.
3. Surface results through `surge-solution` contracts, solver-specific outputs, Python, or CLI interfaces.
4. Optionally export the network or derived artifacts through `surge-io` or interface-specific helpers.

## Structural Caveats

- The workspace is not a strict single-stack pipeline; many analysis crates share the same model layer and compose with each other selectively.
- Public docs should use the published Python names and study entry points such as `solve_ac_pf`, `solve_dc_pf`, `surge.dc.prepare_study`, `surge.transfer.prepare_transfer_study`, `surge.transfer.compute_nerc_atc`, and `surge.contingency.n1_branch_study`.


## How To Re-Verify This Document

Use these commands when auditing future drift:

```bash
git grep -n '^members = \\[$' Cargo.toml
cargo metadata --no-deps
cargo tree -p surge-bindings
cargo tree -p surge-py
```

Those outputs should be preferred over hand-maintained counts or diagrams.
