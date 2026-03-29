// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Stress test: MATPOWER and JSON round-trip for all 206 .m test cases.
//!
//! For each case file:
//! a) MATPOWER round-trip: .m -> write .m -> read back -> compare
//! b) MATPOWER->JSON->MATPOWER: .m -> write .json -> read .json -> compare
//! c) JSON round-trip: .m -> .json -> read -> .json2 -> read -> compare
//! d) Power flow comparison (cases <= 5000 buses): NR on original vs round-tripped
//!
//! Collects ALL failures and prints a summary table at the end.

use std::path::PathBuf;
use std::sync::Mutex;

use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
use surge_io::{load, save};
use surge_network::Network;
use surge_solution::SolveStatus;
use tempfile::TempDir;

/// A single failure record.
#[derive(Debug)]
struct Failure {
    case: String,
    phase: String,
    detail: String,
}

/// Metrics we compare across round-trips.
#[derive(Debug, Clone)]
struct CaseMetrics {
    n_buses: usize,
    n_branches: usize,
    n_gens: usize,
    total_load_mw: f64,
    total_gen_mw: f64,
    base_mva: f64,
}

impl CaseMetrics {
    fn from_network(net: &Network) -> Self {
        let total_load_mw: f64 = net.total_load_mw();
        let total_gen_mw: f64 = net
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.p)
            .sum();
        Self {
            n_buses: net.n_buses(),
            n_branches: net.n_branches(),
            n_gens: net.generators.len(),
            total_load_mw,
            total_gen_mw,
            base_mva: net.base_mva,
        }
    }
}

/// Compare two CaseMetrics, return list of mismatches.
fn compare_metrics(original: &CaseMetrics, roundtrip: &CaseMetrics, label: &str) -> Vec<String> {
    let mut mismatches = Vec::new();

    if original.n_buses != roundtrip.n_buses {
        mismatches.push(format!(
            "[{label}] bus count: {} vs {}",
            original.n_buses, roundtrip.n_buses
        ));
    }
    if original.n_branches != roundtrip.n_branches {
        mismatches.push(format!(
            "[{label}] branch count: {} vs {}",
            original.n_branches, roundtrip.n_branches
        ));
    }
    if original.n_gens != roundtrip.n_gens {
        mismatches.push(format!(
            "[{label}] gen count: {} vs {}",
            original.n_gens, roundtrip.n_gens
        ));
    }
    let load_err = (original.total_load_mw - roundtrip.total_load_mw).abs();
    if load_err > 0.1 {
        mismatches.push(format!(
            "[{label}] total load MW: {:.4} vs {:.4} (err={:.4})",
            original.total_load_mw, roundtrip.total_load_mw, load_err
        ));
    }
    let gen_err = (original.total_gen_mw - roundtrip.total_gen_mw).abs();
    if gen_err > 0.1 {
        mismatches.push(format!(
            "[{label}] total gen MW: {:.4} vs {:.4} (err={:.4})",
            original.total_gen_mw, roundtrip.total_gen_mw, gen_err
        ));
    }
    if (original.base_mva - roundtrip.base_mva).abs() > 1e-6 {
        mismatches.push(format!(
            "[{label}] base_mva: {} vs {}",
            original.base_mva, roundtrip.base_mva
        ));
    }

    mismatches
}

/// Run NR power flow, return (vm, va) vectors or error string.
fn run_power_flow(net: &Network) -> Result<(Vec<f64>, Vec<f64>), String> {
    let opts = AcPfOptions {
        flat_start: true,
        ..AcPfOptions::default()
    };
    match solve_ac_pf_kernel(net, &opts) {
        Ok(sol) if sol.status == SolveStatus::Converged => {
            Ok((sol.voltage_magnitude_pu, sol.voltage_angle_rad))
        }
        Ok(sol) => Err(format!(
            "NR did not converge: {:?} ({} iters, mismatch={:.2e})",
            sol.status, sol.iterations, sol.max_mismatch
        )),
        Err(e) => Err(format!("NR error: {e}")),
    }
}

/// Compare power flow results, return list of mismatches.
fn compare_pf(
    vm_orig: &[f64],
    va_orig: &[f64],
    vm_rt: &[f64],
    va_rt: &[f64],
    label: &str,
) -> Vec<String> {
    let mut mismatches = Vec::new();

    if vm_orig.len() != vm_rt.len() {
        mismatches.push(format!(
            "[{label}] PF vm length: {} vs {}",
            vm_orig.len(),
            vm_rt.len()
        ));
        return mismatches;
    }

    let vm_max_err = vm_orig
        .iter()
        .zip(vm_rt.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f64, f64::max);

    let va_max_err = va_orig
        .iter()
        .zip(va_rt.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f64, f64::max);

    if vm_max_err > 1e-4 {
        mismatches.push(format!(
            "[{label}] PF max|Vm| error: {vm_max_err:.6e} (limit 1e-4)"
        ));
    }
    if va_max_err > 1e-3 {
        mismatches.push(format!(
            "[{label}] PF max|Va| error: {va_max_err:.6e} (limit 1e-3)"
        ));
    }

    mismatches
}

#[test]
#[ignore] // 206 cases × 3 phases + PF = ~8 min; run with `cargo test -p surge-io --test stress_matpower_json -- --ignored`
fn stress_test_matpower_json_roundtrips() {
    // Find all .m files in tests/data/
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let data_dir = PathBuf::from(&manifest_dir).join("../../tests/data");
    let data_dir = std::fs::canonicalize(&data_dir).expect("cannot canonicalize tests/data");

    let mut m_files: Vec<PathBuf> = std::fs::read_dir(&data_dir)
        .expect("cannot read tests/data")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "m").unwrap_or(false))
        .collect();
    m_files.sort();

    eprintln!("\n=== MATPOWER/JSON Round-Trip Stress Test ===");
    eprintln!(
        "Found {} .m files in {}\n",
        m_files.len(),
        data_dir.display()
    );

    let failures: Mutex<Vec<Failure>> = Mutex::new(Vec::new());
    let mut total_cases = 0;
    let mut parse_errors = 0;
    let mut pass_count = 0;
    let mut pf_tested = 0;
    let mut pf_orig_fail = 0;
    let mut pf_skipped_large = 0;

    let tmpdir = TempDir::new().expect("cannot create temp dir");

    for m_file in &m_files {
        let case_name = m_file.file_stem().unwrap().to_string_lossy().to_string();

        total_cases += 1;

        // --- Parse original ---
        let original = match load(m_file) {
            Ok(net) => net,
            Err(e) => {
                // Some files might legitimately fail to parse (e.g., OPF-only formats).
                // Track but don't count as a round-trip failure.
                parse_errors += 1;
                eprintln!("  SKIP {case_name}: parse error: {e}");
                continue;
            }
        };

        let orig_metrics = CaseMetrics::from_network(&original);
        let mut case_failures: Vec<Failure> = Vec::new();

        // === Phase A: MATPOWER round-trip ===
        let m_out = tmpdir.path().join(format!("{case_name}_rt.m"));
        match save(&original, &m_out) {
            Ok(()) => match load(&m_out) {
                Ok(rt_net) => {
                    let rt_metrics = CaseMetrics::from_network(&rt_net);
                    for msg in compare_metrics(&orig_metrics, &rt_metrics, "M->M") {
                        case_failures.push(Failure {
                            case: case_name.clone(),
                            phase: "A: MATPOWER round-trip".into(),
                            detail: msg,
                        });
                    }
                }
                Err(e) => {
                    case_failures.push(Failure {
                        case: case_name.clone(),
                        phase: "A: MATPOWER round-trip".into(),
                        detail: format!("re-parse failed: {e}"),
                    });
                }
            },
            Err(e) => {
                case_failures.push(Failure {
                    case: case_name.clone(),
                    phase: "A: MATPOWER round-trip".into(),
                    detail: format!("write failed: {e}"),
                });
            }
        }

        // === Phase B: MATPOWER -> JSON -> compare ===
        let json_out = tmpdir.path().join(format!("{case_name}_rt.json"));
        match save(&original, &json_out) {
            Ok(()) => match load(&json_out) {
                Ok(json_net) => {
                    let json_metrics = CaseMetrics::from_network(&json_net);
                    for msg in compare_metrics(&orig_metrics, &json_metrics, "M->JSON") {
                        case_failures.push(Failure {
                            case: case_name.clone(),
                            phase: "B: MATPOWER->JSON".into(),
                            detail: msg,
                        });
                    }
                }
                Err(e) => {
                    case_failures.push(Failure {
                        case: case_name.clone(),
                        phase: "B: MATPOWER->JSON".into(),
                        detail: format!("JSON re-parse failed: {e}"),
                    });
                }
            },
            Err(e) => {
                case_failures.push(Failure {
                    case: case_name.clone(),
                    phase: "B: MATPOWER->JSON".into(),
                    detail: format!("JSON write failed: {e}"),
                });
            }
        }

        // === Phase C: JSON round-trip (.json -> read -> .json2 -> read -> compare) ===
        let json_out2 = tmpdir.path().join(format!("{case_name}_rt2.json"));
        // We already wrote json_out in phase B; now read it, write again, read again
        let json_rt_result = (|| -> Result<CaseMetrics, String> {
            let net1 = load(&json_out).map_err(|e| format!("json read1 failed: {e}"))?;
            save(&net1, &json_out2).map_err(|e| format!("json write2 failed: {e}"))?;
            let net2 = load(&json_out2).map_err(|e| format!("json read2 failed: {e}"))?;
            Ok(CaseMetrics::from_network(&net2))
        })();

        match json_rt_result {
            Ok(json2_metrics) => {
                for msg in compare_metrics(&orig_metrics, &json2_metrics, "JSON->JSON") {
                    case_failures.push(Failure {
                        case: case_name.clone(),
                        phase: "C: JSON round-trip".into(),
                        detail: msg,
                    });
                }
            }
            Err(e) => {
                case_failures.push(Failure {
                    case: case_name.clone(),
                    phase: "C: JSON round-trip".into(),
                    detail: e,
                });
            }
        }

        // === Phase D: Power flow comparison (cases <= 5000 buses) ===
        if orig_metrics.n_buses > 5000 {
            pf_skipped_large += 1;
        }
        if orig_metrics.n_buses <= 5000 && orig_metrics.n_buses > 0 {
            // Run PF on original
            match run_power_flow(&original) {
                Ok((vm_orig, va_orig)) => {
                    pf_tested += 1;
                    // D1: PF on MATPOWER round-tripped network
                    let m_rt_path = tmpdir.path().join(format!("{case_name}_rt.m"));
                    if let Ok(m_rt_net) = load(&m_rt_path) {
                        match run_power_flow(&m_rt_net) {
                            Ok((vm_rt, va_rt)) => {
                                for msg in compare_pf(&vm_orig, &va_orig, &vm_rt, &va_rt, "PF:M->M")
                                {
                                    case_failures.push(Failure {
                                        case: case_name.clone(),
                                        phase: "D: PF MATPOWER RT".into(),
                                        detail: msg,
                                    });
                                }
                            }
                            Err(e) => {
                                case_failures.push(Failure {
                                    case: case_name.clone(),
                                    phase: "D: PF MATPOWER RT".into(),
                                    detail: format!("NR on RT network failed: {e}"),
                                });
                            }
                        }
                    }

                    // D2: PF on JSON round-tripped network
                    if let Ok(json_net) = load(&json_out) {
                        match run_power_flow(&json_net) {
                            Ok((vm_json, va_json)) => {
                                for msg in
                                    compare_pf(&vm_orig, &va_orig, &vm_json, &va_json, "PF:M->JSON")
                                {
                                    case_failures.push(Failure {
                                        case: case_name.clone(),
                                        phase: "D: PF JSON RT".into(),
                                        detail: msg,
                                    });
                                }
                            }
                            Err(e) => {
                                case_failures.push(Failure {
                                    case: case_name.clone(),
                                    phase: "D: PF JSON RT".into(),
                                    detail: format!("NR on JSON network failed: {e}"),
                                });
                            }
                        }
                    }
                }
                Err(_) => {
                    // Original doesn't converge — skip PF comparison (not a round-trip bug)
                    pf_orig_fail += 1;
                }
            }
        }

        // === Phase E: base_mva consistency (already checked in metrics comparison) ===
        // The base_mva check is included in compare_metrics. Nothing extra needed.

        if case_failures.is_empty() {
            pass_count += 1;
        } else {
            eprintln!("  FAIL {case_name}: {} issue(s)", case_failures.len());
            for f in &case_failures {
                eprintln!("       - [{}] {}", f.phase, f.detail);
            }
            failures.lock().unwrap().extend(case_failures);
        }
    }

    // === Summary ===
    let all_failures = failures.into_inner().unwrap();

    eprintln!("\n{}", "=".repeat(60));
    eprintln!("=== STRESS TEST SUMMARY ===");
    eprintln!("{}", "=".repeat(60));
    eprintln!("Total .m files:         {total_cases}");
    eprintln!("Parse errors (skip):    {parse_errors}");
    eprintln!("Cases tested (A/B/C):   {}", total_cases - parse_errors);
    eprintln!("PASS:                   {pass_count}");
    eprintln!(
        "FAIL:                   {}",
        total_cases - parse_errors - pass_count
    );
    eprintln!("Total failure items:    {}", all_failures.len());
    eprintln!();
    eprintln!("--- Power Flow Phase D ---");
    eprintln!("Skipped (>5000 buses):  {pf_skipped_large}");
    eprintln!("Original NR failed:     {pf_orig_fail}");
    eprintln!("PF compared (both OK):  {pf_tested}");
    eprintln!();

    if !all_failures.is_empty() {
        eprintln!("=== FAILURE DETAILS ===");
        eprintln!("{:<30} {:<25} Detail", "Case", "Phase");
        eprintln!("{:-<100}", "");
        for f in &all_failures {
            eprintln!("{:<30} {:<25} {}", f.case, f.phase, f.detail);
        }
        eprintln!();

        // Group failures by phase
        let mut by_phase: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for f in &all_failures {
            *by_phase.entry(f.phase.clone()).or_insert(0) += 1;
        }
        eprintln!("=== FAILURES BY PHASE ===");
        for (phase, count) in &by_phase {
            eprintln!("  {phase}: {count}");
        }
        eprintln!();

        // Group failures by case
        let mut by_case: std::collections::BTreeMap<String, Vec<&Failure>> =
            std::collections::BTreeMap::new();
        for f in &all_failures {
            by_case.entry(f.case.clone()).or_default().push(f);
        }
        eprintln!("=== FAILURES BY CASE ===");
        for (case, fails) in &by_case {
            eprintln!("  {case}: {} failure(s)", fails.len());
            for f in fails {
                eprintln!("    - [{}] {}", f.phase, f.detail);
            }
        }
    }

    // Assert: test fails if ANY round-trip failures were found
    assert!(
        all_failures.is_empty(),
        "\n{} round-trip failure(s) found across {} cases — see details above",
        all_failures.len(),
        total_cases - parse_errors - pass_count,
    );
}
