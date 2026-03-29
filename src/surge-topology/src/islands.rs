// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Island detection — connected-component discovery on the in-service branch graph.
//!
//! Finds connected components using BFS. Each connected component ("island")
//! can be solved independently by a power flow solver.

use std::collections::HashMap;
use std::collections::VecDeque;

use surge_network::Network;

/// Result of island detection.
pub struct IslandInfo {
    /// Number of distinct islands found.
    pub n_islands: usize,
    /// `components[k]` is a sorted list of global bus indices in island k.
    pub components: Vec<Vec<usize>>,
}

/// Detect connected components in the in-service branch graph via BFS.
///
/// Isolated buses (no in-service branch to any other bus) form their own
/// single-bus island.
///
/// `bus_map` maps external bus number → internal bus index.
pub fn detect_islands(network: &Network, bus_map: &HashMap<u32, usize>) -> IslandInfo {
    let n = network.buses.len();
    if n == 0 {
        return IslandInfo {
            n_islands: 0,
            components: vec![],
        };
    }

    // Build adjacency list (undirected, in-service branches only)
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for branch in &network.branches {
        if !branch.in_service {
            continue;
        }
        let f = match bus_map.get(&branch.from_bus) {
            Some(&i) => i,
            None => continue,
        };
        let t = match bus_map.get(&branch.to_bus) {
            Some(&i) => i,
            None => continue,
        };
        if f != t {
            adj[f].push(t);
            adj[t].push(f);
        }
    }

    // BFS over all buses
    let mut visited = vec![false; n];
    let mut components: Vec<Vec<usize>> = Vec::new();

    for start in 0..n {
        if visited[start] {
            continue;
        }
        let mut component: Vec<usize> = Vec::new();
        let mut queue: VecDeque<usize> = VecDeque::new();
        queue.push_back(start);
        visited[start] = true;

        while let Some(bus) = queue.pop_front() {
            component.push(bus);
            for &neighbor in &adj[bus] {
                if !visited[neighbor] {
                    visited[neighbor] = true;
                    queue.push_back(neighbor);
                }
            }
        }

        component.sort_unstable();
        components.push(component);
    }

    let n_islands = components.len();
    IslandInfo {
        n_islands,
        components,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    fn make_bus(number: u32, bus_type: BusType) -> Bus {
        let mut b = Bus::new(number, bus_type, 100.0);
        b.voltage_magnitude_pu = 1.0;
        b.voltage_angle_rad = 0.0;
        b
    }

    fn make_line(from_bus: u32, to_bus: u32) -> Branch {
        Branch::new_line(from_bus, to_bus, 0.01, 0.1, 0.0)
    }

    fn make_gen(bus: u32, pg: f64, vs: f64, qmin: f64, qmax: f64) -> Generator {
        let mut g = Generator::new(bus, pg, vs);
        g.qmin = qmin;
        g.qmax = qmax;
        g
    }

    /// 3-bus network: bus 1-2-3 connected, bus 4 isolated.
    /// Expected: 2 islands — {0,1,2} and {3}.
    #[test]
    fn test_island_detection_simple() {
        let mut net = Network::new("test");
        net.buses = vec![
            make_bus(1, BusType::Slack),
            make_bus(2, BusType::PV),
            make_bus(3, BusType::PQ),
            make_bus(4, BusType::PQ), // isolated
        ];
        net.branches = vec![make_line(1, 2), make_line(2, 3)];
        net.generators = vec![make_gen(1, 1.0, 1.0, -0.5, 0.5)];

        let bus_map = net.bus_index_map();
        let info = detect_islands(&net, &bus_map);

        assert_eq!(info.n_islands, 2, "expected 2 islands");

        // Find main island and isolated island
        let main_island: &Vec<usize> = info
            .components
            .iter()
            .find(|c| c.len() == 3)
            .expect("expected an island of size 3");
        assert!(main_island.contains(&0));
        assert!(main_island.contains(&1));
        assert!(main_island.contains(&2));

        let iso_island: &Vec<usize> = info
            .components
            .iter()
            .find(|c| c.len() == 1)
            .expect("expected an isolated bus island");
        assert_eq!(iso_island[0], 3);
    }
}
