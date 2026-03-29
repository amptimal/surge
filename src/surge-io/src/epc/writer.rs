// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GE PSLF EPC (.epc) file writer.
//!
//! Writes the GE PSLF Electric Power Case format used in WECC base-case
//! exchange and TAMU ACTIVSg synthetic test cases.
//!
//! The output is a minimal but round-trip-safe EPC file containing:
//! - Title / Comments / Solution Parameters (preamble)
//! - Bus Data
//! - Branch Data (lines only, tap == 0 or 1)
//! - Transformer Data (branches with tap != 0 and != 1)
//! - Generator Data
//! - Load Data (aggregated per bus from Load objects via bus_load_p_mw / bus_load_q_mvar)
//! - Shunt Data (aggregated per bus from bus.shunt_conductance_mw / bus.shunt_susceptance_mvar)
//! - Area Data

use std::fmt::Write as FmtWrite;
use std::path::Path;

use surge_network::Network;
use surge_network::network::BusType;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum EpcWriteError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("format error: {0}")]
    Fmt(#[from] std::fmt::Error),
}

/// Write a Network to a GE PSLF EPC file on disk.
pub fn write_file(network: &Network, path: &Path) -> Result<(), EpcWriteError> {
    let content = to_string(network)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Serialize a Network to a GE PSLF EPC string.
pub fn to_string(network: &Network) -> Result<String, EpcWriteError> {
    let mut out = String::with_capacity(64 * 1024);

    write_title(&mut out, network)?;
    write_comments(&mut out)?;
    write_solution_parameters(&mut out, network)?;
    write_bus_data(&mut out, network)?;
    write_branch_data(&mut out, network)?;
    write_transformer_data(&mut out, network)?;
    write_generator_data(&mut out, network)?;
    write_load_data(&mut out, network)?;
    write_shunt_data(&mut out, network)?;
    write_area_data(&mut out, network)?;
    writeln!(out, "end")?;

    Ok(out)
}

// ---------------------------------------------------------------------------
// Section writers
// ---------------------------------------------------------------------------

fn write_title(out: &mut String, network: &Network) -> Result<(), EpcWriteError> {
    writeln!(out, "title")?;
    writeln!(
        out,
        "{} — exported by Surge (https://github.com/amptimal/surge)",
        network.name
    )?;
    writeln!(out, "!")?;
    Ok(())
}

fn write_comments(out: &mut String) -> Result<(), EpcWriteError> {
    writeln!(out, "comments")?;
    writeln!(out, "!")?;
    Ok(())
}

fn write_solution_parameters(out: &mut String, network: &Network) -> Result<(), EpcWriteError> {
    writeln!(out, "solution parameters")?;
    writeln!(out, "  {:.1}", network.base_mva)?;
    writeln!(out, "!")?;
    Ok(())
}

fn write_bus_data(out: &mut String, network: &Network) -> Result<(), EpcWriteError> {
    writeln!(out, "bus data [{}]", network.buses.len())?;

    for bus in &network.buses {
        let ty = match bus.bus_type {
            BusType::PQ => 0,
            BusType::PV => 2,
            BusType::Slack => 3,
            BusType::Isolated => 4,
        };
        let st = if bus.bus_type == BusType::Isolated {
            1
        } else {
            0
        };
        let name = format_epc_name(&bus.name);
        let va_deg = bus.voltage_angle_rad.to_degrees();
        let lat = bus.latitude.unwrap_or(0.0);
        let lon = bus.longitude.unwrap_or(0.0);

        // Identity: bus_number "name" base_kv
        // Data: ty vsched volt angle ar zone vmax vmin date_in date_out pid L own st lat lon
        writeln!(
            out,
            "  {} {} {:.4} : {} {:.6} {:.6} {:.6} {} {} {:.6} {:.6} 0 0 0 0 0 {} {:.6} {:.6}",
            bus.number,
            name,
            bus.base_kv,
            ty,
            bus.voltage_magnitude_pu, // vsched = vm
            bus.voltage_magnitude_pu, // volt
            va_deg,
            bus.area,
            bus.zone,
            bus.voltage_max_pu,
            bus.voltage_min_pu,
            st,
            lat,
            lon,
        )?;
    }

    Ok(())
}

fn write_branch_data(out: &mut String, network: &Network) -> Result<(), EpcWriteError> {
    // Lines only: tap == 0 or tap == 1.0 (not transformers)
    let lines: Vec<_> = network.branches.iter().filter(|b| is_line(b)).collect();

    writeln!(out, "branch data [{}]", lines.len())?;

    // Build bus name/kv lookup
    let bus_info: std::collections::HashMap<u32, (&str, f64)> = network
        .buses
        .iter()
        .map(|b| (b.number, (b.name.as_str(), b.base_kv)))
        .collect();

    for br in &lines {
        let (from_name, from_kv) = bus_info.get(&br.from_bus).copied().unwrap_or(("", 0.0));
        let (to_name, to_kv) = bus_info.get(&br.to_bus).copied().unwrap_or(("", 0.0));

        let st = if br.in_service { 1 } else { 0 };
        let ck = &br.circuit;

        // Identity: from_bus "from_name" from_kv  to_bus "to_name" to_kv  "ck" se
        // Data: st resist react charge rate1 rate2 rate3 rate4 aloss lngth
        writeln!(
            out,
            "  {} {} {:.4}  {} {} {:.4}  \"{}\" 1 : {} {:.8E} {:.8E} {:.8E} {:.2} {:.2} {:.2} 0.00 0.0 0.0",
            br.from_bus,
            format_epc_name(from_name),
            from_kv,
            br.to_bus,
            format_epc_name(to_name),
            to_kv,
            ck,
            st,
            br.r,
            br.x,
            br.b,
            br.rating_a_mva,
            br.rating_b_mva,
            br.rating_c_mva,
        )?;
    }

    Ok(())
}

fn write_transformer_data(out: &mut String, network: &Network) -> Result<(), EpcWriteError> {
    // Transformers: tap != 0 and tap != 1.0
    let xfmrs: Vec<_> = network.branches.iter().filter(|b| !is_line(b)).collect();

    writeln!(out, "transformer data [{}]", xfmrs.len())?;

    let bus_info: std::collections::HashMap<u32, (&str, f64)> = network
        .buses
        .iter()
        .map(|b| (b.number, (b.name.as_str(), b.base_kv)))
        .collect();

    for br in &xfmrs {
        let (from_name, from_kv) = bus_info.get(&br.from_bus).copied().unwrap_or(("", 0.0));
        let (to_name, to_kv) = bus_info.get(&br.to_bus).copied().unwrap_or(("", 0.0));

        let st = if br.in_service { 1 } else { 0 };
        let ck = &br.circuit;
        let tbase = network.base_mva;

        // Compute winding kVs from tap ratio:
        //   tap = (kv_primary / from_basekv) / (kv_secondary / to_basekv)
        // For writing, set kv_primary = tap * from_basekv, kv_secondary = to_basekv
        let kv_primary = br.tap * from_kv;
        let kv_secondary = to_kv;

        // Identity: from_bus "from_name" from_kv  to_bus "to_name" to_kv  "ck" "long_id"
        // Data line 1: st ty reg_bus "reg_name" reg_kv zt int_bus "int_name" int_kv
        //              tert_bus "tert_name" tert_kv ar zone tbase ps_r ps_x pt_r pt_x ts_r ts_x /
        // Data line 2 (cont): kv_primary kv_secondary ang1 ang2 ang3 ang4 rate1 rate2 rate3 rate4 /
        // Data line 3 (cont): owner data /
        // Data line 4 (cont): more data
        writeln!(
            out,
            "  {} {} {:.4}  {} {} {:.4}  \"{}\" \"\" : {} 0 0 \"\" 0.000 0 0 \"\" 0.000 /",
            br.from_bus,
            format_epc_name(from_name),
            from_kv,
            br.to_bus,
            format_epc_name(to_name),
            to_kv,
            ck,
            st,
        )?;
        writeln!(
            out,
            "  0 \"\" 0.000 {} {} {:.1} {:.8E} {:.8E} 0.0000E+00 0.0000E+00 0.0000E+00 0.0000E+00 /",
            br.from_bus.min(99999), // area (use from bus area via bus lookup, or placeholder)
            1,                      // zone placeholder
            tbase,
            br.r,
            br.x,
        )?;
        writeln!(
            out,
            "  {:.4} {:.4} 0.0 0.0 0.0 0.0 {:.2} {:.2} {:.2} 0.00 /",
            kv_primary, kv_secondary, br.rating_a_mva, br.rating_b_mva, br.rating_c_mva,
        )?;
        writeln!(out, "  0 1.0 0 1.0 0 1.0 0 1.0")?;
    }

    Ok(())
}

fn write_generator_data(out: &mut String, network: &Network) -> Result<(), EpcWriteError> {
    writeln!(out, "generator data [{}]", network.generators.len())?;

    let bus_info: std::collections::HashMap<u32, (&str, f64)> = network
        .buses
        .iter()
        .map(|b| (b.number, (b.name.as_str(), b.base_kv)))
        .collect();

    for g in &network.generators {
        let (bus_name, bus_kv) = bus_info.get(&g.bus).copied().unwrap_or(("", 0.0));
        let st = if g.in_service { 1 } else { 0 };
        let gen_id = g.machine_id.as_deref().unwrap_or("1");

        // Identity: bus "name" basekv "id" "long_id"
        // Data: st reg_bus "reg_name" reg_kv  prf qrf ar zone
        //       pgen pmax pmin qgen qmax qmin mbase  cmp_r cmp_x gen_r gen_x /
        //       continuation data
        writeln!(
            out,
            "  {} {} {:.4} \"{}\" \"\" : {} 0 \"\" 0.000 /",
            g.bus,
            format_epc_name(bus_name),
            bus_kv,
            gen_id,
            st,
        )?;
        writeln!(
            out,
            "  1.0 1.0 1 1 {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} {:.1} 0.0 0.0 0.0 0.0 /",
            g.p, g.pmax, g.pmin, g.q, g.qmax, g.qmin, g.machine_base_mva,
        )?;
        writeln!(out, "  0 \"\" 0.000 0 \"\" 0.000 0 0 0 0")?;
    }

    Ok(())
}

fn write_load_data(out: &mut String, network: &Network) -> Result<(), EpcWriteError> {
    // Compute per-bus demand from Load objects.
    let bus_demand_p = network.bus_load_p_mw();
    let bus_demand_q = network.bus_load_q_mvar();
    let _bus_map = network.bus_index_map();

    let load_buses: Vec<(usize, &surge_network::network::Bus)> = network
        .buses
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let pd = bus_demand_p.get(*i).copied().unwrap_or(0.0);
            let qd = bus_demand_q.get(*i).copied().unwrap_or(0.0);
            pd.abs() > 1e-10 || qd.abs() > 1e-10
        })
        .collect();

    writeln!(out, "load data [{}]", load_buses.len())?;

    for (bi, bus) in &load_buses {
        let name = format_epc_name(&bus.name);
        let pd = bus_demand_p.get(*bi).copied().unwrap_or(0.0);
        let qd = bus_demand_q.get(*bi).copied().unwrap_or(0.0);
        // Identity: bus "name" basekv "1" "long_id"
        // Data: st mw mvar mw_i mvar_i mw_z mvar_z ar zone
        writeln!(
            out,
            "  {} {} {:.4} \"1\" \"\" : 1 {:.4} {:.4} 0.0 0.0 0.0 0.0 {} {}",
            bus.number, name, bus.base_kv, pd, qd, bus.area, bus.zone,
        )?;
    }

    Ok(())
}

fn write_shunt_data(out: &mut String, network: &Network) -> Result<(), EpcWriteError> {
    // Bus shunts from bus.shunt_conductance_mw / bus.shunt_susceptance_mvar
    let shunts: Vec<_> = network
        .buses
        .iter()
        .filter(|b| b.shunt_conductance_mw.abs() > 1e-10 || b.shunt_susceptance_mvar.abs() > 1e-10)
        .collect();

    writeln!(out, "shunt data [{}]", shunts.len())?;

    for bus in &shunts {
        let name = format_epc_name(&bus.name);
        // Identity: bus "name" basekv "1" ... "ck" se "long_id"
        // Data: st ar zone pu_mw pu_mvar
        writeln!(
            out,
            "  {} {} {:.4} \"1\" \"\" \"1\" 1 \"\" : 1 {} {} {:.6} {:.6}",
            bus.number,
            name,
            bus.base_kv,
            bus.area,
            bus.zone,
            bus.shunt_conductance_mw,
            bus.shunt_susceptance_mvar,
        )?;
    }

    Ok(())
}

fn write_area_data(out: &mut String, network: &Network) -> Result<(), EpcWriteError> {
    if network.area_schedules.is_empty() {
        writeln!(out, "area data [0]")?;
        return Ok(());
    }

    writeln!(out, "area data [{}]", network.area_schedules.len())?;
    for area in &network.area_schedules {
        let name = format_epc_name(&area.name);
        // number "name" : swing desired tol pnet qnet
        writeln!(
            out,
            "  {} {} : {} {:.2} 10.0 0.0 0.0",
            area.number, name, area.slack_bus, area.p_desired_mw,
        )?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format a name string for EPC output (double-quoted).
fn format_epc_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "\"\"".to_string()
    } else {
        format!("\"{}\"", trimmed.replace('"', "'"))
    }
}

/// Determine if a branch is a line (not a transformer).
fn is_line(branch: &surge_network::network::Branch) -> bool {
    branch.tap == 0.0 || (branch.tap - 1.0).abs() < 1e-6
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn mini_network() -> Network {
        let mut net = Network::new("test_epc");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 345.0);
        b1.name = "Bus1".into();
        b1.voltage_magnitude_pu = 1.04;
        b1.voltage_angle_rad = 0.0;
        b1.area = 1;
        b1.zone = 1;
        b1.voltage_max_pu = 1.06;
        b1.voltage_min_pu = 0.94;

        let mut b2 = Bus::new(2, BusType::PV, 345.0);
        b2.name = "Bus2".into();
        b2.voltage_magnitude_pu = 1.025;
        b2.voltage_angle_rad = 0.17;
        b2.area = 1;
        b2.zone = 1;
        b2.voltage_max_pu = 1.06;
        b2.voltage_min_pu = 0.94;

        let mut b3 = Bus::new(3, BusType::PQ, 138.0);
        b3.name = "Bus3".into();
        b3.voltage_magnitude_pu = 1.01;
        b3.voltage_angle_rad = -0.05;
        b3.area = 1;
        b3.zone = 1;
        b3.voltage_max_pu = 1.06;
        b3.voltage_min_pu = 0.94;
        b3.shunt_conductance_mw = 5.0;
        b3.shunt_susceptance_mvar = -10.0;

        net.buses = vec![b1, b2, b3];
        net.loads = vec![Load::new(2, 50.0, 20.0), Load::new(3, 100.0, 40.0)];

        // Line between buses 1 and 2
        let mut line = Branch::new_line(1, 2, 0.01, 0.1, 0.02);
        line.rating_a_mva = 200.0;
        line.circuit = "1".to_string();

        // Transformer between buses 2 and 3
        let mut xfmr = Branch::new_line(2, 3, 0.005, 0.05, 0.0);
        xfmr.tap = 1.0; // nominal tap ratio (same winding ratio on both sides)
        // Make it clearly a transformer
        xfmr.tap = 1.05;
        xfmr.rating_a_mva = 150.0;
        xfmr.circuit = "1".to_string();

        net.branches = vec![line, xfmr];

        let mut g1 = Generator::new(1, 100.0, 1.04);
        g1.machine_id = Some("1".into());
        g1.pmax = 200.0;
        g1.pmin = 10.0;
        g1.qmax = 100.0;
        g1.qmin = -50.0;
        g1.machine_base_mva = 100.0;

        net.generators = vec![g1];
        net
    }

    #[test]
    fn test_round_trip_to_string() {
        let net = mini_network();
        let epc = to_string(&net).unwrap();

        // Verify basic structure
        assert!(epc.contains("title"));
        assert!(epc.contains("bus data [3]"));
        assert!(epc.contains("branch data [1]"));
        assert!(epc.contains("transformer data [1]"));
        assert!(epc.contains("generator data [1]"));
        assert!(epc.contains("load data [2]"));
        assert!(epc.contains("shunt data [1]"));
        assert!(epc.contains("end"));
    }

    #[test]
    fn test_round_trip_parse() {
        let net = mini_network();
        let epc = to_string(&net).unwrap();

        // Parse it back
        let parsed = crate::epc::parse_str(&epc).unwrap();

        // Verify counts match
        assert_eq!(parsed.buses.len(), net.buses.len());
        assert_eq!(parsed.generators.len(), net.generators.len());
        // Branches = lines + transformers
        assert_eq!(parsed.branches.len(), net.branches.len());

        // Verify bus numbers
        let bus_nums: Vec<u32> = parsed.buses.iter().map(|b| b.number).collect();
        assert!(bus_nums.contains(&1));
        assert!(bus_nums.contains(&2));
        assert!(bus_nums.contains(&3));
    }

    #[test]
    fn test_write_file_round_trip() {
        let net = mini_network();
        let tmp = std::env::temp_dir().join("surge_test_epc_writer.epc");
        write_file(&net, &tmp).unwrap();

        let parsed = crate::epc::parse_file(&tmp).unwrap();
        assert_eq!(parsed.buses.len(), 3);
        assert_eq!(parsed.generators.len(), 1);

        // Cleanup
        let _ = std::fs::remove_file(&tmp);
    }
}
