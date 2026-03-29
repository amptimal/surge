// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Corrective action types and application logic.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use surge_ac::AcPfOptions;
use surge_ac::matrix::jacobian::JacobianPattern;
use surge_ac::matrix::mismatch::compute_power_injection;
use surge_ac::matrix::ybus::build_ybus;
use surge_ac::solve_ac_pf_kernel;
use surge_dc::PtdfRequest;
use surge_network::Network;
use surge_network::network::BusType;
use surge_solution::SolveStatus;
use surge_sparse::KluSolver;
use tracing::{debug, info};

use super::schemes::CorrectiveActionConfig;
use crate::violations::effective_voltage_limits;
use crate::{ContingencyOptions, Violation, get_rating};

/// A single corrective action that can be applied post-contingency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CorrectiveAction {
    /// Redispatch generator: adjust real power output (within pmin/pmax limits).
    GeneratorRedispatch {
        /// Index into `Network::generators`.
        gen_idx: usize,
        /// MW change (positive = increase output, negative = decrease output).
        delta_p_mw: f64,
    },
    /// Change transformer off-nominal tap ratio.
    TransformerTapChange {
        /// Index into `Network::branches`.
        branch_idx: usize,
        /// New tap ratio (p.u., e.g. 1.05).
        new_tap: f64,
    },
    /// Switch a shunt in or out at a bus.
    ShuntSwitch {
        /// Internal bus index (into `Network::buses`).
        bus: usize,
        /// Susceptance change in p.u. (positive = capacitive, negative = inductive).
        delta_b_pu: f64,
    },
    /// Load shedding as a last resort (reduces bus load by a fraction).
    LoadShed {
        /// Internal bus index (into `Network::buses`).
        bus: usize,
        /// Fraction of load to shed (0.0 = none, 1.0 = all load).
        shed_fraction: f64,
    },
    /// Open or close a breaker (switch branch in/out of service).
    BreakerSwitch {
        /// Index into `Network::branches`.
        branch_idx: usize,
        /// `true` = close (in_service = true), `false` = open (in_service = false).
        close: bool,
    },
    /// Adjust generator voltage setpoint for reactive power control.
    GeneratorVoltageSetpoint {
        /// Index into `Network::generators`.
        gen_idx: usize,
        /// New Vm setpoint in p.u.
        new_vs_pu: f64,
    },
    /// Adjust generator reactive output directly (Mvar redispatch).
    GeneratorReactiveDispatch {
        /// Index into `Network::generators`.
        gen_idx: usize,
        /// Mvar change (positive = inject vars, negative = absorb).
        delta_q_mvar: f64,
    },
}

// ---------------------------------------------------------------------------
// Apply a single corrective action to the network in-place
// ---------------------------------------------------------------------------

pub(crate) fn apply_action_to_network(network: &mut Network, action: &CorrectiveAction) {
    match action {
        CorrectiveAction::GeneratorRedispatch {
            gen_idx,
            delta_p_mw,
        } => {
            if let Some(generator) = network.generators.get_mut(*gen_idx)
                && generator.in_service
            {
                let new_pg = (generator.p + delta_p_mw).clamp(generator.pmin, generator.pmax);
                debug!(
                    "GeneratorRedispatch: gen[{}] bus={} {:.1}→{:.1} MW (Δ={:+.1})",
                    gen_idx, generator.bus, generator.p, new_pg, delta_p_mw
                );
                generator.p = new_pg;
            }
        }
        CorrectiveAction::TransformerTapChange {
            branch_idx,
            new_tap,
        } => {
            if let Some(branch) = network.branches.get_mut(*branch_idx) {
                debug!(
                    "TransformerTapChange: branch[{}] {}->{} tap {:.4}→{:.4}",
                    branch_idx, branch.from_bus, branch.to_bus, branch.tap, new_tap
                );
                branch.tap = *new_tap;
            }
        }
        CorrectiveAction::ShuntSwitch { bus, delta_b_pu } => {
            if let Some(b) = network.buses.get_mut(*bus) {
                debug!(
                    "ShuntSwitch: bus[{}]={} Δbs={:+.4} pu",
                    bus, b.number, delta_b_pu
                );
                b.shunt_susceptance_mvar += delta_b_pu;
            }
        }
        CorrectiveAction::LoadShed { bus, shed_fraction } => {
            let fraction = shed_fraction.clamp(0.0, 1.0);
            if let Some(b) = network.buses.get(*bus) {
                let bus_num = b.number;
                let mut total_shed_mw = 0.0;
                let mut total_shed_mvar = 0.0;
                for load in &mut network.loads {
                    if load.bus == bus_num && load.in_service {
                        let shed_mw = load.active_power_demand_mw * fraction;
                        let shed_mvar = load.reactive_power_demand_mvar * fraction;
                        load.active_power_demand_mw -= shed_mw;
                        load.reactive_power_demand_mvar -= shed_mvar;
                        total_shed_mw += shed_mw;
                        total_shed_mvar += shed_mvar;
                    }
                }
                debug!(
                    "LoadShed: bus[{}]={} {:.1}% ({:.1} MW, {:.1} MVAr)",
                    bus,
                    bus_num,
                    fraction * 100.0,
                    total_shed_mw,
                    total_shed_mvar
                );
            }
        }
        CorrectiveAction::BreakerSwitch { branch_idx, close } => {
            if let Some(branch) = network.branches.get_mut(*branch_idx) {
                debug!(
                    "BreakerSwitch: branch[{}] {}->{} {}",
                    branch_idx,
                    branch.from_bus,
                    branch.to_bus,
                    if *close { "CLOSE" } else { "OPEN" }
                );
                branch.in_service = *close;
            }
        }
        CorrectiveAction::GeneratorVoltageSetpoint { gen_idx, new_vs_pu } => {
            if let Some(generator) = network.generators.get_mut(*gen_idx)
                && generator.in_service
            {
                debug!(
                    "GeneratorVoltageSetpoint: gen[{}] bus={} Vs {:.4}→{:.4} pu",
                    gen_idx, generator.bus, generator.voltage_setpoint_pu, new_vs_pu
                );
                generator.voltage_setpoint_pu = *new_vs_pu;
            }
        }
        CorrectiveAction::GeneratorReactiveDispatch {
            gen_idx,
            delta_q_mvar,
        } => {
            if let Some(generator) = network.generators.get_mut(*gen_idx)
                && generator.in_service
            {
                let new_qg = (generator.q + delta_q_mvar).clamp(generator.qmin, generator.qmax);
                debug!(
                    "GeneratorReactiveDispatch: gen[{}] bus={} Qg {:.1}→{:.1} MVAr (Δ={:+.1})",
                    gen_idx, generator.bus, generator.q, new_qg, delta_q_mvar
                );
                generator.q = new_qg;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Greedy PTDF-based thermal redispatch
// ---------------------------------------------------------------------------

pub(crate) fn greedy_thermal_redispatch(
    network: &mut Network,
    outaged_branches: &[usize],
    mut remaining_violations: Vec<Violation>,
    applied: &mut Vec<CorrectiveAction>,
    config: &CorrectiveActionConfig,
    acpf_opts: &AcPfOptions,
    ctg_options: &ContingencyOptions,
) -> Vec<Violation> {
    let monitored_set: HashSet<usize> = remaining_violations
        .iter()
        .filter_map(|v| {
            if let Violation::ThermalOverload { branch_idx, .. } = v {
                Some(*branch_idx)
            } else {
                None
            }
        })
        .collect();
    let monitored: Vec<usize> = monitored_set.into_iter().collect();
    let sparse_ptdf = match surge_dc::compute_ptdf(network, &PtdfRequest::for_branches(&monitored))
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "PTDF computation failed for corrective redispatch ({}); \
                 redispatch sensitivities will be zero — no corrective action taken",
                e
            );
            Default::default()
        }
    };
    let bus_map = network.bus_index_map();

    for iter in 0..config.max_redispatch_iter {
        let overloaded: Vec<(usize, f64, f64)> = remaining_violations
            .iter()
            .filter_map(|v| {
                if let Violation::ThermalOverload {
                    branch_idx,
                    flow_mva,
                    limit_mva,
                    ..
                } = v
                {
                    Some((*branch_idx, *flow_mva, *limit_mva))
                } else {
                    None
                }
            })
            .collect();

        if overloaded.is_empty() {
            break;
        }

        debug!(
            "Greedy thermal redispatch iter {}: {} overloaded branches",
            iter,
            overloaded.len()
        );

        let mut any_redispatch = false;

        for (br_idx, flow_mva, limit_mva) in &overloaded {
            let overload_mw = flow_mva - limit_mva;
            let target_relief_mw = overload_mw * config.redispatch_step_fraction;

            let mut ramp_down: Vec<(usize, f64)> = Vec::new();
            let mut ramp_up: Vec<(usize, f64)> = Vec::new();

            let empty_row: Vec<f64> = vec![0.0; network.n_buses()];
            let ptdf_row = sparse_ptdf.row(*br_idx).unwrap_or(&empty_row);

            for (gi, generator) in network.generators.iter().enumerate() {
                if !generator.in_service {
                    continue;
                }
                let Some(&bidx) = bus_map.get(&generator.bus) else {
                    continue;
                };
                let ptdf_val = ptdf_row[bidx];
                if ptdf_val.abs() < 1e-6 {
                    continue;
                }
                if ptdf_val > 0.0 {
                    ramp_down.push((gi, ptdf_val));
                } else {
                    ramp_up.push((gi, -ptdf_val));
                }
            }

            ramp_down.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            ramp_up.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut relief_achieved = 0.0_f64;
            for (gi, ptdf_val) in &ramp_down {
                if relief_achieved >= target_relief_mw {
                    break;
                }
                let g_ref = &network.generators[*gi];
                let headroom_down = g_ref.p - g_ref.pmin;
                if headroom_down < 1.0 {
                    continue;
                }
                let needed_delta = (target_relief_mw - relief_achieved) / ptdf_val;
                let actual_delta = needed_delta.min(headroom_down);
                network.generators[*gi].p -= actual_delta;
                network.generators[*gi].p =
                    network.generators[*gi].p.max(network.generators[*gi].pmin);
                relief_achieved += actual_delta * ptdf_val;
                applied.push(CorrectiveAction::GeneratorRedispatch {
                    gen_idx: *gi,
                    delta_p_mw: -actual_delta,
                });
                any_redispatch = true;
            }

            let balance_needed =
                relief_achieved / config.redispatch_step_fraction.max(f64::EPSILON);
            let mut remaining_balance = balance_needed;
            for (gi, _) in &ramp_up {
                if remaining_balance <= 0.0 {
                    break;
                }
                let g_ref = &network.generators[*gi];
                let headroom_up = g_ref.pmax - g_ref.p;
                if headroom_up < 1.0 {
                    continue;
                }
                let actual_delta = remaining_balance.min(headroom_up);
                network.generators[*gi].p += actual_delta;
                network.generators[*gi].p =
                    network.generators[*gi].p.min(network.generators[*gi].pmax);
                remaining_balance -= actual_delta;
                applied.push(CorrectiveAction::GeneratorRedispatch {
                    gen_idx: *gi,
                    delta_p_mw: actual_delta,
                });
                any_redispatch = true;
            }
        }

        if !any_redispatch {
            debug!(
                "Greedy thermal redispatch: no feasible redispatch found at iter {iter} — stopping"
            );
            break;
        }

        let (new_violations, _) =
            solve_and_detect_violations(network, outaged_branches, acpf_opts, ctg_options);

        let still_thermal = new_violations
            .iter()
            .any(|v| matches!(v, Violation::ThermalOverload { .. }));
        remaining_violations = new_violations;

        if !still_thermal {
            debug!(
                "Greedy thermal redispatch cleared all thermal violations after {} iter(s)",
                iter + 1
            );
            break;
        }
    }

    remaining_violations
}

// ---------------------------------------------------------------------------
// Greedy Q-V sensitivity-based reactive dispatch
// ---------------------------------------------------------------------------

/// Compute dV/dQ sensitivities at target buses using the AC Jacobian.
///
/// From the NR Jacobian partitioned as:
/// ```text
///   [J_Pθ  J_PV] [Δθ]   [ΔP]
///   [J_Qθ  J_QV] [ΔV] = [ΔQ]
/// ```
///
/// With ΔP = 0, we solve the full system with RHS = [0; e_k] for each target
/// bus k to get the dVm/dQ sensitivity column.  This uses one KLU factor +
/// one solve per target bus.
fn compute_qv_sensitivities(
    network: &Network,
    vm: &[f64],
    va: &[f64],
    target_buses: &[usize],
) -> HashMap<usize, Vec<f64>> {
    let n = network.n_buses();
    let ybus = build_ybus(network);

    let slack_idx = network
        .buses
        .iter()
        .position(|b| b.bus_type == BusType::Slack)
        .unwrap_or(0);

    // All non-slack buses are both pvpq and pq for sensitivity purposes.
    let pvpq: Vec<usize> = (0..n).filter(|&i| i != slack_idx).collect();
    let pq: Vec<usize> = pvpq.clone();
    let n_pvpq = pvpq.len();

    let (p_calc, q_calc) = compute_power_injection(&ybus, vm, va);
    let pattern = JacobianPattern::new(&ybus, &pvpq, &pq);
    let jac = pattern.build(vm, va, &p_calc, &q_calc);
    let jac_ref = jac.as_ref();
    let sym = jac_ref.symbolic();
    let col_ptrs: Vec<usize> = sym.col_ptr().to_vec();
    let row_indices: Vec<usize> = sym.row_idx().to_vec();
    let values: Vec<f64> = jac_ref.val().to_vec();

    let dim = pattern.dim();
    let mut klu = match KluSolver::new(dim, &col_ptrs, &row_indices) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("Q-V sensitivity: KLU symbolic failed: {e}");
            return HashMap::new();
        }
    };
    if klu.factor(&values).is_err() {
        tracing::warn!("Q-V sensitivity: KLU factorization failed (singular Jacobian)");
        return HashMap::new();
    }

    // Build position lookup: internal bus index → pvpq position.
    let mut bus_to_pvpq = vec![usize::MAX; n];
    for (pos, &bus) in pvpq.iter().enumerate() {
        bus_to_pvpq[bus] = pos;
    }

    let mut result = HashMap::new();

    for &target_bus in target_buses {
        let pvpq_pos = bus_to_pvpq[target_bus];
        if pvpq_pos == usize::MAX {
            continue; // slack bus — skip
        }

        // RHS: [ΔP; ΔQ] = [0...; 0...1_at_target...0]
        // The Q part starts at offset n_pvpq. The target bus position in the
        // pq section: since pq == pvpq, the position is the same.
        let mut rhs = vec![0.0; dim];
        rhs[n_pvpq + pvpq_pos] = 1.0;

        if klu.solve(&mut rhs).is_err() {
            continue;
        }

        // Extract dVm/dQ for all PQ buses from the solution.
        // The Vm part of the solution is at offsets [n_pvpq .. dim].
        let mut dv_dq = vec![0.0; n];
        for (pos, &bus) in pq.iter().enumerate() {
            dv_dq[bus] = rhs[n_pvpq + pos];
        }

        result.insert(target_bus, dv_dq);
    }

    result
}

pub(crate) fn greedy_reactive_redispatch(
    network: &mut Network,
    outaged_branches: &[usize],
    mut remaining_violations: Vec<Violation>,
    applied: &mut Vec<CorrectiveAction>,
    config: &CorrectiveActionConfig,
    acpf_opts: &AcPfOptions,
    ctg_options: &ContingencyOptions,
) -> Vec<Violation> {
    let bus_map = network.bus_index_map();

    for iter in 0..config.max_redispatch_iter {
        // Collect voltage violations.
        let voltage_viols: Vec<(u32, f64, f64, bool)> = remaining_violations
            .iter()
            .filter_map(|v| match v {
                Violation::VoltageLow {
                    bus_number,
                    vm,
                    limit,
                } => Some((*bus_number, *vm, *limit, true)),
                Violation::VoltageHigh {
                    bus_number,
                    vm,
                    limit,
                } => Some((*bus_number, *vm, *limit, false)),
                _ => None,
            })
            .collect();

        if voltage_viols.is_empty() {
            break;
        }

        debug!(
            "Greedy reactive redispatch iter {}: {} voltage violations",
            iter,
            voltage_viols.len()
        );

        // Solve to get current (Vm, Va) for Q-V sensitivity.
        let (vm, va) = match solve_ac_pf_kernel(network, acpf_opts) {
            Ok(sol) if sol.status == SolveStatus::Converged => {
                (sol.voltage_magnitude_pu, sol.voltage_angle_rad)
            }
            _ => {
                debug!("Greedy reactive: NR failed to converge — stopping");
                break;
            }
        };

        // Compute Q-V sensitivities at violated buses.
        let target_buses: Vec<usize> = voltage_viols
            .iter()
            .filter_map(|(bn, _, _, _)| bus_map.get(bn).copied())
            .collect();

        let qv_sens = compute_qv_sensitivities(network, &vm, &va, &target_buses);
        if qv_sens.is_empty() {
            debug!("Greedy reactive: Q-V sensitivity computation failed — stopping");
            break;
        }

        let mut any_action = false;

        for (bus_number, _vm_actual, vm_target, is_low) in &voltage_viols {
            let Some(&bus_idx) = bus_map.get(bus_number) else {
                continue;
            };
            let Some(dv_dq) = qv_sens.get(&bus_idx) else {
                continue;
            };

            let vm_deficit = if *is_low {
                *vm_target - vm[bus_idx] // positive: need to raise Vm
            } else {
                vm[bus_idx] - *vm_target // positive: need to lower Vm
            };

            if vm_deficit <= 0.0 {
                continue; // already within limits
            }

            let target_dv = vm_deficit * config.redispatch_step_fraction;

            // Priority 1: Generator voltage setpoint adjustment.
            // Find PV generators whose Vset change has highest dVm impact at
            // the violated bus. For a PV generator at bus g, raising Vs by ΔVs
            // is equivalent to injecting ΔQ ≈ ΔVs / dV_dQ[g][g] at bus g,
            // which changes the target bus voltage by dV_dQ[target][g] × ΔQ.
            let mut gen_effectiveness: Vec<(usize, f64)> = Vec::new();
            for (gi, generator) in network.generators.iter().enumerate() {
                if !generator.in_service {
                    continue;
                }
                let Some(&gen_bus) = bus_map.get(&generator.bus) else {
                    continue;
                };
                // How much does voltage at violated bus change per unit Q at gen bus?
                let sens = dv_dq[gen_bus];
                // For low voltage, we want positive sens (injecting Q raises V).
                // For high voltage, we want negative sens (absorbing Q lowers V).
                let effective_sens = if *is_low { sens } else { -sens };
                if effective_sens > 1e-6 {
                    gen_effectiveness.push((gi, effective_sens));
                }
            }
            gen_effectiveness
                .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut remaining_dv = target_dv;

            // Adjust generator voltage setpoints.
            for (gi, sens) in &gen_effectiveness {
                if remaining_dv <= 1e-6 {
                    break;
                }
                let g_ref = &network.generators[*gi];
                let current_vs = g_ref.voltage_setpoint_pu;
                // How much Vs change needed to achieve remaining_dv at target?
                // dV_target ≈ sens × ΔQ, and ΔQ ≈ ΔVs × B_gen (large),
                // so approximate: ΔVs ≈ remaining_dv / sens (in pu).
                // Clamp to ±0.05 pu per step to avoid oscillation.
                let delta_vs = (remaining_dv / sens).min(0.05);
                let new_vs = if *is_low {
                    (current_vs + delta_vs).min(1.10) // don't exceed typical upper bound
                } else {
                    (current_vs - delta_vs).max(0.90) // don't go below typical lower bound
                };
                let actual_delta = (new_vs - current_vs).abs();
                if actual_delta < 1e-5 {
                    continue;
                }

                network.generators[*gi].voltage_setpoint_pu = new_vs;
                remaining_dv -= actual_delta * sens;
                applied.push(CorrectiveAction::GeneratorVoltageSetpoint {
                    gen_idx: *gi,
                    new_vs_pu: new_vs,
                });
                any_action = true;
            }

            // Priority 2: Shunt switching.
            if remaining_dv > 1e-6 {
                // For low voltage: switch in capacitive shunt (positive ΔBs).
                // For high voltage: switch out caps / switch in reactor (negative ΔBs).
                // Approximate: ΔQ_shunt ≈ ΔBs × Vm² (in pu), and
                // dV_target ≈ dV_dQ[target][bus] × ΔQ_shunt.
                // Find buses near the violation with the best sensitivity.
                let mut shunt_candidates: Vec<(usize, f64)> = Vec::new();
                for (bi, _bus) in network.buses.iter().enumerate() {
                    let sens = dv_dq[bi];
                    let effective_sens = if *is_low { sens } else { -sens };
                    if effective_sens > 1e-6 {
                        shunt_candidates.push((bi, effective_sens));
                    }
                }
                shunt_candidates
                    .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

                // Apply one shunt at the most effective bus.
                if let Some((best_bus, sens)) = shunt_candidates.first() {
                    let vm_at_bus = vm[*best_bus];
                    let needed_dq = remaining_dv / sens;
                    let delta_bs = needed_dq / (vm_at_bus * vm_at_bus).max(0.5);
                    // Clamp to a reasonable step (0.5 pu ≈ 50 MVAr at 100 MVA base).
                    let delta_bs = delta_bs.min(0.5);
                    let delta_bs = if *is_low { delta_bs } else { -delta_bs };

                    network.buses[*best_bus].shunt_susceptance_mvar += delta_bs;
                    remaining_dv -= (delta_bs.abs() * vm_at_bus * vm_at_bus * sens).abs();
                    applied.push(CorrectiveAction::ShuntSwitch {
                        bus: *best_bus,
                        delta_b_pu: delta_bs,
                    });
                    any_action = true;
                }
            }

            // Priority 3: Direct Q redispatch for generators near Q limits.
            if remaining_dv > 1e-6 {
                for (gi, sens) in &gen_effectiveness {
                    if remaining_dv <= 1e-6 {
                        break;
                    }
                    let g_ref = &network.generators[*gi];
                    let headroom_q = if *is_low {
                        g_ref.qmax - g_ref.q // inject more vars
                    } else {
                        g_ref.q - g_ref.qmin // absorb more vars
                    };
                    if headroom_q < 0.1 {
                        continue;
                    }
                    let needed_dq = remaining_dv / sens;
                    let actual_dq = needed_dq.min(headroom_q);
                    let delta_q = if *is_low { actual_dq } else { -actual_dq };

                    let new_qg =
                        (network.generators[*gi].q + delta_q).clamp(g_ref.qmin, g_ref.qmax);
                    let actual_delta = (new_qg - network.generators[*gi].q).abs();
                    if actual_delta < 0.01 {
                        continue;
                    }
                    network.generators[*gi].q = new_qg;
                    remaining_dv -= actual_delta * sens;
                    applied.push(CorrectiveAction::GeneratorReactiveDispatch {
                        gen_idx: *gi,
                        delta_q_mvar: delta_q * network.base_mva,
                    });
                    any_action = true;
                }
            }
        }

        if !any_action {
            debug!("Greedy reactive: no feasible reactive action found at iter {iter} — stopping");
            break;
        }

        // Re-solve and update violation list.
        let (new_violations, _) =
            solve_and_detect_violations(network, outaged_branches, acpf_opts, ctg_options);

        let still_voltage = new_violations.iter().any(|v| {
            matches!(
                v,
                Violation::VoltageLow { .. } | Violation::VoltageHigh { .. }
            )
        });
        remaining_violations = new_violations;

        if !still_voltage {
            debug!(
                "Greedy reactive cleared all voltage violations after {} iter(s)",
                iter + 1
            );
            break;
        }
    }

    remaining_violations
}

// ---------------------------------------------------------------------------
// Greedy flowgate/interface redispatch
// ---------------------------------------------------------------------------

pub(crate) fn greedy_flowgate_redispatch(
    network: &mut Network,
    outaged_branches: &[usize],
    mut remaining_violations: Vec<Violation>,
    applied: &mut Vec<CorrectiveAction>,
    config: &CorrectiveActionConfig,
    acpf_opts: &AcPfOptions,
    ctg_options: &ContingencyOptions,
) -> Vec<Violation> {
    // Build a lookup from flowgate/interface name → branch coefficients.
    let mut fg_defs: HashMap<&str, &[(usize, f64)]> = HashMap::new();
    for fg in &config.flowgates {
        fg_defs.insert(&fg.name, &fg.branch_coefficients);
    }
    for ifc in &config.interfaces {
        fg_defs.insert(&ifc.name, &ifc.branch_coefficients);
    }

    // Collect all branch indices involved in any flowgate definition.
    let all_fg_branches: HashSet<usize> = fg_defs
        .values()
        .flat_map(|coeffs| coeffs.iter().map(|(bi, _)| *bi))
        .collect();
    let monitored: Vec<usize> = all_fg_branches.into_iter().collect();

    // Compute PTDF for flowgate component branches.
    let ptdf = match surge_dc::compute_ptdf(network, &PtdfRequest::for_branches(&monitored)) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("PTDF for flowgate redispatch failed: {e}");
            return remaining_violations;
        }
    };
    let bus_map = network.bus_index_map();

    for iter in 0..config.max_redispatch_iter {
        let fg_viols: Vec<(&str, f64, f64)> = remaining_violations
            .iter()
            .filter_map(|v| match v {
                Violation::FlowgateOverload {
                    name,
                    flow_mw,
                    limit_mw,
                    ..
                } => Some((name.as_str(), *flow_mw, *limit_mw)),
                Violation::InterfaceOverload {
                    name,
                    flow_mw,
                    limit_mw,
                    ..
                } => Some((name.as_str(), *flow_mw, *limit_mw)),
                _ => None,
            })
            .collect();

        if fg_viols.is_empty() {
            break;
        }

        debug!(
            "Greedy flowgate redispatch iter {}: {} violations",
            iter,
            fg_viols.len()
        );

        let mut any_redispatch = false;

        for (fg_name, flow_mw, limit_mw) in &fg_viols {
            let overload_mw = flow_mw.abs() - *limit_mw;
            if overload_mw <= 0.0 {
                continue;
            }
            let target_relief_mw = overload_mw * config.redispatch_step_fraction;
            let flow_sign = flow_mw.signum();

            let Some(coeffs) = fg_defs.get(fg_name) else {
                continue;
            };

            // Compute aggregated flowgate PTDF for each bus:
            // fg_ptdf[bus] = Σ coeff_i × ptdf[branch_i][bus]
            let n_buses = network.n_buses();
            let empty_row: Vec<f64> = vec![0.0; n_buses];
            let mut fg_ptdf = vec![0.0; n_buses];
            for (br_idx, coeff) in *coeffs {
                let ptdf_row = ptdf.row(*br_idx).unwrap_or(&empty_row);
                for (bi, val) in ptdf_row.iter().enumerate() {
                    fg_ptdf[bi] += coeff * val;
                }
            }

            // Classify generators by flowgate PTDF sign (adjusted for flow direction).
            let mut ramp_down: Vec<(usize, f64)> = Vec::new();
            let mut ramp_up: Vec<(usize, f64)> = Vec::new();

            for (gi, generator) in network.generators.iter().enumerate() {
                if !generator.in_service {
                    continue;
                }
                let Some(&bidx) = bus_map.get(&generator.bus) else {
                    continue;
                };
                let ptdf_val = fg_ptdf[bidx] * flow_sign;
                if ptdf_val.abs() < 1e-6 {
                    continue;
                }
                if ptdf_val > 0.0 {
                    ramp_down.push((gi, ptdf_val));
                } else {
                    ramp_up.push((gi, -ptdf_val));
                }
            }

            ramp_down.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            ramp_up.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut relief_achieved = 0.0_f64;
            for (gi, ptdf_val) in &ramp_down {
                if relief_achieved >= target_relief_mw {
                    break;
                }
                let g_ref = &network.generators[*gi];
                let headroom_down = g_ref.p - g_ref.pmin;
                if headroom_down < 1.0 {
                    continue;
                }
                let needed_delta = (target_relief_mw - relief_achieved) / ptdf_val;
                let actual_delta = needed_delta.min(headroom_down);
                network.generators[*gi].p -= actual_delta;
                network.generators[*gi].p =
                    network.generators[*gi].p.max(network.generators[*gi].pmin);
                relief_achieved += actual_delta * ptdf_val;
                applied.push(CorrectiveAction::GeneratorRedispatch {
                    gen_idx: *gi,
                    delta_p_mw: -actual_delta,
                });
                any_redispatch = true;
            }

            // Balance: ramp up counter-flow generators.
            let mut remaining_balance = relief_achieved;
            for (gi, _) in &ramp_up {
                if remaining_balance <= 0.0 {
                    break;
                }
                let g_ref = &network.generators[*gi];
                let headroom_up = g_ref.pmax - g_ref.p;
                if headroom_up < 1.0 {
                    continue;
                }
                let actual_delta = remaining_balance.min(headroom_up);
                network.generators[*gi].p += actual_delta;
                network.generators[*gi].p =
                    network.generators[*gi].p.min(network.generators[*gi].pmax);
                remaining_balance -= actual_delta;
                applied.push(CorrectiveAction::GeneratorRedispatch {
                    gen_idx: *gi,
                    delta_p_mw: actual_delta,
                });
                any_redispatch = true;
            }
        }

        if !any_redispatch {
            debug!("Greedy flowgate: no feasible redispatch found at iter {iter} — stopping");
            break;
        }

        let (new_violations, _) =
            solve_and_detect_violations(network, outaged_branches, acpf_opts, ctg_options);

        let still_fg = new_violations.iter().any(|v| {
            matches!(
                v,
                Violation::FlowgateOverload { .. } | Violation::InterfaceOverload { .. }
            )
        });
        remaining_violations = new_violations;

        if !still_fg {
            debug!(
                "Greedy flowgate cleared all fg/interface violations after {} iter(s)",
                iter + 1
            );
            break;
        }
    }

    remaining_violations
}

// ---------------------------------------------------------------------------
// Load shedding (last resort — thermal + voltage)
// ---------------------------------------------------------------------------

pub(crate) fn load_shed_last_resort(
    network: &mut Network,
    outaged_branches: &[usize],
    remaining_violations: Vec<Violation>,
    applied: &mut Vec<CorrectiveAction>,
    config: &CorrectiveActionConfig,
    acpf_opts: &AcPfOptions,
    ctg_options: &ContingencyOptions,
) -> Vec<Violation> {
    // Compute sparse PTDF for thermal overloaded branches.
    let monitored_set: HashSet<usize> = remaining_violations
        .iter()
        .filter_map(|v| {
            if let Violation::ThermalOverload { branch_idx, .. } = v {
                Some(*branch_idx)
            } else {
                None
            }
        })
        .collect();
    let monitored: Vec<usize> = monitored_set.into_iter().collect();
    let sparse_ptdf = match surge_dc::compute_ptdf(network, &PtdfRequest::for_branches(&monitored))
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "PTDF computation failed for corrective load shedding ({}); \
                 load-shed sensitivities will be zero — no load shedding will be applied",
                e
            );
            Default::default()
        }
    };
    let empty_row: Vec<f64> = vec![0.0; network.n_buses()];
    let bus_map = network.bus_index_map();

    let mut total_shed_mw = 0.0_f64;

    // Thermal load shedding: PTDF-based.
    for v in &remaining_violations {
        if let Violation::ThermalOverload {
            branch_idx,
            flow_mva,
            limit_mva,
            ..
        } = v
        {
            if total_shed_mw >= config.max_load_shed_mw {
                break;
            }

            let overload_mw = flow_mva - limit_mva;
            if overload_mw <= 0.0 {
                continue;
            }

            let ptdf_row = sparse_ptdf.row(*branch_idx).unwrap_or(&empty_row);

            let bus_pd_mw = network.bus_load_p_mw();
            let mut load_buses: Vec<(usize, f64, f64)> = Vec::new();
            for bi in 0..network.buses.len() {
                if bus_pd_mw[bi] <= 0.0 {
                    continue;
                }
                let ptdf_val = ptdf_row[bi];
                if ptdf_val > 1e-6 {
                    load_buses.push((bi, ptdf_val, bus_pd_mw[bi]));
                }
            }

            if load_buses.is_empty() {
                for (bi, &pd) in bus_pd_mw.iter().enumerate() {
                    if pd > 0.0 {
                        load_buses.push((bi, 1.0, pd));
                    }
                }
            }

            load_buses.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut remaining_overload = overload_mw;
            for (bi, ptdf_val, _bus_pd) in &load_buses {
                if remaining_overload <= 0.0 || total_shed_mw >= config.max_load_shed_mw {
                    break;
                }
                let bus_pd_current = network.bus_load_p_mw()[*bi];
                if bus_pd_current <= 0.0 {
                    continue;
                }
                let allowed_shed = (config.max_load_shed_mw - total_shed_mw).min(bus_pd_current);
                let shed_for_relief = (remaining_overload / ptdf_val).min(allowed_shed);
                let shed_mw = shed_for_relief.max(0.0);
                let shed_fraction = (shed_mw / bus_pd_current).clamp(0.0, 1.0);

                if shed_fraction < 1e-6 {
                    continue;
                }

                total_shed_mw += shed_mw;
                remaining_overload -= shed_mw * ptdf_val;

                let action = CorrectiveAction::LoadShed {
                    bus: *bi,
                    shed_fraction,
                };
                apply_action_to_network(network, &action);
                applied.push(action);

                info!(
                    "LoadShed (thermal): bus[{}]={} — {:.1} MW ({:.1}%) to relieve branch {} overload",
                    bi,
                    network.buses[*bi].number,
                    shed_mw,
                    shed_fraction * 100.0,
                    branch_idx
                );
            }
        }
    }

    // Voltage load shedding (UVLS): for low-voltage violations, shed load at
    // or near the violated bus to reduce reactive demand and raise voltage.
    for v in &remaining_violations {
        if let Violation::VoltageLow {
            bus_number,
            vm,
            limit,
        } = v
        {
            if total_shed_mw >= config.max_load_shed_mw {
                break;
            }
            let Some(&bus_idx) = bus_map.get(bus_number) else {
                continue;
            };
            let bus_pd = network.bus_load_p_mw()[bus_idx];
            if bus_pd <= 0.0 {
                continue;
            }

            // Heuristic: shed enough load to raise Vm to target.
            // Approximate voltage rise per MW shed ≈ 0.01 pu / (50 MW at 100 MVA base).
            let deficit = limit - vm;
            let approx_shed_mw = (deficit * 50.0 / 0.01).min(bus_pd);
            let allowed = (config.max_load_shed_mw - total_shed_mw).min(bus_pd);
            let shed_mw = approx_shed_mw.min(allowed).max(0.0);
            let shed_fraction = (shed_mw / bus_pd).clamp(0.0, 1.0);

            if shed_fraction < 1e-6 {
                continue;
            }

            total_shed_mw += shed_mw;
            let action = CorrectiveAction::LoadShed {
                bus: bus_idx,
                shed_fraction,
            };
            apply_action_to_network(network, &action);
            applied.push(action);

            info!(
                "LoadShed (UVLS): bus {} — {:.1} MW ({:.1}%) to raise Vm from {:.4} toward {:.4} pu",
                bus_number,
                shed_mw,
                shed_fraction * 100.0,
                vm,
                limit
            );
        }
    }

    let (new_violations, _) =
        solve_and_detect_violations(network, outaged_branches, acpf_opts, ctg_options);
    new_violations
}

// ---------------------------------------------------------------------------
// Helper: solve post-contingency network and detect violations
// ---------------------------------------------------------------------------

pub(crate) fn solve_and_detect_violations(
    network: &Network,
    outaged_branches: &[usize],
    acpf_opts: &AcPfOptions,
    ctg_options: &ContingencyOptions,
) -> (Vec<Violation>, bool) {
    match solve_ac_pf_kernel(network, acpf_opts) {
        Ok(sol) if sol.status == SolveStatus::Converged => {
            let bus_map = network.bus_index_map();
            let violations = detect_violations_from_network(
                network,
                &sol.voltage_magnitude_pu,
                &sol.voltage_angle_rad,
                &bus_map,
                outaged_branches,
                ctg_options,
            );
            (violations, true)
        }
        Ok(sol) => (
            vec![Violation::NonConvergent {
                max_mismatch: sol.max_mismatch,
                iterations: sol.iterations,
            }],
            false,
        ),
        Err(_) => (
            vec![Violation::NonConvergent {
                max_mismatch: f64::INFINITY,
                iterations: 0,
            }],
            false,
        ),
    }
}

/// Compute branch thermal and bus voltage violations from solved voltages,
/// skipping branches in `outaged_branches`.
fn detect_violations_from_network(
    network: &Network,
    vm: &[f64],
    va: &[f64],
    bus_map: &HashMap<u32, usize>,
    outaged_branches: &[usize],
    options: &ContingencyOptions,
) -> Vec<Violation> {
    let outaged_set: std::collections::HashSet<usize> = outaged_branches.iter().cloned().collect();
    let base_mva = network.base_mva;
    let mut violations = Vec::new();

    for (i, branch) in network.branches.iter().enumerate() {
        let rating = get_rating(branch, options.thermal_rating);
        if !branch.in_service || rating <= 0.0 || outaged_set.contains(&i) {
            continue;
        }

        let f = bus_map[&branch.from_bus];
        let t = bus_map[&branch.to_bus];

        let vi = vm[f];
        let vj = vm[t];
        let theta_ij = va[f] - va[t];
        let flows = branch.power_flows_pu(vi, vj, theta_ij, 1e-40);
        let sf_mva = flows.s_from_pu() * base_mva;
        let st_mva = flows.s_to_pu() * base_mva;
        let (flow_mw, s_mva) = if st_mva > sf_mva {
            (flows.p_to_pu * base_mva, st_mva)
        } else {
            (flows.p_from_pu * base_mva, sf_mva)
        };
        let loading_pct = s_mva / rating * 100.0;

        if loading_pct / 100.0 > options.thermal_threshold_frac {
            violations.push(Violation::ThermalOverload {
                branch_idx: i,
                from_bus: branch.from_bus,
                to_bus: branch.to_bus,
                loading_pct,
                flow_mw,
                flow_mva: s_mva,
                limit_mva: rating,
            });
        }
    }

    for (i, bus) in network.buses.iter().enumerate() {
        let v = vm[i];
        let (vmin, vmax) = effective_voltage_limits(bus, options);
        if v < vmin {
            violations.push(Violation::VoltageLow {
                bus_number: bus.number,
                vm: v,
                limit: vmin,
            });
        }
        if v > vmax {
            violations.push(Violation::VoltageHigh {
                bus_number: bus.number,
                vm: v,
                limit: vmax,
            });
        }
    }

    violations
}
