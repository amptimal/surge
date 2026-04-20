// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared post-contingency LODF screening + flowgate cut generation.
//!
//! This module is the single source of truth for the linearized N-1
//! constraint generation that both SCUC (`scuc/security.rs`) and SCED
//! (`sced/security.rs`) call into. It encapsulates:
//!
//!   * Caching of per-period PTDF rows for monitored / contingency branches.
//!   * `screen_branch_violations` — given a period's bus angles, return
//!     every branch overload that exceeds the contingency rating
//!     (`s^max,ctg`).
//!   * `build_branch_lodf_flowgate` — turn a violation into a `Flowgate`
//!     constraint with the right LODF coefficient on the contingency branch
//!     and the contingency-state thermal rating in the per-period limit
//!     schedule. The AC NLP and DC SCUC LP both consume this `Flowgate`
//!     shape natively, so no new row machinery is needed downstream.
//!
//! The HVDC contingency cuts remain inlined in `scuc/security.rs` since
//! the SCED single-period AC pipeline does not own HVDC dispatch
//! variables in the same way SCUC does.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::network::{
    BranchRatingCondition, BranchRef, Flowgate, FlowgateBreachSides, WeightedBranchRef,
};

use crate::error::ScedError;

/// Sentinel limit value (in MW) used in `Flowgate::limit_mw_schedule` for
/// periods other than the violating one. This effectively disables the
/// flowgate constraint outside its target period without changing the LP
/// row count.
#[allow(dead_code)]
pub const INACTIVE_SECURITY_LIMIT_MW: f64 = 1e30;

/// A single post-contingency overload detected by LODF screening.
#[derive(Debug, Clone)]
pub struct BranchSecurityViolation {
    /// Period this violation applies to.
    pub period: usize,
    /// Index of the contingency branch (the one that trips).
    pub contingency_branch_idx: usize,
    /// Index of the monitored branch (the one that overloads in the
    /// post-contingency state).
    pub monitored_branch_idx: usize,
    /// How much the post-contingency `|f|` exceeds `s^max,ctg`, in pu.
    pub severity_pu: f64,
    /// Which side of the thermal band the post-contingency flow
    /// crossed. `true` when `post_flow > +limit` (upper breach);
    /// `false` when `post_flow < -limit` (lower breach). Threaded into
    /// `Flowgate.breach_sides` by the builder so the bounds layer
    /// only allocates a slack column on the breached side. Preseeded
    /// flowgates — where the screener is ranking instead of observing
    /// a breach — set this to `true` by convention and the builder
    /// emits `FlowgateBreachSides::Both`.
    pub breach_upper: bool,
}

/// Per-branch metadata cached for LODF screening of one period's snapshot.
#[derive(Debug, Clone)]
pub struct LodfBranchContingency {
    pub branch_idx: usize,
    pub from_idx: usize,
    pub to_idx: usize,
    /// `1 - (PTDF_kk_from - PTDF_kk_to)`. Pre-computed because every
    /// monitored-branch / contingency-branch pair shares this denominator
    /// (the contingency line itself).
    pub denom: f64,
}

/// All cached LODF state for one period of one network snapshot. Reusing
/// this between SCUC iterations or SCED security rounds avoids repeated
/// PTDF factorisations.
pub struct LodfPeriodContext {
    /// Indices of branches we monitor for post-contingency overloads.
    pub monitored: Vec<usize>,
    /// PTDF rows keyed by branch index for both monitored and contingency
    /// branches. `surge_dc::PtdfRows` lazily extracts the rows we asked
    /// for during construction.
    pub ptdf: surge_dc::PtdfRows,
    /// Metadata for the contingency branches (the `k` index in `LODF_lk`).
    pub branch_contingencies: HashMap<usize, LodfBranchContingency>,
}

/// Build a `LodfPeriodContext` for one period of a snapshot.
///
/// `monitored_predicate` decides which branches to flag as potentially
/// overloadable; `contingency_predicate` decides which branches to consider
/// as outage candidates. The default for both is "in service, finite
/// reactance, base rating above `min_rate`".
///
/// Returning `Ok(None)` would force callers to handle a missing context
/// branch — instead we always return a context (potentially with empty
/// monitored / contingency sets) so the screening loop is a no-op rather
/// than an error.
pub fn build_lodf_period_context(
    network: &Network,
    contingency_branches: &[usize],
    min_rate: f64,
) -> Result<LodfPeriodContext, ScedError> {
    let monitored: Vec<usize> = network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| br.in_service && br.rating_a_mva > min_rate && br.x.abs() > 1e-20)
        .map(|(idx, _)| idx)
        .collect();

    let contingency_candidates: Vec<usize> = if contingency_branches.is_empty() {
        monitored.clone()
    } else {
        contingency_branches
            .iter()
            .copied()
            .filter(|&idx| idx < network.branches.len())
            .collect()
    };

    let mut ptdf_branch_set: HashSet<usize> = monitored.iter().copied().collect();
    ptdf_branch_set.extend(contingency_candidates.iter().copied().filter(|&idx| {
        network
            .branches
            .get(idx)
            .is_some_and(|branch| branch.in_service && branch.x.abs() > 1e-20)
    }));
    let mut ptdf_branches: Vec<usize> = ptdf_branch_set.into_iter().collect();
    ptdf_branches.sort_unstable();
    let ptdf = if ptdf_branches.is_empty() {
        surge_dc::PtdfRows::default()
    } else {
        // Uniform participation factors `α_i = 1/|I|` for
        // post-contingency slack distribution.
        let n_buses = network.buses.len();
        let uniform_weights: Vec<(usize, f64)> = if n_buses > 0 {
            let w = 1.0 / n_buses as f64;
            (0..n_buses).map(|i| (i, w)).collect()
        } else {
            Vec::new()
        };
        let sensitivity_options =
            surge_dc::DcSensitivityOptions::with_slack_weights(&uniform_weights);
        let ptdf_request =
            surge_dc::PtdfRequest::for_branches(&ptdf_branches).with_options(sensitivity_options);
        surge_dc::compute_ptdf(network, &ptdf_request)
            .map_err(|e| ScedError::SolverError(e.to_string()))?
    };

    let bus_map = network.bus_index_map();
    let branch_contingencies: HashMap<usize, LodfBranchContingency> = contingency_candidates
        .iter()
        .filter_map(|&branch_idx| {
            let branch = network.branches.get(branch_idx)?;
            if !branch.in_service || branch.x.abs() < 1e-20 {
                return None;
            }
            let from_idx = *bus_map.get(&branch.from_bus)?;
            let to_idx = *bus_map.get(&branch.to_bus)?;
            let ptdf_k = ptdf.row(branch_idx)?;
            let denom = 1.0 - (ptdf_k[from_idx] - ptdf_k[to_idx]);
            if denom.abs() < 1e-10 {
                return None;
            }
            Some((
                branch_idx,
                LodfBranchContingency {
                    branch_idx,
                    from_idx,
                    to_idx,
                    denom,
                },
            ))
        })
        .collect();

    Ok(LodfPeriodContext {
        monitored,
        ptdf,
        branch_contingencies,
    })
}

/// Screen one period for post-contingency branch overloads.
///
/// Returns a violation for every (monitored, contingency) pair whose
/// LODF-projected post-contingency flow exceeds the monitored branch's
/// `BranchRatingCondition::Emergency` rating by more than `tolerance_pu`.
/// Pairs already in `exclude_pairs` (e.g. because the caller has already
/// added a flowgate for that pair) are skipped to avoid generating
/// duplicate cuts.
///
/// Post-contingency flowgates use the emergency rating
/// (`rating_c_mva`), with a graceful fallback chain
/// `rating_c → rating_b → rating_a` so datasets that only populate a
/// subset of the rating tiers still work.
///
/// `exclude_pairs` keys are `(period, contingency_idx, monitored_idx)`.
/// `period` is part of the key so the same monitored/contingency pair can
/// be re-added in different periods of a multi-period horizon.
pub fn screen_branch_violations(
    period: usize,
    angles: &[f64],
    network: &Network,
    context: &LodfPeriodContext,
    base_mva: f64,
    tolerance_pu: f64,
    exclude_pairs: &HashSet<(usize, usize, usize)>,
) -> Vec<BranchSecurityViolation> {
    let bus_map = network.bus_index_map();
    let mut violations = Vec::new();

    for contingency in context.branch_contingencies.values() {
        let k = contingency.branch_idx;
        let branch_k = &network.branches[k];
        let flow_k = branch_k.b_dc()
            * (angles[contingency.from_idx]
                - angles[contingency.to_idx]
                - branch_k.phase_shift_rad);

        for &l in &context.monitored {
            if l == k || exclude_pairs.contains(&(period, k, l)) {
                continue;
            }
            let Some(ptdf_l) = context.ptdf.row(l) else {
                continue;
            };
            let lodf_lk =
                (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
            if !lodf_lk.is_finite() {
                continue;
            }

            let branch_l = &network.branches[l];
            let Some(&from_l) = bus_map.get(&branch_l.from_bus) else {
                continue;
            };
            let Some(&to_l) = bus_map.get(&branch_l.to_bus) else {
                continue;
            };
            let flow_l =
                branch_l.b_dc() * (angles[from_l] - angles[to_l] - branch_l.phase_shift_rad);
            let post_flow = flow_l + lodf_lk * flow_k;
            // Contingency-state thermal limits may exceed base-case
            // (`s^max,ctg ≥ s^max`). Use `Emergency`, whose fallback
            // chain is `rating_c → rating_b → rating_a`, so datasets
            // that only populate RATE_A still work.
            let limit_pu = branch_l.rating_for(BranchRatingCondition::Emergency) / base_mva;
            let excess = post_flow.abs() - limit_pu;
            if excess > tolerance_pu {
                violations.push(BranchSecurityViolation {
                    period,
                    contingency_branch_idx: k,
                    monitored_branch_idx: l,
                    severity_pu: excess,
                    breach_upper: post_flow > 0.0,
                });
            }
        }
    }

    violations
}

/// Build a `Flowgate` constraint that encodes one LODF post-contingency
/// branch overload. The flowgate uses two `WeightedBranchRef` entries:
/// the monitored branch with coefficient `+1` and the contingency branch
/// with coefficient `LODF_lk`. Both DC SCUC LP and AC NLP enforce the
/// same constraint shape via `network.flowgates`, so this single Flowgate
/// builder serves both pipelines.
///
/// The `n_periods` argument controls the per-period schedule encoding:
/// - When `n_periods > 1` the flowgate uses the compact
///   `limit_mw_active_period = Some(violation.period)` marker and leaves
///   `limit_mw_schedule` empty. `Flowgate::effective_limit_mw(t)` then
///   returns `limit_mw` at `t == violation.period` and
///   `INACTIVE_FLOWGATE_LIMIT_MW` for all other periods. This avoids a
///   per-flowgate `Vec<f64>` of length `n_periods` with (`n_periods − 1`)
///   sentinel entries — ~1.2 GB saved on 617-bus explicit N-1 SCUC
///   (8.6 M flowgates × 144 B).
/// - When `n_periods <= 1` (single-period SCED callers) the compact
///   marker is `None` and `limit_mw_schedule` is empty; the LP row
///   naturally picks up `limit_mw` via the fallback path.
pub fn build_branch_lodf_flowgate(
    violation: &BranchSecurityViolation,
    network: &Network,
    context: &LodfPeriodContext,
    n_periods: usize,
) -> Flowgate {
    let monitored_branch = &network.branches[violation.monitored_branch_idx];
    let contingency_branch = &network.branches[violation.contingency_branch_idx];
    let contingency = context
        .branch_contingencies
        .get(&violation.contingency_branch_idx)
        .expect("branch contingency metadata should exist for screened violation");
    let ptdf_l = context
        .ptdf
        .row(violation.monitored_branch_idx)
        .expect("monitored branch PTDF row should exist for screened violation");
    let lodf_lk = (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
    let limit_mw = monitored_branch.rating_for(BranchRatingCondition::Emergency);

    let active_period =
        (n_periods > 1 && violation.period < n_periods).then_some(violation.period as u32);

    Flowgate {
        name: format!(
            "N1_t{}_{}_{}_{}_{}",
            violation.period,
            contingency_branch.from_bus,
            contingency_branch.to_bus,
            monitored_branch.from_bus,
            monitored_branch.to_bus
        ),
        monitored: vec![
            WeightedBranchRef {
                branch: BranchRef::new(
                    monitored_branch.from_bus,
                    monitored_branch.to_bus,
                    monitored_branch.circuit.clone(),
                ),
                coefficient: 1.0,
            },
            WeightedBranchRef {
                branch: BranchRef::new(
                    contingency_branch.from_bus,
                    contingency_branch.to_bus,
                    contingency_branch.circuit.clone(),
                ),
                coefficient: lodf_lk,
            },
        ],
        contingency_branch: Some(BranchRef::new(
            contingency_branch.from_bus,
            contingency_branch.to_bus,
            contingency_branch.circuit.clone(),
        )),
        limit_mw,
        limit_reverse_mw: 0.0,
        in_service: true,
        limit_mw_schedule: Vec::new(),
        limit_reverse_mw_schedule: Vec::new(),
        hvdc_coefficients: Vec::new(),
        hvdc_band_coefficients: Vec::new(),
        limit_mw_active_period: active_period,
        breach_sides: if violation.breach_upper {
            FlowgateBreachSides::Upper
        } else {
            FlowgateBreachSides::Lower
        },
    }
}

/// Build a per-period limit schedule that activates `limit_mw` only in
/// `active_period`, leaving every other period at `INACTIVE_SECURITY_LIMIT_MW`.
#[allow(dead_code)]
pub fn hour_only_limit_schedule(n_periods: usize, active_period: usize, limit_mw: f64) -> Vec<f64> {
    let mut schedule = vec![INACTIVE_SECURITY_LIMIT_MW; n_periods];
    if active_period < n_periods {
        schedule[active_period] = limit_mw;
    }
    schedule
}

/// Public knob set for the SCED security loop. Mirrors the SCUC loop's
/// existing fields but lives on the public API surface so callers can
/// opt in without reaching into internal types.
///
/// `enabled = false` is the default — the post-contingency SCED
/// security loop is opt-in while it is being more widely validated.
/// Setting `enabled = true` activates the iterative LODF cut loop in
/// `sced/security.rs`.
#[derive(Debug, Clone)]
pub struct SecurityConfig {
    pub enabled: bool,
    pub max_rounds: usize,
    pub max_cuts_per_iteration: usize,
    pub violation_tolerance_pu: f64,
    /// Restricts the contingency set to the listed branch indices. Empty
    /// means "use every monitored branch as a contingency candidate".
    pub contingency_branch_indices: Vec<usize>,
    /// Minimum base rating (`rating_a_mva`) for a branch to be eligible
    /// for monitoring or as a contingency. Tracks `DispatchInput::min_rate_a`.
    pub min_rate_a: f64,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_rounds: 5,
            max_cuts_per_iteration: 32,
            violation_tolerance_pu: 1e-6,
            contingency_branch_indices: Vec::new(),
            min_rate_a: 1.0,
        }
    }
}

impl SecurityConfig {
    #[allow(dead_code)]
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn enabled_with_defaults() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    fn triangle_network(monitored_limit_mw: f64) -> Network {
        let mut net = Network::new("lodf_triangle");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 138.0));

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 100.0;
        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 100.0;
        let mut br13 = Branch::new_line(1, 3, 0.0, 0.1, 0.0);
        br13.rating_a_mva = monitored_limit_mw;
        net.branches = vec![br12, br23, br13];
        // PTDF computation needs at least one generator at the slack bus
        // to anchor the bus injection vector. The screening logic doesn't
        // care about generator parameters; it only reads bus angles.
        net.generators.push(Generator::new(1, 50.0, 1.0));
        net
    }

    #[test]
    fn screen_returns_empty_when_post_contingency_within_limit() {
        let net = triangle_network(80.0);
        let context = build_lodf_period_context(&net, &[0], 1.0).unwrap();
        let angles = [0.0, -0.05, -0.025];
        let violations = screen_branch_violations(
            0,
            &angles,
            &net,
            &context,
            net.base_mva,
            1e-6,
            &HashSet::new(),
        );
        assert!(
            violations.is_empty(),
            "expected no violations when monitored limit comfortably absorbs the LODF flow, got {violations:?}"
        );
    }

    #[test]
    fn screen_finds_violation_when_post_contingency_exceeds_limit() {
        let net = triangle_network(60.0);
        let context = build_lodf_period_context(&net, &[0], 1.0).unwrap();
        let angles = [0.0, -0.05, -0.025];
        let violations = screen_branch_violations(
            0,
            &angles,
            &net,
            &context,
            net.base_mva,
            1e-6,
            &HashSet::new(),
        );
        assert!(
            violations
                .iter()
                .any(|v| v.contingency_branch_idx == 0 && v.monitored_branch_idx == 2),
            "expected the 1-2 outage / 1-3 monitored pair to violate the 60 MW limit, got {violations:?}"
        );
    }

    #[test]
    fn build_flowgate_uses_contingency_rating_and_lodf_coefficient() {
        let mut net = triangle_network(60.0);
        // Set rating_c on the monitored branch above its base rating;
        // the security loop screens against the `Emergency` rating so
        // this test exercises that path.
        net.branches[2].rating_c_mva = 90.0;
        net.branches[2].rating_b_mva = 75.0;
        let context = build_lodf_period_context(&net, &[0], 1.0).unwrap();
        let violation = BranchSecurityViolation {
            period: 0,
            contingency_branch_idx: 0,
            monitored_branch_idx: 2,
            severity_pu: 0.1,
            breach_upper: true,
        };
        let fg = build_branch_lodf_flowgate(&violation, &net, &context, 4);
        assert_eq!(fg.monitored.len(), 2);
        assert_eq!(fg.monitored[0].coefficient, 1.0);
        let lodf = fg.monitored[1].coefficient;
        assert!(
            lodf.is_finite() && lodf.abs() > 1e-9,
            "LODF coefficient should be finite and non-zero, got {lodf}"
        );
        assert_eq!(
            fg.limit_mw, 90.0,
            "should use rating_c (emergency) for the contingency limit"
        );
        // Compact single-period encoding: empty schedule + active-period
        // marker at violation.period = 0. `effective_limit_mw(t)` returns
        // `limit_mw` only at t=0 and INACTIVE_FLOWGATE_LIMIT_MW elsewhere,
        // matching the legacy dense-schedule semantics.
        assert!(fg.limit_mw_schedule.is_empty());
        assert_eq!(fg.limit_mw_active_period, Some(0));
        assert_eq!(fg.effective_limit_mw(0), 90.0);
        for t in 1..4 {
            assert_eq!(
                fg.effective_limit_mw(t),
                surge_network::network::INACTIVE_FLOWGATE_LIMIT_MW
            );
        }
    }

    #[test]
    fn build_flowgate_falls_back_to_rating_b_when_rating_c_missing() {
        let mut net = triangle_network(60.0);
        // Datasets that only populate RATE_A/RATE_B should still get a
        // sensible emergency limit via the `Emergency → rating_b → rating_a`
        // fallback chain. Leaves `rating_c_mva = 0` so the fallback kicks in.
        net.branches[2].rating_b_mva = 75.0;
        let context = build_lodf_period_context(&net, &[0], 1.0).unwrap();
        let violation = BranchSecurityViolation {
            period: 0,
            contingency_branch_idx: 0,
            monitored_branch_idx: 2,
            severity_pu: 0.1,
            breach_upper: true,
        };
        let fg = build_branch_lodf_flowgate(&violation, &net, &context, 1);
        assert_eq!(
            fg.limit_mw, 75.0,
            "should fall back to rating_b when rating_c is 0"
        );
    }
}
