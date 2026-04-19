// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Topology derivation and retopology for node-breaker networks.

use std::collections::{BTreeSet, HashMap, HashSet};

use tracing::debug;

use surge_network::Network;
use surge_network::network::NodeBreakerTopology;
use surge_network::network::multi_section_line::MultiSectionLineGroup;
use surge_network::network::topology::TopologyMapping;
use surge_network::network::{Bus, BusType};

use crate::union_find::UnionFindIdx;

/// Errors that can occur during topology processing.
#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    #[error("network has no topology — cannot rebuild_topology a bus-branch-only network")]
    NoNodeBreakerTopology,
    #[error(
        "network has no topology mapping — cannot rebuild_topology without a prior topology projection"
    )]
    MissingTopologyMapping,
    #[error("duplicate connectivity node id '{0}' in node-breaker topology")]
    DuplicateConnectivityNode(String),
    #[error("duplicate voltage level id '{0}' in node-breaker topology")]
    DuplicateVoltageLevel(String),
    #[error("switch '{switch_id}' references unknown connectivity node '{connectivity_node_id}'")]
    UnknownSwitchConnectivityNode {
        switch_id: String,
        connectivity_node_id: String,
    },
    #[error(
        "connectivity node '{connectivity_node_id}' references unknown voltage level '{voltage_level_id}'"
    )]
    MissingVoltageLevel {
        connectivity_node_id: String,
        voltage_level_id: String,
    },
    #[error(
        "equipment '{equipment_id}' has duplicate terminal sequence {sequence_number} in node-breaker topology"
    )]
    DuplicateEquipmentTerminal {
        equipment_id: String,
        sequence_number: u32,
    },
    #[error("bus {bus_number} has no valid mapping in the current topology")]
    MissingBusMapping { bus_number: u32 },
    #[error(
        "bus {bus_number} split across multiple new buses; exact equipment identity is required to rebuild_topology safely"
    )]
    AmbiguousBusSplit { bus_number: u32 },
    #[error("{context} references invalid bus index {index}")]
    InvalidBusIndex { context: &'static str, index: usize },
}

/// A previous bus that split into multiple current buses after rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyBusSplit {
    pub previous_bus_number: u32,
    pub current_bus_numbers: Vec<u32>,
}

/// A current bus that now contains multiple previous buses after rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyBusMerge {
    pub current_bus_number: u32,
    pub previous_bus_numbers: Vec<u32>,
}

/// A branch that collapsed into a single bus during topology rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollapsedBranch {
    pub previous_from_bus: u32,
    pub previous_to_bus: u32,
    pub circuit: String,
}

impl CollapsedBranch {
    fn from_branch(branch: &surge_network::network::Branch) -> Self {
        Self {
            previous_from_bus: branch.from_bus,
            previous_to_bus: branch.to_bus,
            circuit: branch.circuit.clone(),
        }
    }
}

/// Summary of what changed during topology rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyReport {
    pub previous_bus_count: usize,
    pub current_bus_count: usize,
    pub bus_splits: Vec<TopologyBusSplit>,
    pub bus_merges: Vec<TopologyBusMerge>,
    pub collapsed_branches: Vec<CollapsedBranch>,
    pub consumed_switch_ids: Vec<String>,
    pub isolated_connectivity_node_ids: Vec<String>,
}

/// Topology rebuild result including the rebuilt network and a change summary.
#[derive(Debug, Clone)]
pub struct TopologyRebuild {
    pub network: Network,
    pub report: TopologyReport,
}

/// Initial topology projection: the derived buses and connectivity mapping
/// from a node-breaker topology, before equipment remapping.
#[derive(Debug, Clone)]
pub struct TopologyProjection {
    pub network: Network,
    pub mapping: TopologyMapping,
}

/// Derive a solver-facing bus projection and topology mapping from raw
/// node-breaker topology.
///
/// This is a lower-level entry point than [`rebuild_topology`]. It is useful
/// for importers or runtime flows that need the derived buses and mapping
/// before they have a fully built [`Network`] to rebuild.
pub fn project_node_breaker_topology(
    model: &NodeBreakerTopology,
) -> Result<TopologyProjection, TopologyError> {
    let cn_index = build_connectivity_index(model)?;
    let vl_kv = build_voltage_level_lookup(model)?;
    let mut uf = UnionFindIdx::new(model.connectivity_nodes.len());

    let mut consumed_switch_ids = Vec::new();
    for sw in &model.switches {
        if sw.open || sw.retained {
            continue;
        }
        let cn1 = *cn_index.get(sw.cn1_id.as_str()).ok_or_else(|| {
            TopologyError::UnknownSwitchConnectivityNode {
                switch_id: sw.id.clone(),
                connectivity_node_id: sw.cn1_id.clone(),
            }
        })?;
        let cn2 = *cn_index.get(sw.cn2_id.as_str()).ok_or_else(|| {
            TopologyError::UnknownSwitchConnectivityNode {
                switch_id: sw.id.clone(),
                connectivity_node_id: sw.cn2_id.clone(),
            }
        })?;
        uf.union(cn1, cn2);
        consumed_switch_ids.push(sw.id.clone());
    }

    let mut sorted_cn_indices: Vec<usize> = (0..model.connectivity_nodes.len()).collect();
    sorted_cn_indices.sort_by_key(|&idx| model.connectivity_nodes[idx].id.as_str());

    let mut root_to_bus = vec![0_u32; model.connectivity_nodes.len()];
    let mut connectivity_node_to_bus = HashMap::with_capacity(model.connectivity_nodes.len());
    let mut bus_to_connectivity_nodes: HashMap<u32, Vec<String>> = HashMap::new();
    let mut next_bus = 1_u32;

    for &cn_idx in &sorted_cn_indices {
        let root = uf.find(cn_idx);
        if root_to_bus[root] == 0 {
            root_to_bus[root] = next_bus;
            next_bus += 1;
        }
        let bus = root_to_bus[root];
        let cn_id = model.connectivity_nodes[cn_idx].id.clone();
        connectivity_node_to_bus.insert(cn_id.clone(), bus);
        bus_to_connectivity_nodes
            .entry(bus)
            .or_default()
            .push(cn_id);
    }

    let mut representative_cn = vec![None; (next_bus - 1) as usize];
    for &cn_idx in &sorted_cn_indices {
        let bus = connectivity_node_to_bus[model.connectivity_nodes[cn_idx].id.as_str()];
        representative_cn[(bus - 1) as usize].get_or_insert(cn_idx);
    }

    let mut buses = Vec::with_capacity((next_bus - 1) as usize);
    for bus_number in 1..next_bus {
        let cn_idx = representative_cn[(bus_number - 1) as usize].expect("bus has representative");
        let cn = &model.connectivity_nodes[cn_idx];
        let base_kv = *vl_kv.get(cn.voltage_level_id.as_str()).ok_or_else(|| {
            TopologyError::MissingVoltageLevel {
                connectivity_node_id: cn.id.clone(),
                voltage_level_id: cn.voltage_level_id.clone(),
            }
        })?;
        let mut bus = Bus::new(bus_number, BusType::PQ, base_kv);
        bus.name = format!("Bus_{bus_number}");
        buses.push(bus);
    }

    let connected_roots: HashSet<usize> = model
        .terminal_connections
        .iter()
        .filter_map(|tc| cn_index.get(tc.connectivity_node_id.as_str()).copied())
        .map(|idx| uf.find(idx))
        .collect();
    let isolated_connectivity_node_ids = sorted_cn_indices
        .iter()
        .filter_map(|&idx| {
            let cn = &model.connectivity_nodes[idx];
            (!connected_roots.contains(&uf.find(idx))).then(|| cn.id.clone())
        })
        .collect();

    let mapping = TopologyMapping {
        connectivity_node_to_bus,
        bus_to_connectivity_nodes,
        consumed_switch_ids,
        isolated_connectivity_node_ids,
    };

    debug!(
        n_cns = model.connectivity_nodes.len(),
        n_switches = model.switches.len(),
        n_buses = buses.len(),
        "topology projection complete"
    );

    Ok(TopologyProjection {
        network: Network {
            buses,
            ..Default::default()
        },
        mapping,
    })
}

/// Re-derive the bus-branch topology from the current switch states.
///
/// Reads the retained [`NodeBreakerTopology`] from `network.topology`,
/// rebuilds the bus set and connectivity mapping, and remaps all bus-referenced
/// equipment to the fresh topology.
pub fn rebuild_topology(network: &Network) -> Result<Network, TopologyError> {
    Ok(rebuild_topology_with_report(network)?.network)
}

/// Re-derive the bus-branch topology and return a detailed rebuild report.
pub fn rebuild_topology_with_report(network: &Network) -> Result<TopologyRebuild, TopologyError> {
    let sm = network
        .topology
        .as_ref()
        .ok_or(TopologyError::NoNodeBreakerTopology)?;
    let previous_mapping = sm
        .retained_mapping()
        .ok_or(TopologyError::MissingTopologyMapping)?;
    let topology = project_node_breaker_topology(sm)?;

    let old_bus_targets = build_old_bus_targets(previous_mapping, &topology.mapping);
    let equipment_buses = build_equipment_bus_lookup(sm, &topology.mapping)?;
    let mut updated = network.clone();
    let mut buses = topology.network.buses.clone();

    seed_bus_templates(&mut buses, &network.buses, &old_bus_targets);
    updated.buses = buses;

    let mut collapsed_branches = Vec::new();
    let mut rebuilt_branches = Vec::with_capacity(network.branches.len());
    for branch in &network.branches {
        match remap_branch(branch, &equipment_buses, &old_bus_targets)? {
            Some(mapped) => rebuilt_branches.push(mapped),
            None => collapsed_branches.push(CollapsedBranch::from_branch(branch)),
        }
    }
    updated.branches = rebuilt_branches;

    updated.generators = network
        .generators
        .iter()
        .map(|generator| remap_generator(generator, &equipment_buses, &old_bus_targets))
        .collect::<Result<Vec<_>, _>>()?;

    updated.loads = network
        .loads
        .iter()
        .map(|load| remap_load(load, &equipment_buses, &old_bus_targets))
        .collect::<Result<Vec<_>, _>>()?;

    updated.fixed_shunts = network
        .fixed_shunts
        .iter()
        .map(|shunt| remap_fixed_shunt(shunt, &equipment_buses, &old_bus_targets))
        .collect::<Result<Vec<_>, _>>()?;
    updated.power_injections = network
        .power_injections
        .iter()
        .map(|injection| remap_power_injection(injection, &equipment_buses, &old_bus_targets))
        .collect::<Result<Vec<_>, _>>()?;
    remap_area_schedules(&mut updated.area_schedules, &old_bus_targets)?;
    updated.rebuild_bus_state_from_explicit_equipment();

    remap_hvdc(&mut updated.hvdc, &equipment_buses, &old_bus_targets)?;
    remap_facts_devices(
        &mut updated.facts_devices,
        &equipment_buses,
        &old_bus_targets,
    )?;
    for item in &mut updated.induction_machines {
        item.bus = remap_bus_number(item.bus, &old_bus_targets)?;
    }
    for item in &mut updated.breaker_ratings {
        item.bus = remap_bus_number(item.bus, &old_bus_targets)?;
    }
    for item in &mut updated.cim.grounding_impedances {
        item.bus = remap_bus_number(item.bus, &old_bus_targets)?;
    }
    for item in &mut updated.cim.measurements {
        item.bus = remap_bus_number(item.bus, &old_bus_targets)?;
    }
    remap_oltc_specs(&mut updated.controls.oltc_specs, &old_bus_targets)?;
    remap_par_specs(&mut updated.controls.par_specs, &old_bus_targets)?;
    updated.metadata.multi_section_line_groups = network
        .metadata
        .multi_section_line_groups
        .iter()
        .filter_map(|group| remap_multi_section_line_group(group, &old_bus_targets).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    updated.interfaces = network
        .interfaces
        .iter()
        .filter_map(|interface| {
            remap_interface(interface, &equipment_buses, &old_bus_targets).transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    updated.flowgates = network
        .flowgates
        .iter()
        .filter_map(|flowgate| {
            remap_flowgate(flowgate, &equipment_buses, &old_bus_targets).transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    for limit_set in updated.cim.operational_limits.limit_sets.values_mut() {
        if limit_set.bus != 0 {
            limit_set.bus = remap_bus_number(limit_set.bus, &old_bus_targets)?;
        }
    }
    remap_dispatchable_loads(
        &mut updated.market_data.dispatchable_loads,
        &old_bus_targets,
    )?;
    remap_switched_shunts(&mut updated.controls.switched_shunts, &old_bus_targets)?;
    remap_switched_shunts_opf(&mut updated.controls.switched_shunts_opf, &old_bus_targets)?;

    apply_bus_types(
        &mut updated.buses,
        &network.buses,
        &old_bus_targets,
        &updated.generators,
    )?;

    let report = build_topology_report(
        network,
        &old_bus_targets,
        &topology.mapping,
        collapsed_branches,
    );

    let mut next_model = sm.clone();
    next_model.install_mapping(topology.mapping);
    updated.topology = Some(next_model);

    debug!(
        n_buses = updated.buses.len(),
        n_branches = updated.branches.len(),
        n_generators = updated.generators.len(),
        n_loads = updated.loads.len(),
        n_collapsed_branches = report.collapsed_branches.len(),
        "topology rebuild complete"
    );

    Ok(TopologyRebuild {
        network: updated,
        report,
    })
}

fn build_connectivity_index(
    model: &NodeBreakerTopology,
) -> Result<HashMap<&str, usize>, TopologyError> {
    let mut index = HashMap::with_capacity(model.connectivity_nodes.len());
    for (i, cn) in model.connectivity_nodes.iter().enumerate() {
        if index.insert(cn.id.as_str(), i).is_some() {
            return Err(TopologyError::DuplicateConnectivityNode(cn.id.clone()));
        }
    }
    Ok(index)
}

/// Build a voltage-level kV lookup and validate that every CN references a
/// known voltage level. The structural validation is redundant on rebuild
/// (only switch states change), but the cost is negligible and catches
/// corruption early.
fn build_voltage_level_lookup(
    model: &NodeBreakerTopology,
) -> Result<HashMap<&str, f64>, TopologyError> {
    let mut lookup = HashMap::with_capacity(model.voltage_levels.len());
    for vl in &model.voltage_levels {
        if lookup.insert(vl.id.as_str(), vl.base_kv).is_some() {
            return Err(TopologyError::DuplicateVoltageLevel(vl.id.clone()));
        }
    }
    for cn in &model.connectivity_nodes {
        if !lookup.contains_key(cn.voltage_level_id.as_str()) {
            return Err(TopologyError::MissingVoltageLevel {
                connectivity_node_id: cn.id.clone(),
                voltage_level_id: cn.voltage_level_id.clone(),
            });
        }
    }
    Ok(lookup)
}

fn build_old_bus_targets(
    previous_mapping: &TopologyMapping,
    new_mapping: &TopologyMapping,
) -> HashMap<u32, Vec<u32>> {
    previous_mapping
        .bus_to_connectivity_nodes
        .iter()
        .map(|(&old_bus, cns)| {
            let targets = cns
                .iter()
                .filter_map(|cn| {
                    new_mapping
                        .connectivity_node_to_bus
                        .get(cn.as_str())
                        .copied()
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            (old_bus, targets)
        })
        .collect()
}

fn build_topology_report(
    previous_network: &Network,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
    current_mapping: &TopologyMapping,
    mut collapsed_branches: Vec<CollapsedBranch>,
) -> TopologyReport {
    let mut bus_splits = old_bus_targets
        .iter()
        .filter(|(_, current_bus_numbers)| current_bus_numbers.len() > 1)
        .map(
            |(&previous_bus_number, current_bus_numbers)| TopologyBusSplit {
                previous_bus_number,
                current_bus_numbers: current_bus_numbers.clone(),
            },
        )
        .collect::<Vec<_>>();
    bus_splits.sort_by_key(|split| split.previous_bus_number);

    let mut current_to_previous: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&previous_bus_number, current_bus_numbers) in old_bus_targets {
        for &current_bus_number in current_bus_numbers {
            current_to_previous
                .entry(current_bus_number)
                .or_default()
                .push(previous_bus_number);
        }
    }

    let mut bus_merges = current_to_previous
        .into_iter()
        .filter_map(|(current_bus_number, mut previous_bus_numbers)| {
            previous_bus_numbers.sort_unstable();
            previous_bus_numbers.dedup();
            (previous_bus_numbers.len() > 1).then_some(TopologyBusMerge {
                current_bus_number,
                previous_bus_numbers,
            })
        })
        .collect::<Vec<_>>();
    bus_merges.sort_by_key(|merge| merge.current_bus_number);

    collapsed_branches.sort_by(|a, b| {
        (a.previous_from_bus, a.previous_to_bus, a.circuit.as_str()).cmp(&(
            b.previous_from_bus,
            b.previous_to_bus,
            b.circuit.as_str(),
        ))
    });

    let mut consumed_switch_ids = current_mapping.consumed_switch_ids.clone();
    consumed_switch_ids.sort();
    let mut isolated_connectivity_node_ids = current_mapping.isolated_connectivity_node_ids.clone();
    isolated_connectivity_node_ids.sort();

    TopologyReport {
        previous_bus_count: previous_network.buses.len(),
        current_bus_count: current_mapping.bus_to_connectivity_nodes.len(),
        bus_splits,
        bus_merges,
        collapsed_branches,
        consumed_switch_ids,
        isolated_connectivity_node_ids,
    }
}

fn build_equipment_bus_lookup<'a>(
    model: &'a NodeBreakerTopology,
    mapping: &'a TopologyMapping,
) -> Result<HashMap<&'a str, HashMap<u32, u32>>, TopologyError> {
    let mut lookup: HashMap<&str, HashMap<u32, u32>> = HashMap::new();
    for tc in &model.terminal_connections {
        if let Some(&bus) = mapping
            .connectivity_node_to_bus
            .get(tc.connectivity_node_id.as_str())
            && lookup
                .entry(tc.equipment_id.as_str())
                .or_default()
                .insert(tc.sequence_number, bus)
                .is_some()
        {
            return Err(TopologyError::DuplicateEquipmentTerminal {
                equipment_id: tc.equipment_id.clone(),
                sequence_number: tc.sequence_number,
            });
        }
    }
    Ok(lookup)
}

fn bus_number_to_index(buses: &[Bus]) -> HashMap<u32, usize> {
    buses
        .iter()
        .enumerate()
        .map(|(idx, bus)| (bus.number, idx))
        .collect()
}

fn seed_bus_templates(
    buses: &mut [Bus],
    base_buses: &[Bus],
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) {
    let new_bus_index = bus_number_to_index(buses);
    let mut initialized = vec![false; buses.len()];
    let mut contributor_count = vec![0_usize; buses.len()];

    for base_bus in base_buses {
        if let Some(targets) = old_bus_targets.get(&base_bus.number) {
            for &target in targets {
                let idx = new_bus_index[&target];
                contributor_count[idx] += 1;
                if initialized[idx] {
                    continue;
                }
                let mut template = base_bus.clone();
                template.number = target;
                template.base_kv = buses[idx].base_kv;
                template.shunt_conductance_mw = 0.0;
                template.shunt_susceptance_mvar = 0.0;
                buses[idx] = template;
                initialized[idx] = true;
            }
        }
    }

    for (idx, bus) in buses.iter_mut().enumerate() {
        if contributor_count[idx] != 1 {
            bus.name = format!("Bus_{}", bus.number);
        }
    }
}

fn apply_bus_types(
    buses: &mut [Bus],
    base_buses: &[Bus],
    old_bus_targets: &HashMap<u32, Vec<u32>>,
    generators: &[surge_network::network::Generator],
) -> Result<(), TopologyError> {
    let bus_index = bus_number_to_index(buses);
    let regulated_targets: HashSet<u32> = generators
        .iter()
        .filter(|generator| generator.can_voltage_regulate())
        .map(|generator| generator.reg_bus.unwrap_or(generator.bus))
        .collect();
    for bus in buses.iter_mut() {
        bus.bus_type = BusType::PQ;
    }

    for &target in &regulated_targets {
        let Some(&idx) = bus_index.get(&target) else {
            return Err(TopologyError::MissingBusMapping { bus_number: target });
        };
        if buses[idx].bus_type != BusType::Isolated {
            buses[idx].bus_type = BusType::PV;
        }
    }

    for base_bus in base_buses {
        if !matches!(base_bus.bus_type, BusType::Slack | BusType::PV) {
            continue;
        }
        let target = choose_bus_type_target(base_bus, old_bus_targets, &regulated_targets)?;
        let Some(&idx) = bus_index.get(&target) else {
            return Err(TopologyError::MissingBusMapping { bus_number: target });
        };
        if base_bus.bus_type == BusType::Slack {
            buses[idx].bus_type = BusType::Slack;
        } else if buses[idx].bus_type != BusType::Slack {
            buses[idx].bus_type = BusType::PV;
        }
    }

    Ok(())
}

fn choose_bus_type_target(
    base_bus: &Bus,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
    regulated_targets: &HashSet<u32>,
) -> Result<u32, TopologyError> {
    let Some(targets) = old_bus_targets.get(&base_bus.number) else {
        return Err(TopologyError::MissingBusMapping {
            bus_number: base_bus.number,
        });
    };
    match targets.as_slice() {
        [target] => return Ok(*target),
        [] => {
            return Err(TopologyError::MissingBusMapping {
                bus_number: base_bus.number,
            });
        }
        _ => {}
    }

    let pick_unique = |candidates: &HashSet<u32>| {
        targets
            .iter()
            .copied()
            .filter(|target| candidates.contains(target))
            .collect::<BTreeSet<_>>()
    };

    let regulated_matches = pick_unique(regulated_targets);

    match base_bus.bus_type {
        BusType::Slack => {
            if let Some(target) = regulated_matches
                .iter()
                .copied()
                .next()
                .filter(|_| regulated_matches.len() == 1)
            {
                return Ok(target);
            }
            Err(TopologyError::AmbiguousBusSplit {
                bus_number: base_bus.number,
            })
        }
        BusType::PV => {
            if let Some(target) = regulated_matches
                .iter()
                .copied()
                .next()
                .filter(|_| regulated_matches.len() == 1)
            {
                return Ok(target);
            }
            Err(TopologyError::AmbiguousBusSplit {
                bus_number: base_bus.number,
            })
        }
        _ => unreachable!("only slack/pv buses should be routed here"),
    }
}

fn remap_hvdc(
    hvdc: &mut surge_network::network::HvdcModel,
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<(), TopologyError> {
    for link in &mut hvdc.links {
        match link {
            surge_network::network::HvdcLink::Lcc(dc_line) => {
                let equipment_id = (!dc_line.name.is_empty()).then_some(dc_line.name.as_str());
                dc_line.rectifier.bus = resolve_equipment_bus(
                    dc_line.rectifier.bus,
                    equipment_id,
                    1,
                    equipment_buses,
                    old_bus_targets,
                )?;
                dc_line.inverter.bus = resolve_equipment_bus(
                    dc_line.inverter.bus,
                    equipment_id,
                    2,
                    equipment_buses,
                    old_bus_targets,
                )?;
            }
            surge_network::network::HvdcLink::Vsc(vsc_line) => {
                let equipment_id = (!vsc_line.name.is_empty()).then_some(vsc_line.name.as_str());
                vsc_line.converter1.bus = resolve_equipment_bus(
                    vsc_line.converter1.bus,
                    equipment_id,
                    1,
                    equipment_buses,
                    old_bus_targets,
                )?;
                vsc_line.converter2.bus = resolve_equipment_bus(
                    vsc_line.converter2.bus,
                    equipment_id,
                    2,
                    equipment_buses,
                    old_bus_targets,
                )?;
            }
        }
    }
    for dc_grid in &mut hvdc.dc_grids {
        for dc_converter in &mut dc_grid.converters {
            match dc_converter {
                surge_network::network::DcConverter::Lcc(c) => {
                    let equipment_id = (!c.id.is_empty()).then_some(c.id.as_str());
                    c.ac_bus = resolve_equipment_bus(
                        c.ac_bus,
                        equipment_id,
                        1,
                        equipment_buses,
                        old_bus_targets,
                    )?;
                }
                surge_network::network::DcConverter::Vsc(c) => {
                    let equipment_id = (!c.id.is_empty()).then_some(c.id.as_str());
                    c.ac_bus = resolve_equipment_bus(
                        c.ac_bus,
                        equipment_id,
                        1,
                        equipment_buses,
                        old_bus_targets,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn remap_facts_devices(
    facts_devices: &mut [surge_network::network::FactsDevice],
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<(), TopologyError> {
    for facts in facts_devices {
        let equipment_id = (!facts.name.is_empty()).then_some(facts.name.as_str());
        facts.bus_from = resolve_equipment_bus(
            facts.bus_from,
            equipment_id,
            1,
            equipment_buses,
            old_bus_targets,
        )?;
        if facts.bus_to != 0 {
            facts.bus_to = resolve_equipment_bus(
                facts.bus_to,
                equipment_id,
                2,
                equipment_buses,
                old_bus_targets,
            )?;
        }
    }
    Ok(())
}

fn remap_oltc_specs(
    oltc_specs: &mut [surge_network::network::OltcSpec],
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<(), TopologyError> {
    for oltc in oltc_specs {
        oltc.from_bus = remap_bus_number(oltc.from_bus, old_bus_targets)?;
        oltc.to_bus = remap_bus_number(oltc.to_bus, old_bus_targets)?;
        if oltc.regulated_bus != 0 {
            oltc.regulated_bus = remap_bus_number(oltc.regulated_bus, old_bus_targets)?;
        }
    }
    Ok(())
}

fn remap_par_specs(
    par_specs: &mut [surge_network::network::ParSpec],
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<(), TopologyError> {
    for par in par_specs {
        par.from_bus = remap_bus_number(par.from_bus, old_bus_targets)?;
        par.to_bus = remap_bus_number(par.to_bus, old_bus_targets)?;
        if par.monitored_from_bus != 0 {
            par.monitored_from_bus = remap_bus_number(par.monitored_from_bus, old_bus_targets)?;
        }
        if par.monitored_to_bus != 0 {
            par.monitored_to_bus = remap_bus_number(par.monitored_to_bus, old_bus_targets)?;
        }
    }
    Ok(())
}

fn remap_dispatchable_loads(
    loads: &mut [surge_network::market::DispatchableLoad],
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<(), TopologyError> {
    for load in loads {
        load.bus = remap_bus_number(load.bus, old_bus_targets)?;
    }
    Ok(())
}

fn remap_area_schedules(
    area_schedules: &mut [surge_network::network::AreaSchedule],
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<(), TopologyError> {
    for area in area_schedules {
        area.slack_bus = remap_bus_number(area.slack_bus, old_bus_targets)?;
    }
    Ok(())
}

fn remap_switched_shunts(
    shunts: &mut [surge_network::network::SwitchedShunt],
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<(), TopologyError> {
    for shunt in shunts {
        shunt.bus = remap_bus_number(shunt.bus, old_bus_targets)?;
        shunt.bus_regulated = remap_bus_number(shunt.bus_regulated, old_bus_targets)?;
    }
    Ok(())
}

fn remap_switched_shunts_opf(
    shunts: &mut [surge_network::network::SwitchedShuntOpf],
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<(), TopologyError> {
    for shunt in shunts {
        shunt.bus = remap_bus_number(shunt.bus, old_bus_targets)?;
    }
    Ok(())
}

fn remap_branch(
    branch: &surge_network::network::Branch,
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<Option<surge_network::network::Branch>, TopologyError> {
    let equipment_id = (!branch.circuit.is_empty()).then_some(branch.circuit.as_str());
    let from_bus = resolve_equipment_bus(
        branch.from_bus,
        equipment_id,
        1,
        equipment_buses,
        old_bus_targets,
    )?;
    let to_bus = resolve_equipment_bus(
        branch.to_bus,
        equipment_id,
        2,
        equipment_buses,
        old_bus_targets,
    )?;
    if from_bus == to_bus {
        return Ok(None);
    }
    let mut mapped = branch.clone();
    mapped.from_bus = from_bus;
    mapped.to_bus = to_bus;
    Ok(Some(mapped))
}

fn remap_generator(
    generator: &surge_network::network::Generator,
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<surge_network::network::Generator, TopologyError> {
    let mut mapped = generator.clone();
    mapped.bus = resolve_equipment_bus(
        generator.bus,
        generator.machine_id.as_deref().filter(|id| !id.is_empty()),
        1,
        equipment_buses,
        old_bus_targets,
    )?;
    if let Some(reg_bus) = mapped.reg_bus
        && reg_bus != 0
    {
        mapped.reg_bus = Some(remap_bus_number(reg_bus, old_bus_targets)?);
    }
    Ok(mapped)
}

fn remap_load(
    load: &surge_network::network::Load,
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<surge_network::network::Load, TopologyError> {
    let mut mapped = load.clone();
    mapped.bus = resolve_equipment_bus(
        load.bus,
        (!load.id.is_empty()).then_some(load.id.as_str()),
        1,
        equipment_buses,
        old_bus_targets,
    )?;
    Ok(mapped)
}

fn remap_fixed_shunt(
    shunt: &surge_network::network::FixedShunt,
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<surge_network::network::FixedShunt, TopologyError> {
    let mut mapped = shunt.clone();
    mapped.bus = resolve_equipment_bus(
        shunt.bus,
        (!shunt.id.is_empty()).then_some(shunt.id.as_str()),
        1,
        equipment_buses,
        old_bus_targets,
    )?;
    Ok(mapped)
}

fn remap_power_injection(
    injection: &surge_network::network::PowerInjection,
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<surge_network::network::PowerInjection, TopologyError> {
    let mut mapped = injection.clone();
    mapped.bus = resolve_equipment_bus(
        injection.bus,
        (!injection.id.is_empty()).then_some(injection.id.as_str()),
        1,
        equipment_buses,
        old_bus_targets,
    )?;
    Ok(mapped)
}

fn remap_multi_section_line_group(
    group: &MultiSectionLineGroup,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<Option<MultiSectionLineGroup>, TopologyError> {
    let mut mapped = group.clone();
    mapped.from_bus = remap_bus_number(group.from_bus, old_bus_targets)?;
    mapped.to_bus = remap_bus_number(group.to_bus, old_bus_targets)?;
    if mapped.from_bus == mapped.to_bus {
        return Ok(None);
    }
    let mut dummy_buses = Vec::new();
    for &dummy in &group.dummy_buses {
        let remapped = remap_bus_number(dummy, old_bus_targets)?;
        if remapped != mapped.from_bus
            && remapped != mapped.to_bus
            && dummy_buses.last().copied() != Some(remapped)
        {
            dummy_buses.push(remapped);
        }
    }
    mapped.dummy_buses = dummy_buses;
    Ok(Some(mapped))
}

fn remap_interface(
    interface: &surge_network::network::Interface,
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<Option<surge_network::network::Interface>, TopologyError> {
    let mut mapped = interface.clone();
    mapped.members.clear();
    for member in &interface.members {
        if let Some(remapped) = remap_branch_endpoints(
            &(
                member.branch.from_bus,
                member.branch.to_bus,
                member.branch.circuit.clone(),
            ),
            equipment_buses,
            old_bus_targets,
        )? {
            mapped
                .members
                .push(surge_network::network::WeightedBranchRef::new(
                    remapped.0,
                    remapped.1,
                    remapped.2,
                    member.coefficient,
                ));
        }
    }
    if mapped.members.is_empty() {
        return Ok(None);
    }
    Ok(Some(mapped))
}

fn remap_flowgate(
    flowgate: &surge_network::network::Flowgate,
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<Option<surge_network::network::Flowgate>, TopologyError> {
    let mut mapped = flowgate.clone();
    mapped.monitored.clear();
    for member in &flowgate.monitored {
        if let Some(remapped) = remap_branch_endpoints(
            &(
                member.branch.from_bus,
                member.branch.to_bus,
                member.branch.circuit.clone(),
            ),
            equipment_buses,
            old_bus_targets,
        )? {
            mapped
                .monitored
                .push(surge_network::network::WeightedBranchRef::new(
                    remapped.0,
                    remapped.1,
                    remapped.2,
                    member.coefficient,
                ));
        }
    }
    mapped.contingency_branch = match &flowgate.contingency_branch {
        Some(branch) => remap_branch_endpoints(
            &(branch.from_bus, branch.to_bus, branch.circuit.clone()),
            equipment_buses,
            old_bus_targets,
        )?
        .map(surge_network::network::BranchRef::from),
        None => None,
    };
    if mapped.monitored.is_empty() && mapped.contingency_branch.is_none() {
        return Ok(None);
    }
    Ok(Some(mapped))
}

fn remap_branch_endpoints(
    branch: &(u32, u32, String),
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<Option<(u32, u32, String)>, TopologyError> {
    let equipment_id = (!branch.2.is_empty()).then_some(branch.2.as_str());
    let from_bus =
        resolve_equipment_bus(branch.0, equipment_id, 1, equipment_buses, old_bus_targets)?;
    let to_bus =
        resolve_equipment_bus(branch.1, equipment_id, 2, equipment_buses, old_bus_targets)?;
    if from_bus == to_bus {
        return Ok(None);
    }
    Ok(Some((from_bus, to_bus, branch.2.clone())))
}

fn resolve_equipment_bus(
    old_bus: u32,
    equipment_id: Option<&str>,
    terminal_sequence: u32,
    equipment_buses: &HashMap<&str, HashMap<u32, u32>>,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<u32, TopologyError> {
    if let Some(id) = equipment_id
        && let Some(sequence_map) = equipment_buses.get(id)
        && let Some(&bus) = sequence_map.get(&terminal_sequence)
    {
        return Ok(bus);
    }
    remap_bus_number(old_bus, old_bus_targets)
}

fn remap_bus_number(
    old_bus: u32,
    old_bus_targets: &HashMap<u32, Vec<u32>>,
) -> Result<u32, TopologyError> {
    match old_bus_targets.get(&old_bus).map(Vec::as_slice) {
        Some([bus]) => Ok(*bus),
        Some([]) | None => Err(TopologyError::MissingBusMapping {
            bus_number: old_bus,
        }),
        Some([_, _, ..]) => Err(TopologyError::AmbiguousBusSplit {
            bus_number: old_bus,
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::topology::{
        ConnectivityNode, Substation, TerminalConnection, VoltageLevel,
    };
    use surge_network::network::{PowerInjection, SwitchDevice, SwitchType};

    fn make_model(
        cns: &[(&str, &str)],
        switches: &[(&str, &str, &str, bool)],
    ) -> NodeBreakerTopology {
        NodeBreakerTopology::new(
            vec![Substation {
                id: "SUB_1".into(),
                name: "Station 1".into(),
                region: None,
            }],
            vec![VoltageLevel {
                id: "VL_220".into(),
                name: "220 kV".into(),
                substation_id: "SUB_1".into(),
                base_kv: 220.0,
            }],
            Vec::new(),
            cns.iter()
                .map(|(id, vl)| ConnectivityNode {
                    id: id.to_string(),
                    name: id.to_string(),
                    voltage_level_id: vl.to_string(),
                })
                .collect(),
            Vec::new(),
            switches
                .iter()
                .map(|(id, cn1, cn2, open)| SwitchDevice {
                    id: id.to_string(),
                    name: id.to_string(),
                    switch_type: SwitchType::Breaker,
                    cn1_id: cn1.to_string(),
                    cn2_id: cn2.to_string(),
                    open: *open,
                    normal_open: *open,
                    retained: false,
                    rated_current: None,
                })
                .collect(),
            Vec::new(),
        )
    }

    #[test]
    fn closed_breaker_merges_cns() {
        let model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        let built = project_node_breaker_topology(&model).unwrap();

        assert_eq!(built.network.buses.len(), 1);
        assert_eq!(
            built.mapping.connectivity_node_to_bus["CN_A"],
            built.mapping.connectivity_node_to_bus["CN_B"]
        );
        assert_eq!(built.mapping.consumed_switch_ids, vec!["BRK_1"]);
    }

    #[test]
    fn open_breaker_splits_cns() {
        let model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", true)],
        );
        let built = project_node_breaker_topology(&model).unwrap();

        assert_eq!(built.network.buses.len(), 2);
        assert_ne!(
            built.mapping.connectivity_node_to_bus["CN_A"],
            built.mapping.connectivity_node_to_bus["CN_B"]
        );
        assert!(built.mapping.consumed_switch_ids.is_empty());
    }

    #[test]
    fn isolated_cn_detected() {
        let mut model = make_model(
            &[
                ("CN_A", "VL_220"),
                ("CN_B", "VL_220"),
                ("CN_ORPHAN", "VL_220"),
            ],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        model.terminal_connections = vec![
            TerminalConnection {
                terminal_id: "T1".into(),
                equipment_id: "LINE_1".into(),
                equipment_class: "ACLineSegment".into(),
                sequence_number: 1,
                connectivity_node_id: "CN_A".into(),
            },
            TerminalConnection {
                terminal_id: "T2".into(),
                equipment_id: "LINE_1".into(),
                equipment_class: "ACLineSegment".into(),
                sequence_number: 2,
                connectivity_node_id: "CN_B".into(),
            },
        ];

        let built = project_node_breaker_topology(&model).unwrap();
        assert_eq!(
            built.mapping.isolated_connectivity_node_ids,
            vec!["CN_ORPHAN"]
        );
    }

    #[test]
    fn switch_connected_cn_is_not_reported_isolated_without_direct_terminal() {
        let mut model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        model.terminal_connections.push(TerminalConnection {
            terminal_id: "LOAD_T1".into(),
            equipment_id: "LOAD_1".into(),
            equipment_class: "Load".into(),
            sequence_number: 1,
            connectivity_node_id: "CN_B".into(),
        });

        let built = project_node_breaker_topology(&model).unwrap();
        assert!(built.mapping.isolated_connectivity_node_ids.is_empty());
    }

    #[test]
    fn invalid_switch_endpoint_is_rejected() {
        let model = make_model(
            &[("CN_A", "VL_220")],
            &[("BRK_1", "CN_A", "CN_MISSING", false)],
        );
        assert!(matches!(
            project_node_breaker_topology(&model),
            Err(TopologyError::UnknownSwitchConnectivityNode { .. })
        ));
    }

    #[test]
    fn missing_voltage_level_is_rejected() {
        let model = make_model(&[("CN_A", "VL_MISSING")], &[]);
        assert!(matches!(
            project_node_breaker_topology(&model),
            Err(TopologyError::MissingVoltageLevel { .. })
        ));
    }

    #[test]
    fn rebuild_topology_requires_substation_topology() {
        let net = Network::default();
        assert!(matches!(
            rebuild_topology(&net),
            Err(TopologyError::NoNodeBreakerTopology)
        ));
    }

    #[test]
    fn rebuild_topology_requires_previous_mapping() {
        let net = Network {
            topology: Some(make_model(
                &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
                &[("BRK_1", "CN_A", "CN_B", false)],
            )),
            ..Default::default()
        };
        assert!(matches!(
            rebuild_topology(&net),
            Err(TopologyError::MissingTopologyMapping)
        ));
    }

    #[test]
    fn rebuild_topology_tracks_generator_terminal_on_bus_split() {
        let mut model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        model.terminal_connections.push(TerminalConnection {
            terminal_id: "GEN_1_T1".into(),
            equipment_id: "GEN_1".into(),
            equipment_class: "Generator".into(),
            sequence_number: 1,
            connectivity_node_id: "CN_B".into(),
        });

        let initial = project_node_breaker_topology(&model).unwrap();
        let net = Network {
            buses: initial.network.buses,
            generators: vec![{
                let mut generator = surge_network::network::Generator::new(1, 100.0, 1.0);
                generator.machine_id = Some("GEN_1".into());
                generator
            }],
            topology: Some(model.clone().with_mapping(initial.mapping)),
            ..Default::default()
        };

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology(&stale).unwrap();
        assert_eq!(rebuilt.buses.len(), 2);
        assert_eq!(rebuilt.generators[0].bus, 2);
        assert!(rebuilt.topology.as_ref().unwrap().is_current());
    }

    #[test]
    fn rebuild_topology_moves_slack_bus_to_generator_target_on_split() {
        let net = NetworkBuilder::new(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        )
        .terminal("GEN_T1", "GEN_1", "Generator", 1, "CN_B")
        .generator(1, 100.0, "GEN_1")
        .build();

        let mut stale = net;
        stale.buses[0].bus_type = BusType::Slack;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology(&stale).unwrap();
        let mapping = rebuilt
            .topology
            .as_ref()
            .unwrap()
            .current_mapping()
            .unwrap();
        let gen_bus = mapping.connectivity_node_to_bus["CN_B"];
        let gen_bus_type = rebuilt
            .buses
            .iter()
            .find(|bus| bus.number == gen_bus)
            .map(|bus| bus.bus_type);
        assert_eq!(gen_bus_type, Some(BusType::Slack));
    }

    #[test]
    fn rebuild_topology_rejects_non_regulating_slack_target_on_split() {
        let net = NetworkBuilder::new(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        )
        .terminal("GEN_T1", "GEN_1", "Generator", 1, "CN_B")
        .generator(1, 100.0, "GEN_1")
        .build();

        let mut stale = net;
        stale.buses[0].bus_type = BusType::Slack;
        stale.generators[0].voltage_regulated = false;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        assert!(matches!(
            rebuild_topology(&stale),
            Err(TopologyError::AmbiguousBusSplit { bus_number: 1 })
        ));
    }

    #[test]
    fn rebuild_topology_tracks_power_injection_terminal_on_bus_split() {
        let mut model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        model.terminal_connections.push(TerminalConnection {
            terminal_id: "ENI_1_T1".into(),
            equipment_id: "ENI_1".into(),
            equipment_class: "ExternalNetworkInjection".into(),
            sequence_number: 1,
            connectivity_node_id: "CN_A".into(),
        });

        let initial = project_node_breaker_topology(&model).unwrap();
        let net = Network {
            buses: initial.network.buses,
            power_injections: vec![PowerInjection {
                id: "ENI_1".into(),
                ..PowerInjection::new(1, 25.0, 7.5)
            }],
            topology: Some(model.clone().with_mapping(initial.mapping)),
            ..Default::default()
        };

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology(&stale).unwrap();
        let reduction = rebuilt
            .topology
            .as_ref()
            .and_then(NodeBreakerTopology::current_mapping)
            .unwrap();
        let expected_bus = reduction.connectivity_node_to_bus["CN_A"];
        assert_eq!(rebuilt.buses.len(), 2);
        assert_eq!(rebuilt.power_injections[0].bus, expected_bus);

        // PowerInjection acts as negative load; check via bus_load_p_mw/q_mvar.
        let host_idx = rebuilt
            .buses
            .iter()
            .position(|bus| bus.number == expected_bus)
            .unwrap();
        let p_demand = rebuilt.bus_load_p_mw();
        let q_demand = rebuilt.bus_load_q_mvar();
        assert!((p_demand[host_idx] + 25.0).abs() < 1e-6);
        assert!((q_demand[host_idx] + 7.5).abs() < 1e-6);
    }

    #[test]
    fn rebuild_topology_rejects_duplicate_equipment_terminal_sequence() {
        let mut model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        model.terminal_connections.extend([
            TerminalConnection {
                terminal_id: "LOAD_1_T1".into(),
                equipment_id: "LOAD_1".into(),
                equipment_class: "Load".into(),
                sequence_number: 1,
                connectivity_node_id: "CN_A".into(),
            },
            TerminalConnection {
                terminal_id: "LOAD_1_T1_DUP".into(),
                equipment_id: "LOAD_1".into(),
                equipment_class: "Load".into(),
                sequence_number: 1,
                connectivity_node_id: "CN_B".into(),
            },
        ]);

        let initial = project_node_breaker_topology(&model).unwrap();
        let net = Network {
            buses: initial.network.buses,
            loads: vec![surge_network::network::Load {
                bus: 1,
                id: "LOAD_1".into(),
                ..surge_network::network::Load::new(1, 10.0, 2.0)
            }],
            topology: Some(model.with_mapping(initial.mapping)),
            ..Default::default()
        };

        assert!(matches!(
            rebuild_topology(&net),
            Err(TopologyError::DuplicateEquipmentTerminal {
                equipment_id,
                sequence_number
            }) if equipment_id == "LOAD_1" && sequence_number == 1
        ));
    }

    #[test]
    fn rebuild_topology_with_report_tracks_bus_merges_and_collapsed_branches() {
        let model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", true)],
        );
        let initial = project_node_breaker_topology(&model).unwrap();
        let net = Network {
            buses: initial.network.buses,
            branches: vec![surge_network::network::Branch::new_line(
                1, 2, 0.0, 0.1, 0.0,
            )],
            topology: Some(model.with_mapping(initial.mapping)),
            ..Default::default()
        };

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", false);

        let rebuilt = rebuild_topology_with_report(&stale).unwrap();
        assert_eq!(rebuilt.network.buses.len(), 1);
        assert_eq!(rebuilt.network.branches.len(), 0);
        assert_eq!(rebuilt.report.previous_bus_count, 2);
        assert_eq!(rebuilt.report.current_bus_count, 1);
        assert!(rebuilt.report.bus_splits.is_empty());
        assert_eq!(rebuilt.report.bus_merges.len(), 1);
        assert_eq!(rebuilt.report.bus_merges[0].current_bus_number, 1);
        assert_eq!(
            rebuilt.report.bus_merges[0].previous_bus_numbers,
            vec![1, 2]
        );
        assert_eq!(rebuilt.report.collapsed_branches.len(), 1);
        assert_eq!(rebuilt.report.collapsed_branches[0].previous_from_bus, 1);
        assert_eq!(rebuilt.report.collapsed_branches[0].previous_to_bus, 2);
        assert_eq!(rebuilt.report.consumed_switch_ids, vec!["BRK_1"]);
    }

    // ------------------------------------------------------------------
    // Helper: build a Network with node-breaker topology and initial
    // projection already installed, ready for rebuild_topology().
    // ------------------------------------------------------------------

    /// Build a minimal Network with node-breaker topology, terminal
    /// connections, equipment, and an installed initial projection so that
    /// `rebuild_topology` / `rebuild_topology_with_report` can be called
    /// immediately.
    struct NetworkBuilder {
        model: NodeBreakerTopology,
        generators: Vec<surge_network::network::Generator>,
        loads: Vec<surge_network::network::Load>,
        branches: Vec<surge_network::network::Branch>,
    }

    impl NetworkBuilder {
        fn new(cns: &[(&str, &str)], switches: &[(&str, &str, &str, bool)]) -> Self {
            Self {
                model: make_model(cns, switches),
                generators: Vec::new(),
                loads: Vec::new(),
                branches: Vec::new(),
            }
        }

        fn terminal(
            mut self,
            term_id: &str,
            equip_id: &str,
            equip_class: &str,
            seq: u32,
            cn_id: &str,
        ) -> Self {
            self.model.terminal_connections.push(TerminalConnection {
                terminal_id: term_id.into(),
                equipment_id: equip_id.into(),
                equipment_class: equip_class.into(),
                sequence_number: seq,
                connectivity_node_id: cn_id.into(),
            });
            self
        }

        fn generator(mut self, bus: u32, pg: f64, machine_id: &str) -> Self {
            let mut g = surge_network::network::Generator::new(bus, pg, 1.0);
            g.machine_id = Some(machine_id.into());
            self.generators.push(g);
            self
        }

        fn load(mut self, bus: u32, p: f64, q: f64, id: &str) -> Self {
            let mut ld = surge_network::network::Load::new(bus, p, q);
            ld.id = id.into();
            self.loads.push(ld);
            self
        }

        fn branch(mut self, from: u32, to: u32) -> Self {
            self.branches.push(surge_network::network::Branch::new_line(
                from, to, 0.0, 0.1, 0.0,
            ));
            self
        }

        fn build(self) -> Network {
            let initial = project_node_breaker_topology(&self.model).unwrap();
            Network {
                buses: initial.network.buses,
                generators: self.generators,
                loads: self.loads,
                branches: self.branches,
                topology: Some(self.model.with_mapping(initial.mapping)),
                ..Default::default()
            }
        }
    }

    // ==================================================================
    // 1. Basic node-breaker scenarios
    // ==================================================================

    #[test]
    fn single_cn_no_switches_produces_single_bus() {
        let model = make_model(&[("CN_A", "VL_220")], &[]);
        let built = project_node_breaker_topology(&model).unwrap();

        assert_eq!(built.network.buses.len(), 1);
        assert_eq!(built.mapping.connectivity_node_to_bus["CN_A"], 1);
        assert!(built.mapping.consumed_switch_ids.is_empty());
    }

    #[test]
    fn two_cns_no_switches_produce_two_buses() {
        let model = make_model(&[("CN_A", "VL_220"), ("CN_B", "VL_220")], &[]);
        let built = project_node_breaker_topology(&model).unwrap();

        assert_eq!(built.network.buses.len(), 2);
        assert_ne!(
            built.mapping.connectivity_node_to_bus["CN_A"],
            built.mapping.connectivity_node_to_bus["CN_B"]
        );
    }

    #[test]
    fn transitive_closure_three_cns_two_closed_switches() {
        // CN_A ↔ CN_B (closed), CN_B ↔ CN_C (closed) → all merge into one bus.
        let model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220"), ("CN_C", "VL_220")],
            &[
                ("BRK_1", "CN_A", "CN_B", false),
                ("BRK_2", "CN_B", "CN_C", false),
            ],
        );
        let built = project_node_breaker_topology(&model).unwrap();

        assert_eq!(built.network.buses.len(), 1);
        let bus_a = built.mapping.connectivity_node_to_bus["CN_A"];
        let bus_b = built.mapping.connectivity_node_to_bus["CN_B"];
        let bus_c = built.mapping.connectivity_node_to_bus["CN_C"];
        assert_eq!(bus_a, bus_b);
        assert_eq!(bus_b, bus_c);
        assert_eq!(built.mapping.consumed_switch_ids.len(), 2);
    }

    #[test]
    fn transitive_closure_chain_of_four() {
        // CN1↔CN2 (closed), CN2↔CN3 (closed), CN3↔CN4 (closed) → all one bus.
        let model = make_model(
            &[
                ("CN_1", "VL_220"),
                ("CN_2", "VL_220"),
                ("CN_3", "VL_220"),
                ("CN_4", "VL_220"),
            ],
            &[
                ("BRK_1", "CN_1", "CN_2", false),
                ("BRK_2", "CN_2", "CN_3", false),
                ("BRK_3", "CN_3", "CN_4", false),
            ],
        );
        let built = project_node_breaker_topology(&model).unwrap();

        assert_eq!(built.network.buses.len(), 1);
        let bus = built.mapping.connectivity_node_to_bus["CN_1"];
        for cn in &["CN_2", "CN_3", "CN_4"] {
            assert_eq!(built.mapping.connectivity_node_to_bus[*cn], bus);
        }
    }

    // ==================================================================
    // 2. Open switch behavior
    // ==================================================================

    #[test]
    fn open_switch_between_two_cns_gives_different_buses() {
        let model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", true)],
        );
        let built = project_node_breaker_topology(&model).unwrap();

        assert_eq!(built.network.buses.len(), 2);
        assert_ne!(
            built.mapping.connectivity_node_to_bus["CN_A"],
            built.mapping.connectivity_node_to_bus["CN_B"]
        );
        assert!(built.mapping.consumed_switch_ids.is_empty());
    }

    #[test]
    fn ring_topology_with_one_open_switch_splits_correctly() {
        // Ring: CN_A ↔ CN_B ↔ CN_C ↔ CN_A
        // All switches closed except CN_C → CN_A (open).
        // CN_A and CN_B merge (via closed BRK_1). CN_C is separate.
        let model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220"), ("CN_C", "VL_220")],
            &[
                ("BRK_1", "CN_A", "CN_B", false),
                ("BRK_2", "CN_B", "CN_C", false),
                ("BRK_3", "CN_C", "CN_A", true), // open → breaks the ring
            ],
        );
        let built = project_node_breaker_topology(&model).unwrap();

        // CN_A, CN_B, CN_C all reachable through closed BRK_1 and BRK_2,
        // so they should all end up on the same bus. The open BRK_3 is
        // redundant for separation here because the closed path still
        // connects CN_A ↔ CN_B ↔ CN_C.
        assert_eq!(built.network.buses.len(), 1);

        // Now open BRK_2 to actually split.
        let model2 = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220"), ("CN_C", "VL_220")],
            &[
                ("BRK_1", "CN_A", "CN_B", false), // closed
                ("BRK_2", "CN_B", "CN_C", true),  // open → splits CN_C off
                ("BRK_3", "CN_C", "CN_A", true),  // open
            ],
        );
        let built2 = project_node_breaker_topology(&model2).unwrap();

        assert_eq!(built2.network.buses.len(), 2);
        // CN_A and CN_B share one bus; CN_C is on another.
        assert_eq!(
            built2.mapping.connectivity_node_to_bus["CN_A"],
            built2.mapping.connectivity_node_to_bus["CN_B"]
        );
        assert_ne!(
            built2.mapping.connectivity_node_to_bus["CN_B"],
            built2.mapping.connectivity_node_to_bus["CN_C"]
        );
    }

    // ==================================================================
    // 3. Bus merge/split reporting
    // ==================================================================

    #[test]
    fn report_bus_splits_on_switch_open() {
        // Start: BRK_1 closed → CN_A & CN_B on one bus.
        // Then open BRK_1 → two buses (split).
        let net = NetworkBuilder::new(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        )
        .build();

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology_with_report(&stale).unwrap();
        assert_eq!(rebuilt.report.previous_bus_count, 1);
        assert_eq!(rebuilt.report.current_bus_count, 2);
        assert_eq!(rebuilt.report.bus_splits.len(), 1);
        assert_eq!(rebuilt.report.bus_splits[0].previous_bus_number, 1);
        assert_eq!(rebuilt.report.bus_splits[0].current_bus_numbers.len(), 2);
        assert!(rebuilt.report.bus_merges.is_empty());
    }

    #[test]
    fn report_bus_merges_on_switch_close() {
        // Start: BRK_1 open → CN_A & CN_B on separate buses.
        // Then close BRK_1 → one bus (merge).
        let net = NetworkBuilder::new(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", true)],
        )
        .build();

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", false);

        let rebuilt = rebuild_topology_with_report(&stale).unwrap();
        assert_eq!(rebuilt.report.previous_bus_count, 2);
        assert_eq!(rebuilt.report.current_bus_count, 1);
        assert!(rebuilt.report.bus_splits.is_empty());
        assert_eq!(rebuilt.report.bus_merges.len(), 1);
        assert_eq!(rebuilt.report.bus_merges[0].current_bus_number, 1);
        assert_eq!(rebuilt.report.bus_merges[0].previous_bus_numbers.len(), 2);
    }

    #[test]
    fn rebuild_topology_remaps_area_schedule_slack_bus_on_merge() {
        let mut net = NetworkBuilder::new(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", true)],
        )
        .build();
        net.area_schedules
            .push(surge_network::network::AreaSchedule {
                number: 1,
                slack_bus: 2,
                p_desired_mw: 15.0,
                p_tolerance_mw: 5.0,
                name: "AREA 1".to_string(),
            });

        net.topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", false);

        let rebuilt = rebuild_topology(&net).unwrap();
        assert_eq!(rebuilt.area_schedules[0].slack_bus, 1);
    }

    #[test]
    fn report_no_changes_when_topology_unchanged() {
        // Start: BRK_1 closed. Rebuild without changing anything should
        // produce an identical topology with no splits or merges.
        let model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        let initial = project_node_breaker_topology(&model).unwrap();
        let mut net = Network {
            buses: initial.network.buses,
            topology: Some(model.with_mapping(initial.mapping)),
            ..Default::default()
        };
        // Mark stale so rebuild_topology will run, but don't change switches.
        net.topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);
        net.topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", false);

        let rebuilt = rebuild_topology_with_report(&net).unwrap();
        assert_eq!(rebuilt.report.previous_bus_count, 1);
        assert_eq!(rebuilt.report.current_bus_count, 1);
        assert!(rebuilt.report.bus_splits.is_empty());
        assert!(rebuilt.report.bus_merges.is_empty());
        assert!(rebuilt.report.collapsed_branches.is_empty());
    }

    // ==================================================================
    // 4. Branch collapse
    // ==================================================================

    #[test]
    fn branch_collapses_when_both_terminals_same_bus() {
        // Start: BRK_1 open → two buses, branch connects them.
        // Close BRK_1 → buses merge → branch collapses.
        let net = NetworkBuilder::new(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", true)],
        )
        .branch(1, 2)
        .build();

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", false);

        let rebuilt = rebuild_topology_with_report(&stale).unwrap();
        assert_eq!(rebuilt.network.branches.len(), 0);
        assert_eq!(rebuilt.report.collapsed_branches.len(), 1);
        assert_eq!(rebuilt.report.collapsed_branches[0].previous_from_bus, 1);
        assert_eq!(rebuilt.report.collapsed_branches[0].previous_to_bus, 2);
    }

    #[test]
    fn branch_survives_when_terminals_on_different_buses() {
        // BRK_1 open → two buses, branch connects them. Rebuild with switch
        // still open → branch remains.
        let model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", true)],
        );
        let initial = project_node_breaker_topology(&model).unwrap();
        let mut net = Network {
            buses: initial.network.buses,
            branches: vec![surge_network::network::Branch::new_line(
                1, 2, 0.0, 0.1, 0.0,
            )],
            topology: Some(model.with_mapping(initial.mapping)),
            ..Default::default()
        };
        // Toggle and restore to get stale flag without changing state.
        net.topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", false);
        net.topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology_with_report(&net).unwrap();
        assert_eq!(rebuilt.network.branches.len(), 1);
        assert!(rebuilt.report.collapsed_branches.is_empty());
    }

    // ==================================================================
    // 5. Equipment remapping
    // ==================================================================

    #[test]
    fn generator_bus_remapped_after_split() {
        // Start: BRK_1 closed, one bus. Generator has terminal on CN_B.
        // Open BRK_1 → split → generator moves to CN_B's new bus.
        let net = NetworkBuilder::new(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        )
        .terminal("GEN_T1", "GEN_1", "Generator", 1, "CN_B")
        .generator(1, 100.0, "GEN_1")
        .build();

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology(&stale).unwrap();
        assert_eq!(rebuilt.buses.len(), 2);

        let mapping = rebuilt
            .topology
            .as_ref()
            .unwrap()
            .current_mapping()
            .unwrap();
        let expected_bus = mapping.connectivity_node_to_bus["CN_B"];
        assert_eq!(rebuilt.generators[0].bus, expected_bus);
    }

    #[test]
    fn load_bus_remapped_after_split() {
        // Start: BRK_1 closed, one bus. Load has terminal on CN_A.
        // Open BRK_1 → split → load stays on CN_A's new bus.
        let net = NetworkBuilder::new(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        )
        .terminal("LOAD_T1", "LOAD_1", "Load", 1, "CN_A")
        .load(1, 50.0, 10.0, "LOAD_1")
        .build();

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology(&stale).unwrap();
        assert_eq!(rebuilt.buses.len(), 2);

        let mapping = rebuilt
            .topology
            .as_ref()
            .unwrap()
            .current_mapping()
            .unwrap();
        let expected_bus = mapping.connectivity_node_to_bus["CN_A"];
        assert_eq!(rebuilt.loads[0].bus, expected_bus);
    }

    #[test]
    fn rebuild_topology_does_not_inherit_stale_isolated_bus_type() {
        let net = NetworkBuilder::new(&[("CN_A", "VL_220")], &[]).build();
        let mut stale = net;
        stale.buses[0].bus_type = BusType::Isolated;

        let rebuilt = rebuild_topology(&stale).unwrap();
        assert_ne!(rebuilt.buses[0].bus_type, BusType::Isolated);
        assert_eq!(rebuilt.buses[0].bus_type, BusType::PQ);
    }

    #[test]
    fn branch_endpoints_remapped_after_split() {
        // 2 CNs with a closed switch → 1 bus. A line (circuit="LINE_AC")
        // connects CN_A (terminal 1) to CN_B (terminal 2). After opening the
        // switch the branch endpoints are remapped to the two new buses.
        let mut model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        model.terminal_connections = vec![
            TerminalConnection {
                terminal_id: "LINE_T1".into(),
                equipment_id: "LINE_AC".into(),
                equipment_class: "ACLineSegment".into(),
                sequence_number: 1,
                connectivity_node_id: "CN_A".into(),
            },
            TerminalConnection {
                terminal_id: "LINE_T2".into(),
                equipment_id: "LINE_AC".into(),
                equipment_class: "ACLineSegment".into(),
                sequence_number: 2,
                connectivity_node_id: "CN_B".into(),
            },
        ];

        let initial = project_node_breaker_topology(&model).unwrap();
        assert_eq!(initial.network.buses.len(), 1);

        // Build branch with circuit matching the equipment_id so remap_branch
        // can resolve each endpoint via the terminal connections.
        let mut branch = surge_network::network::Branch::new_line(1, 1, 0.0, 0.1, 0.0);
        branch.circuit = "LINE_AC".into();

        let net = Network {
            buses: initial.network.buses,
            branches: vec![branch],
            topology: Some(model.with_mapping(initial.mapping)),
            ..Default::default()
        };

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology(&stale).unwrap();
        assert_eq!(rebuilt.buses.len(), 2);

        let mapping = rebuilt
            .topology
            .as_ref()
            .unwrap()
            .current_mapping()
            .unwrap();
        let bus_a = mapping.connectivity_node_to_bus["CN_A"];
        let bus_b = mapping.connectivity_node_to_bus["CN_B"];
        assert_ne!(bus_a, bus_b);
        // The branch survived (endpoints differ) and is remapped.
        assert_eq!(rebuilt.branches.len(), 1);
        assert_eq!(rebuilt.branches[0].from_bus, bus_a);
        assert_eq!(rebuilt.branches[0].to_bus, bus_b);
    }

    #[test]
    fn hvdc_link_endpoints_remapped_after_split_via_terminal_identity() {
        let mut model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        model.terminal_connections = vec![
            TerminalConnection {
                terminal_id: "HVDC_T1".into(),
                equipment_id: "HVDC_1".into(),
                equipment_class: "VsConverter".into(),
                sequence_number: 1,
                connectivity_node_id: "CN_A".into(),
            },
            TerminalConnection {
                terminal_id: "HVDC_T2".into(),
                equipment_id: "HVDC_1".into(),
                equipment_class: "VsConverter".into(),
                sequence_number: 2,
                connectivity_node_id: "CN_B".into(),
            },
        ];

        let initial = project_node_breaker_topology(&model).unwrap();
        let mut link = surge_network::network::VscHvdcLink {
            name: "HVDC_1".into(),
            ..Default::default()
        };
        link.converter1.bus = 1;
        link.converter2.bus = 1;

        let net = Network {
            buses: initial.network.buses,
            hvdc: surge_network::network::HvdcModel {
                links: vec![surge_network::network::HvdcLink::Vsc(link)],
                dc_grids: Vec::new(),
            },
            topology: Some(model.with_mapping(initial.mapping)),
            ..Default::default()
        };

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology(&stale).unwrap();
        let mapping = rebuilt
            .topology
            .as_ref()
            .unwrap()
            .current_mapping()
            .unwrap();
        let bus_a = mapping.connectivity_node_to_bus["CN_A"];
        let bus_b = mapping.connectivity_node_to_bus["CN_B"];
        let surge_network::network::HvdcLink::Vsc(link) = &rebuilt.hvdc.links[0] else {
            panic!("expected VSC HVDC link");
        };
        assert_eq!(link.converter1.bus, bus_a);
        assert_eq!(link.converter2.bus, bus_b);
    }

    #[test]
    fn explicit_dc_converter_remapped_after_split_via_terminal_identity() {
        let mut model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        model.terminal_connections.push(TerminalConnection {
            terminal_id: "DC_CONV_T1".into(),
            equipment_id: "DC_CONV_1".into(),
            equipment_class: "VsConverter".into(),
            sequence_number: 1,
            connectivity_node_id: "CN_B".into(),
        });

        let initial = project_node_breaker_topology(&model).unwrap();
        let converter = surge_network::network::DcConverterStation {
            id: "DC_CONV_1".into(),
            dc_bus: 101,
            ac_bus: 1,
            control_type_dc: 1,
            control_type_ac: 1,
            active_power_mw: 0.0,
            reactive_power_mvar: 0.0,
            is_lcc: false,
            voltage_setpoint_pu: 1.0,
            transformer_r_pu: 0.0,
            transformer_x_pu: 0.0,
            transformer: false,
            tap_ratio: 1.0,
            filter_susceptance_pu: 0.0,
            filter: false,
            reactor_r_pu: 0.0,
            reactor_x_pu: 0.0,
            reactor: false,
            base_kv_ac: 220.0,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            current_max_pu: 2.0,
            status: true,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_quadratic_rectifier: 0.0,
            loss_quadratic_inverter: 0.0,
            droop: 0.0,
            power_dc_setpoint_mw: 10.0,
            voltage_dc_setpoint_pu: 1.0,
            active_power_ac_max_mw: 100.0,
            active_power_ac_min_mw: -100.0,
            reactive_power_ac_max_mvar: 50.0,
            reactive_power_ac_min_mvar: -50.0,
        };
        let dc_grid = surge_network::network::DcGrid {
            id: 1,
            name: Some("grid".into()),
            buses: vec![surge_network::network::DcBus {
                bus_id: 101,
                p_dc_mw: 0.0,
                v_dc_pu: 1.0,
                base_kv_dc: 320.0,
                v_dc_max: 1.1,
                v_dc_min: 0.9,
                cost: 0.0,
                g_shunt_siemens: 0.0,
                r_ground_ohm: 0.0,
            }],
            converters: vec![converter.clone().into()],
            branches: Vec::new(),
        };

        let net = Network {
            buses: initial.network.buses,
            hvdc: surge_network::network::HvdcModel {
                links: Vec::new(),
                dc_grids: vec![dc_grid],
            },
            topology: Some(model.with_mapping(initial.mapping)),
            ..Default::default()
        };

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology(&stale).unwrap();
        let mapping = rebuilt
            .topology
            .as_ref()
            .unwrap()
            .current_mapping()
            .unwrap();
        let expected_bus = mapping.connectivity_node_to_bus["CN_B"];
        let converter = rebuilt.hvdc.dc_grids[0].converters[0]
            .as_vsc()
            .expect("expected VSC converter");
        assert_eq!(converter.ac_bus, expected_bus);
    }

    #[test]
    fn facts_endpoints_remapped_after_split_via_terminal_identity() {
        let mut model = make_model(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", false)],
        );
        model.terminal_connections = vec![
            TerminalConnection {
                terminal_id: "FACTS_T1".into(),
                equipment_id: "FACTS_1".into(),
                equipment_class: "StaticVarCompensator".into(),
                sequence_number: 1,
                connectivity_node_id: "CN_A".into(),
            },
            TerminalConnection {
                terminal_id: "FACTS_T2".into(),
                equipment_id: "FACTS_1".into(),
                equipment_class: "StaticVarCompensator".into(),
                sequence_number: 2,
                connectivity_node_id: "CN_B".into(),
            },
        ];

        let initial = project_node_breaker_topology(&model).unwrap();
        let facts = surge_network::network::FactsDevice {
            name: "FACTS_1".into(),
            bus_from: 1,
            bus_to: 1,
            mode: surge_network::network::FactsMode::ShuntSeries,
            p_setpoint_mw: 0.0,
            q_setpoint_mvar: 0.0,
            voltage_setpoint_pu: 1.0,
            q_max: 50.0,
            series_reactance_pu: 0.01,
            in_service: true,
            ..Default::default()
        };

        let net = Network {
            buses: initial.network.buses,
            facts_devices: vec![facts],
            topology: Some(model.with_mapping(initial.mapping)),
            ..Default::default()
        };

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", true);

        let rebuilt = rebuild_topology(&stale).unwrap();
        let mapping = rebuilt
            .topology
            .as_ref()
            .unwrap()
            .current_mapping()
            .unwrap();
        let bus_a = mapping.connectivity_node_to_bus["CN_A"];
        let bus_b = mapping.connectivity_node_to_bus["CN_B"];
        assert_eq!(rebuilt.facts_devices[0].bus_from, bus_a);
        assert_eq!(rebuilt.facts_devices[0].bus_to, bus_b);
    }

    #[test]
    fn generator_and_load_on_same_bus_after_merge() {
        // Start: BRK_1 open → 2 buses. Gen on bus 1 (CN_A), load on bus 2 (CN_B).
        // Close BRK_1 → merge → both on same bus.
        let net = NetworkBuilder::new(
            &[("CN_A", "VL_220"), ("CN_B", "VL_220")],
            &[("BRK_1", "CN_A", "CN_B", true)],
        )
        .terminal("GEN_T1", "GEN_1", "Generator", 1, "CN_A")
        .terminal("LOAD_T1", "LOAD_1", "Load", 1, "CN_B")
        .generator(1, 100.0, "GEN_1")
        .load(2, 50.0, 10.0, "LOAD_1")
        .build();

        let mut stale = net;
        stale
            .topology
            .as_mut()
            .unwrap()
            .set_switch_state("BRK_1", false);

        let rebuilt = rebuild_topology(&stale).unwrap();
        assert_eq!(rebuilt.buses.len(), 1);
        assert_eq!(rebuilt.generators[0].bus, rebuilt.loads[0].bus);
    }

    // ==================================================================
    // 6. Error cases
    // ==================================================================

    #[test]
    fn rebuild_on_bus_branch_only_network_errors() {
        let net = Network::default();
        let err = rebuild_topology(&net).unwrap_err();
        assert!(matches!(err, TopologyError::NoNodeBreakerTopology));
    }

    #[test]
    fn rebuild_without_prior_mapping_errors() {
        let net = Network {
            topology: Some(make_model(&[("CN_A", "VL_220")], &[])),
            ..Default::default()
        };
        let err = rebuild_topology(&net).unwrap_err();
        assert!(matches!(err, TopologyError::MissingTopologyMapping));
    }

    #[test]
    fn duplicate_connectivity_node_id_errors() {
        let mut model = make_model(&[("CN_A", "VL_220")], &[]);
        model.connectivity_nodes.push(ConnectivityNode {
            id: "CN_A".into(),
            name: "CN_A dup".into(),
            voltage_level_id: "VL_220".into(),
        });
        let err = project_node_breaker_topology(&model).unwrap_err();
        assert!(matches!(err, TopologyError::DuplicateConnectivityNode(ref id) if id == "CN_A"));
    }

    #[test]
    fn retained_switch_not_consumed() {
        // A retained switch should not merge CNs even when closed.
        let mut model = make_model(&[("CN_A", "VL_220"), ("CN_B", "VL_220")], &[]);
        model.switches.push(SwitchDevice {
            id: "BRK_RET".into(),
            name: "Retained breaker".into(),
            switch_type: SwitchType::Breaker,
            cn1_id: "CN_A".into(),
            cn2_id: "CN_B".into(),
            open: false, // closed
            normal_open: false,
            retained: true, // but retained → should act as topology boundary
            rated_current: None,
        });

        let built = project_node_breaker_topology(&model).unwrap();
        assert_eq!(built.network.buses.len(), 2);
        assert!(built.mapping.consumed_switch_ids.is_empty());
    }
}
