// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use std::collections::HashMap;

use surge_network::network::topology::{
    self as sub, BusbarSection as SubBusbar, ConnectivityNode as SubCn, TerminalConnection,
    TopologyMapping,
};
use surge_network::network::{NodeBreakerTopology, SwitchDevice, SwitchType};

use super::indices::CgmesIndices;
use super::topology::SWITCH_CLASSES;
use super::types::ObjMap;

// ---------------------------------------------------------------------------
// Substation model extraction (node-breaker topology preservation)
// ---------------------------------------------------------------------------

/// Extract the physical topology hierarchy from the CIM object store into a
/// [`NodeBreakerTopology`].  Returns `None` when the model has no
/// `ConnectivityNode` objects (i.e. a purely bus-branch source).
pub(crate) fn build_substation_topology(
    objects: &ObjMap,
    idx: &CgmesIndices,
) -> Option<NodeBreakerTopology> {
    // Only build when there are connectivity nodes.
    let cn_count = objects
        .values()
        .filter(|o| o.class == "ConnectivityNode")
        .count();
    if cn_count == 0 {
        return None;
    }

    // --- Substations ---
    let substations: Vec<sub::Substation> = objects
        .iter()
        .filter(|(_, o)| o.class == "Substation")
        .map(|(id, o)| sub::Substation {
            id: id.clone(),
            name: o.get_text("name").unwrap_or("").to_string(),
            region: o.get_ref("Region").map(|s| s.to_string()),
        })
        .collect();

    // --- Voltage levels ---
    let voltage_levels: Vec<sub::VoltageLevel> = objects
        .iter()
        .filter(|(_, o)| o.class == "VoltageLevel")
        .map(|(id, o)| {
            let sub_id = o
                .get_ref("Substation")
                .or_else(|| o.get_ref("MemberOf_Substation"))
                .unwrap_or("")
                .to_string();
            let bv_id = o.get_ref("BaseVoltage").unwrap_or("");
            let base_kv = idx.bv_kv.get(bv_id).copied().unwrap_or(1.0);
            sub::VoltageLevel {
                id: id.clone(),
                name: o.get_text("name").unwrap_or("").to_string(),
                substation_id: sub_id,
                base_kv,
            }
        })
        .collect();

    // --- Bays ---
    let bays: Vec<sub::Bay> = objects
        .iter()
        .filter(|(_, o)| o.class == "Bay")
        .map(|(id, o)| sub::Bay {
            id: id.clone(),
            name: o.get_text("name").unwrap_or("").to_string(),
            voltage_level_id: o
                .get_ref("VoltageLevel")
                .or_else(|| o.get_ref("MemberOf_VoltageLevel"))
                .unwrap_or("")
                .to_string(),
        })
        .collect();

    // --- Connectivity nodes ---
    let connectivity_nodes: Vec<SubCn> = objects
        .iter()
        .filter(|(_, o)| o.class == "ConnectivityNode")
        .map(|(id, o)| SubCn {
            id: id.clone(),
            name: o.get_text("name").unwrap_or("").to_string(),
            voltage_level_id: o
                .get_ref("ConnectivityNodeContainer")
                .unwrap_or("")
                .to_string(),
        })
        .collect();

    // --- Busbar sections ---
    // Resolve BusbarSection → CN via its terminal.
    let busbar_sections: Vec<SubBusbar> = objects
        .iter()
        .filter(|(_, o)| o.class == "BusbarSection")
        .filter_map(|(id, o)| {
            let terms = idx.eq_terminals.get(id.as_str())?;
            let cn_id = objects
                .get(terms.first()?)?
                .get_ref("ConnectivityNode")
                .or_else(|| {
                    // After reduce_topology, CN may be gone — try TN→CN fallback.
                    objects
                        .get(terms.first()?)?
                        .get_ref("TopologicalNode")
                        .and_then(|tn| {
                            // Find any CN that maps to this TN (reverse lookup).
                            connectivity_nodes
                                .iter()
                                .find(|cn| {
                                    objects
                                        .get(&cn.id)
                                        .and_then(|o2| o2.get_ref("TopologicalNode"))
                                        .is_some_and(|t| t == tn)
                                })
                                .map(|cn| cn.id.as_str())
                        })
                })?
                .to_string();
            Some(SubBusbar {
                id: id.clone(),
                name: o.get_text("name").unwrap_or("").to_string(),
                connectivity_node_id: cn_id,
                ip_max: o.parse_f64("ipMax"),
            })
        })
        .collect();

    // --- Switches ---
    // Build terminal → CN map (may use ConnectivityNode or fall back to TN→CN).
    let terminal_to_cn: HashMap<&str, &str> = objects
        .iter()
        .filter(|(_, o)| o.class == "Terminal")
        .filter_map(|(id, o)| o.get_ref("ConnectivityNode").map(|cn| (id.as_str(), cn)))
        .collect();

    let switches: Vec<SwitchDevice> = objects
        .iter()
        .filter(|(_, o)| SWITCH_CLASSES.contains(&o.class.as_str()))
        .filter_map(|(id, o)| {
            // Get this switch's terminals.
            let terms = idx.eq_terminals.get(id.as_str())?;
            if terms.len() < 2 {
                return None;
            }
            let cn1 = terminal_to_cn.get(terms[0].as_str())?.to_string();
            let cn2 = terminal_to_cn.get(terms[1].as_str())?.to_string();

            let open = o
                .get_text("open")
                .or_else(|| o.get_text("normalOpen"))
                .is_some_and(|s| s.eq_ignore_ascii_case("true"));
            let normal_open = o
                .get_text("normalOpen")
                .is_some_and(|s| s.eq_ignore_ascii_case("true"));
            let retained = o
                .get_text("retained")
                .is_some_and(|s| s.eq_ignore_ascii_case("true"));

            let switch_type = match o.class.as_str() {
                "Breaker" => SwitchType::Breaker,
                "Disconnector" => SwitchType::Disconnector,
                "LoadBreakSwitch" => SwitchType::LoadBreakSwitch,
                "Fuse" => SwitchType::Fuse,
                "GroundDisconnector" => SwitchType::GroundDisconnector,
                _ => SwitchType::Switch,
            };

            Some(SwitchDevice {
                id: id.clone(),
                name: o.get_text("name").unwrap_or("").to_string(),
                switch_type,
                cn1_id: cn1,
                cn2_id: cn2,
                open,
                normal_open,
                retained,
                rated_current: o.parse_f64("ratedCurrent"),
            })
        })
        .collect();

    // --- Terminal connections ---
    let terminal_connections: Vec<TerminalConnection> = objects
        .iter()
        .filter(|(_, o)| o.class == "Terminal")
        .filter_map(|(tid, t)| {
            let cn_id = t.get_ref("ConnectivityNode")?;
            let eq_id = t.get_ref("ConductingEquipment")?;
            let eq_class = objects.get(eq_id).map(|o| o.class.as_str()).unwrap_or("");
            // Skip switch terminals — they are modelled as SwitchDevice, not equipment.
            if SWITCH_CLASSES.contains(&eq_class) {
                return None;
            }
            let seq = t
                .get_text("sequenceNumber")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1);
            Some(TerminalConnection {
                terminal_id: tid.clone(),
                equipment_id: eq_id.to_string(),
                equipment_class: eq_class.to_string(),
                sequence_number: seq,
                connectivity_node_id: cn_id.to_string(),
            })
        })
        .collect();

    tracing::debug!(
        substations = substations.len(),
        voltage_levels = voltage_levels.len(),
        bays = bays.len(),
        connectivity_nodes = connectivity_nodes.len(),
        busbar_sections = busbar_sections.len(),
        switches = switches.len(),
        terminal_connections = terminal_connections.len(),
        "extracted NodeBreakerTopology from CGMES"
    );

    Some(NodeBreakerTopology::new(
        substations,
        voltage_levels,
        bays,
        connectivity_nodes,
        busbar_sections,
        switches,
        terminal_connections,
    ))
}

/// Build the [`TopologyMapping`] from the CN→TN→bus chain already computed by
/// [`CgmesIndices`] during `build_network`.
pub(crate) fn build_topology_mapping(
    objects: &ObjMap,
    idx: &CgmesIndices,
    sm: &NodeBreakerTopology,
) -> TopologyMapping {
    let mut connectivity_node_to_bus: HashMap<String, u32> = HashMap::new();
    let mut bus_to_connectivity_nodes: HashMap<u32, Vec<String>> = HashMap::new();

    // Pre-build CN mRID → first Terminal mRID index to avoid O(N*M) scan.
    let mut cn_terminal_index: HashMap<&str, &str> = HashMap::new();
    for (tid, obj) in objects.iter() {
        if obj.class == "Terminal"
            && let Some(cn_ref) = obj.get_ref("ConnectivityNode")
        {
            cn_terminal_index.entry(cn_ref).or_insert(tid);
        }
    }

    for cn in &sm.connectivity_nodes {
        // CN → Terminal → TN → bus
        let bus_num = cn_terminal_index
            .get(cn.id.as_str())
            .and_then(|tid| idx.terminal_tn(objects, tid))
            .and_then(|tn| idx.tn_bus(tn));

        if let Some(bus) = bus_num {
            connectivity_node_to_bus.insert(cn.id.clone(), bus);
            bus_to_connectivity_nodes
                .entry(bus)
                .or_default()
                .push(cn.id.clone());
        }
    }

    // Consumed switches: closed switches whose two CNs map to the same bus.
    let consumed_switch_ids: Vec<String> = sm
        .switches
        .iter()
        .filter(|sw| !sw.open && !sw.retained)
        .filter(|sw| {
            connectivity_node_to_bus.get(&sw.cn1_id) == connectivity_node_to_bus.get(&sw.cn2_id)
                && connectivity_node_to_bus.contains_key(&sw.cn1_id)
        })
        .map(|sw| sw.id.clone())
        .collect();

    // Isolated CNs: no terminal connections.
    let connected_cns: std::collections::HashSet<&str> = sm
        .terminal_connections
        .iter()
        .map(|tc| tc.connectivity_node_id.as_str())
        .collect();
    let isolated_connectivity_node_ids: Vec<String> = sm
        .connectivity_nodes
        .iter()
        .filter(|cn| !connected_cns.contains(cn.id.as_str()))
        .map(|cn| cn.id.clone())
        .collect();

    TopologyMapping {
        connectivity_node_to_bus,
        bus_to_connectivity_nodes,
        consumed_switch_ids,
        isolated_connectivity_node_ids,
    }
}
