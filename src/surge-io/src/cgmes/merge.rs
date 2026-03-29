// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES model merging engine — stitch multiple TSO CGMES networks at boundary points.
//!
//! When ENTSO-E TSOs exchange CGMES models for Common Grid Model (CGM) assembly,
//! each TSO provides its own network plus boundary equipment.  This module merges
//! N such networks into a single unified `Network` by:
//!
//! 1. Renumbering buses to avoid collisions across input networks.
//! 2. Identifying shared boundary points via matching `connectivity_node_mrid`.
//! 3. Stitching paired boundary buses (collapsing two buses into one).
//! 4. Concatenating all equipment arrays (branches, generators, loads, ...).
//! 5. Removing duplicate external equivalents at stitched boundaries.
//! 6. Rebuilding metadata and producing a `MergeReport` with diagnostics.

use std::collections::{HashMap, HashSet};

use surge_network::Network;

/// Errors that can occur during network merging.
#[derive(Debug)]
pub enum MergeError {
    /// No networks provided.
    Empty,
    /// A boundary connectivity node mRID appears in more than 2 networks.
    MultipleBoundaryMatch { cn_mrid: String, count: usize },
    /// Bus renumbering would overflow `u32`.
    BusNumberOverflow,
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "no networks provided for merging"),
            Self::MultipleBoundaryMatch { cn_mrid, count } => {
                write!(
                    f,
                    "boundary CN mRID {cn_mrid} appears in {count} networks (max 2 supported)"
                )
            }
            Self::BusNumberOverflow => write!(f, "bus renumbering would overflow u32"),
        }
    }
}

impl std::error::Error for MergeError {}

/// Diagnostics produced by [`merge_networks`].
#[derive(Debug, Clone, Default)]
pub struct MergeReport {
    /// Number of input networks.
    pub input_count: usize,
    /// Number of boundary points matched (stitched).
    pub boundary_points_stitched: usize,
    /// Number of boundary points unmatched (kept as-is).
    pub boundary_points_unmatched: usize,
    /// Bus number offset applied to each input network.
    pub bus_offsets: Vec<u32>,
    /// Total buses in merged network.
    pub total_buses: usize,
    /// Total branches in merged network.
    pub total_branches: usize,
    /// Duplicate equivalent branches removed.
    pub equivalent_branches_removed: usize,
    /// Warnings (non-fatal issues).
    pub warnings: Vec<String>,
}

/// Merge multiple Network models at shared boundary points.
///
/// Takes N networks (each from a different TSO's CGMES export) and produces
/// a single unified Network by:
/// 1. Identifying shared boundary points via matching `connectivity_node_mrid`
/// 2. Renumbering buses to avoid collisions
/// 3. Merging equipment (branches, generators, loads, shunts)
/// 4. Stitching boundary buses (collapsing shared boundary nodes into one bus)
/// 5. Merging area schedules, regions, and operational metadata
/// 6. Removing duplicate equivalent branches/shunts at stitched boundaries
///
/// Returns the merged Network and a [`MergeReport`] with diagnostics.
pub fn merge_networks(networks: Vec<Network>) -> Result<(Network, MergeReport), MergeError> {
    if networks.is_empty() {
        return Err(MergeError::Empty);
    }

    let mut report = MergeReport {
        input_count: networks.len(),
        ..Default::default()
    };

    // Single network: pass-through.
    if networks.len() == 1 {
        let net = networks.into_iter().next().expect("networks.len() == 1");
        report.bus_offsets = vec![0];
        report.total_buses = net.buses.len();
        report.total_branches = net.branches.len();
        return Ok((net, report));
    }

    // --- Step 1: Compute offsets and renumber ---
    let mut offsets: Vec<u32> = Vec::with_capacity(networks.len());
    let mut current_offset: u32 = 0;
    for net in &networks {
        offsets.push(current_offset);
        let max_bus = net.buses.iter().map(|b| b.number).max().unwrap_or(0);
        current_offset = current_offset
            .checked_add(max_bus)
            .ok_or(MergeError::BusNumberOverflow)?;
    }
    report.bus_offsets = offsets.clone();

    let mut renamed_networks: Vec<Network> = Vec::with_capacity(networks.len());
    let mut bus_maps: Vec<HashMap<u32, u32>> = Vec::with_capacity(networks.len());
    for (i, mut net) in networks.into_iter().enumerate() {
        let map = renumber_network(&mut net, offsets[i]);
        bus_maps.push(map);
        renamed_networks.push(net);
    }

    // --- Step 2: Identify boundary pairs ---
    let boundary_map = find_boundary_pairs(&renamed_networks);

    // Validate: no CN mRID in >2 networks.
    for (cn_mrid, entries) in &boundary_map {
        if entries.len() > 2 {
            return Err(MergeError::MultipleBoundaryMatch {
                cn_mrid: cn_mrid.clone(),
                count: entries.len(),
            });
        }
    }

    // Partition into matched (exactly 2) and unmatched (exactly 1).
    let mut stitch_pairs: Vec<(u32, u32)> = Vec::new();
    for entries in boundary_map.values() {
        if entries.len() == 2 {
            // Keep the bus from the first network (lower index), remap the second.
            let (_, bus_a) = entries[0];
            let (_, bus_b) = entries[1];
            stitch_pairs.push((bus_a, bus_b));
            report.boundary_points_stitched += 1;
        } else {
            report.boundary_points_unmatched += 1;
        }
    }

    // --- Step 3+4: Build merged network by concatenating all equipment ---
    let mut merged = Network::default();

    // Name.
    let names: Vec<&str> = renamed_networks.iter().map(|n| n.name.as_str()).collect();
    merged.name = format!("Merged: {}", names.join(" + "));

    // base_mva: check consistency, warn if different.
    let base_mva = renamed_networks[0].base_mva;
    for (i, net) in renamed_networks.iter().enumerate().skip(1) {
        if (net.base_mva - base_mva).abs() > 1e-6 {
            report.warnings.push(format!(
                "base_mva mismatch: network[0]={base_mva}, network[{i}]={}",
                net.base_mva
            ));
        }
    }
    merged.base_mva = base_mva;
    merged.freq_hz = renamed_networks[0].freq_hz;

    // Concatenate all vector-based equipment.
    for net in renamed_networks.into_iter() {
        merged.buses.extend(net.buses);
        merged.branches.extend(net.branches);
        merged.generators.extend(net.generators);
        merged.loads.extend(net.loads);
        merged.power_injections.extend(net.power_injections);
        merged
            .market_data
            .dispatchable_loads
            .extend(net.market_data.dispatchable_loads);
        merged
            .controls
            .switched_shunts
            .extend(net.controls.switched_shunts);
        merged
            .controls
            .switched_shunts_opf
            .extend(net.controls.switched_shunts_opf);
        merged.controls.oltc_specs.extend(net.controls.oltc_specs);
        merged.controls.par_specs.extend(net.controls.par_specs);
        merged.hvdc.links.extend(net.hvdc.links);
        merged.hvdc.dc_grids.extend(net.hvdc.dc_grids);
        merged.facts_devices.extend(net.facts_devices);
        merged.fixed_shunts.extend(net.fixed_shunts);
        merged.induction_machines.extend(net.induction_machines);
        merged
            .metadata
            .multi_section_line_groups
            .extend(net.metadata.multi_section_line_groups);
        merged.breaker_ratings.extend(net.breaker_ratings);
        merged.cim.measurements.extend(net.cim.measurements);
        merged
            .cim
            .grounding_impedances
            .extend(net.cim.grounding_impedances);
        merged.flowgates.extend(net.flowgates);
        merged.interfaces.extend(net.interfaces);
        merged.nomograms.extend(net.nomograms);
        merged
            .market_data
            .pumped_hydro_units
            .extend(net.market_data.pumped_hydro_units);
        merged
            .market_data
            .combined_cycle_plants
            .extend(net.market_data.combined_cycle_plants);
        merged
            .market_data
            .outage_schedule
            .extend(net.market_data.outage_schedule);
        merged
            .market_data
            .reserve_zones
            .extend(net.market_data.reserve_zones);
        merged
            .metadata
            .impedance_corrections
            .extend(net.metadata.impedance_corrections);

        // Dedup area_schedules/regions/owners by name.
        for area in net.area_schedules {
            if !merged.area_schedules.iter().any(|a| a.name == area.name) {
                merged.area_schedules.push(area);
            }
        }
        for region in net.metadata.regions {
            if !merged
                .metadata
                .regions
                .iter()
                .any(|r| r.name == region.name)
            {
                merged.metadata.regions.push(region);
            }
        }
        for owner in net.metadata.owners {
            if !merged.metadata.owners.iter().any(|o| o.name == owner.name) {
                merged.metadata.owners.push(owner);
            }
        }
        merged
            .metadata
            .scheduled_area_transfers
            .extend(net.metadata.scheduled_area_transfers);

        // Merge boundary data.
        merged
            .cim
            .boundary_data
            .boundary_points
            .extend(net.cim.boundary_data.boundary_points);
        merged
            .cim
            .boundary_data
            .model_authority_sets
            .extend(net.cim.boundary_data.model_authority_sets);
        merged
            .cim
            .boundary_data
            .equivalent_networks
            .extend(net.cim.boundary_data.equivalent_networks);
        merged
            .cim
            .boundary_data
            .equivalent_branches
            .extend(net.cim.boundary_data.equivalent_branches);
        merged
            .cim
            .boundary_data
            .equivalent_shunts
            .extend(net.cim.boundary_data.equivalent_shunts);

        // Merge hash-map based data.
        merged
            .cim
            .per_length_phase_impedances
            .extend(net.cim.per_length_phase_impedances);
        merged.cim.mutual_couplings.extend(net.cim.mutual_couplings);
        merged.cim.geo_locations.extend(net.cim.geo_locations);
        merged.conditional_limits.extend(net.conditional_limits);
    }

    // --- Stitch boundary buses ---
    let mut stitched_bus_set: HashSet<u32> = HashSet::new();
    for &(bus_a, bus_b) in &stitch_pairs {
        stitch_buses(&mut merged, bus_a, bus_b);
        stitched_bus_set.insert(bus_a);
        stitched_bus_set.insert(bus_b);
    }

    // --- Step 5: Remove duplicate equivalents at stitched boundaries ---
    let eq_removed = remove_duplicate_equivalents(&mut merged, &stitched_bus_set);
    report.equivalent_branches_removed = eq_removed;

    // --- Step 6: Finalize report ---
    report.total_buses = merged.buses.len();
    report.total_branches = merged.branches.len();

    Ok((merged, report))
}

/// Apply a bus-number offset to every bus reference in the network.
///
/// Returns a mapping from old bus number to new bus number.
fn renumber_network(network: &mut Network, offset: u32) -> HashMap<u32, u32> {
    if offset == 0 {
        return network.buses.iter().map(|b| (b.number, b.number)).collect();
    }

    let mut map: HashMap<u32, u32> = HashMap::new();
    let dc_grid_offset = offset;

    // Helper: look up or create new bus number.
    let remap =
        |bus: u32, m: &mut HashMap<u32, u32>| -> u32 { *m.entry(bus).or_insert(bus + offset) };

    // Buses.
    for bus in &mut network.buses {
        let new_num = bus.number + offset;
        map.insert(bus.number, new_num);
        bus.number = new_num;
    }

    // Branches.
    for br in &mut network.branches {
        br.from_bus = remap(br.from_bus, &mut map);
        br.to_bus = remap(br.to_bus, &mut map);
    }

    // Generators.
    for g in &mut network.generators {
        g.bus = remap(g.bus, &mut map);
    }

    // Loads.
    for load in &mut network.loads {
        load.bus = remap(load.bus, &mut map);
    }

    for injection in &mut network.power_injections {
        injection.bus = remap(injection.bus, &mut map);
    }

    // Dispatchable loads (bus_idx is 0-based internal index, not external bus number;
    // skip renumbering — these are rebuilt at solve time).

    // Fixed shunts.
    for fs in &mut network.fixed_shunts {
        fs.bus = remap(fs.bus, &mut map);
    }

    // FACTS devices.
    for fd in &mut network.facts_devices {
        fd.bus_from = remap(fd.bus_from, &mut map);
        if fd.bus_to != 0 {
            fd.bus_to = remap(fd.bus_to, &mut map);
        }
    }

    for link in &mut network.hvdc.links {
        match link {
            surge_network::network::HvdcLink::Lcc(dcl) => {
                dcl.rectifier.bus = remap(dcl.rectifier.bus, &mut map);
                dcl.inverter.bus = remap(dcl.inverter.bus, &mut map);
            }
            surge_network::network::HvdcLink::Vsc(vsc) => {
                vsc.converter1.bus = remap(vsc.converter1.bus, &mut map);
                vsc.converter2.bus = remap(vsc.converter2.bus, &mut map);
            }
        }
    }

    for dc_grid in &mut network.hvdc.dc_grids {
        dc_grid.id += dc_grid_offset;
        for conv in &mut dc_grid.converters {
            *conv.ac_bus_mut() = remap(conv.ac_bus(), &mut map);
            *conv.dc_bus_mut() = remap(conv.dc_bus(), &mut map);
        }
        for dcb in &mut dc_grid.buses {
            dcb.bus_id = remap(dcb.bus_id, &mut map);
        }
        for dcbr in &mut dc_grid.branches {
            dcbr.from_bus = remap(dcbr.from_bus, &mut map);
            dcbr.to_bus = remap(dcbr.to_bus, &mut map);
        }
    }

    // Area schedules.
    for area in &mut network.area_schedules {
        area.slack_bus = remap(area.slack_bus, &mut map);
    }

    // OLTC specs.
    for oltc in &mut network.controls.oltc_specs {
        oltc.from_bus = remap(oltc.from_bus, &mut map);
        oltc.to_bus = remap(oltc.to_bus, &mut map);
    }

    // PAR specs.
    for par in &mut network.controls.par_specs {
        par.from_bus = remap(par.from_bus, &mut map);
        par.to_bus = remap(par.to_bus, &mut map);
    }

    // Multi-section line groups.
    for msg in &mut network.metadata.multi_section_line_groups {
        msg.from_bus = remap(msg.from_bus, &mut map);
        msg.to_bus = remap(msg.to_bus, &mut map);
        for db in &mut msg.dummy_buses {
            *db = remap(*db, &mut map);
        }
    }

    // Induction machines.
    for im in &mut network.induction_machines {
        im.bus = remap(im.bus, &mut map);
    }

    // Measurements.
    for m in &mut network.cim.measurements {
        m.bus = remap(m.bus, &mut map);
    }

    // Breaker ratings.
    for br in &mut network.breaker_ratings {
        br.bus = remap(br.bus, &mut map);
    }

    // Grounding impedances.
    for gi in &mut network.cim.grounding_impedances {
        gi.bus = remap(gi.bus, &mut map);
    }

    // Boundary points.
    for bp in &mut network.cim.boundary_data.boundary_points {
        if let Some(ref mut b) = bp.bus {
            *b = remap(*b, &mut map);
        }
    }

    // Equivalent branches.
    for eb in &mut network.cim.boundary_data.equivalent_branches {
        if let Some(ref mut b) = eb.from_bus {
            *b = remap(*b, &mut map);
        }
        if let Some(ref mut b) = eb.to_bus {
            *b = remap(*b, &mut map);
        }
    }

    // Equivalent shunts.
    for es in &mut network.cim.boundary_data.equivalent_shunts {
        if let Some(ref mut b) = es.bus {
            *b = remap(*b, &mut map);
        }
    }

    // Operational limits (HashMap keyed by mRID).
    for ls in network.cim.operational_limits.limit_sets.values_mut() {
        ls.bus = remap(ls.bus, &mut map);
    }

    map
}

/// Build a map from boundary CN mRID to the list of (network_index, bus_number).
fn find_boundary_pairs(networks: &[Network]) -> HashMap<String, Vec<(usize, u32)>> {
    let mut cn_map: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
    for (net_idx, net) in networks.iter().enumerate() {
        for bp in &net.cim.boundary_data.boundary_points {
            if let (Some(cn_mrid), Some(bus)) = (&bp.connectivity_node_mrid, bp.bus) {
                cn_map
                    .entry(cn_mrid.clone())
                    .or_default()
                    .push((net_idx, bus));
            }
        }
    }
    cn_map
}

/// Stitch two boundary buses: keep `bus_a`, remap all references to `bus_b` to `bus_a`,
/// then remove `bus_b` from the bus list. Merges load (pd/qd) onto `bus_a`.
fn stitch_buses(merged: &mut Network, bus_a: u32, bus_b: u32) {
    // Merge bus metadata: take the tighter voltage limits, sum loads.
    let (vmax_b, vmin_b, bs_b, gs_b) = merged
        .buses
        .iter()
        .find(|b| b.number == bus_b)
        .map(|b| {
            (
                b.voltage_max_pu,
                b.voltage_min_pu,
                b.shunt_susceptance_mvar,
                b.shunt_conductance_mw,
            )
        })
        .unwrap_or((1.1, 0.9, 0.0, 0.0));

    if let Some(a) = merged.buses.iter_mut().find(|b| b.number == bus_a) {
        // Tighter voltage bounds.
        if vmax_b < a.voltage_max_pu {
            a.voltage_max_pu = vmax_b;
        }
        if vmin_b > a.voltage_min_pu {
            a.voltage_min_pu = vmin_b;
        }
        // Sum shunts.
        a.shunt_susceptance_mvar += bs_b;
        a.shunt_conductance_mw += gs_b;
    }

    // Remove bus_b.
    merged.buses.retain(|b| b.number != bus_b);

    // Remap all references from bus_b -> bus_a across all equipment.
    let remap = |bus: &mut u32| {
        if *bus == bus_b {
            *bus = bus_a;
        }
    };

    for br in &mut merged.branches {
        remap(&mut br.from_bus);
        remap(&mut br.to_bus);
    }
    for g in &mut merged.generators {
        remap(&mut g.bus);
    }
    for load in &mut merged.loads {
        remap(&mut load.bus);
    }
    for injection in &mut merged.power_injections {
        remap(&mut injection.bus);
    }
    for fs in &mut merged.fixed_shunts {
        remap(&mut fs.bus);
    }
    for fd in &mut merged.facts_devices {
        remap(&mut fd.bus_from);
        if fd.bus_to != 0 {
            remap(&mut fd.bus_to);
        }
    }
    for link in &mut merged.hvdc.links {
        match link {
            surge_network::network::HvdcLink::Lcc(dcl) => {
                remap(&mut dcl.rectifier.bus);
                remap(&mut dcl.inverter.bus);
            }
            surge_network::network::HvdcLink::Vsc(vsc) => {
                remap(&mut vsc.converter1.bus);
                remap(&mut vsc.converter2.bus);
            }
        }
    }
    for dc_grid in &mut merged.hvdc.dc_grids {
        for conv in &mut dc_grid.converters {
            remap(conv.ac_bus_mut());
            remap(conv.dc_bus_mut());
        }
        for dcb in &mut dc_grid.buses {
            remap(&mut dcb.bus_id);
        }
        for dcbr in &mut dc_grid.branches {
            remap(&mut dcbr.from_bus);
            remap(&mut dcbr.to_bus);
        }
    }
    for area in &mut merged.area_schedules {
        remap(&mut area.slack_bus);
    }
    for oltc in &mut merged.controls.oltc_specs {
        remap(&mut oltc.from_bus);
        remap(&mut oltc.to_bus);
    }
    for par in &mut merged.controls.par_specs {
        remap(&mut par.from_bus);
        remap(&mut par.to_bus);
    }
    for msg in &mut merged.metadata.multi_section_line_groups {
        remap(&mut msg.from_bus);
        remap(&mut msg.to_bus);
        for db in &mut msg.dummy_buses {
            remap(db);
        }
    }
    for im in &mut merged.induction_machines {
        remap(&mut im.bus);
    }
    for m in &mut merged.cim.measurements {
        remap(&mut m.bus);
    }
    for br in &mut merged.breaker_ratings {
        remap(&mut br.bus);
    }
    for gi in &mut merged.cim.grounding_impedances {
        if gi.bus == bus_b {
            gi.bus = bus_a;
        }
    }
    for bp in &mut merged.cim.boundary_data.boundary_points {
        if let Some(ref mut b) = bp.bus {
            remap(b);
        }
    }
    for eb in &mut merged.cim.boundary_data.equivalent_branches {
        if let Some(ref mut b) = eb.from_bus {
            remap(b);
        }
        if let Some(ref mut b) = eb.to_bus {
            remap(b);
        }
    }
    for es in &mut merged.cim.boundary_data.equivalent_shunts {
        if let Some(ref mut b) = es.bus {
            remap(b);
        }
    }
    for ls in merged.cim.operational_limits.limit_sets.values_mut() {
        remap(&mut ls.bus);
    }
}

/// Remove duplicate equivalent branches/shunts that both touch stitched boundary buses.
///
/// At a stitched boundary, both TSOs may have exported an equivalent branch/shunt
/// for the same boundary. After stitching, these are redundant (the real network
/// on both sides is now internal). We remove any equivalent whose bus(es) are in
/// the stitched set.
fn remove_duplicate_equivalents(merged: &mut Network, stitched_buses: &HashSet<u32>) -> usize {
    if stitched_buses.is_empty() {
        return 0;
    }

    let before_br = merged.cim.boundary_data.equivalent_branches.len();
    let before_sh = merged.cim.boundary_data.equivalent_shunts.len();

    // Remove equivalent branches where BOTH endpoints are stitched boundary buses.
    merged.cim.boundary_data.equivalent_branches.retain(|eb| {
        let from_stitched = eb.from_bus.is_some_and(|b| stitched_buses.contains(&b));
        let to_stitched = eb.to_bus.is_some_and(|b| stitched_buses.contains(&b));
        !(from_stitched && to_stitched)
    });

    // Remove equivalent shunts on stitched boundary buses.
    merged
        .cim
        .boundary_data
        .equivalent_shunts
        .retain(|es| !es.bus.is_some_and(|b| stitched_buses.contains(&b)));

    let after_br = merged.cim.boundary_data.equivalent_branches.len();
    let after_sh = merged.cim.boundary_data.equivalent_shunts.len();

    (before_br - after_br) + (before_sh - after_sh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::boundary::{
        BoundaryPoint, EquivalentBranchData, EquivalentShuntData,
    };
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    /// Helper: create a simple 3-bus network with a boundary point on bus 3.
    #[allow(clippy::field_reassign_with_default)]
    fn make_tso_network(name: &str, bus_start: u32, boundary_cn_mrid: Option<&str>) -> Network {
        let mut net = Network::new(name);
        net.base_mva = 100.0;

        // 3 buses: bus_start, bus_start+1, bus_start+2.
        for i in 0..3 {
            let bus_num = bus_start + i;
            let mut bus = Bus::default();
            bus.number = bus_num;
            bus.base_kv = 220.0;
            bus.voltage_max_pu = 1.1;
            bus.voltage_min_pu = 0.9;
            bus.bus_type = if i == 0 { BusType::Slack } else { BusType::PQ };
            net.buses.push(bus);
            if i == 2 {
                net.loads.push(Load::new(bus_num, 50.0, 0.0)); // load on boundary bus
            }
        }

        // 2 branches: bus_start->(bus_start+1), (bus_start+1)->(bus_start+2).
        for i in 0..2 {
            let mut br = Branch::default();
            br.from_bus = bus_start + i;
            br.to_bus = bus_start + i + 1;
            br.r = 0.01;
            br.x = 0.1;
            br.in_service = true;
            net.branches.push(br);
        }

        // 1 generator on first bus.
        let mut g = Generator::default();
        g.bus = bus_start;
        g.p = 100.0;
        g.pmax = 200.0;
        g.in_service = true;
        net.generators.push(g);

        // 1 load on second bus.
        let mut load = Load::default();
        load.bus = bus_start + 1;
        load.active_power_demand_mw = 80.0;
        load.reactive_power_demand_mvar = 20.0;
        load.in_service = true;
        net.loads.push(load);

        // Boundary point on the third bus.
        if let Some(cn) = boundary_cn_mrid {
            net.cim.boundary_data.boundary_points.push(BoundaryPoint {
                mrid: format!("BP_{name}"),
                connectivity_node_mrid: Some(cn.to_string()),
                from_end_iso_code: None,
                to_end_iso_code: None,
                from_end_name: None,
                to_end_name: None,
                from_end_name_tso: None,
                to_end_name_tso: None,
                is_direct_current: false,
                is_excluded_from_area_interchange: false,
                bus: Some(bus_start + 2),
            });
        }

        net
    }

    #[test]
    fn test_merge_empty() {
        let result = merge_networks(vec![]);
        assert!(matches!(result, Err(MergeError::Empty)));
    }

    #[test]
    fn test_merge_single_passthrough() {
        let net = make_tso_network("TSO_A", 1, Some("CN_SHARED"));
        let (merged, report) = merge_networks(vec![net]).unwrap();
        assert_eq!(report.input_count, 1);
        assert_eq!(merged.buses.len(), 3);
        assert_eq!(merged.branches.len(), 2);
        assert_eq!(merged.generators.len(), 1);
    }

    #[test]
    fn test_merge_two_networks_shared_boundary() {
        let net_a = make_tso_network("TSO_A", 1, Some("CN_SHARED_1"));
        let net_b = make_tso_network("TSO_B", 1, Some("CN_SHARED_1"));

        let (merged, report) = merge_networks(vec![net_a, net_b]).unwrap();

        // 3 + 3 = 6 buses, minus 1 stitched = 5 buses.
        assert_eq!(merged.buses.len(), 5);
        assert_eq!(report.boundary_points_stitched, 1);
        assert_eq!(report.boundary_points_unmatched, 0);
        assert_eq!(report.total_buses, 5);

        // Branches: 2 + 2 = 4, all present (none removed by stitch).
        assert_eq!(merged.branches.len(), 4);

        // Generators: 1 + 1 = 2.
        assert_eq!(merged.generators.len(), 2);

        // Loads: 2 + 2 = 4 (each TSO has a load on bus_start+1 and bus_start+2).
        assert_eq!(merged.loads.len(), 4);

        // The stitched bus should have combined load from both boundary buses.
        // Bus 3 from net_a (pd=50) + bus 3 from net_b (pd=50 after renumber).
        let stitched_bus_num = 3_u32; // net_a's bus 3 stays
        let stitched = merged.buses.iter().find(|b| b.number == stitched_bus_num);
        assert!(stitched.is_some());
        let stitched_pd: f64 = merged
            .loads
            .iter()
            .filter(|l| l.bus == stitched_bus_num)
            .map(|l| l.active_power_demand_mw)
            .sum();
        assert!((stitched_pd - 100.0).abs() < 1e-6); // 50 + 50

        // All bus numbers should be unique.
        let bus_nums: HashSet<u32> = merged.buses.iter().map(|b| b.number).collect();
        assert_eq!(bus_nums.len(), 5);
    }

    #[test]
    fn test_merge_no_shared_boundaries() {
        let net_a = make_tso_network("TSO_A", 1, Some("CN_A_ONLY"));
        let net_b = make_tso_network("TSO_B", 1, Some("CN_B_ONLY"));

        let (merged, report) = merge_networks(vec![net_a, net_b]).unwrap();

        // No shared CN mRIDs, so simple concatenation: 3+3 = 6 buses.
        assert_eq!(merged.buses.len(), 6);
        assert_eq!(report.boundary_points_stitched, 0);
        assert_eq!(report.boundary_points_unmatched, 2);
        assert_eq!(merged.branches.len(), 4);
    }

    #[test]
    fn test_bus_renumbering_no_collision() {
        // Both networks use buses 1,2,3. After renumber, net_b should use 4,5,6.
        let net_a = make_tso_network("TSO_A", 1, None);
        let net_b = make_tso_network("TSO_B", 1, None);

        let (merged, report) = merge_networks(vec![net_a, net_b]).unwrap();

        // 6 buses total, no stitching (no boundary points).
        assert_eq!(merged.buses.len(), 6);

        let bus_nums: HashSet<u32> = merged.buses.iter().map(|b| b.number).collect();
        assert_eq!(bus_nums.len(), 6); // all unique

        // Offsets: net[0] starts at 0 (buses 1,2,3), net[1] starts at 3 (buses 4,5,6).
        assert_eq!(report.bus_offsets[0], 0);
        assert_eq!(report.bus_offsets[1], 3);
    }

    #[test]
    fn test_branches_remapped_at_boundary() {
        let net_a = make_tso_network("TSO_A", 1, Some("CN_SHARED_1"));
        let net_b = make_tso_network("TSO_B", 1, Some("CN_SHARED_1"));

        let (merged, _) = merge_networks(vec![net_a, net_b]).unwrap();

        // All branch endpoints should reference valid buses.
        let bus_nums: HashSet<u32> = merged.buses.iter().map(|b| b.number).collect();
        for br in &merged.branches {
            assert!(
                bus_nums.contains(&br.from_bus),
                "branch from_bus {} not in bus set",
                br.from_bus
            );
            assert!(
                bus_nums.contains(&br.to_bus),
                "branch to_bus {} not in bus set",
                br.to_bus
            );
        }

        // Generator and load buses should also be valid.
        for g in &merged.generators {
            assert!(
                bus_nums.contains(&g.bus),
                "generator bus {} not in bus set",
                g.bus
            );
        }
        for load in &merged.loads {
            assert!(
                bus_nums.contains(&load.bus),
                "load bus {} not in bus set",
                load.bus
            );
        }
    }

    #[test]
    fn test_duplicate_equivalents_removed() {
        let mut net_a = make_tso_network("TSO_A", 1, Some("CN_SHARED_1"));
        let mut net_b = make_tso_network("TSO_B", 1, Some("CN_SHARED_1"));

        // Add equivalent branch/shunt at boundary bus in both networks.
        net_a
            .cim
            .boundary_data
            .equivalent_branches
            .push(EquivalentBranchData {
                mrid: "EB_A".to_string(),
                network_mrid: None,
                r_ohm: 1.0,
                x_ohm: 10.0,
                r0_ohm: None,
                x0_ohm: None,
                r2_ohm: None,
                x2_ohm: None,
                from_bus: Some(3), // boundary bus
                to_bus: Some(3),   // self-loop equiv
            });
        net_a
            .cim
            .boundary_data
            .equivalent_shunts
            .push(EquivalentShuntData {
                mrid: "ES_A".to_string(),
                network_mrid: None,
                g_s: 0.001,
                b_s: 0.01,
                bus: Some(3), // boundary bus
            });

        net_b
            .cim
            .boundary_data
            .equivalent_branches
            .push(EquivalentBranchData {
                mrid: "EB_B".to_string(),
                network_mrid: None,
                r_ohm: 1.5,
                x_ohm: 15.0,
                r0_ohm: None,
                x0_ohm: None,
                r2_ohm: None,
                x2_ohm: None,
                from_bus: Some(3), // boundary bus (will be renumbered)
                to_bus: Some(3),
            });
        net_b
            .cim
            .boundary_data
            .equivalent_shunts
            .push(EquivalentShuntData {
                mrid: "ES_B".to_string(),
                network_mrid: None,
                g_s: 0.002,
                b_s: 0.02,
                bus: Some(3), // boundary bus (will be renumbered)
            });

        let (merged, report) = merge_networks(vec![net_a, net_b]).unwrap();

        // Both equivalent branches and shunts should be removed (they're on stitched buses).
        assert_eq!(report.equivalent_branches_removed, 4); // 2 branches + 2 shunts
        assert!(merged.cim.boundary_data.equivalent_branches.is_empty());
        assert!(merged.cim.boundary_data.equivalent_shunts.is_empty());
    }

    #[test]
    fn test_merge_report_counts() {
        let net_a = make_tso_network("TSO_A", 1, Some("CN_SHARED_1"));
        let net_b = make_tso_network("TSO_B", 1, Some("CN_SHARED_1"));

        let (_, report) = merge_networks(vec![net_a, net_b]).unwrap();

        assert_eq!(report.input_count, 2);
        assert_eq!(report.boundary_points_stitched, 1);
        assert_eq!(report.total_buses, 5);
        assert_eq!(report.total_branches, 4);
        assert_eq!(report.bus_offsets.len(), 2);
    }
}
