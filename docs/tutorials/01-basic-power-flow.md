# Tutorial 01: Power Flow Baseline Experiment

This tutorial establishes a baseline operating point, compares AC and DC
results, and shows how much the answer depends on the method and starting
policy.

## Questions To Answer

- Does the case converge cleanly with the default AC solve?
- How different is the DC approximation from the AC baseline?
- Does a flat start materially change the final solution or only the iteration count?
- Is FDPF close enough for your screening workflow?

## Python Experiment

Use the package entry points below:

- `surge.solve_ac_pf(...)` for AC power flow
- `surge.solve_dc_pf(...)` for the linear baseline
- `surge.powerflow.solve_fdpf(...)` for a screening-oriented AC approximation

```python
import surge

net = surge.case118()

ac = surge.solve_ac_pf(
    net,
    surge.AcPfOptions(
        tolerance=1e-8,
        flat_start=False,
        dc_warm_start=True,
        enforce_q_limits=True,
    ),
)

ac_flat = surge.solve_ac_pf(
    net,
    surge.AcPfOptions(
        tolerance=1e-8,
        flat_start=True,
        dc_warm_start=True,
        enforce_q_limits=True,
    ),
)

fdpf = surge.powerflow.solve_fdpf(
    net,
    surge.powerflow.FdpfOptions(
        tolerance=1e-6,
        variant="xb",
        flat_start=True,
    ),
)

dc = surge.solve_dc_pf(net, surge.DcPfOptions())

print("AC:", ac.converged, ac.iterations, ac.max_mismatch)
print("AC flat-start:", ac_flat.converged, ac_flat.iterations, ac_flat.max_mismatch)
print("FDPF:", fdpf.converged, fdpf.iterations, fdpf.max_mismatch)
print("DC solve time (s):", dc.solve_time_secs)

loading = ac.branch_loading_pct()
print("Worst AC branch loading (%):", float(max(loading)))
print("First 5 AC bus angles (deg):", ac.va_deg[:5])
print("First 5 DC bus angles (rad):", dc.va_rad[:5])
```

## Rust Experiment

Use the crate roots only. Do not reach into internal solver modules for normal
user code.

```rust
use std::path::Path;

use surge_ac::{AcPfOptions, FdpfOptions, solve_ac_pf, solve_fdpf};
use surge_dc::solve_dc;
use surge_io::load;

fn main() -> anyhow::Result<()> {
    let net = load(Path::new("examples/cases/ieee118/case118.surge.json.zst"))?;

    let ac = solve_ac_pf(
        &net,
        &AcPfOptions {
            tolerance: 1e-8,
            flat_start: false,
            dc_warm_start: true,
            enforce_q_limits: true,
            ..Default::default()
        },
    )?;

    let ac_flat = solve_ac_pf(
        &net,
        &AcPfOptions {
            tolerance: 1e-8,
            flat_start: true,
            dc_warm_start: true,
            enforce_q_limits: true,
            ..Default::default()
        },
    )?;

    let fdpf = solve_fdpf(
        &net,
        &FdpfOptions {
            tolerance: 1e-6,
            flat_start: true,
            ..Default::default()
        },
    )?;

    let dc = solve_dc(&net)?;

    println!("ac iterations={} mismatch={:.2e}", ac.iterations, ac.max_mismatch);
    println!(
        "ac flat iterations={} mismatch={:.2e}",
        ac_flat.iterations, ac_flat.max_mismatch
    );
    println!("fdpf iterations={} mismatch={:.2e}", fdpf.iterations, fdpf.max_mismatch);
    println!("dc solve time = {:.6}s", dc.solve_time_secs);

    Ok(())
}
```

## What To Record

- AC iteration count with and without flat start
- Worst branch loading from the AC solution
- A small sample of AC and DC bus angles
- Whether FDPF matches the same qualitative stress pattern as the AC solve

If the AC and FDPF solutions disagree on the stressed buses or branches, use the
AC solve as the operational truth and treat FDPF as a rough screen only.

## Extensions

- Re-run the experiment after `net.scale_loads(1.05)` in Python and compare the
  change in mismatch, voltages, and loading.
- Move on to [Tutorial 02](02-contingency-analysis.md) once you trust the base
  operating point.
