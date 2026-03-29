// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Island-aware LMP energy decomposition.
//!
//! When HVDC ties connect asynchronous AC systems, each electrical island is
//! a separate energy market with its own marginal generator.  The energy
//! component of LMP must use a per-island reference bus price, not a single
//! global slack bus.
//!
//! For single-island networks (the common case), this collapses to the
//! existing behavior with zero overhead beyond a cheap BFS.

use std::collections::HashMap;

use surge_ac::topology::islands::{IslandInfo, detect_islands};
use surge_network::Network;
use surge_network::network::BusType;

/// Per-island reference bus mapping for LMP energy decomposition.
///
/// For single-island networks, `ref_bus[0]` is the original slack bus and
/// `bus_island` maps every bus to island 0.
#[derive(Debug, Clone)]
pub struct IslandRefs {
    /// Number of islands.
    pub n_islands: usize,
    /// Island ID for each internal bus index.
    pub bus_island: Vec<usize>,
    /// Reference bus (internal index) for each island.
    pub island_ref_bus: Vec<usize>,
}

/// Detect islands and pick one reference bus per island.
///
/// Reference bus selection priority:
/// 1. Slack bus (BusType::Slack) in the island
/// 2. First PV bus in the island
/// 3. First bus in the island (PQ fallback)
pub fn detect_island_refs(network: &Network, bus_map: &HashMap<u32, usize>) -> IslandRefs {
    let n_bus = network.buses.len();

    let info: IslandInfo = detect_islands(network, bus_map);

    let mut bus_island = vec![0usize; n_bus];
    let mut island_ref_bus = Vec::with_capacity(info.n_islands);

    for (island_id, component) in info.components.iter().enumerate() {
        // Map each bus to its island
        for &bus_idx in component {
            bus_island[bus_idx] = island_id;
        }

        // Pick reference bus: prefer Slack > PV > first bus
        let mut ref_bus = component[0];
        let mut best_priority = 0u8; // 0=PQ, 1=PV, 2=Slack
        for &bus_idx in component {
            let prio = match network.buses[bus_idx].bus_type {
                BusType::Slack => 2,
                BusType::PV => 1,
                _ => 0,
            };
            if prio > best_priority {
                best_priority = prio;
                ref_bus = bus_idx;
            }
        }
        island_ref_bus.push(ref_bus);
    }

    IslandRefs {
        n_islands: info.n_islands,
        bus_island,
        island_ref_bus,
    }
}

/// Decompose LMP into energy + congestion (lossless DC formulation).
///
/// Returns `(lmp_energy, lmp_congestion, lmp_loss)`.
/// - `lmp_energy[i]` = LMP at bus i's island reference bus
/// - `lmp_congestion[i]` = `lmp[i] - lmp_energy[i]`
/// - `lmp_loss` = all zeros (lossless formulation)
pub fn decompose_lmp_lossless(lmp: &[f64], refs: &IslandRefs) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = lmp.len();
    let mut energy = Vec::with_capacity(n);
    let mut congestion = Vec::with_capacity(n);

    for i in 0..n {
        let ref_bus = refs.island_ref_bus[refs.bus_island[i]];
        let e = lmp[ref_bus];
        energy.push(e);
        congestion.push(lmp[i] - e);
    }

    (energy, congestion, vec![0.0; n])
}

/// Decompose LMP with DC loss factors (penalty factor method).
///
/// Returns `(lmp_energy, lmp_congestion, lmp_loss)` following the ISO-standard
/// convention used by ERCOT, PJM, CAISO, etc.:
///
/// - `lmp_energy[i]` = λ_ref (spatially uniform per island — the reference bus LMP)
/// - `lmp_loss[i]`   = λ_ref × (PF_i / PF_ref − 1)  (penalty factor marginal loss)
/// - `lmp_congestion[i]` = lmp\[i\] − λ_ref × PF_i / PF_ref  (residual)
///
/// The identity `lmp[i] = energy[i] + loss[i] + congestion[i]` always holds.
pub fn decompose_lmp_with_losses(
    lmp: &[f64],
    dloss_dp: &[f64],
    refs: &IslandRefs,
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = lmp.len();
    let mut energy = Vec::with_capacity(n);
    let mut loss = Vec::with_capacity(n);
    let mut congestion = Vec::with_capacity(n);

    for i in 0..n {
        let island = refs.bus_island[i];
        let ref_bus = refs.island_ref_bus[island];
        let lambda_e = lmp[ref_bus];
        let pf_i = 1.0 / (1.0 - dloss_dp[i]).max(0.01);
        let pf_ref = 1.0 / (1.0 - dloss_dp[ref_bus]).max(0.01);
        let l = lambda_e * (pf_i / pf_ref - 1.0);
        energy.push(lambda_e);
        loss.push(l);
        congestion.push(lmp[i] - lambda_e - l);
    }

    (energy, congestion, loss)
}

/// Check if removing `outaged_br` disconnects `from_idx` from `to_idx`.
///
/// If so, returns `Some(ref_bus)` — a bus on the `to_idx` side that should
/// be fixed as an additional reference bus.  Returns `None` if the graph
/// remains connected after the outage (i.e. the branch is not a bridge).
///
/// Uses BFS from `to_idx` excluding `outaged_br`.  O(V+E) worst case,
/// but in practice the BFS terminates early when `from_idx` is found
/// (returns `None`) or when the component is exhausted (returns the first
/// Slack/PV/PQ bus found).
pub fn find_split_ref_bus(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    island_refs: &IslandRefs,
    from_idx: usize,
    to_idx: usize,
    outaged_br: usize,
) -> Option<usize> {
    use std::collections::VecDeque;

    let n_bus = network.buses.len();
    let mut visited = vec![false; n_bus];
    let mut queue = VecDeque::new();
    visited[to_idx] = true;
    queue.push_back(to_idx);

    // BFS from to_idx, excluding the outaged branch
    while let Some(u) = queue.pop_front() {
        if u == from_idx {
            return None; // Still connected — not a bridge
        }
        for (bi, br) in network.branches.iter().enumerate() {
            if bi == outaged_br || !br.in_service || br.x.abs() < 1e-20 {
                continue;
            }
            let Some(&f) = bus_map.get(&br.from_bus) else {
                continue;
            };
            let Some(&t) = bus_map.get(&br.to_bus) else {
                continue;
            };
            let neighbor = if f == u {
                t
            } else if t == u {
                f
            } else {
                continue;
            };
            if !visited[neighbor] {
                visited[neighbor] = true;
                queue.push_back(neighbor);
            }
        }
    }

    // from_idx was NOT reached → bridge split detected.
    // Pick a ref bus for the to_idx side: prefer existing ref if it's on this side,
    // otherwise pick Slack > PV > first bus in the to_idx component.
    let island = island_refs.bus_island[to_idx];
    let existing_ref = island_refs.island_ref_bus[island];
    if visited[existing_ref] {
        // Existing ref is already on the to_idx side — no extra ref needed for this side,
        // but from_idx side needs one.  Pick best bus on from_idx side.
        let mut best = from_idx;
        let mut best_prio = 0u8;
        for (i, &v) in visited.iter().enumerate().take(n_bus) {
            if !v && island_refs.bus_island[i] == island {
                let prio = match network.buses[i].bus_type {
                    BusType::Slack => 2,
                    BusType::PV => 1,
                    _ => 0,
                };
                if prio > best_prio {
                    best_prio = prio;
                    best = i;
                }
            }
        }
        Some(best)
    } else {
        // Existing ref is on the from_idx side — pick best bus on to_idx side.
        let mut best = to_idx;
        let mut best_prio = 0u8;
        for (i, &v) in visited.iter().enumerate().take(n_bus) {
            if v {
                let prio = match network.buses[i].bus_type {
                    BusType::Slack => 2,
                    BusType::PV => 1,
                    _ => 0,
                };
                if prio > best_prio {
                    best_prio = prio;
                    best = i;
                }
            }
        }
        Some(best)
    }
}

/// Fix angle (θ) column bounds for all island reference buses.
///
/// Each island needs one bus with θ = 0 (otherwise the B sub-block for
/// that island is singular).  All other buses get [-π, π].
pub fn fix_island_theta_bounds(
    col_lower: &mut [f64],
    col_upper: &mut [f64],
    theta_offset: usize,
    n_bus: usize,
    refs: &IslandRefs,
) {
    use std::f64::consts::PI;

    // First, set all θ to [-π, π]
    for i in 0..n_bus {
        col_lower[theta_offset + i] = -PI;
        col_upper[theta_offset + i] = PI;
    }

    // Then fix each island's reference bus to 0
    for &ref_bus in &refs.island_ref_bus {
        col_lower[theta_offset + ref_bus] = 0.0;
        col_upper[theta_offset + ref_bus] = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_island_lossless() {
        // Single island: energy is uniform, congestion is residual
        let lmp = vec![25.0, 30.0, 20.0];
        let refs = IslandRefs {
            n_islands: 1,
            bus_island: vec![0, 0, 0],
            island_ref_bus: vec![0], // bus 0 is ref
        };

        let (energy, congestion, loss) = decompose_lmp_lossless(&lmp, &refs);
        assert_eq!(energy, vec![25.0, 25.0, 25.0]);
        assert!((congestion[0]).abs() < 1e-12);
        assert!((congestion[1] - 5.0).abs() < 1e-12);
        assert!((congestion[2] - (-5.0)).abs() < 1e-12);
        assert!(loss.iter().all(|&l| l == 0.0));
    }

    #[test]
    fn test_two_island_lossless() {
        // Two islands with different energy prices
        let lmp = vec![20.0, 22.0, 50.0, 48.0];
        let refs = IslandRefs {
            n_islands: 2,
            bus_island: vec![0, 0, 1, 1],
            island_ref_bus: vec![0, 2], // bus 0 ref for island 0, bus 2 for island 1
        };

        let (energy, congestion, _) = decompose_lmp_lossless(&lmp, &refs);
        // Island 0: energy = 20
        assert!((energy[0] - 20.0).abs() < 1e-12);
        assert!((energy[1] - 20.0).abs() < 1e-12);
        // Island 1: energy = 50
        assert!((energy[2] - 50.0).abs() < 1e-12);
        assert!((energy[3] - 50.0).abs() < 1e-12);

        // Congestion within each island
        assert!((congestion[0]).abs() < 1e-12);
        assert!((congestion[1] - 2.0).abs() < 1e-12);
        assert!((congestion[2]).abs() < 1e-12);
        assert!((congestion[3] - (-2.0)).abs() < 1e-12);
    }

    #[test]
    fn test_lossy_decomposition_iso_convention() {
        // Verify energy is spatially uniform per island (ISO convention)
        // and loss captures the penalty factor variation.
        let lmp = vec![25.0, 28.0, 22.0];
        let dloss_dp = vec![0.0, 0.05, -0.02]; // ref bus 0 has zero loss sensitivity
        let refs = IslandRefs {
            n_islands: 1,
            bus_island: vec![0, 0, 0],
            island_ref_bus: vec![0],
        };

        let (energy, congestion, loss) = decompose_lmp_with_losses(&lmp, &dloss_dp, &refs);

        // Energy must be spatially uniform = λ_ref = 25.0
        assert!((energy[0] - 25.0).abs() < 1e-12, "energy[0]={}", energy[0]);
        assert!((energy[1] - 25.0).abs() < 1e-12, "energy[1]={}", energy[1]);
        assert!((energy[2] - 25.0).abs() < 1e-12, "energy[2]={}", energy[2]);

        // Loss at ref bus = 0 (PF_ref/PF_ref - 1 = 0)
        assert!((loss[0]).abs() < 1e-12, "loss[0]={}", loss[0]);
        // Loss at bus 1: λ_ref * (PF_1/PF_ref - 1)
        // PF_1 = 1/(1-0.05) = 1/0.95, PF_ref = 1/(1-0) = 1.0
        // loss[1] = 25.0 * (1/0.95 - 1) = 25.0 * 0.05263... ≈ 1.3158
        let expected_loss1 = 25.0 * (1.0 / 0.95 - 1.0);
        assert!(
            (loss[1] - expected_loss1).abs() < 1e-10,
            "loss[1]={}",
            loss[1]
        );

        // Identity must hold for all buses
        for i in 0..3 {
            let decomp = energy[i] + congestion[i] + loss[i];
            assert!(
                (lmp[i] - decomp).abs() < 1e-10,
                "identity violated at bus {}: lmp={}, decomp={}",
                i,
                lmp[i],
                decomp
            );
        }
    }

    #[test]
    fn test_theta_bounds() {
        let n_bus = 5;
        let theta_offset = 0;
        let mut col_lower = vec![0.0; n_bus];
        let mut col_upper = vec![0.0; n_bus];

        let refs = IslandRefs {
            n_islands: 2,
            bus_island: vec![0, 0, 0, 1, 1],
            island_ref_bus: vec![0, 3],
        };

        fix_island_theta_bounds(&mut col_lower, &mut col_upper, theta_offset, n_bus, &refs);

        use std::f64::consts::PI;
        // Ref buses fixed at 0
        assert_eq!(col_lower[0], 0.0);
        assert_eq!(col_upper[0], 0.0);
        assert_eq!(col_lower[3], 0.0);
        assert_eq!(col_upper[3], 0.0);
        // Non-ref buses at [-π, π]
        assert_eq!(col_lower[1], -PI);
        assert_eq!(col_upper[1], PI);
        assert_eq!(col_lower[2], -PI);
        assert_eq!(col_upper[2], PI);
        assert_eq!(col_lower[4], -PI);
        assert_eq!(col_upper[4], PI);
    }
}
