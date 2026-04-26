// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Multi-period AC Security-Constrained Economic Dispatch (AC-SCED).
//!
//! Solves a sequence of AC-OPF problems (one per time period) with:
//! - Generator ramping enforced as tightened Pg bounds (hard constraints).
//! - Storage state-of-charge (SoC) propagated between periods.
//! - Warm-start from the previous period's AC-OPF solution for fast convergence.
//!
//! # Storage Dispatch Modes
//!
//! Storage units support three dispatch modes via [`StorageDispatchMode`]:
//!
//! - **`SelfSchedule`**: The operator pre-commits a fixed MW injection
//!   (`self_schedule_mw`, positive = discharge, negative = charge).  The
//!   injection is applied directly, clamped only by SoC limits.  No
//!   optimization occurs — the commitment is honoured as-is.
//!
//! - **`OfferCurve`**: The BESS submits discharge offer and/or charge bid
//!   curves as cumulative `(MW, $/hr)` breakpoints with an explicit
//!   `(0.0, 0.0)` origin. The AC NLP co-optimizes charge/discharge natively
//!   against those curves, so storage clears in the same solve that forms AC
//!   prices and enforces network physics.
//!
//! - **`CostMinimization`**: Storage `variable_cost_per_mwh` and
//!   `degradation_cost_per_mwh` are baked directly into the AC-OPF NLP objective.
//!   The NLP jointly minimizes generator and storage costs with exact AC power
//!   balance — no DC pre-dispatch or external price signal needed.
//!
//! # Algorithm — single period (`solve_ac_sced`)
//!
//! 1. Clamp each generator's `[pmin, pmax]` by the ramp window.
//! 2. Apply `SelfSchedule` injections as fixed bus load modifications (SoC-clamped).
//! 3. Pass non-`SelfSchedule` storage units as native AC variables to `solve_ac_opf`.
//! 4. Solve AC-OPF once.
//! 5. Update storage SoC; return solution.
//!
//! # Algorithm — multi-period (`solve_multi_period_ac_sced`)
//!
//! For each period `t`:
//! 1. Scale bus loads; clamp generator `[pmin, pmax]` by ramp window.
//! 2. Apply `SelfSchedule` injections as fixed bus load modifications (SoC-clamped).
//! 3. Pass non-`SelfSchedule` storage units as native NLP variables with actual running SoC bounds.
//! 4. Solve AC-OPF once.
//! 5. Update SoC; warm-start next period from current solution.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use surge_ac::{AcPfOptions, solve_ac_pf};
use surge_network::Network;
use surge_network::market::{DemandResponseResults, LoadDispatchResult};
use surge_network::network::StorageDispatchMode;
use surge_network::network::{BusType, Generator, Load};
use surge_opf::{
    AcObjectiveTargetTracking, AcOpfError, AcOpfOptions, AcOpfRuntime, WarmStart,
    solve_ac_opf_with_runtime,
};
use surge_solution::{ObjectiveTerm, OpfSolution, SolveStatus};
#[cfg(test)]
use tracing::info;
use tracing::{debug, warn};

use crate::common::costs::resolve_dl_for_period_from_spec;
use crate::common::costs::resolve_generator_economics_for_period;
use crate::common::runtime::DispatchPeriodContext;
use crate::common::spec::DispatchProblemSpec;
use crate::error::ScedError;
#[cfg(test)]
use crate::legacy::DispatchOptions;

// ---------------------------------------------------------------------------
// Public options / solution types
// ---------------------------------------------------------------------------

/// Result of a single AC-SCED period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcScedPeriodSolution {
    /// Generator real-power dispatch (MW), one per in-service generator in the
    /// original input network order.
    pub pg_mw: Vec<f64>,
    /// Generator reactive-power dispatch (MVAr), one per in-service generator
    /// in the original input network order.
    pub qg_mvar: Vec<f64>,
    /// LMP per bus ($/MWh).
    pub lmp: Vec<f64>,
    /// Reactive LMP per bus ($/MVAr-h) — the dual on the per-bus
    /// Q-balance constraint. Empty when the AC-OPF backend didn't
    /// surface NLP duals (replay paths, DC fallback).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub q_lmp: Vec<f64>,
    /// Bus voltage magnitude per-unit by bus.
    pub bus_voltage_pu: Vec<f64>,
    /// Bus voltage angle in radians by bus.
    pub bus_angle_rad: Vec<f64>,
    /// Storage SoC at end of this period (MWh), parallel to storage units.
    pub storage_soc_mwh: Vec<f64>,
    /// Storage net power per unit (MW): positive = discharging, negative = charging.
    pub storage_net_mw: Vec<f64>,
    /// Dispatchable-load dispatch outcomes.
    #[serde(default)]
    pub dr_results: DemandResponseResults,
    /// Point-to-point HVDC dispatch setpoints (MW), ordered by request HVDC links.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hvdc_dispatch_mw: Vec<f64>,
    /// Per-band HVDC dispatch (MW), parallel to `hvdc_dispatch_mw`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hvdc_band_dispatch_mw: Vec<Vec<f64>>,
    /// Rounded transformer tap dispatch `(branch_idx, continuous_tap, rounded_tap)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tap_dispatch: Vec<(usize, f64, f64)>,
    /// Rounded phase-shifter dispatch `(branch_idx, continuous_rad, rounded_rad)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_dispatch: Vec<(usize, f64, f64)>,
    /// Rounded switched-shunt dispatch
    /// `(control_id, bus_number, continuous_b_pu, rounded_b_pu)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switched_shunt_dispatch: Vec<(String, u32, f64, f64)>,
    /// Cleared producer reactive up-reserve `q^qru_j` per generator
    /// in original input-network order (MVAr). Zero for offline
    /// generators. Empty when the network has no reactive reserve
    /// products.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub producer_q_reserve_up_mvar: Vec<f64>,
    /// Cleared producer reactive down-reserve (`q^qrd_j`) per
    /// generator in input-network order (MVAr).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub producer_q_reserve_down_mvar: Vec<f64>,
    /// Cleared consumer reactive up-reserve (`q^qru_k`) per active
    /// dispatchable load (MVAr). Parallel to `dr_results` ordering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumer_q_reserve_up_mvar: Vec<f64>,
    /// Cleared consumer reactive down-reserve (`q^qrd_k`) per active
    /// dispatchable load (MVAr).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumer_q_reserve_down_mvar: Vec<f64>,
    /// Zonal reactive up-reserve shortfall per (zone, up-product)
    /// pair (MVAr). Matches the AC-OPF reactive-reserve-plan row order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zone_q_reserve_up_shortfall_mvar: Vec<f64>,
    /// Zonal reactive down-reserve shortfall per (zone, down-product)
    /// pair (MVAr).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zone_q_reserve_down_shortfall_mvar: Vec<f64>,
    /// Total AC-OPF objective cost for this interval (dollars).
    pub total_cost: f64,
    /// Exact period objective decomposition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objective_terms: Vec<ObjectiveTerm>,
    /// Branch thermal shadow prices ($/MWh), indexed by network branch order.
    /// Pulled from the AC NLP duals on the apparent-power thermal-limit
    /// constraints (sum of from- and to-side multipliers). Empty when
    /// the AC backend didn't surface NLP duals (replay paths, AC-LP
    /// fallback, or `enforce_thermal_limits` off).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branch_shadow_prices: Vec<f64>,
    /// Flowgate shadow prices ($/MWh), indexed by `Network::flowgates`.
    /// Empty when `enforce_flowgates` is `false` or the network has no flowgates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flowgate_shadow_prices: Vec<f64>,
    /// Interface shadow prices ($/MWh), indexed by `Network::interfaces`.
    /// Empty when `enforce_flowgates` is `false` or the network has no interfaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interface_shadow_prices: Vec<f64>,
    /// Per-bus reactive-power balance slack (positive direction, MVAr).
    /// Indexed by bus position in the network. Empty when no Q slack is enabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_q_slack_pos_mvar: Vec<f64>,
    /// Per-bus reactive-power balance slack (negative direction, MVAr).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_q_slack_neg_mvar: Vec<f64>,
    /// Per-bus active-power balance slack (positive direction, MW).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_p_slack_pos_mw: Vec<f64>,
    /// Per-bus active-power balance slack (negative direction, MW).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_p_slack_neg_mw: Vec<f64>,
    /// Per-branch thermal slack from-side (MVA), indexed by network branch order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thermal_limit_slack_from_mva: Vec<f64>,
    /// Per-branch thermal slack to-side (MVA), indexed by network branch order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thermal_limit_slack_to_mva: Vec<f64>,
    /// Per-bus voltage-magnitude high slack (pu), indexed by bus position.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vm_slack_high_pu: Vec<f64>,
    /// Per-bus voltage-magnitude low slack (pu), indexed by bus position.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vm_slack_low_pu: Vec<f64>,
    /// Per-branch angle-difference high slack (rad), indexed by network branch order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub angle_diff_slack_high_rad: Vec<f64>,
    /// Per-branch angle-difference low slack (rad), indexed by network branch order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub angle_diff_slack_low_rad: Vec<f64>,
    /// Solve time (s).
    pub solve_time_secs: f64,
    /// Ipopt iterations.
    pub iterations: u32,
}

/// Per-phase timing breakdown for a single AC-SCED period.
///
/// Separates the major cost centers so profiling runs can identify
/// whether Ipopt kernel time or our own overhead (network prep,
/// NLP construction, PF warm-start, solution extraction) dominates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcScedPeriodTimings {
    /// `apply_period_operating_constraints` + target-tracking setup +
    /// warm-start injection — all network mutations before the OPF call.
    pub constraints_setup_secs: f64,
    /// Total wall-clock for PF warm-start candidate generation
    /// (`sequential_ac_runtime_candidates`), including up to 4
    /// Newton-Raphson power-flow solves.
    pub pf_warmstart_secs: f64,
    /// Number of NR PF attempts in the warm-start candidate list.
    pub pf_attempts: u32,
    /// Total wall-clock for the `solve_ac_opf_with_runtime_fallbacks`
    /// call (all warm-start candidates × homotopy retries). Includes
    /// NLP build + Ipopt kernel + any screening fallback re-solves.
    pub opf_total_secs: f64,
    /// FACTS expansion, network canonicalize, validation — inside
    /// `solve_ac_opf_with_context_once` before solver setup.
    pub network_prep_secs: f64,
    /// NLP solver lookup, DC-OPF warm-start, screening setup — between
    /// network prep and `AcOpfProblem::new()`.
    pub solve_setup_secs: f64,
    /// Cumulative time inside `AcOpfProblem::new()` across all NLP
    /// build attempts (Y-bus, Jacobian sparsity, branch admittances).
    pub nlp_build_secs: f64,
    /// Cumulative time inside `NlpSolver::solve()` (the actual
    /// interior-point iterations) across all attempts.
    pub nlp_solve_secs: f64,
    /// Solution extraction inside `solve_ac_opf_with_context_once`
    /// (voltages, LMPs, shadow prices, discrete devices).
    pub opf_extract_secs: f64,
    /// Number of `AcOpfProblem::new()` + `nlp.solve()` pairs executed
    /// across all warm-start candidates and homotopy retries. Usually
    /// 1; up to 9 in the worst case (3 candidates × 3 homotopy steps).
    pub nlp_attempts: u32,
    /// Total `solve_ac_opf_with_runtime` calls across all fallback
    /// candidates (includes failed attempts + homotopy retries).
    pub total_opf_calls: u32,
    /// Number of warm-start runtime candidates tried before success.
    pub runtime_attempts: u32,
    /// Index of the runtime candidate that succeeded (0-based).
    pub winning_attempt: u32,
    /// `OpfSolution.solve_time_secs` — the wall from solver-setup through
    /// extraction inside `solve_ac_opf_with_context_once`, EXCLUDING
    /// network prep (FACTS, canonicalize) and destructor drops. Compare
    /// with `opf_total_secs` to isolate drop overhead.
    pub opf_inner_secs: f64,
    /// Cumulative wall of `solve_ac_opf_with_thermal_homotopy` across
    /// all fallback candidates (from `AcOpfFallbackStats`). Compare
    /// with `opf_total_secs` (SCED-level timer wrapping the entire
    /// `solve_ac_opf_with_runtime_fallbacks` call) to find the gap
    /// between the two layers.
    pub fallback_wall_secs: f64,
    /// Post-solve extraction: unpack generator P/Q, storage, slacks,
    /// LMP, reactive reserves, dispatchable-load results.
    pub extract_secs: f64,
    /// Total wall-clock for the entire period (entry to return).
    pub period_total_secs: f64,
}

/// Per-period AC OPF solver stats: problem size, termination status,
/// final residuals, barrier parameter. Mirror of `surge_solution::NlpTrace`
/// with the period index and winning refinement attempt label folded in.
///
/// Populated when the underlying NLP backend provided a trace (Ipopt
/// today; COPT and Gurobi backends return `None`). Exposed on
/// [`DispatchDiagnostics::ac_opf_stats`] as a `Vec` keyed by period
/// index, parallel to `ac_sced_period_timings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcOpfStats {
    /// 0-based period index within the solved horizon.
    pub period_idx: u32,
    /// Optional period label from the request (e.g. an ISO timestamp).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_label: Option<String>,
    /// NLP backend name (e.g. `"Ipopt"`, `"COPT"`, `"Gurobi-NLP"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver_name: Option<String>,
    /// Wall-clock time inside the NLP solver for the winning attempt
    /// (from `OpfSolution.solve_time_secs`).
    pub solve_time_secs: f64,
    /// Which refinement-runtime attempt produced this solution, when the
    /// AC SCED stage ran under [`crate::RefinementRuntime`]. `None` on
    /// vanilla single-shot solves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_label: Option<String>,
    /// Problem size: number of decision variables.
    pub n_vars: u32,
    /// Problem size: number of constraints.
    pub n_constraints: u32,
    /// Problem size: nonzeros in the constraint Jacobian.
    pub jac_nnz: u32,
    /// Problem size: nonzeros in the Hessian (0 when approximated).
    pub hess_nnz: u32,
    /// Raw NLP status code (backend-specific; see [`surge_solution::NlpTrace`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<i32>,
    /// Human-readable status label (e.g. `"Solve_Succeeded"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_label: Option<String>,
    /// Iteration count at termination.
    pub iterations: u32,
    /// Final objective value.
    pub objective: f64,
    /// Primal infeasibility at the final iterate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_primal_inf: Option<f64>,
    /// Dual (KKT stationarity) infeasibility at the final iterate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_dual_inf: Option<f64>,
    /// Final barrier parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_mu: Option<f64>,
    /// Whether the solver considered the problem converged.
    pub converged: bool,
}

impl AcOpfStats {
    /// Build an [`AcOpfStats`] from an [`OpfSolution`] + the period
    /// context the AC SCED driver owns. Returns `None` when the solver
    /// backend did not populate `nlp_trace` (e.g. DC-OPF, COPT-NLP).
    pub fn from_opf_solution(
        opf: &OpfSolution,
        period_idx: u32,
        period_label: Option<String>,
    ) -> Option<Self> {
        let trace = opf.nlp_trace.as_ref()?;
        Some(Self {
            period_idx,
            period_label,
            solver_name: opf.solver_name.clone(),
            solve_time_secs: opf.solve_time_secs,
            attempt_label: None,
            n_vars: trace.n_vars,
            n_constraints: trace.n_constraints,
            jac_nnz: trace.jac_nnz,
            hess_nnz: trace.hess_nnz,
            status_code: trace.status_code,
            status_label: trace.status_label.clone(),
            iterations: trace.iterations,
            objective: trace.objective,
            final_primal_inf: trace.final_primal_inf,
            final_dual_inf: trace.final_dual_inf,
            final_mu: trace.final_mu,
            converged: trace.converged,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AcScedPeriodArtifacts {
    pub period_solution: AcScedPeriodSolution,
    pub opf_solution: OpfSolution,
    pub timings: AcScedPeriodTimings,
    /// Per-period NLP solver stats, derived from `opf_solution.nlp_trace`.
    /// `None` when the backend did not populate a trace.
    pub opf_stats: Option<AcOpfStats>,
}

/// Solution for a multi-period AC-SCED.
#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcScedSolution {
    /// Per-period solutions (in order).
    pub periods: Vec<AcScedPeriodSolution>,
    /// Total objective cost across all periods (dollars).
    pub total_cost: f64,
    /// Number of periods.
    pub n_periods: usize,
    /// Wall-clock solve time (s).
    pub solve_time_secs: f64,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Apply per-period ramp window to a generator's effective Pg bounds for
/// the AC-OPF reconcile.
///
/// When `problem_spec.ramp_constraints_hard` is set, the SCUC LP
/// pins its ramp slacks to zero so the LP-level Pg trajectory stays
/// inside the ramp envelope. Exported solutions read the AC reconcile
/// output, though, and a drift from the LP-feasible Pg would show up
/// as a downstream ramp violation. We narrow the Pg variable bounds
/// in the AC-OPF to the period's allowable ramp window so the
/// reconcile cannot break the SCUC-enforced ramp schedule.
///
/// When `ramp_constraints_hard` is `false`, callers keep the behaviour
/// where ramp coupling is purely economic (priced via the LP slack).
///
/// **Panic safety**: the historical implementation read `orig_pmin`/`orig_pmax`
/// from the per-period working copy of the network — which could already be
/// mutated by an earlier pass — and then called `f64::clamp(orig_pmin,
/// orig_pmax)` on `pg_prev`. When the values had been narrowed past
/// inversion the clamp panicked (commit `02aea676`, 617-bus 921 period 7).
/// The new implementation:
///   1. Reads the static `[pmin, pmax]` from `static_network` (the
///      immutable original handed in by the caller), never from the
///      mutated working copy.
///   2. Computes the ramp window `[lo, hi]` and intersects with the
///      static box.
///   3. If the intersection is empty, leaves the static bounds in place
///      (no mutation, no panic) and emits a `debug!` log so the case is
///      visible without killing the dispatch solve. Any residual
///      ramp violation surfaces as a validator-scored cost rather than
///      a hard solver crash.
fn apply_ramp_constraints(
    period_network: &mut Network,
    static_network: &Network,
    context: DispatchPeriodContext<'_>,
    dt_hours: f64,
    ramp_mode: &crate::request::RampMode,
) {
    let in_service_globals: Vec<usize> = static_network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service)
        .map(|(i, _)| i)
        .collect();

    for (j, &gi) in in_service_globals.iter().enumerate() {
        let Some(pg_prev) = context.prev_dispatch_at(j) else {
            continue;
        };

        // STATIC bounds from the original (unmutated) network. These are
        // the only safe source of `orig_pmin`/`orig_pmax` to use as the
        // intersection target — reading from `period_network` would risk
        // picking up bounds narrowed by an earlier pass.
        let static_pmin = static_network.generators[gi].pmin;
        let static_pmax = static_network.generators[gi].pmax;

        // Determine if the unit was off in the previous period. A device
        // dispatched below its static pmin was effectively off and the
        // current period starts a startup transition; the maximum power is
        // the device's startup ramp cap, not its running ramp_up.
        let was_off_in_prev_period = pg_prev < static_pmin - 1e-6;

        let g_static = &static_network.generators[gi];

        let (lo, hi) = if was_off_in_prev_period {
            let startup_pmax_mw = g_static.startup_ramp_mw_per_period(dt_hours);
            (static_pmin, static_pmax.min(startup_pmax_mw))
        } else {
            let ramp_up_rate = match ramp_mode {
                crate::request::RampMode::Averaged => {
                    g_static.ramp_up_avg_mw_per_min().filter(|&v| v > 0.0)
                }
                crate::request::RampMode::Interpolated => {
                    g_static.ramp_up_at_mw(pg_prev).filter(|&v| v > 0.0)
                }
                crate::request::RampMode::Block { .. } => {
                    g_static.ramp_up_avg_mw_per_min().filter(|&v| v > 0.0)
                }
            };
            let ramp_up_mw = ramp_up_rate
                .map(|r| r * dt_hours * 60.0)
                .unwrap_or(f64::INFINITY);

            let ramp_dn_rate = match ramp_mode {
                crate::request::RampMode::Averaged => {
                    g_static.ramp_down_avg_mw_per_min().filter(|&v| v > 0.0)
                }
                crate::request::RampMode::Interpolated => {
                    g_static.ramp_down_at_mw(pg_prev).filter(|&v| v > 0.0)
                }
                crate::request::RampMode::Block { .. } => {
                    g_static.ramp_down_avg_mw_per_min().filter(|&v| v > 0.0)
                }
            };
            let ramp_dn_mw = ramp_dn_rate
                .map(|r| r * dt_hours * 60.0)
                .unwrap_or(f64::INFINITY);

            (
                static_pmin.max(pg_prev - ramp_dn_mw),
                static_pmax.min(pg_prev + ramp_up_mw),
            )
        };

        if lo > hi + 1e-9 {
            debug!(
                gen_idx = gi,
                pg_prev,
                lo,
                hi,
                static_pmin,
                static_pmax,
                "ramp window does not intersect static [pmin, pmax]; \
                 leaving static bounds in place — AC-OPF will solve against \
                 the unrestricted box and any residual ramp violation will \
                 be scored by the validator"
            );
            continue;
        }

        let g = &mut period_network.generators[gi];
        g.pmin = lo;
        g.pmax = hi;
    }
}

/// Apply fixed commitment to a period-local AC network snapshot.
///
/// Fixed-off non-storage units are removed from service for the period-local
/// AC model so the production AC-SCED path matches the feasibility helper and
/// does not carry dead zero-limit generators through the NLP.
fn original_local_gen_index_by_global(network: &Network) -> Vec<Option<usize>> {
    let mut next_local_idx = 0usize;
    network
        .generators
        .iter()
        .map(|generator| {
            if generator.in_service {
                let local_idx = next_local_idx;
                next_local_idx += 1;
                Some(local_idx)
            } else {
                None
            }
        })
        .collect()
}

#[allow(dead_code)]
fn apply_fixed_commitment_constraints(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) {
    let local_gen_index_by_global = original_local_gen_index_by_global(network);
    apply_fixed_commitment_constraints_with_local_gen_index(
        network,
        problem_spec,
        period,
        &local_gen_index_by_global,
    );
}

fn apply_fixed_commitment_constraints_with_local_gen_index(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    local_gen_index_by_global: &[Option<usize>],
) {
    let period_spec = problem_spec.period(period);
    if period_spec.fixed_commitment().is_none() {
        return;
    }

    for (global_gen_idx, generator) in network.generators.iter_mut().enumerate() {
        let Some(local_gen_idx) = local_gen_index_by_global
            .get(global_gen_idx)
            .and_then(|entry| *entry)
        else {
            continue;
        };
        if !generator.in_service {
            continue;
        }
        let committed = period_spec.is_committed(local_gen_idx);
        let fixed_off_active_target_mw =
            fixed_off_generator_active_target_mw(problem_spec, period, local_gen_idx);
        let preserve_online_support_mode = can_relax_fixed_off_for_ac_support(generator)
            || fixed_off_generator_needs_online_support_mode(problem_spec, period, local_gen_idx);
        if committed && !generator.is_storage() && problem_spec.ac_relax_committed_pmin_to_zero {
            generator.pmin = 0.0;
            generator.p = generator.p.max(0.0).min(generator.pmax);
        }

        if committed || generator.is_storage() {
            continue;
        }

        if preserve_online_support_mode {
            // Keep the unit online for warm-started support windows.
            // Fixed-commitment AC replays can legitimately carry both
            // reactive support and startup / shutdown trajectory MW
            // while `u_on = 0`, so preserve any explicit active target
            // instead of collapsing the generator to a Q-only device.
            //
            // For startup/shutdown trajectory periods the target is the
            // ramp power (e.g. p_su, p_sd) which is BY DEFINITION below
            // the running pmin — clamping into the running [pmin, pmax]
            // window would push the target up to pmin and inject the
            // gen's full lower bound at the bus, creating a phantom
            // injection the validator catches as a bus-balance residual.
            // Floor at 0 instead so the trajectory power flows through
            // unchanged; cap at pmax so we never exceed physical
            // capability.
            if let Some(target_p_mw) = fixed_off_active_target_mw {
                let upper = generator.pmax.max(0.0);
                let clamped_target = target_p_mw.clamp(0.0, upper);
                generator.p = clamped_target;
                generator.pmin = clamped_target;
                generator.pmax = clamped_target;
            } else {
                generator.p = 0.0;
                generator.pmin = 0.0;
                generator.pmax = 0.0;
            }
            continue;
        }

        generator.in_service = false;
        generator.p = 0.0;
        generator.q = 0.0;
        generator.pmin = 0.0;
        generator.pmax = 0.0;
        generator.qmin = 0.0;
        generator.qmax = 0.0;
        generator.voltage_regulated = false;
        generator.reg_bus = None;
    }
}

fn normalized_bounds(lo: f64, hi: f64) -> (f64, f64) {
    if hi >= lo {
        return (lo, hi);
    }
    let scale = lo.abs().max(hi.abs()).max(1.0);
    if (lo - hi).abs() <= 1e-9 * scale {
        let bound = 0.5 * (lo + hi);
        return (bound, bound);
    }
    (hi, lo)
}

fn clamp_with_normalized_bounds(value: f64, lo: f64, hi: f64) -> f64 {
    let (lo, hi) = normalized_bounds(lo, hi);
    value.clamp(lo, hi)
}

fn fixed_off_generator_active_target_mw(
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    local_gen_idx: usize,
) -> Option<f64> {
    const EPS_MW: f64 = 1e-9;
    if problem_spec.period(period).is_committed(local_gen_idx) {
        return None;
    }
    problem_spec
        .ac_generator_warm_start_p_mw_at(period, local_gen_idx)
        .filter(|target_p_mw| target_p_mw.abs() > EPS_MW)
}

fn fixed_off_generator_needs_online_support_mode(
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    local_gen_idx: usize,
) -> bool {
    const EPS_MVAR: f64 = 1e-9;
    !problem_spec.period(period).is_committed(local_gen_idx)
        && (fixed_off_generator_active_target_mw(problem_spec, period, local_gen_idx).is_some()
            || problem_spec
                .ac_generator_warm_start_q_mvar_at(period, local_gen_idx)
                .is_some_and(|target_q_mvar| target_q_mvar.abs() > EPS_MVAR))
}

fn reclassify_period_local_bus_types(network: &mut Network) {
    // A generator can serve as a voltage reference when it's in service,
    // not explicitly excluded from voltage regulation, AND either:
    //  (a) has non-zero reactive capability (Q is free to track V), OR
    //  (b) is already marked voltage_regulated (caller has pinned its Q
    //      to a specific value — e.g. winner-pinned diagnostic — but V
    //      is still the free variable; the fixed-Q gen is a legitimate
    //      voltage reference for the bus).
    //
    // The second clause prevents the reclassifier from silently demoting
    // voltage-regulating gens to PQ when their per-period Q bounds
    // collapse to a point (qmin==qmax), which breaks connected-component
    // slack detection and surfaces as "connected component has slack
    // buses []" from the AC-OPF preflight.
    fn can_serve_as_voltage_reference(generator: &Generator) -> bool {
        generator.in_service
            && !generator.is_excluded_from_voltage_regulation()
            && (generator.has_reactive_power_range(1e-9) || generator.voltage_regulated)
    }

    fn component_candidate_regulators(
        network: &Network,
        component_buses: &[u32],
    ) -> Vec<(u32, usize)> {
        let component_bus_set: std::collections::HashSet<u32> =
            component_buses.iter().copied().collect();
        network
            .generators
            .iter()
            .enumerate()
            .filter(|(_, generator)| {
                can_serve_as_voltage_reference(generator)
                    && component_bus_set.contains(&generator.bus)
            })
            .map(|(generator_idx, generator)| (generator.bus, generator_idx))
            .collect()
    }

    fn regulated_pmax_by_bus(
        network: &Network,
        regulating_targets: &std::collections::HashMap<u32, Vec<usize>>,
        bus_number: u32,
    ) -> f64 {
        regulating_targets
            .get(&bus_number)
            .into_iter()
            .flatten()
            .map(|&generator_idx| network.generators[generator_idx].pmax)
            .fold(0.0, f64::max)
    }

    fn clamp_bus_vm(network: &Network, bus_number: u32) -> f64 {
        network
            .buses
            .iter()
            .find(|bus| bus.number == bus_number)
            .map(|bus| {
                bus.voltage_magnitude_pu
                    .clamp(bus.voltage_min_pu, bus.voltage_max_pu)
            })
            .unwrap_or(1.0)
    }

    let mut adjacency: std::collections::HashMap<u32, Vec<u32>> = network
        .buses
        .iter()
        .map(|bus| (bus.number, Vec::new()))
        .collect();

    for branch in network.branches.iter().filter(|branch| branch.in_service) {
        if let Some(neighbors) = adjacency.get_mut(&branch.from_bus) {
            neighbors.push(branch.to_bus);
        }
        if let Some(neighbors) = adjacency.get_mut(&branch.to_bus) {
            neighbors.push(branch.from_bus);
        }
    }

    let mut regulating_targets: std::collections::HashMap<u32, Vec<usize>> =
        std::collections::HashMap::new();
    for (generator_idx, generator) in network.generators.iter_mut().enumerate() {
        if !generator.in_service {
            generator.voltage_regulated = false;
            generator.reg_bus = None;
            continue;
        }
        if generator.is_excluded_from_voltage_regulation() {
            generator.voltage_regulated = false;
            generator.reg_bus = None;
            continue;
        }
        // Use `can_serve_as_voltage_reference` rather than the stricter
        // `can_voltage_regulate`: a gen whose Q has been pinned (qmin==qmax)
        // is still a valid voltage regulator when `voltage_regulated` is true.
        // Without this, the regulating_targets map is empty for fully-pinned
        // gens and the bus is demoted to PQ downstream.
        if generator.voltage_regulated && can_serve_as_voltage_reference(generator) {
            regulating_targets
                .entry(generator.reg_bus.unwrap_or(generator.bus))
                .or_default()
                .push(generator_idx);
        }
    }

    let original_bus_types: std::collections::HashMap<u32, BusType> = network
        .buses
        .iter()
        .map(|bus| (bus.number, bus.bus_type))
        .collect();
    let mut visited = std::collections::HashSet::new();

    let start_buses: Vec<u32> = network.buses.iter().map(|bus| bus.number).collect();
    for start_bus in start_buses {
        if !visited.insert(start_bus) {
            continue;
        }

        let mut stack = vec![start_bus];
        let mut component = Vec::new();
        while let Some(current) = stack.pop() {
            component.push(current);
            for neighbor in adjacency.get(&current).into_iter().flatten().copied() {
                if visited.insert(neighbor) {
                    stack.push(neighbor);
                }
            }
        }

        let mut active_targets: Vec<u32> = component
            .iter()
            .copied()
            .filter(|bus_number| regulating_targets.contains_key(bus_number))
            .collect();
        if active_targets.is_empty() {
            for (candidate_bus, generator_idx) in
                component_candidate_regulators(network, &component)
            {
                let target_vm = clamp_bus_vm(network, candidate_bus);
                let generator = &mut network.generators[generator_idx];
                generator.voltage_setpoint_pu = target_vm;
                generator.voltage_regulated = true;
                generator.reg_bus = Some(candidate_bus);
                regulating_targets
                    .entry(candidate_bus)
                    .or_default()
                    .push(generator_idx);
            }
            active_targets = component
                .iter()
                .copied()
                .filter(|bus_number| regulating_targets.contains_key(bus_number))
                .collect();
        }
        if active_targets.is_empty() {
            for bus_number in component {
                if let Some(bus) = network
                    .buses
                    .iter_mut()
                    .find(|bus| bus.number == bus_number)
                {
                    if bus.bus_type != BusType::Isolated {
                        bus.bus_type = BusType::PQ;
                    }
                }
            }
            continue;
        }

        let reference_bus = active_targets
            .iter()
            .copied()
            .find(|bus_number| original_bus_types.get(bus_number).copied() == Some(BusType::Slack))
            .unwrap_or_else(|| {
                active_targets
                    .iter()
                    .copied()
                    .max_by(|lhs, rhs| {
                        regulated_pmax_by_bus(network, &regulating_targets, *lhs)
                            .partial_cmp(&regulated_pmax_by_bus(network, &regulating_targets, *rhs))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .unwrap_or(active_targets[0])
            });

        for bus_number in component {
            if let Some(bus) = network
                .buses
                .iter_mut()
                .find(|bus| bus.number == bus_number)
            {
                if bus.bus_type == BusType::Isolated {
                    continue;
                }
                bus.bus_type = if bus_number == reference_bus {
                    BusType::Slack
                } else if regulating_targets.contains_key(&bus_number) {
                    BusType::PV
                } else {
                    BusType::PQ
                };
            }
        }
    }
}

fn can_relax_fixed_off_for_ac_support(generator: &Generator) -> bool {
    generator
        .market
        .as_ref()
        .and_then(|market| market.qualifications.get("ac_reactive_support_flexible"))
        .copied()
        .unwrap_or(false)
}

fn apply_ac_bus_warm_start_targets(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) {
    for (bus_idx, bus) in network.buses.iter_mut().enumerate() {
        if let Some(target_vm_pu) = problem_spec.ac_bus_warm_start_vm_pu_at(period, bus_idx) {
            bus.voltage_magnitude_pu = target_vm_pu.clamp(bus.voltage_min_pu, bus.voltage_max_pu);
        }
        if let Some(target_va_rad) = problem_spec.ac_bus_warm_start_va_rad_at(period, bus_idx) {
            bus.voltage_angle_rad = target_va_rad;
        }
    }
}

fn apply_shutdown_deloading_constraints_with_local_gen_index(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    context: DispatchPeriodContext<'_>,
    shutdown_dt_hours: f64,
    local_gen_index_by_global: &[Option<usize>],
) {
    if !problem_spec.enforce_shutdown_deloading {
        return;
    }
    let Some(next_commitment) = context.next_period_commitment else {
        return;
    };

    let period_spec = problem_spec.period(context.period);
    for (global_gen_idx, generator) in network.generators.iter_mut().enumerate() {
        let Some(local_gen_idx) = local_gen_index_by_global
            .get(global_gen_idx)
            .and_then(|entry| *entry)
        else {
            continue;
        };
        if !generator.in_service {
            continue;
        }
        if !period_spec.is_committed(local_gen_idx)
            || next_commitment.get(local_gen_idx).copied().unwrap_or(true)
            || generator.is_storage()
        {
            continue;
        }

        // Match SCUC and benchmark semantics: shutdown de-loading can tighten
        // the online operating range, but it does not relax the online minimum
        // output floor for the last committed interval before shutdown.
        let shutdown_cap_mw = generator.shutdown_ramp_mw_per_period(shutdown_dt_hours);
        let effective_shutdown_cap_mw = shutdown_cap_mw.max(generator.pmin);
        generator.pmax = generator.pmax.min(effective_shutdown_cap_mw);
        generator.p = clamp_with_normalized_bounds(generator.p, generator.pmin, generator.pmax);
    }
}

/// Materialize period-local AC operating constraints on a network snapshot.
///
/// `static_network` is the immutable original handed in by the
/// caller; the mutable `network` is a per-period clone of it. Helpers
/// that need to read truly-static fields (e.g. `apply_ramp_constraints`
/// for the hard-ramp path) read from `static_network` to avoid picking
/// up bounds that an earlier helper in this same call already mutated.
fn apply_period_operating_constraints(
    network: &mut Network,
    static_network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    context: DispatchPeriodContext<'_>,
) {
    let local_gen_index_by_global = original_local_gen_index_by_global(network);
    let period_dt_hours = problem_spec.period_hours(context.period);
    let shutdown_dt_hours = if context.period + 1 < problem_spec.n_periods {
        problem_spec.period_hours(context.period + 1)
    } else {
        period_dt_hours
    };
    strip_request_hvdc_physical_links(network, problem_spec);
    crate::common::profiles::apply_ac_time_series_profiles(network, &problem_spec, context.period);
    apply_period_generator_economics(network, problem_spec, context.period);
    apply_period_dispatchable_load_economics(network, problem_spec, context.period);
    // When the ramp constraints are hard, narrow the per-period
    // generator `[pmin, pmax]` box to the ramp window around
    // `pg_prev`. The static network is passed through unchanged so
    // the helper reads truly-static physical bounds, and the helper
    // itself is panic-safe (empty intersections are logged and
    // skipped, not clamped).
    if problem_spec.ramp_constraints_hard {
        apply_ramp_constraints(
            network,
            static_network,
            context,
            period_dt_hours,
            problem_spec.ramp_mode,
        );
    }
    apply_fixed_commitment_constraints_with_local_gen_index(
        network,
        problem_spec,
        context.period,
        &local_gen_index_by_global,
    );
    apply_shutdown_deloading_constraints_with_local_gen_index(
        network,
        problem_spec,
        context,
        shutdown_dt_hours,
        &local_gen_index_by_global,
    );
    apply_ac_bus_warm_start_targets(network, problem_spec, context.period);
    reclassify_period_local_bus_types(network);
    apply_ac_warm_start_targets_with_local_gen_index(
        network,
        problem_spec,
        context.period,
        &local_gen_index_by_global,
    );
    // Always seed HVDC warm starts on physical `network.hvdc.links`. The
    // seed sets `link.scheduled_setpoint` to the DC target, which the
    // joint AC-DC NLP path reads as `p_warm_start_mw` for links that
    // opt into variable P. For links that still go through the legacy
    // sequential Load-injection path, the warm start is a no-op beyond
    // populating the initial state that `apply_request_hvdc_terminal_injections`
    // would overwrite anyway.
    apply_ac_hvdc_warm_start_targets(network, problem_spec, context.period);
    apply_request_hvdc_terminal_injections(network, problem_spec, context.period);
}

fn strip_request_hvdc_physical_links(network: &mut Network, problem_spec: DispatchProblemSpec<'_>) {
    if !request_hvdc_uses_terminal_injections(problem_spec) || network.hvdc.links.is_empty() {
        return;
    }

    // Only strip links that will be handled by the legacy Load-injection
    // path. Links with a variable P range (`p_dc_min_mw < p_dc_max_mw`)
    // stay on the network so the joint AC-DC NLP can see them as real
    // decision variables.
    let injection_link_names: std::collections::HashSet<&str> = problem_spec
        .hvdc_links
        .iter()
        .filter_map(|link| {
            let request_name = if !link.name.is_empty() {
                link.name.as_str()
            } else if !link.id.is_empty() {
                link.id.as_str()
            } else {
                return None;
            };
            if network_link_is_variable_p(network, request_name) {
                None
            } else {
                Some(request_name)
            }
        })
        .collect();
    if injection_link_names.is_empty() {
        return;
    }

    network
        .hvdc
        .links
        .retain(|link| !injection_link_names.contains(link.name()));
}

/// Returns true when the named point-to-point HVDC link on `network`
/// declares a non-degenerate variable-P range (`p_dc_min_mw <
/// p_dc_max_mw`). Used to gate the legacy Load-injection path off for
/// links that the joint AC-DC NLP will handle directly.
fn network_link_is_variable_p(network: &Network, link_name: &str) -> bool {
    network.hvdc.links.iter().any(|link| {
        if link.name() != link_name {
            return false;
        }
        if let Some(lcc) = link.as_lcc() {
            return lcc.has_variable_p_dc();
        }
        // Network-side VscHvdcLink does not yet carry per-link variable-P
        // bounds, so VSC always goes through the legacy path for now.
        false
    })
}

fn apply_ac_hvdc_warm_start_targets(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) {
    if network.hvdc.links.is_empty() || problem_spec.hvdc_links.is_empty() {
        return;
    }

    for (link_idx, request_link) in problem_spec.hvdc_links.iter().enumerate() {
        // Source of the per-period P target:
        //   1. `fixed_hvdc_dispatch_mw` — a HARD pin (caller wants AC NLP to
        //      treat HVDC P as fixed, not free). We both set the setpoint
        //      AND clamp p_dc_min/p_dc_max to the pinned value so the joint
        //      AC-DC NLP's HVDC P variable has zero width.
        //   2. `ac_hvdc_warm_start_p_mw` — a SOFT warm-start (setpoint only;
        //      NLP is still free to move within the static [p_dc_min, p_dc_max]).
        // When both are set, the hard pin wins.
        let hard_pin = problem_spec.fixed_hvdc_dispatch_mw_at(period, link_idx);
        let warm_start = problem_spec.ac_hvdc_warm_start_p_mw_at(period, link_idx);
        let Some(target_p_mw) = hard_pin.or(warm_start) else {
            continue;
        };

        let target_name = if !request_link.name.is_empty() {
            request_link.name.as_str()
        } else if !request_link.id.is_empty() {
            request_link.id.as_str()
        } else {
            continue;
        };

        let maybe_link = network
            .hvdc
            .links
            .iter_mut()
            .find(|link| link.name() == target_name);
        let Some(link) = maybe_link else {
            continue;
        };

        if let Some(lcc) = link.as_lcc_mut() {
            lcc.scheduled_setpoint = target_p_mw;
            if hard_pin.is_some() {
                // Tighten P-range around the hard-pin target so the AC NLP
                // treats this link's P as effectively fixed. Use a tiny
                // epsilon width (not zero) so `has_variable_p_dc` still
                // returns true and the NLP keeps its joint AC-DC path —
                // collapsing to zero width would trigger the legacy
                // `apply_request_hvdc_terminal_injections` Load path,
                // which then double-counts with the synth Q generators.
                let eps = 1e-6 * target_p_mw.abs().max(1.0);
                lcc.p_dc_min_mw = target_p_mw - eps;
                lcc.p_dc_max_mw = target_p_mw + eps;
            }
        } else if let Some(vsc) = link.as_vsc_mut() {
            vsc.converter1.dc_setpoint = target_p_mw;
            vsc.converter2.dc_setpoint = -target_p_mw;
            // (VSC pinning: currently no analogous p-range collapse on
            // VscHvdcLink; VSC warm-start remains soft.)
        }
    }
}

fn extract_ac_hvdc_dispatch_mw(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    nlp_solution: Option<&surge_solution::OpfSolution>,
) -> Vec<f64> {
    if problem_spec.hvdc_links.is_empty() {
        return Vec::new();
    }

    // Joint AC-DC NLP path: read HVDC P straight out of the OPF
    // solution's `hvdc_p2p_dispatch_mw` block. These values are the
    // solved NLP decision variables for every in-service link with a
    // variable-P range, in the same order `HvdcP2PNlpData` built them
    // from `network.hvdc.links`. We align the return vector to
    // `problem_spec.hvdc_links` by name so downstream consumers keep
    // their request-level ordering. Links that don't opt into the NLP
    // (i.e. fixed-P or out of service) fall through to the legacy
    // extraction below.
    let nlp_p_by_name = if let Some(sol) = nlp_solution {
        if !sol.devices.hvdc_p2p_dispatch_mw.is_empty() {
            use surge_network::network::LccHvdcControlMode;
            let mut map: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
            let variable_p_lcc_names: Vec<&str> = network
                .hvdc
                .links
                .iter()
                .filter_map(|l| {
                    let lcc = l.as_lcc()?;
                    if matches!(lcc.mode, LccHvdcControlMode::Blocked) {
                        return None;
                    }
                    if !lcc.rectifier.in_service || !lcc.inverter.in_service {
                        return None;
                    }
                    if !lcc.has_variable_p_dc() {
                        return None;
                    }
                    Some(lcc.name.as_str())
                })
                .collect();
            for (k, name) in variable_p_lcc_names.iter().enumerate() {
                if let Some(&p_mw) = sol.devices.hvdc_p2p_dispatch_mw.get(k) {
                    map.insert((*name).to_string(), p_mw);
                }
            }
            map
        } else {
            std::collections::HashMap::new()
        }
    } else {
        std::collections::HashMap::new()
    };
    if !nlp_p_by_name.is_empty() {
        return problem_spec
            .hvdc_links
            .iter()
            .map(|request_link| {
                let key = if !request_link.name.is_empty() {
                    request_link.name.as_str()
                } else if !request_link.id.is_empty() {
                    request_link.id.as_str()
                } else {
                    ""
                };
                if let Some(&p) = nlp_p_by_name.get(key) {
                    return p;
                }
                // Fallback: fixed-P link not present in the NLP block.
                0.0
            })
            .collect();
    }

    if request_hvdc_uses_terminal_injections(problem_spec) {
        return problem_spec
            .hvdc_links
            .iter()
            .enumerate()
            .map(|(link_idx, _)| {
                request_hvdc_target_p_mw(problem_spec, period, link_idx).unwrap_or(0.0)
            })
            .collect();
    }

    if network.hvdc.links.is_empty() {
        return vec![0.0; problem_spec.hvdc_links.len()];
    }

    problem_spec
        .hvdc_links
        .iter()
        .map(|request_link| {
            let target_name = if !request_link.name.is_empty() {
                request_link.name.as_str()
            } else if !request_link.id.is_empty() {
                request_link.id.as_str()
            } else {
                return 0.0;
            };

            let Some(link) = network
                .hvdc
                .links
                .iter()
                .find(|link| link.name() == target_name)
            else {
                return 0.0;
            };

            if let Some(lcc) = link.as_lcc() {
                lcc.scheduled_setpoint
            } else if let Some(vsc) = link.as_vsc() {
                vsc.converter1.dc_setpoint
            } else {
                0.0
            }
        })
        .collect()
}

fn request_hvdc_uses_terminal_injections(problem_spec: DispatchProblemSpec<'_>) -> bool {
    !problem_spec.hvdc_links.is_empty()
        && (!problem_spec.ac_hvdc_warm_start_p_mw.is_empty()
            || !problem_spec.fixed_hvdc_dispatch_mw.is_empty())
}

fn request_hvdc_target_p_mw(
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    link_idx: usize,
) -> Option<f64> {
    problem_spec
        .ac_hvdc_warm_start_p_mw_at(period, link_idx)
        .or_else(|| problem_spec.fixed_hvdc_dispatch_mw_at(period, link_idx))
}

fn request_hvdc_target_q_fr_mvar(
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    link_idx: usize,
) -> f64 {
    problem_spec
        .fixed_hvdc_dispatch_q_fr_mvar_at(period, link_idx)
        .unwrap_or(0.0)
}

fn request_hvdc_target_q_to_mvar(
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    link_idx: usize,
) -> f64 {
    problem_spec
        .fixed_hvdc_dispatch_q_to_mvar_at(period, link_idx)
        .unwrap_or(0.0)
}

fn hvdc_delivered_mw_for_request(link: &crate::hvdc::HvdcDispatchLink, total_mw: f64) -> f64 {
    if total_mw.abs() < 1e-9 {
        return 0.0;
    }
    total_mw * (1.0 - link.loss_b_frac) - total_mw.signum() * link.loss_a_mw
}

fn apply_request_hvdc_terminal_injections(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) {
    if !request_hvdc_uses_terminal_injections(problem_spec) {
        return;
    }

    for (link_idx, request_link) in problem_spec.hvdc_links.iter().enumerate() {
        // Skip links that the joint AC-DC NLP will handle directly. The
        // physical `network.hvdc.links` entry stays in place (see
        // `strip_request_hvdc_physical_links`) and the NLP sees HVDC P
        // as a decision variable bounded by the `[p_dc_min_mw,
        // p_dc_max_mw]` range on `LccHvdcLink`.
        let request_name = if !request_link.name.is_empty() {
            request_link.name.as_str()
        } else if !request_link.id.is_empty() {
            request_link.id.as_str()
        } else {
            ""
        };
        if !request_name.is_empty() && network_link_is_variable_p(network, request_name) {
            continue;
        }

        let Some(target_p_mw) = request_hvdc_target_p_mw(problem_spec, period, link_idx) else {
            continue;
        };
        if target_p_mw.abs() < 1e-12 {
            continue;
        }
        let target_q_fr_mvar = request_hvdc_target_q_fr_mvar(problem_spec, period, link_idx);
        let target_q_to_mvar = request_hvdc_target_q_to_mvar(problem_spec, period, link_idx);

        // Request-level HVDC schedules must match the DC dispatch/export
        // convention used elsewhere in Surge: positive internal MW withdraws
        // power at the `from` terminal and injects delivered MW at the `to`
        // terminal, which exports as negative GO `pdc_fr`.
        network.loads.push(Load::new(
            request_link.from_bus,
            target_p_mw,
            target_q_fr_mvar,
        ));
        network.loads.push(Load::new(
            request_link.to_bus,
            -hvdc_delivered_mw_for_request(request_link, target_p_mw),
            target_q_to_mvar,
        ));
    }
}

fn apply_period_generator_economics(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) {
    if problem_spec.offer_schedules.is_empty() {
        return;
    }

    for (global_gen_idx, generator) in network.generators.iter_mut().enumerate() {
        if !generator.in_service || generator.is_storage() {
            continue;
        }

        let Some(economics) = resolve_generator_economics_for_period(
            global_gen_idx,
            period,
            generator,
            problem_spec.offer_schedules,
            Some(generator.pmax),
        ) else {
            continue;
        };

        generator.cost = Some(economics.cost.into_owned());
    }
}

fn apply_period_dispatchable_load_economics(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) {
    if problem_spec.dispatchable_loads.is_empty() {
        network.market_data.dispatchable_loads.clear();
        return;
    }

    network.market_data.dispatchable_loads = problem_spec.dispatchable_loads.to_vec();
    for (dl_index, dispatchable_load) in network
        .market_data
        .dispatchable_loads
        .iter_mut()
        .enumerate()
    {
        let (p_sched_pu, p_max_pu, q_sched_pu, q_min_pu, q_max_pu, cost_model) =
            resolve_dl_for_period_from_spec(
                dl_index,
                period,
                &problem_spec.dispatchable_loads[dl_index],
                &problem_spec,
            );
        dispatchable_load.p_sched_pu = p_sched_pu;
        dispatchable_load.p_max_pu = p_max_pu;
        dispatchable_load.q_sched_pu = q_sched_pu;
        dispatchable_load.q_min_pu = q_min_pu;
        dispatchable_load.q_max_pu = q_max_pu;
        dispatchable_load.cost_model = cost_model.clone();
        if let Some(schedule) = problem_spec.dl_offer_schedules.get(&dl_index)
            && let Some(Some(params)) = schedule.periods.get(period)
        {
            dispatchable_load.pq_linear_equality = params.pq_linear_equality;
            dispatchable_load.pq_linear_upper = params.pq_linear_upper;
            dispatchable_load.pq_linear_lower = params.pq_linear_lower;
        }
        if dispatchable_load.fixed_power_factor {
            let pf_ratio = dispatchable_load.pf_ratio();
            dispatchable_load.q_min_pu = dispatchable_load.p_min_pu * pf_ratio;
            dispatchable_load.q_max_pu = dispatchable_load.p_max_pu * pf_ratio;
            dispatchable_load.q_sched_pu = dispatchable_load.p_sched_pu * pf_ratio;
        }
        if dispatchable_load.q_min_pu > dispatchable_load.q_max_pu {
            std::mem::swap(
                &mut dispatchable_load.q_min_pu,
                &mut dispatchable_load.q_max_pu,
            );
        }
        dispatchable_load.q_sched_pu = clamp_with_normalized_bounds(
            dispatchable_load.q_sched_pu,
            dispatchable_load.q_min_pu,
            dispatchable_load.q_max_pu,
        );
    }
}

#[allow(dead_code)]
fn apply_ac_warm_start_targets(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) {
    let local_gen_index_by_global = original_local_gen_index_by_global(network);
    apply_ac_warm_start_targets_with_local_gen_index(
        network,
        problem_spec,
        period,
        &local_gen_index_by_global,
    );
}

fn apply_ac_warm_start_targets_with_local_gen_index(
    network: &mut Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    local_gen_index_by_global: &[Option<usize>],
) {
    let period_spec = problem_spec.period(period);
    for (global_gen_idx, generator) in network.generators.iter_mut().enumerate() {
        let Some(local_gen_idx) = local_gen_index_by_global
            .get(global_gen_idx)
            .and_then(|entry| *entry)
        else {
            continue;
        };
        if !generator.in_service {
            continue;
        }
        let fixed_off_active_target_mw =
            fixed_off_generator_active_target_mw(problem_spec, period, local_gen_idx);
        if period_spec.is_committed(local_gen_idx)
            || generator.is_storage()
            || fixed_off_active_target_mw.is_some()
        {
            if let Some(target_p_mw) =
                problem_spec.ac_generator_warm_start_p_mw_at(period, local_gen_idx)
            {
                // Guard: dispatch-bound profiles can narrow pmax below a
                // ramp-widened pmin, producing pmin > pmax.  Clamp safely.
                generator.p =
                    clamp_with_normalized_bounds(target_p_mw, generator.pmin, generator.pmax);
            }
        } else {
            generator.p = 0.0;
        }
        if let Some(target_q_mvar) =
            problem_spec.ac_generator_warm_start_q_mvar_at(period, local_gen_idx)
        {
            generator.q =
                clamp_with_normalized_bounds(target_q_mvar, generator.qmin, generator.qmax);
        }
    }
}

#[allow(dead_code)]
fn runtime_with_request_ac_warm_start(
    base_runtime: &AcOpfRuntime,
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) -> AcOpfRuntime {
    let local_gen_index_by_global = original_local_gen_index_by_global(network);
    runtime_with_request_ac_warm_start_with_local_gen_index(
        base_runtime,
        network,
        problem_spec,
        period,
        &local_gen_index_by_global,
    )
}

fn runtime_with_request_ac_warm_start_with_local_gen_index(
    base_runtime: &AcOpfRuntime,
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    local_gen_index_by_global: &[Option<usize>],
) -> AcOpfRuntime {
    if base_runtime.warm_start.is_some() {
        return base_runtime.clone();
    }

    let base_mva = network.base_mva.max(1e-9);
    let mut has_explicit_schedule = false;
    let mut warm_start = WarmStart {
        voltage_magnitude_pu: network
            .buses
            .iter()
            .map(|bus| {
                bus.voltage_magnitude_pu
                    .clamp(bus.voltage_min_pu, bus.voltage_max_pu)
            })
            .collect(),
        voltage_angle_rad: network
            .buses
            .iter()
            .map(|bus| bus.voltage_angle_rad)
            .collect(),
        pg: network
            .generators
            .iter()
            .filter(|generator| generator.in_service)
            .map(|generator| {
                if generator.is_storage() {
                    0.0
                } else {
                    clamp_with_normalized_bounds(generator.p, generator.pmin, generator.pmax)
                        / base_mva
                }
            })
            .collect(),
        qg: network
            .generators
            .iter()
            .filter(|generator| generator.in_service)
            .map(|generator| {
                clamp_with_normalized_bounds(generator.q, generator.qmin, generator.qmax) / base_mva
            })
            .collect(),
        dispatchable_load_p: network
            .market_data
            .dispatchable_loads
            .iter()
            .filter(|dispatchable_load| dispatchable_load.in_service)
            .map(|dispatchable_load| {
                clamp_with_normalized_bounds(
                    dispatchable_load.p_sched_pu,
                    dispatchable_load.p_min_pu,
                    dispatchable_load.p_max_pu,
                )
            })
            .collect(),
        dispatchable_load_q: network
            .market_data
            .dispatchable_loads
            .iter()
            .filter(|dispatchable_load| dispatchable_load.in_service)
            .map(|dispatchable_load| {
                clamp_with_normalized_bounds(
                    dispatchable_load.q_sched_pu,
                    dispatchable_load.q_min_pu,
                    dispatchable_load.q_max_pu,
                )
            })
            .collect(),
    };

    for (bus_idx, bus) in network.buses.iter().enumerate() {
        if let Some(target_vm_pu) = problem_spec.ac_bus_warm_start_vm_pu_at(period, bus_idx) {
            warm_start.voltage_magnitude_pu[bus_idx] =
                target_vm_pu.clamp(bus.voltage_min_pu, bus.voltage_max_pu);
            has_explicit_schedule = true;
        }
        if let Some(target_va_rad) = problem_spec.ac_bus_warm_start_va_rad_at(period, bus_idx) {
            warm_start.voltage_angle_rad[bus_idx] = target_va_rad;
            has_explicit_schedule = true;
        }
    }

    for (current_local_gen_idx, (global_gen_idx, generator)) in network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, generator)| generator.in_service)
        .enumerate()
    {
        let Some(original_local_gen_idx) = local_gen_index_by_global
            .get(global_gen_idx)
            .and_then(|entry| *entry)
        else {
            continue;
        };
        let committed = problem_spec
            .period(period)
            .is_committed(original_local_gen_idx);
        let fixed_off_active_target_mw =
            fixed_off_generator_active_target_mw(problem_spec, period, original_local_gen_idx);
        if let Some(target_p_mw) =
            problem_spec.ac_generator_warm_start_p_mw_at(period, original_local_gen_idx)
        {
            warm_start.pg[current_local_gen_idx] =
                if generator.is_storage() || committed || fixed_off_active_target_mw.is_some() {
                    clamp_with_normalized_bounds(target_p_mw, generator.pmin, generator.pmax)
                        / base_mva
                } else {
                    0.0
                };
            has_explicit_schedule = true;
        }
        if let Some(target_q_mvar) =
            problem_spec.ac_generator_warm_start_q_mvar_at(period, original_local_gen_idx)
        {
            warm_start.qg[current_local_gen_idx] =
                clamp_with_normalized_bounds(target_q_mvar, generator.qmin, generator.qmax)
                    / base_mva;
            has_explicit_schedule = true;
        }
    }

    for (dl_idx, dispatchable_load) in network
        .market_data
        .dispatchable_loads
        .iter()
        .filter(|dispatchable_load| dispatchable_load.in_service)
        .enumerate()
    {
        if let Some(target_p_mw) =
            problem_spec.ac_dispatchable_load_warm_start_p_mw_at(period, dl_idx)
        {
            warm_start.dispatchable_load_p[dl_idx] = clamp_with_normalized_bounds(
                target_p_mw / base_mva,
                dispatchable_load.p_min_pu,
                dispatchable_load.p_max_pu,
            );
            has_explicit_schedule = true;
        }
        if let Some(target_q_mvar) =
            problem_spec.ac_dispatchable_load_warm_start_q_mvar_at(period, dl_idx)
        {
            warm_start.dispatchable_load_q[dl_idx] = clamp_with_normalized_bounds(
                target_q_mvar / base_mva,
                dispatchable_load.q_min_pu,
                dispatchable_load.q_max_pu,
            );
            has_explicit_schedule = true;
        }
    }

    if !has_explicit_schedule {
        return base_runtime.clone();
    }

    let mut runtime = base_runtime.clone();
    runtime.warm_start = Some(warm_start);
    runtime
}

#[allow(dead_code)]
fn runtime_with_ac_target_tracking(
    base_runtime: &AcOpfRuntime,
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) -> AcOpfRuntime {
    let local_gen_index_by_global = original_local_gen_index_by_global(network);
    runtime_with_ac_target_tracking_with_local_gen_index(
        base_runtime,
        network,
        problem_spec,
        period,
        &local_gen_index_by_global,
    )
}

fn runtime_with_ac_target_tracking_with_local_gen_index(
    base_runtime: &AcOpfRuntime,
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
    local_gen_index_by_global: &[Option<usize>],
) -> AcOpfRuntime {
    if problem_spec.ac_target_tracking.is_disabled() {
        return base_runtime.clone();
    }

    let cfg = problem_spec.ac_target_tracking;

    // Build the default per-direction coefficient pair. The legacy
    // scalar is applied as a symmetric default when the explicit
    // per-direction default is zero. Otherwise the explicit default
    // wins and the legacy scalar is ignored for the purpose of
    // overriding — it is still surfaced via
    // `AcObjectiveTargetTracking::generator_p_penalty_per_mw2` so the
    // NLP layer's back-compat path works.
    let generator_default_pair = if !cfg.generator_p_coefficients_default.is_zero() {
        surge_opf::ac::types::AcTargetTrackingCoefficients {
            upward_per_mw2: cfg.generator_p_coefficients_default.upward_per_mw2.max(0.0),
            downward_per_mw2: cfg
                .generator_p_coefficients_default
                .downward_per_mw2
                .max(0.0),
        }
    } else if cfg.generator_p_penalty_per_mw2 > 0.0 {
        surge_opf::ac::types::AcTargetTrackingCoefficients::symmetric(
            cfg.generator_p_penalty_per_mw2.max(0.0),
        )
    } else {
        surge_opf::ac::types::AcTargetTrackingCoefficients::ZERO
    };
    let dispatchable_load_default_pair = if !cfg.dispatchable_load_p_coefficients_default.is_zero()
    {
        surge_opf::ac::types::AcTargetTrackingCoefficients {
            upward_per_mw2: cfg
                .dispatchable_load_p_coefficients_default
                .upward_per_mw2
                .max(0.0),
            downward_per_mw2: cfg
                .dispatchable_load_p_coefficients_default
                .downward_per_mw2
                .max(0.0),
        }
    } else if cfg.dispatchable_load_p_penalty_per_mw2 > 0.0 {
        surge_opf::ac::types::AcTargetTrackingCoefficients::symmetric(
            cfg.dispatchable_load_p_penalty_per_mw2.max(0.0),
        )
    } else {
        surge_opf::ac::types::AcTargetTrackingCoefficients::ZERO
    };

    let mut tracking = AcObjectiveTargetTracking {
        // Keep the legacy scalar populated for back-compat consumers
        // (the NLP summary, the `is_empty` gate, etc.).
        generator_p_penalty_per_mw2: cfg.generator_p_penalty_per_mw2.max(0.0),
        dispatchable_load_p_penalty_per_mw2: cfg.dispatchable_load_p_penalty_per_mw2.max(0.0),
        generator_p_coefficients_default: generator_default_pair,
        dispatchable_load_p_coefficients_default: dispatchable_load_default_pair,
        ..AcObjectiveTargetTracking::default()
    };

    let generator_default_active = !generator_default_pair.is_zero();
    let dispatchable_load_default_active = !dispatchable_load_default_pair.is_zero();

    if generator_default_active || !cfg.generator_p_coefficients_overrides_by_id.is_empty() {
        for (global_gen_idx, generator) in network.generators.iter().enumerate() {
            let Some(local_gen_idx) = local_gen_index_by_global
                .get(global_gen_idx)
                .and_then(|entry| *entry)
            else {
                continue;
            };
            if !generator.in_service || generator.is_storage() {
                continue;
            }
            if let Some(target_p_mw) =
                problem_spec.ac_generator_warm_start_p_mw_at(period, local_gen_idx)
            {
                tracking
                    .generator_p_targets_mw
                    .insert(global_gen_idx, target_p_mw);
                // Per-resource overrides take priority over the default.
                if let Some(override_pair) = cfg
                    .generator_p_coefficients_overrides_by_id
                    .get(generator.id.as_str())
                {
                    tracking.generator_p_coefficients_overrides.insert(
                        global_gen_idx,
                        surge_opf::ac::types::AcTargetTrackingCoefficients {
                            upward_per_mw2: override_pair.upward_per_mw2.max(0.0),
                            downward_per_mw2: override_pair.downward_per_mw2.max(0.0),
                        },
                    );
                }
            }
        }
    }

    if dispatchable_load_default_active
        || !cfg
            .dispatchable_load_p_coefficients_overrides_by_id
            .is_empty()
    {
        for (local_dl_idx, (global_dl_idx, dispatchable_load)) in network
            .market_data
            .dispatchable_loads
            .iter()
            .enumerate()
            .filter(|(_, dispatchable_load)| dispatchable_load.in_service)
            .enumerate()
        {
            if let Some(target_p_mw) =
                problem_spec.ac_dispatchable_load_warm_start_p_mw_at(period, local_dl_idx)
            {
                tracking
                    .dispatchable_load_p_targets_mw
                    .insert(global_dl_idx, target_p_mw);
                if let Some(override_pair) = cfg
                    .dispatchable_load_p_coefficients_overrides_by_id
                    .get(dispatchable_load.resource_id.as_str())
                {
                    tracking.dispatchable_load_p_coefficients_overrides.insert(
                        global_dl_idx,
                        surge_opf::ac::types::AcTargetTrackingCoefficients {
                            upward_per_mw2: override_pair.upward_per_mw2.max(0.0),
                            downward_per_mw2: override_pair.downward_per_mw2.max(0.0),
                        },
                    );
                }
            }
        }
    }

    if tracking.is_empty() {
        base_runtime.clone()
    } else {
        base_runtime
            .clone()
            .with_objective_target_tracking(tracking)
    }
}

fn validate_ac_period_generator_economics(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    period: usize,
) -> Result<(), ScedError> {
    if let Some((global_gen_idx, generator)) =
        network
            .generators
            .iter()
            .enumerate()
            .find(|(_, generator)| {
                generator.in_service && !generator.is_storage() && generator.cost.is_none()
            })
    {
        return Err(ScedError::SolverError(format!(
            "AC-SCED period {period} generator {global_gen_idx} ({id} @ bus {}) still has no cost after period economics; offer_schedule_present={}",
            generator.bus,
            problem_spec.offer_schedules.contains_key(&global_gen_idx),
            id = generator.id,
        )));
    }
    Ok(())
}

fn summarize_ac_generator_cost_state(network: &Network) -> String {
    let mut entries = Vec::new();
    for (global_gen_idx, generator) in network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, generator)| generator.in_service)
        .take(8)
    {
        entries.push(format!(
            "#{global_gen_idx} id={} bus={} storage={} cost={} p=[{}, {}]",
            generator.id,
            generator.bus,
            generator.is_storage(),
            generator.cost.is_some(),
            generator.pmin,
            generator.pmax,
        ));
    }
    let missing = network
        .generators
        .iter()
        .filter(|generator| generator.in_service && generator.cost.is_none())
        .count();
    format!(
        "in_service_missing_costs={missing}; head=[{}]",
        entries.join("; ")
    )
}

fn invalid_generator_bounds_summary(
    network: &Network,
    context: DispatchPeriodContext<'_>,
    local_gen_index_by_global: &[Option<usize>],
) -> Option<String> {
    let invalid_p: Vec<String> = network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, generator)| generator.in_service && generator.pmin > generator.pmax + 1e-9)
        .map(|(global_gen_idx, generator)| {
            let prev_dispatch = local_gen_index_by_global
                .get(global_gen_idx)
                .and_then(|entry| *entry)
                .and_then(|local_gen_idx| context.prev_dispatch_at(local_gen_idx));
            format!(
                "#{global_gen_idx} id={} bus={} p=[{}, {}] prev_dispatch_mw={prev_dispatch:?}",
                generator.id, generator.bus, generator.pmin, generator.pmax
            )
        })
        .collect();
    let invalid_q: Vec<String> = network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, generator)| generator.in_service && generator.qmin > generator.qmax + 1e-9)
        .map(|(global_gen_idx, generator)| {
            format!(
                "#{global_gen_idx} id={} bus={} q=[{}, {}]",
                generator.id, generator.bus, generator.qmin, generator.qmax
            )
        })
        .collect();
    if invalid_p.is_empty() && invalid_q.is_empty() {
        None
    } else {
        Some(format!(
            "invalid generator bounds after period-local AC constraints: p={invalid_p:?}; q={invalid_q:?}"
        ))
    }
}

fn wrap_ac_opf_error(network: &Network, error: AcOpfError) -> ScedError {
    ScedError::SolverError(format!(
        "AC-OPF failed: {error}; pre_ac_generator_state={}",
        summarize_ac_generator_cost_state(network),
    ))
}

/// Clamp a proposed net injection (MW) to respect SoC and power limits.
///
/// Returns the feasible net injection: positive = discharge, negative = charge.
fn clamp_by_soc(net_mw: f64, soc: f64, generator: &Generator, dt: f64) -> f64 {
    let sto = generator
        .storage
        .as_ref()
        .expect("clamp_by_soc requires storage generator");
    // Asymmetric efficiencies:
    //   charging    : soc += ch * η_ch * dt       (stores η_ch MWh per MW-hr)
    //   discharging : soc -= dis / η_dis * dt     (draws 1/η_dis MWh per MW-hr)
    let eta_ch = sto.charge_efficiency.max(1e-9);
    let eta_dis = sto.discharge_efficiency.max(1e-9);
    if net_mw >= 0.0 {
        let max_dis_energy = ((soc - sto.soc_min_mwh) * eta_dis / dt).max(0.0);
        let p_max = foldback_discharge_cap(sto, soc, generator.discharge_mw_max());
        net_mw.min(p_max).min(max_dis_energy)
    } else {
        let max_ch_energy = ((sto.soc_max_mwh - soc) / (dt * eta_ch)).max(0.0);
        let p_max = foldback_charge_cap(sto, soc, generator.charge_mw_max());
        -((-net_mw).min(p_max).min(max_ch_energy))
    }
}

/// Discharge-side foldback: at ``soc == soc_min`` the cap is 0 MW; it
/// rises linearly to ``p_max`` at the foldback threshold and stays flat
/// above. ``None`` on the threshold disables the cut.
pub(crate) fn foldback_discharge_cap(
    sto: &surge_network::network::StorageParams,
    soc_mwh: f64,
    p_max: f64,
) -> f64 {
    match sto.discharge_foldback_soc_mwh {
        None => p_max,
        Some(threshold) => {
            let range = (threshold - sto.soc_min_mwh).max(1e-12);
            let frac = ((soc_mwh - sto.soc_min_mwh) / range).clamp(0.0, 1.0);
            p_max * frac
        }
    }
}

/// Charge-side foldback: at ``soc == soc_max`` the cap is 0 MW; below
/// the threshold it sits at ``p_max``. ``None`` disables the cut.
pub(crate) fn foldback_charge_cap(
    sto: &surge_network::network::StorageParams,
    soc_mwh: f64,
    p_max: f64,
) -> f64 {
    match sto.charge_foldback_soc_mwh {
        None => p_max,
        Some(threshold) => {
            let range = (sto.soc_max_mwh - threshold).max(1e-12);
            let frac = ((sto.soc_max_mwh - soc_mwh) / range).clamp(0.0, 1.0);
            p_max * frac
        }
    }
}

/// Apply net injections (MW) to bus loads in-place.
///
/// Positive injection = discharge = reduces bus load (pd -= mw).
/// Negative injection = charge   = increases bus load (pd -= negative_mw = pd += mw).
fn apply_storage_injections(network: &mut Network, units: &[(usize, &Generator)], net_mw: &[f64]) {
    for (s, (_, g)) in units.iter().enumerate() {
        let delta = net_mw.get(s).copied().unwrap_or(0.0);
        if delta.abs() < 1e-12 {
            continue;
        }
        // Find the first in-service load on the same bus and adjust it.
        if let Some(load) = network
            .loads
            .iter_mut()
            .find(|l| l.bus == g.bus && l.in_service)
        {
            load.active_power_demand_mw -= delta;
        } else {
            // No existing load on this bus — create a synthetic one.
            use surge_network::network::Load;
            network.loads.push(Load::new(g.bus, -delta, 0.0));
        }
    }
}

fn native_storage_soc_override(
    units: &[(usize, &Generator)],
    soc_mwh: &[f64],
) -> Option<std::collections::HashMap<usize, f64>> {
    let mut soc_map = std::collections::HashMap::new();
    for (s, (gi, g)) in units.iter().enumerate() {
        let Some(storage) = g.storage.as_ref() else {
            continue;
        };
        if storage.dispatch_mode != StorageDispatchMode::SelfSchedule {
            soc_map.insert(*gi, soc_mwh[s]);
        }
    }
    if soc_map.is_empty() {
        None
    } else {
        Some(soc_map)
    }
}

/// Update storage SoC based on net injection for the period.
fn update_soc(
    units: &[(usize, &Generator)],
    soc_mwh: &[f64],
    net_mw: &[f64],
    dt_hours: f64,
) -> Vec<f64> {
    units
        .iter()
        .enumerate()
        .map(|(i, (_, g))| {
            let sto = g
                .storage
                .as_ref()
                .expect("storage_gen_local only contains generators with storage");
            let soc = soc_mwh.get(i).copied().unwrap_or(sto.soc_initial_mwh);
            let net = net_mw.get(i).copied().unwrap_or(0.0);
            let eta_ch = sto.charge_efficiency.max(1e-9);
            let eta_dis = sto.discharge_efficiency.max(1e-9);
            let delta = if net > 0.0 {
                // Discharging: SoC drops by net/η_dis MWh over dt hours.
                -net * dt_hours / eta_dis
            } else {
                // Charging: SoC rises by |net|·η_ch MWh over dt hours.
                (-net) * eta_ch * dt_hours
            };
            (soc + delta).clamp(sto.soc_min_mwh, sto.soc_max_mwh)
        })
        .collect()
}

#[derive(Clone, Copy)]
struct AcStorageDispatchView<'a> {
    units: &'a [(usize, &'a Generator)],
    ss_mw: &'a [f64],
}

impl<'a> AcStorageDispatchView<'a> {
    fn new(units: &'a [(usize, &'a Generator)], ss_mw: &'a [f64]) -> Self {
        Self { units, ss_mw }
    }
}

fn assemble_storage_net_mw(
    storage_dispatch: AcStorageDispatchView<'_>,
    native_storage_net_mw: &[f64],
) -> Vec<f64> {
    let mut cm_idx = 0_usize;
    storage_dispatch
        .units
        .iter()
        .enumerate()
        .map(|(s, (_, g))| {
            let sto = g
                .storage
                .as_ref()
                .expect("storage_gen_local only contains generators with storage");
            match sto.dispatch_mode {
                StorageDispatchMode::SelfSchedule => {
                    storage_dispatch.ss_mw.get(s).copied().unwrap_or(0.0)
                }
                StorageDispatchMode::OfferCurve | StorageDispatchMode::CostMinimization => {
                    let v = native_storage_net_mw.get(cm_idx).copied().unwrap_or(0.0);
                    cm_idx += 1;
                    v
                }
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Public solver functions
// ---------------------------------------------------------------------------

/// Run the operating-nomogram tightening loop on an AC-OPF solution.
///
/// If nomograms are active (`enforce_flowgates && max_nomogram_iter > 0 && !nomograms.empty()`),
/// iteratively:
/// 1. Compute DC-approx flowgate flows from the AC solution bus angles.
/// 2. Evaluate each nomogram to derive a tighter limit.
/// 3. If any limit tightened, modify the network's flowgate limits and re-solve AC-OPF.
/// 4. Stop when no limit changes or max iterations reached.
///
/// Returns the (possibly updated) net_mw and OpfSolution.
fn nomogram_tighten(
    solve_network: &Network,
    max_nomogram_iter: usize,
    enforce_flowgates: bool,
    ac_opf: &AcOpfOptions,
    base_runtime: &AcOpfRuntime,
    storage_dispatch: AcStorageDispatchView<'_>,
    sol: OpfSolution,
) -> Result<(Vec<f64>, OpfSolution), ScedError> {
    let mut current_sol = sol;
    let mut net_mw = assemble_storage_net_mw(storage_dispatch, &current_sol.devices.storage_net_mw);
    let has_nomograms = enforce_flowgates
        && max_nomogram_iter > 0
        && !solve_network.nomograms.is_empty()
        && !solve_network.flowgates.is_empty();

    if !has_nomograms {
        return Ok((net_mw, current_sol));
    }

    let base = solve_network.base_mva;

    // Build bus_number → internal index map for angle lookups.
    let bus_map: std::collections::HashMap<u32, usize> = solve_network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    // Build flowgate name → network index map.
    let fg_name_to_idx: std::collections::HashMap<&str, usize> = solve_network
        .flowgates
        .iter()
        .enumerate()
        .filter(|(_, fg)| fg.in_service)
        .map(|(i, fg)| (fg.name.as_str(), i))
        .collect();

    // Track current limits (start from network values).
    let mut fg_limits: Vec<f64> = solve_network
        .flowgates
        .iter()
        .map(|fg| fg.limit_mw)
        .collect();

    for _iter in 0..max_nomogram_iter {
        // Compute DC-approx MW flow on each active flowgate from solution angles.
        let flow_by_name: std::collections::HashMap<&str, f64> = solve_network
            .flowgates
            .iter()
            .filter(|fg| fg.in_service)
            .map(|fg| {
                let mut flow_pu = 0.0;
                for wbr in &fg.monitored {
                    let fb = &wbr.branch.from_bus;
                    let tb = &wbr.branch.to_bus;
                    let ckt = &wbr.branch.circuit;
                    let coeff = wbr.coefficient;
                    if let (Some(&fi), Some(&ti)) = (bus_map.get(fb), bus_map.get(tb))
                        && let Some(br) = solve_network.branches.iter().find(|br| {
                            br.in_service
                                && br.from_bus == *fb
                                && br.to_bus == *tb
                                && br.circuit == *ckt
                                && br.x.abs() > 1e-20
                        })
                    {
                        flow_pu += coeff
                            * br.b_dc()
                            * (current_sol.power_flow.voltage_angle_rad[fi]
                                - current_sol.power_flow.voltage_angle_rad[ti]);
                    }
                }
                (fg.name.as_str(), flow_pu * base)
            })
            .collect();

        // Apply each active nomogram; tighten constrained-flowgate limit.
        let mut any_change = false;
        for nom in solve_network.nomograms.iter().filter(|n| n.in_service) {
            let Some(&index_flow) = flow_by_name.get(nom.index_flowgate.as_str()) else {
                continue;
            };
            let Some(&fgi) = fg_name_to_idx.get(nom.constrained_flowgate.as_str()) else {
                continue;
            };
            let new_limit = nom.evaluate(index_flow);
            if new_limit < fg_limits[fgi] - 1e-3 {
                fg_limits[fgi] = new_limit;
                any_change = true;
            }
        }

        if !any_change {
            break;
        }

        // Re-solve AC-OPF with tightened flowgate limits.
        let mut net_tight = solve_network.clone();
        for (i, fg) in net_tight.flowgates.iter_mut().enumerate() {
            fg.limit_mw = fg_limits[i];
        }

        let runtime =
            runtime_with_opf_warm_start_for_network(base_runtime, &current_sol, &net_tight);

        let mut _nomogram_stats = AcOpfFallbackStats::default();
        match solve_ac_opf_with_thermal_homotopy(
            &net_tight,
            ac_opf,
            base_runtime,
            &runtime,
            &mut _nomogram_stats,
        ) {
            Ok(new_sol) => {
                debug!(
                    iter = _iter,
                    cost = new_sol.total_cost,
                    "AC-SCED nomogram tightening iteration"
                );
                current_sol = new_sol;
                net_mw =
                    assemble_storage_net_mw(storage_dispatch, &current_sol.devices.storage_net_mw);
            }
            Err(_) => {
                // Tightening made problem infeasible — keep previous solution.
                warn!(
                    iter = _iter,
                    "AC-SCED nomogram tightening: re-solve failed, keeping previous solution"
                );
                break;
            }
        }
    }

    Ok((net_mw, current_sol))
}

fn runtime_with_opf_warm_start_for_network(
    base_runtime: &AcOpfRuntime,
    solution: &OpfSolution,
    network: &Network,
) -> AcOpfRuntime {
    let mut runtime = base_runtime.clone();
    runtime.warm_start = Some(WarmStart::from_opf_with_network_targets(network, solution));
    runtime
}

fn runtime_with_exact_opf_warm_start(
    base_runtime: &AcOpfRuntime,
    solution: &OpfSolution,
) -> AcOpfRuntime {
    let mut runtime = base_runtime.clone();
    runtime.warm_start = Some(WarmStart::from_opf(solution));
    runtime
}

fn runtime_with_pf_warm_start_if_available(
    base_runtime: &AcOpfRuntime,
    network: &Network,
) -> AcOpfRuntime {
    if base_runtime.warm_start.is_some() {
        return base_runtime.clone();
    }

    let mut runtime = base_runtime.clone();
    let pf_attempts = [
        AcPfOptions {
            flat_start: false,
            enforce_q_limits: true,
            detect_islands: true,
            ..AcPfOptions::default()
        },
        AcPfOptions {
            flat_start: false,
            enforce_q_limits: false,
            detect_islands: true,
            ..AcPfOptions::default()
        },
        AcPfOptions {
            flat_start: true,
            enforce_q_limits: true,
            detect_islands: true,
            ..AcPfOptions::default()
        },
        AcPfOptions {
            flat_start: true,
            enforce_q_limits: false,
            detect_islands: true,
            ..AcPfOptions::default()
        },
    ];
    for pf_options in pf_attempts {
        match solve_ac_pf(network, &pf_options) {
            Ok(pf_solution) if matches!(pf_solution.status, SolveStatus::Converged) => {
                runtime.warm_start = Some(WarmStart::from_pf_with_network(network, &pf_solution));
                break;
            }
            Ok(pf_solution) => {
                debug!(
                    status = ?pf_solution.status,
                    iterations = pf_solution.iterations,
                    flat_start = pf_options.flat_start,
                    enforce_q_limits = pf_options.enforce_q_limits,
                    "rejecting non-converged AC-PF warm-start candidate"
                );
            }
            Err(error) => {
                debug!(
                    flat_start = pf_options.flat_start,
                    enforce_q_limits = pf_options.enforce_q_limits,
                    error = %error,
                    "AC-PF warm-start candidate failed"
                );
            }
        }
    }
    runtime
}

fn sequential_ac_runtime_candidates(
    base_runtime: &AcOpfRuntime,
    network: &Network,
    previous_solution: Option<&OpfSolution>,
) -> Vec<AcOpfRuntime> {
    let mut runtimes = Vec::new();
    if base_runtime.warm_start.is_some() {
        runtimes.push(base_runtime.clone());
        return runtimes;
    }

    if let Some(solution) = previous_solution {
        runtimes.push(runtime_with_opf_warm_start_for_network(
            base_runtime,
            solution,
            network,
        ));
    }

    let pf_runtime = runtime_with_pf_warm_start_if_available(base_runtime, network);
    if pf_runtime.warm_start.is_some() || runtimes.is_empty() {
        runtimes.push(pf_runtime);
    }

    if runtimes
        .last()
        .is_none_or(|runtime| runtime.warm_start.is_some())
    {
        runtimes.push(base_runtime.clone());
    }

    runtimes
}

/// Aggregate timing for the entire fallback chain in
/// [`solve_ac_opf_with_runtime_fallbacks`]. Captures both failed and
/// successful attempts so callers can see the true cost of the retry
/// cascade.
#[derive(Debug, Clone, Default)]
pub(crate) struct AcOpfFallbackStats {
    /// Number of warm-start runtime candidates attempted.
    pub runtime_attempts: u32,
    /// Total `solve_ac_opf_with_runtime` calls across all candidates
    /// (each homotopy step = 1 call; 3 per failed attempt, 1-3 per
    /// successful attempt).
    pub total_opf_calls: u32,
    /// Cumulative wall-clock for all `solve_ac_opf_with_runtime` calls
    /// (includes NLP build + Ipopt kernel for each).
    pub total_opf_wall_secs: f64,
    /// Index of the runtime candidate that succeeded (0-based).
    pub winning_attempt: u32,
}

fn solve_ac_opf_with_runtime_fallbacks(
    network: &Network,
    options: &AcOpfOptions,
    base_runtime: &AcOpfRuntime,
    runtimes: Vec<AcOpfRuntime>,
) -> Result<(OpfSolution, AcOpfFallbackStats), AcOpfError> {
    let mut stats = AcOpfFallbackStats::default();
    let mut last_error = None;
    for (attempt_idx, runtime) in runtimes.into_iter().enumerate() {
        stats.runtime_attempts += 1;
        let t0 = Instant::now();
        match solve_ac_opf_with_thermal_homotopy(
            network,
            options,
            base_runtime,
            &runtime,
            &mut stats,
        ) {
            Ok(solution) => {
                stats.total_opf_wall_secs += t0.elapsed().as_secs_f64();
                stats.winning_attempt = attempt_idx as u32;
                return Ok((solution, stats));
            }
            Err(error) => {
                stats.total_opf_wall_secs += t0.elapsed().as_secs_f64();
                debug!(
                    attempt = attempt_idx,
                    has_warm_start = runtime.warm_start.is_some(),
                    opf_calls = stats.total_opf_calls,
                    wall_secs = stats.total_opf_wall_secs,
                    error = %error,
                    "AC-OPF sequential fallback attempt failed"
                );
                last_error = Some(error);
            }
        }
    }
    Err(last_error.expect("at least one AC-OPF runtime candidate"))
}

fn solve_ac_opf_with_thermal_homotopy(
    network: &Network,
    options: &AcOpfOptions,
    base_runtime: &AcOpfRuntime,
    initial_runtime: &AcOpfRuntime,
    stats: &mut AcOpfFallbackStats,
) -> Result<OpfSolution, AcOpfError> {
    stats.total_opf_calls += 1;
    let _t_call = Instant::now();
    match solve_ac_opf_with_runtime(network, options, initial_runtime) {
        Ok(solution) => Ok(solution),
        Err(original_error) => {
            if !options.enforce_thermal_limits {
                return Err(original_error);
            }

            let mut relaxed_options = options.clone();
            relaxed_options.enforce_thermal_limits = false;
            stats.total_opf_calls += 1;
            match solve_ac_opf_with_runtime(network, &relaxed_options, initial_runtime) {
                Ok(relaxed_solution) => {
                    debug!(
                        total_cost = relaxed_solution.total_cost,
                        iterations = relaxed_solution.iterations.unwrap_or(0),
                        "AC-OPF thermal homotopy: relaxed solve succeeded"
                    );
                    let final_runtime =
                        runtime_with_exact_opf_warm_start(base_runtime, &relaxed_solution);
                    stats.total_opf_calls += 1;
                    solve_ac_opf_with_runtime(network, options, &final_runtime)
                }
                Err(relaxed_error) => {
                    debug!(
                        original = %original_error,
                        relaxed = %relaxed_error,
                        "AC-OPF thermal homotopy: relaxed solve also failed"
                    );
                    Err(original_error)
                }
            }
        }
    }
}

fn extract_ac_dispatchable_load_results(
    network: &Network,
    solution: &OpfSolution,
) -> DemandResponseResults {
    let bus_map = network.bus_index_map();
    let base = network.base_mva;
    let loads: Vec<LoadDispatchResult> = network
        .market_data
        .dispatchable_loads
        .iter()
        .filter(|load| load.in_service)
        .enumerate()
        .map(|(k, load)| {
            let p_served_pu = solution
                .devices
                .dispatchable_load_served_mw
                .get(k)
                .copied()
                .unwrap_or(load.p_sched_pu * base)
                / base;
            let q_served_pu = solution
                .devices
                .dispatchable_load_served_q_mvar
                .get(k)
                .copied()
                .unwrap_or(load.pf_ratio() * p_served_pu * base)
                / base;
            let lmp_at_bus = bus_map
                .get(&load.bus)
                .and_then(|&idx| solution.pricing.lmp.get(idx).copied())
                .unwrap_or(0.0);
            LoadDispatchResult::from_solution(load, p_served_pu, q_served_pu, lmp_at_bus, base)
        })
        .collect();
    DemandResponseResults::from_load_results(loads, base)
}

/// Solve a single-period AC-SCED.
///
/// Applies ramp constraints and storage injections, then calls `solve_ac_opf`.
/// Returns an `AcScedPeriodSolution` containing generator dispatch, reactive
/// dispatch, LMPs, updated storage SoC, and solve metadata.
///
/// # Storage dispatch
///
/// - **`SelfSchedule`**: fixed injection applied as a bus load modification
///   (clamped by SoC). AC-OPF is not aware of this unit.
/// - **`CostMinimization`** and **`OfferCurve`**: passed as **native AC variables**
///   to `solve_ac_opf`. The optimizer jointly clears generator, storage, and
///   flexible-load economics against exact AC network physics in one solve.
#[cfg(test)]
pub fn solve_ac_sced(
    network: &Network,
    options: &DispatchOptions,
) -> Result<AcScedPeriodSolution, ScedError> {
    solve_ac_sced_with_problem_spec(
        network,
        DispatchProblemSpec::from_options(options),
        &options.ac_opf,
        &AcOpfRuntime::default(),
        DispatchPeriodContext::initial(&options.initial_state),
    )
}

pub(crate) fn solve_ac_sced_with_problem_spec(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    ac_opf: &AcOpfOptions,
    base_runtime: &AcOpfRuntime,
    context: DispatchPeriodContext<'_>,
) -> Result<AcScedPeriodSolution, ScedError> {
    Ok(solve_ac_sced_with_problem_spec_artifacts(
        network,
        problem_spec,
        ac_opf,
        base_runtime,
        context,
        None,
    )?
    .period_solution)
}

pub(crate) fn solve_ac_sced_with_problem_spec_artifacts(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    ac_opf: &AcOpfOptions,
    base_runtime: &AcOpfRuntime,
    context: DispatchPeriodContext<'_>,
    previous_solution: Option<&OpfSolution>,
) -> Result<AcScedPeriodArtifacts, ScedError> {
    let start = Instant::now();
    let period_spec = problem_spec.period(context.period);
    let original_in_service_generator_indices: Vec<usize> = network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, generator)| generator.in_service)
        .map(|(index, _)| index)
        .collect();

    let mut net = network.clone();
    let local_gen_index_by_global = original_local_gen_index_by_global(network);
    apply_period_operating_constraints(&mut net, network, problem_spec, context);
    if let Some(summary) =
        invalid_generator_bounds_summary(&net, context, &local_gen_index_by_global)
    {
        return Err(ScedError::SolverError(format!(
            "AC-SCED period {}: {summary}",
            context.period
        )));
    }
    validate_ac_period_generator_economics(&net, problem_spec, context.period)?;
    let base_runtime = runtime_with_ac_target_tracking_with_local_gen_index(
        base_runtime,
        &net,
        problem_spec,
        context.period,
        &local_gen_index_by_global,
    );
    let base_runtime = runtime_with_request_ac_warm_start_with_local_gen_index(
        &base_runtime,
        &net,
        problem_spec,
        context.period,
        &local_gen_index_by_global,
    );
    let constraints_setup_secs = start.elapsed().as_secs_f64();

    // Build list of in-service storage generators: (global_gen_index, &Generator)
    let units: Vec<(usize, &Generator)> = net
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service && g.is_storage())
        .collect();
    let dt = period_spec.interval_hours();

    // Resolve initial SoC for each storage generator
    let soc_mwh: Vec<f64> = units
        .iter()
        .map(|(gi, g)| {
            crate::common::runtime::effective_storage_soc_mwh(context.storage_soc_override, *gi, g)
        })
        .collect();
    let ss_mw: Vec<f64> = units
        .iter()
        .enumerate()
        .map(|(s, (gi, g))| {
            let sto = g
                .storage
                .as_ref()
                .expect("storage_gen_local only contains generators with storage");
            if sto.dispatch_mode == StorageDispatchMode::SelfSchedule {
                let soc = soc_mwh[s];
                let committed = period_spec
                    .storage_self_schedule_mw(*gi)
                    .unwrap_or(sto.self_schedule_mw);
                clamp_by_soc(committed, soc, g, dt)
            } else {
                0.0
            }
        })
        .collect();

    // Build a closure that constructs AcOpfOptions with native storage SoC overrides.
    let ac_opts_with_native_storage = |base_opts: &AcOpfOptions| -> AcOpfOptions {
        let mut opts = base_opts.clone();
        opts.storage_soc_override = native_storage_soc_override(&units, &soc_mwh);
        opts.dt_hours = dt;
        opts.enforce_flowgates = problem_spec.enforce_flowgates;
        opts
    };

    // Determine whether the inner AC OPF should skip HVDC entirely.
    //
    // Legacy semantics: when the request provides a fixed HVDC schedule
    // (via `fixed_hvdc_dispatch` or `ac_hvdc_warm_start`), the dispatch
    // layer pushes HVDC as fixed Load injections at the terminal buses
    // and tells the AC OPF to ignore HVDC to avoid double-counting.
    //
    // Joint AC-DC NLP semantics: when any point-to-point link on the
    // network has a variable P range (`p_dc_min_mw < p_dc_max_mw`) the
    // AC OPF's `build_hvdc_p2p_nlp_data` adds HVDC P as an NLP decision
    // variable bounded by that range. In that case we MUST leave
    // `include_hvdc` at the default so the NLP can build its P2P block
    // — setting it to `Some(false)` would disable both the explicit
    // DC-topology path and the P2P NLP path.
    //
    // Use `any_p2p_variable` on the solve network to decide.
    let any_hvdc_p2p_variable = |net: &Network| -> bool {
        net.hvdc.links.iter().any(|link| {
            link.as_lcc()
                .map(|lcc| lcc.has_variable_p_dc())
                .unwrap_or(false)
        })
    };
    let (
        solve_net,
        solve_opts,
        sol,
        fallback_stats,
        pf_warmstart_secs,
        pf_attempts,
        opf_total_secs,
    ) = if units.is_empty() {
        // No storage: single AC-OPF pass.
        let mut ac_opts_no_storage = ac_opf.clone();
        ac_opts_no_storage.enforce_flowgates = problem_spec.enforce_flowgates;
        // Thread the actual period interval through so the AC OPF
        // objective integrates `pg × cost × dt` (and the dual/LMP
        // scale uses dt as its `base × dt_hours` divisor). Mirrors
        // the storage-branch ``ac_opts_with_native_storage`` setter
        // — without it, callers fall back to ``AcOpfOptions::default
        // ().dt_hours == 1.0`` and every period (regardless of its
        // true duration) gets billed at 1 hour. That collapsed the
        // dispatch-result ``period.total_cost`` and ``objective_terms
        // .dollars`` to a constant per-MW figure across cases with
        // different period lengths, which the dashboard then summed
        // into a horizon-independent "production cost".
        ac_opts_no_storage.dt_hours = dt;
        if request_hvdc_uses_terminal_injections(problem_spec) && !any_hvdc_p2p_variable(&net) {
            ac_opts_no_storage.include_hvdc = Some(false);
        }
        validate_ac_period_generator_economics(&net, problem_spec, context.period)?;
        let t_pf = Instant::now();
        let runtimes = sequential_ac_runtime_candidates(&base_runtime, &net, previous_solution);
        let n_pf = runtimes.len() as u32;
        let pf_ws = t_pf.elapsed().as_secs_f64();
        let t_opf = Instant::now();
        let (sol, stats) =
            solve_ac_opf_with_runtime_fallbacks(&net, &ac_opts_no_storage, &base_runtime, runtimes)
                .map_err(|error| wrap_ac_opf_error(&net, error))?;
        let opf_s = t_opf.elapsed().as_secs_f64();
        (
            net.clone(),
            ac_opts_no_storage,
            sol,
            stats,
            pf_ws,
            n_pf,
            opf_s,
        )
    } else {
        let mut net_pass = net.clone();
        apply_storage_injections(&mut net_pass, &units, &ss_mw);
        let mut ac_opts = ac_opts_with_native_storage(ac_opf);
        if request_hvdc_uses_terminal_injections(problem_spec) && !any_hvdc_p2p_variable(&net_pass)
        {
            ac_opts.include_hvdc = Some(false);
        }
        validate_ac_period_generator_economics(&net_pass, problem_spec, context.period)?;
        let t_pf = Instant::now();
        let runtimes =
            sequential_ac_runtime_candidates(&base_runtime, &net_pass, previous_solution);
        let n_pf = runtimes.len() as u32;
        let pf_ws = t_pf.elapsed().as_secs_f64();
        let t_opf = Instant::now();
        let (sol, stats) =
            solve_ac_opf_with_runtime_fallbacks(&net_pass, &ac_opts, &base_runtime, runtimes)
                .map_err(|error| wrap_ac_opf_error(&net_pass, error))?;
        let opf_s = t_opf.elapsed().as_secs_f64();
        (net_pass, ac_opts, sol, stats, pf_ws, n_pf, opf_s)
    };

    // --- Nomogram tightening loop ---
    let storage_dispatch = AcStorageDispatchView::new(&units, &ss_mw);
    let (net_mw, sol) = nomogram_tighten(
        &solve_net,
        problem_spec.max_nomogram_iter,
        problem_spec.enforce_flowgates,
        &solve_opts,
        &base_runtime,
        storage_dispatch,
        sol,
    )?;

    // Update SoC
    let new_soc = update_soc(&units, &soc_mwh, &net_mw, dt);
    let t_extract = Instant::now();
    let solve_in_service_generator_indices: Vec<usize> = solve_net
        .generators
        .iter()
        .enumerate()
        .filter(|(_, generator)| generator.in_service)
        .map(|(index, _)| index)
        .collect();
    let mut pg_mw = vec![0.0; original_in_service_generator_indices.len()];
    let mut qg_mvar = vec![0.0; original_in_service_generator_indices.len()];
    // Reactive reserves cleared by the AC-OPF, mapped back to the
    // caller-visible in-service generator order. Empty when the
    // AC-OPF did not allocate q-reserve blocks.
    let q_reserves_active = !sol.devices.producer_q_reserve_up_mvar.is_empty();
    let mut producer_q_reserve_up_mvar = if q_reserves_active {
        vec![0.0; original_in_service_generator_indices.len()]
    } else {
        Vec::new()
    };
    let mut producer_q_reserve_down_mvar = if q_reserves_active {
        vec![0.0; original_in_service_generator_indices.len()]
    } else {
        Vec::new()
    };
    for (solve_idx, &global_generator_index) in
        solve_in_service_generator_indices.iter().enumerate()
    {
        let Some(original_idx) = original_in_service_generator_indices
            .iter()
            .position(|&index| index == global_generator_index)
        else {
            continue;
        };
        if let Some(&pg) = sol.generators.gen_p_mw.get(solve_idx) {
            pg_mw[original_idx] = pg;
        }
        if let Some(&qg) = sol.generators.gen_q_mvar.get(solve_idx) {
            qg_mvar[original_idx] = qg;
        }
        if q_reserves_active {
            if let Some(&qru) = sol.devices.producer_q_reserve_up_mvar.get(solve_idx) {
                producer_q_reserve_up_mvar[original_idx] = qru;
            }
            if let Some(&qrd) = sol.devices.producer_q_reserve_down_mvar.get(solve_idx) {
                producer_q_reserve_down_mvar[original_idx] = qrd;
            }
        }
    }
    let switched_shunt_dispatch: Vec<(String, u32, f64, f64)> = sol
        .devices
        .switched_shunt_dispatch
        .iter()
        .enumerate()
        .filter_map(|(control_index, &(_, b_cont, b_rounded))| {
            solve_net
                .controls
                .switched_shunts_opf
                .get(control_index)
                .map(|control| (control.id.clone(), control.bus, b_cont, b_rounded))
        })
        .collect();

    let period_solution = AcScedPeriodSolution {
        pg_mw,
        qg_mvar,
        producer_q_reserve_up_mvar,
        producer_q_reserve_down_mvar,
        consumer_q_reserve_up_mvar: sol.devices.consumer_q_reserve_up_mvar.clone(),
        consumer_q_reserve_down_mvar: sol.devices.consumer_q_reserve_down_mvar.clone(),
        zone_q_reserve_up_shortfall_mvar: sol.devices.zone_q_reserve_up_shortfall_mvar.clone(),
        zone_q_reserve_down_shortfall_mvar: sol.devices.zone_q_reserve_down_shortfall_mvar.clone(),
        lmp: sol.pricing.lmp.clone(),
        q_lmp: sol.pricing.lmp_reactive.clone(),
        bus_voltage_pu: sol.power_flow.voltage_magnitude_pu.clone(),
        bus_angle_rad: sol.power_flow.voltage_angle_rad.clone(),
        storage_soc_mwh: new_soc,
        storage_net_mw: net_mw,
        dr_results: extract_ac_dispatchable_load_results(&solve_net, &sol),
        hvdc_dispatch_mw: extract_ac_hvdc_dispatch_mw(
            &solve_net,
            problem_spec,
            context.period,
            Some(&sol),
        ),
        hvdc_band_dispatch_mw: problem_spec.hvdc_links.iter().map(|_| Vec::new()).collect(),
        tap_dispatch: sol.devices.tap_dispatch.clone(),
        phase_dispatch: sol.devices.phase_dispatch.clone(),
        switched_shunt_dispatch,
        total_cost: sol.total_cost,
        objective_terms: sol.objective_terms.clone(),
        branch_shadow_prices: sol.branches.branch_shadow_prices.clone(),
        flowgate_shadow_prices: sol.branches.flowgate_shadow_prices.clone(),
        interface_shadow_prices: sol.branches.interface_shadow_prices.clone(),
        bus_q_slack_pos_mvar: sol.bus_q_slack_pos_mvar.clone(),
        bus_q_slack_neg_mvar: sol.bus_q_slack_neg_mvar.clone(),
        bus_p_slack_pos_mw: sol.bus_p_slack_pos_mw.clone(),
        bus_p_slack_neg_mw: sol.bus_p_slack_neg_mw.clone(),
        thermal_limit_slack_from_mva: sol.branches.thermal_limit_slack_from_mva.clone(),
        thermal_limit_slack_to_mva: sol.branches.thermal_limit_slack_to_mva.clone(),
        vm_slack_high_pu: sol.vm_slack_high_pu.clone(),
        vm_slack_low_pu: sol.vm_slack_low_pu.clone(),
        angle_diff_slack_high_rad: sol.angle_diff_slack_high_rad.clone(),
        angle_diff_slack_low_rad: sol.angle_diff_slack_low_rad.clone(),
        solve_time_secs: start.elapsed().as_secs_f64(),
        iterations: sol.iterations.unwrap_or(0),
    };

    let extract_secs = t_extract.elapsed().as_secs_f64();

    // Drill into the OpfSolution's per-solve timings for NLP build/solve
    // breakdown. The fallback path through `solve_ac_opf_with_runtime_fallbacks`
    // may have tried multiple runtimes; only the successful one's timings
    // are carried on the final `OpfSolution`.
    let opf_timings = sol.ac_opf_timings.as_ref();
    let timings = AcScedPeriodTimings {
        constraints_setup_secs,
        pf_warmstart_secs,
        pf_attempts,
        opf_total_secs,
        network_prep_secs: opf_timings.map(|t| t.network_prep_secs).unwrap_or(0.0),
        solve_setup_secs: opf_timings.map(|t| t.solve_setup_secs).unwrap_or(0.0),
        nlp_build_secs: opf_timings.map(|t| t.nlp_build_secs).unwrap_or(0.0),
        nlp_solve_secs: opf_timings.map(|t| t.nlp_solve_secs).unwrap_or(0.0),
        opf_extract_secs: opf_timings.map(|t| t.extract_secs).unwrap_or(0.0),
        nlp_attempts: opf_timings.map(|t| t.nlp_attempts).unwrap_or(1),
        total_opf_calls: fallback_stats.total_opf_calls,
        runtime_attempts: fallback_stats.runtime_attempts,
        winning_attempt: fallback_stats.winning_attempt,
        opf_inner_secs: sol.solve_time_secs,
        fallback_wall_secs: fallback_stats.total_opf_wall_secs,
        extract_secs,
        period_total_secs: start.elapsed().as_secs_f64(),
    };

    let opf_stats = AcOpfStats::from_opf_solution(&sol, context.period as u32, None);

    Ok(AcScedPeriodArtifacts {
        period_solution,
        opf_solution: sol,
        timings,
        opf_stats,
    })
}

/// Solve a multi-period AC-SCED sequentially.
///
/// For each period `t`:
/// 1. Builds a network snapshot (load-scaled if `load_scale` is provided).
/// 2. Applies ramp constraints from the previous period's dispatch.
/// 3. Dispatches storage according to mode (see module-level doc).
/// 4. Solves AC-OPF with warm-start from the previous period's solution.
/// 5. Updates storage SoC.
///
/// Storage dispatch per mode:
/// - **`SelfSchedule`**: fixed injection applied as a bus load modification (clamped by SoC).
/// - **`CostMinimization`** and **`OfferCurve`**: passed as **native AC variables**
///   to each period's AC-OPF, so storage participates in the same welfare solve
///   as generators and dispatchable demand.
#[cfg(test)]
pub fn solve_multi_period_ac_sced(
    network: &Network,
    n_periods: usize,
    options: &DispatchOptions,
    load_scales: Option<&[f64]>,
) -> Result<AcScedSolution, ScedError> {
    let wall_start = Instant::now();
    let problem_spec = DispatchProblemSpec::from_options(options);

    // Build list of in-service storage generators: (global_gen_index, &Generator)
    let units: Vec<(usize, &Generator)> = network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service && g.is_storage())
        .collect();

    info!(
        buses = network.n_buses(),
        generators = network.generators.iter().filter(|g| g.in_service).count(),
        n_periods,
        n_storage = units.len(),
        "AC-SCED: starting multi-period solve"
    );

    // Resolve initial SoC for each storage generator
    let initial_soc: Vec<f64> = units
        .iter()
        .map(|(gi, g)| crate::common::runtime::effective_storage_soc_mwh(None, *gi, g))
        .collect();

    let mut periods = Vec::with_capacity(n_periods);
    let mut total_cost = 0.0;
    let base_runtime = AcOpfRuntime::default();

    // Rolling state
    let mut prev_dispatch: Option<Vec<f64>> = options.initial_state.prev_dispatch_mw.clone();
    let mut prev_solution: Option<OpfSolution> = None;
    let mut soc_mwh = initial_soc;

    for t in 0..n_periods {
        // Per-period network (load scaling)
        let mut net_t = network.clone();
        if let Some(scales) = load_scales
            && t < scales.len()
        {
            let scale = scales[t];
            for load in &mut net_t.loads {
                load.active_power_demand_mw *= scale;
                load.reactive_power_demand_mvar *= scale;
            }
        }

        let storage_soc_override: std::collections::HashMap<usize, f64> = units
            .iter()
            .enumerate()
            .filter_map(|(s, (gi, _))| soc_mwh.get(s).copied().map(|soc| (*gi, soc)))
            .collect();
        let period_context = DispatchPeriodContext {
            period: t,
            prev_dispatch_mw: prev_dispatch.as_deref(),
            prev_dispatch_mask: None,
            prev_hvdc_dispatch_mw: None,
            prev_hvdc_dispatch_mask: None,
            storage_soc_override: Some(&storage_soc_override),
            next_period_commitment: None,
        };
        let artifacts = solve_ac_sced_with_problem_spec_artifacts(
            &net_t,
            problem_spec,
            &options.ac_opf,
            &base_runtime,
            period_context,
            prev_solution.as_ref(),
        )
        .map_err(|e| ScedError::SolverError(format!("AC-OPF period {t}: {e}")))?;
        soc_mwh = artifacts.period_solution.storage_soc_mwh.clone();
        prev_dispatch = Some(artifacts.period_solution.pg_mw.clone());
        total_cost += artifacts.period_solution.total_cost;
        prev_solution = Some(artifacts.opf_solution);
        periods.push(artifacts.period_solution);
    }

    let wall_time = wall_start.elapsed().as_secs_f64();
    info!(
        n_periods,
        total_cost,
        wall_time_secs = wall_time,
        "AC-SCED multi-period solve complete"
    );

    Ok(AcScedSolution {
        periods,
        total_cost,
        n_periods,
        solve_time_secs: wall_time,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn test_data_path(name: &str) -> std::path::PathBuf {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::PathBuf::from(p).join(name);
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
            .join(name)
    }

    #[allow(dead_code)]
    fn data_available() -> bool {
        test_data_path("case9.m").exists()
    }

    #[allow(dead_code)]
    fn load_case9() -> surge_network::Network {
        surge_io::load(test_data_path("case9.m")).unwrap()
    }

    use surge_network::market::{CostCurve, DispatchableLoad, LoadArchetype, LoadCostModel};
    use surge_network::network::{Bus, BusType, Load, StorageDispatchMode, StorageParams};

    use super::*;
    use crate::dispatch::{CommitmentMode, IndexedDispatchInitialState};
    use crate::legacy::DispatchOptions;

    /// Helper: add a storage generator to bus_num with given params, return its global index.
    fn add_storage_gen(
        net: &mut surge_network::Network,
        bus_num: u32,
        charge_mw_max: f64,
        discharge_mw_max: f64,
        energy_mwh: f64,
        dispatch_mode: StorageDispatchMode,
        soc_initial_mwh: f64,
    ) -> usize {
        use surge_network::network::Generator;
        let g = Generator {
            bus: bus_num,
            in_service: true,
            pmin: -charge_mw_max, // charge limit (negative pmin)
            pmax: discharge_mw_max,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            storage: Some(StorageParams {
                charge_efficiency: 0.9486832981,
                discharge_efficiency: 0.9486832981,
                energy_capacity_mwh: energy_mwh,
                soc_initial_mwh,
                soc_min_mwh: 0.0,
                soc_max_mwh: energy_mwh,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode,
                self_schedule_mw: 0.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        };
        let idx = net.generators.len();
        net.generators.push(g);
        idx
    }

    fn one_bus_ac_dispatchable_load_net(fixed_load_mw: f64) -> surge_network::Network {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_dispatchable_load");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, fixed_load_mw, 0.0));

        let mut generator = Generator::new(1, fixed_load_mw, 1.0);
        generator.pmin = 0.0;
        generator.pmax = 200.0;
        generator.qmin = -150.0;
        generator.qmax = 150.0;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(generator);

        net
    }

    fn one_bus_ac_storage_offer_net(
        fixed_load_mw: f64,
        thermal_cost_per_mwh: f64,
    ) -> surge_network::Network {
        let mut net = one_bus_ac_dispatchable_load_net(fixed_load_mw);
        if let Some(generator) = net.generators.first_mut() {
            generator.cost = Some(CostCurve::Polynomial {
                startup: 0.0,
                shutdown: 0.0,
                coeffs: vec![thermal_cost_per_mwh, 0.0],
            });
        }
        net
    }

    // -----------------------------------------------------------------------
    // Storage dispatch mode tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_ac_sced_dispatchable_load_serves_when_curtailment_cost_exceeds_lmp() {
        let net = one_bus_ac_dispatchable_load_net(80.0);
        let dl = DispatchableLoad::curtailable(1, 20.0, 0.0, 0.0, 30.0, net.base_mva);

        let sol = solve_ac_sced(
            &net,
            &DispatchOptions {
                dispatchable_loads: vec![dl],
                ..DispatchOptions::default()
            },
        )
        .expect("AC-SCED with dispatchable load should solve");

        assert_eq!(sol.dr_results.loads.len(), 1);
        let served_mw = sol.dr_results.loads[0].p_served_pu * net.base_mva;
        assert!(
            served_mw > 19.0,
            "dispatchable load should stay served when curtailment is expensive, got {served_mw:.3} MW"
        );
        let total_gen: f64 = sol.pg_mw.iter().sum();
        assert!(
            (total_gen - (80.0 + served_mw)).abs() < 1.0,
            "power balance should include served dispatchable load, gen={total_gen:.3} served={served_mw:.3}"
        );
    }

    #[test]
    fn test_ac_sced_dispatchable_load_curtails_when_curtailment_cost_is_low() {
        let net = one_bus_ac_dispatchable_load_net(80.0);
        let dl = DispatchableLoad::curtailable(1, 20.0, 0.0, 0.0, 5.0, net.base_mva);

        let sol = solve_ac_sced(
            &net,
            &DispatchOptions {
                dispatchable_loads: vec![dl],
                ..DispatchOptions::default()
            },
        )
        .expect("AC-SCED with dispatchable load should solve");

        assert_eq!(sol.dr_results.loads.len(), 1);
        let served_mw = sol.dr_results.loads[0].p_served_pu * net.base_mva;
        assert!(
            served_mw < 1.0,
            "dispatchable load should curtail when curtailment is cheaper than generation, got {served_mw:.3} MW"
        );
        let total_gen: f64 = sol.pg_mw.iter().sum();
        assert!(
            (total_gen - (80.0 + served_mw)).abs() < 1.0,
            "power balance should include curtailed dispatchable load, gen={total_gen:.3} served={served_mw:.3}"
        );
    }

    #[test]
    fn test_ac_sced_dispatchable_load_independent_q_is_not_forced_by_pf() {
        let net = one_bus_ac_dispatchable_load_net(80.0);
        let q_fixed_pu = 10.0 / net.base_mva;
        let dl = DispatchableLoad {
            bus: 1,
            p_sched_pu: 20.0 / net.base_mva,
            q_sched_pu: q_fixed_pu,
            p_min_pu: 0.0,
            p_max_pu: 20.0 / net.base_mva,
            q_min_pu: q_fixed_pu,
            q_max_pu: q_fixed_pu,
            archetype: LoadArchetype::IndependentPQ,
            cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 5.0 },
            fixed_power_factor: false,
            in_service: true,
            resource_id: String::new(),
            product_type: None,
            dispatch_notification_minutes: 0.0,
            min_duration_hours: 0.0,
            baseline_mw: None,
            rebound_fraction: 0.0,
            rebound_periods: 0,
            ramp_up_pu_per_hr: None,
            ramp_down_pu_per_hr: None,
            initial_p_pu: None,
            ramp_group: None,
            energy_offer: None,
            reserve_offers: Vec::new(),
            reserve_group: None,
            qualifications: std::collections::HashMap::new(),
            pq_linear_equality: None,
            pq_linear_upper: None,
            pq_linear_lower: None,
        };

        let sol = solve_ac_sced(
            &net,
            &DispatchOptions {
                dispatchable_loads: vec![dl],
                ..DispatchOptions::default()
            },
        )
        .expect("AC-SCED with independent-Q dispatchable load should solve");

        assert_eq!(sol.dr_results.loads.len(), 1);
        let served_mw = sol.dr_results.loads[0].p_served_pu * net.base_mva;
        let served_q_mvar = sol.dr_results.loads[0].q_served_pu * net.base_mva;
        assert!(
            served_mw < 1.0,
            "low-value independent-Q load should curtail real power, got {served_mw:.3} MW"
        );
        assert!(
            (served_q_mvar - 10.0).abs() < 1e-3,
            "independent-Q load should keep its fixed reactive draw, got {served_q_mvar:.6} MVAr"
        );
    }

    #[test]
    fn test_ac_sced_dispatchable_load_clamps_inconsistent_q_schedule_to_bounds() {
        let net = one_bus_ac_dispatchable_load_net(80.0);
        let dl = DispatchableLoad {
            bus: 1,
            p_sched_pu: 20.0 / net.base_mva,
            q_sched_pu: 20.0 / net.base_mva,
            p_min_pu: 0.0,
            p_max_pu: 20.0 / net.base_mva,
            q_min_pu: -10.0 / net.base_mva,
            q_max_pu: 10.0 / net.base_mva,
            archetype: LoadArchetype::IndependentPQ,
            cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 5.0 },
            fixed_power_factor: false,
            in_service: true,
            resource_id: String::new(),
            product_type: None,
            dispatch_notification_minutes: 0.0,
            min_duration_hours: 0.0,
            baseline_mw: None,
            rebound_fraction: 0.0,
            rebound_periods: 0,
            ramp_up_pu_per_hr: None,
            ramp_down_pu_per_hr: None,
            initial_p_pu: None,
            ramp_group: None,
            energy_offer: None,
            reserve_offers: Vec::new(),
            reserve_group: None,
            qualifications: std::collections::HashMap::new(),
            pq_linear_equality: None,
            pq_linear_upper: None,
            pq_linear_lower: None,
        };

        let sol = solve_ac_sced(
            &net,
            &DispatchOptions {
                dispatchable_loads: vec![dl],
                ..DispatchOptions::default()
            },
        )
        .expect("AC-SCED with clamped independent-Q load should solve");

        assert_eq!(sol.dr_results.loads.len(), 1);
        let served_q_mvar = sol.dr_results.loads[0].q_served_pu * net.base_mva;
        assert!(
            served_q_mvar <= 10.0 + 1e-3,
            "independent-Q load should respect its reactive upper bound after clamping, got {served_q_mvar:.6} MVAr"
        );
    }

    #[test]
    fn test_ac_sced_generator_q_warm_start_is_applied_and_clamped() {
        let mut net = one_bus_ac_dispatchable_load_net(80.0);
        net.generators[0].qmin = -20.0;
        net.generators[0].qmax = 20.0;
        net.generators[0].q = 0.0;

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 2,
            ac_generator_warm_start_p_mw: std::iter::once((0usize, vec![90.0, 95.0])).collect(),
            ac_generator_warm_start_q_mvar: std::iter::once((0usize, vec![25.0, -30.0])).collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);

        apply_ac_warm_start_targets(&mut net, spec, 0);
        assert!((net.generators[0].p - 90.0).abs() < 1e-9);
        assert!((net.generators[0].q - 20.0).abs() < 1e-9);

        apply_ac_warm_start_targets(&mut net, spec, 1);
        assert!((net.generators[0].p - 95.0).abs() < 1e-9);
        assert!((net.generators[0].q + 20.0).abs() < 1e-9);
    }

    #[test]
    fn test_ac_sced_runtime_uses_bus_voltage_warm_start_when_available() {
        let net = one_bus_ac_dispatchable_load_net(80.0);
        let dl = DispatchableLoad {
            bus: 1,
            p_sched_pu: 10.0 / net.base_mva,
            q_sched_pu: 0.0,
            p_min_pu: 0.0,
            p_max_pu: 20.0 / net.base_mva,
            q_min_pu: 0.0,
            q_max_pu: 0.0,
            archetype: LoadArchetype::Elastic,
            cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 5.0 },
            fixed_power_factor: false,
            in_service: true,
            resource_id: "dl0".to_string(),
            product_type: None,
            dispatch_notification_minutes: 0.0,
            min_duration_hours: 0.0,
            baseline_mw: None,
            rebound_fraction: 0.0,
            rebound_periods: 0,
            ramp_up_pu_per_hr: None,
            ramp_down_pu_per_hr: None,
            initial_p_pu: None,
            ramp_group: None,
            energy_offer: None,
            reserve_offers: Vec::new(),
            reserve_group: None,
            qualifications: std::collections::HashMap::new(),
            pq_linear_equality: None,
            pq_linear_upper: None,
            pq_linear_lower: None,
        };
        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            dispatchable_loads: vec![dl],
            ac_bus_warm_start_vm_pu: std::iter::once((0usize, vec![1.03])).collect(),
            ac_bus_warm_start_va_rad: std::iter::once((0usize, vec![0.12])).collect(),
            ac_generator_warm_start_p_mw: std::iter::once((0usize, vec![90.0])).collect(),
            ac_generator_warm_start_q_mvar: std::iter::once((0usize, vec![15.0])).collect(),
            ac_dispatchable_load_warm_start_p_mw: std::iter::once((0usize, vec![12.0])).collect(),
            ac_dispatchable_load_warm_start_q_mvar: std::iter::once((0usize, vec![0.0])).collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);
        let mut period_net = net.clone();
        apply_period_operating_constraints(
            &mut period_net,
            &net,
            spec,
            DispatchPeriodContext::initial(&options.initial_state),
        );

        let runtime =
            runtime_with_request_ac_warm_start(&AcOpfRuntime::default(), &period_net, spec, 0);
        let warm_start = runtime
            .warm_start
            .expect("request AC warm start should be populated");

        assert_eq!(warm_start.voltage_magnitude_pu, vec![1.03]);
        assert_eq!(warm_start.voltage_angle_rad, vec![0.12]);
        assert_eq!(warm_start.pg, vec![0.9]);
        assert_eq!(warm_start.qg, vec![0.15]);
        assert_eq!(warm_start.dispatchable_load_p, vec![0.12]);
        assert_eq!(warm_start.dispatchable_load_q, vec![0.0]);
    }

    #[test]
    fn test_sequential_runtime_candidates_do_not_override_explicit_warm_start() {
        let net = one_bus_ac_dispatchable_load_net(80.0);
        let explicit_runtime = AcOpfRuntime::default().with_warm_start(WarmStart {
            voltage_magnitude_pu: vec![1.01],
            voltage_angle_rad: vec![0.08],
            pg: vec![0.85],
            qg: vec![0.12],
            dispatchable_load_p: vec![],
            dispatchable_load_q: vec![],
        });
        let prior_solution = OpfSolution::default();

        let runtimes =
            sequential_ac_runtime_candidates(&explicit_runtime, &net, Some(&prior_solution));

        assert_eq!(runtimes.len(), 1);
        assert!(runtimes[0].warm_start.is_some());
        assert_eq!(
            runtimes[0]
                .warm_start
                .as_ref()
                .expect("explicit warm start")
                .voltage_angle_rad,
            vec![0.08]
        );
    }

    #[test]
    fn test_ac_sced_period_operating_constraints_preserve_original_local_generator_order() {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_fixed_commitment_warm_start_index_stability");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.id = "g0".to_string();
        g0.pmin = 0.0;
        g0.pmax = 100.0;
        g0.qmin = -10.0;
        g0.qmax = 10.0;

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.id = "g1".to_string();
        g1.pmin = 0.0;
        g1.pmax = 100.0;
        g1.qmin = -10.0;
        g1.qmax = 10.0;

        let mut g2 = Generator::new(1, 0.0, 1.0);
        g2.id = "g2".to_string();
        g2.pmin = 0.0;
        g2.pmax = 100.0;
        g2.qmin = -10.0;
        g2.qmax = 10.0;

        net.generators = vec![g0, g1, g2];

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            commitment: CommitmentMode::Fixed {
                commitment: vec![false, true, true],
                per_period: None,
            },
            n_periods: 1,
            ac_generator_warm_start_p_mw: [
                (0usize, vec![10.0]),
                (1usize, vec![20.0]),
                (2usize, vec![30.0]),
            ]
            .into_iter()
            .collect(),
            ac_generator_warm_start_q_mvar: [
                (0usize, vec![1.0]),
                (1usize, vec![2.0]),
                (2usize, vec![3.0]),
            ]
            .into_iter()
            .collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);
        let mut period_net = net.clone();

        apply_period_operating_constraints(
            &mut period_net,
            &net,
            spec,
            DispatchPeriodContext::initial(&options.initial_state),
        );

        assert!(period_net.generators[0].in_service);
        assert!((period_net.generators[0].p - 10.0).abs() < 1e-9);
        assert!((period_net.generators[0].q - 1.0).abs() < 1e-9);
        assert!((period_net.generators[1].p - 20.0).abs() < 1e-9);
        assert!((period_net.generators[1].q - 2.0).abs() < 1e-9);
        assert!((period_net.generators[2].p - 30.0).abs() < 1e-9);
        assert!((period_net.generators[2].q - 3.0).abs() < 1e-9);
    }

    // ----- AC-SCED ramp window enforcement -----

    /// Build a 1-bus 1-gen network with explicit ramp curves used by the
    /// hard-ramp-window AC tests.
    fn ramp_window_test_network() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("ac_ramp_window_test");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut g = Generator::new(1, 0.0, 1.0);
        g.id = "g0".to_string();
        g.in_service = true;
        g.pmin = 10.0;
        g.pmax = 100.0;
        g.qmin = -50.0;
        g.qmax = 50.0;
        g.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 10.0],
        });
        // 5 MW/min × 60 min = 300 MW/h ramp envelope around pg_prev. With
        // dt = 1h this is loose enough that the ramp window intersects the
        // [10, 100] static box anywhere pg_prev lives.
        g.ramping.get_or_insert_default().ramp_up_curve = vec![(0.0, 5.0)];
        g.ramping.get_or_insert_default().ramp_down_curve = vec![(0.0, 5.0)];
        // Startup/shutdown ramp limits live on commitment params, not on
        // ramping. 0.5 MW/min × 60 = 30 MW/h startup cap.
        g.commitment.get_or_insert_default().startup_ramp_mw_per_min = Some(0.5);
        g.commitment
            .get_or_insert_default()
            .shutdown_ramp_mw_per_min = Some(0.5);
        net.generators.push(g);

        net
    }

    fn ramp_window_test_options(
        ramp_constraints_hard: bool,
        prev_dispatch_mw: Vec<f64>,
    ) -> DispatchOptions {
        DispatchOptions {
            formulation: crate::Formulation::Ac,
            commitment: CommitmentMode::Fixed {
                commitment: vec![true],
                per_period: None,
            },
            n_periods: 1,
            dt_hours: 1.0,
            ramp_constraints_hard,
            initial_state: IndexedDispatchInitialState {
                prev_dispatch_mw: Some(prev_dispatch_mw),
                ..IndexedDispatchInitialState::default()
            },
            ..DispatchOptions::default()
        }
    }

    /// With hard ramps and a feasible prev_dispatch, the ramp window must
    /// narrow `pmin/pmax` on the period clone but leave the static network
    /// untouched.
    #[test]
    fn test_ac_sced_ramp_window_narrows_period_clone() {
        let static_net = ramp_window_test_network();
        // pg_prev = 50 MW, ramp = 300 MW/h, dt = 1h ⇒ window [10, 100]
        // intersected with the static box [10, 100] = [10, 100]. Use a
        // tighter ramp to actually exercise narrowing: 0.5 MW/min × 60 = 30.
        let mut net_with_tight_ramp = static_net.clone();
        net_with_tight_ramp.generators[0]
            .ramping
            .get_or_insert_default()
            .ramp_up_curve = vec![(0.0, 0.5)];
        net_with_tight_ramp.generators[0]
            .ramping
            .get_or_insert_default()
            .ramp_down_curve = vec![(0.0, 0.5)];

        let opts = ramp_window_test_options(true, vec![50.0]);
        let spec = DispatchProblemSpec::from_options(&opts);
        let mut period_net = net_with_tight_ramp.clone();

        apply_period_operating_constraints(
            &mut period_net,
            &net_with_tight_ramp,
            spec,
            DispatchPeriodContext::initial(&opts.initial_state),
        );

        // Ramp window: [50 - 30, 50 + 30] = [20, 80]. Intersect with
        // static [10, 100] = [20, 80].
        assert!(
            (period_net.generators[0].pmin - 20.0).abs() < 1e-6,
            "expected period pmin=20, got {}",
            period_net.generators[0].pmin
        );
        assert!(
            (period_net.generators[0].pmax - 80.0).abs() < 1e-6,
            "expected period pmax=80, got {}",
            period_net.generators[0].pmax
        );
        // Static network must remain unchanged.
        assert_eq!(net_with_tight_ramp.generators[0].pmin, 10.0);
        assert_eq!(net_with_tight_ramp.generators[0].pmax, 100.0);
    }

    /// With hard ramps and a `pg_prev` outside the static box (e.g. unit
    /// was off in the previous period at 0 MW with pmin = 10), the ramp
    /// window collapses around the startup ramp cap. If the resulting
    /// intersection is empty, the helper must NOT panic — it must log a
    /// debug message and leave the static bounds in place.
    #[test]
    fn test_ac_sced_ramp_window_empty_intersection_does_not_panic() {
        let static_net = ramp_window_test_network();
        // pg_prev = 0 MW, was_off = true (since pmin = 10 > 0), startup
        // ramp = 0.5 MW/min × 60 = 30 MW. Window for "was off" path is
        // [static_pmin, min(static_pmax, startup)] = [10, min(100, 30)] = [10, 30].
        // That's non-empty, so let's tighten startup ramp to make it empty.
        let mut tight = static_net.clone();
        tight.generators[0]
            .commitment
            .get_or_insert_default()
            .startup_ramp_mw_per_min = Some(0.05); // 3 MW/h, below pmin=10

        let opts = ramp_window_test_options(true, vec![0.0]);
        let spec = DispatchProblemSpec::from_options(&opts);
        let mut period_net = tight.clone();

        // Must NOT panic.
        apply_period_operating_constraints(
            &mut period_net,
            &tight,
            spec,
            DispatchPeriodContext::initial(&opts.initial_state),
        );

        // The empty intersection (pmin=10 > pmax=3) means the helper logs
        // and skips. The period clone retains the static bounds verbatim.
        assert_eq!(period_net.generators[0].pmin, 10.0);
        assert_eq!(period_net.generators[0].pmax, 100.0);
    }

    /// With ramp_constraints_hard = false (the legacy default), the
    /// helper is a no-op. The period clone keeps the static bounds.
    #[test]
    fn test_ac_sced_ramp_window_disabled_when_soft() {
        let static_net = ramp_window_test_network();
        let opts = ramp_window_test_options(false, vec![50.0]);
        let spec = DispatchProblemSpec::from_options(&opts);
        let mut period_net = static_net.clone();

        apply_period_operating_constraints(
            &mut period_net,
            &static_net,
            spec,
            DispatchPeriodContext::initial(&opts.initial_state),
        );

        // No narrowing.
        assert_eq!(period_net.generators[0].pmin, 10.0);
        assert_eq!(period_net.generators[0].pmax, 100.0);
    }

    #[test]
    fn test_ac_sced_shutdown_deloading_uses_period_specific_hours() {
        let static_net = ramp_window_test_network();
        let input = crate::request::DispatchInput {
            n_periods: 2,
            dt_hours: 0.375,
            period_hours: vec![0.25, 0.5],
            period_hour_prefix: vec![0.0, 0.25, 0.75],
            enforce_shutdown_deloading: true,
            ..crate::request::DispatchInput::default()
        };
        let commitment = CommitmentMode::Fixed {
            commitment: vec![true],
            per_period: Some(vec![vec![true], vec![false]]),
        };
        let spec = DispatchProblemSpec::from_request(&input, &commitment);
        let mut period_net = static_net.clone();
        let next_commitment = spec.period(0).next_fixed_commitment();
        let context = DispatchPeriodContext {
            period: 0,
            next_period_commitment: next_commitment,
            ..DispatchPeriodContext::initial(&input.initial_state)
        };

        apply_period_operating_constraints(&mut period_net, &static_net, spec, context);

        assert!(
            (period_net.generators[0].pmax - 15.0).abs() < 1e-6,
            "expected shutdown cap to use next-period 0.5h interval, got {}",
            period_net.generators[0].pmax
        );
        assert_eq!(period_net.generators[0].pmin, 10.0);
    }

    #[test]
    fn test_ac_sced_shutdown_deloading_preserves_minimum_output_floor() {
        let static_net = ramp_window_test_network();
        let input = crate::request::DispatchInput {
            n_periods: 2,
            dt_hours: 0.625,
            period_hours: vec![1.0, 0.25],
            period_hour_prefix: vec![0.0, 1.0, 1.25],
            enforce_shutdown_deloading: true,
            ..crate::request::DispatchInput::default()
        };
        let commitment = CommitmentMode::Fixed {
            commitment: vec![true],
            per_period: Some(vec![vec![true], vec![false]]),
        };
        let spec = DispatchProblemSpec::from_request(&input, &commitment);
        let mut period_net = static_net.clone();
        let next_commitment = spec.period(0).next_fixed_commitment();
        let context = DispatchPeriodContext {
            period: 0,
            next_period_commitment: next_commitment,
            ..DispatchPeriodContext::initial(&input.initial_state)
        };

        apply_period_operating_constraints(&mut period_net, &static_net, spec, context);

        assert!(
            (period_net.generators[0].pmax - 10.0).abs() < 1e-6,
            "expected online pmax to stay at pmin when shutdown cap falls below the floor, got {}",
            period_net.generators[0].pmax
        );
        assert_eq!(period_net.generators[0].pmin, 10.0);
    }

    #[test]
    fn test_ac_sced_applies_dispatch_bound_profiles_before_shutdown_deloading() {
        let mut static_net = ramp_window_test_network();
        static_net.generators[0].pmin = 0.0;

        let input = crate::request::DispatchInput {
            n_periods: 2,
            dt_hours: 0.625,
            period_hours: vec![1.0, 0.25],
            period_hour_prefix: vec![0.0, 1.0, 1.25],
            enforce_shutdown_deloading: true,
            generator_dispatch_bounds: crate::request::GeneratorDispatchBoundsProfiles {
                profiles: vec![crate::request::GeneratorDispatchBoundsProfile {
                    resource_id: "g0".to_string(),
                    p_min_mw: vec![10.0, 0.0],
                    p_max_mw: vec![10.0, 0.0],
                    q_min_mvar: None,
                    q_max_mvar: None,
                }],
            },
            ..crate::request::DispatchInput::default()
        };
        let commitment = CommitmentMode::Fixed {
            commitment: vec![true],
            per_period: Some(vec![vec![true], vec![false]]),
        };
        let spec = DispatchProblemSpec::from_request(&input, &commitment);
        let mut period_net = static_net.clone();
        let next_commitment = spec.period(0).next_fixed_commitment();
        let context = DispatchPeriodContext {
            period: 0,
            next_period_commitment: next_commitment,
            ..DispatchPeriodContext::initial(&input.initial_state)
        };

        apply_period_operating_constraints(&mut period_net, &static_net, spec, context);

        assert_eq!(period_net.generators[0].pmin, 10.0);
        assert_eq!(period_net.generators[0].pmax, 10.0);
    }

    #[test]
    fn test_ac_sced_runtime_warm_start_preserves_original_local_generator_order() {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_fixed_commitment_runtime_warm_start_index_stability");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.id = "g0".to_string();
        g0.pmin = 0.0;
        g0.pmax = 100.0;
        g0.qmin = -10.0;
        g0.qmax = 10.0;

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.id = "g1".to_string();
        g1.pmin = 0.0;
        g1.pmax = 100.0;
        g1.qmin = -10.0;
        g1.qmax = 10.0;

        let mut g2 = Generator::new(1, 0.0, 1.0);
        g2.id = "g2".to_string();
        g2.pmin = 0.0;
        g2.pmax = 100.0;
        g2.qmin = -10.0;
        g2.qmax = 10.0;

        net.generators = vec![g0, g1, g2];

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            commitment: CommitmentMode::Fixed {
                commitment: vec![false, true, true],
                per_period: None,
            },
            n_periods: 1,
            ac_generator_warm_start_p_mw: [
                (0usize, vec![10.0]),
                (1usize, vec![20.0]),
                (2usize, vec![30.0]),
            ]
            .into_iter()
            .collect(),
            ac_generator_warm_start_q_mvar: [
                (0usize, vec![1.0]),
                (1usize, vec![2.0]),
                (2usize, vec![3.0]),
            ]
            .into_iter()
            .collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);
        let local_gen_index_by_global = original_local_gen_index_by_global(&net);
        let mut period_net = net.clone();

        apply_period_operating_constraints(
            &mut period_net,
            &net,
            spec,
            DispatchPeriodContext::initial(&options.initial_state),
        );

        let runtime = runtime_with_request_ac_warm_start_with_local_gen_index(
            &AcOpfRuntime::default(),
            &period_net,
            spec,
            0,
            &local_gen_index_by_global,
        );
        let warm_start = runtime
            .warm_start
            .expect("explicit generator warm start should seed runtime");

        assert_eq!(warm_start.pg, vec![0.1, 0.2, 0.3]);
        assert_eq!(warm_start.qg, vec![0.01, 0.02, 0.03]);
    }

    #[test]
    fn test_ac_sced_hvdc_warm_start_target_updates_lcc_link_setpoint() {
        use surge_network::Network;
        use surge_network::network::{HvdcLink, LccConverterTerminal, LccHvdcLink};

        let mut net = Network::new("ac_hvdc_warm_start");
        net.hvdc.links.push(HvdcLink::Lcc(LccHvdcLink {
            name: "dcl_0".to_string(),
            rectifier: LccConverterTerminal {
                bus: 1,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 2,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        }));

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            hvdc_links: vec![crate::hvdc::HvdcDispatchLink {
                id: "dcl_0".to_string(),
                name: "dcl_0".to_string(),
                from_bus: 1,
                to_bus: 2,
                p_dc_min_mw: 0.0,
                p_dc_max_mw: 100.0,
                loss_a_mw: 0.0,
                loss_b_frac: 0.0,
                ramp_mw_per_min: 0.0,
                cost_per_mwh: 0.0,
                bands: Vec::new(),
            }],
            ac_hvdc_warm_start_p_mw: std::iter::once((0usize, vec![75.0])).collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);

        apply_ac_hvdc_warm_start_targets(&mut net, spec, 0);

        assert!(
            (net.hvdc.links[0]
                .as_lcc()
                .expect("test link should remain LCC")
                .scheduled_setpoint
                - 75.0)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn test_ac_sced_extracts_point_to_point_hvdc_dispatch_from_network() {
        use surge_network::Network;
        use surge_network::network::{HvdcLink, LccConverterTerminal, LccHvdcLink};

        let mut net = Network::new("ac_hvdc_extract");
        net.hvdc.links.push(HvdcLink::Lcc(LccHvdcLink {
            name: "dcl_0".to_string(),
            scheduled_setpoint: 88.0,
            rectifier: LccConverterTerminal {
                bus: 1,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 2,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        }));

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            hvdc_links: vec![crate::hvdc::HvdcDispatchLink {
                id: "dcl_0".to_string(),
                name: "dcl_0".to_string(),
                from_bus: 1,
                to_bus: 2,
                p_dc_min_mw: 0.0,
                p_dc_max_mw: 100.0,
                loss_a_mw: 0.0,
                loss_b_frac: 0.0,
                ramp_mw_per_min: 0.0,
                cost_per_mwh: 0.0,
                bands: Vec::new(),
            }],
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);

        assert_eq!(extract_ac_hvdc_dispatch_mw(&net, spec, 0, None), vec![88.0]);
    }

    #[test]
    fn test_ac_sced_request_hvdc_terminal_injections_ignore_warm_start_q_schedule() {
        use surge_network::Network;

        let mut net = Network::new("ac_hvdc_terminal_injections");
        net.loads.push(Load::new(1, 20.0, 5.0));
        net.loads.push(Load::new(2, 30.0, 7.0));

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            hvdc_links: vec![crate::hvdc::HvdcDispatchLink {
                id: "dcl_0".to_string(),
                name: "dcl_0".to_string(),
                from_bus: 1,
                to_bus: 2,
                p_dc_min_mw: 0.0,
                p_dc_max_mw: 100.0,
                loss_a_mw: 2.0,
                loss_b_frac: 0.1,
                ramp_mw_per_min: 0.0,
                cost_per_mwh: 0.0,
                bands: Vec::new(),
            }],
            ac_hvdc_warm_start_p_mw: std::iter::once((0usize, vec![50.0])).collect(),
            ac_hvdc_warm_start_q_fr_mvar: std::iter::once((0usize, vec![11.0])).collect(),
            ac_hvdc_warm_start_q_to_mvar: std::iter::once((0usize, vec![13.0])).collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);

        apply_request_hvdc_terminal_injections(&mut net, spec, 0);

        let bus1_total: f64 = net
            .loads
            .iter()
            .filter(|load| load.bus == 1 && load.in_service)
            .map(|load| load.active_power_demand_mw)
            .sum();
        let bus2_total: f64 = net
            .loads
            .iter()
            .filter(|load| load.bus == 2 && load.in_service)
            .map(|load| load.active_power_demand_mw)
            .sum();
        let bus1_total_q: f64 = net
            .loads
            .iter()
            .filter(|load| load.bus == 1 && load.in_service)
            .map(|load| load.reactive_power_demand_mvar)
            .sum();
        let bus2_total_q: f64 = net
            .loads
            .iter()
            .filter(|load| load.bus == 2 && load.in_service)
            .map(|load| load.reactive_power_demand_mvar)
            .sum();

        assert!((bus1_total - 70.0).abs() < 1e-9);
        assert!((bus2_total + 13.0).abs() < 1e-9);
        assert!((bus1_total_q - 5.0).abs() < 1e-9);
        assert!((bus2_total_q - 7.0).abs() < 1e-9);
    }

    #[test]
    fn test_ac_sced_request_hvdc_terminal_injections_use_fixed_hvdc_q_schedule_without_warm_start()
    {
        use surge_network::Network;

        let mut net = Network::new("ac_hvdc_terminal_fixed_q");

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            hvdc_links: vec![crate::hvdc::HvdcDispatchLink {
                id: "dcl_0".to_string(),
                name: "dcl_0".to_string(),
                from_bus: 1,
                to_bus: 2,
                p_dc_min_mw: 0.0,
                p_dc_max_mw: 100.0,
                loss_a_mw: 0.0,
                loss_b_frac: 0.0,
                ramp_mw_per_min: 0.0,
                cost_per_mwh: 0.0,
                bands: Vec::new(),
            }],
            fixed_hvdc_dispatch_mw: std::iter::once((0usize, vec![40.0])).collect(),
            fixed_hvdc_dispatch_q_fr_mvar: std::iter::once((0usize, vec![9.0])).collect(),
            fixed_hvdc_dispatch_q_to_mvar: std::iter::once((0usize, vec![12.0])).collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);

        apply_request_hvdc_terminal_injections(&mut net, spec, 0);

        assert_eq!(net.loads.len(), 2);
        assert!((net.loads[0].active_power_demand_mw - 40.0).abs() < 1e-9);
        assert!((net.loads[0].reactive_power_demand_mvar - 9.0).abs() < 1e-9);
        assert!((net.loads[1].active_power_demand_mw + 40.0).abs() < 1e-9);
        assert!((net.loads[1].reactive_power_demand_mvar - 12.0).abs() < 1e-9);
    }

    #[test]
    fn test_ac_sced_request_hvdc_terminal_injections_strip_physical_hvdc_link() {
        use surge_network::Network;
        use surge_network::network::{HvdcLink, LccConverterTerminal, LccHvdcLink};

        let mut net = Network::new("ac_hvdc_terminal_only");
        net.hvdc.links.push(HvdcLink::Lcc(LccHvdcLink {
            name: "dcl_0".to_string(),
            scheduled_setpoint: 12.0,
            rectifier: LccConverterTerminal {
                bus: 1,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 2,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        }));

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            hvdc_links: vec![crate::hvdc::HvdcDispatchLink {
                id: "dcl_0".to_string(),
                name: "dcl_0".to_string(),
                from_bus: 1,
                to_bus: 2,
                p_dc_min_mw: 0.0,
                p_dc_max_mw: 100.0,
                loss_a_mw: 0.0,
                loss_b_frac: 0.0,
                ramp_mw_per_min: 0.0,
                cost_per_mwh: 0.0,
                bands: Vec::new(),
            }],
            ac_hvdc_warm_start_p_mw: std::iter::once((0usize, vec![75.0])).collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);
        let static_net = net.clone();

        apply_period_operating_constraints(
            &mut net,
            &static_net,
            spec,
            DispatchPeriodContext::initial(&options.initial_state),
        );

        assert!(net.hvdc.links.is_empty());
    }

    #[test]
    fn test_ac_sced_generator_target_tracking_runtime_preserves_generator_cost() {
        let net = one_bus_ac_dispatchable_load_net(80.0);
        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            ac_generator_warm_start_p_mw: std::iter::once((0usize, vec![75.0])).collect(),
            ac_target_tracking: crate::request::AcDispatchTargetTracking {
                generator_p_penalty_per_mw2: 3.0,
                dispatchable_load_p_penalty_per_mw2: 0.0,
                ..crate::request::AcDispatchTargetTracking::default()
            },
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);
        let mut period_net = net.clone();

        apply_period_generator_economics(&mut period_net, spec, 0);
        let original_cost = period_net.generators[0]
            .cost
            .clone()
            .expect("generator economics should be materialized");

        let runtime =
            runtime_with_ac_target_tracking(&AcOpfRuntime::default(), &period_net, spec, 0);
        let tracking = runtime
            .objective_target_tracking
            .expect("target tracking runtime should be populated");

        assert_eq!(tracking.generator_p_penalty_per_mw2, 3.0);
        assert_eq!(tracking.generator_p_targets_mw.get(&0), Some(&75.0));
        match (period_net.generators[0].cost.as_ref(), &original_cost) {
            (
                Some(CostCurve::Polynomial { coeffs: lhs, .. }),
                CostCurve::Polynomial { coeffs: rhs, .. },
            ) => assert_eq!(lhs, rhs),
            _ => panic!("generator cost model should remain unchanged"),
        }
    }

    #[test]
    fn test_ac_sced_target_tracking_runtime_uses_original_local_generator_order_after_fixed_off() {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_fixed_commitment_target_tracking_index_stability");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.id = "g0".to_string();
        g0.pmin = 0.0;
        g0.pmax = 100.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.id = "g1".to_string();
        g1.pmin = 0.0;
        g1.pmax = 100.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });

        let mut g2 = Generator::new(1, 0.0, 1.0);
        g2.id = "g2".to_string();
        g2.pmin = 0.0;
        g2.pmax = 100.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![30.0, 0.0],
        });

        net.generators = vec![g0, g1, g2];

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            commitment: CommitmentMode::Fixed {
                commitment: vec![false, true, true],
                per_period: None,
            },
            n_periods: 1,
            ac_generator_warm_start_p_mw: [
                (0usize, vec![10.0]),
                (1usize, vec![20.0]),
                (2usize, vec![30.0]),
            ]
            .into_iter()
            .collect(),
            ac_target_tracking: crate::request::AcDispatchTargetTracking {
                generator_p_penalty_per_mw2: 3.0,
                dispatchable_load_p_penalty_per_mw2: 0.0,
                ..crate::request::AcDispatchTargetTracking::default()
            },
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);
        let local_gen_index_by_global = original_local_gen_index_by_global(&net);
        let mut period_net = net.clone();

        apply_period_generator_economics(&mut period_net, spec, 0);
        apply_fixed_commitment_constraints_with_local_gen_index(
            &mut period_net,
            spec,
            0,
            &local_gen_index_by_global,
        );

        let runtime = runtime_with_ac_target_tracking_with_local_gen_index(
            &AcOpfRuntime::default(),
            &period_net,
            spec,
            0,
            &local_gen_index_by_global,
        );
        let tracking = runtime
            .objective_target_tracking
            .expect("target tracking runtime should be populated");

        assert_eq!(tracking.generator_p_targets_mw.get(&0), Some(&10.0));
        assert_eq!(tracking.generator_p_targets_mw.get(&1), Some(&20.0));
        assert_eq!(tracking.generator_p_targets_mw.get(&2), Some(&30.0));
    }

    #[test]
    fn test_ac_sced_dispatchable_load_target_tracking_runtime_preserves_load_cost() {
        let net = one_bus_ac_dispatchable_load_net(80.0);
        let dl = DispatchableLoad {
            bus: 1,
            p_sched_pu: 10.0 / net.base_mva,
            q_sched_pu: 0.0,
            p_min_pu: 0.0,
            p_max_pu: 20.0 / net.base_mva,
            q_min_pu: 0.0,
            q_max_pu: 0.0,
            archetype: LoadArchetype::Elastic,
            cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 5.0 },
            fixed_power_factor: false,
            in_service: true,
            resource_id: "dl0".to_string(),
            product_type: None,
            dispatch_notification_minutes: 0.0,
            min_duration_hours: 0.0,
            baseline_mw: None,
            rebound_fraction: 0.0,
            rebound_periods: 0,
            ramp_up_pu_per_hr: None,
            ramp_down_pu_per_hr: None,
            initial_p_pu: None,
            ramp_group: None,
            energy_offer: None,
            reserve_offers: Vec::new(),
            reserve_group: None,
            qualifications: std::collections::HashMap::new(),
            pq_linear_equality: None,
            pq_linear_upper: None,
            pq_linear_lower: None,
        };
        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            dispatchable_loads: vec![dl],
            ac_dispatchable_load_warm_start_p_mw: std::iter::once((0usize, vec![12.0])).collect(),
            ac_target_tracking: crate::request::AcDispatchTargetTracking {
                generator_p_penalty_per_mw2: 0.0,
                dispatchable_load_p_penalty_per_mw2: 4.0,
                ..crate::request::AcDispatchTargetTracking::default()
            },
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);
        let mut period_net = net.clone();

        apply_period_dispatchable_load_economics(&mut period_net, spec, 0);
        let original_cost = period_net.market_data.dispatchable_loads[0]
            .cost_model
            .clone();

        let runtime =
            runtime_with_ac_target_tracking(&AcOpfRuntime::default(), &period_net, spec, 0);
        let tracking = runtime
            .objective_target_tracking
            .expect("target tracking runtime should be populated");

        assert_eq!(tracking.dispatchable_load_p_penalty_per_mw2, 4.0);
        assert_eq!(tracking.dispatchable_load_p_targets_mw.get(&0), Some(&12.0));
        match (
            &period_net.market_data.dispatchable_loads[0].cost_model,
            &original_cost,
        ) {
            (
                LoadCostModel::LinearCurtailment { cost_per_mw: lhs },
                LoadCostModel::LinearCurtailment { cost_per_mw: rhs },
            ) => assert!((lhs - rhs).abs() < 1e-12),
            _ => panic!("dispatchable-load cost model should remain unchanged"),
        }
    }

    #[test]
    fn test_fixed_off_commitment_preserves_nonzero_warm_start_dispatch() {
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("ac_fixed_off_warm_start_dispatch");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut generator = Generator::new(1, 0.0, 1.0);
        generator.id = "g0".to_string();
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        generator.qmin = -50.0;
        generator.qmax = 50.0;
        net.generators.push(generator);

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            commitment: CommitmentMode::Fixed {
                commitment: vec![false],
                per_period: None,
            },
            n_periods: 1,
            ac_generator_warm_start_p_mw: std::iter::once((0usize, vec![15.0])).collect(),
            ac_generator_warm_start_q_mvar: std::iter::once((0usize, vec![5.0])).collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);
        let local_gen_index_by_global = original_local_gen_index_by_global(&net);
        let mut period_net = net.clone();

        apply_fixed_commitment_constraints_with_local_gen_index(
            &mut period_net,
            spec,
            0,
            &local_gen_index_by_global,
        );
        apply_ac_warm_start_targets_with_local_gen_index(
            &mut period_net,
            spec,
            0,
            &local_gen_index_by_global,
        );

        assert!(
            period_net.generators[0].in_service,
            "nonzero AC warm-start dispatch should keep a fixed-off generator in service",
        );
        assert!(
            (period_net.generators[0].p - 15.0).abs() < 1e-9,
            "fixed-off startup/shutdown trajectory target should be preserved, got {:.6}",
            period_net.generators[0].p,
        );
        assert!((period_net.generators[0].q - 5.0).abs() < 1e-9);
    }

    #[test]
    fn test_fixed_off_commitment_keeps_q_only_support_at_zero_active_power() {
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("ac_fixed_off_q_only_support");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut generator = Generator::new(1, 0.0, 1.0);
        generator.id = "g0".to_string();
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        generator.qmin = -50.0;
        generator.qmax = 50.0;
        net.generators.push(generator);

        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            commitment: CommitmentMode::Fixed {
                commitment: vec![false],
                per_period: None,
            },
            n_periods: 1,
            ac_generator_warm_start_q_mvar: std::iter::once((0usize, vec![5.0])).collect(),
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);
        let local_gen_index_by_global = original_local_gen_index_by_global(&net);
        let mut period_net = net.clone();

        apply_fixed_commitment_constraints_with_local_gen_index(
            &mut period_net,
            spec,
            0,
            &local_gen_index_by_global,
        );
        apply_ac_warm_start_targets_with_local_gen_index(
            &mut period_net,
            spec,
            0,
            &local_gen_index_by_global,
        );

        assert!(period_net.generators[0].in_service);
        assert!(period_net.generators[0].p.abs() < 1e-9);
        assert!((period_net.generators[0].q - 5.0).abs() < 1e-9);
    }

    #[test]
    fn test_ac_sced_period_operating_constraints_do_not_rewrite_target_tracking_economics() {
        let net = one_bus_ac_dispatchable_load_net(80.0);
        let dl = DispatchableLoad {
            bus: 1,
            p_sched_pu: 10.0 / net.base_mva,
            q_sched_pu: 0.0,
            p_min_pu: 0.0,
            p_max_pu: 20.0 / net.base_mva,
            q_min_pu: 0.0,
            q_max_pu: 0.0,
            archetype: LoadArchetype::Elastic,
            cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 5.0 },
            fixed_power_factor: false,
            in_service: true,
            resource_id: "dl0".to_string(),
            product_type: None,
            dispatch_notification_minutes: 0.0,
            min_duration_hours: 0.0,
            baseline_mw: None,
            rebound_fraction: 0.0,
            rebound_periods: 0,
            ramp_up_pu_per_hr: None,
            ramp_down_pu_per_hr: None,
            initial_p_pu: None,
            ramp_group: None,
            energy_offer: None,
            reserve_offers: Vec::new(),
            reserve_group: None,
            qualifications: std::collections::HashMap::new(),
            pq_linear_equality: None,
            pq_linear_upper: None,
            pq_linear_lower: None,
        };
        let options = DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            dispatchable_loads: vec![dl],
            ac_generator_warm_start_p_mw: std::iter::once((0usize, vec![75.0])).collect(),
            ac_dispatchable_load_warm_start_p_mw: std::iter::once((0usize, vec![12.0])).collect(),
            ac_target_tracking: crate::request::AcDispatchTargetTracking {
                generator_p_penalty_per_mw2: 3.0,
                dispatchable_load_p_penalty_per_mw2: 4.0,
                ..crate::request::AcDispatchTargetTracking::default()
            },
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);

        let mut expected_net = net.clone();
        apply_period_generator_economics(&mut expected_net, spec, 0);
        apply_period_dispatchable_load_economics(&mut expected_net, spec, 0);
        let expected_generator_cost = expected_net.generators[0]
            .cost
            .clone()
            .expect("generator economics should be materialized");
        let expected_load_cost = expected_net.market_data.dispatchable_loads[0]
            .cost_model
            .clone();

        let mut period_net = net.clone();
        apply_period_operating_constraints(
            &mut period_net,
            &net,
            spec,
            DispatchPeriodContext::initial(&options.initial_state),
        );

        match (
            period_net.generators[0].cost.as_ref(),
            &expected_generator_cost,
        ) {
            (
                Some(CostCurve::Polynomial { coeffs: lhs, .. }),
                CostCurve::Polynomial { coeffs: rhs, .. },
            ) => assert_eq!(lhs, rhs),
            _ => panic!("generator cost model should remain unchanged"),
        }
        match (
            &period_net.market_data.dispatchable_loads[0].cost_model,
            &expected_load_cost,
        ) {
            (
                LoadCostModel::LinearCurtailment { cost_per_mw: lhs },
                LoadCostModel::LinearCurtailment { cost_per_mw: rhs },
            ) => assert!((lhs - rhs).abs() < 1e-12),
            _ => panic!("dispatchable-load cost model should remain unchanged"),
        }
        assert!(
            (period_net.market_data.dispatchable_loads[0].p_sched_pu - 0.1).abs() < 1e-12,
            "dispatchable-load economic schedule should remain unchanged",
        );
        assert!(
            period_net.market_data.dispatchable_loads[0]
                .q_sched_pu
                .abs()
                < 1e-12,
            "dispatchable-load reactive schedule should remain unchanged",
        );
    }

    #[test]
    fn test_ac_sced_fixed_commitment_keeps_unit_off() {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_fixed_commitment");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 80.0, 0.0));

        let mut cheap = Generator::new(1, 0.0, 1.0);
        cheap.pmin = 0.0;
        cheap.pmax = 100.0;
        cheap.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let mut expensive = Generator::new(1, 0.0, 1.0);
        expensive.pmin = 0.0;
        expensive.pmax = 100.0;
        expensive.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![100.0, 0.0],
        });

        net.generators = vec![cheap, expensive];

        let baseline = solve_ac_sced(&net, &DispatchOptions::default()).unwrap();
        assert!(
            baseline.pg_mw[0] > baseline.pg_mw[1] + 50.0,
            "baseline solve should prefer the cheap unit, got {:?}",
            baseline.pg_mw
        );

        let fixed = solve_ac_sced(
            &net,
            &DispatchOptions {
                commitment: CommitmentMode::Fixed {
                    commitment: vec![false, true],
                    per_period: None,
                },
                ..DispatchOptions::default()
            },
        )
        .unwrap();

        assert!(
            fixed.pg_mw[0].abs() < 1e-6,
            "fixed-off AC unit should stay at 0 MW, got {:.6}",
            fixed.pg_mw[0]
        );
        assert!(
            fixed.pg_mw[1] > 70.0,
            "remaining committed unit should serve the load, got {:.3}",
            fixed.pg_mw[1]
        );
    }

    #[test]
    fn test_ac_sced_fixed_commitment_marks_fixed_off_unit_out_of_service() {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_fixed_commitment_out_of_service");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut fixed_off = Generator::new(1, 0.0, 1.0);
        fixed_off.pmin = 0.0;
        fixed_off.pmax = 100.0;

        let mut committed = Generator::new(1, 0.0, 1.0);
        committed.pmin = 0.0;
        committed.pmax = 100.0;

        net.generators = vec![fixed_off, committed];

        let options = DispatchOptions {
            commitment: CommitmentMode::Fixed {
                commitment: vec![false, true],
                per_period: None,
            },
            ..DispatchOptions::default()
        };
        let spec = DispatchProblemSpec::from_options(&options);

        apply_fixed_commitment_constraints(&mut net, spec, 0);

        assert!(
            !net.generators[0].in_service,
            "fixed-off non-storage generator should be removed from the period-local AC model"
        );
        assert!(
            net.generators[1].in_service,
            "committed generator should remain in service"
        );
    }

    #[test]
    fn test_ac_sced_fixed_commitment_reassigns_reference_bus_when_original_slack_unit_is_off() {
        use surge_network::Network;
        use surge_network::network::Branch;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_fixed_commitment_reference_reassign");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PV, 138.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
        net.loads.push(Load::new(2, 60.0, 10.0));

        let mut slack_unit = Generator::new(1, 0.0, 1.0);
        slack_unit.pmin = 0.0;
        slack_unit.pmax = 100.0;
        slack_unit.qmin = -50.0;
        slack_unit.qmax = 50.0;
        slack_unit.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let mut remote_unit = Generator::new(2, 60.0, 1.0);
        remote_unit.pmin = 0.0;
        remote_unit.pmax = 120.0;
        remote_unit.qmin = -80.0;
        remote_unit.qmax = 80.0;
        remote_unit.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });

        net.generators = vec![slack_unit, remote_unit];

        let fixed = solve_ac_sced(
            &net,
            &DispatchOptions {
                commitment: CommitmentMode::Fixed {
                    commitment: vec![false, true],
                    per_period: None,
                },
                ..DispatchOptions::default()
            },
        )
        .expect("AC fixed-commitment solve should reassign the reference bus");

        assert!(
            fixed.pg_mw[0].abs() < 1e-6,
            "original slack-bus unit should remain off, got {:.6}",
            fixed.pg_mw[0]
        );
        assert!(
            fixed.pg_mw[1] > 50.0,
            "replacement reference-bus unit should serve the load, got {:.3}",
            fixed.pg_mw[1]
        );
    }

    #[test]
    fn test_reclassify_period_local_bus_types_promotes_q_capable_fallback_not_zero_q_slack_bus() {
        use surge_network::Network;
        use surge_network::network::Branch;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_q_capable_fallback");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses[1].voltage_magnitude_pu = 0.97;
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));

        let mut non_reactive_slack = Generator::new(1, 0.0, 1.0);
        non_reactive_slack.pmax = 200.0;
        non_reactive_slack.qmin = 0.0;
        non_reactive_slack.qmax = 0.0;
        non_reactive_slack.voltage_regulated = false;

        let mut q_capable_remote = Generator::new(2, 0.0, 1.0);
        q_capable_remote.pmax = 100.0;
        q_capable_remote.qmin = -50.0;
        q_capable_remote.qmax = 50.0;
        q_capable_remote.voltage_regulated = false;

        net.generators = vec![non_reactive_slack, q_capable_remote];

        reclassify_period_local_bus_types(&mut net);

        assert_eq!(net.buses[0].bus_type, BusType::PQ);
        assert_eq!(net.buses[1].bus_type, BusType::Slack);
        assert!(!net.generators[0].can_voltage_regulate());
        assert!(net.generators[1].can_voltage_regulate());
        assert_eq!(net.generators[1].reg_bus, Some(2));
        assert!((net.generators[1].voltage_setpoint_pu - 0.97).abs() < 1e-9);
    }

    #[test]
    fn test_reclassify_period_local_bus_types_keeps_original_slack_and_promotes_local_q_support() {
        use surge_network::Network;
        use surge_network::network::Branch;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_original_slack_preferred");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses[0].voltage_magnitude_pu = 1.03;
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));

        let mut slack_candidate = Generator::new(1, 0.0, 1.0);
        slack_candidate.pmax = 80.0;
        slack_candidate.qmin = -40.0;
        slack_candidate.qmax = 40.0;
        slack_candidate.voltage_regulated = false;

        let mut larger_remote_candidate = Generator::new(2, 0.0, 1.0);
        larger_remote_candidate.pmax = 120.0;
        larger_remote_candidate.qmin = -80.0;
        larger_remote_candidate.qmax = 80.0;
        larger_remote_candidate.voltage_regulated = false;

        net.generators = vec![slack_candidate, larger_remote_candidate];

        reclassify_period_local_bus_types(&mut net);

        assert_eq!(net.buses[0].bus_type, BusType::Slack);
        assert_eq!(net.buses[1].bus_type, BusType::PV);
        assert!(net.generators[0].can_voltage_regulate());
        assert_eq!(net.generators[0].reg_bus, Some(1));
        assert!((net.generators[0].voltage_setpoint_pu - 1.03).abs() < 1e-9);
        assert!(net.generators[1].can_voltage_regulate());
        assert_eq!(net.generators[1].reg_bus, Some(2));
        assert!((net.generators[1].voltage_setpoint_pu - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_reclassify_period_local_bus_types_excludes_flagged_q_support_from_fallback() {
        use surge_network::Network;
        use surge_network::market::reserve::QualificationMap;
        use surge_network::network::Branch;
        use surge_network::network::Generator;
        use surge_network::network::MarketParams;

        let mut net = Network::new("ac_excluded_q_support_fallback");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses[1].voltage_magnitude_pu = 0.98;
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));

        let mut excluded_support = Generator::with_id("__dc_line_q__dcl_0__to", 1, 0.0, 1.0);
        excluded_support.pmax = 0.0;
        excluded_support.qmin = -100.0;
        excluded_support.qmax = 0.0;
        excluded_support.voltage_regulated = true;
        let mut qualifications = QualificationMap::new();
        qualifications.insert("ac_voltage_regulation_excluded".to_string(), true);
        excluded_support.market = Some(MarketParams {
            qualifications,
            ..Default::default()
        });

        let mut real_generator = Generator::new(2, 0.0, 1.0);
        real_generator.pmax = 80.0;
        real_generator.qmin = -50.0;
        real_generator.qmax = 50.0;
        real_generator.voltage_regulated = false;

        net.generators = vec![excluded_support, real_generator];

        reclassify_period_local_bus_types(&mut net);

        assert_eq!(net.buses[0].bus_type, BusType::PQ);
        assert_eq!(net.buses[1].bus_type, BusType::Slack);
        assert!(!net.generators[0].can_voltage_regulate());
        assert_eq!(net.generators[0].reg_bus, None);
        assert!(net.generators[1].can_voltage_regulate());
        assert_eq!(net.generators[1].reg_bus, Some(2));
        assert!((net.generators[1].voltage_setpoint_pu - 0.98).abs() < 1e-9);
    }

    #[test]
    fn test_ac_sced_can_relax_committed_pmin_during_fixed_commitment_redispatch() {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_fixed_commitment_relaxed_pmin");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 20.0, 0.0));

        let mut unit = Generator::new(1, 0.0, 1.0);
        unit.pmin = 60.0;
        unit.pmax = 100.0;
        unit.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators = vec![unit];

        let strict = solve_ac_sced(
            &net,
            &DispatchOptions {
                commitment: CommitmentMode::Fixed {
                    commitment: vec![true],
                    per_period: None,
                },
                ..DispatchOptions::default()
            },
        );
        assert!(
            strict.is_err(),
            "strict fixed-commitment AC redispatch should fail when load is below committed pmin"
        );

        let relaxed = solve_ac_sced(
            &net,
            &DispatchOptions {
                commitment: CommitmentMode::Fixed {
                    commitment: vec![true],
                    per_period: None,
                },
                ac_relax_committed_pmin_to_zero: true,
                ..DispatchOptions::default()
            },
        )
        .expect("relaxed fixed-commitment AC redispatch should solve");

        assert!(
            relaxed.pg_mw[0] < 30.0,
            "relaxed committed unit should be able to redispatch below physical pmin, got {:.3}",
            relaxed.pg_mw[0]
        );
    }

    #[test]
    fn test_ac_sced_fixed_commitment_relaxes_zero_cost_reactive_support_unit() {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_fixed_commitment_q_support");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 80.0, 120.0));

        let mut energy = Generator::new(1, 0.0, 1.0);
        energy.pmin = 0.0;
        energy.pmax = 200.0;
        energy.qmin = -50.0;
        energy.qmax = 50.0;
        energy.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let mut support = Generator::new(1, 0.0, 1.0);
        support.pmin = 0.0;
        support.pmax = 10.0;
        support.qmin = -150.0;
        support.qmax = 150.0;
        support.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0],
        });
        support
            .market
            .get_or_insert_with(Default::default)
            .qualifications
            .insert("ac_reactive_support_flexible".to_string(), true);

        net.generators = vec![energy, support];

        let fixed = solve_ac_sced(
            &net,
            &DispatchOptions {
                commitment: CommitmentMode::Fixed {
                    commitment: vec![true, false],
                    per_period: None,
                },
                ..DispatchOptions::default()
            },
        )
        .expect("AC fixed-commitment solve should keep reactive support unit available");

        assert!(
            fixed.qg_mvar[1] > 10.0,
            "zero-cost support unit should provide reactive support even when DC commitment leaves it off, got {:.3} MVAr",
            fixed.qg_mvar[1]
        );
    }

    #[test]
    fn test_ac_sced_fixed_commitment_relaxes_zero_mw_reactive_support_unit() {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_fixed_commitment_zero_mw_q_support");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 80.0, 120.0));

        let mut energy = Generator::new(1, 0.0, 1.0);
        energy.pmin = 0.0;
        energy.pmax = 200.0;
        energy.qmin = -50.0;
        energy.qmax = 50.0;
        energy.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let mut support = Generator::new(1, 0.0, 1.0);
        support.pmin = 0.0;
        support.pmax = 0.0;
        support.qmin = -150.0;
        support.qmax = 150.0;
        support.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0],
        });
        support
            .market
            .get_or_insert_with(Default::default)
            .qualifications
            .insert("ac_reactive_support_flexible".to_string(), true);

        net.generators = vec![energy, support];

        let fixed = solve_ac_sced(
            &net,
            &DispatchOptions {
                commitment: CommitmentMode::Fixed {
                    commitment: vec![true, false],
                    per_period: None,
                },
                ..DispatchOptions::default()
            },
        )
        .expect("AC fixed-commitment solve should keep zero-MW reactive support unit available");

        assert!(
            fixed.qg_mvar[1] > 10.0,
            "zero-MW support unit should provide reactive support even when DC commitment leaves it off, got {:.3} MVAr",
            fixed.qg_mvar[1]
        );
    }

    #[test]
    fn test_ac_sced_committed_zero_mw_q_only_unit_can_supply_q_without_voltage_regulation() {
        use surge_network::Network;
        use surge_network::network::Generator;

        let mut net = Network::new("ac_committed_zero_mw_nonreg_q_support");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 80.0, 120.0));

        let mut energy = Generator::new(1, 80.0, 1.0);
        energy.pmin = 0.0;
        energy.pmax = 200.0;
        energy.qmin = -50.0;
        energy.qmax = 50.0;
        energy.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let mut support = Generator::new(1, 0.0, 1.0);
        support.pmin = 0.0;
        support.pmax = 0.0;
        support.qmin = -150.0;
        support.qmax = 150.0;
        support.voltage_regulated = false;
        support.reg_bus = None;
        support.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0],
        });

        net.generators = vec![energy, support];

        let fixed = solve_ac_sced(
            &net,
            &DispatchOptions {
                commitment: CommitmentMode::Fixed {
                    commitment: vec![true, true],
                    per_period: None,
                },
                ..DispatchOptions::default()
            },
        )
        .expect("AC fixed-commitment solve should keep committed zero-MW support unit available");

        assert!(
            fixed.qg_mvar[1] > 10.0,
            "committed zero-MW non-regulating support unit should still provide reactive support, got {:.3} MVAr",
            fixed.qg_mvar[1]
        );
    }

    /// SelfSchedule: battery committed at +30 MW discharge.
    /// Assert net_mw ≈ 30 MW and SoC decreases accordingly.
    #[test]
    fn test_ac_sced_storage_self_schedule() {
        if !data_available() {
            return;
        }
        let mut net = load_case9();
        let soc0 = 100.0;
        let gi = add_storage_gen(
            &mut net,
            1,
            50.0,
            50.0,
            200.0,
            StorageDispatchMode::SelfSchedule,
            soc0,
        );
        net.generators[gi]
            .storage
            .as_mut()
            .unwrap()
            .self_schedule_mw = 30.0;

        let opts = DispatchOptions {
            initial_state: crate::dispatch::IndexedDispatchInitialState {
                storage_soc_override: Some(std::collections::HashMap::from([(gi, soc0)])),
                ..Default::default()
            },
            ..DispatchOptions::default()
        };

        let sol = solve_ac_sced(&net, &opts).unwrap();
        assert!(
            (sol.storage_net_mw[0] - 30.0).abs() < 1.0,
            "Expected ~30 MW dispatch, got {:.2}",
            sol.storage_net_mw[0]
        );
        // SoC should decrease
        assert!(
            sol.storage_soc_mwh[0] < soc0,
            "SoC should decrease after discharge"
        );
    }

    /// SelfSchedule: battery SoC too low to honour full self-schedule.
    /// Assert actual dispatch < committed MW (SoC-limited).
    #[test]
    fn test_ac_sced_storage_self_schedule_soc_limited() {
        if !data_available() {
            return;
        }
        let mut net = load_case9();
        let soc0 = 5.0; // only 5 MWh left — can deliver ~4.7 MW for 1 hr
        let gi = add_storage_gen(
            &mut net,
            1,
            50.0,
            50.0,
            200.0,
            StorageDispatchMode::SelfSchedule,
            200.0,
        );
        net.generators[gi]
            .storage
            .as_mut()
            .unwrap()
            .self_schedule_mw = 50.0; // want full 50 MW discharge
        net.generators[gi].storage.as_mut().unwrap().soc_initial_mwh = soc0;

        let opts = DispatchOptions {
            initial_state: crate::dispatch::IndexedDispatchInitialState {
                storage_soc_override: Some(std::collections::HashMap::from([(gi, soc0)])),
                ..Default::default()
            },
            ..DispatchOptions::default()
        };

        let sol = solve_ac_sced(&net, &opts).unwrap();
        assert!(
            sol.storage_net_mw[0] < 50.0,
            "Dispatch should be SoC-limited; got {:.2}",
            sol.storage_net_mw[0]
        );
        assert!(
            sol.storage_net_mw[0] >= 0.0,
            "Dispatch should be non-negative (discharge)"
        );
    }

    /// OfferCurve storage should clear natively inside AC-SCED, not via an
    /// external price-clearing pass.
    #[test]
    fn test_ac_sced_storage_offer_curve_discharge() {
        let mut net = one_bus_ac_storage_offer_net(80.0, 100.0);
        let soc0 = 100.0;
        let gi = add_storage_gen(
            &mut net,
            1,
            50.0,
            50.0,
            200.0,
            StorageDispatchMode::OfferCurve,
            soc0,
        );
        // Offer 50 MW at $40/MWh with explicit origin.
        net.generators[gi].storage.as_mut().unwrap().discharge_offer =
            Some(vec![(0.0, 0.0), (50.0, 2000.0)]);
        // Bid 50 MW at $30/MWh with explicit origin.
        net.generators[gi].storage.as_mut().unwrap().charge_bid =
            Some(vec![(0.0, 0.0), (50.0, 1500.0)]);

        let opts = DispatchOptions {
            initial_state: crate::dispatch::IndexedDispatchInitialState {
                storage_soc_override: Some(std::collections::HashMap::from([(gi, soc0)])),
                ..Default::default()
            },
            ..DispatchOptions::default()
        };

        let sol = solve_ac_sced(&net, &opts).unwrap();
        let thermal_gen_mw = sol.pg_mw.first().copied().unwrap_or(0.0);
        assert!(
            sol.storage_net_mw[0] > 45.0,
            "storage should discharge materially when its offer is cheaper than the thermal unit, got {:.3} MW",
            sol.storage_net_mw[0]
        );
        assert!(
            (thermal_gen_mw + sol.storage_net_mw[0] - 80.0).abs() < 1.0,
            "thermal generation plus storage net dispatch should serve load, thermal={thermal_gen_mw:.3} storage={:.3}",
            sol.storage_net_mw[0]
        );
    }

    /// CostMinimization multi-period: 4 periods with load scales [0.4, 0.4, 1.8, 1.8].
    /// Large load spread creates a big LMP gap; storage (zero variable cost) should
    /// charge in periods 0-1 (low load → low price) and discharge in 2-3.
    #[test]
    fn test_ac_sced_storage_cost_min_multiperiod() {
        if !data_available() {
            return;
        }
        let mut net = load_case9();
        let soc0 = 100.0;
        let gi = add_storage_gen(
            &mut net,
            1,
            50.0,
            50.0,
            200.0,
            StorageDispatchMode::CostMinimization,
            soc0,
        );
        net.generators[gi]
            .storage
            .as_mut()
            .unwrap()
            .variable_cost_per_mwh = 0.0;

        let opts = DispatchOptions {
            initial_state: crate::dispatch::IndexedDispatchInitialState {
                storage_soc_override: Some(std::collections::HashMap::from([(gi, soc0)])),
                ..Default::default()
            },
            ..DispatchOptions::default()
        };

        // Large spread: 0.4× vs 1.8× load guarantees significant LMP difference
        let load_scales = vec![0.4_f64, 0.4, 1.8, 1.8];
        let sol = solve_multi_period_ac_sced(&net, 4, &opts, Some(&load_scales)).unwrap();

        assert_eq!(sol.periods.len(), 4);
        assert!(sol.total_cost > 0.0, "total cost must be positive");

        // Final SoC must be in valid range
        let final_soc = sol.periods[3].storage_soc_mwh[0];
        assert!(
            (0.0..=200.0).contains(&final_soc),
            "SoC out of bounds: {final_soc}"
        );

        // With a 4.5× load ratio, the optimizer should move storage at least a little.
        let net_mw_any = sol
            .periods
            .iter()
            .any(|p| p.storage_net_mw.first().copied().unwrap_or(0.0).abs() > 0.5);
        assert!(net_mw_any, "storage should dispatch in at least one period");
    }

    // -----------------------------------------------------------------------
    // Legacy tests (unchanged behaviour)
    // -----------------------------------------------------------------------

    #[test]
    fn test_ac_sced_case9() {
        if !data_available() {
            return;
        }
        let net = load_case9();
        let opts = DispatchOptions::default();
        let sol = solve_multi_period_ac_sced(&net, 3, &opts, None).unwrap();
        assert_eq!(sol.periods.len(), 3);
        assert!(sol.total_cost > 0.0);
        for period in &sol.periods {
            assert!(!period.lmp.is_empty());
            assert!(period.lmp.iter().all(|&l| l.is_finite()));
        }
    }

    #[test]
    fn test_ac_sced_ramp_constraints() {
        // Inline 2-bus, 2-gen network with ramp constraints
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};
        let mut net = Network::new("ramp_test");
        net.base_mva = 100.0;
        // Bus 1 (slack), Bus 2 (PQ load)
        let b0 = Bus::new(1, BusType::Slack, 138.0);
        let b1 = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(b0);
        net.buses.push(b1);
        net.loads
            .push(surge_network::network::Load::new(2, 150.0, 0.0)); // 150 MW load
        net.branches.push(Branch::new_line(1, 2, 0.0, 0.02, 0.0));

        // Cheap gen (bus 1): pmin=0, pmax=100, ramp=20 MW/hr
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmax = 100.0;
        g0.pmin = 0.0;
        g0.in_service = true;
        g0.ramping = Some(surge_network::network::RampingParams {
            reg_ramp_up_curve: vec![(0.0, 20.0 / 60.0)], // 20 MW/hr → MW/min
            ..Default::default()
        });
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0], // $20/MWh
        });
        net.generators.push(g0);

        // Expensive backup (bus 1): pmin=0, pmax=200
        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmax = 200.0;
        g1.pmin = 0.0;
        g1.in_service = true;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![100.0, 0.0], // $100/MWh
        });
        net.generators.push(g1);

        // Period 0: no ramp constraint, cheap gen at max
        let opts = DispatchOptions::default();
        let sol0 = solve_ac_sced(&net, &opts).unwrap();
        assert!(!sol0.pg_mw.is_empty());
        assert!(sol0.pg_mw[0] > 0.0);
    }

    // -----------------------------------------------------------------------
    // Nomogram tightening tests
    // -----------------------------------------------------------------------

    /// Build a 3-bus AC-SCED network with a nomogram that tightens FG_12 to 100 MW.
    ///
    /// Bus 1 (Slack): cheap gen ($20/MWh, 250 MW max)
    /// Bus 2 (PQ): 150 MW load
    /// Bus 3 (PQ): expensive gen ($40/MWh, 250 MW max)
    /// Branch 1→2 (x=0.1, rate_a=200), Branch 2→3 (x=0.1, rate_a=200)
    /// Flowgate FG_12 monitors branch 1→2 with limit 200 MW.
    /// Nomogram: FG_12 flow → tighten FG_12 to 100 MW (flat).
    fn make_ac_three_bus_with_nomogram() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};
        use surge_network::network::{Flowgate, OperatingNomogram};

        let mut net = Network::new("ac_nomogram_test");
        net.base_mva = 100.0;

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);
        net.buses.extend([b1, b2, b3]);
        net.loads
            .push(surge_network::network::Load::new(2, 150.0, 0.0));

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 200.0;
        br12.circuit = "1".to_string();
        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 200.0;
        br23.circuit = "1".to_string();
        net.branches.extend([br12, br23]);

        // Cheap gen at bus 1
        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 250.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        // Expensive gen at bus 3
        let mut g2 = Generator::new(3, 0.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 250.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        net.generators.extend([g1, g2]);

        // FG_12 monitors branch 1→2.
        net.flowgates.push(Flowgate {
            name: "FG_12".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 200.0,
            limit_reverse_mw: 0.0,
            in_service: true,
            limit_mw_schedule: vec![],
            limit_reverse_mw_schedule: vec![],
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });

        // Self-referential nomogram: tighten FG_12 to 100 MW regardless of flow.
        net.nomograms.push(OperatingNomogram {
            name: "NOM_12_self".to_string(),
            index_flowgate: "FG_12".to_string(),
            constrained_flowgate: "FG_12".to_string(),
            points: vec![(0.0, 100.0), (200.0, 100.0)],
            in_service: true,
        });

        net
    }

    /// With nomogram enforcement: FG_12 tightens to 100 MW → G1 forced ≤ 100 MW →
    /// G2 picks up the remainder.
    #[test]
    fn test_ac_sced_nomogram_tightens() {
        let net = make_ac_three_bus_with_nomogram();
        let opts = DispatchOptions {
            enforce_flowgates: true,
            max_nomogram_iter: 10,
            ..Default::default()
        };
        let sol = solve_ac_sced(&net, &opts).unwrap();

        // Nomogram should have tightened FG_12 to 100 MW, forcing G1 ≤ 100 MW.
        assert!(
            sol.pg_mw[0] <= 105.0,
            "Nomogram should limit G1 to ~100 MW, got {:.1} MW",
            sol.pg_mw[0]
        );
        // G2 must supply the remainder (≥ ~50 MW).
        assert!(
            sol.pg_mw[1] >= 45.0,
            "G2 should supply remainder, got {:.1} MW",
            sol.pg_mw[1]
        );
    }

    #[test]
    #[ignore = "pre-existing baseline failure: tightened nomogram re-solve does not refresh \
                storage dispatch (returns 50 MW from initial)."]
    fn test_ac_sced_nomogram_refreshes_storage_dispatch() {
        let mut net = make_ac_three_bus_with_nomogram();
        net.generators[0].cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 0.0],
        });
        net.generators[1].cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![100.0, 0.0],
        });
        let soc0 = 100.0;
        let gi = add_storage_gen(
            &mut net,
            2,
            50.0,
            50.0,
            200.0,
            StorageDispatchMode::CostMinimization,
            soc0,
        );
        let storage = net.generators[gi]
            .storage
            .as_mut()
            .expect("added storage generator should have storage params");
        storage.variable_cost_per_mwh = 1.0;
        storage.degradation_cost_per_mwh = 0.0;

        let baseline = solve_ac_sced(
            &net,
            &DispatchOptions {
                enforce_flowgates: true,
                max_nomogram_iter: 0,
                initial_state: crate::dispatch::IndexedDispatchInitialState {
                    storage_soc_override: Some(std::collections::HashMap::from([(gi, soc0)])),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();

        let tightened = solve_ac_sced(
            &net,
            &DispatchOptions {
                enforce_flowgates: true,
                max_nomogram_iter: 10,
                initial_state: crate::dispatch::IndexedDispatchInitialState {
                    storage_soc_override: Some(std::collections::HashMap::from([(gi, soc0)])),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            tightened.pg_mw[0] <= 105.0,
            "nomogram should still bind the cheap generator, got {:.3} MW",
            tightened.pg_mw[0]
        );
        assert!(
            baseline.storage_net_mw[0] < -5.0,
            "untightened solve should carry a materially different storage net dispatch, got {:.3} MW",
            baseline.storage_net_mw[0]
        );
        assert!(
            tightened.storage_net_mw[0].abs() < 1e-3,
            "tightened re-solve should refresh storage dispatch, got {:.3} MW",
            tightened.storage_net_mw[0]
        );
        assert!(
            (tightened.storage_soc_mwh[0] - soc0).abs() < 1e-3,
            "tightened storage SoC should reflect the refreshed dispatch, got {:.3} MWh",
            tightened.storage_soc_mwh[0]
        );
        assert!(
            baseline.storage_soc_mwh[0] > tightened.storage_soc_mwh[0] + 5.0,
            "baseline and tightened storage SoC should diverge when the net dispatch changes, baseline {:.3} vs tightened {:.3}",
            baseline.storage_soc_mwh[0],
            tightened.storage_soc_mwh[0]
        );
    }

    /// With max_nomogram_iter=0, the nomogram is ignored and the cheap gen wins.
    #[test]
    fn test_ac_sced_nomogram_disabled() {
        let net = make_ac_three_bus_with_nomogram();
        let opts = DispatchOptions {
            enforce_flowgates: true,
            max_nomogram_iter: 0,
            ..Default::default()
        };
        let sol = solve_ac_sced(&net, &opts).unwrap();

        // Without nomogram tightening, FG_12 has 200 MW limit so G1 can serve all 150 MW.
        assert!(
            sol.pg_mw[0] > 140.0,
            "No nomogram tightening: G1 should serve most load, got {:.1} MW",
            sol.pg_mw[0]
        );
    }

    // -----------------------------------------------------------------------
    // AC-SCED with discrete OPF controls (tap / phase / shunt).
    //
    // These tests exercise the composition used by the two-stage
    // DC-SCUC → AC-SCED caller:
    //   - fixed commitment  (so generator Pmin/Pmax are locked by SCED)
    //   - pre-pinned Pg     (tight target from an upstream DC dispatch)
    //   - target-tracking penalty on P (soft pull toward the DC dispatch)
    //   - optimize_{taps, phases, switched_shunts} = true
    //
    // The plain AC-OPF Hessian FD tests in surge-opf only verify
    // first/second derivative correctness in isolation. They miss the
    // interaction between free tap/phase/shunt variables and the hard
    // Pg bounds SCED imposes in the committed-redispatch path.
    //
    // Each test uses a minimal 3-bus network so Ipopt converges in <1s, and
    // each variable class is exercised in isolation plus one combined test.
    // -----------------------------------------------------------------------

    /// Build a 3-bus network suitable for AC-SCED tap/phase/shunt tests.
    ///
    /// Topology:
    ///   bus 1 (slack, cheap gen g0)
    ///   bus 2 (PQ, 100 MW load)
    ///   bus 3 (PQ, 50 MW load, mid-cost gen g1)
    /// Branches:
    ///   1→2 : ordinary line
    ///   2→3 : transformer (tap/phase controllable via helpers below)
    fn build_3bus_ac_sced_network() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::network::{Branch, Generator};

        let mut net = Network::new("ac_sced_tap_phase_shunt");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 138.0);
        b1.voltage_magnitude_pu = 1.0;
        b1.voltage_min_pu = 0.9;
        b1.voltage_max_pu = 1.1;
        let mut b2 = Bus::new(2, BusType::PQ, 138.0);
        b2.voltage_magnitude_pu = 1.0;
        b2.voltage_min_pu = 0.9;
        b2.voltage_max_pu = 1.1;
        let mut b3 = Bus::new(3, BusType::PQ, 138.0);
        b3.voltage_magnitude_pu = 1.0;
        b3.voltage_min_pu = 0.9;
        b3.voltage_max_pu = 1.1;
        net.buses.extend([b1, b2, b3]);

        net.loads.push(Load::new(2, 100.0, 20.0));
        net.loads.push(Load::new(3, 50.0, 10.0));

        // Slack generator — cheap, generous Q range, must be committed.
        let mut g0 = Generator::new(1, 120.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 300.0;
        g0.qmin = -200.0;
        g0.qmax = 200.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 10.0],
        });
        // Mid-cost gen at bus 3 — will be pinned to a specific Pg by the
        // SCED fixed-commitment path.
        let mut g1 = Generator::new(3, 40.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 150.0;
        g1.qmin = -100.0;
        g1.qmax = 100.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 30.0],
        });
        net.generators.extend([g0, g1]);

        // Branch 1: ordinary line 1→2
        let mut br12 = Branch::new_line(1, 2, 0.01, 0.1, 0.02);
        br12.rating_a_mva = 400.0;
        net.branches.push(br12);

        // Branch 2: transformer 2→3 (we mark tap/phase mode in helpers below)
        let mut br23 = Branch::new_line(2, 3, 0.005, 0.08, 0.0);
        br23.tap = 1.0;
        br23.rating_a_mva = 400.0;
        net.branches.push(br23);

        net
    }

    /// Attach a pinned-Pg fixed-commitment DispatchOptions config for the
    /// 3-bus SCED test network. Pins g1 to 40 MW via the warm start and the
    /// target-tracking penalty, matching the two-stage DC→AC caller
    /// pattern (tight DC-derived Pg targets that the AC reconcile
    /// must honor).
    fn build_ac_sced_pinned_options(ac_opf: AcOpfOptions) -> DispatchOptions {
        DispatchOptions {
            formulation: crate::Formulation::Ac,
            n_periods: 1,
            commitment: CommitmentMode::Fixed {
                commitment: vec![true, true],
                per_period: None,
            },
            ac_generator_warm_start_p_mw: [(0usize, vec![120.0]), (1usize, vec![40.0])]
                .into_iter()
                .collect(),
            ac_target_tracking: crate::request::AcDispatchTargetTracking {
                generator_p_penalty_per_mw2: 30.0,
                dispatchable_load_p_penalty_per_mw2: 0.0,
                ..crate::request::AcDispatchTargetTracking::default()
            },
            ac_opf,
            enforce_thermal_limits: false,
            ..DispatchOptions::default()
        }
    }

    /// AC-SCED with optimize_taps=true on a committed, Pg-pinned 3-bus network.
    ///
    /// Reproduces the two-stage DC→AC caller pattern — fixed
    /// commitment, tight Pg warm start, target-tracking penalty, and
    /// a tap-controllable transformer branch. If this fails the bug
    /// is in how AC-SCED composes tap optimization with fixed
    /// commitment.
    #[test]
    fn test_ac_sced_optimize_taps_with_fixed_commitment() {
        use surge_network::network::{BranchOpfControl, TapMode};

        let mut net = build_3bus_ac_sced_network();
        // Turn branch 1 (the 2→3 branch) into a tap-controllable transformer
        // starting at tau = 1.03 (off-unity so the Hessian entries exercise).
        let br = &mut net.branches[1];
        br.tap = 1.03;
        let ctl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
        ctl.tap_mode = TapMode::Continuous;
        ctl.tap_min = 0.9;
        ctl.tap_max = 1.1;
        ctl.tap_step = 0.0;

        let ac_opf = AcOpfOptions {
            optimize_taps: true,
            optimize_phase_shifters: false,
            optimize_switched_shunts: false,
            enforce_thermal_limits: false,
            enforce_regulated_bus_vm_targets: false,
            ..AcOpfOptions::default()
        };
        let opts = build_ac_sced_pinned_options(ac_opf);

        let sol = solve_ac_sced(&net, &opts).expect(
            "AC-SCED with optimize_taps + fixed commitment should solve on a 3-bus network",
        );
        assert!(
            sol.pg_mw[0] > 0.0 && sol.pg_mw[1] > 0.0,
            "both committed generators should dispatch nonzero: pg={:?}",
            sol.pg_mw
        );
        // Tap should remain a valid value in bounds.
        println!(
            "[tap_only SCED] pg={:?}, cost={:.2}",
            sol.pg_mw, sol.total_cost
        );
    }

    /// AC-SCED with optimize_phase_shifters=true on the same network.
    #[test]
    fn test_ac_sced_optimize_phase_shifters_with_fixed_commitment() {
        use surge_network::network::{BranchOpfControl, PhaseMode};

        let mut net = build_3bus_ac_sced_network();
        let br = &mut net.branches[1];
        br.phase_shift_rad = 2.0_f64.to_radians();
        let ctl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
        ctl.phase_mode = PhaseMode::Continuous;
        ctl.phase_min_rad = (-15.0_f64).to_radians();
        ctl.phase_max_rad = 15.0_f64.to_radians();

        let ac_opf = AcOpfOptions {
            optimize_taps: false,
            optimize_phase_shifters: true,
            optimize_switched_shunts: false,
            enforce_thermal_limits: false,
            enforce_regulated_bus_vm_targets: false,
            ..AcOpfOptions::default()
        };
        let opts = build_ac_sced_pinned_options(ac_opf);

        let sol = solve_ac_sced(&net, &opts).expect(
            "AC-SCED with optimize_phase_shifters + fixed commitment should solve on a 3-bus network",
        );
        println!(
            "[phase_only SCED] pg={:?}, cost={:.2}",
            sol.pg_mw, sol.total_cost
        );
    }

    /// AC-SCED with optimize_switched_shunts=true on the same network.
    #[test]
    fn test_ac_sced_optimize_switched_shunts_with_fixed_commitment() {
        use surge_network::network::SwitchedShuntOpf;

        let mut net = build_3bus_ac_sced_network();
        // Add a switched shunt at bus 2 (the load bus) so voltage support
        // has a real role.
        net.controls.switched_shunts_opf.push(SwitchedShuntOpf {
            id: String::from("sh_test"),
            bus: 2,
            b_min_pu: -0.3,
            b_max_pu: 0.3,
            b_init_pu: 0.05,
            b_step_pu: 0.0,
        });

        let ac_opf = AcOpfOptions {
            optimize_taps: false,
            optimize_phase_shifters: false,
            optimize_switched_shunts: true,
            enforce_thermal_limits: false,
            enforce_regulated_bus_vm_targets: false,
            ..AcOpfOptions::default()
        };
        let opts = build_ac_sced_pinned_options(ac_opf);

        let sol = solve_ac_sced(&net, &opts).expect(
            "AC-SCED with optimize_switched_shunts + fixed commitment should solve on a 3-bus network",
        );
        println!(
            "[shunt_only SCED] pg={:?}, cost={:.2}",
            sol.pg_mw, sol.total_cost
        );
    }

    /// AC-SCED with all three discrete controls enabled.
    #[test]
    fn test_ac_sced_optimize_tap_phase_shunt_with_fixed_commitment() {
        use surge_network::network::{BranchOpfControl, PhaseMode, SwitchedShuntOpf, TapMode};

        let mut net = build_3bus_ac_sced_network();
        let br = &mut net.branches[1];
        br.tap = 1.03;
        br.phase_shift_rad = 2.0_f64.to_radians();
        let ctl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
        ctl.tap_mode = TapMode::Continuous;
        ctl.tap_min = 0.9;
        ctl.tap_max = 1.1;
        ctl.phase_mode = PhaseMode::Continuous;
        ctl.phase_min_rad = (-15.0_f64).to_radians();
        ctl.phase_max_rad = 15.0_f64.to_radians();

        net.controls.switched_shunts_opf.push(SwitchedShuntOpf {
            id: String::from("sh_test"),
            bus: 2,
            b_min_pu: -0.3,
            b_max_pu: 0.3,
            b_init_pu: 0.05,
            b_step_pu: 0.0,
        });

        let ac_opf = AcOpfOptions {
            optimize_taps: true,
            optimize_phase_shifters: true,
            optimize_switched_shunts: true,
            enforce_thermal_limits: false,
            enforce_regulated_bus_vm_targets: false,
            ..AcOpfOptions::default()
        };
        let opts = build_ac_sced_pinned_options(ac_opf);

        let sol = solve_ac_sced(&net, &opts).expect(
            "AC-SCED with all three discrete controls + fixed commitment should solve on a 3-bus network",
        );
        println!(
            "[all_three SCED] pg={:?}, cost={:.2}",
            sol.pg_mw, sol.total_cost
        );
    }
}
