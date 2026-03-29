# Tutorial 03: OPF Tradeoff Experiment

This tutorial compares three operating-point answers for the same network:

- unconstrained or lightly constrained economic dispatch via DC-OPF
- physically richer dispatch via AC-OPF
- security-aware dispatch via SCOPF

## Questions To Answer

- How much does AC physics change total cost and price shape relative to DC?
- Which contingencies become binding once security is enforced?
- When do you need DC, AC, or SCOPF for the operating question in front of you?

## Python Experiment

Use the root APIs for DC-OPF, AC-OPF, and SCOPF.

```python
import surge

net = surge.case118()

dc = surge.solve_dc_opf(
    net,
    options=surge.DcOpfOptions(
        enforce_thermal_limits=True,
        minimum_branch_rating_a_mva=1.0,
    ),
    runtime=surge.DcOpfRuntime(
        lp_solver="highs",
    ),
)

ac = surge.solve_ac_opf(
    net,
    options=surge.AcOpfOptions(
        enforce_thermal_limits=True,
        enforce_flowgates=False,
    ),
    runtime=surge.AcOpfRuntime(
        nlp_solver="ipopt",
        exact_hessian=True,
        print_level=0,
    ),
)

scopf = surge.solve_scopf(
    net,
    options=surge.ScopfOptions(
        formulation=surge.ScopfFormulation.DC,
        mode=surge.ScopfMode.PREVENTIVE,
        contingency_rating=surge.ThermalRating.RATE_A,
    ),
    runtime=surge.ScopfRuntime(
        lp_solver="highs",
        max_iterations=10,
    ),
)

print("DC cost:", dc.total_cost)
print("AC cost:", ac.total_cost)
print("SCOPF cost:", scopf.total_cost)
print("First 5 DC LMPs:", dc.lmp[:5])
print("First 5 AC LMPs:", ac.lmp[:5])
print("Binding contingencies:", len(scopf.binding_contingencies))
```

## Rust Experiment

```rust
use std::path::Path;

use surge_io::load;
use surge_opf::{
    AcOpfOptions, AcOpfRuntime, DcOpfOptions, DcOpfRuntime, ScopfFormulation, ScopfMode,
    ScopfOptions, ScopfRuntime, ThermalRating, solve_ac_opf, solve_dc_opf, solve_scopf,
};

fn main() -> anyhow::Result<()> {
    let net = load(Path::new("examples/cases/ieee118/case118.surge.json.zst"))?;

    let dc = solve_dc_opf(
        &net,
        &DcOpfOptions {
            enforce_thermal_limits: true,
            min_rate_a: 1.0,
            ..Default::default()
        },
    )?;

    let ac = surge_opf::solve_ac_opf_with_runtime(
        &net,
        &AcOpfOptions {
            enforce_thermal_limits: true,
            ..Default::default()
        },
        &AcOpfRuntime {
            nlp_solver: Some("ipopt".into()),
            exact_hessian: true,
            print_level: 0,
            ..Default::default()
        },
    )?;

    let scopf = surge_opf::solve_scopf_with_runtime(
        &net,
        &ScopfOptions {
            formulation: ScopfFormulation::Dc,
            mode: ScopfMode::Preventive,
            contingency_rating: ThermalRating::RateA,
            ..Default::default()
        },
        &ScopfRuntime {
            lp_solver: Some("highs".into()),
            max_iterations: 10,
            ..Default::default()
        },
    )?;

    println!("dc cost = {}", dc.total_cost);
    println!("ac cost = {}", ac.total_cost);
    println!("scopf cost = {}", scopf.total_cost);

    Ok(())
}
```

## What To Compare

- `total_cost` across DC, AC, and SCOPF
- the shape of `lmp`, not just the average
- how many contingencies in SCOPF bind and whether they match your worst N-1 results

If DC-OPF and AC-OPF disagree on the operationally important buses or binding
interfaces, trust the AC result for the final answer and use DC as the planning
screen.

## Extensions

- Re-run the experiment with tighter branch ratings or higher load.
