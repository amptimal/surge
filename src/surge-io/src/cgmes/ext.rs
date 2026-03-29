// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES extension profile reader — SC, DY, GL, TPBD profiles.
//!
//! Extends the base CGMES/CIM parser (cim.rs) with additional profile support:
//! - **SC**   — Short-Circuit: fault parameters (R0, X0, R2, X2)
//! - **DY**   — Dynamics: dynamic model references (governor, exciter, PSS)
//! - **GL**   — Geographical Location: substation coordinates
//! - **TPBD** — Topology Boundary: boundary points for multi-area exchange
//!
//! ## Architecture
//! EQ/SSH profiles are delegated to the existing `cim` module.  The extension
//! profiles are parsed here with lightweight string-scanning over the RDF/XML.
//! CGMES RDF/XML is regular enough that targeted tag-matching is reliable and
//! avoids a full ontology dependency.

use std::collections::HashMap;

use surge_network::Network;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Re-export base parser error so callers only need one import.
// ---------------------------------------------------------------------------
pub use super::Error as CgmesError;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum IoError {
    #[error("CGMES base parse error: {0}")]
    Cgmes(#[from] CgmesError),
    #[error("XML parse error: {0}")]
    Xml(String),
    #[error("missing required attribute: {0}")]
    MissingAttr(String),
}

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// Short-circuit parameters from the SC (Short-Circuit) profile.
///
/// All impedance values are in per-unit on the network base MVA (100 MVA).
#[derive(Debug, Clone, Default)]
pub struct ScProfile {
    /// Zero-sequence resistance (pu)
    pub r0_pu: Option<f64>,
    /// Zero-sequence reactance (pu)
    pub x0_pu: Option<f64>,
    /// Negative-sequence resistance (pu)
    pub r2_pu: Option<f64>,
    /// Negative-sequence reactance (pu)
    pub x2_pu: Option<f64>,
    /// Initial symmetrical short-circuit current (kA)
    pub ikss_ka: Option<f64>,
}

/// Dynamic model reference from the DY (Dynamics) profile.
#[derive(Debug, Clone)]
pub struct DyProfile {
    /// MRID of the associated SynchronousMachine or ExternalNetworkInjection
    pub machine_mrid: String,
    /// Governor model type, e.g. "GovSteamEU", "GovGAST2"
    pub governor_type: Option<String>,
    /// Excitation system type, e.g. "ExcIEEEST1A", "ExcANS"
    pub exciter_type: Option<String>,
    /// Power System Stabiliser type, e.g. "Pss2A", "PssSB4"
    pub pss_type: Option<String>,
}

/// Geographic coordinate record from the GL (Geographical Location) profile.
#[derive(Debug, Clone)]
pub struct GlProfile {
    /// MRID of the Substation whose location this describes
    pub substation_mrid: String,
    /// WGS-84 latitude (degrees)
    pub latitude: f64,
    /// WGS-84 longitude (degrees)
    pub longitude: f64,
}

/// Boundary point from the TPBD (Topology Boundary) profile.
#[derive(Debug, Clone)]
pub struct TpbdProfile {
    /// MRID of the BoundaryPoint element
    pub boundary_point_mrid: String,
    /// MRID of the bus in area A
    pub bus_a_mrid: String,
    /// MRID of the bus in area B
    pub bus_b_mrid: String,
    /// Nominal voltage of the tie-line (kV)
    pub voltage_level_kv: f64,
}

/// CGMES extended dataset combining all profiles.
pub struct CgmesExtDataset {
    /// Assembled network from EQ + SSH profiles
    pub network: Network,
    /// Short-circuit data keyed by equipment MRID
    pub sc_data: HashMap<String, ScProfile>,
    /// Dynamic model references (one per generating unit) — type-name-only summary
    pub dy_data: Vec<DyProfile>,
    /// Full dynamic model parsed from the DY profile (None if DY profile absent or parse failed)
    pub dynamic_model: Option<surge_network::dynamics::DynamicModel>,
    /// Substation coordinates
    pub gl_data: Vec<GlProfile>,
    /// Tie-line boundary points
    pub tpbd_data: Vec<TpbdProfile>,
}

// ---------------------------------------------------------------------------
// Helpers — lightweight XML scanning
// ---------------------------------------------------------------------------

/// Extract the `rdf:about` or `rdf:ID` attribute value from a tag line.
///
/// CGMES RDF/XML marks each described resource with one of:
/// ```xml
/// <cim:ACLineSegment rdf:ID="_abc123">
/// <cim:ACLineSegment rdf:about="#_abc123">
/// ```
fn extract_rdf_id(line: &str) -> Option<String> {
    for attr in &["rdf:ID=\"", "rdf:about=\"", "rdf:ID='", "rdf:about='"] {
        if let Some(pos) = line.find(attr) {
            let start = pos + attr.len();
            let rest = &line[start..];
            let quote_char = if attr.ends_with('"') { '"' } else { '\'' };
            if let Some(end) = rest.find(quote_char) {
                let id = rest[..end].trim_start_matches('#').to_string();
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
    }
    None
}

/// Extract the text content from a simple single-line XML element.
///
/// ```text
/// <cim:ACLineSegment.r0>0.01</cim:ACLineSegment.r0>
/// ```
fn extract_text<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let (Some(s), Some(e)) = (line.find(&open), line.find(&close)) {
        let value_start = s + open.len();
        if value_start <= e {
            return Some(line[value_start..e].trim());
        }
    }
    None
}

/// Parse an f64 from a text node, returning `None` on failure.
fn parse_f64(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// Extract the `rdf:resource` attribute value (MRID reference) from a tag line.
///
/// ```xml
/// <cim:TurbineGovernorDynamics.SynchronousMachineDynamics rdf:resource="#_gen1"/>
/// ```
fn extract_resource(line: &str) -> Option<String> {
    for attr in &["rdf:resource=\"", "rdf:resource='"] {
        if let Some(pos) = line.find(attr) {
            let start = pos + attr.len();
            let rest = &line[start..];
            let quote_char = if attr.ends_with('"') { '"' } else { '\'' };
            if let Some(end) = rest.find(quote_char) {
                let id = rest[..end].trim_start_matches('#').to_string();
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// SC profile parser
// ---------------------------------------------------------------------------

/// Parse the SC (Short-Circuit) profile XML and return a map of equipment
/// MRID → `ScProfile`.
///
/// Handles both `ACLineSegment` and `SynchronousMachine` elements with
/// zero/negative sequence impedance children.
pub fn parse_sc_profile(xml: &str) -> Result<HashMap<String, ScProfile>, IoError> {
    let mut map: HashMap<String, ScProfile> = HashMap::new();
    let mut current_mrid: Option<String> = None;

    for line in xml.lines() {
        let trimmed = line.trim();

        // Opening element that carries an rdf:ID / rdf:about
        if (trimmed.starts_with("<cim:ACLineSegment")
            || trimmed.starts_with("<cim:SynchronousMachine")
            || trimmed.starts_with("<cim:ExternalNetworkInjection")
            || trimmed.starts_with("<cim:PowerTransformerEnd"))
            && let Some(mrid) = extract_rdf_id(trimmed)
        {
            current_mrid = Some(mrid.clone());
            map.entry(mrid).or_default();
        }

        // Zero-sequence resistance
        if let Some(val) = extract_text(trimmed, "cim:ACLineSegment.r0")
            .or_else(|| extract_text(trimmed, "cim:SynchronousMachine.r0"))
            .or_else(|| extract_text(trimmed, "cim:PowerTransformerEnd.r0"))
            && let (Some(mrid), Some(f)) = (&current_mrid, parse_f64(val))
        {
            map.entry(mrid.clone()).or_default().r0_pu = Some(f);
        }

        // Zero-sequence reactance
        if let Some(val) = extract_text(trimmed, "cim:ACLineSegment.x0")
            .or_else(|| extract_text(trimmed, "cim:SynchronousMachine.x0"))
            .or_else(|| extract_text(trimmed, "cim:PowerTransformerEnd.x0"))
            && let (Some(mrid), Some(f)) = (&current_mrid, parse_f64(val))
        {
            map.entry(mrid.clone()).or_default().x0_pu = Some(f);
        }

        // Negative-sequence resistance
        if let Some(val) = extract_text(trimmed, "cim:ACLineSegment.r2")
            .or_else(|| extract_text(trimmed, "cim:SynchronousMachine.r2"))
            && let (Some(mrid), Some(f)) = (&current_mrid, parse_f64(val))
        {
            map.entry(mrid.clone()).or_default().r2_pu = Some(f);
        }

        // Negative-sequence reactance
        if let Some(val) = extract_text(trimmed, "cim:ACLineSegment.x2")
            .or_else(|| extract_text(trimmed, "cim:SynchronousMachine.x2"))
            && let (Some(mrid), Some(f)) = (&current_mrid, parse_f64(val))
        {
            map.entry(mrid.clone()).or_default().x2_pu = Some(f);
        }

        // Initial short-circuit current (kA)
        if let Some(val) = extract_text(trimmed, "cim:ACLineSegment.Ikss")
            .or_else(|| extract_text(trimmed, "cim:SynchronousMachine.ikss"))
            .or_else(|| extract_text(trimmed, "sc:ACLineSegment.ikss"))
            && let (Some(mrid), Some(f)) = (&current_mrid, parse_f64(val))
        {
            map.entry(mrid.clone()).or_default().ikss_ka = Some(f);
        }
    }

    Ok(map)
}

// ---------------------------------------------------------------------------
// DY profile parser
// ---------------------------------------------------------------------------

/// Parse the DY (Dynamics) profile XML and return a list of `DyProfile`
/// records — one per turbine-governor / exciter block found.
///
/// CGMES DY uses typed elements such as:
/// ```xml
/// <cim:GovGAST2 rdf:ID="_gov1">
///   <cim:TurbineGovernorDynamics.SynchronousMachineDynamics rdf:resource="#_smd1"/>
/// </cim:GovGAST2>
/// ```
pub fn parse_dy_profile(xml: &str) -> Result<Vec<DyProfile>, IoError> {
    // Known governor prefixes
    const GOV_PREFIXES: &[&str] = &[
        "cim:GovSteamEU",
        "cim:GovGAST",
        "cim:GovHydro",
        "cim:GovSteamFV",
        "cim:GovCT",
        "cim:GovSteam",
        "cim:GovDum",
    ];
    // Known exciter prefixes
    const EXC_PREFIXES: &[&str] = &[
        "cim:ExcIEEE",
        "cim:ExcANS",
        "cim:ExcBBC",
        "cim:ExcST",
        "cim:ExcAC",
        "cim:ExcDC",
        "cim:ExcELIN",
        "cim:ExcHU",
        "cim:ExcOEX3T",
        "cim:ExcPIC",
        "cim:ExcRQB",
        "cim:ExcSK",
    ];
    // Known PSS prefixes
    const PSS_PREFIXES: &[&str] = &[
        "cim:Pss2",
        "cim:PssIEEE",
        "cim:PssSB",
        "cim:PssWECC",
        "cim:PssPTIST",
        "cim:PssELIN",
    ];

    // Intermediate state
    struct Block {
        kind: String, // "gov" | "exc" | "pss"
        type_name: String,
        machine_mrid: Option<String>,
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut current: Option<Block> = None;

    for line in xml.lines() {
        let trimmed = line.trim();
        // Strip leading '<' so prefixes like "cim:GovGAST2" match "<cim:GovGAST2 …>" lines
        let tag_trimmed = trimmed.trim_start_matches('<');

        // Check if this line opens a known dynamic model element
        let mut matched_kind: Option<(&str, String)> = None;

        for prefix in GOV_PREFIXES {
            if tag_trimmed.starts_with(prefix) {
                let type_name = trimmed
                    .split_whitespace()
                    .next()
                    .unwrap_or(prefix)
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string();
                matched_kind = Some(("gov", type_name));
                break;
            }
        }
        if matched_kind.is_none() {
            for prefix in EXC_PREFIXES {
                if tag_trimmed.starts_with(prefix) {
                    let type_name = trimmed
                        .split_whitespace()
                        .next()
                        .unwrap_or(prefix)
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_string();
                    matched_kind = Some(("exc", type_name));
                    break;
                }
            }
        }
        if matched_kind.is_none() {
            for prefix in PSS_PREFIXES {
                if tag_trimmed.starts_with(prefix) {
                    let type_name = trimmed
                        .split_whitespace()
                        .next()
                        .unwrap_or(prefix)
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_string();
                    matched_kind = Some(("pss", type_name));
                    break;
                }
            }
        }

        if let Some((kind, type_name)) = matched_kind {
            // Flush previous block
            if let Some(blk) = current.take() {
                blocks.push(blk);
            }
            current = Some(Block {
                kind: kind.to_string(),
                type_name,
                machine_mrid: None,
            });
            continue;
        }

        // Machine reference link
        if (trimmed.contains("SynchronousMachineDynamics")
            || trimmed.contains("RotatingMachineDynamics")
            || trimmed.contains("GeneratingUnit"))
            && let (Some(blk), Some(mrid)) = (&mut current, extract_resource(trimmed))
        {
            blk.machine_mrid = Some(mrid);
        }

        // Closing tag — flush
        if (trimmed.starts_with("</cim:Gov")
            || trimmed.starts_with("</cim:Exc")
            || trimmed.starts_with("</cim:Pss")
            || trimmed.starts_with("</cim:Turbine"))
            && let Some(blk) = current.take()
        {
            blocks.push(blk);
        }
    }

    // Flush final block
    if let Some(blk) = current.take() {
        blocks.push(blk);
    }

    // Collate blocks by machine_mrid into DyProfile records
    let mut profiles: HashMap<String, DyProfile> = HashMap::new();

    for blk in blocks {
        let mrid = blk
            .machine_mrid
            .clone()
            .unwrap_or_else(|| format!("unknown_{}", blk.type_name));
        let entry = profiles.entry(mrid.clone()).or_insert_with(|| DyProfile {
            machine_mrid: mrid,
            governor_type: None,
            exciter_type: None,
            pss_type: None,
        });
        match blk.kind.as_str() {
            "gov" => entry.governor_type = Some(blk.type_name),
            "exc" => entry.exciter_type = Some(blk.type_name),
            "pss" => entry.pss_type = Some(blk.type_name),
            _ => {}
        }
    }

    Ok(profiles.into_values().collect())
}

// ---------------------------------------------------------------------------
// GL profile parser
// ---------------------------------------------------------------------------

/// Parse the GL (Geographical Location) profile XML and return a list of
/// `GlProfile` records with substation coordinates.
///
/// CGMES GL uses:
/// ```xml
/// <cim:SubGeographicalRegion rdf:ID="_sub1">
///   <cim:CoordinatePair.xPosition>-97.5</cim:CoordinatePair.xPosition>
///   <cim:CoordinatePair.yPosition>32.8</cim:CoordinatePair.yPosition>
/// </cim:SubGeographicalRegion>
/// ```
/// or the newer `Location` / `PositionPoint` pattern.
pub fn parse_gl_profile(xml: &str) -> Result<Vec<GlProfile>, IoError> {
    let mut profiles: Vec<GlProfile> = Vec::new();
    let mut current_mrid: Option<String> = None;
    let mut current_lon: Option<f64> = None;
    let mut current_lat: Option<f64> = None;

    for line in xml.lines() {
        let trimmed = line.trim();

        // Opening: Substation or Location element
        if trimmed.starts_with("<cim:Substation")
            || trimmed.starts_with("<cim:Location")
            || trimmed.starts_with("<cim:SubGeographicalRegion")
        {
            // Flush previous
            if let (Some(mrid), Some(lat), Some(lon)) =
                (current_mrid.take(), current_lat.take(), current_lon.take())
            {
                profiles.push(GlProfile {
                    substation_mrid: mrid,
                    latitude: lat,
                    longitude: lon,
                });
            } else {
                current_mrid = None;
                current_lat = None;
                current_lon = None;
            }

            if let Some(mrid) = extract_rdf_id(trimmed) {
                current_mrid = Some(mrid);
            }
            continue;
        }

        // xPosition → longitude
        if let Some(val) = extract_text(trimmed, "cim:CoordinatePair.xPosition")
            .or_else(|| extract_text(trimmed, "cim:PositionPoint.xPosition"))
        {
            current_lon = parse_f64(val);
        }

        // yPosition → latitude
        if let Some(val) = extract_text(trimmed, "cim:CoordinatePair.yPosition")
            .or_else(|| extract_text(trimmed, "cim:PositionPoint.yPosition"))
        {
            current_lat = parse_f64(val);
        }

        // Closing tag
        if (trimmed.starts_with("</cim:Substation>")
            || trimmed.starts_with("</cim:Location>")
            || trimmed.starts_with("</cim:SubGeographicalRegion>"))
            && let (Some(mrid), Some(lat), Some(lon)) =
                (current_mrid.take(), current_lat.take(), current_lon.take())
        {
            profiles.push(GlProfile {
                substation_mrid: mrid,
                latitude: lat,
                longitude: lon,
            });
        }
    }

    // Flush tail
    if let (Some(mrid), Some(lat), Some(lon)) = (current_mrid, current_lat, current_lon) {
        profiles.push(GlProfile {
            substation_mrid: mrid,
            latitude: lat,
            longitude: lon,
        });
    }

    Ok(profiles)
}

// ---------------------------------------------------------------------------
// TPBD profile parser
// ---------------------------------------------------------------------------

/// Parse the TPBD (Topology Boundary) profile XML and return a list of
/// `TpbdProfile` boundary-point records.
///
/// ```xml
/// <tp-bd:BoundaryPoint rdf:ID="_bp1">
///   <tp-bd:BoundaryPoint.fromEndIsoCode>A</tp-bd:BoundaryPoint.fromEndIsoCode>
///   <tp-bd:BoundaryPoint.toEndIsoCode>B</tp-bd:BoundaryPoint.toEndIsoCode>
///   <tp-bd:BoundaryPoint.nominalVoltage>400</tp-bd:BoundaryPoint.nominalVoltage>
/// </tp-bd:BoundaryPoint>
/// ```
pub fn parse_tpbd_profile(xml: &str) -> Result<Vec<TpbdProfile>, IoError> {
    let mut profiles: Vec<TpbdProfile> = Vec::new();

    let mut current_mrid: Option<String> = None;
    let mut bus_a: Option<String> = None;
    let mut bus_b: Option<String> = None;
    let mut voltage_kv: Option<f64> = None;

    for line in xml.lines() {
        let trimmed = line.trim();

        // Opening BoundaryPoint element (tp-bd or cim namespace)
        if trimmed.starts_with("<tp-bd:BoundaryPoint") || trimmed.starts_with("<cim:BoundaryPoint")
        {
            // Flush previous
            if let Some(mrid) = current_mrid.take() {
                profiles.push(TpbdProfile {
                    boundary_point_mrid: mrid,
                    bus_a_mrid: bus_a.take().unwrap_or_default(),
                    bus_b_mrid: bus_b.take().unwrap_or_default(),
                    voltage_level_kv: voltage_kv.take().unwrap_or(0.0),
                });
            } else {
                bus_a = None;
                bus_b = None;
                voltage_kv = None;
            }
            current_mrid = extract_rdf_id(trimmed);
            continue;
        }

        // Area A bus reference (from-end)
        if trimmed.contains("BoundaryPoint.fromEndNameTso")
            || trimmed.contains("BoundaryPoint.fromEnd")
        {
            if let Some(res) = extract_resource(trimmed) {
                bus_a = Some(res);
            } else if let Some(val) = extract_text(trimmed, "tp-bd:BoundaryPoint.fromEndNameTso")
                .or_else(|| extract_text(trimmed, "cim:BoundaryPoint.fromEndNameTso"))
            {
                bus_a = Some(val.to_string());
            }
        }

        // Area B bus reference (to-end)
        if trimmed.contains("BoundaryPoint.toEndNameTso") || trimmed.contains("BoundaryPoint.toEnd")
        {
            if let Some(res) = extract_resource(trimmed) {
                bus_b = Some(res);
            } else if let Some(val) = extract_text(trimmed, "tp-bd:BoundaryPoint.toEndNameTso")
                .or_else(|| extract_text(trimmed, "cim:BoundaryPoint.toEndNameTso"))
            {
                bus_b = Some(val.to_string());
            }
        }

        // Nominal voltage
        if let Some(val) = extract_text(trimmed, "tp-bd:BoundaryPoint.nominalVoltage")
            .or_else(|| extract_text(trimmed, "cim:BoundaryPoint.nominalVoltage"))
        {
            voltage_kv = parse_f64(val);
        }

        // Closing tag
        if (trimmed.starts_with("</tp-bd:BoundaryPoint>")
            || trimmed.starts_with("</cim:BoundaryPoint>"))
            && let Some(mrid) = current_mrid.take()
        {
            profiles.push(TpbdProfile {
                boundary_point_mrid: mrid,
                bus_a_mrid: bus_a.take().unwrap_or_default(),
                bus_b_mrid: bus_b.take().unwrap_or_default(),
                voltage_level_kv: voltage_kv.take().unwrap_or(0.0),
            });
        }
    }

    // Flush tail
    if let Some(mrid) = current_mrid {
        profiles.push(TpbdProfile {
            boundary_point_mrid: mrid,
            bus_a_mrid: bus_a.unwrap_or_default(),
            bus_b_mrid: bus_b.unwrap_or_default(),
            voltage_level_kv: voltage_kv.unwrap_or(0.0),
        });
    }

    Ok(profiles)
}

// ---------------------------------------------------------------------------
// Unified entry point
// ---------------------------------------------------------------------------

/// Parse a CGMES dataset from a map of profile name → XML string.
///
/// Supported profile keys: `"EQ"`, `"SSH"`, `"SC"`, `"DY"`, `"GL"`, `"TPBD"`.
/// The `"EQ"` key is required for network assembly; all others are optional.
///
/// # Example
/// ```no_run
/// # use std::collections::HashMap;
/// # use surge_io::cgmes::ext::parse_cgmes_extended;
/// # let eq_xml = String::new();
/// # let sc_xml = String::new();
/// let mut profiles = HashMap::new();
/// profiles.insert("EQ".to_string(), eq_xml);
/// profiles.insert("SC".to_string(), sc_xml);
/// let dataset = parse_cgmes_extended(&profiles).unwrap();
/// ```
pub fn parse_cgmes_extended(
    profiles: &HashMap<String, String>,
) -> Result<CgmesExtDataset, IoError> {
    // --- Base network: delegate to existing CIM parser ---
    // Build a combined ObjMap for EQ+SSH so we can also extract the SM bus map
    // without re-parsing. Collect combined XML for the string-based entry point.
    let mut base_xml = String::new();
    for key in &["EQ", "SSH", "TP", "SV"] {
        if let Some(xml) = profiles.get(*key) {
            base_xml.push_str(xml);
            base_xml.push('\n');
        }
    }

    // Build ObjMap for SM bus map extraction (used by DY parser).
    let sm_bus_map = if !base_xml.trim().is_empty() {
        use super::{ObjMap, build_sm_bus_map, collect_objects};
        let mut objects = ObjMap::new();
        let _ = collect_objects(&base_xml, &mut objects); // ignore parse errors here
        build_sm_bus_map(&objects)
    } else {
        std::collections::HashMap::new()
    };

    let network = if !base_xml.trim().is_empty() {
        super::loads(&base_xml).unwrap_or_else(|_| Network::new("cgmes_ext"))
    } else {
        Network::new("cgmes_ext")
    };

    // --- Extension profiles ---
    let sc_data = if let Some(xml) = profiles.get("SC") {
        parse_sc_profile(xml)?
    } else {
        HashMap::new()
    };

    let dy_data = if let Some(xml) = profiles.get("DY") {
        parse_dy_profile(xml)?
    } else {
        Vec::new()
    };

    // Full dynamic model from DY profile using the new parameter-aware parser.
    let dynamic_model = if let Some(xml) = profiles.get("DY") {
        match super::dynamics::parse_cgmes_dy(&[xml.as_str()], &sm_bus_map) {
            Ok(dm) => {
                tracing::info!(
                    generators = dm.generators.len(),
                    exciters = dm.exciters.len(),
                    governors = dm.governors.len(),
                    pss = dm.pss.len(),
                    "CGMES DY profile parsed"
                );
                Some(dm)
            }
            Err(e) => {
                tracing::warn!(error = %e, "CGMES DY profile parse failed — dynamic_model will be None");
                None
            }
        }
    } else {
        None
    };

    let gl_data = if let Some(xml) = profiles.get("GL") {
        parse_gl_profile(xml)?
    } else {
        Vec::new()
    };

    let tpbd_data = if let Some(xml) = profiles.get("TPBD") {
        parse_tpbd_profile(xml)?
    } else {
        Vec::new()
    };

    Ok(CgmesExtDataset {
        network,
        sc_data,
        dy_data,
        dynamic_model,
        gl_data,
        tpbd_data,
    })
}

// ---------------------------------------------------------------------------
// SC profile writer
// ---------------------------------------------------------------------------

/// Serialise short-circuit data to a CGMES SC profile RDF/XML string.
///
/// Produces a minimal but valid RDF/XML document suitable for exchange.
pub fn write_cgmes_sc_profile(network: &Network, sc_data: &HashMap<String, ScProfile>) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<rdf:RDF\n");
    out.push_str("  xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"\n");
    out.push_str("  xmlns:cim=\"http://iec.ch/TC57/2013/CIM-schema-cim16#\"\n");
    out.push_str("  xmlns:sc=\"http://iec.ch/TC57/2013/CIM-schema-cim16-SC#\">\n");
    out.push_str(&format!(
        "  <!-- SC Profile generated by Surge — network: {} -->\n",
        network.name
    ));

    for (mrid, sc) in sc_data {
        out.push_str(&format!("  <cim:ACLineSegment rdf:ID=\"{}\">\n", mrid));
        if let Some(v) = sc.r0_pu {
            out.push_str(&format!(
                "    <cim:ACLineSegment.r0>{v}</cim:ACLineSegment.r0>\n"
            ));
        }
        if let Some(v) = sc.x0_pu {
            out.push_str(&format!(
                "    <cim:ACLineSegment.x0>{v}</cim:ACLineSegment.x0>\n"
            ));
        }
        if let Some(v) = sc.r2_pu {
            out.push_str(&format!(
                "    <cim:ACLineSegment.r2>{v}</cim:ACLineSegment.r2>\n"
            ));
        }
        if let Some(v) = sc.x2_pu {
            out.push_str(&format!(
                "    <cim:ACLineSegment.x2>{v}</cim:ACLineSegment.x2>\n"
            ));
        }
        if let Some(v) = sc.ikss_ka {
            out.push_str(&format!(
                "    <cim:ACLineSegment.Ikss>{v}</cim:ACLineSegment.Ikss>\n"
            ));
        }
        out.push_str("  </cim:ACLineSegment>\n");
    }

    out.push_str("</rdf:RDF>\n");
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // PLAN-082 / P5-048 — SC profile
    // -----------------------------------------------------------------------

    #[test]
    fn test_sc_profile_parse() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:ACLineSegment rdf:ID="_line1">
    <cim:ACLineSegment.r0>0.05</cim:ACLineSegment.r0>
    <cim:ACLineSegment.x0>0.15</cim:ACLineSegment.x0>
    <cim:ACLineSegment.r2>0.04</cim:ACLineSegment.r2>
    <cim:ACLineSegment.x2>0.12</cim:ACLineSegment.x2>
    <cim:ACLineSegment.Ikss>12.5</cim:ACLineSegment.Ikss>
  </cim:ACLineSegment>
</rdf:RDF>"##;

        let map = parse_sc_profile(xml).expect("SC parse should succeed");
        assert!(map.contains_key("_line1"), "Expected _line1 key in map");
        let sc = &map["_line1"];
        assert!(
            (sc.r0_pu.unwrap() - 0.05).abs() < 1e-10,
            "r0 mismatch: {:?}",
            sc.r0_pu
        );
        assert!(
            (sc.x0_pu.unwrap() - 0.15).abs() < 1e-10,
            "x0 mismatch: {:?}",
            sc.x0_pu
        );
        assert!(
            (sc.r2_pu.unwrap() - 0.04).abs() < 1e-10,
            "r2 mismatch: {:?}",
            sc.r2_pu
        );
        assert!(
            (sc.x2_pu.unwrap() - 0.12).abs() < 1e-10,
            "x2 mismatch: {:?}",
            sc.x2_pu
        );
        assert!(
            (sc.ikss_ka.unwrap() - 12.5).abs() < 1e-10,
            "ikss mismatch: {:?}",
            sc.ikss_ka
        );
    }

    // -----------------------------------------------------------------------
    // DY profile
    // -----------------------------------------------------------------------

    #[test]
    fn test_dy_profile_parse() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:GovGAST2 rdf:ID="_gov1">
    <cim:TurbineGovernorDynamics.SynchronousMachineDynamics rdf:resource="#_gen1"/>
  </cim:GovGAST2>
  <cim:ExcIEEEST1A rdf:ID="_exc1">
    <cim:ExcitationSystemDynamics.SynchronousMachineDynamics rdf:resource="#_gen1"/>
  </cim:ExcIEEEST1A>
</rdf:RDF>"##;

        let profiles = parse_dy_profile(xml).expect("DY parse should succeed");
        assert!(!profiles.is_empty(), "Expected at least one DY profile");

        // Find the record for _gen1
        let gen1 = profiles
            .iter()
            .find(|p| p.machine_mrid == "_gen1")
            .expect("Expected DyProfile for _gen1");

        assert_eq!(
            gen1.governor_type.as_deref(),
            Some("cim:GovGAST2"),
            "governor_type mismatch: {:?}",
            gen1.governor_type
        );
    }

    // -----------------------------------------------------------------------
    // GL profile
    // -----------------------------------------------------------------------

    #[test]
    fn test_gl_coordinates() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:Substation rdf:ID="_sub1">
    <cim:CoordinatePair.xPosition>-97.5</cim:CoordinatePair.xPosition>
    <cim:CoordinatePair.yPosition>32.8</cim:CoordinatePair.yPosition>
  </cim:Substation>
</rdf:RDF>"##;

        let profiles = parse_gl_profile(xml).expect("GL parse should succeed");
        assert!(!profiles.is_empty(), "Expected at least one GL profile");

        let sub1 = profiles
            .iter()
            .find(|p| p.substation_mrid == "_sub1")
            .expect("Expected GlProfile for _sub1");

        assert!(
            (sub1.longitude - (-97.5)).abs() < 1e-10,
            "longitude mismatch: {}",
            sub1.longitude
        );
        assert!(
            (sub1.latitude - 32.8).abs() < 1e-10,
            "latitude mismatch: {}",
            sub1.latitude
        );
    }

    // -----------------------------------------------------------------------
    // write_cgmes_sc_profile round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn test_write_sc_profile_round_trip() {
        use surge_network::Network;
        use surge_network::network::{Bus, BusType};

        let mut net = Network::new("test_sc");
        net.buses.push(Bus::new(1, BusType::Slack, 345.0));

        let mut sc_data = HashMap::new();
        sc_data.insert(
            "_line42".to_string(),
            ScProfile {
                r0_pu: Some(0.03),
                x0_pu: Some(0.09),
                r2_pu: None,
                x2_pu: None,
                ikss_ka: Some(8.0),
            },
        );

        let xml = write_cgmes_sc_profile(&net, &sc_data);
        assert!(xml.contains("_line42"), "MRID should appear in output");
        assert!(xml.contains("<cim:ACLineSegment.r0>0.03</cim:ACLineSegment.r0>"));
        assert!(xml.contains("<cim:ACLineSegment.x0>0.09</cim:ACLineSegment.x0>"));
        assert!(xml.contains("<cim:ACLineSegment.Ikss>8</cim:ACLineSegment.Ikss>"));

        // Re-parse the written XML to verify round-trip
        let reparsed = parse_sc_profile(&xml).unwrap();
        let sc = reparsed
            .get("_line42")
            .expect("_line42 should be present after re-parse");
        assert!((sc.r0_pu.unwrap() - 0.03).abs() < 1e-10);
        assert!((sc.ikss_ka.unwrap() - 8.0).abs() < 1e-10);
    }
}
