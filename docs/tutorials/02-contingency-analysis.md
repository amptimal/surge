# Tutorial 02: Contingency Screening Experiment

This tutorial compares screening modes, identifies the contingencies that
matter, and shows when reusable study objects are worth using.

## Questions To Answer

- How much work does LODF screening remove relative to a full AC run?
- Which contingencies produce the worst thermal or voltage outcomes?
- When should you keep using direct calls versus a prepared study?

## Python Experiment

The root package covers N-1 and N-2 workflows. The `surge.contingency`
namespace is where reusable study objects and follow-on workflows live.

```python
import surge

net = surge.case118()

lodf = surge.analyze_n1_branch(
    net,
    surge.ContingencyOptions(
        screening="lodf",
        thermal_threshold_pct=100.0,
        vm_min=0.95,
        vm_max=1.05,
    ),
)

full = surge.analyze_n1_branch(
    net,
    surge.ContingencyOptions(
        screening="off",
        thermal_threshold_pct=100.0,
        vm_min=0.95,
        vm_max=1.05,
    ),
)

n2 = surge.analyze_n2_branch(
    net,
    surge.ContingencyOptions(
        screening="lodf",
        top_k=100,
        thermal_threshold_pct=100.0,
    ),
)

study = surge.contingency.n1_branch_study(
    net,
    surge.ContingencyOptions(screening="lodf", thermal_threshold_pct=100.0),
)
repeat = study.analyze()

print("N-1 LODF:", lodf.n_contingencies, lodf.n_with_violations, lodf.solve_time_secs)
print("N-1 full:", full.n_contingencies, full.n_with_violations, full.solve_time_secs)
print("N-2 sample:", n2.n_contingencies, n2.n_with_violations, n2.solve_time_secs)
print("Reusable study rerun:", repeat.n_contingencies, repeat.n_with_violations)
```

## Rust Experiment

Use the crate root for the common analysis paths and explicit modules for
anything deeper.

```rust
use std::path::Path;

use surge_contingency::{ContingencyOptions, ScreeningMode, analyze_n1_branch, analyze_n2_branch};
use surge_io::load;

fn main() -> anyhow::Result<()> {
    let net = load(Path::new("examples/cases/ieee118/case118.surge.json.zst"))?;

    let lodf = analyze_n1_branch(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Lodf,
            thermal_threshold: 1.0,
            vm_min: 0.95,
            vm_max: 1.05,
            ..Default::default()
        },
    )?;

    let full = analyze_n1_branch(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Off,
            thermal_threshold: 1.0,
            vm_min: 0.95,
            vm_max: 1.05,
            ..Default::default()
        },
    )?;

    let n2 = analyze_n2_branch(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Lodf,
            thermal_threshold: 1.0,
            ..Default::default()
        },
    )?;

    println!(
        "n1 lodf={} n1 full={} n2={}",
        lodf.n_contingencies, full.n_contingencies, n2.n_contingencies
    );

    Ok(())
}
```

`ScreeningMode::Lodf` is currently a true screening accelerator for single-branch
N-1 studies. For multi-outage studies such as `analyze_n2_branch()`, Surge now
fails closed and runs the exact path instead of reusing single-outage LODF
estimates as a potentially lossy N-2 screen.

## What To Compare

- `solve_time_secs` for LODF-screened versus full AC N-1
- `n_with_violations` to see whether screening changed outcomes
- Whether the same small set of contingencies dominates both N-1 and N-2

If the screened and unscreened N-1 runs disagree materially on which cases are
dangerous, treat that as a validation problem before you trust the faster mode
at scale.

## Extensions

- Use `surge.analyze_n1_generator(...)` to compare line-outage versus
  generator-loss risk.
- Use `surge.contingency.n2_branch_study(...)` when you want to reuse the same
  candidate set repeatedly.
- Feed the worst contingencies into [Tutorial 03](03-optimal-power-flow.md) to
  test whether security-constrained dispatch changes the operating point.
