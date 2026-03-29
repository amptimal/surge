# Tutorial 04: Transfer Capability And Voltage Stress Experiment

This tutorial focuses on transfer capability and voltage stress for a simple
operations question:

How much transfer headroom do I have, and how does voltage stress move when I
push the system harder?

## Questions To Answer

- What is the base-case ATC for a candidate transfer path?
- Which flowgates or contingencies limit that path?
- Does voltage stress rise before or after thermal ATC becomes binding?

## Python Experiment

The Python API for this workflow is namespaced:

- `surge.transfer` for transfer studies
- `surge.contingency` for voltage-stress and advanced reliability helpers

We use the IEEE 30-bus case because it has thermal ratings on every branch,
giving meaningful ATC results with real binding constraints.

```python
import surge
import numpy as np

base = surge.case30()
stressed = surge.case30()
stressed.scale_loads(1.10)

# Transfer from bus 1 (area 1 slack) to bus 30 (area 3 load).
# This path crosses two area boundaries and bottlenecks on the
# 16 MVA corridor between buses 27-29-30.
path = surge.transfer.TransferPath("area1_to_area3", [1], [30])

base_study = surge.transfer.prepare_transfer_study(base)
stressed_study = surge.transfer.prepare_transfer_study(stressed)

# Case30 has 3 bridge branches (9→11, 12→13, 25→26) whose outage
# islands part of the network. Including them as contingencies
# drives ATC to zero. Identify them via infinite LODF diagonal.
lodf_mat = surge.dc.compute_lodf_matrix(base)
bridge_indices = [
    i for i in range(base.n_branches)
    if np.isinf(lodf_mat.lodf[i, i]) or np.isnan(lodf_mat.lodf[i, i])
]
non_bridge = [i for i in range(base.n_branches) if i not in bridge_indices]

# N-1 with all contingencies (islanding drives ATC to 0)
all_ctg = surge.transfer.AtcOptions(
    monitored_branches=list(range(base.n_branches)),
    contingency_branches=list(range(base.n_branches)),
)
base_all = base_study.compute_nerc_atc(path, all_ctg)

# N-1 excluding bridge contingencies (realistic operational set)
ops_ctg = surge.transfer.AtcOptions(
    monitored_branches=list(range(base.n_branches)),
    contingency_branches=non_bridge,
)
base_ops = base_study.compute_nerc_atc(path, ops_ctg)
stressed_ops = stressed_study.compute_nerc_atc(path, ops_ctg)

base_stress = surge.contingency.compute_voltage_stress(base)
stressed_stress = surge.contingency.compute_voltage_stress(stressed)

print(f"{'':25s} {'Base':>10s} {'Stressed':>10s}")
print(f"{'ATC all ctg (MW)':25s} {base_all.atc_mw:10.1f}")
print(f"{'ATC ops ctg (MW)':25s} {base_ops.atc_mw:10.1f} {stressed_ops.atc_mw:10.1f}")
print(f"{'TTC (MW)':25s} {base_ops.ttc_mw:10.1f} {stressed_ops.ttc_mw:10.1f}")
print(f"{'Binding branch':25s} {str(base_ops.binding_branch):>10s} {str(stressed_ops.binding_branch):>10s}")
print(f"{'Binding contingency':25s} {str(base_ops.binding_contingency):>10s} {str(stressed_ops.binding_contingency):>10s}")
print(f"{'Max L-index':25s} {base_stress.max_l_index:10.4f} {stressed_stress.max_l_index:10.4f}")
print(f"\nBridge branches excluded: {bridge_indices}")
```

When all N-1 contingencies are included, islanding contingencies drive ATC to
zero. Excluding islanding (bridge) contingencies from the credible set gives
meaningful results. With that filter, the bus 1→30 path has ~2.9 MW of ATC,
bottlenecked on the 27→30 corridor for the loss of branch 36 (27→29).

## Comparing Multiple Paths

A single study object can evaluate several transfer paths efficiently. Here we
compare the full and operational contingency sets:

```python
paths = [
    surge.transfer.TransferPath("area1_to_area3", [1], [30]),
    surge.transfer.TransferPath("gen2_to_load26", [2], [26]),
    surge.transfer.TransferPath("gen1_to_load24", [1], [24]),
]

print(f"{'Path':20s} {'All ctg':>10s} {'Ops ctg':>10s} {'Binding br':>12s} {'Ctg':>6s}")
for p in paths:
    ra = base_study.compute_nerc_atc(p, all_ctg)
    ro = base_study.compute_nerc_atc(p, ops_ctg)
    bb = str(ro.binding_branch) if ro.binding_branch is not None else "-"
    bc = str(ro.binding_contingency) if ro.binding_contingency is not None else "N-0"
    print(f"{p.name:20s} {ra.atc_mw:10.1f} {ro.atc_mw:10.1f} {bb:>12s} {bc:>6s}")
```

## Alternative: Matrix-Level Sensitivities

If you need the transfer workflow broken down into reusable matrix pieces, keep
that work under the transfer namespace too:

```python
import surge

net = surge.case30()

bldf = surge.transfer.compute_bldf(net)
gsf = surge.transfer.compute_gsf(net)
inj = surge.transfer.compute_injection_capability(net)

print(bldf.matrix.shape)
print(gsf.gsf.shape)
print(inj.by_bus[:5])
```

## What To Compare

- base versus stressed `atc_mw`
- whether `ttc_mw` falls before the exact L-index spikes
- which branch and contingency pair becomes binding as load rises

If ATC collapses while the exact L-index is still mild, the constraint is
likely thermal or outage-driven. If the exact L-index rises sharply first, the
limiting mechanism is likely reactive or voltage-security related.

## Extensions

- Use multiple `TransferPath` definitions to compare parallel commercial paths.
- Add AFC or multi-transfer experiments through the same prepared study object.
- Feed the stressed case into [Tutorial 03](03-optimal-power-flow.md) to see
  whether AC-OPF or SCOPF can recover headroom through dispatch changes.
