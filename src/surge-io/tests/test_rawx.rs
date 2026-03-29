// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Integration tests for the PSS/E RAWX (.rawx) JSON parser.
//!
//! These tests validate the rawx parser produces the correct Network model
//! and that it matches equivalent RAW-parsed networks.

use surge_network::network::BusType;

/// IEEE 9-bus case in RAWX JSON format.
/// This is a hand-crafted RAWX representation of the standard IEEE 9-bus case.
const IEEE9_RAWX: &str = r#"{
    "network": {
        "caseid": {
            "fields": ["ic", "sbase", "rev", "xfrrat", "nxfrat", "basfrq", "title1", "title2"],
            "data": [0, 100.0, 35, 0, 0, 60.0, "IEEE 9 bus", ""]
        },
        "bus": {
            "fields": ["ibus", "name", "baskv", "ide", "area", "zone", "owner", "vm", "va"],
            "data": [
                [1, "Bus 1", 16.5, 3, 1, 1, 1, 1.04, 0.0],
                [2, "Bus 2", 18.0, 2, 1, 1, 1, 1.025, 9.28],
                [3, "Bus 3", 13.8, 2, 1, 1, 1, 1.025, 4.66],
                [4, "Bus 4", 230.0, 1, 1, 1, 1, 1.026, -2.22],
                [5, "Bus 5", 230.0, 1, 1, 1, 1, 0.996, -3.99],
                [6, "Bus 6", 230.0, 1, 1, 1, 1, 1.013, -3.69],
                [7, "Bus 7", 230.0, 1, 1, 1, 1, 1.026, 3.72],
                [8, "Bus 8", 230.0, 1, 1, 1, 1, 1.016, 0.73],
                [9, "Bus 9", 230.0, 1, 1, 1, 1, 1.032, 1.97]
            ]
        },
        "load": {
            "fields": ["ibus", "loadid", "stat", "area", "zone", "pl", "ql"],
            "data": [
                [5, "1", 1, 1, 1, 125.0, 50.0],
                [6, "1", 1, 1, 1, 90.0, 30.0],
                [8, "1", 1, 1, 1, 100.0, 35.0]
            ]
        },
        "generator": {
            "fields": ["ibus", "machid", "pg", "qg", "qt", "qb", "vs", "ireg", "mbase", "zr", "zx", "rt", "xt", "gtap", "stat", "rmpct", "pt", "pb"],
            "data": [
                [1, "1", 71.64, 27.05, 9999.0, -9999.0, 1.04, 0, 100.0, 0.0, 0.04, 0.0, 0.0, 1.0, 1, 100.0, 250.0, 10.0],
                [2, "1", 163.0, 6.65, 9999.0, -9999.0, 1.025, 0, 100.0, 0.0, 0.089, 0.0, 0.0, 1.0, 1, 100.0, 300.0, 10.0],
                [3, "1", 85.0, -10.86, 9999.0, -9999.0, 1.025, 0, 100.0, 0.0, 0.1, 0.0, 0.0, 1.0, 1, 100.0, 270.0, 10.0]
            ]
        },
        "acline": {
            "fields": ["ibus", "jbus", "ckt", "rpu", "xpu", "bpu", "name", "rate1", "rate2", "rate3", "gi", "bi", "gj", "bj", "stat", "met", "len"],
            "data": [
                [4, 5, "1", 0.01, 0.085, 0.176, "Line 4-5", 250.0, 250.0, 250.0, 0.0, 0.0, 0.0, 0.0, 1, 1, 0.0],
                [4, 6, "1", 0.017, 0.092, 0.158, "Line 4-6", 250.0, 250.0, 250.0, 0.0, 0.0, 0.0, 0.0, 1, 1, 0.0],
                [5, 7, "1", 0.032, 0.161, 0.306, "Line 5-7", 250.0, 250.0, 250.0, 0.0, 0.0, 0.0, 0.0, 1, 1, 0.0],
                [6, 9, "1", 0.039, 0.17, 0.358, "Line 6-9", 150.0, 150.0, 150.0, 0.0, 0.0, 0.0, 0.0, 1, 1, 0.0],
                [7, 8, "1", 0.0085, 0.072, 0.149, "Line 7-8", 250.0, 250.0, 250.0, 0.0, 0.0, 0.0, 0.0, 1, 1, 0.0],
                [8, 9, "1", 0.0119, 0.1008, 0.209, "Line 8-9", 150.0, 150.0, 150.0, 0.0, 0.0, 0.0, 0.0, 1, 1, 0.0]
            ]
        },
        "transformer": {
            "fields": ["ibus", "jbus", "kbus", "ckt", "cw", "cz", "cm", "mag1", "mag2", "nmet", "name", "stat", "o1", "f1", "o2", "f2", "o3", "f3", "o4", "f4", "vecgrp", "zcod", "r1-2", "x1-2", "sbase1-2", "windv1", "nomv1", "ang1", "wdg1rate1", "wdg1rate2", "wdg1rate3", "cod1", "cont1", "node1", "rma1", "rmi1", "vma1", "vmi1", "ntp1", "tab1", "cr1", "cx1", "cnxa1", "windv2", "nomv2", "ang2"],
            "data": [
                [1, 4, 0, "1", 1, 1, 1, 0.0, 0.0, 2, "T1-4", 1, 1, 1.0, 0, 1.0, 0, 1.0, 0, 1.0, "", 0, 0.0, 0.0576, 100.0, 1.0, 0.0, 0.0, 250.0, 250.0, 250.0, 0, 0, 0, 1.1, 0.9, 1.1, 0.9, 33, 0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                [2, 7, 0, "1", 1, 1, 1, 0.0, 0.0, 2, "T2-7", 1, 1, 1.0, 0, 1.0, 0, 1.0, 0, 1.0, "", 0, 0.0, 0.0625, 100.0, 1.0, 0.0, 0.0, 250.0, 250.0, 250.0, 0, 0, 0, 1.1, 0.9, 1.1, 0.9, 33, 0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                [3, 9, 0, "1", 1, 1, 1, 0.0, 0.0, 2, "T3-9", 1, 1, 1.0, 0, 1.0, 0, 1.0, 0, 1.0, "", 0, 0.0, 0.0586, 100.0, 1.0, 0.0, 0.0, 250.0, 250.0, 250.0, 0, 0, 0, 1.1, 0.9, 1.1, 0.9, 33, 0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]
            ]
        },
        "area": {
            "fields": ["iarea", "isw", "pdes", "ptol", "arnam"],
            "data": [
                [1, 1, 0.0, 10.0, "Area 1"]
            ]
        }
    }
}"#;

#[test]
fn test_rawx_parse_ieee9() {
    let net = surge_io::psse::rawx::loads(IEEE9_RAWX).unwrap();

    assert_eq!(net.n_buses(), 9);
    assert_eq!(net.branches.len(), 9); // 6 lines + 3 transformers
    assert_eq!(net.generators.len(), 3);
    assert_eq!(net.base_mva, 100.0);
    assert_eq!(net.freq_hz, 60.0);

    // Verify bus types
    let slack = net.buses.iter().find(|b| b.number == 1).unwrap();
    assert_eq!(slack.bus_type, BusType::Slack);
    let pv = net.buses.iter().find(|b| b.number == 2).unwrap();
    assert_eq!(pv.bus_type, BusType::PV);
    let pq = net.buses.iter().find(|b| b.number == 5).unwrap();
    assert_eq!(pq.bus_type, BusType::PQ);

    // Verify load accumulation (via Load objects)
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    let pq_idx = net.buses.iter().position(|b| b.number == 5).unwrap();
    assert!(
        (bus_pd[pq_idx] - 125.0).abs() < 1e-10,
        "Bus 5 Pd should be 125 MW"
    );
    assert!(
        (bus_qd[pq_idx] - 50.0).abs() < 1e-10,
        "Bus 5 Qd should be 50 MVAr"
    );

    // Total load should be 315 MW
    let total_load: f64 = net.total_load_mw();
    assert!(
        (total_load - 315.0).abs() < 1e-6,
        "Total load should be 315 MW, got {total_load}"
    );

    // Verify generator setpoints
    let gen1 = net.generators.iter().find(|g| g.bus == 1).unwrap();
    assert!((gen1.voltage_setpoint_pu - 1.04).abs() < 1e-10);
    assert!((gen1.p - 71.64).abs() < 1e-10);
    assert!((gen1.pmax - 250.0).abs() < 1e-10);

    // Verify area interchange
    assert_eq!(net.area_schedules.len(), 1);
    assert_eq!(net.area_schedules[0].number, 1);
    assert_eq!(net.area_schedules[0].slack_bus, 1);
}

#[test]
fn test_rawx_nr_converges_ieee9() {
    let net = surge_io::psse::rawx::loads(IEEE9_RAWX).unwrap();

    let result = surge_ac::solve_ac_pf(&net, &surge_ac::AcPfOptions::default()).unwrap();
    assert_eq!(
        result.status,
        surge_solution::SolveStatus::Converged,
        "NR should converge on IEEE 9-bus RAWX network"
    );
    assert!(
        result.iterations < 10,
        "NR should converge in < 10 iterations, took {}",
        result.iterations
    );

    // Slack bus should have Vm close to setpoint
    let slack_idx = net.buses.iter().position(|b| b.number == 1).unwrap();
    assert!(
        (result.voltage_magnitude_pu[slack_idx] - 1.04).abs() < 1e-4,
        "Slack bus Vm should be ~1.04, got {}",
        result.voltage_magnitude_pu[slack_idx]
    );

    // PV bus 2 should have Vm close to setpoint
    let pv2_idx = net.buses.iter().position(|b| b.number == 2).unwrap();
    assert!(
        (result.voltage_magnitude_pu[pv2_idx] - 1.025).abs() < 1e-4,
        "PV bus 2 Vm should be ~1.025, got {}",
        result.voltage_magnitude_pu[pv2_idx]
    );
}

#[test]
fn test_rawx_vs_raw_ieee14() {
    // Parse the RAW file
    let raw_path = std::path::Path::new("tests/data/IEEE_14_bus.raw");
    if !raw_path.exists() {
        eprintln!("Skipping: tests/data/IEEE_14_bus.raw not found");
        return;
    }
    let raw_net = surge_io::psse::raw::load(raw_path).unwrap();

    // Construct a RAWX JSON string from the same case data.
    // Build RAWX bus data
    let mut bus_data = Vec::new();
    for b in &raw_net.buses {
        let ide = match b.bus_type {
            BusType::Slack => 3,
            BusType::PV => 2,
            BusType::Isolated => 4,
            _ => 1,
        };
        bus_data.push(format!(
            "[{}, \"{}\", {}, {}, {}, {}, 1, {}, {}]",
            b.number,
            b.name.replace('"', "'"),
            b.base_kv,
            ide,
            b.area,
            b.zone,
            b.voltage_magnitude_pu,
            b.voltage_angle_rad.to_degrees()
        ));
    }

    // Build RAWX load data (from Load objects)
    let bus_lookup: std::collections::HashMap<u32, &surge_network::network::Bus> =
        raw_net.buses.iter().map(|b| (b.number, b)).collect();
    let mut load_data = Vec::new();
    for l in &raw_net.loads {
        if l.active_power_demand_mw.abs() > 1e-10 || l.reactive_power_demand_mvar.abs() > 1e-10 {
            let (area, zone) = bus_lookup
                .get(&l.bus)
                .map(|b| (b.area, b.zone))
                .unwrap_or((1, 1));
            load_data.push(format!(
                "[{}, \"1\", 1, {}, {}, {}, {}]",
                l.bus, area, zone, l.active_power_demand_mw, l.reactive_power_demand_mvar
            ));
        }
    }

    // Build RAWX generator data
    let mut gen_data = Vec::new();
    for g in &raw_net.generators {
        let stat = if g.in_service { 1 } else { 0 };
        gen_data.push(format!(
            "[{}, \"{}\", {}, {}, {}, {}, {}, 0, {}, 0.0, {}, 0.0, 0.0, 1.0, {}, 100.0, {}, {}]",
            g.bus,
            g.machine_id.as_deref().unwrap_or("1"),
            g.p,
            g.q,
            g.qmax,
            g.qmin,
            g.voltage_setpoint_pu,
            g.machine_base_mva,
            g.fault_data.as_ref().and_then(|f| f.xs).unwrap_or(0.0),
            stat,
            g.pmax,
            g.pmin,
        ));
    }

    // Build RAWX branch data
    let mut branch_data = Vec::new();
    let mut xfmr_data = Vec::new();
    for br in &raw_net.branches {
        if (br.tap - 0.0).abs() < 1e-10
            || (br.tap - 1.0).abs() < 1e-10 && br.phase_shift_rad.abs() < 1e-10
        {
            // This looks like a line (tap=0 or tap=1 with no shift)
            if br.tap.abs() > 1e-10 && (br.tap - 1.0).abs() > 1e-6 {
                // Actually a transformer
                xfmr_data.push(format!(
                    "[{}, {}, 0, \"{}\", 1, 1, 1, {}, {}, 2, \"xfmr\", {}, 1, 1.0, 0, 1.0, 0, 1.0, 0, 1.0, \"\", 0, {}, {}, 100.0, {}, 0.0, {}, {}, {}, {}, 0, 0, 0, 1.1, 0.9, 1.1, 0.9, 33, 0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]",
                    br.from_bus, br.to_bus, br.circuit, br.g_mag, br.b_mag,
                    if br.in_service { 1 } else { 0 },
                    br.r, br.x, br.tap, br.phase_shift_rad, br.rating_a_mva, br.rating_b_mva, br.rating_c_mva,
                ));
            } else {
                branch_data.push(format!(
                    "[{}, {}, \"{}\", {}, {}, {}, \"line\", {}, {}, {}, 0.0, 0.0, 0.0, 0.0, {}, 1, 0.0]",
                    br.from_bus, br.to_bus, br.circuit, br.r, br.x, br.b, br.rating_a_mva, br.rating_b_mva, br.rating_c_mva,
                    if br.in_service { 1 } else { 0 },
                ));
            }
        } else {
            // Transformer
            xfmr_data.push(format!(
                "[{}, {}, 0, \"{}\", 1, 1, 1, {}, {}, 2, \"xfmr\", {}, 1, 1.0, 0, 1.0, 0, 1.0, 0, 1.0, \"\", 0, {}, {}, 100.0, {}, 0.0, {}, {}, {}, {}, 0, 0, 0, 1.1, 0.9, 1.1, 0.9, 33, 0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]",
                br.from_bus, br.to_bus, br.circuit, br.g_mag, br.b_mag,
                if br.in_service { 1 } else { 0 },
                br.r, br.x, br.tap, br.phase_shift_rad, br.rating_a_mva, br.rating_b_mva, br.rating_c_mva,
            ));
        }
    }

    // Build RAWX fixshunt data
    let mut shunt_data = Vec::new();
    for b in &raw_net.buses {
        if b.shunt_conductance_mw.abs() > 1e-10 || b.shunt_susceptance_mvar.abs() > 1e-10 {
            shunt_data.push(format!(
                "[{}, \"1\", 1, {}, {}]",
                b.number, b.shunt_conductance_mw, b.shunt_susceptance_mvar
            ));
        }
    }

    // Assemble RAWX JSON
    let mut rawx_json = format!(
        r#"{{"network": {{
        "caseid": {{
            "fields": ["ic", "sbase", "rev", "xfrrat", "nxfrat", "basfrq", "title1", "title2"],
            "data": [0, {}, 35, 0, 0, {}, "IEEE 14 bus", ""]
        }},
        "bus": {{
            "fields": ["ibus", "name", "baskv", "ide", "area", "zone", "owner", "vm", "va"],
            "data": [{}]
        }},
        "load": {{
            "fields": ["ibus", "loadid", "stat", "area", "zone", "pl", "ql"],
            "data": [{}]
        }},
        "generator": {{
            "fields": ["ibus", "machid", "pg", "qg", "qt", "qb", "vs", "ireg", "mbase", "zr", "zx", "rt", "xt", "gtap", "stat", "rmpct", "pt", "pb"],
            "data": [{}]
        }},
        "acline": {{
            "fields": ["ibus", "jbus", "ckt", "rpu", "xpu", "bpu", "name", "rate1", "rate2", "rate3", "gi", "bi", "gj", "bj", "stat", "met", "len"],
            "data": [{}]
        }}"#,
        raw_net.base_mva,
        raw_net.freq_hz,
        bus_data.join(",\n            "),
        load_data.join(",\n            "),
        gen_data.join(",\n            "),
        branch_data.join(",\n            ")
    );

    if !xfmr_data.is_empty() {
        rawx_json.push_str(&format!(
            r#",
        "transformer": {{
            "fields": ["ibus", "jbus", "kbus", "ckt", "cw", "cz", "cm", "mag1", "mag2", "nmet", "name", "stat", "o1", "f1", "o2", "f2", "o3", "f3", "o4", "f4", "vecgrp", "zcod", "r1-2", "x1-2", "sbase1-2", "windv1", "nomv1", "ang1", "wdg1rate1", "wdg1rate2", "wdg1rate3", "cod1", "cont1", "node1", "rma1", "rmi1", "vma1", "vmi1", "ntp1", "tab1", "cr1", "cx1", "cnxa1", "windv2", "nomv2", "ang2"],
            "data": [{}]
        }}"#,
            xfmr_data.join(",\n            ")
        ));
    }

    if !shunt_data.is_empty() {
        rawx_json.push_str(&format!(
            r#",
        "fixshunt": {{
            "fields": ["ibus", "shntid", "stat", "gl", "bl"],
            "data": [{}]
        }}"#,
            shunt_data.join(",\n            ")
        ));
    }

    rawx_json.push_str("\n    }}\n}");

    // Parse the constructed RAWX
    let rawx_net = surge_io::psse::rawx::loads(&rawx_json).unwrap();

    // Compare topology
    assert_eq!(
        raw_net.n_buses(),
        rawx_net.n_buses(),
        "Bus count mismatch: RAW={} vs RAWX={}",
        raw_net.n_buses(),
        rawx_net.n_buses()
    );
    assert_eq!(
        raw_net.generators.len(),
        rawx_net.generators.len(),
        "Generator count mismatch"
    );

    // Compare total load
    let raw_load: f64 = raw_net.total_load_mw();
    let rawx_load: f64 = rawx_net.total_load_mw();
    assert!(
        (raw_load - rawx_load).abs() < 0.1,
        "Total load mismatch: RAW={raw_load:.2} vs RAWX={rawx_load:.2}"
    );

    // Run NR on both and compare
    let raw_result = surge_ac::solve_ac_pf(&raw_net, &surge_ac::AcPfOptions::default()).unwrap();
    let rawx_result = surge_ac::solve_ac_pf(&rawx_net, &surge_ac::AcPfOptions::default()).unwrap();

    assert_eq!(
        raw_result.status,
        surge_solution::SolveStatus::Converged,
        "RAW NR did not converge"
    );
    assert_eq!(
        rawx_result.status,
        surge_solution::SolveStatus::Converged,
        "RAWX NR did not converge"
    );

    // Compare Vm and Va bus-by-bus
    let mut max_vm_diff = 0.0_f64;
    let mut max_va_diff = 0.0_f64;

    for (i, raw_bus) in raw_net.buses.iter().enumerate() {
        // Find matching bus in rawx network
        if let Some(j) = rawx_net
            .buses
            .iter()
            .position(|b| b.number == raw_bus.number)
        {
            let vm_diff =
                (raw_result.voltage_magnitude_pu[i] - rawx_result.voltage_magnitude_pu[j]).abs();
            let va_diff =
                (raw_result.voltage_angle_rad[i] - rawx_result.voltage_angle_rad[j]).abs();
            max_vm_diff = max_vm_diff.max(vm_diff);
            max_va_diff = max_va_diff.max(va_diff);
        }
    }

    println!("RAW vs RAWX: max|Vm| = {max_vm_diff:.2e}, max|Va| = {max_va_diff:.2e}");
    assert!(
        max_vm_diff < 1e-6,
        "Vm mismatch too large: {max_vm_diff:.2e}"
    );
    assert!(
        max_va_diff < 1e-6,
        "Va mismatch too large: {max_va_diff:.2e}"
    );
}

#[test]
fn test_load_routes_rawx_extension() {
    // Verify .rawx extension is routed to the RAWX parser (not "unsupported format")
    let result = surge_io::load(std::path::Path::new("nonexistent.rawx"));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        !msg.contains("unsupported file format"),
        "Expected I/O error, got: {msg}"
    );
}
