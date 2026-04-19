// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Power flow solution representation.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use surge_network::Network;

/// Error returned when querying a [`PfSolution`].
#[derive(Debug, Error)]
pub enum SolutionError {
    #[error("branch count mismatch: solution has {solution} branches, network has {network}")]
    BranchCountMismatch { solution: usize, network: usize },
    #[error(
        "stored from-end branch flow vectors have mismatched lengths (p_from={p_from}, q_from={q_from})"
    )]
    BranchFlowVectorMismatch { p_from: usize, q_from: usize },
    #[error("stored to-end branch flow vectors have mismatched lengths (p_to={p_to}, q_to={q_to})")]
    BranchToEndFlowVectorMismatch { p_to: usize, q_to: usize },
}

// -----------------------------------------------------------------------
// Area interchange control results
// -----------------------------------------------------------------------

/// How a particular area's interchange mismatch was dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AreaDispatchMethod {
    /// Distributed via APF-weighted regulating generators across the area.
    Apf,
    /// Fell back to headroom-proportional dispatch at the area slack bus
    /// (no generators with `agc_participation_factor > 0` found in the area).
    SlackBusFallback,
    /// Area was within tolerance — no adjustment needed.
    Converged,
}

/// Per-area interchange enforcement result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaInterchangeEntry {
    /// Area number.
    pub area: u32,
    /// Net scheduled interchange in MW (p_desired_mw + bilateral transfers).
    pub scheduled_mw: f64,
    /// Actual net interchange in MW (sum of tie-line flows).
    pub actual_mw: f64,
    /// Mismatch: scheduled − actual (MW).
    pub error_mw: f64,
    /// How the mismatch was dispatched for this area.
    pub dispatch_method: AreaDispatchMethod,
}

/// Aggregate result of area interchange enforcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaInterchangeResult {
    /// Per-area results.
    pub areas: Vec<AreaInterchangeEntry>,
    /// Number of outer-loop iterations used.
    pub iterations: usize,
    /// True if all areas converged within tolerance.
    pub converged: bool,
}

/// Status of a power flow solve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SolveStatus {
    /// Converged to a solution within tolerance.
    Converged,
    /// Maximum iterations reached without convergence.
    MaxIterations,
    /// Diverged (numerical instability).
    Diverged,
    /// Not yet solved.
    #[default]
    Unsolved,
}

/// Shared power-flow model family for a solved state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PfModel {
    /// AC power flow or any nonlinear/complex-voltage formulation.
    #[default]
    Ac,
    /// DC B-theta power flow.
    Dc,
}

/// Result of a power flow computation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PfSolution {
    /// Physical model family used to produce this solved state.
    pub pf_model: PfModel,
    /// Solve status.
    pub status: SolveStatus,
    /// Number of iterations taken.
    pub iterations: u32,
    /// Maximum power mismatch at convergence (per-unit).
    #[serde(
        serialize_with = "serialize_max_mismatch",
        deserialize_with = "deserialize_max_mismatch"
    )]
    pub max_mismatch: f64,
    /// Solve time in seconds.
    pub solve_time_secs: f64,
    /// Bus voltage magnitudes in per-unit (indexed by internal bus order).
    pub voltage_magnitude_pu: Vec<f64>,
    /// Bus voltage angles in radians (indexed by internal bus order).
    pub voltage_angle_rad: Vec<f64>,
    /// Real power injection at each bus in per-unit.
    pub active_power_injection_pu: Vec<f64>,
    /// Reactive power injection at each bus in per-unit.
    pub reactive_power_injection_pu: Vec<f64>,
    /// Real power flow at each branch from-end in MW.
    pub branch_p_from_mw: Vec<f64>,
    /// Real power flow at each branch to-end in MW.
    pub branch_p_to_mw: Vec<f64>,
    /// Reactive power flow at each branch from-end in MVAr.
    pub branch_q_from_mvar: Vec<f64>,
    /// Reactive power flow at each branch to-end in MVAr.
    pub branch_q_to_mvar: Vec<f64>,
    /// External bus numbers (for joining with reference solutions).
    pub bus_numbers: Vec<u32>,
    /// Island membership for each bus (0-indexed island ID, per internal bus order).
    ///
    /// Empty when island detection was not performed. When populated, `island_ids[i]`
    /// gives the connected-component index for internal bus `i`. Isolated buses
    /// (degree 0) form their own single-bus island.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub island_ids: Vec<usize>,

    // -----------------------------------------------------------------------
    // AC-02: Q-limit PV→PQ bus switching metadata
    // -----------------------------------------------------------------------
    /// External bus numbers of all buses that were switched from PV to PQ by
    /// reactive power limit enforcement during this solve.
    ///
    /// Empty when the underlying AC solver did not enforce Q limits or no limits
    /// were violated. For DC solves this is always empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub q_limited_buses: Vec<u32>,

    /// Total number of bus type switches (PV→PQ or PQ→PV) performed by
    /// Q-limit enforcement across all outer NR iterations.
    ///
    /// Zero when no limits were enforced. For DC solves this is always zero.
    #[serde(default)]
    pub n_q_limit_switches: u32,

    // -----------------------------------------------------------------------
    // AC-03: Distributed slack contribution metadata
    // -----------------------------------------------------------------------
    /// Active power contribution of each generator to the slack balancing
    /// redistribution, in MW (indexed by `network.generators` order).
    ///
    /// Non-zero only when the originating AC solver used distributed slack.
    /// When multiple generators share a participating bus, the bus-level slack
    /// share is split per the solver's slack-attribution policy instead of
    /// being duplicated onto every generator at that bus.
    /// For DC solves this is empty by construction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gen_slack_contribution_mw: Vec<f64>,

    // -----------------------------------------------------------------------
    // Convergence diagnostics
    // -----------------------------------------------------------------------
    /// Per-iteration convergence history: `(iteration_number, max_mismatch_pu)`.
    ///
    /// Empty unless the originating solver recorded it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convergence_history: Vec<(u32, f64)>,

    /// External bus number with the largest power mismatch on the final solve
    /// iteration. `None` on successful convergence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worst_mismatch_bus: Option<u32>,

    // -----------------------------------------------------------------------
    // Area interchange enforcement results
    // -----------------------------------------------------------------------
    /// Area interchange enforcement results. `None` when interchange
    /// enforcement was not active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_interchange: Option<AreaInterchangeResult>,
}

impl PfSolution {
    fn validate_from_end_branch_vectors(&self) -> Result<(), SolutionError> {
        if self.branch_p_from_mw.len() != self.branch_q_from_mvar.len() {
            return Err(SolutionError::BranchFlowVectorMismatch {
                p_from: self.branch_p_from_mw.len(),
                q_from: self.branch_q_from_mvar.len(),
            });
        }
        Ok(())
    }

    /// Create a diverged solution with flat voltage profile (Vm=1.0, Va=0.0).
    pub fn diverged(n_buses: usize, n_branches: usize, pf_model: PfModel) -> Self {
        Self {
            pf_model,
            status: SolveStatus::Diverged,
            voltage_magnitude_pu: vec![1.0; n_buses],
            voltage_angle_rad: vec![0.0; n_buses],
            active_power_injection_pu: vec![0.0; n_buses],
            reactive_power_injection_pu: vec![0.0; n_buses],
            branch_p_from_mw: vec![0.0; n_branches],
            branch_p_to_mw: vec![0.0; n_branches],
            branch_q_from_mvar: vec![0.0; n_branches],
            branch_q_to_mvar: vec![0.0; n_branches],
            max_mismatch: f64::INFINITY,
            iterations: 0,
            solve_time_secs: 0.0,
            bus_numbers: Vec::new(),
            island_ids: Vec::new(),
            q_limited_buses: Vec::new(),
            n_q_limit_switches: 0,
            gen_slack_contribution_mw: Vec::new(),
            convergence_history: Vec::new(),
            worst_mismatch_bus: None,
            area_interchange: None,
        }
    }

    /// Create an unsolved solution with flat voltage profile (Vm=1.0, Va=0.0).
    pub fn flat_start(n_buses: usize, n_branches: usize, pf_model: PfModel) -> Self {
        let mut sol = Self::diverged(n_buses, n_branches, pf_model);
        sol.status = SolveStatus::Unsolved;
        sol.max_mismatch = 0.0;
        sol
    }

    /// Number of distinct islands detected in this solve.
    pub fn n_islands(&self) -> usize {
        self.island_ids
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len()
    }

    /// Estimate per-generator reactive power output (MVAr) in `network.generators` order.
    ///
    /// This reconstructs generator reactive dispatch from the solved bus-level
    /// reactive injections, fixed bus shunts, and static bus load withdrawals.
    /// For buses with multiple in-service generators, the bus-level reactive
    /// output is apportioned by reactive capability range `(qmax - qmin)` when
    /// available; otherwise it is split evenly across the in-service units at
    /// that bus.
    pub fn generator_reactive_power_mvar(&self, network: &Network) -> Vec<f64> {
        let base = network.base_mva;
        let bus_map = network.bus_index_map();
        let load_q = network.bus_load_q_mvar_with_map(&bus_map);

        let mut bus_qg: HashMap<u32, f64> = HashMap::new();
        for (bus_idx, bus) in network.buses.iter().enumerate() {
            let Some(&solution_bus_idx) = bus_map.get(&bus.number) else {
                continue;
            };
            if solution_bus_idx >= self.reactive_power_injection_pu.len()
                || solution_bus_idx >= self.voltage_magnitude_pu.len()
            {
                continue;
            }
            let vm = self.voltage_magnitude_pu[solution_bus_idx];
            let qd = load_q.get(bus_idx).copied().unwrap_or(0.0);
            let qg_bus = self.reactive_power_injection_pu[solution_bus_idx] * base + qd
                - bus.shunt_susceptance_mvar * vm * vm;
            bus_qg.insert(bus.number, qg_bus);
        }

        let mut bus_range: HashMap<u32, f64> = HashMap::new();
        let mut bus_count: HashMap<u32, usize> = HashMap::new();
        for generator in network
            .generators
            .iter()
            .filter(|generator| generator.in_service)
        {
            *bus_range.entry(generator.bus).or_insert(0.0) +=
                (generator.qmax - generator.qmin).max(0.0);
            *bus_count.entry(generator.bus).or_insert(0) += 1;
        }

        network
            .generators
            .iter()
            .map(|generator| {
                if !generator.in_service {
                    return 0.0;
                }
                let total_qg = bus_qg.get(&generator.bus).copied().unwrap_or(0.0);
                let total_range = bus_range.get(&generator.bus).copied().unwrap_or(0.0);
                let generator_range = (generator.qmax - generator.qmin).max(0.0);
                if total_range > 1e-6 {
                    total_qg * generator_range / total_range
                } else {
                    let units_at_bus = bus_count.get(&generator.bus).copied().unwrap_or(1).max(1);
                    total_qg / units_at_bus as f64
                }
            })
            .collect()
    }

    /// Compute apparent power flow |S_ij| on each branch in MVA.
    ///
    /// Uses the stored from-end branch P/Q values (MW/MVAr).
    pub fn branch_apparent_power(&self) -> Vec<f64> {
        self.validate_from_end_branch_vectors()
            .expect("stored from-end branch flow vectors must have matching lengths");
        self.branch_p_from_mw
            .iter()
            .zip(self.branch_q_from_mvar.iter())
            .map(|(&p, &q)| (p * p + q * q).sqrt())
            .collect()
    }

    /// Compute branch loading as percentage of Rate A for each branch.
    ///
    /// Returns `max(|S_from|, |S_to|) / rate_a * 100`. Branches with
    /// `rate_a <= 0` return 0.0.
    pub fn branch_loading_pct(&self, network: &Network) -> Result<Vec<f64>, SolutionError> {
        if self.branch_p_from_mw.len() != network.branches.len() {
            return Err(SolutionError::BranchCountMismatch {
                solution: self.branch_p_from_mw.len(),
                network: network.branches.len(),
            });
        }
        if self.branch_p_to_mw.len() != network.branches.len() {
            return Err(SolutionError::BranchCountMismatch {
                solution: self.branch_p_to_mw.len(),
                network: network.branches.len(),
            });
        }
        self.validate_from_end_branch_vectors()?;
        if self.branch_p_to_mw.len() != self.branch_q_to_mvar.len() {
            return Err(SolutionError::BranchToEndFlowVectorMismatch {
                p_to: self.branch_p_to_mw.len(),
                q_to: self.branch_q_to_mvar.len(),
            });
        }
        Ok(self
            .branch_p_from_mw
            .iter()
            .zip(self.branch_q_from_mvar.iter())
            .zip(self.branch_p_to_mw.iter().zip(self.branch_q_to_mvar.iter()))
            .zip(network.branches.iter())
            .map(|(((p_from, q_from), (p_to, q_to)), branch)| {
                let from_s = (*p_from * *p_from + *q_from * *q_from).sqrt();
                let to_s = (*p_to * *p_to + *q_to * *q_to).sqrt();
                let flow = from_s.max(to_s);
                if branch.rating_a_mva > 0.0 {
                    flow / branch.rating_a_mva * 100.0
                } else {
                    0.0
                }
            })
            .collect())
    }

    /// Return the stored from-end real and reactive power flows in MW/MVAr.
    pub fn branch_pq_flows(&self) -> Vec<(f64, f64)> {
        self.validate_from_end_branch_vectors()
            .expect("stored from-end branch flow vectors must have matching lengths");
        self.branch_p_from_mw
            .iter()
            .zip(self.branch_q_from_mvar.iter())
            .map(|(&p, &q)| (p, q))
            .collect()
    }
}

fn serialize_max_mismatch<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.is_finite() {
        serializer.serialize_some(value)
    } else {
        serializer.serialize_none()
    }
}

fn deserialize_max_mismatch<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(f64::INFINITY))
}

/// Compute exact branch power flows from a solved bus voltage state.
///
/// Returns `(pf, pt, qf, qt)` in branch order (MW / MVAr), where:
/// - `pf[i]` / `qf[i]` are the from-end flows for branch `i`
/// - `pt[i]` / `qt[i]` are the to-end flows for branch `i`
pub fn compute_branch_power_flows(
    network: &Network,
    voltage_magnitude_pu: &[f64],
    voltage_angle_rad: &[f64],
    base_mva: f64,
) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
    assert_eq!(
        voltage_magnitude_pu.len(),
        network.n_buses(),
        "voltage_magnitude_pu has {} entries, expected {}",
        voltage_magnitude_pu.len(),
        network.n_buses()
    );
    assert_eq!(
        voltage_angle_rad.len(),
        network.n_buses(),
        "voltage_angle_rad has {} entries, expected {}",
        voltage_angle_rad.len(),
        network.n_buses()
    );

    let n_branches = network.n_branches();
    let bus_map = network.bus_index_map();
    let mut pf = vec![0.0; n_branches];
    let mut pt = vec![0.0; n_branches];
    let mut qf = vec![0.0; n_branches];
    let mut qt = vec![0.0; n_branches];

    for (branch_idx, branch) in network.branches.iter().enumerate() {
        if !branch.in_service {
            continue;
        }

        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];

        let vf = voltage_magnitude_pu[from_idx];
        let vt = voltage_magnitude_pu[to_idx];
        let theta_ft = voltage_angle_rad[from_idx] - voltage_angle_rad[to_idx];
        let flows = branch.power_flows_pu(vf, vt, theta_ft, 1e-40);

        pf[branch_idx] = flows.p_from_pu * base_mva;
        qf[branch_idx] = flows.q_from_pu * base_mva;
        pt[branch_idx] = flows.p_to_pu * base_mva;
        qt[branch_idx] = flows.q_to_pu * base_mva;
    }

    (pf, pt, qf, qt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use surge_network::network::branch::Branch;
    use surge_network::network::bus::{Bus, BusType};
    use surge_network::network::generator::Generator;
    use surge_network::network::load::Load;

    fn two_bus_network(rating_a: f64) -> Network {
        let mut net = Network::new("test");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        let mut br = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        br.rating_a_mva = rating_a;
        net.branches.push(br);
        net
    }

    fn two_bus_solution(
        pf_model: PfModel,
        p_from_mw: f64,
        q_from_mvar: f64,
        p_to_mw: f64,
        q_to_mvar: f64,
    ) -> PfSolution {
        PfSolution {
            pf_model,
            status: SolveStatus::Converged,
            voltage_magnitude_pu: vec![1.0, 1.0],
            voltage_angle_rad: vec![0.0, 0.0],
            active_power_injection_pu: vec![0.0, 0.0],
            reactive_power_injection_pu: vec![0.0, 0.0],
            branch_p_from_mw: vec![p_from_mw],
            branch_p_to_mw: vec![p_to_mw],
            branch_q_from_mvar: vec![q_from_mvar],
            branch_q_to_mvar: vec![q_to_mvar],
            ..Default::default()
        }
    }

    fn two_bus_generator_network() -> Network {
        let mut net = two_bus_network(100.0);
        let mut gen_a = Generator::with_id("g1", 1, 50.0, 1.0);
        gen_a.qmin = -10.0;
        gen_a.qmax = 20.0;
        let mut gen_b = Generator::with_id("g2", 1, 25.0, 1.0);
        gen_b.qmin = -5.0;
        gen_b.qmax = 5.0;
        net.generators.push(gen_a);
        net.generators.push(gen_b);
        net.loads.push(Load::new(2, 60.0, 15.0));
        net
    }

    #[test]
    fn test_branch_apparent_power_uses_stored_values() {
        // 60 MW, 80 MVAr → |S| = 100 MVA
        let sol = two_bus_solution(PfModel::Ac, 60.0, 80.0, -58.0, -77.0);
        let flows = sol.branch_apparent_power();
        assert_eq!(flows.len(), 1);
        assert!((flows[0] - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_branch_loading_pct_zero_rating() {
        let net = two_bus_network(0.0);
        let sol = two_bus_solution(PfModel::Ac, 50.0, 0.0, -50.0, 0.0);
        let loading = sol.branch_loading_pct(&net).unwrap();
        assert_eq!(loading, vec![0.0]);
    }

    #[test]
    fn test_branch_loading_pct_normal_operation() {
        let net = two_bus_network(200.0);
        // 60 MW, 80 MVAr → |S| = 100 MVA → 50% of 200 MVA rating
        let sol = two_bus_solution(PfModel::Ac, 60.0, 80.0, -58.0, -77.0);
        let loading = sol.branch_loading_pct(&net).unwrap();
        assert_eq!(loading.len(), 1);
        assert!((loading[0] - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_branch_loading_pct_uses_larger_end() {
        let net = two_bus_network(200.0);
        let sol = two_bus_solution(PfModel::Ac, 30.0, 40.0, 80.0, 60.0);
        let loading = sol.branch_loading_pct(&net).unwrap();
        assert_eq!(loading.len(), 1);
        assert!((loading[0] - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_branch_pq_flows_returns_stored_from_end_values() {
        let sol = two_bus_solution(PfModel::Dc, 42.0, 0.0, -42.0, 0.0);
        let pq = sol.branch_pq_flows();
        assert_eq!(pq, vec![(42.0, 0.0)]);
    }

    #[test]
    fn test_branch_loading_mismatch_returns_error() {
        let net = two_bus_network(100.0);
        let sol = PfSolution {
            branch_p_from_mw: vec![],
            ..Default::default()
        };
        assert!(sol.branch_loading_pct(&net).is_err());
    }

    #[test]
    #[should_panic(expected = "stored from-end branch flow vectors must have matching lengths")]
    fn test_branch_apparent_power_panics_on_mismatched_vectors() {
        let sol = PfSolution {
            branch_p_from_mw: vec![10.0],
            branch_q_from_mvar: vec![],
            ..Default::default()
        };
        let _ = sol.branch_apparent_power();
    }

    #[test]
    #[should_panic(expected = "stored from-end branch flow vectors must have matching lengths")]
    fn test_branch_pq_flows_panics_on_mismatched_vectors() {
        let sol = PfSolution {
            branch_p_from_mw: vec![10.0],
            branch_q_from_mvar: vec![],
            ..Default::default()
        };
        let _ = sol.branch_pq_flows();
    }

    #[test]
    fn test_n_islands_empty() {
        let sol = PfSolution::default();
        assert_eq!(sol.n_islands(), 0);
    }

    #[test]
    fn test_n_islands_single() {
        let sol = PfSolution {
            island_ids: vec![0, 0, 0],
            ..Default::default()
        };
        assert_eq!(sol.n_islands(), 1);
    }

    #[test]
    fn test_n_islands_multiple() {
        let sol = PfSolution {
            island_ids: vec![0, 1, 2, 0, 1],
            ..Default::default()
        };
        assert_eq!(sol.n_islands(), 3);
    }

    #[test]
    fn test_n_islands_non_dense_labels() {
        let sol = PfSolution {
            island_ids: vec![2, 4, 2, 9],
            ..Default::default()
        };
        assert_eq!(sol.n_islands(), 3);
    }

    #[test]
    fn test_n_islands_single_bus() {
        let sol = PfSolution {
            island_ids: vec![0],
            ..Default::default()
        };
        assert_eq!(sol.n_islands(), 1);
    }

    #[test]
    fn test_diverged_creates_flat_profile() {
        let sol = PfSolution::diverged(3, 2, PfModel::Ac);
        assert_eq!(sol.pf_model, PfModel::Ac);
        assert_eq!(sol.status, SolveStatus::Diverged);
        assert_eq!(sol.voltage_magnitude_pu, vec![1.0, 1.0, 1.0]);
        assert_eq!(sol.voltage_angle_rad, vec![0.0, 0.0, 0.0]);
        assert_eq!(sol.active_power_injection_pu, vec![0.0, 0.0, 0.0]);
        assert_eq!(sol.reactive_power_injection_pu, vec![0.0, 0.0, 0.0]);
        assert_eq!(sol.branch_p_from_mw, vec![0.0, 0.0]);
        assert_eq!(sol.branch_q_from_mvar, vec![0.0, 0.0]);
        assert_eq!(sol.max_mismatch, f64::INFINITY);
        assert_eq!(sol.iterations, 0);
    }

    #[test]
    fn test_diverged_serializes_max_mismatch_as_null() {
        let sol = PfSolution::diverged(1, 0, PfModel::Ac);
        let json = serde_json::to_value(&sol).unwrap();
        assert_eq!(json.get("max_mismatch"), Some(&Value::Null));

        let roundtrip: PfSolution = serde_json::from_value(json).unwrap();
        assert!(roundtrip.max_mismatch.is_infinite());
    }

    #[test]
    fn test_flat_start_creates_unsolved_profile() {
        let sol = PfSolution::flat_start(2, 1, PfModel::Dc);
        assert_eq!(sol.pf_model, PfModel::Dc);
        assert_eq!(sol.status, SolveStatus::Unsolved);
        assert_eq!(sol.voltage_magnitude_pu, vec![1.0, 1.0]);
        assert_eq!(sol.voltage_angle_rad, vec![0.0, 0.0]);
        assert_eq!(sol.branch_p_from_mw, vec![0.0]);
        assert_eq!(sol.max_mismatch, 0.0);
    }

    #[test]
    fn test_generator_reactive_power_mvar_apportions_by_capability() {
        let net = two_bus_generator_network();
        let sol = PfSolution {
            status: SolveStatus::Converged,
            voltage_magnitude_pu: vec![1.0, 1.0],
            voltage_angle_rad: vec![0.0, 0.0],
            active_power_injection_pu: vec![0.0, 0.0],
            reactive_power_injection_pu: vec![0.40, -0.15],
            bus_numbers: vec![1, 2],
            ..Default::default()
        };

        let qg = sol.generator_reactive_power_mvar(&net);
        assert_eq!(qg.len(), 2);
        assert!(
            (qg[0] - 30.0).abs() < 1e-9,
            "first generator should absorb 30 MVAr-equivalent share"
        );
        assert!(
            (qg[1] - 10.0).abs() < 1e-9,
            "second generator should absorb 10 MVAr-equivalent share"
        );
    }

    #[test]
    fn test_generator_reactive_power_mvar_splits_evenly_without_capability_range() {
        let mut net = two_bus_network(100.0);
        let mut gen_a = Generator::with_id("g1", 1, 50.0, 1.0);
        gen_a.qmin = 0.0;
        gen_a.qmax = 0.0;
        let mut gen_b = Generator::with_id("g2", 1, 25.0, 1.0);
        gen_b.qmin = 0.0;
        gen_b.qmax = 0.0;
        net.generators.push(gen_a);
        net.generators.push(gen_b);
        let sol = PfSolution {
            status: SolveStatus::Converged,
            voltage_magnitude_pu: vec![1.0, 1.0],
            voltage_angle_rad: vec![0.0, 0.0],
            active_power_injection_pu: vec![0.0, 0.0],
            reactive_power_injection_pu: vec![0.12, 0.0],
            bus_numbers: vec![1, 2],
            ..Default::default()
        };

        let qg = sol.generator_reactive_power_mvar(&net);
        assert_eq!(qg, vec![6.0, 6.0]);
    }

    #[test]
    fn test_generator_reactive_power_mvar_accounts_for_bus_shunt_without_extra_base_factor() {
        let mut net = two_bus_network(100.0);
        net.buses[0].shunt_susceptance_mvar = -100.0;
        net.generators.push(Generator::with_id("g1", 1, 50.0, 1.0));
        let sol = PfSolution {
            status: SolveStatus::Converged,
            voltage_magnitude_pu: vec![1.0, 1.0],
            voltage_angle_rad: vec![0.0, 0.0],
            active_power_injection_pu: vec![0.0, 0.0],
            reactive_power_injection_pu: vec![0.0, 0.0],
            bus_numbers: vec![1, 2],
            ..Default::default()
        };

        let qg = sol.generator_reactive_power_mvar(&net);
        assert_eq!(qg, vec![100.0]);
    }

    #[test]
    fn test_convergence_history_length_matches_iterations() {
        let sol = PfSolution {
            status: SolveStatus::Converged,
            iterations: 4,
            convergence_history: vec![(0, 1.0), (1, 0.5), (2, 0.1), (3, 0.01), (4, 1e-8)],
            ..Default::default()
        };
        assert_eq!(sol.convergence_history.len(), (sol.iterations + 1) as usize);
    }

    #[test]
    fn test_worst_mismatch_bus_populated_on_divergence() {
        let sol = PfSolution {
            status: SolveStatus::Diverged,
            worst_mismatch_bus: Some(42),
            max_mismatch: 1e3,
            ..Default::default()
        };
        assert_eq!(sol.worst_mismatch_bus, Some(42));
        assert_eq!(sol.status, SolveStatus::Diverged);
    }

    #[test]
    fn test_worst_mismatch_bus_none_on_convergence() {
        let sol = PfSolution {
            status: SolveStatus::Converged,
            worst_mismatch_bus: None,
            max_mismatch: 1e-10,
            ..Default::default()
        };
        assert!(sol.worst_mismatch_bus.is_none());
    }
}
