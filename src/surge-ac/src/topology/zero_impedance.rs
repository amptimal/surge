// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Zero-impedance branch elimination via Union-Find bus merging.
//!
//! Branches with negligible series impedance (|z| < `tol` p.u.) cause Y-bus
//! singularity because their admittance is effectively infinite.  The
//! standard power-systems technique is to *merge* the terminal buses of such
//! branches into a single bus, solve on the reduced network, and then expand
//! the solution back to the original bus numbering.
//!
//! # Algorithm
//!
//! 1. **Union-Find**: identify connected components formed by zero-impedance
//!    branches.  Each component maps to one representative bus.
//! 2. **Contraction**: build a `MergedNetwork` in which each component is
//!    replaced by its representative bus.  Bus-attached equipment is
//!    rebuilt once on the merged buses.
//!    Non-zero-impedance branches between different components are retained.
//! 3. **Solve**: run the power flow on the smaller merged network.
//! 4. **Expansion**: each bus in the original network gets the voltage of its
//!    representative bus.
//!
//! # Usage
//!
//! ```rust,no_run
//! # use surge_ac::topology::zero_impedance::{merge_zero_impedance, expand_pf_solution};
//! # use surge_ac::{solve_ac_pf_kernel, AcPfOptions};
//! # use surge_network::Network;
//! # let network = Network::default();
//! let merged = merge_zero_impedance(&network, 1e-6);
//! let sol = solve_ac_pf_kernel(&merged.network, &AcPfOptions::default()).unwrap();
//! let full_sol = expand_pf_solution(&sol, &merged, &network);
//! ```

use crate::matrix::mismatch::compute_power_injection;
use crate::matrix::ybus::build_ybus;
use surge_network::Network;
use surge_network::network::{Bus, BusType};
use surge_solution::{PfSolution, compute_branch_power_flows};
use tracing::warn;

/// Result of contracting all zero-impedance branch pairs.
#[derive(Debug, Clone)]
pub struct MergedNetwork {
    /// The contracted network with merged buses.  Solve this instead of the
    /// original.
    pub network: Network,
    /// `bus_map[i]` = 0-based index into `network.buses` for original bus `i`.
    ///
    /// Buses in the same zero-impedance component share the same representative
    /// index.  `bus_map.len()` equals the original bus count.
    pub bus_map: Vec<usize>,
    /// Number of original buses.
    pub n_original_buses: usize,
}

// ---------------------------------------------------------------------------
// Union-Find (path-compressed, union-by-rank)
// ---------------------------------------------------------------------------

struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]]; // path halving
            x = self.parent[x];
        }
        x
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
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a contracted network by merging buses connected by zero-impedance branches.
///
/// `tol` is the per-unit impedance threshold: branches with `|r| + |x| < tol`
/// and `tap == 1.0` (within `tap_tol = 1e-4`) and `shift == 0` are treated as
/// zero-impedance ties.
///
/// Returns [`MergedNetwork`] wrapping the contracted network and the mapping.
/// When no zero-impedance branches are found the returned `network` is a clone
/// of the input and `bus_map` is the identity mapping.
pub fn merge_zero_impedance(network: &Network, tol: f64) -> MergedNetwork {
    let n = network.buses.len();
    let bus_ext_to_idx: std::collections::HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    let mut uf = UnionFind::new(n);
    let tap_tol = 1e-4_f64;

    // Identify zero-impedance branches and union their terminal buses.
    for branch in &network.branches {
        if !branch.in_service {
            continue;
        }
        let z_mag = branch.r.abs() + branch.x.abs();
        if z_mag >= tol {
            continue;
        }
        // Only merge transformer branches if tap ≈ 1 and shift ≈ 0.
        if (branch.tap - 1.0).abs() > tap_tol || branch.phase_shift_rad.abs() > tap_tol {
            continue;
        }
        let Some(&fi) = bus_ext_to_idx.get(&branch.from_bus) else {
            continue;
        };
        let Some(&ti) = bus_ext_to_idx.get(&branch.to_bus) else {
            continue;
        };
        uf.union(fi, ti);
    }

    // Compute representative for each bus (bus_map[i] = canonical internal index).
    let mut bus_map = vec![0usize; n];
    for (i, slot) in bus_map.iter_mut().enumerate() {
        *slot = uf.find(i);
    }

    // Check if any merging happened.
    let n_unique: std::collections::HashSet<usize> = bus_map.iter().copied().collect();
    if n_unique.len() == n {
        // No zero-impedance branches — return identity mapping.
        return MergedNetwork {
            network: network.clone(),
            bus_map: (0..n).collect(),
            n_original_buses: n,
        };
    }

    // Renumber representatives to contiguous 0..n_merged.
    let mut repr_to_merged: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    let mut merged_buses: Vec<Bus> = Vec::new();

    // Assign merged indices in original bus order (first occurrence of each representative).
    // First pass: build repr_to_merged and merged_buses (iterate by value).
    for &repr in &bus_map {
        if let std::collections::hash_map::Entry::Vacant(e) = repr_to_merged.entry(repr) {
            let merged_idx = merged_buses.len();
            e.insert(merged_idx);
            // Clone the representative bus, reset shunt injections (will be summed below).
            let mut b = network.buses[repr].clone();
            b.shunt_conductance_mw = 0.0;
            b.shunt_susceptance_mvar = 0.0;
            merged_buses.push(b);
        }
    }
    // Second pass: remap bus_map entries from representative → merged index.
    for slot in &mut bus_map {
        *slot = repr_to_merged[slot];
    }

    // Sum bus shunt injections into the merged buses.
    for (i, orig_bus) in network.buses.iter().enumerate() {
        let mi = bus_map[i];
        merged_buses[mi].shunt_conductance_mw += orig_bus.shunt_conductance_mw;
        merged_buses[mi].shunt_susceptance_mvar += orig_bus.shunt_susceptance_mvar;
        // Merge bus types: Slack > PV > PQ.
        match orig_bus.bus_type {
            BusType::Slack => merged_buses[mi].bus_type = BusType::Slack,
            BusType::PV if merged_buses[mi].bus_type == BusType::PQ => {
                merged_buses[mi].bus_type = BusType::PV;
            }
            _ => {}
        }
    }

    // Build a mapping from original external bus number to merged external bus number.
    // Merged bus keeps the representative's external number.
    let old_ext_to_merged_ext: std::collections::HashMap<u32, u32> = network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, merged_buses[bus_map[i]].number))
        .collect();

    // Helper: remap an external bus number; unknown numbers pass through unchanged.
    let remap = |num: u32| -> u32 { *old_ext_to_merged_ext.get(&num).unwrap_or(&num) };

    // Build merged network with all non-bus-topology fields preserved.
    let mut merged_net = Network {
        name: network.name.clone(),
        base_mva: network.base_mva,
        freq_hz: network.freq_hz,
        buses: merged_buses,
        branches: Vec::new(),   // filled below
        generators: Vec::new(), // filled below
        loads: Vec::new(),      // filled below
        // Switched shunts (discrete NR controls): remap external bus numbers.
        controls: surge_network::network::NetworkControlData {
            switched_shunts: network
                .controls
                .switched_shunts
                .iter()
                .map(|s| {
                    let mut m = s.clone();
                    m.bus = remap(s.bus);
                    m.bus_regulated = remap(s.bus_regulated);
                    m
                })
                .collect(),
            // OPF switched shunts: remap external bus numbers.
            switched_shunts_opf: network
                .controls
                .switched_shunts_opf
                .iter()
                .map(|s| {
                    let mut m = s.clone();
                    m.bus = remap(s.bus);
                    m
                })
                .collect(),
            // OLTC/PAR specs: external bus numbers — remap so they match the merged bus numbers.
            oltc_specs: network
                .controls
                .oltc_specs
                .iter()
                .map(|s| {
                    let mut m = s.clone();
                    m.from_bus = remap(s.from_bus);
                    m.to_bus = remap(s.to_bus);
                    m.regulated_bus = remap(s.regulated_bus);
                    m
                })
                .collect(),
            par_specs: network
                .controls
                .par_specs
                .iter()
                .map(|s| {
                    let mut m = s.clone();
                    m.from_bus = remap(s.from_bus);
                    m.to_bus = remap(s.to_bus);
                    m.monitored_from_bus = remap(s.monitored_from_bus);
                    m.monitored_to_bus = remap(s.monitored_to_bus);
                    m
                })
                .collect(),
        }, // end controls
        hvdc: surge_network::network::HvdcModel {
            links: network
                .hvdc
                .links
                .iter()
                .map(|link| {
                    let mut mapped = link.clone();
                    match &mut mapped {
                        surge_network::network::HvdcLink::Lcc(lcc) => {
                            lcc.rectifier.bus = remap(lcc.rectifier.bus);
                            lcc.inverter.bus = remap(lcc.inverter.bus);
                        }
                        surge_network::network::HvdcLink::Vsc(vsc) => {
                            vsc.converter1.bus = remap(vsc.converter1.bus);
                            vsc.converter2.bus = remap(vsc.converter2.bus);
                        }
                    }
                    mapped
                })
                .collect(),
            dc_grids: network
                .hvdc
                .dc_grids
                .iter()
                .map(|grid| {
                    let mut mapped = grid.clone();
                    for converter in &mut mapped.converters {
                        *converter.ac_bus_mut() = remap(converter.ac_bus());
                    }
                    mapped
                })
                .collect(),
        },
        // FACTS devices: remap bus_from and bus_to.
        facts_devices: network
            .facts_devices
            .iter()
            .map(|f| {
                let mut m = f.clone();
                m.bus_from = remap(f.bus_from);
                m.bus_to = remap(f.bus_to);
                m
            })
            .collect(),
        cim: {
            let mut cim_data = network.cim.clone();
            // Grounding impedances: remap bus numbers.
            for gi in &mut cim_data.grounding_impedances {
                gi.bus = remap(gi.bus);
            }
            cim_data
        },
        // Fields that need no bus remapping — copy verbatim.
        area_schedules: network.area_schedules.clone(),
        metadata: network.metadata.clone(),
        // Interfaces and flowgates reference branch indices which change after merge.
        // Emit a warning so callers know to re-validate indices against the merged network.
        interfaces: {
            if !network.interfaces.is_empty() {
                warn!(
                    n_interfaces = network.interfaces.len(),
                    "zero-impedance merge: interface branch indices may be stale — \
                     re-validate against the merged network before running OPF"
                );
            }
            network.interfaces.clone()
        },
        flowgates: {
            if !network.flowgates.is_empty() {
                warn!(
                    n_flowgates = network.flowgates.len(),
                    "zero-impedance merge: flowgate branch indices may be stale — \
                     re-validate against the merged network before running OPF"
                );
            }
            network.flowgates.clone()
        },
        nomograms: network.nomograms.clone(),
        topology: None, // node-breaker model is invalidated by bus merging
        induction_machines: network.induction_machines.clone(),
        conditional_limits: network.conditional_limits.clone(),
        breaker_ratings: network.breaker_ratings.clone(),
        fixed_shunts: network.fixed_shunts.clone(),
        power_injections: Vec::new(),
        market_data: surge_network::network::NetworkMarketData {
            // Dispatchable loads use external bus numbers; remap them onto the
            // merged representatives so market resources stay attached to the
            // correct bus after contraction.
            dispatchable_loads: network
                .market_data
                .dispatchable_loads
                .iter()
                .map(|dl| {
                    let mut m = dl.clone();
                    m.bus = remap(dl.bus);
                    m
                })
                .collect(),
            pumped_hydro_units: network.market_data.pumped_hydro_units.clone(),
            combined_cycle_plants: network.market_data.combined_cycle_plants.clone(),
            outage_schedule: network.market_data.outage_schedule.clone(),
            reserve_zones: network.market_data.reserve_zones.clone(),
            ambient: network.market_data.ambient.clone(),
            emission_policy: network.market_data.emission_policy.clone(),
            market_rules: network.market_data.market_rules.clone(),
        },
    };

    // Retain non-zero-impedance branches (with remapped terminals).
    for branch in &network.branches {
        if !branch.in_service {
            continue;
        }
        let z_mag = branch.r.abs() + branch.x.abs();
        let is_zero_z = z_mag < tol
            && (branch.tap - 1.0).abs() <= tap_tol
            && branch.phase_shift_rad.abs() <= tap_tol;
        if is_zero_z {
            continue; // absorbed by bus merge
        }
        let Some(&fi) = bus_ext_to_idx.get(&branch.from_bus) else {
            continue;
        };
        let Some(&ti) = bus_ext_to_idx.get(&branch.to_bus) else {
            continue;
        };
        let mfi = bus_map[fi];
        let mti = bus_map[ti];
        if mfi == mti {
            // Both endpoints merged into same bus — skip (self-loop).
            continue;
        }
        let mut b = branch.clone();
        b.from_bus = merged_net.buses[mfi].number;
        b.to_bus = merged_net.buses[mti].number;
        merged_net.branches.push(b);
    }

    // Retain generators (with remapped buses).
    for g in &network.generators {
        let Some(&gi) = bus_ext_to_idx.get(&g.bus) else {
            continue;
        };
        let mgi = bus_map[gi];
        let mut mg = g.clone();
        mg.bus = merged_net.buses[mgi].number;
        if let Some(reg_bus) = mg.reg_bus {
            mg.reg_bus = Some(remap(reg_bus));
        }
        merged_net.generators.push(mg);
    }

    // Retain loads (with remapped buses).
    for ld in &network.loads {
        let Some(&li) = bus_ext_to_idx.get(&ld.bus) else {
            continue;
        };
        let mli = bus_map[li];
        let mut ml = ld.clone();
        ml.bus = merged_net.buses[mli].number;
        merged_net.loads.push(ml);
    }

    for injection in &network.power_injections {
        let Some(&ii) = bus_ext_to_idx.get(&injection.bus) else {
            continue;
        };
        let mii = bus_map[ii];
        let mut mapped = injection.clone();
        mapped.bus = merged_net.buses[mii].number;
        merged_net.power_injections.push(mapped);
    }

    MergedNetwork {
        network: merged_net,
        bus_map,
        n_original_buses: n,
    }
}

/// Expand a power flow solution on the merged network back to the original bus ordering.
///
/// Each original bus gets the voltage (`vm`, `va`) of its representative merged bus.
/// All scalar solver metadata is copied verbatim. Bus/branch state is expanded
/// onto the provided original network.
pub fn expand_pf_solution(
    merged_sol: &PfSolution,
    merged: &MergedNetwork,
    original_network: &Network,
) -> PfSolution {
    let n = merged.n_original_buses;
    let mut vm = vec![1.0f64; n];
    let mut va = vec![0.0f64; n];

    for (i, &mi) in merged.bus_map.iter().enumerate() {
        if mi < merged_sol.voltage_magnitude_pu.len() {
            vm[i] = merged_sol.voltage_magnitude_pu[mi];
        }
        if mi < merged_sol.voltage_angle_rad.len() {
            va[i] = merged_sol.voltage_angle_rad[mi];
        }
    }

    let mut island_ids = Vec::new();
    if !merged_sol.island_ids.is_empty() {
        island_ids = merged
            .bus_map
            .iter()
            .map(|&merged_idx| merged_sol.island_ids[merged_idx])
            .collect();
    }

    let ybus = build_ybus(original_network);
    let (active_power_injection_pu, reactive_power_injection_pu) =
        compute_power_injection(&ybus, &vm, &va);
    let (branch_pf, branch_pt, branch_qf, branch_qt) =
        compute_branch_power_flows(original_network, &vm, &va, original_network.base_mva);

    PfSolution {
        pf_model: merged_sol.pf_model,
        status: merged_sol.status,
        iterations: merged_sol.iterations,
        max_mismatch: merged_sol.max_mismatch,
        solve_time_secs: merged_sol.solve_time_secs,
        voltage_magnitude_pu: vm,
        voltage_angle_rad: va,
        active_power_injection_pu,
        reactive_power_injection_pu,
        branch_p_from_mw: branch_pf,
        branch_p_to_mw: branch_pt,
        branch_q_from_mvar: branch_qf,
        branch_q_to_mvar: branch_qt,
        bus_numbers: original_network
            .buses
            .iter()
            .map(|bus| bus.number)
            .collect(),
        island_ids,
        q_limited_buses: merged_sol.q_limited_buses.clone(),
        n_q_limit_switches: merged_sol.n_q_limit_switches,
        gen_slack_contribution_mw: merged_sol.gen_slack_contribution_mw.clone(),
        convergence_history: merged_sol.convergence_history.clone(),
        worst_mismatch_bus: merged_sol.worst_mismatch_bus,
        area_interchange: merged_sol.area_interchange.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::mismatch::compute_power_injection;
    use crate::matrix::ybus::build_ybus;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, PowerInjection};
    use surge_solution::SolveStatus;

    fn simple_bus(number: u32, bus_type: BusType) -> Bus {
        Bus::new(number, bus_type, 100.0)
    }

    fn line_branch(from: u32, to: u32, r: f64, x: f64) -> Branch {
        Branch::new_line(from, to, r, x, 0.0)
    }

    #[test]
    fn no_zero_impedance_identity() {
        let mut net = Network::new("test");
        net.buses.push(simple_bus(1, BusType::Slack));
        net.buses.push(simple_bus(2, BusType::PQ));
        net.branches.push(line_branch(1, 2, 0.01, 0.05));

        let merged = merge_zero_impedance(&net, 1e-6);
        assert_eq!(merged.n_original_buses, 2);
        assert_eq!(merged.network.buses.len(), 2);
        assert_eq!(merged.bus_map, vec![0, 1]);
    }

    #[test]
    fn two_buses_merged_by_zero_impedance_branch() {
        let mut net = Network::new("test");
        net.buses.push(simple_bus(1, BusType::Slack));
        net.buses.push(simple_bus(2, BusType::PQ));
        net.buses.push(simple_bus(3, BusType::PQ));
        // Zero-impedance tie between buses 1 and 2
        net.branches.push(line_branch(1, 2, 0.0, 0.0));
        // Normal branch between merged (1≡2) and 3
        net.branches.push(line_branch(2, 3, 0.01, 0.05));

        let merged = merge_zero_impedance(&net, 1e-6);
        assert_eq!(merged.n_original_buses, 3);
        // Buses 1 and 2 merge → 2 merged buses
        assert_eq!(merged.network.buses.len(), 2);
        // Original bus 0 and 1 map to same merged bus
        assert_eq!(merged.bus_map[0], merged.bus_map[1]);
        // Original bus 2 maps to a different merged bus
        assert_ne!(merged.bus_map[0], merged.bus_map[2]);
        // Only one branch in merged network (the zero-Z tie is removed)
        assert_eq!(merged.network.branches.len(), 1);
    }

    #[test]
    fn expand_solution_maps_voltages() {
        let mut net = Network::new("test");
        net.buses.push(simple_bus(1, BusType::Slack));
        net.buses.push(simple_bus(2, BusType::PQ));
        net.buses.push(simple_bus(3, BusType::PQ));
        net.branches.push(line_branch(1, 2, 0.0, 0.0));
        net.branches.push(line_branch(2, 3, 0.01, 0.05));

        let merged = merge_zero_impedance(&net, 1e-6);
        // Fake a solution on the merged 2-bus network
        let merged_sol = PfSolution {
            pf_model: surge_solution::PfModel::Ac,
            status: SolveStatus::Converged,
            iterations: 3,
            max_mismatch: 1e-10,
            solve_time_secs: 0.001,
            voltage_magnitude_pu: vec![1.02, 0.98],
            voltage_angle_rad: vec![0.0, -0.05],
            active_power_injection_pu: Vec::new(),
            reactive_power_injection_pu: Vec::new(),
            branch_p_from_mw: vec![0.25],
            branch_p_to_mw: vec![-0.24],
            branch_q_from_mvar: vec![0.02],
            branch_q_to_mvar: vec![-0.01],
            bus_numbers: Vec::new(),
            island_ids: vec![4, 7],
            q_limited_buses: vec![3],
            n_q_limit_switches: 2,
            gen_slack_contribution_mw: vec![12.5],
            convergence_history: vec![(1, 1e-2), (2, 1e-6)],
            worst_mismatch_bus: None,
            area_interchange: None,
        };

        let full = expand_pf_solution(&merged_sol, &merged, &net);
        assert_eq!(full.voltage_magnitude_pu.len(), 3);
        // Original buses 0 and 1 were merged — same voltage
        assert_eq!(full.voltage_magnitude_pu[0], full.voltage_magnitude_pu[1]);
        // Bus 2 has the other voltage
        assert_eq!(full.voltage_magnitude_pu[2], 0.98);
        assert_eq!(full.island_ids, vec![4, 4, 7]);
        assert_eq!(full.q_limited_buses, vec![3]);
        assert_eq!(full.n_q_limit_switches, 2);
        assert_eq!(full.gen_slack_contribution_mw, vec![12.5]);
        assert_eq!(full.convergence_history, vec![(1, 1e-2), (2, 1e-6)]);

        let ybus = build_ybus(&net);
        let (expected_p, expected_q) =
            compute_power_injection(&ybus, &full.voltage_magnitude_pu, &full.voltage_angle_rad);
        assert_eq!(full.active_power_injection_pu, expected_p);
        assert_eq!(full.reactive_power_injection_pu, expected_q);
    }

    #[test]
    fn merge_zero_impedance_preserves_power_injections_and_remote_reg_bus() {
        let mut net = Network::new("test");
        net.buses.push(simple_bus(1, BusType::Slack));
        net.buses.push(simple_bus(2, BusType::PQ));
        net.buses.push(simple_bus(3, BusType::PQ));
        net.branches.push(line_branch(1, 2, 0.0, 0.0));
        net.branches.push(line_branch(2, 3, 0.01, 0.05));

        let mut generator = Generator::new(1, 50.0, 1.0);
        generator.reg_bus = Some(2);
        net.generators.push(generator);

        net.power_injections.push(PowerInjection::new(1, 5.0, 1.0));
        net.power_injections.push(PowerInjection::new(2, 7.5, -2.0));

        let merged = merge_zero_impedance(&net, 1e-6);

        assert_eq!(
            merged.network.power_injections.len(),
            2,
            "power injections should be remapped once, not cloned and re-appended"
        );
        assert!(
            merged
                .network
                .power_injections
                .iter()
                .all(|inj| inj.bus == 1)
        );
        assert_eq!(merged.network.generators.len(), 1);
        assert_eq!(
            merged.network.generators[0].reg_bus,
            Some(1),
            "remote regulated bus should be remapped through the merged bus representative"
        );
    }
}
