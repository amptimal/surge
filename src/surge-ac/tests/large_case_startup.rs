// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

mod common;

use surge_ac::solve_ac_pf_kernel;
use surge_ac::{AcPfOptions, StartupPolicy, WarmStart};
use surge_solution::SolveStatus;

#[test]
#[ignore = "large in-repo canonical cases"]
fn large_case_flat_dc_and_case_warm_converge() {
    for case in ["case13659pegase", "case3120sp", "Polish_model_v33"] {
        let net = common::load_case(case);

        let warm = solve_ac_pf_kernel(
            &net,
            &AcPfOptions {
                flat_start: false,
                enforce_q_limits: false,
                max_iterations: 100,
                startup_policy: StartupPolicy::Single,
                ..AcPfOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{case} case-warm solve failed: {e}"));
        assert_eq!(
            warm.status,
            SolveStatus::Converged,
            "{case} case-warm did not converge"
        );

        let flat_dc = solve_ac_pf_kernel(
            &net,
            &AcPfOptions {
                flat_start: true,
                dc_warm_start: true,
                enforce_q_limits: false,
                max_iterations: 100,
                startup_policy: StartupPolicy::Single,
                ..AcPfOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{case} flat-dc solve failed: {e}"));
        assert_eq!(
            flat_dc.status,
            SolveStatus::Converged,
            "{case} flat-dc did not converge"
        );
    }
}

#[test]
#[ignore = "large in-repo canonical cases"]
fn large_case_prior_warm_start_does_not_increase_iterations() {
    let net = common::load_case("case13659pegase");

    let base = solve_ac_pf_kernel(
        &net,
        &AcPfOptions {
            flat_start: false,
            enforce_q_limits: false,
            max_iterations: 100,
            startup_policy: StartupPolicy::Single,
            ..AcPfOptions::default()
        },
    )
    .expect("base solve should converge");
    assert_eq!(base.status, SolveStatus::Converged);

    let warm = solve_ac_pf_kernel(
        &net,
        &AcPfOptions {
            flat_start: false,
            enforce_q_limits: false,
            warm_start: Some(WarmStart::from_solution(&base)),
            max_iterations: 100,
            startup_policy: StartupPolicy::Single,
            ..AcPfOptions::default()
        },
    )
    .expect("warm-start solve should converge");
    assert_eq!(warm.status, SolveStatus::Converged);
    assert!(
        warm.iterations <= base.iterations,
        "prior warm start should not increase iterations: base={} warm={}",
        base.iterations,
        warm.iterations
    );
}

#[test]
#[ignore = "large in-repo canonical cases"]
fn large_case_adaptive_flat_converges() {
    for case in ["case13659pegase", "case3120sp", "Polish_model_v33"] {
        let net = common::load_case(case);

        let adaptive_flat = solve_ac_pf_kernel(
            &net,
            &AcPfOptions {
                flat_start: true,
                dc_warm_start: false,
                enforce_q_limits: false,
                max_iterations: 100,
                startup_policy: StartupPolicy::Adaptive,
                ..AcPfOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{case} adaptive-flat solve failed: {e}"));
        assert_eq!(
            adaptive_flat.status,
            SolveStatus::Converged,
            "{case} adaptive-flat did not converge"
        );
    }
}

#[test]
#[ignore = "large in-repo canonical cases"]
fn adaptive_flat_recovers_case6470rte() {
    let net = common::load_case("case6470rte");

    let flat = solve_ac_pf_kernel(
        &net,
        &AcPfOptions {
            flat_start: true,
            dc_warm_start: false,
            enforce_q_limits: false,
            max_iterations: 100,
            startup_policy: StartupPolicy::Single,
            ..AcPfOptions::default()
        },
    );
    assert!(
        flat.is_err(),
        "single-shot flat start should fail on case6470rte"
    );

    let adaptive_flat = solve_ac_pf_kernel(
        &net,
        &AcPfOptions {
            flat_start: true,
            dc_warm_start: false,
            enforce_q_limits: false,
            max_iterations: 100,
            startup_policy: StartupPolicy::Adaptive,
            ..AcPfOptions::default()
        },
    )
    .expect("adaptive-flat should recover on case6470rte");
    assert_eq!(adaptive_flat.status, SolveStatus::Converged);
}
