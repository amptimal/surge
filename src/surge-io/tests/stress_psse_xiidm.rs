// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Stress test for PSS/E RAW and XIIDM readers/writers.
//!
//! Performs exhaustive round-trip testing across all test case files:
//! - MATPOWER -> PSS/E -> MATPOWER
//! - MATPOWER -> XIIDM -> MATPOWER
//! - PSS/E -> XIIDM (and back)
//! - XIIDM -> PSS/E -> XIIDM
//! - Power flow comparison on small cases
//! - PSS/E round-trip for native .raw files
//!
//! Run with: cargo test -p surge-io --test stress_psse_xiidm -- --nocapture

use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use surge_ac::{AcPfOptions, solve_ac_pf};
use surge_network::Network;
use surge_solution::SolveStatus;

// ---------------------------------------------------------------------------
// Failure tracking
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Failure {
    case: String,
    pipeline: String,
    detail: String,
}

struct TestRunner {
    failures: Mutex<Vec<Failure>>,
    passes: Mutex<u32>,
}

impl TestRunner {
    fn new() -> Self {
        TestRunner {
            failures: Mutex::new(Vec::new()),
            passes: Mutex::new(0),
        }
    }

    fn fail(&self, case: &str, pipeline: &str, detail: String) {
        eprintln!("  FAIL  [{case}] {pipeline}: {detail}");
        self.failures.lock().unwrap().push(Failure {
            case: case.to_string(),
            pipeline: pipeline.to_string(),
            detail,
        });
    }

    fn pass(&self) {
        *self.passes.lock().unwrap() += 1;
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_data_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    PathBuf::from(manifest).join("../../tests/data")
}

fn total_load_mw(net: &Network) -> f64 {
    net.total_load_mw()
}

/// Max bus count for I/O round-trip tests (writing/reading large files is slow).
const MAX_IO_BUSES: usize = 15000;

/// Max bus count for power flow comparison tests.
const MAX_PF_BUSES: usize = 5000;

fn compare_structural(
    runner: &TestRunner,
    case: &str,
    pipeline: &str,
    original: &Network,
    converted: &Network,
) {
    let mut ok = true;

    if original.n_buses() != converted.n_buses() {
        runner.fail(
            case,
            pipeline,
            format!(
                "bus count: {} vs {}",
                original.n_buses(),
                converted.n_buses()
            ),
        );
        ok = false;
    }

    if original.n_branches() != converted.n_branches() {
        runner.fail(
            case,
            pipeline,
            format!(
                "branch count: {} vs {}",
                original.n_branches(),
                converted.n_branches()
            ),
        );
        ok = false;
    }

    if original.generators.len() != converted.generators.len() {
        runner.fail(
            case,
            pipeline,
            format!(
                "gen count: {} vs {}",
                original.generators.len(),
                converted.generators.len()
            ),
        );
        ok = false;
    }

    let load_orig = total_load_mw(original);
    let load_conv = total_load_mw(converted);
    let load_diff = (load_orig - load_conv).abs();
    if load_diff > 1.0 {
        runner.fail(
            case,
            pipeline,
            format!(
                "total load MW: {:.2} vs {:.2} (diff={:.4})",
                load_orig, load_conv, load_diff
            ),
        );
        ok = false;
    }

    if ok {
        runner.pass();
    }
}

/// Run NR power flow and return (vm, va) vectors, or None if it doesn't converge.
fn run_pf(net: &Network) -> Option<(Vec<f64>, Vec<f64>)> {
    let opts = AcPfOptions {
        max_iterations: 50,
        flat_start: true,
        ..AcPfOptions::default()
    };
    match solve_ac_pf(net, &opts) {
        Ok(sol) if sol.status == SolveStatus::Converged => {
            Some((sol.voltage_magnitude_pu, sol.voltage_angle_rad))
        }
        _ => None,
    }
}

fn compare_pf(
    runner: &TestRunner,
    case: &str,
    pipeline: &str,
    orig_vm: &[f64],
    orig_va: &[f64],
    conv_vm: &[f64],
    conv_va: &[f64],
) {
    if orig_vm.len() != conv_vm.len() {
        runner.fail(
            case,
            &format!("{pipeline} PF"),
            format!(
                "Vm vec length mismatch: {} vs {}",
                orig_vm.len(),
                conv_vm.len()
            ),
        );
        return;
    }

    let vm_max_err = orig_vm
        .iter()
        .zip(conv_vm.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f64, f64::max);

    let va_max_err = orig_va
        .iter()
        .zip(conv_va.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f64, f64::max);

    let mut ok = true;
    if vm_max_err > 1e-3 {
        runner.fail(
            case,
            &format!("{pipeline} PF"),
            format!("max|Vm| error = {vm_max_err:.6} pu (limit 1e-3)"),
        );
        ok = false;
    }
    if va_max_err > 1e-2 {
        runner.fail(
            case,
            &format!("{pipeline} PF"),
            format!("max|Va| error = {va_max_err:.6} rad (limit 1e-2)"),
        );
        ok = false;
    }
    if ok {
        runner.pass();
    }
}

fn write_and_read_psse(net: &Network, tmp_path: &Path) -> anyhow::Result<Network> {
    surge_io::psse::raw::save(net, tmp_path, 33)?;
    let read_back = surge_io::psse::raw::load(tmp_path)?;
    Ok(read_back)
}

fn write_and_read_xiidm(net: &Network, tmp_path: &Path) -> anyhow::Result<Network> {
    surge_io::xiidm::save(net, tmp_path)?;
    let read_back = surge_io::xiidm::load(tmp_path)?;
    Ok(read_back)
}

fn write_and_read_matpower(net: &Network, tmp_path: &Path) -> anyhow::Result<Network> {
    surge_io::matpower::save(net, tmp_path)?;
    let read_back = surge_io::matpower::load(tmp_path)?;
    Ok(read_back)
}

// ---------------------------------------------------------------------------
// Main stress test
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Iterates all 206 cases + PF; run with `cargo test -- --ignored`
fn stress_test_psse_xiidm_roundtrips() {
    let data_dir = test_data_dir();
    let runner = TestRunner::new();
    let tmpdir = tempfile::TempDir::new().expect("failed to create temp dir");

    // Collect all .m files, skipping __api variants (redundant pglib variants)
    let mut m_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&data_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "m").unwrap_or(false) {
                let name = p.file_stem().unwrap().to_string_lossy();
                if name.contains("__api") {
                    continue; // skip redundant pglib API variants
                }
                m_files.push(p);
            }
        }
    }
    m_files.sort();

    // Collect all .raw files (top-level and in raw/ subdirectory)
    let mut raw_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&data_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "raw").unwrap_or(false) {
                raw_files.push(p);
            }
        }
    }
    let raw_subdir = data_dir.join("raw");
    if let Ok(entries) = std::fs::read_dir(&raw_subdir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "raw").unwrap_or(false) {
                raw_files.push(p);
            }
        }
    }
    raw_files.sort();

    eprintln!(
        "\n=== STRESS TEST: {} .m files, {} .raw files ===\n",
        m_files.len(),
        raw_files.len()
    );

    // --- Phase 1: MATPOWER files round-trip through PSS/E and XIIDM ---

    for (idx, m_path) in m_files.iter().enumerate() {
        let case_name = m_path.file_stem().unwrap().to_string_lossy().to_string();

        // Parse original MATPOWER
        let original = match surge_io::matpower::load(m_path) {
            Ok(net) => net,
            Err(e) => {
                runner.fail(&case_name, "MATPOWER parse", format!("{e}"));
                continue;
            }
        };

        let n_buses = original.n_buses();

        // Skip very large cases for I/O (too slow in debug)
        if n_buses > MAX_IO_BUSES {
            if idx < 10 || idx % 20 == 0 {
                eprintln!(
                    "[{}/{}] {case_name} ({n_buses} buses) SKIP (>{MAX_IO_BUSES})",
                    idx + 1,
                    m_files.len(),
                );
            }
            continue;
        }

        let do_pf = n_buses <= MAX_PF_BUSES;

        if idx < 10 || idx % 20 == 0 {
            eprintln!(
                "[{}/{}] {case_name} ({} buses, {} branches, {} gens, load={:.1} MW){}",
                idx + 1,
                m_files.len(),
                n_buses,
                original.n_branches(),
                original.generators.len(),
                total_load_mw(&original),
                if do_pf { " +PF" } else { "" }
            );
        }

        // Run original power flow (for comparison later)
        let orig_pf = if do_pf { run_pf(&original) } else { None };

        // (a) MATPOWER -> PSS/E -> MATPOWER
        {
            let tmp_raw = tmpdir.path().join(format!("{case_name}_m2psse.raw"));
            match write_and_read_psse(&original, &tmp_raw) {
                Ok(rt) => {
                    compare_structural(&runner, &case_name, "M->PSS/E->read", &original, &rt);
                    if let Some((ref orig_vm, ref orig_va)) = orig_pf {
                        match run_pf(&rt) {
                            Some((conv_vm, conv_va)) => {
                                compare_pf(
                                    &runner, &case_name, "M->PSS/E", orig_vm, orig_va, &conv_vm,
                                    &conv_va,
                                );
                            }
                            None => {
                                runner.fail(
                                    &case_name,
                                    "M->PSS/E PF",
                                    "NR did not converge on PSS/E round-tripped network"
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    runner.fail(&case_name, "M->PSS/E write/read", format!("{e}"));
                }
            }
        }

        // (b) MATPOWER -> XIIDM -> MATPOWER
        {
            let tmp_xiidm = tmpdir.path().join(format!("{case_name}_m2xiidm.xiidm"));
            match write_and_read_xiidm(&original, &tmp_xiidm) {
                Ok(rt) => {
                    compare_structural(&runner, &case_name, "M->XIIDM->read", &original, &rt);
                    if let Some((ref orig_vm, ref orig_va)) = orig_pf {
                        match run_pf(&rt) {
                            Some((conv_vm, conv_va)) => {
                                compare_pf(
                                    &runner, &case_name, "M->XIIDM", orig_vm, orig_va, &conv_vm,
                                    &conv_va,
                                );
                            }
                            None => {
                                runner.fail(
                                    &case_name,
                                    "M->XIIDM PF",
                                    "NR did not converge on XIIDM round-tripped network"
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    runner.fail(&case_name, "M->XIIDM write/read", format!("{e}"));
                }
            }
        }

        // (d) XIIDM -> PSS/E chain: M -> XIIDM -> read -> write PSS/E -> read PSS/E -> compare
        {
            let tmp_xiidm = tmpdir.path().join(format!("{case_name}_chain_x.xiidm"));
            let tmp_raw = tmpdir.path().join(format!("{case_name}_chain_r.raw"));

            let chain_result: anyhow::Result<Network> = (|| {
                surge_io::xiidm::save(&original, &tmp_xiidm)?;
                let from_xiidm = surge_io::xiidm::load(&tmp_xiidm)?;
                surge_io::psse::raw::save(&from_xiidm, &tmp_raw, 33)?;
                let from_psse = surge_io::psse::raw::load(&tmp_raw)?;
                Ok(from_psse)
            })();

            match chain_result {
                Ok(rt) => {
                    compare_structural(
                        &runner,
                        &case_name,
                        "M->XIIDM->PSS/E->read",
                        &original,
                        &rt,
                    );
                }
                Err(e) => {
                    runner.fail(&case_name, "M->XIIDM->PSS/E chain", format!("{e}"));
                }
            }
        }
    }

    // --- Phase 2: Native .raw files ---

    eprintln!(
        "\n--- Phase 2: Native .raw file round-trips ({} files) ---\n",
        raw_files.len()
    );

    for raw_path in &raw_files {
        let case_name = raw_path.file_stem().unwrap().to_string_lossy().to_string();

        // Parse original PSS/E
        let original = match surge_io::psse::raw::load(raw_path) {
            Ok(net) => net,
            Err(e) => {
                runner.fail(&case_name, "PSS/E parse", format!("{e}"));
                continue;
            }
        };

        let n_buses = original.n_buses();

        eprintln!(
            "  {case_name}: {n_buses} buses, {} branches, {} gens, load={:.1} MW",
            original.n_branches(),
            original.generators.len(),
            total_load_mw(&original),
        );

        // Skip very large cases
        if n_buses > MAX_IO_BUSES {
            eprintln!("    SKIP (>{MAX_IO_BUSES} buses)");
            continue;
        }

        let do_pf = n_buses <= MAX_PF_BUSES;
        let orig_pf = if do_pf { run_pf(&original) } else { None };

        // (a) PSS/E round-trip: .raw -> write .raw -> read .raw
        {
            let tmp_raw = tmpdir.path().join(format!("{case_name}_rt.raw"));
            match write_and_read_psse(&original, &tmp_raw) {
                Ok(rt) => {
                    compare_structural(&runner, &case_name, "RAW->write RAW->read", &original, &rt);
                    if let Some((ref orig_vm, ref orig_va)) = orig_pf {
                        match run_pf(&rt) {
                            Some((conv_vm, conv_va)) => {
                                compare_pf(
                                    &runner, &case_name, "RAW->RAW", orig_vm, orig_va, &conv_vm,
                                    &conv_va,
                                );
                            }
                            None => {
                                runner.fail(
                                    &case_name,
                                    "RAW->RAW PF",
                                    "NR did not converge on round-tripped network".to_string(),
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    runner.fail(&case_name, "RAW->write RAW", format!("{e}"));
                }
            }
        }

        // (b) PSS/E -> XIIDM -> read XIIDM -> compare
        {
            let tmp_xiidm = tmpdir.path().join(format!("{case_name}_r2x.xiidm"));
            match write_and_read_xiidm(&original, &tmp_xiidm) {
                Ok(rt) => {
                    compare_structural(&runner, &case_name, "RAW->XIIDM->read", &original, &rt);
                    if let Some((ref orig_vm, ref orig_va)) = orig_pf {
                        match run_pf(&rt) {
                            Some((conv_vm, conv_va)) => {
                                compare_pf(
                                    &runner,
                                    &case_name,
                                    "RAW->XIIDM",
                                    orig_vm,
                                    orig_va,
                                    &conv_vm,
                                    &conv_va,
                                );
                            }
                            None => {
                                runner.fail(
                                    &case_name,
                                    "RAW->XIIDM PF",
                                    "NR did not converge on round-tripped network".to_string(),
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    runner.fail(&case_name, "RAW->XIIDM write/read", format!("{e}"));
                }
            }
        }

        // (c) Cross-convert: RAW -> MATPOWER -> read -> compare
        {
            let tmp_m = tmpdir.path().join(format!("{case_name}_r2m.m"));
            match write_and_read_matpower(&original, &tmp_m) {
                Ok(rt) => {
                    compare_structural(&runner, &case_name, "RAW->MATPOWER->read", &original, &rt);
                }
                Err(e) => {
                    runner.fail(&case_name, "RAW->MATPOWER write/read", format!("{e}"));
                }
            }
        }

        // (d) Cross-convert chain: RAW -> XIIDM -> PSS/E -> read
        {
            let tmp_xiidm = tmpdir.path().join(format!("{case_name}_chain_x2.xiidm"));
            let tmp_raw2 = tmpdir.path().join(format!("{case_name}_chain_r2.raw"));

            let chain_result: anyhow::Result<Network> = (|| {
                surge_io::xiidm::save(&original, &tmp_xiidm)?;
                let from_xiidm = surge_io::xiidm::load(&tmp_xiidm)?;
                surge_io::psse::raw::save(&from_xiidm, &tmp_raw2, 33)?;
                let from_psse = surge_io::psse::raw::load(&tmp_raw2)?;
                Ok(from_psse)
            })();

            match chain_result {
                Ok(rt) => {
                    compare_structural(
                        &runner,
                        &case_name,
                        "RAW->XIIDM->PSS/E->read",
                        &original,
                        &rt,
                    );
                }
                Err(e) => {
                    runner.fail(&case_name, "RAW->XIIDM->PSS/E chain", format!("{e}"));
                }
            }
        }

        // (e) Cross-convert chain: RAW -> MATPOWER -> XIIDM -> read
        {
            let tmp_m = tmpdir.path().join(format!("{case_name}_chain_m.m"));
            let tmp_xiidm = tmpdir.path().join(format!("{case_name}_chain_mx.xiidm"));

            let chain_result: anyhow::Result<Network> = (|| {
                surge_io::matpower::save(&original, &tmp_m)?;
                let from_m = surge_io::matpower::load(&tmp_m)?;
                surge_io::xiidm::save(&from_m, &tmp_xiidm)?;
                let from_xiidm = surge_io::xiidm::load(&tmp_xiidm)?;
                Ok(from_xiidm)
            })();

            match chain_result {
                Ok(rt) => {
                    compare_structural(&runner, &case_name, "RAW->M->XIIDM->read", &original, &rt);
                }
                Err(e) => {
                    runner.fail(&case_name, "RAW->M->XIIDM chain", format!("{e}"));
                }
            }
        }
    }

    // --- Summary ---

    let failures = runner.failures.lock().unwrap();
    let pass_count = *runner.passes.lock().unwrap();
    let fail_count = failures.len();

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("STRESS TEST SUMMARY");
    eprintln!("{}", "=".repeat(80));
    eprintln!("  PASS: {}  |  FAIL: {}", pass_count, fail_count);

    if !failures.is_empty() {
        eprintln!("\n--- ALL FAILURES ---\n");

        // Build a summary table
        let mut table = String::new();
        writeln!(table, "{:<40} {:<30} DETAIL", "CASE", "PIPELINE").unwrap();
        writeln!(table, "{:-<40} {:-<30} {:-<60}", "", "", "").unwrap();

        for f in failures.iter() {
            writeln!(table, "{:<40} {:<30} {}", f.case, f.pipeline, f.detail).unwrap();
        }
        eprintln!("{table}");
    }

    eprintln!("{}\n", "=".repeat(80));

    // Assert no failures
    assert!(
        failures.is_empty(),
        "{fail_count} failure(s) detected. See summary above."
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Heuristic: extract approximate bus count from case name for size filtering.
fn extract_bus_count_from_name(name: &str) -> usize {
    // Extract ALL digit sequences from the name, return the largest one.
    // Handles: case118, case2383wp, case_ACTIVSg70k, pglib_opf_case10000_goc
    let mut max_num = 0usize;
    let lower = name.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let num_str: String = chars[start..i].iter().collect();
            if let Ok(n) = num_str.parse::<usize>() {
                let val = if i < chars.len() && chars[i] == 'k' {
                    n * 1000
                } else {
                    n
                };
                if val > max_num {
                    max_num = val;
                }
            }
        } else {
            i += 1;
        }
    }
    if max_num == 0 { 100 } else { max_num }
}

#[cfg(test)]
mod bus_count_tests {
    use super::extract_bus_count_from_name;

    #[test]
    fn test_extract_bus_count() {
        assert_eq!(extract_bus_count_from_name("case9"), 9);
        assert_eq!(extract_bus_count_from_name("case118"), 118);
        assert_eq!(extract_bus_count_from_name("case2383wp"), 2383);
        assert_eq!(extract_bus_count_from_name("case_ACTIVSg70k"), 70000);
        assert_eq!(extract_bus_count_from_name("pglib_opf_case118_ieee"), 118);
        assert_eq!(
            extract_bus_count_from_name("pglib_opf_case10000_goc"),
            10000
        );
    }
}
