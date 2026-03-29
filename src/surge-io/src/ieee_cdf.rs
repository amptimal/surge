// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! IEEE Common Data Format (.cdf) parser.
//!
//! Parses the legacy IEEE CDF format used by the classic test cases
//! (IEEE 14, 30, 57, 118, 300 bus systems).
//!
//! # File Structure
//! - Line 1: Title/header (date, originator, MVA base, year, case ID)
//! - BUS DATA FOLLOWS section (fixed-column bus records)
//! - BRANCH DATA FOLLOWS section (fixed-column branch records)
//! - Terminated by `-999` within sections, `-9` or `END OF DATA` at end
//!
//! # References
//! - "Common Format for Exchange of Solved Load Flow Data", IEEE Trans. PAS, 1973
//! - UW PSTCA: <https://labs.ece.uw.edu/pstca/>

use std::path::Path;

use surge_network::Network;
use surge_network::network::{Branch, BranchType, Bus, BusType, Generator, Load};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error on line {line}: {message}")]
    Parse { line: usize, message: String },

    #[error("missing section: {0}")]
    MissingSection(String),

    #[error(
        "truncated {section} section: expected {expected} items but found {actual} \
             (file may be truncated or incomplete)"
    )]
    IncompleteSection {
        section: String,
        expected: usize,
        actual: usize,
    },
}

/// Load an IEEE CDF file from disk.
pub fn load(path: impl AsRef<Path>) -> Result<Network, Error> {
    let path = path.as_ref();
    let content = std::fs::read_to_string(path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    parse_string_with_name(&content, &name)
}

/// Load an IEEE CDF case from an in-memory string.
pub fn loads(content: &str) -> Result<Network, Error> {
    parse_string_with_name(content, "unknown")
}

#[cfg(test)]
fn parse_file(path: &Path) -> Result<Network, Error> {
    load(path)
}

#[cfg(test)]
fn parse_str(content: &str) -> Result<Network, Error> {
    loads(content)
}

fn parse_string_with_name(content: &str, name: &str) -> Result<Network, Error> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Err(Error::Parse {
            line: 1,
            message: "empty CDF file".into(),
        });
    }

    // Parse header line (line 1)
    let base_mva = parse_header(lines[0]);

    let mut network = Network::new(name);
    network.base_mva = base_mva;

    let mut pos = 1;
    let mut found_bus = false;
    let mut found_branch = false;

    while pos < lines.len() {
        let line = lines[pos];
        let upper = line.to_uppercase();

        if upper.contains("BUS DATA FOLLOWS") {
            let expected_count = extract_item_count(line);
            pos += 1;
            let (buses, generators, bus_loads, next_pos, terminated) = parse_bus_data(&lines, pos)?;
            if !terminated {
                return Err(Error::Parse {
                    line: pos + 1,
                    message: "BUS DATA section not terminated (missing -999 marker — \
                              file may be truncated)"
                        .into(),
                });
            }
            if let Some(expected) = expected_count
                && buses.len() < expected
            {
                return Err(Error::IncompleteSection {
                    section: "BUS DATA".into(),
                    expected,
                    actual: buses.len(),
                });
            }
            network.buses = buses;
            network.generators = generators;
            network.loads = bus_loads;
            pos = next_pos;
            found_bus = true;
        } else if upper.contains("BRANCH DATA FOLLOWS") {
            let expected_count = extract_item_count(line);
            pos += 1;
            let (branches, next_pos, terminated) = parse_branch_data(&lines, pos)?;
            if !terminated {
                return Err(Error::Parse {
                    line: pos + 1,
                    message: "BRANCH DATA section not terminated (missing -999 marker — \
                              file may be truncated)"
                        .into(),
                });
            }
            if let Some(expected) = expected_count
                && branches.len() < expected
            {
                return Err(Error::IncompleteSection {
                    section: "BRANCH DATA".into(),
                    expected,
                    actual: branches.len(),
                });
            }
            network.branches = branches;
            pos = next_pos;
            found_branch = true;
        } else if upper.contains("END OF DATA") {
            break;
        } else {
            pos += 1;
        }
    }

    if !found_bus {
        return Err(Error::MissingSection("BUS DATA".into()));
    }
    if !found_branch {
        return Err(Error::MissingSection("BRANCH DATA".into()));
    }
    Ok(network)
}

/// Parse the CDF header line to extract base MVA.
/// Header format: cols 32-37 contain MVA base.
fn parse_header(line: &str) -> f64 {
    if line.len() >= 37 {
        extract_f64(line, 31, 37).unwrap_or(100.0)
    } else {
        100.0
    }
}

/// Extract the expected item count from a section header like "BUS DATA FOLLOWS  14 ITEMS".
fn extract_item_count(header: &str) -> Option<usize> {
    let upper = header.to_uppercase();
    if let Some(idx) = upper.find("ITEMS") {
        // Look backwards from "ITEMS" for the number
        let before = &header[..idx].trim_end();
        before
            .rsplit(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|s| s.parse::<usize>().ok())
    } else {
        None
    }
}

/// Parse bus data section. Each bus record is fixed-column.
/// Returns (buses, generators, next_line_position, terminated).
/// `terminated` is true if the `-999` marker was found.
#[allow(clippy::type_complexity)]
fn parse_bus_data(
    lines: &[&str],
    start: usize,
) -> Result<(Vec<Bus>, Vec<Generator>, Vec<Load>, usize, bool), Error> {
    let mut buses = Vec::new();
    let mut generators = Vec::new();
    let mut loads = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos];

        // Section terminator
        if line.starts_with("-999") || line.trim_start().starts_with("-999") {
            return Ok((buses, generators, loads, pos + 1, true));
        }

        // Skip blank lines
        if line.trim().is_empty() {
            pos += 1;
            continue;
        }

        // Need at least enough columns for basic bus data
        if line.len() < 75 {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;

        // Bus number: cols 1-4 (1-indexed), right-justified
        let number = extract_i32(line, 0, 5).ok_or_else(|| Error::Parse {
            line: line_num,
            message: "invalid bus number".into(),
        })? as u32;

        // Bus name: cols 6-17 (1-indexed)
        let bus_name = if line.len() >= 17 {
            line[5..17].trim().to_string()
        } else {
            String::new()
        };

        // Area: cols 19-20 (1-indexed), right-justified
        let area = extract_i32(line, 18, 21).ok_or_else(|| Error::Parse {
            line: line_num,
            message: "invalid bus area".into(),
        })? as u32;

        // Type: cols 25-26 (1-indexed)
        let bus_type_code = extract_i32(line, 24, 27).ok_or_else(|| Error::Parse {
            line: line_num,
            message: "invalid bus type".into(),
        })?;
        let bus_type = match bus_type_code {
            2 => BusType::PV,
            3 => BusType::Slack,
            1 | 0 => BusType::PQ,
            _ => BusType::PQ,
        };

        // Parse the numeric data portion (after type field) as whitespace-separated values.
        // Require the expected field count so malformed rows cannot silently shift
        // later values into earlier fields.
        //
        // CDF spec order (from IEEE/UW PSTCA):
        //   Vm, Va, Pd, Qd, Pg, Qg, BaseKV, DesiredV, Qmax, Qmin, Gs, Bs, RemoteBus
        let numeric_part = if line.len() > 27 { &line[27..] } else { "" };
        let nums: Vec<&str> = numeric_part.split_whitespace().collect();
        if nums.len() < 12 {
            return Err(Error::Parse {
                line: line_num,
                message: format!(
                    "expected at least 12 numeric fields in bus record, got {}",
                    nums.len()
                ),
            });
        }

        let vm = parse_required_f64(nums[0], line_num, "Vm")?;
        let va_deg = parse_required_f64(nums[1], line_num, "Va")?;
        let pd = parse_required_f64(nums[2], line_num, "Pd")?;
        let qd = parse_required_f64(nums[3], line_num, "Qd")?;
        let pg = parse_required_f64(nums[4], line_num, "Pg")?;
        let qg = parse_required_f64(nums[5], line_num, "Qg")?;
        let base_kv = parse_required_f64(nums[6], line_num, "BaseKV")?;
        let desired_v = parse_required_f64(nums[7], line_num, "DesiredV")?;
        let qmax = parse_required_f64(nums[8], line_num, "Qmax")?;
        let qmin = parse_required_f64(nums[9], line_num, "Qmin")?;
        let gs = parse_required_f64(nums[10], line_num, "Gs")?;
        let bs = parse_required_f64(nums[11], line_num, "Bs")?;

        buses.push(Bus {
            number,
            name: bus_name,
            bus_type,
            shunt_conductance_mw: gs,
            shunt_susceptance_mvar: bs,
            area,
            voltage_magnitude_pu: vm,
            voltage_angle_rad: va_deg.to_radians(),
            base_kv,
            zone: 1,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            island_id: 0,
            latitude: None,
            longitude: None,
            ..Bus::new(0, BusType::PQ, 0.0)
        });

        // Create Load object for buses with nonzero demand.
        if pd.abs() > 1e-10 || qd.abs() > 1e-10 {
            loads.push(Load::new(number, pd, qd));
        }

        // Create generator for PV and Slack buses, or any bus with nonzero Pg
        if bus_type == BusType::PV || bus_type == BusType::Slack || pg.abs() > 1e-10 {
            let vs = if bus_type == BusType::PV || bus_type == BusType::Slack {
                if desired_v > 0.0 { desired_v } else { vm }
            } else {
                vm
            };

            generators.push(Generator {
                bus: number,
                machine_id: None,
                p: pg,
                q: qg,
                qmax,
                qmin,
                voltage_setpoint_pu: vs,
                reg_bus: None,
                machine_base_mva: 100.0,
                pmax: 9999.0,
                pmin: 0.0,
                in_service: true,
                cost: None,
                forced_outage_rate: None,
                agc_participation_factor: None,
                h_inertia_s: None,
                pfr_eligible: true,
                ..Generator::new(0, 0.0, 1.0)
            });
        }

        pos += 1;
    }

    Ok((buses, generators, loads, pos, false))
}

/// Parse branch data section. Each branch record is fixed-column.
/// Returns (branches, next_line_position, terminated).
fn parse_branch_data(lines: &[&str], start: usize) -> Result<(Vec<Branch>, usize, bool), Error> {
    let mut branches = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos];

        // Section terminator
        if line.starts_with("-999") || line.trim_start().starts_with("-999") {
            return Ok((branches, pos + 1, true));
        }
        if line.starts_with("-9") || line.trim_start().starts_with("-9") {
            return Ok((branches, pos + 1, true));
        }

        // Skip blank lines
        if line.trim().is_empty() {
            pos += 1;
            continue;
        }

        if line.len() < 50 {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;

        // From bus: cols 1-4 (1-indexed), right-justified — use wider range
        let from_bus = extract_i32(line, 0, 5).ok_or_else(|| Error::Parse {
            line: line_num,
            message: "invalid from bus".into(),
        })? as u32;

        // To bus: cols 6-9 (1-indexed) → 0-indexed [5..10]
        let to_bus = extract_i32(line, 5, 10).ok_or_else(|| Error::Parse {
            line: line_num,
            message: "invalid to bus".into(),
        })? as u32;

        // Circuit: col 15
        let circuit = if line.len() >= 16 {
            extract_i32(line, 14, 16).unwrap_or(1).to_string()
        } else {
            "1".to_string()
        };

        // Type: col 17 (0=line, 1-4=transformer types)
        let branch_type_code = if line.len() >= 18 {
            extract_i32(line, 16, 18).unwrap_or(0)
        } else {
            0
        };

        // R: cols 20-29
        let r = extract_f64(line, 19, 29).unwrap_or(0.0);

        // X: cols 30-40
        let x = extract_f64(line, 29, 40).unwrap_or(0.01);

        // B: cols 41-50
        let b = extract_f64(line, 40, 50).unwrap_or(0.0);

        // Rate A: cols 51-55
        let rate_a = if line.len() >= 55 {
            extract_f64(line, 50, 55).unwrap_or(0.0)
        } else {
            0.0
        };

        // Rate B: cols 57-61
        let rate_b = if line.len() >= 61 {
            extract_f64(line, 56, 61).unwrap_or(0.0)
        } else {
            0.0
        };

        // Rate C: cols 63-67
        let rate_c = if line.len() >= 67 {
            extract_f64(line, 62, 67).unwrap_or(0.0)
        } else {
            0.0
        };

        // Tap ratio: cols 69-72 (0 means 1.0 for lines)
        let tap_raw = if line.len() >= 72 {
            extract_f64(line, 68, 72).unwrap_or(0.0)
        } else {
            0.0
        };
        let tap = if tap_raw == 0.0 { 1.0 } else { tap_raw };

        // Phase shift: cols 74-77 (degrees)
        let shift = if line.len() >= 77 {
            extract_f64(line, 73, 77).unwrap_or(0.0)
        } else {
            0.0
        };

        branches.push(Branch {
            from_bus,
            to_bus,
            circuit,
            r,
            x,
            b,
            rating_a_mva: rate_a,
            rating_b_mva: rate_b,
            rating_c_mva: rate_c,
            tap,
            phase_shift_rad: shift.to_radians(),
            in_service: true, // CDF doesn't have a status field
            angle_diff_min_rad: None,
            angle_diff_max_rad: None,
            g_pi: 0.0,
            g_mag: 0.0,
            b_mag: 0.0,
            tab: None,
            // IEEE CDF: type 0 = line, 1-4 = transformer; also infer from tap != 1.0
            branch_type: if branch_type_code > 0 || (tap - 1.0).abs() > 1e-6 || shift.abs() > 1e-6 {
                BranchType::Transformer
            } else {
                BranchType::Line
            },
            ..Branch::default()
        });

        pos += 1;
    }

    Ok((branches, pos, false))
}

// ---------------------------------------------------------------------------
// Fixed-column extraction helpers
// ---------------------------------------------------------------------------

/// Extract an f64 from a fixed-column substring (0-indexed, exclusive end).
fn extract_f64(line: &str, start: usize, end: usize) -> Option<f64> {
    if line.len() < end {
        // Try with what we have
        if line.len() <= start {
            return None;
        }
        return line[start..].trim().parse::<f64>().ok();
    }
    line[start..end].trim().parse::<f64>().ok()
}

/// Extract an i32 from a fixed-column substring.
fn extract_i32(line: &str, start: usize, end: usize) -> Option<i32> {
    if line.len() < end {
        if line.len() <= start {
            return None;
        }
        return line[start..].trim().parse::<i32>().ok();
    }
    line[start..end].trim().parse::<i32>().ok()
}

fn parse_required_f64(token: &str, line: usize, field: &str) -> Result<f64, Error> {
    token.trim().parse::<f64>().map_err(|_| Error::Parse {
        line,
        message: format!("invalid {field} value '{token}'"),
    })
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
    fn test_parse_minimal_cdf() {
        let cdf = r#" 08/19/93 UW ARCHIVE           100.0  1993 W IEEE 14 Bus Test Case
BUS DATA FOLLOWS                             3 ITEMS
    1 Bus 1     HV  1  1  3 1.060    0.0      0.0      0.0      0.0      0.0   0.0     0.0     1.060     0.0     0.0    232.4   -16.9   0.0 0.0
    2 Bus 2     HV  1  1  2 1.045   -4.98    21.7     12.7      0.0      0.0  40.0     0.0     1.045    50.0   -40.0     40.0    42.4   0.0 0.0
    3 Bus 3     HV  1  1  2 1.010  -12.72    94.2     19.0      0.0      0.0   0.0     0.0     1.010    40.0     0.0      0.0    23.4   0.0 0.0
-999
BRANCH DATA FOLLOWS                          2 ITEMS
    1    2  1  1  1  0.01938   0.05917   0.0528     0     0     0  0   0 0.0       0.0 0.0    0.0     0.0   0.0   0.0
    1    5  1  1  1  0.05403   0.22304   0.0492     0     0     0  0   0 0.0       0.0 0.0    0.0     0.0   0.0   0.0
-999
LOSS ZONES FOLLOWS                     1 ITEMS
    1 IEEE 14 Bus
-9
END OF DATA
"#;
        let net = parse_str(cdf).expect("failed to parse minimal CDF");

        assert_eq!(net.base_mva, 100.0);
        assert_eq!(net.n_buses(), 3);
        assert_eq!(net.n_branches(), 2);

        // Bus 1: Slack
        assert_eq!(net.buses[0].number, 1);
        assert_eq!(net.buses[0].bus_type, BusType::Slack);
        assert!((net.buses[0].voltage_magnitude_pu - 1.06).abs() < 1e-3);

        // Bus 2: PV
        assert_eq!(net.buses[1].number, 2);
        assert_eq!(net.buses[1].bus_type, BusType::PV);
        let bus_pd = net.bus_load_p_mw();
        assert!((bus_pd[1] - 21.7).abs() < 1e-1);

        // Check generators were created for PV and Slack buses
        assert!(net.generators.len() >= 2);

        // Branch 1-2
        assert_eq!(net.branches[0].from_bus, 1);
        assert_eq!(net.branches[0].to_bus, 2);
        assert!((net.branches[0].r - 0.01938).abs() < 1e-5);
    }

    #[test]
    fn test_parse_rejects_malformed_bus_row() {
        let cdf = r#" 08/19/93 UW ARCHIVE           100.0  1993 W IEEE 14 Bus Test Case
BUS DATA FOLLOWS                             2 ITEMS
    1 Bus 1     HV  1  1  3 1.060    0.0      0.0      0.0      0.0      0.0   0.0     0.0     1.060     0.0     0.0    232.4   -16.9   0.0 0.0
    2 Bus 2     HV  1  1  2 1.045   -4.98    21.7     12.7      0.0      abc   40.0     0.0     1.045    50.0   -40.0     40.0    42.4   0.0 0.0
-999
BRANCH DATA FOLLOWS                          1 ITEMS
    1    2  1  1  1  0.01938   0.05917   0.0528     0     0     0  0   0 0.0       0.0 0.0    0.0     0.0   0.0   0.0
-999
END OF DATA
"#;
        let err = parse_str(cdf).expect_err("malformed bus row should be rejected");
        assert!(
            matches!(err, Error::Parse { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn test_parse_ieee14_cdf() {
        let path = test_data_dir().join("ieee14cdf.cdf");
        if !path.exists() {
            return;
        }
        let net = parse_file(&path).expect("failed to parse IEEE 14 bus CDF");
        assert_eq!(net.n_buses(), 14);
        assert_eq!(net.n_branches(), 20);
        assert!(net.generators.len() >= 5);
        assert!((net.total_load_mw() - 259.0).abs() < 1.0);
    }

    #[test]
    fn test_parse_ieee30_cdf() {
        let path = test_data_dir().join("ieee30cdf.cdf");
        if !path.exists() {
            return;
        }
        let net = parse_file(&path).expect("failed to parse IEEE 30 bus CDF");
        assert_eq!(net.n_buses(), 30);
        assert_eq!(net.n_branches(), 41);
        assert!(net.generators.len() >= 6);
    }

    #[test]
    fn test_parse_ieee57_cdf() {
        let path = test_data_dir().join("ieee57cdf.cdf");
        if !path.exists() {
            return;
        }
        let net = parse_file(&path).expect("failed to parse IEEE 57 bus CDF");
        assert_eq!(net.n_buses(), 57);
        assert_eq!(net.n_branches(), 80);
        assert!(net.generators.len() >= 7);
    }

    #[test]
    fn test_parse_ieee118_cdf() {
        let path = test_data_dir().join("ieee118cdf.cdf");
        if !path.exists() {
            return;
        }
        let net = parse_file(&path).expect("failed to parse IEEE 118 bus CDF");
        assert_eq!(net.n_buses(), 118);
        assert_eq!(net.n_branches(), 186);
        assert!(net.generators.len() >= 54);
    }

    /// Cross-format validation: CDF vs MATPOWER for IEEE 14 bus
    #[test]
    fn test_cross_format_ieee14_cdf_vs_matpower() {
        let cdf_path = test_data_dir().join("ieee14cdf.cdf");
        let m_path = test_data_dir().join("case14.m");
        if !cdf_path.exists() {
            return;
        }

        let cdf_net = parse_file(&cdf_path).expect("failed to parse CDF");
        let m_net = crate::matpower::load(&m_path).expect("failed to parse MATPOWER");

        // Same topology
        assert_eq!(cdf_net.n_buses(), m_net.n_buses());
        assert_eq!(cdf_net.n_branches(), m_net.n_branches());

        // Same total load
        assert!(
            (cdf_net.total_load_mw() - m_net.total_load_mw()).abs() < 1.0,
            "Load mismatch: CDF={:.1}, MATPOWER={:.1}",
            cdf_net.total_load_mw(),
            m_net.total_load_mw()
        );
    }

    #[test]
    fn test_extract_f64() {
        let line = "   1.060    0.0      21.7";
        assert!((extract_f64(line, 0, 8).unwrap() - 1.06).abs() < 1e-10);
        assert!((extract_f64(line, 8, 14).unwrap()).abs() < 1e-10);
    }

    #[test]
    fn test_extract_i32() {
        let line = "    1 Bus 1     HV  1  1  3";
        assert_eq!(extract_i32(line, 0, 5), Some(1));
    }
}
