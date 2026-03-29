// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES power flow convergence tests.
//!
//! Parses real CGMES profiles (EQ+TP+SSH+SV) exported via pypowsybl/surge-io,
//! runs Newton-Raphson power flow, and verifies convergence.  Tests are skipped
//! if the CGMES test data directory does not exist so CI is not blocked.
//!
//! All tests are `#[ignore]` by default — they require large CGMES datasets and
//! can take 10+ minutes for the full suite.  Run explicitly with:
//!   cargo test -p surge-bindings --test test_cgmes_pf -- --ignored
//! or a single case:
//!   cargo test -p surge-bindings --test test_cgmes_pf cgmes_pf_case9 -- --ignored

mod common;

use std::path::{Path, PathBuf};

fn cgmes_dir(case: &str) -> PathBuf {
    common::test_data_dir().join("cgmes").join(case)
}

/// Load all XML files from a CGMES directory (excluding DiagramLayout).
fn load_cgmes_network(dir: &Path) -> Option<surge_network::Network> {
    if !dir.exists() {
        return None;
    }
    let profiles: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "xml").unwrap_or(false))
        .filter(|p| !p.to_string_lossy().contains("DiagramLayout"))
        .collect();
    if profiles.is_empty() {
        return None;
    }
    let refs: Vec<&Path> = profiles.iter().map(|p| p.as_path()).collect();
    surge_io::cgmes::load_all(&refs).ok()
}

/// Solve power flow and assert convergence.
fn assert_cgmes_pf_converges(case: &str, expected_buses: usize) {
    let dir = cgmes_dir(case);
    let net = match load_cgmes_network(&dir) {
        Some(n) => n,
        None => {
            eprintln!("SKIP {case}: CGMES data not found at {}", dir.display());
            return;
        }
    };

    assert!(
        net.n_buses() >= expected_buses,
        "{case}: expected >={expected_buses} buses, got {}",
        net.n_buses()
    );

    // Use flat start (Vm=1.0, Va=0.0) so results don't depend on SvVoltage
    // initialization quality, which varies across CGMES profiles.
    let opts = surge_ac::AcPfOptions {
        flat_start: true,
        ..Default::default()
    };
    let solution = surge_ac::solve_ac_pf_kernel(&net, &opts)
        .unwrap_or_else(|e| panic!("{case}: NR-KLU panicked: {e}"));

    eprintln!(
        "{case:25}: buses={} branches={} iters={} status={:?} time={:.1}ms",
        net.n_buses(),
        net.n_branches(),
        solution.iterations,
        solution.status,
        solution.solve_time_secs * 1000.0
    );

    assert_eq!(
        solution.status,
        surge_solution::SolveStatus::Converged,
        "{case}: power flow did not converge after {} iterations",
        solution.iterations
    );

    // All voltages should be reasonable (0.5 – 1.5 pu)
    for (i, &vm) in solution.voltage_magnitude_pu.iter().enumerate() {
        assert!(
            vm > 0.5 && vm < 1.5,
            "{case}: bus {} Vm={:.4} pu out of range [0.5, 1.5]",
            solution.bus_numbers[i],
            vm
        );
    }
}

// ── Small reference cases (exported via pypowsybl with known PF solutions) ──

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_case9() {
    assert_cgmes_pf_converges("case9", 9);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_case14() {
    assert_cgmes_pf_converges("case14", 14);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_case118() {
    assert_cgmes_pf_converges("case118", 118);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_case300() {
    assert_cgmes_pf_converges("case300", 300);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_ieee9_ppow() {
    assert_cgmes_pf_converges("ieee9_ppow", 9);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_ieee57_ppow() {
    assert_cgmes_pf_converges("ieee57_ppow", 57);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_ieee118_ppow() {
    assert_cgmes_pf_converges("ieee118_ppow", 118);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_ieee300_ppow() {
    assert_cgmes_pf_converges("ieee300_ppow", 300);
}

// ── PowSyBl built-in IEEE networks ──

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_ieee57_pbl() {
    assert_cgmes_pf_converges("ieee57_pbl", 57);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_ieee118_pbl() {
    assert_cgmes_pf_converges("ieee118_pbl", 118);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_ieee300_pbl() {
    assert_cgmes_pf_converges("ieee300_pbl", 300);
}

// ── ENTSO-E / CIGRE microgrid networks ──

#[test]
#[ignore = "microgrid_be uses ENTSO-E boundary set BaseVoltage references that are not bundled \
            in the local CGMES files; Surge cannot resolve them → 0-bus network"]
fn cgmes_pf_microgrid_be() {
    assert_cgmes_pf_converges("microgrid_be", 1);
}

#[test]
#[ignore = "microgrid_nl uses ENTSO-E boundary set BaseVoltage references that are not bundled \
            in the local CGMES files; Surge cannot resolve them → 0-bus network"]
fn cgmes_pf_microgrid_nl() {
    assert_cgmes_pf_converges("microgrid_nl", 1);
}

#[test]
#[ignore = "slow: CGMES parse + power flow"]
fn cgmes_pf_cigremv() {
    assert_cgmes_pf_converges("cigremv", 1);
}

// ── Large-scale cases (converted via surge-io XIIDM → pypowsybl → CGMES) ──

#[test]
#[ignore = "slow: CGMES parse + power flow (1354 buses)"]
fn cgmes_pf_case1354pegase() {
    assert_cgmes_pf_converges("case1354pegase", 1354);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (1888 buses)"]
fn cgmes_pf_case1888rte() {
    assert_cgmes_pf_converges("case1888rte", 1888);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (2383 buses)"]
fn cgmes_pf_case2383wp() {
    assert_cgmes_pf_converges("case2383wp", 2383);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (6470 buses)"]
fn cgmes_pf_case6470rte() {
    assert_cgmes_pf_converges("case6470rte", 6470);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (6515 buses)"]
fn cgmes_pf_case6515rte() {
    assert_cgmes_pf_converges("case6515rte", 6515);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (9241 buses)"]
fn cgmes_pf_case9241pegase() {
    assert_cgmes_pf_converges("case9241pegase", 9241);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (2736 buses)"]
fn cgmes_pf_case2736sp() {
    assert_cgmes_pf_converges("case2736sp", 2736);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (3012 buses)"]
fn cgmes_pf_case3012wp() {
    assert_cgmes_pf_converges("case3012wp", 3012);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (3120 buses)"]
fn cgmes_pf_case3120sp() {
    assert_cgmes_pf_converges("case3120sp", 3120);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (13659 buses)"]
fn cgmes_pf_case13659pegase() {
    assert_cgmes_pf_converges("case13659pegase", 13659);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (1197 buses)"]
fn cgmes_pf_case1197() {
    assert_cgmes_pf_converges("case1197", 1197);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (2000 buses)"]
fn cgmes_pf_activsg2000() {
    assert_cgmes_pf_converges("case_ACTIVSg2000", 2000);
}

#[test]
#[ignore = "slow: CGMES parse + power flow (10000 buses)"]
fn cgmes_pf_activsg10k() {
    assert_cgmes_pf_converges("case_ACTIVSg10k", 10000);
}

/// Diagnostic: true flat start (no DC angle init) — line_search on vs off.
#[test]
#[ignore = "diagnostic: true flat start convergence"]
fn diag_true_flat_start_line_search() {
    let cases = ["case9241pegase", "case6470rte", "case_ACTIVSg10k"];
    for case in &cases {
        let dir = cgmes_dir(case);
        let net = match load_cgmes_network(&dir) {
            Some(n) => n,
            None => {
                eprintln!("SKIP {case}: CGMES data not found");
                continue;
            }
        };

        let report = |label: &str, opts: &surge_ac::AcPfOptions| match surge_ac::solve_ac_pf_kernel(
            &net, opts,
        ) {
            Ok(s) => eprintln!(
                "  {case:20} {label:20} status={:?} iters={} mm={:.2e}",
                s.status, s.iterations, s.max_mismatch
            ),
            Err(e) => eprintln!("  {case:20} {label:20} ERR: {e}"),
        };

        // With line search (default)
        let opts = surge_ac::AcPfOptions {
            flat_start: true,
            dc_warm_start: false,
            max_iterations: 200,
            enforce_q_limits: false,
            ..Default::default()
        };
        report("flat_true", &opts);
    }
}
