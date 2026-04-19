// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Discrete round-and-check verification for AC-OPF.
//!
//! After solving the continuous NLP, transformer taps, phase shifters, and
//! switched shunts are rounded to their nearest realizable discrete step.
//! An AC power flow is then run to verify that the rounded operating point
//! remains feasible (voltage, thermal, reactive limits).

use surge_ac::{AcPfOptions, solve_ac_pf};
use surge_network::Network;
use surge_network::network::discrete_control::{round_phase, round_tap};
use surge_solution::{OpfSolution, SolveStatus};

/// Result of discrete round-and-check verification.
#[derive(Debug, Clone)]
pub struct DiscreteVerification {
    /// Whether the AC power flow converged after applying rounded discrete values.
    pub converged: bool,
    /// Branches where loading exceeds 100% after rounding: `(branch_idx, loading_pct)`.
    pub thermal_violations: Vec<(usize, f64)>,
    /// Buses where voltage is outside `[vmin, vmax]`: `(bus_idx, vm, violated_bound)`.
    pub voltage_violations: Vec<(usize, f64, f64)>,
    /// Generators where Q is outside `[qmin, qmax]`: `(gen_idx, gen_q_mvar, violated_bound_mvar)`.
    pub reactive_violations: Vec<(usize, f64, f64)>,
    /// Human-readable violation descriptions.
    pub violation_descriptions: Vec<String>,
}

/// Extract tap dispatch from the NLP solution and round to discrete steps.
///
/// Returns `Vec<(branch_idx, continuous_tap, rounded_tap)>`.
pub fn extract_tap_dispatch(
    network: &Network,
    sol_x: &[f64],
    tap_ctrl_branches: &[(usize, f64, f64)],
    tap_var_offset: usize,
) -> Vec<(usize, f64, f64)> {
    tap_ctrl_branches
        .iter()
        .enumerate()
        .map(|(k, &(br_idx, _, _))| {
            let tau_cont = sol_x[tap_var_offset + k];
            let br = &network.branches[br_idx];
            let (tap_min, tap_max, tap_step) = br
                .opf_control
                .as_ref()
                .map(|c| (c.tap_min, c.tap_max, c.tap_step))
                .unwrap_or((0.9, 1.1, 0.0));
            let tau_rounded = round_tap(tau_cont, tap_min, tap_max, tap_step);
            (br_idx, tau_cont, tau_rounded)
        })
        .collect()
}

/// Extract phase-shifter dispatch from the NLP solution and round to discrete steps.
///
/// Returns `Vec<(branch_idx, continuous_rad, rounded_rad)>`.
pub fn extract_phase_dispatch(
    network: &Network,
    sol_x: &[f64],
    ps_ctrl_branches: &[(usize, f64, f64)],
    phase_var_offset: usize,
) -> Vec<(usize, f64, f64)> {
    ps_ctrl_branches
        .iter()
        .enumerate()
        .map(|(k, &(br_idx, _, _))| {
            let theta_cont = sol_x[phase_var_offset + k];
            let br = &network.branches[br_idx];
            let (phase_min_rad, phase_max_rad, phase_step_rad) = br
                .opf_control
                .as_ref()
                .map(|c| (c.phase_min_rad, c.phase_max_rad, c.phase_step_rad))
                .unwrap_or(((-30.0_f64).to_radians(), 30.0_f64.to_radians(), 0.0));
            let theta_rounded =
                round_phase(theta_cont, phase_min_rad, phase_max_rad, phase_step_rad);
            (br_idx, theta_cont, theta_rounded)
        })
        .collect()
}

/// Verify feasibility of the discrete operating point by running AC power flow.
///
/// 1. Clone the network and apply rounded discrete values (taps, phases, shunts).
/// 2. Run NR power flow (converges from case-data voltages).
/// 3. Check thermal, voltage, and reactive limits on the converged solution.
pub fn verify_discrete_solution(
    network: &Network,
    _opf_solution: &OpfSolution,
    tap_dispatch: &[(usize, f64, f64)],
    shunt_dispatch: &[(usize, f64, f64)],
    phase_dispatch: &[(usize, f64, f64)],
) -> DiscreteVerification {
    let mut net = network.clone();

    // Apply rounded taps.
    for &(br_idx, _, rounded_tap) in tap_dispatch {
        net.branches[br_idx].tap = rounded_tap;
    }

    // Apply rounded phase shifts (already in radians).
    for &(br_idx, _, rounded_rad) in phase_dispatch {
        net.branches[br_idx].phase_shift_rad = rounded_rad;
    }

    // Apply rounded shunt susceptances (pu → MVAr for Bus::bs).
    for &(bus_idx, _, rounded_b) in shunt_dispatch {
        net.buses[bus_idx].shunt_susceptance_mvar = rounded_b * network.base_mva;
    }

    // Run NR — no discrete outer loop (taps/shunts already fixed at rounded values).
    let acpf_options = AcPfOptions {
        max_iterations: 100,
        tolerance: 1e-8,
        ..Default::default()
    };

    let pf_result = match solve_ac_pf(&net, &acpf_options) {
        Ok(sol) => sol,
        Err(_) => {
            return DiscreteVerification {
                converged: false,
                thermal_violations: vec![],
                voltage_violations: vec![],
                reactive_violations: vec![],
                violation_descriptions: vec![
                    "AC power flow did not converge after discrete rounding".to_string(),
                ],
            };
        }
    };

    let converged = pf_result.status == SolveStatus::Converged;
    let mut thermal_violations = Vec::new();
    let mut voltage_violations = Vec::new();
    let mut reactive_violations = Vec::new();
    let mut descriptions = Vec::new();

    if !converged {
        descriptions.push("AC power flow did not converge after discrete rounding".to_string());
        return DiscreteVerification {
            converged,
            thermal_violations,
            voltage_violations,
            reactive_violations,
            violation_descriptions: descriptions,
        };
    }

    // Check thermal limits.
    let branch_loading_pct = pf_result
        .branch_loading_pct(&net)
        .expect("branch loading should be computable for the verified network");
    for (i, br) in net.branches.iter().enumerate() {
        if !br.in_service || br.rating_a_mva < 1.0 {
            continue;
        }
        let loading = branch_loading_pct[i];
        if loading > 100.0 + 1e-2 {
            thermal_violations.push((i, loading));
            descriptions.push(format!(
                "Branch {} ({}-{}): loading {:.1}% > 100%",
                i, br.from_bus, br.to_bus, loading
            ));
        }
    }

    // Check voltage limits.
    for (i, bus) in net.buses.iter().enumerate() {
        let vm = pf_result.voltage_magnitude_pu[i];
        if vm < bus.voltage_min_pu - 1e-4 {
            voltage_violations.push((i, vm, bus.voltage_min_pu));
            descriptions.push(format!(
                "Bus {} ({}): Vm={:.4} < Vmin={:.4}",
                i, bus.number, vm, bus.voltage_min_pu
            ));
        }
        if vm > bus.voltage_max_pu + 1e-4 {
            voltage_violations.push((i, vm, bus.voltage_max_pu));
            descriptions.push(format!(
                "Bus {} ({}): Vm={:.4} > Vmax={:.4}",
                i, bus.number, vm, bus.voltage_max_pu
            ));
        }
    }

    // After rounding, the PF may redistribute reactive power, so validate the
    // post-round solved generator Q rather than the original OPF record.
    for (gi, qg) in solved_generator_q_mvar(&net, &pf_result) {
        let g = &net.generators[gi];
        if qg < g.qmin - 1e-2 {
            reactive_violations.push((gi, qg, g.qmin));
            descriptions.push(format!(
                "Gen {} (bus {}): Qg={:.1} MVAr < Qmin={:.1}",
                gi, g.bus, qg, g.qmin
            ));
        }
        if qg > g.qmax + 1e-2 {
            reactive_violations.push((gi, qg, g.qmax));
            descriptions.push(format!(
                "Gen {} (bus {}): Qg={:.1} MVAr > Qmax={:.1}",
                gi, g.bus, qg, g.qmax
            ));
        }
    }

    DiscreteVerification {
        converged,
        thermal_violations,
        voltage_violations,
        reactive_violations,
        violation_descriptions: descriptions,
    }
}

fn solved_generator_q_mvar(
    network: &Network,
    pf_result: &surge_solution::PfSolution,
) -> Vec<(usize, f64)> {
    pf_result
        .generator_reactive_power_mvar(network)
        .into_iter()
        .enumerate()
        .filter_map(|(gi, qg)| network.generators[gi].in_service.then_some((gi, qg)))
        .collect()
}
