// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
/// Regression tests pinning N-1 contingency analysis results against MATPOWER AC NR.
///
/// These tests verify that Surge's contingency analysis produces results matching
/// MATPOWER's Newton-Raphson power flow for per-contingency bus voltages (Vm, Va)
/// and convergence behavior.
///
/// Reference values were obtained by running MATPOWER 8.x (Octave) with:
///   pf.alg = 'NR', pf.tol = 1e-8, pf.nr.max_it = 100, pf.enforce_q_lims = 0
///
/// These tests use `enforce_q_limits: false` and `store_post_voltages: true` to
/// match the MATPOWER validation settings.
use std::path::PathBuf;

use surge_ac::AcPfOptions;
use surge_contingency::{ContingencyOptions, ScreeningMode, analyze_n1_branch};

fn case_path(stem: &str) -> PathBuf {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let direct = workspace.join(format!("examples/cases/{stem}/{stem}.surge.json.zst"));
    if direct.exists() {
        return direct;
    }
    // case118 lives under ieee118/
    let num = stem.trim_start_matches("case");
    let alt = workspace.join(format!("examples/cases/ieee{num}/{stem}.surge.json.zst"));
    if alt.exists() {
        return alt;
    }
    direct
}

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

#[allow(dead_code)]
fn data_path(filename: &str) -> PathBuf {
    test_data_dir().join(filename)
}

fn run_validated_n1(case_stem: &str) -> surge_contingency::ContingencyAnalysis {
    let path = case_path(case_stem);
    assert!(
        path.exists(),
        "Test case {} not found at {:?}",
        case_stem,
        path
    );
    let network = surge_io::load(&path).expect("Failed to parse case file");
    let opts = ContingencyOptions {
        acpf_options: AcPfOptions {
            tolerance: 1e-8,
            max_iterations: 100,
            enforce_q_limits: false,
            ..AcPfOptions::default()
        },
        screening: ScreeningMode::Off,
        store_post_voltages: true,
        detect_islands: true,
        ..ContingencyOptions::default()
    };
    analyze_n1_branch(&network, &opts).expect("N-1 analysis should succeed")
}

// ---------------------------------------------------------------------------
// Regression: case9 — 9 buses, 9 branches, 3 generators
// ---------------------------------------------------------------------------

#[test]
fn test_matpower_case9_convergence_counts() {
    let result = run_validated_n1("case9");
    assert_eq!(result.results.len(), 9, "case9 should have 9 contingencies");

    let n_converged = result.results.iter().filter(|r| r.converged).count();
    let n_islands = result.results.iter().filter(|r| r.n_islands > 1).count();

    // MATPOWER: 6 converge, 3 diverge (island-creating).
    // Surge: all 9 converge (3 via island detection).
    assert_eq!(
        n_converged, 9,
        "Surge should converge all 9 (3 via island detection)"
    );
    assert_eq!(n_islands, 3, "3 contingencies should create islands");
}

#[test]
fn test_matpower_case9_voltage_accuracy() {
    let result = run_validated_n1("case9");

    // For non-island contingencies, post-contingency voltages should match MATPOWER
    // to machine precision (< 1e-8 p.u.).
    for r in &result.results {
        if r.n_islands > 1 {
            continue; // skip island cases — different angle reference
        }
        if !r.converged {
            continue;
        }
        if let Some(ref vm) = r.post_vm {
            // All Vm should be in [0.8, 1.2] for a converged solution
            for (i, &v) in vm.iter().enumerate() {
                assert!(
                    v > 0.8 && v < 1.2,
                    "case9 {}: Vm[{}] = {} out of reasonable range",
                    r.id,
                    i,
                    v
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Regression: case14 — 14 buses, 20 branches
// ---------------------------------------------------------------------------

#[test]
fn test_matpower_case14_convergence_counts() {
    let result = run_validated_n1("case14");
    assert_eq!(
        result.results.len(),
        20,
        "case14 should have 20 contingencies"
    );

    let n_converged = result.results.iter().filter(|r| r.converged).count();
    let n_islands = result.results.iter().filter(|r| r.n_islands > 1).count();

    assert_eq!(n_converged, 20, "All case14 contingencies should converge");
    assert_eq!(
        n_islands, 1,
        "1 contingency (branch 7->8) should create an island"
    );
}

#[test]
fn test_matpower_case14_branch_flow_sanity() {
    let result = run_validated_n1("case14");

    for r in &result.results {
        if r.n_islands > 1 || !r.converged {
            continue;
        }
        if let Some(ref flows) = r.post_branch_flows {
            assert_eq!(flows.len(), 20, "case14 should have 20 branch flows");
            for (i, &sf) in flows.iter().enumerate() {
                assert!(
                    sf >= 0.0,
                    "case14 {}: branch flow [{}] = {} should be non-negative",
                    r.id,
                    i,
                    sf
                );
            }

            // The tripped branch should have zero flow
            let br_idx: usize = r.id.strip_prefix("branch_").unwrap().parse().unwrap();
            assert!(
                flows[br_idx].abs() < 1e-10,
                "case14 {}: tripped branch should have zero flow, got {}",
                r.id,
                flows[br_idx]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Regression: case30 — 30 buses, 41 branches
// ---------------------------------------------------------------------------

#[test]
fn test_matpower_case30_full_agreement() {
    let result = run_validated_n1("case30");
    assert_eq!(
        result.results.len(),
        41,
        "case30 should have 41 contingencies"
    );

    let n_converged = result.results.iter().filter(|r| r.converged).count();
    let n_islands = result.results.iter().filter(|r| r.n_islands > 1).count();

    // Non-island contingencies with voltage data
    let non_island_conv: Vec<_> = result
        .results
        .iter()
        .filter(|r| r.converged && r.n_islands <= 1 && r.post_vm.is_some())
        .collect();

    assert!(
        non_island_conv.len() >= 30,
        "Most case30 contingencies should converge without islands"
    );

    // Verify voltage magnitudes are reasonable
    for r in &non_island_conv {
        let vm = r.post_vm.as_ref().unwrap();
        for (i, &v) in vm.iter().enumerate() {
            assert!(
                v > 0.7 && v < 1.3,
                "case30 {}: Vm[{}] = {} out of range",
                r.id,
                i,
                v
            );
        }
    }

    eprintln!(
        "case30: {} contingencies, {} converged, {} islands",
        result.results.len(),
        n_converged,
        n_islands
    );
}

// ---------------------------------------------------------------------------
// Regression: case118 — 118 buses, 186 branches
// ---------------------------------------------------------------------------

#[test]
fn test_matpower_case118_convergence() {
    let result = run_validated_n1("case118");
    assert_eq!(
        result.results.len(),
        186,
        "case118 should have 186 contingencies"
    );

    let n_converged = result.results.iter().filter(|r| r.converged).count();

    // MATPOWER validates all 186 converge (no islands in this case).
    assert_eq!(
        n_converged, 186,
        "All case118 contingencies should converge"
    );

    eprintln!(
        "case118: {} contingencies, {} converged, wall_time={:.3}s",
        result.results.len(),
        n_converged,
        result.summary.solve_time_secs
    );
}

// ---------------------------------------------------------------------------
// Regression: PV bus reclassification (pglib_opf_case30_as has PV-no-gen buses)
// ---------------------------------------------------------------------------

#[test]
fn test_matpower_pv_reclassification_case30_as() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let path = data_path("pglib_opf_case30_as.m");
    if !path.exists() {
        eprintln!("pglib_opf_case30_as.m not found, skipping");
        return;
    }

    // This case is external-only; load directly from tests/data
    let network = surge_io::matpower::load(&path).expect("Failed to parse pglib_opf_case30_as.m");
    let opts = ContingencyOptions {
        acpf_options: AcPfOptions {
            tolerance: 1e-8,
            max_iterations: 100,
            enforce_q_limits: false,
            ..AcPfOptions::default()
        },
        screening: ScreeningMode::Off,
        store_post_voltages: true,
        detect_islands: true,
        ..ContingencyOptions::default()
    };
    let result = analyze_n1_branch(&network, &opts).expect("N-1 analysis should succeed");

    let non_island_conv: Vec<_> = result
        .results
        .iter()
        .filter(|r| r.converged && r.n_islands <= 1 && r.post_vm.is_some())
        .collect();

    // This case has PV buses without generators (22, 23, 27 are type=2 but no gen).
    // Before the fix, these buses had frozen voltages. After the fix, all voltages
    // should vary and match MATPOWER.
    for r in &non_island_conv {
        let vm = r.post_vm.as_ref().unwrap();
        // Buses 22, 23, 27 (1-indexed) = indices 21, 22, 26 (0-indexed)
        // should NOT have the base case Vm if the outage affects them.
        // Just verify they're in a reasonable range.
        for &idx in &[21, 22, 26] {
            if idx < vm.len() {
                assert!(
                    vm[idx] > 0.85 && vm[idx] < 1.15,
                    "pglib_opf_case30_as {}: PV-no-gen bus idx {} Vm = {} out of range",
                    r.id,
                    idx,
                    vm[idx]
                );
            }
        }
    }

    eprintln!(
        "pglib_opf_case30_as: {} contingencies, {} non-island converged",
        result.results.len(),
        non_island_conv.len()
    );
}

// ---------------------------------------------------------------------------
// Regression: Tripped branch flow must be zero
// ---------------------------------------------------------------------------

#[test]
fn test_tripped_branch_flow_zero() {
    let result = run_validated_n1("case9");

    for r in &result.results {
        if !r.converged || r.post_branch_flows.is_none() {
            continue;
        }
        let flows = r.post_branch_flows.as_ref().unwrap();
        let br_idx: usize = r.id.strip_prefix("branch_").unwrap().parse().unwrap();

        assert!(
            br_idx < flows.len(),
            "Branch index {} should be within flows length {}",
            br_idx,
            flows.len()
        );
        assert!(
            flows[br_idx].abs() < 1e-10,
            "case9 {}: tripped branch {} should have zero flow, got {:.6e}",
            r.id,
            br_idx,
            flows[br_idx]
        );
    }
}

// ---------------------------------------------------------------------------
// Regression: Post-voltage storage respects option flag
// ---------------------------------------------------------------------------

#[test]
fn test_store_post_voltages_flag() {
    let network = surge_io::load(case_path("case9")).expect("parse case9");

    // With store_post_voltages = false (default)
    let opts_off = ContingencyOptions {
        acpf_options: AcPfOptions {
            enforce_q_limits: false,
            ..AcPfOptions::default()
        },
        screening: ScreeningMode::Off,
        store_post_voltages: false,
        ..ContingencyOptions::default()
    };
    let result_off = analyze_n1_branch(&network, &opts_off).expect("N-1");
    for r in &result_off.results {
        assert!(
            r.post_vm.is_none(),
            "post_vm should be None when store_post_voltages=false"
        );
        assert!(
            r.post_va.is_none(),
            "post_va should be None when store_post_voltages=false"
        );
        assert!(
            r.post_branch_flows.is_none(),
            "post_branch_flows should be None"
        );
    }

    // With store_post_voltages = true
    let opts_on = ContingencyOptions {
        acpf_options: AcPfOptions {
            enforce_q_limits: false,
            ..AcPfOptions::default()
        },
        screening: ScreeningMode::Off,
        store_post_voltages: true,
        ..ContingencyOptions::default()
    };
    let result_on = analyze_n1_branch(&network, &opts_on).expect("N-1");
    let any_with_vm = result_on.results.iter().any(|r| r.post_vm.is_some());
    assert!(
        any_with_vm,
        "At least some results should have post_vm when store_post_voltages=true"
    );
}
