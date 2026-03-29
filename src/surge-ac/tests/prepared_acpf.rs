// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

mod common;

use std::sync::Arc;

use surge_ac::solve_ac_pf_kernel;
use surge_ac::{AcPfOptions, PreparedAcPf, PreparedStart, StartupPolicy, WarmStart};
use surge_solution::SolveStatus;

fn prepared_opts() -> AcPfOptions {
    AcPfOptions {
        detect_islands: false,
        auto_merge_zero_impedance: false,
        enforce_q_limits: false,
        startup_policy: StartupPolicy::Single,
        ..AcPfOptions::default()
    }
}

fn assert_solution_close(
    lhs: &surge_solution::PfSolution,
    rhs: &surge_solution::PfSolution,
    tol: f64,
) {
    assert_eq!(lhs.status, SolveStatus::Converged);
    assert_eq!(rhs.status, SolveStatus::Converged);
    assert_eq!(lhs.iterations, rhs.iterations);
    assert!(
        (lhs.max_mismatch - rhs.max_mismatch).abs() <= tol,
        "max mismatch differs: {} vs {}",
        lhs.max_mismatch,
        rhs.max_mismatch
    );
    for (a, b) in lhs
        .voltage_magnitude_pu
        .iter()
        .zip(rhs.voltage_magnitude_pu.iter())
    {
        assert!((a - b).abs() <= tol, "Vm differs: {a} vs {b}");
    }
    for (a, b) in lhs
        .voltage_angle_rad
        .iter()
        .zip(rhs.voltage_angle_rad.iter())
    {
        assert!((a - b).abs() <= tol, "Va differs: {a} vs {b}");
    }
}

#[test]
fn prepared_matches_case_warm() {
    let net = common::load_case("case9");
    let opts = prepared_opts();
    let direct = solve_ac_pf_kernel(&net, &opts).expect("direct solve should converge");
    let mut prepared =
        PreparedAcPf::new(Arc::new(net.clone()), &opts).expect("prepared solve should build");
    let cached = prepared.solve().expect("prepared solve should converge");
    assert_solution_close(&direct, &cached, 1e-10);
}

#[test]
fn prepared_matches_flat_dc() {
    let net = common::load_case("case9");
    let opts = AcPfOptions {
        flat_start: true,
        dc_warm_start: true,
        ..prepared_opts()
    };
    let direct = solve_ac_pf_kernel(&net, &opts).expect("direct flat-dc solve should converge");
    let mut prepared =
        PreparedAcPf::new(Arc::new(net.clone()), &opts).expect("prepared solve should build");
    let cached = prepared
        .solve()
        .expect("prepared flat-dc solve should converge");
    assert_solution_close(&direct, &cached, 1e-10);
}

#[test]
fn prepared_matches_prior_warm() {
    let net = common::load_case("case9");
    let opts = prepared_opts();
    let seed = solve_ac_pf_kernel(&net, &opts).expect("seed solve should converge");

    let mut warm_opts = opts.clone();
    let ws = WarmStart::from_solution(&seed);
    warm_opts.warm_start = Some(ws.clone());
    let direct =
        solve_ac_pf_kernel(&net, &warm_opts).expect("direct prior-warm solve should converge");

    let mut prepared =
        PreparedAcPf::new(Arc::new(net.clone()), &opts).expect("prepared solve should build");
    let cached = prepared
        .solve_with_start(PreparedStart::Warm(&ws))
        .expect("prepared prior-warm solve should converge");

    assert_solution_close(&direct, &cached, 1e-12);
}
