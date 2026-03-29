// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Violation detection: thermal overloads, voltage violations, flowgate/interface limits.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::network::{Branch, BranchPowerFlowsPu, Bus, Flowgate, Interface};
use surge_solution::PfSolution;

use crate::types::{ContingencyOptions, Violation, get_rating};

// ---------------------------------------------------------------------------
// Pi-model branch flow computation (shared helper)
// ---------------------------------------------------------------------------

/// Compute from-side real and reactive power (P_ij, Q_ij) in per-unit using
/// the π-model for a single branch.
///
/// Returns `(p_pu, q_pu)` from the "from" side of the branch.
/// The caller is responsible for looking up bus indices from the bus map and
/// computing `vi`, `vj`, and `theta_ij = va[from] - va[to]`.
fn branch_power_flows_pu(branch: &Branch, vi: f64, vj: f64, theta_ij: f64) -> BranchPowerFlowsPu {
    branch.power_flows_pu(vi, vj, theta_ij, 1e-40)
}

pub(crate) fn effective_voltage_limits(bus: &Bus, options: &ContingencyOptions) -> (f64, f64) {
    let vmin = if bus.voltage_min_pu > 0.0 {
        bus.voltage_min_pu
    } else {
        options.vm_min
    };
    let vmax = if bus.voltage_max_pu > 0.0 {
        bus.voltage_max_pu
    } else {
        options.vm_max
    };
    (vmin, vmax)
}

// ---------------------------------------------------------------------------
// Violation key (for base-case deduplication)
// ---------------------------------------------------------------------------

/// Build a stable string key for a violation so it can be deduplicated against
/// the base-case violation set.  Only the element identity is used (not the
/// magnitude), so a thermal overload on branch 3-7 in the base case suppresses
/// that same branch's overload in every contingency result.
pub(crate) fn violation_key(v: &Violation) -> String {
    match v {
        Violation::ThermalOverload {
            branch_idx,
            from_bus,
            to_bus,
            ..
        } => {
            format!("thermal:{branch_idx}:{from_bus}-{to_bus}")
        }
        Violation::VoltageLow { bus_number, .. } => format!("vlow:{bus_number}"),
        Violation::VoltageHigh { bus_number, .. } => format!("vhigh:{bus_number}"),
        Violation::NonConvergent { .. } => "nonconv".to_string(),
        Violation::Islanding { n_components } => format!("island:{n_components}"),
        Violation::FlowgateOverload { name, .. } => format!("flowgate:{name}"),
        Violation::InterfaceOverload { name, .. } => format!("interface:{name}"),
    }
}

/// Return a severity score for comparing violations on the same element.
///
/// Higher values mean more severe violations. This is used to avoid
/// suppressing contingency violations that are materially worse than the
/// corresponding base-case violation.
pub(crate) fn violation_severity(v: &Violation) -> f64 {
    match v {
        Violation::ThermalOverload { loading_pct, .. } => *loading_pct,
        Violation::VoltageLow { vm, limit, .. } => (limit - vm).max(0.0),
        Violation::VoltageHigh { vm, limit, .. } => (vm - limit).max(0.0),
        Violation::NonConvergent { max_mismatch, .. } => *max_mismatch,
        Violation::Islanding { n_components } => *n_components as f64,
        Violation::FlowgateOverload { loading_pct, .. } => *loading_pct,
        Violation::InterfaceOverload { loading_pct, .. } => *loading_pct,
    }
}

// ---------------------------------------------------------------------------
// High-level detection from Network + Solution
// ---------------------------------------------------------------------------

/// Detect thermal overloads and voltage violations from a solved contingency.
pub(crate) fn detect_violations(
    network: &Network,
    solution: &PfSolution,
    options: &ContingencyOptions,
) -> Vec<Violation> {
    let bus_map = network.bus_index_map();
    detect_violations_from_parts(
        &network.branches,
        &network.buses,
        network.base_mva,
        &bus_map,
        None,
        &solution.voltage_magnitude_pu,
        &solution.voltage_angle_rad,
        options,
        &network.flowgates,
        &network.interfaces,
    )
}

// ---------------------------------------------------------------------------
// Low-level detection from parts (no Network clone needed)
// ---------------------------------------------------------------------------

/// Detect violations from individual components (no Network clone needed).
pub(crate) fn detect_violations_from_parts(
    branches: &[Branch],
    buses: &[Bus],
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    outaged_branches: Option<&HashSet<usize>>,
    vm: &[f64],
    va: &[f64],
    options: &ContingencyOptions,
    flowgates: &[Flowgate],
    interfaces: &[Interface],
) -> Vec<Violation> {
    let mut violations = Vec::new();

    // Thermal overloads
    for (i, branch) in branches.iter().enumerate() {
        let rating = get_rating(branch, options.thermal_rating);
        if !branch.in_service || rating <= 0.0 {
            continue;
        }
        if outaged_branches.is_some_and(|outaged| outaged.contains(&i)) {
            continue;
        }

        let Some(&f) = bus_map.get(&branch.from_bus) else {
            continue;
        };
        let Some(&t) = bus_map.get(&branch.to_bus) else {
            continue;
        };

        let vi = vm[f];
        let vj = vm[t];
        let theta_ij = va[f] - va[t];
        let flows = branch_power_flows_pu(branch, vi, vj, theta_ij);
        let sf_mva = flows.s_from_pu() * base_mva;
        let st_mva = flows.s_to_pu() * base_mva;
        let (flow_mw, s_mva) = if st_mva > sf_mva {
            (flows.p_to_pu * base_mva, st_mva)
        } else {
            (flows.p_from_pu * base_mva, sf_mva)
        };
        let loading = s_mva / rating * 100.0;

        if loading / 100.0 > options.thermal_threshold_frac {
            violations.push(Violation::ThermalOverload {
                branch_idx: i,
                from_bus: branch.from_bus,
                to_bus: branch.to_bus,
                loading_pct: loading,
                flow_mw,
                flow_mva: s_mva,
                limit_mva: rating,
            });
        }
    }

    // Voltage violations
    for (i, bus) in buses.iter().enumerate() {
        let v = vm[i];
        let (vmin, vmax) = effective_voltage_limits(bus, options);
        if v < vmin {
            violations.push(Violation::VoltageLow {
                bus_number: bus.number,
                vm: v,
                limit: vmin,
            });
        }
        if v > vmax {
            violations.push(Violation::VoltageHigh {
                bus_number: bus.number,
                vm: v,
                limit: vmax,
            });
        }
    }

    violations.extend(detect_flowgate_interface_violations_from_parts(
        branches,
        base_mva,
        bus_map,
        outaged_branches,
        vm,
        va,
        flowgates,
        interfaces,
    ));

    violations
}

// ---------------------------------------------------------------------------
// Flowgate & interface violation detection
// ---------------------------------------------------------------------------

/// Compute from-side real power P_ij (MW) for a single branch using the pi-model.
pub(crate) fn branch_p_mw(
    branch: &Branch,
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    vm: &[f64],
    va: &[f64],
) -> f64 {
    let Some(&f) = bus_map.get(&branch.from_bus) else {
        return 0.0;
    };
    let Some(&t) = bus_map.get(&branch.to_bus) else {
        return 0.0;
    };
    let flows = branch_power_flows_pu(branch, vm[f], vm[t], va[f] - va[t]);
    flows.p_from_pu * base_mva
}

/// Check flowgate and interface limits against post-contingency AC power flows.
fn detect_flowgate_interface_violations_from_parts(
    branches: &[Branch],
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    outaged_branches: Option<&HashSet<usize>>,
    vm: &[f64],
    va: &[f64],
    flowgates: &[Flowgate],
    interfaces: &[Interface],
) -> Vec<Violation> {
    let mut violations = Vec::new();

    // Flowgate checks
    for fg in flowgates {
        if !fg.in_service || fg.limit_mw <= 0.0 {
            continue;
        }
        let flow = compute_flowgate_flow(fg, branches, base_mva, bus_map, outaged_branches, vm, va);
        let loading_pct = flow.abs() / fg.limit_mw * 100.0;
        if flow.abs() > fg.limit_mw {
            violations.push(Violation::FlowgateOverload {
                name: fg.name.clone(),
                flow_mw: flow,
                limit_mw: fg.limit_mw,
                loading_pct,
            });
        }
    }

    // Interface checks
    for iface in interfaces {
        if !iface.in_service {
            continue;
        }
        let flow =
            compute_interface_flow(iface, branches, base_mva, bus_map, outaged_branches, vm, va);
        // Check forward limit
        if iface.limit_forward_mw > 0.0 && flow > iface.limit_forward_mw {
            let loading_pct = flow / iface.limit_forward_mw * 100.0;
            violations.push(Violation::InterfaceOverload {
                name: iface.name.clone(),
                flow_mw: flow,
                limit_mw: iface.limit_forward_mw,
                loading_pct,
            });
        }
        // Check reverse limit
        if iface.limit_reverse_mw > 0.0 && (-flow) > iface.limit_reverse_mw {
            let loading_pct = (-flow) / iface.limit_reverse_mw * 100.0;
            violations.push(Violation::InterfaceOverload {
                name: iface.name.clone(),
                flow_mw: flow,
                limit_mw: -iface.limit_reverse_mw,
                loading_pct,
            });
        }
    }

    violations
}

/// Compute the weighted sum of real power flows on a flowgate's monitored branches.
fn compute_flowgate_flow(
    fg: &Flowgate,
    branches: &[Branch],
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    outaged_branches: Option<&HashSet<usize>>,
    vm: &[f64],
    va: &[f64],
) -> f64 {
    let mut total = 0.0;
    for member in &fg.monitored {
        let branch_ref = &member.branch;
        let coeff = member.coefficient;
        if let Some(br) = branches.iter().enumerate().find_map(|(idx, br)| {
            if !br.in_service || outaged_branches.is_some_and(|outaged| outaged.contains(&idx)) {
                return None;
            }
            if branch_ref.matches_branch(br) {
                Some(br)
            } else {
                None
            }
        }) {
            total += coeff * branch_p_mw(br, base_mva, bus_map, vm, va);
        }
    }
    total
}

/// Compute the weighted sum of real power flows on an interface's member branches.
fn compute_interface_flow(
    iface: &Interface,
    branches: &[Branch],
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    outaged_branches: Option<&HashSet<usize>>,
    vm: &[f64],
    va: &[f64],
) -> f64 {
    let mut total = 0.0;
    for member in &iface.members {
        let branch_ref = &member.branch;
        let coeff = member.coefficient;
        if let Some(br) = branches.iter().enumerate().find_map(|(idx, br)| {
            if !br.in_service || outaged_branches.is_some_and(|outaged| outaged.contains(&idx)) {
                return None;
            }
            if branch_ref.matches_branch(br) {
                Some(br)
            } else {
                None
            }
        }) {
            total += coeff * branch_p_mw(br, base_mva, bus_map, vm, va);
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Branch flow computation
// ---------------------------------------------------------------------------

/// Compute max-end apparent power (MVA) for every branch using the π-model.
///
/// Returns a vector of length `branches.len()` with `max(|Sf|, |St|)` for each branch.
/// Out-of-service branches get `0.0`.  Uses the same π-model formula as
/// [`detect_violations_from_parts`] for consistency.
pub(crate) fn compute_branch_flows_mva(
    branches: &[Branch],
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    outaged_branches: Option<&HashSet<usize>>,
    vm: &[f64],
    va: &[f64],
) -> Vec<f64> {
    let mut flows = vec![0.0f64; branches.len()];
    for (i, branch) in branches.iter().enumerate() {
        if !branch.in_service || outaged_branches.is_some_and(|outaged| outaged.contains(&i)) {
            continue;
        }
        let Some(&f) = bus_map.get(&branch.from_bus) else {
            continue;
        };
        let Some(&t) = bus_map.get(&branch.to_bus) else {
            continue;
        };

        flows[i] = branch_power_flows_pu(branch, vm[f], vm[t], va[f] - va[t]).max_s_pu() * base_mva;
    }
    flows
}
