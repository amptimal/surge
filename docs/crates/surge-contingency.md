# surge-contingency

`surge-contingency` provides N-1 and N-2 contingency analysis with screening,
parallel AC re-solve, corrective-action workflows, and voltage-stress
assessment.

## Methodology

### Two-Stage Approach

Contingency analysis uses an optional screening stage followed by full AC
re-solve:

1. **Screening** (optional) — quickly filters the full contingency set to
   identify candidates likely to cause violations. Eliminates the majority of
   benign contingencies without running a full nonlinear solve for each.
2. **Parallel AC re-solve** — solves the remaining contingencies using
   Newton-Raphson with KLU, parallelized across CPU cores via rayon.

### Screening Modes

| Mode | Behavior |
|---|---|
| `Off` | No screening; solve all contingencies with full AC power flow |
| `Lodf` | Use LODF-based DC screening for thermal violations plus optional parallel FDPF voltage pre-screening. Typically eliminates ~90% of branch contingencies that cannot cause thermal overloads |
| `Fdpf` | Two-pass: FDPF screening with ~3 half-iterations from warm start, then NR only for violations detected by FDPF |

LODF screening works by computing the post-contingency branch flow using:

```text
P_post(m, k) = P_base(m) + LODF(m, k) * P_base(k)
```

where `m` is a monitored branch and `k` is the outaged branch. If
`|P_post| > threshold * rating`, the contingency is flagged for full AC
analysis. The `lodf_screening_threshold` (default 0.80) sets how aggressively
the screener filters.

Generator contingencies, breaker contingencies, and HVDC contingencies bypass
LODF screening (which is branch-only) and proceed directly to AC analysis.

For large networks, LODF columns are computed lazily using the factored B'
matrix to avoid O(n_branches^2) memory.

### Violation Types

The analysis detects the following violation types:

| Violation | Condition |
|---|---|
| **ThermalOverload** | Branch apparent power exceeds thermal rating × threshold |
| **VoltageLow** | Bus voltage magnitude below `vm_min` (default 0.95 p.u.) |
| **VoltageHigh** | Bus voltage magnitude above `vm_max` (default 1.05 p.u.) |
| **NonConvergent** | Newton-Raphson did not converge for this contingency |
| **Islanding** | Contingency created disconnected electrical islands |
| **FlowgateOverload** | Post-contingency flowgate flow exceeds its MW rating |
| **InterfaceOverload** | Post-contingency interface flow exceeds its MW rating |

### Thermal Ratings

The `thermal_rating` option selects which rating tier is used for
post-contingency thermal checks:

| Rating | Field | Description |
|---|---|---|
| `RateA` (default) | `rating_a_mva` | Long-term continuous |
| `RateB` | `rating_b_mva` | Short-term emergency (falls back to Rate A if zero) |
| `RateC` | `rating_c_mva` | Ultimate emergency (falls back to Rate A if zero) |

The `thermal_threshold_frac` (default 1.0) scales the effective limit:
`effective_limit = rating * thermal_threshold_frac`.

### N-2 Analysis

`analyze_n2_branch` evaluates simultaneous two-branch outages. The contingency
set is constructed from all unique pairs of in-service branches. LODF screening
uses N-2 LODF factors (compensated for the combined outage) to pre-filter.

### Voltage Stress Assessment

When enabled, the solver computes per-contingency voltage stress metrics:

- **Q-V stress proxy** — a local heuristic based on the ratio of reactive
  power consumption to available reactive margin at each PQ bus. Fast but
  approximate.
- **L-index** (Kessel-Glavitsch) — an exact voltage stability indicator
  derived from the hybrid bus admittance matrix. Values near 1.0 indicate
  proximity to voltage collapse.

Results are classified into categories: `Secure`, `Marginal`, `Critical`, or
`Unstable`.

### Corrective Dispatch

When `corrective_dispatch` is `true`, contingencies with thermal violations
trigger a Security-Constrained Redispatch (SCRD) that re-optimizes generator
dispatch to relieve the overloads while respecting ramp-rate limits.

### Prepared Study Objects

`ContingencyStudy` prepares the base-case solution and screening data once,
then reuses them across multiple analysis runs. Available study types:

- `ContingencyStudy::n1_branch` — branch N-1
- `ContingencyStudy::n1_generator` — generator N-1
- `ContingencyStudy::n2_branch` — branch N-2

The prepared study also supports follow-on corrective dispatch via
`study.solve_corrective_dispatch()`.

## Root API

Common entrypoints:

- `analyze_n1_branch` — one-shot branch N-1 analysis
- `analyze_n1_generator` — one-shot generator N-1 analysis
- `analyze_n2_branch` — one-shot branch N-2 analysis
- `analyze_contingencies` — analyze a custom contingency list

Common types:

- `ContingencyOptions` — analysis configuration
- `ContingencyAnalysis` — full result container
- `ContingencyResult` — per-contingency result
- `Violation` — individual violation
- `ThermalRating` — rating tier selection
- `VoltageStressResult` — per-contingency voltage stress

## ContingencyOptions Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `screening` | enum | `Fdpf` | `Off`, `Lodf`, or `Fdpf` (`Lodf` is exact for multi-outage cases today; only single-branch outages use LODF screening) |
| `thermal_threshold_frac` | float | `1.0` | Fraction of rating for thermal check |
| `lodf_screening_threshold` | float | `0.80` | LODF pre-filter threshold |
| `vm_min` | float | `0.95` | Minimum acceptable voltage (p.u.) |
| `vm_max` | float | `1.05` | Maximum acceptable voltage (p.u.) |
| `thermal_rating` | enum | `RateA` | `RateA`, `RateB`, or `RateC` |
| `top_k` | optional | `None` | Return only top-k worst contingencies |
| `corrective_dispatch` | bool | `false` | Enable corrective redispatch |
| `detect_islands` | bool | `true` | Detect post-contingency islands |
| `voltage_stress_mode` | enum | `Proxy` | Voltage stress method: `Off`, `Proxy`, `ExactLIndex` |
| `store_post_voltages` | bool | `false` | Store post-contingency V/theta in results |
| `contingency_flat_start` | bool | `false` | Use flat start instead of base-case warm start |
| `discrete_controls` | bool | `false` | Enable OLTC/PAR/shunt controls |
| `include_breaker_contingencies` | bool | `false` | Generate breaker contingencies from topology |

## ContingencyAnalysis Result

| Field | Description |
|---|---|
| `base_case` | Base-case power flow solution |
| `results` | Per-contingency results (list of `ContingencyResult`) |
| `n_contingencies` | Total contingencies evaluated |
| `n_screened_out` | Contingencies filtered by screening |
| `n_ac_solved` | Contingencies solved with full AC NR |
| `n_converged` | AC-solved contingencies that converged |
| `n_with_violations` | Contingencies with at least one violation |
| `n_violations` | Total violations across all contingencies |
| `n_voltage_critical` | Contingencies classified as voltage-critical |
| `solve_time_secs` | Wall-clock analysis time |

## ContingencyResult Fields

| Field | Description |
|---|---|
| `id` | Contingency identifier |
| `label` | Human-readable label |
| `status` | High-level outcome: `Converged`, `Approximate`, `Islanded`, or `NonConverged` |
| `converged` | Legacy boolean view; true for fully converged and explicitly islanded solved outcomes |
| `iterations` | NR iteration count |
| `violations` | List of `Violation` objects |
| `n_islands` | Number of post-contingency electrical islands |
| `fdpf_fallback` | True when the result came from FDPF fallback and should be treated as approximate |
| `voltage_stress` | Optional voltage stress assessment |
| `corrective_dispatch` | Optional corrective redispatch solution |

## Examples

### One-Shot Analysis (Python)

```python
import surge

net = surge.load("examples/cases/ieee118/case118.surge.json.zst")
result = surge.analyze_n1_branch(net, surge.ContingencyOptions(screening="lodf"))
print(f"{result.n_contingencies} contingencies, {result.n_with_violations} with violations")
```

### Prepared Study (Rust)

```rust
use surge_contingency::prepared::ContingencyStudy;
use surge_contingency::ContingencyOptions;

let study = ContingencyStudy::n1_branch(&network, &ContingencyOptions::default())?;
let result = study.analyze()?;
println!("n_contingencies={}", result.n_contingencies);
# Ok::<(), surge_contingency::ContingencyError>(())
```

## Related Docs

- [Data Model And Conventions](../data-model.md) for thermal rating tiers
- [Method Fidelity](../method-fidelity.md)
- [References](../references.md)
- [surge-dc](surge-dc.md) for LODF computations
- [surge-ac](surge-ac.md) for the underlying NR solver
- [surge-opf](surge-opf.md) for SCOPF
- [Tutorial 02](../tutorials/02-contingency-analysis.md)
