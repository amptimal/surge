// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Node-breaker topology support for solver-facing bus-branch networks.
//!
//! This crate provides:
//!
//! - [`rebuild_topology`] — rebuild the bus-branch view of a node-breaker-backed
//!   network after switch-state changes.
//! - [`project_node_breaker_topology`] — derive an initial bus-branch projection
//!   and connectivity mapping from raw node-breaker topology.

mod engine;
pub mod islands;
mod union_find;

pub use engine::{
    CollapsedBranch, TopologyBusMerge, TopologyBusSplit, TopologyError, TopologyProjection,
    TopologyRebuild, TopologyReport, project_node_breaker_topology, rebuild_topology,
    rebuild_topology_with_report,
};
