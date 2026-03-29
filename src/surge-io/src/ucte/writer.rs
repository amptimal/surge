// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! UCTE-DEF power system format writer.
//!
//! Writes the UCTE Data Exchange Format (UCTE-DEF) used by ENTSO-E for European
//! grid data interchange. The format uses fixed-width text sections delimited by
//! `##` markers.
//!
//! Sections emitted: `##C` (header), `##N` (nodes/buses), `##L` (lines),
//! `##T` (transformers).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as FmtWrite;
use std::path::Path;

use surge_network::Network;
use surge_network::network::BusType;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum UcteWriteError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("format error: {0}")]
    Fmt(#[from] std::fmt::Error),
}

/// Write a Network to a UCTE-DEF file on disk.
pub fn write_file(network: &Network, path: &Path) -> Result<(), UcteWriteError> {
    let content = to_string(network)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Serialize a Network to a UCTE-DEF string.
pub fn to_string(network: &Network) -> Result<String, UcteWriteError> {
    let mut out = String::with_capacity(64 * 1024);

    // Build a mapping from bus number to a UCTE node code (8 chars).
    // If the bus already has a name that looks like a UCTE node code (8 chars,
    // no whitespace issues), use it directly. Otherwise, synthesize one.
    let bus_node_codes = build_node_codes(network);

    // Precompute per-bus demand from Load objects.
    let bus_demand_p = network.bus_load_p_mw();
    let bus_demand_q = network.bus_load_q_mvar();
    let bus_idx_map = network.bus_index_map();

    // Aggregate generator output per bus for the node record Pg/Qg fields.
    let gen_per_bus = aggregate_generators(network);

    // Track which buses have any generators (even zero-output ones like
    // synchronous condensers) so we can emit Pg/Qg to preserve them.
    let buses_with_gens: BTreeSet<u32> = network
        .generators
        .iter()
        .filter(|g| g.in_service)
        .map(|g| g.bus)
        .collect();

    // --- ##C header ---
    let now = format_date_string();
    writeln!(out, "##C {now}")?;
    writeln!(out, "Exported by Surge (https://github.com/amptimal/surge)")?;

    // --- ##N nodes ---
    writeln!(out, "##N")?;

    // Group buses by country code (first two chars of node code → ##Z prefix).
    // Use BTreeMap so zones are emitted in sorted order.
    let mut zone_groups: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for bus in &network.buses {
        let code = &bus_node_codes[&bus.number];
        let zone_key = extract_country_code(code);
        zone_groups.entry(zone_key).or_default().push(bus.number);
    }

    for (zone, bus_numbers) in &zone_groups {
        writeln!(out, "##Z{zone}")?;
        for &bnum in bus_numbers {
            let bus = network
                .buses
                .iter()
                .find(|b| b.number == bnum)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("bus {} not found in network", bnum),
                    )
                })?;
            let node_code = &bus_node_codes[&bnum];

            // Status: 0 = in service (we only write in-service buses)
            let status = 0u8;

            // Node type: 0=PQ, 1=PV, 2=Slack, 3=Isolated
            let node_type = match bus.bus_type {
                BusType::PQ => 0u8,
                BusType::PV => 1,
                BusType::Slack => 2,
                BusType::Isolated => 3,
            };

            // Vm in kV (UCTE convention: values > 5 are in kV)
            let vm_kv = if bus.base_kv > 0.0 {
                bus.voltage_magnitude_pu * bus.base_kv
            } else {
                bus.voltage_magnitude_pu
            };
            let va_deg = bus.voltage_angle_rad.to_degrees();

            // Load values (MW, MVAr) — computed from Load objects.
            let bi = bus_idx_map.get(&bnum).copied().unwrap_or(0);
            let pd = bus_demand_p.get(bi).copied().unwrap_or(0.0);
            let qd = bus_demand_q.get(bi).copied().unwrap_or(0.0);

            // Generator output at this bus (sum of all generators)
            let (pg, qg) = gen_per_bus.get(&bnum).copied().unwrap_or((0.0, 0.0));

            // Simple UCTE node format (reader's simple path):
            //   NODECODE base_kv status node_type Vm Va Pd Qd [Pg Qg]
            // This ensures base_kv round-trips correctly regardless of node code.
            write!(
                out,
                "{:<8} {:.2} {} {} {:.5} {:.5} {:.5} {:.5}",
                node_code, bus.base_kv, status, node_type, vm_kv, va_deg, pd, qd
            )?;

            // Write Pg/Qg if this bus has any generators (even if Pg=0).
            // The reader uses non-zero Pg/Qg as the signal to create a generator,
            // so for zero-output generators we write a tiny epsilon.
            if buses_with_gens.contains(&bnum) {
                let pg_out = if pg.abs() < 1e-10 { 1e-6 } else { pg };
                write!(out, " {:.6} {:.6}", pg_out, qg)?;
            }

            writeln!(out)?;
        }
    }

    // --- ##L lines ---
    writeln!(out, "##L")?;

    // Track order codes for parallel lines between the same pair of buses.
    let mut line_order: BTreeMap<(String, String), u32> = BTreeMap::new();

    for br in &network.branches {
        if br.is_transformer() {
            continue;
        }

        let from_code = match bus_node_codes.get(&br.from_bus) {
            Some(c) => c.clone(),
            None => continue,
        };
        let to_code = match bus_node_codes.get(&br.to_bus) {
            Some(c) => c.clone(),
            None => continue,
        };

        // Determine order code for parallel lines
        let pair_key = if from_code <= to_code {
            (from_code.clone(), to_code.clone())
        } else {
            (to_code.clone(), from_code.clone())
        };
        let order = line_order.entry(pair_key).or_insert(0);
        *order += 1;
        let order_code = *order;

        // Status: 0 = in service
        let status = if br.in_service { 0 } else { 1 };

        // Convert from pu to physical units.
        // z_base = base_kv^2 / base_mva, b_base = 1/z_base
        let base_kv = network
            .buses
            .iter()
            .find(|b| b.number == br.from_bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0);
        let base_mva = network.base_mva;
        let z_base = if base_mva > 0.0 {
            base_kv * base_kv / base_mva
        } else {
            1.0
        };
        let b_base = if z_base > 0.0 { 1.0 / z_base } else { 1.0 };

        let r_ohm = br.r * z_base;
        let x_ohm = br.x * z_base;
        // Reverse of reader: b_pu = b_uS * 1e-6 / b_base = b_uS * 1e-6 * z_base
        // So: b_uS = b_pu / (1e-6 * z_base) = b_pu * b_base * 1e6
        let b_us = br.b * b_base * 1e6;

        // Current limit: rate_a is in MVA, convert to A if base_kv is known.
        // I = S / (sqrt(3) * V_kv) * 1000 [A]
        // However, the UCTE reader just stores rate_a as-is (could be A or MVA).
        // We store the rate_a field directly since the reader doesn't convert it.
        let current_limit = br.rating_a_mva;

        // Format: from to order_code status R(Ohm) X(Ohm) B(uS) currentLimit [name]
        writeln!(
            out,
            "{} {} {} {} {:.4} {:.4} {:.6} {:>6}",
            from_code,
            to_code,
            order_code,
            status,
            r_ohm,
            x_ohm,
            b_us,
            format_current_limit(current_limit)
        )?;
    }

    // --- ##T transformers ---
    writeln!(out, "##T")?;

    let mut xfmr_order: BTreeMap<(String, String), u32> = BTreeMap::new();

    for br in &network.branches {
        if !br.is_transformer() {
            continue;
        }

        let from_code = match bus_node_codes.get(&br.from_bus) {
            Some(c) => c.clone(),
            None => continue,
        };
        let to_code = match bus_node_codes.get(&br.to_bus) {
            Some(c) => c.clone(),
            None => continue,
        };

        // Order code for parallel transformers
        let pair_key = if from_code <= to_code {
            (from_code.clone(), to_code.clone())
        } else {
            (to_code.clone(), from_code.clone())
        };
        let order = xfmr_order.entry(pair_key).or_insert(0);
        *order += 1;
        let order_code = *order;

        let status = if br.in_service { 0 } else { 1 };

        // Rated voltages: from the bus base_kv values.
        let rated_u1 = network
            .buses
            .iter()
            .find(|b| b.number == br.from_bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0);
        let rated_u2 = network
            .buses
            .iter()
            .find(|b| b.number == br.to_bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0);

        // The reader computes:
        //   tap = rated_u1 / rated_u2
        //   r_pu = r_pct / 100 * base_mva / rated_mva
        //   x_pu = x_pct / 100 * base_mva / rated_mva
        //   b_pu = b_pct / 100 * rated_mva / base_mva
        //
        // So we reverse:
        //   rated_mva = base_mva (we use system base as the transformer rating)
        //   r_pct = r_pu * 100 * rated_mva / base_mva = r_pu * 100
        //   x_pct = x_pu * 100
        //   b_pct = b_pu * 100 * base_mva / rated_mva = b_pu * 100
        //
        // For the actual rated voltages, the reader derives tap = u1/u2.
        // We want tap = rated_u1_actual / rated_u2_actual. The bus base_kv values
        // are the nominal voltages; the off-nominal ratio is in br.tap.
        // So: rated_u1_actual = rated_u1 (from bus kV), rated_u2_actual = rated_u1 / br.tap
        // But this only works if tap = rated_u1/rated_u2 from the reader. Since we
        // may have lost the original rated voltages, we reconstruct:
        //   rated_u1_write = rated_u1 (from bus base_kv)
        //   rated_u2_write = rated_u1 / br.tap
        // This ensures that on re-read: tap = rated_u1_write / rated_u2_write = br.tap
        let rated_u2_write = if br.tap.abs() > 1e-10 {
            rated_u1 / br.tap
        } else {
            rated_u2
        };

        let base_mva = network.base_mva;
        let rated_mva = base_mva; // Use system base as transformer rating

        let r_pct = br.r * 100.0 * rated_mva / base_mva;
        let x_pct = br.x * 100.0 * rated_mva / base_mva;
        let b_pct = br.b * 100.0 * base_mva / rated_mva;
        let _g_pct = 0.0; // UCTE G% (typically zero, unused in current format)

        let current_limit = br.rating_a_mva;

        // Format must match reader expectation:
        //   from to order_code status R% X% B% ratedU1 ratedU2 rateA [ratedMVA]
        // The reader parses: parts[4]=R, parts[5]=X, parts[6]=B,
        //   parts[7]=ratedU1, parts[8]=ratedU2, parts[9]=rateA, parts[10]=ratedMVA
        writeln!(
            out,
            "{} {} {} {} {:.4} {:.3} {:.6} {:.1} {:.1} {:>6} {:.1}",
            from_code,
            to_code,
            order_code,
            status,
            r_pct,
            x_pct,
            b_pct,
            rated_u1,
            rated_u2_write,
            format_current_limit(current_limit),
            rated_mva
        )?;
    }

    // --- ##R regulation (empty — we don't have regulation data in the Network model) ---
    writeln!(out, "##R")?;

    Ok(out)
}

/// Build a mapping from bus number to UCTE 8-character node code.
///
/// If a bus has a name that looks like a valid UCTE node code (exactly 8 chars
/// or is a recognizable UCTE ID), use it. Otherwise, generate a synthetic code
/// using the country prefix "XX" and the voltage level character.
fn build_node_codes(network: &Network) -> BTreeMap<u32, String> {
    let mut codes: BTreeMap<u32, String> = BTreeMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();

    // First pass: try to use existing bus names as node codes
    for bus in &network.buses {
        let name = bus.name.trim();
        if name.len() == 8 && name.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
            let code = name.to_string();
            if !used.contains(&code) {
                used.insert(code.clone());
                codes.insert(bus.number, code);
                continue;
            }
        }
        // Will be handled in second pass
    }

    // Second pass: generate synthetic codes for buses without valid names
    let mut synth_counter: u32 = 1;
    for bus in &network.buses {
        if codes.contains_key(&bus.number) {
            continue;
        }

        // Voltage level character (UCTE convention, position 6 of 8-char code):
        // 0=750kV, 1=380kV, 2=220kV, 3=150kV, 4=120kV, 5=110kV, 6=70kV, 7=27kV, 8=330kV, 9=500kV
        let vlevel = voltage_level_char(bus.base_kv);

        loop {
            // Format: XNNNN_V_ where N is a counter, V is voltage level
            // e.g., X0001_1_ for 380kV bus #1
            let code = format!("X{:04}{vlevel}{:02}", synth_counter, bus.number % 100);
            // Ensure exactly 8 chars
            let code = if code.len() > 8 {
                code[..8].to_string()
            } else if code.len() < 8 {
                format!("{:<8}", code)
            } else {
                code
            };
            synth_counter += 1;
            if !used.contains(&code) {
                used.insert(code.clone());
                codes.insert(bus.number, code);
                break;
            }
        }
    }

    codes
}

/// Map a base_kv value to the UCTE voltage level character (index 6 of node code).
fn voltage_level_char(base_kv: f64) -> char {
    if base_kv >= 700.0 {
        '0' // 750 kV
    } else if base_kv >= 450.0 {
        '9' // 500 kV
    } else if base_kv >= 350.0 {
        '1' // 380 kV
    } else if base_kv >= 300.0 {
        '8' // 330 kV
    } else if base_kv >= 200.0 {
        '2' // 220 kV
    } else if base_kv >= 140.0 {
        '3' // 150 kV
    } else if base_kv >= 115.0 {
        '4' // 120 kV
    } else if base_kv >= 90.0 {
        '5' // 110 kV
    } else if base_kv >= 50.0 {
        '6' // 70 kV
    } else {
        '7' // 27 kV and below
    }
}

/// Extract the country code (first two characters) from a UCTE node code.
/// Returns "XX" for synthetic or unrecognizable codes.
fn extract_country_code(node_code: &str) -> String {
    if node_code.len() >= 2 {
        let first_two: String = node_code.chars().take(2).collect();
        // UCTE country codes are two uppercase letters (e.g., FR, DE, BE, NL)
        // or special prefixes like X (cross-border), 0-9 (legacy)
        first_two.to_uppercase()
    } else {
        "XX".to_string()
    }
}

/// Build a 12-character geographic name for the UCTE node record.
/// Aggregate generator Pg and Qg per bus number.
fn aggregate_generators(network: &Network) -> BTreeMap<u32, (f64, f64)> {
    let mut gen_per_bus: BTreeMap<u32, (f64, f64)> = BTreeMap::new();
    for g in &network.generators {
        if !g.in_service {
            continue;
        }
        let entry = gen_per_bus.entry(g.bus).or_insert((0.0, 0.0));
        entry.0 += g.p;
        entry.1 += g.q;
    }
    gen_per_bus
}

/// Format the current date as "YYYY.MM.DD" for the ##C header.
fn format_date_string() -> String {
    // Use a simple approach: read from system time
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs() as i64;

    // Simple date calculation from Unix timestamp
    let days = secs / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}.{:02}.{:02}", year, month, day)
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(mut days: i64) -> (i64, u32, u32) {
    // Civil calendar from days since epoch (algorithm from Howard Hinnant)
    days += 719468;
    let era = if days >= 0 {
        days / 146097
    } else {
        (days - 146096) / 146097
    };
    let doe = (days - era * 146097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month prime [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Format current limit as a right-aligned integer string.
fn format_current_limit(rate: f64) -> String {
    if rate > 0.0 {
        format!("{}", rate as u64)
    } else {
        "0".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn simple_network() -> Network {
        let mut net = Network::new("ucte_test");
        net.base_mva = 100.0;

        // Slack bus at 110 kV
        let mut slack = Bus::new(1, BusType::Slack, 110.0);
        slack.voltage_magnitude_pu = 1.05;
        slack.voltage_angle_rad = 0.0;
        slack.name = "BUS1110A".to_string();
        net.buses.push(slack);

        // PQ bus at 110 kV with load
        let mut pq = Bus::new(2, BusType::PQ, 110.0);
        pq.voltage_magnitude_pu = 1.02;
        pq.voltage_angle_rad = (-5.0_f64).to_radians();
        pq.name = "BUS2110A".to_string();
        net.buses.push(pq);
        net.loads.push(Load::new(2, 100.0, 30.0));

        // PQ bus at 110 kV with load
        let mut pq2 = Bus::new(3, BusType::PQ, 110.0);
        pq2.voltage_magnitude_pu = 0.98;
        pq2.voltage_angle_rad = (-10.0_f64).to_radians();
        pq2.name = "BUS3110A".to_string();
        net.buses.push(pq2);
        net.loads.push(Load::new(3, 150.0, 50.0));

        // Generator on bus 1
        let mut g = Generator::new(1, 250.0, 1.05);
        g.q = 80.0;
        net.generators.push(g);

        // Lines — use pu values as if they came from the reader
        // z_base = 110^2 / 100 = 121 ohm
        // Line 1-2: r=5 ohm, x=20 ohm, b=200 uS
        let z_base = 110.0 * 110.0 / 100.0; // 121.0
        let b_base = 1.0 / z_base;
        net.branches.push(Branch::new_line(
            1,
            2,
            5.0 / z_base,
            20.0 / z_base,
            200.0 * 1e-6 / b_base,
        ));

        // Line 2-3: r=8 ohm, x=30 ohm, b=180 uS
        let mut br2 = Branch::new_line(2, 3, 8.0 / z_base, 30.0 / z_base, 180.0 * 1e-6 / b_base);
        br2.rating_a_mva = 300.0;
        net.branches.push(br2);

        net
    }

    fn transformer_network() -> Network {
        let mut net = Network::new("ucte_xfmr_test");
        net.base_mva = 100.0;

        let mut bus1 = Bus::new(1, BusType::Slack, 400.0);
        bus1.voltage_magnitude_pu = 1.0;
        bus1.name = "FHVBUS1A".to_string();
        net.buses.push(bus1);

        let mut bus2 = Bus::new(2, BusType::PQ, 225.0);
        bus2.name = "FLVBUS2A".to_string();
        net.buses.push(bus2);
        net.loads.push(Load::new(2, 100.0, 0.0));

        net.generators.push(Generator::new(1, 100.0, 1.0));

        // Transformer: tap = 400/225 = 1.7778
        // r_pu = 0.55/100 = 0.0055, x_pu = 1.68/100 = 0.0168
        let mut br = Branch::new_line(1, 2, 0.0055, 0.0168, 0.001325);
        br.tap = 400.0 / 225.0;
        br.rating_a_mva = 5000.0;
        net.branches.push(br);

        net
    }

    #[test]
    fn test_ucte_write_produces_sections() {
        let net = simple_network();
        let s = to_string(&net).unwrap();
        assert!(s.contains("##C"), "missing ##C header");
        assert!(s.contains("##N"), "missing ##N section");
        assert!(s.contains("##L"), "missing ##L section");
        assert!(s.contains("##T"), "missing ##T section");
        assert!(s.contains("##R"), "missing ##R section");
    }

    #[test]
    fn test_ucte_write_node_codes_preserved() {
        let net = simple_network();
        let s = to_string(&net).unwrap();
        assert!(s.contains("BUS1110A"), "node code BUS1110A not found");
        assert!(s.contains("BUS2110A"), "node code BUS2110A not found");
        assert!(s.contains("BUS3110A"), "node code BUS3110A not found");
    }

    #[test]
    fn test_ucte_write_lines_present() {
        let net = simple_network();
        let s = to_string(&net).unwrap();
        // Both lines should appear in the ##L section
        let l_section = s.split("##L").nth(1).unwrap_or("");
        let l_section = l_section.split("##T").next().unwrap_or(l_section);
        // Should have two line records
        let line_count = l_section.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(line_count, 2, "expected 2 line records, got {line_count}");
    }

    #[test]
    fn test_ucte_write_transformer() {
        let net = transformer_network();
        let s = to_string(&net).unwrap();
        let t_section = s.split("##T").nth(1).unwrap_or("");
        let t_section = t_section.split("##R").next().unwrap_or(t_section);
        let xfmr_count = t_section.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(
            xfmr_count, 1,
            "expected 1 transformer record, got {xfmr_count}"
        );
    }

    #[test]
    fn test_ucte_roundtrip_node_count() {
        use crate::ucte::parse_str;
        let net = simple_network();
        let s = to_string(&net).unwrap();
        let net2 = parse_str(&s).unwrap();
        assert_eq!(
            net2.n_buses(),
            net.n_buses(),
            "bus count mismatch after round-trip"
        );
    }

    #[test]
    fn test_ucte_roundtrip_branch_count() {
        use crate::ucte::parse_str;
        let net = simple_network();
        let s = to_string(&net).unwrap();
        let net2 = parse_str(&s).unwrap();
        assert_eq!(
            net2.n_branches(),
            net.n_branches(),
            "branch count mismatch after round-trip"
        );
    }

    #[test]
    fn test_ucte_roundtrip_load_values() {
        use crate::ucte::parse_str;
        let net = simple_network();
        let s = to_string(&net).unwrap();
        let net2 = parse_str(&s).unwrap();
        let total_pd_orig: f64 = net.total_load_mw();
        let total_pd_rt: f64 = net2.total_load_mw();
        assert!(
            (total_pd_orig - total_pd_rt).abs() < 1.0,
            "load mismatch: {total_pd_orig:.2} vs {total_pd_rt:.2}"
        );
    }

    #[test]
    fn test_ucte_roundtrip_impedance() {
        use crate::ucte::parse_str;
        let net = simple_network();
        let s = to_string(&net).unwrap();
        let net2 = parse_str(&s).unwrap();

        // Compare first branch impedance (should be close after ohm→pu→ohm→pu)
        let br1_orig = &net.branches[0];
        let br1_rt = &net2.branches[0];
        let r_tol = 1e-3;
        let x_tol = 1e-3;
        assert!(
            (br1_orig.r - br1_rt.r).abs() / br1_orig.r.max(1e-10) < r_tol,
            "R mismatch: orig={} rt={}",
            br1_orig.r,
            br1_rt.r
        );
        assert!(
            (br1_orig.x - br1_rt.x).abs() / br1_orig.x.max(1e-10) < x_tol,
            "X mismatch: orig={} rt={}",
            br1_orig.x,
            br1_rt.x
        );
    }

    #[test]
    fn test_ucte_file_write() {
        let net = simple_network();
        let tmp = std::env::temp_dir().join("surge_ucte_writer_test.uct");
        write_file(&net, &tmp).unwrap();
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("##C"), "missing ##C in file output");
        assert!(content.contains("##N"), "missing ##N in file output");
        assert!(content.contains("##L"), "missing ##L in file output");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_voltage_level_char_mapping() {
        assert_eq!(voltage_level_char(750.0), '0');
        assert_eq!(voltage_level_char(500.0), '9');
        assert_eq!(voltage_level_char(380.0), '1');
        assert_eq!(voltage_level_char(330.0), '8');
        assert_eq!(voltage_level_char(220.0), '2');
        assert_eq!(voltage_level_char(150.0), '3');
        assert_eq!(voltage_level_char(120.0), '4');
        assert_eq!(voltage_level_char(110.0), '5');
        assert_eq!(voltage_level_char(70.0), '6');
        assert_eq!(voltage_level_char(27.0), '7');
    }

    #[test]
    fn test_synthetic_node_code_generation() {
        // Bus without a valid 8-char name should get a synthetic code
        let mut net = Network::new("synth_test");
        net.base_mva = 100.0;
        let mut bus = Bus::new(1, BusType::PQ, 110.0);
        bus.name = "short".to_string(); // Not 8 chars
        net.buses.push(bus);

        let codes = build_node_codes(&net);
        let code = &codes[&1];
        assert_eq!(code.len(), 8, "synthetic code must be 8 chars: '{code}'");
    }
}
