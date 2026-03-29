// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Element-by-element N-1 contingency validation against MATPOWER reference.
//!
//! Loads pre-generated MATPOWER N-1 results for case_ACTIVSg500 (500 buses, 597 branches)
//! and compares per-contingency Vm, Va, and branch flows (Sf) against Surge's results.
//!
//! MATPOWER settings: NR, tol=1e-8, max_iter=100, enforce_q_limits=0, warm start.
//! Reference generated offline from MATPOWER and checked into this repository.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;
use surge_ac::AcPfOptions;
use surge_contingency::{ContingencyOptions, ScreeningMode, analyze_n1_branch};

// ---------------------------------------------------------------------------
// JSON reference data structures
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct N1Reference {
    n_buses: usize,
    n_branches: usize,
    bus_numbers: Vec<u32>,
    branch_from_bus: Vec<u32>,
    branch_to_bus: Vec<u32>,
    base_vm: Vec<f64>,
    base_va: Vec<f64>,
    #[allow(dead_code)]
    base_sf: Vec<f64>,
    contingencies: Vec<ContingencyRef>,
}

#[derive(Deserialize)]
struct ContingencyRef {
    branch_index: usize,
    from_bus: u32,
    to_bus: u32,
    converged: bool,
    #[serde(default)]
    voltage_magnitude_pu: Vec<f64>,
    #[serde(default)]
    voltage_angle_rad: Vec<f64>,
    #[serde(default)]
    sf: Vec<f64>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/data")
}

fn data_available() -> bool {
    test_data_dir().join("case_ACTIVSg500.m").exists()
}

fn reference_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/references/n1_case_ACTIVSg500.json")
}

fn load_reference() -> N1Reference {
    let path = reference_path();
    assert!(path.exists(), "Reference file not found: {path:?}");
    let data = std::fs::read_to_string(&path).expect("Failed to read reference JSON");
    serde_json::from_str(&data).expect("Failed to parse reference JSON")
}

// ---------------------------------------------------------------------------
// Tolerance constants
// ---------------------------------------------------------------------------

const VM_TOL: f64 = 1e-4; // p.u.
const VA_TOL: f64 = 1e-4; // radians
const SF_ABS_TOL: f64 = 0.1; // MVA absolute
const SF_REL_TOL: f64 = 0.001; // 0.1% relative

fn sf_within_tol(surge: f64, matpower: f64) -> bool {
    let diff = (surge - matpower).abs();
    diff <= SF_ABS_TOL || diff <= SF_REL_TOL * matpower.abs()
}

// ---------------------------------------------------------------------------
// Main validation test
// ---------------------------------------------------------------------------

#[test]
fn test_n1_case_activsg500_matpower_validation() {
    if !data_available() {
        eprintln!("SKIP: case_ACTIVSg500.m not found in tests/data/");
        return;
    }
    if !reference_path().exists() {
        eprintln!("SKIP: reference JSON not found");
        return;
    }

    // Load reference
    let reference = load_reference();
    assert_eq!(reference.n_buses, 500);
    assert_eq!(reference.n_branches, 597);

    // Load case and run Surge N-1
    let case_path = test_data_dir().join("case_ACTIVSg500.m");
    let network = surge_io::matpower::load(&case_path).expect("Failed to parse case file");
    assert_eq!(network.buses.len(), reference.n_buses);
    assert_eq!(network.branches.len(), reference.n_branches);

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

    let analysis = analyze_n1_branch(&network, &opts).expect("N-1 analysis failed");

    // Build bus-number mapping: MATPOWER internal order → Surge internal order
    // Both tools use the same .m file, so bus order should match, but let's be safe
    let surge_bus_map: HashMap<u32, usize> = network.bus_index_map();
    // Build Surge results indexed by branch_index (from contingency ID)
    let mut surge_results: HashMap<usize, &surge_contingency::ContingencyResult> = HashMap::new();
    for r in &analysis.results {
        let br_idx: usize = r.id.strip_prefix("branch_").unwrap().parse().unwrap();
        surge_results.insert(br_idx, r);
    }

    // Tracking
    let mut n_compared = 0;
    let mut n_skipped_island = 0;
    let mut n_skipped_diverged = 0;
    let mut n_convergence_mismatch = 0;
    let mut worst_vm_diff = 0.0_f64;
    let mut worst_va_diff = 0.0_f64;
    let mut worst_sf_diff = 0.0_f64;
    let mut worst_vm_ctg = String::new();
    let mut worst_va_ctg = String::new();
    let mut worst_sf_ctg = String::new();
    let mut vm_fails = Vec::new();
    let mut va_fails = Vec::new();
    let mut sf_fails = Vec::new();

    for ref_ctg in &reference.contingencies {
        let br_idx = ref_ctg.branch_index;
        let surge_r = surge_results.get(&br_idx).unwrap_or_else(|| {
            panic!("Surge missing contingency for branch_index={br_idx}");
        });

        // Verify branch identity
        let surge_br = &network.branches[br_idx];
        assert_eq!(
            surge_br.from_bus, ref_ctg.from_bus,
            "Branch {br_idx} from_bus mismatch"
        );
        assert_eq!(
            surge_br.to_bus, ref_ctg.to_bus,
            "Branch {br_idx} to_bus mismatch"
        );

        // Island handling: MATPOWER diverges, Surge solves islands independently
        if !ref_ctg.converged {
            if surge_r.n_islands > 1 {
                // Expected: MATPOWER diverges on island-creating contingencies
                n_skipped_island += 1;
            } else if !surge_r.converged {
                // Both diverged — fine
                n_skipped_diverged += 1;
            } else {
                // MATPOWER diverged but Surge converged (non-island) — unexpected
                // Could be a difficult case where Surge's warm start helps
                n_skipped_diverged += 1;
                eprintln!(
                    "NOTE: branch_{br_idx} ({}->{}) MATPOWER diverged but Surge converged (non-island)",
                    ref_ctg.from_bus, ref_ctg.to_bus
                );
            }
            continue;
        }

        // Island cases where MATPOWER "converges" by luck: isolated no-load buses
        // have zero mismatch so NR trivially succeeds, but voltages on isolated buses
        // are meaningless warm-start artifacts. Skip these — neither tool has a real
        // solution for the disconnected component.
        if surge_r.n_islands > 1 {
            n_skipped_island += 1;
            continue;
        }

        // MATPOWER converged — Surge must also converge
        if !surge_r.converged {
            n_convergence_mismatch += 1;
            eprintln!(
                "MISMATCH: branch_{br_idx} ({}->{}) MATPOWER converged but Surge diverged",
                ref_ctg.from_bus, ref_ctg.to_bus
            );
            continue;
        }

        // Both converged, no islands — compare element-by-element
        let surge_vm = surge_r.post_vm.as_ref().expect("post_vm missing");
        let surge_va = surge_r.post_va.as_ref().expect("post_va missing");
        let surge_sf = surge_r
            .post_branch_flows
            .as_ref()
            .expect("post_branch_flows missing");

        // Compare Vm bus-by-bus (using bus number mapping)
        for (mp_idx, &ref_vm) in ref_ctg.voltage_magnitude_pu.iter().enumerate() {
            let bus_num = reference.bus_numbers[mp_idx];
            let surge_idx = *surge_bus_map.get(&bus_num).unwrap_or_else(|| {
                panic!("Bus {bus_num} not found in Surge network");
            });
            let s_vm = surge_vm[surge_idx];
            let diff = (s_vm - ref_vm).abs();
            if diff > worst_vm_diff {
                worst_vm_diff = diff;
                worst_vm_ctg = format!("branch_{br_idx} bus {bus_num}");
            }
            if diff > VM_TOL {
                vm_fails.push(format!(
                    "branch_{br_idx} bus {bus_num}: Surge={s_vm:.8}, MATPOWER={ref_vm:.8}, diff={diff:.2e}"
                ));
            }
        }

        // Compare Va bus-by-bus
        for (mp_idx, &ref_va) in ref_ctg.voltage_angle_rad.iter().enumerate() {
            let bus_num = reference.bus_numbers[mp_idx];
            let surge_idx = *surge_bus_map.get(&bus_num).unwrap();
            let s_va = surge_va[surge_idx];
            let diff = (s_va - ref_va).abs();
            if diff > worst_va_diff {
                worst_va_diff = diff;
                worst_va_ctg = format!("branch_{br_idx} bus {bus_num}");
            }
            if diff > VA_TOL {
                va_fails.push(format!(
                    "branch_{br_idx} bus {bus_num}: Surge={s_va:.8}, MATPOWER={ref_va:.8}, diff={diff:.2e}"
                ));
            }
        }

        // Compare Sf branch-by-branch
        for (mp_br_idx, &ref_sf) in ref_ctg.sf.iter().enumerate() {
            // Skip the tripped branch (both should be zero)
            if mp_br_idx == br_idx {
                continue;
            }
            let s_sf = surge_sf[mp_br_idx];
            let diff = (s_sf - ref_sf).abs();
            if diff > worst_sf_diff {
                worst_sf_diff = diff;
                worst_sf_ctg = format!(
                    "branch_{br_idx} line {}->{}",
                    reference.branch_from_bus[mp_br_idx], reference.branch_to_bus[mp_br_idx]
                );
            }
            if !sf_within_tol(s_sf, ref_sf) {
                sf_fails.push(format!(
                    "branch_{br_idx} line {}->{}: Surge={s_sf:.4} MVA, MATPOWER={ref_sf:.4} MVA, diff={diff:.4} MVA",
                    reference.branch_from_bus[mp_br_idx],
                    reference.branch_to_bus[mp_br_idx]
                ));
            }
        }

        n_compared += 1;
    }

    // Report summary
    eprintln!("\n=== N-1 Validation Summary (case_ACTIVSg500) ===");
    eprintln!("Total contingencies:    {}", reference.contingencies.len());
    eprintln!("Compared (both conv.):  {n_compared}");
    eprintln!("Skipped (island):       {n_skipped_island}");
    eprintln!("Skipped (diverged):     {n_skipped_diverged}");
    eprintln!("Convergence mismatch:   {n_convergence_mismatch}");
    eprintln!("Worst Vm diff:          {worst_vm_diff:.2e} p.u. ({worst_vm_ctg})");
    eprintln!("Worst Va diff:          {worst_va_diff:.2e} rad ({worst_va_ctg})");
    eprintln!("Worst Sf diff:          {worst_sf_diff:.2e} MVA ({worst_sf_ctg})");
    eprintln!("Vm violations (>{VM_TOL}):  {}", vm_fails.len());
    eprintln!("Va violations (>{VA_TOL}):  {}", va_fails.len());
    eprintln!("Sf violations:          {}", sf_fails.len());
    eprintln!(
        "Wall time:              {:.3}s",
        analysis.summary.solve_time_secs
    );

    // Print first few failures for diagnostics
    if !vm_fails.is_empty() {
        eprintln!("\n--- Vm failures (first 10) ---");
        for f in vm_fails.iter().take(10) {
            eprintln!("  {f}");
        }
    }
    if !va_fails.is_empty() {
        eprintln!("\n--- Va failures (first 10) ---");
        for f in va_fails.iter().take(10) {
            eprintln!("  {f}");
        }
    }
    if !sf_fails.is_empty() {
        eprintln!("\n--- Sf failures (first 10) ---");
        for f in sf_fails.iter().take(10) {
            eprintln!("  {f}");
        }
    }

    // Hard assertions
    assert_eq!(
        n_convergence_mismatch, 0,
        "MATPOWER-converged contingencies must also converge in Surge"
    );
    assert!(
        n_compared >= 250,
        "At least 250 non-island contingencies should be compared, got {n_compared}"
    );
    assert!(
        vm_fails.is_empty(),
        "{} Vm violations exceed tolerance {VM_TOL} p.u.",
        vm_fails.len()
    );
    assert!(
        va_fails.is_empty(),
        "{} Va violations exceed tolerance {VA_TOL} rad",
        va_fails.len()
    );
    assert!(
        sf_fails.is_empty(),
        "{} Sf violations exceed tolerance",
        sf_fails.len()
    );
}

// ---------------------------------------------------------------------------
// Base case voltage validation (sanity check reference data)
// ---------------------------------------------------------------------------

#[test]
fn test_n1_case_activsg500_base_case() {
    if !data_available() {
        eprintln!("SKIP: case_ACTIVSg500.m not found");
        return;
    }
    if !reference_path().exists() {
        eprintln!("SKIP: reference JSON not found");
        return;
    }

    let reference = load_reference();
    let case_path = test_data_dir().join("case_ACTIVSg500.m");
    let network = surge_io::matpower::load(&case_path).expect("parse");

    // Solve Surge base case
    let acpf_opts = AcPfOptions {
        tolerance: 1e-8,
        max_iterations: 100,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = surge_ac::solve_ac_pf_kernel(&network, &acpf_opts).expect("Base case must converge");

    let surge_bus_map = network.bus_index_map();

    let mut worst_vm = 0.0_f64;
    let mut worst_va = 0.0_f64;
    for (mp_idx, &bus_num) in reference.bus_numbers.iter().enumerate() {
        let surge_idx = surge_bus_map[&bus_num];
        let vm_diff = (sol.voltage_magnitude_pu[surge_idx] - reference.base_vm[mp_idx]).abs();
        let va_diff = (sol.voltage_angle_rad[surge_idx] - reference.base_va[mp_idx]).abs();
        worst_vm = worst_vm.max(vm_diff);
        worst_va = worst_va.max(va_diff);
    }
    eprintln!("Base case: worst Vm diff = {worst_vm:.2e}, worst Va diff = {worst_va:.2e}");
    assert!(worst_vm < 1e-6, "Base case Vm mismatch: {worst_vm:.2e}");
    assert!(worst_va < 1e-6, "Base case Va mismatch: {worst_va:.2e}");
}
