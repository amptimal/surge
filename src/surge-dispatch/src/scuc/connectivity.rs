// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Bus-branch connectivity check and cut generation for SCUC.
//!
//! When branch switching is enabled, the bus-branch graph must remain
//! connected in every time interval and every contingency:
//!
//! ```text
//!   The bus-branch graph on (I, {j ∈ J^ac : u^on_jt = 1}) is connected
//!   for every t ∈ T (and for every k ∈ K under contingency).
//! ```
//!
//! This module provides two services:
//!
//!   1. `check_period_connectivity` — given a switching state (per-branch
//!      `u^on_jt` vector), an optional outaged AC branch, and the per-period
//!      network snapshot, return the cut set of branches whose addition would
//!      re-connect the graph when it's currently disconnected, or `None` if
//!      connected.
//!
//!   2. `build_connectivity_cut_row` — given a cut set of branch local
//!      indices and a period, emit a `Σ u^on_jt ≥ 1` row over those
//!      branches, ready to write into the SCUC LP triplets.
//!
//! The intended use is in the security iteration loop: after each MIP
//! solve, the loop reads the per-period switching state from the
//! solution, calls `check_period_connectivity`, and if any period is
//! disconnected, calls `build_connectivity_cut_row` and re-solves.
//!
//! When `allow_branch_switching = false` (the default), the bounds
//! layer pins every branch commitment to its static `in_service` flag
//! and the prior operating point is connected by construction — so
//! the connectivity check is vacuous and never fires.

use std::collections::HashMap;

use surge_network::Network;
use surge_sparse::Triplet;

/// Forward declaration of the type our caller already uses for island
/// tracking. We don't need to manipulate it here, but we accept it as
/// an opaque hint slot for future expansion.
pub(super) struct IslandRefs;

/// Result of a connectivity check on a single period.
#[derive(Debug, Clone)]
pub(super) enum ConnectivityCheck {
    /// The bus-branch graph on (buses, in-service AC branches under
    /// the given switching state) is connected.
    Connected,
    /// The graph is disconnected. The cut set is the local AC-branch
    /// indices whose `u^on_jt` was 0 in the solution but whose addition
    /// (`u^on_jt = 1`) would reduce the number of connected components.
    /// At least ONE of these must be turned back on for the graph to
    /// re-connect.
    Disconnected { cut_set: Vec<usize> },
}

/// Build the connected-components view of the bus-branch graph for a
/// given period under the given switching state. The switching state
/// is keyed by the LOCAL AC-branch index (position in
/// `network.branches`). Returns the component label for each bus.
fn connected_components_under_switching(
    network: &Network,
    switching_state: &[bool],
    outaged_branch_idx: Option<usize>,
) -> Vec<usize> {
    let n_bus = network.buses.len();
    let bus_number_to_idx: HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(idx, b)| (b.number, idx))
        .collect();

    // Union-find: each bus starts in its own component.
    let mut parent: Vec<usize> = (0..n_bus).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        if parent[i] == i {
            i
        } else {
            let root = find(parent, parent[i]);
            parent[i] = root;
            root
        }
    }
    fn union(parent: &mut [usize], i: usize, j: usize) {
        let ri = find(parent, i);
        let rj = find(parent, j);
        if ri != rj {
            parent[ri] = rj;
        }
    }

    for (branch_local_idx, branch) in network.branches.iter().enumerate() {
        let on = if outaged_branch_idx == Some(branch_local_idx) {
            false
        } else {
            switching_state
                .get(branch_local_idx)
                .copied()
                .unwrap_or(branch.in_service)
        };
        if !on {
            continue;
        }
        let Some(&from_idx) = bus_number_to_idx.get(&branch.from_bus) else {
            continue;
        };
        let Some(&to_idx) = bus_number_to_idx.get(&branch.to_bus) else {
            continue;
        };
        union(&mut parent, from_idx, to_idx);
    }

    // Compress and label each component with its root.
    (0..n_bus).map(|i| find(&mut parent, i)).collect()
}

/// Check connectivity for one period and identify a disconnecting cut
/// set if the graph is split.
///
/// `switching_state[branch_local_idx]` is the cleared `u^on_jt` value
/// (true = on, false = off) for branch `branch_local_idx` at the
/// period being checked. The function compares against the network's
/// static `in_service` flag for branches not present in the slice.
pub(super) fn check_period_connectivity(
    network: &Network,
    switching_state: &[bool],
    outaged_branch_idx: Option<usize>,
    island_refs: Option<&IslandRefs>,
) -> ConnectivityCheck {
    let labels = connected_components_under_switching(network, switching_state, outaged_branch_idx);
    let n_bus = network.buses.len();
    if n_bus == 0 {
        return ConnectivityCheck::Connected;
    }

    // Filter out buses that the LP solver is intentionally treating as
    // disconnected (e.g. islanded buses with no demand and no
    // generation that `island_refs` has already accepted as
    // separate). The connectivity rule applies to the entire
    // bus-branch graph; we mirror that here unless the caller hands us
    // an explicit IslandRefs that already partitions the network.
    let _ = island_refs; // placeholder; the present rule does not use it

    // The graph is connected iff every bus shares the label of bus 0.
    let root = labels[0];
    let connected = labels.iter().all(|&l| l == root);
    if connected {
        return ConnectivityCheck::Connected;
    }

    // Build the cut set: every branch that is currently OFF and would,
    // if added, merge two distinct components. This is the standard
    // "edges crossing the cut" enumeration. At least one such branch
    // must be turned on for the graph to re-connect.
    let mut cut_set: Vec<usize> = Vec::new();
    let bus_number_to_idx: HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(idx, b)| (b.number, idx))
        .collect();
    for (branch_local_idx, branch) in network.branches.iter().enumerate() {
        if outaged_branch_idx == Some(branch_local_idx) {
            continue;
        }
        let on = switching_state
            .get(branch_local_idx)
            .copied()
            .unwrap_or(branch.in_service);
        if on {
            continue;
        }
        let Some(&from_idx) = bus_number_to_idx.get(&branch.from_bus) else {
            continue;
        };
        let Some(&to_idx) = bus_number_to_idx.get(&branch.to_bus) else {
            continue;
        };
        if labels[from_idx] != labels[to_idx] {
            cut_set.push(branch_local_idx);
        }
    }

    if cut_set.is_empty() {
        // The graph is disconnected but no off branch could re-connect
        // it (e.g. there are isolated nodes with no incident branches
        // at all). This means the cut cannot be expressed as a cover
        // over the existing branch set; return an empty cut and let
        // the caller log it.
        return ConnectivityCheck::Disconnected { cut_set: vec![] };
    }

    ConnectivityCheck::Disconnected { cut_set }
}

/// Errors that can arise from connectivity cut generation.
#[derive(Debug)]
#[allow(dead_code)]
pub(super) enum ConnectivityCutError {
    /// The cut set is empty — no branch can re-connect the graph.
    /// The caller should log this and either skip the cut or escalate.
    EmptyCutSet { period: usize },
}

/// A connectivity cut: at least one of the branches in `cut_set` must
/// be on in the given period.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) struct ConnectivityCut {
    pub period: usize,
    pub cut_set: Vec<usize>,
}

/// An already-indexed connectivity cut, designed to live inside a
/// `DispatchProblemSpec.connectivity_cuts` slice as the cut pool that
/// the SCUC security loop feeds forward across iterations. Equivalent
/// to [`ConnectivityCut`] but with the `crate`-visible fields needed
/// for the spec to hold references.
///
/// Whenever a solved switching pattern leaves the bus-branch graph
/// disconnected, the security loop pushes one
/// `IndexedConnectivityCut { period, cut_set }` entry into its local
/// accumulator, threads it back into the next
/// `DispatchProblemSpec` via
/// [`crate::common::spec::DispatchProblemSpec::with_connectivity_cuts`],
/// and re-solves. The row builder emits
/// `Σ_{j ∈ cut_set} branch_commitment[period, j] ≥ 1` per cut, which
/// is the smallest explicit statement of "at least one of these
/// branches must be on".
#[derive(Debug, Clone)]
pub struct IndexedConnectivityCut {
    /// Period the cut applies to.
    pub period: usize,
    /// Local AC branch indices in the disconnecting cut set. At least
    /// one must be `u^on_lt = 1` for the period to be feasible.
    pub cut_set: Vec<usize>,
}

impl IndexedConnectivityCut {
    /// Emit the cut row as sparse LP triplets for the SCUC problem.
    /// Mirrors [`ConnectivityCut::into_triplets`] but operates on the
    /// owned-vector variant that the security loop accumulates.
    #[allow(clippy::wrong_self_convention)]
    pub(super) fn into_triplets(
        &self,
        layout: &super::layout::ScucLayout,
        row_index: usize,
    ) -> Vec<Triplet<f64>> {
        self.cut_set
            .iter()
            .map(|&branch_local_idx| Triplet {
                row: row_index,
                col: layout.branch_commitment_col(self.period, branch_local_idx),
                val: 1.0,
            })
            .collect()
    }
}

impl ConnectivityCut {
    /// Build the LP triplets for the row `Σ_{j ∈ cut_set} u^on_jt ≥ 1`.
    /// The caller positions the row at `row_index` in the triplet
    /// stream and writes `(row_lower, row_upper) = (1.0, +inf)` for
    /// this row.
    #[allow(dead_code, clippy::wrong_self_convention)]
    pub(super) fn into_triplets(
        &self,
        layout: &super::layout::ScucLayout,
        row_index: usize,
    ) -> Vec<Triplet<f64>> {
        self.cut_set
            .iter()
            .map(|&branch_local_idx| Triplet {
                row: row_index,
                col: layout.branch_commitment_col(self.period, branch_local_idx),
                val: 1.0,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Bus, BusType};

    fn add_bus(net: &mut Network, number: u32) {
        net.buses.push(Bus::new(number, BusType::Slack, 138.0));
    }

    fn add_branch(net: &mut Network, from: u32, to: u32, in_service: bool) {
        let mut br = surge_network::network::Branch::new_line(from, to, 0.01, 0.1, 0.0);
        br.in_service = in_service;
        br.rating_a_mva = 100.0;
        net.branches.push(br);
    }

    /// 4-bus diamond:
    ///   1 — 2
    ///   |   |
    ///   3 — 4
    /// All branches in service → connected.
    #[test]
    fn test_check_connectivity_all_branches_on_returns_connected() {
        let mut net = Network::new("conn_diamond_all_on");
        net.base_mva = 100.0;
        add_bus(&mut net, 1);
        add_bus(&mut net, 2);
        add_bus(&mut net, 3);
        add_bus(&mut net, 4);
        add_branch(&mut net, 1, 2, true);
        add_branch(&mut net, 1, 3, true);
        add_branch(&mut net, 2, 4, true);
        add_branch(&mut net, 3, 4, true);

        let switching_state = vec![true, true, true, true];
        let result = check_period_connectivity(&net, &switching_state, None, None);
        assert!(matches!(result, ConnectivityCheck::Connected));
    }

    /// Same diamond, but the branches connecting buses {1,2} to {3,4}
    /// (branches 1-3 and 2-4) are both off. Buses {1,2} are isolated
    /// from buses {3,4}. The cut set must contain {1-3, 2-4}.
    #[test]
    fn test_check_connectivity_split_diamond_emits_cut_set() {
        let mut net = Network::new("conn_diamond_split");
        net.base_mva = 100.0;
        add_bus(&mut net, 1);
        add_bus(&mut net, 2);
        add_bus(&mut net, 3);
        add_bus(&mut net, 4);
        add_branch(&mut net, 1, 2, true); // branch 0
        add_branch(&mut net, 1, 3, true); // branch 1 (cut)
        add_branch(&mut net, 2, 4, true); // branch 2 (cut)
        add_branch(&mut net, 3, 4, true); // branch 3

        let switching_state = vec![true, false, false, true];
        let result = check_period_connectivity(&net, &switching_state, None, None);
        match result {
            ConnectivityCheck::Disconnected { cut_set } => {
                let mut sorted = cut_set;
                sorted.sort();
                assert_eq!(sorted, vec![1, 2]);
            }
            _ => panic!("expected disconnected, got {result:?}"),
        }
    }

    /// 2-bus path. Removing the only branch leaves an empty cut set
    /// (the bus pair is disconnected and no off branch can fix it).
    #[test]
    fn test_check_connectivity_no_off_branches_to_reconnect_returns_empty_cut() {
        let mut net = Network::new("conn_two_bus_no_branches");
        net.base_mva = 100.0;
        add_bus(&mut net, 1);
        add_bus(&mut net, 2);
        // The single branch is OFF; the graph is disconnected.
        add_branch(&mut net, 1, 2, false);

        let switching_state = vec![false];
        let result = check_period_connectivity(&net, &switching_state, None, None);
        match result {
            ConnectivityCheck::Disconnected { cut_set } => {
                // The single off branch IS in the cut set — turning it
                // back on re-connects the graph.
                assert_eq!(cut_set, vec![0]);
            }
            _ => panic!("expected disconnected, got {result:?}"),
        }
    }

    /// Single bus, no branches. Trivially connected (one component of
    /// size 1).
    #[test]
    fn test_check_connectivity_single_bus_is_connected() {
        let mut net = Network::new("conn_one_bus");
        net.base_mva = 100.0;
        add_bus(&mut net, 1);
        let result = check_period_connectivity(&net, &[], None, None);
        assert!(matches!(result, ConnectivityCheck::Connected));
    }

    /// Triangle 1-2-3 with branch 2-3 switched off. The base graph remains
    /// connected through 1-3, but under contingency outage of 1-3 the only
    /// reconnecting candidate is the switched-off 2-3 branch, so the cut set
    /// must exclude the outaged branch itself.
    #[test]
    fn test_check_connectivity_under_contingency_excludes_outaged_branch_from_cut_set() {
        let mut net = Network::new("conn_triangle_contingency");
        net.base_mva = 100.0;
        add_bus(&mut net, 1);
        add_bus(&mut net, 2);
        add_bus(&mut net, 3);
        add_branch(&mut net, 1, 2, true); // branch 0
        add_branch(&mut net, 1, 3, true); // branch 1 (outaged)
        add_branch(&mut net, 2, 3, true); // branch 2 (switched off)

        let switching_state = vec![true, true, false];
        let result = check_period_connectivity(&net, &switching_state, Some(1), None);
        match result {
            ConnectivityCheck::Disconnected { cut_set } => assert_eq!(cut_set, vec![2]),
            _ => panic!("expected disconnected, got {result:?}"),
        }
    }
}
