# surge-py

Python bindings for Surge.

**surge-py** provides the curated Python interface to [Surge](https://github.com/amptimal/surge), a Rust-native power systems analysis engine. The package surface is intentionally small at the root and uses explicit namespaces for specialist workflows such as transfer studies, prepared contingency studies, batch runs, and advanced power-flow methods.

## Installation

```bash
# Distribution name declared in pyproject metadata
pip install surge-py

# From source (requires Rust toolchain + maturin)
cd src/surge-py
pip install maturin
maturin develop --release
```

When `surge-py` is built with `COPT_HOME` pointing at a COPT 8.x install, the
wheel bundles the Surge COPT NLP shim and the `surge` package auto-configures
`SURGE_COPT_NLP_SHIM_PATH` at import time. End users still need a working COPT
runtime installation and license to run `nlp_solver="copt"`, but they do not
need to build a separate shim by hand.

Source builds keep optional commercial backends runtime-loaded. If you want the
build to fail unless the packaged COPT NLP shim is bundled, require it
explicitly:

```bash
COPT_HOME=/opt/copt80 SURGE_PY_REQUIRE_COPT_NLP_SHIM=1 maturin develop --release
```

### System dependencies (build from source)

- Rust stable toolchain
- `libsuitesparse-dev` (KLU sparse solver)
- `libipopt-dev` (optional, open-source AC-OPF backend)
- COPT 8.x install + license (optional, commercial AC-OPF backend)

## Quick Start

```python
import surge

# Load a MATPOWER/PSS-E/CIM case
net = surge.load("case118.m")
print(f"{net.n_buses} buses, {net.n_branches} branches")

# AC power flow (Newton-Raphson)
sol = surge.solve_ac_pf(net)
print(f"Converged: {sol.converged}, max mismatch: {sol.max_mismatch:.2e}")

# Results as pandas DataFrame when pandas is installed
df = sol.to_dataframe()
print(df[["bus_id", "vm_pu", "va_deg"]].head())

# DC optimal power flow
opf = surge.solve_dc_opf(net)
print(f"Total cost: ${opf.total_cost:.2f}/hr")
print(f"LMPs: {opf.lmp}")  # numpy array

# N-1 contingency analysis
ca = surge.analyze_n1_branch(net)
print(f"{ca.n_with_violations} contingencies with violations")
```

`to_dataframe()` returns a pandas DataFrame when pandas is installed and a plain
column dictionary otherwise.

The repository metadata declares the distribution name `surge-py` and the import module `surge`. Publication status is a release-process question, not an unresolved naming question.

## Pandas Network Construction

For pandas-native workflows, build a network directly from DataFrames:

```python
import pandas as pd
import surge

buses = pd.DataFrame(
    [
        {"number": 1, "type": "Slack", "base_kv": 230.0, "name": "SWING"},
        {"number": 2, "type": "PV", "base_kv": 230.0, "name": "GEN"},
        {"number": 3, "type": "PQ", "base_kv": 230.0, "name": "LOAD", "pd_mw": 90.0, "qd_mvar": 30.0},
    ]
)
branches = pd.DataFrame(
    [
        {"from_bus": 1, "to_bus": 2, "r": 0.01, "x": 0.10, "rate_a": 250.0},
        {"from_bus": 2, "to_bus": 3, "r": 0.01, "x": 0.08, "rate_a": 250.0},
        {"from_bus": 1, "to_bus": 3, "r": 0.02, "x": 0.12, "rate_a": 250.0},
    ]
)
generators = pd.DataFrame(
    [
        {"bus": 1, "pg": 100.0, "pmax": 150.0, "qmax": 80.0, "qmin": -80.0},
        {"bus": 2, "pg": 40.0, "pmax": 80.0, "vs": 1.02},
    ]
)

net = surge.construction.from_dataframes(buses, branches, generators, name="pandas-demo")
sol = surge.solve_ac_pf(net)
print(sol.converged)
```

`surge.construction.from_dataframes(...)` follows a MATPOWER-like schema:

- `buses`: required `number`, `base_kv`; optional `type`, `name`, `pd_mw`, `qd_mvar`, `vm_pu`, `va_deg`
- `branches`: required `from_bus`, `to_bus`, `r`, `x`; optional `b`, `rate_a`, `tap`, `shift`, `circuit`
- `generators`: required `bus`, `pg`; optional `pmax`, `pmin`, `qmax`, `qmin`, `vs`, `machine_id`

This helper is intentionally narrow for 0.1. It does not ingest separate load
tables, shunts, HVDC, topology assets, or market objects. See
`docs/tutorials/09-pandas-construction.md` for the full schema and error
semantics.

## Subsystems

`surge.subsystem.Subsystem` gives you a named, reusable bus-set view over a
network for area/zone, voltage-level, or explicit-bus filtering:

```python
import surge

net = surge.case118()
extra_high = surge.subsystem.Subsystem(net, name="ehv", kv_min=345.0)
area_one_load = surge.subsystem.Subsystem(net, areas=[1], bus_type="PQ")

print(extra_high.bus_numbers[:5])
print(area_one_load.total_load_mw)
print(area_one_load.tie_branches[:3])
```

Subsystem filters intersect. `branches` includes only lines whose endpoints are
both inside the bus set, while `tie_branches` includes exactly one-in / one-out
connections. See `docs/tutorials/10-subsystems.md` for examples.

## Prepared DC Studies

For repeated DC power flow and sensitivity work on one network, prepare the DC
study once and reuse it. The prepared study supports both single-island and
multi-island AC networks:

```python
import surge

net = surge.load("case118.m")
dc = surge.dc.prepare_study(net)

monitored = [
    surge.dc.BranchKey(net.branches[i].from_bus, net.branches[i].to_bus, net.branches[i].circuit)
    for i in (0, 3, 7)
]
outages = [
    surge.dc.BranchKey(net.branches[i].from_bus, net.branches[i].to_bus, net.branches[i].circuit)
    for i in (1, 5)
]

pf = dc.solve_pf()
ptdf = dc.compute_ptdf(
    surge.dc.PtdfRequest(monitored_branches=monitored, bus_numbers=[1, 5, 10])
)
lodf = dc.compute_lodf(
    surge.dc.LodfRequest(monitored_branches=monitored, outage_branches=outages)
)
workflow = dc.run_analysis(
    surge.dc.DcAnalysisRequest(
        monitored_branches=monitored,
        lodf_outage_branches=outages,
        n2_outage_pairs=[
            (monitored[0], monitored[1]),
            (monitored[1], monitored[0]),
        ],
    )
)
```

## Prepared Transfer Studies

For repeated transfer work on one network, prepare the transfer study once and
reuse it across ATC, AFC, and multi-interface runs:

```python
import surge

net = surge.load("case118.m")
study = surge.transfer.prepare_transfer_study(net)

path = surge.transfer.TransferPath("north_to_south", [8], [1])
atc = study.compute_nerc_atc(
    path,
    surge.transfer.AtcOptions(
        monitored_branches=list(range(net.n_branches)),
        contingency_branches=list(range(net.n_branches)),
    ),
)
afc = study.compute_afc(
    path,
    [surge.transfer.Flowgate("fg0", 10, 500.0)],
)
multi = study.compute_multi_transfer(
    [
        surge.transfer.TransferPath("north", [8], [1]),
        surge.transfer.TransferPath("west", [10], [2]),
    ],
    weights=[1.0, 1.5],
)
```

For repeated contingency screening on one network, build a study object once,
run it on demand, and reuse the latest analysis for follow-on corrective
redispatch. The same `ContingencyStudy` surface is used for branch N-1,
generator N-1, and branch N-2:

```python
import surge

net = surge.load("case118.m")
n1 = surge.contingency.n1_branch_study(net, surge.ContingencyOptions(screening="lodf"))
gen_n1 = surge.contingency.n1_generator_study(net, surge.ContingencyOptions(screening="lodf"))
n2 = surge.contingency.n2_branch_study(net, surge.ContingencyOptions(screening="lodf", top_k=100))

analysis = n1.analyze()
scrd = n1.solve_corrective_dispatch()
```

## Supported Entry Points

| Solver | Function | Description |
|--------|----------|-------------|
| Newton-Raphson | `solve_ac_pf()` | Full AC power flow (KLU sparse) |
| Fast Decoupled | `surge.powerflow.solve_fdpf()` | BX/XB decoupled AC power flow |
| DC Power Flow | `solve_dc_pf()` | Linear B-theta approximation |
| HVDC Power Flow | `solve_hvdc()` | Coupled AC/DC HVDC power flow with automatic topology-aware method selection |
| DC-OPF | `solve_dc_opf()` | Optimal dispatch via LP (HiGHS) |
| AC-OPF | `solve_ac_opf()` | Optimal dispatch via NLP (auto-detected runtime backend; COPT first, then Ipopt) |
| SCOPF | `solve_scopf()` | Security-constrained OPF (DC/AC, preventive/corrective) |
| N-1 Contingency | `analyze_n1_branch()` | Parallel contingency analysis |
| Contingency Study | `surge.contingency.n1_branch_study()` | Reusable branch N-1 / generator N-1 / N-2 study object family |
| Prepared DC Study | `surge.dc.prepare_study()` | Reusable DC power flow + sensitivity study object |
| DC Analysis Workflow | `surge.dc.run_analysis()` | Canonical one-call DC flow + PTDF/LODF/N-2 sensitivities |
| PTDF/LODF | `surge.dc.compute_ptdf()` / `surge.dc.compute_lodf()` | Individual sensitivity matrix entry points |
| Prepared Transfer Study | `surge.transfer.prepare_transfer_study()` | Reusable ATC / AFC / multi-transfer study object |
| NERC ATC | `surge.transfer.compute_nerc_atc()` | Canonical transfer capability entry point |
| GSF | `surge.transfer.compute_gsf()` | Generation shift factors |
| Voltage Stress | `surge.contingency.compute_voltage_stress()` | Base-case exact L-index and local Q-V proxy screening |

DC power flow angle reporting can be controlled independently of slack
distribution. For example:

```python
dc = surge.solve_dc_pf(
    net,
    surge.DcPfOptions(
        participation_factors={101: 0.7, 205: 0.3},
        angle_reference="distributed_load",
    ),
)
```

This changes only the reported bus-angle reference frame; it does not change
branch flows or how the slack mismatch is shared.

The Python package intentionally does not expose unfinished or out-of-scope
study surfaces such as continuation PF, planning/compliance helpers,
protection convenience layers, dynamics/ride-through workflows, state
estimation, fault analysis, arc flash, GIC, or distribution solves until they
have a supported package contract.

## Supported File Formats

| Format | Read | Write |
|--------|------|-------|
| MATPOWER (.m) | Yes | Yes |
| PSS/E RAW (.raw) | Yes | Yes |
| PSS/E DYR (.dyr) | Yes | Yes |
| IEEE CDF | Yes | — |
| CGMES/CIM XML | Yes | Yes, via `surge.io.cgmes.save(...)` |
| UCTE-DEF | Yes | Yes |
| JSON | Yes | Yes |

## License

[PolyForm Noncommercial 1.0.0](https://polyformproject.org/licenses/noncommercial/1.0.0/) — free for non-commercial use. Commercial use requires a license from [Amptimal](https://github.com/amptimal).
