// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Security-constrained SCUC via iterative N-1 constraint generation.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

use surge_network::Network;
use surge_network::network::{BranchRatingCondition, Flowgate};
use tracing::{debug, info, warn};

use super::solve::{
    solve_scuc_with_owned_network, solve_scuc_with_problem_spec,
    solve_scuc_with_problem_spec_warm_started,
};
use super::types::SecurityDispatchSpec;
use crate::common::contingency::{ContingencyCut, ContingencyCutKind};
use crate::common::spec::{
    DispatchProblemSpec, ExplicitContingencyCase, ExplicitContingencyElement,
    ExplicitContingencyFlowgate,
};
use crate::dispatch::{CommitmentMode, RawDispatchSolution};
use crate::error::ScedError;
use crate::request::{SecurityCutStrategy, SecurityPreseedMethod};
use crate::result::{SecurityDispatchMetadata, SecurityIterationReport, SecuritySetupTimings};

/// An HVDC security cut constraining a monitored branch flow under an HVDC link trip.
///
/// The cut is a linear constraint on existing LP variables. For legacy links it
/// uses one HVDC coefficient; for banded links it uses one coefficient per band.
#[derive(Debug, Clone)]
struct HvdcSecurityCut {
    /// Period index this cut applies to.
    period: usize,
    /// Index into `options.input.hvdc_links`.
    hvdc_link_idx: usize,
    /// Monitored branch index.
    monitored_branch_idx: usize,
    /// Legacy link-level HVDC coefficient for non-banded links.
    coefficient: Option<f64>,
    /// Per-band HVDC coefficients for banded links.
    band_coefficients: Vec<(usize, f64)>,
    /// Thermal limit of the monitored branch (MW).
    f_max_mw: f64,
    /// Violation severity in p.u. over the monitored branch thermal limit.
    excess_pu: f64,
    /// Which side of the thermal band the post-contingency flow
    /// crossed. `true` when `post_flow > +limit` — see
    /// [`crate::common::security::BranchSecurityViolation::breach_upper`].
    breach_upper: bool,
}

#[derive(Debug, Clone)]
struct BranchSecurityViolation {
    period: usize,
    contingency_branch_idx: usize,
    monitored_branch_idx: usize,
    severity_pu: f64,
    /// See
    /// [`crate::common::security::BranchSecurityViolation::breach_upper`].
    breach_upper: bool,
}

#[derive(Debug)]
struct BranchViolationScreen {
    violations: Vec<BranchSecurityViolation>,
    n_violations: usize,
    max_violation_pu: Option<f64>,
}

#[derive(Debug)]
struct BoundedBranchViolationSet {
    cap: usize,
    violations: Vec<BranchSecurityViolation>,
    n_violations: usize,
    max_violation_pu: Option<f64>,
}

impl BoundedBranchViolationSet {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            violations: Vec::with_capacity(cap.min(1024)),
            n_violations: 0,
            max_violation_pu: None,
        }
    }

    fn push(&mut self, violation: BranchSecurityViolation) {
        self.n_violations += 1;
        self.max_violation_pu = Some(self.max_violation_pu.map_or(violation.severity_pu, |prev| {
            prev.max(violation.severity_pu)
        }));
        if self.cap == 0 {
            return;
        }
        self.violations.push(violation);
        let prune_len = self.cap.saturating_mul(2).max(self.cap.saturating_add(1));
        if self.violations.len() >= prune_len {
            self.prune_to_cap();
        }
    }

    fn merge(&mut self, mut other: Self) {
        self.n_violations += other.n_violations;
        self.max_violation_pu = match (self.max_violation_pu, other.max_violation_pu) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        if self.cap == 0 {
            return;
        }
        self.violations.append(&mut other.violations);
        self.prune_to_cap();
    }

    fn finish(mut self) -> BranchViolationScreen {
        self.prune_to_cap();
        self.violations
            .sort_by(compare_branch_violation_severity_desc);
        BranchViolationScreen {
            violations: self.violations,
            n_violations: self.n_violations,
            max_violation_pu: self.max_violation_pu,
        }
    }

    fn prune_to_cap(&mut self) {
        if self.cap == 0 {
            self.violations.clear();
            return;
        }
        if self.violations.len() <= self.cap {
            return;
        }
        self.violations
            .select_nth_unstable_by(self.cap, compare_branch_violation_severity_desc);
        self.violations.truncate(self.cap);
    }
}

fn compare_branch_violation_severity_desc(
    a: &BranchSecurityViolation,
    b: &BranchSecurityViolation,
) -> Ordering {
    b.severity_pu
        .partial_cmp(&a.severity_pu)
        .unwrap_or(Ordering::Equal)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SecurityCutKey {
    Branch {
        period: usize,
        contingency_branch_idx: usize,
        monitored_branch_idx: usize,
    },
    Hvdc {
        period: usize,
        hvdc_link_idx: usize,
        monitored_branch_idx: usize,
    },
}

impl SecurityCutKey {
    fn period(self) -> usize {
        match self {
            SecurityCutKey::Branch { period, .. } | SecurityCutKey::Hvdc { period, .. } => period,
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveSecurityCut {
    key: SecurityCutKey,
    flowgate: Flowgate,
    stale_rounds: usize,
    last_shadow_price_abs: f64,
    last_slack_mw: f64,
}

#[derive(Debug, Default)]
struct SecurityCutPool {
    active: VecDeque<ActiveSecurityCut>,
    retired_cuts: usize,
}

impl SecurityCutPool {
    fn active_len(&self) -> usize {
        self.active.len()
    }

    fn push(&mut self, key: SecurityCutKey, flowgate: Flowgate) {
        self.active.push_back(ActiveSecurityCut {
            key,
            flowgate,
            stale_rounds: 0,
            last_shadow_price_abs: 0.0,
            last_slack_mw: 0.0,
        });
    }

    fn extend_network(&self, network: &mut Network) {
        network
            .flowgates
            .extend(self.active.iter().map(|cut| cut.flowgate.clone()));
    }

    fn active_flowgates(&self) -> Vec<Flowgate> {
        self.active.iter().map(|cut| cut.flowgate.clone()).collect()
    }

    fn refresh_activity(&mut self, sol: &RawDispatchSolution, base_flowgate_count: usize) {
        const SHADOW_ACTIVE_TOL: f64 = 1e-7;
        const SLACK_ACTIVE_TOL_MW: f64 = 1e-4;

        let mut positive_slack_flowgates: Vec<HashSet<&str>> =
            Vec::with_capacity(sol.periods.len());
        for period in &sol.periods {
            let mut positive = HashSet::new();
            for constraint in &period.constraint_results {
                if constraint.slack_mw.unwrap_or(0.0) <= SLACK_ACTIVE_TOL_MW {
                    continue;
                }
                let Some(name) =
                    constraint
                        .constraint_id
                        .strip_prefix("flowgate:")
                        .and_then(|rest| {
                            rest.strip_suffix(":reverse")
                                .or_else(|| rest.strip_suffix(":forward"))
                        })
                else {
                    continue;
                };
                positive.insert(name);
            }
            positive_slack_flowgates.push(positive);
        }

        for (active_idx, cut) in self.active.iter_mut().enumerate() {
            let period = cut.key.period();
            let shadow_abs = sol
                .periods
                .get(period)
                .and_then(|period_result| {
                    period_result
                        .flowgate_shadow_prices
                        .get(base_flowgate_count + active_idx)
                })
                .copied()
                .unwrap_or(0.0)
                .abs();
            let has_positive_slack = positive_slack_flowgates
                .get(period)
                .is_some_and(|names| names.contains(cut.flowgate.name.as_str()));
            cut.last_shadow_price_abs = shadow_abs;
            cut.last_slack_mw = if has_positive_slack {
                SLACK_ACTIVE_TOL_MW
            } else {
                0.0
            };
            if shadow_abs > SHADOW_ACTIVE_TOL || has_positive_slack {
                cut.stale_rounds = 0;
            } else {
                cut.stale_rounds = cut.stale_rounds.saturating_add(1);
            }
        }
    }

    fn remove_pair_for_key(
        key: SecurityCutKey,
        constrained_pairs: &mut HashSet<(usize, usize, usize)>,
        hvdc_constrained_pairs: &mut HashSet<(usize, usize, usize)>,
    ) {
        match key {
            SecurityCutKey::Branch {
                period,
                contingency_branch_idx,
                monitored_branch_idx,
            } => {
                constrained_pairs.remove(&(period, contingency_branch_idx, monitored_branch_idx));
            }
            SecurityCutKey::Hvdc {
                period,
                hvdc_link_idx,
                monitored_branch_idx,
            } => {
                hvdc_constrained_pairs.remove(&(period, hvdc_link_idx, monitored_branch_idx));
            }
        }
    }

    fn retire_marked(
        &mut self,
        retire_indices: &HashSet<usize>,
        constrained_pairs: &mut HashSet<(usize, usize, usize)>,
        hvdc_constrained_pairs: &mut HashSet<(usize, usize, usize)>,
    ) -> usize {
        if retire_indices.is_empty() {
            return 0;
        }

        let mut retained = VecDeque::with_capacity(self.active.len() - retire_indices.len());
        let mut retired_this_round = 0usize;
        for (idx, cut) in self.active.drain(..).enumerate() {
            if retire_indices.contains(&idx) {
                Self::remove_pair_for_key(cut.key, constrained_pairs, hvdc_constrained_pairs);
                retired_this_round += 1;
            } else {
                retained.push_back(cut);
            }
        }
        self.active = retained;
        self.retired_cuts += retired_this_round;
        retired_this_round
    }

    fn retire_inactive_and_enforce_cap(
        &mut self,
        max_active_cuts: Option<usize>,
        cut_retire_after_rounds: Option<usize>,
        constrained_pairs: &mut HashSet<(usize, usize, usize)>,
        hvdc_constrained_pairs: &mut HashSet<(usize, usize, usize)>,
    ) -> usize {
        let mut retire_indices = HashSet::new();

        if let Some(rounds) = cut_retire_after_rounds.filter(|rounds| *rounds > 0) {
            for (idx, cut) in self.active.iter().enumerate() {
                if cut.stale_rounds >= rounds {
                    retire_indices.insert(idx);
                }
            }
        }

        if let Some(max_active_cuts) = max_active_cuts {
            let remaining_after_age = self.active.len().saturating_sub(retire_indices.len());
            if remaining_after_age > max_active_cuts {
                let excess = remaining_after_age - max_active_cuts;
                let mut ranked: Vec<(usize, usize)> = self
                    .active
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| !retire_indices.contains(idx))
                    .map(|(idx, cut)| (idx, cut.stale_rounds))
                    .collect();
                ranked.sort_by(|a, b| {
                    b.1.cmp(&a.1)
                        // On equal staleness, retire older cuts first.
                        .then_with(|| a.0.cmp(&b.0))
                });
                for (idx, _) in ranked.into_iter().take(excess) {
                    retire_indices.insert(idx);
                }
            }
        }

        self.retire_marked(&retire_indices, constrained_pairs, hvdc_constrained_pairs)
    }
}

fn effective_new_cut_cap(
    options: &SecurityDispatchSpec,
    n_branch_violations: usize,
    n_hvdc_violations: usize,
) -> usize {
    let max_cap = options.max_cuts_per_iteration;
    if max_cap == 0 {
        return 0;
    }
    let remaining = n_branch_violations.saturating_add(n_hvdc_violations);
    match options.cut_strategy {
        SecurityCutStrategy::Fixed => max_cap,
        SecurityCutStrategy::Adaptive
            if remaining <= options.targeted_cut_threshold && options.targeted_cut_cap > 0 =>
        {
            max_cap.min(options.targeted_cut_cap)
        }
        SecurityCutStrategy::Adaptive => max_cap,
    }
}

#[derive(Debug, Clone)]
struct HourlyBranchContingency {
    branch_idx: usize,
    from_idx: usize,
    to_idx: usize,
    denom: f64,
}

#[derive(Debug, Clone)]
struct HourlyHvdcContingency {
    hvdc_idx: usize,
    from_idx: usize,
    to_idx: usize,
    band_loss_b: Vec<f64>,
}

struct HourlySecurityContext {
    monitored: Vec<usize>,
    /// Parallel to `monitored`: bus-index of the from-bus endpoint.
    /// Precomputed here so the screener's hot inner loop doesn't hit
    /// `bus_index_map()` (a fresh `HashMap` build) on every pair.
    monitored_from_idx: Vec<usize>,
    /// Parallel to `monitored`: bus-index of the to-bus endpoint.
    monitored_to_idx: Vec<usize>,
    /// Parallel to `monitored`: DC susceptance `b = -1/x` in p.u.
    /// Precomputed to avoid calling `Branch::b_dc()` inside the hot loop.
    monitored_b_dc: Vec<f64>,
    /// Parallel to `monitored`: phase-shift in radians.
    monitored_phase_shift_rad: Vec<f64>,
    /// Parallel to `monitored`: post-contingency emergency rating in p.u.
    /// (`rating_for(Emergency) / base`). `f64::INFINITY` when the branch
    /// has no valid rating — the screener then skips it (loop-invariant).
    monitored_limit_pu: Vec<f64>,
    /// `branch_contingencies` values in insertion order — saves a
    /// `HashMap::values()` iter over a never-mutated map, and lets the
    /// screener pair each contingency with its precomputed `flow_k`.
    contingencies: Vec<HourlyBranchContingency>,
    /// Parallel to `contingencies`: contingency branch's DC susceptance.
    /// Precomputed so screening only touches `angles[..]` + these vecs.
    contingency_b_dc: Vec<f64>,
    /// Parallel to `contingencies`: contingency branch's phase-shift rad.
    contingency_phase_shift_rad: Vec<f64>,
    ptdf: surge_dc::PtdfRows,
    branch_contingencies: HashMap<usize, HourlyBranchContingency>,
    connectivity_contingency_branches: Vec<usize>,
    hvdc_contingencies: Vec<HourlyHvdcContingency>,
}

const MAX_CONNECTIVITY_CUT_ROUNDS: usize = 3;

fn max_dense_ptdf_cache_bytes() -> Option<usize> {
    for var in [
        "SURGE_SCUC_MAX_DENSE_PTDF_CACHE_BYTES",
        "SURGE_SCUC_MAX_LOSS_PTDF_CACHE_BYTES",
    ] {
        if let Ok(value) = std::env::var(var)
            && let Ok(bytes) = value.trim().parse::<usize>()
            && bytes > 0
        {
            return Some(bytes);
        }
    }
    for var in [
        "SURGE_SCUC_MAX_DENSE_PTDF_CACHE_GIB",
        "SURGE_SCUC_MAX_LOSS_PTDF_CACHE_GIB",
    ] {
        if let Ok(value) = std::env::var(var)
            && let Ok(gib) = value.trim().parse::<f64>()
            && gib.is_finite()
            && gib > 0.0
        {
            return Some((gib * 1024.0 * 1024.0 * 1024.0) as usize);
        }
    }
    None
}

fn estimate_dense_ptdf_bytes(hourly_networks: &[Network]) -> usize {
    let n_values: u128 = hourly_networks
        .iter()
        .map(|network| network.n_branches() as u128 * network.n_buses() as u128)
        .sum();
    n_values
        .saturating_mul(std::mem::size_of::<f64>() as u128)
        .min(usize::MAX as u128) as usize
}

fn gib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn security_context_ptdf_fits_budget(hourly_networks: &[Network]) -> bool {
    let Some(limit) = max_dense_ptdf_cache_bytes() else {
        return true;
    };
    let estimate = estimate_dense_ptdf_bytes(hourly_networks);
    if estimate <= limit {
        return true;
    }
    warn!(
        estimated_gib = gib(estimate),
        limit_gib = gib(limit),
        n_periods = hourly_networks.len(),
        "SCUC security-context PTDF cache exceeds memory budget"
    );
    false
}

fn build_hourly_security_contexts(
    hourly_networks: &[Network],
    options: &SecurityDispatchSpec,
    min_rate: f64,
) -> Result<(Vec<HourlySecurityContext>, f64), ScedError> {
    if !security_context_ptdf_fits_budget(hourly_networks) {
        let limit = max_dense_ptdf_cache_bytes().expect("budget checked above");
        return Err(ScedError::SolverError(format!(
            "SCUC security screening requires an estimated {:.1} GiB dense PTDF cache, above the {:.1} GiB budget. Increase SURGE_SCUC_MAX_DENSE_PTDF_CACHE_GIB or reduce security screening scope.",
            gib(estimate_dense_ptdf_bytes(hourly_networks)),
            gib(limit)
        )));
    }
    let start = std::time::Instant::now();
    let contexts: Vec<HourlySecurityContext> = hourly_networks
        .iter()
        .map(|hourly_network| build_hourly_security_context(hourly_network, options, min_rate))
        .collect::<Result<_, _>>()?;
    let secs = start.elapsed().as_secs_f64();
    info!(
        stage = "security_setup",
        hourly_contexts_secs = secs,
        n_periods = hourly_networks.len(),
        n_contingency_branches = options.contingency_branches.len(),
        "Security SCUC setup: hourly contingency contexts built"
    );
    Ok((contexts, secs))
}

/// Per-unit magnitude cutoff below which a `(contingency, monitored
/// branch)` LODF pair is not emitted as a contingency cut in the
/// explicit-N-1 SCUC LP. Pairs with `|lodf| < cutoff` produce a
/// post-contingency flow contribution of at most
/// `cutoff × pre-contingency flow`, which rounds to near-zero in
/// Gurobi's coefficient tolerance and is presolved away anyway.
///
/// 0.01 is tight enough to retain every practically-binding cut
/// (any pair that can actually drive a post-contingency overload
/// under realistic dispatch shifts), while still dropping the long
/// tail of effectively-zero LODF pairs that would otherwise bloat
/// the LP. Production SCUC tools typically sit in the 0.01–0.05
/// range; 0.01 is the conservative end.
///
/// On 617-bus D1 explicit N-1 (562 contingencies × ~850 monitored
/// × 18 periods = 8.6M mathematical LODF pairs), this filter
/// retains the binding cuts and drops the trivially-zero tail.
/// The iterative SCUC path doesn't consult this cutoff — it only
/// materialises cuts for pairs that actually violate (and uses
/// `violation_tolerance_pu` for that test).
const DEFAULT_CONTINGENCY_CUT_LODF_CUTOFF_PU: f64 = 0.01;

fn solved_switching_state_for_period(
    solution: &RawDispatchSolution,
    hourly_network: &Network,
    period: usize,
) -> Vec<bool> {
    solution
        .branch_commitment_state
        .get(period)
        .cloned()
        .unwrap_or_else(|| {
            hourly_network
                .branches
                .iter()
                .map(|branch| branch.in_service)
                .collect()
        })
}

fn push_connectivity_cut(
    connectivity_cuts: &mut Vec<super::connectivity::IndexedConnectivityCut>,
    period: usize,
    mut cut_set: Vec<usize>,
) -> bool {
    cut_set.sort_unstable();
    cut_set.dedup();
    if connectivity_cuts
        .iter()
        .any(|existing| existing.period == period && existing.cut_set == cut_set)
    {
        return false;
    }
    connectivity_cuts.push(super::connectivity::IndexedConnectivityCut { period, cut_set });
    true
}

fn add_connectivity_cuts_from_solution(
    solution: &RawDispatchSolution,
    hourly_networks: &[Network],
    hourly_contexts: &[HourlySecurityContext],
    connectivity_cuts: &mut Vec<super::connectivity::IndexedConnectivityCut>,
    iteration_label: &str,
    iteration_value: usize,
    refit_round: usize,
) -> bool {
    let mut new_cuts_added = false;
    for (period, (hourly_network, context)) in hourly_networks
        .iter()
        .zip(hourly_contexts.iter())
        .enumerate()
    {
        let switching_state = solved_switching_state_for_period(solution, hourly_network, period);
        for outaged_branch_idx in std::iter::once(None).chain(
            context
                .connectivity_contingency_branches
                .iter()
                .copied()
                .map(Some),
        ) {
            match super::connectivity::check_period_connectivity(
                hourly_network,
                &switching_state,
                outaged_branch_idx,
                None,
            ) {
                super::connectivity::ConnectivityCheck::Connected => {}
                super::connectivity::ConnectivityCheck::Disconnected { cut_set } => {
                    if cut_set.is_empty() {
                        warn!(
                            connectivity_iteration_kind = iteration_label,
                            connectivity_iteration = iteration_value,
                            refit_round,
                            period,
                            contingency_branch_idx = outaged_branch_idx,
                            "SCUC: disconnected switching graph has empty cut set; no \
                             available branch can re-connect it under the current \
                             contingency."
                        );
                        continue;
                    }
                    if push_connectivity_cut(connectivity_cuts, period, cut_set.clone()) {
                        debug!(
                            connectivity_iteration_kind = iteration_label,
                            connectivity_iteration = iteration_value,
                            refit_round,
                            period,
                            contingency_branch_idx = outaged_branch_idx,
                            cut_set_len = cut_set.len(),
                            "SCUC: adding connectivity cut"
                        );
                        new_cuts_added = true;
                    }
                }
            }
        }
    }
    new_cuts_added
}

/// Solve security-constrained dispatch from normalized internal options.
///
/// Algorithm:
/// 1. Solve base SCUC (with any user-provided flowgates).
/// 2. Build hourly network snapshots after applying dispatch profiles.
/// 3. For each hour, compute PTDF rows and contingency metadata for that
///    hour's topology and branch limits.
/// 4. Screen branch and HVDC contingencies against the solved hourly angles and
///    dispatch.
/// 5. Convert the worst violations into hour-specific [`Flowgate`] constraints
///    that are active only in the violating hour.
/// 6. Re-solve SCUC with the augmented flowgate set.
/// 7. Repeat until no violations or `max_iterations` reached.
pub(crate) fn solve_security_dispatch(
    network: &Network,
    options: &SecurityDispatchSpec,
) -> Result<RawDispatchSolution, ScedError> {
    let base = network.base_mva;
    let scuc_spec = DispatchProblemSpec::from_request(&options.input, &options.commitment);
    let n_bus = network.n_buses();
    let n_periods = options.input.n_periods;
    let min_rate = options.input.min_rate_a;

    // Active flowgates from iterative security screening. The pool owns
    // retention/retirement so the MIP model does not grow without bound on
    // very large cases.
    let mut security_cut_pool = SecurityCutPool::default();

    let mut last_solution: Option<RawDispatchSolution> = None;
    let mut total_cuts = 0usize;
    let mut last_branch_violations = 0usize;
    let mut last_hvdc_violations = 0usize;
    let mut last_max_branch_violation_pu: Option<f64> = None;
    let mut last_max_hvdc_violation_pu: Option<f64> = None;
    let mut last_solved_security_flowgates: Vec<Flowgate> = Vec::new();

    if options.max_iterations == 0 {
        let sol = solve_scuc_with_problem_spec(network, scuc_spec)?;
        return Ok(attach_security_metadata(
            sol,
            SecurityDispatchMetadata {
                iterations: 0,
                n_cuts: 0,
                active_cuts: 0,
                retired_cuts: 0,
                converged: true,
                last_branch_violations: 0,
                last_hvdc_violations: 0,
                max_branch_violation_pu: None,
                max_hvdc_violation_pu: None,
                n_preseed_cuts: 0,
                n_preseed_pairs_binding: None,
                setup_timings_secs: None,
                per_iteration: Vec::new(),
                near_binding_contingencies: Vec::new(),
            },
        ));
    }

    let _t_hourly_nets = std::time::Instant::now();
    let hourly_networks: Vec<Network> = (0..n_periods)
        .map(|hour| super::snapshot::network_at_hour_with_spec(network, &scuc_spec, hour))
        .collect();
    let hourly_networks_secs = _t_hourly_nets.elapsed().as_secs_f64();
    let mut hourly_contexts: Option<Vec<HourlySecurityContext>> = None;
    let mut hourly_contexts_secs = 0.0_f64;
    info!(
        stage = "security_setup",
        hourly_networks_secs, n_periods, "Security SCUC setup: hourly network snapshots built"
    );

    let mut constrained_pairs: HashSet<(usize, usize, usize)> = HashSet::new();
    let mut hvdc_constrained_pairs: HashSet<(usize, usize, usize)> = HashSet::new();

    // Accumulated connectivity cuts across the security iteration
    // loop. We cap the cut-refit inner loop at 5 rounds per outer
    // iteration — in practice the LP usually converges to a connected
    // pattern within 1-2 rounds; the cap is a safety net for
    // pathological topologies.
    let mut connectivity_cuts: Vec<super::connectivity::IndexedConnectivityCut> = Vec::new();

    info!(
        max_iterations = options.max_iterations,
        max_cuts_per_iteration = options.max_cuts_per_iteration,
        violation_tolerance_pu = options.violation_tolerance_pu,
        n_branch_contingencies = options.contingency_branches.len(),
        n_hvdc_contingencies = options.hvdc_contingency_indices.len(),
        n_periods,
        "Iterative Security SCUC: starting loop"
    );

    // Pre-seed iter 0 with top-ranked (ctg, mon) cuts so the first SCUC
    // already sees a skeleton of structurally-binding N-1 pairs instead
    // of solving blind. The screener dedup set is primed with the same
    // keys so the post-solve pass won't re-materialize them. See
    // `preseed_branch_flowgates` for the ranking heuristic.
    let mut preseed_secs_total = 0.0_f64;
    let n_preseed_cuts = if options.preseed_count_per_period > 0
        && !matches!(options.preseed_method, SecurityPreseedMethod::None)
    {
        let preseed_start = std::time::Instant::now();
        if hourly_contexts.is_none() {
            let (contexts, secs) =
                build_hourly_security_contexts(&hourly_networks, options, min_rate)?;
            hourly_contexts_secs += secs;
            hourly_contexts = Some(contexts);
        }
        let contexts = hourly_contexts
            .as_ref()
            .expect("hourly contexts built for security preseed");
        let preseeded = preseed_branch_flowgates(
            options,
            &hourly_networks,
            contexts,
            n_periods,
            &mut constrained_pairs,
        );
        let n = preseeded.len();
        for (key, flowgate) in preseeded {
            security_cut_pool.push(key, flowgate);
        }
        total_cuts += n;
        preseed_secs_total = preseed_start.elapsed().as_secs_f64();
        info!(
            method = ?options.preseed_method,
            count_per_period = options.preseed_count_per_period,
            n_preseed_cuts = n,
            preseed_secs = preseed_secs_total,
            "Security SCUC: pre-seeded iter 0 with structural N-1 cuts"
        );
        n
    } else {
        0
    };

    // Cross-iteration MIP warm-start option. When a caller supplies a
    // `warm_start_commitment` via policy, the light warm-start path in
    // `solve_problem` threads it into the MIP's `Start` attribute. We
    // *do not* auto-capture this iter's commitment to feed the next
    // iter — empirically on 617-bus (measured 2026-04-17) it regresses
    // wall time: Gurobi treats the prior commitment as infeasible
    // under the new cut set and spends time repairing rather than
    // reusing. Infrastructure stays wired (policy-supplied hints still
    // apply); the auto-capture step is left off as a knob to flip on
    // when workload dynamics favor it.
    let last_commitment: Option<Vec<Vec<bool>>> = None;

    // Cross-iteration loss-factor warm-start cache. Each security
    // iteration's SCUC solve emits a final `(dloss_dp, total_losses_mw)`
    // state on `RawDispatchSolution.scuc_final_loss_warm_start`. We
    // catch it and feed it into the next iteration's solve, so the
    // pre-MIP application in `scuc::problem::solve_problem` can scale
    // bus-balance rows + injection coefficients with a close-to-optimum
    // loss estimate before the first MIP call. On cases where the
    // commitment doesn't change much between security iterations, this
    // elides the post-MIP loss-factor LP re-solve (~40+ s per iter on
    // 617-bus D2) because the lossless-MIP dispatch already matches
    // the warm-started optimum.
    let mut cached_loss_warm_start: Option<crate::scuc::losses::LossFactorWarmStart> = None;
    // Whether `cached_loss_warm_start` reflects realized LFs from a
    // prior iteration's repaired theta (true) or only the cold-start
    // warm start / nothing (false). Set true after the first LF
    // update block runs successfully. Used to suppress the normal
    // "no violations on iter 0 → exit" early return until at least
    // one SCUC solve has actually consumed realized LFs — without
    // this, ScalarFeedback / PenaltyFactors silently no-op on
    // scenarios with a clean contingency profile because they only
    // refresh between iterations and the security loop exits before
    // any feedback can land.
    let mut cached_lfs_are_realized = false;

    // Cold-start loss-factor warm start for iter 0. Only runs when
    // loss factors are enabled AND the policy opts in via
    // `loss_factor_warm_start_mode`. Each mode has its own cost/accuracy
    // tradeoff — see `crate::scuc::losses` for the mode implementations.
    // Falls through gracefully to `None` if the compute fails; the
    // security loop then falls back to lossless-MIP + refinement-LP as
    // before.
    // The cold-start warm-start pre-adjusts the per-bus balance row RHS
    // with an estimate of `dloss_dp`. When bus balance rows don't exist
    // (disable knob), the warm-start has nowhere to land — skip it.
    // System-level expected loss is handled directly in
    // `build_system_power_balance_row`'s RHS instead.
    let mut cold_start_loss_warm_start_secs_total = 0.0_f64;
    if scuc_spec.use_loss_factors
        && !scuc_spec.scuc_disable_bus_power_balance
        && n_bus > 1
        && !matches!(
            scuc_spec.loss_factor_warm_start_mode,
            crate::request::network::LossFactorWarmStartMode::Disabled,
        )
    {
        let _cs_t0 = std::time::Instant::now();
        let cold = build_cold_start_loss_warm_start(
            &scuc_spec,
            network,
            &hourly_networks,
            n_bus,
            n_periods,
        );
        cold_start_loss_warm_start_secs_total = _cs_t0.elapsed().as_secs_f64();
        if let Some(ws) = cold {
            info!(
                stage = "cold_start_loss_warm_start",
                secs = cold_start_loss_warm_start_secs_total,
                mode = ?scuc_spec.loss_factor_warm_start_mode,
                n_hours = ws.dloss_dp.len(),
                total_losses_mw_t0 = ws.total_losses_mw.first().copied().unwrap_or(0.0),
                "Security SCUC: cold-start loss-factor warm start"
            );
            cached_loss_warm_start = Some(ws);
        }
    }

    let mut setup_timings = SecuritySetupTimings {
        hourly_networks_secs,
        hourly_contexts_secs,
        preseed_secs: preseed_secs_total,
        cold_start_loss_warm_start_secs: cold_start_loss_warm_start_secs_total,
    };
    let mut per_iteration: Vec<SecurityIterationReport> = Vec::new();

    // System-row loss treatment per `scuc_loss_treatment`. Only active
    // when the SCUC is in `disable_bus_power_balance` mode — the per-bus
    // path runs `iterate_loss_factors` itself and ignores this field.
    //
    // For both `ScalarFeedback` and `PenaltyFactors` modes we need
    // realized total losses per period (cheap from repaired theta).
    // `PenaltyFactors` additionally computes per-bus loss factors via
    // sparse adjoint solves, avoiding the old dense loss-PTDF cache.
    let sys_row_mode = scuc_spec.scuc_disable_bus_power_balance
        && scuc_spec.use_loss_factors
        && !matches!(
            scuc_spec.scuc_loss_treatment,
            crate::request::network::ScucLossTreatment::Static,
        );
    let need_pf_lfs = sys_row_mode
        && matches!(
            scuc_spec.scuc_loss_treatment,
            crate::request::network::ScucLossTreatment::PenaltyFactors,
        );
    let pf_bus_maps_by_hour: Vec<HashMap<u32, usize>> = if sys_row_mode {
        hourly_networks.iter().map(|n| n.bus_index_map()).collect()
    } else {
        Vec::new()
    };

    for iter in 0..options.max_iterations {
        let iter_start = std::time::Instant::now();
        // Snapshot whether the SCUC solve we're about to run is going
        // to consume realized LFs from a prior iteration. Used by the
        // post-screen "no violations" branch to decide whether the
        // loss feedback has already had a chance to influence the
        // commitment / dispatch (early-exit safe) or whether we need
        // one more iteration to actually apply it.
        let solved_with_realized_lfs = cached_lfs_are_realized;
        // Build network with accumulated security flowgates
        let _t_net_clone = std::time::Instant::now();
        let mut net = network.clone();
        security_cut_pool.extend_network(&mut net);
        let solved_security_flowgates_this_iter = security_cut_pool.active_flowgates();
        let net_clone_secs = _t_net_clone.elapsed().as_secs_f64();

        // Build a per-iter CommitmentMode carrying the prior iter's
        // commitment as `warm_start_commitment`. For iter 0 we reuse
        // the caller's original CommitmentMode unchanged. The owned
        // value must live across the solve call so `iter_spec` can
        // reference it.
        let iter_commitment: CommitmentMode = match (&last_commitment, scuc_spec.commitment) {
            (Some(schedule), CommitmentMode::Optimize(opts)) => {
                let mut next_opts = opts.clone();
                next_opts.warm_start_commitment = Some(schedule.clone());
                next_opts.warm_start_commitment_mask = None;
                CommitmentMode::Optimize(next_opts)
            }
            (
                Some(schedule),
                CommitmentMode::Additional {
                    da_commitment,
                    options,
                },
            ) => {
                let mut next_opts = options.clone();
                next_opts.warm_start_commitment = Some(schedule.clone());
                next_opts.warm_start_commitment_mask = None;
                CommitmentMode::Additional {
                    da_commitment: da_commitment.clone(),
                    options: next_opts,
                }
            }
            _ => scuc_spec.commitment.clone(),
        };
        let iter_scuc_spec = scuc_spec.with_commitment(&iter_commitment);

        // Solve SCUC with any accumulated connectivity cuts threaded
        // through the spec so the LP builder emits them as row family
        // `Σ branch_commitment ≥ 1` over the cut set. The first
        // iteration has no cuts; subsequent rounds grow the cut pool
        // until the LP's switching pattern is connected across every
        // period (or we hit the round cap).
        let mut sol: Option<RawDispatchSolution> = None;
        if iter_scuc_spec.allow_branch_switching {
            if hourly_contexts.is_none() {
                let (contexts, secs) =
                    build_hourly_security_contexts(&hourly_networks, options, min_rate)?;
                setup_timings.hourly_contexts_secs += secs;
                hourly_contexts = Some(contexts);
            }
            let hourly_contexts_for_connectivity = hourly_contexts
                .as_ref()
                .expect("hourly contexts built for connectivity cuts");
            for refit_round in 0..MAX_CONNECTIVITY_CUT_ROUNDS {
                let spec_with_cuts = iter_scuc_spec.with_connectivity_cuts(&connectivity_cuts);
                // Thread the cached loss warm-start into every
                // connectivity-cut refit too — the LP rows added by
                // the cut loop don't change the bus-balance row
                // structure, so the loss factors still apply.
                let round_sol = solve_scuc_with_problem_spec_warm_started(
                    &net,
                    spec_with_cuts,
                    cached_loss_warm_start.clone(),
                )?;
                let new_cuts_added = add_connectivity_cuts_from_solution(
                    &round_sol,
                    &hourly_networks,
                    hourly_contexts_for_connectivity,
                    &mut connectivity_cuts,
                    "outer",
                    iter,
                    refit_round,
                );
                if !new_cuts_added {
                    sol = Some(round_sol);
                    break;
                }
                if refit_round + 1 == MAX_CONNECTIVITY_CUT_ROUNDS {
                    warn!(
                        iter,
                        cuts = connectivity_cuts.len(),
                        "SCUC connectivity cut loop hit {MAX_CONNECTIVITY_CUT_ROUNDS}-round \
                         cap — accepting the last solve even though one or more \
                         periods remain disconnected. A downstream connectivity \
                         penalty may apply."
                    );
                    sol = Some(round_sol);
                }
            }
        }
        let sol = match sol {
            Some(s) => s,
            None => solve_scuc_with_problem_spec_warm_started(
                &net,
                iter_scuc_spec,
                cached_loss_warm_start.clone(),
            )?,
        };
        // Capture the just-solved final loss state so the next security
        // iteration (if any) can skip the lossless-MIP pass.
        if sol
            .scuc_final_loss_warm_start
            .as_ref()
            .map(|w| w.is_populated())
            .unwrap_or(false)
        {
            cached_loss_warm_start = sol.scuc_final_loss_warm_start.clone();
            debug!(
                iter,
                n_hours = cached_loss_warm_start
                    .as_ref()
                    .map(|w| w.dloss_dp.len())
                    .unwrap_or(0),
                "Security SCUC: cached loss-factor warm start for next iteration"
            );
        }
        let inner_solve_secs = iter_start.elapsed().as_secs_f64();
        let inner_mip_trace_this_iter = sol.diagnostics.commitment_mip_trace.clone();

        // With per-bus balance disabled, the MIP's theta is unbound by
        // KCL and meaningless as a flow proxy. Re-solve a DC PF per
        // period with injections matching the SCUC dispatch so the
        // screen below sees physical angles. Sub-second on 6049-bus.
        let mut sol = sol;
        let mut repair_theta_secs = 0.0_f64;
        if scuc_spec.scuc_disable_bus_power_balance {
            let t0 = std::time::Instant::now();
            repair_theta_from_dc_pf(&mut sol, &hourly_networks, &scuc_spec)?;
            repair_theta_secs = t0.elapsed().as_secs_f64();
            info!(
                stage = "security.repair_theta_from_dc_pf",
                secs = repair_theta_secs,
                n_periods = sol.bus_angles_rad.len(),
                "Security SCUC: rebuilt theta via DC PF (disable_bus_power_balance mode)"
            );
        }
        security_cut_pool.refresh_activity(&sol, network.flowgates.len());

        // System-row loss treatment update. After theta is repaired,
        // capture realized losses (and, in PenaltyFactors mode, per-bus
        // LFs in distributed-load slack gauge) so the next security
        // iteration's SCUC sees them on the system-balance row. Skipped
        // when policy is Static or per-bus path runs (which has its own
        // `iterate_loss_factors` machinery via `final_loss_warm_start`).
        let mut sys_row_loss_telemetry: Option<crate::result::SecurityIterationLossTelemetry> =
            None;
        if sys_row_mode && sol.bus_angles_rad.len() == hourly_networks.len() {
            let realized = if need_pf_lfs {
                // PenaltyFactors: full per-bus LFs.
                crate::scuc::penalty_factors::compute_realized_loss_factors(
                    &hourly_networks,
                    &pf_bus_maps_by_hour,
                    &sol.bus_angles_rad,
                )
            } else {
                // ScalarFeedback: just per-period total losses; zero
                // LFs keep the row builder's LHS at face value.
                let mut total_losses_mw = Vec::with_capacity(hourly_networks.len());
                let mut dloss_dp: Vec<Vec<f64>> = Vec::with_capacity(hourly_networks.len());
                for (t, net_t) in hourly_networks.iter().enumerate() {
                    let theta = &sol.bus_angles_rad[t];
                    let bus_map_t = &pf_bus_maps_by_hour[t];
                    let pu = surge_opf::compute_total_dc_losses(net_t, theta, bus_map_t);
                    total_losses_mw.push(pu * net_t.base_mva);
                    // ScalarFeedback path: zero-LF vector keeps the row
                    // builder's LHS at face value. We still populate
                    // n_bus entries so `is_populated()` returns true.
                    dloss_dp.push(vec![0.0; net_t.n_buses()]);
                }
                crate::scuc::losses::LossFactorWarmStart {
                    dloss_dp,
                    total_losses_mw,
                }
            };
            // Damp + cap against the prior iter's cache. Iter 0's prior
            // is `None` (or the per-bus cold-start, which has different
            // semantics — we ignore it here since system-row mode has
            // its own bootstrap path).
            let prior_for_blend = if iter == 0 {
                None
            } else {
                cached_loss_warm_start.as_ref()
            };
            let mut blended = crate::scuc::penalty_factors::blend_with_prior(
                &realized,
                prior_for_blend,
                crate::scuc::penalty_factors::DEFAULT_DAMPING_ALPHA,
                crate::scuc::penalty_factors::DEFAULT_UPWARD_STEP_CAP,
            );
            let cap_hits = crate::scuc::penalty_factors::cap_magnitudes(
                &mut blended,
                crate::scuc::penalty_factors::DEFAULT_LF_MAGNITUDE_CAP,
            );
            let summary = crate::scuc::penalty_factors::summarize(&blended);
            let any_period = summary.per_period.first().cloned();
            info!(
                iter,
                mode = ?scuc_spec.scuc_loss_treatment,
                cap_hits,
                lf_min_p0 = any_period.as_ref().map(|p| p.lf_min).unwrap_or(0.0),
                lf_max_p0 = any_period.as_ref().map(|p| p.lf_max).unwrap_or(0.0),
                lf_p95_abs_p0 = any_period.as_ref().map(|p| p.lf_p95_abs).unwrap_or(0.0),
                total_loss_mw_p0 = any_period.as_ref().map(|p| p.total_losses_mw).unwrap_or(0.0),
                "Security SCUC: cached system-row loss state for next iteration"
            );

            // Build per-period telemetry rows from the blended summary
            // and the realized-but-pre-blend totals.
            let mode_str = match scuc_spec.scuc_loss_treatment {
                crate::request::network::ScucLossTreatment::Static => "static",
                crate::request::network::ScucLossTreatment::ScalarFeedback => "scalar_feedback",
                crate::request::network::ScucLossTreatment::PenaltyFactors => "penalty_factors",
            };
            sys_row_loss_telemetry = Some(crate::result::SecurityIterationLossTelemetry {
                mode: mode_str.to_string(),
                realized_total_loss_mw_per_period: realized.total_losses_mw.clone(),
                blended_total_loss_mw_per_period: blended.total_losses_mw.clone(),
                lf_min_per_period: summary.per_period.iter().map(|p| p.lf_min).collect(),
                lf_max_per_period: summary.per_period.iter().map(|p| p.lf_max).collect(),
                lf_p95_abs_per_period: summary.per_period.iter().map(|p| p.lf_p95_abs).collect(),
                lf_cap_hits: cap_hits,
            });

            cached_loss_warm_start = Some(blended);
            cached_lfs_are_realized = true;
        }

        // If this is the last allowed solve, a fresh security screen
        // can only produce diagnostics and cuts that no later SCUC
        // iteration will consume. On very large cases the screen itself
        // requires a dense PTDF context that can dwarf the MIP. Return
        // the solved dispatch as security-incomplete instead of spending
        // memory on an unused final cut wave.
        if iter + 1 >= options.max_iterations {
            info!(
                iter,
                max_iterations = options.max_iterations,
                "Security SCUC: final solve reached; skipping unused final security screen"
            );
            last_solved_security_flowgates = solved_security_flowgates_this_iter;
            last_solution = Some(sol);
            break;
        }

        if hourly_contexts.is_none() {
            let (contexts, secs) =
                build_hourly_security_contexts(&hourly_networks, options, min_rate)?;
            setup_timings.hourly_contexts_secs += secs;
            hourly_contexts = Some(contexts);
        }
        let hourly_contexts = hourly_contexts
            .as_ref()
            .expect("hourly contexts built before security screening");

        // Check N-1 violations across all periods.
        //
        // Each period's screen is independent of every other period's
        // (hourly_context is per-period-scoped, angles are read-only,
        // constrained_pairs is shared read-only). Fan the period loop
        // across rayon workers so we actually use the box's cores on
        // large cases — per-iter screen on 8316 was ~98s serial.
        use rayon::prelude::*;
        let n_sol_periods = sol.bus_angles_rad.len();
        let branch_retention_cap = options.max_cuts_per_iteration;
        let (branch_acc, mut hvdc_violations) = (0..n_sol_periods)
            .into_par_iter()
            .fold(
                || {
                    (
                        BoundedBranchViolationSet::new(branch_retention_cap),
                        Vec::<HvdcSecurityCut>::new(),
                    )
                },
                |mut acc, t| {
                    let angles = &sol.bus_angles_rad[t];
                    if angles.len() != n_bus {
                        return acc;
                    }
                    let Some(period) = sol.periods.get(t) else {
                        return acc;
                    };
                    let hourly_network = &hourly_networks[t];
                    let context = &hourly_contexts[t];
                    screen_branch_violations_into(
                        t,
                        angles,
                        hourly_network,
                        context,
                        base,
                        options.violation_tolerance_pu,
                        &constrained_pairs,
                        &mut acc.0,
                    );
                    let hv = screen_hvdc_violations(
                        t,
                        angles,
                        period,
                        hourly_network,
                        context,
                        &options.input.hvdc_links,
                        base,
                        options.violation_tolerance_pu,
                        &hvdc_constrained_pairs,
                    );
                    acc.1.extend(hv);
                    acc
                },
            )
            .reduce(
                || {
                    (
                        BoundedBranchViolationSet::new(branch_retention_cap),
                        Vec::<HvdcSecurityCut>::new(),
                    )
                },
                |mut left, right| {
                    left.0.merge(right.0);
                    left.1.extend(right.1);
                    left
                },
            );
        let branch_screen = branch_acc.finish();
        let mut violations = branch_screen.violations;

        last_branch_violations = branch_screen.n_violations;
        last_hvdc_violations = hvdc_violations.len();
        last_max_branch_violation_pu = branch_screen.max_violation_pu;
        last_max_hvdc_violation_pu = hvdc_violations.iter().fold(None, |acc, cut| {
            let severity = cut.excess_pu;
            Some(acc.map_or(severity, |prev| prev.max(severity)))
        });

        let screen_secs = iter_start.elapsed().as_secs_f64() - inner_solve_secs - repair_theta_secs;
        info!(
            iter,
            net_clone_secs,
            inner_solve_secs,
            repair_theta_secs,
            screen_secs,
            "Security SCUC iter breakdown"
        );

        if last_branch_violations == 0 && hvdc_violations.is_empty() {
            info!(
                iterations = iter + 1,
                n_security_cuts = total_cuts,
                inner_solve_secs,
                repair_theta_secs,
                screen_secs,
                total_iter_secs = iter_start.elapsed().as_secs_f64(),
                "Security SCUC converged — no N-1 violations"
            );
            per_iteration.push(SecurityIterationReport {
                iter,
                net_clone_secs,
                inner_solve_secs,
                repair_theta_secs,
                screen_secs,
                cut_build_secs: 0.0,
                n_branch_violations: last_branch_violations,
                n_hvdc_violations: last_hvdc_violations,
                max_branch_violation_pu: last_max_branch_violation_pu,
                max_hvdc_violation_pu: last_max_hvdc_violation_pu,
                new_cuts: 0,
                active_cuts: security_cut_pool.active_len(),
                retired_cuts: 0,
                inner_mip_trace: inner_mip_trace_this_iter,
                sys_row_loss_telemetry: sys_row_loss_telemetry.clone(),
            });

            // System-row loss feedback gate. If sys-row loss treatment
            // (ScalarFeedback / PenaltyFactors) is active and this
            // iter's solve was done with cold-start LFs only, we
            // can't exit yet — the realized LFs cached at end of this
            // iter haven't influenced any SCUC solve. Continue to the
            // next iteration so the next SCUC build consumes them. If
            // that iteration also finds no violations,
            // `solved_with_realized_lfs` will be true on entry and the
            // normal exit path runs.
            if sys_row_mode && !solved_with_realized_lfs {
                info!(
                    iter,
                    mode = ?scuc_spec.scuc_loss_treatment,
                    "Security SCUC: no contingency violations, but sys-row loss feedback has not yet been applied. Continuing to consume realized LFs from this iter."
                );
                last_solved_security_flowgates = solved_security_flowgates_this_iter;
                last_solution = Some(sol);
                continue;
            }

            let near_binding = if options.near_binding_report {
                compute_near_binding_report(&sol, &hourly_networks, hourly_contexts, base, n_bus)
            } else {
                Vec::new()
            };
            return Ok(attach_near_binding_report(
                attach_aux_flowgate_names(
                    attach_security_metadata(
                        sol,
                        SecurityDispatchMetadata {
                            iterations: iter + 1,
                            n_cuts: total_cuts,
                            active_cuts: security_cut_pool.active_len(),
                            retired_cuts: security_cut_pool.retired_cuts,
                            converged: true,
                            last_branch_violations,
                            last_hvdc_violations,
                            max_branch_violation_pu: last_max_branch_violation_pu,
                            max_hvdc_violation_pu: last_max_hvdc_violation_pu,
                            n_preseed_cuts,
                            n_preseed_pairs_binding: None,
                            setup_timings_secs: Some(setup_timings),
                            per_iteration,
                            near_binding_contingencies: Vec::new(),
                        },
                    ),
                    network,
                    &security_cut_pool.active_flowgates(),
                ),
                near_binding,
            ));
        }

        let _t_cut_build = std::time::Instant::now();
        // Sort by severity (worst first) and take top max_cuts_per_iteration unique pairs
        violations.sort_by(compare_branch_violation_severity_desc);
        hvdc_violations.sort_by(|a, b| {
            b.excess_pu
                .partial_cmp(&a.excess_pu)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let cut_cap = effective_new_cut_cap(options, last_branch_violations, last_hvdc_violations);
        let mut new_cuts = 0usize;
        for violation in &violations {
            if new_cuts >= cut_cap {
                break;
            }
            let pair_key = (
                violation.period,
                violation.contingency_branch_idx,
                violation.monitored_branch_idx,
            );
            if constrained_pairs.contains(&pair_key) {
                continue;
            }
            constrained_pairs.insert(pair_key);

            let context = &hourly_contexts[violation.period];
            let hourly_network = &hourly_networks[violation.period];
            let fg = build_branch_security_flowgate(
                violation,
                hourly_network,
                context,
                n_periods,
                false, // iterative cut — breach side is known
            );
            let br_l = &hourly_network.branches[violation.monitored_branch_idx];
            let br_k = &hourly_network.branches[violation.contingency_branch_idx];

            debug!(
                iter,
                period = violation.period,
                ctg = format!("{}-{}", br_k.from_bus, br_k.to_bus),
                mon = format!("{}-{}", br_l.from_bus, br_l.to_bus),
                severity_pu = violation.severity_pu,
                limit_mw = br_l.rating_for(BranchRatingCondition::Emergency),
                "Adding N-1 branch security flowgate"
            );

            security_cut_pool.push(
                SecurityCutKey::Branch {
                    period: violation.period,
                    contingency_branch_idx: violation.contingency_branch_idx,
                    monitored_branch_idx: violation.monitored_branch_idx,
                },
                fg,
            );
            new_cuts += 1;
        }

        // --- Generate HVDC security cuts ---
        // Each HVDC cut creates a flowgate on the monitored branch with an
        // hvdc_coefficients entry encoding the OTDF.
        // Constraint: b_dc_l*(θ_from - θ_to) + (-OTDF_lk)*P_hvdc[k] ∈ [-f_max, f_max]
        for cut in &hvdc_violations {
            if new_cuts >= cut_cap {
                break;
            }
            let k = cut.hvdc_link_idx;
            let l = cut.monitored_branch_idx;
            let pair_key = (cut.period, k, l);
            if hvdc_constrained_pairs.contains(&pair_key) {
                continue;
            }
            hvdc_constrained_pairs.insert(pair_key);

            let hourly_network = &hourly_networks[cut.period];
            let context = &hourly_contexts[cut.period];
            let fg = build_hvdc_security_flowgate(cut, hourly_network, context, n_periods);
            let br_l = &hourly_network.branches[l];
            let hvdc = &options.input.hvdc_links[k];

            debug!(
                iter,
                period = cut.period,
                hvdc_link = %hvdc.name,
                mon = format!("{}-{}", br_l.from_bus, br_l.to_bus),
                limit_mw = cut.f_max_mw,
                "Adding N-1 HVDC security flowgate"
            );

            security_cut_pool.push(
                SecurityCutKey::Hvdc {
                    period: cut.period,
                    hvdc_link_idx: k,
                    monitored_branch_idx: l,
                },
                fg,
            );
            new_cuts += 1;
        }

        total_cuts += new_cuts;
        let retired_this_round = security_cut_pool.retire_inactive_and_enforce_cap(
            options.max_active_cuts,
            options.cut_retire_after_rounds,
            &mut constrained_pairs,
            &mut hvdc_constrained_pairs,
        );
        let cut_build_secs = _t_cut_build.elapsed().as_secs_f64();
        info!(
            iter = iter + 1,
            n_branch_violations = last_branch_violations,
            n_hvdc_violations = last_hvdc_violations,
            max_branch_violation_pu = last_max_branch_violation_pu.unwrap_or(0.0),
            max_hvdc_violation_pu = last_max_hvdc_violation_pu.unwrap_or(0.0),
            new_cuts,
            active_cuts = security_cut_pool.active_len(),
            retired_cuts = retired_this_round,
            total_cuts,
            inner_solve_secs,
            repair_theta_secs,
            screen_secs,
            cut_build_secs,
            "Security SCUC: added cuts, re-solving"
        );

        per_iteration.push(SecurityIterationReport {
            iter,
            net_clone_secs,
            inner_solve_secs,
            repair_theta_secs,
            screen_secs,
            cut_build_secs,
            n_branch_violations: last_branch_violations,
            n_hvdc_violations: last_hvdc_violations,
            max_branch_violation_pu: last_max_branch_violation_pu,
            max_hvdc_violation_pu: last_max_hvdc_violation_pu,
            new_cuts,
            active_cuts: security_cut_pool.active_len(),
            retired_cuts: retired_this_round,
            inner_mip_trace: inner_mip_trace_this_iter,
            sys_row_loss_telemetry: sys_row_loss_telemetry.clone(),
        });

        last_solved_security_flowgates = solved_security_flowgates_this_iter;
        last_solution = Some(sol);
    }

    // Max iterations reached — return last solution
    warn!(
        max_iterations = options.max_iterations,
        remaining_violations = "unknown",
        "Security SCUC: max iterations reached"
    );
    let last_solution = last_solution.expect("loop ran at least one iteration");
    let near_binding = if options.near_binding_report {
        if let Some(hourly_contexts) = hourly_contexts.as_ref() {
            compute_near_binding_report(
                &last_solution,
                &hourly_networks,
                hourly_contexts,
                base,
                n_bus,
            )
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    Ok(attach_near_binding_report(
        attach_aux_flowgate_names(
            attach_security_metadata(
                last_solution,
                SecurityDispatchMetadata {
                    iterations: options.max_iterations,
                    n_cuts: total_cuts,
                    active_cuts: last_solved_security_flowgates.len(),
                    retired_cuts: security_cut_pool.retired_cuts,
                    converged: false,
                    last_branch_violations,
                    last_hvdc_violations,
                    max_branch_violation_pu: last_max_branch_violation_pu,
                    max_hvdc_violation_pu: last_max_hvdc_violation_pu,
                    n_preseed_cuts,
                    n_preseed_pairs_binding: None,
                    setup_timings_secs: Some(setup_timings),
                    per_iteration,
                    near_binding_contingencies: Vec::new(),
                },
            ),
            network,
            &last_solved_security_flowgates,
        ),
        near_binding,
    ))
}

/// Compute a cold-start [`LossFactorWarmStart`] per the policy mode.
///
/// Called on iter 0 of the security loop when the caller opted in via
/// `DispatchProblemSpec::loss_factor_warm_start_mode`. Dispatches to
/// the appropriate loss-estimate source; handles all the per-period
/// bus-load setup that the chosen source needs.
///
/// Returns `None` when loss factors are off or the mode is Disabled —
/// callers should check this and skip the warm-start application.
fn build_cold_start_loss_warm_start(
    spec: &DispatchProblemSpec<'_>,
    _network: &surge_network::Network,
    hourly_networks: &[surge_network::Network],
    n_bus: usize,
    n_periods: usize,
) -> Option<crate::scuc::losses::LossFactorWarmStart> {
    use crate::request::network::LossFactorWarmStartMode;

    // Shared: per-period bus load and total load. Two sources must be
    // summed for GO C3-style markets:
    //   1. `network.loads` — fixed-demand loads (traditional PSS/E
    //      Load objects). Included via `bus_load_p_mw_with_map`, the
    //      same helper `bus_p_injection_pu` uses, so our vector
    //      matches the injection side exactly (Loads minus
    //      PowerInjections).
    //   2. `spec.dispatchable_loads` — demand-response / elastic
    //      load resources. On GO C3 these carry ALL the demand (the
    //      network's `loads` vec is empty at each hour), so missing
    //      them produced `total_load_by_hour = 0` on every period,
    //      which in turn made `build_uniform_loss_warm_start` return
    //      an all-zero warm start and `build_load_pattern_*` skip
    //      every period via the `<= 1e-6` gate — silently inert.
    //
    // For dispatchable loads we use the zero-price warm-start target
    // (same quantity `estimated_dispatchable_load_target_mw` uses
    // when sizing the MIP). This is the LP's expected served demand
    // at zero price; it accurately represents the load the MIP will
    // plan for.
    let mut bus_load_mw_by_hour: Vec<Vec<f64>> = Vec::with_capacity(n_periods);
    let mut total_load_by_hour: Vec<f64> = Vec::with_capacity(n_periods);
    for (hour, network_t) in hourly_networks.iter().enumerate() {
        let map_t = network_t.bus_index_map();
        let mut bus_load = network_t.bus_load_p_mw_with_map(&map_t);

        let base_mva = network_t.base_mva.max(1.0);
        for (dl_idx, dl) in spec.dispatchable_loads.iter().enumerate() {
            if !dl.in_service {
                continue;
            }
            let target_mw = crate::scuc::problem::dispatchable_load_warm_start_target_mw_pub(
                dl_idx, hour, dl, spec, base_mva,
            );
            if target_mw <= 0.0 {
                continue;
            }
            if let Some(&bus_idx) = map_t.get(&dl.bus) {
                if bus_idx < bus_load.len() {
                    bus_load[bus_idx] += target_mw;
                }
            }
        }

        total_load_by_hour.push(bus_load.iter().map(|v| v.max(0.0)).sum());
        bus_load_mw_by_hour.push(bus_load);
    }

    match spec.loss_factor_warm_start_mode {
        LossFactorWarmStartMode::Disabled => None,
        LossFactorWarmStartMode::Uniform { rate } => Some(
            crate::scuc::losses::build_uniform_loss_warm_start(n_bus, &total_load_by_hour, rate),
        ),
        LossFactorWarmStartMode::LoadPattern { rate } => {
            Some(crate::scuc::losses::build_load_pattern_loss_warm_start(
                hourly_networks,
                &bus_load_mw_by_hour,
                &total_load_by_hour,
                n_bus,
                rate,
            ))
        }
        LossFactorWarmStartMode::DcPf => Some(crate::scuc::losses::build_dc_pf_loss_warm_start(
            hourly_networks,
            &bus_load_mw_by_hour,
            &total_load_by_hour,
            n_bus,
        )),
    }
}

fn attach_security_metadata(
    mut dispatch: RawDispatchSolution,
    metadata: SecurityDispatchMetadata,
) -> RawDispatchSolution {
    dispatch.diagnostics.security = Some(metadata);
    dispatch
}

/// Set ``aux_flowgate_names`` to the augmented (caller network +
/// accumulated security cuts) flowgate name list. The dispatch
/// extraction path keys ``period.flowgate_shadow_prices`` and the
/// slack-positive ``constraint_results`` entries by inner-LP
/// flowgate index, but ``attach_keyed_period_views`` only sees the
/// caller's network. Without this attachment, every contingency cut
/// surfaces as ``flowgate:{idx}`` instead of its ``N1_t...`` name.
fn attach_aux_flowgate_names(
    mut dispatch: RawDispatchSolution,
    network: &Network,
    security_flowgates: &[surge_network::network::Flowgate],
) -> RawDispatchSolution {
    if security_flowgates.is_empty() {
        return dispatch;
    }
    dispatch.aux_flowgate_names = network
        .flowgates
        .iter()
        .chain(security_flowgates.iter())
        .map(|fg| fg.name.clone())
        .collect();
    dispatch
}

/// Threshold for the near-binding contingency report. Cuts with
/// post-contingency `|flow| / limit ≥ NEAR_BINDING_THRESHOLD` make
/// it into ``SecurityDispatchMetadata::near_binding_contingencies``.
/// At 0.95 the report only carries cuts within 5 % of their limit
/// (the ones operators actually care about); the triangle-inequality
/// prescreen drops everything well inside the band before any LODF
/// or post-flow compute, keeping the screen cheap on large networks.
const NEAR_BINDING_THRESHOLD: f64 = 0.95;

/// Run the near-binding screen across all periods on the converged
/// dispatch and collect the post-contingency flow report. Parallel
/// per-period (mirrors the violation-screen pattern); empty when the
/// caller didn't populate ``sol.bus_angles_rad``.
fn compute_near_binding_report(
    sol: &RawDispatchSolution,
    hourly_networks: &[Network],
    hourly_contexts: &[HourlySecurityContext],
    base: f64,
    n_bus: usize,
) -> Vec<crate::result::NearBindingContingency> {
    use rayon::prelude::*;
    let n_periods = sol.bus_angles_rad.len();
    if n_periods == 0 {
        return Vec::new();
    }
    let per_period: Vec<Vec<crate::result::NearBindingContingency>> = (0..n_periods)
        .into_par_iter()
        .map(|t| {
            let angles = &sol.bus_angles_rad[t];
            if angles.len() != n_bus
                || hourly_networks.get(t).is_none()
                || hourly_contexts.get(t).is_none()
            {
                return Vec::new();
            }
            screen_near_binding_branch_contingencies(
                t,
                angles,
                &hourly_networks[t],
                &hourly_contexts[t],
                base,
                NEAR_BINDING_THRESHOLD,
            )
        })
        .collect();
    per_period.into_iter().flatten().collect()
}

/// Attach the near-binding contingency report onto an already-
/// finalised dispatch solution's `SecurityDispatchMetadata`. Called
/// after `attach_security_metadata` so the metadata struct exists.
fn attach_near_binding_report(
    mut dispatch: RawDispatchSolution,
    report: Vec<crate::result::NearBindingContingency>,
) -> RawDispatchSolution {
    if report.is_empty() {
        return dispatch;
    }
    if let Some(meta) = dispatch.diagnostics.security.as_mut() {
        meta.near_binding_contingencies = report;
    }
    dispatch
}

/// Solve a DC time-coupled dispatch with the full linearized N-1
/// contingency set built into the SCUC up front.
pub(crate) fn solve_explicit_security_dispatch(
    network: &Network,
    options: &SecurityDispatchSpec,
) -> Result<RawDispatchSolution, ScedError> {
    // Measure everything in this function outside the inner
    // solve_scuc_with_problem_spec call so the "security overhead"
    // has a named line in DispatchPhaseTimings.security_setup_secs.
    let fn_start = std::time::Instant::now();
    let base_spec = DispatchProblemSpec::from_request(&options.input, &options.commitment);
    let n_periods = options.input.n_periods;
    let min_rate = options.input.min_rate_a;

    let hourly_networks: Vec<Network> = (0..n_periods)
        .map(|hour| super::snapshot::network_at_hour_with_spec(network, &base_spec, hour))
        .collect();
    let hourly_contexts: Vec<HourlySecurityContext> = hourly_networks
        .iter()
        .map(|hourly_network| build_hourly_security_context(hourly_network, options, min_rate))
        .collect::<Result<_, _>>()?;

    // Option C: build compact `ContingencyCut` entries directly (one
    // LP row per (contingency × monitored × period)), bypassing the
    // ~500-byte-per-entry `surge_network::network::Flowgate` route and
    // the matching per-flowgate `resolved_flowgates` allocations. The
    // cuts carry exactly the five numbers the LP row emitter needs
    // (`scuc::problem::build_problem`) to materialize the constraint,
    // plus the `case_index` + `period` bookkeeping used by
    // `ExplicitContingencyObjectivePlan` for case-level penalty
    // aggregation and by `extract.rs` for per-case reporting.
    let lodf_cutoff = DEFAULT_CONTINGENCY_CUT_LODF_CUTOFF_PU;
    let mut explicit_cases: Vec<ExplicitContingencyCase> = Vec::new();
    let mut contingency_cuts: Vec<ContingencyCut> = Vec::new();
    let mut hvdc_band_coefs: Vec<(u32, f64)> = Vec::new();
    let mut n_cuts_dropped_by_lodf_cutoff: usize = 0;
    for (period, (hourly_network, context)) in hourly_networks
        .iter()
        .zip(hourly_contexts.iter())
        .enumerate()
    {
        let mut branch_case_indices: Vec<usize> =
            context.branch_contingencies.keys().copied().collect();
        branch_case_indices.sort_unstable();
        for contingency_branch_idx in branch_case_indices {
            let case_index = explicit_cases.len();
            explicit_cases.push(ExplicitContingencyCase {
                period,
                element: ExplicitContingencyElement::Branch(contingency_branch_idx),
            });
            let Some(contingency) = context.branch_contingencies.get(&contingency_branch_idx)
            else {
                continue;
            };
            for &monitored_idx in &context.monitored {
                if monitored_idx == contingency.branch_idx {
                    continue;
                }
                let Some(ptdf_l) = context.ptdf.row(monitored_idx) else {
                    continue;
                };
                let lodf_lk =
                    (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
                if !lodf_lk.is_finite() {
                    continue;
                }
                // LODF magnitude filter — see
                // `DEFAULT_CONTINGENCY_CUT_LODF_CUTOFF_PU` for
                // rationale. Cuts below the threshold are numerically
                // trivial; dropping them here keeps the SCUC LP
                // tractable on full N-1.
                if lodf_lk.abs() < lodf_cutoff {
                    n_cuts_dropped_by_lodf_cutoff += 1;
                    continue;
                }
                let monitored_branch = &hourly_network.branches[monitored_idx];
                // Contingency flowgates use the emergency rating
                // (`rating_c_mva`) so the LODF cut limit matches the
                // post-contingency thermal envelope.
                let limit_mw = monitored_branch.rating_for(BranchRatingCondition::Emergency);
                contingency_cuts.push(ContingencyCut {
                    period: period as u32,
                    case_index: case_index as u32,
                    monitored_branch_idx: monitored_idx as u32,
                    contingency_kind: ContingencyCutKind::Branch,
                    contingency_idx: contingency.branch_idx as u32,
                    coefficient: lodf_lk,
                    limit_mw,
                    hvdc_band_range: (0, 0),
                });
            }
        }

        for contingency in &context.hvdc_contingencies {
            let case_index = explicit_cases.len();
            explicit_cases.push(ExplicitContingencyCase {
                period,
                element: ExplicitContingencyElement::Hvdc(contingency.hvdc_idx),
            });
            let Some(hvdc) = options.input.hvdc_links.get(contingency.hvdc_idx) else {
                continue;
            };
            for &monitored_idx in &context.monitored {
                let Some(ptdf_l) = context.ptdf.row(monitored_idx) else {
                    continue;
                };
                let monitored_branch = &hourly_network.branches[monitored_idx];
                let limit_mw = monitored_branch.rating_for(BranchRatingCondition::Emergency);
                if hvdc.is_banded() {
                    let band_coefficients: Vec<(u32, f64)> = contingency
                        .band_loss_b
                        .iter()
                        .enumerate()
                        .map(|(band_idx, &loss_b)| {
                            (
                                band_idx as u32,
                                ptdf_l[contingency.from_idx]
                                    - (1.0 - loss_b) * ptdf_l[contingency.to_idx],
                            )
                        })
                        .collect();
                    // HVDC-banded: drop when every band coefficient
                    // is below the LODF-equivalent cutoff (same
                    // magnitude interpretation as the branch LODF —
                    // both are DC sensitivities of post-contingency
                    // flow on the monitored branch).
                    if band_coefficients
                        .iter()
                        .all(|(_, coeff)| !coeff.is_finite() || coeff.abs() < lodf_cutoff)
                    {
                        n_cuts_dropped_by_lodf_cutoff += 1;
                        continue;
                    }
                    let band_start = hvdc_band_coefs.len() as u32;
                    hvdc_band_coefs.extend(band_coefficients);
                    let band_end = hvdc_band_coefs.len() as u32;
                    contingency_cuts.push(ContingencyCut {
                        period: period as u32,
                        case_index: case_index as u32,
                        monitored_branch_idx: monitored_idx as u32,
                        contingency_kind: ContingencyCutKind::HvdcBanded,
                        contingency_idx: contingency.hvdc_idx as u32,
                        coefficient: 0.0,
                        limit_mw,
                        hvdc_band_range: (band_start, band_end),
                    });
                } else {
                    let coefficient = ptdf_l[contingency.from_idx]
                        - (1.0 - hvdc.loss_b_frac) * ptdf_l[contingency.to_idx];
                    if !coefficient.is_finite() || coefficient.abs() < lodf_cutoff {
                        n_cuts_dropped_by_lodf_cutoff += 1;
                        continue;
                    }
                    contingency_cuts.push(ContingencyCut {
                        period: period as u32,
                        case_index: case_index as u32,
                        monitored_branch_idx: monitored_idx as u32,
                        contingency_kind: ContingencyCutKind::HvdcLegacy,
                        contingency_idx: contingency.hvdc_idx as u32,
                        coefficient,
                        limit_mw,
                        hvdc_band_range: (0, 0),
                    });
                }
            }
        }
    }

    // Option C: we keep `explicit_network` as a plain clone of the
    // source network (no security Flowgate structs pushed into
    // `flowgates`). The contingency cuts ride the LP through
    // `scuc_spec.contingency_cuts`, and the SCUC problem builder
    // emits one LP row per cut directly. This replaces the
    // ~500-byte-per-cut Flowgate route + matching `resolved_flowgates`
    // allocations (~5 GB on 617-bus D1 explicit N-1) with ~40 bytes
    // per cut.
    let explicit_network = network.clone();
    let n_cuts = contingency_cuts.len();
    info!(
        n_contingency_cuts = n_cuts,
        n_cases = explicit_cases.len(),
        n_cuts_dropped_by_lodf_cutoff,
        lodf_cutoff_pu = lodf_cutoff,
        "Security SCUC: solving with compact contingency cuts"
    );

    // `enforce_flowgates` is *not* flipped on this path — the cut
    // rows emit their own LP rows + slacks independent of the
    // `fg_rows` plumbing, so the base-case flowgate flag stays at
    // the caller's setting.
    let empty_flowgate_mappings: &[ExplicitContingencyFlowgate] = &[];
    let mut scuc_spec =
        base_spec.with_explicit_contingencies(&explicit_cases, empty_flowgate_mappings);
    scuc_spec.contingency_cuts = &contingency_cuts;
    scuc_spec.contingency_cut_hvdc_band_coefs = &hvdc_band_coefs;
    let mut connectivity_cuts: Vec<super::connectivity::IndexedConnectivityCut> = Vec::new();
    let mut result: Option<RawDispatchSolution> = None;
    if scuc_spec.allow_branch_switching {
        for refit_round in 0..MAX_CONNECTIVITY_CUT_ROUNDS {
            let spec_with_cuts = scuc_spec.with_connectivity_cuts(&connectivity_cuts);
            let round_sol = solve_scuc_with_problem_spec(&explicit_network, spec_with_cuts)?;
            let new_cuts_added = add_connectivity_cuts_from_solution(
                &round_sol,
                &hourly_networks,
                &hourly_contexts,
                &mut connectivity_cuts,
                "explicit",
                0,
                refit_round,
            );
            if !new_cuts_added {
                result = Some(round_sol);
                break;
            }
            if refit_round + 1 == MAX_CONNECTIVITY_CUT_ROUNDS {
                warn!(
                    cuts = connectivity_cuts.len(),
                    "Security SCUC explicit connectivity cut loop hit \
                         {MAX_CONNECTIVITY_CUT_ROUNDS}-round cap; accepting the \
                         last solve even though one or more base or contingency \
                         switching graphs remain disconnected."
                );
                result = Some(round_sol);
            }
        }
    }
    // Stop the security-setup clock before handing off to SCUC so the
    // inner solve's wall isn't double-counted here. attach_security_metadata
    // + destructor tail below restart a second accumulator.
    let security_setup_pre = fn_start.elapsed().as_secs_f64();

    let result = match result {
        Some(solution) => solution,
        None => {
            // SW0 (no connectivity-cut loop) and the final solve on
            // SW1 don't need hourly_networks after this point — move
            // them into solve_scuc so build_model_plan can reuse them
            // instead of rebuilding an identical Vec<Network>.
            // hourly_contexts is kept (still borrowed by the caller)
            // but we don't use it below anyway.
            //
            // Move `explicit_network` by value: its `flowgates` array
            // holds up to ~10M security cuts on large N-1 scenarios
            // (617-bus D1 = 8.6M). The old borrowed entry cloned the
            // whole network (flowgates included) inside SCUC. Passing
            // by value saves a peak-RSS copy in that range.
            solve_scuc_with_owned_network(explicit_network, scuc_spec, Some(hourly_networks), None)?
        }
    };

    let t_tail = std::time::Instant::now();
    let mut result = attach_security_metadata(
        result,
        SecurityDispatchMetadata {
            iterations: 0,
            n_cuts: n_cuts + connectivity_cuts.len(),
            active_cuts: n_cuts + connectivity_cuts.len(),
            retired_cuts: 0,
            converged: true,
            last_branch_violations: 0,
            last_hvdc_violations: 0,
            max_branch_violation_pu: None,
            max_hvdc_violation_pu: None,
            n_preseed_cuts: 0,
            n_preseed_pairs_binding: None,
            setup_timings_secs: None,
            per_iteration: Vec::new(),
            near_binding_contingencies: Vec::new(),
        },
    );
    // `security_setup_tail` covers attach_security_metadata itself.
    // Destructors of hourly_contexts + explicit_cases +
    // explicit_case_flowgates fire at function-return epilogue and are
    // NOT in this accumulator — they show up as the residual
    // `solve_scuc_external_secs − (solve_scuc_self_total_secs +
    // security_setup_secs)` in the caller's view. `explicit_flowgates`
    // was moved into `explicit_network.flowgates` above, and
    // `explicit_network` + `hourly_networks` are now moved into
    // `solve_scuc_with_owned_network`, so their destructor walls land
    // inside `scuc_local_drops_secs` on the SCUC timing block instead
    // of leaking into the residual.
    let security_setup_tail = t_tail.elapsed().as_secs_f64();
    if let Some(pt) = result.diagnostics.phase_timings.as_mut() {
        pt.security_setup_secs = security_setup_pre + security_setup_tail;
    }
    Ok(result)
}

/// Uniform bus participation factors (`α_i = 1/|I|`) for
/// post-contingency slack distribution.
fn uniform_participation_weights(network: &Network) -> Vec<(usize, f64)> {
    let n = network.buses.len();
    if n == 0 {
        return Vec::new();
    }
    let w = 1.0 / n as f64;
    (0..n).map(|i| (i, w)).collect()
}

fn build_hourly_security_context(
    hourly_network: &Network,
    options: &SecurityDispatchSpec,
    min_rate: f64,
) -> Result<HourlySecurityContext, ScedError> {
    let bus_map = hourly_network.bus_index_map();
    let monitored: Vec<usize> = hourly_network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| br.in_service && br.rating_a_mva > min_rate && br.x.abs() > 1e-20)
        .map(|(idx, _)| idx)
        .collect();

    let mut connectivity_contingency_branches: Vec<usize> =
        if options.contingency_branches.is_empty() {
            hourly_network
                .branches
                .iter()
                .enumerate()
                .filter_map(|(idx, branch)| branch.in_service.then_some(idx))
                .collect()
        } else {
            options
                .contingency_branches
                .iter()
                .copied()
                .filter(|&idx| {
                    idx < hourly_network.branches.len()
                        && hourly_network
                            .branches
                            .get(idx)
                            .is_some_and(|branch| branch.in_service)
                })
                .collect()
        };
    connectivity_contingency_branches.sort_unstable();
    connectivity_contingency_branches.dedup();

    let contingency_candidates: Vec<usize> = if options.contingency_branches.is_empty() {
        monitored.clone()
    } else {
        connectivity_contingency_branches.clone()
    };

    let mut ptdf_branch_set: HashSet<usize> = monitored.iter().copied().collect();
    ptdf_branch_set.extend(contingency_candidates.iter().copied().filter(|&idx| {
        hourly_network
            .branches
            .get(idx)
            .is_some_and(|branch| branch.in_service && branch.x.abs() > 1e-20)
    }));
    let ptdf_branches: Vec<usize> = ptdf_branch_set.into_iter().collect();
    let ptdf = if ptdf_branches.is_empty() {
        surge_dc::PtdfRows::default()
    } else {
        let mut ptdf_branches = ptdf_branches;
        ptdf_branches.sort_unstable();
        // Uniform participation factors `α_i = 1/|I|` for
        // post-contingency slack distribution.
        let uniform_weights = uniform_participation_weights(hourly_network);
        let sensitivity_options =
            surge_dc::DcSensitivityOptions::with_slack_weights(&uniform_weights);
        let ptdf_request =
            surge_dc::PtdfRequest::for_branches(&ptdf_branches).with_options(sensitivity_options);
        surge_dc::compute_ptdf(hourly_network, &ptdf_request)
            .map_err(|e| ScedError::SolverError(e.to_string()))?
    };

    let branch_contingencies: HashMap<usize, HourlyBranchContingency> = contingency_candidates
        .iter()
        .filter_map(|&branch_idx| {
            let branch = hourly_network.branches.get(branch_idx)?;
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
                HourlyBranchContingency {
                    branch_idx,
                    from_idx,
                    to_idx,
                    denom,
                },
            ))
        })
        .collect();

    let hvdc_contingencies: Vec<HourlyHvdcContingency> = options
        .hvdc_contingency_indices
        .iter()
        .filter_map(|&hvdc_idx| {
            let hvdc = options.input.hvdc_links.get(hvdc_idx)?;
            let from_idx = *bus_map.get(&hvdc.from_bus)?;
            let to_idx = *bus_map.get(&hvdc.to_bus)?;
            let band_loss_b = if hvdc.is_banded() {
                hvdc.bands.iter().map(|band| band.loss_b_frac).collect()
            } else {
                Vec::new()
            };
            Some(HourlyHvdcContingency {
                hvdc_idx,
                from_idx,
                to_idx,
                band_loss_b,
            })
        })
        .collect();

    // Precompute per-monitored-branch arrays used in every screening
    // pass. Hoisting these out of the pair loop drops the per-pair cost
    // from ~2 HashMap gets + a phase-shift fetch + `rating_for()` chain
    // to a handful of indexed Vec reads.
    let base_mva = hourly_network.base_mva.max(1e-6);
    let n_monitored = monitored.len();
    let mut monitored_from_idx = Vec::with_capacity(n_monitored);
    let mut monitored_to_idx = Vec::with_capacity(n_monitored);
    let mut monitored_b_dc = Vec::with_capacity(n_monitored);
    let mut monitored_phase_shift_rad = Vec::with_capacity(n_monitored);
    let mut monitored_limit_pu = Vec::with_capacity(n_monitored);
    for &l in &monitored {
        let branch = &hourly_network.branches[l];
        // These lookups shouldn't fail for entries in `monitored` — that
        // list was filtered on in-service + finite x, and we already
        // built `bus_map` over the hourly network. If a lookup fails we
        // push sentinels so the screener skips the row without
        // reindexing; losing one monitored branch is safer than
        // blowing up the whole solve.
        let from_idx = bus_map.get(&branch.from_bus).copied().unwrap_or(usize::MAX);
        let to_idx = bus_map.get(&branch.to_bus).copied().unwrap_or(usize::MAX);
        monitored_from_idx.push(from_idx);
        monitored_to_idx.push(to_idx);
        monitored_b_dc.push(branch.b_dc());
        monitored_phase_shift_rad.push(branch.phase_shift_rad);
        let rating_mva = branch.rating_for(BranchRatingCondition::Emergency);
        let limit_pu = if rating_mva.is_finite() && rating_mva > 0.0 {
            rating_mva / base_mva
        } else {
            f64::INFINITY
        };
        monitored_limit_pu.push(limit_pu);
    }

    // Freeze contingencies into a Vec so the screener can iterate them
    // with a parallel `contingency_b_dc` / `contingency_phase_shift_rad`
    // slice and skip the `HashMap::values()` detour. Order is arbitrary
    // but stable (insertion order from `HashMap` on this build).
    let contingencies: Vec<HourlyBranchContingency> =
        branch_contingencies.values().cloned().collect();
    let mut contingency_b_dc = Vec::with_capacity(contingencies.len());
    let mut contingency_phase_shift_rad = Vec::with_capacity(contingencies.len());
    for c in &contingencies {
        let branch = &hourly_network.branches[c.branch_idx];
        contingency_b_dc.push(branch.b_dc());
        contingency_phase_shift_rad.push(branch.phase_shift_rad);
    }

    Ok(HourlySecurityContext {
        monitored,
        monitored_from_idx,
        monitored_to_idx,
        monitored_b_dc,
        monitored_phase_shift_rad,
        monitored_limit_pu,
        contingencies,
        contingency_b_dc,
        contingency_phase_shift_rad,
        ptdf,
        branch_contingencies,
        connectivity_contingency_branches,
        hvdc_contingencies,
    })
}

/// Recompute per-period bus angles by solving a DC power flow whose
/// bus injections match the SCUC dispatch. Used only on the
/// `scuc_disable_bus_power_balance` path: without per-bus KCL rows, the
/// LP's theta is vestigial (unconstrained by generation pattern), so
/// the angles returned from the MIP have no physical meaning and the
/// security screen — which computes pre/post-contingency flows via
/// `b·Δθ` and LODFs — would enumerate phantom violations.
///
/// The repair clones each hourly network, overwrites in-service
/// generator `p_mw` with the solved `pg_mw`, and adds
/// `external_p_injections_mw` deltas for DL served MW and HVDC
/// dispatch (not native to the base network). Running `surge_dc`'s
/// DC power flow under the classical single-bus slack convention
/// gives valid theta per period that respects the SCUC commitment.
///
/// No-op when `spec.scuc_disable_bus_power_balance` is false — the
/// default bus-balanced SCUC already produces physical theta from
/// its own per-bus KCL rows.
fn repair_theta_from_dc_pf(
    sol: &mut RawDispatchSolution,
    hourly_networks: &[Network],
    spec: &DispatchProblemSpec<'_>,
) -> Result<(), ScedError> {
    use std::collections::HashMap as StdHashMap;

    for (t, period) in sol.periods.iter().enumerate() {
        if sol.bus_angles_rad.get(t).is_none() {
            continue;
        }
        let hourly_network = match hourly_networks.get(t) {
            Some(n) => n,
            None => continue,
        };
        let base_mva = hourly_network.base_mva;

        // Clone and override in-service generators' dispatch from the
        // SCUC solution. `pg_mw` is one per in-service generator in the
        // same iteration order, which matches the ordering the layout
        // and extract paths use.
        let mut net = hourly_network.clone();
        let mut pg_iter = period.pg_mw.iter();
        for g in net.generators.iter_mut() {
            if g.in_service {
                if let Some(&pg) = pg_iter.next() {
                    g.p = pg;
                }
            }
        }

        // External injections = deltas not carried by base-network
        // state (DL served beyond any fixed-load portion already in
        // `net.loads`, HVDC dispatch since GO C3 links live in
        // `spec.hvdc_links` rather than `net.hvdc`).
        let mut ext: StdHashMap<u32, f64> = StdHashMap::new();
        for ld in &period.dr_results.loads {
            // DL served is a withdrawal: negative injection in MW.
            *ext.entry(ld.bus).or_insert(0.0) -= ld.p_served_pu * base_mva;
        }
        for (k, hvdc) in spec.hvdc_links.iter().enumerate() {
            let p_mw = period.hvdc_dispatch_mw.get(k).copied().unwrap_or(0.0);
            if p_mw.abs() > 1e-12 {
                *ext.entry(hvdc.from_bus).or_insert(0.0) += p_mw;
                *ext.entry(hvdc.to_bus).or_insert(0.0) -= p_mw * (1.0 - hvdc.loss_b_frac);
            }
        }
        let external: Vec<(u32, f64)> = ext.into_iter().collect();

        let opts = surge_dc::DcPfOptions {
            external_p_injections_mw: external,
            ..Default::default()
        };
        match surge_dc::solve_dc_opts(&net, &opts) {
            Ok(pf) => {
                if pf.theta.len() == sol.bus_angles_rad[t].len() {
                    sol.bus_angles_rad[t] = pf.theta;
                }
            }
            Err(e) => {
                warn!(period = t, error = %e, "DC PF theta repair failed; keeping vestigial angles");
            }
        }
    }
    Ok(())
}

/// Below this absolute p.u. flow on a contingency branch we skip the
/// whole contingency: the post-contingency shift it imparts on any
/// monitored line is bounded by `|lodf| * flow_k_abs`, which for any
/// non-radial `|lodf|` well below unity rounds to zero under the tolerance.
/// Applied only to branch contingencies — HVDC contingencies already have
/// a dispatch-presence check upstream.
const CONTINGENCY_FLOW_SKIP_PU: f64 = 1e-10;

#[cfg(test)]
fn screen_branch_violations(
    period: usize,
    angles: &[f64],
    _hourly_network: &Network,
    context: &HourlySecurityContext,
    _base: f64,
    tolerance_pu: f64,
    constrained_pairs: &HashSet<(usize, usize, usize)>,
    retention_cap: usize,
) -> BranchViolationScreen {
    let mut out = BoundedBranchViolationSet::new(retention_cap);
    screen_branch_violations_into(
        period,
        angles,
        _hourly_network,
        context,
        _base,
        tolerance_pu,
        constrained_pairs,
        &mut out,
    );
    out.finish()
}

fn screen_branch_violations_into(
    period: usize,
    angles: &[f64],
    _hourly_network: &Network,
    context: &HourlySecurityContext,
    _base: f64,
    tolerance_pu: f64,
    constrained_pairs: &HashSet<(usize, usize, usize)>,
    out: &mut BoundedBranchViolationSet,
) {
    let n_monitored = context.monitored.len();
    if n_monitored == 0 || context.contingencies.is_empty() {
        return;
    }

    // Precompute pre-contingency flow on every monitored branch in p.u.
    // This value is independent of the outer contingency `k`, so we'd
    // otherwise recompute it n_contingencies times per `l` — a ~11k×
    // multiplier on large cases.
    let mut flow_mon = Vec::with_capacity(n_monitored);
    for i in 0..n_monitored {
        let from_i = context.monitored_from_idx[i];
        let to_i = context.monitored_to_idx[i];
        if from_i == usize::MAX || to_i == usize::MAX {
            flow_mon.push(0.0);
            continue;
        }
        flow_mon.push(
            context.monitored_b_dc[i]
                * (angles[from_i] - angles[to_i] - context.monitored_phase_shift_rad[i]),
        );
    }

    // Precompute pre-contingency flow on every contingency branch. Also
    // cache its absolute value so the inner loop's prescreen is one
    // array load.
    let n_ctg = context.contingencies.len();
    let mut flow_ctg = Vec::with_capacity(n_ctg);
    let mut flow_ctg_abs = Vec::with_capacity(n_ctg);
    for (i, c) in context.contingencies.iter().enumerate() {
        let v = context.contingency_b_dc[i]
            * (angles[c.from_idx] - angles[c.to_idx] - context.contingency_phase_shift_rad[i]);
        flow_ctg.push(v);
        flow_ctg_abs.push(v.abs());
    }

    // Loop order is flipped vs the original: outer over `l`, inner over
    // `k`. This keeps `ptdf_l` (a dense row of length n_bus) cache-hot
    // across all contingencies for a given monitored branch, instead of
    // swapping a new row in on every `(k, l)` pair. On large cases the
    // PTDF rows exceed L1 and the swap dominates.
    for (mon_pos, &l) in context.monitored.iter().enumerate() {
        let limit_pu = context.monitored_limit_pu[mon_pos];
        if !limit_pu.is_finite() {
            continue;
        }
        let flow_l = flow_mon[mon_pos];
        let flow_l_abs = flow_l.abs();

        let Some(ptdf_l) = context.ptdf.row(l) else {
            continue;
        };

        for (k_pos, contingency) in context.contingencies.iter().enumerate() {
            let k = contingency.branch_idx;
            if l == k {
                continue;
            }
            let flow_k_abs = flow_ctg_abs[k_pos];
            // Skip contingencies with negligible pre-contingency flow:
            // their post-contingency impact on every monitored branch is
            // bounded by `|lodf| * tiny`, well inside `tolerance_pu`.
            if flow_k_abs < CONTINGENCY_FLOW_SKIP_PU {
                continue;
            }
            if constrained_pairs.contains(&(period, k, l)) {
                continue;
            }

            let lodf_lk =
                (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
            if !lodf_lk.is_finite() {
                continue;
            }

            // Magnitude prescreen: `|post_flow| <= |flow_l| + |lodf| * |flow_k|`
            // by the triangle inequality. When the upper bound is at or
            // below `limit_pu + tol` the pair cannot violate; skip the
            // `post_flow` + `abs()` + push. Safe because we use the
            // actual `lodf` (not a heuristic bound), so no violation is
            // ever missed.
            let lodf_abs = lodf_lk.abs();
            if flow_l_abs + lodf_abs * flow_k_abs <= limit_pu + tolerance_pu {
                continue;
            }

            let post_flow = flow_l + lodf_lk * flow_ctg[k_pos];
            let excess = post_flow.abs() - limit_pu;
            if excess > tolerance_pu {
                out.push(BranchSecurityViolation {
                    period,
                    contingency_branch_idx: k,
                    monitored_branch_idx: l,
                    severity_pu: excess,
                    breach_upper: post_flow > 0.0,
                });
            }
        }
    }
}

/// Final-pass screen that records post-contingency flow on every
/// `(period, k, l)` pair whose `|post_flow|` reaches
/// `threshold_fraction × limit`. Cheaper than a full report (skips
/// pairs well inside the limit) and meant to run ONCE on the final
/// dispatch — not per iteration. Threshold of 0.95 surfaces
/// "near-binding" contingencies for diagnostic display without
/// flooding the report on large networks.
fn screen_near_binding_branch_contingencies(
    period: usize,
    angles: &[f64],
    hourly_network: &Network,
    context: &HourlySecurityContext,
    base: f64,
    threshold_fraction: f64,
) -> Vec<crate::result::NearBindingContingency> {
    let n_monitored = context.monitored.len();
    if n_monitored == 0 || context.contingencies.is_empty() {
        return Vec::new();
    }

    // Pre-contingency monitored / contingency flows — same as the
    // violation screener.
    let mut flow_mon = Vec::with_capacity(n_monitored);
    for i in 0..n_monitored {
        let from_i = context.monitored_from_idx[i];
        let to_i = context.monitored_to_idx[i];
        if from_i == usize::MAX || to_i == usize::MAX {
            flow_mon.push(0.0);
            continue;
        }
        flow_mon.push(
            context.monitored_b_dc[i]
                * (angles[from_i] - angles[to_i] - context.monitored_phase_shift_rad[i]),
        );
    }
    let n_ctg = context.contingencies.len();
    let mut flow_ctg = Vec::with_capacity(n_ctg);
    let mut flow_ctg_abs = Vec::with_capacity(n_ctg);
    for (i, c) in context.contingencies.iter().enumerate() {
        let v = context.contingency_b_dc[i]
            * (angles[c.from_idx] - angles[c.to_idx] - context.contingency_phase_shift_rad[i]);
        flow_ctg.push(v);
        flow_ctg_abs.push(v.abs());
    }

    let mut out = Vec::new();
    for (mon_pos, &l) in context.monitored.iter().enumerate() {
        let limit_pu = context.monitored_limit_pu[mon_pos];
        if !limit_pu.is_finite() || limit_pu <= 0.0 {
            continue;
        }
        let target_pu = threshold_fraction * limit_pu;
        let flow_l = flow_mon[mon_pos];
        let flow_l_abs = flow_l.abs();

        let Some(ptdf_l) = context.ptdf.row(l) else {
            continue;
        };

        for (k_pos, contingency) in context.contingencies.iter().enumerate() {
            let k = contingency.branch_idx;
            if l == k {
                continue;
            }
            let flow_k_abs = flow_ctg_abs[k_pos];
            // Triangle prescreen: |post| ≤ |flow_l| + |lodf|·|flow_k|.
            // When the upper bound is below the near-binding target,
            // the pair cannot meet the threshold — skip the LODF +
            // post_flow compute.
            // (We don't have lodf yet, so first try a cheaper check
            // using the contingency-flow magnitude as a proxy: if
            // flow_l_abs + flow_k_abs is already below target, the
            // pair definitely can't reach it for any |lodf| ≤ 1. This
            // handles the common case where both pre-contingency
            // flows are small.)
            if flow_l_abs + flow_k_abs < target_pu {
                continue;
            }
            let lodf_lk =
                (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
            if !lodf_lk.is_finite() {
                continue;
            }
            let lodf_abs = lodf_lk.abs();
            if flow_l_abs + lodf_abs * flow_k_abs < target_pu {
                continue;
            }

            let post_flow = flow_l + lodf_lk * flow_ctg[k_pos];
            let post_abs = post_flow.abs();
            if post_abs < target_pu {
                continue;
            }

            let monitored = &hourly_network.branches[l];
            let ctg_branch = &hourly_network.branches[k];
            out.push(crate::result::NearBindingContingency {
                period: period as u32,
                outage_from_bus: ctg_branch.from_bus,
                outage_to_bus: ctg_branch.to_bus,
                outage_circuit: ctg_branch.circuit.clone(),
                monitored_from_bus: monitored.from_bus,
                monitored_to_bus: monitored.to_bus,
                monitored_circuit: monitored.circuit.clone(),
                post_contingency_flow_mw: post_flow * base,
                limit_mw: limit_pu * base,
                utilization: post_abs / limit_pu,
            });
        }
    }
    out
}

fn screen_hvdc_violations(
    period: usize,
    angles: &[f64],
    dispatch: &crate::solution::RawDispatchPeriodResult,
    _hourly_network: &Network,
    context: &HourlySecurityContext,
    hvdc_links: &[crate::hvdc::HvdcDispatchLink],
    base: f64,
    tolerance_pu: f64,
    constrained_pairs: &HashSet<(usize, usize, usize)>,
) -> Vec<HvdcSecurityCut> {
    let n_monitored = context.monitored.len();
    if n_monitored == 0 || context.hvdc_contingencies.is_empty() {
        return Vec::new();
    }

    // Precompute pre-contingency monitored flows in p.u. — same
    // reasoning as `screen_branch_violations`: flow_l is independent of
    // the contingency loop variable.
    let mut flow_mon = Vec::with_capacity(n_monitored);
    for i in 0..n_monitored {
        let from_i = context.monitored_from_idx[i];
        let to_i = context.monitored_to_idx[i];
        if from_i == usize::MAX || to_i == usize::MAX {
            flow_mon.push(0.0);
            continue;
        }
        flow_mon.push(
            context.monitored_b_dc[i]
                * (angles[from_i] - angles[to_i] - context.monitored_phase_shift_rad[i]),
        );
    }

    // Precompute per-HVDC dispatch once and filter out idle contingencies.
    // Each entry carries (contingency_meta_ref, total_dispatch_pu,
    // band_dispatch_pu, is_banded) so the inner loop is pure arithmetic.
    let mut active_ctg: Vec<(
        &HourlyHvdcContingency,
        &crate::hvdc::HvdcDispatchLink,
        f64,
        Vec<f64>,
    )> = Vec::with_capacity(context.hvdc_contingencies.len());
    for contingency in &context.hvdc_contingencies {
        let Some(hvdc) = hvdc_links.get(contingency.hvdc_idx) else {
            continue;
        };
        let total_dispatch_pu = dispatch
            .hvdc_dispatch_mw
            .get(contingency.hvdc_idx)
            .copied()
            .unwrap_or(0.0)
            / base;
        let band_dispatch_pu: Vec<f64> = dispatch
            .hvdc_band_dispatch_mw
            .get(contingency.hvdc_idx)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mw| mw / base)
            .collect();
        if hvdc.is_banded() {
            if band_dispatch_pu.len() != hvdc.bands.len()
                || band_dispatch_pu.iter().all(|mw| mw.abs() < 1e-10)
            {
                continue;
            }
        } else if total_dispatch_pu.abs() < 1e-10 {
            continue;
        }
        active_ctg.push((contingency, hvdc, total_dispatch_pu, band_dispatch_pu));
    }
    if active_ctg.is_empty() {
        return Vec::new();
    }

    let mut violations = Vec::new();

    // Flipped loop order for PTDF row cache locality, matching
    // `screen_branch_violations`.
    for (mon_pos, &l) in context.monitored.iter().enumerate() {
        let limit_pu = context.monitored_limit_pu[mon_pos];
        if !limit_pu.is_finite() {
            continue;
        }
        let flow_l = flow_mon[mon_pos];
        let flow_l_abs = flow_l.abs();
        let limit_mva = limit_pu * base;

        let Some(ptdf_l) = context.ptdf.row(l) else {
            continue;
        };

        for (contingency, hvdc, total_dispatch_pu, band_dispatch_pu) in &active_ctg {
            let k = contingency.hvdc_idx;
            if constrained_pairs.contains(&(period, k, l)) {
                continue;
            }

            let (coefficient, band_coefficients, post_flow) = if hvdc.is_banded() {
                let band_coefficients: Vec<(usize, f64)> = contingency
                    .band_loss_b
                    .iter()
                    .enumerate()
                    .map(|(band_idx, &loss_b)| {
                        (
                            band_idx,
                            ptdf_l[contingency.from_idx]
                                - (1.0 - loss_b) * ptdf_l[contingency.to_idx],
                        )
                    })
                    .collect();
                if band_coefficients
                    .iter()
                    .all(|(_, coeff)| !coeff.is_finite() || coeff.abs() < 1e-12)
                {
                    continue;
                }
                let impact = band_coefficients
                    .iter()
                    .zip(band_dispatch_pu.iter())
                    .map(|((_, coeff), dispatch_pu)| coeff * dispatch_pu)
                    .sum::<f64>();
                (None, band_coefficients, flow_l - impact)
            } else {
                let coefficient = ptdf_l[contingency.from_idx]
                    - (1.0 - hvdc.loss_b_frac) * ptdf_l[contingency.to_idx];
                if !coefficient.is_finite() || coefficient.abs() < 1e-12 {
                    continue;
                }
                (
                    Some(coefficient),
                    Vec::new(),
                    flow_l - coefficient * total_dispatch_pu,
                )
            };

            // Magnitude prescreen: `|post_flow|` cannot exceed
            // `|flow_l| + |impact|`. `impact` = coeff·dispatch for the
            // simple case, or sum(coeff_b·dispatch_b) for banded —
            // either way the safe upper bound is `|flow_l| + |impact|`.
            // Compute it once here to skip clearly-safe pairs without
            // allocating the Flowgate.
            let impact_bound = if let Some(c) = coefficient {
                c.abs() * total_dispatch_pu.abs()
            } else {
                band_coefficients
                    .iter()
                    .zip(band_dispatch_pu.iter())
                    .map(|((_, c), d)| c.abs() * d.abs())
                    .sum::<f64>()
            };
            if flow_l_abs + impact_bound <= limit_pu + tolerance_pu {
                continue;
            }

            // Eq (271) contingency rating: HVDC cuts use the same
            // post-contingency thermal limit policy as branch cuts.
            let excess = post_flow.abs() - limit_pu;
            if excess > tolerance_pu {
                violations.push(HvdcSecurityCut {
                    period,
                    hvdc_link_idx: k,
                    monitored_branch_idx: l,
                    coefficient,
                    band_coefficients,
                    f_max_mw: limit_mva,
                    excess_pu: excess,
                    breach_upper: post_flow > 0.0,
                });
            }
        }
    }

    violations
}

/// Rank (contingency, monitored) pairs per period and emit the top-N as
/// security flowgates before iter 0 of the iterative SCUC. Registers the
/// dedup key in `constrained_pairs` so the post-solve screener does not
/// re-discover the same pair.
///
/// Ranking uses
///     severity = |LODF_{l,k}| * rating_c(k) / rating_c(l)
/// — a dimensionless, dispatch-free proxy for "if contingency k trips at
/// emergency capacity, what fraction of monitored-branch l's limit could
/// get shifted onto it". All inputs are already cached in the
/// [`HourlySecurityContext`] (PTDF, LODF denominator), so this is cheap
/// compared with one extra SCUC re-solve.
///
/// HVDC contingencies are not pre-seeded in this first pass; HVDC
/// flowgate row counts are typically far smaller than AC branch
/// counts, and the iterative screener picks them up quickly. Extend
/// here if that assumption changes.
fn preseed_branch_flowgates(
    options: &SecurityDispatchSpec,
    hourly_networks: &[Network],
    hourly_contexts: &[HourlySecurityContext],
    n_periods: usize,
    constrained_pairs: &mut HashSet<(usize, usize, usize)>,
) -> Vec<(SecurityCutKey, surge_network::network::Flowgate)> {
    let per_period = options.preseed_count_per_period;
    if per_period == 0 || matches!(options.preseed_method, SecurityPreseedMethod::None) {
        return Vec::new();
    }

    let mut out: Vec<(SecurityCutKey, surge_network::network::Flowgate)> = Vec::new();

    for (period, (hourly_network, context)) in hourly_networks
        .iter()
        .zip(hourly_contexts.iter())
        .enumerate()
    {
        let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
        for contingency in context.branch_contingencies.values() {
            let k = contingency.branch_idx;
            let branch_k = &hourly_network.branches[k];
            let rating_k_mw = branch_k.rating_for(BranchRatingCondition::Emergency);
            if rating_k_mw <= 0.0 || rating_k_mw.is_nan() {
                continue;
            }
            for &l in &context.monitored {
                if l == k {
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
                let branch_l = &hourly_network.branches[l];
                let rating_l_mw = branch_l.rating_for(BranchRatingCondition::Emergency);
                if rating_l_mw <= 0.0 || rating_l_mw.is_nan() {
                    continue;
                }
                let severity = lodf_lk.abs() * rating_k_mw / rating_l_mw;
                if !severity.is_finite() || severity <= 0.0 {
                    continue;
                }
                candidates.push((severity, k, l));
            }
        }
        candidates
            .sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut taken = 0usize;
        for (_severity, k, l) in candidates {
            if taken >= per_period {
                break;
            }
            let key = (period, k, l);
            if !constrained_pairs.insert(key) {
                continue;
            }
            let violation = BranchSecurityViolation {
                period,
                contingency_branch_idx: k,
                monitored_branch_idx: l,
                severity_pu: 0.0,
                // Preseed: ranking-based, no observed flow direction —
                // the builder emits FlowgateBreachSides::Both so the
                // bounds layer keeps symmetric slacks.
                breach_upper: true,
            };
            out.push((
                SecurityCutKey::Branch {
                    period,
                    contingency_branch_idx: k,
                    monitored_branch_idx: l,
                },
                build_branch_security_flowgate(
                    &violation,
                    hourly_network,
                    context,
                    n_periods,
                    true, // preseed
                ),
            ));
            taken += 1;
        }
    }

    out
}

fn build_branch_security_flowgate(
    violation: &BranchSecurityViolation,
    hourly_network: &Network,
    context: &HourlySecurityContext,
    n_periods: usize,
    preseed: bool,
) -> surge_network::network::Flowgate {
    use surge_network::network::{BranchRef, Flowgate, FlowgateBreachSides, WeightedBranchRef};

    let monitored_branch = &hourly_network.branches[violation.monitored_branch_idx];
    let contingency_branch = &hourly_network.branches[violation.contingency_branch_idx];
    let contingency = context
        .branch_contingencies
        .get(&violation.contingency_branch_idx)
        .expect("branch contingency metadata should exist");
    let ptdf_l = context
        .ptdf
        .row(violation.monitored_branch_idx)
        .expect("monitored branch PTDF row should exist");
    let lodf_lk = (ptdf_l[contingency.from_idx] - ptdf_l[contingency.to_idx]) / contingency.denom;
    let limit_mw = monitored_branch.rating_for(BranchRatingCondition::Emergency);

    // Compute the effective per-bus PTDF for the post-contingency
    // monitored aggregate `f_l + lodf_lk × f_k`, so the LP can use it
    // as a direct constraint on the dispatch variables when running in
    // `scuc_disable_bus_power_balance` mode (theta is decoupled from
    // pg there, so the theta-form of this row is vestigial).
    let ptdf_per_bus = compute_branch_security_ptdf_per_bus(
        ptdf_l,
        context
            .ptdf
            .row(violation.contingency_branch_idx)
            .expect("contingency branch PTDF row should exist"),
        lodf_lk,
        hourly_network,
    );
    // Compact single-period encoding: see Flowgate::effective_limit_mw.
    // For explicit N-1 SCUC this replaces a dense `Vec<f64>` schedule of
    // length n_periods with 17/18 sentinel entries.
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
        // Contingency flowgates use the emergency rating so the LODF
        // cut limit matches the post-contingency thermal envelope.
        limit_mw,
        limit_reverse_mw: 0.0,
        in_service: true,
        limit_mw_schedule: Vec::new(),
        limit_reverse_mw_schedule: Vec::new(),
        hvdc_coefficients: Vec::new(),
        hvdc_band_coefficients: Vec::new(),
        ptdf_per_bus,
        limit_mw_active_period: active_period,
        // Preseed cuts rank (ctg, mon) pairs by magnitude without
        // observing an actual flow direction, so leave symmetric
        // slacks. Iterative-loop cuts observed `post_flow` and know
        // which side crossed the limit — use that to drop the
        // non-breached side's slack column.
        breach_sides: if preseed {
            FlowgateBreachSides::Both
        } else if violation.breach_upper {
            FlowgateBreachSides::Upper
        } else {
            FlowgateBreachSides::Lower
        },
    }
}

/// Below this magnitude we treat a per-bus effective PTDF coefficient
/// as zero. Avoids storing a long tail of dense-but-trivial entries on
/// every flowgate (e.g. the slack bus row, where PTDF rounds to zero
/// across the entire system). At 1e-4 pu, a 100 MW bus load contributes
/// at most 0.01 MW to a flow that is rated in the hundreds of MW — the
/// dropped tail is below LP feasibility tolerance. On 617-bus D1 #2 the
/// PTDF rows averaged ~2,118 NZ at 1e-6 (essentially fully dense across
/// all 617 buses); raising to 1e-4 sheds the bulk of that without
/// changing which cuts are emitted or their economic meaning.
const PTDF_PER_BUS_TOL: f64 = 1e-3;

/// Effective per-bus PTDF for an N-1 cut on monitored branch `l`
/// post-contingency `k`: `eff_i = ptdf_l[i] + lodf_lk × ptdf_k[i]`.
fn compute_branch_security_ptdf_per_bus(
    ptdf_l: &[f64],
    ptdf_k: &[f64],
    lodf_lk: f64,
    hourly_network: &Network,
) -> Vec<(u32, f64)> {
    debug_assert_eq!(ptdf_l.len(), ptdf_k.len());
    debug_assert_eq!(ptdf_l.len(), hourly_network.buses.len());
    let mut out = Vec::new();
    for (i, bus) in hourly_network.buses.iter().enumerate() {
        let coeff = ptdf_l[i] + lodf_lk * ptdf_k[i];
        if coeff.abs() > PTDF_PER_BUS_TOL {
            out.push((bus.number, coeff));
        }
    }
    out
}

/// Effective per-bus PTDF for a single-branch (base-case or HVDC-cut)
/// flowgate: `eff_i = ptdf_l[i]`.
fn compute_single_branch_ptdf_per_bus(ptdf_l: &[f64], hourly_network: &Network) -> Vec<(u32, f64)> {
    debug_assert_eq!(ptdf_l.len(), hourly_network.buses.len());
    let mut out = Vec::new();
    for (i, bus) in hourly_network.buses.iter().enumerate() {
        let coeff = ptdf_l[i];
        if coeff.abs() > PTDF_PER_BUS_TOL {
            out.push((bus.number, coeff));
        }
    }
    out
}

fn build_hvdc_security_flowgate(
    cut: &HvdcSecurityCut,
    hourly_network: &Network,
    context: &HourlySecurityContext,
    n_periods: usize,
) -> surge_network::network::Flowgate {
    use surge_network::network::{BranchRef, Flowgate, FlowgateBreachSides, WeightedBranchRef};

    let monitored_branch = &hourly_network.branches[cut.monitored_branch_idx];
    let active_period = (n_periods > 1 && cut.period < n_periods).then_some(cut.period as u32);
    let ptdf_per_bus = match context.ptdf.row(cut.monitored_branch_idx) {
        Some(ptdf_l) => compute_single_branch_ptdf_per_bus(ptdf_l, hourly_network),
        None => Vec::new(),
    };

    Flowgate {
        name: format!(
            "HVDC_N1_t{}_{}_{}_{}",
            cut.period, cut.hvdc_link_idx, monitored_branch.from_bus, monitored_branch.to_bus
        ),
        monitored: vec![WeightedBranchRef {
            branch: BranchRef::new(
                monitored_branch.from_bus,
                monitored_branch.to_bus,
                monitored_branch.circuit.clone(),
            ),
            coefficient: 1.0,
        }],
        contingency_branch: None,
        limit_mw: cut.f_max_mw,
        limit_reverse_mw: 0.0,
        in_service: true,
        limit_mw_schedule: Vec::new(),
        limit_reverse_mw_schedule: Vec::new(),
        hvdc_coefficients: cut
            .coefficient
            .map(|coefficient| vec![(cut.hvdc_link_idx, -coefficient)])
            .unwrap_or_default(),
        hvdc_band_coefficients: cut
            .band_coefficients
            .iter()
            .map(|&(band_idx, coefficient)| (cut.hvdc_link_idx, band_idx, -coefficient))
            .collect(),
        ptdf_per_bus,
        limit_mw_active_period: active_period,
        breach_sides: if cut.breach_upper {
            FlowgateBreachSides::Upper
        } else {
            FlowgateBreachSides::Lower
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::dispatch::CommitmentMode;
    use crate::request::DispatchInput;
    use crate::sced::{HvdcBand, HvdcDispatchLink};
    use crate::solution::RawDispatchPeriodResult;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    fn triangle_security_network(monitored_limit_mw: f64) -> Network {
        let mut net = Network::new("security_triangle");
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
        net.generators.push(Generator::new(1, 50.0, 1.0));
        net
    }

    fn security_spec(
        input: DispatchInput,
        contingency_branches: Vec<usize>,
        hvdc_contingency_indices: Vec<usize>,
    ) -> SecurityDispatchSpec {
        SecurityDispatchSpec {
            input,
            commitment: CommitmentMode::AllCommitted,
            max_iterations: 1,
            violation_tolerance_pu: 0.0,
            max_cuts_per_iteration: 10,
            contingency_branches,
            hvdc_contingency_indices,
            preseed_count_per_period: 0,
            preseed_method: SecurityPreseedMethod::None,
            cut_strategy: SecurityCutStrategy::Fixed,
            max_active_cuts: None,
            cut_retire_after_rounds: None,
            targeted_cut_threshold: 50_000,
            targeted_cut_cap: 50_000,
            near_binding_report: false,
        }
    }

    #[test]
    fn bounded_branch_violation_set_counts_all_but_retains_worst() {
        let mut set = BoundedBranchViolationSet::new(2);
        for severity_pu in [0.10, 0.50, 0.30] {
            set.push(BranchSecurityViolation {
                period: 0,
                contingency_branch_idx: 0,
                monitored_branch_idx: 1,
                severity_pu,
                breach_upper: true,
            });
        }

        let screen = set.finish();
        assert_eq!(screen.n_violations, 3);
        assert_eq!(screen.violations.len(), 2);
        assert_eq!(screen.violations[0].severity_pu, 0.50);
        assert_eq!(screen.violations[1].severity_pu, 0.30);
    }

    #[test]
    fn dense_ptdf_estimate_counts_all_period_values() {
        let network = triangle_security_network(100.0);
        let expected = 2 * network.n_branches() * network.n_buses() * std::mem::size_of::<f64>();
        assert_eq!(
            estimate_dense_ptdf_bytes(&[network.clone(), network]),
            expected
        );
    }

    #[test]
    fn branch_screening_uses_hourly_derated_limits() {
        let hour0 = triangle_security_network(80.0);
        let hour1 = triangle_security_network(60.0);
        let options = security_spec(
            DispatchInput {
                n_periods: 2,
                min_rate_a: 1.0,
                ..DispatchInput::default()
            },
            vec![0],
            vec![],
        );

        let context0 = build_hourly_security_context(&hour0, &options, 1.0).unwrap();
        let context1 = build_hourly_security_context(&hour1, &options, 1.0).unwrap();
        let angles = [0.0, -0.05, -0.025];

        let hour0_violations = screen_branch_violations(
            0,
            &angles,
            &hour0,
            &context0,
            hour0.base_mva,
            1e-6,
            &HashSet::new(),
            10,
        );
        let hour1_violations = screen_branch_violations(
            1,
            &angles,
            &hour1,
            &context1,
            hour1.base_mva,
            1e-6,
            &HashSet::new(),
            10,
        );

        assert!(
            hour0_violations.violations.is_empty(),
            "80 MW monitored limit should clear the contingency, got {hour0_violations:?}"
        );
        assert!(
            hour1_violations
                .violations
                .iter()
                .any(|violation| violation.contingency_branch_idx == 0
                    && violation.monitored_branch_idx == 2),
            "60 MW monitored limit should trigger the 1-2 outage / 1-3 monitored violation, got {hour1_violations:?}"
        );
    }

    #[test]
    fn connectivity_contingencies_include_in_service_branches_skipped_by_ptdf_screening() {
        let mut hourly_network = triangle_security_network(100.0);
        hourly_network.branches[1].x = 0.0;
        let options = security_spec(
            DispatchInput {
                n_periods: 1,
                min_rate_a: 1.0,
                ..DispatchInput::default()
            },
            vec![],
            vec![],
        );

        let context = build_hourly_security_context(&hourly_network, &options, 1.0).unwrap();

        assert!(
            !context.branch_contingencies.contains_key(&1),
            "zero-reactance branch should be skipped by PTDF-based branch screening"
        );
        assert!(
            context.connectivity_contingency_branches.contains(&1),
            "connectivity screening should still consider the in-service branch outage"
        );
    }

    #[test]
    fn hvdc_screening_uses_per_band_loss_coefficients() {
        let hourly_network = triangle_security_network(1.0);
        let options = security_spec(
            DispatchInput {
                n_periods: 1,
                min_rate_a: 0.5,
                hvdc_links: vec![HvdcDispatchLink {
                    id: String::new(),
                    name: "BandHVDC".into(),
                    from_bus: 1,
                    to_bus: 2,
                    p_dc_min_mw: 0.0,
                    p_dc_max_mw: 100.0,
                    loss_a_mw: 0.0,
                    loss_b_frac: 0.0,
                    ramp_mw_per_min: 0.0,
                    cost_per_mwh: 0.0,
                    bands: vec![
                        HvdcBand {
                            id: "firm".into(),
                            p_min_mw: 0.0,
                            p_max_mw: 50.0,
                            cost_per_mwh: 0.0,
                            loss_b_frac: 0.0,
                            ramp_mw_per_min: 0.0,
                            reserve_eligible_up: false,
                            reserve_eligible_down: false,
                            max_duration_hours: 0.0,
                        },
                        HvdcBand {
                            id: "econ".into(),
                            p_min_mw: 0.0,
                            p_max_mw: 50.0,
                            cost_per_mwh: 0.0,
                            loss_b_frac: 0.5,
                            ramp_mw_per_min: 0.0,
                            reserve_eligible_up: false,
                            reserve_eligible_down: false,
                            max_duration_hours: 0.0,
                        },
                    ],
                }],
                ..DispatchInput::default()
            },
            vec![],
            vec![0],
        );

        let context = build_hourly_security_context(&hourly_network, &options, 0.5).unwrap();
        let dispatch = RawDispatchPeriodResult {
            hvdc_dispatch_mw: vec![75.0],
            hvdc_band_dispatch_mw: vec![vec![50.0, 25.0]],
            ..RawDispatchPeriodResult::default()
        };
        let violations = screen_hvdc_violations(
            0,
            &[0.0, 0.0, 0.0],
            &dispatch,
            &hourly_network,
            &context,
            &options.input.hvdc_links,
            hourly_network.base_mva,
            1e-6,
            &HashSet::new(),
        );

        let cut = violations
            .iter()
            .find(|cut| cut.hvdc_link_idx == 0 && cut.monitored_branch_idx == 2)
            .expect("expected monitored branch 1-3 HVDC cut");
        let hvdc_ctg = context
            .hvdc_contingencies
            .iter()
            .find(|ctg| ctg.hvdc_idx == 0)
            .expect("HVDC contingency metadata");
        let ptdf_l = context.ptdf.row(2).expect("PTDF row for monitored branch");
        let expected_coefficients = [
            ptdf_l[hvdc_ctg.from_idx] - (1.0 - 0.0) * ptdf_l[hvdc_ctg.to_idx],
            ptdf_l[hvdc_ctg.from_idx] - (1.0 - 0.5) * ptdf_l[hvdc_ctg.to_idx],
        ];

        assert_eq!(
            cut.coefficient, None,
            "banded link should not use legacy link coefficient"
        );
        assert_eq!(cut.band_coefficients.len(), 2);
        assert!(
            (cut.band_coefficients[0].1 - expected_coefficients[0]).abs() < 1e-9,
            "firm-band coefficient mismatch: expected {}, got {}",
            expected_coefficients[0],
            cut.band_coefficients[0].1
        );
        assert!(
            (cut.band_coefficients[1].1 - expected_coefficients[1]).abs() < 1e-9,
            "economic-band coefficient mismatch: expected {}, got {}",
            expected_coefficients[1],
            cut.band_coefficients[1].1
        );
        // With uniform participation factors (`α_i = 1/|I|`) on a
        // symmetric 3-bus triangle, PTDF values at the HVDC from/to
        // buses can coincide. Relax to an approximate inequality that
        // tolerates this while still checking the `loss_b` effect when
        // PTDF endpoints differ.
        let diff = (cut.band_coefficients[0].1 - cut.band_coefficients[1].1).abs();
        let scale = cut.band_coefficients[0]
            .1
            .abs()
            .max(cut.band_coefficients[1].1.abs())
            .max(1e-12);
        assert!(
            diff > 1e-12 || ptdf_l[hvdc_ctg.to_idx].abs() < 1e-12,
            "distinct band losses should produce distinct security coefficients \
             unless PTDF at HVDC to-bus is zero (diff={diff:.2e}, scale={scale:.2e})"
        );
    }
}
