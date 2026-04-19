# Tutorial 12: Dispatch On ACTIVSg With LMP Maps

This walkthrough runs a 24-hour DC dispatch on the refreshed
`ACTIVSg2000` case, compares sequential and time-coupled SCED, and then
plots a nodal-price heat map from the resulting LMPs.

It assumes the repository already has:

- the refreshed case bundle at `examples/cases/case_ACTIVSg2000/case_ACTIVSg2000.surge.json.zst`
- the TAMU time-series package at `research/test-cases/data/ACTIVSg_Time_Series/`
- the Python package built from this source tree with `maturin develop --release`

The companion notebook is
[../notebooks/12-dispatch-activsg.ipynb](../notebooks/12-dispatch-activsg.ipynb).

## 1. Load The Refreshed Case And ACTIVSg Time Series

The Python dispatch namespace now exposes the same TAMU ACTIVSg importer that
the Rust examples use. We import the public CSV package, then apply the
generator nameplate overrides back onto the refreshed RAW-derived network.

```python
from pathlib import Path

import surge

repo_root = Path.cwd().resolve()
case_path = repo_root / "examples" / "cases" / "case_ACTIVSg2000" / "case_ACTIVSg2000.surge.json.zst"
time_series_root = repo_root / "research" / "test-cases" / "data" / "ACTIVSg_Time_Series"

net = surge.load(case_path)
activsg = surge.dispatch.read_tamu_activsg_time_series(net, time_series_root, case="2000")
net = activsg.network_with_nameplate_overrides(net)

print("Imported periods:", activsg.periods)
print("Load buses:", activsg.report["load_buses"])
print("Renewable profiles:", activsg.report["direct_renewable_generators"])
```

`activsg.report` is useful for checking what was mapped cleanly before you
solve anything.

## 2. Build 24-Hour Sequential And Time-Coupled Requests

For these large ACTIVSg studies we keep using real MATPOWER costs with
piecewise-linearized convex costs. We also enable the iterative DC loss-factor
model so both sequential and time-coupled runs expose a non-zero `mlc` term in
the LMP decomposition instead of treating the study as lossless.

```python
def make_request(imported, periods, coupling):
    return {
        "formulation": "dc",
        "coupling": coupling,
        "commitment": "all_committed",
        "timeline": imported.timeline(periods),
        "profiles": imported.dc_dispatch_profiles(periods),
        "market": {
            "generator_cost_modeling": {
                "use_pwl_costs": True,
                "pwl_cost_breakpoints": 20,
            }
        },
        "network": {
            "thermal_limits": {
                "enforce": True,
            },
            "flowgates": {
                "enabled": False,
            },
            "loss_factors": {
                "enabled": True,
            },
        },
    }


seq_24 = surge.solve_dispatch(
    net,
    make_request(activsg, 24, "period_by_period"),
    lp_solver="highs",
)
tc_24 = surge.solve_dispatch(
    net,
    make_request(activsg, 24, "time_coupled"),
    lp_solver="highs",
)

print("Sequential total cost:", seq_24.summary["total_cost"])
print("Time-coupled total cost:", tc_24.summary["total_cost"])
```

If you want the exact same horizon with AC physics later, keep in mind that AC
dispatch is currently period-by-period only.

## 3. Pick A Period To Visualize

The dispatch result exposes per-period `bus_results`, which already contain the
full LMP decomposition and the solved bus angle. For a 24-hour comparison, a
simple choice is the peak-load hour.

```python
import numpy as np
import pandas as pd

bus_df = net.bus_dataframe().reset_index()

def period_total_withdrawals(result):
    return [
        sum(bus["withdrawals_mw"] for bus in period["bus_results"])
        for period in result.periods
    ]


peak_hour = int(np.argmax(period_total_withdrawals(seq_24)))
print("Peak-load hour:", peak_hour)

period_df = pd.DataFrame(seq_24.periods[peak_hour]["bus_results"])
period_df.head()
```

## 4. Plot The Sequential LMP Heat Map

The refreshed `ACTIVSg2000` case now carries bus latitude and longitude from
the PowerWorld AUX source, so we can merge dispatch bus results directly onto
the geographic bus table and plot an LMP heat map. For the visualization, use
the sequential SCED result at the peak-load hour. That gives a stable nodal
price snapshot with visible congestion.

```python
import matplotlib.pyplot as plt
from matplotlib.collections import LineCollection

branch_df = net.branch_dataframe().reset_index()
coords = (
    bus_df[["bus_id", "latitude", "longitude"]]
    .dropna(subset=["latitude", "longitude"])
    .drop_duplicates(subset=["bus_id"])
    .set_index("bus_id")
)


def lmp_frame(result, period_index):
    return (
        bus_df.merge(
            pd.DataFrame(result.periods[period_index]["bus_results"]),
            left_on="bus_id",
            right_on="bus_number",
            how="inner",
        )
        .dropna(subset=["latitude", "longitude"])
        .copy()
    )


def branch_segments():
    segments = []
    for row in branch_df.itertuples(index=False):
        if row.from_bus not in coords.index or row.to_bus not in coords.index:
            continue
        from_pt = coords.loc[row.from_bus]
        to_pt = coords.loc[row.to_bus]
        segments.append(
            [
                (from_pt["longitude"], from_pt["latitude"]),
                (to_pt["longitude"], to_pt["latitude"]),
            ]
        )
    return segments


def plot_lmp_map(frame, title, ax):
    lines = LineCollection(branch_segments(), colors="#d8d8d8", linewidths=0.3, zorder=1)
    ax.add_collection(lines)
    scatter = ax.scatter(
        frame["longitude"],
        frame["latitude"],
        c=frame["lmp"],
        cmap="viridis",
        s=18,
        edgecolors="none",
        zorder=2,
    )
    ax.set_title(title)
    ax.set_xlabel("Longitude")
    ax.set_ylabel("Latitude")
    ax.set_aspect("equal")
    return scatter


seq_map = lmp_frame(seq_24, peak_hour)

fig, ax = plt.subplots(1, 1, figsize=(9, 7), constrained_layout=True)
scatter = plot_lmp_map(seq_map, f"Sequential SCED, hour {peak_hour}", ax)
fig.colorbar(scatter, ax=ax, shrink=0.85, label="LMP ($/MWh)")
plt.show()
```

## 5. What To Compare

When you compare sequential versus time-coupled SCED on this setup, focus on:

- total production cost over the 24-hour horizon
- whether the time-coupled horizon changes total dispatch cost
- which buses pick up congestion-driven separation in the sequential `mcc`
- how large the marginal-loss component `mlc` gets away from the reference bus
- where the highest-price pocket forms at the peak-load hour
- whether the top-LMP buses line up with the high-withdrawal part of the footprint

If you want a quick table of the highest-price buses in the mapped hour:

```python
seq_map.sort_values("lmp", ascending=False)[
    ["bus_id", "name", "lmp", "mcc", "mlc", "withdrawals_mw"]
].head(10)
```
