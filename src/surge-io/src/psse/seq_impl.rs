// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSS/E Sequence Data (.seq) parser.
//!
//! Parses PSS/E sequence impedance files and applies zero-sequence / negative-sequence
//! data to an existing [`Network`]. The `.seq` file is a companion to `.raw` — it uses
//! the same bus numbering, machine IDs, and circuit identifiers.
//!
//! # Sections (in order, each terminated by `Q` or `0 /`)
//!
//! 1. **Machine** — positive/negative/zero-sequence impedance + grounding
//! 2. **Branch** — zero-sequence R0, X0, B0 for transmission lines
//! 3. **Mutual impedance** — zero-sequence coupling between parallel circuits
//! 4. **Two-winding transformer** — connection code + zero-sequence impedance
//! 5. **Switched shunt** — zero-sequence susceptance (skipped)
//! 6. **Three-winding transformer** — connection code + zero-sequence impedance
//!
//! # Example
//!
//! ```ignore
//! let mut net = surge_io::psse::raw::load("case.raw")?;
//! let stats = surge_io::psse::sequence::apply(&mut net, "case.seq")?;
//! println!("Updated {} machines, {} branches", stats.machines_updated, stats.branches_updated);
//! ```

use std::collections::HashMap;
use std::path::Path;

use num_complex::Complex64;
use surge_network::Network;
use surge_network::network::model::MutualCoupling;
use surge_network::network::{TransformerConnection, TransformerData, ZeroSeqData};
use thiserror::Error;
use tracing::warn;

/// Errors that can occur when parsing a `.seq` file.
#[derive(Debug, Error)]
pub enum SeqError {
    /// I/O error reading the file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Parse error at a specific line.
    #[error("parse error at line {line}: {message}")]
    Parse { line: usize, message: String },
}

/// Statistics from applying a `.seq` file to a network.
#[derive(Debug, Clone, Default)]
pub struct SeqStats {
    /// Number of generator (machine) records applied.
    pub machines_updated: usize,
    /// Number of branch (transmission line) records applied.
    pub branches_updated: usize,
    /// Number of transformer connection records applied.
    pub transformers_updated: usize,
    /// Number of mutual coupling records applied.
    pub mutual_couplings: usize,
    /// Number of records skipped (orphaned bus/circuit, malformed, etc.).
    pub skipped_records: usize,
}

/// Which section of the `.seq` file we're currently parsing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Section {
    Machine,
    Branch,
    Mutual,
    TwoWindingTransformer,
    SwitchedShunt,
    ThreeWindingTransformer,
    Done,
}

impl Section {
    fn next(self) -> Self {
        match self {
            Self::Machine => Self::Branch,
            Self::Branch => Self::Mutual,
            Self::Mutual => Self::TwoWindingTransformer,
            Self::TwoWindingTransformer => Self::SwitchedShunt,
            Self::SwitchedShunt => Self::ThreeWindingTransformer,
            Self::ThreeWindingTransformer => Self::Done,
            Self::Done => Self::Done,
        }
    }
}

/// Parse a PSS/E `.seq` file from disk and apply to an existing network.
pub fn parse_file(network: &mut Network, path: &Path) -> Result<SeqStats, SeqError> {
    let content = std::fs::read_to_string(path)?;
    parse_str(network, &content)
}

/// Parse a PSS/E `.seq` format string and apply to an existing network.
pub fn parse_str(network: &mut Network, content: &str) -> Result<SeqStats, SeqError> {
    let mut stats = SeqStats::default();
    let mut section = Section::Machine;

    // Build lookup maps for matching records to network elements.
    let gen_map = build_gen_map(network);
    let branch_map = build_branch_map(network);

    for (line_idx, raw_line) in content.lines().enumerate() {
        let line_num = line_idx + 1;

        // Strip inline comments (PSS/E uses ! for inline, @! for full-line).
        let line = strip_comment(raw_line);
        let trimmed = line.trim();

        // Skip empty lines.
        if trimmed.is_empty() {
            continue;
        }

        // Check for section terminator: Q record or 0 / record.
        if is_section_terminator(trimmed) {
            section = section.next();
            if section == Section::Done {
                break;
            }
            continue;
        }

        // Parse the record based on current section.
        match section {
            Section::Machine => {
                parse_machine_record(trimmed, line_num, network, &gen_map, &mut stats);
            }
            Section::Branch => {
                parse_branch_record(trimmed, line_num, network, &branch_map, &mut stats);
            }
            Section::Mutual => {
                parse_mutual_record(trimmed, line_num, network, &mut stats);
            }
            Section::TwoWindingTransformer => {
                parse_2w_xfmr_record(trimmed, line_num, network, &branch_map, &mut stats);
            }
            Section::SwitchedShunt => {
                // Skip switched shunt zero-sequence data (low priority).
                stats.skipped_records += 1;
            }
            Section::ThreeWindingTransformer => {
                parse_3w_xfmr_record(trimmed, line_num, network, &branch_map, &mut stats);
            }
            Section::Done => break,
        }
    }

    Ok(stats)
}

// ---------------------------------------------------------------------------
// Lookup map builders
// ---------------------------------------------------------------------------

/// Build a map from (bus, machine_id) → generator index.
fn build_gen_map(network: &Network) -> HashMap<(u32, String), usize> {
    let mut map = HashMap::new();
    for (i, g) in network.generators.iter().enumerate() {
        let id = g.machine_id.as_deref().unwrap_or("1").trim().to_string();
        map.insert((g.bus, id), i);
    }
    map
}

/// Build a map from (from_bus, to_bus, circuit) → branch index.
///
/// Also indexes (to_bus, from_bus, circuit) for reverse lookups since the .seq
/// file might list buses in either order.
fn build_branch_map(network: &Network) -> HashMap<(u32, u32, String), usize> {
    let mut map = HashMap::new();
    for (i, br) in network.branches.iter().enumerate() {
        map.insert((br.from_bus, br.to_bus, br.circuit.clone()), i);
        map.insert((br.to_bus, br.from_bus, br.circuit.clone()), i);
    }
    map
}

// ---------------------------------------------------------------------------
// Line/token helpers
// ---------------------------------------------------------------------------

/// Strip inline PSS/E comments (everything after `!` or `@!`).
fn strip_comment(line: &str) -> &str {
    // @! at the start means the whole line is a comment.
    if line.trim_start().starts_with("@!") {
        return "";
    }
    // ! anywhere means inline comment.
    match line.find('!') {
        Some(pos) => &line[..pos],
        None => line,
    }
}

/// Check if a line is a section terminator.
///
/// PSS/E uses `Q` (uppercase) or `0 /` or `0,` as section terminators.
fn is_section_terminator(trimmed: &str) -> bool {
    if trimmed == "Q" || trimmed == "q" {
        return true;
    }
    // "0 /" or "0, ..." pattern — first token is "0" and either followed by / or end.
    let first = trimmed.split([',', ' ', '\t']).next().unwrap_or("");
    if first == "0" {
        // Check if there's a slash after the 0
        let rest = trimmed[first.len()..].trim_start_matches([',', ' ', '\t']);
        if rest.is_empty() || rest.starts_with('/') {
            return true;
        }
    }
    false
}

/// Tokenize a line: split by commas and whitespace, strip quotes.
fn tokenize(line: &str) -> Vec<String> {
    line.split(',')
        .flat_map(|segment| segment.split_whitespace())
        .map(|t| t.trim_matches('\'').trim_matches('"').to_string())
        .filter(|t| !t.is_empty() && t != "/")
        .collect()
}

/// Parse a float from a token, returning None on failure.
fn parse_f64(token: &str) -> Option<f64> {
    // Handle Fortran-style D exponent (e.g., 1.5D-3 → 1.5E-3).
    let normalized = token.replace('D', "E").replace('d', "e");
    normalized.parse::<f64>().ok()
}

/// Parse an integer from a token.
fn parse_i64(token: &str) -> Option<i64> {
    token.parse::<i64>().ok()
}

// ---------------------------------------------------------------------------
// Section parsers
// ---------------------------------------------------------------------------

/// Parse a machine (generator) sequence impedance record.
///
/// Format: `I, ID, ZRPOS, ZXPOS, ZRNEG, ZXNEG, RZERO, XZERO, ZRGRND, ZXGRND`
///
/// All impedances are in per-unit on the machine MVA base.
fn parse_machine_record(
    line: &str,
    line_num: usize,
    network: &mut Network,
    gen_map: &HashMap<(u32, String), usize>,
    stats: &mut SeqStats,
) {
    let tokens = tokenize(line);
    if tokens.len() < 4 {
        warn!(line = line_num, "skipping short machine record: {}", line);
        stats.skipped_records += 1;
        return;
    }

    let bus = match parse_i64(&tokens[0]) {
        Some(b) => b.unsigned_abs() as u32,
        None => {
            warn!(line = line_num, "invalid bus number in machine record");
            stats.skipped_records += 1;
            return;
        }
    };
    let machine_id = tokens[1].trim().to_string();

    let gen_idx = match gen_map.get(&(bus, machine_id.clone())) {
        Some(&idx) => idx,
        None => {
            // Try with default ID "1" if not found.
            match gen_map.get(&(bus, "1".to_string())) {
                Some(&idx) => idx,
                None => {
                    warn!(
                        line = line_num,
                        bus,
                        id = machine_id,
                        "orphaned machine record — generator not found"
                    );
                    stats.skipped_records += 1;
                    return;
                }
            }
        }
    };

    let g = &mut network.generators[gen_idx];

    // ZRPOS, ZXPOS — positive-sequence impedance (indices 2, 3).
    // We don't overwrite xs since it's already set from .raw.

    // ZRNEG, ZXNEG — negative-sequence impedance (indices 4, 5).
    if let (Some(r2), Some(x2)) = (
        tokens.get(4).and_then(|t| parse_f64(t)),
        tokens.get(5).and_then(|t| parse_f64(t)),
    ) {
        if x2.abs() > 1e-20 {
            g.fault_data.get_or_insert_with(Default::default).x2_pu = Some(x2);
        }
        if r2.abs() > 1e-20 {
            g.fault_data.get_or_insert_with(Default::default).r2_pu = Some(r2);
        }
    }

    // RZERO, XZERO — zero-sequence impedance (indices 6, 7).
    if let (Some(r0), Some(x0)) = (
        tokens.get(6).and_then(|t| parse_f64(t)),
        tokens.get(7).and_then(|t| parse_f64(t)),
    ) {
        if x0.abs() > 1e-20 {
            g.fault_data.get_or_insert_with(Default::default).x0_pu = Some(x0);
        }
        if r0.abs() > 1e-20 {
            g.fault_data.get_or_insert_with(Default::default).r0_pu = Some(r0);
        }
    }

    // ZRGRND, ZXGRND — neutral grounding impedance (indices 8, 9).
    // Stored on machine base in .seq; our Generator.zn is on system base.
    // Convert: Zn_sys = Zn_mach × (base_mva / mbase).
    if let (Some(rn), Some(xn)) = (
        tokens.get(8).and_then(|t| parse_f64(t)),
        tokens.get(9).and_then(|t| parse_f64(t)),
    ) && (rn.abs() > 1e-20 || xn.abs() > 1e-20)
    {
        let mbase = if g.machine_base_mva.abs() < 1e-10 {
            network.base_mva
        } else {
            g.machine_base_mva
        };
        let scale = network.base_mva / mbase;
        g.fault_data.get_or_insert_with(Default::default).zn =
            Some(Complex64::new(rn * scale, xn * scale));
    }

    stats.machines_updated += 1;
}

/// Parse a branch (transmission line) zero-sequence record.
///
/// Format: `I, J, CKT, RLINZ, XLINZ, BCHZ, GI, BI, GJ, BJ`
///
/// R0, X0, B0 are in per-unit on the system base.
fn parse_branch_record(
    line: &str,
    line_num: usize,
    network: &mut Network,
    branch_map: &HashMap<(u32, u32, String), usize>,
    stats: &mut SeqStats,
) {
    let tokens = tokenize(line);
    if tokens.len() < 6 {
        warn!(line = line_num, "skipping short branch record: {}", line);
        stats.skipped_records += 1;
        return;
    }

    let from = match parse_i64(&tokens[0]) {
        Some(b) => b.unsigned_abs() as u32,
        None => {
            stats.skipped_records += 1;
            return;
        }
    };
    let to = match parse_i64(&tokens[1]) {
        Some(b) => b.unsigned_abs() as u32,
        None => {
            stats.skipped_records += 1;
            return;
        }
    };
    let circuit = tokens[2].trim_start_matches('&').to_string();

    let br_idx = match branch_map.get(&(from, to, circuit.clone())) {
        Some(&idx) => idx,
        None => {
            warn!(
                line = line_num,
                from, to, circuit, "orphaned branch record — branch not found"
            );
            stats.skipped_records += 1;
            return;
        }
    };

    let rlinz = tokens.get(3).and_then(|t| parse_f64(t)).unwrap_or(0.0);
    let xlinz = tokens.get(4).and_then(|t| parse_f64(t)).unwrap_or(0.0);
    let bchz = tokens.get(5).and_then(|t| parse_f64(t)).unwrap_or(0.0);
    let gi = tokens.get(6).and_then(|t| parse_f64(t)).unwrap_or(0.0);
    let bi = tokens.get(7).and_then(|t| parse_f64(t)).unwrap_or(0.0);
    let gj = tokens.get(8).and_then(|t| parse_f64(t)).unwrap_or(0.0);
    let bj = tokens.get(9).and_then(|t| parse_f64(t)).unwrap_or(0.0);

    let br = &mut network.branches[br_idx];
    let zs = br.zero_seq.get_or_insert_with(ZeroSeqData::default);
    zs.r0 = rlinz;
    zs.x0 = xlinz;
    zs.b0 = bchz;
    zs.gi0 = gi;
    zs.bi0 = bi;
    zs.gj0 = gj;
    zs.bj0 = bj;

    stats.branches_updated += 1;
}

/// Parse a mutual impedance record.
///
/// Format: `I1, J1, CKT1, I2, J2, CKT2, RM, XM`
fn parse_mutual_record(line: &str, line_num: usize, network: &mut Network, stats: &mut SeqStats) {
    let tokens = tokenize(line);
    if tokens.len() < 8 {
        warn!(line = line_num, "skipping short mutual impedance record");
        stats.skipped_records += 1;
        return;
    }

    let i1 = parse_i64(&tokens[0]).unwrap_or(0).unsigned_abs() as u32;
    let j1 = parse_i64(&tokens[1]).unwrap_or(0).unsigned_abs() as u32;
    let _ckt1 = &tokens[2];
    let i2 = parse_i64(&tokens[3]).unwrap_or(0).unsigned_abs() as u32;
    let j2 = parse_i64(&tokens[4]).unwrap_or(0).unsigned_abs() as u32;
    let _ckt2 = &tokens[5];
    let rm = parse_f64(&tokens[6]).unwrap_or(0.0);
    let xm = parse_f64(&tokens[7]).unwrap_or(0.0);

    if rm.abs() > 1e-20 || xm.abs() > 1e-20 {
        // Store as mutual coupling: use bus-pair identifiers as terminal MRIDs.
        let term1 = format!("{}-{}", i1, j1);
        let term2 = format!("{}-{}", i2, j2);
        network.cim.mutual_couplings.push(MutualCoupling {
            line1_id: term1,
            line2_id: term2,
            r: rm,
            x: xm,
        });
        stats.mutual_couplings += 1;
    } else {
        stats.skipped_records += 1;
    }
}

/// Map PSS/E connection code (CC) to our TransformerConnection enum.
fn cc_to_connection(cc: i64) -> Option<TransformerConnection> {
    match cc {
        1 => Some(TransformerConnection::WyeGWyeG),
        2 => Some(TransformerConnection::WyeGDelta),
        3 => Some(TransformerConnection::DeltaWyeG),
        4 => Some(TransformerConnection::DeltaDelta),
        5 => Some(TransformerConnection::WyeGWye),
        _ => None,
    }
}

/// Parse a two-winding transformer zero-sequence record.
///
/// Format: `I, J, CKT, CC, RG1, XG1, R01, X01, RG2, XG2, R02, X02`
fn parse_2w_xfmr_record(
    line: &str,
    line_num: usize,
    network: &mut Network,
    branch_map: &HashMap<(u32, u32, String), usize>,
    stats: &mut SeqStats,
) {
    let tokens = tokenize(line);
    if tokens.len() < 4 {
        warn!(line = line_num, "skipping short 2W transformer record");
        stats.skipped_records += 1;
        return;
    }

    let from = parse_i64(&tokens[0]).unwrap_or(0).unsigned_abs() as u32;
    let to = parse_i64(&tokens[1]).unwrap_or(0).unsigned_abs() as u32;
    let circuit = tokens[2].trim_start_matches('&').to_string();
    let cc = parse_i64(&tokens[3]).unwrap_or(1);

    let br_idx = match branch_map.get(&(from, to, circuit.clone())) {
        Some(&idx) => idx,
        None => {
            warn!(
                line = line_num,
                from, to, circuit, "orphaned 2W transformer record — branch not found"
            );
            stats.skipped_records += 1;
            return;
        }
    };

    // Set transformer connection type.
    if let Some(conn) = cc_to_connection(cc) {
        network.branches[br_idx]
            .transformer_data
            .get_or_insert_with(TransformerData::default)
            .transformer_connection = conn;
    }

    // R01, X01 — winding 1 zero-sequence impedance (indices 6, 7).
    let r01 = tokens.get(6).and_then(|t| parse_f64(t)).unwrap_or(0.0);
    let x01 = tokens.get(7).and_then(|t| parse_f64(t)).unwrap_or(0.0);
    // R02, X02 — winding 2 zero-sequence impedance (indices 10, 11).
    let r02 = tokens.get(10).and_then(|t| parse_f64(t)).unwrap_or(0.0);
    let x02 = tokens.get(11).and_then(|t| parse_f64(t)).unwrap_or(0.0);

    // For WyeGWyeG transformers, set r0/x0 as the combined winding impedance.
    // For other connection types, the topology handling in sequence.rs already
    // takes care of blocking — r0/x0 serve as informational zero-seq impedance.
    if (r01.abs() + x01.abs() + r02.abs() + x02.abs()) > 1e-20 {
        let zs = network.branches[br_idx]
            .zero_seq
            .get_or_insert_with(ZeroSeqData::default);
        zs.r0 = r01 + r02;
        zs.x0 = x01 + x02;
    }

    stats.transformers_updated += 1;
}

/// Parse a three-winding transformer zero-sequence record.
///
/// Format: `I, J, K, CKT, CC, RG1, XG1, R01, X01, RG2, XG2, R02, X02, RG3, XG3, R03, X03`
///
/// 3-winding transformers are already star-expanded into 3 branches in the network.
/// We match by bus pairs to find the corresponding star-expanded branches.
fn parse_3w_xfmr_record(
    line: &str,
    line_num: usize,
    network: &mut Network,
    branch_map: &HashMap<(u32, u32, String), usize>,
    stats: &mut SeqStats,
) {
    let tokens = tokenize(line);
    if tokens.len() < 5 {
        warn!(line = line_num, "skipping short 3W transformer record");
        stats.skipped_records += 1;
        return;
    }

    let bus_i = parse_i64(&tokens[0]).unwrap_or(0).unsigned_abs() as u32;
    let bus_j = parse_i64(&tokens[1]).unwrap_or(0).unsigned_abs() as u32;
    let bus_k = parse_i64(&tokens[2]).unwrap_or(0).unsigned_abs() as u32;
    let circuit = tokens[3].trim_start_matches('&').to_string();
    let cc = parse_i64(&tokens[4]).unwrap_or(1);

    let conn = cc_to_connection(cc);

    // 3-winding transformers are star-expanded: the star bus has a synthetic number.
    // Try to find branches connecting bus_i, bus_j, bus_k to a common star bus.
    // The star bus is typically numbered as a large synthetic bus.
    // Since we can't always predict the star bus number, try matching by the
    // original bus numbers + circuit in the branch map.
    let winding_data: [(u32, usize, usize); 3] = [
        (bus_i, 7, 8),   // R01, X01 at token indices 7, 8
        (bus_j, 10, 11), // R02, X02 at token indices 10, 11
        (bus_k, 13, 14), // R03, X03 at token indices 13, 14
    ];

    let mut found_any = false;

    for &(bus, r_idx, x_idx) in &winding_data {
        // Try to find a branch from this bus to any star bus with the right circuit.
        // The star-expanded branches use the same circuit number as the original transformer.
        if let Some(&br_idx) = branch_map
            .get(&(bus_i, bus_j, circuit.clone()))
            .or_else(|| branch_map.get(&(bus_i, bus_k, circuit.clone())))
            .or_else(|| branch_map.get(&(bus_j, bus_k, circuit.clone())))
        {
            // Found a matching branch — set connection and zero-seq data.
            if let Some(c) = conn {
                network.branches[br_idx]
                    .transformer_data
                    .get_or_insert_with(TransformerData::default)
                    .transformer_connection = c;
            }
            let r0 = tokens.get(r_idx).and_then(|t| parse_f64(t)).unwrap_or(0.0);
            let x0 = tokens.get(x_idx).and_then(|t| parse_f64(t)).unwrap_or(0.0);
            if r0.abs() + x0.abs() > 1e-20 {
                let zs = network.branches[br_idx]
                    .zero_seq
                    .get_or_insert_with(ZeroSeqData::default);
                zs.r0 = r0;
                zs.x0 = x0;
            }
            found_any = true;
        }

        // Also try direct lookup with this specific bus.
        for &other in &[bus_i, bus_j, bus_k] {
            if other == bus {
                continue;
            }
            if let Some(&br_idx) = branch_map.get(&(bus, other, circuit.clone())) {
                if let Some(c) = conn {
                    network.branches[br_idx]
                        .transformer_data
                        .get_or_insert_with(TransformerData::default)
                        .transformer_connection = c;
                }
                let r0 = tokens.get(r_idx).and_then(|t| parse_f64(t)).unwrap_or(0.0);
                let x0 = tokens.get(x_idx).and_then(|t| parse_f64(t)).unwrap_or(0.0);
                if r0.abs() + x0.abs() > 1e-20 {
                    let zs = network.branches[br_idx]
                        .zero_seq
                        .get_or_insert_with(ZeroSeqData::default);
                    zs.r0 = r0;
                    zs.x0 = x0;
                }
                found_any = true;
            }
        }
    }

    if found_any {
        stats.transformers_updated += 1;
    } else {
        warn!(
            line = line_num,
            bus_i,
            bus_j,
            bus_k,
            circuit,
            "orphaned 3W transformer record — no matching branches found"
        );
        stats.skipped_records += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_network() -> Network {
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("seq_test");
        net.base_mva = 100.0;
        net.buses = vec![
            {
                let mut b = Bus::new(1, BusType::Slack, 230.0);
                b.name = "Bus1".into();
                b.voltage_magnitude_pu = 1.04;
                b
            },
            {
                let mut b = Bus::new(2, BusType::PV, 230.0);
                b.name = "Bus2".into();
                b.voltage_magnitude_pu = 1.025;
                b
            },
            {
                let mut b = Bus::new(3, BusType::PQ, 230.0);
                b.name = "Bus3".into();
                b
            },
        ];
        net.loads = vec![Load::new(2, 21.7, 12.7), Load::new(3, 94.2, 19.0)];
        net.generators = vec![
            {
                let mut g = Generator::new(1, 71.6, 1.04);
                g.machine_id = Some("1".to_string());
                g.q = 27.0;
                g.machine_base_mva = 100.0;
                g.fault_data.get_or_insert_with(Default::default).xs = Some(0.15);
                g.pmax = 200.0;
                g
            },
            {
                let mut g = Generator::new(2, 163.0, 1.025);
                g.machine_id = Some("1".to_string());
                g.q = 6.7;
                g.machine_base_mva = 200.0;
                g.fault_data.get_or_insert_with(Default::default).xs = Some(0.20);
                g.pmax = 300.0;
                g
            },
        ];
        net.branches = vec![
            Branch::new_line(1, 2, 0.01, 0.085, 0.176),
            Branch::new_line(2, 3, 0.032, 0.161, 0.306),
            {
                let mut br = Branch::new_line(1, 3, 0.005, 0.10, 0.0);
                br.tap = 1.05;
                br.rating_a_mva = 300.0;
                br
            },
        ];
        net
    }

    #[test]
    fn test_seq_machine_section() {
        let mut net = make_test_network();
        let seq_data = "\
1, '1', 0.003, 0.15, 0.008, 0.17, 0.005, 0.12, 0.0, 0.1
2, '1', 0.004, 0.20, 0.010, 0.22, 0.006, 0.14, 0.02, 0.15
Q
Q
Q
Q
Q
Q
";
        let stats = parse_str(&mut net, seq_data).unwrap();
        assert_eq!(stats.machines_updated, 2);
        assert_eq!(stats.skipped_records, 0);

        // Gen 1: X2=0.17, R2=0.008, X0=0.12, R0=0.005, Zn=(0, 0.1) scaled.
        let g1 = &net.generators[0];
        let fd1 = g1.fault_data.as_ref().expect("gen1 fault_data");
        assert!((fd1.x2_pu.unwrap() - 0.17).abs() < 1e-10);
        assert!((fd1.r2_pu.unwrap() - 0.008).abs() < 1e-10);
        assert!((fd1.x0_pu.unwrap() - 0.12).abs() < 1e-10);
        assert!((fd1.r0_pu.unwrap() - 0.005).abs() < 1e-10);
        // Zn: mbase=100, base_mva=100, scale=1.0, so Zn = (0.0, 0.1).
        let zn1 = fd1.zn.unwrap();
        assert!(zn1.re.abs() < 1e-10);
        assert!((zn1.im - 0.1).abs() < 1e-10);

        // Gen 2: mbase=200, base_mva=100, scale=0.5.
        // Zn_sys = (0.02*0.5, 0.15*0.5) = (0.01, 0.075).
        let g2 = &net.generators[1];
        let fd2 = g2.fault_data.as_ref().expect("gen2 fault_data");
        assert!((fd2.x2_pu.unwrap() - 0.22).abs() < 1e-10);
        assert!((fd2.x0_pu.unwrap() - 0.14).abs() < 1e-10);
        let zn2 = fd2.zn.unwrap();
        assert!((zn2.re - 0.01).abs() < 1e-10);
        assert!((zn2.im - 0.075).abs() < 1e-10);
    }

    #[test]
    fn test_seq_branch_section() {
        let mut net = make_test_network();
        let seq_data = "\
Q
1, 2, 1, 0.04, 0.30, 0.10, 0.0, 0.0, 0.0, 0.0
2, 3, 1, 0.10, 0.50, 0.15, 0.0, 0.0, 0.0, 0.0
Q
Q
Q
Q
Q
";
        let stats = parse_str(&mut net, seq_data).unwrap();
        assert_eq!(stats.branches_updated, 2);

        let br1 = &net.branches[0]; // 1→2
        let zs1 = br1.zero_seq.as_ref().expect("zero_seq should be set");
        assert!((zs1.r0 - 0.04).abs() < 1e-10);
        assert!((zs1.x0 - 0.30).abs() < 1e-10);
        assert!((zs1.b0 - 0.10).abs() < 1e-10);

        let br2 = &net.branches[1]; // 2→3
        let zs2 = br2.zero_seq.as_ref().expect("zero_seq should be set");
        assert!((zs2.r0 - 0.10).abs() < 1e-10);
        assert!((zs2.x0 - 0.50).abs() < 1e-10);
        assert!((zs2.b0 - 0.15).abs() < 1e-10);
        // GI/BI/GJ/BJ defaulted to 0 when all zero.
        assert_eq!(zs1.gi0, 0.0);
        assert_eq!(zs1.bi0, 0.0);
        assert_eq!(zs1.gj0, 0.0);
        assert_eq!(zs1.bj0, 0.0);
    }

    #[test]
    fn test_seq_branch_terminal_shunts() {
        // GI/BI/GJ/BJ tokens (6-9) parsed correctly for cable circuits.
        let mut net = make_test_network();
        let seq_data = "\
Q
1, 2, 1, 0.02, 0.15, 0.08, 0.001, 0.050, 0.002, 0.060
Q
Q
Q
Q
Q
";
        let stats = parse_str(&mut net, seq_data).unwrap();
        assert_eq!(stats.branches_updated, 1);

        let br = &net.branches[0]; // 1→2
        let zs = br.zero_seq.as_ref().expect("zero_seq should be set");
        assert!((zs.r0 - 0.02).abs() < 1e-10);
        assert!((zs.x0 - 0.15).abs() < 1e-10);
        assert!((zs.b0 - 0.08).abs() < 1e-10);
        assert!((zs.gi0 - 0.001).abs() < 1e-12, "GI parsed: {}", zs.gi0);
        assert!((zs.bi0 - 0.050).abs() < 1e-12, "BI parsed: {}", zs.bi0);
        assert!((zs.gj0 - 0.002).abs() < 1e-12, "GJ parsed: {}", zs.gj0);
        assert!((zs.bj0 - 0.060).abs() < 1e-12, "BJ parsed: {}", zs.bj0);
    }

    #[test]
    fn test_seq_transformer_connection() {
        let mut net = make_test_network();
        let seq_data = "\
Q
Q
Q
1, 3, 1, 2, 0.0, 0.0, 0.003, 0.08, 0.0, 0.0, 0.002, 0.05
Q
Q
Q
";
        let stats = parse_str(&mut net, seq_data).unwrap();
        assert_eq!(stats.transformers_updated, 1);

        let br = &net.branches[2]; // 1→3 transformer
        assert_eq!(
            br.transformer_data.as_ref().unwrap().transformer_connection,
            TransformerConnection::WyeGDelta
        );
        // R0 = R01 + R02 = 0.003 + 0.002 = 0.005.
        assert!((br.zero_seq.as_ref().unwrap().r0 - 0.005).abs() < 1e-10);
        // X0 = X01 + X02 = 0.08 + 0.05 = 0.13.
        assert!((br.zero_seq.as_ref().unwrap().x0 - 0.13).abs() < 1e-10);
    }

    #[test]
    fn test_seq_orphaned_records() {
        let mut net = make_test_network();
        let seq_data = "\
99, '1', 0.003, 0.15, 0.008, 0.17, 0.005, 0.12, 0.0, 0.1
Q
5, 6, 1, 0.04, 0.30, 0.10
Q
Q
Q
Q
Q
";
        let stats = parse_str(&mut net, seq_data).unwrap();
        assert_eq!(stats.machines_updated, 0);
        assert_eq!(stats.branches_updated, 0);
        assert_eq!(stats.skipped_records, 2);
    }
}
