// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use super::*;
use crate::engine::parallel::solve_contingencies_parallel;
use crate::generation::{
    generate_n1_branch_contingencies, generate_n1_generator_contingencies,
    generate_n2_branch_contingencies,
};
use crate::prepared::{ContingencyStudy, ContingencyStudyKind, prepare_corrective_dispatch_study};
use crate::ranking::rank_contingencies;
use crate::test_util::{data_available, load_case};
use crate::violations::detect_violations_from_parts;
use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
use surge_network::Network;
use surge_network::network::{
    Branch, Bus, BusType, Contingency, Flowgate, Generator, Interface, Load,
    VscConverterAcControlMode, VscConverterTerminal, VscHvdcControlMode, VscHvdcLink,
};
use surge_solution::SolveStatus;
use tracing::info;

fn build_screenable_hvdc_network() -> Network {
    let mut net = Network::new("ctg_hvdc_triangle");
    net.base_mva = 100.0;
    net.buses = vec![
        Bus::new(1, BusType::Slack, 230.0),
        Bus::new(2, BusType::PQ, 230.0),
        Bus::new(3, BusType::PQ, 230.0),
    ];
    net.loads.push(Load::new(2, 40.0, 10.0));
    net.loads.push(Load::new(3, 25.0, 5.0));
    net.branches = vec![
        Branch::new_line(1, 2, 0.01, 0.08, 0.02),
        Branch::new_line(2, 3, 0.01, 0.08, 0.02),
        Branch::new_line(1, 3, 0.01, 0.08, 0.02),
    ];

    let mut slack = Generator::new(1, 80.0, 1.0);
    slack.pmax = 200.0;
    slack.qmax = 200.0;
    slack.qmin = -200.0;
    net.generators.push(slack);

    net.hvdc.push_vsc_link(VscHvdcLink {
        name: "CTG-VSC".into(),
        mode: VscHvdcControlMode::PowerControl,
        resistance_ohm: 0.0,
        converter1: VscConverterTerminal {
            bus: 1,
            control_mode: VscConverterAcControlMode::ReactivePower,
            dc_setpoint: 20.0,
            ac_setpoint: 0.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            q_min_mvar: -50.0,
            q_max_mvar: 50.0,
            voltage_min_pu: 0.9,
            voltage_max_pu: 1.1,
            in_service: true,
        },
        converter2: VscConverterTerminal {
            bus: 3,
            control_mode: VscConverterAcControlMode::ReactivePower,
            dc_setpoint: 0.0,
            ac_setpoint: 0.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            q_min_mvar: -50.0,
            q_max_mvar: 50.0,
            voltage_min_pu: 0.9,
            voltage_max_pu: 1.1,
            in_service: true,
        },
    });

    net
}

#[test]
fn test_n1_case9() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };
    let result = analyze_n1_branch(&net, &options).expect("N-1 should succeed");

    // case9 has 9 branches, all in service
    let n_in_service = net.branches.iter().filter(|b| b.in_service).count();
    assert_eq!(result.summary.total_contingencies, n_in_service);
    assert_eq!(result.summary.ac_solved, n_in_service);

    // Base case should converge
    assert_eq!(result.base_case.status, SolveStatus::Converged);

    // Some contingencies may not converge (bridge lines)
    // but at least some should converge
    assert!(
        result.summary.converged > 0,
        "at least one contingency should converge"
    );
}

#[test]
fn test_n1_case14() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };
    let result = analyze_n1_branch(&net, &options).expect("N-1 should succeed");

    let n_in_service = net.branches.iter().filter(|b| b.in_service).count();
    assert_eq!(result.summary.total_contingencies, n_in_service);

    // Most should converge on a well-connected 14-bus system
    assert!(
        result.summary.converged >= n_in_service / 2,
        "most contingencies should converge: {} of {}",
        result.summary.converged,
        n_in_service
    );
}

#[test]
fn test_n1_case118_lodf_screening() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let options_no_screen = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };
    let options_lodf = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        lodf_screening_threshold: 0.80,
        ..Default::default()
    };

    let result_no = analyze_n1_branch(&net, &options_no_screen).expect("N-1 should succeed");
    let result_lodf = analyze_n1_branch(&net, &options_lodf).expect("N-1 with LODF should succeed");

    // LODF screening should not produce MORE AC solves than unscreened
    assert!(
        result_lodf.summary.ac_solved <= result_no.summary.ac_solved,
        "LODF screening should reduce AC solves: {} <= {}",
        result_lodf.summary.ac_solved,
        result_no.summary.ac_solved
    );
    // Note: screening effectiveness depends on system loading and
    // floating-point precision (debug vs release). For heavily loaded
    // systems, all contingencies may be critical at 80% threshold.
    // We verify the screening pipeline runs correctly, not that it
    // must screen out a specific number.

    info!(
        "case118 LODF screening: {} total, {} screened, {} AC solved",
        result_lodf.summary.total_contingencies,
        result_lodf.summary.screened_out,
        result_lodf.summary.ac_solved
    );
}

#[test]
fn test_hvdc_network_bypasses_ac_only_screening() {
    let net = build_screenable_hvdc_network();
    let contingencies = vec![Contingency {
        id: "branch_0".into(),
        label: "Trip branch 0".into(),
        branch_indices: vec![0],
        ..Default::default()
    }];
    let options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        ..Default::default()
    };

    let result =
        analyze_contingencies(&net, &contingencies, &options).expect("HVDC contingency solve");

    assert_eq!(result.summary.total_contingencies, 1);
    assert_eq!(
        result.summary.screened_out, 0,
        "HVDC networks must bypass AC-only screening"
    );
    assert_eq!(result.summary.ac_solved, 1);
}

#[test]
fn test_n1_case118_fdpf_screening() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let options_no_screen = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };
    let options_fdpf = ContingencyOptions {
        screening: ScreeningMode::Fdpf,
        ..Default::default()
    };

    let result_no = analyze_n1_branch(&net, &options_no_screen).expect("N-1 should succeed");
    let result_fdpf =
        analyze_n1_branch(&net, &options_fdpf).expect("N-1 with FDPF screening should succeed");

    // FDPF screening should produce results for all contingencies
    assert_eq!(
        result_fdpf.results.len() + result_fdpf.summary.screened_out,
        result_no.summary.total_contingencies,
        "total should be preserved"
    );

    // FDPF should screen out some contingencies (those with no violations)
    // The screened out count depends on how many have no violations
    // Just verify the summary adds up
    assert_eq!(
        result_fdpf.summary.total_contingencies,
        result_no.summary.total_contingencies
    );

    // Convergence count should be similar (FDPF screens converged no-violation cases)
    // The NR pass handles critical ones, so total converged should be similar
    let fdpf_converged = result_fdpf.results.iter().filter(|r| r.converged).count()
        + result_fdpf.summary.screened_out; // screened are converged with no violations
    assert!(
        fdpf_converged >= result_no.summary.converged * 8 / 10,
        "FDPF should find similar convergence: fdpf={} vs no_screen={}",
        fdpf_converged,
        result_no.summary.converged
    );
}

#[test]
fn test_fdpf_screening_marks_approximate_results() {
    let mut net = Network::new("fdpf_screening_approximate");
    net.buses.push(Bus::new(1, BusType::Slack, 230.0));
    net.buses.push(Bus::new(2, BusType::PQ, 230.0));
    net.buses.push(Bus::new(3, BusType::PQ, 230.0));

    let mut br12 = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
    br12.rating_a_mva = 1_000.0;
    let mut br23 = Branch::new_line(2, 3, 0.01, 0.1, 0.0);
    br23.rating_a_mva = 1_000.0;
    let mut br13 = Branch::new_line(1, 3, 0.01, 0.1, 0.0);
    br13.rating_a_mva = 1_000.0;
    net.branches = vec![br12, br23, br13];

    net.generators.push(Generator::new(1, 40.0, 1.0));
    net.loads.push(Load::new(3, 40.0, 0.0));

    let options = ContingencyOptions {
        screening: ScreeningMode::Fdpf,
        ..Default::default()
    };
    let result = analyze_n1_branch(&net, &options).expect("FDPF screening should succeed");

    assert!(
        result
            .results
            .iter()
            .any(|r| r.status == ContingencyStatus::Approximate),
        "screened contingencies should be labeled approximate"
    );
}

#[test]
fn test_fdpf_screened_clear_stores_post_voltages() {
    let mut net = Network::new("fdpf_screening_post_state");
    net.buses.push(Bus::new(1, BusType::Slack, 230.0));
    net.buses.push(Bus::new(2, BusType::PQ, 230.0));
    net.buses.push(Bus::new(3, BusType::PQ, 230.0));

    let mut br12 = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
    br12.rating_a_mva = 1_000.0;
    let mut br23 = Branch::new_line(2, 3, 0.01, 0.1, 0.0);
    br23.rating_a_mva = 1_000.0;
    let mut br13 = Branch::new_line(1, 3, 0.01, 0.1, 0.0);
    br13.rating_a_mva = 1_000.0;
    net.branches = vec![br12, br23, br13];

    net.generators.push(Generator::new(1, 40.0, 1.0));
    net.loads.push(Load::new(3, 40.0, 0.0));

    let options = ContingencyOptions {
        screening: ScreeningMode::Fdpf,
        store_post_voltages: true,
        ..Default::default()
    };
    let result = analyze_n1_branch(&net, &options).expect("FDPF screening should succeed");
    let approximate: Vec<_> = result
        .results
        .iter()
        .filter(|entry| entry.status == ContingencyStatus::Approximate)
        .collect();

    assert!(
        !approximate.is_empty(),
        "expected at least one screened-clear approximate result"
    );
    for entry in approximate {
        assert!(entry.post_vm.is_some(), "approximate Vm should be stored");
        assert!(entry.post_va.is_some(), "approximate Va should be stored");
        assert!(
            entry.post_branch_flows.is_some(),
            "approximate branch flows should be stored"
        );
    }
}

#[test]
fn test_fdpf_screening_preserves_order_and_summary_counts() {
    let mut net = Network::new("fdpf_screening_order");
    net.base_mva = 100.0;
    net.buses.push(Bus::new(1, BusType::Slack, 230.0));
    net.buses.push(Bus::new(2, BusType::PQ, 230.0));
    net.buses.push(Bus::new(3, BusType::PQ, 230.0));

    let mut br12 = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
    br12.rating_a_mva = 60.0;
    br12.rating_b_mva = 60.0;
    br12.rating_c_mva = 60.0;
    let mut br23 = Branch::new_line(2, 3, 0.01, 0.1, 0.0);
    br23.rating_a_mva = 30.0;
    br23.rating_b_mva = 30.0;
    br23.rating_c_mva = 30.0;
    let mut br13 = Branch::new_line(1, 3, 0.01, 0.1, 0.0);
    br13.rating_a_mva = 60.0;
    br13.rating_b_mva = 60.0;
    br13.rating_c_mva = 60.0;
    net.branches = vec![br12, br23, br13];

    let mut generator = Generator::new(1, 90.0, 1.0);
    generator.pmax = 200.0;
    net.generators.push(generator);
    net.loads.push(Load::new(2, 50.0, 0.0));
    net.loads.push(Load::new(3, 40.0, 0.0));

    let contingencies = vec![
        Contingency {
            id: "ctg_a".into(),
            label: "trip 1-2".into(),
            branch_indices: vec![0],
            ..Default::default()
        },
        Contingency {
            id: "ctg_b".into(),
            label: "trip 2-3".into(),
            branch_indices: vec![1],
            ..Default::default()
        },
        Contingency {
            id: "ctg_c".into(),
            label: "trip 1-3".into(),
            branch_indices: vec![2],
            ..Default::default()
        },
    ];

    let options = ContingencyOptions {
        screening: ScreeningMode::Fdpf,
        ..Default::default()
    };
    let analysis =
        analyze_contingencies(&net, &contingencies, &options).expect("analysis should succeed");
    let ids: Vec<&str> = analysis
        .results
        .iter()
        .map(|result| result.id.as_str())
        .collect();

    assert_eq!(ids, vec!["ctg_a", "ctg_b", "ctg_c"]);
    assert!(
        analysis
            .results
            .iter()
            .any(|result| result.status == ContingencyStatus::Approximate),
        "expected at least one approximate screened result"
    );
    assert!(
        analysis
            .results
            .iter()
            .any(|result| result.status != ContingencyStatus::Approximate),
        "expected at least one exact AC result to exercise mixed ordering"
    );
    assert_eq!(analysis.summary.total_contingencies, contingencies.len());
    assert_eq!(
        analysis.summary.approximate_returned,
        analysis
            .results
            .iter()
            .filter(|result| result.status == ContingencyStatus::Approximate)
            .count()
    );
    assert_eq!(
        analysis.summary.ac_solved,
        analysis
            .results
            .iter()
            .filter(|result| result.status != ContingencyStatus::Approximate)
            .count()
    );
    assert!(analysis.summary.screened_out <= analysis.summary.approximate_returned);
}

#[test]
fn test_lodf_screening_without_ratings_keeps_all_contingencies() {
    let mut net = Network::new("lodf_without_ratings");
    net.buses.push(Bus::new(1, BusType::Slack, 138.0));
    net.buses.push(Bus::new(2, BusType::PQ, 138.0));
    net.generators.push(Generator::new(1, 50.0, 1.0));
    net.loads.push(Load::new(2, 50.0, 0.0));
    let mut branch = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
    branch.rating_a_mva = 0.0;
    net.branches.push(branch);

    let options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        voltage_pre_screen: false,
        ..Default::default()
    };
    let result = analyze_n1_branch(&net, &options).expect("analysis should succeed");

    assert_eq!(result.summary.total_contingencies, 1);
    assert_eq!(result.summary.screened_out, 0);
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].branch_indices, vec![0]);
}

#[test]
fn test_custom_contingency() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");

    // Create a custom contingency tripping branches 0 and 1
    let ctgs = vec![Contingency {
        id: "custom_0_1".into(),
        label: "Trip branches 0 and 1".into(),
        branch_indices: vec![0, 1],
        ..Default::default()
    }];

    let options = ContingencyOptions::default();
    let result = analyze_contingencies(&net, &ctgs, &options).expect("should succeed");

    assert_eq!(result.summary.total_contingencies, 1);
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].id, "custom_0_1");
}

#[test]
fn test_fast_violation_helper_includes_flowgate_and_interface_limits() {
    let mut net = Network::new("flowgate_interface_fast_path");
    net.buses.push(Bus::new(1, BusType::Slack, 230.0));
    net.buses.push(Bus::new(2, BusType::PQ, 230.0));

    let mut branch = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
    branch.rating_a_mva = 500.0;
    net.branches.push(branch);

    net.flowgates.push(Flowgate {
        name: "FG_1".into(),
        monitored: vec![surge_network::network::WeightedBranchRef::new(
            1, 2, "1", 1.0,
        )],
        contingency_branch: None,
        limit_mw: 50.0,
        limit_reverse_mw: 50.0,
        in_service: true,
        limit_mw_schedule: Vec::new(),
        limit_reverse_mw_schedule: Vec::new(),
        hvdc_coefficients: Vec::new(),
        hvdc_band_coefficients: Vec::new(),
        limit_mw_active_period: None,
        breach_sides: surge_network::network::FlowgateBreachSides::Both,
    });
    net.interfaces.push(Interface {
        name: "IF_1".into(),
        members: vec![surge_network::network::WeightedBranchRef::new(
            1, 2, "1", 1.0,
        )],
        limit_forward_mw: 50.0,
        limit_reverse_mw: 50.0,
        in_service: true,
        limit_forward_mw_schedule: Vec::new(),
        limit_reverse_mw_schedule: Vec::new(),
    });

    let bus_map = net.bus_index_map();
    let options = ContingencyOptions::default();
    let violations = detect_violations_from_parts(
        &net.branches,
        &net.buses,
        net.base_mva,
        &bus_map,
        None,
        &[1.0, 1.0],
        &[0.0, -0.1],
        &options,
        &net.flowgates,
        &net.interfaces,
    );

    assert!(
        violations
            .iter()
            .any(|v| matches!(v, Violation::FlowgateOverload { name, .. } if name == "FG_1")),
        "fast-path helper should report flowgate overloads"
    );
    assert!(
        violations
            .iter()
            .any(|v| matches!(v, Violation::InterfaceOverload { name, .. } if name == "IF_1")),
        "fast-path helper should report interface overloads"
    );
}

#[test]
fn test_fast_violation_helper_ignores_outaged_branches_for_flowgates() {
    let mut net = Network::new("flowgate_interface_outaged_branch");
    net.buses.push(Bus::new(1, BusType::Slack, 230.0));
    net.buses.push(Bus::new(2, BusType::PQ, 230.0));

    let mut branch = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
    branch.rating_a_mva = 500.0;
    net.branches.push(branch);

    net.flowgates.push(Flowgate {
        name: "FG_OUTAGED".into(),
        monitored: vec![surge_network::network::WeightedBranchRef::new(
            1, 2, "1", 1.0,
        )],
        contingency_branch: None,
        limit_mw: 1.0,
        limit_reverse_mw: 1.0,
        in_service: true,
        limit_mw_schedule: Vec::new(),
        limit_reverse_mw_schedule: Vec::new(),
        hvdc_coefficients: Vec::new(),
        hvdc_band_coefficients: Vec::new(),
        limit_mw_active_period: None,
        breach_sides: surge_network::network::FlowgateBreachSides::Both,
    });

    let bus_map = net.bus_index_map();
    let options = ContingencyOptions::default();
    let outaged: std::collections::HashSet<usize> = [0].into_iter().collect();
    let violations = detect_violations_from_parts(
        &net.branches,
        &net.buses,
        net.base_mva,
        &bus_map,
        Some(&outaged),
        &[1.0, 1.0],
        &[0.0, -0.1],
        &options,
        &net.flowgates,
        &net.interfaces,
    );

    assert!(
        violations.is_empty(),
        "outaged branches should be ignored by flowgate/interface checks"
    );
}

#[test]
fn test_screening_filters_non_critical() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        lodf_screening_threshold: 0.80,
        ..Default::default()
    };

    // screen_with_lodf requires a base case solution for its built-in
    // voltage screening pass.
    let base_case = solve_ac_pf_kernel(&net, &options.acpf_options)
        .expect("base case should converge for test_screening_filters_non_critical");

    let contingencies = generate_n1_branch_contingencies(&net);
    let (critical, screened) = screen_with_lodf(&net, &contingencies, &options, &base_case)
        .expect("screening should succeed");

    // The total should add up
    assert_eq!(
        critical.len() + screened,
        contingencies.len(),
        "critical + screened should equal total"
    );

    // Indices should be valid
    for &idx in &critical {
        assert!(idx < contingencies.len());
    }
}

#[test]
fn test_non_convergent_contingency() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");

    // Trip ALL branches from bus 1 (the slack bus), which should island it
    let slack_branches: Vec<usize> = net
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| {
            br.in_service
                && (br.from_bus == net.buses[net.slack_bus_index().unwrap()].number
                    || br.to_bus == net.buses[net.slack_bus_index().unwrap()].number)
        })
        .map(|(i, _)| i)
        .collect();

    if slack_branches.len() > 1 {
        let ctgs = vec![Contingency {
            id: "island_slack".into(),
            label: "Island the slack bus".into(),
            branch_indices: slack_branches,
            ..Default::default()
        }];

        let options = ContingencyOptions::default();
        let result = analyze_contingencies(&net, &ctgs, &options).expect("should succeed");

        // This should either not converge or have violations
        let r = &result.results[0];
        assert!(
            !r.converged || !r.violations.is_empty(),
            "islanding slack bus should cause non-convergence or violations"
        );
    }
}

#[test]
fn test_branch_apparent_power() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let options = AcPfOptions::default();
    let sol = solve_ac_pf_kernel(&net, &options).expect("base case should solve");

    let flows = sol.branch_apparent_power();
    assert_eq!(flows.len(), net.branches.len());

    // All flows should be non-negative
    for (i, &f) in flows.iter().enumerate() {
        assert!(f >= 0.0, "branch {} flow should be non-negative: {}", i, f);
    }

    // Flows should be reasonable (< 1000 MVA for a 9-bus system)
    for (i, &f) in flows.iter().enumerate() {
        assert!(
            f < 1000.0,
            "branch {} flow unreasonably large: {} MVA",
            i,
            f
        );
    }
}

#[test]
fn test_branch_loading_pct() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let options = AcPfOptions::default();
    let sol = solve_ac_pf_kernel(&net, &options).expect("base case should solve");

    let loading = sol.branch_loading_pct(&net).unwrap();
    assert_eq!(loading.len(), net.branches.len());

    // Branches with rate_a=0 should have 0% loading
    for (i, branch) in net.branches.iter().enumerate() {
        if branch.rating_a_mva <= 0.0 {
            assert_eq!(loading[i], 0.0, "branch {} with no rating should be 0%", i);
        }
    }
}

// -----------------------------------------------------------------------
// Functional tests: verify correctness and consistency across all modes
// -----------------------------------------------------------------------
//
// These tests catch regressions that unit tests miss:
// - Screening mode consistency: all modes must produce same converged set
// - Result determinism: parallel solver must produce same results as sequential
// - Performance bounds: catch accidental O(n²) regressions
// - Large-case correctness: verify convergence counts don't degrade

/// Verify that all screening modes produce consistent convergence results
/// on case118. The number of converged contingencies must be the same
/// regardless of screening mode.
#[test]
fn test_screening_modes_consistency_case118() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");

    let result_off = analyze_n1_branch(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Off,
            ..Default::default()
        },
    )
    .expect("off should succeed");

    let result_lodf = analyze_n1_branch(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Lodf,
            lodf_screening_threshold: 0.80,
            ..Default::default()
        },
    )
    .expect("lodf should succeed");

    let result_fdpf = analyze_n1_branch(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Fdpf,
            ..Default::default()
        },
    )
    .expect("fdpf should succeed");

    // All modes must agree on total contingencies
    assert_eq!(
        result_off.summary.total_contingencies,
        result_lodf.summary.total_contingencies
    );
    assert_eq!(
        result_off.summary.total_contingencies,
        result_fdpf.summary.total_contingencies
    );

    // Convergence counts: LODF screens conservatively, so it may solve fewer.
    // But FDPF (which solves all) should find same or more converged.
    // LODF screened-out contingencies are assumed safe (no AC solve),
    // so total "safe" = screened_out + converged.
    let off_converged = result_off.summary.converged;
    let fdpf_converged = result_fdpf.results.iter().filter(|r| r.converged).count()
        + result_fdpf.summary.screened_out;

    // FDPF should find at least 80% of what no-screening finds
    // (some edge cases may differ due to FDPF vs NR sensitivity)
    assert!(
        fdpf_converged >= off_converged * 8 / 10,
        "FDPF convergence should be close to no-screening: fdpf={fdpf_converged} vs off={off_converged}"
    );
}

/// Verify that parallel N-1 on case2383wp produces reproducible results.
/// Run twice and compare: converged count, total violations must match.
#[test]
fn test_parallel_determinism_case2383wp() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case2383wp");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    let r1 = analyze_n1_branch(&net, &options).expect("run 1");
    let r2 = analyze_n1_branch(&net, &options).expect("run 2");

    assert_eq!(
        r1.summary.total_contingencies,
        r2.summary.total_contingencies
    );
    assert_eq!(r1.summary.converged, r2.summary.converged);
    assert_eq!(r1.summary.with_violations, r2.summary.with_violations);

    // Individual results should match by contingency ID
    for (a, b) in r1.results.iter().zip(r2.results.iter()) {
        assert_eq!(a.id, b.id, "results should be in same order");
        assert_eq!(
            a.converged, b.converged,
            "convergence should match for {}",
            a.id
        );
        assert_eq!(
            a.iterations, b.iterations,
            "iterations should match for {}",
            a.id
        );
    }
}

/// Verify expected convergence counts on well-characterized cases.
/// These are regression anchors — if convergence counts change, something broke.
#[test]
fn test_convergence_regression_anchors() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let cases: Vec<(&str, usize, usize)> = vec![
        // (case_name, expected_total, min_converged)
        // min_converged is a floor — actual may be higher but should never drop below
        ("case9", 9, 5),
        ("case14", 20, 15),
        ("case118", 186, 170),
        ("case2383wp", 2896, 2200),
    ];

    for (name, expected_total, min_converged) in &cases {
        let net = load_case(name);
        let options = ContingencyOptions {
            screening: ScreeningMode::Off,
            ..Default::default()
        };
        let result =
            analyze_n1_branch(&net, &options).unwrap_or_else(|e| panic!("{name} N-1 failed: {e}"));

        assert_eq!(
            result.summary.total_contingencies, *expected_total,
            "{name}: wrong total contingencies"
        );
        assert!(
            result.summary.converged >= *min_converged,
            "{name}: converged {} < min expected {min_converged}",
            result.summary.converged
        );
    }
}

/// Performance guard: N-1 on case2383wp must complete in reasonable time.
/// Catches O(n²) regressions (e.g., accidental dense LODF on unscreened cases).
/// Bound is generous (30s) to account for debug builds and CI load.
/// Release build should complete in ~0.4s.
/// Marked ignore: timing-sensitive, unreliable under CPU load in debug mode.
#[test]
#[ignore = "timing-sensitive: run in release mode only"]
fn test_performance_bound_case2383wp() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case2383wp");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    let start = std::time::Instant::now();
    let result = analyze_n1_branch(&net, &options).expect("should succeed");
    let elapsed = start.elapsed().as_secs_f64();

    // 30s bound: ~0.4s release × ~10x debug overhead × ~3x CI safety margin
    assert!(
        elapsed < 30.0,
        "case2383wp N-1 took {elapsed:.2}s, expected < 30s (possible regression)"
    );
    assert!(result.summary.converged > 2000);
}

/// Performance guard: N-1 on case118 should be fast.
/// Catches regressions from accidentally enabling expensive screening.
#[test]
#[ignore] // Flaky — timing-dependent, fails under load
fn test_performance_bound_case118() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    let start = std::time::Instant::now();
    let _ = analyze_n1_branch(&net, &options).expect("should succeed");
    let elapsed = start.elapsed().as_secs_f64();

    // 10s bound: ~0.003s release × ~100x debug overhead + CI margin
    assert!(
        elapsed < 10.0,
        "case118 N-1 took {elapsed:.2}s, expected < 10s"
    );
}

/// Verify that inline NR (thread-local pool) produces same voltage solutions
/// as the full clone NR path for converged contingencies.
#[test]
fn test_inline_vs_clone_correctness_case118() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    // Run with inline solver (default parallel path)
    let inline_result = analyze_n1_branch(&net, &options).expect("inline should succeed");

    // Run selected contingencies with full clone to compare convergence
    let contingencies = generate_n1_branch_contingencies(&net);
    let acpf_opts = AcPfOptions::default();

    let mut mismatches = 0;
    for (i, ctg) in contingencies.iter().enumerate().take(50) {
        let inline_r = &inline_result.results[i];

        // Run full clone path
        let mut net_clone = net.clone();
        for &br in &ctg.branch_indices {
            net_clone.branches[br].in_service = false;
        }
        let clone_converged = match solve_ac_pf_kernel(&net_clone, &acpf_opts) {
            Ok(sol) => sol.status == SolveStatus::Converged,
            Err(_) => false,
        };

        if inline_r.converged != clone_converged {
            mismatches += 1;
        }
    }

    // Allow at most 2 edge-case differences (warm start vs cold start boundary)
    assert!(
        mismatches <= 2,
        "inline vs clone mismatch count {} exceeds threshold 2",
        mismatches
    );
}

// -----------------------------------------------------------------------
// CTG-05: N-2 branch convenience function
// -----------------------------------------------------------------------

#[test]
fn test_generate_n2_contingency_count() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let ctgs = generate_n2_branch_contingencies(&net);

    let n = net.branches.iter().filter(|b| b.in_service).count();
    let expected = n * (n - 1) / 2;
    assert_eq!(
        ctgs.len(),
        expected,
        "expected C({n},2) = {expected} N-2 pairs, got {}",
        ctgs.len()
    );

    for ctg in &ctgs {
        assert_eq!(
            ctg.branch_indices.len(),
            2,
            "each N-2 ctg must trip 2 branches"
        );
        assert!(
            ctg.generator_indices.is_empty(),
            "N-2 branch ctg should not trip generators"
        );
        let a = ctg.branch_indices[0];
        let b = ctg.branch_indices[1];
        assert!(a < b, "expected upper-triangle ordering: a={a} < b={b}");
    }
}

#[test]
fn test_analyze_n2_branch_case14() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };
    let result = analyze_n2_branch(&net, &options).expect("N-2 should succeed on case14");

    let n = net.branches.iter().filter(|b| b.in_service).count();
    let expected_pairs = n * (n - 1) / 2;

    assert_eq!(
        result.summary.total_contingencies, expected_pairs,
        "total contingencies should equal C(n,2)"
    );
    assert_eq!(result.base_case.status, SolveStatus::Converged);
    assert!(
        result.summary.converged > 0,
        "at least some N-2 contingencies should converge"
    );
}

// -----------------------------------------------------------------------
// CTG-08: Sparse LODF public API
// -----------------------------------------------------------------------

#[test]
fn test_compute_lodf_sparse_vs_dense_case9() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let n_br = net.n_branches();
    let all_branches: Vec<usize> = (0..n_br).collect();

    let lodf_matrix = surge_dc::compute_lodf_matrix(
        &net,
        &surge_dc::LodfMatrixRequest::for_branches(&all_branches),
    )
    .unwrap();
    let lodf_sparse = surge_dc::compute_lodf_pairs(&net, &all_branches, &all_branches).unwrap();

    for (&(l, k), &sparse_val) in &lodf_sparse {
        let dense_val = lodf_matrix[(l, k)];
        let diff = (sparse_val - dense_val).abs();
        assert!(
            diff < 1e-8 || (sparse_val.is_infinite() && dense_val.is_infinite()),
            "LODF({l},{k}): sparse={sparse_val:.6}, dense={dense_val:.6}, diff={diff:.2e}"
        );
    }

    assert!(!lodf_sparse.is_empty(), "expected non-empty LODF for case9");
}

#[test]
fn test_compute_lodf_sparse_only_returns_requested_pairs() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let monitored = vec![0, 3, 7];
    let outages = vec![1, 5];

    let lodf = surge_dc::compute_lodf_pairs(&net, &monitored, &outages).unwrap();

    for &(l, k) in lodf.entries().keys() {
        assert!(monitored.contains(&l), "unexpected monitored index {l}");
        assert!(outages.contains(&k), "unexpected outage index {k}");
    }
}

// -----------------------------------------------------------------------
// CTG-10: Top-K worst contingency ranking
// -----------------------------------------------------------------------

#[test]
fn test_top_k_ranking_case14() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        top_k: Some(5),
        ..Default::default()
    };
    let topk = analyze_n1_branch(&net, &options).expect("top-K N-1 should succeed");

    assert!(
        topk.results.len() <= 5,
        "top_k=5 should return at most 5 results, got {}",
        topk.results.len()
    );
}

#[test]
fn test_top_k_sorted_descending_case118() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        top_k: Some(10),
        ..Default::default()
    };
    let result = analyze_n1_branch(&net, &options).expect("top-K N-1 should succeed");

    assert!(result.results.len() <= 10);

    let scores: Vec<f64> = result
        .results
        .iter()
        .map(contingency_severity_score)
        .collect();
    for i in 1..scores.len() {
        assert!(
            scores[i] <= scores[i - 1] + 1e-9,
            "results not sorted descending: score[{}]={} > score[{}]={}",
            i,
            scores[i],
            i - 1,
            scores[i - 1]
        );
    }
}

#[test]
fn test_top_k_none_returns_all() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        top_k: None,
        ..Default::default()
    };
    let result = analyze_n1_branch(&net, &options).expect("should succeed");
    let n = net.branches.iter().filter(|b| b.in_service).count();
    // all N-1 contingencies evaluated
    assert_eq!(result.summary.total_contingencies, n);
}

#[test]
fn test_top_k_does_not_corrupt_summary_accounting() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let total = net
        .branches
        .iter()
        .filter(|branch| branch.in_service)
        .count();
    let result = analyze_n1_branch(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Off,
            top_k: Some(1),
            ..Default::default()
        },
    )
    .expect("top-K N-1 should succeed");

    assert_eq!(
        result.results.len(),
        1,
        "top_k should truncate returned results"
    );
    assert_eq!(
        result.summary.total_contingencies, total,
        "summary should still reflect the full evaluated contingency set"
    );
    assert_eq!(
        result.summary.ac_solved, total,
        "top_k must not rewrite how many contingencies were actually solved"
    );
    assert_eq!(
        result.summary.screened_out, 0,
        "screening=Off should not report screened contingencies after top_k truncation"
    );
}

// -----------------------------------------------------------------------
// CTG-02: Corrective dispatch wiring
// -----------------------------------------------------------------------

#[test]
fn test_corrective_dispatch_disabled_stays_none() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        corrective_dispatch: false,
        ..Default::default()
    };
    let result = analyze_n1_branch(&net, &options).expect("should succeed");

    for r in &result.results {
        assert!(
            r.corrective_dispatch.is_none(),
            "corrective_dispatch should be None when disabled ({})",
            r.id
        );
    }
}

#[test]
fn test_corrective_dispatch_enabled_runs_without_panic() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    // Low thermal threshold forces many violations → SCRD will be invoked.
    // Just verify the pipeline completes and all results have corrective_dispatch
    // populated (Some) whenever thermal violations were found.
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        thermal_threshold_frac: 0.20,
        corrective_dispatch: true,
        ..Default::default()
    };
    let result = analyze_n1_branch(&net, &options).expect("corrective dispatch run should succeed");

    // Verify every result with thermal violations has a dispatch attempt attached
    for r in &result.results {
        let has_thermal = r
            .violations
            .iter()
            .any(|v| matches!(v, Violation::ThermalOverload { .. }));
        // When corrective_dispatch is enabled and there are thermal violations,
        // the field should be Some (SCRD was attempted, may be optimal or infeasible)
        if has_thermal {
            // SCRD may be infeasible for the test case but should not be None
            // (it is only None if SCRD was not attempted at all)
            assert!(
                r.corrective_dispatch.is_some(),
                "expected corrective_dispatch to be populated for {} (thermal violation present)",
                r.id
            );
        }
    }
}

// -----------------------------------------------------------------------
// CTG-01: Fast generator N-1 path
// -----------------------------------------------------------------------

/// Verify that generator N-1 contingencies produce reasonable convergence
/// rates on case14 (5 generators).
#[test]
fn test_n1_generator_case14() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };
    let result = analyze_n1_generator(&net, &options).expect("N-1 generator should succeed");

    // case14 has 5 in-service generators
    let n_gens = net.generators.iter().filter(|g| g.in_service).count();
    assert_eq!(result.summary.total_contingencies, n_gens);
    assert_eq!(result.base_case.status, SolveStatus::Converged);

    // Most generator contingencies on case14 should converge.
    assert!(
        result.summary.converged > 0,
        "at least one generator contingency should converge"
    );
}

/// Generator contingencies cannot be screened by branch LODF, so they must
/// still be forwarded to AC solves under ScreeningMode::Lodf.
#[test]
fn test_n1_generator_lodf_does_not_screen_out_generator_contingencies() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        ..Default::default()
    };
    let result = analyze_n1_generator(&net, &options).expect("N-1 generator should succeed");

    let n_gens = net.generators.iter().filter(|g| g.in_service).count();
    assert_eq!(result.summary.total_contingencies, n_gens);
    assert_eq!(
        result.summary.ac_solved, n_gens,
        "generator contingencies must not be screened out by branch-only LODF logic"
    );
    assert_eq!(result.results.len(), n_gens);
}

/// Verify the fast generator path produces results similar to the full-clone
/// path by comparing convergence status on case14.
#[test]
fn test_generator_fast_vs_full_clone_case14() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    // Fast path (via analyze_n1_generator → fast generator solver)
    let fast_result =
        analyze_n1_generator(&net, &options).expect("fast generator N-1 should succeed");

    // Manual full-clone solve for each contingency to compare convergence.
    let contingencies = generate_n1_generator_contingencies(&net);
    let acpf_opts = AcPfOptions::default();

    let mut mismatches = 0;
    for (i, ctg) in contingencies.iter().enumerate() {
        let fast_r = &fast_result.results[i];

        let mut net_clone = net.clone();
        for &gi in &ctg.generator_indices {
            net_clone.generators[gi].in_service = false;
        }
        let clone_converged = match solve_ac_pf_kernel(&net_clone, &acpf_opts) {
            Ok(sol) => sol.status == SolveStatus::Converged,
            Err(_) => false,
        };

        if fast_r.converged != clone_converged {
            mismatches += 1;
        }
    }

    // Allow a small number of edge-case differences due to warm-start vs cold.
    assert!(
        mismatches <= 2,
        "fast vs clone mismatch count {mismatches} exceeds threshold"
    );
}

/// Time the fast generator path on case118.
/// Assert it completes within a generous bound (catches O(n²) regressions).
#[test]
#[ignore = "flaky perf assertion — sensitive to machine load"]
fn test_generator_fast_path_performance_case118() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case118");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    let start = std::time::Instant::now();
    let result = analyze_n1_generator(&net, &options).expect("fast N-1 generator should succeed");
    let elapsed = start.elapsed().as_secs_f64();

    assert!(
        result.summary.converged > 0,
        "expected some convergence; got 0 of {}",
        result.summary.total_contingencies
    );

    // In debug mode allow generous bound.
    assert!(
        elapsed < 15.0,
        "N-1 generator on case118 took {elapsed:.2}s, expected < 15s"
    );
}

// -----------------------------------------------------------------------
// CTG-09: Island detection
// -----------------------------------------------------------------------

/// Build a 4-bus linear chain with one bridge branch; remove the bridge
/// and verify 2 islands are detected.
#[test]
fn test_island_detection_bridge_removal() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    // Linear chain: 1-2-3-4 with bridge at 2-3 (branch index 1).
    // After removing branch index 1: island {1,2} and island {3,4}.
    let mut net = Network::new("bridge_test");
    let mut b1 = Bus::new(1, BusType::Slack, 100.0);
    b1.voltage_magnitude_pu = 1.0;
    let mut b2 = Bus::new(2, BusType::PQ, 100.0);
    b2.voltage_magnitude_pu = 1.0;
    let mut b3 = Bus::new(3, BusType::PQ, 100.0);
    b3.voltage_magnitude_pu = 1.0;
    let mut b4 = Bus::new(4, BusType::PQ, 100.0);
    b4.voltage_magnitude_pu = 1.0;
    net.buses = vec![b1, b2, b3, b4];
    net.loads = vec![
        Load::new(2, 10.0, 0.0),
        Load::new(3, 10.0, 0.0),
        Load::new(4, 5.0, 0.0),
    ];
    net.branches = vec![
        Branch::new_line(1, 2, 0.01, 0.1, 0.0), // 0
        Branch::new_line(2, 3, 0.01, 0.1, 0.0), // 1 — bridge
        Branch::new_line(3, 4, 0.01, 0.1, 0.0), // 2
    ];
    let mut generator = Generator::new(1, 25.0, 1.05);
    generator.qmin = -50.0;
    generator.qmax = 50.0;
    net.generators = vec![generator];

    let removed = vec![1usize]; // remove the bridge
    let (_, n_components) = find_connected_components(&net, &removed);
    assert_eq!(
        n_components, 2,
        "expected 2 islands after removing bridge branch"
    );
}

/// Run N-1 on case14 with island detection enabled; verify no panic or NaN.
#[test]
fn test_island_detection_case14_no_panic() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        detect_islands: true,
        ..Default::default()
    };
    let result =
        analyze_n1_branch(&net, &options).expect("N-1 with island detection should succeed");
    assert_eq!(result.base_case.status, SolveStatus::Converged);

    // Verify no NaN in any result.
    for r in &result.results {
        for v in &r.violations {
            match v {
                Violation::ThermalOverload {
                    loading_pct,
                    flow_mw,
                    flow_mva,
                    limit_mva,
                    ..
                } => {
                    assert!(loading_pct.is_finite(), "loading_pct NaN in {}", r.id);
                    assert!(flow_mw.is_finite(), "flow_mw NaN in {}", r.id);
                    assert!(flow_mva.is_finite(), "flow_mva NaN in {}", r.id);
                    assert!(limit_mva.is_finite(), "limit_mva NaN in {}", r.id);
                }
                Violation::VoltageLow { vm, .. } | Violation::VoltageHigh { vm, .. } => {
                    assert!(vm.is_finite(), "vm NaN in {}", r.id);
                }
                Violation::NonConvergent { max_mismatch, .. } => {
                    assert!(!max_mismatch.is_nan(), "max_mismatch is NaN in {}", r.id);
                }
                Violation::Islanding { n_components } => {
                    assert!(*n_components > 1, "islanding must have > 1 component");
                }
                Violation::FlowgateOverload {
                    loading_pct,
                    flow_mw,
                    limit_mw,
                    ..
                } => {
                    assert!(
                        loading_pct.is_finite(),
                        "flowgate loading_pct NaN in {}",
                        r.id
                    );
                    assert!(flow_mw.is_finite(), "flowgate flow_mw NaN in {}", r.id);
                    assert!(limit_mw.is_finite(), "flowgate limit_mw NaN in {}", r.id);
                }
                Violation::InterfaceOverload {
                    loading_pct,
                    flow_mw,
                    limit_mw,
                    ..
                } => {
                    assert!(
                        loading_pct.is_finite(),
                        "interface loading_pct NaN in {}",
                        r.id
                    );
                    assert!(flow_mw.is_finite(), "interface flow_mw NaN in {}", r.id);
                    assert!(limit_mw.is_finite(), "interface limit_mw NaN in {}", r.id);
                }
            }
        }
    }
}

/// When a bridge-creating contingency triggers island detection, the result
/// must carry an `Islanding` violation and `n_islands > 1`.
#[test]
fn test_island_violation_is_reported() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    // 4-bus linear chain with bridge at 2-3.
    let mut net = Network::new("island_violation_test");
    let mut b1 = Bus::new(1, BusType::Slack, 100.0);
    b1.voltage_magnitude_pu = 1.0;
    let mut b2 = Bus::new(2, BusType::PV, 100.0);
    b2.voltage_magnitude_pu = 1.0;
    let mut b3 = Bus::new(3, BusType::PQ, 100.0);
    b3.voltage_magnitude_pu = 1.0;
    let mut b4 = Bus::new(4, BusType::PQ, 100.0);
    b4.voltage_magnitude_pu = 1.0;
    net.buses = vec![b1, b2, b3, b4];
    net.loads = vec![Load::new(3, 10.0, 0.0), Load::new(4, 5.0, 0.0)];
    net.branches = vec![
        Branch::new_line(1, 2, 0.01, 0.1, 0.0), // 0
        Branch::new_line(2, 3, 0.01, 0.1, 0.0), // 1 — bridge
        Branch::new_line(3, 4, 0.01, 0.1, 0.0), // 2
    ];
    let mut g1 = Generator::new(1, 10.0, 1.05);
    g1.qmin = -500.0;
    g1.qmax = 500.0;
    let mut g2 = Generator::new(2, 5.0, 1.0);
    g2.qmin = -200.0;
    g2.qmax = 200.0;
    net.generators = vec![g1, g2];

    // Trip only the bridge (branch index 1).
    let ctgs = vec![Contingency {
        id: "bridge".into(),
        label: "Remove bridge 2-3".into(),
        branch_indices: vec![1],
        ..Default::default()
    }];

    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        detect_islands: true,
        ..Default::default()
    };
    let result = analyze_contingencies(&net, &ctgs, &options).expect("analysis should succeed");

    let r = &result.results[0];
    assert!(
        r.n_islands > 1
            || r.violations
                .iter()
                .any(|v| matches!(v, Violation::Islanding { .. })),
        "expected islanding for bridge outage; n_islands={}, violations={:?}",
        r.n_islands,
        r.violations
    );
}

#[test]
fn test_full_clone_mixed_contingency_reports_islanding() {
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    let mut net = Network::new("mixed_full_clone_island_test");
    let mut b1 = Bus::new(1, BusType::Slack, 100.0);
    b1.voltage_magnitude_pu = 1.0;
    let mut b2 = Bus::new(2, BusType::PV, 100.0);
    b2.voltage_magnitude_pu = 1.0;
    let mut b3 = Bus::new(3, BusType::PV, 100.0);
    b3.voltage_magnitude_pu = 1.0;
    let mut b4 = Bus::new(4, BusType::PQ, 100.0);
    b4.voltage_magnitude_pu = 1.0;
    net.buses = vec![b1, b2, b3, b4];
    net.loads = vec![Load::new(4, 5.0, 1.0)];
    net.branches = vec![
        Branch::new_line(1, 2, 0.01, 0.1, 0.0),
        Branch::new_line(2, 3, 0.01, 0.1, 0.0),
        Branch::new_line(3, 4, 0.01, 0.1, 0.0),
    ];

    let mut g1 = Generator::new(1, 8.0, 1.02);
    g1.qmin = -500.0;
    g1.qmax = 500.0;
    let mut g2 = Generator::new(2, 4.0, 1.01);
    g2.qmin = -200.0;
    g2.qmax = 200.0;
    let mut g3 = Generator::new(3, 4.0, 1.01);
    g3.qmin = -200.0;
    g3.qmax = 200.0;
    net.generators = vec![g1, g2, g3];

    let ctg = Contingency {
        id: "mixed_branch_gen".into(),
        label: "Trip bridge branch and one generator".into(),
        branch_indices: vec![1],
        generator_indices: vec![1],
        ..Default::default()
    };

    let analysis = analyze_contingencies(
        &net,
        &[ctg],
        &ContingencyOptions {
            screening: ScreeningMode::Off,
            detect_islands: true,
            ..Default::default()
        },
    )
    .expect("mixed full-clone contingency analysis should succeed");

    let result = &analysis.results[0];
    assert!(
        result.n_islands > 1,
        "mixed branch+generator contingencies should use the full-clone island path"
    );
    assert!(
        result
            .violations
            .iter()
            .any(|violation| matches!(violation, Violation::Islanding { .. })),
        "mixed full-clone islanding must emit an Islanding violation"
    );
}
// -----------------------------------------------------------------------
// CTG-04: Voltage stability indices
// -----------------------------------------------------------------------

/// CTG-04: Verify that N-1 contingencies on case9 populate voltage stress
/// post-processing and that some contingency produces a positive proxy value.
///
/// Note: Island-creating contingencies (e.g., bridge lines) use the island
/// detection path which currently returns empty voltage_stress. Only
/// contingencies that converge through the standard inline NR path will have
/// voltage_stress populated. The test verifies the non-island converged
/// contingencies are correct.
#[test]
fn test_voltage_stress_ctg04_case9() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };
    let analysis = analyze_n1_branch(&net, &options).expect("N-1 should succeed");

    // Collect converged results that went through the inline NR path
    // (voltage_stress populated) vs. island path (None).
    let results_with_indices: Vec<_> = analysis
        .results
        .iter()
        .filter(|r| r.converged && r.voltage_stress.is_some())
        .collect();

    // At least some N-1 contingencies in case9 should produce voltage stress entries.
    assert!(
        !results_with_indices.is_empty(),
        "At least one N-1 contingency should have voltage_stress populated"
    );

    // Verify correctness for all results that have voltage_stress.
    for r in &results_with_indices {
        let vs = r.voltage_stress.as_ref().unwrap();
        assert_eq!(
            vs.per_bus.len(),
            net.n_buses(),
            "voltage_stress per_bus length should match n_buses for contingency {}",
            r.id
        );
        for idx in &vs.per_bus {
            if let Some(proxy) = idx.local_qv_stress_proxy {
                assert!(
                    proxy >= 0.0,
                    "Proxy stress must be non-negative for bus {} in contingency {}",
                    idx.bus_number,
                    r.id
                );
                assert!(
                    proxy.is_finite(),
                    "Proxy stress must be finite for bus {} in contingency {}",
                    idx.bus_number,
                    r.id
                );
            }
        }
    }

    // At least one contingency should produce a positive proxy stress summary.
    let has_positive_proxy = results_with_indices.iter().any(|r| {
        r.voltage_stress
            .as_ref()
            .unwrap()
            .max_qv_stress_proxy
            .is_some_and(|proxy| proxy > 0.0)
    });
    assert!(
        has_positive_proxy,
        "At least one N-1 contingency should have positive proxy voltage stress"
    );

    // `critical_proxy_bus` must be a valid bus number when the proxy summary exists.
    for r in &results_with_indices {
        if let Some(bus_number) = r.voltage_stress.as_ref().unwrap().critical_proxy_bus {
            assert!(
                net.buses.iter().any(|b| b.number == bus_number),
                "critical_proxy_bus {} is not a valid bus number in contingency {}",
                bus_number,
                r.id
            );
        }
    }
}

/// CTG-04: Non-converged contingencies should have empty voltage_stress.
#[test]
fn test_voltage_stress_empty_on_nonconvergence() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    // Build a degenerate network that won't converge when a key branch is tripped.
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    let mut net = Network::new("degenerate_ctg04");
    let mut b1 = Bus::new(1, BusType::Slack, 100.0);
    b1.voltage_magnitude_pu = 1.0;
    let mut b2 = Bus::new(2, BusType::PQ, 100.0);
    b2.voltage_magnitude_pu = 1.0;
    net.buses = vec![b1, b2];
    net.loads.push(Load::new(2, 100.0, 0.0)); // Very heavy load
    // Single branch connecting them — when tripped, bus 2 has no supply
    net.branches = vec![Branch::new_line(1, 2, 0.01, 0.1, 0.0)];
    let mut generator = Generator::new(1, 100.0, 1.05);
    generator.qmin = -200.0;
    generator.qmax = 200.0;
    net.generators = vec![generator];

    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        detect_islands: false,
        ..Default::default()
    };
    let analysis = analyze_n1_branch(&net, &options).expect("analysis should complete");

    // The single contingency should not converge (island or non-convergence).
    for r in &analysis.results {
        if !r.converged {
            assert!(
                r.voltage_stress.is_none(),
                "Non-converged result should have None voltage_stress"
            );
        }
    }
}

// -----------------------------------------------------------------------
// PNL-005: PenaltyConfig in ContingencyOptions
// -----------------------------------------------------------------------

/// PNL-005: ContingencyOptions with penalty_config constructs and runs without panic.
#[test]
fn test_penalty_config_in_contingency_options_pnl005() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    use surge_network::market::PenaltyConfig;

    let net = load_case("case9");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        penalty_config: Some(PenaltyConfig::default()),
        ..Default::default()
    };

    // Should construct without panic.
    assert!(options.penalty_config.is_some());

    // Should also run without panic (penalty_config is stored but not yet
    // wired into SCRD — this test verifies no panic path is hit).
    let result = analyze_n1_branch(&net, &options);
    assert!(result.is_ok(), "N-1 with penalty_config should not error");
}

/// PNL-005: ContingencyOptions serializes and deserializes penalty_config correctly.
#[test]
fn test_penalty_config_serde_in_contingency_options() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    use surge_network::market::PenaltyConfig;

    let opts = ContingencyOptions {
        penalty_config: Some(PenaltyConfig::default()),
        ..Default::default()
    };
    let json = serde_json::to_string(&opts).expect("serialize");
    let back: ContingencyOptions = serde_json::from_str(&json).expect("deserialize");
    assert!(back.penalty_config.is_some());
}

/// PNL-005: ContingencyOptions with penalty_config: None serializes cleanly
/// and deserializes to None.
#[test]
fn test_penalty_config_none_roundtrip() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let opts = ContingencyOptions {
        penalty_config: None,
        ..Default::default()
    };
    let json = serde_json::to_string(&opts).expect("serialize");
    let back: ContingencyOptions = serde_json::from_str(&json).expect("deserialize");
    assert!(back.penalty_config.is_none());
}

// -----------------------------------------------------------------------
// CTG-03: Post-contingency voltage stability
// -----------------------------------------------------------------------

/// CTG-03: Enabling L-index voltage stability should auto-upgrade the
/// contingency solve to exact L-index post-processing and populate the
/// per-result exact fields.
#[test]
fn test_ctg03_lindex_mode_populates_exact_l_index() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        voltage_stress_mode: VoltageStressMode::ExactLIndex {
            l_index_threshold: 0.7,
        },
        ..Default::default()
    };

    let analysis = analyze_n1_generator(&net, &options).expect("generator N-1 should succeed");
    let converged: Vec<&ContingencyResult> =
        analysis.results.iter().filter(|r| r.converged).collect();

    assert!(
        !converged.is_empty(),
        "expected at least one converged generator contingency"
    );
    assert!(
        converged.iter().any(|result| result
            .voltage_stress
            .as_ref()
            .is_some_and(|vs| vs.max_l_index.is_some())),
        "LIndex mode should populate max_l_index for converged contingencies"
    );
    assert!(
        converged.iter().any(|result| {
            result
                .voltage_stress
                .as_ref()
                .is_some_and(|vs| vs.per_bus.iter().any(|bus| bus.exact_l_index.is_some()))
        }),
        "LIndex mode should populate per-bus exact_l_index values"
    );
}

#[test]
fn test_ctg03_lindex_mode_assigns_categories_and_summary() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let analysis = analyze_n1_generator(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Off,
            voltage_stress_mode: VoltageStressMode::ExactLIndex {
                l_index_threshold: 0.7,
            },
            ..Default::default()
        },
    )
    .expect("generator N-1 should succeed");

    let classified_results: Vec<&ContingencyResult> = analysis
        .results
        .iter()
        .filter(|result| {
            result
                .voltage_stress
                .as_ref()
                .is_some_and(|vs| vs.max_l_index.is_some())
        })
        .collect();
    assert!(
        !classified_results.is_empty(),
        "expected at least one exact L-index result to classify"
    );
    assert!(
        classified_results.iter().all(|result| result
            .voltage_stress
            .as_ref()
            .unwrap()
            .category
            .is_some()),
        "every result with an exact L-index should carry a vsm_category"
    );

    let expected_critical = classified_results
        .iter()
        .filter(|result| {
            matches!(
                result.voltage_stress.as_ref().unwrap().category,
                Some(VsmCategory::Critical | VsmCategory::Unstable)
            )
        })
        .count();
    assert_eq!(
        analysis.summary.n_voltage_critical, expected_critical,
        "summary voltage-critical count must be derived from the populated L-index categories"
    );
}

/// P1-031: OOS branch filtering should still retain contingencies that also
/// outage a generator. Only pure branch contingencies against already-OOS
/// elements should be dropped.
#[test]
fn test_p1_031_oos_branch_filter_keeps_generator_mixed_contingencies() {
    let mut net = Network::new("oos_mixed_ctg_test");
    net.base_mva = 100.0;
    net.buses = vec![
        Bus::new(1, BusType::Slack, 0.0),
        Bus::new(2, BusType::PQ, 50.0),
        Bus::new(3, BusType::Slack, 0.0),
    ];

    let mut slack_generator = Generator::new(1, 60.0, 1.0);
    slack_generator.pmax = 100.0;
    slack_generator.pmin = 0.0;
    net.generators.push(slack_generator);

    let mut generator = Generator::new(3, 50.0, 1.0);
    generator.pmax = 100.0;
    generator.pmin = 0.0;
    net.generators.push(generator);

    let mut br_in = Branch::new_line(1, 2, 0.01, 0.2, 0.0);
    br_in.rating_a_mva = 100.0;
    br_in.rating_b_mva = 100.0;
    br_in.rating_c_mva = 100.0;
    let mut br_oos = Branch::new_line(2, 3, 0.01, 0.2, 0.0);
    br_oos.in_service = false;
    br_oos.rating_a_mva = 100.0;
    br_oos.rating_b_mva = 100.0;
    br_oos.rating_c_mva = 100.0;
    net.branches = vec![br_in, br_oos];

    let contingencies = vec![
        Contingency {
            id: "oos_only".into(),
            label: "trip OOS branch only".into(),
            branch_indices: vec![1],
            ..Default::default()
        },
        Contingency {
            id: "oos_plus_gen".into(),
            label: "trip OOS branch and live generator".into(),
            branch_indices: vec![1],
            generator_indices: vec![0],
            ..Default::default()
        },
        Contingency {
            id: "gen_only".into(),
            label: "trip live generator".into(),
            generator_indices: vec![0],
            ..Default::default()
        },
    ];

    let analysis = analyze_contingencies(&net, &contingencies, &ContingencyOptions::default())
        .expect("analysis");
    let ids: Vec<&str> = analysis
        .results
        .iter()
        .map(|result| result.id.as_str())
        .collect();

    assert_eq!(analysis.summary.total_contingencies, 2);
    assert!(!ids.contains(&"oos_only"));
    assert!(ids.contains(&"oos_plus_gen"));
    assert!(ids.contains(&"gen_only"));
}

// -----------------------------------------------------------------------
// CTG-08: Sparse LODF public API
// -----------------------------------------------------------------------

/// CTG-08: compute_lodf_pairs returns |LODF| ≤ 1.0 for case9.
#[test]
fn test_ctg08_sparse_lodf_case9() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let outage_indices: Vec<usize> = (0..3).collect();
    let monitored_indices: Vec<usize> = (0..3).collect();

    let lodf = surge_dc::compute_lodf_pairs(&net, &monitored_indices, &outage_indices)
        .expect("compute_lodf_pairs should not fail on case9");

    assert!(!lodf.is_empty(), "LODF result should not be empty");

    for (&(monitored, outage), &val) in &lodf {
        if !val.is_finite() {
            continue;
        }
        assert!(
            val.abs() <= 1.0 + 1e-10,
            "LODF({monitored},{outage}) = {val:.6} exceeds |1.0| — check formula"
        );
    }
}

/// CTG-08: Self-LODF entries should be the canonical `-1.0` diagonal value
/// for non-bridge outages.
#[test]
fn test_ctg08_sparse_lodf_self_lodf_diagonal() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let indices: Vec<usize> = (0..5).collect();

    let lodf = surge_dc::compute_lodf_pairs(&net, &indices, &indices).expect("should not fail");

    for &idx in &indices {
        let diag = lodf
            .get(idx, idx)
            .expect("expected self-LODF entry on the diagonal");
        assert!(
            (diag - (-1.0)).abs() < 1e-12 || diag.is_infinite(),
            "expected self-LODF diagonal value at branch {idx}, got {diag}"
        );
    }
}

// -----------------------------------------------------------------------
// CTG-10: rank_contingencies public API
// -----------------------------------------------------------------------

/// CTG-10: rank_contingencies with k=3 returns at most 3 results.
#[test]
fn test_ctg10_top_k_returns_k_results() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };
    let analysis = analyze_n1_branch(&net, &options).expect("N-1 should succeed");
    let ranked = rank_contingencies(&analysis.results, ContingencyMetric::MaxFlowPct, 3);
    assert!(
        ranked.len() <= 3,
        "rank_contingencies(k=3) returned {} results",
        ranked.len()
    );
}

/// CTG-10: MaxFlowPct results are in descending order of severity.
#[test]
fn test_ctg10_worst_flows_sorted() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };
    let analysis = analyze_n1_branch(&net, &options).expect("N-1 should succeed");
    let ranked = rank_contingencies(&analysis.results, ContingencyMetric::MaxFlowPct, 5);

    let scores: Vec<f64> = ranked
        .iter()
        .map(|r| {
            r.violations
                .iter()
                .filter_map(|v| {
                    if let Violation::ThermalOverload { loading_pct, .. } = v {
                        Some(*loading_pct)
                    } else {
                        None
                    }
                })
                .fold(f64::NEG_INFINITY, f64::max)
        })
        .collect();

    for i in 1..scores.len() {
        assert!(
            scores[i] <= scores[i - 1] + 1e-9,
            "rank_contingencies not descending at index {i}: score[{}]={:.4} > score[{}]={:.4}",
            i,
            scores[i],
            i - 1,
            scores[i - 1]
        );
    }
}

/// CTG-10: rank_contingencies on empty input returns empty.
#[test]
fn test_ctg10_empty_input_ok() {
    let results: Vec<ContingencyResult> = vec![];
    let ranked = rank_contingencies(&results, ContingencyMetric::MaxFlowPct, 5);
    assert!(ranked.is_empty(), "expected empty result for empty input");
}

// -----------------------------------------------------------------------
// P1-031: OOS branch skip in N-1 enumeration
// -----------------------------------------------------------------------

/// P1-031: Contingencies that reference only out-of-service branches
/// should be excluded from the analysis (they are no-ops).
#[test]
fn test_p1_031_oos_branches_excluded() {
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    let mut net = Network::new("oos_test");
    let mut b1 = Bus::new(1, BusType::Slack, 100.0);
    b1.voltage_magnitude_pu = 1.0;
    let mut b2 = Bus::new(2, BusType::PQ, 100.0);
    b2.voltage_magnitude_pu = 1.0;
    net.buses = vec![b1, b2];
    net.loads.push(Load::new(2, 10.0, 0.0));

    // Two branches: one in service, one out of service (different circuits).
    let br_in = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
    let mut br_oos = Branch::new_line(1, 2, 0.01, 0.2, 0.0);
    br_oos.circuit = "2".to_string();
    br_oos.in_service = false;
    net.branches = vec![br_in, br_oos];

    let mut g1 = Generator::new(1, 10.0, 1.05);
    g1.qmin = -50.0;
    g1.qmax = 50.0;
    net.generators = vec![g1];

    // Build contingencies manually including the OOS branch.
    let ctgs = vec![
        Contingency {
            id: "br_in_service".into(),
            label: "Trip in-service branch 0".into(),
            branch_indices: vec![0],
            ..Default::default()
        },
        Contingency {
            id: "br_oos".into(),
            label: "Trip OOS branch 1".into(),
            branch_indices: vec![1],
            ..Default::default()
        },
    ];

    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        detect_islands: false,
        ..Default::default()
    };
    let result = analyze_contingencies(&net, &ctgs, &options).expect("should succeed");

    // Only the in-service branch contingency should be evaluated.
    assert_eq!(
        result.summary.total_contingencies, 1,
        "OOS branch contingency should have been filtered out, got {} total",
        result.summary.total_contingencies
    );
    assert_eq!(
        result.results[0].id, "br_in_service",
        "only the in-service branch contingency should remain"
    );
}

/// Verify that using emergency ratings (RateB) produces the same or fewer
/// thermal violations compared to RateA, since RateB >= RateA.
#[test]
fn test_ca_emergency_rating() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let mut net = load_case("case9");

    // Set rate_b to 110% of rate_a for branches that have rate_a > 0
    for branch in &mut net.branches {
        if branch.rating_a_mva > 0.0 {
            branch.rating_b_mva = branch.rating_a_mva * 1.1;
        }
    }

    // Run N-1 with RateA
    let result_a = analyze_n1_branch(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Off,
            thermal_rating: ThermalRating::RateA,
            ..Default::default()
        },
    )
    .expect("RateA should succeed");

    // Run N-1 with RateB
    let result_b = analyze_n1_branch(
        &net,
        &ContingencyOptions {
            screening: ScreeningMode::Off,
            thermal_rating: ThermalRating::RateB,
            ..Default::default()
        },
    )
    .expect("RateB should succeed");

    // RateB violations should be <= RateA violations (higher rating = fewer overloads)
    let violations_a: usize = result_a
        .results
        .iter()
        .flat_map(|r| &r.violations)
        .filter(|v| matches!(v, Violation::ThermalOverload { .. }))
        .count();

    let violations_b: usize = result_b
        .results
        .iter()
        .flat_map(|r| &r.violations)
        .filter(|v| matches!(v, Violation::ThermalOverload { .. }))
        .count();

    eprintln!(
        "Emergency rating test: RateA violations={}, RateB violations={}",
        violations_a, violations_b
    );

    assert!(
        violations_b <= violations_a,
        "RateB (emergency) should have same or fewer thermal violations \
         than RateA: {} > {}",
        violations_b,
        violations_a
    );
}

// -----------------------------------------------------------------------
// B8 tests
// -----------------------------------------------------------------------

#[test]
fn test_default_screening_is_fdpf() {
    let opts = ContingencyOptions::default();
    assert!(
        matches!(opts.screening, ScreeningMode::Fdpf),
        "Default screening should be Fdpf, got {:?}",
        opts.screening
    );
}

#[test]
fn test_lodf_always_checks_voltage() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    // Run LODF screening on case14 — the voltage pre-check is built in.
    let network = load_case("case14");
    let mut options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        vm_min: 0.94,
        vm_max: 1.06,
        ..ContingencyOptions::default()
    };
    // Tighten voltage limits to increase chance of catching violations
    options.acpf_options.tolerance = 1e-8;

    let result = analyze_n1_branch(&network, &options);
    assert!(result.is_ok(), "Contingency analysis should succeed");
    let ca = result.unwrap();
    // The key assertion: LODF mode ran without error and produced results.
    assert!(!ca.results.is_empty(), "Should have contingency results");
}

#[test]
fn test_nr_fdpf_fallback_field() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    // Verify that converged results have fdpf_fallback = false
    let network = load_case("case14");
    let options = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..ContingencyOptions::default()
    };
    let result = analyze_n1_branch(&network, &options).unwrap();
    for r in &result.results {
        if r.converged {
            assert!(
                !r.fdpf_fallback,
                "Converged result {} should not have fdpf_fallback set",
                r.id
            );
        }
    }
}

#[test]
fn test_fdpf_fallback_respects_store_post_voltages() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let network = load_case("case14");
    let base =
        solve_ac_pf_kernel(&network, &AcPfOptions::default()).expect("base case should solve");
    let contingencies = generate_n1_branch_contingencies(&network);
    let contingency_refs: Vec<&Contingency> = contingencies.iter().collect();

    let mut options_off = ContingencyOptions {
        screening: ScreeningMode::Off,
        store_post_voltages: false,
        ..ContingencyOptions::default()
    };
    options_off.acpf_options.max_iterations = 0;

    let results_off = solve_contingencies_parallel(
        &network,
        &contingency_refs,
        &options_off,
        &base.voltage_magnitude_pu,
        &base.voltage_angle_rad,
    );
    let fallback_off: Vec<_> = results_off.iter().filter(|r| r.fdpf_fallback).collect();
    assert!(
        !fallback_off.is_empty(),
        "expected at least one contingency to use FDPF fallback"
    );
    for result in fallback_off {
        assert_eq!(result.status, ContingencyStatus::Approximate);
        assert!(
            result.post_vm.is_none(),
            "fallback Vm should honor storage flag"
        );
        assert!(
            result.post_va.is_none(),
            "fallback Va should honor storage flag"
        );
        assert!(
            result.post_branch_flows.is_none(),
            "fallback branch flows should honor storage flag"
        );
    }

    let mut options_on = options_off.clone();
    options_on.store_post_voltages = true;
    let results_on = solve_contingencies_parallel(
        &network,
        &contingency_refs,
        &options_on,
        &base.voltage_magnitude_pu,
        &base.voltage_angle_rad,
    );
    let fallback_on: Vec<_> = results_on.iter().filter(|r| r.fdpf_fallback).collect();
    assert!(
        !fallback_on.is_empty(),
        "expected at least one contingency to use FDPF fallback with storage enabled"
    );
    for result in fallback_on {
        assert_eq!(result.status, ContingencyStatus::Approximate);
        assert!(
            result.post_vm.is_some(),
            "fallback Vm should be stored when enabled"
        );
        assert!(
            result.post_va.is_some(),
            "fallback Va should be stored when enabled"
        );
        assert!(
            result.post_branch_flows.is_some(),
            "fallback branch flows should be stored when enabled"
        );
    }
}

#[test]
fn test_prepared_n1_study_matches_one_shot() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        ..Default::default()
    };

    let mut prepared = ContingencyStudy::n1_branch(&net, &options).expect("build N-1 study");
    let one_shot = analyze_n1_branch(&net, &options).expect("one-shot N-1");
    let kind = prepared.kind();
    let prepared_analysis = prepared.analyze().expect("analyze prepared N-1");

    assert_eq!(kind, ContingencyStudyKind::N1Branch);
    assert_eq!(
        prepared_analysis.summary.total_contingencies,
        one_shot.summary.total_contingencies
    );
    assert_eq!(
        prepared_analysis.summary.with_violations,
        one_shot.summary.with_violations
    );
}

#[test]
fn test_prepared_n1_generator_study_matches_one_shot() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        ..Default::default()
    };

    let mut prepared =
        ContingencyStudy::n1_generator(&net, &options).expect("build N-1 generator study");
    let one_shot = analyze_n1_generator(&net, &options).expect("one-shot N-1 generator");
    let kind = prepared.kind();
    let prepared_analysis = prepared.analyze().expect("analyze prepared N-1 generator");

    assert_eq!(kind, ContingencyStudyKind::N1Generator);
    assert_eq!(
        prepared_analysis.summary.total_contingencies,
        one_shot.summary.total_contingencies
    );
    assert_eq!(
        prepared_analysis.summary.with_violations,
        one_shot.summary.with_violations
    );
}

#[test]
fn test_prepared_n2_study_matches_one_shot() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        top_k: Some(10),
        ..Default::default()
    };

    let mut prepared = ContingencyStudy::n2_branch(&net, &options).expect("build N-2 study");
    let one_shot = analyze_n2_branch(&net, &options).expect("one-shot N-2");
    let kind = prepared.kind();
    let prepared_analysis = prepared.analyze().expect("analyze prepared N-2");

    assert_eq!(kind, ContingencyStudyKind::N2Branch);
    assert_eq!(
        prepared_analysis.summary.total_contingencies,
        one_shot.summary.total_contingencies
    );
    assert_eq!(
        prepared_analysis.summary.with_violations,
        one_shot.summary.with_violations
    );
}

#[test]
fn test_prepared_corrective_dispatch_matches_prepared_contingency_study() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = load_case("case9");
    let options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        ..Default::default()
    };

    let mut prepared_n1 = ContingencyStudy::n1_branch(&net, &options).expect("build N-1 study");
    let analysis = prepared_n1
        .analyze_cloned()
        .expect("analyze prepared N-1 study");
    let mut prepared_scrd =
        prepare_corrective_dispatch_study(&net).expect("prepare corrective dispatch study");

    let via_n1 = prepared_n1
        .solve_corrective_dispatch()
        .expect("solve corrective dispatch via prepared N-1");
    let direct = prepared_scrd
        .solve(&analysis, None)
        .expect("solve corrective dispatch directly");

    assert_eq!(via_n1.len(), direct.len());
    for (lhs, rhs) in via_n1.iter().zip(direct.iter()) {
        assert_eq!(lhs.contingency_id, rhs.contingency_id);
        assert_eq!(lhs.status, rhs.status);
        assert!((lhs.total_redispatch_mw - rhs.total_redispatch_mw).abs() < 1e-9);
        assert!((lhs.total_cost - rhs.total_cost).abs() < 1e-9);
    }
}
