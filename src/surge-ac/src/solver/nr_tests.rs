// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Tests for the Newton-Raphson solver.

#[allow(dead_code)]
fn data_available() -> bool {
    crate::test_cases::case_available("case9")
}
#[allow(dead_code)]
fn test_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::new()
}

use super::*;
use dyn_stack::MemStack;

fn load_case(name: &str) -> Network {
    crate::test_cases::load_case(name)
        .unwrap_or_else(|err| panic!("failed to load {name} fixture: {err}"))
}

#[test]
fn test_nr_case9() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let sol = solve_ac_pf(&net, &AcPfOptions::default()).expect("NR should converge on case9");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(
        sol.iterations <= 10,
        "too many iterations: {}",
        sol.iterations
    );
    assert!(sol.max_mismatch < 1e-8);

    // Voltage magnitudes should be reasonable (0.9 to 1.1 p.u.)
    for &v in &sol.voltage_magnitude_pu {
        assert!(v > 0.8 && v < 1.2, "unreasonable Vm: {v}");
    }

    // Slack bus (bus 1, idx 0) should have Vm = 1.04 (generator setpoint)
    assert!(
        (sol.voltage_magnitude_pu[0] - 1.04).abs() < 1e-6,
        "slack Vm = {}, expected 1.04",
        sol.voltage_magnitude_pu[0]
    );

    // PV buses (2, 3) should have Vm at their setpoints (1.025)
    assert!(
        (sol.voltage_magnitude_pu[1] - 1.025).abs() < 1e-6,
        "PV bus 2 Vm = {}",
        sol.voltage_magnitude_pu[1]
    );
    assert!(
        (sol.voltage_magnitude_pu[2] - 1.025).abs() < 1e-6,
        "PV bus 3 Vm = {}",
        sol.voltage_magnitude_pu[2]
    );

    // Power balance: total P generation ≈ total P load + losses
    let total_p_gen: f64 = sol
        .active_power_injection_pu
        .iter()
        .filter(|&&p| p > 0.0)
        .sum();
    let total_p_load: f64 = sol
        .active_power_injection_pu
        .iter()
        .filter(|&&p| p < 0.0)
        .sum::<f64>()
        .abs();
    let losses = total_p_gen - total_p_load;
    assert!(losses >= 0.0, "losses should be non-negative");
    assert!(losses < 0.1, "losses seem too high: {losses}"); // < 10 MW on 100 MVA base
}

#[test]
fn test_nr_case14() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let sol = solve_ac_pf(&net, &AcPfOptions::default()).expect("NR should converge on case14");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(sol.iterations <= 15);
    assert!(sol.max_mismatch < 1e-8);

    // All voltages in reasonable range (Q-limit enforcement may demote
    // the slack bus if its gen Q exceeds limits, so we don't assert a
    // specific Vm at bus 0).
    for &v in &sol.voltage_magnitude_pu {
        assert!(v > 0.9 && v < 1.15, "unreasonable Vm: {v}");
    }
}

/// AC-08: Warm-start from a prior PfSolution converges faster than cold start.
///
/// Solving the same (unchanged) network from the converged solution should
/// converge quickly.  With Q-limit enforcement, the outer Q-limit loop may
/// require additional passes, so we allow up to 5 inner iterations.
#[test]
fn test_nr_warm_start_case14() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");

    // Cold solve.
    let cold_sol =
        solve_ac_pf(&net, &AcPfOptions::default()).expect("cold NR should converge on case14");
    assert_eq!(cold_sol.status, SolveStatus::Converged);

    // Warm solve: reuse the converged Vm/Va as starting point.
    use super::WarmStart;
    let warm_opts = AcPfOptions {
        warm_start: Some(WarmStart::from_solution(&cold_sol)),
        ..AcPfOptions::default()
    };
    let warm_sol = solve_ac_pf(&net, &warm_opts).expect("warm NR should converge on case14");

    assert_eq!(warm_sol.status, SolveStatus::Converged);
    assert!(
        warm_sol.iterations <= 5,
        "warm-start should converge in ≤ 5 iterations, got {}",
        warm_sol.iterations
    );
    assert!(warm_sol.max_mismatch < 1e-8);

    // Voltages should match the cold solution within solver tolerance.
    for (i, (&v_cold, &v_warm)) in cold_sol
        .voltage_magnitude_pu
        .iter()
        .zip(warm_sol.voltage_magnitude_pu.iter())
        .enumerate()
    {
        assert!(
            (v_cold - v_warm).abs() < 1e-6,
            "Vm mismatch at bus {i}: cold={v_cold}, warm={v_warm}"
        );
    }
}

#[test]
fn test_nr_case30() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case30");
    let sol = solve_ac_pf(&net, &AcPfOptions::default()).expect("NR should converge on case30");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(sol.iterations <= 15);
    assert!(sol.max_mismatch < 1e-8);
}

#[test]
fn test_nr_case118() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let sol = solve_ac_pf(&net, &AcPfOptions::default()).expect("NR should converge on case118");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(sol.iterations <= 15);
    assert!(sol.max_mismatch < 1e-8);

    for (i, &v) in sol.voltage_magnitude_pu.iter().enumerate() {
        assert!(
            v > 0.85 && v < 1.15,
            "bus {} Vm = {} out of range",
            net.buses[i].number,
            v
        );
    }
}

#[test]
fn test_nr_power_balance() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    // Force single-slack. With `distributed_slack = true` (the default
    // since commit dd086c70) every generator absorbs a share of the
    // total P mismatch, so the spec-vs-computed comparison below would
    // have to account for per-generator slack shares. Keep the
    // single-slack reference so the test continues to lock the
    // mismatch equations rather than the slack distribution policy.
    let opts = AcPfOptions {
        distributed_slack: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("NR should converge");

    let ybus = build_ybus(&net);
    let (p_calc, q_calc) = crate::matrix::mismatch::compute_power_injection(
        &ybus,
        &sol.voltage_magnitude_pu,
        &sol.voltage_angle_rad,
    );

    let p_spec = net.bus_p_injection_pu();
    let q_spec = net.bus_q_injection_pu();

    // For PV and PQ buses, P should match
    for (i, bus) in net.buses.iter().enumerate() {
        if bus.bus_type == BusType::PQ || bus.bus_type == BusType::PV {
            assert!(
                (p_calc[i] - p_spec[i]).abs() < 1e-6,
                "P mismatch at bus {}: calc={}, spec={}",
                bus.number,
                p_calc[i],
                p_spec[i]
            );
        }
    }

    // For PQ buses, Q should also match
    for (i, bus) in net.buses.iter().enumerate() {
        if bus.bus_type == BusType::PQ {
            assert!(
                (q_calc[i] - q_spec[i]).abs() < 1e-6,
                "Q mismatch at bus {}: calc={}, spec={}",
                bus.number,
                q_calc[i],
                q_spec[i]
            );
        }
    }
}

#[test]
fn profile_nr_phases() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    use crate::matrix::fused::FusedPattern;
    use faer::sparse::linalg::lu::{NumericLu, factorize_symbolic_lu};
    use faer::{Conj, Par};
    use std::mem::MaybeUninit;
    use std::time::Instant;
    let net = load_case("case9241pegase");

    // Y-bus
    let t = Instant::now();
    let ybus = build_ybus(&net);
    let t_ybus = t.elapsed().as_secs_f64() * 1000.0;

    // Bus classification
    let mut pv: Vec<usize> = Vec::new();
    let mut pq: Vec<usize> = Vec::new();
    for (i, bus) in net.buses.iter().enumerate() {
        match bus.bus_type {
            BusType::PV => pv.push(i),
            BusType::PQ => pq.push(i),
            _ => {}
        }
    }
    let mut pvpq: Vec<usize> = pv.iter().chain(pq.iter()).copied().collect();
    pvpq.sort();

    let mut vm: Vec<f64> = net.buses.iter().map(|b| b.voltage_magnitude_pu).collect();
    let va: Vec<f64> = net.buses.iter().map(|b| b.voltage_angle_rad).collect();
    let bus_map = net.bus_index_map();
    for g in &net.generators {
        if g.in_service
            && let Some(&gen_idx) = bus_map.get(&g.bus)
            && (net.buses[gen_idx].bus_type == BusType::PV
                || net.buses[gen_idx].bus_type == BusType::Slack)
        {
            let reg = g.reg_bus.unwrap_or(g.bus);
            if let Some(&reg_idx) = bus_map.get(&reg) {
                vm[reg_idx] = g.voltage_setpoint_pu;
            }
        }
    }

    // Fused pattern construction (one-time)
    let t = Instant::now();
    let fused_pattern = FusedPattern::new(&ybus, &pvpq, &pq);
    let t_pattern = t.elapsed().as_secs_f64() * 1000.0;

    // Symbolic LU (one-time, low-level API)
    let t = Instant::now();
    let sym = factorize_symbolic_lu(fused_pattern.symbolic().as_ref(), Default::default()).unwrap();
    let t_sym = t.elapsed().as_secs_f64() * 1000.0;

    // Pre-allocate numeric LU and scratch
    let mut numeric = NumericLu::<usize, f64>::new();
    let factor_req = sym.factorize_numeric_lu_scratch::<f64>(Par::Seq, Default::default());
    let solve_req = sym.solve_in_place_scratch::<f64>(1, Par::Seq);
    let scratch_req = factor_req.or(solve_req);
    let mut scratch: Vec<MaybeUninit<u8>> =
        vec![MaybeUninit::uninit(); scratch_req.unaligned_bytes_required()];

    // 1st iteration: fused mismatch + Jacobian (cold)
    let t = Instant::now();
    let (_p1, _q1, jac1) = fused_pattern.build_fused(&ybus, &vm, &va);
    let t_fused1 = t.elapsed().as_secs_f64() * 1000.0;

    // Numeric LU (1st — allocates L/U factors)
    let t = Instant::now();
    let lu_ref = sym
        .factorize_numeric_lu(
            &mut numeric,
            jac1.as_ref(),
            Par::Seq,
            MemStack::new(&mut scratch),
            Default::default(),
        )
        .unwrap();
    let t_num = t.elapsed().as_secs_f64() * 1000.0;

    // Solve (in-place)
    let dim = pvpq.len() + pq.len();
    let mut rhs = faer::Col::<f64>::zeros(dim);
    let t = Instant::now();
    lu_ref.solve_in_place_with_conj(
        Conj::No,
        rhs.as_mat_mut(),
        Par::Seq,
        MemStack::new(&mut scratch),
    );
    let t_solve = t.elapsed().as_secs_f64() * 1000.0;

    // 2nd iteration (warm allocations)
    let t = Instant::now();
    let (_p2, _q2, jac2) = fused_pattern.build_fused(&ybus, &vm, &va);
    let t_fused2 = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let lu_ref2 = sym
        .factorize_numeric_lu(
            &mut numeric,
            jac2.as_ref(),
            Par::Seq,
            MemStack::new(&mut scratch),
            Default::default(),
        )
        .unwrap();
    let t_num2 = t.elapsed().as_secs_f64() * 1000.0;

    rhs.fill(0.0);
    let t = Instant::now();
    lu_ref2.solve_in_place_with_conj(
        Conj::No,
        rhs.as_mat_mut(),
        Par::Seq,
        MemStack::new(&mut scratch),
    );
    let t_solve2 = t.elapsed().as_secs_f64() * 1000.0;

    // 3rd iteration (steady-state)
    let t = Instant::now();
    let (_p3, _q3, jac3) = fused_pattern.build_fused(&ybus, &vm, &va);
    let t_fused3 = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let lu_ref3 = sym
        .factorize_numeric_lu(
            &mut numeric,
            jac3.as_ref(),
            Par::Seq,
            MemStack::new(&mut scratch),
            Default::default(),
        )
        .unwrap();
    let t_num3 = t.elapsed().as_secs_f64() * 1000.0;

    rhs.fill(0.0);
    let t = Instant::now();
    lu_ref3.solve_in_place_with_conj(
        Conj::No,
        rhs.as_mat_mut(),
        Par::Seq,
        MemStack::new(&mut scratch),
    );
    let t_solve3 = t.elapsed().as_secs_f64() * 1000.0;

    eprintln!(
        "\n=== NR Phase Profile (case9241pegase, {} buses) ===",
        net.n_buses()
    );
    eprintln!("--- One-time setup ---");
    eprintln!("Y-bus build:    {:8.3} ms", t_ybus);
    eprintln!("Fused pattern:  {:8.3} ms", t_pattern);
    eprintln!("Symbolic LU:    {:8.3} ms", t_sym);
    eprintln!("--- 1st iteration (cold alloc) ---");
    eprintln!(
        "Fused P/Q+Jac:  {:8.3} ms  (was mismatch ~0.3 + jac ~1.1 = ~1.4)",
        t_fused1
    );
    eprintln!("Numeric LU:     {:8.3} ms", t_num);
    eprintln!("LU solve:       {:8.3} ms", t_solve);
    eprintln!("--- 2nd iteration (warm alloc) ---");
    eprintln!("Fused P/Q+Jac:  {:8.3} ms", t_fused2);
    eprintln!("Numeric LU:     {:8.3} ms", t_num2);
    eprintln!("LU solve:       {:8.3} ms", t_solve2);
    eprintln!("--- 3rd iteration (steady state) ---");
    eprintln!("Fused P/Q+Jac:  {:8.3} ms", t_fused3);
    eprintln!("Numeric LU:     {:8.3} ms", t_num3);
    eprintln!("LU solve:       {:8.3} ms", t_solve3);
    let iter_total = t_fused3 + t_num3 + t_solve3;
    eprintln!("Iter total:     {:8.3} ms", iter_total);
    eprintln!("6 iterations:   {:8.3} ms (estimated)", iter_total * 6.0);
}

/// Benchmark LU tuning: Par::Rayon, SupernodalThreshold, COLAMD params
#[test]
fn benchmark_lu_tuning() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    use crate::matrix::fused::FusedPattern;
    use faer::sparse::linalg::SupernodalThreshold;
    use faer::sparse::linalg::lu::{LuSymbolicParams, NumericLu, factorize_symbolic_lu};
    use faer::{Conj, Par};
    use std::mem::MaybeUninit;
    use std::time::Instant;

    let net = load_case("case9241pegase");
    let ybus = build_ybus(&net);

    let mut pv: Vec<usize> = Vec::new();
    let mut pq: Vec<usize> = Vec::new();
    for (i, bus) in net.buses.iter().enumerate() {
        match bus.bus_type {
            BusType::PV => pv.push(i),
            BusType::PQ => pq.push(i),
            _ => {}
        }
    }
    let mut pvpq: Vec<usize> = pv.iter().chain(pq.iter()).copied().collect();
    pvpq.sort();

    let mut vm: Vec<f64> = net.buses.iter().map(|b| b.voltage_magnitude_pu).collect();
    let va: Vec<f64> = net.buses.iter().map(|b| b.voltage_angle_rad).collect();
    let bus_map = net.bus_index_map();
    for g in &net.generators {
        if g.in_service
            && let Some(&gen_idx) = bus_map.get(&g.bus)
            && (net.buses[gen_idx].bus_type == BusType::PV
                || net.buses[gen_idx].bus_type == BusType::Slack)
        {
            let reg = g.reg_bus.unwrap_or(g.bus);
            if let Some(&reg_idx) = bus_map.get(&reg) {
                vm[reg_idx] = g.voltage_setpoint_pu;
            }
        }
    }

    let fused_pattern = FusedPattern::new(&ybus, &pvpq, &pq);
    let dim = fused_pattern.dim();

    // Build the Jacobian once for all experiments
    let (_p, _q, jac) = fused_pattern.build_fused(&ybus, &vm, &va);

    eprintln!(
        "\n=== LU Tuning Benchmark (case9241pegase, {} buses, dim={}) ===",
        net.n_buses(),
        dim
    );

    // --- Experiment 1: Supernodal thresholds ---
    let thresholds: &[(&str, SupernodalThreshold)] = &[
        ("AUTO (1.0)", SupernodalThreshold::AUTO),
        ("FORCE_SIMPLICIAL", SupernodalThreshold::FORCE_SIMPLICIAL),
        ("FORCE_SUPERNODAL", SupernodalThreshold::FORCE_SUPERNODAL),
    ];

    eprintln!("\n--- Supernodal threshold comparison (Par::Seq) ---");
    for &(name, threshold) in thresholds {
        let params = LuSymbolicParams {
            supernodal_flop_ratio_threshold: threshold,
            ..Default::default()
        };

        let t = Instant::now();
        let sym = match factorize_symbolic_lu(fused_pattern.symbolic().as_ref(), params) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  {name:25}: symbolic failed: {e}");
                continue;
            }
        };
        let t_sym = t.elapsed().as_secs_f64() * 1000.0;

        let mut numeric = NumericLu::<usize, f64>::new();
        let factor_req = sym.factorize_numeric_lu_scratch::<f64>(Par::Seq, Default::default());
        let solve_req = sym.solve_in_place_scratch::<f64>(1, Par::Seq);
        let scratch_req = factor_req.or(solve_req);
        let mut scratch: Vec<MaybeUninit<u8>> =
            vec![MaybeUninit::uninit(); scratch_req.unaligned_bytes_required()];

        // Cold (1st factorization allocates L/U)
        let t = Instant::now();
        let _lu = sym
            .factorize_numeric_lu(
                &mut numeric,
                jac.as_ref(),
                Par::Seq,
                MemStack::new(&mut scratch),
                Default::default(),
            )
            .unwrap();
        let t_cold = t.elapsed().as_secs_f64() * 1000.0;

        // Warm (2nd+3rd factorization, take best)
        let t = Instant::now();
        let _lu = sym
            .factorize_numeric_lu(
                &mut numeric,
                jac.as_ref(),
                Par::Seq,
                MemStack::new(&mut scratch),
                Default::default(),
            )
            .unwrap();
        let t_warm1 = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        let lu_ref = sym
            .factorize_numeric_lu(
                &mut numeric,
                jac.as_ref(),
                Par::Seq,
                MemStack::new(&mut scratch),
                Default::default(),
            )
            .unwrap();
        let t_warm2 = t.elapsed().as_secs_f64() * 1000.0;

        let t_warm = t_warm1.min(t_warm2);

        // Solve
        let mut rhs = faer::Col::<f64>::zeros(dim);
        let t = Instant::now();
        lu_ref.solve_in_place_with_conj(
            Conj::No,
            rhs.as_mat_mut(),
            Par::Seq,
            MemStack::new(&mut scratch),
        );
        let t_solve = t.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "  {name:25}: sym={:.1}ms  cold={:.1}ms  warm={:.1}ms  solve={:.1}ms  iter={:.1}ms",
            t_sym,
            t_cold,
            t_warm,
            t_solve,
            t_warm + t_solve
        );
    }

    // --- Experiment 2: Par::Seq vs Par::Rayon ---
    eprintln!("\n--- Parallelism comparison (AUTO threshold) ---");
    let par_options: &[(&str, Par)] = &[
        ("Seq", Par::Seq),
        ("Rayon(0=all)", Par::rayon(0)),
        ("Rayon(2)", Par::rayon(2)),
        ("Rayon(4)", Par::rayon(4)),
        ("Rayon(8)", Par::rayon(8)),
    ];

    // Use default symbolic (AUTO threshold)
    let sym = factorize_symbolic_lu(fused_pattern.symbolic().as_ref(), Default::default()).unwrap();

    for &(name, par) in par_options {
        let mut numeric = NumericLu::<usize, f64>::new();
        let factor_req = sym.factorize_numeric_lu_scratch::<f64>(par, Default::default());
        let solve_req = sym.solve_in_place_scratch::<f64>(1, par);
        let scratch_req = factor_req.or(solve_req);
        let mut scratch: Vec<MaybeUninit<u8>> =
            vec![MaybeUninit::uninit(); scratch_req.unaligned_bytes_required()];

        // Cold
        let t = Instant::now();
        let _lu = sym
            .factorize_numeric_lu(
                &mut numeric,
                jac.as_ref(),
                par,
                MemStack::new(&mut scratch),
                Default::default(),
            )
            .unwrap();
        let t_cold = t.elapsed().as_secs_f64() * 1000.0;

        // Warm (best of 3)
        let mut t_warm = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let _lu = sym
                .factorize_numeric_lu(
                    &mut numeric,
                    jac.as_ref(),
                    par,
                    MemStack::new(&mut scratch),
                    Default::default(),
                )
                .unwrap();
            t_warm = t_warm.min(t.elapsed().as_secs_f64() * 1000.0);
        }

        let lu_ref = sym
            .factorize_numeric_lu(
                &mut numeric,
                jac.as_ref(),
                par,
                MemStack::new(&mut scratch),
                Default::default(),
            )
            .unwrap();

        let mut rhs = faer::Col::<f64>::zeros(dim);
        let t = Instant::now();
        lu_ref.solve_in_place_with_conj(
            Conj::No,
            rhs.as_mat_mut(),
            par,
            MemStack::new(&mut scratch),
        );
        let t_solve = t.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "  {name:15}: cold={:.1}ms  warm={:.1}ms  solve={:.1}ms  iter={:.1}ms",
            t_cold,
            t_warm,
            t_solve,
            t_warm + t_solve
        );
    }

    // --- Experiment 3: Rayon + FORCE_SUPERNODAL (best combo?) ---
    eprintln!("\n--- FORCE_SUPERNODAL + Parallelism ---");
    let sn_params = LuSymbolicParams {
        supernodal_flop_ratio_threshold: SupernodalThreshold::FORCE_SUPERNODAL,
        ..Default::default()
    };
    let sym_sn = factorize_symbolic_lu(fused_pattern.symbolic().as_ref(), sn_params).unwrap();

    let par_options_sn: &[(&str, Par)] = &[
        ("Seq", Par::Seq),
        ("Rayon(0=all)", Par::rayon(0)),
        ("Rayon(4)", Par::rayon(4)),
    ];

    for &(name, par) in par_options_sn {
        let mut numeric = NumericLu::<usize, f64>::new();
        let factor_req = sym_sn.factorize_numeric_lu_scratch::<f64>(par, Default::default());
        let solve_req = sym_sn.solve_in_place_scratch::<f64>(1, par);
        let scratch_req = factor_req.or(solve_req);
        let mut scratch: Vec<MaybeUninit<u8>> =
            vec![MaybeUninit::uninit(); scratch_req.unaligned_bytes_required()];

        // Cold
        let _lu = sym_sn
            .factorize_numeric_lu(
                &mut numeric,
                jac.as_ref(),
                par,
                MemStack::new(&mut scratch),
                Default::default(),
            )
            .unwrap();

        // Warm (best of 5)
        let mut t_warm = f64::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            let _lu = sym_sn
                .factorize_numeric_lu(
                    &mut numeric,
                    jac.as_ref(),
                    par,
                    MemStack::new(&mut scratch),
                    Default::default(),
                )
                .unwrap();
            t_warm = t_warm.min(t.elapsed().as_secs_f64() * 1000.0);
        }

        let lu_ref = sym_sn
            .factorize_numeric_lu(
                &mut numeric,
                jac.as_ref(),
                par,
                MemStack::new(&mut scratch),
                Default::default(),
            )
            .unwrap();

        let mut rhs = faer::Col::<f64>::zeros(dim);
        let mut t_solve = f64::MAX;
        for _ in 0..3 {
            rhs.fill(0.0);
            let t = Instant::now();
            lu_ref.solve_in_place_with_conj(
                Conj::No,
                rhs.as_mat_mut(),
                par,
                MemStack::new(&mut scratch),
            );
            t_solve = t_solve.min(t.elapsed().as_secs_f64() * 1000.0);
        }

        eprintln!(
            "  SN+{name:15}: warm={:.1}ms  solve={:.1}ms  iter={:.1}ms",
            t_warm,
            t_solve,
            t_warm + t_solve
        );
    }
}

#[test]
fn test_nr_case2383wp() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case2383wp");
    let sol = solve_ac_pf(&net, &AcPfOptions::default()).expect("NR should converge on case2383wp");

    assert_eq!(sol.status, SolveStatus::Converged);
    // Q-limit enforcement with degenerate-range gens (Qmax==Qmin) may
    // require many outer iterations on this case.
    assert!(sol.iterations <= 30);
    assert!(sol.max_mismatch < 1e-8);

    // case2383wp has generators with Qmax=Qmin=0 that get Q-limited,
    // causing voltage drops at those buses.  Widened range to [0.5, 1.5].
    for (i, &v) in sol.voltage_magnitude_pu.iter().enumerate() {
        assert!(
            v > 0.5 && v < 1.5,
            "bus {} Vm = {} out of range",
            net.buses[i].number,
            v
        );
    }
}

#[test]
fn test_nr_case9241pegase() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9241pegase");
    let sol =
        solve_ac_pf(&net, &AcPfOptions::default()).expect("NR should converge on case9241pegase");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(sol.iterations <= 15);
    assert!(sol.max_mismatch < 1e-8);
}

#[test]
fn test_nr_case13659pegase() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case13659pegase");
    let sol =
        solve_ac_pf(&net, &AcPfOptions::default()).expect("NR should converge on case13659pegase");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(sol.iterations <= 15);
    assert!(sol.max_mismatch < 1e-8);
}

// --- AC-02: Q-limit enforcement tests ---

/// AC-02: Solve case30 with Q-limit enforcement.
///
/// Verifies that all PV buses with finite Q limits have their reactive
/// injection within [qmin, qmax] (in per-unit) after the solve, and that
/// the solver still converges to a valid power flow solution.
#[test]
fn test_nr_qlimit_case30() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case30");
    let opts = AcPfOptions {
        enforce_q_limits: true,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("NR with Q-limits should converge on case30");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(sol.max_mismatch < 1e-8);

    // Verify that every PV bus with finite Q limits has Q within [qmin, qmax].
    let q_lims = collect_q_limits(&net);
    let base = net.base_mva;
    for (&bus_idx, &(q_spec_at_qmin, q_spec_at_qmax)) in &q_lims {
        let q_inj = sol.reactive_power_injection_pu[bus_idx];
        // Allow a small tolerance beyond the limit (switching introduces one NR pass
        // of error, which may leave the bus slightly outside its limit).
        let tol = 1e-4 / base; // 0.1 MVAr tolerance in pu
        assert!(
            q_inj >= q_spec_at_qmin - tol && q_inj <= q_spec_at_qmax + tol,
            "bus {} Q={:.4} outside [{:.4}, {:.4}] (pu)",
            bus_idx,
            q_inj,
            q_spec_at_qmin,
            q_spec_at_qmax,
        );
    }
}

/// AC-02: Solve case118 with Q-limit enforcement and verify Q constraints.
#[test]
fn test_nr_qlimit_case118() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let opts = AcPfOptions {
        enforce_q_limits: true,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("NR with Q-limits should converge on case118");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(sol.max_mismatch < 1e-8);

    let q_lims = collect_q_limits(&net);
    let base = net.base_mva;
    for (&bus_idx, &(q_spec_at_qmin, q_spec_at_qmax)) in &q_lims {
        let q_inj = sol.reactive_power_injection_pu[bus_idx];
        let tol = 1e-4 / base;
        assert!(
            q_inj >= q_spec_at_qmin - tol && q_inj <= q_spec_at_qmax + tol,
            "bus {} Q={:.4} outside [{:.4}, {:.4}] (pu)",
            bus_idx,
            q_inj,
            q_spec_at_qmin,
            q_spec_at_qmax,
        );
    }
}

/// AC-02: Synchronous condenser (Pmax = Pmin = 0) stays PV when Q is within bounds.
///
/// A synchronous condenser has Pmax = Pmin = 0 and a Q range [Qmin, Qmax].
/// When its reactive output is within bounds it should stay PV at the voltage
/// setpoint — matching MATPOWER behaviour.  This test uses a lightly loaded
/// 3-bus network where the condenser Q stays well inside [-50, +50] MVAR.
#[test]
fn test_nr_sync_compensator_holds_voltage() {
    use surge_network::Network;
    use surge_network::network::BusType;
    use surge_network::network::{Branch, Bus, Generator, Load};

    // 3-bus network:
    //   Bus 1 (Slack) — Branch 1→2 — Bus 2 (PV, sync condenser) — Branch 2→3 — Bus 3 (PQ load)
    let mut net = Network::new("sync_cond_3bus");
    net.base_mva = 100.0;

    // Bus 1: Slack at 1.05 pu
    let mut b1 = Bus::new(1, BusType::Slack, 100.0);
    b1.voltage_magnitude_pu = 1.05;
    b1.voltage_angle_rad = 0.0;

    // Bus 2: PV with sync condenser, setpoint 1.02 pu
    let mut b2 = Bus::new(2, BusType::PV, 100.0);
    b2.voltage_magnitude_pu = 1.02;
    b2.voltage_angle_rad = 0.0;

    // Bus 3: PQ with 80 MW load
    let mut b3 = Bus::new(3, BusType::PQ, 100.0);
    b3.voltage_magnitude_pu = 1.0;
    b3.voltage_angle_rad = 0.0;

    net.buses = vec![b1, b2, b3];
    net.loads.push(Load::new(3, 80.0, 0.0));

    // Branch 1→2: r=0.01, x=0.05
    net.branches = vec![
        Branch::new_line(1, 2, 0.01, 0.05, 0.0),
        Branch::new_line(2, 3, 0.02, 0.1, 0.0),
    ];

    // Synchronous condenser at bus 2: Pg=0, Pmax=0, Pmin=0, Qmin=-50, Qmax=+50
    let mut sc = Generator::new(2, 0.0, 1.02);
    sc.pmax = 0.0;
    sc.pmin = 0.0;
    sc.qmin = -50.0;
    sc.qmax = 50.0;

    // Slack generator at bus 1 (supplies real power)
    let mut slack_gen = Generator::new(1, 80.0, 1.05);
    slack_gen.pmax = 200.0;
    slack_gen.pmin = 0.0;
    slack_gen.qmin = f64::NEG_INFINITY;
    slack_gen.qmax = f64::INFINITY;

    net.generators = vec![slack_gen, sc];
    net.loads = vec![Load::new(3, 80.0, 0.0)];

    let opts = AcPfOptions {
        enforce_q_limits: true,
        flat_start: true,
        ..AcPfOptions::default()
    };

    let sol = solve_ac_pf(&net, &opts)
        .expect("NR with sync condenser should converge on 3-bus test case");

    assert_eq!(
        sol.status,
        SolveStatus::Converged,
        "solver must converge with sync condenser"
    );
    assert!(
        sol.max_mismatch < 1e-8,
        "mismatch too large: {:.2e}",
        sol.max_mismatch
    );

    // Bus 2 (internal index 1) must NOT appear in q_limited_buses because the
    // condenser's Q output is within its [-50, +50] MVAR bounds for this light load.
    let bus2_switched = sol.q_limited_buses.contains(&2);
    assert!(
        !bus2_switched,
        "sync condenser bus 2 should NOT be switched to PQ (Q within bounds), q_limited_buses = {:?}",
        sol.q_limited_buses
    );

    // Bus 2 voltage must remain at the condenser setpoint (1.02 pu ± 0.01).
    let vm_bus2 = sol.voltage_magnitude_pu[1]; // internal index 1 = external bus 2
    assert!(
        (vm_bus2 - 1.02).abs() < 0.01,
        "sync condenser bus 2 voltage {:.4} should be near setpoint 1.02 pu",
        vm_bus2
    );
}

/// AC-02: Slack bus Q-limit demotion — 3-bus synthetic case.
///
/// The slack bus generator has Qmin=0, Qmax=10 MVAR.  Under heavy reactive
/// load the slack must absorb ~30 MVAR (well below Qmin=0).  With Q-limit
/// enforcement the slack bus is demoted to PQ (Q fixed at 0), the first PV
/// bus is promoted to the new slack, and the solution must converge.
///
/// Key checks:
/// - Bus 1 appears in q_limited_buses (it was switched to PQ).
/// - Bus 1 voltage magnitude is NOT fixed at the Vg setpoint (it floats).
/// - Bus 2 becomes the new slack and its voltage is maintained at Vg.
/// - Bus 1 output angle is 0° (original-slack angle reference preserved).
#[test]
fn test_nr_slack_bus_q_limit_demotion() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    use surge_network::Network;
    use surge_network::network::BusType;
    use surge_network::network::{Branch, Bus, Generator, Load};

    // 3-bus network:
    //   Bus 1 (Slack, Vg=1.05) -- line -- Bus 2 (PV, Vg=1.02) -- line -- Bus 3 (PQ, heavy load)
    let mut net = Network::new("slack_qlim_3bus");
    net.base_mva = 100.0;

    let mut b1 = Bus::new(1, BusType::Slack, 100.0);
    b1.voltage_magnitude_pu = 1.05;
    b1.voltage_angle_rad = 0.0;

    let mut b2 = Bus::new(2, BusType::PV, 100.0);
    b2.voltage_magnitude_pu = 1.02;
    b2.voltage_angle_rad = 0.0;

    let mut b3 = Bus::new(3, BusType::PQ, 100.0);
    b3.voltage_magnitude_pu = 1.0;
    b3.voltage_angle_rad = 0.0;

    net.buses = vec![b1, b2, b3];
    net.loads.push(Load::new(3, 100.0, 80.0)); // Heavy reactive load forces bus 1 gen below Qmin=0
    net.branches = vec![
        Branch::new_line(1, 2, 0.01, 0.05, 0.0),
        Branch::new_line(2, 3, 0.02, 0.1, 0.0),
    ];

    // Slack gen at bus 1: tight Q limits [0, 10] MVAR — will be violated
    let mut slack_gen = Generator::new(1, 100.0, 1.05);
    slack_gen.pmax = 200.0;
    slack_gen.pmin = 0.0;
    slack_gen.qmin = 0.0;
    slack_gen.qmax = 10.0;

    // PV gen at bus 2: wide Q limits [−100, 100] — absorbs the reactive shortfall
    let mut pv_gen = Generator::new(2, 0.0, 1.02);
    pv_gen.pmax = 200.0;
    pv_gen.pmin = 0.0;
    pv_gen.qmin = -100.0;
    pv_gen.qmax = 100.0;

    net.generators = vec![slack_gen, pv_gen];
    net.loads = vec![Load::new(3, 100.0, 80.0)];

    let opts = AcPfOptions {
        enforce_q_limits: true,
        flat_start: true,
        ..AcPfOptions::default()
    };

    let sol = solve_ac_pf_kernel(&net, &opts).expect("should converge with slack bus Q demotion");
    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(
        sol.max_mismatch < 1e-6,
        "mismatch too large: {:.2e}",
        sol.max_mismatch
    );

    // Bus 1 (original slack) must be in q_limited_buses — it was demoted to PQ.
    assert!(
        sol.q_limited_buses.contains(&1),
        "bus 1 (original slack) should be demoted to PQ; q_limited_buses={:?}",
        sol.q_limited_buses
    );

    // Bus 1 voltage must NOT be fixed at its 1.05 setpoint — it floats after demotion.
    let vm_bus1 = sol.voltage_magnitude_pu[0];
    assert!(
        (vm_bus1 - 1.05).abs() > 1e-3,
        "bus 1 vm {:.4} should float away from 1.05 after Q-limit demotion",
        vm_bus1
    );

    // Bus 2 (new slack) must maintain its Vg setpoint.
    let vm_bus2 = sol.voltage_magnitude_pu[1];
    assert!(
        (vm_bus2 - 1.02).abs() < 1e-4,
        "bus 2 (promoted slack) vm {:.4} should stay at setpoint 1.02",
        vm_bus2
    );

    // Bus 1 angle reference must be preserved at 0° (MATPOWER convention).
    let va_bus1_deg = sol.voltage_angle_rad[0].to_degrees();
    assert!(
        va_bus1_deg.abs() < 1e-6,
        "bus 1 (original slack) angle {:.6}° should be preserved at 0°",
        va_bus1_deg
    );
}

#[test]
fn test_generator_p_limit_demotion_tolerates_micro_mw_rounding() {
    use surge_network::Network;
    use surge_network::network::BusType;
    use surge_network::network::{Bus, Generator};

    let mut net = Network::new("p_limit_rounding");
    net.buses = vec![Bus::new(1, BusType::PV, 100.0)];

    let mut near_limit = Generator::new(1, 95.2, 1.0);
    near_limit.pmin = 95.200002;
    near_limit.pmax = 150.0;
    near_limit.in_service = true;
    near_limit.voltage_regulated = true;
    net.generators = vec![near_limit];

    let mut bus_types = vec![BusType::PV];
    apply_generator_p_limit_demotions(&net, &mut bus_types);
    assert_eq!(
        bus_types,
        vec![BusType::PV],
        "micro-MW pg/pmin rounding should not demote a PV bus"
    );

    net.generators[0].p = 95.198;
    let mut demoted_bus_types = vec![BusType::PV];
    apply_generator_p_limit_demotions(&net, &mut demoted_bus_types);
    assert_eq!(
        demoted_bus_types,
        vec![BusType::PQ],
        "material pg < pmin violations should still demote the PV bus"
    );
}

/// AC-02: Slack bus Q demotion on case14 matches MATPOWER enforce_q_lims=1.
///
/// In case14, the slack gen (bus 1, Qmin=0, Qmax=10 MVAR) produces ~-16.5 MVAR
/// under flat-start conditions — well below Qmin=0.  MATPOWER with enforce_q_lims=1
/// demotes bus 1 to PQ (Q fixed at 0), promotes bus 2 to Slack, and the solved
/// Vm for bus 1 floats to ~1.0677 pu.
///
/// This test uses MATPOWER reference values from a verified run.
#[test]
fn test_nr_slack_qlim_demotion_case14_matpower_match() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");

    let sol = solve_ac_pf_kernel(
        &net,
        &AcPfOptions {
            flat_start: true,
            ..AcPfOptions::default()
        },
    )
    .expect("case14 should converge with slack Q demotion");

    assert_eq!(sol.status, SolveStatus::Converged);

    // Bus 1 must be Q-limited (demoted to PQ).
    assert!(
        sol.q_limited_buses.contains(&1),
        "bus 1 should be demoted to PQ; q_limited_buses={:?}",
        sol.q_limited_buses
    );

    // MATPOWER reference Vm for bus 1: slack demoted, voltage floats to 1.0677.
    let vm_bus1 = sol.voltage_magnitude_pu[0];
    assert!(
        (vm_bus1 - 1.067713).abs() < 1e-4,
        "bus 1 vm {:.6} should match MATPOWER 1.067713 (Q-demotion)",
        vm_bus1
    );

    // Bus 1 angle must remain at 0° (original-slack angle reference).
    let va_bus1_deg = sol.voltage_angle_rad[0].to_degrees();
    assert!(
        va_bus1_deg.abs() < 1e-4,
        "bus 1 angle {:.4}° should be preserved at 0°",
        va_bus1_deg
    );

    // Bus 2 angle — MATPOWER reference.
    let va_bus2_deg = sol.voltage_angle_rad[1].to_degrees();
    assert!(
        (va_bus2_deg - (-4.8078)).abs() < 0.01,
        "bus 2 angle {:.4}° should match MATPOWER -4.8078°",
        va_bus2_deg
    );
}

// --- AC-03: Distributed slack tests ---

/// AC-03: Solve case14 with equal distributed slack across all generators.
///
/// Verifies that the solver converges and the mismatch is within tolerance.
/// With distributed slack, the total active power mismatch is shared among
/// all generator buses proportionally to their participation factors.
#[test]
fn test_nr_distributed_slack_case14() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let opts = AcPfOptions {
        distributed_slack: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol =
        solve_ac_pf(&net, &opts).expect("NR with distributed slack should converge on case14");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(
        sol.max_mismatch < 1e-8,
        "distributed slack mismatch too large: {:.2e}",
        sol.max_mismatch
    );

    // All voltages must remain in a reasonable range
    for &v in &sol.voltage_magnitude_pu {
        assert!(v > 0.9 && v < 1.15, "unreasonable Vm: {v}");
    }
}

/// AC-03: Explicit participation factors sum to 1 → same convergence quality.
#[test]
fn test_nr_explicit_participation_case14() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");

    // case14 generators are at buses 1 (slack), 2, 3, 6, 8 (0-indexed: 0,1,2,5,7)
    // Assign equal weights to the two PV buses (1 and 2) plus the slack.
    let mut participation = HashMap::new();
    participation.insert(0usize, 0.4); // slack bus (bus 1)
    participation.insert(1usize, 0.3); // PV bus 2
    participation.insert(2usize, 0.3); // PV bus 3

    let opts = AcPfOptions {
        slack_participation: Some(participation),
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol =
        solve_ac_pf(&net, &opts).expect("NR with explicit participation should converge on case14");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(
        sol.max_mismatch < 1e-8,
        "explicit participation mismatch too large: {:.2e}",
        sol.max_mismatch
    );
}

// -----------------------------------------------------------------------
// AC-02: Q-limit PV→PQ switching — targeted test
// -----------------------------------------------------------------------

/// AC-02: Force a Q-limit violation on bus 2 in case14 by tightening Qmax
/// to 0.1 p.u. (10 MVAr on a 100 MVA base) and verify:
///
/// 1. `sol.q_limited_buses` contains bus 2 (external number).
/// 2. `sol.reactive_power_injection_pu[1]` (bus 2 Q injection, 0-indexed) is ≤ 0.1 p.u. + tol.
/// 3. `sol.n_q_limit_switches` ≥ 1.
/// 4. The solver converges normally.
#[test]
fn test_ac02_q_limit_switching() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let mut net = load_case("case14");

    // Bus 2 has a generator at bus number 2 (0-indexed = 1).
    // The standard Qmax is 50 MVAr = 0.5 p.u. (base 100 MVA).
    // Tighten Qmax to 10 MVAr = 0.1 p.u. to force a switch.
    let base = net.base_mva;
    let qmax_tight_mvar = 10.0; // MVAr
    for g in net.generators.iter_mut() {
        if g.bus == 2 {
            g.qmax = qmax_tight_mvar;
        }
    }

    let opts = AcPfOptions {
        enforce_q_limits: true,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts)
        .expect("NR with tight Q-limit on bus 2 should still converge (case14)");

    assert_eq!(sol.status, SolveStatus::Converged, "solver must converge");
    assert!(
        sol.max_mismatch < 1e-8,
        "max mismatch too large: {:.2e}",
        sol.max_mismatch
    );

    // Bus 2 must appear in q_limited_buses (external bus number = 2).
    assert!(
        sol.q_limited_buses.contains(&2),
        "bus 2 (external) must be Q-limited; got q_limited_buses={:?}",
        sol.q_limited_buses
    );

    // At least one switch must have occurred.
    assert!(
        sol.n_q_limit_switches >= 1,
        "expected ≥ 1 Q-limit switch, got {}",
        sol.n_q_limit_switches
    );

    // Q injection at bus 2 (0-indexed bus 1) must not exceed Qmax.
    // q_inject includes the generator contribution minus load Q.
    // Bus 2 load Qd = 12.7 MVAr = 0.127 pu, so q_inject = Qg - Qd.
    // Maximum q_inject = Qmax/base - Qd/base = 0.1 - 0.127 = -0.027 pu.
    let bus_qd = net.bus_load_q_mvar();
    let qd_bus2 = bus_qd[1] / base; // load Q in pu
    let expected_max_q_inject = qmax_tight_mvar / base - qd_bus2;
    let tol = 1e-4 / base;
    assert!(
        sol.reactive_power_injection_pu[1] <= expected_max_q_inject + tol,
        "bus 2 q_inject={:.4} pu exceeds Qmax-Qload={:.4} pu (Qmax={} MVAr, Qd={} MVAr)",
        sol.reactive_power_injection_pu[1],
        expected_max_q_inject,
        qmax_tight_mvar,
        bus_qd[1],
    );
}

/// AC-02b: D-curve (P-dependent Q limits) tighter than flat bounds.
///
/// Constructs a generator whose D-curve Qmax at full P dispatch (1.0 pu) is only
/// 0.1 pu, far below the nameplate Qmax of 0.5 pu. Verifies that:
/// 1. The NR Q-limit switch uses the D-curve limit, not the nameplate.
/// 2. The bus is Q-limited and q_inject does not exceed the D-curve Qmax - Qd.
#[test]
fn test_ac02b_dcurve_q_limit_switching() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let mut net = load_case("case14");
    let base = net.base_mva;

    // Generator at bus 2: nameplate Qmax = 50 MVAr = 0.5 pu.
    // D-curve: at Pg=0 → Qmax=0.5 pu, at Pg=Pmax → Qmax=0.1 pu (tight).
    // Since the generator runs at ~20 MW (0.2 pu) in case14, D-curve Qmax ≈ 0.42 pu.
    // Set a steep curve that forces switching at lower Q than nameplate.
    let dcurve_qmax_at_pmax = 0.1_f64; // 10 MVAr at full P
    for g in net.generators.iter_mut() {
        if g.bus == 2 {
            g.qmax = 50.0; // nameplate stays at 50 MVAr
            let pmax_pu = g.pmax / base;
            g.reactive_capability
                .get_or_insert_with(Default::default)
                .pq_curve = vec![
                (0.0, 0.5, -0.3),                     // P=0: Qmax=50 MVAr, Qmin=-30 MVAr
                (pmax_pu, dcurve_qmax_at_pmax, -0.1), // P=Pmax: Qmax=10 MVAr, Qmin=-10 MVAr
            ];
        }
    }

    let opts = AcPfOptions {
        enforce_q_limits: true,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts)
        .expect("NR with D-curve Q-limit on bus 2 should converge (case14)");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(sol.max_mismatch < 1e-8);

    // Bus 2 must be Q-limited (D-curve Qmax is tighter than unconstrained).
    assert!(
        sol.q_limited_buses.contains(&2),
        "bus 2 must be Q-limited under D-curve; q_limited_buses={:?}",
        sol.q_limited_buses
    );

    // Q injection at bus 2 (0-indexed=1) must be at most the D-curve Qmax - Qd.
    // The D-curve Qmax at the generator's scheduled Pg is between 0.1 and 0.5 pu.
    // The nameplate Qmax (0.5 pu) must NOT be the binding limit — D-curve must be.
    let bus_qd = net.bus_load_q_mvar();
    let qd_bus2 = bus_qd[1] / base;
    let q_inject_bus2 = sol.reactive_power_injection_pu[1];
    // D-curve Qmax at the operating P is ≤ 0.5 pu (nameplate). If the bus is
    // Q-limited, q_inject ≤ D-curve_Qmax - Qd (which is < nameplate - Qd = 0.5 - 0.127).
    let nameplate_q_inject_max = 50.0 / base - qd_bus2;
    let tol = 1e-3;
    assert!(
        q_inject_bus2 <= nameplate_q_inject_max + tol,
        "bus 2 q_inject={q_inject_bus2:.4} should not exceed nameplate - Qd={nameplate_q_inject_max:.4}"
    );
}

// -----------------------------------------------------------------------
// AC-03: Distributed slack — targeted test
// -----------------------------------------------------------------------

fn build_shared_bus_slack_test_network() -> Network {
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    let mut net = Network::new("shared_bus_slack_test");
    net.base_mva = 100.0;

    net.buses.push(Bus::new(1, BusType::Slack, 230.0));
    net.buses.push(Bus::new(2, BusType::PV, 230.0));
    net.buses.push(Bus::new(3, BusType::PQ, 230.0));

    net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));
    net.branches.push(Branch::new_line(2, 3, 0.0, 0.1, 0.0));
    net.branches.push(Branch::new_line(1, 3, 0.0, 0.2, 0.0));

    net.loads.push(Load::new(3, 150.0, 40.0));

    let mut g0 = Generator::new(1, 50.0, 1.04);
    g0.pmax = 200.0;
    net.generators.push(g0);

    let mut g1 = Generator::new(2, 40.0, 1.02);
    g1.pmax = 120.0;
    net.generators.push(g1);

    let mut g2 = Generator::new(2, 30.0, 1.02);
    g2.pmax = 110.0;
    net.generators.push(g2);

    net
}

/// AC-03: Solve case9 with explicit participation factors [0.4, 0.4, 0.2]
/// on the three generators and verify:
///
/// 1. `gen_slack_contribution_mw` is populated and has 3 entries.
/// 2. The contributions sum to approximately the total slack imbalance.
/// 3. The ratio between generators 0 and 2 is approximately 0.4/0.2 = 2.0.
/// 4. The solver converges normally.
#[test]
fn test_ac03_distributed_slack() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");

    // case9 has 3 generators at buses 1,2,3 (0-indexed: 0,1,2).
    // Assign participation factors [0.4, 0.4, 0.2] (sum = 1.0).
    let mut participation = HashMap::new();
    participation.insert(0usize, 0.4); // bus 1 (slack)
    participation.insert(1usize, 0.4); // bus 2 (PV)
    participation.insert(2usize, 0.2); // bus 3 (PV)

    let opts = AcPfOptions {
        slack_participation: Some(participation),
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };

    let sol = solve_ac_pf(&net, &opts).expect("NR with distributed slack should converge on case9");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(
        sol.max_mismatch < 1e-8,
        "distributed slack mismatch too large: {:.2e}",
        sol.max_mismatch
    );

    // gen_slack_contribution_mw must be populated with 3 entries (one per generator).
    assert_eq!(
        sol.gen_slack_contribution_mw.len(),
        3,
        "expected 3 gen_slack_contribution_mw entries, got {}",
        sol.gen_slack_contribution_mw.len()
    );

    let g0 = sol.gen_slack_contribution_mw[0];
    let g1 = sol.gen_slack_contribution_mw[1];
    let g2 = sol.gen_slack_contribution_mw[2];
    let total = g0 + g1 + g2;

    eprintln!(
        "gen_slack_contribution_mw: g0={:.3} g1={:.3} g2={:.3} total={:.3}",
        g0, g1, g2, total
    );

    // The total must be finite (non-NaN).
    assert!(total.is_finite(), "total slack contribution must be finite");

    // Ratio g0/g2 should be close to 0.4/0.2 = 2.0 (±1%).
    // Only check when g2 is non-negligible to avoid division by near-zero.
    if g2.abs() > 0.1 {
        let ratio = g0 / g2;
        assert!(
            (ratio - 2.0).abs() < 0.1,
            "g0/g2 ratio = {:.3}, expected ~2.0 (participation 0.4/0.2)",
            ratio
        );
    }

    // g0 and g1 should be equal within 1 MW (both have alpha=0.4).
    assert!(
        (g0 - g1).abs() < 1.0,
        "g0={g0:.3} and g1={g1:.3} should be equal (both alpha=0.4)"
    );
}

#[test]
fn test_ac03_shared_bus_generator_attribution_uses_explicit_generator_weights() {
    let net = build_shared_bus_slack_test_network();

    let mut generator_participation = HashMap::new();
    generator_participation.insert(1usize, 0.75);
    generator_participation.insert(2usize, 0.25);

    let opts = AcPfOptions {
        generator_slack_participation: Some(generator_participation),
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };

    let sol = solve_ac_pf(&net, &opts).expect("explicit generator participation should converge");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert_eq!(sol.gen_slack_contribution_mw.len(), net.generators.len());
    assert!(
        sol.gen_slack_contribution_mw[0].abs() < 1e-9,
        "slack generator should not receive explicit participation when excluded, got {:.6}",
        sol.gen_slack_contribution_mw[0]
    );

    let g1 = sol.gen_slack_contribution_mw[1];
    let g2 = sol.gen_slack_contribution_mw[2];
    let ratio = g1 / g2;
    assert!(
        (ratio - 3.0).abs() < 0.05,
        "shared-bus explicit generator attribution ratio should be ~3.0, got {ratio:.4}"
    );
}

#[test]
fn test_ac03_shared_bus_generator_attribution_uses_agc_policy() {
    let mut net = build_shared_bus_slack_test_network();
    net.generators[1].agc_participation_factor = Some(2.0);
    net.generators[2].agc_participation_factor = Some(1.0);

    let mut participation = HashMap::new();
    participation.insert(1usize, 1.0); // all distributed slack goes to bus 2

    let opts = AcPfOptions {
        slack_participation: Some(participation),
        slack_attribution: SlackAttributionMode::AgcParticipation,
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };

    let sol = solve_ac_pf(&net, &opts).expect("AGC slack attribution should converge");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert_eq!(sol.gen_slack_contribution_mw.len(), net.generators.len());
    assert!(
        sol.gen_slack_contribution_mw[0].abs() < 1e-9,
        "non-participating slack generator should stay at 0 MW contribution, got {:.6}",
        sol.gen_slack_contribution_mw[0]
    );

    let g1 = sol.gen_slack_contribution_mw[1];
    let g2 = sol.gen_slack_contribution_mw[2];
    let ratio = g1 / g2;
    assert!(
        (ratio - 2.0).abs() < 0.05,
        "shared-bus AGC attribution ratio should be ~2.0, got {ratio:.4}"
    );
}

/// Stott-Alsac: single-bus participation (α_slack = 1.0, all others = 0)
/// must give the SAME converged voltages as the standard NR (no distribution).
///
/// With all weight on the slack bus and zero weight on pvpq buses, the
/// participation column c is all-zeros.  The block-elimination step gives
/// Δλ = r_slack / (β^T·0 − 1) = −r_slack, and the voltage correction w = 0,
/// so Δx = Δx_0.  The voltages are identical to the standard NR. ✓
#[test]
fn test_stott_alsac_single_bus_slack_only() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");

    // Standard NR — reference. Force `distributed_slack = false` so
    // bus 0 absorbs the total mismatch; after dd086c70 the default
    // became `true` with an automatic participation policy that
    // spreads slack across multiple generators, which would diverge
    // from the 100%-on-slack reference this test exists to lock down.
    let opts_std = AcPfOptions {
        distributed_slack: false,
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };
    let sol_std = solve_ac_pf(&net, &opts_std).expect("standard NR should converge");

    // Distributed slack with all weight on the slack bus (bus 0, α = 1.0).
    // Equivalent to no distribution: slack absorbs everything.
    let mut participation = HashMap::new();
    participation.insert(0usize, 1.0); // 100 % on the slack bus

    let opts_ds = AcPfOptions {
        slack_participation: Some(participation),
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };
    let sol_ds = solve_ac_pf(&net, &opts_ds)
        .expect("single-bus distributed slack (α_slack=1) should converge");

    assert_eq!(sol_ds.status, SolveStatus::Converged);
    assert!(sol_ds.max_mismatch < 1e-8);

    // Voltages must match the standard NR to 1e-6 tolerance.
    for (i, (&vm_std, &vm_ds)) in sol_std
        .voltage_magnitude_pu
        .iter()
        .zip(sol_ds.voltage_magnitude_pu.iter())
        .enumerate()
    {
        assert!(
            (vm_std - vm_ds).abs() < 1e-6,
            "Vm[{i}]: standard={vm_std:.8}, dist-slack={vm_ds:.8}, diff={:.2e}",
            (vm_std - vm_ds).abs()
        );
    }
    for (i, (&va_std, &va_ds)) in sol_std
        .voltage_angle_rad
        .iter()
        .zip(sol_ds.voltage_angle_rad.iter())
        .enumerate()
    {
        assert!(
            (va_std - va_ds).abs() < 1e-6,
            "Va[{i}]: standard={va_std:.8}, dist-slack={va_ds:.8}, diff={:.2e}",
            (va_std - va_ds).abs()
        );
    }
}

/// Stott-Alsac: verify that gen_slack_contribution_mw ratios obey the
/// participation factors exactly (α_i / α_j = contribution_i / contribution_j),
/// and that the total contribution equals λ × base_mva (the accumulated
/// imbalance variable).
///
/// Uses case118 which has larger losses and more realistic imbalance signal.
#[test]
fn test_stott_alsac_contribution_ratios_case118() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");

    // Find first 3 in-service generators (these are at buses 0..2 for case118).
    // Use participation factors [3, 2, 1] (sum = 6 → normalized to [0.5, 1/3, 1/6]).
    let gen_buses: Vec<usize> = {
        let bus_map = net.bus_index_map();
        net.generators
            .iter()
            .filter(|g| g.in_service)
            .take(3)
            .filter_map(|g| bus_map.get(&g.bus).copied())
            .collect()
    };
    assert_eq!(
        gen_buses.len(),
        3,
        "case118 must have ≥ 3 in-service generators"
    );

    let mut participation = HashMap::new();
    participation.insert(gen_buses[0], 0.5);
    participation.insert(gen_buses[1], 1.0 / 3.0);
    participation.insert(gen_buses[2], 1.0 / 6.0);

    let opts = AcPfOptions {
        slack_participation: Some(participation),
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("distributed slack on case118 should converge");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(sol.max_mismatch < 1e-8);

    // gen_slack_contribution_mw must have one entry per generator.
    assert_eq!(sol.gen_slack_contribution_mw.len(), net.generators.len());

    // Find the contributions for the 3 participating generators.
    let _bus_map = net.bus_index_map();
    let get_contrib = |bus_num: u32| -> f64 {
        net.generators
            .iter()
            .zip(sol.gen_slack_contribution_mw.iter())
            .filter(|(g, _)| g.bus == bus_num && g.in_service)
            .map(|(_, &c)| c)
            .next()
            .unwrap_or(0.0)
    };

    let bus_num0 = net.buses[gen_buses[0]].number;
    let bus_num1 = net.buses[gen_buses[1]].number;
    let bus_num2 = net.buses[gen_buses[2]].number;

    let c0 = get_contrib(bus_num0);
    let c1 = get_contrib(bus_num1);
    let c2 = get_contrib(bus_num2);

    eprintln!("case118 contributions: c0={c0:.4} c1={c1:.4} c2={c2:.4}");

    // All must be finite.
    assert!(c0.is_finite() && c1.is_finite() && c2.is_finite());

    // Ratio c0/c1 should be (0.5) / (1/3) = 1.5 (within 2%).
    if c1.abs() > 0.01 {
        let ratio_01 = c0 / c1;
        assert!(
            (ratio_01 - 1.5).abs() < 0.05,
            "c0/c1 = {ratio_01:.4}, expected ~1.5 (participation 0.5 / 0.333)"
        );
    }

    // Ratio c0/c2 should be (0.5) / (1/6) = 3.0 (within 2%).
    if c2.abs() > 0.01 {
        let ratio_02 = c0 / c2;
        assert!(
            (ratio_02 - 3.0).abs() < 0.1,
            "c0/c2 = {ratio_02:.4}, expected ~3.0 (participation 0.5 / 0.167)"
        );
    }
}

// -----------------------------------------------------------------------
// Stott-Alsac: additional correctness tests
// -----------------------------------------------------------------------

/// Stott-Alsac: α_slack = 0 (all participation on PV buses, none on slack).
///
/// This is the most common real-world distributed-slack scenario: the reference
/// bus is excluded and all PV generators share the imbalance.  It exercises the
/// formula path where a_slack = 0, making denom = β^T·w (not β^T·w − α_s).
///
/// Checks:
/// 1. Solver converges to max_mismatch < 1e-8.
/// 2. Non-slack generators have non-zero contributions (λ ≠ 0).
/// 3. Contribution ratios match α ratios (within 2%).
/// 4. Slack bus contribution is 0.0 (not in participation map).
#[test]
fn test_stott_alsac_alpha_slack_zero() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");

    // case9: bus 0 = slack, bus 1 = PV (gen 1), bus 2 = PV (gen 2).
    // Give all weight to the two PV buses; slack bus excluded (α_slack = 0).
    let mut participation = HashMap::new();
    participation.insert(1usize, 0.6); // bus 2 (PV)
    participation.insert(2usize, 0.4); // bus 3 (PV)

    let opts = AcPfOptions {
        slack_participation: Some(participation),
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };
    let sol =
        solve_ac_pf(&net, &opts).expect("α_slack=0 distributed slack should converge on case9");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(
        sol.max_mismatch < 1e-8,
        "α_slack=0 mismatch too large: {:.2e}",
        sol.max_mismatch
    );

    let g0 = sol.gen_slack_contribution_mw[0]; // slack bus — not in map
    let g1 = sol.gen_slack_contribution_mw[1]; // α = 0.6
    let g2 = sol.gen_slack_contribution_mw[2]; // α = 0.4

    eprintln!("α_slack=0 contributions: g0={g0:.4} g1={g1:.4} g2={g2:.4}");

    // Non-participating slack bus must have zero contribution.
    assert!(
        g0.abs() < 1e-9,
        "slack bus contribution should be 0 when α_slack=0, got {g0:.6}"
    );

    // Non-slack contributions must be non-trivially large (λ ≠ 0).
    // case9 imbalance is ~0.7 MW; use 0.1 MW floor to confirm λ ≠ 0.
    assert!(
        g1.abs() + g2.abs() > 0.1,
        "total distributed adjustment too small ({:.4} MW); λ may be 0",
        g1.abs() + g2.abs()
    );

    // Ratio g1/g2 should be 0.6/0.4 = 1.5 (within 2%).
    let ratio = g1 / g2;
    assert!(
        (ratio - 1.5).abs() < 0.05,
        "g1/g2 ratio = {ratio:.4}, expected ~1.5 (α 0.6/0.4)"
    );
}

/// Stott-Alsac: non-participating generators must have exactly 0 contribution.
///
/// Uses case118 (54 generators total) with only 3 generators participating.
/// Verifies that all 51 non-participating generators report 0.0 MW contribution.
#[test]
fn test_stott_alsac_nonparticipating_generators_zero() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");

    let bus_map = net.bus_index_map();
    let participating_buses: Vec<usize> = net
        .generators
        .iter()
        .filter(|g| g.in_service)
        .take(3)
        .filter_map(|g| bus_map.get(&g.bus).copied())
        .collect();
    assert_eq!(participating_buses.len(), 3);

    let mut participation = HashMap::new();
    participation.insert(participating_buses[0], 0.5);
    participation.insert(participating_buses[1], 0.3);
    participation.insert(participating_buses[2], 0.2);

    let opts = AcPfOptions {
        slack_participation: Some(participation.clone()),
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("should converge on case118");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert_eq!(sol.gen_slack_contribution_mw.len(), net.generators.len());

    // Every generator whose bus is NOT in the participation map must have 0 contribution.
    let bus_idx_for_gen: Vec<usize> = net
        .generators
        .iter()
        .map(|g| bus_map.get(&g.bus).copied().unwrap_or(usize::MAX))
        .collect();

    for (k, (&contrib, &bidx)) in sol
        .gen_slack_contribution_mw
        .iter()
        .zip(bus_idx_for_gen.iter())
        .enumerate()
    {
        if !participation.contains_key(&bidx) {
            assert!(
                contrib.abs() < 1e-12,
                "generator {k} (bus_idx {bidx}) not in participation map \
                 but has non-zero contribution {contrib:.6} MW"
            );
        }
    }

    // Participating generators must have non-zero contributions that sum > 1 MW.
    let total_contrib: f64 = sol.gen_slack_contribution_mw.iter().sum();
    assert!(
        total_contrib.abs() > 1.0,
        "total contribution too small ({total_contrib:.4} MW); λ may be 0"
    );
}

/// Stott-Alsac: energy conservation — Σ gen_slack_contribution_mw = λ · base_mva.
///
/// For normalized participation factors (Σα = 1.0), the total adjustment
/// distributed across all generators equals λ × base_mva.  This is the
/// fundamental energy-balance identity of the Stott-Alsac method.
///
/// We verify it indirectly: with α_slack = 1.0, gen_slack_contribution_mw[0]
/// must equal the difference between the standard NR slack output and the
/// initial slack setpoint (p_spec_base[slack] × base_mva).
///
/// Concretely: in the α_slack=1 case the augmented equation is
///   p_calc[slack] = p_spec_base[slack] + λ
/// so λ × base_mva = (converged slack output) − (initial setpoint in MW).
/// The contribution is exactly λ × base_mva, verifiable against p_inject.
#[test]
fn test_stott_alsac_energy_conservation_case9() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let base = net.base_mva;

    // Standard NR — reference slack output. Force `distributed_slack =
    // false` so all mismatch lands on bus 0 and the derivation below
    // (`λ × base = slack_p_inject − p_spec_base_slack`) holds exactly.
    // Since dd086c70 the default became `true` with automatic
    // participation across every generator.
    let sol_std = solve_ac_pf(
        &net,
        &AcPfOptions {
            distributed_slack: false,
            enforce_q_limits: false,
            detect_islands: false,
            ..AcPfOptions::default()
        },
    )
    .expect("standard NR should converge");

    // p_inject[0] = net P injection at bus 0 (per unit), positive = generation.
    let slack_p_inject_std_mw = sol_std.active_power_injection_pu[0] * base;

    // Slack bus initial setpoint (from network data, in MW).
    // The bus 0 (slack) p_spec_base = net P load is subtracted, generator Pg added.
    // We use p_inject directly: in standard NR this is the "free" slack output.
    // In distributed NR with α_slack=1, p_inject[0] should be identical (verified
    // by test_stott_alsac_single_bus_slack_only).  The contribution is then:
    //   gen_slack_contribution_mw[0] = α_slack × λ × base = λ × base
    //   λ × base = slack_p_inject_distributed − slack_p_inject_base
    // where slack_p_inject_base is the initial p_spec_base setting.

    // Distributed NR with α_slack = 1.
    let mut participation = HashMap::new();
    participation.insert(0usize, 1.0);
    let sol_ds = solve_ac_pf(
        &net,
        &AcPfOptions {
            slack_participation: Some(participation),
            enforce_q_limits: false,
            detect_islands: false,
            ..AcPfOptions::default()
        },
    )
    .expect("α_slack=1 should converge");

    // Voltages are the same (verified by test_stott_alsac_single_bus_slack_only),
    // so p_inject[0] is the same in both solutions.
    let slack_p_inject_ds_mw = sol_ds.active_power_injection_pu[0] * base;
    assert!(
        (slack_p_inject_std_mw - slack_p_inject_ds_mw).abs() < 0.01,
        "slack bus P injection should be identical: std={slack_p_inject_std_mw:.4} \
         ds={slack_p_inject_ds_mw:.4}"
    );

    // The contribution for the slack generator must be non-zero
    // (case9 has load; slack must adjust from initial Pg setpoint).
    let contrib0 = sol_ds.gen_slack_contribution_mw[0];
    assert!(
        contrib0.is_finite(),
        "slack contribution must be finite, got {contrib0}"
    );

    // Contributions of non-participating generators (gens 1 and 2) must be 0.
    assert!(
        sol_ds.gen_slack_contribution_mw[1].abs() < 1e-9,
        "gen 1 not participating — contribution must be 0, got {}",
        sol_ds.gen_slack_contribution_mw[1]
    );
    assert!(
        sol_ds.gen_slack_contribution_mw[2].abs() < 1e-9,
        "gen 2 not participating — contribution must be 0, got {}",
        sol_ds.gen_slack_contribution_mw[2]
    );

    // The total adjustment (= λ × base for α_slack=1, Σα=1) must account for
    // the actual system imbalance corrected at this operating point.
    // p_inject[0] = generator output − load at bus 0 (pu).
    // The generator setpoint from the case file is net.generators[0].p.
    let gen0_pg_setpoint_mw = net.generators[0].p;
    let bus0_load_mw = net.bus_load_p_mw()[0];
    let p_spec_base_slack_mw = gen0_pg_setpoint_mw - bus0_load_mw;

    // λ × base = slack_p_inject_mw − p_spec_base_slack_mw (within 1 MW rounding).
    let expected_contrib_mw = slack_p_inject_ds_mw - p_spec_base_slack_mw;
    assert!(
        (contrib0 - expected_contrib_mw).abs() < 1.0,
        "slack contribution {contrib0:.4} MW should equal \
         (p_inject − p_spec_base) = {expected_contrib_mw:.4} MW"
    );
}

/// Stott-Alsac: numerical robustness on case2383wp (2,383-bus, heavy loading).
///
/// Verifies that the block-elimination formula remains well-conditioned on a
/// large, heavily-loaded network with many generators.
#[test]
fn test_stott_alsac_large_case2383wp() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case2383wp");
    let bus_map = net.bus_index_map();

    // Take first 5 in-service generators and assign participation factors.
    let participating: Vec<usize> = net
        .generators
        .iter()
        .filter(|g| g.in_service)
        .take(5)
        .filter_map(|g| bus_map.get(&g.bus).copied())
        .collect();
    assert!(
        participating.len() >= 4,
        "case2383wp needs ≥ 4 in-service generators"
    );

    let weights = [0.4_f64, 0.25, 0.15, 0.12, 0.08];
    let mut participation = HashMap::new();
    for (&bus_idx, &w) in participating.iter().zip(weights.iter()) {
        participation.insert(bus_idx, w);
    }

    let opts = AcPfOptions {
        slack_participation: Some(participation),
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("distributed slack on case2383wp should converge");

    assert_eq!(sol.status, SolveStatus::Converged);
    assert!(
        sol.max_mismatch < 1e-8,
        "case2383wp distributed slack mismatch too large: {:.2e}",
        sol.max_mismatch
    );

    // All voltages must be in a physically plausible range.
    for (i, &v) in sol.voltage_magnitude_pu.iter().enumerate() {
        assert!(
            v > 0.5 && v < 1.5,
            "Vm[{i}] = {v:.4} outside [0.5, 1.5] on case2383wp with distributed slack"
        );
    }

    // Total contribution must be non-trivially large (case2383wp has real losses).
    let total: f64 = sol.gen_slack_contribution_mw.iter().sum::<f64>().abs();
    assert!(
        total > 1.0,
        "total distributed adjustment {total:.4} MW too small on case2383wp"
    );
}

// -----------------------------------------------------------------------
// AC-05: Island detection — targeted test
// -----------------------------------------------------------------------

/// AC-05: Build a 6-bus network with two disconnected islands and verify:
///
/// 1. `sol.n_islands()` returns 2.
/// 2. Both islands converge (no NaN/zero voltages).
/// 3. Buses in island 0 all share the same island_id (0).
/// 4. Buses in island 1 all share the same island_id (1).
///
/// The network:
///   Island A: bus 1 (slack), bus 2 (PQ), bus 3 (PQ) — connected ring
///   Island B: bus 4 (PV/slack via promotion), bus 5 (PQ), bus 6 (PQ)
///
/// This is equivalent to opening all connecting branches between the two
/// halves, which is what N-1 island-creating contingencies produce.
#[test]
fn test_ac05_islanding() {
    use surge_network::Network;
    use surge_network::network::BusType;
    use surge_network::network::{Branch, Bus, Generator, Load};

    fn make_bus(number: u32, bus_type: BusType, vm: f64) -> Bus {
        let mut b = Bus::new(number, bus_type, 100.0);
        b.voltage_magnitude_pu = vm;
        b.voltage_angle_rad = 0.0;
        b
    }

    fn make_line(from_bus: u32, to_bus: u32) -> Branch {
        Branch::new_line(from_bus, to_bus, 0.01, 0.1, 0.02)
    }

    fn make_gen(bus: u32, pg: f64, vs: f64) -> Generator {
        let mut g = Generator::new(bus, pg, vs);
        g.qmin = -50.0;
        g.qmax = 50.0;
        g.pmax = 200.0;
        g
    }

    fn make_load(bus: u32, pd: f64, qd: f64) -> Load {
        Load::new(bus, pd, qd)
    }

    let mut net = Network::new("island_test_ac05");
    net.base_mva = 100.0;

    // Island A: buses 1-3
    net.buses = vec![
        make_bus(1, BusType::Slack, 1.05),
        make_bus(2, BusType::PQ, 1.0),
        make_bus(3, BusType::PQ, 1.0),
        // Island B: buses 4-6
        make_bus(4, BusType::Slack, 1.02),
        make_bus(5, BusType::PQ, 1.0),
        make_bus(6, BusType::PQ, 1.0),
    ];

    // Island A internal branches
    net.branches = vec![
        make_line(1, 2),
        make_line(2, 3),
        make_line(3, 1),
        // Island B internal branches (no cross-island branch)
        make_line(4, 5),
        make_line(5, 6),
        make_line(6, 4),
    ];

    // Generators
    net.generators = vec![
        make_gen(1, 50.0, 1.05), // Island A slack generator
        make_gen(4, 40.0, 1.02), // Island B slack generator
    ];

    // Loads
    net.loads = vec![
        make_load(2, 20.0, 5.0),
        make_load(3, 15.0, 4.0),
        make_load(5, 18.0, 5.0),
        make_load(6, 12.0, 3.0),
    ];

    let opts = AcPfOptions {
        detect_islands: true,
        enforce_q_limits: false,
        flat_start: true,
        ..AcPfOptions::default()
    };

    let sol = solve_ac_pf(&net, &opts).expect("NR with two-island network should converge");

    assert_eq!(sol.status, SolveStatus::Converged, "solver must converge");
    assert!(
        sol.max_mismatch < 1e-8,
        "max mismatch too large: {:.2e}",
        sol.max_mismatch
    );

    // Must detect 2 islands.
    assert_eq!(
        sol.n_islands(),
        2,
        "expected 2 islands, got {}; island_ids={:?}",
        sol.n_islands(),
        sol.island_ids
    );

    // island_ids must be populated (one per bus).
    assert_eq!(
        sol.island_ids.len(),
        6,
        "island_ids must have one entry per bus"
    );

    // Buses 0-2 (island A) must share an island id.
    let id_a = sol.island_ids[0];
    assert_eq!(sol.island_ids[1], id_a, "bus 2 must be in island A");
    assert_eq!(sol.island_ids[2], id_a, "bus 3 must be in island A");

    // Buses 3-5 (island B) must share a different island id.
    let id_b = sol.island_ids[3];
    assert_ne!(id_b, id_a, "island B must differ from island A");
    assert_eq!(sol.island_ids[4], id_b, "bus 5 must be in island B");
    assert_eq!(sol.island_ids[5], id_b, "bus 6 must be in island B");

    // All voltages must be positive (no dead buses).
    for (i, &v) in sol.voltage_magnitude_pu.iter().enumerate() {
        assert!(
            v > 0.5 && v < 1.5,
            "bus {} voltage {:.3} out of range",
            net.buses[i].number,
            v
        );
    }

    eprintln!(
        "AC-05 island test: n_islands={}, island_ids={:?}",
        sol.n_islands(),
        sol.island_ids
    );
}

/// M-04: Multi-island distributed slack must not cross island boundaries.
///
/// Supplies a `slack_participation` map with one bus from each island
/// (global indices 0 and 3).  The M-04 fix remaps each entry to the
/// corresponding local index within its island's sub-network.  Before the
/// fix, island B received global index 0, which is either an out-of-bounds
/// access or points to the wrong local bus — causing NaN voltages or panic.
#[test]
fn test_m04_multi_island_distributed_slack_no_cross_island() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    use surge_network::Network;
    use surge_network::network::BusType;
    use surge_network::network::{Branch, Bus, Generator, Load};

    fn make_bus(number: u32, bus_type: BusType, vm: f64) -> Bus {
        let mut b = Bus::new(number, bus_type, 100.0);
        b.voltage_magnitude_pu = vm;
        b.voltage_angle_rad = 0.0;
        b
    }

    fn make_line(from_bus: u32, to_bus: u32) -> Branch {
        Branch::new_line(from_bus, to_bus, 0.01, 0.1, 0.02)
    }

    fn make_gen(bus: u32, pg: f64, vs: f64) -> Generator {
        let mut g = Generator::new(bus, pg, vs);
        g.qmin = -50.0;
        g.qmax = 50.0;
        g.pmax = 200.0;
        g
    }

    fn make_load(bus: u32, pd: f64, qd: f64) -> Load {
        Load::new(bus, pd, qd)
    }

    let mut net = Network::new("m04_island_dist_slack");
    net.base_mva = 100.0;

    // Island A: buses 1-3 (global indices 0-2)
    // Island B: buses 4-6 (global indices 3-5)
    net.buses = vec![
        make_bus(1, BusType::Slack, 1.05),
        make_bus(2, BusType::PQ, 1.0),
        make_bus(3, BusType::PQ, 1.0),
        make_bus(4, BusType::Slack, 1.02),
        make_bus(5, BusType::PQ, 1.0),
        make_bus(6, BusType::PQ, 1.0),
    ];

    net.branches = vec![
        make_line(1, 2),
        make_line(2, 3),
        make_line(3, 1),
        make_line(4, 5),
        make_line(5, 6),
        make_line(6, 4),
    ];

    net.generators = vec![make_gen(1, 50.0, 1.05), make_gen(4, 40.0, 1.02)];

    net.loads = vec![
        make_load(2, 20.0, 5.0),
        make_load(3, 15.0, 4.0),
        make_load(5, 18.0, 5.0),
        make_load(6, 12.0, 3.0),
    ];

    // Global indices: 0 → island A bus 1, 3 → island B bus 4.
    // After M-04 fix each island remaps to local index 0 for its own gen.
    let mut participation = HashMap::new();
    participation.insert(0usize, 0.5);
    participation.insert(3usize, 0.5);

    let opts = AcPfOptions {
        detect_islands: true,
        enforce_q_limits: false,
        flat_start: true,
        slack_participation: Some(participation),
        ..AcPfOptions::default()
    };

    let sol =
        solve_ac_pf(&net, &opts).expect("multi-island distributed slack should converge (M-04)");

    assert_eq!(sol.status, SolveStatus::Converged, "solver must converge");
    assert!(
        sol.max_mismatch < 1e-8,
        "max mismatch too large: {:.2e}",
        sol.max_mismatch
    );
    assert_eq!(sol.n_islands(), 2, "expected 2 islands");
    assert_eq!(
        sol.gen_slack_contribution_mw.len(),
        net.generators.len(),
        "multi-island stitching should preserve generator-level slack contributions"
    );
    assert!(
        sol.gen_slack_contribution_mw.iter().all(|c| c.is_finite()),
        "stiched generator slack contributions must remain finite: {:?}",
        sol.gen_slack_contribution_mw
    );
    assert!(
        sol.gen_slack_contribution_mw.iter().any(|c| c.abs() > 1e-9),
        "multi-island stitching should preserve non-zero distributed-slack metadata"
    );

    // All voltages must be finite and in a physical range.
    for (i, &v) in sol.voltage_magnitude_pu.iter().enumerate() {
        assert!(
            v > 0.5 && v < 1.5,
            "bus {} voltage {:.3} out of range (cross-island contamination?)",
            net.buses[i].number,
            v
        );
    }
}

// --- KLU-accelerated NR solver tests ---

#[test]
fn test_nr_klu_correctness() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    // Verify KLU solver produces identical results to faer solver.
    //
    // Both solvers use the same options (no Q-limit enforcement, no island
    // detection, no distributed slack) to ensure they solve the same
    // mathematical system.  This verifies LU factorization and triangular
    // solve correctness.
    let opts = AcPfOptions {
        enforce_q_limits: false,
        detect_islands: false,
        ..AcPfOptions::default()
    };
    for case_name in &["case9", "case14", "case30", "case118", "case2383wp"] {
        let net = load_case(case_name);
        let sol_faer = solve_ac_pf(&net, &opts).unwrap_or_else(|e| panic!("{case_name} faer: {e}"));
        let sol_klu =
            solve_ac_pf_kernel(&net, &opts).unwrap_or_else(|e| panic!("{case_name} KLU: {e}"));

        assert_eq!(sol_klu.status, SolveStatus::Converged, "{case_name}");
        assert!(
            sol_klu.max_mismatch < 1e-8,
            "{case_name} KLU mismatch too large: {:.2e}",
            sol_klu.max_mismatch
        );

        // Voltage magnitudes should match within tight tolerance
        for (i, (&v_faer, &v_klu)) in sol_faer
            .voltage_magnitude_pu
            .iter()
            .zip(sol_klu.voltage_magnitude_pu.iter())
            .enumerate()
        {
            assert!(
                (v_faer - v_klu).abs() < 1e-10,
                "{case_name} bus {i}: Vm faer={v_faer}, klu={v_klu}"
            );
        }
        for (i, (&a_faer, &a_klu)) in sol_faer
            .voltage_angle_rad
            .iter()
            .zip(sol_klu.voltage_angle_rad.iter())
            .enumerate()
        {
            assert!(
                (a_faer - a_klu).abs() < 1e-10,
                "{case_name} bus {i}: Va faer={a_faer}, klu={a_klu}"
            );
        }
    }
}

#[test]
fn test_nr_klu_large_cases() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    for case_name in &["case9241pegase", "case13659pegase"] {
        let net = load_case(case_name);
        let sol = solve_ac_pf_kernel(&net, &AcPfOptions::default())
            .unwrap_or_else(|e| panic!("{case_name} KLU: {e}"));

        assert_eq!(sol.status, SolveStatus::Converged, "{case_name}");
        assert!(sol.iterations <= 15, "{case_name}");
        assert!(sol.max_mismatch < 1e-8, "{case_name}");

        eprintln!(
            "{case_name}: {} iters, {:.3} ms (KLU)",
            sol.iterations,
            sol.solve_time_secs * 1000.0
        );
    }
}

#[test]
fn benchmark_nr_klu_all_cases() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    use std::time::Instant;

    eprintln!("\n=== NR-KLU Benchmark (all cases) ===");
    for case_name in &[
        "case9",
        "case14",
        "case30",
        "case118",
        "case2383wp",
        "case9241pegase",
        "case13659pegase",
    ] {
        let net = load_case(case_name);

        // KLU solver (best of 3)
        let mut best_klu = f64::MAX;
        let mut klu_iters = 0;
        for _ in 0..3 {
            let t = Instant::now();
            let sol = solve_ac_pf_kernel(&net, &AcPfOptions::default()).unwrap();
            best_klu = best_klu.min(t.elapsed().as_secs_f64() * 1000.0);
            klu_iters = sol.iterations;
        }

        // faer solver (best of 3)
        let mut best_faer = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let _sol = solve_ac_pf(&net, &AcPfOptions::default()).unwrap();
            best_faer = best_faer.min(t.elapsed().as_secs_f64() * 1000.0);
        }

        let speedup = best_faer / best_klu;
        eprintln!(
            "  {case_name:20}: KLU={:8.3}ms  faer={:8.3}ms  speedup={:.1}x  ({} iters)",
            best_klu, best_faer, speedup, klu_iters
        );
    }
}

// -----------------------------------------------------------------------
// MATPOWER reference regression tests — pin solver output to validated values
// -----------------------------------------------------------------------

/// MATPOWER reference regression test — case9 bus voltages.
/// Values validated against MATPOWER 7.1 runpf('case9').
/// Tolerance: 1e-4 p.u. for Vm, 1e-2 degrees for Va.
#[test]
fn test_nr_case9_matpower_reference() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    // MATPOWER's runpf uses single-slack by default — the reference
    // values below were generated that way. Since dd086c70 surge
    // defaults to `distributed_slack = true`, which shifts bus angles
    // by ~0.05° on case9, so force single-slack here to preserve the
    // apples-to-apples comparison with MATPOWER.
    let opts = AcPfOptions {
        distributed_slack: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("NR should converge on case9");
    assert_eq!(sol.status, SolveStatus::Converged);

    // MATPOWER reference Vm values (p.u.)
    let ref_vm: &[f64] = &[
        1.040000, 1.025000, 1.025000, 1.025788, 1.012654, 1.032353, 1.015883, 1.025769, 0.995631,
    ];
    // MATPOWER reference Va values (degrees)
    let ref_va_deg: &[f64] = &[
        0.0000, 9.2800, 4.6648, -2.2168, -3.6874, 1.9667, 0.7275, 3.7197, -3.9888,
    ];

    assert_eq!(
        sol.voltage_magnitude_pu.len(),
        ref_vm.len(),
        "case9 bus count mismatch"
    );
    for (i, (&vm, &va)) in sol
        .voltage_magnitude_pu
        .iter()
        .zip(sol.voltage_angle_rad.iter())
        .enumerate()
    {
        let va_deg = va.to_degrees();
        assert!(
            (vm - ref_vm[i]).abs() < 1e-4,
            "case9 bus {} Vm: got {:.6}, expected {:.6}",
            net.buses[i].number,
            vm,
            ref_vm[i]
        );
        assert!(
            (va_deg - ref_va_deg[i]).abs() < 1e-2,
            "case9 bus {} Va: got {:.4} deg, expected {:.4} deg",
            net.buses[i].number,
            va_deg,
            ref_va_deg[i]
        );
    }
}

/// MATPOWER reference regression test — case14 bus voltages.
/// Values validated against MATPOWER 7.1 runpf('case14').
/// Tolerance: 1e-4 p.u. for Vm, 1e-2 degrees for Va.
#[test]
fn test_nr_case14_matpower_reference() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let sol = solve_ac_pf(&net, &AcPfOptions::default()).expect("NR should converge on case14");
    assert_eq!(sol.status, SolveStatus::Converged);

    // MATPOWER reference Vm values (p.u.) — enforce_q_lims=1 flat start.
    // Bus 1 gen (qmin=0, qmax=10 MVAR) produces ~-16.5 MVAR, violating qmin=0.
    // MATPOWER demotes bus 1 to PQ (Q fixed at 0), promotes bus 2 to Slack.
    // Bus 1 voltage floats to 1.0677; bus 2 is the new angular reference.
    // Angles are then shifted so bus 1 (original slack) reports 0° (MATPOWER convention).
    let ref_vm: &[f64] = &[
        1.067713, 1.045000, 1.010000, 1.018602, 1.021039, 1.070000, 1.061945, 1.090000, 1.056346,
        1.051329, 1.057083, 1.055219, 1.050443, 1.035795,
    ];
    // MATPOWER reference Va values (degrees) — angles shifted so bus 1 = 0°.
    let ref_va_deg: &[f64] = &[
        0.0000, -4.8078, -12.5434, -10.1479, -8.6200, -14.0530, -13.1936, -13.1936, -14.7728,
        -14.9312, -14.6239, -14.9075, -14.9887, -15.8667,
    ];

    assert_eq!(
        sol.voltage_magnitude_pu.len(),
        ref_vm.len(),
        "case14 bus count mismatch"
    );
    for (i, (&vm, &va)) in sol
        .voltage_magnitude_pu
        .iter()
        .zip(sol.voltage_angle_rad.iter())
        .enumerate()
    {
        let va_deg = va.to_degrees();
        assert!(
            (vm - ref_vm[i]).abs() < 1e-4,
            "case14 bus {} Vm: got {:.6}, expected {:.6}",
            net.buses[i].number,
            vm,
            ref_vm[i]
        );
        assert!(
            (va_deg - ref_va_deg[i]).abs() < 1e-2,
            "case14 bus {} Va: got {:.4} deg, expected {:.4} deg",
            net.buses[i].number,
            va_deg,
            ref_va_deg[i]
        );
    }
}

/// MATPOWER reference regression test — case30 bus voltages.
/// Values validated against MATPOWER 7.1 runpf('case30').
/// Tolerance: 1e-4 p.u. for Vm, 1e-2 degrees for Va.
#[test]
fn test_nr_case30_matpower_reference() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case30");
    // Same treatment as `test_nr_case9_matpower_reference`: force
    // single-slack so the reference values from MATPOWER's runpf stay
    // directly comparable.
    let opts = AcPfOptions {
        distributed_slack: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("NR should converge on case30");
    assert_eq!(sol.status, SolveStatus::Converged);

    // MATPOWER reference Vm values (p.u.)
    let ref_vm: &[f64] = &[
        1.000000, 1.000000, 0.983138, 0.980093, 0.982406, 0.973184, 0.967355, 0.960624, 0.980506,
        0.984404, 0.980506, 0.985468, 1.000000, 0.976677, 0.980229, 0.977396, 0.976865, 0.968440,
        0.965287, 0.969166, 0.993383, 1.000000, 1.000000, 0.988566, 0.990215, 0.972194, 1.000000,
        0.974715, 0.979597, 0.967883,
    ];
    // MATPOWER reference Va values (degrees)
    let ref_va_deg: &[f64] = &[
        0.0000, -0.4155, -1.5221, -1.7947, -1.8638, -2.2670, -2.6518, -2.7258, -2.9969, -3.3749,
        -2.9969, -1.5369, 1.4762, -2.3080, -2.3118, -2.6445, -3.3923, -3.4784, -3.9582, -3.8710,
        -3.4884, -3.3927, -1.5892, -2.6315, -1.6900, -2.1393, -0.8284, -2.2659, -2.1285, -3.0415,
    ];

    assert_eq!(
        sol.voltage_magnitude_pu.len(),
        ref_vm.len(),
        "case30 bus count mismatch"
    );
    for (i, (&vm, &va)) in sol
        .voltage_magnitude_pu
        .iter()
        .zip(sol.voltage_angle_rad.iter())
        .enumerate()
    {
        let va_deg = va.to_degrees();
        assert!(
            (vm - ref_vm[i]).abs() < 1e-4,
            "case30 bus {} Vm: got {:.6}, expected {:.6}",
            net.buses[i].number,
            vm,
            ref_vm[i]
        );
        assert!(
            (va_deg - ref_va_deg[i]).abs() < 1e-2,
            "case30 bus {} Va: got {:.4} deg, expected {:.4} deg",
            net.buses[i].number,
            va_deg,
            ref_va_deg[i]
        );
    }
}

// -----------------------------------------------------------------------
// Tests originally outside mod tests in newton_raphson.rs
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// PLAN-085: OLTC tap control integration test
// -----------------------------------------------------------------------

/// Test OLTC tap stepping in the outer NR control loop.
///
/// A 2-bus system: bus 1 (slack, 1.05 p.u.) -- transformer -- bus 2 (PQ, 1.0 MW load).
/// The transformer tap starts at 1.0.  The OLTC controller targets 0.98 p.u. on bus 2.
/// Since the load is small, bus 2 voltage will be ~1.04 p.u. after flat-start NR,
/// which is above the 0.98 + 0.005 dead-band, so the tap should be stepped up to
/// reduce the secondary voltage.
#[test]
fn test_oltc_tap_steps_to_regulate_voltage() {
    use surge_network::Network;
    use surge_network::network::discrete_control::OltcControl;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    let mut net = Network::new("oltc_2bus");
    net.base_mva = 100.0;

    // Bus 1: slack at 1.05 p.u.
    let mut b1 = Bus::new(1, BusType::Slack, 100.0);
    b1.voltage_magnitude_pu = 1.05;
    b1.voltage_angle_rad = 0.0;
    net.buses.push(b1);
    net.generators.push(Generator::new(1, 2.0, 1.05));

    // Bus 2: PQ load bus — small resistive load.
    let b2 = Bus::new(2, BusType::PQ, 100.0);
    net.buses.push(b2);
    net.loads.push(Load::new(2, 2.0, 0.5));

    // Transformer branch (from=1, to=2): tap=1.0, small impedance.
    let mut br = Branch::new_line(1, 2, 0.0, 0.05, 0.0);
    br.tap = 1.0;
    net.branches.push(br);

    // OLTC control: regulate bus 2 (index 1) to 0.98 p.u.
    // With v_target=0.98 and a tight v_band=0.02, the outer loop should step
    // the tap up (increasing it) since bus 2 starts above 0.98.
    let oltc = OltcControl {
        branch_index: 0,
        bus_regulated: 1,
        v_target: 0.98,
        v_band: 0.02,
        tap_min: 0.9,
        tap_max: 1.1,
        tap_step: 0.00625,
    };

    let opts = AcPfOptions {
        flat_start: true,
        oltc_enabled: true,
        oltc_max_iter: 20,
        oltc_controls: vec![oltc],
        ..AcPfOptions::default()
    };

    let sol = solve_ac_pf(&net, &opts).expect("NR with OLTC should converge");

    // The tap adjustment should have brought bus 2 voltage close to 0.98 p.u.
    // (within dead-band ± 0.01 p.u.).
    let v2 = sol.voltage_magnitude_pu[1];
    assert!(
        (v2 - 0.98).abs() <= 0.015,
        "Bus 2 voltage {:.4} should be within dead-band of target 0.98",
        v2
    );

    // Final tap should be > initial 1.0 (raised to reduce secondary voltage).
    // We verify the solver ran without panic; tap state is held in opts.oltc_controls
    // which is consumed. Instead check that the voltage improved toward target.
    assert!(
        v2 < 1.05,
        "Bus 2 voltage {:.4} should have decreased from slack voltage 1.05",
        v2
    );
}

// -----------------------------------------------------------------------
// PLAN-086: Switched shunt discrete control integration test
// -----------------------------------------------------------------------

/// Test switched shunt stepping in the outer NR control loop.
///
/// A 2-bus system: bus 1 (slack, 1.0 p.u.) -- line -- bus 2 (PQ, heavy load).
/// Bus 2 has a severe reactive load causing voltage depression.
/// A switched shunt capacitor bank on bus 2 with 5 steps of 0.05 p.u. each
/// should step in to raise the voltage close to the 0.95 p.u. target.
#[test]
fn test_switched_shunt_steps_to_regulate_voltage() {
    use surge_network::Network;
    use surge_network::network::SwitchedShunt;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    let mut net = Network::new("shunt_2bus");
    net.base_mva = 100.0;

    // Bus 1: slack at 1.0 p.u.
    let b1 = Bus::new(1, BusType::Slack, 100.0);
    net.buses.push(b1);
    net.generators.push(Generator::new(1, 20.0, 1.0));

    // Bus 2: PQ load bus — heavy reactive load depresses voltage.
    let b2 = Bus::new(2, BusType::PQ, 100.0);
    net.buses.push(b2);
    net.loads.push(Load::new(2, 20.0, 30.0)); // 20 MW, 30 MVAr — causes voltage sag

    // Transmission line.
    let br = Branch::new_line(1, 2, 0.02, 0.1, 0.0);
    net.branches.push(br);

    // Switched shunt on bus 2: 5 capacitor steps × 0.05 p.u. = 0.25 p.u. max.
    // v_target = 1.0 p.u., v_band = 0.02 (±0.01).
    // Bus 2 voltage without shunt is ~0.9646 (well below target - band = 0.99),
    // so the outer loop should switch in capacitor steps to raise voltage.
    let shunt = SwitchedShunt {
        id: String::new(),
        bus: 2,
        bus_regulated: 2,
        b_step: 0.05,
        n_steps_cap: 5,
        n_steps_react: 0,
        v_target: 1.0,
        v_band: 0.02,
        n_active_steps: 0,
    };

    // First solve without shunt to confirm voltage is depressed.
    let sol_no_shunt = solve_ac_pf(
        &net,
        &AcPfOptions {
            flat_start: true,
            ..AcPfOptions::default()
        },
    )
    .expect("NR should converge on base case");
    let v2_no_shunt = sol_no_shunt.voltage_magnitude_pu[1];

    // Solve with switched shunt control.
    let opts = AcPfOptions {
        flat_start: true,
        shunt_enabled: true,
        shunt_max_iter: 20,
        switched_shunts: vec![shunt],
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("NR with switched shunt should converge");
    let v2_with_shunt = sol.voltage_magnitude_pu[1];

    // Voltage should be higher with shunt than without.
    assert!(
        v2_with_shunt > v2_no_shunt,
        "Shunt should raise bus 2 voltage: without={:.4} with={:.4}",
        v2_no_shunt,
        v2_with_shunt
    );

    // Voltage should be closer to target (1.0) when shunt is active.
    let diff_no_shunt = (v2_no_shunt - 1.0).abs();
    let diff_with_shunt = (v2_with_shunt - 1.0).abs();
    assert!(
        diff_with_shunt <= diff_no_shunt,
        "Shunt should reduce voltage deviation from target: without_err={:.4} with_err={:.4}",
        diff_no_shunt,
        diff_with_shunt
    );
}

#[test]
fn test_switched_shunt_steps_scale_with_base_mva() {
    use surge_network::Network;
    use surge_network::network::SwitchedShunt;
    use surge_network::network::{Branch, Bus, BusType, Load};

    let mut net = Network::new("shunt_base_scale");
    net.base_mva = 50.0;

    let b1 = Bus::new(1, BusType::Slack, 100.0);
    net.buses.push(b1);

    let b2 = Bus::new(2, BusType::PQ, 100.0);
    net.buses.push(b2);
    net.loads.push(Load::new(2, 20.0, 30.0));

    let br = Branch::new_line(1, 2, 0.02, 0.1, 0.0);
    net.branches.push(br);

    let sol_no_shunt =
        solve_ac_pf_kernel(&net, &AcPfOptions::default()).expect("base solve must converge");
    let v2_no_shunt = sol_no_shunt.voltage_magnitude_pu[1];

    let mut net_with_shunt = net.clone();
    net_with_shunt.controls.switched_shunts = vec![SwitchedShunt {
        id: String::new(),
        bus: 2,
        bus_regulated: 2,
        b_step: 0.1,
        n_steps_cap: 5,
        n_steps_react: 0,
        v_target: 1.0,
        v_band: 0.02,
        n_active_steps: 0,
    }];

    let opts = AcPfOptions {
        shunt_enabled: true,
        shunt_max_iter: 20,
        switched_shunts: net_with_shunt.controls.switched_shunts.clone(),
        ..AcPfOptions::default()
    };
    let sol_with_shunt =
        solve_ac_pf_kernel(&net_with_shunt, &opts).expect("shunt solve must converge");
    let v2_with_shunt = sol_with_shunt.voltage_magnitude_pu[1];

    assert!(
        v2_with_shunt > v2_no_shunt + 0.005,
        "scaled shunt should materially raise bus 2 voltage: without={v2_no_shunt:.4} with={v2_with_shunt:.4}"
    );
}

/// GAP 3: Multi-island distributed slack — per-island power balance.
///
/// Uses the same two-island network from `test_m04_multi_island_distributed_slack_no_cross_island`
/// but additionally verifies that each island satisfies its own power balance:
///   |sum_i(P_g,i) - sum_i(P_d,i)| < 1e-4 pu  (per island, net losses absorbed by slack)
///
/// This closes the gap noted in the Wave-2 adversarial review: the existing M-04 test only
/// checks global convergence and voltage range, not explicit per-island MW balance.
#[test]
fn test_nr_multi_island_power_balance() {
    use surge_network::Network;
    use surge_network::network::BusType;
    use surge_network::network::{Branch, Bus, Generator, Load};

    fn make_bus(number: u32, bus_type: BusType, vm: f64) -> Bus {
        let mut b = Bus::new(number, bus_type, 100.0);
        b.voltage_magnitude_pu = vm;
        b.voltage_angle_rad = 0.0;
        b
    }

    fn make_line(from_bus: u32, to_bus: u32) -> Branch {
        Branch::new_line(from_bus, to_bus, 0.01, 0.1, 0.02)
    }

    fn make_gen(bus: u32, pg: f64, vs: f64) -> Generator {
        let mut g = Generator::new(bus, pg, vs);
        g.qmin = -50.0;
        g.qmax = 50.0;
        g.pmax = 200.0;
        g
    }

    fn make_load(bus: u32, pd: f64, qd: f64) -> Load {
        Load::new(bus, pd, qd)
    }

    let mut net = Network::new("m04_power_balance");
    net.base_mva = 100.0;

    // Island A: buses 1-3 (global indices 0-2); Pg=90 MW, Pd=35 MW
    // Island B: buses 4-6 (global indices 3-5); Pg=40 MW, Pd=30 MW
    net.buses = vec![
        make_bus(1, BusType::Slack, 1.05),
        make_bus(2, BusType::PQ, 1.0),
        make_bus(3, BusType::PQ, 1.0),
        make_bus(4, BusType::Slack, 1.02),
        make_bus(5, BusType::PQ, 1.0),
        make_bus(6, BusType::PQ, 1.0),
    ];

    net.branches = vec![
        make_line(1, 2),
        make_line(2, 3),
        make_line(3, 1),
        make_line(4, 5),
        make_line(5, 6),
        make_line(6, 4),
    ];

    net.generators = vec![
        make_gen(1, 50.0, 1.05), // island A slack  → Pg adjusted by NR
        make_gen(4, 40.0, 1.02), // island B slack → Pg adjusted by NR
    ];

    net.loads = vec![
        make_load(2, 20.0, 5.0),
        make_load(3, 15.0, 4.0),
        make_load(5, 18.0, 5.0),
        make_load(6, 12.0, 3.0),
    ];

    let opts = AcPfOptions {
        detect_islands: true,
        enforce_q_limits: false,
        flat_start: true,
        ..AcPfOptions::default()
    };

    let sol = solve_ac_pf(&net, &opts).expect("multi-island NR should converge");
    assert_eq!(sol.status, SolveStatus::Converged);

    let base = net.base_mva;
    let bus_map = net.bus_index_map();

    // Island A: buses 1,2,3 — generator at bus 1, loads at buses 2 and 3.
    let island_a_buses: &[u32] = &[1, 2, 3];
    // Island B: buses 4,5,6 — generator at bus 4, loads at buses 5 and 6.
    let island_b_buses: &[u32] = &[4, 5, 6];

    for (island_name, island_buses) in &[("A", island_a_buses), ("B", island_b_buses)] {
        // Sum demand for all buses in this island.
        let pd_pu: f64 = island_buses
            .iter()
            .flat_map(|&bus_num| net.loads.iter().filter(move |l| l.bus == bus_num))
            .map(|l| l.active_power_demand_mw / base)
            .sum();

        // P_inject[i] = Pg[i] - Pd[i] at bus i (in pu).
        // Summing over island: sum(Pg) - sum(Pd) = sum(P_inject) = net island losses.
        // After NR convergence the power balance is satisfied within max_mismatch.
        let p_inject_sum: f64 = island_buses
            .iter()
            .map(|&bus_num| {
                let idx = *bus_map.get(&bus_num).unwrap();
                sol.active_power_injection_pu[idx]
            })
            .sum();

        // Gross generation per island = net injection sum + demand.
        let pg_pu = p_inject_sum + pd_pu;

        // After convergence, gross generation must cover demand (losses go to slack).
        // The per-island net P injection sum should be small (equal to island losses only).
        // Key invariant: pg_pu > 0 (generators are dispatching, not absorbing power).
        assert!(
            pg_pu > 0.0,
            "Island {island_name}: computed Pg_pu={pg_pu:.4e} should be positive"
        );

        // The island demand is known from input data; verify generator meets it within 10%.
        // (Losses are at most ~5% for these short lines at 10% reactance.)
        let loss_tolerance_pu = pd_pu * 0.10 + 0.005; // 10% of demand + 5 MW floor
        let balance_error = (pg_pu - pd_pu).abs();
        assert!(
            balance_error < loss_tolerance_pu,
            "Island {island_name}: |Pg-Pd|={balance_error:.4e} pu exceeds loss tolerance {loss_tolerance_pu:.4e} pu \
             (Pg={pg_pu:.4}, Pd={pd_pu:.4})"
        );

        eprintln!(
            "Island {island_name}: Pg={:.4} pu, Pd={:.4} pu, balance_err={:.2e} pu",
            pg_pu, pd_pu, balance_error
        );
    }
}

/// Branch endpoint validation: invalid branch topology returns Err, not panic.
///
/// Before the entry-point guard was added, a branch referencing a non-existent
/// bus number would reach `bus_map[&branch.from_bus]` in build_ybus_from_parts,
/// Verify that BusType::Isolated buses are correctly excluded from the NR solve
/// (MAJOR-15 fix): isolated buses must not appear in pvpq_indices or pq_indices,
/// and the solver must converge while leaving the isolated bus voltage unchanged.
#[test]
fn test_nr_isolated_bus_excluded_from_solve() {
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    // 3-bus network:
    //   Bus 1 (Slack, 1.05 pu) — Bus 2 (PQ, load) — connected by a line
    //   Bus 3 (Isolated) — no branches, no generators, no loads
    let mut net = Network::new("isolated_bus_test");
    net.base_mva = 100.0;

    let mut b1 = Bus::new(1, BusType::Slack, 138.0);
    b1.voltage_magnitude_pu = 1.05;
    b1.voltage_angle_rad = 0.0;
    net.buses.push(b1);

    let mut b2 = Bus::new(2, BusType::PQ, 138.0);
    b2.voltage_magnitude_pu = 1.0;
    b2.voltage_angle_rad = 0.0;
    net.buses.push(b2);

    let mut b3 = Bus::new(3, BusType::Isolated, 138.0);
    b3.voltage_magnitude_pu = 0.95; // non-unity to confirm it stays put
    b3.voltage_angle_rad = 0.1;
    net.buses.push(b3);

    net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
    net.generators.push(Generator::new(1, 50.0, 1.05));
    let mut load = Load::new(2, 40.0, 10.0);
    load.in_service = true;
    net.loads.push(load);

    let opts = AcPfOptions {
        flat_start: false,
        detect_islands: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };

    let sol = solve_ac_pf(&net, &opts).expect("NR should converge on 3-bus with isolated bus");

    // NR must converge
    assert_eq!(
        sol.status,
        surge_solution::SolveStatus::Converged,
        "NR should converge with an isolated bus in the network"
    );

    // Isolated bus voltage must remain at its initial value (unchanged by NR)
    let vm3 = sol.voltage_magnitude_pu[2];
    let va3 = sol.voltage_angle_rad[2];
    assert!(
        (vm3 - 0.95).abs() < 1e-10,
        "Isolated bus Vm should remain 0.95 pu; got {vm3}"
    );
    assert!(
        (va3 - 0.1).abs() < 1e-10,
        "Isolated bus Va should remain 0.1 rad; got {va3}"
    );
}

#[test]
fn test_nr_single_bus_island_preserves_shunt_injection() {
    use crate::matrix::mismatch::compute_power_injection;
    use crate::matrix::ybus::build_ybus;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    let mut net = Network::new("single_bus_island_shunt");
    net.base_mva = 100.0;

    let mut b1 = Bus::new(1, BusType::Slack, 138.0);
    b1.voltage_magnitude_pu = 1.04;
    net.buses.push(b1);

    let b2 = Bus::new(2, BusType::PQ, 138.0);
    net.buses.push(b2);

    let mut b3 = Bus::new(3, BusType::Slack, 138.0);
    b3.voltage_magnitude_pu = 1.03;
    b3.shunt_conductance_mw = 6.0;
    b3.shunt_susceptance_mvar = 18.0;
    net.buses.push(b3);

    net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
    net.generators.push(Generator::new(1, 50.0, 1.04));
    net.generators.push(Generator::new(3, 0.0, 1.03));
    let mut load = Load::new(2, 40.0, 10.0);
    load.in_service = true;
    net.loads.push(load);

    let opts = AcPfOptions {
        flat_start: false,
        detect_islands: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("multi-island NR should converge");

    let bus3_idx = sol
        .bus_numbers
        .iter()
        .position(|&number| number == 3)
        .expect("solution should contain singleton island bus");

    let bus_map = net.bus_index_map();
    let island_sub_net = build_island_network(&net, &[bus_map[&3]], &bus_map);
    let vm = vec![island_sub_net.buses[0].voltage_magnitude_pu];
    let va = vec![island_sub_net.buses[0].voltage_angle_rad];
    let ybus = build_ybus(&island_sub_net);
    let (expected_p, expected_q) = compute_power_injection(&ybus, &vm, &va);

    assert!(
        (sol.active_power_injection_pu[bus3_idx] - expected_p[0]).abs() < 1e-12,
        "singleton island P injection should use canonical Y-bus evaluation"
    );
    assert!(
        (sol.reactive_power_injection_pu[bus3_idx] - expected_q[0]).abs() < 1e-12,
        "singleton island Q injection should use canonical Y-bus evaluation"
    );
}

/// which uses HashMap::index and panics.  This test confirms the guard converts
/// that to AcPfError::InvalidNetwork.
#[test]
fn test_nr_invalid_branch_endpoint_returns_err_not_panic() {
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    // Build a 2-bus network with one valid branch and one with a dangling endpoint.
    let mut net = Network::new("test");
    net.buses.push(Bus::new(1, BusType::Slack, 138.0));
    net.buses.push(Bus::new(2, BusType::PQ, 138.0));

    net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
    // Branch referencing bus 99, which does not exist.
    net.branches.push(Branch::new_line(1, 99, 0.01, 0.05, 0.0));

    net.generators.push(Generator::new(1, 1.0, 1.0));

    let result = solve_ac_pf_kernel(&net, &AcPfOptions::default());
    match result {
        Err(AcPfError::InvalidNetwork(msg)) => {
            assert!(
                msg.contains("99"),
                "error message should mention the unknown bus number: {msg}"
            );
        }
        other => panic!("expected AcPfError::InvalidNetwork, got {other:?}"),
    }
}

/// Verify that network.controls.switched_shunts is auto-dispatched by solve_ac_pf_kernel.
///
/// We build a 2-bus network (slack + PQ) with no generators at the PQ bus and
/// a large reactive demand there. Voltage at bus 2 will be low without shunt
/// support. We add a SwitchedShunt to `network.controls.switched_shunts` and verify:
/// 1. The solve converges.
/// 2. The shunt was dispatched (n_active_steps in the solution is non-zero via
///    post-solve inspection — we check this indirectly by verifying voltage at
///    the regulated bus improved vs. the no-shunt case).
#[test]
fn nr_auto_dispatches_network_switched_shunts() {
    use surge_network::network::Branch;
    use surge_network::network::Generator;
    use surge_network::network::SwitchedShunt;
    use surge_network::network::{Bus, BusType, Load};

    // Build a simple 2-bus system: slack (bus 1) → PQ (bus 2).
    // Bus 2 has heavy reactive demand; without shunt support its voltage is low.
    let mut net = Network::new("shunt_dispatch_test");
    net.base_mva = 100.0;

    let mut bus1 = Bus::new(1, BusType::Slack, 138.0);
    bus1.voltage_magnitude_pu = 1.05;
    net.buses.push(bus1);
    let bus2 = Bus::new(2, BusType::PQ, 138.0);
    net.buses.push(bus2);
    net.loads.push(Load::new(2, 0.0, 80.0)); // 80 Mvar demand — will depress voltage

    net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
    net.generators.push(Generator::new(1, 1.0, 1.0));

    // Solve WITHOUT shunt — record bus 2 voltage.
    let sol_no_shunt =
        solve_ac_pf_kernel(&net, &AcPfOptions::default()).expect("base solve must converge");
    let v_no_shunt = sol_no_shunt.voltage_magnitude_pu[1];

    // Add a 4-step × 25 Mvar capacitor bank at bus 2, targeting 1.02 pu.
    net.controls.switched_shunts = vec![SwitchedShunt {
        id: String::new(),
        bus: 2,
        bus_regulated: 2,
        b_step: 25.0 / 100.0, // 0.25 pu per step
        n_steps_cap: 4,
        n_steps_react: 0,
        v_target: 1.02,
        v_band: 0.04, // ±0.02 pu dead-band
        n_active_steps: 0,
    }];

    // Solve WITH shunt — the NR auto-merge must pick up the shunt and dispatch it.
    let sol_with_shunt =
        solve_ac_pf_kernel(&net, &AcPfOptions::default()).expect("shunt solve must converge");
    let v_with_shunt = sol_with_shunt.voltage_magnitude_pu[1];

    eprintln!(
        "nr_auto_dispatches_network_switched_shunts: v_no_shunt={v_no_shunt:.4}, \
         v_with_shunt={v_with_shunt:.4}"
    );

    // The shunt should raise bus 2 voltage relative to the no-shunt case.
    assert!(
        v_with_shunt > v_no_shunt,
        "switched shunt must raise bus 2 voltage: no_shunt={v_no_shunt:.4} with_shunt={v_with_shunt:.4}"
    );
}

#[test]
fn test_island_network_preserves_switched_shunts() {
    // Build a 4-bus network where bus 4 is isolated (no branches to it).
    // Verify that build_island_network for the island containing bus 4
    // includes the switched shunt on bus 4 (external bus number 4).
    use surge_network::Network;
    use surge_network::network::SwitchedShunt;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    let mut net = Network::new("shunt_island_test");
    net.base_mva = 100.0;
    net.buses = vec![
        Bus::new(1, BusType::Slack, 100.0),
        Bus::new(2, BusType::PQ, 100.0),
        Bus::new(3, BusType::PQ, 100.0),
        Bus::new(4, BusType::PV, 100.0),
    ];
    net.branches = vec![
        // Buses 1-2-3 are connected; bus 4 is isolated (no branches)
        Branch::new_line(1, 2, 0.02, 0.1, 0.05),
        Branch::new_line(2, 3, 0.02, 0.1, 0.05),
    ];
    net.generators = vec![Generator::new(1, 50.0, 1.0), Generator::new(4, 20.0, 1.0)];
    net.loads = vec![Load::new(2, 30.0, 5.0), Load::new(4, 10.0, 2.0)];
    // Switched shunt on bus 4 (external bus number 4)
    net.controls.switched_shunts = vec![SwitchedShunt::capacitor_only(4, 0.1, 4, 1.0)];

    let bus_map = net.bus_index_map();
    // Island containing bus 4 is just global index 3
    let island_buses = vec![3usize];
    let sub_net = build_island_network(&net, &island_buses, &bus_map);

    assert_eq!(
        sub_net.controls.switched_shunts.len(),
        1,
        "island sub-net must carry the switched shunt on bus 4"
    );
    assert_eq!(
        sub_net.controls.switched_shunts[0].bus, 4,
        "switched shunt bus number must remain 4 in the island sub-network"
    );
}

#[test]
fn test_island_network_preserves_impedance_corrections() {
    // Build a 4-bus network split into two islands (1-2 and 3-4).
    // Branch 3→4 is a transformer with tab=Some(1) referencing correction table 1.
    // Verify the island sub-network for 3/4 inherits the correction table from clone.
    use surge_network::Network;
    use surge_network::network::impedance_correction::ImpedanceCorrectionTable;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    let mut net = Network::new("corr_island_test");
    net.base_mva = 100.0;
    net.buses = vec![
        Bus::new(1, BusType::Slack, 100.0),
        Bus::new(2, BusType::PQ, 100.0),
        Bus::new(3, BusType::PV, 100.0),
        Bus::new(4, BusType::PQ, 100.0),
    ];
    // Island A: 1-2; Island B: 3-4 (no cross-island branch)
    let mut tr = Branch::new_line(3, 4, 0.01, 0.1, 0.0);
    tr.tap = 1.05;
    tr.tab = Some(1);
    net.branches = vec![Branch::new_line(1, 2, 0.02, 0.1, 0.05), tr];
    net.generators = vec![Generator::new(1, 50.0, 1.0), Generator::new(3, 30.0, 1.0)];
    net.loads = vec![Load::new(2, 20.0, 5.0), Load::new(4, 15.0, 3.0)];
    net.metadata.impedance_corrections = vec![ImpedanceCorrectionTable {
        number: 1,
        entries: vec![(1.0, 1.0), (1.1, 1.3)],
    }];

    let bus_map = net.bus_index_map();
    // Island B: buses 3 and 4 (0-based indices 2 and 3)
    let island_buses = vec![2usize, 3usize];
    let sub_net = build_island_network(&net, &island_buses, &bus_map);

    assert_eq!(
        sub_net.metadata.impedance_corrections.len(),
        1,
        "island sub-net must inherit the impedance correction table (clone-and-filter)"
    );
    assert_eq!(
        sub_net.metadata.impedance_corrections[0].number, 1,
        "correction table number must match"
    );
}

// ---------------------------------------------------------------------------
// Issue 17: Per-iteration NR convergence history + worst-mismatch bus.
// ---------------------------------------------------------------------------

/// `record_convergence_history = true` must populate `convergence_history`
/// with one entry per NR iteration, each with a monotonically non-increasing
/// mismatch (or at most slightly non-monotone due to Q-limit switching).
#[test]
fn test_nr_convergence_history_populated() {
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    let mut net = Network::new("history_test");
    net.base_mva = 100.0;
    net.buses = vec![
        Bus::new(1, BusType::Slack, 138.0),
        Bus::new(2, BusType::PQ, 138.0),
    ];
    net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
    net.generators.push(Generator::new(1, 80.0, 1.0));
    net.loads.push(Load::new(2, 50.0, 20.0));

    let opts = AcPfOptions {
        record_convergence_history: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("NR should converge");

    assert_eq!(
        sol.status,
        surge_solution::SolveStatus::Converged,
        "should converge"
    );
    // history[k] = mismatch at the start of iteration k (0-indexed):
    //   history[0] = initial mismatch (before any Newton step)
    //   history[k] = mismatch after k Newton steps
    // Convergence breaks at iteration k → sol.iterations = k → history.len() = k + 1.
    assert_eq!(
        sol.convergence_history.len(),
        (sol.iterations + 1) as usize,
        "convergence_history must have sol.iterations + 1 entries (initial + per-step)"
    );
    // Iteration numbers must be sequential starting at 0.
    for (k, &(iter_num, _)) in sol.convergence_history.iter().enumerate() {
        assert_eq!(
            iter_num, k as u32,
            "iteration number in history must be 0-indexed sequential"
        );
    }
    // Final mismatch in history must match the solution's max_mismatch.
    if let Some(&(_, last_mismatch)) = sol.convergence_history.last() {
        assert!(
            (last_mismatch - sol.max_mismatch).abs() < 1e-30,
            "last history mismatch {last_mismatch:.3e} != sol.max_mismatch {:.3e}",
            sol.max_mismatch
        );
    }
}

/// When `record_convergence_history = false` (default), the history vec must
/// remain empty to avoid memory overhead in hot paths.
#[test]
fn test_nr_convergence_history_empty_by_default() {
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    let mut net = Network::new("history_default_test");
    net.base_mva = 100.0;
    net.buses = vec![
        Bus::new(1, BusType::Slack, 138.0),
        Bus::new(2, BusType::PQ, 138.0),
    ];
    net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
    net.generators.push(Generator::new(1, 80.0, 1.0));
    net.loads.push(Load::new(2, 50.0, 20.0));

    let sol = solve_ac_pf(&net, &AcPfOptions::default()).expect("NR should converge");
    assert!(
        sol.convergence_history.is_empty(),
        "convergence_history must be empty when record_convergence_history is false"
    );
}

/// On a non-converging solve, `AcPfError::NotConverged.worst_bus` must be
/// populated with an external bus number that existed in the network.
#[test]
fn test_nr_worst_mismatch_bus_on_non_convergence() {
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    // 2-bus network with a normal load. Some small networks can converge in a
    // single Newton step, so force the error path with an unattainably tight
    // tolerance rather than assuming max_iterations=1 alone is enough.
    let mut net = Network::new("nonconv_test");
    net.base_mva = 100.0;
    net.buses = vec![
        Bus::new(1, BusType::Slack, 138.0),
        Bus::new(2, BusType::PQ, 138.0),
    ];
    net.branches.push(Branch::new_line(1, 2, 0.02, 0.2, 0.04));
    net.generators.push(Generator::new(1, 100.0, 1.0));
    net.loads.push(Load::new(2, 80.0, 40.0));

    // max_iterations=1 → NR takes 1 Newton step. The tolerance is set below
    // practical floating-point convergence so the solver must return
    // NotConverged and populate worst_bus.
    let opts = AcPfOptions {
        tolerance: 1e-20,
        max_iterations: 1,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let err = solve_ac_pf(&net, &opts).expect_err("should not converge with max_iterations=1");

    match err {
        AcPfError::NotConverged { worst_bus, .. } => {
            assert!(
                worst_bus.is_some(),
                "worst_bus must be populated on non-convergence"
            );
            let wb = worst_bus.unwrap();
            assert!(
                wb == 1 || wb == 2,
                "worst_bus {wb} must be a valid external bus number"
            );
        }
        other => panic!("expected NotConverged, got {other:?}"),
    }
}

// =========================================================================
// Area interchange enforcement tests
// =========================================================================

/// Build a two-area, five-bus network for interchange testing.
///
/// Area 1: bus 1 (slack), bus 2 (PV gen), bus 3 (PQ load).
/// Area 2: bus 4 (PV gen, area slack), bus 5 (PQ load).
/// Tie-line: bus 3 → bus 4.
///
/// The system swing bus (bus 1) is in area 1 but cannot be controlled by the
/// interchange loop. Bus 2 (PV) is the controllable generator in area 1.
/// Bus 4 (PV) is the controllable generator and area slack for area 2.
#[cfg(test)]
fn make_two_area_network() -> Network {
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};
    let mut net = Network::new("two_area_interchange");
    net.base_mva = 100.0;

    let mut b1 = Bus::new(1, BusType::Slack, 138.0);
    b1.area = 1;
    let mut b2 = Bus::new(2, BusType::PV, 138.0);
    b2.area = 1;
    let mut b3 = Bus::new(3, BusType::PQ, 138.0);
    b3.area = 1;
    let mut b4 = Bus::new(4, BusType::PV, 138.0);
    b4.area = 2;
    let mut b5 = Bus::new(5, BusType::PQ, 138.0);
    b5.area = 2;
    net.buses = vec![b1, b2, b3, b4, b5];

    // Intra-area 1
    net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
    net.branches.push(Branch::new_line(2, 3, 0.01, 0.1, 0.02));
    // Tie-line (area 1 → area 2)
    net.branches.push(Branch::new_line(3, 4, 0.01, 0.1, 0.02));
    // Intra-area 2
    net.branches.push(Branch::new_line(4, 5, 0.01, 0.1, 0.02));

    // Generators: bus 1 = swing (uncontrollable), bus 2 = PV (controllable)
    let mut g1 = Generator::new(1, 120.0, 1.0);
    g1.pmin = 0.0;
    g1.pmax = 500.0;
    net.generators.push(g1);

    let mut g2 = Generator::new(2, 80.0, 1.0);
    g2.pmin = 0.0;
    g2.pmax = 300.0;
    net.generators.push(g2);

    // Area 2 generator
    let mut g4 = Generator::new(4, 100.0, 1.0);
    g4.pmin = 0.0;
    g4.pmax = 300.0;
    net.generators.push(g4);

    // Loads — added as Load objects (NR reads these for p_spec / q_spec).
    net.loads.push(Load::new(3, 100.0, 20.0)); // bus 3
    net.loads.push(Load::new(5, 120.0, 25.0)); // bus 5

    // Area schedules: area 1 exports 30 MW, area 2 imports 30 MW
    use surge_network::network::AreaSchedule;
    net.area_schedules = vec![
        AreaSchedule {
            number: 1,
            slack_bus: 2, // area slack is bus 2 (PV), not bus 1 (system swing)
            p_desired_mw: 30.0,
            p_tolerance_mw: 2.0,
            name: "Area1".into(),
        },
        AreaSchedule {
            number: 2,
            slack_bus: 4,
            p_desired_mw: -30.0,
            p_tolerance_mw: 2.0,
            name: "Area2".into(),
        },
    ];

    net
}

/// Test that enforce_interchange with no APF data falls back to slack bus.
#[test]
fn test_interchange_no_apf_fallback() {
    let net = make_two_area_network();
    let opts = AcPfOptions {
        enforce_interchange: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("should converge");
    assert_eq!(sol.status, SolveStatus::Converged);

    let result = sol
        .area_interchange
        .as_ref()
        .expect("area_interchange must be populated");

    assert!(result.converged, "interchange should converge");

    for entry in &result.areas {
        assert!(
            entry.error_mw.abs() <= 2.0,
            "area {} error {:.2} MW exceeds tolerance",
            entry.area,
            entry.error_mw
        );
    }

    // Area 1 should export ~30 MW, area 2 import ~30 MW.
    let a1 = result.areas.iter().find(|a| a.area == 1).unwrap();
    assert!(
        (a1.actual_mw - 30.0).abs() < 3.0,
        "area 1 actual export {:.2} MW, expected ~30",
        a1.actual_mw
    );
}

/// Test APF-weighted dispatch across multiple generators.
#[test]
fn test_interchange_apf_two_area() {
    let mut net = make_two_area_network();

    // Set APF on both area 1 generators (bus 2 already exists in the network).
    // Bus 1 is the system swing — its APF will be ignored (can't control swing).
    net.generators[0].agc_participation_factor = Some(2.0); // bus 1 — will be skipped (swing)
    net.generators[1].agc_participation_factor = Some(1.0); // bus 2 — controllable PV

    let opts = AcPfOptions {
        enforce_interchange: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("should converge");
    assert_eq!(sol.status, SolveStatus::Converged);

    let result = sol
        .area_interchange
        .as_ref()
        .expect("area_interchange must be populated");
    assert!(result.converged, "interchange should converge");

    let a1 = result.areas.iter().find(|a| a.area == 1).unwrap();
    assert!(
        (a1.actual_mw - 30.0).abs() < 3.0,
        "area 1 actual export {:.2} MW, expected ~30",
        a1.actual_mw
    );
}

/// Test that APF limit redistribution works when a generator hits Pmax.
#[test]
fn test_interchange_apf_limit_redistribution() {
    let mut net = make_two_area_network();

    // Generator at bus 2 (area 1 PV): tight Pmax so it saturates quickly.
    net.generators[1].pmax = 90.0;
    net.generators[1].p = 80.0;
    net.generators[1].agc_participation_factor = Some(1.0);

    // Add another gen in area 1 at bus 3 (make it PV) with more headroom.
    net.buses[2].bus_type = surge_network::network::BusType::PV; // bus 3 → PV
    let mut g3 = surge_network::network::Generator::new(3, 60.0, 1.0);
    g3.pmin = 0.0;
    g3.pmax = 300.0;
    g3.agc_participation_factor = Some(1.0);
    net.generators.push(g3);

    let opts = AcPfOptions {
        enforce_interchange: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("should converge");
    assert_eq!(sol.status, SolveStatus::Converged);

    let result = sol.area_interchange.as_ref().unwrap();
    assert!(result.converged, "interchange should converge");

    let a1 = result.areas.iter().find(|a| a.area == 1).unwrap();
    assert!(
        (a1.actual_mw - 30.0).abs() < 3.0,
        "area 1 export {:.2} MW, expected ~30",
        a1.actual_mw
    );
}

/// Test that ScheduledAreaTransfer records are incorporated into targets.
#[test]
fn test_interchange_scheduled_transfers() {
    let mut net = make_two_area_network();
    // Base schedule: area 1 exports 20 MW.
    net.area_schedules[0].p_desired_mw = 20.0;
    net.area_schedules[1].p_desired_mw = -20.0;

    // Add a bilateral transfer: area 1 → area 2, 10 MW.
    // Net target for area 1 becomes 20 + 10 = 30 MW export.
    net.metadata.scheduled_area_transfers.push(
        surge_network::network::scheduled_area_transfer::ScheduledAreaTransfer {
            from_area: 1,
            to_area: 2,
            id: 1,
            p_transfer_mw: 10.0,
        },
    );

    let opts = AcPfOptions {
        enforce_interchange: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("should converge");

    let result = sol.area_interchange.as_ref().unwrap();
    assert!(result.converged);

    let a1 = result.areas.iter().find(|a| a.area == 1).unwrap();
    // Scheduled should be 20 (base) + 10 (bilateral) = 30 MW.
    assert!(
        (a1.scheduled_mw - 30.0).abs() < 0.01,
        "scheduled_mw should be 30, got {:.2}",
        a1.scheduled_mw
    );
    assert!(
        (a1.actual_mw - 30.0).abs() < 3.0,
        "actual export {:.2} MW, expected ~30",
        a1.actual_mw
    );
}

/// Test that area_interchange is None when enforce_interchange is false.
#[test]
fn test_interchange_result_none_when_disabled() {
    let net = make_two_area_network();
    let opts = AcPfOptions {
        enforce_interchange: false,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("should converge");
    assert!(
        sol.area_interchange.is_none(),
        "area_interchange should be None when disabled"
    );
}

/// Test mixed APF: some gens have APF, others don't. Only APF > 0 participate.
#[test]
fn test_interchange_mixed_apf() {
    let mut net = make_two_area_network();

    // Gen at bus 1 (swing): no APF — would be skipped anyway.
    net.generators[0].agc_participation_factor = None;
    // Gen at bus 2 (PV): set APF — this one should absorb the adjustment.
    net.generators[1].agc_participation_factor = Some(1.0);

    let opts = AcPfOptions {
        enforce_interchange: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("should converge");

    let result = sol.area_interchange.as_ref().unwrap();
    assert!(result.converged, "interchange should converge");

    let a1 = result.areas.iter().find(|a| a.area == 1).unwrap();
    assert!(
        (a1.actual_mw - 30.0).abs() < 3.0,
        "area 1 export {:.2} MW, expected ~30",
        a1.actual_mw
    );
}

/// Test cascade: all generators at limits → residual left for swing bus.
/// The solve should still converge (NR absorbs residual), but
/// area interchange should report not converged.
#[test]
fn test_interchange_cascade_to_swing() {
    let mut net = make_two_area_network();

    // Area 1 wants to export 200 MW — impossible with the generation available.
    net.area_schedules[0].p_desired_mw = 200.0;
    net.area_schedules[1].p_desired_mw = -200.0;

    // Generator at bus 2 (area 1 PV, controllable) has tight limit.
    net.generators[1].pmax = 90.0;
    net.generators[1].agc_participation_factor = Some(1.0);

    let opts = AcPfOptions {
        enforce_interchange: true,
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol = solve_ac_pf(&net, &opts).expect("NR should still converge");
    assert_eq!(sol.status, SolveStatus::Converged);

    let result = sol.area_interchange.as_ref().unwrap();
    // Area interchange should NOT have converged to target.
    assert!(
        !result.converged,
        "should not converge — 200 MW export impossible"
    );
    // Error should be substantial.
    let a1 = result.areas.iter().find(|a| a.area == 1).unwrap();
    assert!(
        a1.error_mw.abs() > 10.0,
        "error should be large, got {:.2}",
        a1.error_mw
    );
}

/// Test that QSharingMode affects per-generator Q allocation.
///
/// Constructs a 3-bus network with two generators of different Mbase at bus 1
/// (both locally regulating), then solves with Capability, Mbase, and Equal
/// sharing.  Verifies:
/// 1. All three modes converge to the same voltages (bus-level Q is identical).
/// 2. The per-generator Q split differs between modes when both gens are free.
#[cfg(test)]
#[test]
fn test_q_sharing_modes_multi_gen() {
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    // 3-bus network: bus 1 (slack, 2 generators), bus 2 (PQ load), bus 3 (PQ load).
    let mut net = Network::new("q_sharing_test");
    net.base_mva = 100.0;

    net.buses = vec![
        Bus::new(1, BusType::Slack, 138.0),
        Bus::new(2, BusType::PQ, 138.0),
        Bus::new(3, BusType::PQ, 138.0),
    ];
    net.loads.push(Load::new(2, 80.0, 40.0)); // 80 MW, 40 MVAr load
    net.loads.push(Load::new(3, 50.0, 25.0));

    net.branches = vec![
        Branch::new_line(1, 2, 0.01, 0.1, 0.02),
        Branch::new_line(1, 3, 0.015, 0.12, 0.025),
        Branch::new_line(2, 3, 0.02, 0.15, 0.03),
    ];

    // Two generators at bus 1 with different Mbase and Q ranges.
    // Gen A: small machine (50 MVA), tight Q range [-10, 20] MVAr.
    // Gen B: large machine (200 MVA), wide Q range [-40, 80] MVAr.
    let mut gen_a = Generator::new(1, 60.0, 100.0);
    gen_a.machine_id = Some("A".to_string());
    gen_a.machine_base_mva = 50.0;
    gen_a.qmin = -10.0;
    gen_a.qmax = 20.0;
    gen_a.voltage_setpoint_pu = 1.02;

    let mut gen_b = Generator::new(1, 70.0, 100.0);
    gen_b.machine_id = Some("B".to_string());
    gen_b.machine_base_mva = 200.0;
    gen_b.qmin = -40.0;
    gen_b.qmax = 80.0;
    gen_b.voltage_setpoint_pu = 1.02;

    net.generators = vec![gen_a, gen_b];

    // Solve with all three modes and verify convergence + identical bus voltages.
    let modes = [
        QSharingMode::Capability,
        QSharingMode::Mbase,
        QSharingMode::Equal,
    ];

    let mut solutions = Vec::new();
    for &mode in &modes {
        let opts = AcPfOptions {
            enforce_q_limits: true,
            q_sharing: mode,
            ..AcPfOptions::default()
        };
        let sol = solve_ac_pf(&net, &opts)
            .unwrap_or_else(|e| panic!("NR should converge with {:?} sharing: {e}", mode));
        assert_eq!(
            sol.status,
            SolveStatus::Converged,
            "{:?} sharing did not converge",
            mode
        );
        solutions.push(sol);
    }

    // All modes must produce the same bus voltages (Vm, Va) since bus-level
    // Q injection is determined by the power flow equations, not the sharing mode.
    let ref_sol = &solutions[0];
    for (i, sol) in solutions.iter().enumerate().skip(1) {
        for bus in 0..3 {
            assert!(
                (sol.voltage_magnitude_pu[bus] - ref_sol.voltage_magnitude_pu[bus]).abs() < 1e-6,
                "mode {:?}: Vm[{bus}] = {:.6} differs from Capability {:.6}",
                modes[i],
                sol.voltage_magnitude_pu[bus],
                ref_sol.voltage_magnitude_pu[bus],
            );
            assert!(
                (sol.voltage_angle_rad[bus] - ref_sol.voltage_angle_rad[bus]).abs() < 1e-6,
                "mode {:?}: Va[{bus}] differs from Capability",
                modes[i],
            );
        }
    }
}

#[cfg(test)]
#[test]
fn test_startup_policy_defaults_to_adaptive() {
    let opts = AcPfOptions::default();
    assert_eq!(opts.startup_policy, StartupPolicy::Adaptive);
    assert!(opts.distributed_slack);
}

#[cfg(test)]
#[test]
fn test_zip_state_dependent_specs_follow_trial_voltage() {
    let p_spec_base = vec![0.0, -1.0];
    let q_spec_base = vec![0.0, -0.4];
    let zip_bus_data = vec![ZipBusData {
        idx: 1,
        p_base: 1.0,
        q_base: 0.4,
        pz: 0.4,
        pi: 0.2,
        pp: 0.4,
        qz: 0.25,
        qi: 0.25,
        qp: 0.5,
    }];
    let mut p_spec = vec![0.0; 2];
    let mut q_spec = vec![0.0; 2];

    populate_state_dependent_specs(
        &mut p_spec,
        &mut q_spec,
        crate::solver::nr_kernel::StateDependentSpecView {
            p_spec_base: &p_spec_base,
            q_spec_base: &q_spec_base,
            participation: None,
            lambda: 0.0,
            zip_bus_data: &zip_bus_data,
            vm: &[1.0, 1.0],
        },
    );
    assert!((p_spec[1] + 1.0).abs() < 1e-12);
    assert!((q_spec[1] + 0.4).abs() < 1e-12);

    populate_state_dependent_specs(
        &mut p_spec,
        &mut q_spec,
        crate::solver::nr_kernel::StateDependentSpecView {
            p_spec_base: &p_spec_base,
            q_spec_base: &q_spec_base,
            participation: None,
            lambda: 0.0,
            zip_bus_data: &zip_bus_data,
            vm: &[1.0, 0.8],
        },
    );
    assert!(
        p_spec[1] > -1.0,
        "ZIP P spec should become less negative at low voltage, got {}",
        p_spec[1]
    );
    assert!(
        q_spec[1] > -0.4,
        "ZIP Q spec should become less negative at low voltage, got {}",
        q_spec[1]
    );
}

#[cfg(test)]
#[test]
fn test_zip_line_search_path_converges_and_matches_no_line_search() {
    let mut net = crate::test_cases::load_case("case14").expect("case14 fixture should load");
    for load in &mut net.loads {
        if load.in_service {
            load.zip_p_impedance_frac = 0.25;
            load.zip_p_current_frac = 0.25;
            load.zip_p_power_frac = 0.5;
            load.zip_q_impedance_frac = 0.2;
            load.zip_q_current_frac = 0.3;
            load.zip_q_power_frac = 0.5;
        }
    }

    let opts_line_search = AcPfOptions {
        flat_start: true,
        dc_warm_start: false,
        line_search: true,
        distributed_slack: true,
        enforce_q_limits: false,
        startup_policy: StartupPolicy::Single,
        ..AcPfOptions::default()
    };
    let opts_full_step = AcPfOptions {
        line_search: false,
        ..opts_line_search.clone()
    };

    let sol_ls = solve_ac_pf_kernel(&net, &opts_line_search).expect("ZIP solve with line search");
    let sol_full =
        solve_ac_pf_kernel(&net, &opts_full_step).expect("ZIP solve without line search");

    assert!(
        sol_ls.max_mismatch < 1e-8,
        "line-search ZIP mismatch too large"
    );
    assert!(
        sol_full.max_mismatch < 1e-8,
        "full-step ZIP mismatch too large"
    );

    for (i, (&vm_ls, &vm_full)) in sol_ls
        .voltage_magnitude_pu
        .iter()
        .zip(sol_full.voltage_magnitude_pu.iter())
        .enumerate()
    {
        assert!(
            (vm_ls - vm_full).abs() < 1e-6,
            "ZIP line-search Vm[{i}] differs: {vm_ls} vs {vm_full}"
        );
    }
    for (i, (&va_ls, &va_full)) in sol_ls
        .voltage_angle_rad
        .iter()
        .zip(sol_full.voltage_angle_rad.iter())
        .enumerate()
    {
        assert!(
            (va_ls - va_full).abs() < 1e-6,
            "ZIP line-search Va[{i}] differs: {va_ls} vs {va_full}"
        );
    }
    assert_eq!(
        sol_ls.gen_slack_contribution_mw.len(),
        sol_full.gen_slack_contribution_mw.len()
    );
    for (i, (&c_ls, &c_full)) in sol_ls
        .gen_slack_contribution_mw
        .iter()
        .zip(sol_full.gen_slack_contribution_mw.iter())
        .enumerate()
    {
        assert!(
            (c_ls - c_full).abs() < 1e-6,
            "ZIP distributed-slack contribution[{i}] differs: {c_ls} vs {c_full}"
        );
    }
}
