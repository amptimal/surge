# surge-ac

`surge-ac` is the AC power-flow crate in the Surge workspace. It provides the
nonlinear steady-state solvers used directly by users and by higher-level
crates such as `surge-contingency`, `surge-hvdc`, and `surge-opf`.

## Formulation

### Newton-Raphson (NR)

The Newton-Raphson solver iterates on the polar-form AC power flow mismatch
equations:

```text
Delta_P_i = P_scheduled_i - P_calc_i(V, theta)
Delta_Q_i = Q_scheduled_i - Q_calc_i(V, theta)
```

where the calculated injections are the standard AC power flow equations:

```text
P_calc_i = V_i * sum_j( V_j * (G_ij * cos(theta_i - theta_j) + B_ij * sin(theta_i - theta_j)) )
Q_calc_i = V_i * sum_j( V_j * (G_ij * sin(theta_i - theta_j) - B_ij * cos(theta_i - theta_j)) )
```

At each iteration, the Jacobian matrix `J` is assembled in sparse CSC format
from the current voltage state and factored via KLU (SuiteSparse). The
correction vector is:

```text
[Delta_theta]         [Delta_P]
[           ] = -J^-1 [       ]
[Delta_V / V]         [Delta_Q]
```

Convergence is declared when `max(|Delta_P|, |Delta_Q|) < tolerance` across all
non-slack buses. The default tolerance is `1e-8` per-unit.

### Fast Decoupled Power Flow (FDPF)

FDPF exploits the approximate decoupling between P-theta and Q-V subproblems
in high-voltage transmission networks. Instead of the full Jacobian, it uses
two constant matrices `B'` (for P-theta) and `B''` (for Q-V), factored once
and reused across iterations.

Two variants are available:

- **XB** (default) — uses `B' = -1/x` for the P-theta subproblem. Better for
  typical transmission networks with low R/X ratios.
- **BX** — uses the full B matrix for both subproblems. Can be more robust on
  networks with higher R/X ratios.

FDPF converges in more iterations than NR but each iteration is cheaper. It is
best suited for screening, approximate studies, and contingency pre-filtering.

## Solver Behavior

### Convergence

`max_mismatch` in the result is the infinity-norm of the power balance mismatch
vector at the final iteration, in per-unit on the system MVA base. For a
converged solution, this will be below `tolerance`.

`SolveStatus` reports whether the solve `Converged`, hit `MaxIterations`, or
`Diverged` (numerical breakdown).

### Reactive Power Limits (PV-to-PQ Switching)

When `enforce_q_limits` is `true` (default), the solver enforces generator
reactive power limits. If a PV bus generator reaches `qmax` or `qmin`, the bus
is switched to PQ type and the reactive output is clamped. This outer loop
re-solves until no further switches occur or stability is reached.

`q_sharing` controls how reactive power is distributed among multiple generators
at the same bus before limit checking:

| Mode | Behavior |
|---|---|
| `Capability` (default) | Proportional to `qmax - qmin` range |
| `Mbase` | Proportional to machine base MVA |
| `Equal` | Equal share among all free generators |

### Active Power Limits

When `enforce_gen_p_limits` is `true` (default), generators whose dispatch
exceeds `pmax` or falls below `pmin` are clamped, and the slack bus absorbs
the difference.

### Slack Bus And Distributed Slack

By default, one slack bus per island absorbs the active power mismatch. When
`distributed_slack` is `true`, the mismatch is distributed across participating
generators according to `slack_participation` weights.

### Island Detection

When `detect_islands` is `true` (default), the solver identifies electrically
disconnected islands and solves each independently with its own reference bus.
Isolated single-bus islands are excluded from the solve.

### Zero-Impedance Branch Handling

When `auto_merge_zero_impedance` is `true`, branches with `x ≈ 0` (such as
`ZeroImpedanceTie` branches) are merged before the solve. The merged buses
share a single voltage, reducing numerical difficulty. Results are expanded
back to the original bus numbering after the solve.

### Area Interchange Enforcement

When `enforce_interchange` is `true`, the solver adjusts regulating generators
(those with AGC participation factors) to match area net interchange targets
from `network.area_schedules`. This adds an outer loop around the NR solve.

### Discrete Controls

The solver supports outer-loop control of:

- **OLTCs** (on-load tap changers) — adjusts transformer tap ratios to regulate
  voltage at the controlled bus, within `[tap_min, tap_max]`.
- **PARs** (phase-angle regulators) — adjusts phase shift to achieve a target
  MW flow, within `[phase_min, phase_max]`.
- **Switched shunts** — steps susceptance blocks to regulate bus voltage within
  a deadband.

Each control type has its own iteration limit (`oltc_max_iter`, `par_max_iter`,
`shunt_max_iter`). Controls are enabled by default when the corresponding data
is present in the network.

### Angle Reference

The `angle_reference` option controls how output voltage angles are reported:

| Mode | Behavior |
|---|---|
| `PreserveInitial` (default) | Keep the original slack bus angle from the input data |
| `Zero` | Shift all angles so the slack bus angle is zero |
| `Distributed(weight)` | Use a weighted center-of-inertia reference |

### Startup Policy

`startup_policy` controls the initialization strategy:

| Policy | Behavior |
|---|---|
| `Adaptive` (Rust default) | Escalate through sequential fallbacks if initial attempts fail |
| `Single` (Python default) | Run a single solve with the requested initialization |
| `ParallelWarmAndFlat` | Race case-data initialization against flat start |

When `flat_start` is `true`, all buses start at V = 1.0 p.u. and theta = 0.
When `dc_warm_start` is `true` (default), a DC power flow initializes the
voltage angles before the NR solve.

## Public Surface

Root entrypoints:

- `solve_ac_pf` — full Newton-Raphson AC power flow
- `solve_fdpf` — fast-decoupled power flow
- `AcPfOptions` — options for NR solves
- `FdpfOptions` — options for FDPF solves
- `AcPfError` — error type
- `PreparedAcPf` — reusable prepared model for repeated NR solves

Advanced re-exports (for crate integration, not typical user code):

- `solve_ac_pf_kernel`, `run_nr_inner`, `PreparedNrModel`
- `NrKernelOptions`, `NrState`, `NrWorkspace`
- `merge_zero_impedance`, `expand_pf_solution`, `expand_facts`

## AcPfOptions Reference

Defaults shown are the Rust defaults. Where the Python wrapper uses a different
default, both are noted. Python field names differ slightly (e.g., `oltc`
instead of `oltc_enabled`, `merge_zero_impedance` instead of
`auto_merge_zero_impedance`).

| Field | Type | Default | Description |
|---|---|---|---|
| `tolerance` | float | `1e-8` | Convergence tolerance in per-unit |
| `max_iterations` | int | `100` | Maximum NR iterations |
| `flat_start` | bool | `false` | Initialize all buses at V=1.0, theta=0 |
| `dc_warm_start` | bool | `true` | Initialize angles from DC power flow |
| `line_search` | bool | `true` | Enable step-size damping for robustness |
| `vm_min` | float | no clamp | Lower voltage clamp during iteration (Rust: `NEG_INFINITY`; Python: `0.5`) |
| `vm_max` | float | no clamp | Upper voltage clamp during iteration (Rust: `INFINITY`; Python: `1.5`) |
| `startup_policy` | enum | `Adaptive` (Rust) / `Single` (Python) | Initialization strategy (see above) |
| `enforce_q_limits` | bool | `true` | Enable PV-to-PQ reactive limit switching |
| `q_sharing` | enum | `Capability` | Reactive sharing mode among generators at the same bus |
| `enforce_gen_p_limits` | bool | `true` | Clamp generators to `[pmin, pmax]` |
| `distributed_slack` | bool | `false` | Distribute slack across generators |
| `detect_islands` | bool | `true` | Detect and solve islands independently |
| `enforce_interchange` | bool | `false` | Enforce area interchange schedules |
| `interchange_max_iter` | int | `10` | Maximum area interchange adjustment iterations |
| `oltc_enabled` | bool | `true` | Enable OLTC tap control |
| `oltc_max_iter` | int | `20` | Maximum OLTC adjustment iterations |
| `shunt_enabled` | bool | `true` | Enable switched shunt control |
| `angle_reference` | enum | `PreserveInitial` | Output angle reference convention |
| `dc_line_model` | enum | `FixedSchedule` | HVDC line treatment (`FixedSchedule` or `SequentialAcDc`) |
| `par_enabled` | bool | `true` | Enable PAR phase-angle regulator control |
| `par_max_iter` | int | `20` | Maximum PAR adjustment iterations |
| `auto_merge_zero_impedance` | bool | `true` (Rust) / `false` (Python) | Merge zero-impedance branches before solve |
| `record_convergence_history` | bool | `false` | Store per-iteration mismatch history |

## FdpfOptions Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `variant` | enum | `Xb` | `Xb` or `Bx` decoupling variant |
| `tolerance` | float | `1e-6` | Convergence tolerance in per-unit |
| `max_iterations` | int | `100` | Maximum half-iterations |
| `flat_start` | bool | `true` | Initialize from flat start |
| `enforce_q_limits` | bool | `true` | Enable reactive limit switching |

## Result Contract

Both `solve_ac_pf` and `solve_fdpf` return a `PfSolution` with:

| Field | Type | Description |
|---|---|---|
| `status` | enum | `Converged`, `MaxIterations`, `Diverged`, or `Unsolved` |
| `iterations` | int | Iteration count |
| `max_mismatch` | float | Final max power mismatch (p.u.) |
| `solve_time_secs` | float | Wall-clock solve time |
| `voltage_magnitude_pu` | array | Solved voltage magnitudes per bus |
| `voltage_angle_rad` | array | Solved voltage angles per bus (radians) |
| `branch_p_from_mw` | array | Active power flow at each branch from-end (MW) |
| `branch_q_from_mvar` | array | Reactive power flow at each branch from-end (MVAr) |
| `branch_p_to_mw` | array | Active power flow at each branch to-end (MW) |
| `branch_q_to_mvar` | array | Reactive power flow at each branch to-end (MVAr) |
| `q_limited_buses` | array | Buses where PV-to-PQ switching occurred |
| `worst_mismatch_bus` | optional | Bus number with the largest remaining mismatch |
| `convergence_history` | array | Per-iteration `(iteration, mismatch)` pairs (when recorded) |

## When To Use Each Solver

| Need | Solver |
|---|---|
| Production-quality AC operating point | `solve_ac_pf` (Newton-Raphson) |
| Fast approximate screening | `solve_fdpf` |
| Repeated solves on one network | `PreparedAcPf` |
| Voltage, reactive power, and loss accuracy | `solve_ac_pf` |
| Contingency pre-filtering | `solve_fdpf` |

For studies that do not require voltage magnitudes, reactive power, or losses,
the DC power flow in `surge-dc` is faster and sufficient.

## Examples

### Rust

```rust
use surge_ac::{AcPfOptions, solve_ac_pf};
use surge_io::load;

let net = load("examples/cases/ieee118/case118.surge.json.zst")?;
let sol = solve_ac_pf(&net, &AcPfOptions::default())?;
println!(
    "converged={} iterations={} mismatch={:.2e}",
    sol.converged, sol.iterations, sol.max_mismatch
);
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Python

```python
import surge

net = surge.load("examples/cases/ieee118/case118.surge.json.zst")
ac = surge.solve_ac_pf(net, surge.AcPfOptions(tolerance=1e-10))
print(f"converged={ac.converged} iterations={ac.iterations}")
```

## Related Docs

- [Data Model And Conventions](../data-model.md)
- [Method Fidelity](../method-fidelity.md)
- [References](../references.md)
- [surge-dc](surge-dc.md) for DC power flow
- [surge-contingency](surge-contingency.md) for contingency analysis
