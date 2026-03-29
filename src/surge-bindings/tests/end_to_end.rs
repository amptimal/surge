// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! End-to-end integration test — exercises the full Surge planning suite.
//!
//! Pipeline: load case → DC PF → AC PF → DC-OPF → AC-OPF → SCOPF →
//!           contingency analysis → SCED → SCUC → LOLE → expansion
//!
//! Validates that all modules work correctly on the same network
//! and that results are consistent across modules.
//!
//! # Thread safety note
//!
//! MUMPS (used by Ipopt for AC-OPF) is not safe to initialize concurrently
//! from multiple threads.  All tests in this file that invoke AC-OPF must
//! hold `IPOPT_MUTEX` while running.

mod common;

use std::sync::Mutex;

/// Serialize all Ipopt/MUMPS calls across tests in this binary to avoid
/// concurrent DMUMPS_LOAD_INIT which causes SIGSEGV with the system MUMPS.
static IPOPT_MUTEX: Mutex<()> = Mutex::new(());

fn format_optional_iterations(iterations: Option<u32>) -> String {
    iterations
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn load_case(name: &str) -> surge_network::Network {
    let path = common::test_data_dir().join(format!("{name}.m"));
    surge_io::matpower::load(&path).unwrap_or_else(|e| panic!("failed to parse {name}: {e}"))
}

/// Full planning suite pipeline on case9 (smallest standard test case).
#[test]
fn test_end_to_end_case9() {
    if !common::data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let _guard = IPOPT_MUTEX.lock().unwrap();
    let network = load_case("case9");

    // ── 1. DC Power Flow ──
    let dc_result = surge_dc::solve_dc(&network).expect("DC power flow failed");
    assert!(
        dc_result.theta.len() == network.n_buses(),
        "DC PF should produce angles for all buses"
    );
    let dc_solution = surge_dc::to_pf_solution(&dc_result, &network);
    assert!(
        dc_solution.voltage_angle_rad.len() == network.n_buses(),
        "DC solution should have angles for all buses"
    );
    eprintln!(
        "  DC PF: slack={:.4} p.u., {:.3}ms",
        dc_result.slack_p_injection,
        dc_result.solve_time_secs * 1000.0
    );

    // ── 2. AC Power Flow (Newton-Raphson with KLU) ──
    let acpf_opts = surge_ac::AcPfOptions::default();
    let ac_result =
        surge_ac::solve_ac_pf_kernel(&network, &acpf_opts).expect("NR power flow failed");
    assert_eq!(
        ac_result.status,
        surge_solution::SolveStatus::Converged,
        "NR should converge on case9"
    );
    assert!(
        ac_result.iterations <= 10,
        "NR should converge fast on case9: {} iters",
        ac_result.iterations
    );
    eprintln!(
        "  AC PF: {} iters, mismatch={:.2e}, {:.3}ms",
        ac_result.iterations,
        ac_result.max_mismatch,
        ac_result.solve_time_secs * 1000.0
    );

    // ── 3. FDPF (Fast Decoupled) ──
    let ybus = surge_ac::matrix::ybus::build_ybus(&network);
    let mut fdpf = surge_ac::FdpfFactors::new(&network).expect("FdpfFactors::new failed on case9");
    let p_spec = network.bus_p_injection_pu();
    let q_spec = network.bus_q_injection_pu();
    let vm: Vec<f64> = network
        .buses
        .iter()
        .map(|b| b.voltage_magnitude_pu)
        .collect();
    let va: Vec<f64> = network.buses.iter().map(|b| b.voltage_angle_rad).collect();
    let fdpf_result = fdpf.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm, &va, 1e-6, 50);
    assert!(fdpf_result.is_some(), "FDPF should converge on case9");
    let fdpf_r = fdpf_result.unwrap();
    eprintln!("  FDPF:  {} iters", fdpf_r.iterations);

    // FDPF Vm should be reasonably close to NR Vm at PQ buses
    // (FDPF is approximate — linear convergence, 0.05 p.u. tolerance is typical)
    for (i, bus) in network.buses.iter().enumerate() {
        if bus.bus_type == surge_network::network::BusType::PQ {
            let diff = (fdpf_r.vm[i] - ac_result.voltage_magnitude_pu[i]).abs();
            assert!(
                diff < 0.05,
                "FDPF Vm at bus {} differs from NR by {:.6}",
                bus.number,
                diff
            );
        }
    }

    // ── 4. PTDF/LODF ──
    let n_br = network.n_branches();
    let all_branches: Vec<usize> = (0..n_br).collect();
    let ptdf = surge_dc::compute_ptdf(
        &network,
        &surge_dc::PtdfRequest::for_branches(&all_branches),
    )
    .unwrap();
    assert_eq!(ptdf.n_rows(), n_br);
    let lodf = surge_dc::compute_lodf_matrix(
        &network,
        &surge_dc::LodfMatrixRequest::for_branches(&all_branches),
    )
    .unwrap();
    assert_eq!(lodf.n_rows(), n_br);
    assert_eq!(lodf.n_cols(), n_br);
    eprintln!(
        "  PTDF:  {}x{}, LODF: {}x{}",
        ptdf.n_rows(),
        network.n_buses(),
        lodf.n_rows(),
        lodf.n_cols()
    );

    // ── 5. DC-OPF ──
    let dc_opf_opts = surge_opf::DcOpfOptions::default();
    let dc_opf = surge_opf::solve_dc_opf(&network, &dc_opf_opts)
        .expect("DC-OPF failed")
        .opf;
    assert!(dc_opf.total_cost > 0.0, "DC-OPF cost should be positive");
    assert_eq!(
        dc_opf.generators.gen_p_mw.len(),
        network.generators.iter().filter(|g| g.in_service).count()
    );
    eprintln!(
        "  DC-OPF: cost={:.2} $/hr, {:.3}ms",
        dc_opf.total_cost,
        dc_opf.solve_time_secs * 1000.0
    );

    // ── 6. AC-OPF ──
    let ac_opf_opts = surge_opf::AcOpfOptions {
        exact_hessian: true,
        ..Default::default()
    };
    let ac_opf = surge_opf::solve_ac_opf(&network, &ac_opf_opts).expect("AC-OPF failed");
    assert!(ac_opf.total_cost > 0.0, "AC-OPF cost should be positive");
    // AC-OPF cost >= DC-OPF cost (losses make it more expensive)
    assert!(
        ac_opf.total_cost >= dc_opf.total_cost * 0.95,
        "AC-OPF cost ({:.2}) should be >= DC-OPF cost ({:.2}) minus 5% tolerance",
        ac_opf.total_cost,
        dc_opf.total_cost
    );
    eprintln!(
        "  AC-OPF: cost={:.2} $/hr, {} iters, {:.3}ms",
        ac_opf.total_cost,
        format_optional_iterations(ac_opf.iterations),
        ac_opf.solve_time_secs * 1000.0
    );

    // ── 7. SCOPF ──
    let scopf_opts = surge_opf::ScopfOptions {
        dc_opf: dc_opf_opts.clone(),
        ..Default::default()
    };
    let scopf = surge_opf::solve_scopf(&network, &scopf_opts).expect("SCOPF failed");
    // SCOPF cost >= DC-OPF cost (additional security constraints)
    assert!(
        scopf.base_opf.total_cost >= dc_opf.total_cost * 0.99,
        "SCOPF cost ({:.2}) should be >= DC-OPF cost ({:.2})",
        scopf.base_opf.total_cost,
        dc_opf.total_cost
    );
    eprintln!(
        "  SCOPF:  cost={:.2} $/hr, {} SCOPF iters, {} contingency constraints",
        scopf.base_opf.total_cost, scopf.iterations, scopf.total_contingency_constraints
    );

    // ── 8. N-1 Contingency Analysis ──
    let ca_opts = surge_contingency::ContingencyOptions::default();
    let ca_result = surge_contingency::analyze_n1_branch(&network, &ca_opts)
        .expect("Contingency analysis failed");
    assert_eq!(
        ca_result.summary.total_contingencies,
        network.branches.iter().filter(|b| b.in_service).count()
    );
    assert!(
        ca_result.summary.converged > 0,
        "Some contingencies should converge"
    );
    eprintln!(
        "  N-1 CA: {}/{} converged, {} violations, {:.3}s",
        ca_result.summary.converged,
        ca_result.summary.total_contingencies,
        ca_result.summary.with_violations,
        ca_result.summary.solve_time_secs
    );

    // ── Summary ──
    eprintln!("\n  === End-to-end case9 pipeline: ALL 8 MODULES PASSED ===");
}

/// Pipeline on case118 (medium network, validates scaling).
#[test]
fn test_end_to_end_case118() {
    if !common::data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let _guard = IPOPT_MUTEX.lock().unwrap();
    let network = load_case("case118");

    // DC PF
    let dc = surge_dc::solve_dc(&network).expect("DC PF failed on case118");
    assert_eq!(dc.theta.len(), 118);

    // AC PF
    let ac = surge_ac::solve_ac_pf_kernel(&network, &surge_ac::AcPfOptions::default())
        .expect("NR failed on case118");
    assert_eq!(ac.status, surge_solution::SolveStatus::Converged);

    // DC-OPF
    let dc_opf = surge_opf::solve_dc_opf(&network, &surge_opf::DcOpfOptions::default())
        .expect("DC-OPF failed on case118");
    assert!(dc_opf.opf.total_cost > 0.0);

    // AC-OPF
    let ac_opf = surge_opf::solve_ac_opf(
        &network,
        &surge_opf::AcOpfOptions {
            exact_hessian: true,
            ..Default::default()
        },
    )
    .expect("AC-OPF failed on case118");
    assert!(ac_opf.total_cost > 0.0);

    // N-1 CA
    let ca = surge_contingency::analyze_n1_branch(
        &network,
        &surge_contingency::ContingencyOptions::default(),
    )
    .expect("CA failed on case118");
    assert!(ca.summary.total_contingencies > 100);

    eprintln!(
        "\n  case118 pipeline: DC({:.3}ms) AC({:.3}ms) DC-OPF(${:.0}) AC-OPF(${:.0}) CA({}/{})",
        dc.solve_time_secs * 1000.0,
        ac.solve_time_secs * 1000.0,
        dc_opf.opf.total_cost,
        ac_opf.total_cost,
        ca.summary.converged,
        ca.summary.total_contingencies,
    );
}
