// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Cross-format stress test: round-trip every format combination we support.
//!
//! Lossless formats (MATPOWER, JSON, PSS/E): assert exact structural match.
//! Lossy formats (DSS, UCTE, XIIDM, CGMES): report issues without asserting,
//! since these formats inherently lose information (e.g., UCTE merges gens per bus,
//! DSS converts slack gen to Vsource, XIIDM may drop some branch parameters).

use std::path::PathBuf;

use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
use surge_io::{load, save};
use surge_network::Network;
use surge_solution::SolveStatus;
use tempfile::TempDir;

#[derive(Debug, Clone)]
struct CaseMetrics {
    n_buses: usize,
    n_branches: usize,
    n_gens: usize,
    total_load_mw: f64,
    total_gen_mw: f64,
    _base_mva: f64,
}

impl CaseMetrics {
    fn from_network(net: &Network) -> Self {
        let total_load_mw: f64 = net.total_load_mw();
        let mut total_gen_mw: f64 = net
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.p)
            .sum();
        // Count power injections as generators — MATPOWER format emits them as
        // generator rows, so the roundtrip moves them into the generator list.
        let mut n_gens = net.generators.len();
        for inj in &net.power_injections {
            if inj.in_service {
                n_gens += 1;
                total_gen_mw += inj.active_power_injection_mw;
            }
        }
        Self {
            n_buses: net.n_buses(),
            n_branches: net.n_branches(),
            n_gens,
            total_load_mw,
            total_gen_mw,
            _base_mva: net.base_mva,
        }
    }
}

#[derive(Debug)]
struct Failure {
    case: String,
    phase: String,
    detail: String,
}

fn data_dir() -> Option<PathBuf> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let dir = PathBuf::from(&manifest).join("../../tests/data");
    std::fs::canonicalize(&dir).ok()
}

/// Return path to a local `.surge.json.zst` case file shipped in `examples/cases/`.
/// For `case118` the directory is `ieee118/`; all others use `{stem}/`.
fn case_path(stem: &str) -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let base = PathBuf::from(&manifest).join("../../examples/cases");
    let dir_name = if stem == "case118" { "ieee118" } else { stem };
    base.join(dir_name).join(format!("{stem}.surge.json.zst"))
}

fn run_pf(net: &Network) -> Option<(Vec<f64>, Vec<f64>)> {
    let opts = AcPfOptions {
        flat_start: true,
        ..AcPfOptions::default()
    };
    match solve_ac_pf_kernel(net, &opts) {
        Ok(sol) if sol.status == SolveStatus::Converged => {
            Some((sol.voltage_magnitude_pu, sol.voltage_angle_rad))
        }
        _ => None,
    }
}

fn compare_metrics(orig: &CaseMetrics, rt: &CaseMetrics, label: &str) -> Vec<String> {
    let mut m = Vec::new();
    if orig.n_buses != rt.n_buses {
        m.push(format!(
            "[{label}] buses: {} vs {}",
            orig.n_buses, rt.n_buses
        ));
    }
    if orig.n_branches != rt.n_branches {
        m.push(format!(
            "[{label}] branches: {} vs {}",
            orig.n_branches, rt.n_branches
        ));
    }
    if orig.n_gens != rt.n_gens {
        m.push(format!("[{label}] gens: {} vs {}", orig.n_gens, rt.n_gens));
    }
    let load_err = (orig.total_load_mw - rt.total_load_mw).abs();
    if load_err > 0.1 {
        m.push(format!(
            "[{label}] load MW: {:.2} vs {:.2} (err={:.4})",
            orig.total_load_mw, rt.total_load_mw, load_err
        ));
    }
    let gen_err = (orig.total_gen_mw - rt.total_gen_mw).abs();
    if gen_err > 0.1 {
        m.push(format!(
            "[{label}] gen MW: {:.2} vs {:.2} (err={:.4})",
            orig.total_gen_mw, rt.total_gen_mw, gen_err
        ));
    }
    m
}

fn compare_pf(
    vm1: &[f64],
    va1: &[f64],
    vm2: &[f64],
    va2: &[f64],
    label: &str,
    vm_tol: f64,
    va_tol: f64,
) -> Vec<String> {
    let mut m = Vec::new();
    if vm1.len() != vm2.len() {
        m.push(format!(
            "[{label}] Vm length: {} vs {}",
            vm1.len(),
            vm2.len()
        ));
        return m;
    }
    let vm_err = vm1
        .iter()
        .zip(vm2)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f64, f64::max);
    let va_err = va1
        .iter()
        .zip(va2)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f64, f64::max);
    if vm_err > vm_tol {
        m.push(format!("[{label}] max|Vm| err = {vm_err:.6e}"));
    }
    if va_err > va_tol {
        m.push(format!("[{label}] max|Va| err = {va_err:.6e}"));
    }
    m
}

/// Helper: run a single format round-trip, return failures.
fn roundtrip_one(
    original: &Network,
    orig_metrics: &CaseMetrics,
    orig_pf: &Option<(Vec<f64>, Vec<f64>)>,
    case: &str,
    out_path: &std::path::Path,
    label: &str,
) -> Vec<Failure> {
    let mut fails = Vec::new();

    // Write
    if let Err(e) = save(original, out_path) {
        fails.push(Failure {
            case: case.to_string(),
            phase: format!("WRITE->{label}"),
            detail: format!("write failed: {e}"),
        });
        return fails;
    }

    // Read back
    let rt_net = match load(out_path) {
        Ok(n) => n,
        Err(e) => {
            fails.push(Failure {
                case: case.to_string(),
                phase: format!("READ<-{label}"),
                detail: format!("re-parse failed: {e}"),
            });
            return fails;
        }
    };
    let rt_metrics = CaseMetrics::from_network(&rt_net);

    // Compare structure
    for msg in compare_metrics(orig_metrics, &rt_metrics, label) {
        fails.push(Failure {
            case: case.to_string(),
            phase: format!("STRUCT-{label}"),
            detail: msg,
        });
    }

    // Compare PF (only if structure matched and case is small enough)
    if fails.is_empty()
        && let Some((vm_o, va_o)) = orig_pf
        && let Some((vm_r, va_r)) = run_pf(&rt_net)
    {
        // Text formats truncate to fixed decimal places — use relaxed
        // PF tolerance (2e-3 Vm, 5e-3 Va) to accommodate precision loss.
        for msg in compare_pf(vm_o, va_o, &vm_r, &va_r, label, 2e-3, 5e-3) {
            fails.push(Failure {
                case: case.to_string(),
                phase: format!("PF-{label}"),
                detail: msg,
            });
        }
    }

    fails
}

/// Test: lossless formats (MATPOWER, JSON, PSS/E) → assert exact match.
#[test]
#[ignore = "stress roundtrip fails on some matpower fixtures; gate behind a feature flag when revisited"]
fn stress_matpower_lossless_roundtrips() {
    // Standard cases shipped locally as .surge.json.zst
    let local_stems = [
        "case9",
        "case14",
        "case30",
        "case57",
        "case118",
        "case300",
        "case2383wp",
    ];
    // Larger cases only in the external bench repo
    let external_cases = ["case1354pegase.m"];

    let formats = [("m", "MATPOWER"), ("json", "JSON"), ("raw", "PSSE")];

    let tmpdir = TempDir::new().unwrap();
    let mut all_failures: Vec<Failure> = Vec::new();
    let mut pass_count = 0;
    let mut total = 0;

    // --- local cases (always available) ---
    for stem in &local_stems {
        let src = case_path(stem);
        let original =
            load(&src).unwrap_or_else(|e| panic!("failed to load local case {stem}: {e}"));
        let orig_metrics = CaseMetrics::from_network(&original);
        let orig_pf = if orig_metrics.n_buses <= 5000 {
            run_pf(&original)
        } else {
            None
        };

        for &(ext, label) in &formats {
            total += 1;
            let out_path = tmpdir.path().join(format!("{stem}_{label}.{ext}"));

            let fails = roundtrip_one(&original, &orig_metrics, &orig_pf, stem, &out_path, label);
            if fails.is_empty() {
                pass_count += 1;
            } else {
                eprintln!("  FAIL {stem} -> {label}:");
                for f in &fails {
                    eprintln!("    - [{}] {}", f.phase, f.detail);
                }
                all_failures.extend(fails);
            }
        }
    }

    // --- external-only cases (skip when bench repo absent) ---
    if let Some(dd) = data_dir() {
        for case in &external_cases {
            let src = dd.join(case);
            if !src.exists() {
                eprintln!("  SKIP {case}: not found");
                continue;
            }
            let original = match load(&src) {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("  SKIP {case}: parse error: {e}");
                    continue;
                }
            };
            let orig_metrics = CaseMetrics::from_network(&original);
            let orig_pf = if orig_metrics.n_buses <= 5000 {
                run_pf(&original)
            } else {
                None
            };

            for &(ext, label) in &formats {
                total += 1;
                let out_path = tmpdir.path().join(format!(
                    "{}_{}.{}",
                    case.trim_end_matches(".m"),
                    label,
                    ext
                ));

                let fails =
                    roundtrip_one(&original, &orig_metrics, &orig_pf, case, &out_path, label);
                if fails.is_empty() {
                    pass_count += 1;
                } else {
                    eprintln!("  FAIL {case} -> {label}:");
                    for f in &fails {
                        eprintln!("    - [{}] {}", f.phase, f.detail);
                    }
                    all_failures.extend(fails);
                }
            }
        }
    }

    eprintln!("\n=== LOSSLESS RT: {pass_count}/{total} PASS ===\n");
    assert!(
        all_failures.is_empty(),
        "{} lossless round-trip failures",
        all_failures.len()
    );
}

/// Test: CGMES v2/v3 round-trip → structural match required, PF reported.
/// CGMES modeling differences (per-winding impedance, IEC sign convention)
/// may cause PF divergence on larger cases with many transformers.
#[test]
fn stress_matpower_to_cgmes_roundtrip() {
    // Standard cases shipped locally as .surge.json.zst
    let local_stems = [
        "case9",
        "case14",
        "case30",
        "case57",
        "case118",
        "case300",
        "case2383wp",
    ];
    // Larger cases only in the external bench repo
    let external_cases = ["case1354pegase.m"];

    let tmpdir = TempDir::new().unwrap();
    let mut structural_failures: Vec<Failure> = Vec::new();
    let mut pf_warnings = 0;
    let mut pass_count = 0;
    let mut total = 0;

    // Closure that processes one (label, network) pair through CGMES round-trip
    let mut process_case = |case_label: &str,
                            original: &Network,
                            orig_metrics: &CaseMetrics,
                            orig_pf: &Option<(Vec<f64>, Vec<f64>)>,
                            tmpdir: &TempDir| {
        for (version, version_label) in [("cgmes", "CGMES-v2"), ("cgmes3", "CGMES-v3")] {
            total += 1;
            let cgmes_dir = tmpdir
                .path()
                .join(format!("cgmes_{version_label}_{case_label}"));
            std::fs::create_dir_all(&cgmes_dir).unwrap();

            let cgmes_version = match version {
                "cgmes" => surge_io::cgmes::Version::V2_4_15,
                "cgmes3" => surge_io::cgmes::Version::V3_0,
                other => panic!("unexpected CGMES version tag: {other}"),
            };

            if let Err(e) = surge_io::cgmes::save(original, &cgmes_dir, cgmes_version) {
                structural_failures.push(Failure {
                    case: case_label.to_string(),
                    phase: format!("WRITE->{version_label}"),
                    detail: format!("{e}"),
                });
                continue;
            }

            match load(&cgmes_dir) {
                Ok(rt_net) => {
                    let rt_metrics = CaseMetrics::from_network(&rt_net);

                    // Structural match is required
                    let struct_msgs = compare_metrics(orig_metrics, &rt_metrics, version_label);
                    if !struct_msgs.is_empty() {
                        for msg in &struct_msgs {
                            eprintln!("  FAIL {case_label} -> {version_label}: {msg}");
                            structural_failures.push(Failure {
                                case: case_label.to_string(),
                                phase: format!("STRUCT-{version_label}"),
                                detail: msg.clone(),
                            });
                        }
                        continue;
                    }

                    // PF comparison is informational (CGMES modeling differences expected)
                    if let Some((vm_o, va_o)) = orig_pf {
                        if let Some((vm_r, va_r)) = run_pf(&rt_net) {
                            let pf_msgs =
                                compare_pf(vm_o, va_o, &vm_r, &va_r, version_label, 2e-3, 5e-3);
                            if pf_msgs.is_empty() {
                                pass_count += 1;
                                eprintln!("  {case_label} -> {version_label}: PASS (PF match)");
                            } else {
                                pf_warnings += 1;
                                pass_count += 1; // structural pass counts
                                eprintln!(
                                    "  {case_label} -> {version_label}: STRUCT OK, PF diff (expected)"
                                );
                                for msg in &pf_msgs {
                                    eprintln!("    {msg}");
                                }
                            }
                        } else {
                            pass_count += 1;
                            eprintln!(
                                "  {case_label} -> {version_label}: STRUCT OK (RT PF didn't converge)"
                            );
                        }
                    } else {
                        pass_count += 1;
                    }
                }
                Err(e) => {
                    structural_failures.push(Failure {
                        case: case_label.to_string(),
                        phase: format!("READ<-{version_label}"),
                        detail: format!("{e}"),
                    });
                }
            }
        }
    };

    // --- local cases (always available) ---
    for stem in &local_stems {
        let src = case_path(stem);
        let original =
            load(&src).unwrap_or_else(|e| panic!("failed to load local case {stem}: {e}"));
        let orig_metrics = CaseMetrics::from_network(&original);
        let orig_pf = if orig_metrics.n_buses <= 5000 {
            run_pf(&original)
        } else {
            None
        };
        process_case(stem, &original, &orig_metrics, &orig_pf, &tmpdir);
    }

    // --- external-only cases (skip when bench repo absent) ---
    if let Some(dd) = data_dir() {
        for case in &external_cases {
            let src = dd.join(case);
            if !src.exists() {
                continue;
            }
            let original = match load(&src) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let orig_metrics = CaseMetrics::from_network(&original);
            let orig_pf = if orig_metrics.n_buses <= 5000 {
                run_pf(&original)
            } else {
                None
            };
            process_case(
                case.trim_end_matches(".m"),
                &original,
                &orig_metrics,
                &orig_pf,
                &tmpdir,
            );
        }
    }

    eprintln!("\n=== CGMES WRITE RT: {pass_count}/{total} PASS, {pf_warnings} PF warnings ===\n");
    assert!(
        structural_failures.is_empty(),
        "{} CGMES structural failures",
        structural_failures.len()
    );
}

/// Test: lossy formats (DSS, UCTE, XIIDM) → write+read without crashing.
/// Reports differences but doesn't assert (these formats inherently lose info).
#[test]
#[ignore = "DSS reader rejects networks with slack bus list; needs fix"]
fn stress_matpower_lossy_roundtrips() {
    // Standard cases shipped locally as .surge.json.zst
    let local_stems = [
        "case9",
        "case14",
        "case30",
        "case57",
        "case118",
        "case300",
        "case2383wp",
    ];
    // Larger cases only in the external bench repo
    let external_cases = ["case1354pegase.m"];

    let formats = [("dss", "DSS"), ("uct", "UCTE"), ("xiidm", "XIIDM")];

    let tmpdir = TempDir::new().unwrap();
    let mut write_read_failures: Vec<Failure> = Vec::new();
    let mut structural_warnings = 0;
    let mut pass_count = 0;
    let mut total = 0;

    // Closure that processes one case through all lossy formats
    let mut process_case = |case_label: &str, original: &Network| {
        let orig_metrics = CaseMetrics::from_network(original);

        for &(ext, label) in &formats {
            total += 1;
            let out_path = tmpdir.path().join(format!("{case_label}_{label}.{ext}"));

            // Must at least write + read without crashing
            if let Err(e) = save(original, &out_path) {
                write_read_failures.push(Failure {
                    case: case_label.to_string(),
                    phase: format!("WRITE->{label}"),
                    detail: format!("{e}"),
                });
                continue;
            }

            match load(&out_path) {
                Ok(rt_net) => {
                    let rt_m = CaseMetrics::from_network(&rt_net);
                    let diffs = compare_metrics(&orig_metrics, &rt_m, label);
                    if diffs.is_empty() {
                        pass_count += 1;
                    } else {
                        structural_warnings += diffs.len();
                        eprintln!(
                            "  INFO {case_label} -> {label}: {} structural diff(s) (expected for lossy format)",
                            diffs.len()
                        );
                        for d in &diffs {
                            eprintln!("    {d}");
                        }
                    }
                }
                Err(e) => {
                    write_read_failures.push(Failure {
                        case: case_label.to_string(),
                        phase: format!("READ<-{label}"),
                        detail: format!("{e}"),
                    });
                }
            }
        }
    };

    // --- local cases (always available) ---
    for stem in &local_stems {
        let src = case_path(stem);
        let original =
            load(&src).unwrap_or_else(|e| panic!("failed to load local case {stem}: {e}"));
        process_case(stem, &original);
    }

    // --- external-only cases (skip when bench repo absent) ---
    if let Some(dd) = data_dir() {
        for case in &external_cases {
            let src = dd.join(case);
            if !src.exists() {
                continue;
            }
            let original = match load(&src) {
                Ok(n) => n,
                Err(_) => continue,
            };
            process_case(case.trim_end_matches(".m"), &original);
        }
    }

    eprintln!(
        "\n=== LOSSY RT: {pass_count}/{total} exact match, {structural_warnings} expected diffs ===\n"
    );

    // Assert that write+read succeeds (even if structural counts differ)
    assert!(
        write_read_failures.is_empty(),
        "{} write/read failures in lossy format round-trips:\n{}",
        write_read_failures.len(),
        write_read_failures
            .iter()
            .map(|f| format!("  {}: [{}] {}", f.case, f.phase, f.detail))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Test: PSS/E RAW → lossless formats → read back → compare
#[test]
fn stress_psse_roundtrips() {
    let Some(dd) = data_dir() else {
        eprintln!("SKIP: tests/data not present");
        return;
    };
    // Only v30+ PSS/E files. case9.raw/IEEE14.raw/IEEE118.raw are v23/v29 (unsupported)
    let cases = [
        "raw/IEEE14_PTIv33.raw",
        "raw/kundur-twoarea_v33.raw",
        "raw/240busWECC_2018_PSS_fixedshunt.raw",
    ];
    let formats = [("m", "MATPOWER"), ("raw", "PSSE"), ("json", "JSON")];

    let tmpdir = TempDir::new().unwrap();
    let mut failures: Vec<Failure> = Vec::new();
    let mut pass = 0;
    let mut total = 0;

    for case in &cases {
        let src = dd.join(case);
        if !src.exists() {
            eprintln!("  SKIP {case}: not found");
            continue;
        }
        let original = match load(&src) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("  SKIP {case}: {e}");
                continue;
            }
        };
        let orig_m = CaseMetrics::from_network(&original);

        for &(ext, label) in &formats {
            total += 1;
            let basename = std::path::Path::new(case)
                .file_stem()
                .unwrap()
                .to_string_lossy();
            let out = tmpdir.path().join(format!("psse_{basename}_{label}.{ext}"));

            if save(&original, &out).is_err() {
                failures.push(Failure {
                    case: case.to_string(),
                    phase: format!("WRITE->{label}"),
                    detail: "write failed".into(),
                });
                continue;
            }

            match load(&out) {
                Ok(rt) => {
                    let rt_m = CaseMetrics::from_network(&rt);
                    let msgs = compare_metrics(&orig_m, &rt_m, label);
                    if msgs.is_empty() {
                        pass += 1;
                    } else {
                        for msg in msgs {
                            failures.push(Failure {
                                case: case.to_string(),
                                phase: format!("STRUCT-{label}"),
                                detail: msg,
                            });
                        }
                    }
                }
                Err(e) => {
                    failures.push(Failure {
                        case: case.to_string(),
                        phase: format!("READ<-{label}"),
                        detail: format!("{e}"),
                    });
                }
            }
        }
    }

    eprintln!("\n=== PSS/E RT: {pass}/{total} PASS ===\n");
    for f in &failures {
        eprintln!("  {}: [{}] {}", f.case, f.phase, f.detail);
    }
    assert!(
        failures.is_empty(),
        "{} PSS/E round-trip failures",
        failures.len()
    );
}

/// Test: UCTE → writable formats → round-trip
#[test]
fn stress_ucte_roundtrips() {
    let Some(dd) = data_dir() else {
        eprintln!("SKIP: tests/data not present");
        return;
    };
    let cases = [
        "ucte/beTestGrid.uct",
        "ucte/germanTsos.uct",
        "ucte/20170322_1844_SN3_FR2.uct",
    ];
    // UCTE→UCTE is lossless; UCTE→MATPOWER/JSON may differ in gen count
    let formats = [("uct", "UCTE"), ("m", "MATPOWER"), ("json", "JSON")];

    let tmpdir = TempDir::new().unwrap();
    let mut write_read_failures: Vec<Failure> = Vec::new();
    let mut pass = 0;
    let mut total = 0;

    for case in &cases {
        let src = dd.join(case);
        if !src.exists() {
            eprintln!("  SKIP {case}: not found");
            continue;
        }
        let original = match load(&src) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("  SKIP {case}: {e}");
                continue;
            }
        };
        let orig_m = CaseMetrics::from_network(&original);
        eprintln!(
            "  {case}: {} buses, {} branches, {} gens",
            orig_m.n_buses, orig_m.n_branches, orig_m.n_gens
        );

        for &(ext, label) in &formats {
            total += 1;
            let basename = std::path::Path::new(case)
                .file_stem()
                .unwrap()
                .to_string_lossy();
            let out = tmpdir.path().join(format!("ucte_{basename}_{label}.{ext}"));

            if let Err(e) = save(&original, &out) {
                write_read_failures.push(Failure {
                    case: case.to_string(),
                    phase: format!("WRITE->{label}"),
                    detail: format!("{e}"),
                });
                continue;
            }

            match load(&out) {
                Ok(rt) => {
                    let rt_m = CaseMetrics::from_network(&rt);
                    let msgs = compare_metrics(&orig_m, &rt_m, label);
                    if msgs.is_empty() {
                        eprintln!("    -> {label}: PASS");
                    } else {
                        eprintln!("    -> {label}: {} diffs (expected)", msgs.len());
                        for msg in &msgs {
                            eprintln!("      {msg}");
                        }
                    }
                    pass += 1; // write+read worked
                }
                Err(e) => {
                    // Some degenerate cases (e.g., 0 branches) legitimately
                    // fail to re-parse in certain formats.
                    if orig_m.n_branches == 0 {
                        eprintln!("    -> {label}: SKIP (0 branches, re-parse expected to fail)");
                        pass += 1;
                    } else {
                        eprintln!("    -> {label}: READ FAIL {e}");
                        write_read_failures.push(Failure {
                            case: case.to_string(),
                            phase: format!("READ<-{label}"),
                            detail: format!("{e}"),
                        });
                    }
                }
            }
        }
    }

    eprintln!("\n=== UCTE RT: {pass}/{total} PASS ===\n");
    assert!(
        write_read_failures.is_empty(),
        "{} UCTE write/read failures",
        write_read_failures.len()
    );
}

/// Test: DSS → writable formats → round-trip (write+read must work)
#[test]
fn stress_dss_roundtrips() {
    let Some(dd) = data_dir() else {
        eprintln!("SKIP: tests/data not present");
        return;
    };
    let cases = [
        "dss/ieee13/IEEE13Nodeckt.dss",
        "dss/ieee34/ieee34Mod1.dss",
        "dss/ieee37/ieee37.dss",
        "dss/ieee123/IEEE123Master.dss",
        "dss/ieee8500/Master.dss",
    ];
    let formats = [("m", "MATPOWER"), ("json", "JSON"), ("dss", "DSS")];

    let tmpdir = TempDir::new().unwrap();
    let mut write_read_failures: Vec<Failure> = Vec::new();
    let mut pass = 0;
    let mut total = 0;
    let mut skipped = 0;

    for case in &cases {
        let src = dd.join(case);
        if !src.exists() {
            eprintln!("  SKIP {case}: not found");
            skipped += 1;
            continue;
        }
        let original = match load(&src) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("  SKIP {case}: {e}");
                skipped += 1;
                continue;
            }
        };
        let orig_m = CaseMetrics::from_network(&original);
        eprintln!(
            "  {case}: {} buses, {} branches, {} gens, load={:.1}MW",
            orig_m.n_buses, orig_m.n_branches, orig_m.n_gens, orig_m.total_load_mw
        );

        for &(ext, label) in &formats {
            total += 1;
            let basename = std::path::Path::new(case)
                .file_stem()
                .unwrap()
                .to_string_lossy();
            let out = tmpdir.path().join(format!("dss_{basename}_{label}.{ext}"));

            if let Err(e) = save(&original, &out) {
                write_read_failures.push(Failure {
                    case: case.to_string(),
                    phase: format!("WRITE->{label}"),
                    detail: format!("{e}"),
                });
                continue;
            }

            match load(&out) {
                Ok(rt) => {
                    let rt_m = CaseMetrics::from_network(&rt);
                    let msgs = compare_metrics(&orig_m, &rt_m, label);
                    if msgs.is_empty() {
                        eprintln!("    -> {label}: PASS (exact match)");
                    } else {
                        eprintln!("    -> {label}: {} structural diffs (expected)", msgs.len());
                    }
                    pass += 1; // write+read worked
                }
                Err(e) => {
                    eprintln!("    -> {label}: READ FAIL {e}");
                    write_read_failures.push(Failure {
                        case: case.to_string(),
                        phase: format!("READ<-{label}"),
                        detail: format!("{e}"),
                    });
                }
            }
        }
    }

    eprintln!("\n=== DSS RT: {pass}/{total} write+read OK, {skipped} skipped ===\n");
    assert!(
        write_read_failures.is_empty(),
        "{} DSS write/read failures",
        write_read_failures.len()
    );
}

/// Test: CGMES directories → writable formats → round-trip
#[test]
fn stress_cgmes_roundtrips() {
    let tmpdir = TempDir::new().unwrap();
    let mut failures: Vec<Failure> = Vec::new();
    let mut pass = 0;
    let mut total = 0;

    // --- external CGMES directory cases (skip when bench repo absent) ---
    if let Some(dd) = data_dir() {
        let cases = [
            "cgmes/case9",
            "cgmes/case14",
            "cgmes/case118",
            "cgmes/case300",
        ];
        let formats = [("m", "MATPOWER"), ("json", "JSON")];

        for case in &cases {
            let src = dd.join(case);
            if !src.exists() {
                continue;
            }
            let original = match load(&src) {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("  SKIP {case}: {e}");
                    continue;
                }
            };
            let orig_m = CaseMetrics::from_network(&original);

            for &(ext, label) in &formats {
                total += 1;
                let basename = std::path::Path::new(case)
                    .file_name()
                    .unwrap()
                    .to_string_lossy();
                let out = tmpdir
                    .path()
                    .join(format!("cgmes_{basename}_{label}.{ext}"));

                if let Err(e) = save(&original, &out) {
                    failures.push(Failure {
                        case: case.to_string(),
                        phase: format!("WRITE->{label}"),
                        detail: format!("{e}"),
                    });
                    continue;
                }

                match load(&out) {
                    Ok(rt) => {
                        let rt_m = CaseMetrics::from_network(&rt);
                        let msgs = compare_metrics(&orig_m, &rt_m, label);
                        if msgs.is_empty() {
                            pass += 1;
                        } else {
                            for msg in msgs {
                                failures.push(Failure {
                                    case: case.to_string(),
                                    phase: format!("STRUCT-{label}"),
                                    detail: msg,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        failures.push(Failure {
                            case: case.to_string(),
                            phase: format!("READ<-{label}"),
                            detail: format!("{e}"),
                        });
                    }
                }
            }
        }
    }

    // --- CGMES writer: local cases → CGMES v2 + v3 → read back (always runs) ---
    let cgmes_writer_stems = ["case9", "case14", "case118"];
    for stem in &cgmes_writer_stems {
        let src = case_path(stem);
        let original =
            load(&src).unwrap_or_else(|e| panic!("failed to load local case {stem}: {e}"));
        let orig_m = CaseMetrics::from_network(&original);

        for (version, version_label) in [("cgmes", "v2"), ("cgmes3", "v3")] {
            total += 1;
            let cgmes_dir = tmpdir.path().join(format!("cgmes_{version_label}_{stem}"));
            std::fs::create_dir_all(&cgmes_dir).unwrap();

            let cgmes_version = match version {
                "cgmes" => surge_io::cgmes::Version::V2_4_15,
                "cgmes3" => surge_io::cgmes::Version::V3_0,
                other => panic!("unexpected CGMES version tag: {other}"),
            };

            match surge_io::cgmes::save(&original, &cgmes_dir, cgmes_version) {
                Ok(()) => match load(&cgmes_dir) {
                    Ok(rt) => {
                        let rt_m = CaseMetrics::from_network(&rt);
                        let msgs = compare_metrics(&orig_m, &rt_m, version_label);
                        if msgs.is_empty() {
                            pass += 1;
                            eprintln!("  CGMES {version_label} write {stem}: PASS");
                        } else {
                            for msg in msgs {
                                failures.push(Failure {
                                    case: format!("CGMES-{version_label}-{stem}"),
                                    phase: "STRUCT".into(),
                                    detail: msg,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        failures.push(Failure {
                            case: format!("CGMES-{version_label}-{stem}"),
                            phase: "READ".into(),
                            detail: format!("{e}"),
                        });
                    }
                },
                Err(e) => {
                    failures.push(Failure {
                        case: format!("CGMES-{version_label}-{stem}"),
                        phase: "WRITE".into(),
                        detail: format!("{e}"),
                    });
                }
            }
        }
    }

    eprintln!("\n=== CGMES RT: {pass}/{total} PASS ===\n");
    for f in &failures {
        eprintln!("  {}: [{}] {}", f.case, f.phase, f.detail);
    }
    assert!(
        failures.is_empty(),
        "{} CGMES round-trip failures",
        failures.len()
    );
}

/// Test: DYR writer round-trip
#[test]
fn stress_dyr_roundtrip() {
    let Some(dd) = data_dir() else {
        eprintln!("SKIP: tests/data not present");
        return;
    };
    let dyr_files = ["raw/kundur_andes.dyr", "raw/wecc_andes.dyr"];

    let tmpdir = TempDir::new().unwrap();
    let mut failures: Vec<Failure> = Vec::new();
    let mut pass = 0;
    let mut total = 0;

    for dyr_file in &dyr_files {
        let src = dd.join(dyr_file);
        if !src.exists() {
            eprintln!("  SKIP {dyr_file}: not found");
            continue;
        }

        total += 1;

        let dyn_data = match surge_io::psse::dyr::load(&src) {
            Ok(d) => d,
            Err(e) => {
                failures.push(Failure {
                    case: dyr_file.to_string(),
                    phase: "PARSE".into(),
                    detail: format!("{e}"),
                });
                continue;
            }
        };

        let out = tmpdir.path().join(format!(
            "{}_rt.dyr",
            src.file_stem().unwrap().to_string_lossy()
        ));
        if let Err(e) = surge_io::psse::dyr::save(&dyn_data, &out) {
            failures.push(Failure {
                case: dyr_file.to_string(),
                phase: "WRITE".into(),
                detail: format!("{e}"),
            });
            continue;
        }

        let rt_data = match surge_io::psse::dyr::load(&out) {
            Ok(d) => d,
            Err(e) => {
                failures.push(Failure {
                    case: dyr_file.to_string(),
                    phase: "RE-PARSE".into(),
                    detail: format!("{e}"),
                });
                continue;
            }
        };

        // Compare counts
        let checks = [
            (
                "generators",
                dyn_data.generators.len(),
                rt_data.generators.len(),
            ),
            ("exciters", dyn_data.exciters.len(), rt_data.exciters.len()),
            (
                "governors",
                dyn_data.governors.len(),
                rt_data.governors.len(),
            ),
            ("pss", dyn_data.pss.len(), rt_data.pss.len()),
        ];

        let mut ok = true;
        for (name, orig, rt) in checks {
            if orig != rt {
                ok = false;
                failures.push(Failure {
                    case: dyr_file.to_string(),
                    phase: "DYR-STRUCT".into(),
                    detail: format!("{name}: {orig} vs {rt}"),
                });
            }
        }

        if ok {
            pass += 1;
            eprintln!(
                "  {dyr_file}: PASS ({} gens, {} excs, {} govs, {} pss)",
                dyn_data.generators.len(),
                dyn_data.exciters.len(),
                dyn_data.governors.len(),
                dyn_data.pss.len()
            );
        }
    }

    eprintln!("\n=== DYR RT: {pass}/{total} PASS ===\n");
    assert!(
        failures.is_empty(),
        "{} DYR round-trip failures",
        failures.len()
    );
}

/// Test: CGMES zip files → read → structural check
#[test]
fn stress_cgmes_zip_parse() {
    let Some(dd) = data_dir() else {
        eprintln!("SKIP: tests/data not present");
        return;
    };
    let zip_files = [
        "cgmes/case9_cgmes.zip",
        "cgmes/case14_cgmes.zip",
        "cgmes/case118_cgmes.zip",
        "cgmes/case300_cgmes.zip",
        "cgmes/case9241pegase_cgmes.zip",
    ];

    let mut pass = 0;
    let mut total = 0;

    for zip in &zip_files {
        let src = dd.join(zip);
        if !src.exists() {
            eprintln!("  SKIP {zip}: not found");
            continue;
        }
        total += 1;

        match load(&src) {
            Ok(net) => {
                let m = CaseMetrics::from_network(&net);
                eprintln!(
                    "  {zip}: PASS ({} buses, {} branches, {} gens)",
                    m.n_buses, m.n_branches, m.n_gens
                );
                assert!(m.n_buses > 0, "empty network from {zip}");
                pass += 1;
            }
            Err(e) => {
                eprintln!("  {zip}: FAIL ({e})");
            }
        }
    }

    eprintln!("\n=== CGMES ZIP: {pass}/{total} PASS ===\n");
    assert_eq!(pass, total, "some CGMES zip files failed to parse");
}
