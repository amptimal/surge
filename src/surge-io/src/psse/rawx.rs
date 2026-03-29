// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSS/E RAWX (.rawx) JSON file parser.
//!
//! Parses the JSON-based PSS/E RAWX power flow data format introduced in PSS/E v35.
//! RAWX encodes the same power system data as the positional-text RAW format but uses
//! a structured JSON schema with self-describing field names per section.
//!
//! # JSON Structure
//! ```json
//! {
//!   "network": {
//!     "caseid": { "fields": [...], "data": [...] },
//!     "bus":    { "fields": [...], "data": [[...], ...] },
//!     ...
//!   }
//! }
//! ```
//!
//! The `caseid` section has a flat `data` array; all other sections have `data`
//! as an array of arrays (one per record).

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use surge_network::Network;
use surge_network::network::AreaSchedule;
use surge_network::network::facts::FactsType;
use surge_network::network::{Branch, BranchType, Bus, BusType, Generator};
use surge_network::network::{FactsDevice, FactsMode};
use surge_network::network::{LccConverterTerminal, LccHvdcControlMode, LccHvdcLink};
use surge_network::network::{
    VscConverterAcControlMode, VscConverterTerminal, VscHvdcControlMode, VscHvdcLink,
};
use thiserror::Error;

use super::reader::{apply_cz_conversion, compute_winding_tap_pu, make_xfmr_branch};

#[derive(Error, Debug)]
pub enum RawxError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("missing section: {0}")]
    MissingSection(String),

    #[error("missing field '{field}' in section '{section}'")]
    MissingField { section: String, field: String },

    #[error("invalid value in section '{section}', field '{field}': {message}")]
    InvalidValue {
        section: String,
        field: String,
        message: String,
    },
}

/// Parse a PSS/E RAWX file from disk.
pub fn parse_file(path: &Path) -> Result<Network, RawxError> {
    let content = std::fs::read_to_string(path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    parse_string_with_name(&content, &name)
}

/// Parse a PSS/E RAWX case from a JSON string.
pub fn parse_str(content: &str) -> Result<Network, RawxError> {
    parse_string_with_name(content, "unknown")
}

fn parse_string_with_name(content: &str, name: &str) -> Result<Network, RawxError> {
    let root: Value = serde_json::from_str(content)?;
    let net_obj = root
        .get("network")
        .ok_or_else(|| RawxError::MissingSection("network".into()))?;

    // Parse caseid (flat data array)
    let (sbase, _version, freq_hz) = parse_caseid(net_obj)?;

    let mut network = Network::new(name);
    network.base_mva = sbase;
    network.freq_hz = freq_hz;

    // Bus data
    let (buses, bus_idx, bus_kv) = parse_buses(net_obj)?;
    network.buses = buses;

    // Post-parse: fix vmin/vmax in kV rather than p.u.
    crate::parse_utils::sanitize_voltage_limits(&mut network);

    // Load data
    if let Some(section) = get_section(net_obj, "load") {
        parse_loads(&section, &bus_idx, &mut network)?;
    }

    // Fixed shunt data
    if let Some(section) = get_section(net_obj, "fixshunt") {
        parse_fixshunts(&section, &bus_idx, &mut network)?;
    }

    // Generator data
    if let Some(section) = get_section(net_obj, "generator") {
        network.generators = parse_generators(&section)?;
    }

    // AC line (branch) data
    if let Some(section) = get_section(net_obj, "acline") {
        network.branches = parse_aclines(&section)?;
    }

    // Transformer data
    if let Some(section) = get_section(net_obj, "transformer") {
        let (xfmrs, star_buses) = parse_transformers(&section, sbase, &bus_kv)?;
        network.buses.extend(star_buses);
        network.branches.extend(xfmrs);
    }

    // Area interchange data
    if let Some(section) = get_section(net_obj, "area") {
        network.area_schedules = parse_areas(&section)?;
    }

    // Two-terminal DC line data
    if let Some(section) = get_section(net_obj, "twotermdc") {
        network.hvdc.links = parse_twotermdc(&section)?
            .into_iter()
            .map(surge_network::network::HvdcLink::Lcc)
            .collect();
    }

    // VSC DC line data
    if let Some(section) = get_section(net_obj, "vscdc") {
        network.hvdc.links.extend(
            parse_vscdc(&section)?
                .into_iter()
                .map(surge_network::network::HvdcLink::Vsc),
        );
    }

    // FACTS device data
    if let Some(section) = get_section(net_obj, "facts") {
        network.facts_devices = parse_facts(&section)?;
    }

    // Switched shunt data
    if let Some(section) = get_section(net_obj, "swshunt") {
        parse_switched_shunts(&section, &bus_idx, &mut network)?;
    }
    Ok(network)
}

// ---------------------------------------------------------------------------
// Section helper
// ---------------------------------------------------------------------------

/// A parsed RAWX section with field-name → column-index mapping.
struct RawxSection<'a> {
    fields: HashMap<String, usize>,
    data: &'a Vec<Value>,
}

impl<'a> RawxSection<'a> {
    fn get_f64(&self, row: &Value, field: &str) -> Option<f64> {
        let idx = *self.fields.get(field)?;
        let arr = row.as_array()?;
        arr.get(idx)?.as_f64()
    }

    fn get_f64_or(&self, row: &Value, field: &str, default: f64) -> f64 {
        self.get_f64(row, field).unwrap_or(default)
    }

    fn get_i64(&self, row: &Value, field: &str) -> Option<i64> {
        let idx = *self.fields.get(field)?;
        let arr = row.as_array()?;
        let val = arr.get(idx)?;
        val.as_i64().or_else(|| val.as_f64().map(|f| f as i64))
    }

    fn get_i64_or(&self, row: &Value, field: &str, default: i64) -> i64 {
        self.get_i64(row, field).unwrap_or(default)
    }

    fn get_str<'b>(&self, row: &'b Value, field: &str) -> Option<&'b str> {
        let idx = *self.fields.get(field)?;
        let arr = row.as_array()?;
        arr.get(idx)?.as_str()
    }

    fn get_str_or<'b>(&self, row: &'b Value, field: &str, default: &'b str) -> &'b str {
        self.get_str(row, field).unwrap_or(default)
    }
}

fn get_section<'a>(net_obj: &'a Value, name: &str) -> Option<RawxSection<'a>> {
    let section = net_obj.get(name)?;
    let fields_arr = section.get("fields")?.as_array()?;
    let data = section.get("data")?.as_array()?;
    if data.is_empty() {
        return None;
    }
    let mut fields = HashMap::new();
    for (i, f) in fields_arr.iter().enumerate() {
        if let Some(s) = f.as_str() {
            fields.insert(s.to_lowercase(), i);
        }
    }
    Some(RawxSection { fields, data })
}

// ---------------------------------------------------------------------------
// Case ID
// ---------------------------------------------------------------------------

fn parse_caseid(net_obj: &Value) -> Result<(f64, u32, f64), RawxError> {
    let caseid = net_obj
        .get("caseid")
        .ok_or_else(|| RawxError::MissingSection("caseid".into()))?;
    let fields_arr = caseid
        .get("fields")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RawxError::MissingField {
            section: "caseid".into(),
            field: "fields".into(),
        })?;
    let data = caseid
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RawxError::MissingField {
            section: "caseid".into(),
            field: "data".into(),
        })?;

    let mut field_map: HashMap<String, usize> = HashMap::new();
    for (i, f) in fields_arr.iter().enumerate() {
        if let Some(s) = f.as_str() {
            field_map.insert(s.to_lowercase(), i);
        }
    }

    let get = |name: &str| -> Option<f64> {
        let idx = *field_map.get(name)?;
        data.get(idx)?.as_f64()
    };

    let sbase = get("sbase").unwrap_or(100.0);
    let rev = get("rev").unwrap_or(35.0) as u32;
    let freq = get("basfrq").unwrap_or(60.0);
    let freq_hz = if freq > 0.0 { freq } else { 60.0 };

    Ok((sbase, rev, freq_hz))
}

// ---------------------------------------------------------------------------
// Bus Data
// ---------------------------------------------------------------------------

#[allow(clippy::type_complexity)]
fn parse_buses(
    net_obj: &Value,
) -> Result<(Vec<Bus>, HashMap<u32, usize>, HashMap<u32, f64>), RawxError> {
    let section =
        get_section(net_obj, "bus").ok_or_else(|| RawxError::MissingSection("bus".into()))?;

    let mut buses = Vec::new();
    let mut bus_idx: HashMap<u32, usize> = HashMap::new();
    let mut bus_kv: HashMap<u32, f64> = HashMap::new();

    for row in section.data {
        let number = section.get_i64_or(row, "ibus", 0) as u32;
        if number == 0 {
            continue;
        }
        let name = section.get_str_or(row, "name", "").trim().to_string();
        let base_kv = section.get_f64_or(row, "baskv", 1.0);
        let ide = section.get_i64_or(row, "ide", 1);
        let bus_type = match ide {
            2 => BusType::PV,
            3 => BusType::Slack,
            4 => BusType::Isolated,
            _ => BusType::PQ,
        };
        let area = section.get_i64_or(row, "area", 1) as u32;
        let zone = section.get_i64_or(row, "zone", 1) as u32;
        let vm = section.get_f64_or(row, "vm", 1.0);
        let va_deg = section.get_f64_or(row, "va", 0.0);
        let vmax = section.get_f64_or(row, "nvhi", 1.1);
        let vmin = section.get_f64_or(row, "nvlo", 0.9);

        let idx = buses.len();
        bus_idx.insert(number, idx);
        bus_kv.insert(number, base_kv);

        buses.push(Bus {
            number,
            name,
            bus_type,
            shunt_conductance_mw: 0.0,
            shunt_susceptance_mvar: 0.0,
            area,
            voltage_magnitude_pu: vm,
            voltage_angle_rad: va_deg.to_radians(),
            base_kv,
            zone,
            voltage_max_pu: vmax,
            voltage_min_pu: vmin,
            island_id: 0,
            latitude: None,
            longitude: None,
            ..Bus::new(0, BusType::PQ, 0.0)
        });
    }

    Ok((buses, bus_idx, bus_kv))
}

// ---------------------------------------------------------------------------
// Load Data
// ---------------------------------------------------------------------------

fn parse_loads(
    section: &RawxSection,
    _bus_idx: &HashMap<u32, usize>,
    network: &mut Network,
) -> Result<(), RawxError> {
    for row in section.data {
        let bus = section.get_i64_or(row, "ibus", 0) as u32;
        let stat = section.get_i64_or(row, "stat", 1);
        if stat == 0 || bus == 0 {
            continue;
        }
        let pl = section.get_f64_or(row, "pl", 0.0);
        let ql = section.get_f64_or(row, "ql", 0.0);

        use surge_network::network::Load;
        network.loads.push(Load::new(bus, pl, ql));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fixed Shunt Data
// ---------------------------------------------------------------------------

fn parse_fixshunts(
    section: &RawxSection,
    bus_idx: &HashMap<u32, usize>,
    network: &mut Network,
) -> Result<(), RawxError> {
    for row in section.data {
        let bus = section.get_i64_or(row, "ibus", 0) as u32;
        let stat = section.get_i64_or(row, "stat", 1);
        if stat == 0 || bus == 0 {
            continue;
        }
        let gl = section.get_f64_or(row, "gl", 0.0);
        let bl = section.get_f64_or(row, "bl", 0.0);

        if let Some(&idx) = bus_idx.get(&bus) {
            network.buses[idx].shunt_conductance_mw += gl;
            network.buses[idx].shunt_susceptance_mvar += bl;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Generator Data
// ---------------------------------------------------------------------------

fn parse_generators(section: &RawxSection) -> Result<Vec<Generator>, RawxError> {
    let mut generators = Vec::new();

    for row in section.data {
        let bus = section.get_i64_or(row, "ibus", 0) as u32;
        if bus == 0 {
            continue;
        }
        let machid = section
            .get_str(row, "machid")
            .map(|s| s.trim().trim_matches('\'').to_string());
        let stat = section.get_i64_or(row, "stat", 1);
        let pg = section.get_f64_or(row, "pg", 0.0);
        let qg = section.get_f64_or(row, "qg", 0.0);
        let qt = section.get_f64_or(row, "qt", 9999.0); // qmax
        let qb = section.get_f64_or(row, "qb", -9999.0); // qmin
        let vs = section.get_f64_or(row, "vs", 1.0);
        let mbase = section.get_f64_or(row, "mbase", 100.0);
        let pt = section.get_f64_or(row, "pt", 9999.0); // pmax
        let pb = section.get_f64_or(row, "pb", 0.0); // pmin
        let zx = section.get_f64_or(row, "zx", 0.0); // machine leakage reactance

        // Reactive capability curve
        let pc1 = section.get_f64(row, "pc1");
        let pc2 = section.get_f64(row, "pc2");
        let qc1min = section.get_f64(row, "qc1min");
        let qc1max = section.get_f64(row, "qc1max");
        let qc2min = section.get_f64(row, "qc2min");
        let qc2max = section.get_f64(row, "qc2max");

        let mut g = Generator::new(bus, pg, vs);
        g.machine_id = machid;
        g.q = qg;
        g.qmax = qt;
        g.qmin = qb;
        g.machine_base_mva = mbase;
        g.pmax = pt;
        g.pmin = pb;
        g.in_service = stat != 0;
        if zx != 0.0 {
            g.fault_data.get_or_insert_with(Default::default).xs = Some(zx);
        }
        if pc1.is_some()
            || pc2.is_some()
            || qc1min.is_some()
            || qc1max.is_some()
            || qc2min.is_some()
            || qc2max.is_some()
        {
            let rc = g.reactive_capability.get_or_insert_with(Default::default);
            rc.pc1 = pc1;
            rc.pc2 = pc2;
            rc.qc1min = qc1min;
            rc.qc1max = qc1max;
            rc.qc2min = qc2min;
            rc.qc2max = qc2max;
        }

        generators.push(g);
    }

    Ok(generators)
}

// ---------------------------------------------------------------------------
// AC Line (Branch) Data
// ---------------------------------------------------------------------------

fn parse_aclines(section: &RawxSection) -> Result<Vec<Branch>, RawxError> {
    let mut branches = Vec::new();

    for row in section.data {
        let from_bus = section.get_i64_or(row, "ibus", 0) as u32;
        let to_bus = section.get_i64_or(row, "jbus", 0) as u32;
        if from_bus == 0 || to_bus == 0 {
            continue;
        }
        let ckt_str = section.get_str_or(row, "ckt", "1").trim().to_string();
        let circuit = ckt_str.trim_matches('\'').trim().to_string();
        let r = section.get_f64_or(row, "rpu", 0.0);
        let x = section.get_f64_or(row, "xpu", 0.01);
        let b = section.get_f64_or(row, "bpu", 0.0);
        let stat = section.get_i64_or(row, "stat", 1);

        let rate_a = section.get_f64_or(row, "rate1", 0.0);
        let rate_b = section.get_f64_or(row, "rate2", 0.0);
        let rate_c = section.get_f64_or(row, "rate3", 0.0);

        // Terminal shunt admittances (GI/BI/GJ/BJ) — accumulate into branch
        // These are rarely used but supported.
        let _gi = section.get_f64_or(row, "gi", 0.0);
        let _bi = section.get_f64_or(row, "bi", 0.0);
        let _gj = section.get_f64_or(row, "gj", 0.0);
        let _bj = section.get_f64_or(row, "bj", 0.0);

        let mut branch = Branch::new_line(from_bus, to_bus, r, x, b);
        branch.circuit = circuit;
        branch.rating_a_mva = rate_a;
        branch.rating_b_mva = rate_b;
        branch.rating_c_mva = rate_c;
        branch.in_service = stat != 0;

        branches.push(branch);
    }

    Ok(branches)
}

// ---------------------------------------------------------------------------
// Transformer Data
// ---------------------------------------------------------------------------

fn parse_transformers(
    section: &RawxSection,
    sbase: f64,
    bus_kv: &HashMap<u32, f64>,
) -> Result<(Vec<Branch>, Vec<Bus>), RawxError> {
    let mut transformers = Vec::new();
    let mut star_buses = Vec::new();
    let mut max_bus_num: u32 = bus_kv.keys().copied().max().unwrap_or(0);

    for row in section.data {
        let from_bus = section.get_i64_or(row, "ibus", 0) as u32;
        let to_bus = section.get_i64_or(row, "jbus", 0).unsigned_abs() as u32;
        let k = section.get_i64_or(row, "kbus", 0);
        if from_bus == 0 || to_bus == 0 {
            continue;
        }

        let ckt_str = section.get_str_or(row, "ckt", "1").trim().to_string();
        let circuit = ckt_str.trim_matches('\'').trim().to_string();

        let cw = section.get_i64_or(row, "cw", 1) as u32;
        let cz = section.get_i64_or(row, "cz", 1) as u32;
        let mag1 = section.get_f64_or(row, "mag1", 0.0);
        let mag2 = section.get_f64_or(row, "mag2", 0.0);
        let stat = section.get_i64_or(row, "stat", 1);

        // Record 2 fields (impedance)
        let r12_raw = section.get_f64_or(row, "r1-2", 0.0);
        let x12_raw = section.get_f64_or(row, "x1-2", 0.01);
        let sbase12 = section.get_f64_or(row, "sbase1-2", sbase);

        let (r12, x12) = apply_cz_conversion(r12_raw, x12_raw, sbase12, sbase, cz);

        let is_3winding = k != 0;
        let k_bus = k.unsigned_abs() as u32;

        if is_3winding {
            // 3-winding transformer: star (Y) topology expansion
            let r23_raw = section.get_f64_or(row, "r2-3", 0.0);
            let x23_raw = section.get_f64_or(row, "x2-3", 0.01);
            let sbase23 = section.get_f64_or(row, "sbase2-3", sbase);
            let r31_raw = section.get_f64_or(row, "r3-1", 0.0);
            let x31_raw = section.get_f64_or(row, "x3-1", 0.01);
            let sbase31 = section.get_f64_or(row, "sbase3-1", sbase);
            let vmstar = section.get_f64_or(row, "vmstar", 1.0);
            let anstar_deg = section.get_f64_or(row, "anstar", 0.0);

            let (r23, x23) = apply_cz_conversion(r23_raw, x23_raw, sbase23, sbase, cz);
            let (r31, x31) = apply_cz_conversion(r31_raw, x31_raw, sbase31, sbase, cz);

            // Star-delta impedance conversion
            let r1 = (r12 + r31 - r23) / 2.0;
            let x1 = (x12 + x31 - x23) / 2.0;
            let r2 = (r12 + r23 - r31) / 2.0;
            let x2 = (x12 + x23 - x31) / 2.0;
            let r3 = (r23 + r31 - r12) / 2.0;
            let x3 = (x23 + x31 - x12) / 2.0;

            // Winding taps
            let windv1 = section.get_f64_or(row, "windv1", 1.0);
            let nomv1 = section.get_f64_or(row, "nomv1", 0.0);
            let ang1 = section.get_f64_or(row, "ang1", 0.0);
            let rata1 = section.get_f64_or(row, "wdg1rate1", 0.0);
            let ratb1 = section.get_f64_or(row, "wdg1rate2", 0.0);
            let ratc1 = section.get_f64_or(row, "wdg1rate3", 0.0);

            let windv2 = section.get_f64_or(row, "windv2", 1.0);
            let nomv2 = section.get_f64_or(row, "nomv2", 0.0);
            let ang2 = section.get_f64_or(row, "ang2", 0.0);
            let rata2 = section.get_f64_or(row, "wdg2rate1", 0.0);
            let ratb2 = section.get_f64_or(row, "wdg2rate2", 0.0);
            let ratc2 = section.get_f64_or(row, "wdg2rate3", 0.0);

            let windv3 = section.get_f64_or(row, "windv3", 1.0);
            let nomv3 = section.get_f64_or(row, "nomv3", 0.0);
            let ang3 = section.get_f64_or(row, "ang3", 0.0);
            let rata3 = section.get_f64_or(row, "wdg3rate1", 0.0);
            let ratb3 = section.get_f64_or(row, "wdg3rate2", 0.0);
            let ratc3 = section.get_f64_or(row, "wdg3rate3", 0.0);

            let bkv1 = bus_kv.get(&from_bus).copied().unwrap_or(1.0);
            let bkv2 = bus_kv.get(&to_bus).copied().unwrap_or(1.0);
            let bkv3 = bus_kv.get(&k_bus).copied().unwrap_or(1.0);
            let tap1 = compute_winding_tap_pu(windv1, nomv1, bkv1, cw);
            let tap2 = compute_winding_tap_pu(windv2, nomv2, bkv2, cw);
            let tap3 = compute_winding_tap_pu(windv3, nomv3, bkv3, cw);

            // Create fictitious star bus
            max_bus_num += 1;
            let star_bus_num = max_bus_num;
            star_buses.push(Bus {
                number: star_bus_num,
                name: format!("STAR_{from_bus}_{to_bus}_{k_bus}"),
                bus_type: BusType::PQ,
                shunt_conductance_mw: 0.0,
                shunt_susceptance_mvar: 0.0,
                area: 1,
                voltage_magnitude_pu: vmstar,
                voltage_angle_rad: anstar_deg.to_radians(),
                base_kv: bkv1.max(bkv2).max(bkv3).max(1.0),
                zone: 1,
                voltage_max_pu: 1.1,
                voltage_min_pu: 0.9,
                island_id: 0,
                latitude: None,
                longitude: None,
                ..Bus::new(0, BusType::PQ, 0.0)
            });

            let in_service = stat > 0;

            // Winding 1 → star
            let mut w1 = make_xfmr_branch(
                from_bus,
                star_bus_num,
                circuit.clone(),
                r1,
                x1,
                rata1,
                ratb1,
                ratc1,
                tap1,
                ang1,
                in_service,
                mag1,
                mag2,
            );
            w1.branch_type = BranchType::Transformer3W;
            transformers.push(w1);
            // Winding 2 → star
            let mut w2 = make_xfmr_branch(
                to_bus,
                star_bus_num,
                circuit.clone(),
                r2,
                x2,
                rata2,
                ratb2,
                ratc2,
                tap2,
                ang2,
                in_service,
                0.0,
                0.0,
            );
            w2.branch_type = BranchType::Transformer3W;
            transformers.push(w2);
            // Winding 3 → star
            let mut w3 = make_xfmr_branch(
                k_bus,
                star_bus_num,
                circuit,
                r3,
                x3,
                rata3,
                ratb3,
                ratc3,
                tap3,
                ang3,
                in_service,
                0.0,
                0.0,
            );
            w3.branch_type = BranchType::Transformer3W;
            transformers.push(w3);
        } else {
            // 2-winding transformer
            let windv1 = section.get_f64_or(row, "windv1", 1.0);
            let nomv1 = section.get_f64_or(row, "nomv1", 0.0);
            let ang1 = section.get_f64_or(row, "ang1", 0.0);
            let rata1 = section.get_f64_or(row, "wdg1rate1", 0.0);
            let ratb1 = section.get_f64_or(row, "wdg1rate2", 0.0);
            let ratc1 = section.get_f64_or(row, "wdg1rate3", 0.0);

            let windv2 = section.get_f64_or(row, "windv2", 1.0);
            let nomv2 = section.get_f64_or(row, "nomv2", 0.0);

            // Compute 2-winding tap ratio based on CW code
            let tap = match cw {
                1 => {
                    if windv2 != 0.0 {
                        windv1 / windv2
                    } else {
                        windv1
                    }
                }
                2 => {
                    let bkv1 = bus_kv.get(&from_bus).copied().unwrap_or(1.0);
                    let bkv2 = bus_kv.get(&to_bus).copied().unwrap_or(1.0);
                    let t1 = if bkv1 > 0.0 { windv1 / bkv1 } else { windv1 };
                    let t2 = if bkv2 > 0.0 { windv2 / bkv2 } else { windv2 };
                    if t2 != 0.0 { t1 / t2 } else { t1 }
                }
                3 => {
                    let bkv1 = bus_kv.get(&from_bus).copied().unwrap_or(1.0);
                    let bkv2 = bus_kv.get(&to_bus).copied().unwrap_or(1.0);
                    let n1 = if nomv1 > 0.0 { nomv1 } else { bkv1 };
                    let n2 = if nomv2 > 0.0 { nomv2 } else { bkv2 };
                    let t1 = if bkv1 > 0.0 {
                        windv1 * n1 / bkv1
                    } else {
                        windv1
                    };
                    let t2 = if bkv2 > 0.0 {
                        windv2 * n2 / bkv2
                    } else {
                        windv2
                    };
                    if t2 != 0.0 { t1 / t2 } else { t1 }
                }
                _ => windv1,
            };

            transformers.push(make_xfmr_branch(
                from_bus,
                to_bus,
                circuit,
                r12,
                x12,
                rata1,
                ratb1,
                ratc1,
                tap,
                ang1,
                stat > 0,
                mag1,
                mag2,
            ));
        }
    }

    Ok((transformers, star_buses))
}

// ---------------------------------------------------------------------------
// Area Interchange Data
// ---------------------------------------------------------------------------

fn parse_areas(section: &RawxSection) -> Result<Vec<AreaSchedule>, RawxError> {
    let mut areas = Vec::new();
    for row in section.data {
        let number = section.get_i64_or(row, "iarea", 0) as u32;
        if number == 0 {
            continue;
        }
        let slack_bus = section.get_i64_or(row, "isw", 0) as u32;
        let pdes = section.get_f64_or(row, "pdes", 0.0);
        let ptol = section.get_f64_or(row, "ptol", 10.0);
        let name = section
            .get_str(row, "arnam")
            .unwrap_or("")
            .trim()
            .trim_matches('\'')
            .to_string();

        areas.push(AreaSchedule {
            number,
            slack_bus,
            p_desired_mw: pdes,
            p_tolerance_mw: ptol,
            name,
        });
    }
    Ok(areas)
}

// ---------------------------------------------------------------------------
// Switched Shunt Data
// ---------------------------------------------------------------------------

fn parse_switched_shunts(
    section: &RawxSection,
    _bus_idx: &HashMap<u32, usize>,
    network: &mut Network,
) -> Result<(), RawxError> {
    use crate::parse_utils::{RawSwitchedShunt, apply_switched_shunts};

    let mut raw_shunts: Vec<RawSwitchedShunt> = Vec::new();

    for row in section.data {
        let bus = section.get_i64_or(row, "ibus", 0) as u32;
        if bus == 0 {
            continue;
        }
        let modsw = section.get_i64_or(row, "modsw", 0) as i32;
        let stat = section.get_i64_or(row, "stat", 1) as i32;
        let vswhi = section.get_f64_or(row, "vswhi", 1.1);
        let vswlo = section.get_f64_or(row, "vswlo", 0.9);
        let swrem = section.get_i64_or(row, "swrem", 0) as u32;
        let binit = section.get_f64_or(row, "binit", 0.0);

        // Parse up to 8 (N, B) step blocks.
        let mut blocks = Vec::new();
        for i in 1u32..=8 {
            let nk = section.get_i64_or(row, &format!("n{i}"), 0) as i32;
            let bk = section.get_f64_or(row, &format!("b{i}"), 0.0);
            blocks.push((nk, bk));
        }

        raw_shunts.push(RawSwitchedShunt {
            bus,
            modsw,
            stat,
            vswhi,
            vswlo,
            swrem,
            binit,
            blocks,
        });
    }

    let base_mva = network.base_mva;
    apply_switched_shunts(network, &raw_shunts, base_mva);
    Ok(())
}

// ---------------------------------------------------------------------------
// Two-Terminal DC Line Data
// ---------------------------------------------------------------------------

fn parse_twotermdc(section: &RawxSection) -> Result<Vec<LccHvdcLink>, RawxError> {
    let mut lcc_links = Vec::new();
    for row in section.data {
        let name = section
            .get_str(row, "name")
            .unwrap_or("")
            .trim()
            .trim_matches('\'')
            .to_string();
        let mdc = section.get_i64_or(row, "mdc", 0) as u32;
        let resistance_ohm = section.get_f64_or(row, "resistance_ohm", 0.0);
        let setvl = section.get_f64_or(row, "setvl", 0.0);
        let vschd = section.get_f64_or(row, "vschd", 0.0);
        let vcmod = section.get_f64_or(row, "vcmod", 0.0);
        let rcomp = section.get_f64_or(row, "rcomp", 0.0);
        let delti = section.get_f64_or(row, "delti", 0.0);

        // Rectifier
        let ipr = section.get_i64_or(row, "ipr", 0) as u32;
        let nbr = section.get_i64_or(row, "nbr", 6) as u32;
        let anmxr = section.get_f64_or(row, "anmxr", 90.0);
        let anmnr = section.get_f64_or(row, "anmnr", 0.0);
        let rcr = section.get_f64_or(row, "rcr", 0.0);
        let xcr = section.get_f64_or(row, "xcr", 0.0);
        let ebasr = section.get_f64_or(row, "ebasr", 0.0);
        let trr = section.get_f64_or(row, "trr", 1.0);
        let tapr = section.get_f64_or(row, "tapr", 1.0);
        let tmxr = section.get_f64_or(row, "tmxr", 1.5);
        let tmnr = section.get_f64_or(row, "tmnr", 0.51);
        let stpr = section.get_f64_or(row, "stpr", 0.00625);

        // Inverter
        let ipi = section.get_i64_or(row, "ipi", 0) as u32;
        let nbi = section.get_i64_or(row, "nbi", 6) as u32;
        let anmxi = section.get_f64_or(row, "anmxi", 90.0);
        let anmni = section.get_f64_or(row, "anmni", 0.0);
        let rci = section.get_f64_or(row, "rci", 0.0);
        let xci = section.get_f64_or(row, "xci", 0.0);
        let ebasi = section.get_f64_or(row, "ebasi", 0.0);
        let tri = section.get_f64_or(row, "tri", 1.0);
        let tapi = section.get_f64_or(row, "tapi", 1.0);
        let tmxi = section.get_f64_or(row, "tmxi", 1.5);
        let tmni = section.get_f64_or(row, "tmni", 0.51);
        let stpi = section.get_f64_or(row, "stpi", 0.00625);

        lcc_links.push(LccHvdcLink {
            name,
            mode: LccHvdcControlMode::from_u32(mdc),
            resistance_ohm,
            scheduled_setpoint: setvl,
            scheduled_voltage_kv: vschd,
            voltage_mode_switch_kv: vcmod,
            compounding_resistance_ohm: rcomp,
            current_margin_ka: delti,
            meter: 'I',
            voltage_min_kv: 0.0,
            ac_dc_iteration_max: 20,
            ac_dc_iteration_acceleration: 1.0,
            rectifier: LccConverterTerminal {
                bus: ipr,
                n_bridges: nbr,
                alpha_max: anmxr,
                alpha_min: anmnr,
                commutation_resistance_ohm: rcr,
                commutation_reactance_ohm: xcr,
                base_voltage_kv: ebasr,
                turns_ratio: trr,
                tap: tapr,
                tap_max: tmxr,
                tap_min: tmnr,
                tap_step: stpr,
                in_service: true,
            },
            inverter: LccConverterTerminal {
                bus: ipi,
                n_bridges: nbi,
                alpha_max: anmxi,
                alpha_min: anmni,
                commutation_resistance_ohm: rci,
                commutation_reactance_ohm: xci,
                base_voltage_kv: ebasi,
                turns_ratio: tri,
                tap: tapi,
                tap_max: tmxi,
                tap_min: tmni,
                tap_step: stpi,
                in_service: true,
            },
        });
    }
    Ok(lcc_links)
}

// ---------------------------------------------------------------------------
// VSC DC Line Data
// ---------------------------------------------------------------------------

fn parse_vscdc(section: &RawxSection) -> Result<Vec<VscHvdcLink>, RawxError> {
    let mut vsc_lines = Vec::new();
    for row in section.data {
        let name = section
            .get_str(row, "name")
            .unwrap_or("")
            .trim()
            .trim_matches('\'')
            .to_string();
        let mdc = section.get_i64_or(row, "mdc", 0) as u32;
        let resistance_ohm = section.get_f64_or(row, "resistance_ohm", 0.0);

        let ibus1 = section.get_i64_or(row, "ibus1", 0) as u32;
        let mode1 = section.get_i64_or(row, "mode1", 1) as u32;
        let acset1 = section.get_f64_or(row, "acset1", 1.0);
        let dcset1 = section.get_f64_or(row, "dcset1", 0.0);
        let aloss1 = section.get_f64_or(row, "aloss1", 0.0);
        let bloss1 = section.get_f64_or(row, "bloss1", 0.0);

        let ibus2 = section.get_i64_or(row, "ibus2", 0) as u32;
        let mode2 = section.get_i64_or(row, "mode2", 1) as u32;
        let acset2 = section.get_f64_or(row, "acset2", 1.0);
        let dcset2 = section.get_f64_or(row, "dcset2", 0.0);
        let aloss2 = section.get_f64_or(row, "aloss2", 0.0);
        let bloss2 = section.get_f64_or(row, "bloss2", 0.0);

        vsc_lines.push(VscHvdcLink {
            name,
            mode: VscHvdcControlMode::from_u32(mdc),
            resistance_ohm,
            converter1: VscConverterTerminal {
                bus: ibus1,
                control_mode: VscConverterAcControlMode::from_u32(mode1),
                ac_setpoint: acset1,
                dc_setpoint: dcset1,
                loss_constant_mw: aloss1,
                loss_linear: bloss1,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: ibus2,
                control_mode: VscConverterAcControlMode::from_u32(mode2),
                ac_setpoint: acset2,
                dc_setpoint: dcset2,
                loss_constant_mw: aloss2,
                loss_linear: bloss2,
                ..VscConverterTerminal::default()
            },
        });
    }
    Ok(vsc_lines)
}

// ---------------------------------------------------------------------------
// FACTS Device Data
// ---------------------------------------------------------------------------

fn parse_facts(section: &RawxSection) -> Result<Vec<FactsDevice>, RawxError> {
    let mut facts = Vec::new();
    for row in section.data {
        let name = section
            .get_str(row, "name")
            .unwrap_or("")
            .trim()
            .trim_matches('\'')
            .to_string();
        let bus_i = section.get_i64_or(row, "ibus", 0) as u32;
        let bus_j = section.get_i64_or(row, "jbus", 0) as u32;
        let mode = section.get_i64_or(row, "mode", 1) as u32;
        let pdes = section.get_f64_or(row, "pdes", 0.0);
        let qdes = section.get_f64_or(row, "qdes", 0.0);
        let vset = section.get_f64_or(row, "vset", 1.0);
        let shmx = section.get_f64_or(row, "shmx", 9999.0);
        let linx = section.get_f64_or(row, "linx", 0.05);
        let stat = section.get_i64_or(row, "stat", 1);

        let facts_mode = FactsMode::from_u32(mode);

        // Infer FACTS device type from operating mode
        let facts_type = match facts_mode {
            FactsMode::ShuntOnly => FactsType::Svc,
            FactsMode::SeriesOnly | FactsMode::ImpedanceModulation => FactsType::Tcsc,
            FactsMode::ShuntSeries | FactsMode::SeriesPowerControl => FactsType::Upfc,
            FactsMode::OutOfService => FactsType::Svc,
        };

        facts.push(FactsDevice {
            name,
            bus_from: bus_i,
            bus_to: bus_j,
            mode: facts_mode,
            p_setpoint_mw: pdes,
            q_setpoint_mvar: qdes,
            voltage_setpoint_pu: vset,
            q_max: shmx,
            series_reactance_pu: linx,
            in_service: stat != 0,
            facts_type,
            ..FactsDevice::default()
        });
    }
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_rawx() {
        let json = r#"{
            "network": {
                "caseid": {
                    "fields": ["ic", "sbase", "rev", "xfrrat", "nxfrat", "basfrq", "title1", "title2"],
                    "data": [0, 100.0, 35, 0, 0, 60.0, "test", ""]
                },
                "bus": {
                    "fields": ["ibus", "name", "baskv", "ide", "area", "zone", "owner", "vm", "va"],
                    "data": [
                        [1, "Bus 1", 138.0, 3, 1, 1, 1, 1.06, 0.0],
                        [2, "Bus 2", 138.0, 1, 1, 1, 1, 1.0, -5.0]
                    ]
                },
                "load": {
                    "fields": ["ibus", "loadid", "stat", "area", "zone", "pl", "ql"],
                    "data": [
                        [2, "1", 1, 1, 1, 100.0, 35.0]
                    ]
                },
                "generator": {
                    "fields": ["ibus", "machid", "pg", "qg", "qt", "qb", "vs", "ireg", "mbase", "zr", "zx", "rt", "xt", "gtap", "stat", "rmpct", "pt", "pb"],
                    "data": [
                        [1, "1", 80.0, 30.0, 200.0, -200.0, 1.06, 0, 100.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1, 100.0, 200.0, 0.0]
                    ]
                },
                "acline": {
                    "fields": ["ibus", "jbus", "ckt", "rpu", "xpu", "bpu", "name", "rate1", "rate2", "rate3", "gi", "bi", "gj", "bj", "stat", "met", "len"],
                    "data": [
                        [1, 2, "1", 0.02, 0.06, 0.03, "Line 1-2", 100.0, 100.0, 100.0, 0.0, 0.0, 0.0, 0.0, 1, 1, 0.0]
                    ]
                }
            }
        }"#;

        let net = parse_str(json).unwrap();
        assert_eq!(net.n_buses(), 2);
        assert_eq!(net.branches.len(), 1);
        assert_eq!(net.generators.len(), 1);
        assert_eq!(net.base_mva, 100.0);
        assert_eq!(net.freq_hz, 60.0);

        // Check bus data
        assert_eq!(net.buses[0].number, 1);
        assert_eq!(net.buses[0].bus_type, BusType::Slack);
        assert_eq!(net.buses[0].base_kv, 138.0);
        assert!((net.buses[0].voltage_magnitude_pu - 1.06).abs() < 1e-10);

        assert_eq!(net.buses[1].number, 2);
        assert_eq!(net.buses[1].bus_type, BusType::PQ);
        let bus_pd = net.bus_load_p_mw();
        let bus_qd = net.bus_load_q_mvar();
        assert!((bus_pd[1] - 100.0).abs() < 1e-10);
        assert!((bus_qd[1] - 35.0).abs() < 1e-10);

        // Check generator
        assert_eq!(net.generators[0].bus, 1);
        assert!((net.generators[0].p - 80.0).abs() < 1e-10);
        assert!((net.generators[0].voltage_setpoint_pu - 1.06).abs() < 1e-10);
        assert!((net.generators[0].pmax - 200.0).abs() < 1e-10);

        // Check branch
        assert_eq!(net.branches[0].from_bus, 1);
        assert_eq!(net.branches[0].to_bus, 2);
        assert!((net.branches[0].r - 0.02).abs() < 1e-10);
        assert!((net.branches[0].x - 0.06).abs() < 1e-10);
    }

    #[test]
    fn test_parse_rawx_with_transformer() {
        let json = r#"{
            "network": {
                "caseid": {
                    "fields": ["ic", "sbase", "rev", "xfrrat", "nxfrat", "basfrq"],
                    "data": [0, 100.0, 35, 0, 0, 60.0]
                },
                "bus": {
                    "fields": ["ibus", "name", "baskv", "ide", "area", "zone", "owner", "vm", "va"],
                    "data": [
                        [1, "HV Bus", 345.0, 3, 1, 1, 1, 1.04, 0.0],
                        [2, "LV Bus", 138.0, 1, 1, 1, 1, 1.0, -3.0]
                    ]
                },
                "transformer": {
                    "fields": ["ibus", "jbus", "kbus", "ckt", "cw", "cz", "cm", "mag1", "mag2", "nmet", "name", "stat", "o1", "f1", "o2", "f2", "o3", "f3", "o4", "f4", "vecgrp", "zcod", "r1-2", "x1-2", "sbase1-2", "windv1", "nomv1", "ang1", "wdg1rate1", "wdg1rate2", "wdg1rate3", "cod1", "cont1", "node1", "rma1", "rmi1", "vma1", "vmi1", "ntp1", "tab1", "cr1", "cx1", "cnxa1", "windv2", "nomv2", "ang2"],
                    "data": [
                        [1, 2, 0, "1", 1, 1, 1, 0.0, 0.0, 2, "Xfmr 1-2", 1, 1, 1.0, 0, 1.0, 0, 1.0, 0, 1.0, "", 0, 0.005, 0.1, 100.0, 1.0, 0.0, 0.0, 200.0, 200.0, 200.0, 0, 0, 0, 1.1, 0.9, 1.1, 0.9, 33, 0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]
                    ]
                }
            }
        }"#;

        let net = parse_str(json).unwrap();
        assert_eq!(net.n_buses(), 2);
        // 1 transformer branch
        assert_eq!(net.branches.len(), 1);
        let xfmr = &net.branches[0];
        assert_eq!(xfmr.from_bus, 1);
        assert_eq!(xfmr.to_bus, 2);
        assert!((xfmr.r - 0.005).abs() < 1e-10);
        assert!((xfmr.x - 0.1).abs() < 1e-10);
        // CW=1, WINDV1=1.0, WINDV2=1.0 → tap = 1.0
        assert!((xfmr.tap - 1.0).abs() < 1e-10);
        assert!((xfmr.rating_a_mva - 200.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_rawx_empty_sections() {
        // Minimal file with only caseid and bus — no loads, gens, branches
        let json = r#"{
            "network": {
                "caseid": {
                    "fields": ["ic", "sbase"],
                    "data": [0, 100.0]
                },
                "bus": {
                    "fields": ["ibus", "name", "baskv", "ide"],
                    "data": [
                        [1, "Solo Bus", 69.0, 3]
                    ]
                }
            }
        }"#;

        let net = parse_str(json).unwrap();
        assert_eq!(net.n_buses(), 1);
        assert_eq!(net.branches.len(), 0);
        assert_eq!(net.generators.len(), 0);
        assert_eq!(net.base_mva, 100.0);
    }

    #[test]
    fn test_parse_rawx_fixshunt_accumulation() {
        let json = r#"{
            "network": {
                "caseid": {
                    "fields": ["ic", "sbase"],
                    "data": [0, 100.0]
                },
                "bus": {
                    "fields": ["ibus", "name", "baskv", "ide"],
                    "data": [
                        [1, "Bus 1", 138.0, 3]
                    ]
                },
                "fixshunt": {
                    "fields": ["ibus", "shntid", "stat", "gl", "bl"],
                    "data": [
                        [1, "1", 1, 0.0, 25.0],
                        [1, "2", 1, 0.0, 10.0]
                    ]
                }
            }
        }"#;

        let net = parse_str(json).unwrap();
        assert!((net.buses[0].shunt_susceptance_mvar - 35.0).abs() < 1e-10);
    }
}
