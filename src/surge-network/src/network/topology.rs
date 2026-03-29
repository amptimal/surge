// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Physical node-breaker topology model for transmission networks.
//!
//! Captures the IEC 61970 (CIM) hierarchy:
//! **Substation → VoltageLevel → Bay → ConnectivityNode**
//!
//! This model lives alongside [`crate::Network`] (which is bus-branch).
//! Solvers never see `NodeBreakerTopology` directly — it is reduced to bus-branch
//! by the topology engine in `surge-topology`.  After solving, results are
//! mapped back to physical elements via [`TopologyMapping`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level container
// ---------------------------------------------------------------------------

/// Physical node-breaker topology model.
///
/// When present on [`crate::Network::topology`], the network was
/// imported from a node-breaker source (CGMES, XIIDM node-breaker).  The model
/// retains the full physical hierarchy and the mapping from connectivity nodes
/// to bus-branch buses produced by topology mapping.
///
/// When absent, the network is purely bus-branch (MATPOWER, PSS/E, etc.) and
/// all existing workflows are unaffected.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeBreakerTopology {
    /// Physical substations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substations: Vec<Substation>,

    /// Voltage levels within substations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voltage_levels: Vec<VoltageLevel>,

    /// Equipment bays within voltage levels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bays: Vec<Bay>,

    /// Physical junction points (connectivity nodes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connectivity_nodes: Vec<ConnectivityNode>,

    /// Physical busbars.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub busbar_sections: Vec<BusbarSection>,

    /// Switching devices (breakers, disconnectors, fuses, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switches: Vec<SwitchDevice>,

    /// Equipment terminal ↔ connectivity-node associations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal_connections: Vec<TerminalConnection>,

    /// Reduction produced by the last topology rebuild (connectivity node → bus).
    /// `None` until topology mapping has been computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mapping: Option<TopologyMapping>,

    /// Whether the retained topology mapping is stale relative to the current
    /// switch states.
    ///
    /// The previous mapping is intentionally kept when switches change so the
    /// topology engine can reassign existing bus-branch equipment safely during
    /// `rebuild_topology()`. User-facing lookup helpers treat stale reductions as
    /// unavailable until a fresh reduction is performed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    mapping_stale: bool,
}

/// Freshness state for the retained topology mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TopologyMappingState {
    Missing,
    Current,
    Stale,
}

impl NodeBreakerTopology {
    /// Build a retained physical topology with no reduction installed yet.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        substations: Vec<Substation>,
        voltage_levels: Vec<VoltageLevel>,
        bays: Vec<Bay>,
        connectivity_nodes: Vec<ConnectivityNode>,
        busbar_sections: Vec<BusbarSection>,
        switches: Vec<SwitchDevice>,
        terminal_connections: Vec<TerminalConnection>,
    ) -> Self {
        Self {
            substations,
            voltage_levels,
            bays,
            connectivity_nodes,
            busbar_sections,
            switches,
            terminal_connections,
            mapping: None,
            mapping_stale: false,
        }
    }

    /// Attach a fresh topology mapping and return the updated topology.
    pub fn with_mapping(mut self, reduction: TopologyMapping) -> Self {
        self.install_mapping(reduction);
        self
    }

    /// Replace the retained mapping with a fresh one.
    #[doc(hidden)]
    pub fn install_mapping(&mut self, reduction: TopologyMapping) {
        self.mapping = Some(reduction);
        self.mapping_stale = false;
    }

    /// Remove any retained reduction and reset freshness state.
    #[doc(hidden)]
    pub fn clear_mapping(&mut self) {
        self.mapping = None;
        self.mapping_stale = false;
    }

    /// Access the retained mapping even if it is stale.
    #[doc(hidden)]
    pub fn retained_mapping(&self) -> Option<&TopologyMapping> {
        self.mapping.as_ref()
    }

    /// Set a switch to open (`true`) or closed (`false`).
    ///
    /// Returns `true` if the switch was found and its state changed,
    /// `false` if the switch was not found or state was already equal.
    pub fn set_switch_state(&mut self, switch_id: &str, open: bool) -> bool {
        if let Some(sw) = self.switches.iter_mut().find(|s| s.id == switch_id)
            && sw.open != open
        {
            sw.open = open;
            // Retain the previous mapping so rebuild_topology() can safely remap
            // existing equipment, but mark it stale for user-facing lookups.
            self.mapping_stale = true;
            return true;
        }
        false
    }

    /// Whether the currently stored topology mapping is fresh.
    pub fn is_current(&self) -> bool {
        self.mapping.is_some() && !self.mapping_stale
    }

    /// Freshness state for the retained topology mapping.
    pub fn status(&self) -> TopologyMappingState {
        match (self.mapping.is_some(), self.mapping_stale) {
            (false, _) => TopologyMappingState::Missing,
            (true, false) => TopologyMappingState::Current,
            (true, true) => TopologyMappingState::Stale,
        }
    }

    /// The current topology mapping, if one is available and fresh.
    pub fn current_mapping(&self) -> Option<&TopologyMapping> {
        self.mapping.as_ref().filter(|_| self.is_current())
    }

    /// Query the current open/closed state of a switch.
    ///
    /// Returns `Some(true)` if open, `Some(false)` if closed, `None` if not found.
    pub fn switch_state(&self, switch_id: &str) -> Option<bool> {
        self.switches
            .iter()
            .find(|s| s.id == switch_id)
            .map(|s| s.open)
    }

    /// Return all switches of a given type.
    pub fn switches_of_kind(&self, sw_type: SwitchType) -> Vec<&SwitchDevice> {
        self.switches
            .iter()
            .filter(|s| s.switch_type == sw_type)
            .collect()
    }

    /// Look up which bus a connectivity node is currently mapped to.
    ///
    /// Returns `None` if there is no current topology mapping or the node is not
    /// mapped.
    pub fn bus_for_connectivity_node(&self, cn_id: &str) -> Option<u32> {
        self.current_mapping()
            .and_then(|m| m.connectivity_node_to_bus.get(cn_id).copied())
    }

    /// Look up which connectivity nodes were merged into a given bus.
    ///
    /// Returns `None` if there is no current topology mapping or the bus is not
    /// found.
    pub fn connectivity_nodes_for_bus(&self, bus_num: u32) -> Option<&Vec<String>> {
        self.current_mapping()
            .and_then(|m| m.bus_to_connectivity_nodes.get(&bus_num))
    }
}

// ---------------------------------------------------------------------------
// Hierarchy elements
// ---------------------------------------------------------------------------

/// A physical substation (CIM `Substation`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Substation {
    /// Unique identifier (CIM mRID).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Parent sub-geographical region mRID (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

/// A voltage level within a substation (CIM `VoltageLevel`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoltageLevel {
    /// Unique identifier (CIM mRID).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Parent substation mRID.
    pub substation_id: String,
    /// Nominal base voltage in kV.
    pub base_kv: f64,
}

/// An equipment bay within a voltage level (CIM `Bay`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bay {
    /// Unique identifier (CIM mRID).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Parent voltage-level mRID.
    pub voltage_level_id: String,
}

/// A physical junction point in the substation (CIM `ConnectivityNode`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectivityNode {
    /// Unique identifier (CIM mRID).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Parent voltage-level mRID (via `ConnectivityNodeContainer`).
    pub voltage_level_id: String,
}

/// A physical busbar section (CIM `BusbarSection`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusbarSection {
    /// Unique identifier (CIM mRID).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// The connectivity node this busbar is connected to.
    pub connectivity_node_id: String,
    /// Rated peak withstand current (kA), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_max: Option<f64>,
}

// ---------------------------------------------------------------------------
// Switching devices
// ---------------------------------------------------------------------------

/// Classification of a switching device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SwitchType {
    Breaker,
    Disconnector,
    LoadBreakSwitch,
    Fuse,
    GroundDisconnector,
    /// Generic CIM `Switch` (unspecified subtype).
    Switch,
}

/// A switching device connecting two connectivity nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchDevice {
    /// Unique identifier (CIM mRID).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Device classification.
    pub switch_type: SwitchType,
    /// "From" connectivity-node mRID.
    pub cn1_id: String,
    /// "To" connectivity-node mRID.
    pub cn2_id: String,
    /// `true` = open (no current flow), `false` = closed.
    pub open: bool,
    /// Normal (design) open state from the EQ profile.
    pub normal_open: bool,
    /// Whether this switch is "retained" — i.e. it defines a topology boundary
    /// even when closed (CIM `Switch.retained`).
    #[serde(default)]
    pub retained: bool,
    /// Rated continuous current in amperes, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rated_current: Option<f64>,
}

// ---------------------------------------------------------------------------
// Terminal connections
// ---------------------------------------------------------------------------

/// An equipment terminal's connection to a connectivity node.
///
/// This captures the CIM `Terminal → ConnectivityNode` association so that
/// equipment can be resolved to buses through the topology mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConnection {
    /// CIM Terminal mRID.
    pub terminal_id: String,
    /// CIM ConductingEquipment mRID.
    pub equipment_id: String,
    /// CIM class name (e.g. `"ACLineSegment"`, `"PowerTransformer"`).
    pub equipment_class: String,
    /// Terminal sequence number (1-based, as in CIM).
    pub sequence_number: u32,
    /// The connectivity node this terminal connects to.
    pub connectivity_node_id: String,
}

// ---------------------------------------------------------------------------
// Topology reduction (output of reduction)
// ---------------------------------------------------------------------------

/// The result of reducing a node-breaker model to bus-branch.
///
/// Maps connectivity nodes to bus numbers and vice versa, tracking which
/// switches were "consumed" (closed, merging their CNs) and which CNs ended
/// up isolated in the current topology mapping.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TopologyMapping {
    /// Connectivity-node mRID → bus number in the reduced `Network`.
    pub connectivity_node_to_bus: HashMap<String, u32>,

    /// Bus number → list of CN mRIDs that merged into this bus.
    pub bus_to_connectivity_nodes: HashMap<u32, Vec<String>>,

    /// Switch mRIDs that were consumed (closed, their two CNs share a bus).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumed_switch_ids: Vec<String>,

    /// CN mRIDs that are electrically isolated (no energized equipment path).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub isolated_connectivity_node_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substation_topology_default_is_empty() {
        let sm = NodeBreakerTopology::default();
        assert!(sm.substations.is_empty());
        assert!(sm.switches.is_empty());
        assert!(sm.retained_mapping().is_none());
        assert_eq!(sm.status(), TopologyMappingState::Missing);
    }

    #[test]
    fn set_switch_state_toggle() {
        let mut sm = NodeBreakerTopology::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![SwitchDevice {
                id: "BRK_1".into(),
                name: "Breaker 1".into(),
                switch_type: SwitchType::Breaker,
                cn1_id: "CN_A".into(),
                cn2_id: "CN_B".into(),
                open: false,
                normal_open: false,
                retained: false,
                rated_current: None,
            }],
            Vec::new(),
        )
        .with_mapping(TopologyMapping::default());

        // Open the breaker.
        assert!(sm.set_switch_state("BRK_1", true));
        assert_eq!(sm.switch_state("BRK_1"), Some(true));
        // Previous mapping is retained for retopology, but hidden from lookups.
        assert!(sm.retained_mapping().is_some());
        assert_eq!(sm.status(), TopologyMappingState::Stale);
        assert_eq!(sm.bus_for_connectivity_node("CN_A"), None);

        // No-op (already open).
        assert!(!sm.set_switch_state("BRK_1", true));

        // Unknown switch.
        assert!(!sm.set_switch_state("BRK_UNKNOWN", false));
    }

    #[test]
    fn switches_of_type_filter() {
        let sm = NodeBreakerTopology::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![
                SwitchDevice {
                    id: "BRK_1".into(),
                    name: "B1".into(),
                    switch_type: SwitchType::Breaker,
                    cn1_id: "A".into(),
                    cn2_id: "B".into(),
                    open: false,
                    normal_open: false,
                    retained: false,
                    rated_current: None,
                },
                SwitchDevice {
                    id: "DIS_1".into(),
                    name: "D1".into(),
                    switch_type: SwitchType::Disconnector,
                    cn1_id: "B".into(),
                    cn2_id: "C".into(),
                    open: false,
                    normal_open: false,
                    retained: false,
                    rated_current: None,
                },
            ],
            Vec::new(),
        );

        assert_eq!(sm.switches_of_kind(SwitchType::Breaker).len(), 1);
        assert_eq!(sm.switches_of_kind(SwitchType::Disconnector).len(), 1);
        assert_eq!(sm.switches_of_kind(SwitchType::Fuse).len(), 0);
    }

    #[test]
    fn serde_roundtrip() {
        let sm = NodeBreakerTopology::new(
            vec![Substation {
                id: "SUB_1".into(),
                name: "Station Alpha".into(),
                region: Some("RGN_1".into()),
            }],
            vec![VoltageLevel {
                id: "VL_220".into(),
                name: "220 kV".into(),
                substation_id: "SUB_1".into(),
                base_kv: 220.0,
            }],
            Vec::new(),
            vec![ConnectivityNode {
                id: "CN_A".into(),
                name: "Node A".into(),
                voltage_level_id: "VL_220".into(),
            }],
            Vec::new(),
            vec![SwitchDevice {
                id: "BRK_1".into(),
                name: "Breaker 1".into(),
                switch_type: SwitchType::Breaker,
                cn1_id: "CN_A".into(),
                cn2_id: "CN_B".into(),
                open: false,
                normal_open: false,
                retained: false,
                rated_current: Some(2000.0),
            }],
            Vec::new(),
        )
        .with_mapping(TopologyMapping {
            connectivity_node_to_bus: [("CN_A".into(), 1), ("CN_B".into(), 1)]
                .into_iter()
                .collect(),
            bus_to_connectivity_nodes: [(1, vec!["CN_A".into(), "CN_B".into()])]
                .into_iter()
                .collect(),
            consumed_switch_ids: vec!["BRK_1".into()],
            isolated_connectivity_node_ids: vec![],
        });

        let json = serde_json::to_string(&sm).unwrap();
        let deser: NodeBreakerTopology = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.substations.len(), 1);
        assert_eq!(deser.switches.len(), 1);
        assert_eq!(deser.switches[0].switch_type, SwitchType::Breaker);
        assert_eq!(deser.bus_for_connectivity_node("CN_A"), Some(1));
        assert_eq!(deser.connectivity_nodes_for_bus(1).unwrap().len(), 2);
        assert!(deser.is_current());
        assert_eq!(deser.status(), TopologyMappingState::Current);
    }

    #[test]
    fn network_serde_without_substation_topology() {
        // Existing JSON without topology field should deserialize fine.
        let json = r#"{"name":"test","base_mva":100.0,"buses":[],"branches":[],"generators":[],"loads":[]}"#;
        let net: crate::Network = serde_json::from_str(json).unwrap();
        assert!(net.topology.is_none());
    }
}
