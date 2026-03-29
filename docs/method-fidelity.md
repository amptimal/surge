# Method Fidelity

This page explains how Surge uses method labels in public documentation. The
goal is simple: when Surge says a workflow is a full solve, an approximation, or
a screening method, those terms should mean something consistent.

## Method Labels

| Label | Meaning |
|---|---|
| `Reference-equation` | A solver for the stated mathematical formulation within its stated assumptions. |
| `Approximation` | A simplified model used for speed, scale, or tractability. |
| `Heuristic` | A ranking, screening, or stress-indicator method that is useful in practice but is not presented as a proof or certificate. |
| `Empirical standard` | A method defined by an external standard, rule set, or regression-based practice rather than first-principles network equations alone. |
| `Experimental` | Present and usable, but not presented as the primary production path for that study type. |
| `Supporting infrastructure` | Data models, parsers, sparse algebra, bindings, and similar implementation layers rather than study methods. |

## Current Method Map

### AC Power Flow

- Newton-Raphson is the primary `reference-equation` AC power flow method.
- Fast Decoupled Power Flow is an `approximation`.

### DC Power Flow And Sensitivities

- DC power flow is a `reference-equation` solve of the DC approximation itself.
- PTDF, LODF, and related linear sensitivity workflows are `approximations`
  relative to full nonlinear AC behavior.

### Contingency Analysis

- Post-contingency AC re-solve is the nonlinear validation step and should be
  read as the `reference-equation` path.
- LODF and FDPF screening layers are `approximations`.
- Local voltage-stress indicators are `heuristics`.

### OPF

- DC-OPF is an `approximation`.
- AC-OPF is a `reference-equation` solve for the stated nonlinear optimization
  model.
- Convex relaxations and screening variants should be read according to their
  own model assumptions, not as aliases for AC-OPF.

### HVDC

- Supported HVDC workflows model converter and network behavior directly, but
  the current public workflows still include sequential or block-iterative
  coupling choices.
- Those coupling choices should not be described as a monolithic simultaneous
  Newton solve unless the implementation actually does that.

### Transfer Capability

- PTDF-, DFAX-, and screening-led transfer workflows are `approximations`.
- AC confirmation and stability-limited follow-on steps are the higher-fidelity
  parts of the workflow.

### I/O, Topology, Sparse Algebra, CLI, And Python Bindings

- These are `supporting infrastructure`, not study methods.

## How To Read Public Docs

When Surge documentation describes a workflow:

- `reference-equation` means the implementation is intended to solve the stated
  formulation, not a looser screening proxy.
- `approximation` means the workflow is simplified by design and should be
  interpreted within that model's limits.
- `heuristic` means the result can guide prioritization or screening, but is
  not presented as a formal guarantee.

## Validation And Evidence

Public evidence claims should point to maintained tests in this repository or to
real, maintained artifacts in `surge-bench`.

## Related Pages

- [References](references.md) for governing equations and literature citations
- [Glossary](glossary.md) for terminology definitions
- [Data Model And Conventions](data-model.md) for per-unit conventions
