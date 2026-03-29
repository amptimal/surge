// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CTG-07 / P5-008 — Relay Cascade Simulation.
//!
//! After an N-1 contingency causes thermal overloads, this module simulates
//! which protective relays trip sympathetically (cascade), potentially causing
//! additional overloads and further trips.
//!
//! ## Algorithm
//!
//! 1. Start with N-1 contingency result (overloaded branches identified).
//! 2. For each overloaded branch, check Zone 3 relay pickup (slowest: 0.8–1.5 s delay).
//!    - Zone 3 picks up when: `|flow_mw| > Z3_pickup_fraction × rate_a_mw`
//!    - Zone 3 trips after `z3_delay_s` seconds.
//! 3. Apply the Zone 3 trip: mark branch as out-of-service and redistribute flows
//!    with a first-order DC LODF approximation.
//!    - `flow_j_new = flow_j_pre + LODF[j, tripped_branch] × flow_j_pre_tripped`
//! 4. Check for new overloads. If new overloads exist, repeat from step 2.
//! 5. Stop when: no new trips, or `max_cascade_levels` reached, or `blackout_fraction`
//!    of load is interrupted.
//! 6. Record the cascade sequence: which branch tripped at each level, cascade depth.
//!
//! This model is a cascade-screening approximation. It reuses prepared single-outage
//! LODF columns across cascade levels rather than rebuilding the post-topology DC model
//! after every trip.

use serde::{Deserialize, Serialize};
use surge_dc::PreparedDcStudy;
use surge_network::Network;
use thiserror::Error;

use crate::{ThermalRating, get_rating};

// ── Options ──────────────────────────────────────────────────────────────────

/// Options controlling Zone-3 relay cascade simulation behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeOptions {
    /// Zone 3 pickup fraction (per-unit of rate_a).
    ///
    /// A branch trips when `|flow_mw| > z3_pickup_fraction × rate_a_mw`.
    /// Default: 0.8 (80 % of thermal rating).
    pub z3_pickup_fraction: f64,

    /// Zone 3 trip time delay in seconds.
    ///
    /// Default: 1.0 s (representative Zone 3 clearing time).
    pub z3_delay_s: f64,

    /// Maximum cascade levels before forcibly stopping.
    ///
    /// Prevents infinite loops on pathological networks.  Default: 5.
    pub max_cascade_levels: u32,

    /// Stop if this fraction of total system load is interrupted.
    ///
    /// Default: 0.5 (50 %).  Set to 1.0 to disable the early-stop.
    pub blackout_fraction: f64,

    /// Thermal rating tier for relay pickup checks.
    ///
    /// NERC TPL-001 allows emergency ratings (Rate B or C) for post-contingency
    /// thermal checks.  Default: `RateA` (long-term continuous rating).
    #[serde(default)]
    pub thermal_rating: ThermalRating,
}

impl Default for CascadeOptions {
    fn default() -> Self {
        Self {
            z3_pickup_fraction: 0.8,
            z3_delay_s: 1.0,
            max_cascade_levels: 5,
            blackout_fraction: 0.5,
            thermal_rating: ThermalRating::default(),
        }
    }
}

// ── Cause enum ────────────────────────────────────────────────────────────────

/// Why a branch was removed from service during the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CascadeCause {
    /// The initiating N-1 event (fault, forced outage, etc.).
    Initial,
    /// Zone 3 distance relay picked up due to thermal overload redistribution.
    Zone3Relay,
    /// Zone 2 distance relay picked up (faster — reserved for future use).
    Zone2Relay,
}

// ── Event ─────────────────────────────────────────────────────────────────────

/// One trip event in the cascade sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeEvent {
    /// Cascade level at which this trip occurred (0 = initiating event).
    pub cascade_level: u32,

    /// Internal (0-based) index of the branch that tripped.
    pub tripped_branch_index: usize,

    /// Human-readable label for the tripped branch (`"from_bus->to_bus"`).
    pub branch_label: String,

    /// Why the branch was removed.
    pub cause: CascadeCause,

    /// Branch flow in MW immediately before the trip.
    pub flow_before_trip_mw: f64,

    /// Thermal rating of the tripped branch in MW.
    pub rating_mw: f64,

    /// Simulation time (seconds from t=0) when the trip occurred.
    pub time_s: f64,
}

// ── Result ────────────────────────────────────────────────────────────────────

/// Full cascade simulation result for one initiating contingency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeResult {
    /// Internal index of the branch that initiated the cascade.
    pub initiating_contingency: usize,

    /// Ordered list of branch trips from first (level 0) to last.
    pub cascade_events: Vec<CascadeEvent>,

    /// Total load interrupted in MW (sum of `pd` at buses that became isolated).
    pub total_load_interrupted_mw: f64,

    /// Depth of the cascade (number of levels beyond the initiating event).
    pub cascade_depth: u32,

    /// `true` when `total_load_interrupted_mw / total_load_mw >= blackout_fraction`.
    pub blackout: bool,
}

/// Errors returned by relay-cascade screening and simulation.
#[derive(Debug, Error)]
pub enum RelayCascadeError {
    /// The network has no branches to analyze.
    #[error("Network has no branches")]
    NoBranches,
    /// The initiating outage index is out of range.
    #[error("initiating_branch contains invalid branch index {0}")]
    InvalidBranchIndex(usize),
    /// Base-case DC preparation or solve failed.
    #[error("DC power flow failed: {0}")]
    DcFlowFailed(String),
}

/// Prepared relay-cascade model for repeated large-system cascade studies.
///
/// This caches the base-case DC flows and the prepared DC sensitivity model so
/// callers can run many initiating outages without materializing a dense
/// all-pairs LODF matrix.
pub struct PreparedCascadeModel<'a> {
    network: &'a Network,
    base_flows_mw: Vec<f64>,
    all_branches: Vec<usize>,
    dc_model: PreparedDcStudy<'a>,
}

// ── Core simulation ───────────────────────────────────────────────────────────

impl<'a> PreparedCascadeModel<'a> {
    /// Prepare a relay-cascade model for repeated simulations on one network.
    pub fn new(network: &'a Network) -> Result<Self, RelayCascadeError> {
        let n_br = network.n_branches();
        if n_br == 0 {
            return Err(RelayCascadeError::NoBranches);
        }

        let mut dc_model = PreparedDcStudy::new(network)
            .map_err(|e| RelayCascadeError::DcFlowFailed(e.to_string()))?;
        let dc_result = dc_model
            .solve(&surge_dc::DcPfOptions::default())
            .map_err(|e| RelayCascadeError::DcFlowFailed(e.to_string()))?;
        let base_flows_mw = dc_result
            .branch_p_flow
            .iter()
            .map(|&f| f * network.base_mva)
            .collect();
        let all_branches = (0..n_br).collect();

        Ok(Self {
            network,
            base_flows_mw,
            all_branches,
            dc_model,
        })
    }

    /// Simulate one initiating outage using the cached prepared DC model.
    pub fn simulate(
        &mut self,
        initiating_branch: usize,
        options: &CascadeOptions,
    ) -> Result<CascadeResult, RelayCascadeError> {
        if initiating_branch >= self.network.n_branches() {
            return Err(RelayCascadeError::InvalidBranchIndex(initiating_branch));
        }

        let monitored_branches = &self.all_branches;
        let mut lodf_columns = self.dc_model.lodf_columns();
        simulate_cascade_with_column_provider(
            self.network,
            &self.base_flows_mw,
            initiating_branch,
            options,
            |tripped| {
                lodf_columns
                    .compute_column(monitored_branches, tripped)
                    .map_err(|e| RelayCascadeError::DcFlowFailed(e.to_string()))
            },
        )
    }

    /// Run relay-cascade screening for every in-service branch with a valid rating.
    pub fn run_all(
        &mut self,
        options: &CascadeOptions,
    ) -> Result<Vec<CascadeResult>, RelayCascadeError> {
        let mut lodf_columns = self.dc_model.lodf_columns();
        let monitored_branches = &self.all_branches;

        let mut results: Vec<CascadeResult> = (0..self.network.n_branches())
            .filter(|&k| {
                let b = &self.network.branches[k];
                b.in_service && get_rating(b, options.thermal_rating) > 0.0
            })
            .map(|initiating_branch| {
                simulate_cascade_with_column_provider(
                    self.network,
                    &self.base_flows_mw,
                    initiating_branch,
                    options,
                    |tripped| {
                        lodf_columns
                            .compute_column(monitored_branches, tripped)
                            .map_err(|e| RelayCascadeError::DcFlowFailed(e.to_string()))
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Most severe cascades first.
        results.sort_by(|a, b| {
            b.cascade_depth.cmp(&a.cascade_depth).then_with(|| {
                b.total_load_interrupted_mw
                    .partial_cmp(&a.total_load_interrupted_mw)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

        Ok(results)
    }
}

/// Simulate Zone-3 relay cascade following an N-1 initiating event.
///
/// # Arguments
/// * `network`           – Power system network (ratings, load, connectivity).
/// * `initiating_branch` – Index of the N-1 outaged branch.
/// * `options`           – Cascade behaviour options.
///
/// # Returns
/// A [`CascadeResult`] describing the full cascade sequence.
pub fn simulate_cascade(
    network: &Network,
    initiating_branch: usize,
    options: &CascadeOptions,
) -> Result<CascadeResult, RelayCascadeError> {
    let mut model = PreparedCascadeModel::new(network)?;
    model.simulate(initiating_branch, options)
}

fn simulate_cascade_with_column_provider<F>(
    network: &Network,
    base_flows_mw: &[f64],
    initiating_branch: usize,
    options: &CascadeOptions,
    mut outage_column: F,
) -> Result<CascadeResult, RelayCascadeError>
where
    F: FnMut(usize) -> Result<Vec<f64>, RelayCascadeError>,
{
    let n_br = network.n_branches();
    if n_br == 0 {
        return Err(RelayCascadeError::NoBranches);
    }
    if initiating_branch >= n_br {
        return Err(RelayCascadeError::InvalidBranchIndex(initiating_branch));
    }
    let total_load_mw = network.total_load_mw();

    // Current flows — modified in-place as branches trip.
    let mut current_flows_mw: Vec<f64> = base_flows_mw.to_vec();

    // Which branches are currently out of service.
    let mut out_of_service: Vec<bool> = vec![false; n_br];

    // Simulation clock.
    let mut sim_time_s: f64 = 0.0;
    let mut cascade_events: Vec<CascadeEvent> = Vec::new();

    // ── Level 0: initiating N-1 event ────────────────────────────────────────
    let init_flow = current_flows_mw[initiating_branch];
    let init_rating = get_rating(&network.branches[initiating_branch], options.thermal_rating);

    cascade_events.push(CascadeEvent {
        cascade_level: 0,
        tripped_branch_index: initiating_branch,
        branch_label: branch_label(network, initiating_branch),
        cause: CascadeCause::Initial,
        flow_before_trip_mw: init_flow,
        rating_mw: init_rating,
        time_s: sim_time_s,
    });

    // Apply initiating outage: redistribute flows via LODF.
    apply_outage(
        &mut current_flows_mw,
        &mut out_of_service,
        initiating_branch,
        &mut outage_column,
    )?;

    // ── Cascade levels ────────────────────────────────────────────────────────
    let mut cascade_depth: u32 = 0;
    let mut load_interrupted_mw: f64 = 0.0;

    for level in 1..=options.max_cascade_levels {
        // Advance simulation clock by one Zone-3 delay step.
        sim_time_s += options.z3_delay_s;

        // Find all in-service branches whose loading exceeds Zone 3 pickup.
        let new_trips: Vec<usize> = (0..n_br)
            .filter(|&j| {
                if out_of_service[j] {
                    return false;
                }
                let branch = &network.branches[j];
                let rating = get_rating(branch, options.thermal_rating);
                if !branch.in_service || rating <= 0.0 {
                    return false;
                }
                current_flows_mw[j].abs() > options.z3_pickup_fraction * rating
            })
            .collect();

        if new_trips.is_empty() {
            break;
        }

        let flows_before_level = current_flows_mw.clone();

        // Record all relay trips from the pre-trip state at this cascade level.
        for &tripped in &new_trips {
            let flow_before = flows_before_level[tripped];
            let rating = get_rating(&network.branches[tripped], options.thermal_rating);

            cascade_events.push(CascadeEvent {
                cascade_level: level,
                tripped_branch_index: tripped,
                branch_label: branch_label(network, tripped),
                cause: CascadeCause::Zone3Relay,
                flow_before_trip_mw: flow_before,
                rating_mw: rating,
                time_s: sim_time_s,
            });
        }
        apply_outages_simultaneously(
            &mut current_flows_mw,
            &mut out_of_service,
            &new_trips,
            &flows_before_level,
            &mut outage_column,
        )?;

        cascade_depth = level;

        // Estimate load interrupted by this cascade level.
        load_interrupted_mw = estimate_load_interrupted(network, &out_of_service);

        // Check blackout threshold.
        if total_load_mw > 0.0 && load_interrupted_mw / total_load_mw >= options.blackout_fraction {
            break;
        }
    }

    let blackout =
        total_load_mw > 0.0 && load_interrupted_mw / total_load_mw >= options.blackout_fraction;

    Ok(CascadeResult {
        initiating_contingency: initiating_branch,
        cascade_events,
        total_load_interrupted_mw: load_interrupted_mw,
        cascade_depth,
        blackout,
    })
}

// ── analyze_cascade ─────────────────────────────────────────────────────

/// Run N-1 cascade analysis for every in-service branch with a valid rating.
///
/// Results are sorted by cascade depth (descending), then load interrupted
/// (descending), so the most severe cascades appear first.
///
/// # Arguments
/// * `network` – Power system network.
/// * `options` – Cascade options.
pub fn analyze_cascade(
    network: &Network,
    options: &CascadeOptions,
) -> Result<Vec<CascadeResult>, RelayCascadeError> {
    let mut model = PreparedCascadeModel::new(network)?;
    model.run_all(options)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Redistribute flows via LODF for a single outage and mark `tripped` out of service.
fn apply_outage<F>(
    flows: &mut [f64],
    out_of_service: &mut [bool],
    tripped: usize,
    outage_column: &mut F,
) -> Result<(), RelayCascadeError>
where
    F: FnMut(usize) -> Result<Vec<f64>, RelayCascadeError>,
{
    let n_br = flows.len();
    let flow_tripped = flows[tripped];
    let lodf_column = outage_column(tripped)?;

    for j in 0..n_br {
        if j == tripped || out_of_service[j] {
            continue;
        }
        let l = lodf_column[j];
        if l.is_finite() {
            flows[j] += l * flow_tripped;
        }
    }

    flows[tripped] = 0.0;
    out_of_service[tripped] = true;
    Ok(())
}

/// Apply a whole relay level simultaneously from the same pre-trip flow state.
///
/// This avoids order dependence within a single cascade level. The redistribution
/// still uses the prepared single-outage LODF columns, so it remains a
/// first-order screening approximation rather than an exact post-topology solve.
fn apply_outages_simultaneously<F>(
    flows: &mut [f64],
    out_of_service: &mut [bool],
    tripped_branches: &[usize],
    flows_before_level: &[f64],
    outage_column: &mut F,
) -> Result<(), RelayCascadeError>
where
    F: FnMut(usize) -> Result<Vec<f64>, RelayCascadeError>,
{
    if tripped_branches.is_empty() {
        return Ok(());
    }

    let n_br = flows.len();
    let mut delta = vec![0.0; n_br];
    let mut tripped_at_level = vec![false; n_br];
    for &tripped in tripped_branches {
        tripped_at_level[tripped] = true;
    }

    for &tripped in tripped_branches {
        let flow_tripped = flows_before_level[tripped];
        let lodf_column = outage_column(tripped)?;

        for j in 0..n_br {
            if j == tripped || tripped_at_level[j] || out_of_service[j] {
                continue;
            }
            let l = lodf_column[j];
            if l.is_finite() {
                delta[j] += l * flow_tripped;
            }
        }
    }

    for j in 0..n_br {
        if out_of_service[j] || tripped_at_level[j] {
            flows[j] = 0.0;
        } else {
            flows[j] = flows_before_level[j] + delta[j];
        }
    }
    for &tripped in tripped_branches {
        flows[tripped] = 0.0;
        out_of_service[tripped] = true;
    }
    Ok(())
}

/// Estimate MW of load now isolated (buses with no in-service branch connected).
///
/// Conservative heuristic: a bus is interrupted when every branch touching it
/// is out of service.  Overestimates for meshed networks but is appropriate
/// for relay-cascade screening where we want a pessimistic bound.
fn estimate_load_interrupted(network: &Network, out_of_service: &[bool]) -> f64 {
    let n_bus = network.n_buses();
    let bus_idx = network.bus_index_map();

    let mut in_service_count: Vec<u32> = vec![0u32; n_bus];

    for (j, branch) in network.branches.iter().enumerate() {
        if out_of_service[j] || !branch.in_service {
            continue;
        }
        if let Some(&fi) = bus_idx.get(&branch.from_bus) {
            in_service_count[fi] += 1;
        }
        if let Some(&ti) = bus_idx.get(&branch.to_bus) {
            in_service_count[ti] += 1;
        }
    }

    let bus_pd_mw = network.bus_load_p_mw();
    let mut interrupted_mw = 0.0;
    for i in 0..network.buses.len() {
        if in_service_count[i] == 0 && bus_pd_mw[i] > 0.0 {
            interrupted_mw += bus_pd_mw[i];
        }
    }
    interrupted_mw
}

/// Build a human-readable label for branch `k`.
fn branch_label(network: &Network, k: usize) -> String {
    let b = &network.branches[k];
    format!("{}->{}", b.from_bus, b.to_bus)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use faer::Mat;
    use surge_network::Network;
    use surge_network::network::{Bus, BusType, Generator, Load};

    fn simulate_with_dense_lodf(
        network: &Network,
        base_flows_mw: &[f64],
        lodf: &Mat<f64>,
        initiating_branch: usize,
        options: &CascadeOptions,
    ) -> Result<CascadeResult, RelayCascadeError> {
        simulate_cascade_with_column_provider(
            network,
            base_flows_mw,
            initiating_branch,
            options,
            |tripped| {
                Ok((0..network.n_branches())
                    .map(|j| lodf[(j, tripped)])
                    .collect())
            },
        )
    }

    fn run_with_dense_lodf(
        network: &Network,
        base_flows_mw: &[f64],
        lodf: &Mat<f64>,
        options: &CascadeOptions,
    ) -> Result<Vec<CascadeResult>, RelayCascadeError> {
        let mut results: Vec<CascadeResult> = (0..network.n_branches())
            .filter(|&k| {
                let b = &network.branches[k];
                b.in_service && get_rating(b, options.thermal_rating) > 0.0
            })
            .map(|initiating_branch| {
                simulate_with_dense_lodf(network, base_flows_mw, lodf, initiating_branch, options)
            })
            .collect::<Result<Vec<_>, _>>()?;
        results.sort_by(|a, b| {
            b.cascade_depth.cmp(&a.cascade_depth).then_with(|| {
                b.total_load_interrupted_mw
                    .partial_cmp(&a.total_load_interrupted_mw)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        Ok(results)
    }

    fn mk_bus(num: u32, btype: BusType) -> Bus {
        Bus {
            number: num,
            name: format!("Bus {num}"),
            bus_type: btype,
            shunt_conductance_mw: 0.0,
            shunt_susceptance_mvar: 0.0,
            area: 1,
            voltage_magnitude_pu: 1.0,
            voltage_angle_rad: 0.0,
            base_kv: 345.0,
            zone: 1,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            latitude: None,
            longitude: None,
            island_id: 0,
            ..Default::default()
        }
    }

    fn mk_branch_rated(from: u32, to: u32, rate_a: f64) -> surge_network::network::Branch {
        let mut b = surge_network::network::Branch::new_line(from, to, 0.01, 0.1, 0.0);
        b.rating_a_mva = rate_a;
        b.rating_b_mva = rate_a;
        b.rating_c_mva = rate_a;
        b
    }

    /// 4-bus ring: Bus 1 (slack) -- L12(80 MW) -- Bus 2(50 MW load)
    ///                  |                                  |
    ///                L14(200)                           L23(200)
    ///                  |                                  |
    ///             Bus 4(50 MW load) -- L34(200) -- Bus 3(50 MW load)
    fn make_4bus_network() -> Network {
        let mut net = Network::new("4bus");
        net.base_mva = 100.0;

        net.buses.push(mk_bus(1, BusType::Slack));
        net.buses.push(mk_bus(2, BusType::PQ));
        net.buses.push(mk_bus(3, BusType::PQ));
        net.buses.push(mk_bus(4, BusType::PQ));

        net.loads.push(Load::new(2, 50.0, 0.0));
        net.loads.push(Load::new(3, 50.0, 0.0));
        net.loads.push(Load::new(4, 50.0, 0.0));

        net.branches.push(mk_branch_rated(1, 2, 80.0)); // 0 — weak link
        net.branches.push(mk_branch_rated(2, 3, 200.0)); // 1
        net.branches.push(mk_branch_rated(3, 4, 200.0)); // 2
        net.branches.push(mk_branch_rated(4, 1, 200.0)); // 3

        let mut slack = Generator::new(1, 150.0, 1.0);
        slack.pmax = 300.0;
        slack.pmin = 0.0;
        net.generators.push(slack);

        net
    }

    #[test]
    fn test_cascade_result_structure() {
        let network = make_4bus_network();
        let n_br = network.n_branches();

        let base_flows: Vec<f64> = vec![70.0, 40.0, 30.0, 20.0];

        let mut lodf = Mat::<f64>::zeros(n_br, n_br);
        for i in 0..n_br {
            lodf[(i, i)] = -1.0;
        }
        lodf[(0, 3)] = 0.5;
        lodf[(1, 3)] = 0.3;
        lodf[(2, 3)] = 0.2;

        let options = CascadeOptions::default();
        let result = simulate_with_dense_lodf(&network, &base_flows, &lodf, 3, &options).unwrap();

        assert!(!result.cascade_events.is_empty());
        assert_eq!(result.cascade_events[0].cascade_level, 0);
        assert_eq!(result.cascade_events[0].cause, CascadeCause::Initial);
        assert_eq!(result.cascade_events[0].tripped_branch_index, 3);
    }

    #[test]
    fn test_cascade_triggers_zone3() {
        let network = make_4bus_network();
        let n_br = network.n_branches();

        // Branch 0 = 78 MW / 80 MW rating (97.5 % loaded).
        // After branch 3 trips with LODF=0.6:
        // new flow on br 0 = 78 + 0.6×20 = 90 MW > 0.8×80 = 64 MW → Zone 3 picks up.
        let base_flows: Vec<f64> = vec![78.0, 30.0, 20.0, 20.0];

        let mut lodf = Mat::<f64>::zeros(n_br, n_br);
        for i in 0..n_br {
            lodf[(i, i)] = -1.0;
        }
        lodf[(0, 3)] = 0.6;
        lodf[(1, 3)] = 0.3;
        lodf[(2, 3)] = 0.1;

        let options = CascadeOptions {
            z3_pickup_fraction: 0.8,
            z3_delay_s: 1.0,
            max_cascade_levels: 5,
            blackout_fraction: 0.95, // don't stop early on load
            ..Default::default()
        };

        let result = simulate_with_dense_lodf(&network, &base_flows, &lodf, 3, &options).unwrap();

        let zone3_count = result
            .cascade_events
            .iter()
            .filter(|e| e.cause == CascadeCause::Zone3Relay)
            .count();

        assert!(
            zone3_count >= 1,
            "Expected ≥ 1 Zone3Relay trip; events = {:?}",
            result.cascade_events
        );
        assert!(result.cascade_depth >= 1);
    }

    #[test]
    fn test_cascade_max_levels_limit() {
        let network = make_4bus_network();
        let n_br = network.n_branches();

        // All branches severely overloaded.
        let base_flows: Vec<f64> = vec![100.0, 250.0, 250.0, 250.0];

        let mut lodf = Mat::<f64>::zeros(n_br, n_br);
        for i in 0..n_br {
            lodf[(i, i)] = -1.0;
            for j in 0..n_br {
                if i != j {
                    lodf[(i, j)] = 0.5;
                }
            }
        }

        let options = CascadeOptions {
            z3_pickup_fraction: 0.8,
            z3_delay_s: 1.0,
            max_cascade_levels: 2, // hard stop at level 2
            blackout_fraction: 1.0,
            ..Default::default()
        };

        let result = simulate_with_dense_lodf(&network, &base_flows, &lodf, 0, &options).unwrap();

        assert!(
            result.cascade_depth <= 2,
            "cascade_depth {} exceeds max_cascade_levels=2",
            result.cascade_depth
        );
    }

    #[test]
    fn test_analyze_cascade_sorted_by_depth() {
        let network = make_4bus_network();
        let n_br = network.n_branches();

        let base_flows: Vec<f64> = vec![60.0, 60.0, 60.0, 60.0];

        let mut lodf = Mat::<f64>::zeros(n_br, n_br);
        for i in 0..n_br {
            lodf[(i, i)] = -1.0;
        }

        let options = CascadeOptions::default();
        let results = run_with_dense_lodf(&network, &base_flows, &lodf, &options).unwrap();

        // Results must be sorted by cascade_depth descending.
        for w in results.windows(2) {
            assert!(
                w[0].cascade_depth >= w[1].cascade_depth,
                "Not sorted: depth[0]={} < depth[1]={}",
                w[0].cascade_depth,
                w[1].cascade_depth
            );
        }
    }

    #[test]
    fn test_same_level_trips_use_pre_trip_flows() {
        let mut network = make_4bus_network();
        network.branches[1].rating_a_mva = 90.0;
        network.branches[1].rating_b_mva = 90.0;
        network.branches[1].rating_c_mva = 90.0;
        let n_br = network.n_branches();

        let base_flows: Vec<f64> = vec![70.0, 65.0, 20.0, 20.0];

        let mut lodf = Mat::<f64>::zeros(n_br, n_br);
        for i in 0..n_br {
            lodf[(i, i)] = -1.0;
        }
        lodf[(0, 3)] = 0.5;
        lodf[(1, 3)] = 0.5;
        lodf[(1, 0)] = 10.0;

        let options = CascadeOptions {
            z3_pickup_fraction: 0.8,
            z3_delay_s: 1.0,
            max_cascade_levels: 2,
            blackout_fraction: 1.0,
            ..Default::default()
        };

        let result = simulate_with_dense_lodf(&network, &base_flows, &lodf, 3, &options).unwrap();
        let branch_one_trip = result
            .cascade_events
            .iter()
            .find(|event| event.tripped_branch_index == 1 && event.cascade_level == 1)
            .expect("branch 1 trips at level 1");

        assert!(
            (branch_one_trip.flow_before_trip_mw - 75.0).abs() < 1e-9,
            "same-level relay trips must record the shared pre-trip flow state"
        );
    }

    #[test]
    fn test_same_level_trip_redistribution_is_order_independent() {
        let mut network = make_4bus_network();
        network.branches[1].rating_a_mva = 90.0;
        network.branches[1].rating_b_mva = 90.0;
        network.branches[1].rating_c_mva = 90.0;
        network.branches[2].rating_a_mva = 80.0;
        network.branches[2].rating_b_mva = 80.0;
        network.branches[2].rating_c_mva = 80.0;
        let n_br = network.n_branches();

        let base_flows: Vec<f64> = vec![70.0, 65.0, 20.0, 20.0];

        let mut lodf = Mat::<f64>::zeros(n_br, n_br);
        for i in 0..n_br {
            lodf[(i, i)] = -1.0;
        }
        lodf[(0, 3)] = 0.5;
        lodf[(1, 3)] = 0.5;
        lodf[(2, 0)] = 0.4;
        lodf[(2, 1)] = 0.3;
        lodf[(1, 0)] = 10.0;

        let options = CascadeOptions {
            z3_pickup_fraction: 0.8,
            z3_delay_s: 1.0,
            max_cascade_levels: 2,
            blackout_fraction: 1.0,
            ..Default::default()
        };

        let result = simulate_with_dense_lodf(&network, &base_flows, &lodf, 3, &options).unwrap();
        let last_event = result
            .cascade_events
            .last()
            .expect("cascade includes at least the initiating event");

        assert_eq!(last_event.tripped_branch_index, 2);
        assert!(
            (last_event.flow_before_trip_mw - 74.5).abs() < 1e-9,
            "level-2 redistribution should use the combined effect of all level-1 trips"
        );
    }

    #[test]
    fn test_public_simulate_cascade_matches_dense_reference() {
        let network = make_4bus_network();
        let dc = surge_dc::solve_dc(&network).expect("DC solve");
        let base_flows: Vec<f64> = dc
            .branch_p_flow
            .iter()
            .map(|flow| flow * network.base_mva)
            .collect();
        let all_branches: Vec<usize> = (0..network.n_branches()).collect();
        let lodf = surge_dc::compute_lodf_matrix(
            &network,
            &surge_dc::LodfMatrixRequest::for_branches(&all_branches),
        )
        .expect("dense LODF");
        let options = CascadeOptions {
            z3_pickup_fraction: 0.8,
            z3_delay_s: 1.0,
            max_cascade_levels: 5,
            blackout_fraction: 1.0,
            ..Default::default()
        };

        let dense = simulate_with_dense_lodf(&network, &base_flows, lodf.matrix(), 0, &options)
            .expect("dense cascade reference");
        let public = simulate_cascade(&network, 0, &options).expect("public cascade");

        assert_eq!(public.cascade_depth, dense.cascade_depth);
        assert_eq!(public.blackout, dense.blackout);
        assert_eq!(public.cascade_events.len(), dense.cascade_events.len());
        for (lhs, rhs) in public
            .cascade_events
            .iter()
            .zip(dense.cascade_events.iter())
        {
            assert_eq!(lhs.cascade_level, rhs.cascade_level);
            assert_eq!(lhs.tripped_branch_index, rhs.tripped_branch_index);
            assert_eq!(lhs.cause, rhs.cause);
        }
    }

    #[test]
    fn test_prepared_cascade_model_run_all_matches_public_wrapper() {
        let network = make_4bus_network();
        let options = CascadeOptions::default();

        let wrapper = analyze_cascade(&network, &options).expect("wrapper results");
        let mut prepared = PreparedCascadeModel::new(&network).expect("prepared cascade model");
        let direct = prepared.run_all(&options).expect("prepared run_all");

        assert_eq!(wrapper.len(), direct.len());
        for (lhs, rhs) in wrapper.iter().zip(direct.iter()) {
            assert_eq!(lhs.initiating_contingency, rhs.initiating_contingency);
            assert_eq!(lhs.cascade_depth, rhs.cascade_depth);
            assert_eq!(lhs.blackout, rhs.blackout);
        }
    }

    #[test]
    fn test_public_cascade_reports_invalid_branch_index() {
        let network = make_4bus_network();
        let error = simulate_cascade(&network, network.n_branches(), &CascadeOptions::default())
            .expect_err("invalid branch should error");
        assert!(matches!(error, RelayCascadeError::InvalidBranchIndex(_)));
    }

    #[test]
    fn test_prepared_cascade_model_rejects_empty_network() {
        let network = Network::new("empty");
        let error = match PreparedCascadeModel::new(&network) {
            Ok(_) => panic!("empty network should fail"),
            Err(error) => error,
        };
        assert!(matches!(error, RelayCascadeError::NoBranches));
    }
}
