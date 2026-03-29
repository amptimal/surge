// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! MATPOWER (.m) case file parser.
//!
//! Parses the MATPOWER case format, which defines bus, generator, branch,
//! and other data in MATLAB struct arrays. This is the primary format for
//! academic test cases (IEEE, PEGASE, ACTIVSg, Polish, RTE).
//!
//! # Supported sections
//! - `mpc.baseMVA` — System base power
//! - `mpc.bus` — Bus data (13 columns)
//! - `mpc.gen` — Generator data (10+ columns)
//! - `mpc.branch` — Branch data (13 columns)
//!
//! # Column mappings
//! See MATPOWER's `caseformat.m` for the canonical column definitions.

use std::path::Path;

use surge_network::Network;
use surge_network::market::CostCurve;
use surge_network::network::{
    Branch, BranchType, Bus, BusType, DcBranch, DcBus, DcConverter, DcConverterStation, Generator,
    Load,
};
use thiserror::Error;

/// Convert a MATPOWER/PSS/E integer bus type code to `BusType`.
fn bus_type_from_matpower(code: u32) -> BusType {
    match code {
        2 => BusType::PV,
        3 => BusType::Slack,
        4 => BusType::Isolated,
        _ => BusType::PQ,
    }
}

#[derive(Error, Debug)]
pub enum MatpowerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error on line {line}: {message}")]
    Parse { line: usize, message: String },

    #[error("missing required section: {0}")]
    MissingSection(String),

    #[error("insufficient columns in {section} row {row}: expected {expected}, got {got}")]
    InsufficientColumns {
        section: String,
        row: usize,
        expected: usize,
        got: usize,
    },

    #[error("invalid gencost: {0}")]
    InvalidGencost(String),

    #[error("invalid float value: {0}")]
    InvalidFloat(String),

    #[error("non-finite value in {section} row {row}: {field} = {value}")]
    NonFiniteValue {
        section: &'static str,
        row: usize,
        field: &'static str,
        value: f64,
    },

    #[error("expression nesting too deep: {0}")]
    ExpressionTooDeep(String),

    #[error("file too large: {0} bytes exceeds limit of {1} bytes")]
    FileTooLarge(u64, u64),
}

/// Parse a MATPOWER .m case file from disk.
pub fn parse_file(path: &Path) -> Result<Network, MatpowerError> {
    const MAX_FILE_SIZE: u64 = 500 * 1024 * 1024; // 500 MB
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_FILE_SIZE {
        return Err(MatpowerError::FileTooLarge(metadata.len(), MAX_FILE_SIZE));
    }
    let content = std::fs::read_to_string(path)?;
    parse_str(&content)
}

/// Parse a MATPOWER case from a string.
pub fn parse_str(content: &str) -> Result<Network, MatpowerError> {
    let mut name = String::from("unknown");
    let mut base_mva = 100.0;
    let mut bus_rows: Vec<Vec<f64>> = Vec::new();
    let mut gen_rows: Vec<Vec<f64>> = Vec::new();
    let mut branch_rows: Vec<Vec<f64>> = Vec::new();
    let mut gencost_rows: Vec<Vec<f64>> = Vec::new();
    let mut bus_name_rows: Vec<String> = Vec::new();
    let mut dc_bus_rows: Vec<Vec<f64>> = Vec::new();
    let mut dc_conv_rows: Vec<Vec<f64>> = Vec::new();
    let mut dc_branch_rows: Vec<Vec<f64>> = Vec::new();

    #[derive(PartialEq)]
    enum Section {
        None,
        Bus,
        Gen,
        Branch,
        GenCost,
        BusName,
        DcBus,
        DcConv,
        DcBranch,
        Other,
    }
    let mut section = Section::None;

    for (line_idx, raw_line) in content.lines().enumerate() {
        let line_num = line_idx + 1;

        // Strip inline comments (% to end of line).
        // Be careful not to strip % inside strings, but MATPOWER numeric data
        // never contains quoted strings in data sections.
        let line = match raw_line.find('%') {
            Some(idx) => &raw_line[..idx],
            None => raw_line,
        };
        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        // Check for cell array section end: `};` (used by mpc.bus_name and similar)
        if line.contains("};") && section == Section::BusName {
            section = Section::None;
            continue;
        }

        // Check for section end: `];` or `};` (pglib-opf-hvdc uses curly braces)
        let is_bracket_end = line.contains("];");
        let is_brace_end = line.contains("};");
        if is_bracket_end || is_brace_end {
            // Parse any data before the closing delimiter on the same line
            if section == Section::Bus
                || section == Section::Gen
                || section == Section::Branch
                || section == Section::GenCost
                || section == Section::DcBus
                || section == Section::DcConv
                || section == Section::DcBranch
            {
                let delim = if is_bracket_end { "];" } else { "};" };
                let data_part = line.split(delim).next().unwrap_or("").trim();
                let data_part = data_part
                    .trim_end_matches(';')
                    .trim_end_matches(']')
                    .trim_end_matches('}')
                    .trim();
                if !data_part.is_empty()
                    && let Some(row) = parse_numeric_row(data_part)
                {
                    match section {
                        Section::Bus => bus_rows.push(row),
                        Section::Gen => gen_rows.push(row),
                        Section::Branch => branch_rows.push(row),
                        Section::GenCost => gencost_rows.push(row),
                        Section::DcBus => dc_bus_rows.push(row),
                        Section::DcConv => dc_conv_rows.push(row),
                        Section::DcBranch => dc_branch_rows.push(row),
                        _ => {}
                    }
                }
            }
            section = Section::None;
            continue;
        }

        // Function declaration: `function mpc = case9`
        if line.starts_with("function") {
            if let Some(eq_pos) = line.find('=') {
                name = line[eq_pos + 1..]
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .to_string();
            }
            continue;
        }

        // baseMVA: `mpc.baseMVA = 100;` or `baseMVA = 100;`
        // Must NOT match `Sbase = mpc.baseMVA * 1e6;` (baseMVA on RHS)
        if line.contains("baseMVA") && line.contains('=') {
            if let Some(eq_pos) = line.find('=') {
                let lhs = line[..eq_pos].trim();
                if lhs == "mpc.baseMVA" || lhs == "baseMVA" {
                    let val_str = line[eq_pos + 1..].trim().trim_end_matches(';').trim();
                    base_mva = eval_simple_expr(val_str).ok_or_else(|| MatpowerError::Parse {
                        line: line_num,
                        message: format!("invalid baseMVA value: '{val_str}'"),
                    })?;
                }
            }
            continue;
        }

        // Section start detection
        if is_section_start(line, "bus") {
            section = Section::Bus;
            try_parse_inline_data(line, &mut bus_rows);
            continue;
        }
        if is_section_start(line, "gencost") {
            section = Section::GenCost;
            try_parse_inline_data(line, &mut gencost_rows);
            continue;
        }
        if is_section_start(line, "gen") {
            section = Section::Gen;
            try_parse_inline_data(line, &mut gen_rows);
            continue;
        }
        if is_section_start(line, "branch") {
            section = Section::Branch;
            try_parse_inline_data(line, &mut branch_rows);
            continue;
        }

        // DC network DC bus section: mpc.busdc or mpc.dcbus
        if is_section_start(line, "busdc") || is_section_start(line, "dcbus") {
            section = Section::DcBus;
            try_parse_inline_data(line, &mut dc_bus_rows);
            continue;
        }
        // DC network DC converter section: mpc.convdc or mpc.dcconv
        if is_section_start(line, "convdc") || is_section_start(line, "dcconv") {
            section = Section::DcConv;
            try_parse_inline_data(line, &mut dc_conv_rows);
            continue;
        }
        // DC network DC branch section: mpc.branchdc or mpc.dcbranch
        if is_section_start(line, "branchdc") || is_section_start(line, "dcbranch") {
            section = Section::DcBranch;
            try_parse_inline_data(line, &mut dc_branch_rows);
            continue;
        }

        // bus_name cell array: `mpc.bus_name = {`
        if line.contains("mpc.bus_name") && line.contains('=') && line.contains('{') {
            section = Section::BusName;
            continue;
        }

        // Other mpc.xxx sections — skip their contents
        if line.contains("mpc.") && line.contains('=') {
            if section != Section::Bus
                && section != Section::Gen
                && section != Section::Branch
                && section != Section::DcBus
                && section != Section::DcConv
                && section != Section::DcBranch
            {
                section = Section::Other;
            }
            continue;
        }

        // Parse data rows within active sections
        match section {
            Section::Bus
            | Section::Gen
            | Section::Branch
            | Section::GenCost
            | Section::DcBus
            | Section::DcConv
            | Section::DcBranch => {
                let row_str = line.trim_end_matches(';').trim();
                if let Some(row) = parse_numeric_row(row_str) {
                    match section {
                        Section::Bus => bus_rows.push(row),
                        Section::Gen => gen_rows.push(row),
                        Section::Branch => branch_rows.push(row),
                        Section::GenCost => gencost_rows.push(row),
                        Section::DcBus => dc_bus_rows.push(row),
                        Section::DcConv => dc_conv_rows.push(row),
                        Section::DcBranch => dc_branch_rows.push(row),
                        _ => {}
                    }
                }
            }
            Section::BusName => {
                // Each entry looks like: `\t'Riversde  V2';`
                // Only process lines that contain a quoted string.
                if line.contains('\'') {
                    bus_name_rows.push(parse_bus_name_entry(line));
                }
            }
            _ => {}
        }
    }

    // Validate we got the essential sections
    if bus_rows.is_empty() {
        return Err(MatpowerError::MissingSection("bus".into()));
    }
    if branch_rows.is_empty() {
        return Err(MatpowerError::MissingSection("branch".into()));
    }

    // Detect distribution case unit conversions from MATLAB code in the file
    let conversions = detect_conversions(content);

    // Build network
    let mut network = Network::new(&name);
    network.base_mva = base_mva;

    // Temporary storage for bus-level pd/qd before Load creation.
    let mut bus_pd_qd: Vec<(u32, f64, f64)> = Vec::new();

    // Parse buses (13 columns minimum)
    for (i, row) in bus_rows.iter().enumerate() {
        if row.len() < 13 {
            return Err(MatpowerError::InsufficientColumns {
                section: "bus".into(),
                row: i + 1,
                expected: 13,
                got: row.len(),
            });
        }
        network.buses.push(Bus {
            number: row[0] as u32,
            name: String::new(),
            bus_type: bus_type_from_matpower(row[1] as u32),
            shunt_conductance_mw: row[4],
            shunt_susceptance_mvar: row[5],
            area: row[6] as u32,
            voltage_magnitude_pu: row[7],
            voltage_angle_rad: row[8].to_radians(), // MATPOWER stores degrees
            base_kv: row[9],
            zone: row[10] as u32,
            voltage_max_pu: row[11],
            voltage_min_pu: row[12],
            // Optional solved-case columns (lam/mu) are intentionally ignored.
            island_id: 0,
            latitude: None, // lat/lon not in standard MATPOWER bus matrix
            longitude: None,
            ..Bus::new(0, BusType::PQ, 0.0)
        });
        // Store raw pd/qd for later Load creation (before conversions).
        bus_pd_qd.push((row[0] as u32, row[2], row[3]));
        // M-2: Validate critical bus fields for finiteness
        let row_num = i + 1;
        check_finite(row[2], "bus", row_num, "pd")?;
        check_finite(row[3], "bus", row_num, "qd")?;
        check_finite(row[4], "bus", row_num, "gs")?;
        check_finite(row[5], "bus", row_num, "bs")?;
        check_finite(row[7], "bus", row_num, "vm")?;
        check_finite(row[8], "bus", row_num, "va")?;
        check_finite(row[9], "bus", row_num, "base_kv")?;
        check_finite(row[11], "bus", row_num, "vmax")?;
        check_finite(row[12], "bus", row_num, "vmin")?;
    }

    // MP-05: Build a set of valid bus numbers so we can validate generator bus references.
    // A generator referencing a non-existent bus causes a panic later in Y-bus construction.
    let bus_set: std::collections::HashSet<u32> = network.buses.iter().map(|b| b.number).collect();
    let mut gen_row_to_network_idx: Vec<Option<usize>> = Vec::with_capacity(gen_rows.len());

    // Parse generators (first 10 columns of 21)
    for (i, row) in gen_rows.iter().enumerate() {
        if row.len() < 10 {
            return Err(MatpowerError::InsufficientColumns {
                section: "gen".into(),
                row: i + 1,
                expected: 10,
                got: row.len(),
            });
        }
        let gen_bus_number = row[0] as u32;
        // MP-05: Reject generators whose bus does not exist in the parsed bus list.
        if !bus_set.contains(&gen_bus_number) {
            return Err(MatpowerError::Parse {
                line: i + 1,
                message: format!("generator at bus {gen_bus_number} references missing bus"),
            });
        }
        // Optional MATPOWER cols 16-18 (0-indexed): RAMP_AGC, RAMP_10, RAMP_30
        // MATPOWER units: RAMP_AGC in MW/min; RAMP_10 in MW/10-min; RAMP_30 in MW/30-min.
        // We normalize all ramp fields to MW/min for consistent internal representation.
        // ramp_agc → reg_ramp_up_curve, ramp_10 → ramp_up_curve (preferred over ramp_30).
        let reg_ramp_up_curve: Vec<(f64, f64)> = row
            .get(16)
            .copied()
            .filter(|&v| v.abs() > 1e-20)
            .map(|v| vec![(0.0, v)])
            .unwrap_or_default();
        let ramp_up_curve: Vec<(f64, f64)> = row
            .get(17)
            .copied()
            .filter(|&v| v.abs() > 1e-20)
            .map(|v| vec![(0.0, v / 10.0)])
            .or_else(|| {
                row.get(18)
                    .copied()
                    .filter(|&v| v.abs() > 1e-20)
                    .map(|v| vec![(0.0, v / 30.0)])
            })
            .unwrap_or_default();
        let ramping = if !reg_ramp_up_curve.is_empty() || !ramp_up_curve.is_empty() {
            Some(surge_network::network::RampingParams {
                reg_ramp_up_curve,
                ramp_up_curve,
                ..Default::default()
            })
        } else {
            None
        };

        // Reactive capability curve (MATPOWER cols 10-15, 0-indexed)
        let reactive_capability = {
            let pc1_val = row.get(10).copied();
            let pc2_val = row.get(11).copied();
            let qc1min_val = row.get(12).copied();
            let qc1max_val = row.get(13).copied();
            let qc2min_val = row.get(14).copied();
            let qc2max_val = row.get(15).copied();
            // Convert MATPOWER two-point capability curve to pq_curve.
            // pc1/pc2 are P breakpoints in MW; qcX values are reactive limits in MVAr.
            // pq_curve stores per-unit on system base: (p_pu, qmax_pu, qmin_pu).
            let pc1_f = pc1_val.unwrap_or(0.0);
            let pc2_f = pc2_val.unwrap_or(0.0);
            let pq_curve = match (row.get(12), row.get(13), row.get(14), row.get(15)) {
                (Some(&qc1min), Some(&qc1max), Some(&qc2min), Some(&qc2max))
                    if (pc2_f - pc1_f).abs() > 1e-6 =>
                {
                    let inv = 1.0 / base_mva;
                    if pc1_f <= pc2_f {
                        vec![
                            (pc1_f * inv, qc1max * inv, qc1min * inv),
                            (pc2_f * inv, qc2max * inv, qc2min * inv),
                        ]
                    } else {
                        vec![
                            (pc2_f * inv, qc2max * inv, qc2min * inv),
                            (pc1_f * inv, qc1max * inv, qc1min * inv),
                        ]
                    }
                }
                _ => vec![],
            };
            let has_any = pc1_val.is_some()
                || pc2_val.is_some()
                || qc1min_val.is_some()
                || qc1max_val.is_some()
                || qc2min_val.is_some()
                || qc2max_val.is_some()
                || !pq_curve.is_empty();
            if has_any {
                Some(surge_network::network::ReactiveCapability {
                    pc1: pc1_val,
                    pc2: pc2_val,
                    qc1min: qc1min_val,
                    qc1max: qc1max_val,
                    qc2min: qc2min_val,
                    qc2max: qc2max_val,
                    pq_curve,
                })
            } else {
                None
            }
        };

        network.generators.push(Generator {
            bus: gen_bus_number,
            machine_id: None,
            p: row[1],
            q: row[2],
            qmax: row[3],
            qmin: row[4],
            voltage_setpoint_pu: row[5],
            reg_bus: None,
            machine_base_mva: if row[6].is_finite() && row[6].abs() > 1e-10 {
                row[6]
            } else {
                network.base_mva
            },
            in_service: row[7] as i32 > 0,
            pmax: row[8],
            pmin: row[9],
            cost: None,
            ramping,
            reactive_capability,
            // col 19 = ramp_q (skip — not a planning field)
            // col 20 = agc_participation_factor
            agc_participation_factor: row.get(20).copied(),
            // Optional solved-case KKT multiplier columns are intentionally ignored.
            forced_outage_rate: None,
            h_inertia_s: None,
            pfr_eligible: true,
            ..Generator::new(0, 0.0, 1.0)
        });
        gen_row_to_network_idx.push(Some(network.generators.len() - 1));
        // M-2: Validate critical generator fields for finiteness
        let row_num = i + 1;
        check_finite(row[1], "gen", row_num, "pg")?;
        check_finite(row[2], "gen", row_num, "qg")?;
        // qmax/qmin: Inf is valid in MATPOWER (means "unlimited"), only reject NaN
        if row[3].is_nan() {
            return Err(MatpowerError::NonFiniteValue {
                section: "gen",
                row: row_num,
                field: "qmax",
                value: row[3],
            });
        }
        if row[4].is_nan() {
            return Err(MatpowerError::NonFiniteValue {
                section: "gen",
                row: row_num,
                field: "qmin",
                value: row[4],
            });
        }
        check_finite(row[5], "gen", row_num, "vs")?;
        // mbase: NaN or 0 means "use system baseMVA" (standard MATPOWER convention)
    }

    // Parse gencost (optional — only present in OPF cases)
    // MATPOWER format: [type, startup, shutdown, n, data...]
    //   type 1 (piecewise-linear): data = x1, y1, x2, y2, ..., xn, yn
    //   type 2 (polynomial): data = c_{n-1}, ..., c_1, c_0
    for (i, row) in gencost_rows.iter().enumerate() {
        if i >= gen_rows.len() {
            break; // More gencost rows than generators (reactive cost rows) — skip
        }
        let Some(gen_idx) = gen_row_to_network_idx.get(i).copied().flatten() else {
            continue;
        };
        if row.len() < 4 {
            continue;
        }
        let cost_type = row[0] as i32;
        let startup = row[1];
        let shutdown = row[2];

        // MP-01: Validate n before any allocation. A float like 1e300 casts to usize::MAX
        // causing attempted 296-exabyte allocations. No valid gencost has >1000 breakpoints.
        let n_raw = row[3];
        if !(0.0..=1000.0).contains(&n_raw) || !n_raw.is_finite() {
            return Err(MatpowerError::InvalidGencost(format!(
                "gencost row {} has n={} breakpoints, must be 0–1000",
                i + 1,
                n_raw
            )));
        }
        let n = n_raw as usize;

        // Validate the row is long enough before indexing
        let expected_len = 4 + if cost_type == 1 { 2 * n } else { n };
        if row.len() < expected_len {
            return Err(MatpowerError::InvalidGencost(format!(
                "gencost row {} has {} fields, expected at least {}",
                i + 1,
                row.len(),
                expected_len
            )));
        }

        let cost = match cost_type {
            2 => {
                // Polynomial: n coefficients follow
                Some(CostCurve::Polynomial {
                    startup,
                    shutdown,
                    coeffs: row[4..4 + n].to_vec(),
                })
            }
            1 => {
                // Piecewise-linear: n (x,y) pairs follow
                let points: Vec<(f64, f64)> = (0..n)
                    .map(|j| (row[4 + 2 * j], row[4 + 2 * j + 1]))
                    .collect();
                Some(CostCurve::PiecewiseLinear {
                    startup,
                    shutdown,
                    points,
                })
            }
            _ => None,
        };

        if let Some(c) = cost {
            network.generators[gen_idx].cost = Some(c);
        }
    }

    // Parse branches (13 columns)
    for (i, row) in branch_rows.iter().enumerate() {
        if row.len() < 11 {
            return Err(MatpowerError::InsufficientColumns {
                section: "branch".into(),
                row: i + 1,
                expected: 11,
                got: row.len(),
            });
        }
        // MATPOWER convention: ratio = 0 means 1.0 for transmission lines
        let tap = if row[8] == 0.0 { 1.0 } else { row[8] };

        // MP-04: MATPOWER uses Inf (or 0.0) to signal an unconstrained branch rating.
        // Surge uses 0.0 as the unconstrained sentinel. Convert Inf → 0.0 here.
        let rate_a = if row[5].is_infinite() { 0.0 } else { row[5] };
        let rate_b = if row[6].is_infinite() { 0.0 } else { row[6] };
        let rate_c = if row[7].is_infinite() { 0.0 } else { row[7] };

        network.branches.push(Branch {
            from_bus: row[0] as u32,
            to_bus: row[1] as u32,
            circuit: "1".to_string(),
            r: row[2],
            x: row[3],
            b: row[4],
            rating_a_mva: rate_a,
            rating_b_mva: rate_b,
            rating_c_mva: rate_c,
            tap,
            phase_shift_rad: row[9].to_radians(),
            in_service: row[10] as i32 > 0,
            // Standard MATPOWER cols 11-12: angmin, angmax (degrees in file -> radians internally)
            angle_diff_min_rad: row.get(11).copied().map(f64::to_radians),
            angle_diff_max_rad: row.get(12).copied().map(f64::to_radians),
            g_pi: 0.0,
            g_mag: 0.0,
            b_mag: 0.0,
            tab: None,
            // MATPOWER: tap != 1.0 or shift != 0.0 indicates a transformer
            branch_type: if (tap - 1.0).abs() > 1e-6 || row[9].abs() > 1e-6 {
                BranchType::Transformer
            } else {
                BranchType::Line
            },
            ..Branch::default()
        });
        // M-2: Validate critical branch fields for finiteness (skip rate_a — Inf→0.0 is valid)
        let row_num = i + 1;
        check_finite(row[2], "branch", row_num, "r")?;
        check_finite(row[3], "branch", row_num, "x")?;
        check_finite(row[4], "branch", row_num, "b")?;
    }

    // MP-05: MATPOWER has no circuit column, so parallel branches between the
    // same (from_bus, to_bus) pair all got circuit "1" above.  Disambiguate by
    // assigning "1", "2", … in file order for each bus pair.
    {
        let mut counts: std::collections::HashMap<(u32, u32), u32> =
            std::collections::HashMap::new();
        for branch in &mut network.branches {
            let key = (branch.from_bus, branch.to_bus);
            let n = counts.entry(key).or_insert(0);
            *n += 1;
            branch.circuit = n.to_string();
        }
    }

    // Apply distribution case unit conversions
    apply_conversions(&mut network, &conversions, &mut bus_pd_qd);

    // Synthesize explicit Load objects from bus-level Pd/Qd.
    // MUST happen after apply_conversions() so Load MW values match the converted values.
    // MATPOWER stores loads in the bus matrix (columns 3-4). Other formats (PSS/E, CGMES)
    // have separate load records, but MATPOWER embeds them on buses.
    for &(bus_num, pd, qd) in &bus_pd_qd {
        if pd.abs() > 1e-10 || qd.abs() > 1e-10 {
            network.loads.push(Load::new(bus_num, pd, qd));
        }
    }

    // Apply bus names from mpc.bus_name (positional — index i in bus_name = index i in mpc.bus).
    // Trim trailing whitespace since PSS/E-originated files pad names to fixed width.
    for (i, name) in bus_name_rows.iter().enumerate() {
        if let Some(bus) = network.buses.get_mut(i)
            && !name.is_empty()
        {
            bus.name = name.clone();
        }
    }

    // MATPOWER convention: copy generator Vg to bus Vm for PV and REF buses.
    // In MATPOWER's ext2int, gen(:, VG) overrides bus(:, VM) for generator buses.
    // The last in-service generator at each bus sets the voltage setpoint.
    {
        let bus_map = network.bus_index_map();
        for g in &network.generators {
            if !g.in_service {
                continue;
            }
            if let Some(&idx) = bus_map.get(&g.bus) {
                let bt = network.buses[idx].bus_type;
                if bt == BusType::PV || bt == BusType::Slack {
                    network.buses[idx].voltage_magnitude_pu = g.voltage_setpoint_pu;
                }
            }
        }
    }

    // Parse DC bus rows (8 columns: busdc_i, grid, Pdc, Vdc, basekVdc, Vdcmax, Vdcmin, Cdc)
    for (row_idx, row) in dc_bus_rows.iter().enumerate() {
        if row.len() < 7 {
            return Err(MatpowerError::InsufficientColumns {
                section: "busdc".to_string(),
                row: row_idx,
                expected: 7,
                got: row.len(),
            });
        }
        network
            .hvdc
            .ensure_dc_grid(row[1] as u32, None)
            .buses
            .push(DcBus {
                bus_id: row[0] as u32,
                p_dc_mw: row[2],
                v_dc_pu: row[3],
                base_kv_dc: row[4],
                v_dc_max: row[5],
                v_dc_min: row[6],
                cost: row.get(7).copied().unwrap_or(0.0),
                g_shunt_siemens: 0.0,
                r_ground_ohm: 0.0,
            });
    }

    // Parse DC converter rows (up to 34 columns)
    for (row_idx, row) in dc_conv_rows.iter().enumerate() {
        if row.len() < 22 {
            return Err(MatpowerError::InsufficientColumns {
                section: "convdc".to_string(),
                row: row_idx,
                expected: 22,
                got: row.len(),
            });
        }
        if let Some(dc_grid) = network.hvdc.find_dc_grid_by_bus_mut(row[0] as u32) {
            dc_grid
                .converters
                .push(DcConverter::Vsc(DcConverterStation {
                    id: String::new(),
                    dc_bus: row[0] as u32,
                    ac_bus: row[1] as u32,
                    control_type_dc: row[2] as u32,
                    control_type_ac: row[3] as u32,
                    active_power_mw: row[4],
                    reactive_power_mvar: row[5],
                    is_lcc: row[6] as u32 != 0,
                    voltage_setpoint_pu: row[7],
                    transformer_r_pu: row[8],
                    transformer_x_pu: row[9],
                    transformer: row[10] as u32 != 0,
                    tap_ratio: row[11],
                    filter_susceptance_pu: row[12],
                    filter: row[13] as u32 != 0,
                    reactor_r_pu: row[14],
                    reactor_x_pu: row[15],
                    reactor: row[16] as u32 != 0,
                    base_kv_ac: row.get(17).copied().unwrap_or(0.0),
                    voltage_max_pu: row.get(18).copied().unwrap_or(1.1),
                    voltage_min_pu: row.get(19).copied().unwrap_or(0.9),
                    current_max_pu: row.get(20).copied().unwrap_or(0.0),
                    status: row.get(21).map(|&v| v as u32 != 0).unwrap_or(true),
                    loss_constant_mw: row.get(22).copied().unwrap_or(0.0),
                    loss_linear: row.get(23).copied().unwrap_or(0.0),
                    loss_quadratic_rectifier: row.get(24).copied().unwrap_or(0.0),
                    loss_quadratic_inverter: row.get(25).copied().unwrap_or(0.0),
                    droop: row.get(26).copied().unwrap_or(0.0),
                    power_dc_setpoint_mw: row.get(27).copied().unwrap_or(0.0),
                    voltage_dc_setpoint_pu: row.get(28).copied().unwrap_or(1.0),
                    // Column 29 may be dVdcset (34-column format) or Pacmax (33-column).
                    // Detect by checking total columns: >=34 → skip dVdcset at col 29.
                    active_power_ac_max_mw: if row.len() >= 34 {
                        row.get(30).copied().unwrap_or(f64::MAX)
                    } else {
                        row.get(29).copied().unwrap_or(f64::MAX)
                    },
                    active_power_ac_min_mw: if row.len() >= 34 {
                        row.get(31).copied().unwrap_or(f64::MIN)
                    } else {
                        row.get(30).copied().unwrap_or(f64::MIN)
                    },
                    reactive_power_ac_max_mvar: if row.len() >= 34 {
                        row.get(32).copied().unwrap_or(f64::MAX)
                    } else {
                        row.get(31).copied().unwrap_or(f64::MAX)
                    },
                    reactive_power_ac_min_mvar: if row.len() >= 34 {
                        row.get(33).copied().unwrap_or(f64::MIN)
                    } else {
                        row.get(32).copied().unwrap_or(f64::MIN)
                    },
                }));
        }
    }

    // Parse DC branch rows (9 columns: fbusdc, tbusdc, r, l, c, rateA, rateB, rateC, status)
    for (row_idx, row) in dc_branch_rows.iter().enumerate() {
        if row.len() < 6 {
            return Err(MatpowerError::InsufficientColumns {
                section: "branchdc".to_string(),
                row: row_idx,
                expected: 6,
                got: row.len(),
            });
        }
        if let Some(dc_grid) = network.hvdc.find_dc_grid_by_bus_mut(row[0] as u32) {
            dc_grid.branches.push(DcBranch {
                id: format!(
                    "dc_grid_{}_branch_{}",
                    dc_grid.id,
                    dc_grid.branches.len() + 1
                ),
                from_bus: row[0] as u32,
                to_bus: row[1] as u32,
                r_ohm: row[2],
                l_mh: row[3],
                c_uf: row[4],
                rating_a_mva: row[5],
                rating_b_mva: row.get(6).copied().unwrap_or(0.0),
                rating_c_mva: row.get(7).copied().unwrap_or(0.0),
                status: row.get(8).map(|&v| v as u32 != 0).unwrap_or(true),
            });
        }
    }
    Ok(network)
}

/// Extract a bus name from a `mpc.bus_name` cell array line.
/// Input: `\t'Riversde  V2';` → output: `"Riversde  V2"` (trailing spaces trimmed).
/// Returns an empty string if no quoted content is found.
fn parse_bus_name_entry(line: &str) -> String {
    let line = line.trim();
    if let Some(start) = line.find('\'')
        && let Some(end) = line[start + 1..].find('\'')
    {
        return line[start + 1..start + 1 + end].trim_end().to_string();
    }
    String::new()
}

/// Check if a line starts a MATPOWER section like `mpc.bus = [`.
/// Ensures we don't match `mpc.bus_name` when looking for `mpc.bus`.
fn is_section_start(line: &str, section: &str) -> bool {
    let pattern = format!("mpc.{section}");
    if let Some(pos) = line.find(&pattern) {
        let end = pos + pattern.len();
        if end >= line.len() {
            return false;
        }
        // Next character after the section name must not be alphanumeric or _
        let next_byte = line.as_bytes()[end];
        if next_byte.is_ascii_alphanumeric() || next_byte == b'_' {
            return false;
        }
        // Must contain = and either [ or { (pglib-opf-hvdc uses curly braces)
        let rest = &line[end..];
        rest.contains('=') && (rest.contains('[') || rest.contains('{'))
    } else {
        false
    }
}

/// Try to parse data on the same line as a section header (after `[` or `{`).
fn try_parse_inline_data(line: &str, rows: &mut Vec<Vec<f64>>) {
    // Find either `[` or `{` as the opening delimiter.
    let bracket_pos = line.find('[').or_else(|| line.find('{'));
    if let Some(pos) = bracket_pos {
        let rest = &line[pos + 1..];
        let rest = rest
            .trim_end_matches(';')
            .trim_end_matches(']')
            .trim_end_matches('}')
            .trim();
        if !rest.is_empty()
            && let Some(row) = parse_numeric_row(rest)
        {
            rows.push(row);
        }
    }
}

/// M-2: Validate that a parsed numeric field is finite (not NaN or Inf).
/// Called after Bus/Gen/Branch struct construction to catch corrupted data
/// that slipped through expression evaluation or other parse paths.
fn check_finite(
    val: f64,
    section: &'static str,
    row: usize,
    field: &'static str,
) -> Result<(), MatpowerError> {
    if !val.is_finite() {
        return Err(MatpowerError::NonFiniteValue {
            section,
            row,
            field,
            value: val,
        });
    }
    Ok(())
}

/// MP-04: Parse a finite f64 value, rejecting NaN and Inf which propagate silently
/// into the Network struct and corrupt Y-bus construction.
/// Exception: "Inf" is allowed through as f64::INFINITY so that branch rate_a=Inf
/// (MATPOWER's convention for unconstrained) can be converted to 0.0 at the call site.
fn parse_finite_f64(s: &str) -> Option<f64> {
    let val: f64 = s.trim().parse().ok()?;
    // Reject NaN — it has no physical meaning and corrupts matrix arithmetic
    if val.is_nan() {
        return None;
    }
    // Allow Inf (unconstrained branch rating); caller must convert to 0.0 as needed
    Some(val)
}

/// Parse a row of space/tab-separated numeric values.
/// Handles MATLAB expressions like `12/sqrt(3)`, `50/3`, and `1.33E-05`.
fn parse_numeric_row(s: &str) -> Option<Vec<f64>> {
    let s = s.trim();
    if s.is_empty() || s.starts_with(']') {
        return None;
    }

    let values: Result<Vec<f64>, _> = s
        .split_whitespace()
        .filter(|t| !t.is_empty() && *t != ";")
        .map(|t| {
            let t = t.trim_end_matches(';');
            // MP-04: use parse_finite_f64 to reject NaN; Inf is allowed for rate_a=Inf
            parse_finite_f64(t)
                .or_else(|| eval_simple_expr(t))
                .ok_or(())
        })
        .collect();

    match values {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Unit conversions needed for MATPOWER distribution cases.
///
/// Many MATPOWER distribution cases store loads in kW and branch impedances in Ohms,
/// with MATLAB code at the bottom to convert to MW and per-unit. Since we can't execute
/// MATLAB code, we detect these patterns and apply the conversions ourselves.
struct Conversions {
    /// Divide Pd, Qd by 1000 (kW → MW, kVAr → MVAr)
    kw_to_mw: bool,
    /// Convert branch R, X from Ohms to per-unit using Zbase = base_kv² / baseMVA
    ohms_to_pu: bool,
    /// Apply power factor conversion (kVA → MW + MVAr)
    power_factor: Option<f64>,
}

/// Detect distribution case unit conversions from MATLAB code in the file.
fn detect_conversions(content: &str) -> Conversions {
    // MP-02: Only scan non-comment lines for conversion triggers. Without this filter,
    // a comment like `% Old code: mpc.bus(:, [PD, QD]) = ... / 1e3` would incorrectly
    // trigger the kW→MW conversion on all bus loads.
    let code_content: String = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with('%') && !trimmed.is_empty()
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Look for: mpc.bus(:, [PD, QD]) = mpc.bus(:, [PD, QD]) / 1e3
    let kw_to_mw = code_content.contains("/ 1e3") && code_content.contains("PD, QD");

    // Look for: mpc.branch(:, [BR_R BR_X]) = ... / (Vbase^2 / Sbase)
    let ohms_to_pu =
        code_content.contains("Vbase^2 / Sbase") || code_content.contains("Vbase^2/Sbase");

    // Look for power factor conversion: pf = 0.85 (or similar)
    let power_factor = if code_content.contains("* pf") || code_content.contains("*pf") {
        // Extract pf value from line like "pf = 0.85;"
        code_content
            .lines()
            .find(|l| {
                let t = l.trim();
                t.starts_with("pf ") || t.starts_with("pf=")
            })
            .and_then(|l| {
                l.find('=').and_then(|eq| {
                    l[eq + 1..]
                        .trim()
                        .trim_end_matches(';')
                        .trim()
                        .parse::<f64>()
                        .ok()
                })
            })
    } else {
        None
    };

    Conversions {
        kw_to_mw,
        ohms_to_pu,
        power_factor,
    }
}

/// Apply detected unit conversions to the parsed network.
///
/// `bus_pd_qd` carries the raw (bus_number, pd, qd) triples before Load creation.
/// Demand conversions (kW→MW, power factor) are applied to this vector, not
/// to the Bus struct which no longer carries demand fields.
fn apply_conversions(network: &mut Network, conv: &Conversions, bus_pd_qd: &mut [(u32, f64, f64)]) {
    if conv.kw_to_mw {
        for entry in bus_pd_qd.iter_mut() {
            entry.1 /= 1000.0;
            entry.2 /= 1000.0;
        }
    }

    if conv.ohms_to_pu {
        // Build bus_num → base_kv lookup for per-branch impedance base
        let bus_kv: std::collections::HashMap<u32, f64> = network
            .buses
            .iter()
            .map(|b| (b.number, b.base_kv))
            .collect();
        let base_mva = network.base_mva;
        if base_mva > 0.0 {
            for branch in &mut network.branches {
                let base_kv = bus_kv.get(&branch.from_bus).copied().unwrap_or(1.0);
                if base_kv > 0.0 {
                    let z_base = base_kv * base_kv / base_mva;
                    branch.r /= z_base;
                    branch.x /= z_base;
                }
            }
        }
    }

    if let Some(pf) = conv.power_factor {
        // After kW→MW conversion, Pd is in MVA (apparent power).
        // Convert: P_real = Pd * pf, Q = Pd * sin(acos(pf))
        let sin_phi = (1.0 - pf * pf).sqrt();
        for entry in bus_pd_qd.iter_mut() {
            let apparent = entry.1;
            entry.2 = apparent * sin_phi;
            entry.1 = apparent * pf;
        }
    }
}

/// Evaluate a simple arithmetic expression (for MATPOWER values like "50/3", "12/sqrt(3)").
/// Supports: number literals, +, -, *, /, sqrt()
/// Operator precedence: +/- lowest, then */÷; left-to-right associativity.
fn eval_simple_expr(s: &str) -> Option<f64> {
    eval_expr_depth(s, 0)
}

/// MP-03: Depth-limited recursive expression evaluator. Returns None when depth > 100
/// to prevent stack overflow from crafted inputs like 100k nested sqrt() calls.
fn eval_expr_depth(s: &str, depth: usize) -> Option<f64> {
    if depth > 100 {
        return None;
    }
    let s = s.trim();
    // Try direct parse first (fast path)
    if let Ok(v) = s.parse::<f64>() {
        return Some(v);
    }
    // Handle sqrt(x)
    if s.starts_with("sqrt(") && s.ends_with(')') {
        let inner = &s[5..s.len() - 1];
        return eval_expr_depth(inner, depth + 1).map(|v| v.sqrt());
    }
    let bytes = s.as_bytes();
    // Find the rightmost +/- (lowest precedence, skip first char to allow leading sign).
    // Rightmost split gives left-to-right associativity: a - b + c → (a - b) + c.
    for i in (1..bytes.len()).rev() {
        if bytes[i] == b'+' || bytes[i] == b'-' {
            let left = eval_expr_depth(&s[..i], depth + 1)?;
            let right = eval_expr_depth(&s[i + 1..], depth + 1)?;
            return Some(if bytes[i] == b'+' {
                left + right
            } else {
                left - right
            });
        }
    }
    // Find the rightmost */ (higher precedence than +/-).
    for i in (1..bytes.len()).rev() {
        if bytes[i] == b'*' || bytes[i] == b'/' {
            let left = eval_expr_depth(&s[..i], depth + 1)?;
            let right = eval_expr_depth(&s[i + 1..], depth + 1)?;
            return Some(if bytes[i] == b'*' {
                left * right
            } else {
                left / right
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn data_available() -> bool {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::Path::new(&p).exists();
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
            .exists()
    }
    #[allow(dead_code)]
    fn test_data_dir() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::PathBuf::from(p);
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
    }

    #[test]
    fn test_parse_case9() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case9.m");
        let net = parse_file(&path).expect("failed to parse case9");

        assert_eq!(net.name, "case9");
        assert_eq!(net.base_mva, 100.0);
        assert_eq!(net.n_buses(), 9);
        assert_eq!(net.n_branches(), 9);
        assert_eq!(net.generators.len(), 3);

        // Verify slack bus
        let slack = net.buses.iter().find(|b| b.is_slack()).unwrap();
        assert_eq!(slack.number, 1);

        // Verify PV buses
        let pv_buses: Vec<u32> = net
            .buses
            .iter()
            .filter(|b| b.is_pv())
            .map(|b| b.number)
            .collect();
        assert_eq!(pv_buses, vec![2, 3]);

        // Verify loads (via Load objects)
        let bus5_pd: f64 = net
            .loads
            .iter()
            .filter(|l| l.bus == 5)
            .map(|l| l.active_power_demand_mw)
            .sum();
        let bus5_qd: f64 = net
            .loads
            .iter()
            .filter(|l| l.bus == 5)
            .map(|l| l.reactive_power_demand_mvar)
            .sum();
        assert!((bus5_pd - 90.0).abs() < 1e-10);
        assert!((bus5_qd - 30.0).abs() < 1e-10);

        let bus7_pd: f64 = net
            .loads
            .iter()
            .filter(|l| l.bus == 7)
            .map(|l| l.active_power_demand_mw)
            .sum();
        assert!((bus7_pd - 100.0).abs() < 1e-10);

        let bus9_pd: f64 = net
            .loads
            .iter()
            .filter(|l| l.bus == 9)
            .map(|l| l.active_power_demand_mw)
            .sum();
        assert!((bus9_pd - 125.0).abs() < 1e-10);

        // Verify generators
        let gen1 = &net.generators[0];
        assert_eq!(gen1.bus, 1);
        assert!((gen1.p - 72.3).abs() < 1e-10);
        assert!((gen1.voltage_setpoint_pu - 1.04).abs() < 1e-10);
        assert!(gen1.in_service);

        // Verify branches
        let br0 = &net.branches[0];
        assert_eq!(br0.from_bus, 1);
        assert_eq!(br0.to_bus, 4);
        assert!((br0.r - 0.0).abs() < 1e-10);
        assert!((br0.x - 0.0576).abs() < 1e-10);
        assert!((br0.tap - 1.0).abs() < 1e-10); // ratio=0 → tap=1.0
        assert!(br0.in_service);

        // Verify total generation and load
        assert!((net.total_generation_mw() - 320.3).abs() < 1e-10);
        assert!((net.total_load_mw() - 315.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_case14() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case14.m");
        let net = parse_file(&path).expect("failed to parse case14");

        assert_eq!(net.name, "case14");
        assert_eq!(net.base_mva, 100.0);
        assert_eq!(net.n_buses(), 14);
        assert_eq!(net.n_branches(), 20);
        assert_eq!(net.generators.len(), 5);

        // Verify transformer taps (branches with non-zero ratio)
        let br_4_7 = net
            .branches
            .iter()
            .find(|b| b.from_bus == 4 && b.to_bus == 7)
            .unwrap();
        assert!((br_4_7.tap - 0.978).abs() < 1e-10);

        let br_4_9 = net
            .branches
            .iter()
            .find(|b| b.from_bus == 4 && b.to_bus == 9)
            .unwrap();
        assert!((br_4_9.tap - 0.969).abs() < 1e-10);

        // Verify bus with shunt susceptance (bus 9: Bs=19)
        let bus9 = net.buses.iter().find(|b| b.number == 9).unwrap();
        assert!((bus9.shunt_susceptance_mvar - 19.0).abs() < 1e-10);

        // Verify baseKV — case14 HV buses (1-5, 8) at 132 kV, LV buses at 33 kV.
        // These values are set in the .m file (corrected from the original CDF conversion
        // which set baseKV = 0 for all buses).
        let bus1 = net.buses.iter().find(|b| b.number == 1).unwrap();
        assert!(
            (bus1.base_kv - 132.0).abs() < 1e-6,
            "bus 1 should be 132 kV HV"
        );
        let bus5 = net.buses.iter().find(|b| b.number == 5).unwrap();
        assert!(
            (bus5.base_kv - 132.0).abs() < 1e-6,
            "bus 5 should be 132 kV HV"
        );
        let bus6 = net.buses.iter().find(|b| b.number == 6).unwrap();
        assert!(
            (bus6.base_kv - 33.0).abs() < 1e-6,
            "bus 6 should be 33 kV LV"
        );
        let bus8 = net.buses.iter().find(|b| b.number == 8).unwrap();
        assert!(
            (bus8.base_kv - 132.0).abs() < 1e-6,
            "bus 8 should be 132 kV HV"
        );
        let bus9_kv = net.buses.iter().find(|b| b.number == 9).unwrap();
        assert!(
            (bus9_kv.base_kv - 33.0).abs() < 1e-6,
            "bus 9 should be 33 kV LV"
        );
        // All buses must have non-zero baseKV (fault analysis requirement)
        for bus in &net.buses {
            assert!(
                bus.base_kv > 0.0,
                "bus {} has zero baseKV — fault analysis will fail",
                bus.number
            );
        }
    }

    #[test]
    fn test_parse_case30() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case30.m");
        let net = parse_file(&path).expect("failed to parse case30");

        assert_eq!(net.n_buses(), 30);
        assert_eq!(net.generators.len(), 6);
    }

    #[test]
    fn test_parse_case118() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case118.m");
        let net = parse_file(&path).expect("failed to parse case118");

        assert_eq!(net.n_buses(), 118);
        assert_eq!(net.generators.len(), 54);
    }

    #[test]
    fn test_parse_string_minimal() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let case = r#"
function mpc = testcase
mpc.version = '2';
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10  0  0  0  0  0  0  0  0  0  0  0;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
"#;
        let net = parse_str(case).expect("failed to parse minimal case");
        assert_eq!(net.name, "testcase");
        assert_eq!(net.n_buses(), 2);
        assert_eq!(net.generators.len(), 1);
        assert_eq!(net.n_branches(), 1);
        let bus_pd = net.bus_load_p_mw();
        assert!((bus_pd[1] - 50.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_gencost_case9() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case9.m");
        let net = parse_file(&path).expect("failed to parse case9");

        // case9 has 3 generators with polynomial (type 2) cost curves
        assert_eq!(net.generators.len(), 3);
        for g in &net.generators {
            assert!(
                g.cost.is_some(),
                "generator at bus {} should have cost",
                g.bus
            );
        }

        // Gen 1: 0.11*P^2 + 5*P + 150, startup=1500, shutdown=0
        let cost0 = net.generators[0].cost.as_ref().unwrap();
        match cost0 {
            CostCurve::Polynomial {
                startup,
                shutdown,
                coeffs,
            } => {
                assert!((startup - 1500.0).abs() < 1e-10);
                assert!((shutdown - 0.0).abs() < 1e-10);
                assert_eq!(coeffs.len(), 3);
                assert!((coeffs[0] - 0.11).abs() < 1e-10);
                assert!((coeffs[1] - 5.0).abs() < 1e-10);
                assert!((coeffs[2] - 150.0).abs() < 1e-10);
            }
            _ => panic!("expected polynomial cost"),
        }

        // Gen 2: 0.085*P^2 + 1.2*P + 600, startup=2000
        let cost1 = net.generators[1].cost.as_ref().unwrap();
        match cost1 {
            CostCurve::Polynomial {
                startup, coeffs, ..
            } => {
                assert!((startup - 2000.0).abs() < 1e-10);
                assert_eq!(coeffs.len(), 3);
                assert!((coeffs[0] - 0.085).abs() < 1e-10);
                assert!((coeffs[1] - 1.2).abs() < 1e-10);
                assert!((coeffs[2] - 600.0).abs() < 1e-10);
            }
            _ => panic!("expected polynomial cost"),
        }

        // Gen 3: 0.1225*P^2 + 1*P + 335, startup=3000
        let cost2 = net.generators[2].cost.as_ref().unwrap();
        match cost2 {
            CostCurve::Polynomial {
                startup, coeffs, ..
            } => {
                assert!((startup - 3000.0).abs() < 1e-10);
                assert_eq!(coeffs.len(), 3);
                assert!((coeffs[0] - 0.1225).abs() < 1e-10);
                assert!((coeffs[1] - 1.0).abs() < 1e-10);
                assert!((coeffs[2] - 335.0).abs() < 1e-10);
            }
            _ => panic!("expected polynomial cost"),
        }
    }

    #[test]
    fn test_parse_gencost_rejects_invalid_generator_rows() {
        let case = r#"
function mpc = testcase
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    99  10  0   200  -100  1.0  100  1  200  0;
    2   40  0   150  -75   1.0  100  1  150  0;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
mpc.gencost = [
    2   0   0   3   1   2   3;
    2   0   0   3   4   5   6;
];
"#;
        let err = parse_str(case).expect_err("invalid generator row should be rejected");
        assert!(
            matches!(err, MatpowerError::Parse { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn test_parse_gencost_case118() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case118.m");
        let net = parse_file(&path).expect("failed to parse case118");

        // case118 has 54 generators, all should have cost data
        let with_cost = net.generators.iter().filter(|g| g.cost.is_some()).count();
        assert_eq!(with_cost, 54, "all 54 generators should have cost data");
    }

    #[test]
    fn test_parse_no_gencost() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        // Network without gencost section — generators should have cost = None
        let case = r#"
function mpc = testcase
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
"#;
        let net = parse_str(case).expect("failed to parse");
        assert!(net.generators[0].cost.is_none());
    }

    #[test]
    fn test_parse_gencost_piecewise_linear() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let case = r#"
function mpc = testcase
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
mpc.gencost = [
    1  0  0  3  0  0  100  1000  200  3000;
];
"#;
        let net = parse_str(case).expect("failed to parse");
        let cost = net.generators[0].cost.as_ref().unwrap();
        match cost {
            CostCurve::PiecewiseLinear { points, .. } => {
                assert_eq!(points.len(), 3);
                assert!((points[0].0 - 0.0).abs() < 1e-10);
                assert!((points[0].1 - 0.0).abs() < 1e-10);
                assert!((points[1].0 - 100.0).abs() < 1e-10);
                assert!((points[1].1 - 1000.0).abs() < 1e-10);
                assert!((points[2].0 - 200.0).abs() < 1e-10);
                assert!((points[2].1 - 3000.0).abs() < 1e-10);
            }
            _ => panic!("expected piecewise-linear cost"),
        }
    }

    #[test]
    fn test_parse_ramp_rates_rts_gmlc() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case_RTS_GMLC.m");
        let net = parse_file(&path).expect("failed to parse RTS-GMLC");

        // RTS-GMLC has 21-column gen data with nonzero ramp rates in cols 16-18
        // Raw file values: ramp_agc=3 (MW/min), ramp_10=3 (MW/10-min), ramp_30=3 (MW/30-min)
        // After normalization to MW/min:
        //   reg_ramp_up_curve = [(0.0, 3.0)] (from RAMP_AGC, already in MW/min)
        //   ramp_up_curve     = [(0.0, 0.3)] (from RAMP_10: 3/10 = 0.3 MW/min)
        let g0 = &net.generators[0];
        assert_eq!(g0.ramp_agc_mw_per_min(), Some(3.0));
        assert!(
            (g0.ramp_up_mw_per_min().unwrap() - 0.3).abs() < 1e-10,
            "ramp_up={:?}",
            g0.ramp_up_mw_per_min()
        );

        // Gen 2: bus 101, raw file: ramp_agc=2, ramp_10=2, ramp_30=2
        // Normalized: reg_ramp_up_curve=[(0,2.0)], ramp_up_curve=[(0,0.2)]
        let g2 = &net.generators[2];
        assert_eq!(g2.ramp_agc_mw_per_min(), Some(2.0));
        assert!(
            (g2.ramp_up_mw_per_min().unwrap() - 0.2).abs() < 1e-10,
            "ramp_up={:?}",
            g2.ramp_up_mw_per_min()
        );
    }

    #[test]
    fn test_parse_ramp_rates_case9_none() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        // case9 has only 10 gen columns — ramp rates should be None
        let path = test_data_dir().join("case9.m");
        let net = parse_file(&path).expect("failed to parse case9");
        for g in &net.generators {
            assert!(
                g.ramp_up_mw_per_min().is_none(),
                "case9 should have no ramp_up"
            );
            assert!(
                g.ramp_agc_mw_per_min().is_none(),
                "case9 should have no ramp_agc"
            );
        }
    }

    #[test]
    fn test_parse_bus_name_case14() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case14.m");
        let net = parse_file(&path).expect("failed to parse case14");

        assert_eq!(net.n_buses(), 14);
        // mpc.bus_name entries are positional — bus[0] corresponds to bus number 1
        assert_eq!(net.buses[0].name, "Bus 1     HV");
        assert_eq!(net.buses[5].name, "Bus 6     LV");
        assert_eq!(net.buses[13].name, "Bus 14    LV");
    }

    #[test]
    fn test_parse_bus_name_case118() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case118.m");
        let net = parse_file(&path).expect("failed to parse case118");

        assert_eq!(net.n_buses(), 118);
        // First few buses should have real substation names
        assert_eq!(net.buses[0].name, "Riversde  V2");
        assert_eq!(net.buses[1].name, "Pokagon   V2");
        assert_eq!(net.buses[4].name, "Olive     V2");
        // All buses should have non-empty names
        for bus in &net.buses {
            assert!(
                !bus.name.is_empty(),
                "bus {} should have a name",
                bus.number
            );
        }
    }

    #[test]
    fn test_parse_bus_name_inline() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let case = r#"
function mpc = testcase
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
    3   2   30  10  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
    2   3   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
mpc.bus_name = {
    'SUBSTATION A';
    'SUBSTATION B';
    'SUBSTATION C';
};
"#;
        let net = parse_str(case).expect("failed to parse");
        assert_eq!(net.buses[0].name, "SUBSTATION A");
        assert_eq!(net.buses[1].name, "SUBSTATION B");
        assert_eq!(net.buses[2].name, "SUBSTATION C");
    }

    #[test]
    fn test_parse_bus_name_activsg2000() {
        if !data_available() {
            eprintln!("SKIP: SURGE_TEST_DATA not set and tests/data not present");
            return;
        }
        let path = test_data_dir().join("case_ACTIVSg2000.m");
        let net = parse_file(&path).expect("failed to parse ACTIVSg2000");

        assert_eq!(net.n_buses(), 2000);
        // First bus should be ODESSA
        assert_eq!(net.buses[0].name, "ODESSA 2 0");
        assert_eq!(net.buses[1].name, "PRESIDIO 2 0");
        // All buses should have names
        let unnamed = net.buses.iter().filter(|b| b.name.is_empty()).count();
        assert_eq!(
            unnamed, 0,
            "all 2000 buses should have names from mpc.bus_name"
        );
    }

    #[test]
    fn test_bus_angles_converted_to_radians() {
        let case = r#"
function mpc = test
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   45.0   345   1   1.1   0.9;
];
mpc.gen = [
    1   0  0   300  -300  1.0  100  1  250  10  0  0  0  0  0  0  0  0  0  0  0;
];
mpc.branch = [
    1   1   0.01  0.1  0  100  100  100  0  0  1  -360  360;
];
"#;
        let net = parse_str(case).expect("failed to parse");
        let expected_radians = 45.0_f64.to_radians();
        assert!((net.buses[0].voltage_angle_rad - expected_radians).abs() < 1e-10);
    }

    #[test]
    fn test_branch_angmin_angmax_converted_to_radians() {
        let case = r#"
function mpc = test
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -30  30;
];
"#;
        let net = parse_str(case).expect("failed to parse");
        let br = &net.branches[0];
        let expected_min = (-30.0_f64).to_radians();
        let expected_max = 30.0_f64.to_radians();
        assert!(
            (br.angle_diff_min_rad.unwrap() - expected_min).abs() < 1e-12,
            "angmin should be converted to radians: got {}, expected {}",
            br.angle_diff_min_rad.unwrap(),
            expected_min
        );
        assert!(
            (br.angle_diff_max_rad.unwrap() - expected_max).abs() < 1e-12,
            "angmax should be converted to radians: got {}, expected {}",
            br.angle_diff_max_rad.unwrap(),
            expected_max
        );
    }

    #[test]
    fn test_branch_angmin_angmax_default_360_converted() {
        // When MATPOWER specifies -360/360 degrees, verify proper radian conversion
        let case = r#"
function mpc = test
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
"#;
        let net = parse_str(case).expect("failed to parse");
        let br = &net.branches[0];
        let expected_min = (-360.0_f64).to_radians(); // -2*pi
        let expected_max = 360.0_f64.to_radians(); // +2*pi
        assert!(
            (br.angle_diff_min_rad.unwrap() - expected_min).abs() < 1e-12,
            "angmin -360 deg -> -2*pi rad"
        );
        assert!(
            (br.angle_diff_max_rad.unwrap() - expected_max).abs() < 1e-12,
            "angmax +360 deg -> +2*pi rad"
        );
    }

    #[test]
    fn test_branch_angmin_angmax_none_when_missing() {
        // When MATPOWER data has only 11 columns (no angmin/angmax), fields should be None
        let case = r#"
function mpc = test
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1;
];
"#;
        let net = parse_str(case).expect("failed to parse");
        let br = &net.branches[0];
        assert!(
            br.angle_diff_min_rad.is_none(),
            "angmin should be None when column absent"
        );
        assert!(
            br.angle_diff_max_rad.is_none(),
            "angmax should be None when column absent"
        );
    }

    #[test]
    fn test_parse_dc_busdc() {
        let content = r#"
function mpc = case_acdc
mpc.version = '2';
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
mpc.busdc = [
    1   1   0   1.0   345   1.1   0.9;
    2   1   0   1.0   345   1.1   0.9;
];
mpc.convdc = [
    1   1   1   1   0   0   0   1.0   0.01  0.01  1  1.0  0.01  1  0.01  0.01  1  345  1.1  0.9  1.1  1;
    2   2   2   1   0   0   0   1.0   0.01  0.01  1  1.0  0.01  1  0.01  0.01  1  345  1.1  0.9  1.1  1;
];
mpc.branchdc = [
    1   2   0.052   0   0   100   0   0   1;
];
"#;
        let net = parse_str(content).expect("Should parse DC network format");
        let dc_buses: Vec<_> = net.hvdc.dc_buses().collect();
        let dc_converters: Vec<_> = net
            .hvdc
            .dc_converters()
            .filter_map(|c| c.as_vsc())
            .collect();
        let dc_branches: Vec<_> = net.hvdc.dc_branches().collect();
        assert_eq!(dc_buses.len(), 2);
        assert_eq!(dc_converters.len(), 2);
        assert_eq!(dc_branches.len(), 1);
        assert_eq!(net.hvdc.dc_grids.len(), 1);

        // Verify DC bus fields
        assert_eq!(dc_buses[0].bus_id, 1);
        assert!((dc_buses[0].v_dc_pu - 1.0).abs() < 1e-10);

        // Verify DC converter fields
        assert_eq!(dc_converters[0].dc_bus, 1);
        assert_eq!(dc_converters[0].ac_bus, 1);
        assert_eq!(dc_converters[0].control_type_dc, 1);
        assert_eq!(dc_converters[1].control_type_dc, 2);

        // Verify DC branch fields
        assert_eq!(dc_branches[0].from_bus, 1);
        assert_eq!(dc_branches[0].to_bus, 2);
        assert!((dc_branches[0].r_ohm - 0.052).abs() < 1e-10);
    }

    #[test]
    fn test_parse_dc_alternate_keys() {
        // pglib uses mpc.dcbus, mpc.dcconv, mpc.dcbranch
        let content = r#"
function mpc = case_pglib
mpc.version = '2';
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
mpc.dcbus = [
    1   1   0   1.0   345   1.1   0.9;
];
mpc.dcconv = [
    1   1   1   1   0   0   0   1.0   0.01  0.01  1  1.0  0.01  1  0.01  0.01  1  345  1.1  0.9  1.1  1;
];
mpc.dcbranch = [
    1   1   0.1   0   0   100   0   0   1;
];
"#;
        let net = parse_str(content).expect("Should parse pglib alternate keys");
        assert_eq!(net.hvdc.dc_bus_count(), 1);
        assert_eq!(net.hvdc.dc_converter_count(), 1);
        assert_eq!(net.hvdc.dc_branch_count(), 1);
    }

    #[test]
    fn test_parse_dc_converter_loss_params() {
        let content = r#"
function mpc = case_loss
mpc.version = '2';
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
mpc.busdc = [
    1   1   0   1.0   345   1.1   0.9;
];
mpc.convdc = [
    1   1   1   1   50   10   0   1.0   0.01  0.05  1  1.01  0.02  1  0.005  0.08  1  345  1.1  0.9  1.5  1   1.103  0.887  2.885  4.371  0.005  50.0  1.0  100  -100  50  -50;
];
mpc.branchdc = [
    1   1   0.1   0   0   100   0   0   1;
];
"#;
        let net = parse_str(content).expect("Should parse converter loss params");
        let dc_converters: Vec<_> = net
            .hvdc
            .dc_converters()
            .filter_map(|c| c.as_vsc())
            .collect();
        let conv = dc_converters[0];
        assert!((conv.loss_constant_mw - 1.103).abs() < 1e-10);
        assert!((conv.loss_linear - 0.887).abs() < 1e-10);
        assert!((conv.loss_quadratic_rectifier - 2.885).abs() < 1e-10);
        assert!((conv.loss_quadratic_inverter - 4.371).abs() < 1e-10);
        assert!((conv.droop - 0.005).abs() < 1e-10);
        assert!((conv.power_dc_setpoint_mw - 50.0).abs() < 1e-10);
        assert!((conv.active_power_mw - 50.0).abs() < 1e-10);
        assert!((conv.reactive_power_mvar - 10.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_dc_curly_brace_syntax() {
        // pglib-opf-hvdc nem_2000bus uses curly braces `{...}` instead of `[...]`
        let content = r#"
function mpc = case_curly
mpc.version = '2';
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   500   1   1.1   0.9;
    2   1   50  20  0   0   1   1.0   0   500   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
mpc.busdc = {
    1   1   0   1.0   400   1.1   0.9   0;
    2   1   0   1.0   400   1.1   0.9   0;
};
mpc.convdc = {
    1   1   1   1   50  10  0   1   0.00086  0.03  1  1  0.01  0  0.0005  0.015  1  500  1.1  0.9  500.0  1  0.5517  3.031  0.0175  0.0  0.005  -58.6  1.008  0  500  -500  500  -500;
    2   2   2   1  -50 -10  0   1   0.00086  0.03  1  1  0.01  0  0.0005  0.015  1  500  1.1  0.9  500.0  1  0.5517  3.031  0.0175  0.0  0.007   21.9  1.000  0  500  -500  500  -500;
};
mpc.branchdc = {
    1   2   0.001   0   0   510   500   500   1;
};
"#;
        let net = parse_str(content).expect("Should parse curly-brace DC network sections");
        let dc_buses: Vec<_> = net.hvdc.dc_buses().collect();
        let dc_converters: Vec<_> = net
            .hvdc
            .dc_converters()
            .filter_map(|c| c.as_vsc())
            .collect();
        let dc_branches: Vec<_> = net.hvdc.dc_branches().collect();
        assert_eq!(dc_buses.len(), 2);
        assert_eq!(dc_buses[0].bus_id, 1);
        assert!((dc_buses[0].base_kv_dc - 400.0).abs() < 1e-10);
        assert_eq!(dc_converters.len(), 2);
        assert_eq!(dc_converters[0].dc_bus, 1);
        assert_eq!(dc_converters[0].ac_bus, 1);
        assert!((dc_converters[0].loss_constant_mw - 0.5517).abs() < 1e-10);
        assert!((dc_converters[0].loss_linear - 3.031).abs() < 1e-10);
        // 34-column format: dVdcset at col 29, then Pacmax at col 30
        assert!((dc_converters[0].active_power_ac_max_mw - 500.0).abs() < 1e-10);
        assert!((dc_converters[0].active_power_ac_min_mw - (-500.0)).abs() < 1e-10);
        assert_eq!(dc_branches.len(), 1);
        assert!((dc_branches[0].r_ohm - 0.001).abs() < 1e-10);
    }

    #[test]
    fn test_parse_dc_no_dc_sections() {
        // Standard MATPOWER file without DC sections should still work
        let content = r#"
function mpc = case_plain
mpc.version = '2';
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   300  -300  1.0  100  1  250  10;
];
mpc.branch = [
    1   1   0.01  0.1  0.02  100  100  100  0  0  1  -360  360;
];
"#;
        let net = parse_str(content).expect("Standard MATPOWER should still parse");
        assert_eq!(net.hvdc.dc_bus_count(), 0);
        assert_eq!(net.hvdc.dc_converter_count(), 0);
        assert_eq!(net.hvdc.dc_branch_count(), 0);
    }

    /// Parser converts MATPOWER pc1/pc2 two-point capability curve to pq_curve.
    ///
    /// MATPOWER gen columns (0-indexed):
    ///   0:bus 1:pg 2:qg 3:qmax 4:qmin 5:vs 6:mbase 7:status 8:pmax 9:pmin
    ///   10:pc1 11:pc2 12:qc1min 13:qc1max 14:qc2min 15:qc2max
    /// pq_curve stores (p_pu, qmax_pu, qmin_pu) in ascending P order.
    #[test]
    fn test_pq_curve_from_pc_fields() {
        // Test 1: pc1 == pc2 → degenerate → empty pq_curve.
        // Gen row columns: bus pg qg qmax qmin vs mbase status pmax pmin pc1 pc2 qc1min qc1max qc2min qc2max
        //                   1  100  0  200 -100  1.0  100    1  200    0  100  100    -30     80    -20     60
        let case_equal = r#"
function mpc = test_equal_pc
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   80  30  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   100  0   200  -100  1.0  100  1  200  0   100  100  -30  80  -20  60;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  300  300  300  0  0  1;
];
mpc.gencost = [
    2   0   0   3   0   1   0;
];
"#;
        let net = parse_str(case_equal).expect("failed to parse equal-pc case");
        assert!(
            net.generators[0]
                .reactive_capability
                .as_ref()
                .is_none_or(|r| r.pq_curve.is_empty()),
            "equal pc1=pc2=100 → degenerate → empty pq_curve"
        );

        // Test 2: distinct pc1=50, pc2=200 MW → 2-point curve.
        // Col 10=pc1=50, col 11=pc2=200, col 12=qc1min=-50, col 13=qc1max=150,
        // col 14=qc2min=-20, col 15=qc2max=80.
        // pq_curve = [(50/100, 150/100, -50/100), (200/100, 80/100, -20/100)]
        //          = [(0.5, 1.5, -0.5), (2.0, 0.8, -0.2)]
        let case_curve = r#"
function mpc = test_dcurve
mpc.baseMVA = 100;
mpc.bus = [
    1   3   0   0   0   0   1   1.0   0   345   1   1.1   0.9;
    2   1   80  30  0   0   1   1.0   0   345   1   1.1   0.9;
];
mpc.gen = [
    1   150  0   200  -100  1.0  100  1  200  0   50  200  -50  150  -20  80;
];
mpc.branch = [
    1   2   0.01  0.1  0.02  300  300  300  0  0  1;
];
mpc.gencost = [
    2   0   0   3   0   1   0;
];
"#;
        let net2 = parse_str(case_curve).expect("failed to parse D-curve case");
        let g = &net2.generators[0];
        let pq_curve = &g
            .reactive_capability
            .as_ref()
            .expect("should have reactive_capability")
            .pq_curve;
        assert_eq!(
            pq_curve.len(),
            2,
            "two distinct pc breakpoints → 2 pq_curve points"
        );

        let (p1_pu, qmax1_pu, qmin1_pu) = pq_curve[0];
        let (p2_pu, qmax2_pu, qmin2_pu) = pq_curve[1];

        assert!(
            (p1_pu - 0.5).abs() < 1e-10,
            "p1 should be 0.5 pu, got {p1_pu}"
        );
        assert!(
            (qmax1_pu - 1.5).abs() < 1e-10,
            "qmax1 should be 1.5 pu, got {qmax1_pu}"
        );
        assert!(
            (qmin1_pu - (-0.5)).abs() < 1e-10,
            "qmin1 should be -0.5 pu, got {qmin1_pu}"
        );

        assert!(
            (p2_pu - 2.0).abs() < 1e-10,
            "p2 should be 2.0 pu, got {p2_pu}"
        );
        assert!(
            (qmax2_pu - 0.8).abs() < 1e-10,
            "qmax2 should be 0.8 pu, got {qmax2_pu}"
        );
        assert!(
            (qmin2_pu - (-0.2)).abs() < 1e-10,
            "qmin2 should be -0.2 pu, got {qmin2_pu}"
        );
    }
}
