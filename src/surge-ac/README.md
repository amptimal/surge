# surge-ac

`surge-ac` is the AC power-flow crate in the Surge workspace. It provides the
nonlinear steady-state solvers and the AC-side helper layers used directly by
users and by higher-level crates such as `surge-contingency`, `surge-hvdc`, and
`surge-opf`.

## Root Surface

The crate root re-exports the main public entry points:

- `solve_ac_pf` and `AcPfOptions` for full Newton-Raphson AC power flow
- `solve_fdpf`, `FdpfOptions`, `FdpfVariant`, and `FdpfFactors` for
  fast-decoupled power flow
- `PreparedAcPf` and `PreparedStart` for repeated Newton-Raphson solves on a
  prepared model
- `merge_zero_impedance`, `expand_pf_solution`, and `MergedNetwork` for
  zero-impedance contraction and result expansion
- lower-level Newton-Raphson kernel types such as `PreparedNrModel`,
  `NrKernelOptions`, `NrState`, `NrWorkspace`, and `run_nr_inner` for advanced
  integration work

Public modules remain available for advanced callers:

- `control` for AC-side outer-loop and control support
- `matrix` for AC matrix assembly helpers
- `topology` for AC-specific topology helpers
- `ac_dc` for AC/DC-adjacent support code used by the wider workspace

## Choosing A Solver

Use `solve_ac_pf` when you need the full nonlinear AC solve:

- quadratic convergence on well-conditioned transmission cases
- reactive power limits, slack policies, island handling, and AC-side outer loops
- the canonical path for accuracy-sensitive studies

Use `solve_fdpf` when you need a cheaper approximate AC solve:

- screening and repeated approximate studies on transmission networks
- XB and BX variants through `FdpfVariant`
- lower per-iteration cost than Newton-Raphson, but less robust and less exact

## Example

```rust
use surge_ac::{solve_ac_pf, AcPfOptions};
use surge_io::load;

let net = load("examples/cases/ieee118/case118.surge.json.zst")?;
let sol = solve_ac_pf(&net, &AcPfOptions::default())?;

println!("iterations={} mismatch={:.2e}", sol.iterations, sol.max_mismatch);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Notes

- The current public contract is centered on Newton-Raphson, fast-decoupled
  power flow, prepared solves, and zero-impedance handling.
- Continuation power flow is not part of the current public crate surface.
- When you need a workspace-level interface instead of direct Rust use, reach
  for `surge-solve` or the Python package in `surge-py`.
