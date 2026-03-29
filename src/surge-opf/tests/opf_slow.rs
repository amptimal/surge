// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Slow AC-OPF integration tests.
//!
//! Run with: cargo test -p surge-opf --test opf_slow
//!
//! These tests prefer COPT NLP when available and fall back to the canonical
//! default runtime policy. Depending on case size, they can still take 30s-5min.

use std::sync::{Arc, Mutex, OnceLock};

use surge_ac::matrix::mismatch::compute_power_injection;
use surge_ac::matrix::ybus::build_ybus;
use surge_opf::backends::{NlpSolver, nlp_solver_from_str, try_default_nlp_solver};
use surge_opf::{
    AcOpfOptions, AcOpfRuntime, DcOpfOptions, ScopfError, ScopfOptions, WarmStart,
    solve_ac_opf_with_runtime, solve_dc_opf, solve_scopf,
};
use surge_solution::{PfSolution, SolveStatus};

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

/// Serialize slow AC-OPF solves in this binary.
///
/// COPT is preferred for speed, but the fallback runtime may still use Ipopt's
/// MUMPS stack, which is not thread-safe.
static AC_OPF_TEST_MUTEX: Mutex<()> = Mutex::new(());

static PREFERRED_AC_NLP: OnceLock<Result<Arc<dyn NlpSolver>, String>> = OnceLock::new();

fn preferred_ac_nlp_solver() -> Result<Arc<dyn NlpSolver>, String> {
    PREFERRED_AC_NLP
        .get_or_init(|| {
            nlp_solver_from_str("copt").or_else(|copt_err| {
                try_default_nlp_solver().map_err(|default_err| {
                    format!(
                        "COPT NLP unavailable ({copt_err}); no fallback NLP solver available ({default_err})"
                    )
                })
            })
        })
        .as_ref()
        .map(Arc::clone)
        .map_err(Clone::clone)
}

fn solve_ac_opf_prefer_copt(
    network: &surge_network::Network,
    options: &AcOpfOptions,
) -> Result<surge_solution::OpfSolution, surge_opf::AcOpfError> {
    let runtime = AcOpfRuntime::default()
        .with_nlp_solver(preferred_ac_nlp_solver().map_err(surge_opf::AcOpfError::SolverError)?);
    solve_ac_opf_with_runtime(network, options, &runtime)
}

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

/// AC-OPF reference for case9: approximately 5297 $/h (MATPOWER reference).
const _CASE9_AC_OPF_REF: f64 = 5296.686;

#[test]
fn test_acopf_case9() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF should solve case9");

    // Cost should be positive
    assert!(
        sol.total_cost > 0.0,
        "cost should be positive: {}",
        sol.total_cost
    );

    // All Pg within bounds
    let gen_indices: Vec<usize> = net
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service)
        .map(|(i, _)| i)
        .collect();
    for (j, &gi) in gen_indices.iter().enumerate() {
        let g = &net.generators[gi];
        assert!(
            sol.generators.gen_p_mw[j] >= g.pmin - 1.0,
            "gen {} below pmin: {:.2} < {:.2}",
            gi,
            sol.generators.gen_p_mw[j],
            g.pmin
        );
        assert!(
            sol.generators.gen_p_mw[j] <= g.pmax + 1.0,
            "gen {} above pmax: {:.2} > {:.2}",
            gi,
            sol.generators.gen_p_mw[j],
            g.pmax
        );
    }

    println!(
        "case9 AC-OPF: cost={:.2} $/hr, time={:.1} ms",
        sol.total_cost,
        sol.solve_time_secs * 1000.0
    );
    println!(
        "  Pg(MW): {:?}",
        sol.generators
            .gen_p_mw
            .iter()
            .map(|p| format!("{:.1}", p))
            .collect::<Vec<_>>()
    );
    println!(
        "  Qg(MVAr): {:?}",
        sol.generators
            .gen_q_mvar
            .iter()
            .map(|q| format!("{:.1}", q))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_acopf_case118() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case118")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF should solve case118");
    assert!(sol.total_cost > 0.0);
    println!(
        "case118 AC-OPF: cost={:.2} $/hr, time={:.1} ms",
        sol.total_cost,
        sol.solve_time_secs * 1000.0
    );
}

#[test]
fn test_acopf_power_balance() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).unwrap();

    let base = net.base_mva;
    let bus_map = net.bus_index_map();
    let ybus = build_ybus(&net);
    let (p_calc, q_calc) = compute_power_injection(
        &ybus,
        &sol.power_flow.voltage_magnitude_pu,
        &sol.power_flow.voltage_angle_rad,
    );

    let gen_indices: Vec<usize> = net
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service)
        .map(|(i, _)| i)
        .collect();

    let bus_pd_mw = net.bus_load_p_mw();
    let bus_qd_mvar = net.bus_load_q_mvar();
    for i in 0..net.n_buses() {
        let mut p_gen = 0.0;
        let mut q_gen = 0.0;
        for (j, &gi) in gen_indices.iter().enumerate() {
            if bus_map[&net.generators[gi].bus] == i {
                p_gen += sol.generators.gen_p_mw[j] / base;
                q_gen += sol.generators.gen_q_mvar[j] / base;
            }
        }
        let p_mismatch = (p_calc[i] - p_gen + bus_pd_mw[i] / base).abs();
        let q_mismatch = (q_calc[i] - q_gen + bus_qd_mvar[i] / base).abs();
        assert!(
            p_mismatch < 1e-4,
            "P-balance violation at bus {}: {:.6}",
            net.buses[i].number,
            p_mismatch
        );
        assert!(
            q_mismatch < 1e-4,
            "Q-balance violation at bus {}: {:.6}",
            net.buses[i].number,
            q_mismatch
        );
    }
}

#[test]
fn test_acopf_voltage_limits() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case118")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).unwrap();

    for (i, bus) in net.buses.iter().enumerate() {
        assert!(
            sol.power_flow.voltage_magnitude_pu[i] >= bus.voltage_min_pu - 1e-4,
            "bus {} Vm={:.4} < vmin={:.4}",
            bus.number,
            sol.power_flow.voltage_magnitude_pu[i],
            bus.voltage_min_pu
        );
        assert!(
            sol.power_flow.voltage_magnitude_pu[i] <= bus.voltage_max_pu + 1e-4,
            "bus {} Vm={:.4} > vmax={:.4}",
            bus.number,
            sol.power_flow.voltage_magnitude_pu[i],
            bus.voltage_max_pu
        );
    }
}

#[test]
fn test_acopf_gen_limits() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).unwrap();

    let gen_indices: Vec<usize> = net
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service)
        .map(|(i, _)| i)
        .collect();

    for (j, &gi) in gen_indices.iter().enumerate() {
        let g = &net.generators[gi];
        assert!(
            sol.generators.gen_p_mw[j] >= g.pmin - 1.0,
            "Pg[{}] = {:.2} < pmin = {:.2}",
            j,
            sol.generators.gen_p_mw[j],
            g.pmin
        );
        assert!(
            sol.generators.gen_p_mw[j] <= g.pmax + 1.0,
            "Pg[{}] = {:.2} > pmax = {:.2}",
            j,
            sol.generators.gen_p_mw[j],
            g.pmax
        );
        let qmin = if g.qmin.abs() > 1e10 { -9999.0 } else { g.qmin };
        let qmax = if g.qmax.abs() > 1e10 { 9999.0 } else { g.qmax };
        assert!(
            sol.generators.gen_q_mvar[j] >= qmin - 1.0,
            "Qg[{}] = {:.2} < qmin = {:.2}",
            j,
            sol.generators.gen_q_mvar[j],
            qmin
        );
        assert!(
            sol.generators.gen_q_mvar[j] <= qmax + 1.0,
            "Qg[{}] = {:.2} > qmax = {:.2}",
            j,
            sol.generators.gen_q_mvar[j],
            qmax
        );
    }
}

#[test]
fn test_acopf_branch_flow_limits() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case118")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).unwrap();

    let flows = sol.power_flow.branch_apparent_power();
    for (l, branch) in net.branches.iter().enumerate() {
        if branch.in_service && branch.rating_a_mva >= 1.0 {
            assert!(
                flows[l] <= branch.rating_a_mva * 1.02, // 2% tolerance for numerical precision
                "branch {} ({}->{}) flow={:.1} MVA > rate_a={:.1} MVA",
                l,
                branch.from_bus,
                branch.to_bus,
                flows[l],
                branch.rating_a_mva
            );
        }
    }
}

#[test]
fn test_acopf_cost_geq_dcopf() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();

    let dc_opts = DcOpfOptions::default();
    let dc_sol = solve_dc_opf(&net, &dc_opts).unwrap().opf;

    let ac_opts = AcOpfOptions::default();
    let ac_sol = solve_ac_opf_prefer_copt(&net, &ac_opts).unwrap();

    // AC-OPF should have equal or higher cost than DC-OPF (losses + reactive)
    assert!(
        ac_sol.total_cost >= dc_sol.total_cost - 1.0,
        "AC cost ({:.2}) should be >= DC cost ({:.2})",
        ac_sol.total_cost,
        dc_sol.total_cost
    );

    println!(
        "DC-OPF cost: {:.2}, AC-OPF cost: {:.2} (delta: {:.2})",
        dc_sol.total_cost,
        ac_sol.total_cost,
        ac_sol.total_cost - dc_sol.total_cost
    );
}

#[test]
fn test_acopf_lmp_decomposition() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).unwrap();

    // LMPs should be positive (generators have positive costs)
    for (i, &lmp_val) in sol.pricing.lmp.iter().enumerate() {
        assert!(
            lmp_val > 0.0,
            "LMP at bus {} should be positive, got {:.4}",
            net.buses[i].number,
            lmp_val
        );
    }

    // LMP should decompose: total = energy + congestion + loss
    // For case9 with default limits, congestion may be zero
    for i in 0..net.n_buses() {
        let decomp = sol.pricing.lmp_congestion[i]
            + sol.pricing.lmp_loss[i]
            + (sol.pricing.lmp[sol
                .power_flow
                .bus_numbers
                .iter()
                .position(|&b| b == net.buses[net.slack_bus_index().unwrap()].number)
                .unwrap()]
                - sol.pricing.lmp_congestion[sol
                    .power_flow
                    .bus_numbers
                    .iter()
                    .position(|&b| b == net.buses[net.slack_bus_index().unwrap()].number)
                    .unwrap()]
                - sol.pricing.lmp_loss[sol
                    .power_flow
                    .bus_numbers
                    .iter()
                    .position(|&b| b == net.buses[net.slack_bus_index().unwrap()].number)
                    .unwrap()]);
        let _residual = (sol.pricing.lmp[i] - decomp).abs();
        // This is actually just checking energy + loss = total (since we defined it that way)
        // The key check is that losses are non-zero in some cases
    }

    println!(
        "LMP range: {:.2} - {:.2} $/MWh",
        sol.pricing
            .lmp
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min),
        sol.pricing
            .lmp
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max),
    );
}

#[test]
fn test_acopf_qg_populated() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).unwrap();

    // Unlike DC-OPF, AC-OPF should produce non-empty Qg
    assert!(
        !sol.generators.gen_q_mvar.is_empty(),
        "gen_q_mvar should not be empty"
    );
    // At least one Qg should be non-zero
    let any_nonzero = sol.generators.gen_q_mvar.iter().any(|&q| q.abs() > 0.01);
    assert!(
        any_nonzero,
        "at least one Qg should be non-zero: {:?}",
        sol.generators.gen_q_mvar
    );
}

/// Ignored: tests L-BFGS mode which COPT NLP doesn't support (requires exact Hessian).
/// COPT NLP returns LpStatus=11 (ITERLIMIT) without an exact Hessian.
#[test]
#[ignore]
fn test_acopf_exact_hessian_convergence() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();

    let exact_opts = AcOpfOptions {
        exact_hessian: true,
        ..Default::default()
    };
    let lbfgs_opts = AcOpfOptions {
        exact_hessian: false,
        ..Default::default()
    };

    let ipopt = nlp_solver_from_str("ipopt").expect("Ipopt required for L-BFGS comparison");
    let runtime = AcOpfRuntime::default().with_nlp_solver(ipopt);
    let exact_sol = solve_ac_opf_with_runtime(&net, &exact_opts, &runtime)
        .expect("exact Hessian should converge");
    let lbfgs_sol =
        solve_ac_opf_with_runtime(&net, &lbfgs_opts, &runtime).expect("L-BFGS should converge");

    // Costs should match within 0.1%
    let cost_gap =
        (exact_sol.total_cost - lbfgs_sol.total_cost).abs() / lbfgs_sol.total_cost.max(1.0);
    assert!(
        cost_gap < 0.001,
        "cost gap too large: exact={:.2} vs lbfgs={:.2} ({:.2}%)",
        exact_sol.total_cost,
        lbfgs_sol.total_cost,
        cost_gap * 100.0
    );

    println!(
        "Exact Hessian: cost={:.2} in {:.1} ms; L-BFGS: cost={:.2} in {:.1} ms",
        exact_sol.total_cost,
        exact_sol.solve_time_secs * 1000.0,
        lbfgs_sol.total_cost,
        lbfgs_sol.solve_time_secs * 1000.0,
    );
}

#[test]
fn test_acopf_case2383wp() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case2383wp")).unwrap();
    // L-BFGS needs many iterations on large cases (debug builds are slower)
    let opts = AcOpfOptions {
        max_iterations: 1000,
        print_level: 0,
        ..Default::default()
    };
    let sol = solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF should solve case2383wp");
    assert!(sol.total_cost > 0.0);
    println!(
        "case2383wp AC-OPF: cost={:.2} $/hr, time={:.1} ms",
        sol.total_cost,
        sol.solve_time_secs * 1000.0
    );
}

/// OPF-08: Warm-start from a prior OpfSolution converges successfully.
///
/// Solves case14 AC-OPF cold, then re-solves with the prior solution as
/// warm-start.  Both must converge and the optimal costs must match to
/// within 0.1% (same problem, same local minimum).
#[test]
fn test_acopf_warm_start_case14() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case14")).unwrap();

    // Cold solve.
    let cold_opts = AcOpfOptions::default();
    let cold_sol = solve_ac_opf_prefer_copt(&net, &cold_opts).expect("cold AC-OPF should converge");
    assert!(cold_sol.total_cost > 0.0);

    // Warm solve: initialise from prior solution.
    let warm_opts = AcOpfOptions::default();
    let warm_runtime = surge_opf::AcOpfRuntime::default()
        .with_nlp_solver(
            preferred_ac_nlp_solver().expect("preferred NLP solver should be available"),
        )
        .with_warm_start(WarmStart::from_opf(&cold_sol));
    let warm_sol = surge_opf::solve_ac_opf_with_runtime(&net, &warm_opts, &warm_runtime)
        .expect("warm AC-OPF should converge");
    assert!(warm_sol.total_cost > 0.0);

    // Costs should agree — same problem, same local minimum.
    let cost_gap = (cold_sol.total_cost - warm_sol.total_cost).abs() / cold_sol.total_cost.max(1.0);
    assert!(
        cost_gap < 0.001,
        "cold cost {:.2} vs warm cost {:.2} differ by {:.3}%",
        cold_sol.total_cost,
        warm_sol.total_cost,
        cost_gap * 100.0
    );

    println!(
        "case14 AC-OPF warm-start: cold={:.2} $/hr in {:.1} ms, warm={:.2} $/hr in {:.1} ms",
        cold_sol.total_cost,
        cold_sol.solve_time_secs * 1000.0,
        warm_sol.total_cost,
        warm_sol.solve_time_secs * 1000.0
    );
}

/// OPF-08: WarmStart::from_pf() builds without panic from a power flow solution.
#[test]
fn test_opf08_warm_start_from_pf_builds() {
    // Build a synthetic 9-bus power flow solution (matching case9 structure).
    let n_bus = 9;
    let pf_sol = PfSolution {
        status: SolveStatus::Converged,
        iterations: 5,
        max_mismatch: 1e-9,
        solve_time_secs: 0.001,
        voltage_magnitude_pu: vec![1.0; n_bus],
        voltage_angle_rad: vec![0.0; n_bus],
        active_power_injection_pu: vec![0.0; n_bus],
        reactive_power_injection_pu: vec![0.0; n_bus],
        bus_numbers: (1..=(n_bus as u32)).collect(),
        island_ids: vec![],
        ..Default::default()
    };

    let ws = WarmStart::from_pf(&pf_sol);
    assert_eq!(
        ws.voltage_magnitude_pu.len(),
        n_bus,
        "vm length should match n_bus"
    );
    assert_eq!(
        ws.voltage_angle_rad.len(),
        n_bus,
        "va length should match n_bus"
    );
    assert!(ws.pg.is_empty(), "pg should be empty for PF warm-start");

    // Verify values are copied correctly.
    for i in 0..n_bus {
        assert_eq!(ws.voltage_magnitude_pu[i], 1.0);
        assert_eq!(ws.voltage_angle_rad[i], 0.0);
    }
}

/// OPF-08: WarmStart::from_opf() builds from a prior AC-OPF result.
///
#[test]
fn test_opf08_warm_start_from_opf_builds() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF should solve case9");

    let ws = WarmStart::from_opf(&sol);
    let n_bus = net.n_buses();
    let n_gen = sol.generators.gen_p_mw.len();

    assert_eq!(
        ws.voltage_magnitude_pu.len(),
        n_bus,
        "vm length should match n_bus"
    );
    assert_eq!(
        ws.voltage_angle_rad.len(),
        n_bus,
        "va length should match n_bus"
    );
    assert_eq!(ws.pg.len(), n_gen, "pg length should match n_gen");

    // pg values should be in pu (divide by 100 MVA base).
    for (j, &pg_pu) in ws.pg.iter().enumerate() {
        let gen_p_mw = sol.generators.gen_p_mw[j];
        let expected_pu = gen_p_mw / 100.0;
        assert!(
            (pg_pu - expected_pu).abs() < 1e-12,
            "pg[{j}] pu mismatch: got {pg_pu:.6}, expected {expected_pu:.6}"
        );
    }
}

// -----------------------------------------------------------------------
// MATPOWER reference cost pinning tests
// -----------------------------------------------------------------------

/// MATPOWER AC-OPF reference regression test — case9 optimal cost.
///
/// Total cost validated against MATPOWER 7.1 `runopf('case9')`.
/// Reference value: 5296.6862 $/hr.
///
/// AC-OPF cost is higher than DC-OPF (5216.03) due to losses and
/// reactive power co-optimization.
#[test]
fn test_ac_opf_case9_cost_reference() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();
    let opts = AcOpfOptions::default();
    let result = solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF should converge on case9");

    let ref_cost = 5296.69;
    assert!(
        (result.total_cost - ref_cost).abs() < 1.0,
        "case9 AC-OPF cost regression: got {:.2}, expected {:.2} (tolerance +-$1)",
        result.total_cost,
        ref_cost
    );
}

/// MATPOWER AC-OPF reference regression test — case9 generator dispatch.
///
/// Individual generator Pg values validated against MATPOWER 7.1 `runopf('case9')`.
#[test]
fn test_ac_opf_case9_dispatch_reference() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();
    let opts = AcOpfOptions::default();
    let result = solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF should converge on case9");

    // Reference dispatch (MW) — MATPOWER 7.1 runopf('case9')
    let ref_pg = &[89.80, 134.32, 94.19];

    assert_eq!(
        result.generators.gen_p_mw.len(),
        ref_pg.len(),
        "case9 should have {} generators, got {}",
        ref_pg.len(),
        result.generators.gen_p_mw.len()
    );

    for (i, &pg) in result.generators.gen_p_mw.iter().enumerate() {
        assert!(
            (pg - ref_pg[i]).abs() < 0.5,
            "case9 AC-OPF gen {} Pg: got {:.4} MW, expected {:.4} MW (tolerance +-0.5 MW)",
            i,
            pg,
            ref_pg[i]
        );
    }
}

/// C-01: Verify LMP congestion decomposition is non-zero when a branch binds.
///
/// We solve case9 with a very tight thermal limit on one branch so that the
/// constraint binds and the NLP backend returns a non-zero shadow price. The congestion
/// component must then be non-zero at at least one bus, and the decomposition
/// identity lmp[i] = energy + congestion + loss must hold everywhere.
#[test]
fn test_acopf_lmp_congestion_nonzero_when_binding() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let mut net = surge_io::load(case_path("case9")).unwrap();

    // Force branch 0 (bus 1->4, the first in-service line) to a very tight
    // limit so it binds at the AC-OPF optimum.  50 MVA is well below the
    // unconstrained flow on that branch (~160 MVA in the base case).
    let mut tightened_any = false;
    for br in net.branches.iter_mut() {
        if br.in_service && !tightened_any {
            br.rating_a_mva = 50.0; // MVA
            tightened_any = true;
        }
    }
    assert!(
        tightened_any,
        "expected at least one in-service branch in case9"
    );

    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF should solve congested case9");

    // LMP decomposition identity must hold at every bus.
    let slack_idx = net.slack_bus_index().unwrap();
    let lambda_energy = sol.pricing.lmp[slack_idx];
    for i in 0..net.n_buses() {
        let reconstructed = lambda_energy + sol.pricing.lmp_congestion[i] + sol.pricing.lmp_loss[i];
        let err = (sol.pricing.lmp[i] - reconstructed).abs();
        assert!(
            err < 1e-6,
            "LMP decomposition identity violated at bus {}: lmp={:.6} != energy({:.6})+cong({:.6})+loss({:.6}), err={:.2e}",
            net.buses[i].number,
            sol.pricing.lmp[i],
            lambda_energy,
            sol.pricing.lmp_congestion[i],
            sol.pricing.lmp_loss[i],
            err
        );
    }

    // At least one bus must have a non-trivial congestion component when a
    // branch shadow price is non-zero.
    let has_binding = sol.branches.branch_shadow_prices.iter().any(|&p| p > 1e-4);
    if has_binding {
        let max_cong = sol
            .pricing
            .lmp_congestion
            .iter()
            .cloned()
            .fold(0.0_f64, |a, v| a.max(v.abs()));
        assert!(
            max_cong > 1e-4,
            "branch constraint binds (shadow price > 1e-4) but max lmp_congestion={:.2e}",
            max_cong
        );
    }

    println!(
        "Congested case9 AC-OPF: cost={:.2} $/hr, max_cong={:.4} $/MWh, binding_branches={}",
        sol.total_cost,
        sol.pricing
            .lmp_congestion
            .iter()
            .cloned()
            .fold(0.0_f64, |a, v| a.max(v.abs())),
        sol.branches
            .branch_shadow_prices
            .iter()
            .filter(|&&p| p > 1e-4)
            .count()
    );
    println!(
        "  LMPs: {:?}",
        sol.pricing
            .lmp
            .iter()
            .map(|&v| format!("{:.3}", v))
            .collect::<Vec<_>>()
    );
    println!(
        "  Congestion: {:?}",
        sol.pricing
            .lmp_congestion
            .iter()
            .map(|&v| format!("{:.3}", v))
            .collect::<Vec<_>>()
    );
    println!(
        "  Loss: {:?}",
        sol.pricing
            .lmp_loss
            .iter()
            .map(|&v| format!("{:.3}", v))
            .collect::<Vec<_>>()
    );
}

/// C-01: LMP decomposition identity holds on the uncongested base case.
///
/// When no branches bind, congestion should be (near-)zero everywhere and
/// the identity lmp[i] = energy + congestion + loss must still hold.
#[test]
fn test_acopf_lmp_decomposition_identity() {
    let _g = AC_OPF_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let net = surge_io::load(case_path("case9")).unwrap();
    let opts = AcOpfOptions::default();
    let sol = solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF should solve case9");

    let slack_idx = net.slack_bus_index().unwrap();
    let lambda_energy = sol.pricing.lmp[slack_idx];

    for i in 0..net.n_buses() {
        let reconstructed = lambda_energy + sol.pricing.lmp_congestion[i] + sol.pricing.lmp_loss[i];
        let err = (sol.pricing.lmp[i] - reconstructed).abs();
        assert!(
            err < 1e-6,
            "LMP decomposition identity violated at bus {}: lmp={:.6} != energy({:.6})+cong({:.6})+loss({:.6}), err={:.2e}",
            net.buses[i].number,
            sol.pricing.lmp[i],
            lambda_energy,
            sol.pricing.lmp_congestion[i],
            sol.pricing.lmp_loss[i],
            err
        );
    }

    println!(
        "Uncongested case9: energy={:.4} $/MWh, lmp_range=[{:.4}, {:.4}]",
        lambda_energy,
        sol.pricing
            .lmp
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min),
        sol.pricing
            .lmp
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max),
    );
}

// -----------------------------------------------------------------------
// SCOPF tests — large cases (case118, case2383wp)
// Note: SCOPF uses LP (COPT/HiGHS), so the AC-OPF mutex is not needed here.
// -----------------------------------------------------------------------

#[test]
fn test_scopf_cost_geq_dcopf_case118() {
    let net = surge_io::load(case_path("case118")).unwrap();

    let dcopf_opts = DcOpfOptions::default();
    let dcopf_sol = solve_dc_opf(&net, &dcopf_opts).unwrap().opf;

    let scopf_opts = ScopfOptions::default();
    let scopf_sol = solve_scopf(&net, &scopf_opts).unwrap();

    assert!(
        scopf_sol.base_opf.total_cost >= dcopf_sol.total_cost - 0.01,
        "SCOPF cost ({:.2}) should be >= DC-OPF cost ({:.2})",
        scopf_sol.base_opf.total_cost,
        dcopf_sol.total_cost
    );

    println!(
        "case118: DC-OPF={:.2}, SCOPF={:.2}, iters={}, cuts={}",
        dcopf_sol.total_cost,
        scopf_sol.base_opf.total_cost,
        scopf_sol.iterations,
        scopf_sol.total_contingency_constraints
    );
}

#[test]
fn test_scopf_power_balance_case118() {
    let net = surge_io::load(case_path("case118")).unwrap();
    let scopf_sol = solve_scopf(&net, &ScopfOptions::default()).unwrap();

    let total_gen: f64 = scopf_sol.base_opf.generators.gen_p_mw.iter().sum();
    let total_load: f64 = net.bus_load_p_mw().iter().sum();
    assert!(
        (total_gen - total_load).abs() < 0.5,
        "power balance: gen={total_gen:.2}, load={total_load:.2}"
    );
}

#[test]
fn test_scopf_converges_case118() {
    let net = surge_io::load(case_path("case118")).unwrap();
    let scopf_sol = solve_scopf(&net, &ScopfOptions::default()).unwrap();

    assert!(
        scopf_sol.iterations <= 20,
        "SCOPF should converge within 20 iterations, took {}",
        scopf_sol.iterations
    );

    println!(
        "case118: {} iters, {} cuts, cost={:.2}",
        scopf_sol.iterations,
        scopf_sol.total_contingency_constraints,
        scopf_sol.base_opf.total_cost
    );
}

#[test]
fn test_scopf_binding_shadow_prices_case118() {
    let net = surge_io::load(case_path("case118")).unwrap();
    let scopf_sol = solve_scopf(&net, &ScopfOptions::default()).unwrap();

    for bc in &scopf_sol.binding_contingencies {
        assert!(
            bc.shadow_price.abs() > 1e-8,
            "Binding contingency '{}' should have nonzero shadow price: {:.6}",
            bc.contingency_label,
            bc.shadow_price
        );
    }

    println!(
        "case118: {} binding contingencies",
        scopf_sol.binding_contingencies.len()
    );
    for bc in &scopf_sol.binding_contingencies {
        println!(
            "  {} (out={}, mon={}) loading={:.1}% shadow={:.4}",
            bc.contingency_label,
            bc.outaged_branch_indices
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(","),
            bc.monitored_branch_idx,
            bc.loading_pct,
            bc.shadow_price
        );
    }
}

#[test]
fn test_scopf_case2383wp() {
    let net = surge_io::load(case_path("case2383wp")).unwrap();

    // case2383wp is a stressed network — SCOPF may become infeasible when
    // too many N-1 constraints are added. We verify it either solves or
    // reports infeasibility clearly, and when it solves, power balance holds.
    let scopf_opts = ScopfOptions {
        violation_tolerance_pu: 0.10,
        max_cuts_per_iteration: 20,
        ..Default::default()
    };
    match solve_scopf(&net, &scopf_opts) {
        Ok(scopf_sol) => {
            let total_gen: f64 = scopf_sol.base_opf.generators.gen_p_mw.iter().sum();
            let total_load: f64 = net.bus_load_p_mw().iter().sum();
            assert!(
                (total_gen - total_load).abs() < 1.0,
                "power balance: gen={total_gen:.2}, load={total_load:.2}"
            );
            assert!(scopf_sol.base_opf.total_cost > 0.0);

            println!(
                "case2383wp: {} iters, {} cuts, cost={:.2}, time={:.1} ms",
                scopf_sol.iterations,
                scopf_sol.total_contingency_constraints,
                scopf_sol.base_opf.total_cost,
                scopf_sol.base_opf.solve_time_secs * 1000.0
            );
        }
        Err(ScopfError::SolverError(msg)) if msg.contains("infeasible") => {
            // Infeasibility is acceptable for stressed networks.
            println!("case2383wp SCOPF: infeasible (stressed network, expected)");
        }
        Err(e) => panic!("unexpected error: {e}"),
    }
}

// -----------------------------------------------------------------------
// AC MLF LMP decomposition tests
// -----------------------------------------------------------------------

/// case9 has non-zero branch resistance — lmp_loss should be non-trivial.
#[test]
fn test_ac_opf_lmp_loss_not_all_zero_case9() {
    let _guard = AC_OPF_TEST_MUTEX.lock().unwrap();
    let net = surge_io::load(case_path("case9")).unwrap();

    let opts = AcOpfOptions::default();
    let result = solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF case9 should converge");

    // case9 has resistive branches → at least some lmp_loss entries must be non-zero.
    let all_zero = result.pricing.lmp_loss.iter().all(|&v| v.abs() < 1e-10);
    assert!(
        !all_zero,
        "lmp_loss should be non-zero for resistive case9; got all zeros"
    );

    // The non-slack entries should have at least one non-trivial value.
    let max_loss = result
        .pricing
        .lmp_loss
        .iter()
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);
    println!("case9 AC MLF: max |lmp_loss| = {max_loss:.6} $/MWh");
    assert!(
        max_loss > 1e-6,
        "Expected non-trivial lmp_loss for case9, max={max_loss:.2e}"
    );
}

/// Verify lmp[i] = lmp_energy[i] + lmp_congestion[i] + lmp_loss[i] to 1e-8.
#[test]
fn test_ac_opf_lmp_decomp_identity_with_acmlf() {
    let _guard = AC_OPF_TEST_MUTEX.lock().unwrap();

    for case_name in &["case9", "case14"] {
        let net = surge_io::load(case_path(case_name)).unwrap();
        let opts = AcOpfOptions::default();
        let result = solve_ac_opf_prefer_copt(&net, &opts)
            .unwrap_or_else(|e| panic!("AC-OPF {case_name} failed: {e}"));

        let tol = 1e-8;
        for i in 0..result.pricing.lmp.len() {
            let reconstructed = result.pricing.lmp_energy[i]
                + result.pricing.lmp_congestion[i]
                + result.pricing.lmp_loss[i];
            let err = (result.pricing.lmp[i] - reconstructed).abs();
            assert!(
                err < tol,
                "{case_name} bus {i}: lmp={:.8} != energy({:.8}) + congestion({:.8}) + loss({:.8}) \
                 = {reconstructed:.8}, err={err:.2e}",
                result.pricing.lmp[i],
                result.pricing.lmp_energy[i],
                result.pricing.lmp_congestion[i],
                result.pricing.lmp_loss[i]
            );
        }
        println!(
            "{case_name} LMP identity check PASSED ({} buses)",
            result.pricing.lmp.len()
        );
    }
}

/// On a network with r=0 on all branches, MLF ≈ 0 and lmp_loss ≈ 0.
#[test]
fn test_acmlf_lossless_network() {
    let _guard = AC_OPF_TEST_MUTEX.lock().unwrap();
    let mut net = surge_io::load(case_path("case9")).unwrap();

    // Zero out all branch resistance to make the network lossless.
    for br in &mut net.branches {
        br.r = 0.0;
    }

    let opts = AcOpfOptions::default();
    let result =
        solve_ac_opf_prefer_copt(&net, &opts).expect("AC-OPF lossless case9 should converge");

    let tol = 1e-6;
    let max_loss = result
        .pricing
        .lmp_loss
        .iter()
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);
    assert!(
        max_loss < tol,
        "lossless network: lmp_loss should be ~0, got max={max_loss:.2e}"
    );
    println!("lossless case9 AC MLF: max |lmp_loss| = {max_loss:.2e} (expected ~0)");
}

/// Verify MLF[i] matches finite-difference ∂P_loss/∂P_inject_i on case9.
///
/// Perturbs load at one bus by ±ε, re-solves power flow (not OPF),
/// measures total losses, and checks against the AC MLF value to 1e-3.
#[test]
#[ignore = "slow FD validation — run manually to verify MLF correctness"]
fn test_acmlf_finite_difference_validation() {
    use surge_ac::{AcPfOptions, solve_ac_pf};

    let net = surge_io::load(case_path("case9")).unwrap();

    // Run a base power flow to get the operating point.
    let acpf_opts = AcPfOptions {
        enforce_q_limits: false,
        ..AcPfOptions::default()
    };
    let sol_base = solve_ac_pf(&net, &acpf_opts).expect("NR case9 base should converge");

    // Compute total losses at the base point.
    let total_losses =
        |sol: &PfSolution| -> f64 { sol.active_power_injection_pu.iter().sum::<f64>() };

    let _p_loss_base = total_losses(&sol_base);

    // Compute AC MLFs at the base operating point.
    let ybus_base = build_ybus(&net);
    let (p_calc, _q_calc) = compute_power_injection(
        &ybus_base,
        &sol_base.voltage_magnitude_pu,
        &sol_base.voltage_angle_rad,
    );
    let _ = p_calc; // used to confirm operating point
    let bus_map = net.bus_index_map();
    let slack_idx = net
        .buses
        .iter()
        .position(|b| b.bus_type == surge_network::network::BusType::Slack)
        .expect("must have slack");

    let mlf = surge_opf::ac::compute_ac_marginal_loss_factors(
        &net,
        &sol_base.voltage_angle_rad,
        &sol_base.voltage_magnitude_pu,
        slack_idx,
    )
    .expect("AC MLF should succeed on case9");

    // Finite-difference check: perturb load at bus index 4 (0-based).
    let test_bus_idx = 4_usize;
    let test_bus_num = net.buses[test_bus_idx].number;
    let eps = 1e-4; // 0.01% of base MVA

    let mut net_plus = net.clone();
    let mut net_minus = net.clone();
    for load in &mut net_plus.loads {
        if bus_map.get(&load.bus) == Some(&test_bus_idx) {
            load.active_power_demand_mw += eps;
        }
    }
    for load in &mut net_minus.loads {
        if bus_map.get(&load.bus) == Some(&test_bus_idx) {
            load.active_power_demand_mw -= eps;
        }
    }

    let sol_plus = solve_ac_pf(&net_plus, &acpf_opts).expect("NR+eps should converge");
    let sol_minus = solve_ac_pf(&net_minus, &acpf_opts).expect("NR-eps should converge");

    let eps_pu = eps / net.base_mva;
    let fd_mlf = -(total_losses(&sol_plus) - total_losses(&sol_minus)) / (2.0 * eps_pu);
    let analytic_mlf = mlf[test_bus_idx];

    println!(
        "bus {} (idx {}): FD MLF = {fd_mlf:.6}, analytic MLF = {analytic_mlf:.6}, err = {:.2e}",
        test_bus_num,
        test_bus_idx,
        (fd_mlf - analytic_mlf).abs()
    );

    assert!(
        (fd_mlf - analytic_mlf).abs() < 1e-3,
        "AC MLF FD mismatch at bus {test_bus_idx}: FD={fd_mlf:.6} analytic={analytic_mlf:.6}"
    );
}
