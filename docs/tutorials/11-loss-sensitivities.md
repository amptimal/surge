# Tutorial 11: Loss Sensitivities

If 1 MW is injected at a given bus, how much do total system losses change?

The answer is the **marginal loss factor** (MLF): `MLF[i] = dP_loss / dP_inject_i`.
MLFs determine the loss component of locational marginal prices and define
delivery factors for interconnection studies.

## Questions To Answer

- What is the marginal loss factor at every bus?
- Which buses are the most and least lossy locations for generation?
- How does the loss component of LMP vary across the network?

## Python Experiment

### AC Marginal Loss Factors

`surge.losses.compute_loss_factors` computes exact AC MLFs via one
Jacobian-transpose solve — approximately the cost of a single Newton-Raphson
iteration. Case30 has nonzero branch resistance on every branch, giving
meaningful loss structure.

```python
import surge

net = surge.case30()

lsf = surge.losses.compute_loss_factors(net)

print(f"Total base-case losses: {lsf.base_losses_mw:.2f} MW")
print(f"Number of buses: {len(lsf.bus_numbers)}")
print()
print(f"{'Bus':>5s} {'MLF':>10s}")
for bus, mlf in zip(lsf.bus_numbers, lsf.lsf):
    print(f"{bus:5d} {mlf:+10.6f}")
```

The slack bus MLF is zero by definition — it is the loss accounting reference.
Buses far from generation on resistive paths have larger MLFs.

### Tabular View

```python
df = lsf.to_dataframe()
print("5 highest-loss buses:")
print(df.nlargest(5, "lsf"))
print()
print("5 lowest-loss buses:")
print(df.nsmallest(5, "lsf"))
```

### Verify Against Finite Difference

Add a small generator at one bus and compare the observed loss change to the
analytical MLF.

```python
import surge

net = surge.case30()

lsf = surge.losses.compute_loss_factors(net)

ac_base = surge.solve_ac_pf(net, surge.AcPfOptions())
base_losses = sum(ac_base.p_inject_mw)

delta_mw = 0.1
perturbed = surge.case30()
perturbed.add_generator(bus=30, p_mw=delta_mw, pmax_mw=delta_mw + 1, pmin_mw=0.0, machine_id="FD")
ac_pert = surge.solve_ac_pf(perturbed, surge.AcPfOptions())
pert_losses = sum(ac_pert.p_inject_mw)

fd_mlf = (pert_losses - base_losses) / delta_mw
bus_col = lsf.bus_numbers.index(30)
analytical_mlf = lsf.lsf[bus_col]

print(f"Finite-difference MLF at bus 30: {fd_mlf:+.6f}")
print(f"Analytical MLF at bus 30:        {analytical_mlf:+.6f}")
```

The FD estimate is a secant over a finite step; the analytical MLF is the exact
tangent. AC losses are nonlinear (proportional to I²), so the two will not match
exactly. Smaller perturbations close the gap but cannot eliminate it — this is
why the analytical Jacobian method exists.

### Stressed Operating Point

Pass a pre-computed AC solution to skip the internal solve:

```python
import surge

net = surge.case30()

net.scale_loads(1.10)
ac = surge.solve_ac_pf(net, surge.AcPfOptions(enforce_q_limits=True))

lsf_stressed = surge.losses.compute_loss_factors(net, solution=ac)

print(f"Stressed losses: {lsf_stressed.base_losses_mw:.2f} MW")
print(f"Max MLF: {max(lsf_stressed.lsf):.6f}")
```

### Loss-Aware DC-OPF (Penalty Factor Method)

DC power flow is lossless by definition — `total_losses_mw` is always zero.
The penalty-factor loss model adjusts each generator's effective cost so the
optimizer favors low-loss locations. The dispatch may or may not shift depending
on the network; the primary output is the nonzero loss component in the LMP
decomposition.

```python
import surge
from surge.opf import DcOpfOptions, DcLossModel, DcOpfRuntime

net = surge.case30()

dc_lossless = surge.solve_dc_opf(
    net,
    options=DcOpfOptions(
        enforce_thermal_limits=True,
        loss_model=DcLossModel.IGNORE,
    ),
    runtime=DcOpfRuntime(lp_solver="highs"),
)

dc_lossy = surge.solve_dc_opf(
    net,
    options=DcOpfOptions(
        enforce_thermal_limits=True,
        loss_model=DcLossModel.ITERATIVE,
        loss_iterations=3,
        loss_tolerance=1e-3,
    ),
    runtime=DcOpfRuntime(lp_solver="highs"),
)

print(f"{'':25s} {'Lossless':>12s} {'Loss-aware':>12s}")
print(f"{'Total cost ($/hr)':25s} {dc_lossless.total_cost:12.2f} {dc_lossy.total_cost:12.2f}")
print()
print(f"{'Gen bus':>10s} {'Lossless':>12s} {'Loss-aware':>12s}")
for gl, ga in zip(dc_lossless.generators, dc_lossy.generators):
    print(f"{gl.bus:10d} {gl.p_mw:12.2f} {ga.p_mw:12.2f}")
```

### LMP Decomposition

The loss component of each bus's LMP reflects how its network position
contributes to system losses.

```python
print(f"{'Bus':>5s} {'LMP':>10s} {'Energy':>10s} {'Congestion':>10s} {'Loss':>10s}")
for b in dc_lossy.buses[:10]:
    print(f"{b.number:5d} {b.lmp:10.4f} {b.lmp_energy:10.4f} "
          f"{b.lmp_congestion:10.4f} {b.lmp_loss:10.4f}")

print()
print(f"Max |loss component|: {max(abs(b.lmp_loss) for b in dc_lossy.buses):.4f} $/MWh")
print(f"Max |cong component|: {max(abs(b.lmp_congestion) for b in dc_lossy.buses):.4f} $/MWh")
```

## Rust Experiment

```rust
use std::path::Path;

use surge_ac::{AcPfOptions, solve_ac_pf};
use surge_io::load;
use surge_network::network::BusType;
use surge_opf::compute_ac_marginal_loss_factors;

fn main() -> anyhow::Result<()> {
    let net = load(Path::new("examples/cases/ieee30/case30.surge.json.zst"))?;

    let ac = solve_ac_pf(&net, &AcPfOptions::default())?;

    let slack_idx = net
        .buses
        .iter()
        .position(|b| b.bus_type == BusType::Slack)
        .expect("no slack bus");

    let mlf = compute_ac_marginal_loss_factors(
        &net,
        &ac.voltage_angle_rad,
        &ac.voltage_magnitude_pu,
        slack_idx,
    )?;

    println!("Bus    MLF");
    for (i, bus) in net.buses.iter().enumerate() {
        if mlf[i].abs() > 0.001 {
            println!("{:5}  {:+.6}", bus.number, mlf[i]);
        }
    }

    Ok(())
}
```

## What To Record

- Spatial pattern of MLFs — buses far from generation on resistive paths have
  the largest loss factors
- How total losses and MLF magnitudes change between base and stressed cases
- Relative size of the loss component versus the congestion component in the
  LMP decomposition
- Delivery factor at each bus: `DF[i] = 1 - MLF[i]`

## Extensions

- Compute delivery factors (`1 - MLF`) for interconnection screening —
  generators at low-delivery-factor buses impose higher system losses.
- Compare AC MLFs at base and 110% load to see how loss factors shift under
  stress.
- Feed the loss-aware DC-OPF into [Tutorial 03](03-optimal-power-flow.md) to
  compare loss-adjusted DC costs against full AC-OPF.
- DC loss sensitivities use PTDF internally —
  see [Tutorial 08](08-shift-factors.md).
