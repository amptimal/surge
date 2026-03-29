// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! IEC 61850-6 Substation Configuration Language (SCL) parser.
//!
//! Parses `.scd`, `.ssd`, `.icd`, and `.cid` SCL XML files per:
//! - **IEC 61850-6:2009** — Configuration description language for communication
//!   in electrical substations related to IEDs
//! - **IEC 61850-6:2019** (Edition 2.1) — Updated SCL schema
//!
//! ## What this parses
//!
//! The SCL file contains the complete IED configuration of a substation,
//! including:
//! - Substation topology (voltage levels, bays, busbars, connectivity nodes)
//! - IED definitions (device types, logical nodes, data objects)
//! - Communication (GOOSE, Sampled Values, MMS)
//! - Protection function parameters (PTOC, PDIS, PDIF settings)
//!
//! ## Logical Node class → Surge protection type mapping
//!
//! | IEC 61850 LN Class | ANSI Function | Surge Type |
//! |---------------------|--------------|------------|
//! | PTOC                | 50/51        | `OvercurrentRelay` |
//! | PTOC (directional)  | 67           | `DirectionalOcRelay` |
//! | PDIS                | 21           | `DistanceRelay` |
//! | PDIF                | 87           | `DigitalRelay` (F87) |
//! | PDIF (TRPD)         | 87T          | Transformer differential |
//! | PDIF (MMXU)         | 87M          | Motor differential |
//! | RBRF                | 50BF         | `BreakerFailureRelay` |
//!
//! ## SCL file structure
//!
//! ```xml
//! <SCL xmlns="http://www.iec.ch/61850/2003/SCL">
//!   <Header id="..." version="..." revision="..."/>
//!   <Substation name="...">
//!     <VoltageLevel name="...">
//!       <Bay name="...">
//!         <ConductingEquipment type="CBR" name="..."/>
//!       </Bay>
//!     </VoltageLevel>
//!   </Substation>
//!   <IED name="..." type="..." manufacturer="...">
//!     <AccessPoint name="...">
//!       <Server>
//!         <LDevice inst="...">
//!           <LN0 lnClass="LLN0" inst="" lnType="..."/>
//!           <LN lnClass="PTOC" inst="1" lnType="...">
//!             <DOI name="StrVal">
//!               <DAI name="setMag">
//!                 <Val>1.2</Val>
//!               </DAI>
//!             </DOI>
//!           </LN>
//!         </LDevice>
//!       </Server>
//!     </AccessPoint>
//!   </IED>
//! </SCL>
//! ```

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

use surge_network::Network;
use surge_network::network::{Branch, BranchType, Bus, BusType, TransformerData};

// ─────────────────────────────────────────────────────────────────────────────
// SCL data structures
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed SCL document.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SclDocument {
    /// SCL header metadata.
    pub header: SclHeader,
    /// Substations defined in this SCL file.
    pub substations: Vec<SclSubstation>,
    /// IED (Intelligent Electronic Device) definitions.
    pub ieds: Vec<SclIed>,
    /// Communication section (GOOSE / SV / MMS bindings — optional).
    pub communication: Option<SclCommunication>,
}

/// SCL Header metadata.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SclHeader {
    pub id: String,
    pub version: Option<String>,
    pub revision: Option<String>,
    pub tool_id: Option<String>,
}

/// A substation in the SCL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclSubstation {
    pub name: String,
    pub desc: Option<String>,
    pub voltage_levels: Vec<SclVoltageLevel>,
}

/// A voltage level (e.g., 345kV bus, 138kV bus) within a substation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclVoltageLevel {
    pub name: String,
    /// Nominal voltage in kV.
    pub nominal_kv: Option<f64>,
    pub bays: Vec<SclBay>,
}

/// A bay (feeder, transformer, busbar section) within a voltage level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclBay {
    pub name: String,
    pub desc: Option<String>,
    pub equipment: Vec<SclEquipment>,
    /// Power transformers in this bay.
    pub transformers: Vec<SclPowerTransformer>,
}

/// Conducting equipment type per IEC 61850-6 Table 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EquipmentType {
    /// Circuit breaker (CBR).
    CircuitBreaker,
    /// Disconnector / isolator (DIS).
    Disconnector,
    /// Earthing switch (ERS).
    EarthingSwitch,
    /// Current transformer (CTR).
    CurrentTransformer,
    /// Voltage transformer (VTR).
    VoltageTransformer,
    /// Shunt reactor (REA).
    ShuntReactor,
    /// Capacitor bank (CAP).
    Capacitor,
    /// Unknown / other.
    Other,
}

impl EquipmentType {
    fn from_str(s: &str) -> Self {
        match s {
            "CBR" => EquipmentType::CircuitBreaker,
            "DIS" => EquipmentType::Disconnector,
            "ERS" => EquipmentType::EarthingSwitch,
            "CTR" => EquipmentType::CurrentTransformer,
            "VTR" => EquipmentType::VoltageTransformer,
            "REA" => EquipmentType::ShuntReactor,
            "CAP" => EquipmentType::Capacitor,
            _ => EquipmentType::Other,
        }
    }
}

/// A conducting equipment item (breaker, CT, disconnect, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclEquipment {
    pub name: String,
    pub eq_type: EquipmentType,
    pub desc: Option<String>,
    /// Bus (connectivity node) this equipment connects to.
    pub connected_to: Vec<String>,
}

/// A power transformer in the SCL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclPowerTransformer {
    pub name: String,
    pub desc: Option<String>,
    /// Transformer windings.
    pub windings: Vec<SclTransformerWinding>,
}

/// One winding of a power transformer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclTransformerWinding {
    pub name: String,
    /// Rated voltage (kV).
    pub rated_kv: Option<f64>,
    /// Rated MVA.
    pub rated_mva: Option<f64>,
    /// Terminal (connectivity node) this winding connects to.
    pub terminal: Option<String>,
}

/// An IED in the SCL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclIed {
    pub name: String,
    pub ied_type: Option<String>,
    pub manufacturer: Option<String>,
    pub desc: Option<String>,
    /// Logical devices hosted by this IED.
    pub logical_devices: Vec<SclLogicalDevice>,
}

/// A logical device (LD) within an IED.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclLogicalDevice {
    pub inst: String,
    pub desc: Option<String>,
    /// Logical nodes within this device.
    pub logical_nodes: Vec<SclLogicalNode>,
}

/// IEC 61850 protection logical node class.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LnClass {
    /// PTOC: Time overcurrent protection
    Ptoc,
    /// PDIS: Distance protection
    Pdis,
    /// PDIF: Differential protection
    Pdif,
    /// RBRF: Breaker failure protection
    Rbrf,
    /// PSCH: Protection scheme (pilot, permissive)
    Psch,
    /// RREC: Auto-reclose
    Rrec,
    /// MMXU: Measurement unit (metering)
    Mmxu,
    /// LLN0: Logical node zero (device-level data)
    Lln0,
    /// XCBR: Circuit breaker logical node
    Xcbr,
    /// Other (includes all non-protection LNs)
    Other(String),
}

impl LnClass {
    fn from_str(s: &str) -> Self {
        match s {
            "PTOC" => LnClass::Ptoc,
            "PDIS" => LnClass::Pdis,
            "PDIF" => LnClass::Pdif,
            "RBRF" => LnClass::Rbrf,
            "PSCH" => LnClass::Psch,
            "RREC" => LnClass::Rrec,
            "MMXU" => LnClass::Mmxu,
            "LLN0" => LnClass::Lln0,
            "XCBR" => LnClass::Xcbr,
            _ => LnClass::Other(s.to_string()),
        }
    }

    /// Returns true if this is a protection-related logical node.
    pub fn is_protection(&self) -> bool {
        matches!(
            self,
            LnClass::Ptoc | LnClass::Pdis | LnClass::Pdif | LnClass::Rbrf
        )
    }
}

/// A logical node within a logical device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclLogicalNode {
    pub ln_class: LnClass,
    pub inst: String,
    pub ln_type: Option<String>,
    pub desc: Option<String>,
    /// Data object instances (settings values).
    /// Key = data object name (e.g., "StrVal"), Value = settings map.
    pub data_objects: HashMap<String, SclDataObject>,
}

/// A data object instance with its data attribute values.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SclDataObject {
    pub name: String,
    /// Data attribute values: attribute name → value string.
    pub attributes: HashMap<String, String>,
}

/// SCL protection function extracted from a PTOC/PDIS/PDIF logical node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SclProtectionFunction {
    /// IED name.
    pub ied_name: String,
    /// Logical device instance.
    pub ld_inst: String,
    /// Logical node class.
    pub ln_class: LnClass,
    /// Logical node instance number.
    pub ln_inst: String,
    /// Pickup current (A) — from StrVal or equivalent DO.
    pub pickup_current_a: Option<f64>,
    /// Time dial setting — from TmMult or TmStrMul DO.
    pub tds: Option<f64>,
    /// Curve type name (as string from SCL, not enum).
    pub curve_type: Option<String>,
    /// Instantaneous pickup — from OpDlTmms (in ms → s).
    pub instantaneous_pickup_a: Option<f64>,
    /// Zone 1 reach (Ω primary) — for PDIS only.
    pub zone1_reach_ohm: Option<f64>,
    /// Zone 2 reach (Ω primary) — for PDIS only.
    pub zone2_reach_ohm: Option<f64>,
    /// Zone 2 time delay (s) — for PDIS only.
    pub zone2_delay_s: Option<f64>,
    /// Zone 3 reach (Ω primary) — for PDIS only.
    pub zone3_reach_ohm: Option<f64>,
    /// Zone 3 time delay (s) — for PDIS only.
    pub zone3_delay_s: Option<f64>,
}

/// SCL communication section (simplified).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SclCommunication {
    /// GOOSE publisher/subscriber bindings: IED name → list of GOOSE IDs.
    pub goose_bindings: HashMap<String, Vec<String>>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Parser
// ─────────────────────────────────────────────────────────────────────────────

/// Error type for SCL parsing.
#[derive(Debug, thiserror::Error)]
pub enum SclError {
    #[error("XML parse error: {0}")]
    XmlError(#[from] quick_xml::Error),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Invalid SCL structure: {0}")]
    InvalidStructure(String),
    #[error("UTF-8 error: {0}")]
    Utf8Error(#[from] std::str::Utf8Error),
}

/// Parse an SCL file from a string.
///
/// Parses the XML structure and extracts substations, IEDs, and protection
/// function settings.
///
/// # Arguments
/// - `xml` - SCL file content as a string
///
/// # Returns
/// `SclDocument` with all parsed data.
pub fn parse_scl(xml: &str) -> Result<SclDocument, SclError> {
    let mut doc = SclDocument::default();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    // Parse state
    let mut context_stack: Vec<String> = Vec::new();
    let mut current_substation: Option<SclSubstation> = None;
    let mut current_vl: Option<SclVoltageLevel> = None;
    let mut current_bay: Option<SclBay> = None;
    let mut current_ied: Option<SclIed> = None;
    let mut current_ld: Option<SclLogicalDevice> = None;
    let mut current_ln: Option<SclLogicalNode> = None;
    let mut current_doi: Option<SclDataObject> = None;
    let mut current_dai_name: Option<String> = None;
    let mut current_xfmr: Option<SclPowerTransformer> = None;

    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf);
        let is_empty_elem = matches!(event, Ok(Event::Empty(_)));
        match event {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let tag = std::str::from_utf8(e.name().as_ref())?.to_string();
                // Strip namespace prefix if present (e.g., "scl:SCL" → "SCL")
                let tag = tag.rsplit(':').next().unwrap_or(&tag).to_string();
                let attrs = parse_attrs(e)?;

                context_stack.push(tag.clone());

                match tag.as_str() {
                    "Header" => {
                        doc.header.id = attrs.get("id").cloned().unwrap_or_default();
                        doc.header.version = attrs.get("version").cloned();
                        doc.header.revision = attrs.get("revision").cloned();
                        doc.header.tool_id = attrs.get("toolID").cloned();
                    }
                    "Substation" => {
                        current_substation = Some(SclSubstation {
                            name: attrs.get("name").cloned().unwrap_or_default(),
                            desc: attrs.get("desc").cloned(),
                            voltage_levels: Vec::new(),
                        });
                    }
                    "VoltageLevel" => {
                        let kv = attrs
                            .get("nominalVoltage")
                            .or_else(|| attrs.get("Volt"))
                            .and_then(|v| v.parse().ok());
                        current_vl = Some(SclVoltageLevel {
                            name: attrs.get("name").cloned().unwrap_or_default(),
                            nominal_kv: kv,
                            bays: Vec::new(),
                        });
                    }
                    "Bay" => {
                        current_bay = Some(SclBay {
                            name: attrs.get("name").cloned().unwrap_or_default(),
                            desc: attrs.get("desc").cloned(),
                            equipment: Vec::new(),
                            transformers: Vec::new(),
                        });
                    }
                    "ConductingEquipment" => {
                        let eq = SclEquipment {
                            name: attrs.get("name").cloned().unwrap_or_default(),
                            eq_type: attrs
                                .get("type")
                                .map(|t| EquipmentType::from_str(t))
                                .unwrap_or(EquipmentType::Other),
                            desc: attrs.get("desc").cloned(),
                            connected_to: Vec::new(),
                        };
                        if let Some(bay) = current_bay.as_mut() {
                            bay.equipment.push(eq);
                        }
                    }
                    "PowerTransformer" => {
                        current_xfmr = Some(SclPowerTransformer {
                            name: attrs.get("name").cloned().unwrap_or_default(),
                            desc: attrs.get("desc").cloned(),
                            windings: Vec::new(),
                        });
                    }
                    "TransformerWinding" => {
                        let winding = SclTransformerWinding {
                            name: attrs.get("name").cloned().unwrap_or_default(),
                            rated_kv: attrs
                                .get("ratedVoltage")
                                .or_else(|| attrs.get("Volt"))
                                .and_then(|v| v.parse().ok()),
                            rated_mva: attrs
                                .get("ratedS")
                                .or_else(|| attrs.get("MVA"))
                                .and_then(|v| v.parse().ok()),
                            terminal: None,
                        };
                        if let Some(xfmr) = current_xfmr.as_mut() {
                            xfmr.windings.push(winding);
                        }
                    }
                    "IED" => {
                        current_ied = Some(SclIed {
                            name: attrs.get("name").cloned().unwrap_or_default(),
                            ied_type: attrs.get("type").cloned(),
                            manufacturer: attrs.get("manufacturer").cloned(),
                            desc: attrs.get("desc").cloned(),
                            logical_devices: Vec::new(),
                        });
                    }
                    "LDevice" => {
                        current_ld = Some(SclLogicalDevice {
                            inst: attrs.get("inst").cloned().unwrap_or_default(),
                            desc: attrs.get("desc").cloned(),
                            logical_nodes: Vec::new(),
                        });
                    }
                    "LN" | "LN0" => {
                        let class_str = attrs.get("lnClass").map(String::as_str).unwrap_or("Other");
                        current_ln = Some(SclLogicalNode {
                            ln_class: LnClass::from_str(class_str),
                            inst: attrs.get("inst").cloned().unwrap_or_default(),
                            ln_type: attrs.get("lnType").cloned(),
                            desc: attrs.get("desc").cloned(),
                            data_objects: HashMap::new(),
                        });
                    }
                    "DOI" => {
                        let doi_name = attrs.get("name").cloned().unwrap_or_default();
                        current_doi = Some(SclDataObject {
                            name: doi_name,
                            attributes: HashMap::new(),
                        });
                    }
                    "DAI" => {
                        current_dai_name = attrs.get("name").cloned();
                    }
                    _ => {}
                }
                // Empty elements have no End event — pop context immediately
                if is_empty_elem {
                    context_stack.pop();
                }
            }
            Ok(Event::Text(ref e)) => {
                // Text content — used for Val elements inside DAI
                if context_stack.last().map(|s| s.as_str()) == Some("Val")
                    && let (Some(doi), Some(dai_name)) =
                        (current_doi.as_mut(), current_dai_name.clone())
                {
                    let text = e.unescape().unwrap_or_default().to_string();
                    doi.attributes.insert(dai_name, text);
                }
            }
            Ok(Event::End(ref e)) => {
                let tag = std::str::from_utf8(e.name().as_ref())?.to_string();
                let tag = tag.rsplit(':').next().unwrap_or(&tag).to_string();
                context_stack.pop();

                match tag.as_str() {
                    "Substation" => {
                        if let Some(ss) = current_substation.take() {
                            doc.substations.push(ss);
                        }
                    }
                    "VoltageLevel" => {
                        if let (Some(ss), Some(vl)) =
                            (current_substation.as_mut(), current_vl.take())
                        {
                            ss.voltage_levels.push(vl);
                        }
                    }
                    "Bay" => {
                        if let (Some(vl), Some(bay)) = (current_vl.as_mut(), current_bay.take()) {
                            vl.bays.push(bay);
                        }
                    }
                    "PowerTransformer" => {
                        if let (Some(bay), Some(xfmr)) = (current_bay.as_mut(), current_xfmr.take())
                        {
                            bay.transformers.push(xfmr);
                        }
                    }
                    "IED" => {
                        if let Some(ied) = current_ied.take() {
                            doc.ieds.push(ied);
                        }
                    }
                    "LDevice" => {
                        if let (Some(ied), Some(ld)) = (current_ied.as_mut(), current_ld.take()) {
                            ied.logical_devices.push(ld);
                        }
                    }
                    "LN" | "LN0" => {
                        if let (Some(ld), Some(ln)) = (current_ld.as_mut(), current_ln.take()) {
                            ld.logical_nodes.push(ln);
                        }
                        current_doi = None;
                    }
                    "DOI" => {
                        if let (Some(ln), Some(doi)) = (current_ln.as_mut(), current_doi.take()) {
                            ln.data_objects.insert(doi.name.clone(), doi);
                        }
                        current_dai_name = None;
                    }
                    "DAI" => {
                        current_dai_name = None;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(SclError::XmlError(e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(doc)
}

/// Parse SCL from a file path.
pub fn parse_scl_file(path: &std::path::Path) -> Result<SclDocument, SclError> {
    let content = std::fs::read_to_string(path)?;
    parse_scl(&content)
}

fn parse_attrs(e: &quick_xml::events::BytesStart<'_>) -> Result<HashMap<String, String>, SclError> {
    let mut map = HashMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(quick_xml::Error::from)?;
        let key = std::str::from_utf8(attr.key.as_ref())?.to_string();
        let key = key.rsplit(':').next().unwrap_or(&key).to_string();
        let value = std::str::from_utf8(&attr.value)?.to_string();
        map.insert(key, value);
    }
    Ok(map)
}

// ─────────────────────────────────────────────────────────────────────────────
// Network extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Extract a power network topology from an SCL document.
///
/// Creates buses from voltage levels and branches from transformers.
/// Suitable for building a skeleton network for power flow studies.
///
/// # Limitations
/// - SCL does not contain power flow parameters (R, X, B).  The resulting
///   network will have zero impedances — you must supplement from SCADA data
///   or equipment databases.
/// - Load/generation data is not in SCL (it's in the measurement/control config).
pub fn scl_to_network(doc: &SclDocument) -> Network {
    let mut network = Network::new("SCL");
    let mut bus_id = 1u32;
    let mut vl_to_bus = HashMap::<(String, String), u32>::new();
    let mut transformer_buses = HashMap::<String, BTreeSet<u32>>::new();
    let mut transformer_ratings = HashMap::<String, f64>::new();

    for substation in &doc.substations {
        for vl in &substation.voltage_levels {
            let kv = vl.nominal_kv.unwrap_or(0.0);
            let mut bus = Bus::new(bus_id, BusType::PQ, kv);
            bus.name = format!("{}/{}", substation.name, vl.name);
            network.buses.push(bus);
            vl_to_bus.insert((substation.name.clone(), vl.name.clone()), bus_id);
            bus_id += 1;
        }
    }

    for substation in &doc.substations {
        for vl in &substation.voltage_levels {
            let Some(&bus_num) = vl_to_bus.get(&(substation.name.clone(), vl.name.clone())) else {
                continue;
            };
            for bay in &vl.bays {
                for xfmr in &bay.transformers {
                    let key = format!("{}/{}", substation.name, xfmr.name);
                    transformer_buses
                        .entry(key.clone())
                        .or_default()
                        .insert(bus_num);
                    if let Some(rated_mva) = xfmr
                        .windings
                        .iter()
                        .filter_map(|w| w.rated_mva)
                        .find(|&mva| mva > 0.0)
                    {
                        transformer_ratings
                            .entry(key)
                            .and_modify(|existing| *existing = existing.min(rated_mva))
                            .or_insert(rated_mva);
                    }
                }
            }
        }
    }

    for (transformer_id, buses) in transformer_buses {
        if buses.len() < 2 {
            continue;
        }
        let mut bus_iter = buses.into_iter();
        let Some(anchor_bus) = bus_iter.next() else {
            continue;
        };
        for (idx, other_bus) in bus_iter.enumerate() {
            let mut branch = Branch::new_line(anchor_bus, other_bus, 0.0, 0.0, 0.0);
            branch.branch_type = BranchType::Transformer;
            branch.circuit = (idx + 1).to_string();
            branch.rating_a_mva = transformer_ratings
                .get(&transformer_id)
                .copied()
                .unwrap_or(0.0);
            branch.transformer_data = Some(TransformerData {
                parent_transformer_id: Some(transformer_id.clone()),
                winding_number: Some((idx + 2) as u8),
                winding_rated_mva: transformer_ratings.get(&transformer_id).copied(),
                ..TransformerData::default()
            });
            network.branches.push(branch);
        }
    }
    network
}

/// Extract protection functions from an SCL document.
///
/// Returns a flat list of all PTOC, PDIS, and PDIF logical nodes found
/// across all IEDs, with available settings extracted from DOI/DAI values.
pub fn scl_to_protection_relays(doc: &SclDocument) -> Vec<SclProtectionFunction> {
    let mut functions = Vec::new();

    for ied in &doc.ieds {
        for ld in &ied.logical_devices {
            for ln in &ld.logical_nodes {
                if !ln.ln_class.is_protection() {
                    continue;
                }

                let mut pf = SclProtectionFunction {
                    ied_name: ied.name.clone(),
                    ld_inst: ld.inst.clone(),
                    ln_class: ln.ln_class.clone(),
                    ln_inst: ln.inst.clone(),
                    pickup_current_a: None,
                    tds: None,
                    curve_type: None,
                    instantaneous_pickup_a: None,
                    zone1_reach_ohm: None,
                    zone2_reach_ohm: None,
                    zone2_delay_s: None,
                    zone3_reach_ohm: None,
                    zone3_delay_s: None,
                };

                // Extract common PTOC settings
                // StrVal (pickup current setpoint)
                if let Some(doi) = ln.data_objects.get("StrVal") {
                    pf.pickup_current_a = doi
                        .attributes
                        .get("setMag")
                        .or_else(|| doi.attributes.get("f"))
                        .and_then(|v| v.parse().ok());
                }
                // TmMult (time multiplier / TDS)
                if let Some(doi) = ln
                    .data_objects
                    .get("TmMult")
                    .or_else(|| ln.data_objects.get("TmStrMul"))
                {
                    pf.tds = doi
                        .attributes
                        .get("setMag")
                        .or_else(|| doi.attributes.get("f"))
                        .and_then(|v| v.parse().ok());
                }
                // Curve type (vendor-specific): look for "TmACrv" or "CrvSat"
                if let Some(doi) = ln
                    .data_objects
                    .get("TmACrv")
                    .or_else(|| ln.data_objects.get("CrvSat"))
                {
                    pf.curve_type = doi
                        .attributes
                        .get("setVal")
                        .cloned()
                        .or_else(|| doi.attributes.get("Enum").cloned());
                }

                // PDIS: zone reach settings
                if ln.ln_class == LnClass::Pdis {
                    if let Some(doi) = ln.data_objects.get("Str1") {
                        pf.zone1_reach_ohm =
                            doi.attributes.get("setMag").and_then(|v| v.parse().ok());
                    }
                    if let Some(doi) = ln.data_objects.get("Str2") {
                        pf.zone2_reach_ohm =
                            doi.attributes.get("setMag").and_then(|v| v.parse().ok());
                    }
                    if let Some(doi) = ln.data_objects.get("Op2DlTmms") {
                        pf.zone2_delay_s = doi
                            .attributes
                            .get("setVal")
                            .and_then(|v| v.parse::<f64>().ok())
                            .map(|ms| ms / 1000.0);
                    }
                    if let Some(doi) = ln.data_objects.get("Str3") {
                        pf.zone3_reach_ohm =
                            doi.attributes.get("setMag").and_then(|v| v.parse().ok());
                    }
                    if let Some(doi) = ln.data_objects.get("Op3DlTmms") {
                        pf.zone3_delay_s = doi
                            .attributes
                            .get("setVal")
                            .and_then(|v| v.parse::<f64>().ok())
                            .map(|ms| ms / 1000.0);
                    }
                }

                functions.push(pf);
            }
        }
    }

    functions
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid SCL document for testing.
    const MINIMAL_SCL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<SCL xmlns="http://www.iec.ch/61850/2003/SCL" version="2007">
  <Header id="test-scd" version="1" revision="A" toolID="TestTool"/>
  <Substation name="SUB1" desc="Test Substation">
    <VoltageLevel name="138kV">
      <Bay name="FDR1">
        <ConductingEquipment type="CBR" name="CB1"/>
      </Bay>
    </VoltageLevel>
    <VoltageLevel name="13.8kV">
      <Bay name="BUS1">
      </Bay>
    </VoltageLevel>
  </Substation>
  <IED name="SEL_351A_FDR1" type="SEL-351A" manufacturer="SEL" desc="Feeder 1 relay">
    <AccessPoint name="S1">
      <Server>
        <LDevice inst="PROT">
          <LN0 lnClass="LLN0" inst="" lnType="LLN0"/>
          <LN lnClass="PTOC" inst="1" lnType="PTOC1">
            <DOI name="StrVal">
              <DAI name="setMag">
                <Val>5.0</Val>
              </DAI>
            </DOI>
            <DOI name="TmMult">
              <DAI name="setMag">
                <Val>0.5</Val>
              </DAI>
            </DOI>
          </LN>
          <LN lnClass="PDIS" inst="1" lnType="PDIS1">
            <DOI name="Str1">
              <DAI name="setMag">
                <Val>2.5</Val>
              </DAI>
            </DOI>
            <DOI name="Str2">
              <DAI name="setMag">
                <Val>8.0</Val>
              </DAI>
            </DOI>
            <DOI name="Op2DlTmms">
              <DAI name="setVal">
                <Val>400</Val>
              </DAI>
            </DOI>
          </LN>
        </LDevice>
      </Server>
    </AccessPoint>
  </IED>
</SCL>"#;

    #[test]
    fn test_parse_header() {
        let doc = parse_scl(MINIMAL_SCL).expect("should parse");
        assert_eq!(doc.header.id, "test-scd");
        assert_eq!(doc.header.version.as_deref(), Some("1"));
        assert_eq!(doc.header.tool_id.as_deref(), Some("TestTool"));
    }

    #[test]
    fn test_parse_substations() {
        let doc = parse_scl(MINIMAL_SCL).expect("should parse");
        assert_eq!(doc.substations.len(), 1);
        assert_eq!(doc.substations[0].name, "SUB1");
        assert_eq!(doc.substations[0].voltage_levels.len(), 2);
    }

    #[test]
    fn test_parse_ied() {
        let doc = parse_scl(MINIMAL_SCL).expect("should parse");
        assert_eq!(doc.ieds.len(), 1);
        let ied = &doc.ieds[0];
        assert_eq!(ied.name, "SEL_351A_FDR1");
        assert_eq!(ied.manufacturer.as_deref(), Some("SEL"));
    }

    #[test]
    fn test_parse_logical_nodes() {
        let doc = parse_scl(MINIMAL_SCL).expect("should parse");
        let ied = &doc.ieds[0];
        assert_eq!(ied.logical_devices.len(), 1);
        let ld = &ied.logical_devices[0];
        // LLN0 + PTOC + PDIS = 3 LNs
        let ptoc = ld
            .logical_nodes
            .iter()
            .find(|ln| ln.ln_class == LnClass::Ptoc);
        assert!(ptoc.is_some(), "Should have PTOC logical node");
        let pdis = ld
            .logical_nodes
            .iter()
            .find(|ln| ln.ln_class == LnClass::Pdis);
        assert!(pdis.is_some(), "Should have PDIS logical node");
    }

    #[test]
    fn test_protection_extraction_ptoc() {
        let doc = parse_scl(MINIMAL_SCL).expect("should parse");
        let funcs = scl_to_protection_relays(&doc);
        let ptoc = funcs.iter().find(|f| f.ln_class == LnClass::Ptoc);
        assert!(ptoc.is_some(), "Should extract PTOC");
        let ptoc = ptoc.unwrap();
        assert_eq!(ptoc.ied_name, "SEL_351A_FDR1");
        assert_eq!(ptoc.pickup_current_a, Some(5.0));
        assert_eq!(ptoc.tds, Some(0.5));
    }

    #[test]
    fn test_protection_extraction_pdis() {
        let doc = parse_scl(MINIMAL_SCL).expect("should parse");
        let funcs = scl_to_protection_relays(&doc);
        let pdis = funcs.iter().find(|f| f.ln_class == LnClass::Pdis);
        assert!(pdis.is_some(), "Should extract PDIS");
        let pdis = pdis.unwrap();
        assert_eq!(pdis.zone1_reach_ohm, Some(2.5));
        assert_eq!(pdis.zone2_reach_ohm, Some(8.0));
        assert_eq!(pdis.zone2_delay_s, Some(0.4));
    }

    #[test]
    fn test_scl_to_network_buses() {
        let doc = parse_scl(MINIMAL_SCL).expect("should parse");
        let net = scl_to_network(&doc);
        // Should create one bus per voltage level
        assert_eq!(net.buses.len(), 2, "One bus per voltage level");
    }

    #[test]
    fn test_scl_to_network_extracts_transformer_branches() {
        let doc = SclDocument {
            substations: vec![SclSubstation {
                name: "SUB1".to_string(),
                desc: None,
                voltage_levels: vec![
                    SclVoltageLevel {
                        name: "138kV".to_string(),
                        nominal_kv: Some(138.0),
                        bays: vec![SclBay {
                            name: "HV".to_string(),
                            desc: None,
                            equipment: vec![],
                            transformers: vec![SclPowerTransformer {
                                name: "TX1".to_string(),
                                desc: None,
                                windings: vec![SclTransformerWinding {
                                    name: "W1".to_string(),
                                    rated_kv: Some(138.0),
                                    rated_mva: Some(50.0),
                                    terminal: None,
                                }],
                            }],
                        }],
                    },
                    SclVoltageLevel {
                        name: "13.8kV".to_string(),
                        nominal_kv: Some(13.8),
                        bays: vec![SclBay {
                            name: "LV".to_string(),
                            desc: None,
                            equipment: vec![],
                            transformers: vec![SclPowerTransformer {
                                name: "TX1".to_string(),
                                desc: None,
                                windings: vec![SclTransformerWinding {
                                    name: "W2".to_string(),
                                    rated_kv: Some(13.8),
                                    rated_mva: Some(50.0),
                                    terminal: None,
                                }],
                            }],
                        }],
                    },
                ],
            }],
            ..SclDocument::default()
        };

        let net = scl_to_network(&doc);
        assert_eq!(net.buses.len(), 2);
        assert_eq!(
            net.branches.len(),
            1,
            "shared transformer should create one branch"
        );
        assert_eq!(net.branches[0].branch_type, BranchType::Transformer);
        assert_eq!(net.branches[0].rating_a_mva, 50.0);
    }

    #[test]
    fn test_empty_scl() {
        let empty = r#"<?xml version="1.0"?><SCL xmlns="http://www.iec.ch/61850/2003/SCL"><Header id="x"/></SCL>"#;
        let doc = parse_scl(empty).expect("should parse empty SCL");
        assert!(doc.substations.is_empty());
        assert!(doc.ieds.is_empty());
    }
}
