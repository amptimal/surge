// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CTG-09: Island detection and per-island slack assignment.

use std::collections::{HashMap, VecDeque};

use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
use surge_network::Network;
use surge_network::network::BusType;
use surge_network::network::Contingency;
use surge_solution::SolveStatus;
use tracing::warn;

use super::solvers::solve_contingency_full_clone;
use crate::types::{ContingencyOptions, ContingencyResult, Violation};
use crate::violations::{compute_branch_flows_mva, detect_violations};

/// Find connected components in the network after removing specified branches.
///
/// Uses BFS over the in-service adjacency graph (excluding `removed_branches`).
/// Returns a vector of length `n_buses` where each entry is the component ID
/// (0-indexed) for that internal bus.  Returns the number of components found.
///
/// Isolated buses (no in-service connections after outage) form their own
/// single-bus component.
pub(crate) fn find_connected_components(
    network: &Network,
    removed_branches: &[usize],
) -> (Vec<usize>, usize) {
    let n = network.buses.len();
    let bus_map = network.bus_index_map();
    let removed_set: std::collections::HashSet<usize> = removed_branches.iter().cloned().collect();

    // Build adjacency list from in-service branches (excluding removed ones).
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (br_idx, branch) in network.branches.iter().enumerate() {
        if !branch.in_service || removed_set.contains(&br_idx) {
            continue;
        }
        let Some(&fi) = bus_map.get(&branch.from_bus) else {
            continue;
        };
        let Some(&ti) = bus_map.get(&branch.to_bus) else {
            continue;
        };
        if fi == ti {
            continue; // self-loop (degenerate)
        }
        adj[fi].push(ti);
        adj[ti].push(fi);
    }

    let mut component_id = vec![usize::MAX; n];
    let mut n_components = 0;

    for start in 0..n {
        if component_id[start] != usize::MAX {
            continue;
        }
        // BFS from `start`
        let mut queue = VecDeque::new();
        queue.push_back(start);
        component_id[start] = n_components;
        while let Some(u) = queue.pop_front() {
            for &v in &adj[u] {
                if component_id[v] == usize::MAX {
                    component_id[v] = n_components;
                    queue.push_back(v);
                }
            }
        }
        n_components += 1;
    }

    (component_id, n_components)
}

/// Solve a contingency that may create electrical islands (CTG-09).
///
/// If island detection is disabled or only one component is found, falls back
/// to the standard NR solve via the inline path.
///
/// When multiple islands are detected:
/// 1. For the component containing the original slack bus: solve normally.
/// 2. For every other component: find its highest-voltage PV bus and promote
///    it to slack; if no PV bus exists, use the bus with the highest rated
///    voltage (or bus 0 of the component as a last resort).
/// 3. Solve each component independently; combine vm/va into a single result.
/// 4. Add an `Islanding` violation to flag the event.
///
/// Isolated buses (degree-0 components) are solved as explicit one-bus islands
/// with a promoted slack bus so the result stays truthful instead of being
/// silently stubbed out.
pub(crate) fn solve_with_island_detection(
    network: &Network,
    ctg: &Contingency,
    options: &ContingencyOptions,
    _base_vm: &[f64],
    _base_va: &[f64],
) -> ContingencyResult {
    let mut post_network = network.clone();
    for &br_idx in &ctg.branch_indices {
        if br_idx < post_network.branches.len() {
            post_network.branches[br_idx].in_service = false;
        }
    }
    for &gen_idx in &ctg.generator_indices {
        if gen_idx < post_network.generators.len() {
            post_network.generators[gen_idx].in_service = false;
        }
    }

    let (component_id, n_components) = find_connected_components(&post_network, &[]);

    // Single component — no islanding; fall back to standard solve.
    if n_components <= 1 {
        return solve_contingency_full_clone(network, ctg, options);
    }

    solve_islanded_post_network(&post_network, ctg, options, &component_id, n_components)
}

/// Solve an already-modified post-contingency network when it contains multiple islands.
pub(crate) fn solve_post_network_with_island_detection(
    post_network: &Network,
    ctg: &Contingency,
    options: &ContingencyOptions,
) -> Option<ContingencyResult> {
    let (component_id, n_components) = find_connected_components(post_network, &[]);
    if n_components <= 1 {
        return None;
    }

    Some(solve_islanded_post_network(
        post_network,
        ctg,
        options,
        &component_id,
        n_components,
    ))
}

fn solve_islanded_post_network(
    post_network: &Network,
    ctg: &Contingency,
    options: &ContingencyOptions,
    component_id: &[usize],
    n_components: usize,
) -> ContingencyResult {
    // --- Multi-island case ---
    let n_bus = post_network.buses.len();
    let bus_map = post_network.bus_index_map();

    // Identify the component containing the original slack bus.
    let slack_idx = post_network.slack_bus_index().unwrap_or(0);
    let slack_component = component_id[slack_idx];

    // Solve each component independently.
    let mut combined_vm = vec![1.0f64; n_bus]; // flat-start default
    let mut combined_va = vec![0.0f64; n_bus];
    let mut total_iterations = 0u32;
    let mut all_converged = true;
    let mut all_violations: Vec<Violation> = Vec::new();

    for comp in 0..n_components {
        // Collect bus indices belonging to this component.
        let comp_buses: Vec<usize> = (0..n_bus).filter(|&i| component_id[i] == comp).collect();

        // Build a sub-network for this island.
        let mut subnet = post_network.clone();

        // Disable all branches connecting to other components.
        for branch in &mut subnet.branches {
            let Some(&fi) = bus_map.get(&branch.from_bus) else {
                continue;
            };
            let Some(&ti) = bus_map.get(&branch.to_bus) else {
                continue;
            };
            if component_id[fi] != comp || component_id[ti] != comp {
                branch.in_service = false;
            }
        }

        // Assign per-island slack.
        if comp != slack_component {
            // Find the best bus to serve as slack in this island:
            // prefer highest-voltage PV bus (by vs), else any in-service bus.
            let best_slack = {
                // PV buses in this island
                let pv_candidates: Vec<usize> = comp_buses
                    .iter()
                    .cloned()
                    .filter(|&bi| subnet.buses[bi].bus_type == BusType::PV)
                    .collect();

                if !pv_candidates.is_empty() {
                    // Pick the one with the highest generator voltage setpoint.
                    let bus_num_map: HashMap<usize, u32> = subnet
                        .buses
                        .iter()
                        .enumerate()
                        .map(|(i, b)| (i, b.number))
                        .collect();
                    pv_candidates
                        .iter()
                        .cloned()
                        .max_by(|&a, &b| {
                            // Higher voltage setpoint = higher vs on the generator
                            let vs_a = subnet
                                .generators
                                .iter()
                                .filter(|g| g.in_service && bus_map.get(&g.bus).copied() == Some(a))
                                .map(|g| g.voltage_setpoint_pu)
                                .fold(f64::NEG_INFINITY, f64::max);
                            let vs_b = subnet
                                .generators
                                .iter()
                                .filter(|g| g.in_service && bus_map.get(&g.bus).copied() == Some(b))
                                .map(|g| g.voltage_setpoint_pu)
                                .fold(f64::NEG_INFINITY, f64::max);
                            // Fall back to base_kv comparison
                            let kv_a = bus_num_map
                                .get(&a)
                                .and_then(|num| subnet.buses.iter().find(|bus| &bus.number == num))
                                .map(|bus| bus.base_kv)
                                .unwrap_or(0.0);
                            let kv_b = bus_num_map
                                .get(&b)
                                .and_then(|num| subnet.buses.iter().find(|bus| &bus.number == num))
                                .map(|bus| bus.base_kv)
                                .unwrap_or(0.0);
                            vs_a.partial_cmp(&vs_b).unwrap_or_else(|| {
                                kv_a.partial_cmp(&kv_b).unwrap_or(std::cmp::Ordering::Equal)
                            })
                        })
                        .unwrap_or(comp_buses[0])
                } else {
                    // No PV buses — use the bus with the highest base_kv.
                    comp_buses
                        .iter()
                        .cloned()
                        .max_by(|&a, &b| {
                            subnet.buses[a]
                                .base_kv
                                .partial_cmp(&subnet.buses[b].base_kv)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .unwrap_or(comp_buses[0])
                }
            };

            // Demote all current slack buses in this island to PQ.
            for &bi in &comp_buses {
                if subnet.buses[bi].bus_type == BusType::Slack {
                    subnet.buses[bi].bus_type = BusType::PQ;
                }
            }
            // Promote the chosen bus to slack.
            subnet.buses[best_slack].bus_type = BusType::Slack;
        }

        // Also disable all buses not in this component so NR sees a clean subnet.
        // We do this by isolating them (type=Isolated) to prevent mismatches.
        for (bi, &cid) in component_id.iter().enumerate().take(n_bus) {
            if cid != comp {
                subnet.buses[bi].bus_type = BusType::Isolated;
            }
        }

        // P1-030: Demote PV buses that have no in-service generators in this
        // island to PQ.  After island detection splits the network, a PV bus
        // may end up in an island where its generators are all out of service
        // or in a different island.  Without this reclassification the solver
        // freezes the voltage setpoint of a bus with no generator backing it.
        for &bi in &comp_buses {
            if subnet.buses[bi].bus_type != BusType::PV {
                continue;
            }
            let bus_num = subnet.buses[bi].number;
            let has_gen_in_island = subnet.generators.iter().any(|g| {
                g.in_service
                    && g.bus == bus_num
                    && bus_map.get(&g.bus).copied() == Some(bi)
                    && component_id.get(bi).copied() == Some(comp)
            });
            if !has_gen_in_island {
                warn!(
                    "P1-030: PV bus {} has no in-service generator in island {} — demoting to PQ",
                    bus_num, comp
                );
                subnet.buses[bi].bus_type = BusType::PQ;
            }
        }

        // Solve this island with NR.
        let acpf_opts = AcPfOptions {
            flat_start: comp != slack_component, // fresh start for non-slack islands
            ..options.acpf_options.clone()
        };
        match solve_ac_pf_kernel(&subnet, &acpf_opts) {
            Ok(sol) if sol.status == SolveStatus::Converged => {
                total_iterations += sol.iterations;
                // Copy island solution into combined arrays.
                for &bi in &comp_buses {
                    combined_vm[bi] = sol.voltage_magnitude_pu[bi];
                    combined_va[bi] = sol.voltage_angle_rad[bi];
                }
                // Collect violations for this island.
                let island_viols = detect_violations(&subnet, &sol, options);
                all_violations.extend(island_viols);
            }
            Ok(sol) => {
                all_converged = false;
                total_iterations += sol.iterations;
                all_violations.push(Violation::NonConvergent {
                    max_mismatch: sol.max_mismatch,
                    iterations: sol.iterations,
                });
            }
            Err(_) => {
                all_converged = false;
                all_violations.push(Violation::NonConvergent {
                    max_mismatch: f64::INFINITY,
                    iterations: 0,
                });
            }
        }
    }

    // Always add an Islanding violation to flag the topology event.
    all_violations.push(Violation::Islanding { n_components });

    let (pvm, pva, pflows) = if options.store_post_voltages && all_converged {
        let bus_map_isl = post_network.bus_index_map();
        (
            Some(combined_vm.clone()),
            Some(combined_va.clone()),
            Some(compute_branch_flows_mva(
                &post_network.branches,
                post_network.base_mva,
                &bus_map_isl,
                None,
                &combined_vm,
                &combined_va,
            )),
        )
    } else {
        (None, None, None)
    };

    ContingencyResult {
        id: ctg.id.clone(),
        label: ctg.label.clone(),
        branch_indices: ctg.branch_indices.clone(),
        generator_indices: ctg.generator_indices.clone(),
        status: crate::types::ContingencyStatus::Islanded,
        converged: all_converged,
        iterations: total_iterations,
        violations: all_violations,
        n_islands: n_components,
        post_vm: pvm,
        post_va: pva,
        post_branch_flows: pflows,
        tpl_category: ctg.tpl_category,
        ..Default::default()
    }
}
