# surge-topology

Node-breaker topology processing for Surge.

This crate bridges retained physical node-breaker topology and the
solver-facing bus-branch network used by the rest of the workspace. Its main
workflow is rebuilding a network after switch-state changes, with a secondary
projection API for importer and runtime plumbing.
