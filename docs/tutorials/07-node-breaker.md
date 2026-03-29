# Node-Breaker Topology

Surge solves bus-branch networks, but it can retain full node-breaker topology
from node-breaker sources such as CGMES and XIIDM. The retained physical model
lets you inspect switches, change their state, rebuild the derived bus-branch
network, and then solve the rebuilt case.

---

## Basic Workflow

```python
import surge

net = surge.load("model_EQ.xml", "model_TP.xml", "model_SSH.xml")
topology = net.topology

if topology is None:
    raise RuntimeError("this case has no retained node-breaker topology")

print(topology.status)          # "current"
print(topology.switches[0].id)  # physical switch record

topology.set_switch_state("BRK_123", is_open=True)
print(topology.status)          # "stale"

rebuilt = topology.rebuild()
rebuilt_topology = rebuilt.topology
reduction = rebuilt_topology.current_mapping()

print(reduction.bus_for_connectivity_node("CN_42"))
solution = surge.solve_ac_pf(rebuilt)
```

The contract is explicit:

1. Change physical switch state on `NodeBreakerTopology`.
2. Rebuild the derived bus-branch network.
3. Solve the rebuilt network.

User-facing mapping helpers never return stale answers. Once a switch change
makes topology stale, `topology.mapping` becomes `None` and
`topology.current_mapping()` raises until you rebuild.

---

## Python Surface

`Network.topology`
: `NodeBreakerTopology | None`. `None` means the network is pure bus-branch.

`NodeBreakerTopology.status`
: `"missing" | "current" | "stale"`

`NodeBreakerTopology.is_current`
: `bool`

`NodeBreakerTopology.switches`
: `list[TopologySwitch]`

`NodeBreakerTopology.switch_state(id)`
: Current open/closed state, or `None` if the switch does not exist.

`NodeBreakerTopology.set_switch_state(id, *, is_open=...)`
: Mutates physical switch state and marks the retained mapping stale.

`NodeBreakerTopology.rebuild()`
: Returns a new `Network` with fresh bus-branch topology.

`NodeBreakerTopology.rebuild_with_report()`
: Returns `TopologyRebuildResult(network=..., report=...)`.

`NodeBreakerTopology.current_mapping()`
: Returns the current `TopologyMapping` or raises if topology is stale/missing.

`TopologyMapping.bus_for_connectivity_node(id)`
: Map a physical connectivity node to the current bus number.

`TopologyMapping.connectivity_nodes_for_bus(bus_number)`
: Map a bus number back to the physical connectivity nodes it contains.

Switch records expose only physical switch data:

```python
switch = topology.switches[0]
print(switch.kind)
print(switch.is_open)
print(switch.from_connectivity_node_id)
print(switch.to_connectivity_node_id)
```

They do not expose bus endpoints. Bus assignment belongs to the derived
`TopologyMapping`, not to the physical switch itself.

---

## Rebuild Reports

For switching studies, `rebuild_with_report()` is useful when you need a
structured summary of what changed:

```python
result = topology.rebuild_with_report()
report = result.report

print(report.previous_bus_count, report.current_bus_count)
print(report.consumed_switch_ids)
print(report.isolated_connectivity_node_ids)

for split in report.bus_splits:
    print(split.previous_bus_number, split.current_bus_numbers)

for merge in report.bus_merges:
    print(merge.current_bus_number, merge.previous_bus_numbers)

for branch in report.collapsed_branches:
    print(branch.previous_from_bus, branch.previous_to_bus, branch.circuit)
```

`rebuild()` is the normal path. `rebuild_with_report()` is the inspection path.

---

## Data Model

The retained physical topology lives on `Network.topology` in Rust
and `Network.topology` in Python:

```text
NodeBreakerTopology
|- substations
|- voltage_levels
|- bays
|- connectivity_nodes
|- busbar_sections
|- switches
|- terminal_connections
`- current TopologyMapping (only when status == "current")
   |- connectivity_node_to_bus
   |- bus_to_connectivity_nodes
   |- consumed_switch_ids
   `- isolated_connectivity_node_ids
```

Use full domain names in user code. Avoid abbreviations such as `cn_to_bus`.

---

## Rust Surface

```rust
use surge_topology::{rebuild_topology, rebuild_topology_with_report};

let rebuilt = rebuild_topology(&network)?;

let detailed = rebuild_topology_with_report(&network)?;
println!("{}", detailed.report.current_bus_count);
```

The public rebuild helpers are the supported workflow surface. User-facing docs
should stay on `rebuild_topology(...)` and `rebuild_topology_with_report(...)`
rather than lower-level importer details.

---

## Notes

- Solvers reject stale retained topology. Rebuild before solving.
- Rebuilding remaps equipment to the fresh topology; it does not re-run OPF or
  redispatch generation automatically.
- Bus-branch-only formats such as MATPOWER have `net.topology is None`.
