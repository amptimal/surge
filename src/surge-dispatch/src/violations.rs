// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Post-solve violation assessment using AC pi-model power flow.
//!
//! Evaluates a dispatch solution against the standard AC pi-model power
//! flow equations and reports bus P/Q balance mismatches, branch thermal
//! overloads, and reserve shortfalls.

use serde::{Deserialize, Serialize};
use surge_network::Network;
use surge_solution::compute_branch_power_flows;

use crate::result::DispatchSolution;

/// Per-bus violation for a single period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusBalanceViolation {
    pub bus_number: u32,
    pub p_mismatch_mw: f64,
    pub q_mismatch_mvar: f64,
}

/// Per-branch thermal violation for a single period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchThermalViolation {
    pub from_bus: u32,
    pub to_bus: u32,
    pub circuit: String,
    pub flow_mva: f64,
    pub limit_mva: f64,
    pub overload_mva: f64,
}

/// Per-period violation results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodViolations {
    pub period_index: usize,
    pub bus_p_total_abs_mismatch_mw: f64,
    pub bus_q_total_abs_mismatch_mvar: f64,
    pub thermal_total_overload_mva: f64,
    pub worst_bus_p_mismatch_mw: f64,
    pub worst_bus_p_bus: u32,
    pub worst_bus_q_mismatch_mvar: f64,
    pub worst_bus_q_bus: u32,
    pub bus_violations: Vec<BusBalanceViolation>,
    pub thermal_violations: Vec<BranchThermalViolation>,
}

/// Violation penalty configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationCosts {
    /// Active power bus balance violation cost ($/pu/hr).
    #[serde(default = "default_p_bus_vio_cost")]
    pub p_bus_vio_cost: f64,
    /// Reactive power bus balance violation cost ($/pu/hr).
    #[serde(default = "default_q_bus_vio_cost")]
    pub q_bus_vio_cost: f64,
    /// Branch thermal violation cost ($/pu/hr).
    #[serde(default = "default_s_vio_cost")]
    pub s_vio_cost: f64,
}

fn default_p_bus_vio_cost() -> f64 {
    1_000_000.0
}
fn default_q_bus_vio_cost() -> f64 {
    1_000_000.0
}
fn default_s_vio_cost() -> f64 {
    500.0
}

impl Default for ViolationCosts {
    fn default() -> Self {
        Self {
            p_bus_vio_cost: default_p_bus_vio_cost(),
            q_bus_vio_cost: default_q_bus_vio_cost(),
            s_vio_cost: default_s_vio_cost(),
        }
    }
}

/// Aggregated violation assessment report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationAssessment {
    pub bus_p_total_mismatch_mw: f64,
    pub bus_p_total_penalty: f64,
    pub bus_q_total_mismatch_mvar: f64,
    pub bus_q_total_penalty: f64,
    pub thermal_total_overload_mva: f64,
    pub thermal_total_penalty: f64,
    pub total_penalty: f64,
    pub periods: Vec<PeriodViolations>,
}

/// Compute AC pi-model violations for a solved dispatch.
///
/// For each period in the dispatch solution:
/// 1. Extracts bus voltages and angles.
/// 2. Computes branch power flows using the exact pi-model.
/// 3. Computes bus active/reactive power balance mismatches.
/// 4. Reports branch thermal overloads.
///
/// The bus mismatch is computed as:
///   `mismatch = device_injection - branch_flow`
///
/// where device injections come from the dispatch resource results
/// and branch flows come from the AC pi-model.
pub fn assess_dispatch_violations(
    network: &Network,
    solution: &DispatchSolution,
    costs: &ViolationCosts,
    interval_hours: &[f64],
) -> ViolationAssessment {
    let base_mva = network.base_mva;
    let bus_map = network.bus_index_map();
    let n_bus = network.n_buses();

    let mut total_p_mismatch_mw = 0.0f64;
    let mut total_p_penalty = 0.0f64;
    let mut total_q_mismatch_mvar = 0.0f64;
    let mut total_q_penalty = 0.0f64;
    let mut total_thermal_overload_mva = 0.0f64;
    let mut total_thermal_penalty = 0.0f64;

    let mut all_periods = Vec::with_capacity(solution.periods().len());

    for (t, period) in solution.periods().iter().enumerate() {
        let dt = interval_hours.get(t).copied().unwrap_or(1.0);

        // Extract bus voltages, angles, and net injections from bus results.
        // For DC solutions, voltage_pu defaults to 1.0 and angle_rad to 0.0.
        let mut vm = vec![1.0f64; n_bus];
        let mut va = vec![0.0f64; n_bus];
        let mut p_dev = vec![0.0f64; n_bus];
        let mut q_dev = vec![0.0f64; n_bus];

        for br in period.bus_results() {
            if let Some(&idx) = bus_map.get(&br.bus_number) {
                if let Some(v) = br.voltage_pu {
                    vm[idx] = v;
                }
                if let Some(a) = br.angle_rad {
                    va[idx] = a;
                }
                // net_injection_mw already aggregates all device P at this bus
                p_dev[idx] = br.net_injection_mw;
                if let Some(q) = br.net_reactive_injection_mvar {
                    q_dev[idx] = q;
                }
            }
        }

        // Compute AC pi-model branch power flows.
        let (pf, pt, qf, qt) = compute_branch_power_flows(network, &vm, &va, base_mva);

        // Accumulate branch power flows into bus injection totals.
        let mut p_flow = vec![0.0f64; n_bus];
        let mut q_flow = vec![0.0f64; n_bus];
        for (i, branch) in network.branches.iter().enumerate() {
            if !branch.in_service {
                continue;
            }
            if let (Some(&fi), Some(&ti)) =
                (bus_map.get(&branch.from_bus), bus_map.get(&branch.to_bus))
            {
                p_flow[fi] += pf[i];
                q_flow[fi] += qf[i];
                p_flow[ti] += pt[i];
                q_flow[ti] += qt[i];
            }
        }

        // Bus balance mismatches.
        let mut bus_violations = Vec::new();
        let mut period_p_abs = 0.0f64;
        let mut period_q_abs = 0.0f64;
        let mut worst_p = 0.0f64;
        let mut worst_p_bus = 0u32;
        let mut worst_q = 0.0f64;
        let mut worst_q_bus = 0u32;

        for (idx, bus) in network.buses.iter().enumerate() {
            let dp = p_dev[idx] - p_flow[idx];
            let dq = q_dev[idx] - q_flow[idx];
            let adp = dp.abs();
            let adq = dq.abs();

            if adp > 1e-3 || adq > 1e-3 {
                bus_violations.push(BusBalanceViolation {
                    bus_number: bus.number,
                    p_mismatch_mw: dp,
                    q_mismatch_mvar: dq,
                });
            }

            period_p_abs += adp;
            period_q_abs += adq;
            if adp > worst_p {
                worst_p = adp;
                worst_p_bus = bus.number;
            }
            if adq > worst_q {
                worst_q = adq;
                worst_q_bus = bus.number;
            }
        }

        // Branch thermal violations.
        let mut thermal_violations = Vec::new();
        let mut period_thermal_overload = 0.0f64;

        for (i, branch) in network.branches.iter().enumerate() {
            if !branch.in_service {
                continue;
            }
            let limit_mva = branch.rating_a_mva;
            if limit_mva <= 0.0 || !limit_mva.is_finite() {
                continue;
            }
            let sf = (pf[i] * pf[i] + qf[i] * qf[i]).sqrt();
            let st = (pt[i] * pt[i] + qt[i] * qt[i]).sqrt();
            let flow_mva = sf.max(st);
            let overload = flow_mva - limit_mva;
            if overload > 1e-3 {
                thermal_violations.push(BranchThermalViolation {
                    from_bus: branch.from_bus,
                    to_bus: branch.to_bus,
                    circuit: branch.circuit.clone(),
                    flow_mva,
                    limit_mva,
                    overload_mva: overload,
                });
                period_thermal_overload += overload;
            }
        }

        // Penalty costs.
        // Costs are in $/pu/hr. Mismatch is in MW, so we convert to pu.
        let p_penalty = costs.p_bus_vio_cost * (period_p_abs / base_mva) * dt;
        let q_penalty = costs.q_bus_vio_cost * (period_q_abs / base_mva) * dt;
        let thermal_penalty = costs.s_vio_cost * (period_thermal_overload / base_mva) * dt;

        total_p_mismatch_mw += period_p_abs;
        total_q_mismatch_mvar += period_q_abs;
        total_p_penalty += p_penalty;
        total_q_penalty += q_penalty;
        total_thermal_overload_mva += period_thermal_overload;
        total_thermal_penalty += thermal_penalty;

        // Sort violations by severity.
        bus_violations.sort_by(|a, b| {
            let sa = a.p_mismatch_mw.abs() + a.q_mismatch_mvar.abs();
            let sb = b.p_mismatch_mw.abs() + b.q_mismatch_mvar.abs();
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        thermal_violations.sort_by(|a, b| {
            b.overload_mva
                .partial_cmp(&a.overload_mva)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        all_periods.push(PeriodViolations {
            period_index: t,
            bus_p_total_abs_mismatch_mw: period_p_abs,
            bus_q_total_abs_mismatch_mvar: period_q_abs,
            thermal_total_overload_mva: period_thermal_overload,
            worst_bus_p_mismatch_mw: worst_p,
            worst_bus_p_bus: worst_p_bus,
            worst_bus_q_mismatch_mvar: worst_q,
            worst_bus_q_bus: worst_q_bus,
            bus_violations,
            thermal_violations,
        });
    }

    ViolationAssessment {
        bus_p_total_mismatch_mw: total_p_mismatch_mw,
        bus_p_total_penalty: total_p_penalty,
        bus_q_total_mismatch_mvar: total_q_mismatch_mvar,
        bus_q_total_penalty: total_q_penalty,
        thermal_total_overload_mva: total_thermal_overload_mva,
        thermal_total_penalty: total_thermal_penalty,
        total_penalty: total_p_penalty + total_q_penalty + total_thermal_penalty,
        periods: all_periods,
    }
}
