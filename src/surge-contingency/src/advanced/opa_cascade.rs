// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! P5-B08 — OPA (Optimal Power Acceleration) Cascading Failure Simulation.
//!
//! The OPA model simulates cascading transmission failures through repeated
//! probabilistic line tripping driven by overload fractions.  It is a
//! stochastic, DC-linear model originally proposed by Carreras et al. (2004)
//! and Dobson et al. (2007) to study the statistics of large blackouts.
//!
//! ## Algorithm (per Monte Carlo trial)
//!
//! 1. Start from the specified initial outages.
//! 2. Rebalance power using proportional load/generation scaling (simplified
//!    DC balance — no full OPF in this first version).
//! 3. For each in-service branch `j`:
//!    ```text
//!    P(trip) = min(1, (|flow_j| / rating_j)^beta)   if |flow_j| > rating_j
//!            = 0                                       otherwise
//!    ```
//! 4. Sample trips using the LCG PRNG.  Apply outages; redistribute flows via LODF.
//! 5. Repeat from step 2 until no new trips or `max_steps` reached.
//! 6. Compute load shed: sum of `pd` at isolated buses.
//!
//! ## References
//! - Dobson I., Carreras B. A., Lynch V. E., Newman D. E. (2007)
//!   "Complex systems analysis of series of blackouts."  *Chaos* 17, 026103.
//! - Carreras B. A. et al. (2004) "Evidence for self-organized criticality in a
//!   time series of electric power system blackouts."  *IEEE Trans. Circuits Syst.* 51.

use serde::{Deserialize, Serialize};
use surge_dc::PreparedDcStudy;
use surge_dc::streaming::LodfColumnBuilder;
use surge_network::Network;
use thiserror::Error;

use crate::{ThermalRating, get_rating};

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors that can occur in OPA cascade simulation.
#[derive(Debug, Error)]
pub enum CascadeError {
    /// The network has no in-service branches (degenerate case).
    #[error("Network has no in-service branches")]
    NoBranches,
    /// An initial-outage index is out of range.
    #[error("initial_outages contains invalid branch index {0}")]
    InvalidBranchIndex(usize),
    /// Base-case DC power flow failed.
    #[error("DC power flow failed: {0}")]
    DcFlowFailed(String),
}

// ── Options ───────────────────────────────────────────────────────────────────

/// Options for the OPA cascading failure Monte Carlo simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpaOptions {
    /// Beta exponent for the overload-to-trip probability.
    ///
    /// `P(trip) = min(1, (|flow| / rating)^beta)`.
    /// Higher beta → trips only when severely overloaded.  Default: 2.0.
    pub beta: f64,

    /// Maximum simulation steps per trial.  Default: 100.
    pub max_steps: u32,

    /// Number of Monte Carlo trials.  Default: 1000.
    pub n_trials: u32,

    /// Random seed for reproducibility.  `None` = use a fixed default seed.
    pub seed: Option<u64>,

    /// Thermal rating tier for overload probability checks.
    ///
    /// Default: `RateA` (long-term continuous rating).
    #[serde(default)]
    pub thermal_rating: ThermalRating,
}

impl Default for OpaOptions {
    fn default() -> Self {
        Self {
            beta: 2.0,
            max_steps: 100,
            n_trials: 1000,
            seed: None,
            thermal_rating: ThermalRating::default(),
        }
    }
}

// ── Results ───────────────────────────────────────────────────────────────────

/// Aggregate results from the OPA Monte Carlo cascade simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpaCascadeResult {
    /// Mean load shed in MW across all trials.
    pub mean_load_shed_mw: f64,

    /// Standard deviation of load shed in MW.
    pub std_load_shed_mw: f64,

    /// Probability of a "large" cascade (shed ≥ 50 % of total load).
    pub p_blackout: f64,

    /// Empirical CDF of load-shed fraction.
    ///
    /// Each entry is `(load_shed_fraction, cumulative_probability)` sorted by
    /// `load_shed_fraction` ascending.
    pub cascade_size_distribution: Vec<(f64, f64)>,

    /// Most critical branches ranked by expected load shed contribution.
    ///
    /// Each entry is `(branch_index, expected_load_shed_mw)`, sorted descending
    /// by expected load shed.  Capped at 20 entries.
    pub most_critical_branches: Vec<(usize, f64)>,
}

// ── LCG PRNG ─────────────────────────────────────────────────────────────────

/// 64-bit Linear Congruential Generator (LCG).
///
/// Parameters: m = 2^64, a = 6364136223846793005, c = 1442695040888963407
/// (Knuth MMIX constants).
struct LcgRng(u64);

impl LcgRng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(1))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    /// Uniform float in [0, 1).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

// ── Main entry point ─────────────────────────────────────────────────────────

/// Run the OPA Monte Carlo cascading failure simulation.
///
/// # Arguments
/// * `network`         – Power system network.
/// * `initial_outages` – Branch indices to outage at time 0.
/// * `options`         – Simulation parameters.
///
/// # Errors
/// Returns [`CascadeError`] if inputs are invalid or if the base-case DC solve fails.
pub fn analyze_opa_cascade(
    network: &Network,
    initial_outages: &[usize],
    options: &OpaOptions,
) -> Result<OpaCascadeResult, CascadeError> {
    let n_br = network.n_branches();

    if n_br == 0 {
        return Err(CascadeError::NoBranches);
    }
    for &idx in initial_outages {
        if idx >= n_br {
            return Err(CascadeError::InvalidBranchIndex(idx));
        }
    }

    // Base-case DC power flow → per-unit branch flows.
    let dc_result =
        surge_dc::solve_dc(network).map_err(|e| CascadeError::DcFlowFailed(e.to_string()))?;

    let base_mva = network.base_mva;
    // Convert per-unit flows to MW.
    let base_flows_mw: Vec<f64> = dc_result
        .branch_p_flow
        .iter()
        .map(|&f| f * base_mva)
        .collect();

    let total_load_mw = network.total_load_mw();
    let n_trials = options.n_trials as usize;

    let mut load_shed_samples: Vec<f64> = Vec::with_capacity(n_trials);
    let mut branch_shed_accum: Vec<f64> = vec![0.0_f64; n_br];
    let all_branches: Vec<usize> = (0..n_br).collect();
    let mut prepared_model =
        PreparedDcStudy::new(network).map_err(|e| CascadeError::DcFlowFailed(e.to_string()))?;
    let mut lodf_columns = prepared_model.lodf_columns();

    let seed = options.seed.unwrap_or(0xDEAD_BEEF_CAFE_1234);
    let mut rng = LcgRng::new(seed);

    for _trial in 0..n_trials {
        let shed = run_single_trial(
            network,
            &base_flows_mw,
            &all_branches,
            &mut lodf_columns,
            initial_outages,
            options,
            &mut rng,
            &mut branch_shed_accum,
        )?;
        load_shed_samples.push(shed);
    }

    // ── Statistics ────────────────────────────────────────────────────────────
    let n = n_trials as f64;
    let mean = load_shed_samples.iter().sum::<f64>() / n;
    let variance = load_shed_samples
        .iter()
        .map(|&x| (x - mean).powi(2))
        .sum::<f64>()
        / n;
    let std_dev = variance.sqrt();

    let blackout_threshold = total_load_mw * 0.5;
    let p_blackout = load_shed_samples
        .iter()
        .filter(|&&s| s >= blackout_threshold)
        .count() as f64
        / n;

    // Empirical CDF of load-shed fraction.
    let mut sorted = load_shed_samples.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut cascade_size_distribution: Vec<(f64, f64)> = Vec::with_capacity(sorted.len());
    for (rank, &shed) in sorted.iter().enumerate() {
        let fraction = if total_load_mw > 0.0 {
            shed / total_load_mw
        } else {
            0.0
        };
        let cdf_prob = (rank + 1) as f64 / n;
        cascade_size_distribution.push((fraction, cdf_prob));
    }

    // Most critical branches by expected load shed.
    let mut branch_expected: Vec<(usize, f64)> = branch_shed_accum
        .iter()
        .enumerate()
        .map(|(i, &acc)| (i, acc / n))
        .collect();
    branch_expected.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let most_critical_branches = branch_expected.into_iter().take(20).collect();

    Ok(OpaCascadeResult {
        mean_load_shed_mw: mean,
        std_load_shed_mw: std_dev,
        p_blackout,
        cascade_size_distribution,
        most_critical_branches,
    })
}

// ── Single trial ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn run_single_trial(
    network: &Network,
    base_flows_mw: &[f64],
    monitored_branches: &[usize],
    lodf_columns: &mut LodfColumnBuilder<'_, '_>,
    initial_outages: &[usize],
    options: &OpaOptions,
    rng: &mut LcgRng,
    branch_shed_accum: &mut [f64],
) -> Result<f64, CascadeError> {
    let n_br = network.n_branches();
    let mut flows: Vec<f64> = base_flows_mw.to_vec();
    let mut out_of_service: Vec<bool> = vec![false; n_br];

    // Apply initial outages.
    for &k in initial_outages {
        redistribute_flow(
            &mut flows,
            &mut out_of_service,
            monitored_branches,
            lodf_columns,
            k,
        )?;
    }

    // Iterative cascade steps.
    for _step in 0..options.max_steps {
        // Identify overloaded branches.
        let candidates: Vec<usize> = (0..n_br)
            .filter(|&j| {
                if out_of_service[j] {
                    return false;
                }
                let b = &network.branches[j];
                let r = get_rating(b, options.thermal_rating);
                b.in_service && r > 0.0 && flows[j].abs() > r
            })
            .collect();

        if candidates.is_empty() {
            break;
        }

        let mut any_tripped = false;
        for j in candidates {
            let rating = get_rating(&network.branches[j], options.thermal_rating);
            let overload_ratio = flows[j].abs() / rating;
            let p_trip = overload_ratio.powf(options.beta).min(1.0);

            if rng.next_f64() < p_trip {
                redistribute_flow(
                    &mut flows,
                    &mut out_of_service,
                    monitored_branches,
                    lodf_columns,
                    j,
                )?;
                any_tripped = true;
            }
        }

        if !any_tripped {
            break;
        }
    }

    Ok(estimate_load_interrupted_with_accum(
        network,
        &out_of_service,
        branch_shed_accum,
    ))
}

/// Redistribute flows when branch `tripped` is removed, then mark it OOS.
fn redistribute_flow(
    flows: &mut [f64],
    out_of_service: &mut [bool],
    monitored_branches: &[usize],
    lodf_columns: &mut LodfColumnBuilder<'_, '_>,
    tripped: usize,
) -> Result<(), CascadeError> {
    let f_k = flows[tripped];
    let lodf_column = lodf_columns
        .compute_column(monitored_branches, tripped)
        .map_err(|e| CascadeError::DcFlowFailed(e.to_string()))?;

    for (position, &j) in monitored_branches.iter().enumerate() {
        if j == tripped {
            continue;
        }
        let l = lodf_column[position];
        if l.is_finite() {
            flows[j] += l * f_k;
        }
    }

    flows[tripped] = 0.0;
    out_of_service[tripped] = true;
    Ok(())
}

/// Compute isolated load MW; attribute each isolated bus's load to the first
/// OOS branch connected to it (for `most_critical_branches` ranking).
fn estimate_load_interrupted_with_accum(
    network: &Network,
    out_of_service: &[bool],
    branch_shed_accum: &mut [f64],
) -> f64 {
    let n_bus = network.n_buses();
    let bus_idx = network.bus_index_map();

    let mut in_service_count: Vec<u32> = vec![0u32; n_bus];
    let mut first_oos_branch: Vec<Option<usize>> = vec![None; n_bus];

    for (j, branch) in network.branches.iter().enumerate() {
        if let Some(&fi) = bus_idx.get(&branch.from_bus) {
            if !out_of_service[j] && branch.in_service {
                in_service_count[fi] += 1;
            } else if out_of_service[j] && first_oos_branch[fi].is_none() {
                first_oos_branch[fi] = Some(j);
            }
        }
        if let Some(&ti) = bus_idx.get(&branch.to_bus) {
            if !out_of_service[j] && branch.in_service {
                in_service_count[ti] += 1;
            } else if out_of_service[j] && first_oos_branch[ti].is_none() {
                first_oos_branch[ti] = Some(j);
            }
        }
    }

    let bus_pd_mw = network.bus_load_p_mw();
    let mut interrupted_mw = 0.0_f64;
    for i in 0..network.buses.len() {
        if in_service_count[i] == 0 && bus_pd_mw[i] > 0.0 {
            interrupted_mw += bus_pd_mw[i];
            if let Some(oos_br) = first_oos_branch[i] {
                branch_shed_accum[oos_br] += bus_pd_mw[i];
            }
        }
    }

    interrupted_mw
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Bus, BusType, Generator, Load};

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

    /// 5-bus radial network supplying 100 MW total load.
    ///
    /// ```text
    ///   Bus1(slack) --L12(50MW)-- Bus2(20 MW)
    ///       |
    ///    L13(100MW)
    ///       |
    ///   Bus3(30 MW) --L34(80MW)-- Bus4(25 MW) --L45(60MW)-- Bus5(25 MW)
    /// ```
    fn make_5bus_radial() -> Network {
        let mut net = Network::new("5bus_radial");
        net.base_mva = 100.0;

        net.buses.push(mk_bus(1, BusType::Slack));
        net.buses.push(mk_bus(2, BusType::PQ));
        net.buses.push(mk_bus(3, BusType::PQ));
        net.buses.push(mk_bus(4, BusType::PQ));
        net.buses.push(mk_bus(5, BusType::PQ));

        let mut slack_gen = Generator::new(1, 100.0, 1.0);
        slack_gen.pmax = 200.0;
        slack_gen.qmax = 100.0;
        slack_gen.qmin = -100.0;
        net.generators.push(slack_gen);

        net.loads.push(Load::new(2, 20.0, 0.0));
        net.loads.push(Load::new(3, 30.0, 0.0));
        net.loads.push(Load::new(4, 25.0, 0.0));
        net.loads.push(Load::new(5, 25.0, 0.0));

        let mk = |from: u32, to: u32, rate: f64| {
            let mut b = surge_network::network::Branch::new_line(from, to, 0.01, 0.1, 0.0);
            b.rating_a_mva = rate;
            b.rating_b_mva = rate;
            b.rating_c_mva = rate;
            b
        };

        net.branches.push(mk(1, 2, 50.0)); // 0: L12
        net.branches.push(mk(1, 3, 100.0)); // 1: L13
        net.branches.push(mk(3, 4, 80.0)); // 2: L34
        net.branches.push(mk(4, 5, 60.0)); // 3: L45

        net
    }

    #[test]
    fn test_single_outage_produces_nonzero_shed() {
        let network = make_5bus_radial();

        // Outage L12 (index 0): Bus 2 becomes isolated → 20 MW shed every trial.
        let opts = OpaOptions {
            beta: 2.0,
            max_steps: 10,
            n_trials: 100,
            seed: Some(42),
            ..Default::default()
        };

        let result = analyze_opa_cascade(&network, &[0], &opts).unwrap();

        assert!(
            result.mean_load_shed_mw > 0.0,
            "Expected mean_load_shed_mw > 0, got {}",
            result.mean_load_shed_mw
        );
    }

    #[test]
    fn test_all_lines_out_approaches_full_blackout() {
        let network = make_5bus_radial();
        let n_br = network.n_branches();

        let all_outages: Vec<usize> = (0..n_br).collect();

        let opts = OpaOptions {
            beta: 2.0,
            max_steps: 10,
            n_trials: 50,
            seed: Some(99),
            ..Default::default()
        };

        let result = analyze_opa_cascade(&network, &all_outages, &opts).unwrap();

        assert!(
            result.p_blackout >= 0.9,
            "Expected p_blackout ≥ 0.9 when all lines outaged; got {}",
            result.p_blackout
        );
        let total_load = network.total_load_mw();
        assert!(
            result.mean_load_shed_mw >= total_load * 0.9,
            "Expected mean shed ≥ 90 MW; got {}",
            result.mean_load_shed_mw
        );
    }

    #[test]
    fn test_cascade_size_distribution_ends_at_one() {
        let network = make_5bus_radial();

        let opts = OpaOptions {
            beta: 2.0,
            max_steps: 20,
            n_trials: 200,
            seed: Some(7),
            ..Default::default()
        };

        let result = analyze_opa_cascade(&network, &[0], &opts).unwrap();

        if let Some(&(_, last_prob)) = result.cascade_size_distribution.last() {
            assert!(
                (last_prob - 1.0).abs() < 1e-9,
                "Last CDF entry should be 1.0; got {last_prob}"
            );
        }
    }

    #[test]
    fn test_invalid_branch_index_returns_error() {
        let network = make_5bus_radial();
        let opts = OpaOptions::default();

        assert!(
            analyze_opa_cascade(&network, &[999], &opts).is_err(),
            "Expected CascadeError for invalid branch index"
        );
    }

    #[test]
    fn test_lcg_rng_uniform_range() {
        let mut rng = LcgRng::new(12345);
        for _ in 0..10_000 {
            let v = rng.next_f64();
            assert!((0.0..1.0).contains(&v), "LCG out of [0,1): {v}");
        }
    }
}
