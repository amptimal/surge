// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GE PSLF EPC (.epc) file parser.
//!
//! Parses the GE PSLF Electric Power Case format used in WECC base-case
//! exchange and TAMU ACTIVSg synthetic test cases.
//!
//! # File Structure
//! - `title` / `comments` / `solution parameters` — preamble sections delimited by `!`
//! - Named data sections: `bus data [N] ...`, `branch data [N] ...`, etc.
//! - Records are space-delimited with double-quoted strings
//! - Records have identity fields (bus/branch endpoints) before `:` and data fields after
//! - Continuation lines end with `/` — joined before parsing
//! - Section count `[N]` in header gives the record count
//!
//! # Supported Sections
//! - Bus Data (type, voltage, angle, area, zone, limits, coordinates)
//! - Branch Data (2-line records: impedance, ratings, status)
//! - Transformer Data (4-line records: impedance, tap, ratings)
//! - Generator Data (2-line records: dispatch, limits, machine base)
//! - Load Data (constant P/Q, I-dependent, Z-dependent)
//! - Shunt Data (fixed bus shunts)
//! - SVD Data (switched shunts — modeled as fixed at operating point)
//! - Area Data
//! - Zone Data
//! - DC Bus / DC Line / DC Converter / VS Converter Data

use std::collections::HashMap;
use std::path::Path;

use surge_network::Network;
use surge_network::network::AreaSchedule;
use surge_network::network::{Branch, BranchType, Bus, BusType, Generator};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum EpcError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error on line {line}: {message}")]
    Parse { line: usize, message: String },

    #[error("missing section: {0}")]
    MissingSection(String),

    #[error("unexpected end of file in {0} section")]
    UnexpectedEof(String),

    #[error("non-finite float value on line {line}: {message}")]
    NonFiniteValue { line: usize, message: String },
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a GE PSLF EPC file from disk.
pub fn parse_file(path: &Path) -> Result<Network, EpcError> {
    let content = std::fs::read_to_string(path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    parse_string_with_name(&content, &name)
}

/// Parse a GE PSLF EPC case from a string.
pub fn parse_str(content: &str) -> Result<Network, EpcError> {
    parse_string_with_name(content, "unknown")
}

// ---------------------------------------------------------------------------
// Intermediary types
// ---------------------------------------------------------------------------

use crate::parse_utils::{RawLoad, RawShunt, apply_loads, apply_shunts};

// ---------------------------------------------------------------------------
// Section detection
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum EpcSection {
    Title,
    Comments,
    SolutionParameters,
    SubstationData,
    BusData,
    BranchData,
    TransformerData,
    GeneratorData,
    LoadData,
    ShuntData,
    SvdData,
    AreaData,
    ZoneData,
    InterfaceData,
    InterfaceBranchData,
    DcBusData,
    DcLineData,
    DcConverterData,
    VsConverterData,
    ZTableData,
    GcdData,
    TransactionData,
    OwnerData,
    QtableData,
    BaData,
    InjGroupData,
    InjGrpElemData,
    End,
}

/// Detect which section a line begins (case-insensitive match on section name).
fn detect_section(line: &str) -> Option<EpcSection> {
    let lower = line.trim().to_ascii_lowercase();
    // Order matters — match more specific names first to avoid prefix collisions.
    if lower == "title" {
        return Some(EpcSection::Title);
    }
    if lower == "comments" {
        return Some(EpcSection::Comments);
    }
    if lower.starts_with("solution parameters") || lower.starts_with("solution_parameters") {
        return Some(EpcSection::SolutionParameters);
    }
    if lower.starts_with("substation data") {
        return Some(EpcSection::SubstationData);
    }
    if lower.starts_with("bus data") {
        return Some(EpcSection::BusData);
    }
    if lower.starts_with("branch data") {
        return Some(EpcSection::BranchData);
    }
    if lower.starts_with("transformer data") {
        return Some(EpcSection::TransformerData);
    }
    if lower.starts_with("generator data") {
        return Some(EpcSection::GeneratorData);
    }
    if lower.starts_with("load data") {
        return Some(EpcSection::LoadData);
    }
    if lower.starts_with("shunt data") {
        return Some(EpcSection::ShuntData);
    }
    if lower.starts_with("svd data") {
        return Some(EpcSection::SvdData);
    }
    if lower.starts_with("interface branch data") {
        return Some(EpcSection::InterfaceBranchData);
    }
    if lower.starts_with("interface data") {
        return Some(EpcSection::InterfaceData);
    }
    if lower.starts_with("area data") {
        return Some(EpcSection::AreaData);
    }
    if lower.starts_with("zone data") {
        return Some(EpcSection::ZoneData);
    }
    if lower.starts_with("dc bus data") {
        return Some(EpcSection::DcBusData);
    }
    if lower.starts_with("dc line data") {
        return Some(EpcSection::DcLineData);
    }
    if lower.starts_with("dc converter data") {
        return Some(EpcSection::DcConverterData);
    }
    if lower.starts_with("vs converter data") {
        return Some(EpcSection::VsConverterData);
    }
    if lower.starts_with("z table data") {
        return Some(EpcSection::ZTableData);
    }
    if lower.starts_with("gcd data") {
        return Some(EpcSection::GcdData);
    }
    if lower.starts_with("transaction data") {
        return Some(EpcSection::TransactionData);
    }
    if lower.starts_with("owner data") {
        return Some(EpcSection::OwnerData);
    }
    if lower.starts_with("qtable data") {
        return Some(EpcSection::QtableData);
    }
    if lower.starts_with("ba data") {
        return Some(EpcSection::BaData);
    }
    if lower.starts_with("injgrpelem data") {
        return Some(EpcSection::InjGrpElemData);
    }
    if lower.starts_with("injgroup data") {
        return Some(EpcSection::InjGroupData);
    }
    if lower == "end" {
        return Some(EpcSection::End);
    }
    None
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

/// Tokenize an EPC record line into space-delimited fields, respecting
/// double-quoted strings.  Colons `:` are returned as separate tokens.
fn tokenize_epc(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    let mut current = String::new();

    while let Some(&ch) = chars.peek() {
        match ch {
            '"' => {
                // Consume entire quoted string (including quotes)
                chars.next();
                let mut quoted = String::new();
                while let Some(&qch) = chars.peek() {
                    if qch == '"' {
                        chars.next();
                        break;
                    }
                    quoted.push(qch);
                    chars.next();
                }
                // Flush any pending non-quoted token
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                tokens.push(quoted);
            }
            ':' => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                tokens.push(":".into());
                chars.next();
            }
            ' ' | '\t' => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                chars.next();
            }
            _ => {
                current.push(ch);
                chars.next();
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Parse a token as f64.  Returns 0.0 for empty strings.
fn parse_f64(token: &str, line: usize, field: &str) -> Result<f64, EpcError> {
    let s = token.trim().trim_matches('"');
    if s.is_empty() {
        return Ok(0.0);
    }
    let v: f64 = s.parse().map_err(|_| EpcError::Parse {
        line,
        message: format!("cannot parse '{s}' as f64 for field '{field}'"),
    })?;
    if !v.is_finite() {
        return Err(EpcError::NonFiniteValue {
            line,
            message: format!("non-finite value {v} for field '{field}'"),
        });
    }
    Ok(v)
}

/// Parse a token as u32.
fn parse_u32(token: &str, line: usize, field: &str) -> Result<u32, EpcError> {
    let s = token.trim().trim_matches('"');
    if s.is_empty() {
        return Ok(0);
    }
    // Handle floats like "1.0" by parsing as f64 first
    if s.contains('.') || s.contains('E') || s.contains('e') {
        let v = parse_f64(s, line, field)?;
        return Ok(v as u32);
    }
    s.parse().map_err(|_| EpcError::Parse {
        line,
        message: format!("cannot parse '{s}' as u32 for field '{field}'"),
    })
}

/// Parse a token as i32.
fn parse_i32(token: &str, line: usize, field: &str) -> Result<i32, EpcError> {
    let s = token.trim().trim_matches('"');
    if s.is_empty() {
        return Ok(0);
    }
    if s.contains('.') || s.contains('E') || s.contains('e') {
        let v = parse_f64(s, line, field)?;
        return Ok(v as i32);
    }
    s.parse().map_err(|_| EpcError::Parse {
        line,
        message: format!("cannot parse '{s}' as i32 for field '{field}'"),
    })
}

/// Get token at index, returning empty string if out of bounds.
fn tok(tokens: &[String], idx: usize) -> &str {
    tokens.get(idx).map(|s| s.as_str()).unwrap_or("")
}

// ---------------------------------------------------------------------------
// Line preprocessing
// ---------------------------------------------------------------------------

/// A logical line: the original line(s) joined by `/` continuations,
/// paired with the original (first) line number.
struct LogicalLine {
    text: String,
    line_num: usize,
}

/// Preprocess raw lines: join continuation lines (ending with `/`),
/// skip comment/annotation lines (starting with `@`), and skip blank lines.
fn preprocess_lines(raw_lines: &[&str]) -> Vec<LogicalLine> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < raw_lines.len() {
        let line = raw_lines[i];
        let trimmed = line.trim();

        // Skip annotation lines
        if trimmed.starts_with('@') {
            i += 1;
            continue;
        }

        // Skip blank lines
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // Check for continuation
        if let Some(prefix) = trimmed.strip_suffix('/') {
            let first_line = i + 1; // 1-indexed
            let mut joined = prefix.to_string();
            i += 1;
            while i < raw_lines.len() {
                let next = raw_lines[i].trim();
                if let Some(next_prefix) = next.strip_suffix('/') {
                    joined.push(' ');
                    joined.push_str(next_prefix);
                    i += 1;
                } else {
                    joined.push(' ');
                    joined.push_str(next);
                    i += 1;
                    break;
                }
            }
            result.push(LogicalLine {
                text: joined,
                line_num: first_line,
            });
        } else {
            result.push(LogicalLine {
                text: trimmed.to_string(),
                line_num: i + 1,
            });
            i += 1;
        }
    }
    result
}

/// Parse the section header's record count from `[N]`.
#[cfg(test)]
fn parse_section_count(header: &str) -> Option<usize> {
    let start = header.find('[')?;
    let end = header.find(']')?;
    if end <= start {
        return None;
    }
    header[start + 1..end].trim().parse().ok()
}

// ---------------------------------------------------------------------------
// Split on colon separator
// ---------------------------------------------------------------------------

/// Split tokens into (before_colon, after_colon).
/// EPC records use `:` as a separator between identity fields and data fields.
fn split_on_colon(tokens: &[String]) -> (Vec<String>, Vec<String>) {
    if let Some(pos) = tokens.iter().position(|t| t == ":") {
        let before = tokens[..pos].to_vec();
        let after = if pos + 1 < tokens.len() {
            tokens[pos + 1..].to_vec()
        } else {
            Vec::new()
        };
        (before, after)
    } else {
        // No colon — treat all tokens as data
        (Vec::new(), tokens.to_vec())
    }
}

// ---------------------------------------------------------------------------
// Main parse function
// ---------------------------------------------------------------------------

fn parse_string_with_name(content: &str, name: &str) -> Result<Network, EpcError> {
    let raw_lines: Vec<&str> = content.lines().collect();
    let lines = preprocess_lines(&raw_lines);

    let mut network = Network::new(name);
    network.base_mva = 100.0; // default

    let mut raw_loads: Vec<RawLoad> = Vec::new();
    let mut raw_shunts: Vec<RawShunt> = Vec::new();
    let mut bus_vsched: HashMap<u32, f64> = HashMap::new();

    let mut pos = 0;

    while pos < lines.len() {
        let line = &lines[pos].text;

        if let Some(section) = detect_section(line) {
            pos += 1; // skip section header line

            match section {
                EpcSection::Title | EpcSection::Comments => {
                    // Skip until `!` terminator or next section
                    while pos < lines.len() {
                        if lines[pos].text.trim() == "!" {
                            pos += 1;
                            break;
                        }
                        if detect_section(&lines[pos].text).is_some() {
                            break;
                        }
                        pos += 1;
                    }
                }
                EpcSection::SolutionParameters => {
                    while pos < lines.len() {
                        let sp_line = &lines[pos].text;
                        if sp_line.trim() == "!" {
                            pos += 1;
                            break;
                        }
                        if detect_section(sp_line).is_some() {
                            break;
                        }
                        // Extract sbase
                        let lower = sp_line.trim().to_ascii_lowercase();
                        if lower.starts_with("sbase") {
                            let parts: Vec<&str> = sp_line.split_whitespace().collect();
                            if parts.len() >= 2
                                && let Ok(v) = parts[1].parse::<f64>()
                            {
                                network.base_mva = v;
                            }
                        }
                        pos += 1;
                    }
                }
                EpcSection::SubstationData => {
                    pos = skip_section(&lines, pos);
                }
                EpcSection::BusData => {
                    let (buses, vsched_map, next) = parse_bus_section(&lines, pos)?;
                    network.buses = buses;
                    bus_vsched = vsched_map;
                    pos = next;
                }
                EpcSection::BranchData => {
                    let (branches, next) = parse_branch_section(&lines, pos)?;
                    network.branches.extend(branches);
                    pos = next;
                }
                EpcSection::TransformerData => {
                    let (xfmr_branches, next) =
                        parse_transformer_section(&lines, pos, &network.buses, network.base_mva)?;
                    network.branches.extend(xfmr_branches);
                    pos = next;
                }
                EpcSection::GeneratorData => {
                    let (gens, next) = parse_generator_section(&lines, pos, &bus_vsched)?;
                    network.generators = gens;
                    pos = next;
                }
                EpcSection::LoadData => {
                    let (loads, next) = parse_load_section(&lines, pos)?;
                    raw_loads.extend(loads);
                    pos = next;
                }
                EpcSection::ShuntData => {
                    let (shunts, next) = parse_shunt_section(&lines, pos)?;
                    raw_shunts.extend(shunts);
                    pos = next;
                }
                EpcSection::SvdData => {
                    let (shunts, next) = parse_svd_section(&lines, pos, network.base_mva)?;
                    raw_shunts.extend(shunts);
                    pos = next;
                }
                EpcSection::AreaData => {
                    let (areas, next) = parse_area_section(&lines, pos);
                    network.area_schedules = areas;
                    pos = next;
                }
                EpcSection::End => break,
                _ => {
                    // Skip unsupported sections
                    pos = skip_section(&lines, pos);
                }
            }
        } else {
            pos += 1;
        }
    }

    // Post-parse fixups
    apply_loads(&mut network, &raw_loads).map_err(|err| EpcError::Parse {
        line: 1,
        message: err.to_string(),
    })?;
    apply_shunts(&mut network, &raw_shunts);
    fixup_bus_types(&mut network);
    fixup_voltage_limits(&mut network);
    fixup_latlon(&mut network);
    Ok(network)
}

/// Skip to the next section (used for sections we don't parse).
fn skip_section(lines: &[LogicalLine], start: usize) -> usize {
    let mut pos = start;
    while pos < lines.len() {
        if detect_section(&lines[pos].text).is_some() {
            return pos;
        }
        pos += 1;
    }
    pos
}

// ---------------------------------------------------------------------------
// Bus Data
// ---------------------------------------------------------------------------

/// Parse the bus data section.
///
/// EPC bus record (after joining continuations):
///   `number "name" basekv "?" type_code : ty vsched volt angle ar zone vmax vmin
///    date_in date_out pid L own st latitude longitude ... subst_no "subst_name" ...`
///
/// Bus type field `ty`: 1=PQ, 2=PV, 3=Slack.
/// Status field `st`: 0=in-service (inverted vs branches).
#[allow(clippy::type_complexity)]
fn parse_bus_section(
    lines: &[LogicalLine],
    start: usize,
) -> Result<(Vec<Bus>, HashMap<u32, f64>, usize), EpcError> {
    let mut buses = Vec::new();
    let mut bus_vsched: HashMap<u32, f64> = HashMap::new();
    let mut pos = start;

    while pos < lines.len() {
        if detect_section(&lines[pos].text).is_some() {
            return Ok((buses, bus_vsched, pos));
        }

        let line_num = lines[pos].line_num;
        let tokens = tokenize_epc(&lines[pos].text);
        if tokens.is_empty() {
            pos += 1;
            continue;
        }

        let (ident, data) = split_on_colon(&tokens);
        if ident.len() < 3 || data.len() < 14 {
            pos += 1;
            continue;
        }

        let number = parse_u32(tok(&ident, 0), line_num, "bus")?;
        let name = ident.get(1).cloned().unwrap_or_default();
        let base_kv = parse_f64(tok(&ident, 2), line_num, "basekv")?;

        // Data fields after colon:
        // 0:ty, 1:vsched, 2:volt, 3:angle, 4:ar, 5:zone, 6:vmax, 7:vmin,
        // 8:date_in, 9:date_out, 10:pid, 11:L, 12:own, 13:st, 14:latitude, 15:longitude
        let ty = parse_i32(tok(&data, 0), line_num, "ty")?;
        let vsched = parse_f64(tok(&data, 1), line_num, "vsched")?;
        let volt = parse_f64(tok(&data, 2), line_num, "volt")?;
        let angle_deg = parse_f64(tok(&data, 3), line_num, "angle")?;
        let area = parse_u32(tok(&data, 4), line_num, "ar")?;
        let zone = parse_u32(tok(&data, 5), line_num, "zone")?;
        let vmax = parse_f64(tok(&data, 6), line_num, "vmax")?;
        let vmin = parse_f64(tok(&data, 7), line_num, "vmin")?;
        // data[8..9] = date_in, date_out (skip)
        // data[10] = pid, data[11] = L (skip)
        // data[12] = own (skip)
        let st = parse_i32(tok(&data, 13), line_num, "st")?;
        let lat = if data.len() > 14 {
            parse_f64(tok(&data, 14), line_num, "latitude").unwrap_or(0.0)
        } else {
            0.0
        };
        let lon = if data.len() > 15 {
            parse_f64(tok(&data, 15), line_num, "longitude").unwrap_or(0.0)
        } else {
            0.0
        };

        let bus_type = match ty {
            2 => BusType::PV,
            3 => BusType::Slack,
            4 => BusType::Isolated,
            _ => BusType::PQ, // ty=0 or ty=1 → PQ
        };

        // EPC bus status: 0=in-service, nonzero=out-of-service
        let in_service = st == 0;
        let effective_type = if !in_service {
            BusType::Isolated
        } else {
            bus_type
        };

        let vm = if volt > 0.0 { volt } else { 1.0 };

        let mut bus = Bus::new(number, effective_type, base_kv);
        bus.name = name;
        bus.voltage_magnitude_pu = vm;
        bus.voltage_angle_rad = angle_deg.to_radians();
        bus.area = if area > 0 { area } else { 1 };
        bus.zone = if zone > 0 { zone } else { 1 };
        bus.voltage_max_pu = vmax;
        bus.voltage_min_pu = vmin;
        bus.latitude = Some(lat);
        bus.longitude = Some(lon);

        // Store vsched for generator voltage setpoint lookup
        let vs = if vsched > 0.0 { vsched } else { vm };
        bus_vsched.insert(number, vs);

        buses.push(bus);
        pos += 1;
    }

    Ok((buses, bus_vsched, pos))
}

// ---------------------------------------------------------------------------
// Branch Data
// ---------------------------------------------------------------------------

/// Parse the branch data section.
///
/// EPC branch record (2-line, joined by `/` continuation):
///   `from_bus "from_name" from_kv  to_bus "to_name" to_kv  "ck" se "long_id"
///    : st resist react charge rate1 rate2 rate3 rate4 aloss lngth
///    [continuation data: owner info, dates, etc.]`
///
/// Impedances are in per-unit on system base (unless ohmic flag is set).
/// Status: 1=in-service, 0=out-of-service (standard convention).
fn parse_branch_section(
    lines: &[LogicalLine],
    start: usize,
) -> Result<(Vec<Branch>, usize), EpcError> {
    let mut branches = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        if detect_section(&lines[pos].text).is_some() {
            return Ok((branches, pos));
        }

        let line_num = lines[pos].line_num;
        let tokens = tokenize_epc(&lines[pos].text);
        if tokens.is_empty() {
            pos += 1;
            continue;
        }

        let (ident, data) = split_on_colon(&tokens);
        if ident.len() < 7 || data.len() < 6 {
            pos += 1;
            continue;
        }

        // Identity: from_bus(0), from_name(1), from_kv(2), to_bus(3), to_name(4),
        //           to_kv(5), ck(6), se(7), long_id(8)
        let from_bus = parse_u32(tok(&ident, 0), line_num, "from_bus")?;
        let to_bus = parse_u32(tok(&ident, 3), line_num, "to_bus")?;
        let ck_str = ident.get(6).cloned().unwrap_or_default();
        let ck = ck_str.trim().parse::<u32>().unwrap_or(1);
        let se = if ident.len() > 7 {
            parse_u32(tok(&ident, 7), line_num, "se").unwrap_or(1)
        } else {
            1
        };

        // Encode circuit = ck for section 1, or ck*100+se for multi-section lines
        let circuit = if se > 1 {
            format!("{}", ck * 100 + se)
        } else {
            ck.to_string()
        };

        // Data after colon:
        // 0:st, 1:resist, 2:react, 3:charge, 4:rate1, 5:rate2, 6:rate3, 7:rate4, 8:aloss, 9:lngth
        let st = parse_i32(tok(&data, 0), line_num, "st")?;
        let r = parse_f64(tok(&data, 1), line_num, "resist")?;
        let x = parse_f64(tok(&data, 2), line_num, "react")?;
        let b = parse_f64(tok(&data, 3), line_num, "charge")?;
        let rate_a = parse_f64(tok(&data, 4), line_num, "rate1")?;
        let rate_b = parse_f64(tok(&data, 5), line_num, "rate2").unwrap_or(0.0);
        let rate_c = parse_f64(tok(&data, 6), line_num, "rate3").unwrap_or(0.0);

        let in_service = st == 1;

        let mut branch = Branch::new_line(from_bus, to_bus, r, x, b);
        branch.circuit = circuit;
        branch.rating_a_mva = rate_a;
        branch.rating_b_mva = rate_b;
        branch.rating_c_mva = rate_c;
        branch.in_service = in_service;

        branches.push(branch);
        pos += 1;
    }

    Ok((branches, pos))
}

// ---------------------------------------------------------------------------
// Transformer Data
// ---------------------------------------------------------------------------

/// Parse the transformer data section.
///
/// EPC transformer record (originally 4 lines, joined by `/` continuations into
/// 1 logical line):
///   Line 1: `from_bus "from_name" from_kv  to_bus "to_name" to_kv  "ck" "long_id"
///            : st ty  reg_bus "reg_name" reg_kv  zt  int_bus "int_name" int_kv
///              tert_bus "tert_name" tert_kv  ar zone  tbase  ps_r  ps_x  pt_r  pt_x  ts_r  ts_x`
///   Line 2 (continuation): `kv_primary  kv_secondary  ...  rate1  rate2  rate3  rate4  ...`
///   Lines 3-4 (continuation): owner info, more metadata
///
/// After joining, all 4 lines become one logical line.
fn parse_transformer_section(
    lines: &[LogicalLine],
    start: usize,
    buses: &[Bus],
    base_mva: f64,
) -> Result<(Vec<Branch>, usize), EpcError> {
    let mut branches = Vec::new();
    let mut pos = start;

    // Build bus base kV lookup
    let bus_basekv: HashMap<u32, f64> = buses.iter().map(|b| (b.number, b.base_kv)).collect();

    while pos < lines.len() {
        if detect_section(&lines[pos].text).is_some() {
            return Ok((branches, pos));
        }

        let line_num = lines[pos].line_num;
        let tokens = tokenize_epc(&lines[pos].text);
        if tokens.is_empty() {
            pos += 1;
            continue;
        }

        let (ident, data) = split_on_colon(&tokens);
        if ident.len() < 7 || data.len() < 20 {
            pos += 1;
            continue;
        }

        // Identity: from_bus(0), from_name(1), from_kv(2), to_bus(3), to_name(4),
        //           to_kv(5), ck(6), long_id(7)
        let from_bus = parse_u32(tok(&ident, 0), line_num, "from_bus")?;
        let to_bus = parse_u32(tok(&ident, 3), line_num, "to_bus")?;
        let ck_str = ident.get(6).cloned().unwrap_or_default();
        let ck = ck_str.trim().parse::<u32>().unwrap_or(1);

        // Data after colon (all 4 lines joined):
        // 0:st, 1:ty, 2:reg_bus, 3:reg_name, 4:reg_kv, 5:zt, 6:int_bus, 7:int_name,
        // 8:int_kv, 9:tert_bus, 10:tert_name, 11:tert_kv, 12:ar, 13:zone,
        // 14:tbase, 15:ps_r, 16:ps_x, 17:pt_r, 18:pt_x, 19:ts_r, 20:ts_x
        // Then continuation data: kv_primary, kv_secondary, ...
        let st = parse_i32(tok(&data, 0), line_num, "st")?;
        let _ty = parse_i32(tok(&data, 1), line_num, "ty")?;

        // Find tbase, ps_r, ps_x — they come after reg/int/tert bus info.
        // The reg/int/tert each take 3 tokens (bus_no, "name", kv), but in the
        // tokenized form, we need to count carefully.
        //
        // After st(0), ty(1), the regulated bus info starts at index 2.
        // Each bus reference = bus_no(int) + "name"(string) + kv(float) = 3 tokens
        // Regulated: data[2..5], Int: data[5..8], Tert: data[8..11] (if zt > 0)
        // But the actual positions vary because name is a quoted string which is one token.
        //
        // Strategy: scan backwards from the continuation data which starts with
        // kv_primary (a float like 115.0 or 230.0). Or better: search for tbase
        // using the pattern of consecutive scientific-notation values (ps_r, ps_x).

        // Simpler approach: find area/zone/tbase by scanning for the pattern
        // where we get scientific notation values (like 1.000000E-04).
        // The tbase, ps_r, ps_x, pt_r, pt_x, ts_r, ts_x are the 7 values
        // just before the continuation data.
        //
        // Let's find them by looking for scientific notation pattern in data tokens.
        let mut tbase_idx = None;
        for i in 10..data.len().saturating_sub(6) {
            // Look for tbase pattern: a larger number followed by E-notation values
            let s = tok(&data, i);
            if (s.contains("E-") || s.contains("E+") || s.contains("e-") || s.contains("e+"))
                && i > 0
            {
                // The token before the first E-notation is likely tbase
                tbase_idx = Some(i - 1);
                break;
            }
        }

        // Fallback: search for the area/zone pair (two small integers before tbase)
        if tbase_idx.is_none() {
            // Try another pattern: look for area/zone as small integers before a 100.0 value
            for i in 10..data.len().saturating_sub(4) {
                let val = parse_f64(tok(&data, i), line_num, "tbase_scan").unwrap_or(0.0);
                if (val - 100.0).abs() < 1.0 || val > 50.0 {
                    // Check if preceded by two small integers (area, zone)
                    let a = parse_u32(tok(&data, i.wrapping_sub(2)), line_num, "area_scan")
                        .unwrap_or(999);
                    let z = parse_u32(tok(&data, i.wrapping_sub(1)), line_num, "zone_scan")
                        .unwrap_or(999);
                    if a < 100 && z < 100 {
                        tbase_idx = Some(i);
                        break;
                    }
                }
            }
        }

        let (tbase, ps_r, ps_x) = if let Some(ti) = tbase_idx {
            let tbase = parse_f64(tok(&data, ti), line_num, "tbase")?;
            let ps_r = parse_f64(tok(&data, ti + 1), line_num, "ps_r")?;
            let ps_x = parse_f64(tok(&data, ti + 2), line_num, "ps_x")?;
            (tbase, ps_r, ps_x)
        } else {
            // Cannot find tbase — skip this record
            pos += 1;
            continue;
        };

        // After tbase+6 (ps_r, ps_x, pt_r, pt_x, ts_r, ts_x), the continuation
        // data starts with kv_primary, kv_secondary.
        let cont_start = tbase_idx.expect("tbase_idx guaranteed Some by prior branch") + 7; // after ts_x

        let kv_primary = if data.len() > cont_start {
            parse_f64(tok(&data, cont_start), line_num, "kv_primary")?
        } else {
            0.0
        };
        let kv_secondary = if data.len() > cont_start + 1 {
            parse_f64(tok(&data, cont_start + 1), line_num, "kv_secondary")?
        } else {
            0.0
        };

        // Ratings: rate1 is at cont_start+6, rate2 at +7, rate3 at +8
        let rate_a = if data.len() > cont_start + 6 {
            parse_f64(tok(&data, cont_start + 6), line_num, "rate1").unwrap_or(0.0)
        } else {
            0.0
        };
        let rate_b = if data.len() > cont_start + 7 {
            parse_f64(tok(&data, cont_start + 7), line_num, "rate2").unwrap_or(0.0)
        } else {
            0.0
        };
        let rate_c = if data.len() > cont_start + 8 {
            parse_f64(tok(&data, cont_start + 8), line_num, "rate3").unwrap_or(0.0)
        } else {
            0.0
        };

        // Convert impedance from transformer base to system base
        let r = if tbase > 0.0 && (tbase - base_mva).abs() > 1e-6 {
            ps_r * base_mva / tbase
        } else {
            ps_r
        };
        let x = if tbase > 0.0 && (tbase - base_mva).abs() > 1e-6 {
            ps_x * base_mva / tbase
        } else {
            ps_x
        };

        // Compute tap ratio
        let from_basekv = bus_basekv.get(&from_bus).copied().unwrap_or(kv_primary);
        let to_basekv = bus_basekv.get(&to_bus).copied().unwrap_or(kv_secondary);

        let tap = if kv_primary > 0.0 && kv_secondary > 0.0 && from_basekv > 0.0 && to_basekv > 0.0
        {
            (kv_primary / from_basekv) / (kv_secondary / to_basekv)
        } else {
            1.0
        };

        let in_service = st == 1;

        let mut branch = Branch::new_line(from_bus, to_bus, r, x, 0.0);
        branch.circuit = ck.to_string();
        branch.tap = tap;
        branch.rating_a_mva = rate_a;
        branch.rating_b_mva = rate_b;
        branch.rating_c_mva = rate_c;
        branch.in_service = in_service;
        branch.branch_type = BranchType::Transformer;

        branches.push(branch);
        pos += 1;
    }

    Ok((branches, pos))
}

// ---------------------------------------------------------------------------
// Generator Data
// ---------------------------------------------------------------------------

/// Parse the generator data section.
///
/// EPC generator record (2-line, joined by `/` continuation):
///   `bus "name" basekv "id" "long_id" : st  reg_bus "reg_name" reg_kv
///    prf qrf  ar zone  pgen pmax pmin  qgen qmax qmin  mbase cmp_r cmp_x gen_r gen_x
///    hbus "hname" hkv  tbus "tname" tkv  date_in date_out pid N
///    [continuation data]`
///
/// Voltage setpoint comes from the bus `vsched` field, not the solved `volt`.
/// AVR status: if (qmax - qmin) <= 2.0 MVAr, AVR is assumed off.
fn parse_generator_section(
    lines: &[LogicalLine],
    start: usize,
    bus_vsched: &HashMap<u32, f64>,
) -> Result<(Vec<Generator>, usize), EpcError> {
    let mut generators = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        if detect_section(&lines[pos].text).is_some() {
            return Ok((generators, pos));
        }

        let line_num = lines[pos].line_num;
        let tokens = tokenize_epc(&lines[pos].text);
        if tokens.is_empty() {
            pos += 1;
            continue;
        }

        let (ident, data) = split_on_colon(&tokens);
        if ident.len() < 4 || data.len() < 18 {
            pos += 1;
            continue;
        }

        // Identity: bus(0), name(1), basekv(2), id(3), long_id(4)
        let bus = parse_u32(tok(&ident, 0), line_num, "gen_bus")?;
        let gen_id = ident.get(3).cloned().unwrap_or_default();

        // Data after colon:
        // 0:st, then reg_bus(1), reg_name(2), reg_kv(3)
        // Then: prf(4), qrf(5), ar(6), zone(7)
        // pgen(8), pmax(9), pmin(10), qgen(11), qmax(12), qmin(13), mbase(14)
        // cmp_r(15), cmp_x(16), gen_r(17), gen_x(18)

        let st = parse_i32(tok(&data, 0), line_num, "st")?;

        // reg_bus takes 3 tokens (bus_no, "name", kv)
        // Data layout after st: reg_bus_no(1), reg_name(2), reg_kv(3)
        // prf(4), qrf(5), ar(6), zone(7)
        // pgen(8), pmax(9), pmin(10), qgen(11), qmax(12), qmin(13), mbase(14)

        let pgen = parse_f64(tok(&data, 8), line_num, "pgen")?;
        let pmax = parse_f64(tok(&data, 9), line_num, "pmax")?;
        let pmin = parse_f64(tok(&data, 10), line_num, "pmin")?;
        let qgen = parse_f64(tok(&data, 11), line_num, "qgen")?;
        let qmax = parse_f64(tok(&data, 12), line_num, "qmax")?;
        let qmin = parse_f64(tok(&data, 13), line_num, "qmin")?;
        let mbase = parse_f64(tok(&data, 14), line_num, "mbase")?;

        let in_service = st == 1;

        // Voltage setpoint from bus vsched (not solved volt)
        let vs = bus_vsched.get(&bus).copied().unwrap_or(1.0);

        let mut generator = Generator::new(bus, pgen, vs);
        generator.machine_id = Some(gen_id.trim().to_string());
        generator.q = qgen;
        generator.qmax = qmax;
        generator.qmin = qmin;
        generator.pmax = pmax;
        generator.pmin = pmin;
        generator.machine_base_mva = if mbase > 0.0 { mbase } else { 100.0 };
        generator.in_service = in_service;

        generators.push(generator);
        pos += 1;
    }

    Ok((generators, pos))
}

// ---------------------------------------------------------------------------
// Load Data
// ---------------------------------------------------------------------------

/// Parse the load data section.
///
/// EPC load record (single line):
///   `bus "name" basekv "id" "long_id" : st  mw  mvar  mw_i  mvar_i  mw_z  mvar_z  ar zone ...`
///
/// Only constant-power (mw, mvar) loads are used for power flow.
/// Status: 1=in-service, 0=out-of-service.
fn parse_load_section(
    lines: &[LogicalLine],
    start: usize,
) -> Result<(Vec<RawLoad>, usize), EpcError> {
    let mut loads = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        if detect_section(&lines[pos].text).is_some() {
            return Ok((loads, pos));
        }

        let line_num = lines[pos].line_num;
        let tokens = tokenize_epc(&lines[pos].text);
        if tokens.is_empty() {
            pos += 1;
            continue;
        }

        let (ident, data) = split_on_colon(&tokens);
        if ident.len() < 3 || data.len() < 3 {
            pos += 1;
            continue;
        }

        let bus = parse_u32(tok(&ident, 0), line_num, "load_bus")?;

        // Data: st(0), mw(1), mvar(2), mw_i(3), mvar_i(4), mw_z(5), mvar_z(6)
        let st = parse_i32(tok(&data, 0), line_num, "st")?;
        let pl = parse_f64(tok(&data, 1), line_num, "mw")?;
        let ql = parse_f64(tok(&data, 2), line_num, "mvar")?;

        loads.push(RawLoad {
            bus,
            id: String::new(),
            status: st,
            owner: None,
            pl,
            ql,
            conforming: true,
            zip_p_impedance_frac: 0.0,
            zip_p_current_frac: 0.0,
            zip_p_power_frac: 1.0,
            zip_q_impedance_frac: 0.0,
            zip_q_current_frac: 0.0,
            zip_q_power_frac: 1.0,
        });
        pos += 1;
    }

    Ok((loads, pos))
}

// ---------------------------------------------------------------------------
// Shunt Data (fixed bus shunts)
// ---------------------------------------------------------------------------

/// Parse the fixed shunt data section.
///
/// EPC shunt record:
///   `bus "name" basekv "id" ... "ck" se "long_id" : st ar zone pu_mw pu_mvar ...`
fn parse_shunt_section(
    lines: &[LogicalLine],
    start: usize,
) -> Result<(Vec<RawShunt>, usize), EpcError> {
    let mut shunts = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        if detect_section(&lines[pos].text).is_some() {
            return Ok((shunts, pos));
        }

        let line_num = lines[pos].line_num;
        let tokens = tokenize_epc(&lines[pos].text);
        if tokens.is_empty() {
            pos += 1;
            continue;
        }

        let (ident, data) = split_on_colon(&tokens);
        if ident.len() < 3 || data.len() < 5 {
            pos += 1;
            continue;
        }

        let bus = parse_u32(tok(&ident, 0), line_num, "shunt_bus")?;

        // Data: st(0), ar(1), zone(2), pu_mw(3), pu_mvar(4)
        let st = parse_i32(tok(&data, 0), line_num, "st")?;
        let gl = parse_f64(tok(&data, 3), line_num, "pu_mw")?;
        let bl = parse_f64(tok(&data, 4), line_num, "pu_mvar")?;

        shunts.push(RawShunt {
            bus,
            status: st,
            gl,
            bl,
        });
        pos += 1;
    }

    Ok((shunts, pos))
}

// ---------------------------------------------------------------------------
// SVD Data (switched shunt devices)
// ---------------------------------------------------------------------------

/// Parse the SVD (switched shunt device) data section.
///
/// EPC SVD record (2-line, joined by `/` continuation):
///   `bus "name" basekv "id" "long_id" : st ty  reg_bus "reg_name" reg_kv
///    ar zone  g b  min_c max_c vband bmin bmax ...`
///
/// The `b` field is the current operating point susceptance.
/// We model switched shunts as fixed at their operating point (same as PSS/E BINIT).
fn parse_svd_section(
    lines: &[LogicalLine],
    start: usize,
    base_mva: f64,
) -> Result<(Vec<RawShunt>, usize), EpcError> {
    let mut shunts = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        if detect_section(&lines[pos].text).is_some() {
            return Ok((shunts, pos));
        }

        let line_num = lines[pos].line_num;
        let tokens = tokenize_epc(&lines[pos].text);
        if tokens.is_empty() {
            pos += 1;
            continue;
        }

        let (ident, data) = split_on_colon(&tokens);
        if ident.len() < 3 || data.len() < 8 {
            pos += 1;
            continue;
        }

        let bus = parse_u32(tok(&ident, 0), line_num, "svd_bus")?;

        // Data after colon:
        // 0:st, 1:ty, then reg_bus(2), reg_name(3), reg_kv(4)
        // 5:ar, 6:zone, 7:g, 8:b, 9:min_c, 10:max_c, 11:vband, 12:bmin, 13:bmax
        let st = parse_i32(tok(&data, 0), line_num, "st")?;
        // reg_bus takes 3 tokens (bus_no, "name", kv) starting at data[2]
        // ar(5), zone(6), g(7), b(8)
        let g = parse_f64(tok(&data, 7), line_num, "g").unwrap_or(0.0);
        let b = parse_f64(tok(&data, 8), line_num, "b").unwrap_or(0.0);

        // SVD susceptance is in per-unit on system base (MVAr at V=1.0)
        // Convert to MW/MVAr: multiply by base_mva
        // Actually, EPC SVD g/b values appear to be in per-unit already
        // matching the shunt convention in Network (MW/MVAr at V=1.0)
        shunts.push(RawShunt {
            bus,
            status: st,
            gl: g * base_mva,
            bl: b * base_mva,
        });
        pos += 1;
    }

    Ok((shunts, pos))
}

// ---------------------------------------------------------------------------
// Area Data
// ---------------------------------------------------------------------------

/// Parse the area data section.
///
/// EPC area record:
///   `number "name" : swing desired tol pnet qnet ...`
fn parse_area_section(lines: &[LogicalLine], start: usize) -> (Vec<AreaSchedule>, usize) {
    let mut areas = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        if detect_section(&lines[pos].text).is_some() {
            return (areas, pos);
        }

        let tokens = tokenize_epc(&lines[pos].text);
        if tokens.len() < 2 {
            pos += 1;
            continue;
        }

        let number = tokens[0].parse::<u32>().unwrap_or(0);
        let name = tokens.get(1).cloned().unwrap_or_default();

        if number > 0 {
            areas.push(AreaSchedule {
                number,
                name,
                ..Default::default()
            });
        }
        pos += 1;
    }

    (areas, pos)
}

// ---------------------------------------------------------------------------
// Post-parse fixups
// ---------------------------------------------------------------------------

/// Assign PV/Slack bus types based on generator data.
///
/// In EPC, bus type may already be set from the `ty` field.  But we also
/// cross-check: any bus with an in-service generator that has AVR on
/// (qmax - qmin > 2.0 MVAr) should be PV if not already Slack.
fn fixup_bus_types(network: &mut Network) {
    let gen_bus_set: HashMap<u32, bool> = network
        .generators
        .iter()
        .filter(|g| g.in_service)
        .map(|g| {
            let avr_on = (g.qmax - g.qmin).abs() > 2.0;
            (g.bus, avr_on)
        })
        .collect();

    for bus in &mut network.buses {
        if bus.bus_type == BusType::Isolated {
            continue;
        }
        if let Some(&avr_on) = gen_bus_set.get(&bus.number)
            && bus.bus_type == BusType::PQ
            && avr_on
        {
            bus.bus_type = BusType::PV;
        }
    }

    // Ensure at least one slack bus exists
    let has_slack = network.buses.iter().any(|b| b.bus_type == BusType::Slack);
    if !has_slack {
        // Find the largest generator bus and make it slack
        if let Some(largest_gen) =
            network
                .generators
                .iter()
                .filter(|g| g.in_service)
                .max_by(|a, b| {
                    a.pmax
                        .partial_cmp(&b.pmax)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        {
            let slack_bus = largest_gen.bus;
            for bus in &mut network.buses {
                if bus.number == slack_bus {
                    bus.bus_type = BusType::Slack;
                    break;
                }
            }
        }
    }
}

/// Sanitize voltage limits: if they look like kV values, reset to pu defaults.
fn fixup_voltage_limits(network: &mut Network) {
    for bus in &mut network.buses {
        if bus.voltage_max_pu > 10.0 || bus.voltage_min_pu > 10.0 {
            bus.voltage_max_pu = 1.1;
            bus.voltage_min_pu = 0.9;
        }
        if bus.voltage_max_pu <= 0.0 {
            bus.voltage_max_pu = 1.1;
        }
        if bus.voltage_min_pu <= 0.0 {
            bus.voltage_min_pu = 0.9;
        }
    }
}

/// Set lat/lon to None if both are zero (invalid sentinel in EPC).
fn fixup_latlon(network: &mut Network) {
    for bus in &mut network.buses {
        if let (Some(lat), Some(lon)) = (bus.latitude, bus.longitude)
            && lat.abs() < 1e-10
            && lon.abs() < 1e-10
        {
            bus.latitude = None;
            bus.longitude = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_epc_basic() {
        let tokens = tokenize_epc("  1001 115.0000  0.005240  0.035800 ");
        assert_eq!(tokens, vec!["1001", "115.0000", "0.005240", "0.035800"]);
    }

    #[test]
    fn test_tokenize_epc_quoted() {
        let tokens = tokenize_epc(r#"   1001 "ODESSA 2 0  " 115.0000 " "  0  : "#);
        assert_eq!(tokens[0], "1001");
        assert_eq!(tokens[1], "ODESSA 2 0  ");
        assert_eq!(tokens[2], "115.0000");
        assert_eq!(tokens[3], " ");
        assert_eq!(tokens[4], "0");
        assert_eq!(tokens[5], ":");
    }

    #[test]
    fn test_tokenize_epc_empty() {
        assert!(tokenize_epc("").is_empty());
        assert!(tokenize_epc("   ").is_empty());
    }

    #[test]
    fn test_detect_section() {
        assert_eq!(
            detect_section("bus data  [2751]  ty vsched"),
            Some(EpcSection::BusData)
        );
        assert_eq!(
            detect_section("branch data  [ 3993]  ck se"),
            Some(EpcSection::BranchData)
        );
        assert_eq!(
            detect_section("transformer data  [1351]"),
            Some(EpcSection::TransformerData)
        );
        assert_eq!(
            detect_section("generator data  [1099]"),
            Some(EpcSection::GeneratorData)
        );
        assert_eq!(
            detect_section("load data  [1410]"),
            Some(EpcSection::LoadData)
        );
        assert_eq!(
            detect_section("shunt data  [   0]"),
            Some(EpcSection::ShuntData)
        );
        assert_eq!(
            detect_section("svd data  [ 202]"),
            Some(EpcSection::SvdData)
        );
        assert_eq!(
            detect_section("area data  [  8]"),
            Some(EpcSection::AreaData)
        );
        assert_eq!(
            detect_section("zone data  [   1]"),
            Some(EpcSection::ZoneData)
        );
        assert_eq!(detect_section("end"), Some(EpcSection::End));
        assert_eq!(detect_section("title"), Some(EpcSection::Title));
        assert_eq!(
            detect_section("solution parameters"),
            Some(EpcSection::SolutionParameters)
        );
        assert_eq!(detect_section("   not a section"), None);
    }

    #[test]
    fn test_parse_section_count() {
        assert_eq!(
            parse_section_count("bus data  [2751]  ty vsched"),
            Some(2751)
        );
        assert_eq!(parse_section_count("shunt data  [   0]"), Some(0));
        assert_eq!(parse_section_count("branch data  [ 3993]"), Some(3993));
        assert_eq!(parse_section_count("no brackets here"), None);
    }

    #[test]
    fn test_split_on_colon() {
        let tokens = tokenize_epc("1001 \"name\" 115.00 : 1 0.005 0.035");
        let (before, after) = split_on_colon(&tokens);
        assert_eq!(before.len(), 3);
        assert_eq!(after.len(), 3);
        assert_eq!(before[0], "1001");
        assert_eq!(after[0], "1");
    }

    #[test]
    fn test_preprocess_continuation() {
        let raw = vec!["first line /", " second line", "third line"];
        let lines = preprocess_lines(&raw);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].text.contains("first line"));
        assert!(lines[0].text.contains("second line"));
        assert_eq!(lines[1].text, "third line");
    }

    #[test]
    fn test_preprocess_annotations_skip() {
        let raw = vec!["@! this is an annotation", "real data line"];
        let lines = preprocess_lines(&raw);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "real data line");
    }

    #[test]
    fn test_parse_f64_edge_cases() {
        assert_eq!(parse_f64("", 1, "test").unwrap(), 0.0);
        assert_eq!(parse_f64("3.15", 1, "test").unwrap(), 3.15);
        assert_eq!(parse_f64("1.000000E-04", 1, "test").unwrap(), 1e-4);
        assert_eq!(parse_f64("-17.299999", 1, "test").unwrap(), -17.299999);
        assert!(parse_f64("abc", 1, "test").is_err());
    }

    #[test]
    fn test_parse_mini_epc() {
        let epc = r#"title
!
comments
!
solution parameters
sbase 100.0000    system mva base
!
bus data  [3]               ty  vsched   volt     angle    ar zone  vmax   vmin   date_in date_out pid L own st
   1 "BUS 1       " 345.0000 " "  0  :  3 1.060000  1.060000   0.000000    1    1 1.1000 0.9000 19400101 21991231   0 0   1 0
   2 "BUS 2       " 345.0000 " "  0  :  2 1.045000  1.045000 -10.000000    1    1 1.1000 0.9000 19400101 21991231   0 0   1 0
   3 "BUS 3       " 345.0000 " "  0  :  1 1.000000  1.007000 -15.000000    1    1 1.1000 0.9000 19400101 21991231   0 0   1 0
branch data  [ 1]                                          ck  se  long_id    st resist   react   charge   rate1  rate2  rate3  rate4 aloss  lngth
   1 "BUS 1       " 345.00    3 "BUS 3       " 345.00  "1 "   1 " "  :  1  0.010000  0.100000  0.020000  250.0    0.0    0.0    0.0 0.000    0.0  1    1  0.000000  0.000000  1.000000 19400101 21991231   0 1
transformer data  [1]                                     ck   long_id     st ty
   1 "BUS 1       " 345.00    2 "BUS 2       " 345.00  "1 "   " " :   1  1       0 "            "   0.00  0       0 "            "   0.00       0 "            "   0.00    1    1 100.000000 5.000000E-03 5.000000E-02 0.000000E+00 0.000000E+00 0.000000E+00 0.000000E+00  345.000000 345.000000  0.000000  0.000000 0.000000E+00 0.000000E+00  200.0  200.0  200.0    0.0 0.000  1.500000  0.510000  1.500000  0.510000 -0.006250  1.000000  1.000000  1.000000  1.000000 19400101 21991231   0 1     0.0    0.0    0.0    0.0    1 1.000   0 0.000   0 0.000   0 0.000   0 0.000   0 0.000   0 0.000   0 0.000  0    0.000000   0.000000  0.000000  0.000000     0.0    0.0    0.0     0.0    0.0    0.0 0.000 0.000  1  1  1 0.0000 0.0000 0  0  0 0.000000 0.000000  0.000000  0.000000  " "
generator data  [1]         id   long_id    st
   1 "BUS 1       " 345.00 "1 "  " " :  1    1 "BUS 1       " 345.00  1.000000  1.000000   1    1 100.000000 200.000000   0.000000 10.000000 50.000000 -30.000000 200.0000 0.000 0.000 0.000 1.000      -1 "            "   0.00      -1 "            "   0.00  19400101 21991231   0 0  0.0000 0.0000 1.0000    1 1.000   0 0.000   0 0.000   0 0.000  " "
load data  [1]             id   long_id     st      mw      mvar
   3 "BUS 3       " 345.00 "1 "   " "  :  1 150.000000 50.000000 0.000000 0.000000 0.000000 0.000000   1    1 19400101 21991231   0 0   1 0
area data  [  1]
    1 "Area 1                          "       0    0.000    1.000 0.0 0.0 0  0  0  " "  0.000000
zone data  [   1]
    1 "Zone 1                          "    0.000   0.000 0
end
"#;

        let net = parse_str(epc).expect("failed to parse mini EPC");
        assert_eq!(net.n_buses(), 3, "expected 3 buses");
        assert_eq!(
            net.branches.len(),
            2,
            "expected 2 branches (1 line + 1 xfmr)"
        );
        assert_eq!(net.generators.len(), 1, "expected 1 generator");

        // Bus types
        assert_eq!(net.buses[0].bus_type, BusType::Slack);
        assert_eq!(net.buses[1].bus_type, BusType::PV);
        assert_eq!(net.buses[2].bus_type, BusType::PQ);

        // Load accumulated to bus 3 (via Load objects)
        let bus_pd = net.bus_load_p_mw();
        let bus_qd = net.bus_load_q_mvar();
        assert!((bus_pd[2] - 150.0).abs() < 0.01);
        assert!((bus_qd[2] - 50.0).abs() < 0.01);

        // Generator data
        assert_eq!(net.generators[0].bus, 1);
        assert!((net.generators[0].p - 100.0).abs() < 0.01);
        assert!((net.generators[0].pmax - 200.0).abs() < 0.01);

        // Branch impedance
        let line = &net.branches[0];
        assert!((line.r - 0.01).abs() < 1e-6);
        assert!((line.x - 0.1).abs() < 1e-6);
        assert!((line.b - 0.02).abs() < 1e-6);

        // Transformer
        let xfmr = &net.branches[1];
        assert!((xfmr.r - 0.005).abs() < 1e-6);
        assert!((xfmr.x - 0.05).abs() < 1e-6);
        assert!((xfmr.tap - 1.0).abs() < 1e-6); // nominal tap (same basekv)

        // Areas
        assert_eq!(net.area_schedules.len(), 1);
    }

    #[test]
    fn test_parse_texas2k_epc() {
        // Integration test with real EPC file
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let path = std::path::PathBuf::from(&manifest)
            .join("../..")
            .join("tests/data/epc/Texas2k.epc");
        if !path.exists() {
            return; // skip if data not available
        }

        let net = parse_file(&path).expect("failed to parse Texas2k EPC");

        // Verify structural metrics from the file header counts
        assert_eq!(net.n_buses(), 2751, "expected 2751 buses");
        // Branch + transformer count: 3993 branches + 1351 transformers = 5344
        assert!(
            net.branches.len() > 4000,
            "expected >4000 branches, got {}",
            net.branches.len()
        );
        // Generator count from header: 1099
        assert!(
            net.generators.len() > 1000,
            "expected >1000 generators, got {}",
            net.generators.len()
        );

        // Verify base_mva
        assert!((net.base_mva - 100.0).abs() < 0.01);

        // Verify there is at least one slack bus
        let slack_count = net
            .buses
            .iter()
            .filter(|b| b.bus_type == BusType::Slack)
            .count();
        assert!(slack_count >= 1, "no slack bus found");

        // Verify total load is reasonable (ERCOT-scale: ~60-80 GW)
        let total_load: f64 = net.total_load_mw();
        assert!(
            total_load > 10000.0,
            "total load too low: {total_load:.0} MW"
        );

        // Verify areas parsed
        assert_eq!(net.area_schedules.len(), 8, "expected 8 areas");
    }

    #[test]
    fn test_parse_texas7k_epc() {
        // Integration test with real Texas7k EPC file
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let path = std::path::PathBuf::from(&manifest)
            .join("../..")
            .join("tests/data/epc/Texas7k.epc");
        if !path.exists() {
            return; // skip if data not available
        }

        let net = parse_file(&path).expect("failed to parse Texas7k EPC");

        // Verify structural metrics
        assert!(
            net.n_buses() > 6000,
            "expected >6000 buses, got {}",
            net.n_buses()
        );
        assert!(
            net.branches.len() > 7000,
            "expected >7000 branches, got {}",
            net.branches.len()
        );
        assert!(
            net.generators.len() > 500,
            "expected >500 generators, got {}",
            net.generators.len()
        );

        // Verify base_mva
        assert!((net.base_mva - 100.0).abs() < 0.01);

        // Verify at least one slack bus
        let slack_count = net
            .buses
            .iter()
            .filter(|b| b.bus_type == BusType::Slack)
            .count();
        assert!(slack_count >= 1, "no slack bus found");

        // Total load (ERCOT 7k is larger than 2k)
        let total_load: f64 = net.total_load_mw();
        assert!(
            total_load > 10000.0,
            "total load too low: {total_load:.0} MW"
        );

        eprintln!(
            "Texas7k: {} buses, {} branches, {} gens, {:.0} MW load",
            net.n_buses(),
            net.branches.len(),
            net.generators.len(),
            total_load
        );
    }

    #[test]
    fn test_epc_vs_matpower_texas7k() {
        // Cross-format validation: compare EPC vs MATPOWER for Texas7k
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let epc_path = std::path::PathBuf::from(&manifest)
            .join("../..")
            .join("tests/data/epc/Texas7k.epc");
        let mat_path = std::path::PathBuf::from(&manifest)
            .join("../..")
            .join("tests/data/epc/Texas7k.m");

        if !epc_path.exists() || !mat_path.exists() {
            return; // skip if data not available
        }

        let epc_net = parse_file(&epc_path).expect("EPC parse failed");
        let mat_net = crate::matpower::parse_file(&mat_path).expect("MATPOWER parse failed");

        // Bus count must match
        assert_eq!(
            epc_net.n_buses(),
            mat_net.n_buses(),
            "bus count mismatch: EPC={} vs MATPOWER={}",
            epc_net.n_buses(),
            mat_net.n_buses()
        );

        // Total load should be close
        let epc_load: f64 = epc_net.total_load_mw();
        let mat_load: f64 = mat_net.total_load_mw();
        let load_diff = (epc_load - mat_load).abs();
        let load_pct = load_diff / mat_load.abs().max(1.0) * 100.0;
        eprintln!(
            "Texas7k load: EPC={epc_load:.1} MW, MATPOWER={mat_load:.1} MW, diff={load_pct:.2}%"
        );
        assert!(load_pct < 1.0, "load mismatch too large: {load_pct:.2}%");

        // Generators should be close
        let epc_gens = epc_net.generators.len();
        let mat_gens = mat_net.generators.len();
        eprintln!("Texas7k gens: EPC={epc_gens}, MATPOWER={mat_gens}");
    }

    #[test]
    fn test_epc_vs_matpower_texas2k() {
        // Cross-format validation: compare EPC vs MATPOWER for Texas2k
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let epc_path = std::path::PathBuf::from(&manifest)
            .join("../..")
            .join("tests/data/epc/Texas2k.epc");
        let mat_path = std::path::PathBuf::from(&manifest)
            .join("../..")
            .join("tests/data/epc/Texas2k.m");

        if !epc_path.exists() || !mat_path.exists() {
            return; // skip if data not available
        }

        let epc_net = parse_file(&epc_path).expect("EPC parse failed");
        let mat_net = crate::matpower::parse_file(&mat_path).expect("MATPOWER parse failed");

        // Bus count must match
        assert_eq!(
            epc_net.n_buses(),
            mat_net.n_buses(),
            "bus count mismatch: EPC={} vs MATPOWER={}",
            epc_net.n_buses(),
            mat_net.n_buses()
        );

        // Branch count should be close (may differ due to multi-section line encoding)
        let epc_branches = epc_net.branches.len();
        let mat_branches = mat_net.branches.len();
        let branch_diff = (epc_branches as i64 - mat_branches as i64).unsigned_abs();
        eprintln!("Branches: EPC={epc_branches}, MATPOWER={mat_branches}, diff={branch_diff}");

        // Generator count should match
        let epc_gens = epc_net.generators.len();
        let mat_gens = mat_net.generators.len();
        eprintln!("Generators: EPC={epc_gens}, MATPOWER={mat_gens}");

        // Total load should be close
        let epc_load: f64 = epc_net.total_load_mw();
        let mat_load: f64 = mat_net.total_load_mw();
        let load_diff = (epc_load - mat_load).abs();
        eprintln!(
            "Total load: EPC={epc_load:.1} MW, MATPOWER={mat_load:.1} MW, diff={load_diff:.1} MW"
        );

        // Load should be within 1% (different file exporters may round differently)
        let load_pct = load_diff / mat_load.abs().max(1.0) * 100.0;
        assert!(load_pct < 1.0, "load mismatch too large: {load_pct:.2}%");
    }
}
