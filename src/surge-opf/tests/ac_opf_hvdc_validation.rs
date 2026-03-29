// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Validation of joint AC-DC OPF against PowerModelsACDC.jl references.
//!
//! Loads pglib-opf-hvdc benchmark cases, solves with Surge's joint NLP,
//! and compares OPF objectives and bus voltages against pre-generated
//! reference solutions from PowerModelsACDC.jl.
//!
//! Skips automatically if cases or references are not available locally.

use std::path::PathBuf;
use std::time::Instant;

fn surge_bench_dir() -> Option<PathBuf> {
    std::env::var("SURGE_BENCH_DIR").ok().map(PathBuf::from)
}

fn pglib_dir() -> Option<PathBuf> {
    surge_bench_dir().map(|dir| dir.join("instances/pglib-opf-hvdc"))
}

fn refs_dir() -> Option<PathBuf> {
    surge_bench_dir().map(|dir| dir.join("hvdc/references"))
}

fn has_data() -> bool {
    pglib_dir()
        .map(|dir| dir.join("case5_3_he.m").exists())
        .unwrap_or(false)
        && refs_dir()
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

fn load_ref(name: &str) -> serde_json::Value {
    let path = refs_dir()
        .expect("set SURGE_BENCH_DIR to the surge-bench checkout root")
        .join(format!("{}_ref.json", name));
    let text = std::fs::read_to_string(&path).expect("Failed to read reference file");
    serde_json::from_str(&text).expect("Failed to parse reference JSON")
}

/// Solve a single case and compare against the PowerModelsACDC reference.
/// Returns (case_name, gap_pct, max_vm_err, solve_time_ms, passed).
fn validate_case(case_name: &str) -> (String, f64, f64, f64, bool) {
    let net = load_case(case_name);
    let ref_json = load_ref(case_name);

    let ref_opf = &ref_json["opf"];
    let ref_status = ref_opf["termination_status"].as_str().unwrap_or("N/A");
    let ref_objective = ref_opf["objective"].as_f64().unwrap_or(0.0);

    eprintln!(
        "\n{}\n  {} -- {} AC buses, {} DC buses, {} converters",
        "=".repeat(60),
        case_name,
        net.buses.len(),
        net.hvdc.dc_bus_count(),
        net.hvdc.dc_converter_count(),
    );
    eprintln!(
        "  Reference: status={}, obj={:.2}",
        ref_status, ref_objective
    );

    let nlp = match surge_opf::backends::nlp_solver_from_str("ipopt") {
        Ok(s) => s,
        Err(_) => {
            eprintln!("  SKIP: Ipopt not available");
            return (case_name.to_string(), f64::NAN, f64::NAN, 0.0, false);
        }
    };

    let opts = surge_opf::AcOpfOptions {
        include_hvdc: None,
        ..surge_opf::AcOpfOptions::default()
    };
    let runtime = surge_opf::AcOpfRuntime::default().with_nlp_solver(nlp);

    let start = Instant::now();
    let result = surge_opf::solve_ac_opf_with_runtime(&net, &opts, &runtime);
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(sol) => {
            let gap_pct = if ref_objective.abs() > 1e-6 {
                ((sol.total_cost - ref_objective) / ref_objective * 100.0).abs()
            } else {
                0.0
            };

            // Vm comparison
            let ref_buses = ref_opf["bus"].as_object();
            let mut max_vm_err = 0.0_f64;
            let mut n_buses_compared = 0;
            if let Some(buses) = ref_buses {
                for (bid_str, bdata) in buses {
                    if let Some(ref_vm) = bdata["vm"].as_f64() {
                        let bid: u32 = bid_str.parse().unwrap_or(0);
                        if let Some(idx) = net.buses.iter().position(|b| b.number == bid) {
                            let err = (sol.power_flow.voltage_magnitude_pu[idx] - ref_vm).abs();
                            max_vm_err = max_vm_err.max(err);
                            n_buses_compared += 1;
                        }
                    }
                }
            }

            eprintln!(
                "  Surge:     obj={:.2}, time={:.0}ms",
                sol.total_cost, elapsed_ms
            );
            eprintln!(
                "  Gap:       {:.2}%, Max Vm err: {:.4} pu ({} buses compared)",
                gap_pct, max_vm_err, n_buses_compared
            );

            // Print converter dispatch from solution
            // (P_conv variables are after the standard AC-OPF variables)
            let n_conv = net
                .hvdc
                .dc_converters()
                .filter_map(|c| c.as_vsc())
                .filter(|c| c.status)
                .count();
            if n_conv > 0 {
                eprintln!("  Converters: {} active in NLP", n_conv);
            }

            (case_name.to_string(), gap_pct, max_vm_err, elapsed_ms, true)
        }
        Err(e) => {
            eprintln!("  FAIL: {:?} (time={:.0}ms)", e, elapsed_ms);
            (case_name.to_string(), f64::NAN, f64::NAN, elapsed_ms, false)
        }
    }
}

#[test]
fn test_joint_nlp_all_pglib_cases() {
    if !has_data() {
        eprintln!("SKIP: pglib-opf-hvdc data not found; set SURGE_BENCH_DIR");
        return;
    }

    let refs_dir = refs_dir().unwrap();
    let pglib_dir = pglib_dir().unwrap();

    let cases = [
        "case5_3_he",
        "case24_7_jb",
        "case39_10_he",
        "case67",
        "case3120_5_he",
        "nem_2000bus_hvdc",
    ];

    let mut results = Vec::new();
    for case_name in &cases {
        let ref_path = refs_dir.join(format!("{}_ref.json", case_name));
        let case_path = pglib_dir.join(format!("{}.m", case_name));
        if !ref_path.exists() || !case_path.exists() {
            eprintln!("SKIP {}: files not found", case_name);
            continue;
        }
        results.push(validate_case(case_name));
    }

    // Summary table
    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  SUMMARY: Surge Joint AC-DC NLP vs PowerModelsACDC.jl");
    eprintln!("{}", "=".repeat(60));
    eprintln!(
        "  {:25} {:>8} {:>10} {:>10} {:>6}",
        "Case", "Gap %", "Max Vm", "Time ms", "OK?"
    );
    eprintln!(
        "  {:-<25} {:-<8} {:-<10} {:-<10} {:-<6}",
        "", "", "", "", ""
    );
    let mut n_pass = 0;
    let mut n_fail = 0;
    for (name, gap, vm_err, time, passed) in &results {
        let status = if *passed { "PASS" } else { "FAIL" };
        if *passed {
            n_pass += 1;
        } else {
            n_fail += 1;
        }
        eprintln!(
            "  {:25} {:>7.2}% {:>9.4} {:>9.0} {:>6}",
            name, gap, vm_err, time, status
        );
    }
    eprintln!(
        "\n  Total: {} pass, {} fail out of {}",
        n_pass,
        n_fail,
        results.len()
    );

    // Assert at least case5_3 passes tightly
    for (name, gap, _vm_err, _time, passed) in &results {
        if name == "case5_3_he" {
            assert!(*passed, "case5_3_he should converge");
            assert!(
                *gap < 2.0,
                "case5_3_he objective gap {:.2}% exceeds 2%",
                gap
            );
        }
    }
}

/// Parse and solve synthetic HVDC overlay cases (ACTIVSg25k/70k).
/// No reference comparison — just verifies parse + convergence.
#[test]
fn test_synthetic_hvdc_parse_and_solve() {
    let synthetic_cases = [
        ("case_ACTIVSg25k_hvdc", 25000, 14),
        ("case_ACTIVSg70k_hvdc", 70000, 20),
    ];

    let Some(pglib_dir) = pglib_dir() else {
        eprintln!("SKIP: synthetic HVDC cases not found; set SURGE_BENCH_DIR");
        return;
    };

    for (case_name, expected_min_buses, expected_converters) in &synthetic_cases {
        let case_path = pglib_dir.join(format!("{}.m", case_name));
        if !case_path.exists() {
            eprintln!("SKIP {}: file not found at {:?}", case_name, case_path);
            continue;
        }

        let net = surge_io::matpower::load(&case_path).expect("Failed to parse case file");
        eprintln!(
            "\n{}: {} AC buses, {} DC buses, {} converters, {} DC branches",
            case_name,
            net.buses.len(),
            net.hvdc.dc_bus_count(),
            net.hvdc.dc_converter_count(),
            net.hvdc.dc_branch_count(),
        );

        assert!(
            net.buses.len() >= *expected_min_buses,
            "{}: expected >= {} buses, got {}",
            case_name,
            expected_min_buses,
            net.buses.len()
        );
        assert_eq!(
            net.hvdc.dc_converter_count(),
            *expected_converters,
            "{}: expected {} converters",
            case_name,
            expected_converters
        );
        assert!(
            net.hvdc.dc_bus_count() > 0,
            "{}: DC buses should not be empty",
            case_name
        );
        assert!(
            net.hvdc.dc_branch_count() > 0,
            "{}: DC branches should not be empty",
            case_name
        );

        // Verify DC grid assignment
        let grids: std::collections::HashSet<u32> =
            net.hvdc.dc_grids.iter().map(|g| g.id).collect();
        eprintln!("  DC grids: {:?}", grids);
        assert!(
            grids.len() >= 3,
            "{}: expected >= 3 DC grids, got {}",
            case_name,
            grids.len()
        );

        // Verify at least one Vdc slack per grid
        for grid_id in &grids {
            let has_slack = net
                .hvdc
                .find_dc_grid(*grid_id)
                .into_iter()
                .flat_map(|grid| grid.converters.iter())
                .filter_map(|converter| converter.as_vsc())
                .any(|converter| converter.control_type_dc == 2);
            assert!(
                has_slack,
                "{}: grid {} has no Vdc slack converter",
                case_name, grid_id
            );
        }
    }
}
