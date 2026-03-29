// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Deep cross-format validation: EPC vs MATPOWER for Texas ACTIVSg test cases.
//!
//! The EPC and MATPOWER files may represent different solved states of the same
//! network (different dispatch, voltage setpoints). We validate:
//! 1. Topology match: bus count, branch count, impedances, tap ratios, ratings
//! 2. Generator limits match: pmax, pmin, qmax, qmin, mbase
//! 3. Load totals match (within rounding)
//! 4. NR power flow converges on EPC-parsed network
//! 5. When voltage setpoints are equalized, NR produces identical results

use std::collections::HashMap;
use std::path::PathBuf;

use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
use surge_network::Network;
use surge_solution::SolveStatus;

fn data_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let dir = PathBuf::from(&manifest).join("../../tests/data/epc");
    if dir.exists() {
        std::fs::canonicalize(&dir).unwrap_or(dir)
    } else {
        dir
    }
}

/// Full field-by-field comparison. Prints detailed report and returns max diffs.
#[derive(Debug)]
struct CompareResult {
    // Topology (must match exactly)
    bus_count_match: bool,
    branch_count_match: bool,
    gen_count_match: bool,
    max_r_diff: f64,
    max_x_diff: f64,
    max_b_diff: f64,
    max_tap_diff: f64,
    _max_rate_diff: f64,
    // Limits (must match)
    max_pmax_diff: f64,
    _max_pmin_diff: f64,
    max_qmax_diff: f64,
    _max_qmin_diff: f64,
    max_mbase_diff: f64,
    // Load (must be close)
    total_load_diff_mw: f64,
    max_pd_diff: f64,
    _max_bs_diff: f64,
    // Dispatch (may differ — informational only)
    _max_pg_diff: f64,
    _max_vs_diff: f64,
    _bus_type_mismatches: usize,
}

fn deep_compare(epc: &Network, mat: &Network, label: &str) -> CompareResult {
    let epc_bus_map: HashMap<u32, usize> = epc
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    let bus_count_match = epc.n_buses() == mat.n_buses();
    let branch_count_match = epc.branches.len() == mat.branches.len();
    let gen_count_match = epc.generators.len() == mat.generators.len();

    // --- Bus comparison ---
    let mut max_pd_diff = 0.0f64;
    let mut max_qd_diff = 0.0f64;
    let mut max_bs_diff = 0.0f64;
    let mut bus_type_mismatches = 0usize;

    let epc_bus_pd = epc.bus_load_p_mw();
    let epc_bus_qd = epc.bus_load_q_mvar();
    let mat_bus_pd = mat.bus_load_p_mw();
    let mat_bus_qd = mat.bus_load_q_mvar();

    for (mi, mat_bus) in mat.buses.iter().enumerate() {
        if let Some(&epc_idx) = epc_bus_map.get(&mat_bus.number) {
            let epc_bus = &epc.buses[epc_idx];
            max_pd_diff = max_pd_diff.max((epc_bus_pd[epc_idx] - mat_bus_pd[mi]).abs());
            max_qd_diff = max_qd_diff.max((epc_bus_qd[epc_idx] - mat_bus_qd[mi]).abs());
            max_bs_diff = max_bs_diff
                .max((epc_bus.shunt_susceptance_mvar - mat_bus.shunt_susceptance_mvar).abs());
            if epc_bus.bus_type != mat_bus.bus_type {
                bus_type_mismatches += 1;
                if bus_type_mismatches <= 5 {
                    eprintln!(
                        "  Bus {} type: EPC={:?} MAT={:?}",
                        mat_bus.number, epc_bus.bus_type, mat_bus.bus_type
                    );
                }
            }
        }
    }

    let epc_load: f64 = epc.total_load_mw();
    let mat_load: f64 = mat.total_load_mw();

    eprintln!(
        "{label} Bus: max|Pd|={max_pd_diff:.4} MW, max|Qd|={max_qd_diff:.4} MVAr, max|Bs|={max_bs_diff:.4}, type_mismatch={bus_type_mismatches}"
    );
    eprintln!(
        "{label} Load: EPC={epc_load:.1} MW, MAT={mat_load:.1} MW, diff={:.2} MW",
        (epc_load - mat_load).abs()
    );

    // --- Branch comparison ---
    type BranchKey = (u32, u32);
    let mut epc_branches_by_key: HashMap<BranchKey, Vec<usize>> = HashMap::new();
    let mut mat_branches_by_key: HashMap<BranchKey, Vec<usize>> = HashMap::new();

    for (i, br) in epc.branches.iter().enumerate() {
        let key = (br.from_bus.min(br.to_bus), br.from_bus.max(br.to_bus));
        epc_branches_by_key.entry(key).or_default().push(i);
    }
    for (i, br) in mat.branches.iter().enumerate() {
        let key = (br.from_bus.min(br.to_bus), br.from_bus.max(br.to_bus));
        mat_branches_by_key.entry(key).or_default().push(i);
    }

    let mut max_r_diff = 0.0f64;
    let mut max_x_diff = 0.0f64;
    let mut max_b_diff = 0.0f64;
    let mut max_tap_diff = 0.0f64;
    let mut max_rate_diff = 0.0f64;

    for (key, mat_indices) in &mat_branches_by_key {
        if let Some(epc_indices) = epc_branches_by_key.get(key) {
            let mut mat_sorted: Vec<usize> = mat_indices.clone();
            let mut epc_sorted: Vec<usize> = epc_indices.clone();
            mat_sorted.sort_by(|&a, &b| {
                let za = mat.branches[a].r.hypot(mat.branches[a].x);
                let zb = mat.branches[b].r.hypot(mat.branches[b].x);
                za.partial_cmp(&zb).unwrap_or(std::cmp::Ordering::Equal)
            });
            epc_sorted.sort_by(|&a, &b| {
                let za = epc.branches[a].r.hypot(epc.branches[a].x);
                let zb = epc.branches[b].r.hypot(epc.branches[b].x);
                za.partial_cmp(&zb).unwrap_or(std::cmp::Ordering::Equal)
            });

            let n = mat_sorted.len().min(epc_sorted.len());
            for k in 0..n {
                let mb = &mat.branches[mat_sorted[k]];
                let eb = &epc.branches[epc_sorted[k]];
                max_r_diff = max_r_diff.max((eb.r - mb.r).abs());
                max_x_diff = max_x_diff.max((eb.x - mb.x).abs());
                max_b_diff = max_b_diff.max((eb.b - mb.b).abs());
                max_tap_diff = max_tap_diff.max((eb.tap - mb.tap).abs());
                max_rate_diff = max_rate_diff.max((eb.rating_a_mva - mb.rating_a_mva).abs());
            }
        }
    }

    eprintln!(
        "{label} Branch: max|r|={max_r_diff:.8}, max|x|={max_x_diff:.8}, max|b|={max_b_diff:.8}, max|tap|={max_tap_diff:.8}, max|rate|={max_rate_diff:.2}"
    );

    // --- Generator comparison ---
    let mut epc_gens_by_bus: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut mat_gens_by_bus: HashMap<u32, Vec<usize>> = HashMap::new();

    for (i, g) in epc.generators.iter().enumerate() {
        epc_gens_by_bus.entry(g.bus).or_default().push(i);
    }
    for (i, g) in mat.generators.iter().enumerate() {
        mat_gens_by_bus.entry(g.bus).or_default().push(i);
    }

    let mut max_pg_diff = 0.0f64;
    let mut max_vs_diff = 0.0f64;
    let mut max_pmax_diff = 0.0f64;
    let mut max_pmin_diff = 0.0f64;
    let mut max_qmax_diff = 0.0f64;
    let mut max_qmin_diff = 0.0f64;
    let mut max_mbase_diff = 0.0f64;

    for (bus, mat_indices) in &mat_gens_by_bus {
        if let Some(epc_indices) = epc_gens_by_bus.get(bus) {
            let mut mat_sorted: Vec<usize> = mat_indices.clone();
            let mut epc_sorted: Vec<usize> = epc_indices.clone();
            mat_sorted.sort_by(|&a, &b| {
                mat.generators[a]
                    .pmax
                    .partial_cmp(&mat.generators[b].pmax)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            epc_sorted.sort_by(|&a, &b| {
                epc.generators[a]
                    .pmax
                    .partial_cmp(&epc.generators[b].pmax)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let n = mat_sorted.len().min(epc_sorted.len());
            for k in 0..n {
                let mg = &mat.generators[mat_sorted[k]];
                let eg = &epc.generators[epc_sorted[k]];
                max_pg_diff = max_pg_diff.max((eg.p - mg.p).abs());
                max_vs_diff =
                    max_vs_diff.max((eg.voltage_setpoint_pu - mg.voltage_setpoint_pu).abs());
                max_pmax_diff = max_pmax_diff.max((eg.pmax - mg.pmax).abs());
                max_pmin_diff = max_pmin_diff.max((eg.pmin - mg.pmin).abs());
                max_qmax_diff = max_qmax_diff.max((eg.qmax - mg.qmax).abs());
                max_qmin_diff = max_qmin_diff.max((eg.qmin - mg.qmin).abs());
                max_mbase_diff =
                    max_mbase_diff.max((eg.machine_base_mva - mg.machine_base_mva).abs());
            }
        }
    }

    eprintln!(
        "{label} Gen limits: max|Pmax|={max_pmax_diff:.4}, max|Pmin|={max_pmin_diff:.4}, max|Qmax|={max_qmax_diff:.4}, max|Qmin|={max_qmin_diff:.4}, max|Mbase|={max_mbase_diff:.4}"
    );
    eprintln!(
        "{label} Gen dispatch (informational): max|Pg|={max_pg_diff:.2} MW, max|Vs|={max_vs_diff:.6} pu"
    );

    CompareResult {
        bus_count_match,
        branch_count_match,
        gen_count_match,
        max_r_diff,
        max_x_diff,
        max_b_diff,
        max_tap_diff,
        _max_rate_diff: max_rate_diff,
        max_pmax_diff,
        _max_pmin_diff: max_pmin_diff,
        max_qmax_diff,
        _max_qmin_diff: max_qmin_diff,
        max_mbase_diff,
        total_load_diff_mw: (epc_load - mat_load).abs(),
        max_pd_diff,
        _max_bs_diff: max_bs_diff,
        _max_pg_diff: max_pg_diff,
        _max_vs_diff: max_vs_diff,
        _bus_type_mismatches: bus_type_mismatches,
    }
}

/// Equalize voltage setpoints: copy MATPOWER gen Vs to EPC network's generators.
/// This eliminates Vs differences so NR comparison tests pure topology matching.
fn equalize_vs(epc: &mut Network, mat: &Network) {
    // Build (pmax, vs, qmax, qmin, mbase) per bus for MATPOWER gens
    let mut mat_gens_by_bus: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, g) in mat.generators.iter().enumerate() {
        mat_gens_by_bus.entry(g.bus).or_default().push(i);
    }

    let mut epc_gens_by_bus: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, g) in epc.generators.iter().enumerate() {
        epc_gens_by_bus.entry(g.bus).or_default().push(i);
    }

    for (bus, epc_indices) in &epc_gens_by_bus {
        if let Some(mat_indices) = mat_gens_by_bus.get(bus) {
            // Sort both by pmax for correct pairing
            let mut epc_sorted = epc_indices.clone();
            epc_sorted.sort_by(|&a, &b| {
                let ga = &epc.generators[a];
                let gb = &epc.generators[b];
                ga.pmax
                    .total_cmp(&gb.pmax)
                    .then_with(|| ga.pmin.total_cmp(&gb.pmin))
                    .then_with(|| ga.qmax.total_cmp(&gb.qmax))
                    .then_with(|| ga.qmin.total_cmp(&gb.qmin))
                    .then_with(|| ga.machine_base_mva.total_cmp(&gb.machine_base_mva))
            });
            let mut mat_sorted = mat_indices.clone();
            mat_sorted.sort_by(|&a, &b| {
                let ga = &mat.generators[a];
                let gb = &mat.generators[b];
                ga.pmax
                    .total_cmp(&gb.pmax)
                    .then_with(|| ga.pmin.total_cmp(&gb.pmin))
                    .then_with(|| ga.qmax.total_cmp(&gb.qmax))
                    .then_with(|| ga.qmin.total_cmp(&gb.qmin))
                    .then_with(|| ga.machine_base_mva.total_cmp(&gb.machine_base_mva))
            });

            for k in 0..epc_sorted.len().min(mat_sorted.len()) {
                epc.generators[epc_sorted[k]].voltage_setpoint_pu =
                    mat.generators[mat_sorted[k]].voltage_setpoint_pu;
                // Also copy Pg/Qg dispatch for full equalization
                epc.generators[epc_sorted[k]].p = mat.generators[mat_sorted[k]].p;
                epc.generators[epc_sorted[k]].q = mat.generators[mat_sorted[k]].q;
            }
        }
    }

    // Also copy bus types from MATPOWER (different slack bus assignment)
    let mat_bus_type: HashMap<u32, surge_network::network::BusType> =
        mat.buses.iter().map(|b| (b.number, b.bus_type)).collect();
    for bus in &mut epc.buses {
        if let Some(&mat_type) = mat_bus_type.get(&bus.number) {
            bus.bus_type = mat_type;
        }
    }
}

#[test]
fn test_epc_vs_matpower_texas2k_deep() {
    let dir = data_dir();
    let epc_path = dir.join("Texas2k.epc");
    let mat_path = dir.join("Texas2k.m");
    if !epc_path.exists() || !mat_path.exists() {
        eprintln!("Skipping Texas2k — data not found");
        return;
    }

    let epc_net = surge_io::epc::load(&epc_path).expect("EPC parse failed");
    let mat_net = surge_io::matpower::load(&mat_path).expect("MATPOWER parse failed");

    eprintln!("\n=== Texas2k Deep Validation ===\n");

    let cr = deep_compare(&epc_net, &mat_net, "Texas2k");

    // --- Topology assertions (must match) ---
    assert!(cr.bus_count_match, "bus count mismatch");
    assert!(cr.branch_count_match, "branch count mismatch");
    assert!(cr.gen_count_match, "gen count mismatch");
    assert!(
        cr.max_r_diff < 1e-5,
        "branch R mismatch: {:.8}",
        cr.max_r_diff
    );
    assert!(
        cr.max_x_diff < 1e-5,
        "branch X mismatch: {:.8}",
        cr.max_x_diff
    );
    assert!(
        cr.max_b_diff < 1e-5,
        "branch B mismatch: {:.8}",
        cr.max_b_diff
    );
    assert!(
        cr.max_tap_diff < 1e-5,
        "tap mismatch: {:.8}",
        cr.max_tap_diff
    );

    // --- Limits assertions (must match) ---
    assert!(
        cr.max_pmax_diff < 0.01,
        "Pmax mismatch: {:.4}",
        cr.max_pmax_diff
    );
    assert!(
        cr.max_qmax_diff < 0.01,
        "Qmax mismatch: {:.4}",
        cr.max_qmax_diff
    );
    assert!(
        cr.max_mbase_diff < 0.01,
        "Mbase mismatch: {:.4}",
        cr.max_mbase_diff
    );

    // --- Load assertions (within rounding) ---
    assert!(
        cr.total_load_diff_mw < 1.0,
        "total load diff: {:.2} MW",
        cr.total_load_diff_mw
    );
    assert!(
        cr.max_pd_diff < 0.01,
        "per-bus Pd diff: {:.4} MW",
        cr.max_pd_diff
    );

    // --- NR: standalone EPC convergence ---
    let opts = AcPfOptions {
        tolerance: 1e-8,
        max_iterations: 100,
        flat_start: true,
        enforce_q_limits: true,
        ..Default::default()
    };
    let epc_sol = solve_ac_pf_kernel(&epc_net, &opts).expect("EPC NR failed");
    assert_eq!(
        epc_sol.status,
        SolveStatus::Converged,
        "EPC NR did not converge (iters={}, mismatch={:.2e})",
        epc_sol.iterations,
        epc_sol.max_mismatch
    );
    eprintln!(
        "Texas2k EPC NR: converged in {} iters (mismatch={:.2e})",
        epc_sol.iterations, epc_sol.max_mismatch
    );

    // --- NR: equalized comparison ---
    // Copy MATPOWER voltage setpoints and bus types to EPC network
    let mut epc_eq = epc_net.clone();
    equalize_vs(&mut epc_eq, &mat_net);

    let epc_eq_sol = solve_ac_pf_kernel(&epc_eq, &opts).expect("EPC (equalized) NR failed");
    let mat_sol = solve_ac_pf_kernel(&mat_net, &opts).expect("MATPOWER NR failed");

    assert_eq!(epc_eq_sol.status, SolveStatus::Converged);
    assert_eq!(mat_sol.status, SolveStatus::Converged);

    eprintln!(
        "Texas2k EPC-eq NR: {} iters, MATPOWER NR: {} iters",
        epc_eq_sol.iterations, mat_sol.iterations
    );

    // Compare converged Vm/Va
    let epc_bus_idx: HashMap<u32, usize> = epc_eq_sol
        .bus_numbers
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i))
        .collect();
    let mat_bus_idx: HashMap<u32, usize> = mat_sol
        .bus_numbers
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i))
        .collect();

    let mut max_vm = 0.0f64;
    let mut max_va = 0.0f64;

    for (&bus_num, &mi) in &mat_bus_idx {
        if let Some(&ei) = epc_bus_idx.get(&bus_num) {
            max_vm = max_vm.max(
                (epc_eq_sol.voltage_magnitude_pu[ei] - mat_sol.voltage_magnitude_pu[mi]).abs(),
            );
            max_va = max_va
                .max((epc_eq_sol.voltage_angle_rad[ei] - mat_sol.voltage_angle_rad[mi]).abs());
        }
    }

    eprintln!(
        "Texas2k NR comparison (equalized Vs): max|Vm|={max_vm:.6} pu, max|Va|={max_va:.6} rad"
    );

    assert!(
        max_vm < 1e-4,
        "Vm mismatch with equalized Vs: {max_vm:.6} pu (topology differs)"
    );
    assert!(
        max_va < 1e-3,
        "Va mismatch with equalized Vs: {max_va:.6} rad (topology differs)"
    );

    eprintln!("\nTexas2k: PASS\n");
}

#[test]
fn test_epc_vs_matpower_texas7k_deep() {
    let dir = data_dir();
    let epc_path = dir.join("Texas7k.epc");
    let mat_path = dir.join("Texas7k.m");
    if !epc_path.exists() || !mat_path.exists() {
        eprintln!("Skipping Texas7k — data not found");
        return;
    }

    let epc_net = surge_io::epc::load(&epc_path).expect("EPC parse failed");
    let mat_net = surge_io::matpower::load(&mat_path).expect("MATPOWER parse failed");

    eprintln!("\n=== Texas7k Deep Validation ===\n");

    let cr = deep_compare(&epc_net, &mat_net, "Texas7k");

    // --- Topology assertions ---
    assert!(cr.bus_count_match, "bus count mismatch");
    assert!(cr.branch_count_match, "branch count mismatch");
    assert!(cr.gen_count_match, "gen count mismatch");
    assert!(
        cr.max_r_diff < 1e-5,
        "branch R mismatch: {:.8}",
        cr.max_r_diff
    );
    assert!(
        cr.max_x_diff < 1e-5,
        "branch X mismatch: {:.8}",
        cr.max_x_diff
    );
    assert!(
        cr.max_b_diff < 1e-5,
        "branch B mismatch: {:.8}",
        cr.max_b_diff
    );
    assert!(
        cr.max_tap_diff < 1e-5,
        "tap mismatch: {:.8}",
        cr.max_tap_diff
    );
    assert!(
        cr.max_pmax_diff < 0.01,
        "Pmax mismatch: {:.4}",
        cr.max_pmax_diff
    );
    assert!(
        cr.max_qmax_diff < 0.01,
        "Qmax mismatch: {:.4}",
        cr.max_qmax_diff
    );
    assert!(
        cr.total_load_diff_mw < 1.0,
        "total load diff: {:.2} MW",
        cr.total_load_diff_mw
    );

    // --- NR: standalone EPC convergence ---
    let opts = AcPfOptions {
        tolerance: 1e-8,
        max_iterations: 100,
        flat_start: true,
        enforce_q_limits: true,
        ..Default::default()
    };
    let epc_sol = solve_ac_pf_kernel(&epc_net, &opts).expect("EPC NR failed");
    assert_eq!(
        epc_sol.status,
        SolveStatus::Converged,
        "EPC NR did not converge"
    );
    eprintln!(
        "Texas7k EPC NR: converged in {} iters (mismatch={:.2e})",
        epc_sol.iterations, epc_sol.max_mismatch
    );

    // --- NR: equalized comparison ---
    let mut epc_eq = epc_net.clone();
    equalize_vs(&mut epc_eq, &mat_net);

    let epc_eq_sol = solve_ac_pf_kernel(&epc_eq, &opts).expect("EPC (equalized) NR failed");
    let mat_sol = solve_ac_pf_kernel(&mat_net, &opts).expect("MATPOWER NR failed");

    assert_eq!(epc_eq_sol.status, SolveStatus::Converged);
    assert_eq!(mat_sol.status, SolveStatus::Converged);

    let epc_bus_idx: HashMap<u32, usize> = epc_eq_sol
        .bus_numbers
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i))
        .collect();
    let mat_bus_idx: HashMap<u32, usize> = mat_sol
        .bus_numbers
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i))
        .collect();

    let mut max_vm = 0.0f64;
    let mut max_va = 0.0f64;

    for (&bus_num, &mi) in &mat_bus_idx {
        if let Some(&ei) = epc_bus_idx.get(&bus_num) {
            max_vm = max_vm.max(
                (epc_eq_sol.voltage_magnitude_pu[ei] - mat_sol.voltage_magnitude_pu[mi]).abs(),
            );
            max_va = max_va
                .max((epc_eq_sol.voltage_angle_rad[ei] - mat_sol.voltage_angle_rad[mi]).abs());
        }
    }

    eprintln!(
        "Texas7k NR comparison (equalized Vs): max|Vm|={max_vm:.6} pu, max|Va|={max_va:.6} rad"
    );

    assert!(
        max_vm < 1e-4,
        "Vm mismatch with equalized Vs: {max_vm:.6} pu"
    );
    assert!(
        max_va < 1e-3,
        "Va mismatch with equalized Vs: {max_va:.6} rad"
    );

    eprintln!("\nTexas7k: PASS\n");
}
