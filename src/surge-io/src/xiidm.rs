// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! XIIDM — PowSyBl's native XML interchange format.
//!
//! Supports reading and writing XIIDM/IIDM files, enabling lossless round-trips
//! through PowSyBl and access to PowSyBl's test case library.
//!
//! XIIDM supports two topology models:
//! - **BUS_BREAKER**: each bus is wrapped in a voltageLevel inside a substation.
//! - **NODE_BREAKER**: busbars + switches + connectivity nodes define topology;
//!   buses are computed via Union-Find reduction.
//!
//! ## Unit conventions
//! XIIDM stores impedances in SI: r/x in Ohms, b/g in Siemens, voltages in kV.
//! This module converts to/from MATPOWER per-unit (base_mva = 100 MVA) on read/write.

use std::collections::HashMap;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use surge_network::Network;
use surge_network::network::topology::{BusbarSection, ConnectivityNode, TerminalConnection};
use surge_network::network::{
    Branch, BranchType, Bus, BusType, FixedShunt, Generator, Load, NodeBreakerTopology, ShuntType,
    SwitchDevice, SwitchType,
};
use thiserror::Error;

// Deferred equipment for NODE_BREAKER resolution (bus number unknown during parse).
struct DeferredGen {
    cn_id: String,
    generator: Generator,
    vreg_on: bool,
}
struct DeferredLoad {
    id: String,
    cn_id: String,
    p0: f64,
    q0: f64,
}
struct DeferredBranch {
    cn1_id: String,
    cn2_id: String,
    branch: Branch,
}
struct DeferredShunt {
    id: String,
    cn_id: String,
    shunt_susceptance_mvar: f64,
    shunt_conductance_mw: f64,
}

/// Ensure a connectivity node exists in the accumulator.
fn ensure_cn(
    cns: &mut Vec<ConnectivityNode>,
    seen: &mut HashMap<String, bool>,
    cn_id: &str,
    vl_id: &str,
) {
    if seen.contains_key(cn_id) {
        return;
    }
    seen.insert(cn_id.to_string(), true);
    cns.push(ConnectivityNode {
        id: cn_id.to_string(),
        name: cn_id.to_string(),
        voltage_level_id: vl_id.to_string(),
    });
}

/// Extract the integer node index from a CN ID like "VL1_N3" → "3".
fn extract_node_from_cn_id(cn_id: &str) -> &str {
    cn_id.rsplit_once("_N").map(|(_, n)| n).unwrap_or("0")
}

fn missing_attr(element: &str, attr: &str) -> Error {
    Error::MissingAttr {
        element: element.to_string(),
        attr: attr.to_string(),
    }
}

fn invalid_bus_ref(attr: &str, value: &str) -> Error {
    Error::InvalidValue {
        attr: attr.to_string(),
        value: value.to_string(),
    }
}

fn invalid_numeric_attr(element: &str, attr: &str, value: &str) -> Error {
    Error::InvalidValue {
        attr: format!("{element}.{attr}"),
        value: value.to_string(),
    }
}

fn parse_f64_attr(
    attrs: &HashMap<String, String>,
    element: &str,
    attr: &str,
) -> Result<Option<f64>, Error> {
    match attrs.get(attr) {
        Some(value) => value
            .parse::<f64>()
            .map(Some)
            .map_err(|_| invalid_numeric_attr(element, attr, value)),
        None => Ok(None),
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("XML parse error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("missing attribute '{attr}' on <{element}>")]
    MissingAttr { element: String, attr: String },
    #[error("invalid value for '{attr}': {value}")]
    InvalidValue { attr: String, value: String },
    #[error("topology error: {0}")]
    Topology(#[from] surge_topology::TopologyError),
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Load a XIIDM file from disk.
pub fn load(path: impl AsRef<Path>) -> Result<Network, Error> {
    parse_file(path.as_ref())
}

/// Load a XIIDM case from an in-memory string.
pub fn loads(content: &str) -> Result<Network, Error> {
    parse_str(content)
}

/// Save a XIIDM case to disk.
pub fn save(network: &Network, path: impl AsRef<Path>) -> Result<(), Error> {
    write_file(network, path.as_ref())
}

/// Serialize a XIIDM case to an in-memory string.
pub fn dumps(network: &Network) -> Result<String, Error> {
    to_string(network)
}

fn parse_file(path: &Path) -> Result<Network, Error> {
    let content = std::fs::read_to_string(path)?;
    parse_str(&content)
}

fn parse_str(content: &str) -> Result<Network, Error> {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);

    let mut network = Network::new("unknown");
    // Map from XIIDM bus id (e.g. "B1") to external bus number
    let mut bus_id_to_num: HashMap<String, u32> = HashMap::new();
    // Map from voltageLevel id → nominalV (kV), used for SI→pu conversion
    let mut vl_nom_v: HashMap<String, f64> = HashMap::new();
    // Current voltageLevel nominal voltage (kV)
    let mut current_nom_v: f64 = 1.0;
    // Track internal bus number counter for when we can't extract from id
    let mut next_bus_num: u32 = 1;
    // Index of the last twoWindingsTransformer branch pushed; used to apply
    // phaseTapChanger/step alpha back to branch.phase_shift_rad (MATPOWER convention).
    let mut last_xfmr_branch_idx: Option<usize> = None;
    // Bus number for the shunt element currently being parsed (set on "shunt",
    // consumed by "shuntLinearModel").
    let mut current_shunt_bus: Option<u32> = None;
    // NODE_BREAKER: CN ID for shunt (when bus_num == u32::MAX sentinel)
    let mut current_shunt_cn: Option<String> = None;
    let mut current_shunt_id: Option<String> = None;

    // NODE_BREAKER state --------------------------------------------------
    let mut current_sub_id: Option<String> = None;
    let mut current_vl_id: Option<String> = None;
    let mut current_topo_kind = String::new(); // "BUS_BREAKER" or "NODE_BREAKER"
    let mut has_node_breaker = false;

    // NodeBreakerTopology accumulators
    let mut sm_subs: Vec<surge_network::network::topology::Substation> = Vec::new();
    let mut sm_vls: Vec<surge_network::network::topology::VoltageLevel> = Vec::new();
    let mut sm_cns: Vec<ConnectivityNode> = Vec::new();
    let mut sm_bbs: Vec<BusbarSection> = Vec::new();
    let mut sm_switches: Vec<SwitchDevice> = Vec::new();
    let mut sm_terminals: Vec<TerminalConnection> = Vec::new();
    let mut cn_seen: HashMap<String, bool> = HashMap::new();

    // Pre-computed solved state from busbarSection v/angle or <bus nodes="">
    let mut nb_bus_solved: HashMap<String, (f64, f64)> = HashMap::new();

    // Deferred equipment (resolved after topology reduction)
    let mut deferred_gens: Vec<DeferredGen> = Vec::new();
    let mut deferred_loads: Vec<DeferredLoad> = Vec::new();
    let mut deferred_branches: Vec<DeferredBranch> = Vec::new();
    let mut deferred_shunts: Vec<DeferredShunt> = Vec::new();

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                // Strip namespace prefix for matching
                let local = local_name(e.name().as_ref());
                let attrs = collect_attrs(e)?;

                match local.as_str() {
                    "network" => {
                        if let Some(id) = attrs.get("id") {
                            network.name = id.clone();
                        }
                        // baseMVA not in XIIDM standard; default 100
                        if let Some(v) = attrs.get("baseMVA")
                            && let Ok(f) = v.parse::<f64>()
                        {
                            network.base_mva = f;
                        }
                    }
                    "substation" => {
                        let sub_id = attrs.get("id").cloned().unwrap_or_default();
                        current_sub_id = Some(sub_id.clone());
                        sm_subs.push(surge_network::network::topology::Substation {
                            id: sub_id,
                            name: attrs.get("name").cloned().unwrap_or_default(),
                            region: attrs.get("country").cloned(),
                        });
                    }
                    "voltageLevel" => {
                        if let Some(v) = parse_f64_attr(&attrs, "voltageLevel", "nominalV")? {
                            current_nom_v = v;
                        }
                        let vl_id = attrs.get("id").cloned().unwrap_or_default();
                        vl_nom_v.insert(vl_id.clone(), current_nom_v);
                        current_vl_id = Some(vl_id.clone());
                        current_topo_kind = attrs
                            .get("topologyKind")
                            .cloned()
                            .unwrap_or_else(|| "BUS_BREAKER".into());
                        if current_topo_kind == "NODE_BREAKER" {
                            has_node_breaker = true;
                        }
                        sm_vls.push(surge_network::network::topology::VoltageLevel {
                            id: vl_id,
                            name: attrs.get("name").cloned().unwrap_or_default(),
                            substation_id: current_sub_id.clone().unwrap_or_default(),
                            base_kv: current_nom_v,
                        });
                    }
                    "bus" => {
                        // NODE_BREAKER: <bus nodes="0,2,4" v="402" angle="0.5"/>
                        if current_topo_kind == "NODE_BREAKER" {
                            if let Some(nodes_str) = attrs.get("nodes") {
                                let v_kv = attrs
                                    .get("v")
                                    .map(|_| parse_f64_attr(&attrs, "bus", "v"))
                                    .transpose()?
                                    .flatten()
                                    .unwrap_or(current_nom_v);
                                let angle = attrs
                                    .get("angle")
                                    .map(|_| parse_f64_attr(&attrs, "bus", "angle"))
                                    .transpose()?
                                    .flatten()
                                    .unwrap_or(0.0);
                                let vl_id = current_vl_id.clone().unwrap_or_default();
                                for node in nodes_str.split(',') {
                                    let node = node.trim();
                                    let cn_id = format!("{vl_id}_N{node}");
                                    nb_bus_solved.insert(cn_id, (v_kv, angle));
                                }
                            }
                            // Don't fall through to BUS_BREAKER bus creation
                        } else {
                            // Inside busBreakerTopology
                            let bus_xml_id = attrs.get("id").cloned().unwrap_or_default();
                            let bus_num = extract_bus_number(&bus_xml_id).unwrap_or_else(|| {
                                let n = next_bus_num;
                                next_bus_num += 1;
                                n
                            });
                            if next_bus_num <= bus_num {
                                next_bus_num = bus_num + 1;
                            }

                            let v_kv = attrs
                                .get("v")
                                .map(|_| parse_f64_attr(&attrs, "bus", "v"))
                                .transpose()?
                                .flatten()
                                .unwrap_or(current_nom_v);
                            let angle_deg = attrs
                                .get("angle")
                                .map(|_| parse_f64_attr(&attrs, "bus", "angle"))
                                .transpose()?
                                .flatten()
                                .unwrap_or(0.0);
                            let vm_pu = if current_nom_v > 0.0 {
                                v_kv / current_nom_v
                            } else {
                                1.0
                            };
                            let va_rad = angle_deg.to_radians();

                            let mut bus = Bus::new(bus_num, BusType::PQ, current_nom_v);
                            bus.voltage_magnitude_pu = vm_pu;
                            bus.voltage_angle_rad = va_rad;
                            bus_id_to_num.insert(bus_xml_id, bus_num);
                            network.buses.push(bus);
                        } // end else (BUS_BREAKER)
                    }
                    "generator" => {
                        // Parse generator parameters first (shared by both paths)
                        let pg = attrs
                            .get("targetP")
                            .map(|_| parse_f64_attr(&attrs, "generator", "targetP"))
                            .transpose()?
                            .flatten()
                            .unwrap_or(0.0);
                        let target_v = attrs
                            .get("targetV")
                            .map(|_| parse_f64_attr(&attrs, "generator", "targetV"))
                            .transpose()?
                            .flatten();
                        let vs = if let Some(tv) = target_v {
                            if current_nom_v > 0.0 {
                                tv / current_nom_v
                            } else {
                                tv
                            }
                        } else {
                            1.0
                        };
                        let pmax = attrs
                            .get("maxP")
                            .map(|_| parse_f64_attr(&attrs, "generator", "maxP"))
                            .transpose()?
                            .flatten()
                            .unwrap_or(f64::MAX);
                        let pmin = attrs
                            .get("minP")
                            .map(|_| parse_f64_attr(&attrs, "generator", "minP"))
                            .transpose()?
                            .flatten()
                            .unwrap_or(0.0);
                        let vreg_on = attrs
                            .get("voltageRegulatorOn")
                            .map(|s| s == "true")
                            .unwrap_or(false);

                        if current_topo_kind == "NODE_BREAKER" {
                            let Some(node_str) = attrs.get("node") else {
                                continue;
                            };
                            let vl_id = current_vl_id.clone().unwrap_or_default();
                            let cn_id = format!("{vl_id}_N{node_str}");
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn_id, &vl_id);
                            let eq_id = attrs.get("id").cloned().unwrap_or_default();
                            sm_terminals.push(TerminalConnection {
                                terminal_id: format!("{eq_id}_T1"),
                                equipment_id: eq_id,
                                equipment_class: "Generator".into(),
                                sequence_number: 1,
                                connectivity_node_id: cn_id.clone(),
                            });
                            let mut g = Generator::new(0, pg, vs);
                            g.machine_id = Some(attrs.get("id").cloned().unwrap_or_default());
                            g.pmax = pmax;
                            g.pmin = pmin;
                            deferred_gens.push(DeferredGen {
                                cn_id,
                                generator: g,
                                vreg_on,
                            });
                        } else {
                            let bus_xml_id = attrs
                                .get("bus")
                                .cloned()
                                .ok_or_else(|| missing_attr("generator", "bus"))?;
                            let bus_num = bus_id_to_num
                                .get(&bus_xml_id)
                                .copied()
                                .ok_or_else(|| invalid_bus_ref("bus", &bus_xml_id))?;
                            let mut g = Generator::new(bus_num, pg, vs);
                            g.pmax = pmax;
                            g.pmin = pmin;
                            if vreg_on
                                && let Some(bus) =
                                    network.buses.iter_mut().find(|b| b.number == bus_num)
                                && bus.bus_type == BusType::PQ
                            {
                                bus.bus_type = BusType::PV;
                            }
                            network.generators.push(g);
                        }
                    }
                    "load" => {
                        let p0 = attrs
                            .get("p0")
                            .map(|_| parse_f64_attr(&attrs, "load", "p0"))
                            .transpose()?
                            .flatten()
                            .unwrap_or(0.0);
                        let q0 = attrs
                            .get("q0")
                            .map(|_| parse_f64_attr(&attrs, "load", "q0"))
                            .transpose()?
                            .flatten()
                            .unwrap_or(0.0);

                        if current_topo_kind == "NODE_BREAKER" {
                            let Some(node_str) = attrs.get("node") else {
                                continue;
                            };
                            let vl_id = current_vl_id.clone().unwrap_or_default();
                            let cn_id = format!("{vl_id}_N{node_str}");
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn_id, &vl_id);
                            let eq_id = attrs.get("id").cloned().unwrap_or_default();
                            sm_terminals.push(TerminalConnection {
                                terminal_id: format!("{eq_id}_T1"),
                                equipment_id: eq_id,
                                equipment_class: "Load".into(),
                                sequence_number: 1,
                                connectivity_node_id: cn_id.clone(),
                            });
                            deferred_loads.push(DeferredLoad {
                                id: attrs.get("id").cloned().unwrap_or_default(),
                                cn_id,
                                p0,
                                q0,
                            });
                        } else {
                            let bus_xml_id = attrs
                                .get("bus")
                                .cloned()
                                .ok_or_else(|| missing_attr("load", "bus"))?;
                            let bus_num = bus_id_to_num
                                .get(&bus_xml_id)
                                .copied()
                                .ok_or_else(|| invalid_bus_ref("bus", &bus_xml_id))?;
                            let load_id = attrs.get("id").cloned().unwrap_or_default();
                            network.loads.push(Load {
                                bus: bus_num,
                                id: load_id,
                                active_power_demand_mw: p0,
                                reactive_power_demand_mvar: q0,
                                in_service: true,
                                ..Load::new(0, 0.0, 0.0)
                            });
                        }
                    }
                    "line" | "danglingLine" => {
                        // Parse impedance values (shared by both paths)
                        let vl1_id = attrs.get("voltageLevelId1").cloned().unwrap_or_default();
                        let vl1_kv = vl_nom_v.get(&vl1_id).copied().unwrap_or(1.0);
                        let base_kv_line = if vl1_kv > 0.0 { vl1_kv } else { 1.0 };
                        let z_base = if base_kv_line > 0.0 && network.base_mva > 0.0 {
                            base_kv_line * base_kv_line / network.base_mva
                        } else {
                            1.0
                        };
                        let r_raw = parse_f64_attr(&attrs, local.as_str(), "r")?.unwrap_or(0.0);
                        let x_raw = parse_f64_attr(&attrs, local.as_str(), "x")?.unwrap_or(0.001);
                        let b1_raw = parse_f64_attr(&attrs, local.as_str(), "b1")?.unwrap_or(0.0);
                        let b2_raw = parse_f64_attr(&attrs, local.as_str(), "b2")?.unwrap_or(0.0);
                        let r = r_raw / z_base;
                        let x = x_raw / z_base;
                        let b = (b1_raw + b2_raw) * z_base;
                        let rate_a = parse_f64_attr(&attrs, local.as_str(), "permanentLimit")?
                            .unwrap_or(0.0);

                        // NODE_BREAKER: node1/node2 + voltageLevelId1/Id2
                        if attrs.contains_key("node1") && attrs.contains_key("node2") {
                            let n1 = attrs.get("node1").cloned().unwrap_or_default();
                            let n2 = attrs.get("node2").cloned().unwrap_or_default();
                            let vl2_id = attrs.get("voltageLevelId2").cloned().unwrap_or_default();
                            let cn1 = format!("{vl1_id}_N{n1}");
                            let cn2 = format!("{vl2_id}_N{n2}");
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn1, &vl1_id);
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn2, &vl2_id);
                            let eq_id = attrs.get("id").cloned().unwrap_or_default();
                            sm_terminals.push(TerminalConnection {
                                terminal_id: format!("{eq_id}_T1"),
                                equipment_id: eq_id.clone(),
                                equipment_class: "ACLineSegment".into(),
                                sequence_number: 1,
                                connectivity_node_id: cn1.clone(),
                            });
                            sm_terminals.push(TerminalConnection {
                                terminal_id: format!("{eq_id}_T2"),
                                equipment_id: eq_id,
                                equipment_class: "ACLineSegment".into(),
                                sequence_number: 2,
                                connectivity_node_id: cn2.clone(),
                            });
                            let mut br = Branch::new_line(0, 0, r, x, b);
                            br.circuit = attrs.get("id").cloned().unwrap_or_default();
                            br.rating_a_mva = rate_a;
                            deferred_branches.push(DeferredBranch {
                                cn1_id: cn1,
                                cn2_id: cn2,
                                branch: br,
                            });
                        } else {
                            // BUS_BREAKER path
                            let bus1_id = attrs
                                .get("bus1")
                                .cloned()
                                .ok_or_else(|| missing_attr("line", "bus1"))?;
                            let bus2_id = attrs
                                .get("bus2")
                                .cloned()
                                .ok_or_else(|| missing_attr("line", "bus2"))?;
                            let from = bus_id_to_num
                                .get(&bus1_id)
                                .copied()
                                .ok_or_else(|| invalid_bus_ref("bus1", &bus1_id))?;
                            let to = bus_id_to_num
                                .get(&bus2_id)
                                .copied()
                                .ok_or_else(|| invalid_bus_ref("bus2", &bus2_id))?;
                            let mut br = Branch::new_line(from, to, r, x, b);
                            br.circuit = attrs.get("id").cloned().unwrap_or_default();
                            br.rating_a_mva = rate_a;
                            network.branches.push(br);
                        }
                    }
                    "twoWindingsTransformer" => {
                        // Parse impedance values (shared)
                        let vl1_id = attrs.get("voltageLevelId1").cloned().unwrap_or_default();
                        let vl1_kv = vl_nom_v.get(&vl1_id).copied().unwrap_or(1.0);
                        let base_kv_xfmr = if vl1_kv > 0.0 { vl1_kv } else { 1.0 };
                        let z_base = if base_kv_xfmr > 0.0 && network.base_mva > 0.0 {
                            base_kv_xfmr * base_kv_xfmr / network.base_mva
                        } else {
                            1.0
                        };
                        let r_raw =
                            parse_f64_attr(&attrs, "twoWindingsTransformer", "r")?.unwrap_or(0.0);
                        let x_raw =
                            parse_f64_attr(&attrs, "twoWindingsTransformer", "x")?.unwrap_or(0.001);
                        let b_raw =
                            parse_f64_attr(&attrs, "twoWindingsTransformer", "b")?.unwrap_or(0.0);
                        let r = r_raw / z_base;
                        let x = x_raw / z_base;
                        let b = b_raw * z_base;
                        let rated_u1 = attrs
                            .get("ratedU1")
                            .map(|_| parse_f64_attr(&attrs, "twoWindingsTransformer", "ratedU1"))
                            .transpose()?
                            .flatten()
                            .unwrap_or(base_kv_xfmr);
                        let vl2_id = attrs.get("voltageLevelId2").cloned().unwrap_or_default();
                        let vl2_kv = vl_nom_v.get(&vl2_id).copied().unwrap_or(base_kv_xfmr);
                        let rated_u2 = attrs
                            .get("ratedU2")
                            .map(|_| parse_f64_attr(&attrs, "twoWindingsTransformer", "ratedU2"))
                            .transpose()?
                            .flatten()
                            .unwrap_or(vl2_kv);

                        // NODE_BREAKER: node1/node2
                        if attrs.contains_key("node1") && attrs.contains_key("node2") {
                            let n1 = attrs.get("node1").cloned().unwrap_or_default();
                            let n2 = attrs.get("node2").cloned().unwrap_or_default();
                            let cn1 = format!("{vl1_id}_N{n1}");
                            let cn2 = format!("{vl2_id}_N{n2}");
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn1, &vl1_id);
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn2, &vl2_id);
                            let eq_id = attrs.get("id").cloned().unwrap_or_default();
                            sm_terminals.push(TerminalConnection {
                                terminal_id: format!("{eq_id}_T1"),
                                equipment_id: eq_id.clone(),
                                equipment_class: "PowerTransformer".into(),
                                sequence_number: 1,
                                connectivity_node_id: cn1.clone(),
                            });
                            sm_terminals.push(TerminalConnection {
                                terminal_id: format!("{eq_id}_T2"),
                                equipment_id: eq_id,
                                equipment_class: "PowerTransformer".into(),
                                sequence_number: 2,
                                connectivity_node_id: cn2.clone(),
                            });
                            // Compute tap from ratedU values and voltage levels
                            let tap = if rated_u2 != 0.0 && base_kv_xfmr != 0.0 {
                                let nom_ratio = base_kv_xfmr / vl2_kv;
                                (rated_u1 / rated_u2) / nom_ratio
                            } else {
                                1.0
                            };
                            let mut br = Branch::new_line(0, 0, r, x, b);
                            br.circuit = attrs.get("id").cloned().unwrap_or_default();
                            br.tap = tap;
                            br.branch_type = BranchType::Transformer;
                            deferred_branches.push(DeferredBranch {
                                cn1_id: cn1,
                                cn2_id: cn2,
                                branch: br,
                            });
                            // For step/phase tap changer: track index in deferred list
                            last_xfmr_branch_idx = None; // handle via deferred
                        } else {
                            // BUS_BREAKER path
                            let bus1_id = attrs
                                .get("bus1")
                                .cloned()
                                .ok_or_else(|| missing_attr("twoWindingsTransformer", "bus1"))?;
                            let bus2_id = attrs
                                .get("bus2")
                                .cloned()
                                .ok_or_else(|| missing_attr("twoWindingsTransformer", "bus2"))?;
                            let from = bus_id_to_num
                                .get(&bus1_id)
                                .copied()
                                .ok_or_else(|| invalid_bus_ref("bus1", &bus1_id))?;
                            let to = bus_id_to_num
                                .get(&bus2_id)
                                .copied()
                                .ok_or_else(|| invalid_bus_ref("bus2", &bus2_id))?;
                            let tap = if rated_u2 != 0.0 && base_kv_xfmr != 0.0 {
                                let nom_ratio = base_kv_xfmr
                                    / network
                                        .buses
                                        .iter()
                                        .find(|b| b.number == to)
                                        .map(|b| b.base_kv)
                                        .unwrap_or(base_kv_xfmr);
                                (rated_u1 / rated_u2) / nom_ratio
                            } else {
                                1.0
                            };
                            let mut br = Branch::new_line(from, to, r, x, b);
                            br.circuit = attrs.get("id").cloned().unwrap_or_default();
                            br.tap = tap;
                            br.branch_type = BranchType::Transformer;
                            network.branches.push(br);
                            last_xfmr_branch_idx = Some(network.branches.len() - 1);
                        }
                    }
                    "step" => {
                        // phaseTapChanger/step: alpha (degrees) encodes phase shift.
                        // MATPOWER convention: shift = -alpha (see writer for derivation).
                        if let Some(idx) = last_xfmr_branch_idx
                            && let Some(alpha) = parse_f64_attr(&attrs, "step", "alpha")?
                        {
                            network.branches[idx].phase_shift_rad = (-alpha).to_radians();
                        }
                        // ratioTapChanger/step: rho encodes the off-nominal turns ratio
                        // multiplier. Multiply into the branch tap ratio.
                        if let Some(idx) = last_xfmr_branch_idx
                            && let Some(rho) = parse_f64_attr(&attrs, "step", "rho")?
                        {
                            network.branches[idx].tap *= rho;
                        }
                    }
                    "shunt" => {
                        // Record which bus/CN this shunt belongs to; the susceptance
                        // value is in the child <iidm:shuntLinearModel> element.
                        current_shunt_id = attrs.get("id").cloned();
                        if let Some(node_str) = attrs.get("node") {
                            // NODE_BREAKER: store CN ID for shuntLinearModel
                            let vl_id = current_vl_id.clone().unwrap_or_default();
                            let cn_id = format!("{vl_id}_N{node_str}");
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn_id, &vl_id);
                            let shunt_id = current_shunt_id
                                .clone()
                                .unwrap_or_else(|| format!("{vl_id}_SHUNT_{node_str}"));
                            current_shunt_id = Some(shunt_id.clone());
                            sm_terminals.push(TerminalConnection {
                                terminal_id: format!("{shunt_id}_T1"),
                                equipment_id: shunt_id,
                                equipment_class: "ShuntCompensator".into(),
                                connectivity_node_id: cn_id.clone(),
                                sequence_number: 1,
                            });
                            current_shunt_bus = Some(u32::MAX); // sentinel
                            current_shunt_cn = Some(cn_id);
                        } else if let Some(bus_xml_id) = attrs.get("bus").cloned() {
                            current_shunt_bus = Some(
                                bus_id_to_num
                                    .get(&bus_xml_id)
                                    .copied()
                                    .ok_or_else(|| invalid_bus_ref("bus", &bus_xml_id))?,
                            );
                        } else {
                            return Err(missing_attr("shunt", "bus"));
                        }
                    }
                    "shuntLinearModel" => {
                        if let Some(bus_num) = current_shunt_bus {
                            let b_per_s =
                                parse_f64_attr(&attrs, "shuntLinearModel", "bPerSection")?
                                    .unwrap_or(0.0);
                            let g_per_s =
                                parse_f64_attr(&attrs, "shuntLinearModel", "gPerSection")?
                                    .unwrap_or(0.0);
                            if b_per_s.abs() > 0.0 || g_per_s.abs() > 0.0 {
                                let nom_v = current_nom_v;
                                let bs = b_per_s * nom_v * nom_v;
                                let gs = g_per_s * nom_v * nom_v;
                                let shunt_id = current_shunt_id.clone().unwrap_or_else(|| {
                                    format!(
                                        "SHUNT_{}",
                                        network.fixed_shunts.len() + deferred_shunts.len() + 1
                                    )
                                });
                                if bus_num == u32::MAX {
                                    // NODE_BREAKER: defer using current_shunt_cn
                                    if let Some(ref cn) = current_shunt_cn {
                                        deferred_shunts.push(DeferredShunt {
                                            id: shunt_id,
                                            cn_id: cn.clone(),
                                            shunt_susceptance_mvar: bs,
                                            shunt_conductance_mw: gs,
                                        });
                                    }
                                } else if let Some(bus) =
                                    network.buses.iter_mut().find(|b| b.number == bus_num)
                                {
                                    bus.shunt_susceptance_mvar += bs;
                                    bus.shunt_conductance_mw += gs;
                                    network.fixed_shunts.push(FixedShunt {
                                        bus: bus_num,
                                        id: shunt_id,
                                        shunt_type: if bs < 0.0 {
                                            ShuntType::Reactor
                                        } else {
                                            ShuntType::Capacitor
                                        },
                                        g_mw: gs,
                                        b_mvar: bs,
                                        in_service: true,
                                        rated_kv: Some(nom_v),
                                        rated_mvar: Some(bs.abs()),
                                    });
                                }
                            }
                            current_shunt_bus = None;
                            current_shunt_cn = None;
                            current_shunt_id = None;
                        }
                    }
                    // ----- NODE_BREAKER topology elements -----
                    "busbarSection" => {
                        if current_topo_kind == "NODE_BREAKER" {
                            let vl_id = current_vl_id.clone().unwrap_or_default();
                            let node_str = attrs.get("node").cloned().unwrap_or_default();
                            let cn_id = format!("{vl_id}_N{node_str}");
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn_id, &vl_id);

                            let bbs_id = attrs.get("id").cloned().unwrap_or_default();
                            sm_bbs.push(BusbarSection {
                                id: bbs_id,
                                name: attrs.get("name").cloned().unwrap_or_default(),
                                connectivity_node_id: cn_id.clone(),
                                ip_max: None,
                            });

                            // V1_0 files have v/angle on busbarSection directly
                            if let Some(v) = parse_f64_attr(&attrs, "busbarSection", "v")? {
                                let angle = attrs
                                    .get("angle")
                                    .map(|_| parse_f64_attr(&attrs, "busbarSection", "angle"))
                                    .transpose()?
                                    .flatten()
                                    .unwrap_or(0.0);
                                nb_bus_solved.insert(cn_id, (v, angle));
                            }
                        }
                    }
                    "switch" => {
                        if current_topo_kind == "NODE_BREAKER" {
                            let vl_id = current_vl_id.clone().unwrap_or_default();
                            let n1 = attrs.get("node1").cloned().unwrap_or_default();
                            let n2 = attrs.get("node2").cloned().unwrap_or_default();
                            let cn1 = format!("{vl_id}_N{n1}");
                            let cn2 = format!("{vl_id}_N{n2}");
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn1, &vl_id);
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn2, &vl_id);

                            let kind_str = attrs.get("kind").cloned().unwrap_or_default();
                            let switch_type = match kind_str.as_str() {
                                "BREAKER" => SwitchType::Breaker,
                                "DISCONNECTOR" => SwitchType::Disconnector,
                                "LOAD_BREAK_SWITCH" => SwitchType::LoadBreakSwitch,
                                _ => SwitchType::Switch,
                            };
                            let open = attrs.get("open").map(|s| s == "true").unwrap_or(false);
                            let retained =
                                attrs.get("retained").map(|s| s == "true").unwrap_or(false);

                            sm_switches.push(SwitchDevice {
                                id: attrs.get("id").cloned().unwrap_or_default(),
                                name: attrs.get("name").cloned().unwrap_or_default(),
                                switch_type,
                                cn1_id: cn1,
                                cn2_id: cn2,
                                open,
                                normal_open: open,
                                retained,
                                rated_current: None,
                            });
                        }
                    }
                    "internalConnection" => {
                        if current_topo_kind == "NODE_BREAKER" {
                            let vl_id = current_vl_id.clone().unwrap_or_default();
                            let n1 = attrs.get("node1").cloned().unwrap_or_default();
                            let n2 = attrs.get("node2").cloned().unwrap_or_default();
                            let cn1 = format!("{vl_id}_N{n1}");
                            let cn2 = format!("{vl_id}_N{n2}");
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn1, &vl_id);
                            ensure_cn(&mut sm_cns, &mut cn_seen, &cn2, &vl_id);

                            sm_switches.push(SwitchDevice {
                                id: format!("{vl_id}_IC_{n1}_{n2}"),
                                name: "InternalConnection".into(),
                                switch_type: SwitchType::Switch,
                                cn1_id: cn1,
                                cn2_id: cn2,
                                open: false,
                                normal_open: false,
                                retained: false,
                                rated_current: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name(e.name().as_ref());
                match local.as_str() {
                    "substation" => {
                        current_sub_id = None;
                    }
                    "voltageLevel" => {
                        current_vl_id = None;
                        current_topo_kind.clear();
                    }
                    "twoWindingsTransformer" => {
                        last_xfmr_branch_idx = None;
                    }
                    "shunt" => {
                        current_shunt_bus = None;
                        current_shunt_cn = None;
                        current_shunt_id = None;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::Xml(e)),
            _ => {}
        }
        buf.clear();
    }

    // -----------------------------------------------------------------------
    // NODE_BREAKER: topology reduction + deferred equipment resolution
    // -----------------------------------------------------------------------
    if has_node_breaker {
        let sm = NodeBreakerTopology::new(
            sm_subs,
            sm_vls,
            Vec::new(),
            sm_cns,
            sm_bbs,
            sm_switches,
            sm_terminals,
        );

        // Build the initial bus projection for this node-breaker model.
        let built = surge_topology::project_node_breaker_topology(&sm)?;
        let reduced_net = built.network;
        let mapping = built.mapping;

        // Create buses from reduction, applying solved v/angle if available
        for bus in &reduced_net.buses {
            let mut new_bus = bus.clone();
            // Try to apply solved state from busbarSection or <bus> element
            if let Some(cns) = mapping.bus_to_connectivity_nodes.get(&bus.number) {
                for cn in cns {
                    if let Some(&(v_kv, angle)) = nb_bus_solved.get(cn) {
                        if new_bus.base_kv > 0.0 {
                            new_bus.voltage_magnitude_pu = v_kv / new_bus.base_kv;
                        }
                        // XIIDM angle is always in degrees.
                        new_bus.voltage_angle_rad = angle.to_radians();
                        break;
                    }
                }
            }
            network.buses.push(new_bus);
        }

        // Resolve deferred generators
        for dg in deferred_gens {
            if let Some(&bus) = mapping.connectivity_node_to_bus.get(&dg.cn_id) {
                let mut g = dg.generator;
                g.bus = bus;
                if dg.vreg_on
                    && let Some(b) = network.buses.iter_mut().find(|b| b.number == bus)
                    && b.bus_type == BusType::PQ
                {
                    b.bus_type = BusType::PV;
                }
                network.generators.push(g);
            }
        }

        // Resolve deferred loads
        for dl in deferred_loads {
            if let Some(&bus) = mapping.connectivity_node_to_bus.get(&dl.cn_id) {
                if dl.p0.abs() > 1e-10 || dl.q0.abs() > 1e-10 {
                    let mut load = Load::new(bus, dl.p0, dl.q0);
                    load.id = dl.id;
                    network.loads.push(load);
                }
            }
        }

        // Resolve deferred branches
        for db in deferred_branches {
            if let (Some(&from), Some(&to)) = (
                mapping.connectivity_node_to_bus.get(&db.cn1_id),
                mapping.connectivity_node_to_bus.get(&db.cn2_id),
            ) && from != to
            {
                let mut br = db.branch;
                br.from_bus = from;
                br.to_bus = to;
                network.branches.push(br);
            }
        }

        // Resolve deferred shunts
        for ds in deferred_shunts {
            if let Some(&bus) = mapping.connectivity_node_to_bus.get(&ds.cn_id)
                && let Some(b) = network.buses.iter_mut().find(|b| b.number == bus)
            {
                b.shunt_susceptance_mvar += ds.shunt_susceptance_mvar;
                b.shunt_conductance_mw += ds.shunt_conductance_mw;
                network.fixed_shunts.push(FixedShunt {
                    bus,
                    id: ds.id,
                    shunt_type: if ds.shunt_susceptance_mvar < 0.0 {
                        ShuntType::Reactor
                    } else {
                        ShuntType::Capacitor
                    },
                    g_mw: ds.shunt_conductance_mw,
                    b_mvar: ds.shunt_susceptance_mvar,
                    in_service: true,
                    rated_kv: b.base_kv.into(),
                    rated_mvar: Some(ds.shunt_susceptance_mvar.abs()),
                });
            }
        }

        // Attach NodeBreakerTopology with the fresh topology mapping.
        let mut final_sm = sm;
        final_sm.install_mapping(mapping);
        network.topology = Some(final_sm);
    }

    // Set slack bus: first generator's bus becomes slack
    if !network.buses.is_empty() && !network.generators.is_empty() {
        let slack_bus = network.generators[0].bus;
        if let Some(bus) = network.buses.iter_mut().find(|b| b.number == slack_bus)
            && bus.bus_type != BusType::Isolated
        {
            bus.bus_type = BusType::Slack;
        }
    }
    Ok(network)
}

fn local_name(full: &[u8]) -> String {
    // Strip XML namespace prefix (e.g., "iidm:network" → "network")
    let s = std::str::from_utf8(full).unwrap_or("");
    if let Some(colon) = s.rfind(':') {
        s[colon + 1..].to_string()
    } else {
        s.to_string()
    }
}

fn collect_attrs(e: &quick_xml::events::BytesStart<'_>) -> Result<HashMap<String, String>, Error> {
    let mut map = HashMap::new();
    for attr in e.attributes() {
        let a = attr.map_err(|e| Error::Xml(quick_xml::Error::from(e)))?;
        let key = local_name(a.key.as_ref());
        let val = String::from_utf8_lossy(&a.value).to_string();
        map.insert(key, val);
    }
    Ok(map)
}

/// Extract a bus number from an XIIDM bus id like "B1", "VL_1_0", "bus_42", etc.
fn extract_bus_number(id: &str) -> Option<u32> {
    // Try to parse trailing digits
    let digits: String = id
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if !digits.is_empty() {
        let num_str: String = digits.chars().rev().collect();
        num_str.parse().ok()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Write a Network to a XIIDM file on disk.
fn write_file(network: &Network, path: &Path) -> Result<(), Error> {
    let content = to_string(network)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Serialize a Network to a XIIDM XML string.
///
/// Impedances are written in SI units (Ohms / Siemens) as required by the
/// XIIDM format and pypowsybl. All branches (lines and tap-changer branches)
/// are written as `<iidm:line>` elements for simplicity; pypowsybl can run
/// accurate AC/DC power flow on the resulting network.
fn to_string(network: &Network) -> Result<String, Error> {
    use std::fmt::Write;
    let mut out = String::with_capacity(64 * 1024);

    // Precompute per-bus demand from Load objects for export.
    let bus_demand_p = network.bus_load_p_mw();
    let bus_demand_q = network.bus_load_q_mvar();
    let bus_idx_map = network.bus_index_map();

    writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#)
        .expect("writing to String is infallible");
    writeln!(
        out,
        r#"<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_15""#
    )
    .expect("writing to String is infallible");
    writeln!(
        out,
        r#"              id="{}" caseDate="2025-01-01T00:00:00.000+00:00""#,
        xml_escape(&network.name)
    )
    .expect("writing to String is infallible");
    writeln!(
        out,
        r#"              forecastDistance="0" sourceFormat="SURGE" baseMVA="{}" minimumValidationLevel="STEADY_STATE_HYPOTHESIS">"#,
        network.base_mva
    )
    .expect("writing to String is infallible");

    // NODE_BREAKER writer path: when NodeBreakerTopology is present, write the
    // real physical hierarchy instead of synthetic bus-breaker substations.
    if let Some(ref sm) = network.topology {
        write_node_breaker_network(&mut out, network, sm)?;
        writeln!(out, r#"</iidm:network>"#).expect("writing to String is infallible");
        return Ok(out);
    }

    // Group generators by bus
    let mut gen_by_bus: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, g) in network.generators.iter().enumerate() {
        gen_by_bus.entry(g.bus).or_default().push(i);
    }

    // Union-Find: group transformer-connected buses into the same substation.
    // In XIIDM, twoWindingsTransformer must live INSIDE a substation.
    let mut uf_parent: HashMap<u32, u32> =
        network.buses.iter().map(|b| (b.number, b.number)).collect();

    fn uf_find(p: &mut HashMap<u32, u32>, x: u32) -> u32 {
        // iterative path compression (path halving)
        let mut x = x;
        loop {
            let parent = *p.get(&x).unwrap_or(&x);
            if parent == x {
                break;
            }
            // path halving
            let grandparent = *p.get(&parent).unwrap_or(&parent);
            p.insert(x, grandparent);
            x = grandparent;
        }
        x
    }

    // Pre-compute transformer charging shunts (b/2 at each end of transformer branches).
    //
    // MATPOWER uses a symmetric π-model: b/2 at both the from-bus and to-bus.
    // The IIDM twoWindingsTransformer `b` attribute is ONLY placed at side-1 (from-bus).
    // To match MATPOWER exactly we:
    //   1. Write the transformer with b=0 (no magnetizing shunt in the element).
    //   2. Add explicit shunt compensators at both buses for b/2 each.
    //
    // The b/2 in MATPOWER per-unit is the reactive injection at 1.0 pu voltage =
    // (b_pu/2) × base_mva [MVAr]. We store it in the same "Bs MVAr" unit used by bus.shunt_susceptance_mvar,
    // so the existing shunt-writing path can combine it with any bus fixed-shunt.
    let mut xfmr_bus_b_mvar: HashMap<u32, f64> = HashMap::new();

    // bus_map needed here for cross-kV detection in xfmr_indices filter.
    let bus_map: HashMap<u32, &surge_network::network::Bus> =
        network.buses.iter().map(|b| (b.number, b)).collect();

    // Compute effective base_kv for each bus.
    //
    // MATPOWER cases sometimes use base_kv ≤ 1.0 as a placeholder meaning
    // "per-unit only, no physical voltage level specified".  Exporting these
    // as 1.0 kV VoltageLevels in XIIDM/CGMES is semantically misleading and
    // creates cross-kV transformer entries with very small impedance bases
    // (zb = 1.0²/100 = 0.01 Ω) that look unusual to other tools.
    //
    // Fix: for each placeholder bus (base_kv ≤ 1.0) propagate the maximum
    // base_kv of its directly-connected real-kV neighbours.  This makes the
    // bus appear at the dominant voltage level of the surrounding network
    // (e.g. 138 kV instead of 1.0 kV) while leaving the per-unit admittance
    // matrix unchanged (the turns-ratio ρ = ratedU2/ratedU1 × nomV1/nomV2
    // remains 1.0 for all affected branches, so Y_bus is identical).
    let mut effective_kv: HashMap<u32, f64> = network
        .buses
        .iter()
        .map(|b| (b.number, if b.base_kv > 1.0 { b.base_kv } else { 0.0_f64 }))
        .collect();
    for br in &network.branches {
        if !br.in_service {
            continue;
        }
        let kv_f = effective_kv.get(&br.from_bus).copied().unwrap_or(0.0);
        let kv_t = effective_kv.get(&br.to_bus).copied().unwrap_or(0.0);
        if kv_f > 1.0 {
            let e = effective_kv.entry(br.to_bus).or_insert(0.0);
            if kv_f > *e {
                *e = kv_f;
            }
        }
        if kv_t > 1.0 {
            let e = effective_kv.entry(br.from_bus).or_insert(0.0);
            if kv_t > *e {
                *e = kv_t;
            }
        }
    }
    // Any bus still at 0.0 kV (completely isolated placeholder) → keep 1.0 kV.
    for kv in effective_kv.values_mut() {
        if *kv <= 0.0 {
            *kv = 1.0;
        }
    }

    // Transformers written as twoWindingsTransformer inside a substation:
    //   (a) off-nominal tap ratio (|tap - 1| > 1e-6), or
    //   (b) phase shift (|shift| > 1e-6), or
    //   (c) cross-voltage-level connection (|nu1 - nu2| > 0.5 kV).
    //
    // Cross-kV branches with tap≈1.0 must be written as twoWindingsTransformer,
    // not as iidm:line. pypowsybl's DC initializer incorrectly scales the line
    // reactance when the two voltage levels differ (e.g. 154 kV ↔ 6.3 kV gives
    // a 24× error in susceptance), causing cascading failures in the AC solve.
    let xfmr_indices: Vec<usize> = network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| {
            if !br.in_service {
                return false;
            } // skip out-of-service branches
            let tap_diff = (br.tap - 1.0).abs() > 1e-6;
            let shift_nonzero = br.phase_shift_rad.abs() > 1e-6;
            if tap_diff || shift_nonzero {
                return true;
            }
            let nu1 = effective_kv.get(&br.from_bus).copied().unwrap_or(1.0);
            let nu2 = effective_kv.get(&br.to_bus).copied().unwrap_or(1.0);
            (nu1 - nu2).abs() > 0.5 // different voltage class → treat as transformer
        })
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let xfmr_set: std::collections::HashSet<usize> = xfmr_indices.iter().copied().collect();

    for &bi in &xfmr_indices {
        let br = &network.branches[bi];
        let rf = uf_find(&mut uf_parent, br.from_bus);
        let rt = uf_find(&mut uf_parent, br.to_bus);
        if rf != rt {
            uf_parent.insert(rt, rf);
        }
        // Accumulate b/2 × base_mva [MVAr] at each end for the π-model charging.
        if br.b.abs() > 1e-12 {
            let b_half_mvar = (br.b / 2.0) * network.base_mva;
            *xfmr_bus_b_mvar.entry(br.from_bus).or_insert(0.0) += b_half_mvar;
            *xfmr_bus_b_mvar.entry(br.to_bus).or_insert(0.0) += b_half_mvar;
        }
    }

    // Build ordered substation_root -> bus_numbers map
    let mut sub_buses: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut sub_order: Vec<u32> = Vec::new();
    for bus in &network.buses {
        let root = uf_find(&mut uf_parent, bus.number);
        let e = sub_buses.entry(root).or_insert_with(|| {
            sub_order.push(root);
            Vec::new()
        });
        e.push(bus.number);
    }

    // Normalize bus angles relative to the slack bus so that PowSyBl receives
    // the correct voltage-angle reference.  MATPOWER case files store angles
    // from a solved operating point where the slack bus angle may be non-zero
    // (e.g. −49.41° in ACTIVSg10k, −82.2° in ACTIVSg25k).  PowSyBl uses the
    // XIIDM bus `angle` attribute as initialization, so writing the raw
    // case-file angles would push the NR solver far from flat-start.
    // Subtracting the slack Va makes the slack appear at 0° and all other
    // buses at their *relative* angles — the correct reference convention.
    let slack_va_rad = network
        .buses
        .iter()
        .find(|b| b.bus_type == BusType::Slack)
        .map(|b| b.voltage_angle_rad)
        .unwrap_or(0.0);

    // Emit substations
    for sub_root in &sub_order {
        let bus_nums = &sub_buses[sub_root];
        writeln!(
            out,
            r#"  <iidm:substation id="S{}" country="FR">"#,
            sub_root
        )
        .expect("writing to String is infallible");

        for &bnum in bus_nums {
            let bus = bus_map[&bnum];
            let nom_v = effective_kv
                .get(&bnum)
                .copied()
                .unwrap_or(if bus.base_kv > 0.0 { bus.base_kv } else { 1.0 });
            let v_kv = bus.voltage_magnitude_pu * nom_v;
            let angle_deg = (bus.voltage_angle_rad - slack_va_rad).to_degrees();
            writeln!(
                out,
                r#"    <iidm:voltageLevel id="VL{}" nominalV="{}" topologyKind="BUS_BREAKER">"#,
                bnum, nom_v
            )
            .expect("writing to String is infallible");
            writeln!(out, r#"      <iidm:busBreakerTopology>"#)
                .expect("writing to String is infallible");
            writeln!(
                out,
                r#"        <iidm:bus id="B{}" v="{:.6}" angle="{:.6}"/>"#,
                bnum, v_kv, angle_deg
            )
            .expect("writing to String is infallible");
            writeln!(out, r#"      </iidm:busBreakerTopology>"#)
                .expect("writing to String is infallible");

            // Generators
            let is_pv_slack = bus.bus_type == BusType::PV || bus.bus_type == BusType::Slack;
            let has_online_gen = gen_by_bus
                .get(&bnum)
                .map(|idxs| idxs.iter().any(|&gi| network.generators[gi].in_service))
                .unwrap_or(false);

            if let Some(g_indices) = gen_by_bus.get(&bnum) {
                for (j, &gi) in g_indices.iter().enumerate() {
                    let g = &network.generators[gi];
                    if !g.in_service {
                        continue;
                    } // skip offline generators
                    let qmax = if g.qmax.is_finite() { g.qmax } else { 9999.0 };
                    let qmin = if g.qmin.is_finite() { g.qmin } else { -9999.0 };
                    let pmax = if g.pmax.is_finite() { g.pmax } else { 9999.0 };
                    let pmin = if g.pmin.is_finite() { g.pmin } else { -9999.0 };
                    let reg_on = is_pv_slack;
                    let tv_kv = g.voltage_setpoint_pu * nom_v;
                    writeln!(out,
                        r#"      <iidm:generator id="G{}_{}" connectableBus="B{}" bus="B{}" energySource="OTHER" minP="{}" maxP="{}" targetP="{}" targetQ="{:.6}" targetV="{:.6}" voltageRegulatorOn="{}">"#,
                        bnum, j + 1, bnum, bnum, pmin, pmax, g.p, g.q, tv_kv, reg_on).expect("writing to String is infallible");
                    writeln!(
                        out,
                        r#"        <iidm:minMaxReactiveLimits minQ="{}" maxQ="{}"/>"#,
                        qmin, qmax
                    )
                    .expect("writing to String is infallible");
                    writeln!(out, r#"      </iidm:generator>"#)
                        .expect("writing to String is infallible");
                }
            }

            // For PV/Slack buses where ALL generators are offline: write a ghost
            // voltage regulator so pypowsybl treats the bus as voltage-regulated,
            // matching Surge/MATPOWER which keep PV bus type regardless of generator
            // online status and fix Vm to the stored bus voltage.
            if is_pv_slack && !has_online_gen {
                let tv_kv = bus.voltage_magnitude_pu * nom_v;
                writeln!(out,
                    r#"      <iidm:generator id="G{}_ghost" connectableBus="B{}" bus="B{}" energySource="OTHER" minP="0.0" maxP="0.0" targetP="0.0" targetQ="0.0" targetV="{:.6}" voltageRegulatorOn="true">"#,
                    bnum, bnum, bnum, tv_kv).expect("writing to String is infallible");
                writeln!(
                    out,
                    r#"        <iidm:minMaxReactiveLimits minQ="-9999.0" maxQ="9999.0"/>"#
                )
                .expect("writing to String is infallible");
                writeln!(out, r#"      </iidm:generator>"#)
                    .expect("writing to String is infallible");
            }

            // Load
            let bus_idx_val = bus_idx_map.get(&bnum).copied().unwrap_or(0);
            let pd_val = bus_demand_p.get(bus_idx_val).copied().unwrap_or(0.0);
            let qd_val = bus_demand_q.get(bus_idx_val).copied().unwrap_or(0.0);
            if pd_val.abs() > 1e-10 || qd_val.abs() > 1e-10 {
                writeln!(out,
                    r#"      <iidm:load id="L{}" connectableBus="B{}" bus="B{}" loadType="UNDEFINED" p0="{}" q0="{}" p="{}" q="{}"/>"#,
                    bnum, bnum, bnum, pd_val, qd_val, pd_val, qd_val).expect("writing to String is infallible");
            }

            // Shunt compensator: combine fixed bus shunt (Gs/Bs) with transformer π-model
            // charging (b/2 at each transformer end, in the same MVAr units as bus.shunt_susceptance_mvar).
            // MATPOWER Bs > 0 = capacitive (injects reactive power).
            // bPerSection [S] = effective_bs_mvar / nom_v² → Q = V_kV² × b_S [MVAr]
            let extra_bs = xfmr_bus_b_mvar.get(&bnum).copied().unwrap_or(0.0);
            let eff_bs = bus.shunt_susceptance_mvar + extra_bs;
            let eff_gs = bus.shunt_conductance_mw;
            if eff_gs.abs() > 1e-10 || eff_bs.abs() > 1e-10 {
                let b_per_section = eff_bs / (nom_v * nom_v);
                let g_per_section = eff_gs / (nom_v * nom_v);
                writeln!(out,
                    r#"      <iidm:shunt id="SHC_{}" connectableBus="B{}" bus="B{}" sectionCount="1" voltageRegulatorOn="false">"#,
                    bnum, bnum, bnum).expect("writing to String is infallible");
                writeln!(out,
                    r#"        <iidm:shuntLinearModel bPerSection="{:.10}" gPerSection="{:.10}" maximumSectionCount="1"/>"#,
                    b_per_section, g_per_section).expect("writing to String is infallible");
                writeln!(out, r#"      </iidm:shunt>"#).expect("writing to String is infallible");
            }

            writeln!(out, r#"    </iidm:voltageLevel>"#).expect("writing to String is infallible");
        }

        // Transformers inside the substation
        for &bi in &xfmr_indices {
            let br = &network.branches[bi];
            if uf_find(&mut uf_parent, br.from_bus) != *sub_root {
                continue;
            }
            let nu1 = effective_kv.get(&br.from_bus).copied().unwrap_or(1.0);
            let nu2 = effective_kv.get(&br.to_bus).copied().unwrap_or(1.0);
            // Physical impedance referenced to to-bus (side2) voltage base (nu2²/SN).
            // ratedU1 = tap*nu1, ratedU2 = nu2.
            // PowSyBl computes ρ = ratedU2/ratedU1 * nomV1/nomV2
            //                    = nu2/(tap*nu1) * nu1/nu2 = 1/tap.
            // Y11=ρ²*y_pu, Y22=y_pu, Y12=-ρ*y_pu — matching MATPOWER Y_ff=y/tap², Y_tt=y, Y_ft=-y/tap.
            // y_pu = y_SI * nomV2²/SN; with z_SI = z_pu * nu2²/SN → y_pu = z_pu⁻¹ = y_matpower. ✓
            // b=0.0 here: charging susceptance is represented as bus shunts (see above).
            let zb = nu2 * nu2 / network.base_mva;
            let has_phase_shift = br.phase_shift_rad.abs() > 1e-6;
            if has_phase_shift {
                // Phase-shifting transformer: need phaseTapChanger child element.
                // From OpenLoadFlow: Y12 = -rho*exp(-j*alpha)*y_pu.
                // MATPOWER: Y12 = -y/conj(a) = -y/tap * exp(+j*shift).
                // Match requires alpha = -shift (converted to degrees for XIIDM).
                let alpha = -br.phase_shift_rad.to_degrees();
                writeln!(out,
                    r#"    <iidm:twoWindingsTransformer id="T_{}_{}_{}" r="{:.8}" x="{:.8}" b="0.0" g="0.0" ratedU1="{:.4}" ratedU2="{:.4}" voltageLevelId1="VL{}" bus1="B{}" connectableBus1="B{}" voltageLevelId2="VL{}" bus2="B{}" connectableBus2="B{}">"#,
                    br.from_bus, br.to_bus, bi + 1,
                    br.r * zb, br.x * zb,
                    br.tap * nu1, nu2,
                    br.from_bus, br.from_bus, br.from_bus,
                    br.to_bus, br.to_bus, br.to_bus).expect("writing to String is infallible");
                writeln!(out,
                    r#"      <iidm:phaseTapChanger lowTapPosition="0" tapPosition="0" loadTapChangingCapabilities="false"><iidm:step r="0.0" x="0.0" g="0.0" b="0.0" rho="1.0" alpha="{:.6}"/></iidm:phaseTapChanger>"#,
                    alpha).expect("writing to String is infallible");
                writeln!(out, r#"    </iidm:twoWindingsTransformer>"#)
                    .expect("writing to String is infallible");
            } else {
                writeln!(out,
                    r#"    <iidm:twoWindingsTransformer id="T_{}_{}_{}" r="{:.8}" x="{:.8}" b="0.0" g="0.0" ratedU1="{:.4}" ratedU2="{:.4}" voltageLevelId1="VL{}" bus1="B{}" connectableBus1="B{}" voltageLevelId2="VL{}" bus2="B{}" connectableBus2="B{}"/>"#,
                    br.from_bus, br.to_bus, bi + 1,
                    br.r * zb, br.x * zb,
                    br.tap * nu1, nu2,
                    br.from_bus, br.from_bus, br.from_bus,
                    br.to_bus, br.to_bus, br.to_bus).expect("writing to String is infallible");
            }
        }

        writeln!(out, r#"  </iidm:substation>"#).expect("writing to String is infallible");
    }

    // Lines at root level (connect different substations) — physical units.
    //
    // Uses the asymmetric π-model derived from the MATPOWER pu Y-bus:
    //   Y12_phys = -y_pu × MVA/(nu1×nu2)          (geometric-mean base for series branch)
    //   Y11_phys = (y_pu + jb/2) × MVA/nu1²        (bus-1 self-admittance)
    //   Y22_phys = (y_pu + jb/2) × MVA/nu2²        (bus-2 self-admittance)
    //   g1+jb1   = Y11_phys − (−Y12_phys)          (bus-1 π-model shunt)
    //   g2+jb2   = Y22_phys − (−Y12_phys)          (bus-2 π-model shunt)
    // For same-kV branches nu1=nu2 so y_base1=y_base_gm and g1=g2=0, b1=b2 (symmetric π).
    for (i, br) in network.branches.iter().enumerate() {
        if xfmr_set.contains(&i) {
            continue;
        }
        if !br.in_service {
            continue;
        } // skip out-of-service lines
        let nu1 = effective_kv.get(&br.from_bus).copied().unwrap_or(1.0);
        let nu2 = effective_kv.get(&br.to_bus).copied().unwrap_or(1.0);
        let z_base_gm = nu1 * nu2 / network.base_mva; // Ω — geometric-mean impedance base
        let y_base_gm = network.base_mva / (nu1 * nu2); // S
        let y_base1 = network.base_mva / (nu1 * nu1); // S
        let y_base2 = network.base_mva / (nu2 * nu2); // S
        let denom = br.r * br.r + br.x * br.x;
        let (g1, b1, g2, b2) = if denom > 1e-20 {
            let g_pu = br.r / denom;
            let b_pu_s = -br.x / denom; // imaginary part of series admittance
            (
                g_pu * (y_base1 - y_base_gm),
                b_pu_s * (y_base1 - y_base_gm) + (br.b / 2.0) * y_base1,
                g_pu * (y_base2 - y_base_gm),
                b_pu_s * (y_base2 - y_base_gm) + (br.b / 2.0) * y_base2,
            )
        } else {
            // Degenerate (r=x≈0): only charging
            (0.0, (br.b / 2.0) * y_base1, 0.0, (br.b / 2.0) * y_base2)
        };
        writeln!(out,
            r#"  <iidm:line id="L_{}_{}_{}" r="{:.8}" x="{:.8}" g1="{:.10e}" b1="{:.10e}" g2="{:.10e}" b2="{:.10e}" voltageLevelId1="VL{}" bus1="B{}" connectableBus1="B{}" voltageLevelId2="VL{}" bus2="B{}" connectableBus2="B{}"/>"#,
            br.from_bus, br.to_bus, i + 1,
            br.r * z_base_gm, br.x * z_base_gm, g1, b1, g2, b2,
            br.from_bus, br.from_bus, br.from_bus,
            br.to_bus, br.to_bus, br.to_bus).expect("writing to String is infallible");
    }

    writeln!(out, r#"</iidm:network>"#).expect("writing to String is infallible");
    Ok(out)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Write a NODE_BREAKER network using real NodeBreakerTopology hierarchy.
fn write_node_breaker_network(
    out: &mut String,
    network: &Network,
    sm: &NodeBreakerTopology,
) -> Result<(), Error> {
    use std::collections::{HashMap, HashSet};
    use std::fmt::Write;

    // Precompute per-bus demand from Load objects.
    let bus_demand_p = network.bus_load_p_mw();
    let bus_demand_q = network.bus_load_q_mvar();
    let bus_idx_map = network.bus_index_map();

    let mapping = sm.current_mapping();

    // Build reverse mapping: bus → set of CN IDs
    let mut bus_to_connectivity_nodes: HashMap<u32, Vec<&str>> = HashMap::new();
    if let Some(m) = mapping {
        for (cn, &bus) in &m.connectivity_node_to_bus {
            bus_to_connectivity_nodes
                .entry(bus)
                .or_default()
                .push(cn.as_str());
        }
    }

    // Build CN→bus lookup
    let connectivity_node_to_bus: HashMap<&str, u32> = mapping
        .map(|m| {
            m.connectivity_node_to_bus
                .iter()
                .map(|(k, &v)| (k.as_str(), v))
                .collect()
        })
        .unwrap_or_default();

    // Index: equipment_id → bus number (for finding equipment connected to a VL)
    let bus_map: HashMap<u32, &surge_network::network::Bus> =
        network.buses.iter().map(|b| (b.number, b)).collect();

    // Group VLs by substation
    let mut sub_vl_map: HashMap<&str, Vec<&surge_network::network::topology::VoltageLevel>> =
        HashMap::new();
    for vl in &sm.voltage_levels {
        sub_vl_map
            .entry(vl.substation_id.as_str())
            .or_default()
            .push(vl);
    }

    // Group CNs by VL
    let mut vl_cn_ids: HashMap<&str, Vec<&str>> = HashMap::new();
    for cn in &sm.connectivity_nodes {
        vl_cn_ids
            .entry(cn.voltage_level_id.as_str())
            .or_default()
            .push(cn.id.as_str());
    }

    // Group terminal connections by CN
    let mut cn_terminals: HashMap<&str, Vec<&TerminalConnection>> = HashMap::new();
    for tc in &sm.terminal_connections {
        cn_terminals
            .entry(tc.connectivity_node_id.as_str())
            .or_default()
            .push(tc);
    }

    // Track which branches have been written (inside substations as xfmr vs root lines)
    let mut written_branches: HashSet<usize> = HashSet::new();

    // Emit substations
    for sub in &sm.substations {
        writeln!(
            out,
            r#"  <iidm:substation id="{}" country="{}">"#,
            xml_escape(&sub.id),
            sub.region.as_deref().unwrap_or("XX")
        )
        .expect("writing to String is infallible");

        let vls = sub_vl_map.get(sub.id.as_str()).cloned().unwrap_or_default();

        for vl in &vls {
            let cn_ids = vl_cn_ids.get(vl.id.as_str()).cloned().unwrap_or_default();
            let has_nb = !cn_ids.is_empty();

            if has_nb {
                // NODE_BREAKER voltage level
                writeln!(
                    out,
                    r#"    <iidm:voltageLevel id="{}" nominalV="{}" topologyKind="NODE_BREAKER">"#,
                    xml_escape(&vl.id),
                    vl.base_kv
                )
                .expect("writing to String is infallible");

                let node_count = cn_ids.len();
                writeln!(
                    out,
                    r#"      <iidm:nodeBreakerTopology nodeCount="{}">"#,
                    node_count
                )
                .expect("writing to String is infallible");

                // Busbar sections
                for bbs in &sm.busbar_sections {
                    if cn_ids.contains(&bbs.connectivity_node_id.as_str()) {
                        let node = extract_node_from_cn_id(&bbs.connectivity_node_id);
                        writeln!(
                            out,
                            r#"        <iidm:busbarSection id="{}" node="{}"/>"#,
                            xml_escape(&bbs.id),
                            node
                        )
                        .expect("writing to String is infallible");
                    }
                }

                // Switches
                let cn_set: HashSet<&str> = cn_ids.iter().copied().collect();
                for sw in &sm.switches {
                    if !cn_set.contains(sw.cn1_id.as_str()) {
                        continue;
                    }
                    if sw.name == "InternalConnection" {
                        let n1 = extract_node_from_cn_id(&sw.cn1_id);
                        let n2 = extract_node_from_cn_id(&sw.cn2_id);
                        writeln!(
                            out,
                            r#"        <iidm:internalConnection node1="{}" node2="{}"/>"#,
                            n1, n2
                        )
                        .expect("writing to String is infallible");
                    } else {
                        let kind = match sw.switch_type {
                            SwitchType::Breaker => "BREAKER",
                            SwitchType::Disconnector => "DISCONNECTOR",
                            SwitchType::LoadBreakSwitch => "LOAD_BREAK_SWITCH",
                            _ => "BREAKER",
                        };
                        let n1 = extract_node_from_cn_id(&sw.cn1_id);
                        let n2 = extract_node_from_cn_id(&sw.cn2_id);
                        writeln!(
                            out,
                            r#"        <iidm:switch id="{}" kind="{}" retained="{}" open="{}" node1="{}" node2="{}"/>"#,
                            xml_escape(&sw.id),
                            kind,
                            sw.retained,
                            sw.open,
                            n1,
                            n2
                        )
                        .expect("writing to String is infallible");
                    }
                }

                writeln!(out, r#"      </iidm:nodeBreakerTopology>"#)
                    .expect("writing to String is infallible");

                let mut vl_bus_nodes: HashMap<u32, &str> = HashMap::new();
                for &cn in &cn_ids {
                    if let Some(&bus) = connectivity_node_to_bus.get(cn) {
                        vl_bus_nodes.entry(bus).or_insert(cn);
                    }
                }
                let mut equipment_cn: HashMap<&str, &str> = HashMap::new();
                for tc in &sm.terminal_connections {
                    if cn_set.contains(tc.connectivity_node_id.as_str()) {
                        equipment_cn
                            .entry(tc.equipment_id.as_str())
                            .or_insert(tc.connectivity_node_id.as_str());
                    }
                }

                let mut residual_bus_loads: HashMap<u32, (f64, f64)> = HashMap::new();
                let mut residual_bus_shunts: HashMap<u32, (f64, f64)> = HashMap::new();
                for &bus in vl_bus_nodes.keys() {
                    if let Some(b) = bus_map.get(&bus) {
                        let bi = bus_idx_map.get(&bus).copied().unwrap_or(0);
                        let pd = bus_demand_p.get(bi).copied().unwrap_or(0.0);
                        let qd = bus_demand_q.get(bi).copied().unwrap_or(0.0);
                        residual_bus_loads.insert(bus, (pd, qd));
                        residual_bus_shunts
                            .insert(bus, (b.shunt_conductance_mw, b.shunt_susceptance_mvar));
                    }
                }

                // Equipment connected to this VL via terminal connections
                // Generators
                for (gi, g) in network.generators.iter().enumerate() {
                    let explicit_id = g.machine_id.as_deref().filter(|id| !id.is_empty());
                    let explicit_cn = explicit_id.and_then(|id| equipment_cn.get(id).copied());
                    let fallback_cn = bus_to_connectivity_nodes
                        .get(&g.bus)
                        .and_then(|cns| cns.iter().copied().find(|cn| cn_set.contains(cn)));
                    let Some(cn) = explicit_cn.or(fallback_cn) else {
                        continue;
                    };
                    let node = extract_node_from_cn_id(cn);
                    let qmax = if g.qmax.is_finite() { g.qmax } else { 9999.0 };
                    let qmin = if g.qmin.is_finite() { g.qmin } else { -9999.0 };
                    let pmax = if g.pmax.is_finite() { g.pmax } else { 9999.0 };
                    let pmin = if g.pmin.is_finite() { g.pmin } else { -9999.0 };
                    let tv_kv = g.voltage_setpoint_pu * vl.base_kv;
                    let bus = bus_map.get(&g.bus);
                    let is_pv_slack = bus
                        .map(|b| b.bus_type == BusType::PV || b.bus_type == BusType::Slack)
                        .unwrap_or(false);
                    let generator_id = explicit_id
                        .map(xml_escape)
                        .unwrap_or_else(|| format!("G{}_{}", g.bus, gi + 1));
                    writeln!(out,
                        r#"      <iidm:generator id="{}" energySource="OTHER" minP="{}" maxP="{}" targetP="{}" targetQ="{:.6}" targetV="{:.6}" voltageRegulatorOn="{}" node="{}">"#,
                        generator_id, pmin, pmax, g.p, g.q, tv_kv, is_pv_slack, node)
                        .expect("writing to String is infallible");
                    writeln!(
                        out,
                        r#"        <iidm:minMaxReactiveLimits minQ="{}" maxQ="{}"/>"#,
                        qmin, qmax
                    )
                    .expect("writing to String is infallible");
                    writeln!(out, r#"      </iidm:generator>"#)
                        .expect("writing to String is infallible");
                }

                for load in &network.loads {
                    if !load.in_service || load.id.is_empty() {
                        continue;
                    }
                    let Some(cn) = equipment_cn.get(load.id.as_str()).copied() else {
                        continue;
                    };
                    let node = extract_node_from_cn_id(cn);
                    writeln!(out,
                        r#"      <iidm:load id="{}" loadType="UNDEFINED" p0="{}" q0="{}" node="{}"/>"#,
                        xml_escape(&load.id), load.active_power_demand_mw, load.reactive_power_demand_mvar, node)
                        .expect("writing to String is infallible");
                    if let Some(residual) = residual_bus_loads.get_mut(&load.bus) {
                        residual.0 -= load.active_power_demand_mw;
                        residual.1 -= load.reactive_power_demand_mvar;
                    }
                }

                for injection in &network.power_injections {
                    if !injection.in_service || injection.id.is_empty() {
                        continue;
                    }
                    let Some(cn) = equipment_cn.get(injection.id.as_str()).copied() else {
                        continue;
                    };
                    let node = extract_node_from_cn_id(cn);
                    if injection.active_power_injection_mw > 1e-9
                        || (injection.active_power_injection_mw.abs() <= 1e-9
                            && injection.reactive_power_injection_mvar >= 0.0)
                    {
                        let target_v_kv = bus_map
                            .get(&injection.bus)
                            .map(|bus| bus.voltage_magnitude_pu * vl.base_kv)
                            .unwrap_or(vl.base_kv);
                        writeln!(out,
                            r#"      <iidm:generator id="{}" energySource="OTHER" minP="{}" maxP="{}" targetP="{}" targetQ="{:.6}" targetV="{:.6}" voltageRegulatorOn="false" node="{}">"#,
                            xml_escape(&injection.id),
                            injection.active_power_injection_mw,
                            injection.active_power_injection_mw,
                            injection.active_power_injection_mw,
                            injection.reactive_power_injection_mvar,
                            target_v_kv,
                            node)
                            .expect("writing to String is infallible");
                        writeln!(
                            out,
                            r#"        <iidm:minMaxReactiveLimits minQ="{}" maxQ="{}"/>"#,
                            injection.reactive_power_injection_mvar,
                            injection.reactive_power_injection_mvar
                        )
                        .expect("writing to String is infallible");
                        writeln!(out, r#"      </iidm:generator>"#)
                            .expect("writing to String is infallible");
                    } else {
                        writeln!(out,
                            r#"      <iidm:load id="{}" loadType="UNDEFINED" p0="{}" q0="{}" node="{}"/>"#,
                            xml_escape(&injection.id),
                            -injection.active_power_injection_mw,
                            -injection.reactive_power_injection_mvar,
                            node)
                            .expect("writing to String is infallible");
                    }
                    if let Some(residual) = residual_bus_loads.get_mut(&injection.bus) {
                        residual.0 += injection.active_power_injection_mw;
                        residual.1 += injection.reactive_power_injection_mvar;
                    }
                }

                // Residual loads: emit once per bus within this voltage level.
                for (&bus, &cn) in &vl_bus_nodes {
                    if let Some(&(p_residual, q_residual)) = residual_bus_loads.get(&bus)
                        && (p_residual.abs() > 1e-10 || q_residual.abs() > 1e-10)
                    {
                        let node = extract_node_from_cn_id(cn);
                        writeln!(out,
                            r#"      <iidm:load id="L_{}" loadType="UNDEFINED" p0="{}" q0="{}" node="{}"/>"#,
                            cn, p_residual, q_residual, node)
                            .expect("writing to String is infallible");
                    }
                }

                for shunt in &network.fixed_shunts {
                    if !shunt.in_service || shunt.id.is_empty() {
                        continue;
                    }
                    let Some(cn) = equipment_cn.get(shunt.id.as_str()).copied() else {
                        continue;
                    };
                    let node = extract_node_from_cn_id(cn);
                    let b_per_s = shunt.b_mvar / (vl.base_kv * vl.base_kv);
                    let g_per_s = shunt.g_mw / (vl.base_kv * vl.base_kv);
                    writeln!(out,
                        r#"      <iidm:shunt id="{}" sectionCount="1" voltageRegulatorOn="false" node="{}">"#,
                        xml_escape(&shunt.id), node)
                        .expect("writing to String is infallible");
                    writeln!(out,
                        r#"        <iidm:shuntLinearModel bPerSection="{:.10}" gPerSection="{:.10}" maximumSectionCount="1"/>"#,
                        b_per_s, g_per_s)
                        .expect("writing to String is infallible");
                    writeln!(out, r#"      </iidm:shunt>"#)
                        .expect("writing to String is infallible");
                    if let Some(residual) = residual_bus_shunts.get_mut(&shunt.bus) {
                        residual.0 -= shunt.g_mw;
                        residual.1 -= shunt.b_mvar;
                    }
                }

                // Residual shunts: emit once per bus within this voltage level.
                for (&bus, &cn) in &vl_bus_nodes {
                    if let Some(&(g_residual, b_residual)) = residual_bus_shunts.get(&bus)
                        && (b_residual.abs() > 1e-10 || g_residual.abs() > 1e-10)
                    {
                        let node = extract_node_from_cn_id(cn);
                        let b_per_s = b_residual / (vl.base_kv * vl.base_kv);
                        let g_per_s = g_residual / (vl.base_kv * vl.base_kv);
                        writeln!(out,
                            r#"      <iidm:shunt id="SHC_{}" sectionCount="1" voltageRegulatorOn="false" node="{}">"#,
                            cn, node)
                            .expect("writing to String is infallible");
                        writeln!(out,
                            r#"        <iidm:shuntLinearModel bPerSection="{:.10}" gPerSection="{:.10}" maximumSectionCount="1"/>"#,
                            b_per_s, g_per_s)
                            .expect("writing to String is infallible");
                        writeln!(out, r#"      </iidm:shunt>"#)
                            .expect("writing to String is infallible");
                    }
                }

                writeln!(out, r#"    </iidm:voltageLevel>"#)
                    .expect("writing to String is infallible");
            }
            // else: BUS_BREAKER VL within NodeBreakerTopology — skip for now
        }

        // Transformers inside this substation (both ends' VLs belong to this sub)
        let sub_vl_ids: HashSet<&str> = vls.iter().map(|vl| vl.id.as_str()).collect();
        for (bi, br) in network.branches.iter().enumerate() {
            if written_branches.contains(&bi) || !br.in_service {
                continue;
            }
            // Check if this is a cross-VL branch within this substation
            let from_vl = find_vl_for_bus(br.from_bus, &connectivity_node_to_bus, &vl_cn_ids);
            let to_vl = find_vl_for_bus(br.to_bus, &connectivity_node_to_bus, &vl_cn_ids);
            let (Some(fvl), Some(tvl)) = (from_vl, to_vl) else {
                continue;
            };
            if !sub_vl_ids.contains(fvl) || !sub_vl_ids.contains(tvl) {
                continue;
            }
            if fvl == tvl {
                continue; // same VL → not a transformer
            }

            // Write as twoWindingsTransformer
            let from_cn = find_cn_for_bus(br.from_bus, &bus_to_connectivity_nodes, fvl, &vl_cn_ids);
            let to_cn = find_cn_for_bus(br.to_bus, &bus_to_connectivity_nodes, tvl, &vl_cn_ids);
            let (Some(fcn), Some(tcn)) = (from_cn, to_cn) else {
                continue;
            };
            let n1 = extract_node_from_cn_id(fcn);
            let n2 = extract_node_from_cn_id(tcn);
            let nu1 = sm
                .voltage_levels
                .iter()
                .find(|v| v.id == fvl)
                .map(|v| v.base_kv)
                .unwrap_or(1.0);
            let nu2 = sm
                .voltage_levels
                .iter()
                .find(|v| v.id == tvl)
                .map(|v| v.base_kv)
                .unwrap_or(1.0);
            let zb = nu2 * nu2 / network.base_mva;
            writeln!(out,
                r#"    <iidm:twoWindingsTransformer id="T_{}_{}_{}" r="{:.8}" x="{:.8}" b="0.0" g="0.0" ratedU1="{:.4}" ratedU2="{:.4}" voltageLevelId1="{}" node1="{}" voltageLevelId2="{}" node2="{}"/>"#,
                br.from_bus, br.to_bus, bi + 1,
                br.r * zb, br.x * zb,
                br.tap * nu1, nu2,
                fvl, n1, tvl, n2)
                .expect("writing to String is infallible");
            written_branches.insert(bi);
        }

        writeln!(out, r#"  </iidm:substation>"#).expect("writing to String is infallible");
    }

    // Lines at root level (inter-substation branches)
    for (bi, br) in network.branches.iter().enumerate() {
        if written_branches.contains(&bi) || !br.in_service {
            continue;
        }
        let from_vl = find_vl_for_bus(br.from_bus, &connectivity_node_to_bus, &vl_cn_ids);
        let to_vl = find_vl_for_bus(br.to_bus, &connectivity_node_to_bus, &vl_cn_ids);
        let (Some(fvl), Some(tvl)) = (from_vl, to_vl) else {
            continue;
        };
        let from_cn = find_cn_for_bus(br.from_bus, &bus_to_connectivity_nodes, fvl, &vl_cn_ids);
        let to_cn = find_cn_for_bus(br.to_bus, &bus_to_connectivity_nodes, tvl, &vl_cn_ids);
        let (Some(fcn), Some(tcn)) = (from_cn, to_cn) else {
            continue;
        };
        let n1 = extract_node_from_cn_id(fcn);
        let n2 = extract_node_from_cn_id(tcn);
        let nu1 = sm
            .voltage_levels
            .iter()
            .find(|v| v.id == fvl)
            .map(|v| v.base_kv)
            .unwrap_or(1.0);
        let nu2 = sm
            .voltage_levels
            .iter()
            .find(|v| v.id == tvl)
            .map(|v| v.base_kv)
            .unwrap_or(1.0);
        let z_base_gm = nu1 * nu2 / network.base_mva;
        let y_base_gm = network.base_mva / (nu1 * nu2);
        let y_base1 = network.base_mva / (nu1 * nu1);
        let y_base2 = network.base_mva / (nu2 * nu2);
        let denom = br.r * br.r + br.x * br.x;
        let (g1, b1, g2, b2) = if denom > 1e-20 {
            let g_pu = br.r / denom;
            let b_pu_s = -br.x / denom;
            (
                g_pu * (y_base1 - y_base_gm),
                b_pu_s * (y_base1 - y_base_gm) + (br.b / 2.0) * y_base1,
                g_pu * (y_base2 - y_base_gm),
                b_pu_s * (y_base2 - y_base_gm) + (br.b / 2.0) * y_base2,
            )
        } else {
            (0.0, (br.b / 2.0) * y_base1, 0.0, (br.b / 2.0) * y_base2)
        };
        writeln!(out,
            r#"  <iidm:line id="L_{}_{}_{}" r="{:.8}" x="{:.8}" g1="{:.10e}" b1="{:.10e}" g2="{:.10e}" b2="{:.10e}" voltageLevelId1="{}" node1="{}" voltageLevelId2="{}" node2="{}"/>"#,
            br.from_bus, br.to_bus, bi + 1,
            br.r * z_base_gm, br.x * z_base_gm, g1, b1, g2, b2,
            fvl, n1, tvl, n2)
            .expect("writing to String is infallible");
    }

    Ok(())
}

/// Find the voltage level ID for a bus by looking up its CN mappings.
fn find_vl_for_bus<'a>(
    bus: u32,
    connectivity_node_to_bus: &HashMap<&str, u32>,
    vl_cn_ids: &HashMap<&'a str, Vec<&str>>,
) -> Option<&'a str> {
    // Find any CN mapped to this bus, then find which VL it belongs to
    for (vl_id, cns) in vl_cn_ids {
        for cn in cns {
            if connectivity_node_to_bus.get(cn).copied() == Some(bus) {
                return Some(vl_id);
            }
        }
    }
    None
}

/// Find a CN ID for a bus within a specific voltage level.
fn find_cn_for_bus<'a>(
    bus: u32,
    bus_to_connectivity_nodes: &HashMap<u32, Vec<&'a str>>,
    vl_id: &str,
    vl_cn_ids: &HashMap<&str, Vec<&str>>,
) -> Option<&'a str> {
    let cns = bus_to_connectivity_nodes.get(&bus)?;
    let vl_cns = vl_cn_ids.get(vl_id)?;
    cns.iter().find(|cn| vl_cns.contains(cn)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn simple_network() -> Network {
        let mut net = Network::new("case9");
        net.base_mva = 100.0;
        let mut slack = Bus::new(1, BusType::Slack, 345.0);
        slack.voltage_magnitude_pu = 1.04;
        slack.voltage_angle_rad = 0.0;
        net.buses.push(slack);
        let pq = Bus::new(2, BusType::PQ, 345.0);
        net.buses.push(pq);
        net.loads.push(Load::new(2, 125.0, 50.0));
        let mut g = Generator::new(1, 72.3, 1.04);
        g.pmax = 250.0;
        g.qmax = 300.0;
        g.qmin = -300.0;
        net.generators.push(g);
        net.branches
            .push(Branch::new_line(1, 2, 0.01938, 0.05917, 0.0528));
        net
    }

    #[test]
    fn test_xiidm_write_produces_xml() {
        let net = simple_network();
        let s = to_string(&net).expect("writing to String is infallible");
        assert!(s.contains(r#"<?xml version="1.0""#));
        assert!(s.contains("iidm:network"));
        assert!(s.contains("iidm:bus"));
        assert!(s.contains("iidm:generator"));
        assert!(s.contains("iidm:line"));
        // Verify voltageLevelId attributes are present
        assert!(s.contains("voltageLevelId1="));
        assert!(s.contains("voltageLevelId2="));
    }

    #[test]
    fn test_xiidm_si_conversion() {
        // Verify that per-unit impedances are written in SI (Ohms/Siemens)
        // For 345kV, 100MVA base: Z_base = 345^2/100 = 1190.25 Ohms
        let net = simple_network();
        let s = to_string(&net).expect("writing to String is infallible");
        // r = 0.01938 pu → 0.01938 * 1190.25 ≈ 23.07 Ohms
        // Check that the r value in the output is >> 1 (SI, not per-unit)
        assert!(!s.contains(r#"r="0.01938""#), "r should be in Ohms not pu");
        // Verify round-trip preserves impedances within 0.1%
        let net2 = parse_str(&s).expect("writing to String is infallible");
        let br = &net2.branches[0];
        assert!(
            (br.r - 0.01938).abs() / 0.01938 < 0.001,
            "r round-trip error: got {}, expected 0.01938",
            br.r
        );
        assert!(
            (br.x - 0.05917).abs() / 0.05917 < 0.001,
            "x round-trip error: got {}, expected 0.05917",
            br.x
        );
    }

    #[test]
    fn test_xiidm_roundtrip() {
        let net = simple_network();
        let s = to_string(&net).expect("writing to String is infallible");
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.n_buses(), net.n_buses());
        assert_eq!(net2.generators.len(), net.generators.len());
        assert_eq!(net2.n_branches(), net.n_branches());
    }

    #[test]
    fn test_xiidm_rejects_malformed_nominal_voltage() {
        let xml = r#"
<iidm:network xmlns:iidm="http://www.itesla_project.eu/schema/iidm/1_4" id="bad">
  <iidm:substation id="S1">
    <iidm:voltageLevel id="VL1" nominalV="abc" topologyKind="BUS_BREAKER">
      <iidm:bus id="B1"/>
    </iidm:voltageLevel>
  </iidm:substation>
</iidm:network>
"#;
        let err = parse_str(xml).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidValue { attr, .. } if attr == "voltageLevel.nominalV"
        ));
    }

    #[test]
    fn test_xiidm_rejects_malformed_line_impedance() {
        let xml = r#"
<iidm:network xmlns:iidm="http://www.itesla_project.eu/schema/iidm/1_4" id="bad">
  <iidm:substation id="S1">
    <iidm:voltageLevel id="VL1" nominalV="220" topologyKind="BUS_BREAKER">
      <iidm:bus id="B1"/>
      <iidm:bus id="B2"/>
      <iidm:line id="L1" bus1="B1" bus2="B2" voltageLevelId1="VL1" voltageLevelId2="VL1" r="oops" x="5.0" b1="0.0" b2="0.0"/>
    </iidm:voltageLevel>
  </iidm:substation>
</iidm:network>
"#;
        let err = parse_str(xml).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidValue { attr, .. } if attr == "line.r"
        ));
    }

    #[test]
    fn test_xiidm_bus_voltage() {
        let net = simple_network();
        let s = to_string(&net).expect("writing to String is infallible");
        let net2 = parse_str(&s).expect("writing to String is infallible");
        let slack = net2
            .buses
            .iter()
            .find(|b| b.number == 1)
            .expect("writing to String is infallible");
        assert!((slack.voltage_magnitude_pu - 1.04).abs() < 1e-4);
    }

    #[test]
    fn test_xiidm_file_roundtrip() {
        let net = simple_network();
        let tmp = std::env::temp_dir().join("surge_xiidm_test.xiidm");
        write_file(&net, &tmp).expect("writing to String is infallible");
        let net2 = parse_file(&tmp).expect("writing to String is infallible");
        assert_eq!(net2.n_buses(), net.n_buses());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_transformer_written_as_twowindingstransformer() {
        // Transformer branches (tap != 1) should be written as
        // <iidm:twoWindingsTransformer> inside a substation in XIIDM.
        let mut net = Network::new("xfmr_test");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 345.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.generators.push(Generator::new(1, 0.0, 1.0));
        let mut br = Branch::new_line(1, 2, 0.0, 0.05, 0.0);
        br.tap = 2.5; // 345/138 ≈ 2.5
        net.branches.push(br);
        let s = to_string(&net).expect("writing to String is infallible");
        // Transformer written as twoWindingsTransformer (pypowsybl native format)
        assert!(
            s.contains("iidm:twoWindingsTransformer"),
            "transformer should be written as twoWindingsTransformer"
        );
        // Both buses grouped into same substation (union-find)
        assert!(s.contains("voltageLevelId1=\"VL1\""));
        assert!(s.contains("voltageLevelId2=\"VL2\""));
        // No iidm:line for this network (only a transformer branch)
        assert!(
            !s.contains("<iidm:line"),
            "transformer should not be written as line"
        );
    }

    #[test]
    fn test_offline_generator_pv_bus_ghost() {
        // A PV bus where ALL generators are offline must get a ghost voltage regulator
        // so pypowsybl treats it as PV (matches Surge/MATPOWER behavior).
        let mut net = Network::new("offline_gen_test");
        net.base_mva = 100.0;
        let mut slack = Bus::new(1, BusType::Slack, 345.0);
        slack.voltage_magnitude_pu = 1.04;
        net.buses.push(slack);
        let mut pv = Bus::new(2, BusType::PV, 345.0);
        pv.voltage_magnitude_pu = 1.02; // stored voltage setpoint
        net.buses.push(pv);
        // Online generator at slack
        net.generators.push(Generator::new(1, 100.0, 1.04));
        // OFFLINE generator at PV bus
        let mut offline_gen = Generator::new(2, 0.0, 1.02);
        offline_gen.in_service = false;
        net.generators.push(offline_gen);
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));

        let s = to_string(&net).expect("writing to String is infallible");
        // Ghost generator should appear at bus 2 (all-offline PV bus)
        assert!(
            s.contains("G2_ghost"),
            "ghost voltage regulator must be written for all-offline PV bus"
        );
        assert!(
            s.contains("voltageRegulatorOn=\"true\""),
            "ghost gen must have voltageRegulatorOn=true"
        );
        // Offline generator at PQ bus: should NOT appear (no ghost needed)
        let mut net2 = Network::new("offline_gen_pq");
        net2.base_mva = 100.0;
        let mut slack2 = Bus::new(1, BusType::Slack, 345.0);
        slack2.voltage_magnitude_pu = 1.04;
        net2.buses.push(slack2);
        net2.buses.push(Bus::new(2, BusType::PQ, 345.0)); // PQ bus
        net2.generators.push(Generator::new(1, 100.0, 1.04));
        let mut offline_pq = Generator::new(2, 0.0, 1.0);
        offline_pq.in_service = false;
        net2.generators.push(offline_pq);
        net2.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
        let s2 = to_string(&net2).expect("writing to String is infallible");
        // No ghost generator at PQ bus
        assert!(
            !s2.contains("G2_ghost"),
            "ghost generator must NOT be written for PQ buses"
        );
    }

    #[test]
    fn test_phase_shifting_transformer_roundtrip() {
        // Phase-shifting transformer (shift != 0) should survive a write→parse round-trip.
        let mut net = Network::new("pst_test");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 345.0));
        net.buses.push(Bus::new(2, BusType::PQ, 345.0));
        net.generators.push(Generator::new(1, 100.0, 1.0));
        let mut br = Branch::new_line(1, 2, 0.001, 0.02, 0.0);
        br.tap = 1.05;
        br.phase_shift_rad = (-3.6_f64).to_radians();
        net.branches.push(br);

        let s = to_string(&net).expect("writing to String is infallible");
        // phaseTapChanger element must be present
        assert!(
            s.contains("iidm:phaseTapChanger"),
            "phase-shifting transformer must contain phaseTapChanger"
        );
        // alpha should be +3.6 (= -shift per sign convention)
        assert!(
            s.contains("alpha=\"3.600000\""),
            "alpha should be 3.6 (= -shift)"
        );

        // Round-trip: parse back and verify shift is restored
        let net2 = parse_str(&s).expect("writing to String is infallible");
        assert_eq!(net2.branches.len(), 1);
        let br2 = &net2.branches[0];
        assert!(
            (br2.tap - 1.05).abs() < 1e-4,
            "tap should survive round-trip"
        );
        assert!(
            (br2.phase_shift_rad - (-3.6_f64).to_radians()).abs() < 1e-4,
            "shift should survive round-trip"
        );
    }

    /// Bug 1: shunt compensators must survive a write→parse round-trip.
    /// Previously the reader had no "shunt"/"shuntLinearModel" match arm, so
    /// all bus shunts were silently dropped on re-read.
    #[test]
    fn test_shunt_roundtrip() {
        let mut net = Network::new("shunt_test");
        net.base_mva = 100.0;
        let mut slack = Bus::new(1, BusType::Slack, 345.0);
        slack.voltage_magnitude_pu = 1.04;
        // Fixed shunt: capacitor bank — Bs = 50 MVAr, Gs = 5 MW (both non-zero)
        slack.shunt_susceptance_mvar = 50.0;
        slack.shunt_conductance_mw = 5.0;
        net.buses.push(slack);
        let mut pq = Bus::new(2, BusType::PQ, 345.0);
        pq.shunt_susceptance_mvar = -20.0; // reactor (inductive, consumes VArs)
        net.buses.push(pq);
        net.generators.push(Generator::new(1, 100.0, 1.04));
        net.branches
            .push(Branch::new_line(1, 2, 0.01938, 0.05917, 0.0528));

        let s = to_string(&net).expect("writing to String is infallible");
        // Shunt element must be present in written XML
        assert!(
            s.contains("iidm:shunt"),
            "shunt compensator must be written to XIIDM"
        );
        assert!(
            s.contains("shuntLinearModel"),
            "shuntLinearModel child element must be written"
        );

        let net2 = parse_str(&s).expect("writing to String is infallible");
        let b1 = net2
            .buses
            .iter()
            .find(|b| b.number == 1)
            .expect("writing to String is infallible");
        let b2 = net2
            .buses
            .iter()
            .find(|b| b.number == 2)
            .expect("writing to String is infallible");
        assert!(
            (b1.shunt_susceptance_mvar - 50.0).abs() < 1.0,
            "bus 1 Bs round-trip: got {}, expected ~50 MVAr",
            b1.shunt_susceptance_mvar
        );
        assert!(
            (b1.shunt_conductance_mw - 5.0).abs() < 0.5,
            "bus 1 Gs round-trip: got {}, expected ~5 MW",
            b1.shunt_conductance_mw
        );
        assert!(
            (b2.shunt_susceptance_mvar - (-20.0)).abs() < 1.0,
            "bus 2 Bs round-trip: got {}, expected ~-20 MVAr",
            b2.shunt_susceptance_mvar
        );
    }

    /// Bug 2: SI heuristic must NOT misclassify short/low-impedance branches.
    /// A 1 km 110 kV cable with r=0.05 Ω (r < 1 Ω) used to fall through the
    /// `else` branch and be treated as per-unit, giving ~120× wrong impedance.
    /// Now XIIDM is always SI; always divide by z_base.
    #[test]
    fn test_si_conversion_low_impedance_line() {
        // 110 kV, 100 MVA base: z_base = 110² / 100 = 121 Ω
        // A short cable: r = 0.05 Ω, x = 0.5 Ω (both < 1.0 — old heuristic failed)
        // Expected per-unit: r_pu = 0.05/121 ≈ 4.13e-4, x_pu = 0.5/121 ≈ 4.13e-3
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_15"
              id="cable_test" baseMVA="100">
  <iidm:substation id="S1" country="FR">
    <iidm:voltageLevel id="VL1" nominalV="110" topologyKind="BUS_BREAKER">
      <iidm:busBreakerTopology><iidm:bus id="B1" v="110.0" angle="0.0"/></iidm:busBreakerTopology>
      <iidm:generator id="G1_1" connectableBus="B1" bus="B1" energySource="OTHER"
                      minP="0" maxP="500" targetP="100" targetQ="0" targetV="110"
                      voltageRegulatorOn="true">
        <iidm:minMaxReactiveLimits minQ="-300" maxQ="300"/>
      </iidm:generator>
    </iidm:voltageLevel>
    <iidm:voltageLevel id="VL2" nominalV="110" topologyKind="BUS_BREAKER">
      <iidm:busBreakerTopology><iidm:bus id="B2" v="109.0" angle="-1.0"/></iidm:busBreakerTopology>
    </iidm:voltageLevel>
  </iidm:substation>
  <iidm:line id="L_1_2_1" r="0.05" x="0.5" g1="0.0" b1="0.0" g2="0.0" b2="0.0"
             voltageLevelId1="VL1" bus1="B1" connectableBus1="B1"
             voltageLevelId2="VL2" bus2="B2" connectableBus2="B2"/>
</iidm:network>"#;
        let net = parse_str(xml).expect("writing to String is infallible");
        assert_eq!(net.branches.len(), 1);
        let br = &net.branches[0];
        // z_base = 110² / 100 = 121 Ω
        let z_base = 110.0_f64 * 110.0 / 100.0;
        let expected_r = 0.05 / z_base;
        let expected_x = 0.5 / z_base;
        assert!(
            (br.r - expected_r).abs() < 1e-8,
            "r_pu: got {:.6e}, expected {:.6e} (SI always converted)",
            br.r,
            expected_r
        );
        assert!(
            (br.x - expected_x).abs() < 1e-8,
            "x_pu: got {:.6e}, expected {:.6e} (SI always converted)",
            br.x,
            expected_x
        );
    }

    /// Bug 3: ratioTapChanger step rho must be applied to transformer tap.
    /// A ratioTapChanger at a non-unity step (rho=1.05) must multiply br.tap.
    #[test]
    fn test_ratio_tap_changer_rho_roundtrip() {
        // Craft a minimal XIIDM with a twoWindingsTransformer + ratioTapChanger
        // at step rho=1.05 (5% above nominal).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_15"
              id="rtc_test" baseMVA="100">
  <iidm:substation id="S1" country="FR">
    <iidm:voltageLevel id="VL1" nominalV="345" topologyKind="BUS_BREAKER">
      <iidm:busBreakerTopology><iidm:bus id="B1" v="345.0" angle="0.0"/></iidm:busBreakerTopology>
      <iidm:generator id="G1_1" connectableBus="B1" bus="B1" energySource="OTHER"
                      minP="0" maxP="500" targetP="100" targetQ="0" targetV="345"
                      voltageRegulatorOn="true">
        <iidm:minMaxReactiveLimits minQ="-300" maxQ="300"/>
      </iidm:generator>
    </iidm:voltageLevel>
    <iidm:voltageLevel id="VL2" nominalV="138" topologyKind="BUS_BREAKER">
      <iidm:busBreakerTopology><iidm:bus id="B2" v="138.0" angle="-2.0"/></iidm:busBreakerTopology>
    </iidm:voltageLevel>
    <iidm:twoWindingsTransformer id="T_1_2_1"
        r="0.0" x="5.0" b="0.0" g="0.0"
        ratedU1="345.0" ratedU2="138.0"
        voltageLevelId1="VL1" bus1="B1" connectableBus1="B1"
        voltageLevelId2="VL2" bus2="B2" connectableBus2="B2">
      <iidm:ratioTapChanger lowTapPosition="0" tapPosition="0" loadTapChangingCapabilities="false">
        <iidm:step r="0.0" x="0.0" g="0.0" b="0.0" rho="1.05"/>
      </iidm:ratioTapChanger>
    </iidm:twoWindingsTransformer>
  </iidm:substation>
</iidm:network>"#;
        let net = parse_str(xml).expect("writing to String is infallible");
        assert_eq!(net.branches.len(), 1);
        let br = &net.branches[0];
        // Base tap from ratedU1/ratedU2 adjusted by nomV ratio:
        //   nom_ratio = 345/138 ≈ 2.5, rated_ratio = 345/138 ≈ 2.5 → tap_base = 1.0
        //   then rho=1.05 multiplied in → tap_final = 1.05
        assert!(
            (br.tap - 1.05).abs() < 1e-6,
            "tap should be 1.05 after applying rho=1.05; got {}",
            br.tap
        );
    }

    #[test]
    fn test_bus_breaker_rejects_missing_bus_reference() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_15" id="strict_test" baseMVA="100">
  <iidm:substation id="S1" country="FR">
    <iidm:voltageLevel id="VL1" nominalV="345" topologyKind="BUS_BREAKER">
      <iidm:busBreakerTopology><iidm:bus id="B1" v="345.0" angle="0.0"/></iidm:busBreakerTopology>
      <iidm:generator id="G1" bus="B2" energySource="OTHER" minP="0" maxP="100" targetP="10" targetQ="0" targetV="345" voltageRegulatorOn="true"/>
    </iidm:voltageLevel>
  </iidm:substation>
</iidm:network>"#;

        let err = parse_str(xml).expect_err("missing bus reference should be rejected");
        assert!(
            matches!(err, Error::InvalidValue { ref attr, ref value } if attr == "bus" && value == "B2"),
            "unexpected error: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // NODE_BREAKER tests
    // -----------------------------------------------------------------------

    /// Synthetic NODE_BREAKER XML: 2 VLs, busbar sections, switches,
    /// internal connection, generator, load, line. Verify topology reduction
    /// produces correct buses and NodeBreakerTopology is populated.
    #[test]
    fn test_node_breaker_basic_parse() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_1"
    id="nb_test" caseDate="2024-01-01T00:00:00.000Z" forecastDistance="0"
    sourceFormat="test" minimumValidationLevel="STEADY_STATE_HYPOTHESIS">
  <iidm:substation id="SUB1" name="Station1" country="US">
    <iidm:voltageLevel id="VL1" nominalV="345.0" topologyKind="NODE_BREAKER">
      <iidm:nodeBreakerTopology>
        <iidm:busbarSection id="BBS1" node="0"/>
        <iidm:switch id="BRK1" kind="BREAKER" retained="false" open="false" node1="0" node2="1"/>
        <iidm:switch id="DIS1" kind="DISCONNECTOR" retained="false" open="false" node1="1" node2="2"/>
        <iidm:internalConnection node1="0" node2="3"/>
        <iidm:bus v="345.0" angle="0.0" nodes="0,1,2,3"/>
      </iidm:nodeBreakerTopology>
      <iidm:generator id="GEN1" node="2" energySource="OTHER"
          minP="0.0" maxP="200.0" voltageRegulatorOn="true" targetP="100.0"
          targetV="345.0" targetQ="0.0"/>
      <iidm:load id="LOAD1" node="3" loadType="UNDEFINED" p0="50.0" q0="20.0"/>
    </iidm:voltageLevel>
    <iidm:voltageLevel id="VL2" nominalV="138.0" topologyKind="NODE_BREAKER">
      <iidm:nodeBreakerTopology>
        <iidm:busbarSection id="BBS2" node="0"/>
        <iidm:switch id="BRK2" kind="BREAKER" retained="false" open="false" node1="0" node2="1"/>
        <iidm:bus v="138.0" angle="-2.0" nodes="0,1"/>
      </iidm:nodeBreakerTopology>
      <iidm:load id="LOAD2" node="1" loadType="UNDEFINED" p0="30.0" q0="10.0"/>
    </iidm:voltageLevel>
    <iidm:twoWindingsTransformer id="TF1"
        r="0.0" x="5.0" b="0.0" g="0.0"
        ratedU1="345.0" ratedU2="138.0"
        voltageLevelId1="VL1" node1="0"
        voltageLevelId2="VL2" node2="0"/>
  </iidm:substation>
</iidm:network>"#;
        let net = parse_str(xml).expect("NB parse failed");

        // Topology reduction: VL1 nodes {0,1,2,3} all closed (non-retained) → 1 bus;
        // VL2 nodes {0,1} all closed → 1 bus. Total = 2 buses.
        assert_eq!(
            net.buses.len(),
            2,
            "expected 2 buses from NB topology reduction"
        );

        // NodeBreakerTopology present
        let sm = net
            .topology
            .as_ref()
            .expect("NodeBreakerTopology should be present");
        assert!(
            sm.current_mapping().is_some(),
            "topology reduction should exist"
        );

        // Check switch counts: 3 real switches + 1 internal connection = 4
        assert_eq!(sm.switches.len(), 4, "3 switches + 1 internal connection");

        // Check busbar section count
        assert_eq!(sm.busbar_sections.len(), 2, "2 busbar sections");

        // Generator
        assert_eq!(net.generators.len(), 1);
        assert!((net.generators[0].p - 100.0).abs() < 1e-6);

        // Loads: 50+30=80 MW across the 2 buses
        let total_pd: f64 = net.total_load_mw();
        assert!(
            (total_pd - 80.0).abs() < 1e-6,
            "total load should be 80 MW, got {}",
            total_pd
        );

        // Transformer
        assert_eq!(net.branches.len(), 1, "1 transformer branch");

        // Check solved voltages applied
        let bus1 = net
            .buses
            .iter()
            .find(|b| (b.base_kv - 345.0).abs() < 1.0)
            .unwrap();
        assert!(
            (bus1.voltage_magnitude_pu - 1.0).abs() < 1e-4,
            "VL1 bus vm should be ~1.0 pu"
        );

        let bus2 = net
            .buses
            .iter()
            .find(|b| (b.base_kv - 138.0).abs() < 1.0)
            .unwrap();
        assert!(
            (bus2.voltage_magnitude_pu - 1.0).abs() < 1e-4,
            "VL2 bus vm should be ~1.0 pu"
        );
        assert!(
            (bus2.voltage_angle_rad - (-2.0_f64).to_radians()).abs() < 1e-4,
            "VL2 bus angle should be -2 deg"
        );
    }

    /// Parse NB, open a breaker, rebuild_topology, verify bus count increases.
    #[test]
    fn test_node_breaker_switch_toggle() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_1"
    id="nb_toggle" caseDate="2024-01-01T00:00:00.000Z" forecastDistance="0"
    sourceFormat="test" minimumValidationLevel="STEADY_STATE_HYPOTHESIS">
  <iidm:substation id="SUB1" name="Station1" country="US">
    <iidm:voltageLevel id="VL1" nominalV="345.0" topologyKind="NODE_BREAKER">
      <iidm:nodeBreakerTopology>
        <iidm:busbarSection id="BBS1" node="0"/>
        <iidm:switch id="BRK1" kind="BREAKER" retained="false" open="false" node1="0" node2="1"/>
        <iidm:busbarSection id="BBS2" node="2"/>
        <iidm:switch id="BRK2" kind="BREAKER" retained="false" open="false" node1="1" node2="2"/>
        <iidm:bus v="345.0" angle="0.0" nodes="0,1,2"/>
      </iidm:nodeBreakerTopology>
      <iidm:generator id="GEN1" node="0" energySource="OTHER"
          minP="0.0" maxP="200.0" voltageRegulatorOn="true" targetP="100.0"
          targetV="345.0" targetQ="0.0"/>
    </iidm:voltageLevel>
  </iidm:substation>
</iidm:network>"#;
        let mut net = parse_str(xml).expect("NB toggle parse failed");

        // Initially all closed (non-retained) → 1 bus (nodes 0,1,2 merged)
        assert_eq!(net.buses.len(), 1, "all closed → 1 bus");

        // Open BRK1 → splits node 0 from {1,2}
        let sm = net.topology.as_mut().expect("SM present");
        assert!(sm.set_switch_state("BRK1", true), "BRK1 should toggle");

        // Retopologize — returns a new network
        let net2 = surge_topology::rebuild_topology(&net).expect("rebuild_topology failed");
        assert_eq!(net2.buses.len(), 2, "opening BRK1 → 2 buses");
        assert!(net2.topology.as_ref().unwrap().current_mapping().is_some());
    }

    #[test]
    fn test_node_breaker_shunt_tracks_exact_bus_after_split() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_1"
    id="nb_shunt_toggle" caseDate="2024-01-01T00:00:00.000Z" forecastDistance="0"
    sourceFormat="test" minimumValidationLevel="STEADY_STATE_HYPOTHESIS">
  <iidm:substation id="SUB1" name="Station1" country="US">
    <iidm:voltageLevel id="VL1" nominalV="345.0" topologyKind="NODE_BREAKER">
      <iidm:nodeBreakerTopology>
        <iidm:busbarSection id="BBS1" node="0"/>
        <iidm:switch id="BRK1" kind="BREAKER" retained="false" open="false" node1="0" node2="1"/>
        <iidm:busbarSection id="BBS2" node="1"/>
        <iidm:bus v="345.0" angle="0.0" nodes="0,1"/>
      </iidm:nodeBreakerTopology>
      <iidm:shunt id="SH1" node="0" sectionCount="1" voltageRegulatorOn="false">
        <iidm:shuntLinearModel bPerSection="0.000336" gPerSection="0.0" maximumSectionCount="1"/>
      </iidm:shunt>
    </iidm:voltageLevel>
  </iidm:substation>
</iidm:network>"#;
        let mut net = parse_str(xml).expect("NB shunt parse failed");
        assert_eq!(
            net.fixed_shunts.len(),
            1,
            "shunt identity should be preserved"
        );
        assert!(
            (net.buses[0].shunt_susceptance_mvar - 40.0).abs() < 0.1,
            "initial bus shunt should be preserved"
        );

        let sm = net.topology.as_mut().expect("SM present");
        assert!(sm.set_switch_state("BRK1", true), "BRK1 should toggle");

        let net2 = surge_topology::rebuild_topology(&net).expect("rebuild_topology failed");
        assert_eq!(net2.buses.len(), 2, "opening BRK1 should split the bus");
        assert_eq!(
            net2.fixed_shunts.len(),
            1,
            "shunt identity should survive retopology"
        );

        let mapping = &net2
            .topology
            .as_ref()
            .and_then(NodeBreakerTopology::current_mapping)
            .expect("fresh topology reduction");
        let shunt_bus = mapping
            .connectivity_node_to_bus
            .get("VL1_N0")
            .copied()
            .expect("node 0 should remain mapped");
        assert_eq!(net2.fixed_shunts[0].bus, shunt_bus);

        let shunt_host_bus = net2
            .buses
            .iter()
            .find(|bus| bus.number == shunt_bus)
            .expect("host bus present");
        assert!(
            (shunt_host_bus.shunt_susceptance_mvar - 40.0).abs() < 0.1,
            "shunt should remain on the exact split bus"
        );
    }

    /// Parse NB → write → re-parse → verify preservation.
    #[test]
    fn test_node_breaker_roundtrip() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_1"
    id="nb_roundtrip" caseDate="2024-01-01T00:00:00.000Z" forecastDistance="0"
    sourceFormat="test" minimumValidationLevel="STEADY_STATE_HYPOTHESIS">
  <iidm:substation id="SUB1" name="Station1" country="US">
    <iidm:voltageLevel id="VL1" nominalV="345.0" topologyKind="NODE_BREAKER">
      <iidm:nodeBreakerTopology>
        <iidm:busbarSection id="BBS1" node="0"/>
        <iidm:switch id="BRK1" kind="BREAKER" retained="false" open="false" node1="0" node2="1"/>
        <iidm:switch id="DIS1" kind="DISCONNECTOR" retained="false" open="false" node1="1" node2="2"/>
        <iidm:bus v="345.0" angle="0.0" nodes="0,1,2"/>
      </iidm:nodeBreakerTopology>
      <iidm:generator id="GEN1" node="2" energySource="OTHER"
          minP="0.0" maxP="200.0" voltageRegulatorOn="true" targetP="100.0"
          targetV="345.0" targetQ="0.0"/>
      <iidm:load id="LOAD1" node="0" loadType="UNDEFINED" p0="50.0" q0="20.0"/>
    </iidm:voltageLevel>
    <iidm:voltageLevel id="VL2" nominalV="138.0" topologyKind="NODE_BREAKER">
      <iidm:nodeBreakerTopology>
        <iidm:busbarSection id="BBS2" node="0"/>
        <iidm:switch id="BRK2" kind="BREAKER" retained="false" open="false" node1="0" node2="1"/>
        <iidm:bus v="138.0" angle="-1.5" nodes="0,1"/>
      </iidm:nodeBreakerTopology>
      <iidm:load id="LOAD2" node="1" loadType="UNDEFINED" p0="40.0" q0="15.0"/>
    </iidm:voltageLevel>
    <iidm:twoWindingsTransformer id="TF1"
        r="0.0" x="5.0" b="0.0" g="0.0"
        ratedU1="345.0" ratedU2="138.0"
        voltageLevelId1="VL1" node1="0"
        voltageLevelId2="VL2" node2="0"/>
  </iidm:substation>
</iidm:network>"#;
        let net1 = parse_str(xml).expect("NB roundtrip parse 1 failed");
        assert!(net1.topology.is_some());

        // Write → string
        let output = to_string(&net1).expect("NB write failed");

        // Verify NB markers in output
        assert!(
            output.contains("NODE_BREAKER"),
            "output should contain NODE_BREAKER"
        );
        assert!(
            output.contains("nodeBreakerTopology"),
            "output should contain nodeBreakerTopology"
        );
        assert!(
            output.contains("busbarSection"),
            "output should contain busbarSection"
        );
        assert!(
            output.contains("BREAKER"),
            "output should contain BREAKER kind"
        );
        assert!(output.contains(r#"generator id="GEN1""#));
        assert!(output.contains(r#"load id="LOAD1""#));
        assert!(output.contains(r#"load id="LOAD2""#));

        // Re-parse
        let net2 = parse_str(&output).expect("NB roundtrip parse 2 failed");

        // Verify preservation
        assert_eq!(net2.buses.len(), net1.buses.len(), "bus count preserved");
        assert_eq!(
            net2.generators.len(),
            net1.generators.len(),
            "gen count preserved"
        );
        assert_eq!(
            net2.branches.len(),
            net1.branches.len(),
            "branch count preserved"
        );
        assert!(net2.topology.is_some(), "SM preserved on re-parse");
        let sm2 = net2.topology.as_ref().unwrap();
        let sm1 = net1.topology.as_ref().unwrap();
        assert_eq!(
            sm2.switches.len(),
            sm1.switches.len(),
            "switch count preserved"
        );
    }

    /// Mixed topology: one NB VL + one BB VL in same file. Verifies both
    /// topology kinds parse independently and coexist in the same network.
    #[test]
    fn test_mixed_topology() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_1"
    id="mixed_topo" caseDate="2024-01-01T00:00:00.000Z" forecastDistance="0"
    sourceFormat="test" minimumValidationLevel="STEADY_STATE_HYPOTHESIS">
  <iidm:substation id="SUB1" name="Station1" country="US">
    <iidm:voltageLevel id="VL1" nominalV="345.0" topologyKind="NODE_BREAKER">
      <iidm:nodeBreakerTopology>
        <iidm:busbarSection id="BBS1" node="0"/>
        <iidm:switch id="BRK1" kind="BREAKER" retained="false" open="false" node1="0" node2="1"/>
        <iidm:bus v="345.0" angle="0.0" nodes="0,1"/>
      </iidm:nodeBreakerTopology>
      <iidm:generator id="GEN1" node="1" energySource="OTHER"
          minP="0.0" maxP="200.0" voltageRegulatorOn="true" targetP="100.0"
          targetV="345.0" targetQ="0.0"/>
    </iidm:voltageLevel>
  </iidm:substation>
  <iidm:substation id="SUB2" name="Station2" country="US">
    <iidm:voltageLevel id="VL2" nominalV="138.0" topologyKind="BUS_BREAKER">
      <iidm:busBreakerTopology>
        <iidm:bus id="BUS_BB" v="138.0" angle="-1.0"/>
      </iidm:busBreakerTopology>
      <iidm:load id="LOAD_BB" bus="BUS_BB" connectableBus="BUS_BB"
          loadType="UNDEFINED" p0="60.0" q0="25.0"/>
    </iidm:voltageLevel>
  </iidm:substation>
</iidm:network>"#;
        let net = parse_str(xml).expect("mixed topology parse failed");

        // NB VL1: nodes 0,1 closed → 1 bus. BB VL2: 1 bus. Total = 2.
        assert_eq!(net.buses.len(), 2, "mixed: 1 NB bus + 1 BB bus = 2");

        // Generator from NB VL
        assert_eq!(net.generators.len(), 1);
        assert!((net.generators[0].p - 100.0).abs() < 1e-6);

        // Load from BB VL
        let bb_bus = net
            .buses
            .iter()
            .find(|b| (b.base_kv - 138.0).abs() < 1.0)
            .unwrap();
        let bb_load_mw: f64 = net
            .loads
            .iter()
            .filter(|l| l.bus == bb_bus.number)
            .map(|l| l.active_power_demand_mw)
            .sum();
        assert!((bb_load_mw - 60.0).abs() < 1e-6, "BB load should be 60 MW");

        // NodeBreakerTopology present (because there's at least one NB VL)
        assert!(net.topology.is_some());
    }
}
