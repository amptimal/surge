// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Bus classification, voltage initialization, and setup helpers for the NR solver.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::network::BusType;
use surge_network::network::apply_angle_reference as apply_shared_angle_reference;
use tracing::{debug, warn};

use super::nr_kernel::ZipBusData;
use super::nr_options::AcPfOptions;
use surge_network::AngleReference;

/// Apply angle reference convention to output voltage angles.
///
/// For `PreserveInitial` and `Zero`, shifts all angles uniformly so the
/// original reference bus ends up at the target value.
///
pub(crate) fn apply_angle_reference(
    va: &mut [f64],
    orig_ref_idx: usize,
    va_ref0: f64,
    mode: AngleReference,
    network: &Network,
) {
    apply_shared_angle_reference(va, network, orig_ref_idx, va_ref0, mode);
}

/// Compute DC power flow angles as an initial guess for AC NR.
///
/// Builds a B' matrix (1/x for each branch, slack row/column removed),
/// assembles P injection (Pgen - Pload) / baseMVA, and solves the linear
/// system B' * theta = P_inj using KLU.  Returns a full-size angle vector
/// (length n) with 0.0 at the slack bus.
///
/// For multi-island networks (disconnected components), the function
/// detects islands, picks a reference bus per island, builds per-island
/// B' matrices, and solves each independently.
///
/// Returns `None` only if the network has no buses.
pub(crate) fn dc_angle_init(network: &Network) -> Option<Vec<f64>> {
    use crate::topology::islands::detect_islands;

    let n = network.n_buses();
    if n == 0 {
        return None;
    }

    debug!(buses = n, "computing DC angle warm-start for NR");
    let bus_map = network.bus_index_map();

    // Detect islands to handle multi-component networks.
    let islands = detect_islands(network, &bus_map);

    let p_full = network.bus_p_injection_pu();
    let mut theta = vec![0.0f64; n];

    for island_buses in &islands.components {
        // Single-bus islands: angle stays 0.
        if island_buses.len() <= 1 {
            continue;
        }

        // Pick reference bus for this island: prefer existing Slack, then
        // highest-base_kv PV bus, then first bus.
        let ref_idx = island_buses
            .iter()
            .copied()
            .find(|&i| network.buses[i].bus_type == BusType::Slack)
            .or_else(|| {
                island_buses
                    .iter()
                    .copied()
                    .filter(|&i| network.buses[i].bus_type == BusType::PV)
                    .max_by(|&a, &b| {
                        network.buses[a]
                            .base_kv
                            .partial_cmp(&network.buses[b].base_kv)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            })
            .unwrap_or(island_buses[0]);

        // Build reduced-index mapping for this island (skip reference bus).
        const SENTINEL: usize = usize::MAX;
        let island_dim = island_buses.len() - 1;
        if island_dim == 0 {
            continue;
        }

        let mut full_to_reduced = vec![SENTINEL; n];
        let mut reduced_idx = 0usize;
        for &gi in island_buses {
            if gi != ref_idx {
                full_to_reduced[gi] = reduced_idx;
                reduced_idx += 1;
            }
        }

        // Build B' triplets for this island.
        let mut triplets: HashMap<(usize, usize), f64> = HashMap::with_capacity(4 * island_dim);

        for branch in &network.branches {
            if !branch.in_service || branch.x.abs() < 1e-20 {
                continue;
            }
            let Some(&f) = bus_map.get(&branch.from_bus) else {
                continue;
            };
            let Some(&t) = bus_map.get(&branch.to_bus) else {
                continue;
            };
            // Only consider branches within this island.
            if full_to_reduced[f] == SENTINEL && f != ref_idx {
                continue;
            }
            if full_to_reduced[t] == SENTINEL && t != ref_idx {
                continue;
            }

            let b = branch.b_dc();
            let ri = full_to_reduced[f];
            let rj = full_to_reduced[t];

            match (ri != SENTINEL, rj != SENTINEL) {
                (true, true) => {
                    *triplets.entry((ri, ri)).or_default() += b;
                    *triplets.entry((rj, rj)).or_default() += b;
                    *triplets.entry((ri, rj)).or_default() -= b;
                    *triplets.entry((rj, ri)).or_default() -= b;
                }
                (true, false) => {
                    *triplets.entry((ri, ri)).or_default() += b;
                }
                (false, true) => {
                    *triplets.entry((rj, rj)).or_default() += b;
                }
                (false, false) => {}
            }
        }

        // Ensure all diagonals exist.
        for i in 0..island_dim {
            triplets.entry((i, i)).or_default();
        }

        // Convert triplets to CSC.
        let mut entries: Vec<(usize, usize, f64)> =
            triplets.into_iter().map(|((r, c), v)| (r, c, v)).collect();
        entries.sort_by_key(|&(r, c, _)| (c, r));

        let nnz = entries.len();
        let mut col_ptrs = vec![0usize; island_dim + 1];
        let mut row_indices = Vec::with_capacity(nnz);
        let mut values = Vec::with_capacity(nnz);

        for &(r, c, v) in &entries {
            col_ptrs[c + 1] += 1;
            row_indices.push(r);
            values.push(v);
        }
        for i in 1..=island_dim {
            col_ptrs[i] += col_ptrs[i - 1];
        }

        // Factor.
        let mut klu = match surge_sparse::KluSolver::new(island_dim, &col_ptrs, &row_indices) {
            Ok(k) => k,
            Err(_) => {
                // This island's B' is structurally singular — leave angles at 0.
                continue;
            }
        };
        if klu.factor(&values).is_err() {
            // This island's B' is numerically singular — leave angles at 0.
            continue;
        }

        // Build P injection RHS (reduced, reference bus removed).
        let mut rhs = vec![0.0f64; island_dim];
        for &gi in island_buses {
            let ri = full_to_reduced[gi];
            if ri != SENTINEL {
                rhs[ri] = p_full[gi];
            }
        }

        // Apply PST corrections (phase-shifting transformers).
        for branch in &network.branches {
            if !branch.in_service || branch.x.abs() < 1e-20 || branch.phase_shift_rad.abs() < 1e-12
            {
                continue;
            }
            let Some(&f) = bus_map.get(&branch.from_bus) else {
                continue;
            };
            let Some(&t) = bus_map.get(&branch.to_bus) else {
                continue;
            };
            if full_to_reduced[f] == SENTINEL && f != ref_idx {
                continue;
            }
            if full_to_reduced[t] == SENTINEL && t != ref_idx {
                continue;
            }
            let phi_rad = branch.phase_shift_rad;
            let b = branch.b_dc();
            let correction = b * phi_rad;
            let ri = full_to_reduced[f];
            let rj = full_to_reduced[t];
            if ri != SENTINEL {
                rhs[ri] += correction;
            }
            if rj != SENTINEL {
                rhs[rj] -= correction;
            }
        }

        if klu.solve(&mut rhs).is_err() {
            continue; // Solve failed — leave angles at 0 for this island.
        }

        // Write solved angles into full-size vector.
        let ref_va = network.buses[ref_idx].voltage_angle_rad; // radians
        for &gi in island_buses {
            let ri = full_to_reduced[gi];
            let raw = if ri != SENTINEL {
                rhs[ri] + ref_va
            } else {
                // Reference bus itself.
                ref_va
            };
            // Normalise to (−π, π] so the NR always starts in the principal angle range.
            theta[gi] = raw - (std::f64::consts::TAU * (raw / std::f64::consts::TAU).round());
        }
    }

    Some(theta)
}

/// Classify buses into (pvpq_indices, pq_indices) from the current bus_types.
pub(crate) fn classify_indices(bus_types: &[BusType]) -> (Vec<usize>, Vec<usize>) {
    let mut pv_idx: Vec<usize> = Vec::new();
    let mut pq_idx: Vec<usize> = Vec::new();
    for (i, &bt) in bus_types.iter().enumerate() {
        match bt {
            BusType::PV => pv_idx.push(i),
            BusType::PQ => pq_idx.push(i),
            _ => {}
        }
    }
    let mut pvpq: Vec<usize> = Vec::with_capacity(pv_idx.len() + pq_idx.len());
    pvpq.extend(&pv_idx);
    pvpq.extend(&pq_idx);
    pvpq.sort_unstable();
    (pvpq, pq_idx)
}

/// Data for remote voltage regulation bus type switching.
pub(crate) struct RemoteRegMap {
    /// Terminal bus indices to demote PV→PQ (only if ALL gens regulate remotely).
    pub(crate) terminal_demote: HashSet<usize>,
    /// Remote bus index → voltage setpoint (highest Vs among controlling gens).
    pub(crate) remote_promote: HashMap<usize, f64>,
    /// Remote bus index → list of controlling generator terminal bus indices.
    pub(crate) remote_controllers: HashMap<usize, Vec<usize>>,
}

pub(crate) fn build_remote_reg_map(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    bus_types: &[BusType],
) -> RemoteRegMap {
    let mut terminal_all_remote: HashMap<usize, bool> = HashMap::new();
    let mut remote_promote: HashMap<usize, f64> = HashMap::new();
    let mut remote_controllers: HashMap<usize, Vec<usize>> = HashMap::new();

    for g in &network.generators {
        if !g.in_service || !g.voltage_regulated {
            continue;
        }
        let Some(&gen_idx) = bus_map.get(&g.bus) else {
            continue;
        };
        if !matches!(bus_types[gen_idx], BusType::PV | BusType::Slack) {
            continue;
        }

        let reg = g.reg_bus.unwrap_or(g.bus);
        let Some(&reg_idx) = bus_map.get(&reg) else {
            continue;
        };

        if reg == g.bus || reg_idx == gen_idx {
            // Local regulation — this terminal bus must NOT be demoted.
            terminal_all_remote.insert(gen_idx, false);
        } else {
            // Remote regulation.
            let entry = terminal_all_remote.entry(gen_idx).or_insert(true);
            // Don't overwrite false (a local gen already claimed this bus).
            if *entry {
                *entry = true;
            }
            let vs_entry = remote_promote
                .entry(reg_idx)
                .or_insert(g.voltage_setpoint_pu);
            if g.voltage_setpoint_pu > *vs_entry {
                *vs_entry = g.voltage_setpoint_pu;
            }
            remote_controllers.entry(reg_idx).or_default().push(gen_idx);
        }
    }

    let terminal_demote: HashSet<usize> = terminal_all_remote
        .into_iter()
        .filter(|&(_, all_remote)| all_remote)
        .map(|(idx, _)| idx)
        .collect();

    RemoteRegMap {
        terminal_demote,
        remote_promote,
        remote_controllers,
    }
}

pub(crate) fn apply_remote_reg_types(
    map: &RemoteRegMap,
    bus_types: &mut [BusType],
    vm: &mut [f64],
    switched_to_pq: &mut [bool],
    blocked_remote_promotions: Option<&HashSet<usize>>,
) {
    for &idx in &map.terminal_demote {
        if bus_types[idx] == BusType::PV {
            bus_types[idx] = BusType::PQ;
            // Don't set switched_to_pq — that flag is for Q-limit enforcement.
        }
    }
    for (&idx, &vs) in &map.remote_promote {
        if blocked_remote_promotions.is_some_and(|blocked| blocked.contains(&idx)) {
            continue;
        }
        if bus_types[idx] == BusType::PQ && !switched_to_pq[idx] {
            bus_types[idx] = BusType::PV;
            vm[idx] = vs;
        }
    }
}

/// Reclassify PV buses with no in-service generators as PQ.
///
/// Returns the number of buses reclassified.
pub(crate) fn reclassify_dead_pv_buses(
    network: &Network,
    bus_types: &mut [BusType],
    switched_to_pq: &mut [bool],
) -> u32 {
    let bus_map = network.bus_index_map();
    // Count in-service generators per bus index.
    let mut live_gen_count = vec![0u32; network.buses.len()];
    for g in &network.generators {
        if g.in_service
            && let Some(&idx) = bus_map.get(&g.bus)
        {
            live_gen_count[idx] += 1;
        }
    }
    let mut n_reclassified = 0u32;
    for (i, bt) in bus_types.iter_mut().enumerate() {
        if *bt == BusType::PV && live_gen_count[i] == 0 {
            *bt = BusType::PQ;
            switched_to_pq[i] = true; // mark so it stays PQ (no back-switch logic)
            n_reclassified += 1;
        }
    }
    n_reclassified
}

pub(crate) fn apply_generator_p_limit_demotions(network: &Network, bus_types: &mut [BusType]) {
    // Parser/source rounding routinely differs at the micro-MW level.
    const P_MIN_TOL_MW: f64 = 1e-3;
    for (i, bus) in network.buses.iter().enumerate() {
        if bus_types[i] != BusType::PV {
            continue;
        }
        let gens_on_bus: Vec<&surge_network::network::Generator> = network
            .generators
            .iter()
            .filter(|g| g.bus == bus.number && g.in_service && g.voltage_regulated)
            .collect();
        if gens_on_bus.is_empty() {
            continue;
        }
        let all_infeasible = gens_on_bus.iter().all(|g| g.p < g.pmin - P_MIN_TOL_MW);
        if all_infeasible {
            bus_types[i] = BusType::PQ;
            debug!(
                bus = bus.number,
                "PV→PQ: all generators have pg < pmin (infeasible P setpoint)"
            );
        }
    }
}

pub(crate) fn apply_generator_voltage_setpoints(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    bus_types: &[BusType],
    vm: &mut [f64],
) {
    for g in &network.generators {
        if g.in_service
            && g.voltage_regulated
            && let Some(&gen_idx) = bus_map.get(&g.bus)
            && (bus_types[gen_idx] == BusType::PV || bus_types[gen_idx] == BusType::Slack)
        {
            let reg = g.reg_bus.unwrap_or(g.bus);
            if let Some(&reg_idx) = bus_map.get(&reg) {
                vm[reg_idx] = g.voltage_setpoint_pu;
            }
        }
    }
}

pub(crate) fn build_zip_bus_data(network: &Network) -> Vec<ZipBusData> {
    type ZipLoadAccum = (f64, f64, f64, f64, f64, f64, f64, f64);

    let bm = network.bus_index_map();
    let mut bus_accum: HashMap<u32, ZipLoadAccum> = HashMap::new();
    for load in &network.loads {
        if !load.in_service {
            continue;
        }
        if load.zip_p_power_frac >= 1.0 && load.zip_q_power_frac >= 1.0 {
            continue;
        }
        let entry = bus_accum
            .entry(load.bus)
            .or_insert((0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0));
        let pd = load.active_power_demand_mw / network.base_mva;
        let qd = load.reactive_power_demand_mvar / network.base_mva;
        entry.0 += pd;
        entry.1 += qd;
        entry.2 += load.zip_p_impedance_frac * pd;
        entry.3 += load.zip_p_current_frac * pd;
        entry.4 += load.zip_p_power_frac * pd;
        entry.5 += load.zip_q_impedance_frac * qd;
        entry.6 += load.zip_q_current_frac * qd;
        entry.7 += load.zip_q_power_frac * qd;
    }

    let mut data = Vec::new();
    for (bus_num, (p_sum, q_sum, pz_w, pi_w, pp_w, qz_w, qi_w, qp_w)) in &bus_accum {
        if let Some(&idx) = bm.get(bus_num)
            && (p_sum.abs() > 1e-15 || q_sum.abs() > 1e-15)
        {
            let (pz, pi_f, pp) = if p_sum.abs() > 1e-15 {
                (pz_w / p_sum, pi_w / p_sum, pp_w / p_sum)
            } else {
                (0.0, 0.0, 1.0)
            };
            let (qz, qi, qp) = if q_sum.abs() > 1e-15 {
                (qz_w / q_sum, qi_w / q_sum, qp_w / q_sum)
            } else {
                (0.0, 0.0, 1.0)
            };
            data.push(ZipBusData {
                idx,
                p_base: *p_sum,
                q_base: *q_sum,
                pz,
                pi: pi_f,
                pp,
                qz,
                qi,
                qp,
            });
        }
    }
    data
}

/// Build participation factor map from `AcPfOptions`.
///
/// Returns `None` if distributed slack is not requested.
pub(crate) fn build_participation_map(
    network: &Network,
    options: &AcPfOptions,
) -> Option<HashMap<usize, f64>> {
    if let Some(ref generator_map) = options.generator_slack_participation {
        if options.slack_participation.is_some() || options.distributed_slack {
            warn!(
                "generator_slack_participation takes precedence over slack_participation and distributed_slack"
            );
        }
        let n_generators = network.generators.len();
        let bus_map = network.bus_index_map();
        let valid: HashMap<usize, f64> = generator_map
            .iter()
            .filter_map(|(&gen_idx, &v)| {
                if gen_idx >= n_generators {
                    warn!(
                        gen_idx,
                        n_generators,
                        "generator_slack_participation key out of range — ignored"
                    );
                    return None;
                }
                if !v.is_finite() || v <= 0.0 {
                    warn!(
                        gen_idx,
                        factor = v,
                        "generator_slack_participation factor is non-finite or non-positive — ignored"
                    );
                    return None;
                }
                let generator = &network.generators[gen_idx];
                if !generator.in_service {
                    warn!(gen_idx, "generator_slack_participation references out-of-service generator — ignored");
                    return None;
                }
                let &bus_idx = bus_map.get(&generator.bus)?;
                Some((bus_idx, v))
            })
            .fold(HashMap::new(), |mut acc, (bus_idx, weight)| {
                *acc.entry(bus_idx).or_insert(0.0) += weight;
                acc
            });
        let sum: f64 = valid.values().sum();
        if sum < 1e-12 {
            warn!(
                "generator_slack_participation provided but all factors are zero/invalid \
                 after validation — falling back to single slack"
            );
            return None;
        }
        if (sum - 1.0).abs() < 1e-6 {
            return Some(valid);
        }
        return Some(valid.iter().map(|(&k, &v)| (k, v / sum)).collect());
    }

    if let Some(ref map) = options.slack_participation {
        let n = network.n_buses();
        // Drop out-of-range keys.
        let valid: HashMap<usize, f64> = map
            .iter()
            .filter_map(|(&k, &v)| {
                if k >= n {
                    warn!(
                        bus_idx = k,
                        n_buses = n,
                        "slack_participation key out of range — ignored"
                    );
                    return None;
                }
                if !v.is_finite() || v <= 0.0 {
                    warn!(
                        bus_idx = k,
                        factor = v,
                        "slack_participation factor is non-finite or non-positive — ignored"
                    );
                    return None;
                }
                Some((k, v))
            })
            .collect();
        let sum: f64 = valid.values().sum();
        if sum < 1e-12 {
            warn!(
                "slack_participation map provided but all factors are zero/invalid \
                 after validation — falling back to single slack"
            );
            return None;
        }
        if (sum - 1.0).abs() < 1e-6 {
            return Some(valid);
        }
        // Normalise
        return Some(valid.iter().map(|(&k, &v)| (k, v / sum)).collect());
    }

    if options.distributed_slack {
        let bus_map = network.bus_index_map();
        let gen_bus_set: std::collections::HashSet<usize> = network
            .generators
            .iter()
            .filter(|g| g.in_service)
            .filter_map(|g| bus_map.get(&g.bus).copied())
            .collect();

        if gen_bus_set.is_empty() {
            warn!(
                "distributed_slack=true but no in-service generators found \
                 — falling back to single slack"
            );
            return None;
        }
        let alpha = 1.0 / gen_bus_set.len() as f64;
        return Some(gen_bus_set.into_iter().map(|idx| (idx, alpha)).collect());
    }

    None
}
