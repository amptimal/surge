// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! P1-019: AC-OPF regression tests validated against MATPOWER MIPS reference values.
//!
//! These tests run Surge's AC-OPF on standard IEEE test cases and assert that the
//! objective value (total generation cost $/hr) is within tolerance of known MATPOWER
//! 8.x MIPS results.  Surge uses a different NLP method (Ipopt interior-point) than
//! MIPS, so exact agreement is not expected; however, order-of-magnitude agreement
//! and convergence are required.
//!
//! MATPOWER reference objectives (MIPS solver, default settings):
//!   - case14:  ~$8,081.53
//!   - case30:  ~$576.89
//!   - case118: ~$129,660.69
//!
//! Tolerance: 5% relative difference.
//!
//! NOTE: Ipopt/MUMPS is not thread-safe. These tests must run with --test-threads=1
//! or be serialized externally. Since each integration test file compiles to its own
//! binary and tests within a binary default to parallel execution, we run these
//! sequentially by design (only 3 tests, fast enough).

use std::sync::Mutex;

use surge_opf::{AcOpfOptions, solve_ac_opf};

fn format_optional_iterations(iterations: Option<u32>) -> String {
    iterations
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
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

/// Ipopt's MUMPS linear solver is not thread-safe — serialize all OPF tests.
static IPOPT_MUTEX: Mutex<()> = Mutex::new(());

#[allow(dead_code)]
fn test_data_path(name: &str) -> std::path::PathBuf {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return std::path::PathBuf::from(p).join(name);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/data")
        .join(name)
}

/// Return the path to a local `.surge.json.zst` case file shipped in `examples/cases/`.
fn case_path(stem: &str) -> std::path::PathBuf {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    for dir_name in [stem, &format!("ieee{}", stem.trim_start_matches("case"))] {
        let p = workspace.join(format!("examples/cases/{dir_name}/{stem}.surge.json.zst"));
        if p.exists() {
            return p;
        }
    }
    panic!(
        "case_path({stem:?}): file not found in examples/cases/{stem}/ or examples/cases/ieee{}/",
        stem.trim_start_matches("case")
    );
}

/// Assert that `actual` is within `tolerance` fraction of `expected`.
/// e.g., tolerance = 0.05 means 5% relative error.
fn assert_within_tolerance(actual: f64, expected: f64, tolerance: f64, label: &str) {
    let rel_error = ((actual - expected) / expected).abs();
    assert!(
        rel_error <= tolerance,
        "{label}: objective {actual:.2} differs from MATPOWER reference {expected:.2} \
         by {:.2}% (tolerance = {:.0}%)",
        rel_error * 100.0,
        tolerance * 100.0,
    );
}

// ---------------------------------------------------------------------------
// P1-019 Test 1: case14 AC-OPF vs MATPOWER MIPS reference
// ---------------------------------------------------------------------------
#[test]
fn test_acopf_case14_matpower_validation() {
    let _lock = IPOPT_MUTEX.lock().unwrap();
    let net = surge_io::load(case_path("case14")).unwrap();
    let opts = AcOpfOptions::default();

    let sol = solve_ac_opf(&net, &opts).expect("AC-OPF should converge on case14");

    // Convergence check
    assert!(
        sol.total_cost > 0.0,
        "case14: objective should be positive, got {}",
        sol.total_cost
    );

    // MATPOWER MIPS reference: ~$8,081.53
    let matpower_ref = 8081.53;
    assert_within_tolerance(sol.total_cost, matpower_ref, 0.05, "case14");

    println!(
        "case14 AC-OPF validation: Surge={:.2} $/hr, MATPOWER={:.2} $/hr, \
         rel_err={:.2}%, iters={}, time={:.1} ms",
        sol.total_cost,
        matpower_ref,
        ((sol.total_cost - matpower_ref) / matpower_ref).abs() * 100.0,
        format_optional_iterations(sol.iterations),
        sol.solve_time_secs * 1000.0,
    );
}

// ---------------------------------------------------------------------------
// P1-019 Test 2: case30 AC-OPF vs MATPOWER MIPS reference
// ---------------------------------------------------------------------------
#[test]
fn test_acopf_case30_matpower_validation() {
    let _lock = IPOPT_MUTEX.lock().unwrap();
    let net = surge_io::load(case_path("case30")).unwrap();
    let opts = AcOpfOptions::default();

    let sol = solve_ac_opf(&net, &opts).expect("AC-OPF should converge on case30");

    // Convergence check
    assert!(
        sol.total_cost > 0.0,
        "case30: objective should be positive, got {}",
        sol.total_cost
    );

    // MATPOWER MIPS reference: ~$576.89
    let matpower_ref = 576.89;
    assert_within_tolerance(sol.total_cost, matpower_ref, 0.05, "case30");

    println!(
        "case30 AC-OPF validation: Surge={:.2} $/hr, MATPOWER={:.2} $/hr, \
         rel_err={:.2}%, iters={}, time={:.1} ms",
        sol.total_cost,
        matpower_ref,
        ((sol.total_cost - matpower_ref) / matpower_ref).abs() * 100.0,
        format_optional_iterations(sol.iterations),
        sol.solve_time_secs * 1000.0,
    );
}

// ---------------------------------------------------------------------------
// P1-019 Test 3: case118 AC-OPF vs MATPOWER MIPS reference
// ---------------------------------------------------------------------------
#[test]
fn test_acopf_case118_matpower_validation() {
    let _lock = IPOPT_MUTEX.lock().unwrap();
    let net = surge_io::load(case_path("case118")).unwrap();
    let opts = AcOpfOptions::default();

    let sol = solve_ac_opf(&net, &opts).expect("AC-OPF should converge on case118");

    // Convergence check
    assert!(
        sol.total_cost > 0.0,
        "case118: objective should be positive, got {}",
        sol.total_cost
    );

    // MATPOWER MIPS reference: ~$129,660.69
    let matpower_ref = 129_660.69;
    assert_within_tolerance(sol.total_cost, matpower_ref, 0.05, "case118");

    println!(
        "case118 AC-OPF validation: Surge={:.2} $/hr, MATPOWER={:.2} $/hr, \
         rel_err={:.2}%, iters={}, time={:.1} ms",
        sol.total_cost,
        matpower_ref,
        ((sol.total_cost - matpower_ref) / matpower_ref).abs() * 100.0,
        format_optional_iterations(sol.iterations),
        sol.solve_time_secs * 1000.0,
    );
}
