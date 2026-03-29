# Tutorial 08: Shift Factors For Branches, Interfaces, And Flowgates

This tutorial answers a fundamental question in power systems operations:

If I inject 1 MW at a given bus, how much of that power flows through a
particular branch, interface, or flowgate?

The answer is the **shift factor** (also called PTDF — Power Transfer
Distribution Factor). Shift factors underpin congestion pricing, flowgate
monitoring, generator interconnection studies, and transfer capability analysis.

## Questions To Answer

- What is the shift factor of every bus with respect to a specific branch?
- How do I combine branch-level shift factors into an interface shift factor?
- How does the choice of slack distribution change the shift factor picture?
- What is a flowgate, and how does its shift factor differ from a branch or interface?

## Python Experiment

The sensitivity API lives under `surge.dc`. We start with the 9-bus case for
intuition, then move to IEEE 30-bus for a more realistic flowgate example.

### Single-Branch Shift Factors

```python
import surge
from surge.dc import BranchKey, PtdfRequest

net = surge.case9()

# Pick a branch to monitor: bus 4 → bus 5, circuit "1"
branch = BranchKey(4, 5)

ptdf = surge.dc.compute_ptdf(net, PtdfRequest(
    monitored_branches=(branch,),
))

print("PTDF shape:", ptdf.ptdf.shape)      # (1, n_buses)
print("Bus numbers:", ptdf.bus_numbers)

# Shift factor of each bus on the 4→5 branch
for bus, sf in zip(ptdf.bus_numbers, ptdf.ptdf[0]):
    print(f"  Bus {bus:3d}: {sf:+.4f}")
```

The shift factor at the slack bus is zero by definition (under single-slack
semantics). Buses electrically close to the monitored branch have larger
absolute values.

### Verify Against DC Power Flow

A quick sanity check: perturb injection at one bus and confirm the flow change
matches the shift factor.

```python
import surge
from surge.dc import BranchKey, PtdfRequest

net = surge.case9()
branch = BranchKey(4, 5)

ptdf = surge.dc.compute_ptdf(net, PtdfRequest(
    monitored_branches=(branch,),
))

# Baseline DC power flow
dc_base = surge.solve_dc_pf(net, surge.DcPfOptions())

# Perturb: add 10 MW injection at bus 5 (reduce load by 10 MW)
perturbed = surge.case9()
original_load = perturbed.bus_pd[perturbed.bus_numbers.index(5)]
perturbed.set_bus_load(5, original_load - 10.0)
dc_pert = surge.solve_dc_pf(perturbed, surge.DcPfOptions())

branch_idx = net.branch_index(4, 5, "1")
delta_flow = dc_pert.branch_p_mw[branch_idx] - dc_base.branch_p_mw[branch_idx]
bus_col = ptdf.bus_numbers.index(5)
predicted = ptdf.ptdf[0, bus_col] * 10.0

print(f"Actual flow change:    {delta_flow:+.4f} MW")
print(f"PTDF prediction:       {predicted:+.4f} MW")
```

### Interface Shift Factors

An interface is a weighted combination of branches that defines a transmission
boundary — for example, the total flow across a corridor between two areas. Its
shift factor is the matching weighted sum of branch PTDFs.

```python
import numpy as np
import surge
from surge.dc import BranchKey, PtdfRequest

net = surge.case30()

# Define an interface: 60% of branch 27→30 + 40% of branch 29→30
iface_branches = (BranchKey(27, 30), BranchKey(29, 30))
iface_weights = np.array([0.6, 0.4])

ptdf = surge.dc.compute_ptdf(net, PtdfRequest(
    monitored_branches=iface_branches,
))

# Interface shift factor = weighted sum of branch PTDF rows
iface_sf = iface_weights @ ptdf.ptdf  # shape: (n_buses,)

print("Top 5 buses by absolute interface shift factor:")
order = np.argsort(-np.abs(iface_sf))
for rank, col in enumerate(order[:5]):
    print(f"  Bus {ptdf.bus_numbers[col]:3d}: {iface_sf[col]:+.4f}")
```

### Distributed Slack

Single-slack shift factors assume all mismatch is absorbed at the reference bus.
In practice, generation is shared across units. The `SlackPolicy` parameter
reshapes the PTDF to reflect participation factors.

```python
import surge
from surge.dc import BranchKey, PtdfRequest, SlackPolicy

net = surge.case30()
branch = BranchKey(6, 8)

# Single slack (default)
sf_single = surge.dc.compute_ptdf(net, PtdfRequest(
    monitored_branches=(branch,),
    slack=SlackPolicy.single(),
))

# Weighted distributed slack: 50% bus 1, 30% bus 2, 20% bus 13
sf_dist = surge.dc.compute_ptdf(net, PtdfRequest(
    monitored_branches=(branch,),
    slack=SlackPolicy.weights({1: 0.5, 2: 0.3, 13: 0.2}),
))

# Headroom-based slack: all generators participate proportional to headroom
sf_headroom = surge.dc.compute_ptdf(net, PtdfRequest(
    monitored_branches=(branch,),
    slack=SlackPolicy.headroom(),
))

print("Shift factor at bus 5:")
col = sf_single.bus_numbers.index(5)
print(f"  Single slack:   {sf_single.ptdf[0, col]:+.4f}")
print(f"  Weighted slack: {sf_dist.ptdf[0, col]:+.4f}")
print(f"  Headroom slack: {sf_headroom.ptdf[0, col]:+.4f}")
```

Under distributed slack, the slack-bus column is no longer zero — every bus
gets a nonzero shift factor because the balancing response is spread out.

### Flowgate Shift Factors

A flowgate is a **monitored element paired with a specific contingency**. The
monitored element can be a single branch or an interface. What makes it a
flowgate — rather than just a branch or interface — is that its binding
constraint only matters under that contingency. The flowgate shift factor is
therefore an OTDF (Outage Transfer Distribution Factor), not a base-case PTDF.

Here we define a flowgate on the IEEE 30-bus case: monitor branch 27→30 for
the contingency loss of branch 29→30.

```python
import numpy as np
import surge
from surge.dc import BranchKey, PtdfRequest, OtdfRequest

net = surge.case30()

monitored = BranchKey(27, 30)
contingency = BranchKey(29, 30)

# Base-case shift factor (branch only, no contingency — not a flowgate)
ptdf = surge.dc.compute_ptdf(net, PtdfRequest(
    monitored_branches=(monitored,),
))

# Flowgate shift factor: monitored element + contingency = OTDF
otdf = surge.dc.compute_otdf(net, OtdfRequest(
    monitored_branches=(monitored,),
    outage_branches=(contingency,),
))

pre = ptdf.ptdf[0]
post = otdf.otdf[0, 0]

print("Buses with largest shift factor increase under flowgate contingency:")
delta = np.abs(post) - np.abs(pre)
order = np.argsort(-delta)
for col in order[:5]:
    bus = ptdf.bus_numbers[col]
    print(f"  Bus {bus:3d}: {pre[col]:+.4f} → {post[col]:+.4f}  (Δ = {delta[col]:+.4f})")
```

When the contingency trips branch 29→30, flow that previously split between
29→30 and 27→30 now concentrates on 27→30 alone. The OTDF captures this
redistribution. Flowgate monitoring works the same way: precompute OTDFs for
each credible contingency and check whether post-contingency flow would violate
the flowgate limit.

### Prepared Study (Performance)

When computing multiple sensitivity products, factor the network once and reuse:

```python
import surge
from surge.dc import BranchKey, PtdfRequest, LodfRequest, OtdfRequest

net = surge.case118()
study = surge.dc.prepare_study(net)

# One factorization, many queries
ptdf = study.compute_ptdf()
lodf = study.compute_lodf()
otdf = study.compute_otdf(OtdfRequest(
    monitored_branches=(BranchKey(69, 75), BranchKey(69, 77)),
    outage_branches=(BranchKey(65, 68),),
))

print("PTDF:", ptdf.ptdf.shape)
print("LODF:", lodf.lodf.shape)
print("OTDF:", otdf.otdf.shape)
```

The `to_dataframe()` method on `PtdfResult` is convenient for tabular analysis:

```python
df = ptdf.to_dataframe()
print(df.head())
```

## Rust Experiment

The equivalent workflow in Rust uses the `surge_dc` crate directly.

```rust
use std::path::Path;

use surge_dc::{
    DcSensitivityOptions, OtdfRequest, PtdfRequest, compute_otdf, compute_ptdf,
};
use surge_io::load;

fn main() -> anyhow::Result<()> {
    let net = load(Path::new("examples/cases/ieee30/case30.surge.json.zst"))?;

    // Single-branch PTDF for branch index 36 (bus 27→30)
    let ptdf = compute_ptdf(
        &net,
        &PtdfRequest::for_branches(&[36]),
    )?;

    println!("PTDF row for branch 36 ({} buses):", ptdf.n_cols());
    let row = ptdf.row_at(0);
    for (i, &bus_idx) in ptdf.bus_indices().iter().enumerate() {
        if row[i].abs() > 0.01 {
            println!("  bus_idx={bus_idx}: {:.4}", row[i]);
        }
    }

    // Post-contingency OTDF: monitor branch 36, outage branch 35
    let otdf = compute_otdf(
        &net,
        &OtdfRequest::new(&[36], &[35]),
    )?;

    println!("\nOTDF (branch 36 | outage 35):");
    let vec = otdf.vector_at(0, 0);
    for (i, &bus_idx) in otdf.bus_indices().iter().enumerate() {
        let pre = ptdf.get(36, bus_idx);
        let post = vec[i];
        if (post - pre).abs() > 0.005 {
            println!("  bus_idx={bus_idx}: {pre:.4} → {post:.4}");
        }
    }

    // Distributed slack via participation weights
    let ptdf_dist = compute_ptdf(
        &net,
        &PtdfRequest::for_branches(&[36])
            .with_options(DcSensitivityOptions::with_slack_weights(&[
                (0, 0.5),  // bus index 0
                (1, 0.3),  // bus index 1
                (4, 0.2),  // bus index 4
            ])),
    )?;

    println!("\nDistributed-slack PTDF row for branch 36:");
    let row_dist = ptdf_dist.row_at(0);
    for (i, &bus_idx) in ptdf_dist.bus_indices().iter().enumerate() {
        if row_dist[i].abs() > 0.01 {
            println!("  bus_idx={bus_idx}: {:.4}", row_dist[i]);
        }
    }

    Ok(())
}
```

## What To Record

- Which buses dominate flow on the monitored element — these are the buses
  whose generation or load changes have the biggest congestion impact
- How distributed slack dampens shift factors compared to single slack — in
  real networks the difference can be substantial
- How contingencies amplify shift factors — an OTDF can exceed the base-case
  PTDF by a large margin when the outaged branch was a parallel path

## Extensions

- Feed shift factor vectors into a congestion revenue rights (CRR) or financial
  transmission rights (FTR) valuation workflow.
- Use the GSF matrix (`surge.transfer.compute_gsf`) for the generator-indexed
  variant of these same sensitivities — see
  [Tutorial 04](04-transfer-capability.md).
- Combine PTDF with branch ratings to screen for the binding branch under a
  proposed generation dispatch change.
- Move on to [Tutorial 02](02-contingency-analysis.md) to see how LODF and OTDF
  are used in automated N-1 contingency screening.
