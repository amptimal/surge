// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

mod common;

use std::collections::HashMap;

use surge_dc::{solve_dc, to_pf_solution};
use surge_network::network::BusType;
use surge_solution::{PfModel, PfSolution, SolveStatus};

/// Load case9, run DC power flow, convert to PfSolution, then verify KCL at
/// every bus: the sum of branch flows into/out of the bus must equal the net
/// active power injection (MW).
#[test]
fn dc_to_pf_solution_kcl_balance() {
    let network = common::load_case("case9");
    let dc_result = solve_dc(&network).expect("DC solve failed");
    let pf = to_pf_solution(&dc_result, &network);

    let bus_map: HashMap<u32, usize> = network.bus_index_map();
    let n_buses = network.n_buses();
    let base = network.base_mva;

    // Accumulate net branch flow contribution at each bus (MW).
    let mut bus_flow_sum = vec![0.0_f64; n_buses];
    for (k, branch) in network.branches.iter().enumerate() {
        if !branch.in_service {
            continue;
        }
        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];
        bus_flow_sum[from_idx] += pf.branch_p_from_mw[k];
        bus_flow_sum[to_idx] += pf.branch_p_to_mw[k];
    }

    let tol = 1e-6; // 1e-6 MW tolerance
    for (i, &flow_sum) in bus_flow_sum.iter().enumerate().take(n_buses) {
        let injection_mw = pf.active_power_injection_pu[i] * base;
        assert!(
            (injection_mw - flow_sum).abs() < tol,
            "KCL violation at bus index {i} (bus {}): injection_mw={injection_mw}, flow_sum_mw={flow_sum}, diff={}",
            network.buses[i].number,
            (injection_mw - flow_sum).abs(),
        );
    }
}

/// DC power flow assumes |V| = 1.0 p.u. for all buses. Verify that the
/// PfSolution reflects this assumption.
#[test]
fn dc_pf_solution_vm_is_one() {
    let network = common::load_case("case9");
    let dc_result = solve_dc(&network).expect("DC solve failed");
    let pf = to_pf_solution(&dc_result, &network);

    assert_eq!(pf.voltage_magnitude_pu.len(), network.n_buses());
    for (i, &vm) in pf.voltage_magnitude_pu.iter().enumerate() {
        assert!(
            (vm - 1.0).abs() < f64::EPSILON,
            "bus index {i}: expected Vm=1.0, got {vm}",
        );
    }
}

/// DC power flow does not compute reactive power. Verify that all reactive
/// flow fields (branch_q_from_mvar, branch_q_to_mvar)
/// are identically 0.0.
#[test]
fn dc_pf_solution_qf_is_zero() {
    let network = common::load_case("case9");
    let dc_result = solve_dc(&network).expect("DC solve failed");
    let pf = to_pf_solution(&dc_result, &network);

    let n_branches = network.n_branches();
    assert_eq!(pf.branch_q_from_mvar.len(), n_branches);
    assert_eq!(pf.branch_q_to_mvar.len(), n_branches);

    for (k, (&qf, &qt)) in pf
        .branch_q_from_mvar
        .iter()
        .zip(pf.branch_q_to_mvar.iter())
        .enumerate()
    {
        assert!(qf == 0.0, "branch {k}: expected Qf=0.0, got {qf}",);
        assert!(qt == 0.0, "branch {k}: expected Qt=0.0, got {qt}",);
    }
}

/// Verify that PfSolution::diverged produces arrays with the requested
/// dimensions and the correct status.
#[test]
fn pf_solution_diverged_has_correct_dimensions() {
    let sol = PfSolution::diverged(10, 15, PfModel::Dc);

    assert_eq!(sol.status, SolveStatus::Diverged);
    assert_eq!(sol.pf_model, PfModel::Dc);

    // Bus-indexed arrays must have length 10.
    assert_eq!(sol.voltage_magnitude_pu.len(), 10);
    assert_eq!(sol.voltage_angle_rad.len(), 10);
    assert_eq!(sol.active_power_injection_pu.len(), 10);
    assert_eq!(sol.reactive_power_injection_pu.len(), 10);

    // Branch-indexed arrays must have length 15.
    assert_eq!(sol.branch_p_from_mw.len(), 15);
    assert_eq!(sol.branch_p_to_mw.len(), 15);
    assert_eq!(sol.branch_q_from_mvar.len(), 15);
    assert_eq!(sol.branch_q_to_mvar.len(), 15);
}

/// Verify that PfSolution::flat_start produces an Unsolved solution with
/// Vm=1.0 and Va=0.0 defaults.
#[test]
fn pf_solution_flat_start_has_correct_defaults() {
    let sol = PfSolution::flat_start(5, 3, PfModel::Ac);

    assert_eq!(sol.status, SolveStatus::Unsolved);
    assert_eq!(sol.pf_model, PfModel::Ac);

    // All voltage magnitudes should be 1.0.
    assert_eq!(sol.voltage_magnitude_pu.len(), 5);
    for &vm in &sol.voltage_magnitude_pu {
        assert!((vm - 1.0).abs() < f64::EPSILON, "expected Vm=1.0, got {vm}");
    }

    // All voltage angles should be 0.0.
    assert_eq!(sol.voltage_angle_rad.len(), 5);
    for &va in &sol.voltage_angle_rad {
        assert!((va).abs() < f64::EPSILON, "expected Va=0.0, got {va}");
    }

    // Branch arrays should have length 3.
    assert_eq!(sol.branch_p_from_mw.len(), 3);
    assert_eq!(sol.branch_p_to_mw.len(), 3);
    assert_eq!(sol.branch_q_from_mvar.len(), 3);
    assert_eq!(sol.branch_q_to_mvar.len(), 3);
}

/// The slack bus is the angle reference in DC power flow, so its voltage
/// angle must be exactly 0.0 rad.
#[test]
fn dc_pf_solution_slack_angle_zero() {
    let network = common::load_case("case9");
    let dc_result = solve_dc(&network).expect("DC solve failed");
    let pf = to_pf_solution(&dc_result, &network);

    let slack_idx = network
        .buses
        .iter()
        .position(|b| b.bus_type == BusType::Slack)
        .expect("case9 must have a slack bus");

    assert!(
        pf.voltage_angle_rad[slack_idx].abs() < f64::EPSILON,
        "slack bus angle should be 0.0, got {}",
        pf.voltage_angle_rad[slack_idx],
    );
}

/// A successful DC solve should produce a Converged status in the PfSolution.
#[test]
fn dc_pf_solution_converged_status() {
    let network = common::load_case("case9");
    let dc_result = solve_dc(&network).expect("DC solve failed");
    let pf = to_pf_solution(&dc_result, &network);

    assert_eq!(
        pf.status,
        SolveStatus::Converged,
        "expected Converged status, got {:?}",
        pf.status,
    );
    assert_eq!(
        pf.pf_model,
        PfModel::Dc,
        "expected Dc model, got {:?}",
        pf.pf_model,
    );
}
