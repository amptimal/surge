// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES reader/writer stress tests.
//!
//! For every CGMES test-data directory and zip file we exercise:
//!   (a) CGMES read — parse and verify bus/branch/gen counts > 0
//!   (b) CGMES round-trip v2.4.15 — write → re-read, compare counts
//!   (c) CGMES round-trip v3.0 — write → re-read, compare counts
//!   (d) CGMES→MATPOWER→CGMES — cross-format round-trip
//!   (e) CGMES→JSON — round-trip via JSON serialization
//!   (f) Power flow — NR on original and round-tripped networks, compare Vm/Va
//!
//! Failures are collected and reported as a summary at the end.

use std::path::PathBuf;

use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
use surge_io::cgmes::{Version as CgmesVersion, save as write_cgmes};
use surge_io::load;
use surge_network::Network;

// ---------------------------------------------------------------------------
// Test data discovery
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(manifest)
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn cgmes_data_dir() -> PathBuf {
    workspace_root().join("tests/data/cgmes")
}

/// All CGMES directories (non-zip, non-entsoe — entsoe contains nested zips).
fn cgmes_directories() -> Vec<(String, PathBuf)> {
    let base = cgmes_data_dir();
    if !base.exists() {
        return vec![];
    }
    let mut dirs: Vec<(String, PathBuf)> = std::fs::read_dir(&base)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Skip entsoe (nested zips, not raw XML dirs) and ieee*_pbl (known model-diff failures)
            name != "entsoe"
        })
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (name, e.path())
        })
        .collect();
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    dirs
}

/// All CGMES zip files.
fn cgmes_zips() -> Vec<(String, PathBuf)> {
    let base = cgmes_data_dir();
    if !base.exists() {
        return vec![];
    }
    let mut zips: Vec<(String, PathBuf)> = std::fs::read_dir(&base)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (name, e.path())
        })
        .collect();
    zips.sort_by(|a, b| a.0.cmp(&b.0));
    zips
}

// ---------------------------------------------------------------------------
// Failure collector
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Failure {
    case: String,
    phase: String,
    message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.case, self.phase, self.message)
    }
}

// ---------------------------------------------------------------------------
// Helper: run NR power flow, return (vm, va) or error string
// ---------------------------------------------------------------------------

fn run_pf(net: &Network) -> Result<(Vec<f64>, Vec<f64>), String> {
    let opts = AcPfOptions {
        tolerance: 1e-6,
        max_iterations: 50,
        flat_start: true,
        ..Default::default()
    };
    match solve_ac_pf_kernel(net, &opts) {
        Ok(sol) => {
            if sol.status != surge_solution::SolveStatus::Converged {
                Err(format!(
                    "NR did not converge: {} iters, max_mismatch={:.2e}",
                    sol.iterations, sol.max_mismatch
                ))
            } else {
                Ok((sol.voltage_magnitude_pu, sol.voltage_angle_rad))
            }
        }
        Err(e) => Err(format!("NR error: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Helper: compare two networks structurally
// ---------------------------------------------------------------------------

fn compare_structure(
    orig: &Network,
    rt: &Network,
    case: &str,
    phase: &str,
    failures: &mut Vec<Failure>,
) {
    // Bus count must match exactly
    if orig.n_buses() != rt.n_buses() {
        failures.push(Failure {
            case: case.to_string(),
            phase: phase.to_string(),
            message: format!(
                "bus count mismatch: orig={} rt={}",
                orig.n_buses(),
                rt.n_buses()
            ),
        });
    }

    // Branch count within +/-2 (3-winding transformer star-bus expansion can differ)
    let branch_diff = (orig.n_branches() as i64 - rt.n_branches() as i64).unsigned_abs();
    if branch_diff > 2 {
        failures.push(Failure {
            case: case.to_string(),
            phase: phase.to_string(),
            message: format!(
                "branch count mismatch: orig={} rt={} (diff={})",
                orig.n_branches(),
                rt.n_branches(),
                branch_diff,
            ),
        });
    }

    // Gen count within +/-1
    let gen_diff = (orig.generators.len() as i64 - rt.generators.len() as i64).unsigned_abs();
    if gen_diff > 1 {
        failures.push(Failure {
            case: case.to_string(),
            phase: phase.to_string(),
            message: format!(
                "gen count mismatch: orig={} rt={} (diff={})",
                orig.generators.len(),
                rt.generators.len(),
                gen_diff,
            ),
        });
    }

    // Total load MW comparison via Load objects / total_load_mw().
    let load_orig = orig.total_load_mw();
    let load_rt = rt.total_load_mw();
    let load_base = load_orig.abs().max(1.0);
    let load_err = (load_orig - load_rt).abs() / load_base;
    if load_err > 0.05 {
        failures.push(Failure {
            case: case.to_string(),
            phase: phase.to_string(),
            message: format!(
                "total load MW mismatch: orig={:.2} rt={:.2} (err={:.1}%)",
                load_orig,
                load_rt,
                load_err * 100.0,
            ),
        });
    }

    // Flag net bus demand divergence (includes power injection effects)
    if !orig.loads.is_empty() {
        let bus_pd_orig: f64 = orig.bus_load_p_mw().iter().sum();
        let bus_pd_rt: f64 = rt.bus_load_p_mw().iter().sum();
        let bus_load_diff = (bus_pd_orig - bus_pd_rt).abs();
        if bus_load_diff > 1.0 && bus_load_diff / load_base > 0.01 {
            failures.push(Failure {
                case: case.to_string(),
                phase: format!("{phase}-bus-pd"),
                message: format!(
                    "bus net demand total mismatch: orig={:.2} rt={:.2} (diff={:.2} MW)",
                    bus_pd_orig, bus_pd_rt, bus_load_diff,
                ),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: compare power flow results
// ---------------------------------------------------------------------------

/// Compare PF solutions by matching bus numbers, not array indices.
/// The round-tripped network may have different bus ordering due to TN mRID
/// alphabetical sorting (BUG: writer uses non-zero-padded bus numbers).
#[allow(clippy::too_many_arguments)]
fn compare_pf(
    orig: &Network,
    vm_orig: &[f64],
    va_orig: &[f64],
    rt: &Network,
    vm_rt: &[f64],
    va_rt: &[f64],
    case: &str,
    phase: &str,
    failures: &mut Vec<Failure>,
) {
    use std::collections::HashMap;

    // Build bus_number → index maps
    let orig_map: HashMap<u32, usize> = orig
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();
    let rt_map: HashMap<u32, usize> = rt
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    // Try to match by bus number. If bus numbers don't correspond (due to
    // renumbering), fall back to sorted-voltage comparison.
    let mut matched = 0;
    let mut max_vm_err = 0.0_f64;
    let mut max_va_err = 0.0_f64;

    for (&bus_num, &oi) in &orig_map {
        if let Some(&ri) = rt_map.get(&bus_num)
            && oi < vm_orig.len()
            && ri < vm_rt.len()
        {
            max_vm_err = max_vm_err.max((vm_orig[oi] - vm_rt[ri]).abs());
            max_va_err = max_va_err.max((va_orig[oi] - va_rt[ri]).abs());
            matched += 1;
        }
    }

    if matched == 0 {
        // No matching bus numbers — bus numbering changed completely.
        // Compare sorted voltage profiles as a rough check.
        let mut vms_orig: Vec<f64> = vm_orig.to_vec();
        let mut vms_rt: Vec<f64> = vm_rt.to_vec();
        vms_orig.sort_by(|a, b| a.partial_cmp(b).unwrap());
        vms_rt.sort_by(|a, b| a.partial_cmp(b).unwrap());
        max_vm_err = vms_orig
            .iter()
            .zip(vms_rt.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f64, f64::max);
        max_va_err = 0.0; // Can't meaningfully compare angles without bus correspondence
    }

    if max_vm_err > 1e-3 {
        failures.push(Failure {
            case: case.to_string(),
            phase: phase.to_string(),
            message: format!(
                "PF Vm max error = {max_vm_err:.6} pu (threshold 1e-3, matched={matched}/{})",
                orig.n_buses()
            ),
        });
    }

    if max_va_err > 1e-2 {
        failures.push(Failure {
            case: case.to_string(),
            phase: phase.to_string(),
            message: format!(
                "PF Va max error = {max_va_err:.6} rad (threshold 1e-2, matched={matched}/{})",
                orig.n_buses()
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// The main stress test
// ---------------------------------------------------------------------------

#[test]
#[ignore = "stress test; works the parser and solver hard and is not meant for core testing"]
fn stress_test_cgmes_all_cases() {
    let mut failures: Vec<Failure> = Vec::new();
    let mut pass_count: usize = 0;
    let mut skip_count: usize = 0;

    let dirs = cgmes_directories();
    let zips = cgmes_zips();

    println!("\n=== CGMES Stress Test ===");
    println!(
        "Found {} directories, {} zip files\n",
        dirs.len(),
        zips.len()
    );

    // ------------------------------------------------------------------
    // Phase 1: Test each CGMES directory
    // ------------------------------------------------------------------
    for (name, path) in &dirs {
        println!("--- {name} ---");

        // (a) CGMES read
        let net = match load(path) {
            Ok(n) => n,
            Err(e) => {
                failures.push(Failure {
                    case: name.clone(),
                    phase: "read".to_string(),
                    message: format!("load failed: {e}"),
                });
                continue;
            }
        };

        if net.n_buses() == 0 {
            failures.push(Failure {
                case: name.clone(),
                phase: "read".to_string(),
                message: "0 buses parsed".to_string(),
            });
            continue;
        }
        if net.n_branches() == 0 {
            failures.push(Failure {
                case: name.clone(),
                phase: "read".to_string(),
                message: "0 branches parsed".to_string(),
            });
        }
        if net.generators.is_empty() {
            // Some test cases (cigremv, microgrid_nl) may legitimately have 0 generators
            // as loads-only networks. Log but don't fail.
            println!("  [WARN] 0 generators in {name}");
        }

        println!(
            "  read: {} buses, {} branches, {} gens, {:.1} MW load",
            net.n_buses(),
            net.n_branches(),
            net.generators.len(),
            net.total_load_mw(),
        );
        pass_count += 1;

        // (b) CGMES round-trip v2.4.15
        {
            let tmpdir = tempfile::TempDir::new().unwrap();
            let rt_dir = tmpdir.path().join("v2");
            match write_cgmes(&net, &rt_dir, CgmesVersion::V2_4_15) {
                Ok(()) => match load(&rt_dir) {
                    Ok(rt_net) => {
                        compare_structure(&net, &rt_net, name, "rt-v2.4.15", &mut failures);
                        println!(
                            "  rt-v2.4.15: {} buses, {} branches, {} gens",
                            rt_net.n_buses(),
                            rt_net.n_branches(),
                            rt_net.generators.len(),
                        );
                        pass_count += 1;
                    }
                    Err(e) => {
                        failures.push(Failure {
                            case: name.clone(),
                            phase: "rt-v2.4.15-read".to_string(),
                            message: format!("re-read failed: {e}"),
                        });
                    }
                },
                Err(e) => {
                    failures.push(Failure {
                        case: name.clone(),
                        phase: "rt-v2.4.15-write".to_string(),
                        message: format!("write_cgmes failed: {e}"),
                    });
                }
            }
        }

        // (c) CGMES round-trip v3.0
        {
            let tmpdir = tempfile::TempDir::new().unwrap();
            let rt_dir = tmpdir.path().join("v3");
            match write_cgmes(&net, &rt_dir, CgmesVersion::V3_0) {
                Ok(()) => match load(&rt_dir) {
                    Ok(rt_net) => {
                        compare_structure(&net, &rt_net, name, "rt-v3.0", &mut failures);
                        println!(
                            "  rt-v3.0: {} buses, {} branches, {} gens",
                            rt_net.n_buses(),
                            rt_net.n_branches(),
                            rt_net.generators.len(),
                        );
                        pass_count += 1;
                    }
                    Err(e) => {
                        failures.push(Failure {
                            case: name.clone(),
                            phase: "rt-v3.0-read".to_string(),
                            message: format!("re-read failed: {e}"),
                        });
                    }
                },
                Err(e) => {
                    failures.push(Failure {
                        case: name.clone(),
                        phase: "rt-v3.0-write".to_string(),
                        message: format!("write_cgmes failed: {e}"),
                    });
                }
            }
        }

        // (d) CGMES → MATPOWER → CGMES
        {
            let tmpdir = tempfile::TempDir::new().unwrap();
            let mat_path = tmpdir.path().join("case.m");
            match surge_io::matpower::save(&net, &mat_path) {
                Ok(()) => match surge_io::matpower::load(&mat_path) {
                    Ok(mat_net) => {
                        // Write back to CGMES
                        let cgmes_dir2 = tmpdir.path().join("cgmes2");
                        match write_cgmes(&mat_net, &cgmes_dir2, CgmesVersion::V2_4_15) {
                            Ok(()) => match load(&cgmes_dir2) {
                                Ok(rt2_net) => {
                                    // Cross-format path: MATPOWER has loads as Load objects.
                                    // Compare bus counts and total load directly.
                                    if net.n_buses() != rt2_net.n_buses() {
                                        failures.push(Failure {
                                            case: name.clone(),
                                            phase: "cgmes-mat-cgmes".to_string(),
                                            message: format!(
                                                "bus count mismatch: orig={} rt={}",
                                                net.n_buses(),
                                                rt2_net.n_buses()
                                            ),
                                        });
                                    }
                                    let br_diff = (net.n_branches() as i64
                                        - rt2_net.n_branches() as i64)
                                        .unsigned_abs();
                                    if br_diff > 2 {
                                        failures.push(Failure {
                                            case: name.clone(),
                                            phase: "cgmes-mat-cgmes".to_string(),
                                            message: format!(
                                                "branch count mismatch: orig={} rt={}",
                                                net.n_branches(),
                                                rt2_net.n_branches()
                                            ),
                                        });
                                    }
                                    println!(
                                        "  cgmes-mat-cgmes: {} buses, {} branches",
                                        rt2_net.n_buses(),
                                        rt2_net.n_branches(),
                                    );
                                    pass_count += 1;
                                }
                                Err(e) => {
                                    failures.push(Failure {
                                        case: name.clone(),
                                        phase: "cgmes-mat-cgmes-read2".to_string(),
                                        message: format!("final CGMES re-read failed: {e}"),
                                    });
                                }
                            },
                            Err(e) => {
                                failures.push(Failure {
                                    case: name.clone(),
                                    phase: "cgmes-mat-cgmes-write2".to_string(),
                                    message: format!("write_cgmes from matpower failed: {e}"),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        failures.push(Failure {
                            case: name.clone(),
                            phase: "cgmes-mat-read".to_string(),
                            message: format!("matpower re-read failed: {e}"),
                        });
                    }
                },
                Err(e) => {
                    failures.push(Failure {
                        case: name.clone(),
                        phase: "cgmes-mat-write".to_string(),
                        message: format!("matpower::save failed: {e}"),
                    });
                }
            }
        }

        // (e) CGMES → JSON → compare
        {
            let tmpdir = tempfile::TempDir::new().unwrap();
            let json_path = tmpdir.path().join("case.json");
            match surge_io::json::save(&net, &json_path) {
                Ok(()) => match surge_io::json::load(&json_path) {
                    Ok(json_net) => {
                        // JSON should be lossless
                        if net.n_buses() != json_net.n_buses() {
                            failures.push(Failure {
                                case: name.clone(),
                                phase: "json-rt".to_string(),
                                message: format!(
                                    "bus count mismatch: orig={} json={}",
                                    net.n_buses(),
                                    json_net.n_buses()
                                ),
                            });
                        }
                        if net.n_branches() != json_net.n_branches() {
                            failures.push(Failure {
                                case: name.clone(),
                                phase: "json-rt".to_string(),
                                message: format!(
                                    "branch count mismatch: orig={} json={}",
                                    net.n_branches(),
                                    json_net.n_branches()
                                ),
                            });
                        }
                        if net.generators.len() != json_net.generators.len() {
                            failures.push(Failure {
                                case: name.clone(),
                                phase: "json-rt".to_string(),
                                message: format!(
                                    "gen count mismatch: orig={} json={}",
                                    net.generators.len(),
                                    json_net.generators.len()
                                ),
                            });
                        }
                        println!("  json-rt: OK");
                        pass_count += 1;
                    }
                    Err(e) => {
                        failures.push(Failure {
                            case: name.clone(),
                            phase: "json-rt-read".to_string(),
                            message: format!("JSON re-read failed: {e}"),
                        });
                    }
                },
                Err(e) => {
                    failures.push(Failure {
                        case: name.clone(),
                        phase: "json-rt-write".to_string(),
                        message: format!("write_json failed: {e}"),
                    });
                }
            }
        }

        // (f) Power flow comparison: original vs v2.4.15 round-tripped
        {
            let pf_orig = run_pf(&net);
            let tmpdir = tempfile::TempDir::new().unwrap();
            let rt_dir = tmpdir.path().join("pf_rt");
            let rt_net_opt = write_cgmes(&net, &rt_dir, CgmesVersion::V2_4_15)
                .ok()
                .and_then(|()| load(&rt_dir).ok());
            let pf_rt = rt_net_opt.as_ref().map(run_pf);

            match (&pf_orig, &pf_rt, &rt_net_opt) {
                (Ok((vm_o, va_o)), Some(Ok((vm_r, va_r))), Some(rt_net)) => {
                    compare_pf(
                        &net,
                        vm_o,
                        va_o,
                        rt_net,
                        vm_r,
                        va_r,
                        name,
                        "pf-v2.4.15",
                        &mut failures,
                    );
                    pass_count += 1;
                }
                (Err(e1), _, _) => {
                    // PF on original didn't converge — not a writer bug, skip comparison
                    println!("  pf-orig: SKIP ({e1})");
                    skip_count += 1;
                }
                (Ok(_), Some(Err(e2)), _) => {
                    failures.push(Failure {
                        case: name.clone(),
                        phase: "pf-v2.4.15-rt".to_string(),
                        message: format!("PF on round-tripped network failed: {e2}"),
                    });
                }
                (Ok(_), None, _) | (Ok(_), Some(Ok(_)), _) => {
                    // write_cgmes or re-read already failed — skip
                    println!("  pf-rt: SKIP (write/read failure)");
                    skip_count += 1;
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Phase 2: Test zip files
    // ------------------------------------------------------------------
    println!("\n=== ZIP Files ===\n");
    for (name, path) in &zips {
        print!("  {name}: ");
        match load(path) {
            Ok(net) => {
                if net.n_buses() == 0 {
                    failures.push(Failure {
                        case: name.clone(),
                        phase: "zip-read".to_string(),
                        message: "0 buses parsed from zip".to_string(),
                    });
                    println!("FAIL (0 buses)");
                } else {
                    println!(
                        "{} buses, {} branches, {} gens",
                        net.n_buses(),
                        net.n_branches(),
                        net.generators.len(),
                    );
                    pass_count += 1;
                }
            }
            Err(e) => {
                failures.push(Failure {
                    case: name.clone(),
                    phase: "zip-read".to_string(),
                    message: format!("load failed: {e}"),
                });
                println!("FAIL ({e})");
            }
        }
    }

    // ------------------------------------------------------------------
    // Phase 3: Edge cases — write CGMES with empty/minimal network
    // ------------------------------------------------------------------
    println!("\n=== Edge Cases ===\n");

    // Empty network
    {
        let empty_net = Network::new("empty");
        let tmpdir = tempfile::TempDir::new().unwrap();
        let out = tmpdir.path().join("empty_cgmes");
        match write_cgmes(&empty_net, &out, CgmesVersion::V2_4_15) {
            Ok(()) => {
                match load(&out) {
                    Ok(rt) => {
                        if rt.n_buses() != 0 {
                            failures.push(Failure {
                                case: "empty-network".to_string(),
                                phase: "edge-empty".to_string(),
                                message: format!(
                                    "expected 0 buses from empty network, got {}",
                                    rt.n_buses()
                                ),
                            });
                        }
                        println!("  empty-network: {} buses (expected 0)", rt.n_buses());
                    }
                    Err(e) => {
                        // It's acceptable for an empty CGMES dir to fail parsing
                        // if there are no XML files or no buses.
                        println!("  empty-network: re-read error (acceptable): {e}");
                    }
                }
                pass_count += 1;
            }
            Err(e) => {
                failures.push(Failure {
                    case: "empty-network".to_string(),
                    phase: "edge-empty-write".to_string(),
                    message: format!("write_cgmes failed on empty network: {e}"),
                });
            }
        }
    }

    // Single-bus network (no branches)
    {
        use surge_network::network::{Bus, BusType, Generator, Load};
        let mut net = Network::new("single_bus");
        net.base_mva = 100.0;
        let mut bus = Bus::new(1, BusType::Slack, 230.0);
        bus.voltage_magnitude_pu = 1.0;
        net.buses.push(bus);
        net.loads.push(Load::new(1, 10.0, 5.0));
        net.generators.push(Generator::new(1, 10.0, 1.0));

        let tmpdir = tempfile::TempDir::new().unwrap();
        let out = tmpdir.path().join("single_bus_cgmes");
        match write_cgmes(&net, &out, CgmesVersion::V2_4_15) {
            Ok(()) => match load(&out) {
                Ok(rt) => {
                    if rt.n_buses() != 1 {
                        failures.push(Failure {
                            case: "single-bus".to_string(),
                            phase: "edge-single".to_string(),
                            message: format!("expected 1 bus, got {}", rt.n_buses()),
                        });
                    }
                    println!(
                        "  single-bus: {} buses, {} gens",
                        rt.n_buses(),
                        rt.generators.len()
                    );
                    pass_count += 1;
                }
                Err(e) => {
                    failures.push(Failure {
                        case: "single-bus".to_string(),
                        phase: "edge-single-read".to_string(),
                        message: format!("re-read failed: {e}"),
                    });
                }
            },
            Err(e) => {
                failures.push(Failure {
                    case: "single-bus".to_string(),
                    phase: "edge-single-write".to_string(),
                    message: format!("write_cgmes failed: {e}"),
                });
            }
        }
    }

    // Large-bus-number network (bus number > 100000)
    {
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};
        let mut net = Network::new("large_bus_num");
        net.base_mva = 100.0;
        let mut b1 = Bus::new(999999, BusType::Slack, 500.0);
        b1.voltage_magnitude_pu = 1.05;
        net.buses.push(b1);
        let b2 = Bus::new(1000000, BusType::PQ, 500.0);
        net.buses.push(b2);
        net.loads.push(Load::new(1000000, 200.0, 80.0));
        net.generators.push(Generator::new(999999, 200.0, 1.05));
        net.branches
            .push(Branch::new_line(999999, 1000000, 0.01, 0.05, 0.02));

        let tmpdir = tempfile::TempDir::new().unwrap();
        let out = tmpdir.path().join("large_busnum_cgmes");
        match write_cgmes(&net, &out, CgmesVersion::V2_4_15) {
            Ok(()) => match load(&out) {
                Ok(rt) => {
                    compare_structure(&net, &rt, "large-bus-num", "edge-large-bus", &mut failures);
                    println!(
                        "  large-bus-num: {} buses, {} branches",
                        rt.n_buses(),
                        rt.n_branches()
                    );
                    pass_count += 1;
                }
                Err(e) => {
                    failures.push(Failure {
                        case: "large-bus-num".to_string(),
                        phase: "edge-large-bus-read".to_string(),
                        message: format!("re-read failed: {e}"),
                    });
                }
            },
            Err(e) => {
                failures.push(Failure {
                    case: "large-bus-num".to_string(),
                    phase: "edge-large-bus-write".to_string(),
                    message: format!("write_cgmes failed: {e}"),
                });
            }
        }
    }

    // Network with zero-impedance branches
    {
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};
        let mut net = Network::new("zero_impedance");
        net.base_mva = 100.0;
        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.0;
        net.buses.push(b1);
        let b2 = Bus::new(2, BusType::PQ, 230.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 50.0, 0.0));
        net.generators.push(Generator::new(1, 50.0, 1.0));
        // Zero-impedance tie line
        net.branches.push(Branch::new_line(1, 2, 0.0, 0.0001, 0.0));

        let tmpdir = tempfile::TempDir::new().unwrap();
        let out = tmpdir.path().join("zero_z_cgmes");
        match write_cgmes(&net, &out, CgmesVersion::V2_4_15) {
            Ok(()) => match load(&out) {
                Ok(rt) => {
                    compare_structure(&net, &rt, "zero-impedance", "edge-zero-z", &mut failures);
                    // Check that the impedance survived
                    if let Some(br) = rt.branches.first()
                        && (br.x - 0.0001).abs() > 1e-6
                    {
                        failures.push(Failure {
                            case: "zero-impedance".to_string(),
                            phase: "edge-zero-z-value".to_string(),
                            message: format!("branch x mismatch: expected 0.0001, got {}", br.x),
                        });
                    }
                    println!(
                        "  zero-impedance: {} buses, {} branches",
                        rt.n_buses(),
                        rt.n_branches()
                    );
                    pass_count += 1;
                }
                Err(e) => {
                    failures.push(Failure {
                        case: "zero-impedance".to_string(),
                        phase: "edge-zero-z-read".to_string(),
                        message: format!("re-read failed: {e}"),
                    });
                }
            },
            Err(e) => {
                failures.push(Failure {
                    case: "zero-impedance".to_string(),
                    phase: "edge-zero-z-write".to_string(),
                    message: format!("write_cgmes failed: {e}"),
                });
            }
        }
    }

    // Network with transformer (tap != 1.0)
    {
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};
        let mut net = Network::new("transformer");
        net.base_mva = 100.0;
        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.0;
        net.buses.push(b1);
        let b2 = Bus::new(2, BusType::PQ, 115.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 50.0, 20.0));
        net.generators.push(Generator::new(1, 60.0, 1.0));
        let mut xfmr = Branch::new_line(1, 2, 0.01, 0.08, 0.0);
        xfmr.tap = 1.05; // 5% tap
        xfmr.phase_shift_rad = 0.0;
        net.branches.push(xfmr);

        let tmpdir = tempfile::TempDir::new().unwrap();
        let out = tmpdir.path().join("xfmr_cgmes");
        match write_cgmes(&net, &out, CgmesVersion::V2_4_15) {
            Ok(()) => match load(&out) {
                Ok(rt) => {
                    compare_structure(&net, &rt, "transformer", "edge-xfmr", &mut failures);
                    // Check tap value survived
                    if let Some(br) = rt.branches.first()
                        && (br.tap - 1.05).abs() > 0.01
                    {
                        failures.push(Failure {
                            case: "transformer".to_string(),
                            phase: "edge-xfmr-tap".to_string(),
                            message: format!("tap mismatch: expected ~1.05, got {}", br.tap),
                        });
                    }
                    println!(
                        "  transformer: {} buses, {} branches, tap={}",
                        rt.n_buses(),
                        rt.n_branches(),
                        rt.branches.first().map(|b| b.tap).unwrap_or(0.0),
                    );
                    pass_count += 1;
                }
                Err(e) => {
                    failures.push(Failure {
                        case: "transformer".to_string(),
                        phase: "edge-xfmr-read".to_string(),
                        message: format!("re-read failed: {e}"),
                    });
                }
            },
            Err(e) => {
                failures.push(Failure {
                    case: "transformer".to_string(),
                    phase: "edge-xfmr-write".to_string(),
                    message: format!("write_cgmes failed: {e}"),
                });
            }
        }
    }

    // Network with phase-shifting transformer
    {
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};
        let mut net = Network::new("phase_shifter");
        net.base_mva = 100.0;
        let mut b1 = Bus::new(1, BusType::Slack, 345.0);
        b1.voltage_magnitude_pu = 1.02;
        net.buses.push(b1);
        let b2 = Bus::new(2, BusType::PQ, 345.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 100.0, 40.0));
        net.generators.push(Generator::new(1, 120.0, 1.02));
        let mut pst = Branch::new_line(1, 2, 0.005, 0.05, 0.02);
        pst.tap = 1.0;
        pst.phase_shift_rad = 15.0_f64.to_radians();
        net.branches.push(pst);

        let tmpdir = tempfile::TempDir::new().unwrap();
        let out = tmpdir.path().join("pst_cgmes");
        match write_cgmes(&net, &out, CgmesVersion::V2_4_15) {
            Ok(()) => match load(&out) {
                Ok(rt) => {
                    compare_structure(&net, &rt, "phase-shifter", "edge-pst", &mut failures);
                    if let Some(br) = rt.branches.first() {
                        let shift_rad_orig = 15.0_f64.to_radians();
                        let shift_rad_rt = br.phase_shift_rad;
                        if (shift_rad_orig - shift_rad_rt).abs() > 1.0_f64.to_radians() {
                            failures.push(Failure {
                                case: "phase-shifter".to_string(),
                                phase: "edge-pst-shift".to_string(),
                                message: format!(
                                    "shift mismatch: expected ~{:.4} rad, got {:.4} rad",
                                    shift_rad_orig, shift_rad_rt,
                                ),
                            });
                        }
                    }
                    println!(
                        "  phase-shifter: {} buses, {} branches, shift={:.4} rad",
                        rt.n_buses(),
                        rt.n_branches(),
                        rt.branches
                            .first()
                            .map(|b| b.phase_shift_rad)
                            .unwrap_or(0.0),
                    );
                    pass_count += 1;
                }
                Err(e) => {
                    failures.push(Failure {
                        case: "phase-shifter".to_string(),
                        phase: "edge-pst-read".to_string(),
                        message: format!("re-read failed: {e}"),
                    });
                }
            },
            Err(e) => {
                failures.push(Failure {
                    case: "phase-shifter".to_string(),
                    phase: "edge-pst-write".to_string(),
                    message: format!("write_cgmes failed: {e}"),
                });
            }
        }
    }

    // Network with shunt elements (bus.shunt_conductance_mw, bus.shunt_susceptance_mvar)
    {
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};
        let mut net = Network::new("shunt_case");
        net.base_mva = 100.0;
        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.0;
        net.buses.push(b1);
        let mut b2 = Bus::new(2, BusType::PQ, 230.0);
        b2.shunt_conductance_mw = 5.0; // shunt conductance (MW at V=1)
        b2.shunt_susceptance_mvar = 25.0; // shunt susceptance (MVAr at V=1)
        net.buses.push(b2);
        net.loads.push(Load::new(2, 50.0, 20.0));
        net.generators.push(Generator::new(1, 55.0, 1.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.06, 0.03));

        let tmpdir = tempfile::TempDir::new().unwrap();
        let out = tmpdir.path().join("shunt_cgmes");
        match write_cgmes(&net, &out, CgmesVersion::V2_4_15) {
            Ok(()) => {
                match load(&out) {
                    Ok(rt) => {
                        compare_structure(&net, &rt, "shunt", "edge-shunt", &mut failures);
                        // Check shunt values survived
                        let rt_bus_pd = rt.bus_load_p_mw();
                        if let Some(bus2) = rt
                            .buses
                            .iter()
                            .enumerate()
                            .find(|(i, b)| rt_bus_pd[*i] > 0.0 || b.shunt_conductance_mw > 0.0)
                            .map(|(_, b)| b)
                        {
                            if (bus2.shunt_conductance_mw - 5.0).abs() > 1.0 {
                                failures.push(Failure {
                                    case: "shunt".to_string(),
                                    phase: "edge-shunt-gs".to_string(),
                                    message: format!(
                                        "gs mismatch: expected ~5.0, got {}",
                                        bus2.shunt_conductance_mw
                                    ),
                                });
                            }
                            if (bus2.shunt_susceptance_mvar - 25.0).abs() > 5.0 {
                                failures.push(Failure {
                                    case: "shunt".to_string(),
                                    phase: "edge-shunt-bs".to_string(),
                                    message: format!(
                                        "bs mismatch: expected ~25.0, got {}",
                                        bus2.shunt_susceptance_mvar
                                    ),
                                });
                            }
                        }
                        println!(
                            "  shunt: {} buses, gs={:.1} bs={:.1}",
                            rt.n_buses(),
                            rt.buses.iter().map(|b| b.shunt_conductance_mw).sum::<f64>(),
                            rt.buses
                                .iter()
                                .map(|b| b.shunt_susceptance_mvar)
                                .sum::<f64>(),
                        );
                        pass_count += 1;
                    }
                    Err(e) => {
                        failures.push(Failure {
                            case: "shunt".to_string(),
                            phase: "edge-shunt-read".to_string(),
                            message: format!("re-read failed: {e}"),
                        });
                    }
                }
            }
            Err(e) => {
                failures.push(Failure {
                    case: "shunt".to_string(),
                    phase: "edge-shunt-write".to_string(),
                    message: format!("write_cgmes failed: {e}"),
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // Phase 4: Double round-trip (write → read → write → read)
    // Catches state that only fails on 2nd pass.
    // ------------------------------------------------------------------
    println!("\n=== Double Round-Trip (v2.4.15) ===\n");
    // Use a few representative cases
    let double_rt_cases = ["case9", "case14", "case118", "case300"];
    for case_name in &double_rt_cases {
        let case_dir = cgmes_data_dir().join(case_name);
        if !case_dir.exists() {
            continue;
        }
        let net = match load(&case_dir) {
            Ok(n) => n,
            Err(_) => continue,
        };

        let tmpdir = tempfile::TempDir::new().unwrap();

        // First round-trip
        let rt1_dir = tmpdir.path().join("rt1");
        if write_cgmes(&net, &rt1_dir, CgmesVersion::V2_4_15).is_err() {
            continue;
        }
        let rt1_net = match load(&rt1_dir) {
            Ok(n) => n,
            Err(e) => {
                failures.push(Failure {
                    case: case_name.to_string(),
                    phase: "double-rt1-read".to_string(),
                    message: format!("first re-read failed: {e}"),
                });
                continue;
            }
        };

        // Second round-trip
        let rt2_dir = tmpdir.path().join("rt2");
        match write_cgmes(&rt1_net, &rt2_dir, CgmesVersion::V2_4_15) {
            Ok(()) => match load(&rt2_dir) {
                Ok(rt2_net) => {
                    // Compare rt1 vs rt2 (should be identical since both went through the writer)
                    if rt1_net.n_buses() != rt2_net.n_buses() {
                        failures.push(Failure {
                            case: case_name.to_string(),
                            phase: "double-rt".to_string(),
                            message: format!(
                                "bus count diverged in double rt: rt1={} rt2={}",
                                rt1_net.n_buses(),
                                rt2_net.n_buses()
                            ),
                        });
                    }
                    if rt1_net.n_branches() != rt2_net.n_branches() {
                        failures.push(Failure {
                            case: case_name.to_string(),
                            phase: "double-rt".to_string(),
                            message: format!(
                                "branch count diverged in double rt: rt1={} rt2={}",
                                rt1_net.n_branches(),
                                rt2_net.n_branches()
                            ),
                        });
                    }
                    println!(
                        "  {case_name}: rt1=({},{}) rt2=({},{})",
                        rt1_net.n_buses(),
                        rt1_net.n_branches(),
                        rt2_net.n_buses(),
                        rt2_net.n_branches(),
                    );
                    pass_count += 1;
                }
                Err(e) => {
                    failures.push(Failure {
                        case: case_name.to_string(),
                        phase: "double-rt2-read".to_string(),
                        message: format!("second re-read failed: {e}"),
                    });
                }
            },
            Err(e) => {
                failures.push(Failure {
                    case: case_name.to_string(),
                    phase: "double-rt2-write".to_string(),
                    message: format!("second write failed: {e}"),
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // Phase 5: Cross-version round-trip (write v2 → read → write v3 → read)
    // ------------------------------------------------------------------
    println!("\n=== Cross-Version Round-Trip (v2 -> v3) ===\n");
    let cross_cases = ["case9", "case14", "case118"];
    for case_name in &cross_cases {
        let case_dir = cgmes_data_dir().join(case_name);
        if !case_dir.exists() {
            continue;
        }
        let net = match load(&case_dir) {
            Ok(n) => n,
            Err(_) => continue,
        };

        let tmpdir = tempfile::TempDir::new().unwrap();

        // Write v2 -> read -> write v3 -> read
        let v2_dir = tmpdir.path().join("v2");
        if write_cgmes(&net, &v2_dir, CgmesVersion::V2_4_15).is_err() {
            continue;
        }
        let v2_net = match load(&v2_dir) {
            Ok(n) => n,
            Err(e) => {
                failures.push(Failure {
                    case: case_name.to_string(),
                    phase: "cross-v2-read".to_string(),
                    message: format!("{e}"),
                });
                continue;
            }
        };

        let v3_dir = tmpdir.path().join("v3");
        match write_cgmes(&v2_net, &v3_dir, CgmesVersion::V3_0) {
            Ok(()) => match load(&v3_dir) {
                Ok(v3_net) => {
                    compare_structure(&net, &v3_net, case_name, "cross-v2-v3", &mut failures);
                    println!(
                        "  {case_name}: orig=({},{}) v3=({},{})",
                        net.n_buses(),
                        net.n_branches(),
                        v3_net.n_buses(),
                        v3_net.n_branches(),
                    );
                    pass_count += 1;
                }
                Err(e) => {
                    failures.push(Failure {
                        case: case_name.to_string(),
                        phase: "cross-v3-read".to_string(),
                        message: format!("{e}"),
                    });
                }
            },
            Err(e) => {
                failures.push(Failure {
                    case: case_name.to_string(),
                    phase: "cross-v3-write".to_string(),
                    message: format!("{e}"),
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // Summary
    // ------------------------------------------------------------------
    println!("\n{}", "=".repeat(60));
    println!("=== CGMES STRESS TEST SUMMARY ===");
    println!("{}", "=".repeat(60));
    println!("Passed : {pass_count}");
    println!("Skipped: {skip_count}");
    println!("Failed : {}", failures.len());

    if !failures.is_empty() {
        println!("\n--- FAILURES ---\n");
        for (i, f) in failures.iter().enumerate() {
            println!("  {}: {f}", i + 1);
        }
    }

    println!();

    // Assert at the end so we see the full summary first
    assert!(
        failures.is_empty(),
        "\n\n{} CGMES stress test failure(s) detected. See summary above.\n",
        failures.len()
    );
}

/// Detailed diagnostic: check total load on cases with known load mismatch.
#[test]
#[ignore = "stress diagnostic; works the parser hard and is not meant for core testing"]
fn diag_load_mismatch() {
    let cases = [
        "case6470rte",
        "case6515rte",
        "case13659pegase",
        "case9241pegase",
    ];
    for case_name in &cases {
        let case_dir = cgmes_data_dir().join(case_name);
        if !case_dir.exists() {
            continue;
        }
        let orig = load(&case_dir).unwrap();
        let tmpdir = tempfile::TempDir::new().unwrap();
        let rt_dir = tmpdir.path().join("rt");
        write_cgmes(&orig, &rt_dir, CgmesVersion::V2_4_15).unwrap();
        let rt = load(&rt_dir).unwrap();

        let orig_load: f64 = orig.total_load_mw();
        let rt_load: f64 = rt.total_load_mw();
        let orig_net_pd: f64 = orig.bus_load_p_mw().iter().sum();
        let rt_net_pd: f64 = rt.bus_load_p_mw().iter().sum();

        println!(
            "{case_name}: total_load orig={orig_load:.2} rt={rt_load:.2} | net bus pd orig={orig_net_pd:.2} rt={rt_net_pd:.2} | n_loads orig={} rt={}",
            orig.loads.len(),
            rt.loads.len(),
        );

        // Check if shunt gs contributes to apparent load
        let orig_gs: f64 = orig.buses.iter().map(|b| b.shunt_conductance_mw).sum();
        let rt_gs: f64 = rt.buses.iter().map(|b| b.shunt_conductance_mw).sum();
        let orig_bs: f64 = orig.buses.iter().map(|b| b.shunt_susceptance_mvar).sum();
        let rt_bs: f64 = rt.buses.iter().map(|b| b.shunt_susceptance_mvar).sum();
        println!("  shunt: orig gs={orig_gs:.2} bs={orig_bs:.2} | rt gs={rt_gs:.2} bs={rt_bs:.2}",);
    }
}

/// Detailed diagnostic: compare case14 original vs round-tripped network properties.
/// Match buses by number to handle bus reordering.
#[test]
#[ignore = "stress diagnostic; works the parser hard and is not meant for core testing"]
fn diag_case14_roundtrip_impedances() {
    use std::collections::HashMap;

    let case_dir = cgmes_data_dir().join("case14");
    if !case_dir.exists() {
        return;
    }
    let orig = load(&case_dir).unwrap();

    let tmpdir = tempfile::TempDir::new().unwrap();
    let rt_dir = tmpdir.path().join("rt_diag");
    write_cgmes(&orig, &rt_dir, CgmesVersion::V2_4_15).unwrap();
    let rt = load(&rt_dir).unwrap();

    println!("\n=== CASE14 DIAGNOSTIC ===");
    println!(
        "orig: {} buses, {} branches, {} gens, base_mva={}",
        orig.n_buses(),
        orig.n_branches(),
        orig.generators.len(),
        orig.base_mva,
    );
    println!(
        "rt:   {} buses, {} branches, {} gens, base_mva={}",
        rt.n_buses(),
        rt.n_branches(),
        rt.generators.len(),
        rt.base_mva,
    );

    // Build bus_num → bus maps
    let orig_bus_map: HashMap<u32, &surge_network::network::Bus> =
        orig.buses.iter().map(|b| (b.number, b)).collect();
    let rt_bus_map: HashMap<u32, &surge_network::network::Bus> =
        rt.buses.iter().map(|b| (b.number, b)).collect();

    // Compare bus properties by bus number
    println!("\n--- Bus Comparison (by bus number) ---");
    let orig_pd = orig.bus_load_p_mw();
    let rt_pd = rt.bus_load_p_mw();
    let orig_idx_map: HashMap<u32, usize> = orig
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();
    let rt_idx_map: HashMap<u32, usize> = rt
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();
    let mut bus_nums: Vec<u32> = orig_bus_map.keys().copied().collect();
    bus_nums.sort();
    for &bn in &bus_nums {
        let ob = orig_bus_map[&bn];
        let Some(rb) = rt_bus_map.get(&bn) else {
            println!("  bus #{bn}: MISSING in round-trip");
            continue;
        };
        let vm_diff = (ob.voltage_magnitude_pu - rb.voltage_magnitude_pu).abs();
        let ob_pd = orig_idx_map.get(&bn).map(|&i| orig_pd[i]).unwrap_or(0.0);
        let rb_pd = rt_idx_map.get(&bn).map(|&i| rt_pd[i]).unwrap_or(0.0);
        let pd_diff = (ob_pd - rb_pd).abs();
        let kv_diff = (ob.base_kv - rb.base_kv).abs();
        if vm_diff > 1e-6 || pd_diff > 0.1 || kv_diff > 0.1 {
            println!(
                "  bus #{bn}: vm={:.4}/{:.4} pd={:.1}/{:.1} kv={:.1}/{:.1} type={:?}/{:?}",
                ob.voltage_magnitude_pu,
                rb.voltage_magnitude_pu,
                ob_pd,
                rb_pd,
                ob.base_kv,
                rb.base_kv,
                ob.bus_type,
                rb.bus_type,
            );
        }
    }
    // Check for buses in rt that are not in orig
    for &bn in rt_bus_map.keys() {
        if !orig_bus_map.contains_key(&bn) {
            println!("  bus #{bn}: EXTRA in round-trip (not in original)");
        }
    }

    // Compare branches: build (from,to) → branch maps (may have parallel branches)
    println!("\n--- Branch Comparison (by from-to) ---");
    let orig_br_map: HashMap<(u32, u32), Vec<&surge_network::network::Branch>> = {
        let mut m: HashMap<(u32, u32), Vec<&surge_network::network::Branch>> = HashMap::new();
        for br in &orig.branches {
            let key = (br.from_bus.min(br.to_bus), br.from_bus.max(br.to_bus));
            m.entry(key).or_default().push(br);
        }
        m
    };
    let rt_br_map: HashMap<(u32, u32), Vec<&surge_network::network::Branch>> = {
        let mut m: HashMap<(u32, u32), Vec<&surge_network::network::Branch>> = HashMap::new();
        for br in &rt.branches {
            let key = (br.from_bus.min(br.to_bus), br.from_bus.max(br.to_bus));
            m.entry(key).or_default().push(br);
        }
        m
    };

    let mut br_keys: Vec<(u32, u32)> = orig_br_map.keys().copied().collect();
    br_keys.sort();
    for key in &br_keys {
        let orig_brs = &orig_br_map[key];
        let rt_brs = rt_br_map.get(key);
        if rt_brs.is_none() {
            println!(
                "  br {}->{}: MISSING in round-trip ({} in orig)",
                key.0,
                key.1,
                orig_brs.len(),
            );
            continue;
        }
        let rt_brs = rt_brs.unwrap();
        // Compare first branch of each pair
        for (j, ob) in orig_brs.iter().enumerate() {
            if j >= rt_brs.len() {
                println!("  br {}->{} [{}]: MISSING in round-trip", key.0, key.1, j,);
                continue;
            }
            let rb = rt_brs[j];
            let r_diff = (ob.r - rb.r).abs();
            let x_diff = (ob.x - rb.x).abs();
            let b_diff = (ob.b - rb.b).abs();
            let tap_diff = (ob.tap - rb.tap).abs();
            if r_diff > 1e-6 || x_diff > 1e-6 || b_diff > 1e-6 || tap_diff > 1e-4 {
                println!(
                    "  br {}->{} [{}]: r={:.6}/{:.6} x={:.6}/{:.6} b={:.6}/{:.6} tap={:.4}/{:.4} bmag={:.6}/{:.6}",
                    key.0,
                    key.1,
                    j,
                    ob.r,
                    rb.r,
                    ob.x,
                    rb.x,
                    ob.b,
                    rb.b,
                    ob.tap,
                    rb.tap,
                    ob.b_mag,
                    rb.b_mag,
                );
            }
        }
    }
    for key in rt_br_map.keys() {
        if !orig_br_map.contains_key(key) {
            println!("  br {}->{}: EXTRA in round-trip", key.0, key.1);
        }
    }

    // Compare generators by bus number
    println!("\n--- Generator Comparison (by bus number) ---");
    let orig_gen_map: HashMap<u32, Vec<&surge_network::network::Generator>> = {
        let mut m: HashMap<u32, Vec<&surge_network::network::Generator>> = HashMap::new();
        for g in &orig.generators {
            m.entry(g.bus).or_default().push(g);
        }
        m
    };
    let rt_gen_map: HashMap<u32, Vec<&surge_network::network::Generator>> = {
        let mut m: HashMap<u32, Vec<&surge_network::network::Generator>> = HashMap::new();
        for g in &rt.generators {
            m.entry(g.bus).or_default().push(g);
        }
        m
    };
    let mut gen_buses: Vec<u32> = orig_gen_map.keys().copied().collect();
    gen_buses.sort();
    for &bus in &gen_buses {
        let ogs = &orig_gen_map[&bus];
        let rgs = rt_gen_map.get(&bus);
        if rgs.is_none() {
            println!("  gen bus={bus}: MISSING in round-trip");
            continue;
        }
        let rgs = rgs.unwrap();
        for (j, og) in ogs.iter().enumerate() {
            if j >= rgs.len() {
                println!("  gen bus={bus} [{j}]: MISSING in round-trip");
                continue;
            }
            let rg = rgs[j];
            let vs_diff = (og.voltage_setpoint_pu - rg.voltage_setpoint_pu).abs();
            let pg_diff = (og.p - rg.p).abs();
            if vs_diff > 1e-4 || pg_diff > 0.1 {
                println!(
                    "  gen bus={bus} [{j}]: vs={:.4}/{:.4} pg={:.1}/{:.1}",
                    og.voltage_setpoint_pu, rg.voltage_setpoint_pu, og.p, rg.p,
                );
            }
        }
    }
}
