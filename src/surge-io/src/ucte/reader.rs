// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! UCTE-DEF power system format reader.
//!
//! Parses the UCTE Data Exchange Format (UCTE-DEF) used by ENTSO-E for European
//! grid data interchange. The format uses text sections delimited by ## markers.
//!
//! Sections: ##N (nodes/buses), ##L (lines), ##T (transformers), ##R (regulation)

use std::collections::HashMap;
use std::path::Path;

use surge_network::Network;
use surge_network::network::{Branch, BranchType, Bus, BusType, Generator, Load};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum UcteError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error on line {line}: {message}")]
    Parse { line: usize, message: String },
}

fn parse_required_u32(token: Option<&str>, line: usize, field: &str) -> Result<u32, UcteError> {
    let value = token.ok_or_else(|| UcteError::Parse {
        line,
        message: format!("missing required field {field}"),
    })?;
    value.parse::<u32>().map_err(|_| UcteError::Parse {
        line,
        message: format!("invalid {field}: {value}"),
    })
}

fn parse_required_f64(token: Option<&str>, line: usize, field: &str) -> Result<f64, UcteError> {
    let value = token.ok_or_else(|| UcteError::Parse {
        line,
        message: format!("missing required field {field}"),
    })?;
    value.parse::<f64>().map_err(|_| UcteError::Parse {
        line,
        message: format!("invalid {field}: {value}"),
    })
}

fn parse_optional_f64(
    token: Option<&str>,
    line: usize,
    field: &str,
) -> Result<Option<f64>, UcteError> {
    match token {
        Some(value) => value
            .parse::<f64>()
            .map(Some)
            .map_err(|_| UcteError::Parse {
                line,
                message: format!("invalid {field}: {value}"),
            }),
        None => Ok(None),
    }
}

fn parse_required_digit_at(
    raw: &str,
    idx: usize,
    line: usize,
    field: &str,
) -> Result<u32, UcteError> {
    let ch = raw.chars().nth(idx).ok_or_else(|| UcteError::Parse {
        line,
        message: format!("missing required field {field}"),
    })?;
    ch.to_digit(10).ok_or_else(|| UcteError::Parse {
        line,
        message: format!("invalid {field}: {ch}"),
    })
}

/// Parse a UCTE-DEF file from disk.
pub fn parse_file(path: &Path) -> Result<Network, UcteError> {
    let content = std::fs::read_to_string(path)?;
    parse_str(&content)
}

/// Parse a UCTE-DEF string.
pub fn parse_str(content: &str) -> Result<Network, UcteError> {
    let mut network = Network::new("ucte_network");
    // Map from UCTE node name (8 chars) to internal bus number
    let mut node_to_num: HashMap<String, u32> = HashMap::new();
    let mut next_num: u32 = 1;

    #[derive(PartialEq)]
    enum Section {
        Header,
        Node,
        Line,
        Transformer,
        Regulation,
        Other,
    }

    let mut section = Section::Header;

    for (line_idx, raw) in content.lines().enumerate() {
        let line_num = line_idx + 1;
        let trimmed = raw.trim();

        // Section markers
        if let Some(stripped) = trimmed.strip_prefix("##") {
            let tag = stripped.trim_start();
            let tag_upper = tag.to_uppercase();
            // ##Z* tags (e.g. ##ZFR, ##ZDE) are zone sub-group markers within ##N;
            // they do NOT change the current section.
            if tag_upper.starts_with('Z') {
                continue;
            }
            section = if tag_upper.starts_with('N') {
                Section::Node
            } else if tag_upper.starts_with('L') {
                Section::Line
            } else if tag_upper.starts_with('T') {
                Section::Transformer
            } else if tag_upper.starts_with('R') {
                Section::Regulation
            } else {
                Section::Other
            };
            continue;
        }

        // Skip blank lines and comments
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }

        let _ = line_num; // suppress unused warning

        match section {
            Section::Node => {
                // UCTE-DEF node format has two variants:
                //
                // Simple (no geographic name — used in some synthetic test files):
                //   "NODECODE base_kv status node_type Vm Va Pd Qd [Pg Qg ...]"
                //   All fields space-delimited; parts[1] is a parseable float (base_kv).
                //
                // Full UCTE-DEF (real-world files from ENTSO-E, PowSyBl, etc.):
                //   "NODECODE<sp>GEONAME_12CH<sp>status<sp>node_type<sp>Vm Va Pd Qd [...]"
                //   Position 0-7: node code (8 chars)
                //   Position 8:   space
                //   Position 9-20: geographic name (12 chars, may contain spaces)
                //   Position 21:  space
                //   Position 22:  status (single char: 0=connected, 8=equivalent)
                //   Position 23:  space
                //   Position 24:  node type (single char: 0=PQ, 1=PV, 2=slack, 3=isolated)
                //   Position 25:  space
                //   Position 26+: numeric fields (Vm kV, Va deg, Pd, Qd, Pg, Qg, ...)

                if raw.len() < 8 {
                    continue;
                }

                let node_id = raw[..8].trim().to_string();
                if node_id.is_empty() {
                    continue;
                }

                // Detect format by testing whether field at offset 9 looks like
                // the beginning of a float (simple format) or text (full UCTE format).
                let parts: Vec<&str> = raw[8..].split_whitespace().collect();
                if parts.is_empty() {
                    continue;
                }

                let has_geo_name = parts[0].parse::<f64>().is_err();

                let (base_kv, status, node_type_code, vm, va_deg, pd, qd, pg, qg) = if has_geo_name
                {
                    // Full UCTE-DEF fixed-width format
                    let status = parse_required_digit_at(raw, 22, line_num, "status")?;
                    let node_type_code = parse_required_digit_at(raw, 24, line_num, "node_type")?;
                    let numeric: Vec<&str> = if raw.len() > 26 {
                        raw[26..].split_whitespace().collect()
                    } else {
                        vec![]
                    };
                    let vm = parse_optional_f64(numeric.first().copied(), line_num, "vm")?
                        .unwrap_or(0.0);
                    let va_deg = parse_optional_f64(numeric.get(1).copied(), line_num, "va_deg")?
                        .unwrap_or(0.0);
                    let pd =
                        parse_optional_f64(numeric.get(2).copied(), line_num, "pd")?.unwrap_or(0.0);
                    let qd =
                        parse_optional_f64(numeric.get(3).copied(), line_num, "qd")?.unwrap_or(0.0);
                    let pg =
                        parse_optional_f64(numeric.get(4).copied(), line_num, "pg")?.unwrap_or(0.0);
                    let qg =
                        parse_optional_f64(numeric.get(5).copied(), line_num, "qg")?.unwrap_or(0.0);
                    let base_kv = infer_base_kv(&node_id);
                    (base_kv, status, node_type_code, vm, va_deg, pd, qd, pg, qg)
                } else {
                    // Simple space-delimited format (no geographic name)
                    let base_kv = parse_required_f64(parts.first().copied(), line_num, "base_kv")?;
                    let status = parse_required_u32(parts.get(1).copied(), line_num, "status")?;
                    let node_type_code =
                        parse_required_u32(parts.get(2).copied(), line_num, "node_type")?;
                    let vm =
                        parse_optional_f64(parts.get(3).copied(), line_num, "vm")?.unwrap_or(1.0);
                    let va_deg = parse_optional_f64(parts.get(4).copied(), line_num, "va_deg")?
                        .unwrap_or(0.0);
                    let pd =
                        parse_optional_f64(parts.get(5).copied(), line_num, "pd")?.unwrap_or(0.0);
                    let qd =
                        parse_optional_f64(parts.get(6).copied(), line_num, "qd")?.unwrap_or(0.0);
                    let pg =
                        parse_optional_f64(parts.get(7).copied(), line_num, "pg")?.unwrap_or(0.0);
                    let qg =
                        parse_optional_f64(parts.get(8).copied(), line_num, "qg")?.unwrap_or(0.0);
                    (base_kv, status, node_type_code, vm, va_deg, pd, qd, pg, qg)
                };

                // Node type: 0=PQ, 1=PV, 2=Slack, 3=isolated
                let bus_type = match node_type_code {
                    1 => BusType::PV,
                    2 => BusType::Slack,
                    3 => BusType::Isolated,
                    _ => BusType::PQ,
                };

                // Status: 0=in service, others=outage
                if status != 0 {
                    continue; // skip outaged nodes
                }

                let bus_num = next_num;
                next_num += 1;
                node_to_num.insert(node_id, bus_num);

                let mut bus = Bus::new(bus_num, bus_type, base_kv);
                // In UCTE, Vm may be in kV or pu (>5 → kV heuristic)
                let vm_pu = if vm > 5.0 && base_kv > 0.0 {
                    vm / base_kv
                } else if vm > 0.0 {
                    vm
                } else {
                    1.0
                };
                bus.voltage_magnitude_pu = vm_pu;
                bus.voltage_angle_rad = va_deg.to_radians();
                // Synthesize Load object from bus-level Pd/Qd (UCTE has no separate load records).
                if pd.abs() > 1e-10 || qd.abs() > 1e-10 {
                    network.loads.push(Load::new(bus_num, pd, qd));
                }
                network.buses.push(bus);

                // Create a generator if the node has non-zero generation
                if pg.abs() > 1e-10 || qg.abs() > 1e-10 {
                    let mut generator = Generator::new(bus_num, pg, vm_pu);
                    generator.q = qg;
                    network.generators.push(generator);
                }
            }

            Section::Line => {
                // UCTE line: "from_node to_node status r x b rateA [rateB rateC]"
                // or extended: "from_node to_node order_code status r x b currentLimit"
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() < 7 {
                    return Err(UcteError::Parse {
                        line: line_num,
                        message: "truncated line record".to_string(),
                    });
                }

                let from_id = parts[0].to_string();
                let to_id = parts[1].to_string();
                // parts[2] may be order_code or status depending on file version.
                // Extended records carry an explicit integer status token at [3];
                // simple records keep status at [2] even when optional ratings
                // are present.
                let status_idx = if parts.len() >= 8 && parts[3].parse::<u32>().is_ok() {
                    3
                } else {
                    2
                };
                let r_idx = status_idx + 1;
                let x_idx = r_idx + 1;
                let b_idx = x_idx + 1;
                let rate_idx = b_idx + 1;

                if parts.len() <= rate_idx {
                    return Err(UcteError::Parse {
                        line: line_num,
                        message: "truncated line record".to_string(),
                    });
                }

                let status =
                    parse_required_u32(parts.get(status_idx).copied(), line_num, "status")?;
                let r = parse_required_f64(parts.get(r_idx).copied(), line_num, "r")?;
                let x = parse_required_f64(parts.get(x_idx).copied(), line_num, "x")?;
                let b = parse_required_f64(parts.get(b_idx).copied(), line_num, "b")?;
                let rate_a = parse_required_f64(parts.get(rate_idx).copied(), line_num, "rate_a")?;

                let from = node_to_num
                    .get(&from_id)
                    .copied()
                    .ok_or_else(|| UcteError::Parse {
                        line: line_num,
                        message: format!("line references unknown from node {from_id}"),
                    })?;
                let to = node_to_num
                    .get(&to_id)
                    .copied()
                    .ok_or_else(|| UcteError::Parse {
                        line: line_num,
                        message: format!("line references unknown to node {to_id}"),
                    })?;

                // UCTE r, x in Ohm; b in µS — convert to pu
                let base_kv = network
                    .buses
                    .iter()
                    .find(|bus| bus.number == from)
                    .map(|bus| bus.base_kv)
                    .unwrap_or(1.0);
                let base_mva = network.base_mva;
                let z_base = if base_kv > 0.0 && base_mva > 0.0 {
                    base_kv * base_kv / base_mva
                } else {
                    1.0
                };
                let b_base = if z_base > 1e-20 { 1.0 / z_base } else { 1.0 };
                let r_pu = if z_base > 1e-20 { r / z_base } else { r };
                let x_pu = if z_base > 1e-20 { x / z_base } else { x };
                let b_pu = b * 1e-6 / b_base; // µS → S → pu

                let mut br = Branch::new_line(from, to, r_pu, x_pu, b_pu);
                br.rating_a_mva = rate_a;
                br.in_service = status == 0;
                network.branches.push(br);
            }

            Section::Transformer => {
                // UCTE transformer: "from to order_code status r x b ratedU1 ratedU2 rateA"
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() < 9 {
                    return Err(UcteError::Parse {
                        line: line_num,
                        message: "truncated transformer record".to_string(),
                    });
                }

                let from_id = parts[0].to_string();
                let to_id = parts[1].to_string();
                let status = parse_required_u32(parts.get(3).copied(), line_num, "status")?;
                let r = parse_required_f64(parts.get(4).copied(), line_num, "r")?;
                let x = parse_required_f64(parts.get(5).copied(), line_num, "x")?;
                let b = parse_required_f64(parts.get(6).copied(), line_num, "b")?;
                let rated_u1 = parse_required_f64(parts.get(7).copied(), line_num, "rated_u1")?;
                let rated_u2 = parse_required_f64(parts.get(8).copied(), line_num, "rated_u2")?;
                let rate_a =
                    parse_optional_f64(parts.get(9).copied(), line_num, "rate_a")?.unwrap_or(0.0);

                let from = node_to_num
                    .get(&from_id)
                    .copied()
                    .ok_or_else(|| UcteError::Parse {
                        line: line_num,
                        message: format!("transformer references unknown from node {from_id}"),
                    })?;
                let to = node_to_num
                    .get(&to_id)
                    .copied()
                    .ok_or_else(|| UcteError::Parse {
                        line: line_num,
                        message: format!("transformer references unknown to node {to_id}"),
                    })?;

                // Convert r, x from % on transformer base to pu on system base
                let base_mva = network.base_mva;
                let rated_mva = if parts.len() > 10 {
                    let v = parse_required_f64(parts.get(10).copied(), line_num, "rated_mva")?;
                    if v > 0.0 { v } else { base_mva }
                } else {
                    base_mva
                };
                let ratio = if rated_mva > 0.0 {
                    base_mva / rated_mva
                } else {
                    1.0
                };
                let r_pu = r / 100.0 * ratio;
                let x_pu = x / 100.0 * ratio;
                let b_ratio = if base_mva > 0.0 {
                    rated_mva / base_mva
                } else {
                    1.0
                };
                let b_pu = b / 100.0 * b_ratio;
                let tap = if rated_u2 != 0.0 {
                    rated_u1 / rated_u2
                } else {
                    1.0
                };

                let mut br = Branch::new_line(from, to, r_pu, x_pu, b_pu);
                br.tap = tap;
                br.rating_a_mva = rate_a;
                br.in_service = status == 0;
                br.branch_type = BranchType::Transformer;
                network.branches.push(br);
            }

            _ => {}
        }
    }

    // Default base_mva = 100 (UCTE doesn't specify it)
    network.base_mva = 100.0;

    // Designate a slack bus if none exists.
    // UCTE format does not always mark a slack bus.  Choose the bus connected
    // to the largest generator (largest Pg injection) as the reference.
    let has_slack = network.buses.iter().any(|b| b.bus_type == BusType::Slack);
    if !has_slack && !network.buses.is_empty() {
        // Collect total generation per bus from generators (if any)
        let gen_by_bus: HashMap<u32, f64> = {
            let mut m: HashMap<u32, f64> = HashMap::new();
            for g in &network.generators {
                *m.entry(g.bus).or_default() += g.p;
            }
            m
        };

        // Also consider negative load (net injection) as generation proxy
        let slack_bus_num = if !gen_by_bus.is_empty() {
            // Choose the bus with the largest total generation
            gen_by_bus
                .iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(&bus, _)| bus)
        } else {
            // No generators found — pick the first bus connected to the most branches
            // (a heuristic for the most central bus)
            let mut degree: HashMap<u32, usize> = HashMap::new();
            for br in &network.branches {
                *degree.entry(br.from_bus).or_default() += 1;
                *degree.entry(br.to_bus).or_default() += 1;
            }
            degree
                .iter()
                .max_by_key(|&(_, d)| *d)
                .map(|(&bus, _)| bus)
                .or_else(|| network.buses.first().map(|b| b.number))
        };

        if let Some(num) = slack_bus_num
            && let Some(bus) = network.buses.iter_mut().find(|b| b.number == num)
        {
            tracing::warn!(
                "UCTE network has no slack bus; designating bus {} as slack",
                num
            );
            bus.bus_type = BusType::Slack;
        }
    }

    // Ensure all buses have vm > 0 (some UCTE files have vm=0 which means "no data").
    for bus in &mut network.buses {
        if bus.voltage_magnitude_pu <= 0.0 || !bus.voltage_magnitude_pu.is_finite() {
            bus.voltage_magnitude_pu = 1.0;
        }
    }
    Ok(network)
}

/// Infer base voltage from UCTE node name encoding.
/// UCTE node names encode the voltage level as the 6th character (0-indexed: 5):
/// 0=750kV, 1=380kV, 2=220kV, 3=150kV, 4=120kV, 5=110kV, 6=70kV, 7=27kV, 8=330kV, 9=500kV
fn infer_base_kv(node_id: &str) -> f64 {
    let chars: Vec<char> = node_id.chars().collect();
    if chars.len() >= 6 {
        match chars[5] {
            '0' => 750.0,
            '1' => 380.0,
            '2' => 220.0,
            '3' => 150.0,
            '4' => 120.0,
            '5' => 110.0,
            '6' => 70.0,
            '7' => 27.0,
            '8' => 330.0,
            '9' => 500.0,
            _ => 1.0,
        }
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_UCTE: &str = r#"##C 2007.05.01;12:00;CSE2;CSE2;0001;test case
##N
BUS1110A 110.00 0 2 1.050 0.00 0.0 0.0
BUS2110A 110.00 0 0 1.020 -5.0 100.0 30.0
BUS3110A 110.00 0 0 0.980 -10.0 150.0 50.0
##L
BUS1110A BUS2110A 1 0 5.0 20.0 200.0 400.0
BUS2110A BUS3110A 1 0 8.0 30.0 180.0 300.0
##T
"#;

    #[test]
    fn test_ucte_parse_nodes() {
        let net = parse_str(SAMPLE_UCTE).unwrap();
        assert_eq!(net.n_buses(), 3);
    }

    #[test]
    fn test_ucte_parse_lines() {
        let net = parse_str(SAMPLE_UCTE).unwrap();
        assert_eq!(net.n_branches(), 2);
    }

    #[test]
    fn test_ucte_slack_bus() {
        let net = parse_str(SAMPLE_UCTE).unwrap();
        // BUS1110A has node_type=2 → Slack
        let slack = net.buses.iter().find(|b| b.bus_type == BusType::Slack);
        assert!(slack.is_some());
    }

    #[test]
    fn test_ucte_load_values() {
        let net = parse_str(SAMPLE_UCTE).unwrap();
        // BUS2 has pd=100, BUS3 has pd=150
        let total_load: f64 = net.total_load_mw();
        assert!((total_load - 250.0).abs() < 1.0);
    }

    #[test]
    fn test_ucte_base_kv_inference() {
        assert!((infer_base_kv("ATBER5GR") - 110.0).abs() < 1.0);
        assert!((infer_base_kv("ATBER1GR") - 380.0).abs() < 1.0);
        assert!((infer_base_kv("ATBER2GR") - 220.0).abs() < 1.0);
    }

    #[test]
    fn test_ucte_file_parse() {
        let tmp = std::env::temp_dir().join("surge_ucte_test.uct");
        std::fs::write(&tmp, SAMPLE_UCTE).unwrap();
        let net = parse_file(&tmp).unwrap();
        assert_eq!(net.n_buses(), 3);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_ucte_parse_simple_line_with_optional_rating() {
        let doc = r#"##C 2007.05.01;12:00;CSE2;CSE2;0001;test case
##N
BUS1110A 110.00 0 2 1.050 0.00 0.0 0.0
BUS2110A 110.00 0 0 1.020 -5.0 100.0 30.0
##L
BUS1110A BUS2110A 1 5.0 20.0 200.0 400.0 450.0
##T
"#;
        let net = parse_str(doc).unwrap();
        assert_eq!(net.n_branches(), 1);
        let branch = &net.branches[0];
        assert!(
            !branch.in_service,
            "simple-layout line status should remain aligned with the status column"
        );
        assert!((branch.rating_a_mva - 400.0).abs() < 1e-9);
    }

    #[test]
    fn test_ucte_rejects_malformed_line_impedance() {
        let doc = r#"##N
BUS1110A 110.00 0 2 1.050 0.00 0.0 0.0
BUS2110A 110.00 0 0 1.020 -5.0 100.0 30.0
##L
BUS1110A BUS2110A 1 0 BAD 20.0 200.0 400.0
"#;
        let err = parse_str(doc).unwrap_err();
        assert!(matches!(err, UcteError::Parse { message, .. } if message.contains("invalid r")));
    }

    #[test]
    fn test_ucte_rejects_unknown_line_endpoint() {
        let doc = r#"##N
BUS1110A 110.00 0 2 1.050 0.00 0.0 0.0
##L
BUS1110A BUS9999A 1 0 5.0 20.0 200.0 400.0
"#;
        let err = parse_str(doc).unwrap_err();
        assert!(matches!(
            err,
            UcteError::Parse { message, .. } if message.contains("unknown to node BUS9999A")
        ));
    }
}
