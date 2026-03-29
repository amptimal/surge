# surge-opf

`surge-opf` provides optimal power flow solvers for DC-OPF, AC-OPF, SCOPF,
and adjacent specialist studies (optimal transmission switching, reactive
dispatch).

## Formulations

### DC-OPF

The DC-OPF minimizes generation cost subject to the DC power flow constraints:

```text
minimize   sum_i C_i(P_gi)
subject to:
  P_gi_min <= P_gi <= P_gi_max            (generator limits)
  sum_g(P_gi) - sum_d(P_di) = 0           (system power balance)
  P_f = B'_f * theta                       (DC flow on each branch f)
  |P_f| <= rating_a_f                      (thermal limits)
```

where `C_i(P_gi)` is the cost curve for generator `i` and `B'` is the DC
susceptance matrix.

**Cost models:**

- **Quadratic** (default) — `C(P) = c2*P^2 + c1*P + c0`. Formulated as a QP
  with a positive semidefinite Hessian.
- **Piecewise-linear** — outer linearization of quadratic costs using tangent
  lines, or direct PWL breakpoint curves. Eliminates the QP Hessian, producing
  a pure LP. Use `cost_model = "piecewise_linear"` with
  `piecewise_linear_breakpoints` to control approximation quality.

**Loss modeling:**

The DC approximation is lossless. When `loss_model = "iterative"`, the solver
iteratively adjusts generator penalty factors using marginal loss sensitivity:
`pf_i = 1 / (1 - dLoss/dP_i)`. This shifts LMPs to reflect approximate loss
contributions. Convergence is controlled by `loss_iterations` and
`loss_tolerance`.

**LMP decomposition:**

The DC-OPF dual variables yield Locational Marginal Prices (LMPs) decomposed
into energy, congestion, and (when loss factors are enabled) loss components.

### AC-OPF

The AC-OPF minimizes generation cost subject to the full nonlinear AC power
flow equations:

```text
minimize   sum_i C_i(P_gi)
subject to:
  P_gi_min <= P_gi <= P_gi_max
  Q_gi_min <= Q_gi <= Q_gi_max
  V_i_min  <= V_i  <= V_i_max
  P_calc_i(V, theta) = P_gi - P_di        (active balance at each bus)
  Q_calc_i(V, theta) = Q_gi - Q_di        (reactive balance at each bus)
  S_f(V, theta) <= rating_a_f             (apparent power thermal limits)
```

The NLP is solved using an interior-point method (Ipopt by default). Surge
provides the exact analytical Hessian of the Lagrangian to the NLP solver,
enabling superlinear convergence. A quasi-Newton (L-BFGS) fallback is
available via `exact_hessian = false`.

**Warm-start strategy:**

For large cases, AC-OPF can seed initial voltage angles from a DC-OPF
solution. This is enabled automatically when `n_buses > 2000` or forced with
`angle_warm_start = "dc_opf"`. The DC-OPF provides a feasible angle profile
that significantly reduces NLP iteration count.

**Discrete controls:**

When `discrete_mode = "round_and_check"`, the solver first solves a continuous
relaxation, then rounds transformer taps, phase-shifter angles, and switched
shunt steps to their nearest discrete values, and verifies feasibility with an
AC power flow.

**Optional co-optimization:**

- Transformer tap ratios (`optimize_taps`)
- Phase-shifter angles (`optimize_phase_shifters`)
- Switched shunt susceptance (`optimize_switched_shunts`)
- SVC/STATCOM susceptance (`optimize_svc`)
- TCSC compensating reactance (`optimize_tcsc`)
- HVDC converter setpoints (`hvdc_mode`)
- Generator P-Q capability curves (`enforce_capability_curves`)
- Storage charge/discharge within SoC bounds (`storage_state_mwh_by_generator_id`)

**Active constraint screening:**

For large networks (default threshold: 1000+ buses), Surge can pre-screen
thermal constraints using a DC-OPF loading estimate. Only branches loaded
above `constraint_screening.threshold_fraction` (default 0.9) of their rating
are included in the initial NLP. This reduces problem size without sacrificing
solution quality for well-loaded systems.

### SCOPF

Security-Constrained OPF finds a base-case dispatch that satisfies both
base-case constraints and post-contingency constraints for a specified set of
N-1 contingencies. Surge uses an iterative constraint generation (Benders
decomposition) approach:

1. Solve the base-case OPF (DC or AC).
2. Evaluate post-contingency flows for all contingencies.
3. Add violated contingency-branch pairs as cuts to the master problem.
4. Re-solve. Repeat until no violations exceed `violation_tolerance_pu`.

**DC SCOPF** adds linear post-contingency flow constraints (LODF-based) to the
DC-OPF LP. Fast and scalable to large contingency sets.

**AC SCOPF** uses Benders decomposition with NLP subproblems. The master
AC-OPF is augmented with linearized cuts from post-contingency NR solves.
Handles voltage and reactive constraints in addition to thermal.

**Modes:**

- **Preventive** (default) — the base-case dispatch must satisfy all
  post-contingency constraints. No corrective redispatch is allowed.
- **Corrective** — post-contingency corrective redispatch is allowed within
  generator ramp-rate limits over the `corrective_ramp_window_minutes` time
  window (default: 10 minutes).

**Pre-screening:**

DC SCOPF includes an optional LODF-based pre-screener that identifies the most
likely binding contingency-branch pairs before entering the constraint
generation loop. Controlled by `ScopfScreeningPolicy`:

- `enabled` — whether to pre-screen (default `true`)
- `threshold_fraction` — LODF loading threshold (default 0.9)
- `max_initial_contingencies` — cap on pre-screened contingencies (default 500)

**Post-contingency thermal rating:**

The `contingency_rating` option selects which branch rating tier applies to
post-contingency limits:

| Rating | Description |
|---|---|
| `rate-a` (default) | Long-term continuous rating |
| `rate-b` | Short-term emergency rating |
| `rate-c` | Ultimate emergency rating |

RTO practice typically uses Rate A for base case and Rate B for
post-contingency.

## Solver Backends

All solver backends are discovered at runtime via dynamic loading. No Cargo
feature flags are needed.

| Backend | Type | License | Use case |
|---|---|---|---|
| HiGHS | LP/QP/MIP | MIT | Default for DC-OPF, SCOPF, OTS |
| Ipopt | NLP | EPL | Open-source AC-OPF backend and fallback |
| Gurobi | LP/QP/MIP/NLP | Commercial | Optional high-performance alternative; native AC-OPF path supports the core model and now errors clearly on unsupported advanced features |
| COPT | LP/QP/MIP/NLP | Commercial | Preferred AC-OPF NLP backend when available; Python wheels can bundle the shim |
| CPLEX | LP/QP/MIP | Commercial | Optional alternative |

**Canonical default policy:**

- LP/QP problems (DC-OPF, DC-SCOPF, OTS): HiGHS unless overridden.
- Generic NLP problems (AC-SCOPF, ORPD): best available runtime backend unless
  overridden. Current detection priority is COPT, then Ipopt.
- AC-OPF: best available AC-OPF runtime backend unless overridden. Current
  detection priority is COPT, then Ipopt, then Gurobi.
- AC-OPF keeps explicit solver selections explicit. Under the implicit default
  policy only, the known `case6470rte` default-COPT miss is retried with Ipopt.

Override in Python with `runtime=DcOpfRuntime(lp_solver="gurobi")` or
`runtime=AcOpfRuntime(nlp_solver="copt")`.

Override in CLI with a method-compatible `--solver` value. `--solver gurobi`
is AC-OPF-specific; generic NLP workflows such as ORPD and AC-SCOPF use COPT
or Ipopt.

## Canonical Root Surface

Entrypoints:

- `solve_dc_opf`, `solve_dc_opf_with_runtime`
- `solve_ac_opf`, `solve_ac_opf_with_runtime`
- `solve_scopf`, `solve_scopf_with_runtime`

Specialist studies under `switching`:

- `solve_ots`, `solve_ots_with_runtime` (optimal transmission switching)
- `solve_orpd` (optimal reactive power dispatch)

## DcOpfOptions Reference

The tables below use the Python field names. The Rust structs use abbreviated
names internally (e.g., `min_rate_a` instead of `minimum_branch_rating_a_mva`,
`use_pwl_costs` instead of `cost_model`). The Python wrapper translates
between the two in `to_native_kwargs()`.

| Field | Type | Default | Description |
|---|---|---|---|
| `enforce_thermal_limits` | bool | `true` | Enforce branch thermal constraints |
| `minimum_branch_rating_a_mva` | float | `1.0` | Branches below this rating are unconstrained |
| `cost_model` | enum | `Quadratic` | `Quadratic` (QP) or `PiecewiseLinear` (LP) |
| `piecewise_linear_breakpoints` | int | `20` | Tangent-line count for PWL approximation |
| `enforce_flowgates` | bool | `true` | Include interface and flowgate constraints |
| `loss_model` | enum | `Ignore` | `Ignore` or `Iterative` (penalty-factor loss approximation) |
| `loss_iterations` | int | `3` | Maximum loss-factor convergence iterations |
| `loss_tolerance` | float | `1e-3` | Penalty-factor convergence threshold |
| `generator_limit_mode` | enum | `Hard` | `Hard` (infeasible if violated) or `Soft` (penalized) |
| `generator_limit_penalty_per_mw` | float | — | Penalty cost for soft generator limit violations ($/MW) |
| `par_setpoints` | list | `[]` | Phase-shifter MW setpoint constraints |
| `hvdc_links` | list | `[]` | HVDC links to co-optimize |
| `virtual_bids` | list | `[]` | Day-ahead virtual energy bids |

## DcOpfRuntime Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `tolerance` | float | `1e-8` | LP solver convergence tolerance |
| `max_iterations` | int | `200` | LP solver iteration limit |
| `lp_solver` | string | `None` | Override LP backend (`"highs"`, `"gurobi"`, `"cplex"`, `"copt"`) |

## AcOpfOptions Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `enforce_thermal_limits` | bool | `true` | Enforce apparent-power thermal limits |
| `minimum_branch_rating_a_mva` | float | `1.0` | Branches below this rating are unconstrained |
| `enforce_angle_limits` | bool | `false` | Enforce branch angle-difference limits |
| `enforce_capability_curves` | bool | `true` | Enforce generator P-Q capability curves |
| `enforce_flowgates` | bool | `false` | Include interface and flowgate constraints |
| `optimize_switched_shunts` | bool | `false` | Co-optimize switched shunt susceptance |
| `optimize_taps` | bool | `false` | Co-optimize transformer tap ratios |
| `optimize_phase_shifters` | bool | `false` | Co-optimize phase-shifter angles |
| `optimize_svc` | bool | `false` | Co-optimize SVC/STATCOM susceptance |
| `optimize_tcsc` | bool | `false` | Co-optimize TCSC reactance |
| `hvdc_mode` | enum | `Auto` | HVDC handling: `Auto`, `Enabled`, or `Disabled` |
| `discrete_mode` | enum | `Continuous` | `Continuous` or `RoundAndCheck` |
| `interval_hours` | float | `1.0` | Storage dispatch interval for SoC bounds |

## AcOpfRuntime Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `tolerance` | float | `1e-6` | NLP convergence tolerance |
| `max_iterations` | int | `0` | NLP iteration limit (0 = auto-scale) |
| `exact_hessian` | bool | `true` | Use exact analytical Hessian (vs L-BFGS) |
| `nlp_solver` | string | `None` | Override NLP backend (`"ipopt"`, `"gurobi"`, `"copt"`) |
| `print_level` | int | `0` | NLP solver verbosity (0 = silent, 5 = verbose) |
| `angle_warm_start` | enum | `Auto` | `Auto`, `DcOpf`, or `DcPowerFlow` |
| `constraint_screening` | object | `None` | Active constraint screening policy |

## ScopfOptions Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `formulation` | enum | `Dc` | `Dc` or `Ac` |
| `mode` | enum | `Preventive` | `Preventive` or `Corrective` |
| `corrective_ramp_window_minutes` | float | `10.0` | Corrective action time window |
| `contingency_rating` | enum | `RateA` | Post-contingency thermal rating tier |
| `enforce_flowgates` | bool | `true` | Include flowgate constraints |
| `enforce_voltage_security` | bool | `true` | Post-contingency voltage limits (AC only) |
| `voltage_threshold_pu` | float | `0.01` | Voltage violation threshold (AC only) |
| `max_contingencies` | int | `0` | Cap on contingencies evaluated (0 = all) |
| `minimum_branch_rating_a_mva` | float | `1.0` | Minimum rating for thermal constraints |

## ScopfRuntime Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `violation_tolerance_pu` | float | `0.01` | Post-contingency violation threshold |
| `max_iterations` | int | `20` | Maximum constraint generation iterations |
| `max_cuts_per_iteration` | int | `100` | Maximum violated constraints added per iteration |
| `lp_solver` | string | `None` | LP backend for DC-SCOPF |
| `nlp_solver` | string | `None` | NLP backend for AC-SCOPF |
| `screening` | object | enabled | LODF pre-screening policy |

## Examples

### DC-OPF (Python)

```python
import surge

net = surge.load("examples/cases/ieee118/case118.surge.json.zst")
result = surge.solve_dc_opf(
    net,
    options=surge.DcOpfOptions(enforce_thermal_limits=True),
    runtime=surge.DcOpfRuntime(lp_solver="highs"),
)
print(f"cost={result.total_cost:.2f} $/hr")
```

### AC-OPF (Rust)

```rust
use surge_opf::{AcOpfOptions, solve_ac_opf};
use surge_io::load;

let net = load("examples/cases/ieee118/case118.surge.json.zst")?;
let result = solve_ac_opf(&net, &AcOpfOptions::default())?;
println!("total_cost={:.2f}", result.total_cost);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Related Docs

- [Data Model And Conventions](../data-model.md) for cost curve and rating conventions
- [Method Fidelity](../method-fidelity.md)
- [References](../references.md)
- [surge-dc](surge-dc.md) for DC power flow and sensitivities
- [surge-contingency](surge-contingency.md) for standalone contingency analysis
