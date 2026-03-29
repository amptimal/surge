# Tutorial 05: Python Workbench Experiment

This tutorial gives a quick overview of the Python package layout. The package
follows three simple rules:

- use the root `surge` package for the most common study entry points
- use explicit namespaces for deeper workflows
- pass typed options, runtime, and request objects instead of loose kwargs

## Surface Map

| Need | Package path |
|---|---|
| Load and save cases | `surge.load`, `surge.save`, `surge.io.*` |
| Common power flow | `surge.solve_ac_pf`, `surge.solve_dc_pf` |
| Additional power flow methods | `surge.powerflow.*` |
| Common OPF workflows | `surge.solve_dc_opf`, `surge.solve_ac_opf`, `surge.solve_scopf` |
| Common contingency workflows | `surge.analyze_n1_branch`, `surge.analyze_n1_generator`, `surge.analyze_n2_branch` |
| Prepared contingency workflows | `surge.contingency.*` |
| Transfer capability | `surge.transfer.*` |
| Batch studies | `surge.batch.*` |
| Pandas-native model ingestion | `surge.construction.*` |
| Named bus-set filtering | `surge.subsystem.*` |

## Experiment 1: Root Solve Versus Namespaced Power Flow

```python
import surge

net = surge.case118()

ac = surge.solve_ac_pf(
    net,
    surge.AcPfOptions(
        flat_start=False,
        dc_warm_start=True,
    ),
)

fdpf = surge.powerflow.solve_fdpf(
    net,
    surge.powerflow.FdpfOptions(
        variant="bx",
        tolerance=1e-6,
    ),
)

print("AC converged:", ac.converged, "iterations:", ac.iterations)
print("FDPF converged:", fdpf.converged, "iterations:", fdpf.iterations)
```

The root package holds the most common study entry points. The namespace holds
methods that are used less often or need extra structure.

## Experiment 2: Typed OPF Options And Runtimes

```python
import surge

net = surge.case118()

dc = surge.solve_dc_opf(
    net,
    options=surge.DcOpfOptions(
        enforce_thermal_limits=True,
    ),
    runtime=surge.DcOpfRuntime(
        lp_solver="highs",
    ),
)

ac = surge.solve_ac_opf(
    net,
    options=surge.AcOpfOptions(
        enforce_thermal_limits=True,
    ),
    runtime=surge.AcOpfRuntime(
        nlp_solver="ipopt",
        exact_hessian=True,
    ),
)

print("DC cost:", dc.total_cost)
print("AC cost:", ac.total_cost)
```

Do not pass ad hoc keyword overrides to the root OPF solves. The package-level
contract is `(network, options=None, runtime=None)`.

## Experiment 3: Prepared Domain Workflows

```python
import surge

net = surge.case118()

dc = surge.dc.prepare_study(net)
transfer = surge.transfer.prepare_transfer_study(net)
cont = surge.contingency.n1_branch_study(
    net,
    surge.ContingencyOptions(screening="lodf"),
)

monitored = [
    surge.dc.BranchKey(branch.from_bus, branch.to_bus, branch.circuit)
    for branch in net.branches[: min(10, net.n_branches)]
]

ptdf = dc.compute_ptdf(
    surge.dc.PtdfRequest(
        monitored_branches=monitored,
        bus_numbers=[1, 5, 10],
    )
)

path = surge.transfer.TransferPath("north_to_south", [8], [1])
atc = transfer.compute_nerc_atc(
    path,
    surge.transfer.AtcOptions(
        monitored_branches=list(range(net.n_branches)),
        contingency_branches=list(range(net.n_branches)),
    ),
)

analysis = cont.analyze()

print("PTDF shape:", ptdf.ptdf.shape)
print("ATC:", atc.atc_mw)
print("Contingencies:", analysis.n_contingencies)
```

Use prepared objects when you will run the same structural study repeatedly on
one network. Otherwise, stay with the root solve.

## Experiment 4: Batch Studies

```python
import surge

cases = [surge.case9(), surge.case14(), surge.case30()]

results = surge.batch.batch_solve(
    cases,
    solver="acpf",
    options=surge.AcPfOptions(tolerance=1e-8),
)

for result in results.results:
    print(result.case_name, result.wall_time_s, result.error)
```

For OPF batch runs, pass only `options=` and `runtime=`. Power-flow batch runs
accept only `options=`.

## Experiment 5: Pandas Construction And Subsystems

```python
import pandas as pd
import surge

buses = pd.DataFrame(
    [
        {"number": 1, "type": "Slack", "base_kv": 230.0},
        {"number": 2, "type": "PV", "base_kv": 230.0},
        {"number": 3, "type": "PQ", "base_kv": 230.0, "pd_mw": 90.0, "qd_mvar": 30.0},
    ]
)
branches = pd.DataFrame(
    [
        {"from_bus": 1, "to_bus": 2, "r": 0.01, "x": 0.08},
        {"from_bus": 2, "to_bus": 3, "r": 0.01, "x": 0.09},
    ]
)
generators = pd.DataFrame(
    [
        {"bus": 1, "pg": 90.0, "pmax": 140.0},
        {"bus": 2, "pg": 35.0, "pmax": 70.0},
    ]
)

net = surge.construction.from_dataframes(buses, branches, generators)
sub = surge.subsystem.Subsystem(net, name="load-pocket", buses=[2, 3])

print(net.n_buses, net.n_branches, net.n_generators)
print(sub.bus_numbers)
print(sub.tie_branches)
```

Use `surge.construction` when your data already lives in pandas. Use
`surge.subsystem` when you need stable bus-set filters for reporting, transfer,
or contingency follow-on analysis.

## What Not To Expect At Root

These names are either namespaced or not exposed at the root package:

- `surge.solve_fdpf`
- `surge.compute_nerc_atc`
- `surge.compute_voltage_stress`
- `surge.batch_solve`

If you reach for one of those names, you are probably trying to use an old
interface or the wrong namespace.

## Next Steps

- Use [Tutorial 01](01-basic-power-flow.md) for convergence and initialization experiments.
- Use [Tutorial 03](03-optimal-power-flow.md) for dispatch-cost tradeoffs.
- Use [Tutorial 09](09-pandas-construction.md) for DataFrame schema details.
- Use [Tutorial 10](10-subsystems.md) for bus-set filtering patterns.
