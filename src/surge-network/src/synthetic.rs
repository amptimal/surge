#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Synthetic power network generator for ML training datasets (P5-B09).
//!
//! Generates power-flow-valid synthetic networks at scale (1 k–100 k buses)
//! for machine learning training data generation. Networks are:
//!
//! - **Connected** — every bus is reachable from the slack bus.
//! - **Physically plausible** — branch impedances, loads, and generation are
//!   sampled from ranges consistent with real transmission systems.
//! - **Reproducible** — fixed seed yields identical topology and parameters.
//! - **Generation surplus** — total generation always exceeds total load, so
//!   Newton-Raphson power flow will have a feasible solution.
//!
//! # Example
//!
//! ```rust
//! use surge_network::synthetic::{SyntheticNetworkConfig, SyntheticTopology, generate_synthetic_network};
//!
//! let config = SyntheticNetworkConfig {
//!     n_buses: 100,
//!     avg_degree: 2.5,
//!     gen_fraction: 0.2,
//!     voltage_levels: vec![345.0, 138.0, 69.0],
//!     seed: 42,
//!     topology: SyntheticTopology::BarabasiAlbert { m: 2 },
//! };
//! let net = generate_synthetic_network(&config);
//! assert_eq!(net.n_buses(), 100);
//! ```

use crate::Network;
use crate::network::{Branch, BranchType, Bus, BusType, Generator, Load};

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Topology model for synthetic network generation.
#[derive(Debug, Clone)]
pub enum SyntheticTopology {
    /// Random sparse graph (Erdős–Rényi G(n, p) with p = avg_degree / n).
    ///
    /// Produces Poisson-distributed degree sequences. A spanning tree is
    /// added first to guarantee connectivity.
    Random,

    /// Barabási–Albert preferential attachment.
    ///
    /// Each new bus connects to `m` existing buses with probability
    /// proportional to their current degree. Produces scale-free networks
    /// with a power-law degree distribution, which resembles real
    /// transmission grids.
    ///
    /// `m` must be ≥ 1 and ≤ n_buses − 1. Recommended: 2–3.
    BarabasiAlbert { m: usize },

    /// Watts-Strogatz small-world model.
    ///
    /// Start from a ring lattice where each bus connects to its `k` nearest
    /// neighbors (k/2 on each side). Then rewire each edge to a random bus
    /// with probability `beta`. Produces high clustering and short path lengths.
    ///
    /// `k` must be even and ≥ 2. `beta` ∈ [0, 1].
    SmallWorld { k: usize, beta: f64 },
}

/// Configuration for synthetic network generation.
#[derive(Debug, Clone)]
pub struct SyntheticNetworkConfig {
    /// Number of buses in the generated network. Must be ≥ 2.
    pub n_buses: usize,

    /// Average node degree (edges per bus). Used by the Random topology as the
    /// edge probability parameter. For BarabasiAlbert the effective degree is
    /// approximately 2·m. For SmallWorld the initial degree is `k`.
    pub avg_degree: f64,

    /// Fraction of non-slack buses assigned as generators (PV buses).
    /// Value in [0, 1]. At least one bus (bus 0) is always the slack generator.
    pub gen_fraction: f64,

    /// Base voltage levels to assign (kV). Each bus randomly gets one of these.
    /// Branches between buses at different voltage levels are treated as
    /// transformers (tap ≠ 1.0). Must be non-empty.
    pub voltage_levels: Vec<f64>,

    /// Random seed for reproducibility. Same seed → identical network.
    pub seed: u64,

    /// Topology model.
    pub topology: SyntheticTopology,
}

impl Default for SyntheticNetworkConfig {
    fn default() -> Self {
        Self {
            n_buses: 100,
            avg_degree: 2.5,
            gen_fraction: 0.2,
            voltage_levels: vec![345.0, 138.0],
            seed: 0,
            topology: SyntheticTopology::BarabasiAlbert { m: 2 },
        }
    }
}

// ---------------------------------------------------------------------------
// Lightweight PRNG (xoshiro256** — no external dependency required)
// ---------------------------------------------------------------------------

/// xoshiro256** PRNG state — 64-bit output, period 2^256 − 1.
struct Rng {
    s: [u64; 4],
}

impl Rng {
    fn new(seed: u64) -> Self {
        // Seed with splitmix64 to avoid all-zero state
        fn splitmix64(x: &mut u64) -> u64 {
            *x = x.wrapping_add(0x9e3779b97f4a7c15);
            let mut z = *x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
            z ^ (z >> 31)
        }
        let mut s = seed;
        let a = splitmix64(&mut s);
        let b = splitmix64(&mut s);
        let c = splitmix64(&mut s);
        let d = splitmix64(&mut s);
        Self { s: [a, b, c, d] }
    }

    fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform float in [0, 1).
    fn next_f64(&mut self) -> f64 {
        // Use 53 mantissa bits
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform float in [lo, hi).
    fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.next_f64() * (hi - lo)
    }

    /// Uniform usize in [0, n).
    fn usize_below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

// ---------------------------------------------------------------------------
// BFS reachability check
// ---------------------------------------------------------------------------

/// Returns a Vec<bool> where `reachable[i]` is true if bus `i` is reachable
/// from `start` via the provided adjacency list.
fn bfs_reachable(adj: &[Vec<usize>], start: usize) -> Vec<bool> {
    let n = adj.len();
    let mut visited = vec![false; n];
    let mut queue = std::collections::VecDeque::new();
    if n == 0 {
        return visited;
    }
    visited[start] = true;
    queue.push_back(start);
    while let Some(u) = queue.pop_front() {
        for &v in &adj[u] {
            if !visited[v] {
                visited[v] = true;
                queue.push_back(v);
            }
        }
    }
    visited
}

// ---------------------------------------------------------------------------
// Topology generators
// ---------------------------------------------------------------------------

/// Returns an edge list (undirected, no self-loops, no duplicates).
fn build_random_graph(n: usize, avg_degree: f64, rng: &mut Rng) -> Vec<(usize, usize)> {
    let p = (avg_degree / (n.saturating_sub(1)) as f64).min(1.0);
    let mut edges = Vec::new();
    // Guaranteed spanning tree first
    edges.extend(random_spanning_tree(n, rng));
    // Additional random edges
    for i in 0..n {
        for j in (i + 1)..n {
            if rng.next_f64() < p {
                edges.push((i, j));
            }
        }
    }
    deduplicate_edges(edges)
}

/// Returns a random spanning tree using Wilson's algorithm (loop-erased random walk).
fn random_spanning_tree(n: usize, rng: &mut Rng) -> Vec<(usize, usize)> {
    if n <= 1 {
        return Vec::new();
    }
    let mut in_tree = vec![false; n];
    let mut next = vec![0usize; n];
    in_tree[0] = true;
    let mut edges = Vec::with_capacity(n - 1);

    for start in 1..n {
        if in_tree[start] {
            continue;
        }
        // Loop-erased random walk from start until we hit a tree node
        let mut u = start;
        while !in_tree[u] {
            next[u] = rng.usize_below(n);
            u = next[u];
        }
        // Commit the walk
        u = start;
        while !in_tree[u] {
            in_tree[u] = true;
            let v = next[u];
            edges.push((u.min(v), u.max(v)));
            u = v;
        }
    }
    edges
}

fn build_barabasi_albert(n: usize, m: usize, rng: &mut Rng) -> Vec<(usize, usize)> {
    let m = m.max(1).min(n.saturating_sub(1));
    let mut edges: Vec<(usize, usize)> = Vec::new();
    // degree array: degree[i] = current degree of node i (used for preferential attachment)
    let mut degree = vec![0usize; n];

    // Bootstrap: fully connect the first m+1 nodes (ensures initial degree > 0)
    let init = (m + 1).min(n);
    for i in 0..init {
        for j in (i + 1)..init {
            edges.push((i, j));
            degree[i] += 1;
            degree[j] += 1;
        }
    }

    // Grow: attach each new node to m existing nodes with PA probability
    for new_node in init..n {
        let total_degree: usize = degree[..new_node].iter().sum();
        let mut targets = Vec::with_capacity(m);
        let mut attempts = 0usize;

        while targets.len() < m && attempts < 10 * m * n {
            attempts += 1;
            // Pick a target proportional to degree (stub selection)
            let r = rng.usize_below(total_degree.max(1));
            let mut cum = 0;
            let mut target = 0;
            for (i, &d) in degree[..new_node].iter().enumerate() {
                cum += d;
                if r < cum {
                    target = i;
                    break;
                }
            }
            // Fallback to uniform if total_degree was 0
            if total_degree == 0 {
                target = rng.usize_below(new_node);
            }
            if !targets.contains(&target) {
                targets.push(target);
            }
        }

        for t in targets {
            let u = t.min(new_node);
            let v = t.max(new_node);
            edges.push((u, v));
            degree[u] += 1;
            degree[v] += 1;
        }
    }

    deduplicate_edges(edges)
}

fn build_small_world(n: usize, k: usize, beta: f64, rng: &mut Rng) -> Vec<(usize, usize)> {
    let k = k.max(2) & !1; // ensure even, ≥ 2
    let half_k = k / 2;
    let mut edges: Vec<(usize, usize)> = Vec::new();

    // Regular ring lattice
    for i in 0..n {
        for d in 1..=half_k {
            let j = (i + d) % n;
            edges.push((i.min(j), i.max(j)));
        }
    }

    // Rewire with probability beta
    let edges_snap: Vec<(usize, usize)> = edges.clone();
    for (i, j) in &edges_snap {
        if rng.next_f64() < beta {
            // Rewire: replace j with a random node != i, avoiding existing edges
            let new_j = loop {
                let candidate = rng.usize_below(n);
                if candidate != *i && !edges.contains(&((*i).min(candidate), (*i).max(candidate))) {
                    break candidate;
                }
            };
            let old_edge = (*i, *j);
            if let Some(pos) = edges.iter().position(|&e| e == old_edge) {
                edges[pos] = ((*i).min(new_j), (*i).max(new_j));
            }
        }
    }

    // Ensure connectivity by connecting any isolated components
    let mut adj = vec![Vec::new(); n];
    for &(u, v) in &edges {
        adj[u].push(v);
        adj[v].push(u);
    }
    let reachable = bfs_reachable(&adj, 0);
    for i in 0..n {
        if !reachable[i] {
            // Connect to a reachable node
            let target = rng.usize_below(i.max(1));
            let u = i.min(target);
            let v = i.max(target);
            edges.push((u, v));
        }
    }

    deduplicate_edges(edges)
}

fn deduplicate_edges(mut edges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    edges.sort_unstable();
    edges.dedup();
    // Remove self-loops
    edges.retain(|&(u, v)| u != v);
    edges
}

// ---------------------------------------------------------------------------
// Main generator
// ---------------------------------------------------------------------------

/// Generate a synthetic power network.
///
/// The returned [`Network`] is a valid power system model suitable for
/// Newton-Raphson AC power flow. Total generation ≥ total load + 10% headroom.
///
/// # Panics
///
/// Panics if `config.n_buses < 2` or `config.voltage_levels` is empty.
pub fn generate_synthetic_network(config: &SyntheticNetworkConfig) -> Network {
    assert!(config.n_buses >= 2, "n_buses must be ≥ 2");
    assert!(
        !config.voltage_levels.is_empty(),
        "voltage_levels must be non-empty"
    );

    let n = config.n_buses;
    let mut rng = Rng::new(config.seed);

    // --- 1. Assign voltage levels to buses ---
    let vl = &config.voltage_levels;
    let bus_kv: Vec<f64> = (0..n).map(|_| vl[rng.usize_below(vl.len())]).collect();

    // --- 2. Generate topology ---
    let edges = match &config.topology {
        SyntheticTopology::Random => build_random_graph(n, config.avg_degree, &mut rng),
        SyntheticTopology::BarabasiAlbert { m } => build_barabasi_albert(n, *m, &mut rng),
        SyntheticTopology::SmallWorld { k, beta } => build_small_world(n, *k, *beta, &mut rng),
    };

    // --- 3. Guarantee connectivity (add any missing spanning tree edges) ---
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(u, v) in &edges {
        adj[u].push(v);
        adj[v].push(u);
    }

    // Check connectivity and add edges to connect any isolated components
    let mut all_edges = edges;
    let mut uf = UnionFindIdx::new(n);
    for &(u, v) in &all_edges {
        uf.union(u, v);
    }
    // For each bus not connected to bus 0, add an edge to a random connected bus
    for i in 1..n {
        if !uf.same(0, i) {
            // Find a connected node (one that is in the same component as 0)
            let mut j = rng.usize_below(i);
            while !uf.same(0, j) {
                j = (j + 1) % i;
            }
            all_edges.push((j.min(i), j.max(i)));
            uf.union(j, i);
        }
    }
    let all_edges = deduplicate_edges(all_edges);

    // --- 4. Assign bus types ---
    // Bus 0 = slack (always)
    // gen_fraction of remaining buses = PV
    // rest = PQ
    let n_gen = ((n - 1) as f64 * config.gen_fraction).round() as usize;
    // Assign the first n_gen non-slack buses as PV (deterministic for reproducibility)
    let mut bus_types = vec![BusType::PQ; n];
    bus_types[0] = BusType::Slack;
    // Shuffle indices 1..n and pick first n_gen
    let mut pv_indices: Vec<usize> = (1..n).collect();
    // Fisher-Yates partial shuffle for first n_gen elements
    for i in 0..n_gen.min(n - 1) {
        let j = i + rng.usize_below(n - 1 - i);
        pv_indices.swap(i, j);
    }
    for &idx in pv_indices.iter().take(n_gen) {
        bus_types[idx] = BusType::PV;
    }

    // --- 5. Assign loads ---
    // p_load ~ Uniform(0, 0.5) pu, q_load = p_load * tan(acos(0.9)) ≈ 0.4843 * p_load
    // Slack bus has no load (it's the reference)
    let pf_angle = (0.9f64).acos();
    let tan_pf = pf_angle.tan(); // ≈ 0.4843
    let base_mva = 100.0f64;

    let mut bus_pd_mw = vec![0.0f64; n];
    let mut bus_qd_mvar = vec![0.0f64; n];
    for i in 1..n {
        // Load only on PQ buses; PV buses can have load too but smaller
        let load_scale = if bus_types[i] == BusType::PV {
            0.2
        } else {
            0.5
        };
        let p_pu = rng.uniform(0.0, load_scale);
        let q_pu = p_pu * tan_pf;
        bus_pd_mw[i] = p_pu * base_mva;
        bus_qd_mvar[i] = q_pu * base_mva;
    }

    let total_load_mw: f64 = bus_pd_mw.iter().sum();

    // --- 6. Assign generator outputs ---
    // Generators are on slack + PV buses.
    // For PV buses: p_gen ~ Uniform(0.1, 1.0) pu * base_mva
    // Slack bus absorbs any remaining mismatch (no explicit pg; NR handles it).
    let gen_buses: Vec<usize> = std::iter::once(0)
        .chain(pv_indices.iter().take(n_gen).copied())
        .collect();

    // Assign pg for PV buses; slack pg is set later for headroom.
    let mut gen_pg: Vec<f64> = gen_buses
        .iter()
        .map(|&i| {
            if i == 0 {
                0.0 // placeholder; set after summing PV output
            } else {
                rng.uniform(0.1, 1.0) * base_mva
            }
        })
        .collect();

    let pv_total: f64 = gen_pg[1..].iter().sum();
    // Slack bus pg: whatever is needed to cover total load + 10% headroom
    let slack_pg = (total_load_mw - pv_total + total_load_mw * 0.1).max(50.0);
    gen_pg[0] = slack_pg;

    // --- 7. Build the Network ---
    let mut net = Network::new("synthetic");
    net.base_mva = base_mva;

    // Buses
    for i in 0..n {
        let bus_num = (i + 1) as u32; // 1-indexed external numbers
        let mut bus = Bus::new(bus_num, bus_types[i], bus_kv[i]);
        bus.voltage_magnitude_pu = 1.0;
        bus.voltage_angle_rad = 0.0;
        bus.voltage_max_pu = 1.1;
        bus.voltage_min_pu = 0.9;
        net.buses.push(bus);
        // Create Load object for buses with nonzero demand.
        if bus_pd_mw[i].abs() > 1e-10 || bus_qd_mvar[i].abs() > 1e-10 {
            net.loads
                .push(Load::new(bus_num, bus_pd_mw[i], bus_qd_mvar[i]));
        }
    }

    // Generators
    for (pos, &bus_idx) in gen_buses.iter().enumerate() {
        let bus_num = (bus_idx + 1) as u32;
        let pg = gen_pg[pos];
        let vs = 1.04; // nominal voltage setpoint
        let mut g = Generator::new(bus_num, pg, vs);
        g.pmax = pg * 1.5 + 50.0;
        g.pmin = 0.0;
        g.qmax = pg * 0.6 + 10.0;
        g.qmin = -(pg * 0.3 + 5.0);
        net.generators.push(g);
    }

    // Branches
    for &(i, j) in &all_edges {
        let bus_i_num = (i + 1) as u32;
        let bus_j_num = (j + 1) as u32;
        let kv_i = bus_kv[i];
        let kv_j = bus_kv[j];

        // Scale impedance by voltage level ratio (higher kV → lower pu impedance)
        let kv_base = kv_i.max(kv_j).max(1.0);
        let scale = (345.0 / kv_base).sqrt().clamp(0.5, 2.0);

        let r = rng.uniform(0.001, 0.05) * scale;
        let x = rng.uniform(0.01, 0.20) * scale;
        let b = rng.uniform(0.0, 0.05) / scale;

        let tap = if (kv_i - kv_j).abs() > 1.0 {
            // Transformer: tap = from/to voltage ratio
            (kv_i / kv_j).clamp(0.5, 2.0)
        } else {
            1.0
        };

        let mut br = Branch::new_line(bus_i_num, bus_j_num, r, x, b);
        br.tap = tap;
        br.rating_a_mva = rng.uniform(100.0, 1000.0); // MVA rating
        if (tap - 1.0).abs() > 1e-6 {
            br.branch_type = BranchType::Transformer;
        }
        net.branches.push(br);
    }
    net
}

// ---------------------------------------------------------------------------
// Lightweight disjoint-set for connectivity checks
// ---------------------------------------------------------------------------

struct UnionFindIdx {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFindIdx {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cur = x;
        while cur != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        match self.rank[ra].cmp(&self.rank[rb]) {
            std::cmp::Ordering::Less => self.parent[ra] = rb,
            std::cmp::Ordering::Greater => self.parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                self.parent[rb] = ra;
                self.rank[ra] += 1;
            }
        }
    }

    fn same(&mut self, a: usize, b: usize) -> bool {
        self.find(a) == self.find(b)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config(n: usize, seed: u64) -> SyntheticNetworkConfig {
        SyntheticNetworkConfig {
            n_buses: n,
            avg_degree: 2.5,
            gen_fraction: 0.2,
            voltage_levels: vec![345.0, 138.0],
            seed,
            topology: SyntheticTopology::BarabasiAlbert { m: 2 },
        }
    }

    /// Check that all buses are reachable from the slack bus via BFS.
    fn is_fully_connected(net: &Network) -> bool {
        let n = net.n_buses();
        if n == 0 {
            return true;
        }
        let bus_map = net.bus_index_map();
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for br in &net.branches {
            if !br.in_service {
                continue;
            }
            if let (Some(&fi), Some(&ti)) = (bus_map.get(&br.from_bus), bus_map.get(&br.to_bus)) {
                adj[fi].push(ti);
                adj[ti].push(fi);
            }
        }
        let slack_idx = net.slack_bus_index().unwrap_or(0);
        let reachable = bfs_reachable(&adj, slack_idx);
        reachable.iter().all(|&r| r)
    }

    #[test]
    fn synthetic_100bus_is_connected() {
        let config = default_config(100, 7);
        let net = generate_synthetic_network(&config);
        assert_eq!(net.n_buses(), 100, "bus count mismatch");
        assert!(
            is_fully_connected(&net),
            "100-bus synthetic network is not fully connected"
        );
    }

    #[test]
    fn synthetic_reproducible() {
        let config = default_config(50, 12345);
        let net1 = generate_synthetic_network(&config);
        let net2 = generate_synthetic_network(&config);

        // Same bus count, same branch count
        assert_eq!(net1.n_buses(), net2.n_buses());
        assert_eq!(net1.n_branches(), net2.n_branches());

        // Same bus voltages and loads
        for (b1, b2) in net1.buses.iter().zip(net2.buses.iter()) {
            assert_eq!(b1.number, b2.number, "bus number mismatch");
            assert_eq!(
                b1.bus_type, b2.bus_type,
                "bus type mismatch at bus {}",
                b1.number
            );
            assert!(
                (b1.base_kv - b2.base_kv).abs() < 1e-10,
                "base_kv mismatch at bus {}",
                b1.number
            );
        }
        // Same loads
        let pd1 = net1.bus_load_p_mw();
        let pd2 = net2.bus_load_p_mw();
        for (i, (p1, p2)) in pd1.iter().zip(pd2.iter()).enumerate() {
            assert!(
                (p1 - p2).abs() < 1e-10,
                "pd mismatch at bus index {}: {} vs {}",
                i,
                p1,
                p2
            );
        }

        // Same branch impedances
        for (br1, br2) in net1.branches.iter().zip(net2.branches.iter()) {
            assert_eq!(br1.from_bus, br2.from_bus);
            assert_eq!(br1.to_bus, br2.to_bus);
            assert!(
                (br1.r - br2.r).abs() < 1e-12,
                "r mismatch: {} vs {}",
                br1.r,
                br2.r
            );
            assert!(
                (br1.x - br2.x).abs() < 1e-12,
                "x mismatch: {} vs {}",
                br1.x,
                br2.x
            );
        }
    }

    #[test]
    fn synthetic_barabasi_albert() {
        // Verify that the BA network has a roughly power-law degree distribution:
        // the max degree should be significantly larger than the mean degree.
        // This is a structural property of BA networks (hubs form).
        let config = SyntheticNetworkConfig {
            n_buses: 500,
            avg_degree: 2.5,
            gen_fraction: 0.15,
            voltage_levels: vec![345.0, 138.0, 69.0],
            seed: 99,
            topology: SyntheticTopology::BarabasiAlbert { m: 2 },
        };
        let net = generate_synthetic_network(&config);

        // Compute degree of each bus
        let n = net.n_buses();
        let bus_map = net.bus_index_map();
        let mut degree = vec![0usize; n];
        for br in &net.branches {
            if let (Some(&fi), Some(&ti)) = (bus_map.get(&br.from_bus), bus_map.get(&br.to_bus)) {
                degree[fi] += 1;
                degree[ti] += 1;
            }
        }

        let max_degree = *degree.iter().max().unwrap_or(&0);
        let mean_degree = degree.iter().sum::<usize>() as f64 / n as f64;

        // BA networks always have a hub with degree >> mean
        assert!(
            max_degree as f64 > mean_degree * 2.0,
            "BA degree distribution too uniform: max={max_degree}, mean={mean_degree:.2}"
        );

        // Network must be connected
        assert!(
            is_fully_connected(&net),
            "BA 500-bus network is not fully connected"
        );
    }

    #[test]
    fn synthetic_generation_exceeds_load() {
        let config = default_config(200, 42);
        let net = generate_synthetic_network(&config);

        let total_gen = net.total_generation_mw();
        let total_load = net.total_load_mw();

        assert!(
            total_gen > total_load,
            "Generation ({total_gen:.1} MW) must exceed load ({total_load:.1} MW)"
        );
    }

    #[test]
    fn synthetic_random_topology_connected() {
        let config = SyntheticNetworkConfig {
            n_buses: 80,
            avg_degree: 3.0,
            gen_fraction: 0.25,
            voltage_levels: vec![345.0],
            seed: 7,
            topology: SyntheticTopology::Random,
        };
        let net = generate_synthetic_network(&config);
        assert!(
            is_fully_connected(&net),
            "Random-topology 80-bus network is not fully connected"
        );
    }

    #[test]
    fn synthetic_small_world_topology_connected() {
        let config = SyntheticNetworkConfig {
            n_buses: 60,
            avg_degree: 4.0,
            gen_fraction: 0.2,
            voltage_levels: vec![138.0, 69.0],
            seed: 555,
            topology: SyntheticTopology::SmallWorld { k: 4, beta: 0.1 },
        };
        let net = generate_synthetic_network(&config);
        assert!(
            is_fully_connected(&net),
            "SmallWorld 60-bus network is not fully connected"
        );
    }

    #[test]
    fn synthetic_1000bus_scales() {
        let config = SyntheticNetworkConfig {
            n_buses: 1000,
            avg_degree: 2.5,
            gen_fraction: 0.15,
            voltage_levels: vec![345.0, 230.0, 138.0, 69.0],
            seed: 9999,
            topology: SyntheticTopology::BarabasiAlbert { m: 2 },
        };
        let net = generate_synthetic_network(&config);
        assert_eq!(net.n_buses(), 1000);
        assert!(net.n_branches() > 0);
        assert!(
            is_fully_connected(&net),
            "1000-bus network is not connected"
        );
    }

    #[test]
    fn synthetic_has_slack_bus() {
        let config = default_config(30, 1);
        let net = generate_synthetic_network(&config);
        let slack_count = net.buses.iter().filter(|b| b.is_slack()).count();
        assert_eq!(slack_count, 1, "should have exactly one slack bus");
    }
}
