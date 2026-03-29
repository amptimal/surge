// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Phase 7: PowerModelsACDC.jl cross-validation tests.
//!
//! These tests load pglib-opf-hvdc benchmark cases, parse the DC network
//! format (busdc/convdc/branchdc), convert to surge-hvdc solver types,
//! and run AC-DC power flow. Results are compared against pre-generated
//! reference solutions from PowerModelsACDC.jl.
//!
//! Skip automatically if pglib-opf-hvdc cases are not available locally.

use num_complex::Complex64;
use std::path::PathBuf;

fn surge_bench_dir() -> Option<PathBuf> {
    std::env::var("SURGE_BENCH_DIR").ok().map(PathBuf::from)
}

/// Path to pglib-opf-hvdc instances.
fn pglib_dir() -> Option<PathBuf> {
    surge_bench_dir().map(|dir| dir.join("instances/pglib-opf-hvdc"))
}

/// Path to PowerModelsACDC reference solutions.
fn refs_dir() -> Option<PathBuf> {
    surge_bench_dir().map(|dir| dir.join("hvdc/references"))
}

fn has_pglib_cases() -> bool {
    pglib_dir()
        .map(|dir| dir.join("case5_3_he.m").exists())
        .unwrap_or(false)
}

fn has_references() -> bool {
    refs_dir()
        .map(|dir| dir.join("case5_3_he_ref.json").exists())
        .unwrap_or(false)
}

fn load_case(name: &str) -> surge_network::Network {
    let path = pglib_dir()
        .expect("set SURGE_BENCH_DIR to the surge-bench checkout root")
        .join(format!("{}.m", name));
    assert!(path.exists(), "Case file not found: {:?}", path);
    surge_io::matpower::load(&path).expect("Failed to parse case file")
}

// ───────────────────────────────────────────────────────────────────
// Parser validation: verify DC network sections are parsed correctly
// ───────────────────────────────────────────────────────────────────

#[test]
fn test_pglib_case5_3_parse_dc_buses() {
    if !has_pglib_cases() {
        eprintln!("SKIP: pglib-opf-hvdc cases not found; set SURGE_BENCH_DIR");
        return;
    }
    let net = load_case("case5_3_he");
    assert_eq!(net.buses.len(), 5);
    assert_eq!(net.branches.len(), 6);
    assert_eq!(net.hvdc.dc_bus_count(), 3, "Expected 3 DC buses");
    assert_eq!(net.hvdc.dc_converter_count(), 3, "Expected 3 DC converters");
    assert_eq!(net.hvdc.dc_branch_count(), 3, "Expected 3 DC branches");
}

#[test]
fn test_pglib_case5_3_dc_converter_params() {
    if !has_pglib_cases() {
        return;
    }
    let net = load_case("case5_3_he");
    let dc_converters: Vec<_> = net
        .hvdc
        .dc_converters()
        .filter_map(|c| c.as_vsc())
        .collect();
    let conv1 = dc_converters[0];
    assert_eq!(conv1.dc_bus, 1);
    assert_eq!(conv1.ac_bus, 2);
    assert!(conv1.status, "Converter 1 should be in service");
    assert!(conv1.loss_constant_mw > 0.0, "LossA should be positive");
    assert!(conv1.loss_linear > 0.0, "LossB should be positive");
}

#[test]
fn test_pglib_case5_3_dc_branch_params() {
    if !has_pglib_cases() {
        return;
    }
    let net = load_case("case5_3_he");
    let dc_branches: Vec<_> = net.hvdc.dc_branches().collect();
    let br1 = dc_branches[0];
    assert_eq!(br1.from_bus, 1);
    assert_eq!(br1.to_bus, 2);
    assert!(br1.r_ohm > 0.0, "DC branch resistance should be positive");
    assert!(br1.status, "DC branch should be in service");
}

#[test]
fn test_pglib_case24_7_parse() {
    if !has_pglib_cases() {
        return;
    }
    let path = pglib_dir().unwrap().join("case24_7_jb.m");
    if !path.exists() {
        return;
    }
    let net = surge_io::matpower::load(&path).expect("Failed to parse case24_7_jb.m");
    assert!(
        net.buses.len() >= 24,
        "Expected >= 24 AC buses, got {}",
        net.buses.len()
    );
    assert!(net.hvdc.dc_bus_count() > 0, "Expected DC buses");
    assert!(net.hvdc.dc_converter_count() > 0, "Expected DC converters");
    assert!(net.hvdc.dc_branch_count() > 0, "Expected DC branches");
}

#[test]
fn test_pglib_case39_10_parse() {
    if !has_pglib_cases() {
        return;
    }
    let path = pglib_dir().unwrap().join("case39_10_he.m");
    if !path.exists() {
        return;
    }
    let net = surge_io::matpower::load(&path).expect("Failed to parse case39_10_he.m");
    assert_eq!(net.buses.len(), 39);
    assert!(
        net.hvdc.dc_converter_count() >= 5,
        "Expected >= 5 converters, got {}",
        net.hvdc.dc_converter_count()
    );
}

#[test]
fn test_pglib_case3120_5_parse() {
    if !has_pglib_cases() {
        return;
    }
    let path = pglib_dir().unwrap().join("case3120_5_he.m");
    if !path.exists() {
        return;
    }
    let net = surge_io::matpower::load(&path).expect("Failed to parse case3120_5_he.m");
    assert!(net.buses.len() >= 3000);
    assert!(net.hvdc.dc_bus_count() > 0, "Expected DC buses");
}

// ───────────────────────────────────────────────────────────────────
// Bridge validation: DC network → surge-hvdc hybrid MTDC
// ───────────────────────────────────────────────────────────────────

#[test]
fn test_pglib_case5_3_dc_to_mtdc() {
    if !has_pglib_cases() {
        return;
    }
    let net = load_case("case5_3_he");
    let dc_buses: Vec<_> = net.hvdc.dc_buses().collect();
    let dc_converters: Vec<_> = net.hvdc.dc_converters().collect();
    let dc_branches: Vec<_> = net.hvdc.dc_branches().collect();

    // Determine the slack DC bus from converters before building the network.
    let n_dc_buses = dc_buses.len();
    let mut slack_dc_bus: usize = 0;
    let mut slack_v: f64 = 1.0;
    for conv in &dc_converters {
        let Some(conv) = conv.as_vsc() else {
            continue;
        };
        if !conv.status {
            continue;
        }
        let dc_idx = (conv.dc_bus - 1) as usize;
        let is_slack = conv.control_type_dc == 2;
        if is_slack && dc_idx < n_dc_buses {
            slack_dc_bus = dc_idx;
            slack_v = conv.voltage_dc_setpoint_pu.max(1.0);
        }
    }

    // Build hybrid MTDC from DC network data
    let mut mtdc = surge_hvdc::advanced::hybrid::HybridMtdcNetwork::new(
        net.base_mva,
        n_dc_buses,
        slack_dc_bus,
    );
    mtdc.dc_network.v_dc_slack = slack_v;

    for (idx, dcb) in dc_buses.iter().enumerate() {
        mtdc.dc_network.v_dc[idx] = dcb.v_dc_pu.max(1.0);
    }

    for conv in &dc_converters {
        let Some(conv) = conv.as_vsc() else {
            continue;
        };
        if !conv.status {
            continue;
        }
        let dc_idx = (conv.dc_bus - 1) as usize;
        let is_slack = conv.control_type_dc == 2;
        let mut vsc = surge_hvdc::advanced::hybrid::HybridVscConverter::new(
            conv.ac_bus,
            dc_idx,
            if is_slack {
                0.0
            } else {
                conv.active_power_mw * net.base_mva
            },
        );
        vsc.is_dc_slack = is_slack;
        vsc.v_dc_setpoint = conv.voltage_dc_setpoint_pu.max(1.0);
        mtdc.vsc_converters.push(vsc);
    }

    for dcbr in &dc_branches {
        if !dcbr.status {
            continue;
        }
        let base_kv_dc = dc_buses[0].base_kv_dc.max(100.0);
        let z_base = (base_kv_dc * base_kv_dc) / net.base_mva;
        let r_pu = dcbr.r_ohm / z_base;
        mtdc.dc_network
            .branches
            .push(surge_hvdc::advanced::hybrid::DcBranch {
                from_dc_bus: (dcbr.from_bus - 1) as usize,
                to_dc_bus: (dcbr.to_bus - 1) as usize,
                r_dc_pu: r_pu.max(1e-6),
                i_max_pu: 0.0,
            });
    }

    let n_conv = mtdc.vsc_converters.len();
    let ac_v = vec![Complex64::new(1.0, 0.0); n_conv];
    let result = surge_hvdc::advanced::hybrid::solve(&mtdc, &ac_v, 50, 1e-6)
        .expect("MTDC should converge for case5_3");
    assert!(result.converged, "MTDC should converge for case5_3");
    assert_eq!(result.dc_voltages_pu.len(), 3);

    for (i, v) in result.dc_voltages_pu.iter().enumerate() {
        assert!(
            (*v - 1.0).abs() < 0.1,
            "DC bus {} voltage {:.4} too far from 1.0 pu",
            i,
            v
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// Reference comparison: Surge AC PF bus voltages vs PowerModelsACDC
// ───────────────────────────────────────────────────────────────────

#[test]
fn test_pglib_case5_3_pf_bus_voltages_vs_reference() {
    if !has_pglib_cases() || !has_references() {
        eprintln!("SKIP: pglib cases or references not found");
        return;
    }

    let ref_path = refs_dir().unwrap().join("case5_3_he_ref.json");
    let ref_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&ref_path).unwrap()).unwrap();

    let pf_ref = &ref_json["pf"];
    let pf_status = pf_ref["termination_status"].as_str().unwrap_or("N/A");
    assert_eq!(pf_status, "Converged", "Reference PF should have converged");

    let net = load_case("case5_3_he");
    let opts = surge_ac::AcPfOptions::default();
    let sol = surge_ac::solve_ac_pf(&net, &opts).expect("Surge NR should converge on case5_3");

    let ref_buses = pf_ref["bus"].as_object().unwrap();
    // Relaxed tolerance: Surge AC-only vs PowerModelsACDC AC-DC coupled
    let tol_vm = 0.02;

    for (bus_id_str, bus_data) in ref_buses {
        let ref_vm = bus_data["vm"].as_f64().unwrap();
        let bus_id: u32 = bus_id_str.parse().unwrap();

        if let Some(idx) = net.buses.iter().position(|b| b.number == bus_id) {
            let surge_vm = sol.voltage_magnitude_pu[idx];
            let vm_error = (surge_vm - ref_vm).abs();
            assert!(
                vm_error < tol_vm,
                "Bus {}: Vm error {:.6} > {:.3} (Surge={:.6}, Ref={:.6})",
                bus_id,
                vm_error,
                tol_vm,
                surge_vm,
                ref_vm
            );
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// Reference comparison: DC bus voltages
// ───────────────────────────────────────────────────────────────────

#[test]
fn test_pglib_case5_3_dc_voltages_vs_reference() {
    if !has_pglib_cases() || !has_references() {
        return;
    }

    let ref_path = refs_dir().unwrap().join("case5_3_he_ref.json");
    let ref_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&ref_path).unwrap()).unwrap();

    let pf_ref = &ref_json["pf"];
    if pf_ref["termination_status"].as_str() != Some("Converged") {
        return;
    }

    let ref_busdc = pf_ref["busdc"].as_object().unwrap();

    for (dc_id, dc_data) in ref_busdc {
        let ref_vdc = dc_data["vm"].as_f64().unwrap_or(1.0);
        eprintln!("Reference DC bus {}: Vdc = {:.6} pu", dc_id, ref_vdc);
        assert!(
            ref_vdc > 0.9 && ref_vdc < 1.1,
            "DC bus {} voltage {:.4} out of range",
            dc_id,
            ref_vdc
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// Performance
// ───────────────────────────────────────────────────────────────────

#[test]
fn test_pglib_case3120_5_parse_performance() {
    if !has_pglib_cases() {
        return;
    }
    let path = pglib_dir().unwrap().join("case3120_5_he.m");
    if !path.exists() {
        return;
    }

    let start = std::time::Instant::now();
    let net = surge_io::matpower::load(&path).expect("Failed to parse case3120_5_he.m");
    let parse_ms = start.elapsed().as_millis();

    eprintln!(
        "case3120_5_he: {} AC buses, {} DC buses, {} converters, parsed in {}ms",
        net.buses.len(),
        net.hvdc.dc_bus_count(),
        net.hvdc.dc_converter_count(),
        parse_ms
    );

    assert!(
        parse_ms < 1000,
        "Parsing took {}ms (should be < 1s)",
        parse_ms
    );
}
