// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared helpers for surge-topology integration tests.

use std::collections::HashMap;

use surge_network::network::topology::{
    ConnectivityNode, Substation, TopologyMapping, VoltageLevel,
};
use surge_network::network::{NodeBreakerTopology, SwitchDevice, SwitchType};

/// Build a minimal `NodeBreakerTopology` with a single substation, a single
/// voltage level, the given connectivity nodes, and the given switches.
pub fn make_topology(cns: &[&str], switches: &[(&str, &str, &str, bool)]) -> NodeBreakerTopology {
    make_topology_with_vl(cns, switches, "VL_220")
}

pub fn make_topology_with_vl(
    cns: &[&str],
    switches: &[(&str, &str, &str, bool)],
    vl_id: &str,
) -> NodeBreakerTopology {
    NodeBreakerTopology::new(
        vec![Substation {
            id: "SUB_1".into(),
            name: "Station 1".into(),
            region: None,
        }],
        vec![VoltageLevel {
            id: vl_id.into(),
            name: "220 kV".into(),
            substation_id: "SUB_1".into(),
            base_kv: 220.0,
        }],
        Vec::new(),
        cns.iter()
            .map(|id| ConnectivityNode {
                id: id.to_string(),
                name: id.to_string(),
                voltage_level_id: vl_id.into(),
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

/// Build a `TopologyMapping` that maps each CN to a distinct bus (1-based).
pub fn make_identity_mapping(cn_ids: &[&str]) -> TopologyMapping {
    let mut cn_to_bus = HashMap::new();
    let mut bus_to_cns: HashMap<u32, Vec<String>> = HashMap::new();
    for (i, cn_id) in cn_ids.iter().enumerate() {
        let bus = (i as u32) + 1;
        cn_to_bus.insert(cn_id.to_string(), bus);
        bus_to_cns.entry(bus).or_default().push(cn_id.to_string());
    }
    TopologyMapping {
        connectivity_node_to_bus: cn_to_bus,
        bus_to_connectivity_nodes: bus_to_cns,
        consumed_switch_ids: Vec::new(),
        isolated_connectivity_node_ids: Vec::new(),
    }
}

/// Build a `TopologyMapping` where the given groups of CNs share a bus.
pub fn make_grouped_mapping(groups: &[&[&str]]) -> TopologyMapping {
    let mut cn_to_bus = HashMap::new();
    let mut bus_to_cns: HashMap<u32, Vec<String>> = HashMap::new();
    for (i, group) in groups.iter().enumerate() {
        let bus = (i as u32) + 1;
        for cn_id in *group {
            cn_to_bus.insert(cn_id.to_string(), bus);
            bus_to_cns.entry(bus).or_default().push(cn_id.to_string());
        }
    }
    TopologyMapping {
        connectivity_node_to_bus: cn_to_bus,
        bus_to_connectivity_nodes: bus_to_cns,
        consumed_switch_ids: Vec::new(),
        isolated_connectivity_node_ids: Vec::new(),
    }
}
