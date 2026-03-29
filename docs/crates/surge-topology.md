# surge-topology

`surge-topology` rebuilds the solver-facing bus-branch view from retained
node-breaker topology.

## Public Surface

- `rebuild_topology`
- `rebuild_topology_with_report`
- `CollapsedBranch`
- `TopologyBusMerge`
- `TopologyBusSplit`
- `TopologyError`
- `TopologyRebuild`
- `TopologyReport`
- `projection` module for importer/runtime plumbing

## What This Crate Is For

- rebuilding a `surge_network::Network` after switch-state changes
- producing a report that explains merges, splits, and collapsed branches
- deriving an initial solver-facing projection from retained topology data

## Example

```rust
use surge_topology::rebuild_topology_with_report;

let rebuilt = rebuild_topology_with_report(&network)?;
println!(
    "buses={} collapsed_branches={}",
    rebuilt.network.buses.len(),
    rebuilt.report.collapsed_branches.len()
);
# Ok::<(), surge_topology::TopologyError>(())
```

## Notes

- Use the root rebuild functions for normal client code.
- Use `projection` only when you explicitly need lower-level importer/runtime
  helpers.
