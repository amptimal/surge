# surge-dc

`surge-dc` is the DC power-flow and linear sensitivity crate in the Surge
workspace. It provides the canonical Rust entry points for one-shot DC solves,
prepared reusable DC studies, PTDF/LODF/OTDF workflows, and batched N-2
sensitivity calculations.

Transfer-capability workflows such as ATC, AFC, and reusable transfer studies
live in [`surge-transfer`](surge-transfer.md), not in this crate.

## Public Surface

The crate root is the supported API. The internal modules are implementation
details.

| Area | Public entry points |
|---|---|
| DC power flow | `solve_dc`, `solve_dc_opts`, `DcPfOptions`, `DcPfSolution`, `DcError` |
| One-pass workflow | `run_dc_analysis`, `DcAnalysisRequest`, `DcAnalysisResult` |
| PTDF / OTDF | `compute_ptdf`, `compute_ptdf_request`, `compute_otdf`, `compute_otdf_request`, `DcSensitivityOptions`, `DcSensitivitySlack`, `PtdfRequest`, `OtdfRequest` |
| LODF / N-2 | `compute_lodf`, `compute_lodf_request`, `compute_lodf_matrix`, `compute_lodf_matrix_request`, `compute_lodf_pairs`, `compute_n2_lodf`, `compute_n2_lodf_request`, `compute_n2_lodf_batch`, `compute_n2_lodf_batch_request`, `LodfRequest`, `LodfMatrixRequest`, `N2LodfRequest`, `N2LodfBatchRequest` |
| Reuse on one network | `PreparedDcStudy` |
| Lazy column builders | `streaming::LodfColumnBuilder`, `streaming::N2LodfColumnBuilder` |
| Result conversion | `to_pf_solution` |

The crate does not expose `solver`, `sensitivity`, or `bprime` as stable user
modules. Downstream code should treat the root re-exports and `streaming` as
the public contract.

## What The Model Assumes

`surge-dc` uses the standard DC approximation:

- flat voltage magnitudes (`|V| = 1.0`)
- small angle differences
- lossless branches (`r` neglected in the flow equations)
- no reactive power solution

Because of those assumptions, DC results are appropriate for fast active-power
studies and sensitivities, but not for voltage magnitude, reactive dispatch,
loss, or voltage-stability questions.

## Typical Workflows

### One-shot DC power flow

```rust
use surge_dc::solve_dc;
use surge_io::load;

let net = load("examples/cases/ieee118/case118.surge.json.zst")?;
let sol = solve_dc(&net)?;
println!("{} branch flows", sol.branch_p_flow.len());
# Ok::<(), Box<dyn std::error::Error>>(())
```

### One-shot power flow plus sensitivities

```rust
use surge_dc::{run_dc_analysis, DcAnalysisRequest};
use surge_io::load;

let net = load("examples/cases/ieee118/case118.surge.json.zst")?;
let request = DcAnalysisRequest::all_branches();
let result = run_dc_analysis(&net, &request)?;
println!("{} monitored branches", result.monitored_branch_indices.len());
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Repeated work on one network

```rust
use surge_dc::{PreparedDcStudy, PtdfRequest};
use surge_io::load;

let net = load("examples/cases/ieee118/case118.surge.json.zst")?;
let mut study = PreparedDcStudy::new(&net)?;
let _base = study.solve(&Default::default())?;
let _ptdf = study.compute_ptdf_request(&PtdfRequest::new())?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Use `PreparedDcStudy` when you are reusing the same network for many DC solves
or sensitivity queries. It prepares island-aware kernels once and then reuses
them across the follow-on calls.

### Distributed slack (participation factors)

By default, DC power flow pins one slack bus as the angle reference and
assigns all system mismatch to it. For RTO-style studies where mismatch
should be distributed across generators in proportion to AGC participation
factors, use `DcPfOptions::with_participation_factors` or
`DcPfOptions::with_network_participation`:

```rust
use surge_dc::{DcPfOptions, solve_dc_opts};
use surge_io::load;

let net = load("examples/cases/ieee118/case118.surge.json.zst")?;
// Use AGC participation factors from the generators:
let opts = DcPfOptions::with_network_participation(&net);
let sol = solve_dc_opts(&net, &opts)?;
// sol.slack_distribution shows per-bus MW shares
# Ok::<(), Box<dyn std::error::Error>>(())
```

The sensitivity layer has matching support via `DcSensitivitySlack::SlackWeights`.
When participation factors are set on `DcPfOptions`, the one-pass
`run_dc_analysis` workflow automatically uses the distributed PTDF
(D-PTDF) formulation:

```
D-PTDF[l, b] = PTDF[l, b] - Σ_k(α_k · PTDF[l, k])
```

LODF is algebraically invariant to the slack distribution (the reference-bus
correction cancels in the branch-endpoint difference), so no separate
distributed LODF is needed.

## When To Use This Crate

Good fits:

- market and dispatch formulations built on DC physics
- PTDF/LODF-based congestion or contingency screening
- repeated transfer/sensitivity calculations on one network
- workflows where speed and linearity matter more than AC voltage fidelity

Reach for `surge-ac`, `surge-contingency`, or `surge-opf` instead when the
study needs AC voltage behavior, reactive power, nonlinear loss effects, or
full AC post-contingency validation.

## Related Docs

- [../method-fidelity.md](../method-fidelity.md)
- [../tutorials/01-basic-power-flow.md](../tutorials/01-basic-power-flow.md)
- [../tutorials/02-contingency-analysis.md](../tutorials/02-contingency-analysis.md)
- [surge-transfer.md](surge-transfer.md)
