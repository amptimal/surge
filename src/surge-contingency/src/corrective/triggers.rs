// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Arming conditions and trigger conditions for RAS/SPS schemes.

use serde::{Deserialize, Serialize};
use surge_network::Network;

use crate::{ContingencyResult, Violation};

/// Pre-contingency condition that determines whether a RAS is armed.
///
/// Evaluated against the base-case power flow solution (before any contingency
/// is applied).  A scheme that is not armed cannot fire regardless of
/// post-contingency trigger conditions.
///
/// All variants use **internal indices** (into `Network::buses` /
/// `Network::branches`).  The Python binding layer handles name-to-index
/// resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArmCondition {
    /// Armed when base-case branch loading exceeds threshold.
    BranchLoading {
        /// Index into `Network::branches`.
        branch_idx: usize,
        /// Loading percentage threshold (e.g. 80.0 = 80%).
        threshold_pct: f64,
    },
    /// Armed when base-case bus voltage is below threshold.
    VoltageLow {
        /// Internal bus index (into `Network::buses`).
        bus_idx: usize,
        /// Voltage magnitude threshold in p.u.
        threshold_pu: f64,
    },
    /// Armed when base-case bus voltage is above threshold.
    VoltageHigh {
        /// Internal bus index (into `Network::buses`).
        bus_idx: usize,
        /// Voltage magnitude threshold in p.u.
        threshold_pu: f64,
    },
    /// Armed when aggregate flow across an interface exceeds threshold.
    InterfaceFlow {
        /// Human-readable interface name (for logging).
        name: String,
        /// Component branch indices + direction coefficients.
        branch_coefficients: Vec<(usize, f64)>,
        /// Flow threshold in MW (absolute value compared).
        threshold_mw: f64,
    },
    /// Armed when total system generation exceeds threshold.
    SystemGenerationAbove { threshold_mw: f64 },
    /// Armed when total system generation is below threshold.
    SystemGenerationBelow { threshold_mw: f64 },
    /// Negate an arming condition.
    Not(Box<ArmCondition>),
    /// All nested conditions must be satisfied (logical AND).
    All(Vec<ArmCondition>),
    /// Any nested condition must be satisfied (logical OR).
    Any(Vec<ArmCondition>),
}

/// Pre-contingency solved state used for RAS arming evaluation.
///
/// Computed once from the base-case AC power flow solution and passed to
/// every contingency's corrective action evaluation.
#[derive(Debug, Clone)]
pub struct BaseCaseState {
    /// Base-case voltage magnitudes (per bus, p.u.).
    pub vm: Vec<f64>,
    /// Base-case voltage angles (per bus, radians).
    pub va: Vec<f64>,
    /// Base-case real power branch flows (per branch, MW).
    pub branch_flow_mw: Vec<f64>,
    /// Total in-service generation (MW).
    pub total_gen_mw: f64,
}

impl BaseCaseState {
    /// Build from a converged AC power flow solution.
    pub fn from_solution(network: &Network, vm: &[f64], va: &[f64]) -> Self {
        let base_mva = network.base_mva;
        let bus_map = network.bus_index_map();
        let branch_flow_mw: Vec<f64> = network
            .branches
            .iter()
            .map(|br| {
                if !br.in_service {
                    return 0.0;
                }
                let f = bus_map[&br.from_bus];
                let t = bus_map[&br.to_bus];
                let vi = vm[f];
                let vj = vm[t];
                let theta_ij = va[f] - va[t];
                br.power_flows_pu(vi, vj, theta_ij, 1e-40).p_from_pu * base_mva
            })
            .collect();
        let total_gen_mw = network
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.p)
            .sum();
        BaseCaseState {
            vm: vm.to_vec(),
            va: va.to_vec(),
            branch_flow_mw,
            total_gen_mw,
        }
    }
}

impl ArmCondition {
    /// Evaluate this arming condition against the base-case network and
    /// solved state.
    pub fn evaluate(&self, net: &Network, state: &BaseCaseState) -> bool {
        match self {
            ArmCondition::BranchLoading {
                branch_idx,
                threshold_pct,
            } => {
                let flow = state
                    .branch_flow_mw
                    .get(*branch_idx)
                    .copied()
                    .unwrap_or(0.0);
                let rating = net
                    .branches
                    .get(*branch_idx)
                    .map(|b| b.rating_a_mva)
                    .unwrap_or(f64::INFINITY);
                if rating <= 0.0 {
                    return false;
                }
                (flow.abs() / rating * 100.0) >= *threshold_pct
            }
            ArmCondition::VoltageLow {
                bus_idx,
                threshold_pu,
            } => state.vm.get(*bus_idx).copied().unwrap_or(1.0) < *threshold_pu,
            ArmCondition::VoltageHigh {
                bus_idx,
                threshold_pu,
            } => state.vm.get(*bus_idx).copied().unwrap_or(1.0) > *threshold_pu,
            ArmCondition::InterfaceFlow {
                branch_coefficients,
                threshold_mw,
                ..
            } => {
                let flow: f64 = branch_coefficients
                    .iter()
                    .map(|(bi, coeff)| {
                        coeff * state.branch_flow_mw.get(*bi).copied().unwrap_or(0.0)
                    })
                    .sum();
                flow.abs() >= *threshold_mw
            }
            ArmCondition::SystemGenerationAbove { threshold_mw } => {
                state.total_gen_mw >= *threshold_mw
            }
            ArmCondition::SystemGenerationBelow { threshold_mw } => {
                state.total_gen_mw <= *threshold_mw
            }
            ArmCondition::Not(c) => !c.evaluate(net, state),
            ArmCondition::All(cs) => cs.iter().all(|c| c.evaluate(net, state)),
            ArmCondition::Any(cs) => cs.iter().any(|c| c.evaluate(net, state)),
        }
    }
}

/// A condition that determines when a RAS fires (post-contingency evaluation).
///
/// Conditions can be combined with [`All`](RasTriggerCondition::All) (AND)
/// and [`Any`](RasTriggerCondition::Any) (OR) to express compound logic such
/// as "branch X is outaged AND post-contingency flow on branch Y > 95%".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RasTriggerCondition {
    /// Fire when a specific branch index appears in the outaged set.
    BranchOutaged { branch_idx: usize },
    /// Fire when the post-contingency loading on a branch exceeds a threshold.
    PostCtgBranchLoading {
        branch_idx: usize,
        /// Loading percentage threshold (e.g. 95.0 = 95 %).
        threshold_pct: f64,
    },
    /// Fire when the post-contingency voltage at a bus drops below the given threshold.
    PostCtgVoltageLow {
        bus_number: u32,
        /// Voltage magnitude threshold in p.u.
        threshold_pu: f64,
    },
    /// Fire when the post-contingency voltage at a bus exceeds the threshold.
    PostCtgVoltageHigh {
        bus_number: u32,
        /// Voltage magnitude threshold in p.u.
        threshold_pu: f64,
    },
    /// Fire when a named flowgate is overloaded above a threshold.
    PostCtgFlowgateOverload {
        flowgate_name: String,
        /// Loading percentage threshold (e.g. 100.0 = at limit).
        threshold_pct: f64,
    },
    /// Fire when a named interface is overloaded above a threshold.
    PostCtgInterfaceOverload {
        interface_name: String,
        /// Loading percentage threshold.
        threshold_pct: f64,
    },
    /// Negate a condition.
    Not(Box<RasTriggerCondition>),
    /// ALL nested conditions must be satisfied (logical AND).
    All(Vec<RasTriggerCondition>),
    /// ANY nested condition must be satisfied (logical OR).
    Any(Vec<RasTriggerCondition>),
}

impl RasTriggerCondition {
    /// Evaluate this condition against the outaged branch set and the
    /// pre-corrective post-contingency result.
    pub fn evaluate(&self, outaged: &[usize], result: &ContingencyResult) -> bool {
        match self {
            RasTriggerCondition::BranchOutaged { branch_idx } => outaged.contains(branch_idx),
            RasTriggerCondition::PostCtgBranchLoading {
                branch_idx,
                threshold_pct,
            } => result.violations.iter().any(|v| {
                matches!(
                    v,
                    Violation::ThermalOverload { branch_idx: bi, loading_pct: lp, .. }
                    if bi == branch_idx && *lp >= *threshold_pct
                )
            }),
            RasTriggerCondition::PostCtgVoltageLow {
                bus_number,
                threshold_pu,
            } => result.violations.iter().any(|v| {
                matches!(
                    v,
                    Violation::VoltageLow { bus_number: bn, vm, .. }
                    if bn == bus_number && *vm < *threshold_pu
                )
            }),
            RasTriggerCondition::PostCtgVoltageHigh {
                bus_number,
                threshold_pu,
            } => result.violations.iter().any(|v| {
                matches!(
                    v,
                    Violation::VoltageHigh { bus_number: bn, vm, .. }
                    if bn == bus_number && *vm > *threshold_pu
                )
            }),
            RasTriggerCondition::PostCtgFlowgateOverload {
                flowgate_name,
                threshold_pct,
            } => result.violations.iter().any(|v| {
                matches!(
                    v,
                    Violation::FlowgateOverload { name, loading_pct, .. }
                    if name == flowgate_name && *loading_pct >= *threshold_pct
                )
            }),
            RasTriggerCondition::PostCtgInterfaceOverload {
                interface_name,
                threshold_pct,
            } => result.violations.iter().any(|v| {
                matches!(
                    v,
                    Violation::InterfaceOverload { name, loading_pct, .. }
                    if name == interface_name && *loading_pct >= *threshold_pct
                )
            }),
            RasTriggerCondition::Not(cond) => !cond.evaluate(outaged, result),
            RasTriggerCondition::All(conds) => conds.iter().all(|c| c.evaluate(outaged, result)),
            RasTriggerCondition::Any(conds) => conds.iter().any(|c| c.evaluate(outaged, result)),
        }
    }
}
