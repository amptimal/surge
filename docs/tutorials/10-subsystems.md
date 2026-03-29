# Tutorial 10: Filter Networks With `surge.subsystem`

`surge.subsystem.Subsystem` is a lightweight view over a network's buses and
connected elements. Use it when you need repeatable bus-set filters for
reporting, contingency follow-on analysis, or transfer study scoping.

## Basic Example

```python
import surge

net = surge.case118()

ehv = surge.subsystem.Subsystem(net, name="ehv", kv_min=345.0)
area_one = surge.subsystem.Subsystem(net, name="area-1", areas=[1])
load_pocket = surge.subsystem.Subsystem(net, name="load-pocket", buses=[8, 30, 38])

print(ehv.bus_numbers[:10])
print(area_one.total_load_mw)
print(load_pocket.tie_branches)
```

## Filter Semantics

All filters intersect. A bus must satisfy every supplied criterion to remain in
the subsystem.

Available filters:

- `areas=[...]`
- `zones=[...]`
- `kv_min=...`
- `kv_max=...`
- `buses=[...]`
- `bus_type="PQ" | "PV" | "Slack" | "Isolated"`

Example:

```python
import surge

net = surge.case118()
sub = surge.subsystem.Subsystem(
    net,
    name="area-1-ehv-load",
    areas=[1],
    kv_min=230.0,
    bus_type="PQ",
)

print(sub.bus_numbers)
```

## Elements And Totals

`Subsystem` exposes a few derived views:

- `bus_numbers`: sorted external bus numbers in the subsystem
- `branches`: branches whose endpoints are both inside the bus set
- `tie_branches`: branches with exactly one endpoint inside the bus set
- `generators`: in-service generators inside the bus set as `(bus, machine_id)`
- `loads`: bus numbers with nonzero load inside the bus set
- `total_load_mw`
- `total_generation_mw`

## Snapshot Versus Live Values

The bus set is fixed when the subsystem is created. Aggregate values and
element lists are computed against the current network state each time you read
them.

That means:

- changing generator dispatch changes `total_generation_mw`
- changing bus load changes `total_load_mw`
- taking a generator out of service removes it from `generators`
- adding new buses does not expand an existing subsystem's bus set

## Typical Uses

- isolate an area before running follow-on contingency ranking
- report internal versus tie-line interfaces for a transfer path
- summarize load and generation for a named footprint
- keep a stable bus set while dispatch or contingency results change underneath it
