// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared loss-factor iteration for horizon SCUC solves and pricing LPs.
//!
//! This module carries three pieces that together support the DC loss
//! model in SCUC:
//!
//! 1. [`LossFactorPrep`] — the immutable setup data (original bus-balance
//!    RHS, bus load allocations, loss-PTDF rows, and the per-period list
//!    of `(a_value position, bus_idx, original coefficient)` injections).
//!    Built once per `solve_problem` call from [`build_loss_factor_prep`].
//!
//! 2. [`apply_bus_loss_factors`] — applies a given `(dloss_dp,
//!    total_losses_mw)` estimate to the problem's bus-balance rows and
//!    injection-coefficient entries. Call before the first MIP to
//!    warm-start the LP with a loss estimate; call again inside
//!    [`iterate_loss_factors`] for the post-solve refinement.
//!
//! 3. [`iterate_loss_factors`] — the classical fixed-point refinement
//!    that reads the solved theta, recomputes `dloss_dp` and total
//!    losses, reapplies them, and re-solves. Accepts an optional
//!    `initial_dloss_dp` hint used as the convergence-detection
//!    reference (so a pre-MIP warm-start seeded by the same dloss can
//!    converge in zero inner solves when the lossless MIP reached the
//!    same dispatch).
//!
//! A warm-start source (security-loop cache, DC PF on rough dispatch,
//! load-pattern approximation, or caller-supplied uniform rate)
//! populates a [`LossFactorWarmStart`] and hands it to `solve_problem`,
//! which calls `apply_bus_loss_factors` before the initial MIP and
//! threads the same `dloss_dp` into `iterate_loss_factors`.

#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;

use surge_network::Network;
use surge_opf::backends::{LpPrimalStart, LpResult, LpSolveStatus, LpSolver, SparseProblem};
use tracing::{debug, info, warn};

use super::layout::ScucLayout;
use crate::common::dc::solve_sparse_problem_with_start;
use crate::common::spec::DispatchProblemSpec;
use crate::error::ScedError;

/// Minimum `dloss_dp` magnitude that gets written into the LP bus-
/// balance coefficients. Values below this (i.e. loss sensitivity <
/// 0.01% per MW) are rounded to zero to avoid micro-perturbations
/// that disturb the warm basis without meaningfully changing the
/// optimum. Applied symmetrically in `apply_bus_loss_factors` for
/// both pre-MIP warm start and post-MIP refinement.
const DLOSS_MIN: f64 = 1e-4;

pub(super) struct ScucLossIterationInput<'a> {
    pub solver: &'a dyn LpSolver,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub hourly_networks: &'a [Network],
    pub bus_map: &'a HashMap<u32, usize>,
    pub layout: &'a ScucLayout,
    #[allow(dead_code)]
    pub gen_bus_idx: &'a [usize],
    pub hour_row_bases: &'a [usize],
    pub n_flow: usize,
    pub n_bus: usize,
    pub time_limit_secs: Option<f64>,
    pub problem: &'a mut SparseProblem,
    pub solution: &'a mut LpResult,
}

/// One physical injection/withdrawal coefficient that participates in
/// a bus power-balance row.
///
/// Captured once at setup time so the loss-factor iteration (or
/// pre-MIP warm-start) can keep scaling the original coefficient by
/// the per-period bus penalty factor without losing track of the
/// unscaled value.
#[derive(Clone, Copy, Debug)]
pub(super) struct BusInjectionCoeff {
    /// Index into `SparseProblem::a_value` of the entry to scale.
    pos: usize,
    /// Bus the row corresponds to (used to look up `dloss_dp[t][bus_idx]`).
    bus_idx: usize,
    /// Coefficient as it was when the matrix was first built (before
    /// any loss-factor scaling). Multiplied by `(1 - dloss[bus_idx])`
    /// at every loss-factor application.
    original: f64,
}

/// Immutable setup data for loss-factor applications on a SCUC LP.
///
/// Built once per `solve_problem` call from
/// [`build_loss_factor_prep`]. Reused for both pre-MIP warm-start
/// application via [`apply_bus_loss_factors`] and the post-MIP
/// refinement iteration inside [`iterate_loss_factors`], so the
/// ~O(n_nz) walk of the problem's column data and the per-period
/// loss-PTDF construction only happen once.
pub(super) struct LossFactorPrep {
    /// Per-period list of bus-balance-row coefficient entries (one per
    /// physical injection or withdrawal column). Ordered by the order
    /// in which columns were walked; position is an index into
    /// `SparseProblem::a_value`.
    pub injections_by_hour: Vec<Vec<BusInjectionCoeff>>,
    /// Per-period bus-balance row RHS captured before any loss-factor
    /// adjustment. `[t][bus_idx]`. Used to reset the RHS each apply so
    /// repeated calls remain correct.
    pub orig_rhs_by_hour: Vec<Vec<f64>>,
    /// Per-period bus load in MW: `[t][bus_idx]`. Used to distribute
    /// the total system losses back onto each bus as `loss_share[i] =
    /// total_losses * (bus_load[i] / total_load)`.
    pub bus_load_mw_by_hour: Vec<Vec<f64>>,
    /// Per-period total load in MW. Precomputed for the loss-share
    /// weighted distribution.
    pub total_load_by_hour: Vec<f64>,
    /// Per-period loss-PTDF rows. Needed by
    /// [`surge_opf::advanced::compute_dc_loss_sensitivities`] when the
    /// refinement iteration recomputes `dloss_dp` from solved angles.
    pub loss_ptdf_by_hour: Vec<surge_dc::PtdfRows>,
    /// Number of bus-balance rows per period; matches `ScucBoundsInput::n_bus`.
    pub n_bus: usize,
    /// Number of periods on the horizon.
    pub n_hours: usize,
}

/// Loss-factor warm-start — the marginal sensitivities and the implied
/// total system losses per period. All warm-start sources (prior
/// security iteration cache, DC PF on rough dispatch, load-pattern
/// approximation, uniform loss rate) produce this shape.
///
/// Sized as `dloss_dp.len() == total_losses_mw.len() == n_hours`, each
/// inner dloss vector `== n_bus` entries.
#[derive(Debug, Clone, Default)]
pub(crate) struct LossFactorWarmStart {
    /// `[t][bus_idx]`: marginal `∂losses/∂P_inj` at each bus in each
    /// period.
    pub dloss_dp: Vec<Vec<f64>>,
    /// `[t]`: estimated total transmission losses in MW at each period.
    pub total_losses_mw: Vec<f64>,
}

impl LossFactorWarmStart {
    /// Whether this warm-start has populated data (both `dloss_dp` and
    /// `total_losses_mw` non-empty and consistent).
    pub fn is_populated(&self) -> bool {
        !self.dloss_dp.is_empty() && self.dloss_dp.len() == self.total_losses_mw.len()
    }
}

/// Result of the loss-factor iteration: marginal sensitivities for LMP
/// decomposition, plus the MW loss allocation per bus per period for the
/// bus-balance result.
pub(super) struct LossFactorResult {
    /// `dloss_dp[t][bus_idx]`: marginal loss sensitivity at each bus.
    pub dloss_dp: Vec<Vec<f64>>,
    /// `loss_allocation_mw[t][bus_idx]`: MW of transmission losses allocated
    /// to each bus, proportional to bus load. Always non-negative.
    /// `sum(loss_allocation_mw[t]) ≈ total_system_losses_mw[t]`.
    pub loss_allocation_mw: Vec<Vec<f64>>,
}

/// Walk a single column's CSC entries and append every entry that
/// lands inside the bus-balance row range to `out`.
fn collect_bus_injection_coeffs(
    problem: &SparseProblem,
    out: &mut Vec<BusInjectionCoeff>,
    col: usize,
    pb_start: usize,
    n_bus: usize,
) {
    if col >= problem.n_col {
        return;
    }
    let col_start = problem.a_start[col] as usize;
    let col_end = problem.a_start[col + 1] as usize;
    for pos in col_start..col_end {
        let row = problem.a_index[pos] as usize;
        if row >= pb_start && row < pb_start + n_bus {
            let bus_idx = row - pb_start;
            let original = problem.a_value[pos];
            out.push(BusInjectionCoeff {
                pos,
                bus_idx,
                original,
            });
        }
    }
}

/// Build the immutable loss-factor setup from the pre-solve problem.
///
/// Walks the column data to capture every physical injection/withdrawal
/// coefficient in the bus-balance rows, precomputes per-period loads,
/// and constructs the loss-PTDF rows needed by the refinement loop.
///
/// Runs once per SCUC solve. The returned prep is passed unchanged to
/// every subsequent [`apply_bus_loss_factors`] and
/// [`iterate_loss_factors`] call on the same problem.
///
/// Coefficient kinds captured (matching the existing model — unchanged
/// by this refactor):
///
/// * Generator Pg coefficients (`-1` or `-(1 - α)` depending on
///   AC/block encoding)
/// * Storage charge/discharge coefficients on the storage bus
/// * HVDC band-dispatch coefficients (both from-bus and to-bus
///   terminals, banded or legacy layouts)
/// * Dispatchable load coefficients
/// * Virtual bid (INC/DEC) coefficients
///
/// Power-balance penalty slacks (`pb_curtailment_bus`, `pb_excess_bus`)
/// and DR rebound injections are deliberately NOT captured — the
/// slacks must absorb a full pu of imbalance per pu of slack (so
/// multiplying them by `(1 - dloss)` would cut the slack's effective
/// gain below one and leave the row un-clearable), and DR rebound is a
/// small effect the existing code does not pin to a single `bus_idx`.
pub(super) fn build_loss_factor_prep(
    problem: &SparseProblem,
    hourly_networks: &[Network],
    bus_map: &HashMap<u32, usize>,
    layout: &ScucLayout,
    gen_bus_idx: &[usize],
    hour_row_bases: &[usize],
    n_flow: usize,
    n_bus: usize,
) -> Result<LossFactorPrep, ScedError> {
    let n_hours = hourly_networks.len();

    let mut injections_by_hour: Vec<Vec<BusInjectionCoeff>> = Vec::with_capacity(n_hours);
    let mut orig_rhs_by_hour: Vec<Vec<f64>> = Vec::with_capacity(n_hours);
    let mut bus_load_mw_by_hour: Vec<Vec<f64>> = Vec::with_capacity(n_hours);
    let mut total_load_by_hour: Vec<f64> = Vec::with_capacity(n_hours);
    let mut loss_ptdf_by_hour = Vec::with_capacity(n_hours);

    let dispatch = &layout.dispatch;
    let n_gen = gen_bus_idx.len();
    let n_storage = dispatch.sto_dis - dispatch.sto_ch;
    let n_hvdc_vars = dispatch.e_g - dispatch.hvdc;
    let n_dl = dispatch.vbid - dispatch.dl;
    let n_vbid = dispatch.block - dispatch.vbid;

    for (t, network_t) in hourly_networks.iter().enumerate() {
        let pb_start = hour_row_bases[t] + n_flow;
        orig_rhs_by_hour.push(
            (0..n_bus)
                .map(|i| problem.row_lower[pb_start + i])
                .collect(),
        );

        let mut bus_load_mw = vec![0.0_f64; n_bus];
        for load in &network_t.loads {
            if load.in_service
                && let Some(&bus_idx) = bus_map.get(&load.bus)
            {
                bus_load_mw[bus_idx] += load.active_power_demand_mw;
            }
        }
        total_load_by_hour.push(bus_load_mw.iter().map(|v| v.max(0.0)).sum());
        bus_load_mw_by_hour.push(bus_load_mw);

        let monitored_branches: Vec<usize> = (0..network_t.n_branches()).collect();
        loss_ptdf_by_hour.push(
            surge_dc::compute_ptdf(
                network_t,
                &surge_dc::PtdfRequest::for_branches(&monitored_branches),
            )
            .map_err(|e| ScedError::SolverError(format!("PTDF for SCUC loss factors: {e}")))?,
        );

        // Allocate roughly enough room for every physical bus
        // injection / withdrawal column at this period.
        let mut hour_injections: Vec<BusInjectionCoeff> =
            Vec::with_capacity(n_gen + 2 * n_storage + 2 * n_hvdc_vars + n_dl + n_vbid);

        // Generators contribute Pg with coefficient -1 at the gen bus.
        for j in 0..n_gen {
            collect_bus_injection_coeffs(
                problem,
                &mut hour_injections,
                layout.pg_col(t, j),
                pb_start,
                n_bus,
            );
        }

        // Storage charge/discharge each touch the storage bus.
        for s in 0..n_storage {
            collect_bus_injection_coeffs(
                problem,
                &mut hour_injections,
                layout.storage_charge_col(t, s),
                pb_start,
                n_bus,
            );
            collect_bus_injection_coeffs(
                problem,
                &mut hour_injections,
                layout.storage_discharge_col(t, s),
                pb_start,
                n_bus,
            );
        }

        // HVDC link variables touch BOTH the from-bus and the to-bus
        // (banded layouts have one column per band; the helper picks
        // up every entry the column writes into the bus-balance row
        // range, regardless of how many terminals it touches).
        for k in 0..n_hvdc_vars {
            collect_bus_injection_coeffs(
                problem,
                &mut hour_injections,
                layout.col(t, dispatch.hvdc + k),
                pb_start,
                n_bus,
            );
        }

        // Dispatchable loads (DR) consume at one bus.
        for k in 0..n_dl {
            collect_bus_injection_coeffs(
                problem,
                &mut hour_injections,
                layout.col(t, dispatch.dl + k),
                pb_start,
                n_bus,
            );
        }

        // Virtual bids (INC/DEC) inject or withdraw at one bus.
        for k in 0..n_vbid {
            collect_bus_injection_coeffs(
                problem,
                &mut hour_injections,
                layout.col(t, dispatch.vbid + k),
                pb_start,
                n_bus,
            );
        }

        injections_by_hour.push(hour_injections);
    }

    Ok(LossFactorPrep {
        injections_by_hour,
        orig_rhs_by_hour,
        bus_load_mw_by_hour,
        total_load_by_hour,
        loss_ptdf_by_hour,
        n_bus,
        n_hours,
    })
}

/// Apply a given `(dloss_dp, total_losses_pu)` estimate to the problem.
///
/// Mutates both the sparse A-matrix entries captured in
/// `prep.injections_by_hour` (scaling each negative injection
/// coefficient by `(1 - dloss[bus_idx])`, clamped to `[0.5, 1.0]`) and
/// the per-bus balance RHS (subtracting `loss_share[i] =
/// total_losses_pu * (bus_load[i] / total_load)` from the captured
/// `orig_rhs`).
///
/// Units: `total_losses_pu` is in per-unit (loss / base_mva), matching
/// the PU encoding of the bus-balance RHS. Callers holding losses in
/// MW (e.g. a [`LossFactorWarmStart`]) divide by `network.base_mva`
/// before handing off. The bus-load weighting in `prep` cancels units
/// so no additional conversion is needed there.
///
/// Safe to call on a fresh problem as a pre-MIP warm start, or inside
/// the refinement loop after a solve. Each call starts from
/// `prep.orig_rhs_by_hour` for the RHS reset, so repeated calls do not
/// accumulate stale corrections.
///
/// # Panics
///
/// Panics in debug builds if `dloss_dp.len() != prep.n_hours`,
/// `total_losses_pu.len() != prep.n_hours`, or any inner dloss vector
/// length mismatches `prep.n_bus`.
pub(super) fn apply_bus_loss_factors(
    problem: &mut SparseProblem,
    prep: &LossFactorPrep,
    hour_row_bases: &[usize],
    n_flow: usize,
    dloss_dp: &[Vec<f64>],
    total_losses_pu_by_hour: &[f64],
) {
    debug_assert_eq!(dloss_dp.len(), prep.n_hours);
    debug_assert_eq!(total_losses_pu_by_hour.len(), prep.n_hours);

    for t in 0..prep.n_hours {
        debug_assert_eq!(dloss_dp[t].len(), prep.n_bus);

        // Apply the per-period bus penalty factor *only* to
        // injections (negative original coefficients), not to
        // withdrawals (positive original coefficients).
        //
        // The DC loss factor model says each MW injected at bus
        // i delivers only `(1 - dloss[i])` MW of useful power
        // because the rest is dissipated as line losses. That
        // multiplier belongs on the **injection** side of the
        // bus balance equation. Withdrawals (loads, storage
        // charge, HVDC from-bus terminal, DEC virtual bids) are
        // modelled at face value: 1 MW of demand is 1 MW of
        // withdrawal regardless of losses, and the extra
        // generation needed to cover the resulting losses is
        // already accounted for via the loss share added to the
        // RHS plus the (1 - dloss) factor applied to the
        // generators that provide that extra generation. Multiplying
        // withdrawals by `pf` would double-count losses on the
        // load side and produce a P-balanced LP whose AC reconcile
        // pass cannot follow at the gen Q boundaries (because it
        // shifts gens to a P-pattern that has no Q headroom).
        //
        // Negative original coefficients in the bus balance row:
        //   -1                 generator Pg
        //   -(1 - loss_b_frac) HVDC to-bus terminal
        //   -1 / -1/base       storage discharge
        //   -1                 INC virtual bid
        //
        // Positive original coefficients (left untouched):
        //   +1                 dispatchable load
        //   +1 / +1/base       storage charge
        //   +1                 HVDC from-bus terminal
        //   +1                 DEC virtual bid
        for term in &prep.injections_by_hour[t] {
            if term.original >= 0.0 {
                continue;
            }
            // Cutoff: treat `|dloss| < DLOSS_MIN` as no-loss to avoid
            // micro-perturbations (e.g. `pf = 0.99999`) that don't
            // meaningfully change the LP but do disturb the warm-basis
            // and force extra simplex work. Buses with tiny loss
            // sensitivity are kept at the original coefficient.
            //
            // Test on `.abs()`: refinement-computed `dloss_dp` from
            // `compute_dc_loss_sensitivities` routinely produces small
            // negative values on downstream buses (injection there
            // *saves* loss). Those are physically real and we want to
            // apply them, so the cutoff must be magnitude-based, not
            // signed.
            //
            // Clamp pf to [0.5, 1.10]: the lower cap stops pf from
            // reaching zero on degenerate large-positive dloss. The
            // upper cap of 1.10 lets loss-saving buses (negative
            // `dloss`) credit up to 10% — enough to meaningfully
            // influence the MIP's dispatch toward loss-saving gens,
            // but bounded tight enough that a runaway negative dloss
            // won't produce the `sum(gen) < sum(load)` imbalance seen
            // with the previous uncapped / 1.5-cap versions.
            let dloss = dloss_dp[t][term.bus_idx];
            let pf = if dloss.abs() < DLOSS_MIN {
                1.0
            } else {
                (1.0 - dloss).clamp(0.5, 1.10)
            };
            problem.a_value[term.pos] = term.original * pf;
        }

        let pb_start = hour_row_bases[t] + n_flow;
        let total_load = prep.total_load_by_hour[t];
        let total_losses_pu = total_losses_pu_by_hour[t];
        for (i, &orig_rhs) in prep.orig_rhs_by_hour[t].iter().enumerate() {
            let loss_share = if total_load > 1e-6 {
                total_losses_pu * (prep.bus_load_mw_by_hour[t][i].max(0.0) / total_load)
            } else {
                total_losses_pu / prep.n_bus as f64
            };
            let rhs = orig_rhs - loss_share;
            problem.row_lower[pb_start + i] = rhs;
            problem.row_upper[pb_start + i] = rhs;
        }
    }
}

/// Distribute the per-period total-system losses to each bus
/// proportional to bus load. Input and output are both in MW (unlike
/// [`apply_bus_loss_factors`] which uses PU internally to match the
/// bus-balance RHS encoding).
///
/// Pure helper, no side effects.
#[allow(dead_code)]
pub(super) fn allocate_bus_losses_mw(
    prep: &LossFactorPrep,
    total_losses_mw: &[f64],
) -> Vec<Vec<f64>> {
    let mut out = vec![vec![0.0; prep.n_bus]; prep.n_hours];
    for t in 0..prep.n_hours {
        let total_load = prep.total_load_by_hour[t];
        let total_losses = total_losses_mw[t];
        for i in 0..prep.n_bus {
            out[t][i] = if total_load > 1e-6 {
                total_losses * (prep.bus_load_mw_by_hour[t][i].max(0.0) / total_load)
            } else {
                total_losses / prep.n_bus as f64
            };
        }
    }
    out
}

/// Read the solved theta vector for each period from an LP solution.
///
/// Convenience helper used by the security-loop warm-start cache when
/// the final-iteration dispatch gets rolled into the next
/// iteration's [`LossFactorWarmStart`].
pub(super) fn extract_theta_by_hour(
    solution: &LpResult,
    layout: &ScucLayout,
    n_hours: usize,
    n_bus: usize,
) -> Vec<Vec<f64>> {
    (0..n_hours)
        .map(|t| {
            (0..n_bus)
                .map(|bus_idx| solution.x[layout.theta_col(t, bus_idx)])
                .collect()
        })
        .collect()
}

/// Compute the total transmission losses in MW for each period, given
/// a precomputed theta vector per period.
///
/// Pure DC-losses-from-angles — wraps
/// [`surge_opf::compute_total_dc_losses`]. Results are in MW at the
/// network's base MVA.
pub(super) fn compute_total_losses_mw_from_theta(
    hourly_networks: &[Network],
    bus_map: &HashMap<u32, usize>,
    theta_by_hour: &[Vec<f64>],
) -> Vec<f64> {
    hourly_networks
        .iter()
        .zip(theta_by_hour.iter())
        .map(|(network_t, theta)| {
            let pu = surge_opf::compute_total_dc_losses(network_t, theta, bus_map);
            pu * network_t.base_mva
        })
        .collect()
}

pub(super) fn iterate_loss_factors(
    input: ScucLossIterationInput<'_>,
    prep: &LossFactorPrep,
    initial_dloss: Option<&[Vec<f64>]>,
) -> Result<LossFactorResult, ScedError> {
    let n_hours = input.hourly_networks.len();
    let mut dloss_dp_out = vec![vec![0.0; input.n_bus]; n_hours];

    if !input.spec.use_loss_factors
        || input.n_bus <= 1
        || !matches!(
            input.solution.status,
            LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
        )
    {
        return Ok(LossFactorResult {
            dloss_dp: dloss_dp_out,
            loss_allocation_mw: vec![vec![0.0; input.n_bus]; n_hours],
        });
    }

    // Zero-iteration mode: the caller wants to use the warm-start
    // estimate as the final answer without any refinement re-solve.
    // This is the "warm start IS the result" workflow — useful when
    // (a) the warm-start is known-good (e.g. from a prior solve on
    // an identical-commitment problem), or (b) the caller explicitly
    // wants to measure the warm-start's quality in isolation.
    //
    // With `max_loss_factor_iters == 0` the main loop below never
    // runs, so `dloss_dp_out` would otherwise stay at its zero init.
    // When an initial estimate is available we propagate it out so
    // LMP decomposition and `loss_allocation_mw` still reflect the
    // applied warm-start. Without an initial estimate we return
    // zeros (the conservative "we don't have any loss info" path).
    if input.spec.max_loss_factor_iters == 0 {
        if let Some(seed) = initial_dloss {
            for (t, row) in seed.iter().enumerate().take(n_hours) {
                if row.len() >= input.n_bus {
                    dloss_dp_out[t].copy_from_slice(&row[..input.n_bus]);
                }
            }
        }
        // Recompute loss_allocation_mw from the solved theta (which
        // already reflects whatever warm-start the caller applied to
        // the problem). Pure read + arithmetic; no solver calls.
        let base_mva = input.hourly_networks.first().map_or(100.0, |n| n.base_mva);
        let mut loss_allocation_mw = vec![vec![0.0; input.n_bus]; n_hours];
        for (t, network_t) in input.hourly_networks.iter().enumerate() {
            let theta: Vec<f64> = (0..input.n_bus)
                .map(|bus_idx| input.solution.x[input.layout.theta_col(t, bus_idx)])
                .collect();
            let total_losses_pu =
                surge_opf::compute_total_dc_losses(network_t, &theta, input.bus_map);
            let total_losses_mw = total_losses_pu * base_mva;
            let total_load = prep.total_load_by_hour[t];
            for i in 0..input.n_bus {
                loss_allocation_mw[t][i] = if total_load > 1e-6 {
                    total_losses_mw * (prep.bus_load_mw_by_hour[t][i].max(0.0) / total_load)
                } else {
                    total_losses_mw / input.n_bus as f64
                };
            }
        }
        debug!(
            seeded = initial_dloss.is_some(),
            "SCUC loss factors: max_iter=0, returning warm-start state directly"
        );
        return Ok(LossFactorResult {
            dloss_dp: dloss_dp_out,
            loss_allocation_mw,
        });
    }

    // Seed `prev_dloss` with the caller-supplied warm-start dloss when
    // one is available. Otherwise start from zero (the historic
    // behaviour, i.e. the first iter's convergence test is unreachable
    // — the loop always re-solves at iter 0 because convergence
    // detection is gated on `loss_iter > 0`). With a warm-start, if the
    // pre-MIP application already seeded the problem with the same
    // dloss AND the MIP solved to the same dispatch, iter 0's newly
    // computed dloss will match prev_dloss within tolerance and the
    // `loss_iter > 0` gate in the next iteration will fire early,
    // saving a full LP re-solve.
    let mut prev_dloss = match initial_dloss {
        Some(seed) => seed.to_vec(),
        None => vec![vec![0.0_f64; input.n_bus]; n_hours],
    };

    let mut converged = false;
    for loss_iter in 0..input.spec.max_loss_factor_iters {
        let mut total_losses_by_hour = vec![0.0_f64; n_hours];
        let mut max_delta = 0.0_f64;

        for (t, network_t) in input.hourly_networks.iter().enumerate() {
            let theta: Vec<f64> = (0..input.n_bus)
                .map(|bus_idx| input.solution.x[input.layout.theta_col(t, bus_idx)])
                .collect();
            let dloss = surge_opf::advanced::compute_dc_loss_sensitivities(
                network_t,
                &theta,
                input.bus_map,
                &prep.loss_ptdf_by_hour[t],
            );
            max_delta = max_delta.max(
                dloss
                    .iter()
                    .zip(prev_dloss[t].iter())
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0_f64, f64::max),
            );
            // Total losses are carried in PU here because the bus-balance
            // RHS (`prep.orig_rhs_by_hour`) is also in PU and the
            // `loss_share` subtraction below mixes a PU scalar with a
            // unitless bus-load-ratio weight. The PU→MW conversion
            // happens only when building the public
            // `loss_allocation_mw` output at the bottom of this fn,
            // and when populating a [`LossFactorWarmStart`] for
            // warm-start plumbing (total_losses_mw is in MW there).
            total_losses_by_hour[t] =
                surge_opf::compute_total_dc_losses(network_t, &theta, input.bus_map);
            dloss_dp_out[t] = dloss;
        }

        // Detect convergence but do NOT exit before doing the final
        // update + re-solve. Without this, the returned LP solution
        // would have been solved with the *previous* iteration's
        // coefficients while we report `dloss_dp_out` from the new
        // theta — i.e. the dispatch and the loss factors we hand back
        // to the caller are from different LP states. The
        // discrepancy is bounded by `loss_factor_tol` but it still
        // means the bus balance row in `input.problem` does not
        // match what `input.solution.x` was actually optimized
        // against, which downstream LMP/dispatch consumers should
        // not have to reason about.
        //
        // With a warm-start (initial_dloss seeded both prev_dloss and
        // the pre-MIP problem), iter 0's max_delta IS meaningful —
        // the condition below fires at iter 0 when the lossless MIP
        // would have produced the same dispatch (rare but possible on
        // well-warm-started problems). Otherwise fires at iter ≥ 1
        // once consecutive iterations agree.
        let convergence_gate_active = loss_iter > 0 || initial_dloss.is_some();
        if convergence_gate_active && max_delta < input.spec.loss_factor_tol {
            debug!(
                iter = loss_iter,
                max_delta,
                seeded = initial_dloss.is_some(),
                "SCUC loss factors converged"
            );
            converged = true;
        }
        prev_dloss.clone_from(&dloss_dp_out);

        // Apply the newly computed dloss + total_losses to the problem
        // and re-solve. (Converged runs still re-solve so the returned
        // LP dispatch and the reported dloss are from the same LP
        // state — see the comment block above.)
        apply_bus_loss_factors(
            input.problem,
            prep,
            input.hour_row_bases,
            input.n_flow,
            &dloss_dp_out,
            &total_losses_by_hour,
        );

        // Loss-factor re-solve: warm-start from the prior dispatch so
        // Gurobi's Start attribute seeds the MIP with a feasible
        // primal — this lets the B&B tree close immediately because
        // the commitment is unchanged, only the bus-balance RHS
        // shifted by the loss allocation. We keep the integrality
        // array intact so the solver still enforces the commitment
        // binaries (a previous attempt to drop integrality here
        // produced fractional dispatches that violated bus balance
        // post-solve — see scenario 73-D1 303).
        let warm_start = Some(LpPrimalStart::Dense(input.solution.x.clone()));
        let _loss_lp_t0 = std::time::Instant::now();
        *input.solution = solve_sparse_problem_with_start(
            input.solver,
            input.problem,
            input.spec.tolerance,
            input.time_limit_secs,
            input.spec.mip_rel_gap(),
            warm_start,
        )?;
        info!(
            stage = "iterate_loss_factors.lp_resolve",
            secs = _loss_lp_t0.elapsed().as_secs_f64(),
            iterations = input.solution.iterations,
            "SCUC helper solve timing",
        );
        if !matches!(
            input.solution.status,
            LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
        ) {
            warn!(
                iter = loss_iter,
                status = ?input.solution.status,
                "SCUC loss factor iteration: solver did not converge"
            );
            break;
        }

        if converged {
            break;
        }
    }

    // Build per-bus MW loss allocation from the final iteration's losses.
    // Recompute total losses from the final solution's theta.
    let base_mva = input.hourly_networks.first().map_or(100.0, |n| n.base_mva);
    let mut loss_allocation_mw = vec![vec![0.0; input.n_bus]; n_hours];
    for (t, network_t) in input.hourly_networks.iter().enumerate() {
        let theta: Vec<f64> = (0..input.n_bus)
            .map(|bus_idx| input.solution.x[input.layout.theta_col(t, bus_idx)])
            .collect();
        let total_losses_pu = surge_opf::compute_total_dc_losses(network_t, &theta, input.bus_map);
        let total_losses_mw = total_losses_pu * base_mva;
        let total_load = prep.total_load_by_hour[t];
        for i in 0..input.n_bus {
            loss_allocation_mw[t][i] = if total_load > 1e-6 {
                total_losses_mw * (prep.bus_load_mw_by_hour[t][i].max(0.0) / total_load)
            } else {
                total_losses_mw / input.n_bus as f64
            };
        }
    }

    Ok(LossFactorResult {
        dloss_dp: dloss_dp_out,
        loss_allocation_mw,
    })
}

// ─────────────────────────────────────────────────────────────────────
// Cold-start warm-start sources
//
// Used on the FIRST security iteration (before any prior
// `LossFactorWarmStart` is available) to seed the SCUC MIP with a
// loss-factor estimate instead of the lossless default. The three
// sources trade off accuracy and cost:
//
// * [`build_uniform_loss_warm_start`] — uniform `dloss = rate` on every
//   bus, `total_losses_mw = rate × total_load_mw`. No per-bus
//   variation; crude but effectively free. Use as a bottom-line
//   baseline or when the network has no computable sensitivity
//   (degenerate topology).
//
// * [`build_load_pattern_loss_warm_start`] — Option #4: PTDF-weighted
//   loss sensitivity from the load vector alone, without a dispatch
//   guess. Uses the network's loss-PTDF × load pattern to give buses
//   near load centers lower `dloss` (closer to 0) and buses far from
//   load higher `dloss` (closer to the system-average loss rate). No
//   DC PF; ~O(n_bus × n_branch) per period.
//
// * [`build_dc_pf_loss_warm_start`] — Option #3: run a DC PF on each
//   hourly network using whatever gen setpoints it carries (initial
//   condition / network-spec dispatch), take the solved theta, and
//   derive `dloss_dp` from [`surge_opf::advanced::compute_dc_loss_sensitivities`].
//   Most accurate cold-start but ~O(DC PF cost) per period (ms-level
//   on 617-bus).
//
// All three produce a [`LossFactorWarmStart`] that plugs straight into
// [`ScucProblemInput::initial_loss_warm_start`].
// ─────────────────────────────────────────────────────────────────────

/// Uniform-loss-rate warm start: every bus gets the same `dloss`, and
/// `total_losses_mw` equals `rate × total_load_mw` per period.
///
/// `rate` is the expected transmission loss fraction (typical 0.01–
/// 0.03). `total_load_mw[t]` must match the per-period load sum; the
/// caller usually already has this from
/// [`LossFactorPrep::total_load_by_hour`] or equivalent.
///
/// Cheapest cold-start — no PTDF work, no DC PF. Use as a sanity
/// baseline or when per-bus variation isn't worth the extra
/// computation.
pub(crate) fn build_uniform_loss_warm_start(
    n_bus: usize,
    total_load_mw: &[f64],
    rate: f64,
) -> LossFactorWarmStart {
    let clamped = rate.clamp(0.0, 0.5);
    let n_hours = total_load_mw.len();
    LossFactorWarmStart {
        dloss_dp: vec![vec![clamped; n_bus]; n_hours],
        total_losses_mw: total_load_mw.iter().map(|l| clamped * l).collect(),
    }
}

/// Load-pattern warm start (Option #4): per-period marginal
/// `dloss_dp[t, i] ≈ ∂losses/∂P_inj[i]` derived from the DC-loss
/// sensitivity formula applied to the **per-period** bus load pattern.
/// No dispatch guess, no DC PF.
///
/// DC-loss formula: `losses = Σ_l r_l × flow_l²`. Differentiating wrt
/// injection at bus `i` and using `flow_l = Σ_j PTDF[l,j] × P_inj[j]`:
///
/// ```text
///   ∂losses/∂P_inj[i] = 2 × Σ_l r_l × flow_l × PTDF[l,i]
/// ```
///
/// To evaluate this without a dispatch guess we approximate the
/// operating flows by treating the load pattern as the injection
/// vector (gen implicitly absorbed at the slack):
///
/// ```text
///   flow_l ≈ -Σ_j PTDF[l,j] × load_mw[j] / base_mva
/// ```
///
/// Then `|dloss/dP_inj[i]|` ranks buses by how much their injection
/// drives net branch flow through resistive lines. Buses on the
/// generation side of the slack-to-load flow axis get large
/// sensitivities; buses near loads get small ones (injecting at a
/// load bus locally reduces flow). Magnitudes are calibrated so the
/// max per-period `dloss` hits `2 × rate` (mirrors the observation
/// that gen buses far from load typically see ~ 2× the mean loss
/// fraction).
///
/// O(n_branch × n_ptdf_col) per period; no DC PF invocation. About
/// 5× the work of the pure topology proxy (previous implementation)
/// but the resulting `dloss` actually tracks the per-period load
/// pattern — if the load concentrates at a different set of buses in
/// a given hour, the sensitivity pattern shifts with it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_load_pattern_loss_warm_start(
    hourly_networks: &[Network],
    _bus_map: &HashMap<u32, usize>,
    loss_ptdf_by_hour: &[surge_dc::PtdfRows],
    bus_load_mw_by_hour: &[Vec<f64>],
    total_load_by_hour: &[f64],
    n_bus: usize,
    rate: f64,
) -> LossFactorWarmStart {
    let n_hours = hourly_networks.len();
    let clamped_rate = rate.clamp(0.0, 0.5);

    let mut dloss_dp = vec![vec![0.0_f64; n_bus]; n_hours];
    let mut total_losses_mw = vec![0.0_f64; n_hours];

    for (t, network_t) in hourly_networks.iter().enumerate() {
        let total_load = total_load_by_hour[t];
        if total_load <= 1e-6 {
            continue;
        }
        let loss_ptdf = &loss_ptdf_by_hour[t];
        let bus_load = &bus_load_mw_by_hour[t];
        let base_mva = network_t.base_mva.max(1.0);

        // Build a dense per-ptdf-column injection vector from the load
        // pattern: `inj_pu[col_pos] = -load_mw[bus_indices[col_pos]] /
        // base_mva`. Convention: positive P_inj = generation; loads
        // enter as negative injections (slack absorbs the balance).
        let bus_indices = loss_ptdf.bus_indices();
        let n_col = bus_indices.len();
        let mut inj_pu = vec![0.0_f64; n_col];
        for (col_pos, &bus_idx) in bus_indices.iter().enumerate() {
            if bus_idx < bus_load.len() {
                inj_pu[col_pos] = -bus_load[bus_idx] / base_mva;
            }
        }

        // First pass: approximate per-branch flow in pu from the
        // load-pattern injection: flow_l = Σ_col PTDF[l, col] ×
        // inj_pu[col]. Keep monitored branches that are in service;
        // others contribute zero flow (they've been derated out).
        let mut raw = vec![0.0_f64; n_bus];
        for branch_idx in 0..network_t.n_branches() {
            let branch = &network_t.branches[branch_idx];
            if !branch.in_service {
                continue;
            }
            let Some(row) = loss_ptdf.row(branch_idx) else {
                continue;
            };
            // flow_l in pu, signed — sign conventions are PTDF-
            // dependent but the magnitude is what matters for loss.
            let mut flow_pu = 0.0_f64;
            for col_pos in 0..n_col {
                if let Some(&coeff) = row.get(col_pos) {
                    flow_pu += coeff * inj_pu[col_pos];
                }
            }

            // Coefficient of dloss/dP_inj[i] contributed by this
            // branch: `2 × r_l × flow_l × PTDF[l, i]`. `r_l` is the
            // per-unit resistance; `flow_pu` the approximate flow.
            // The factor of 2 is absorbed by the later max-normalise
            // so we can drop it and keep the scale implicit.
            let r_pu = branch.r;
            if r_pu <= 0.0 || flow_pu.abs() < 1e-12 {
                continue;
            }
            let weight = r_pu * flow_pu;
            for (col_pos, &bus_idx) in bus_indices.iter().enumerate() {
                if let Some(&coeff) = row.get(col_pos)
                    && bus_idx < n_bus
                {
                    raw[bus_idx] += weight * coeff;
                }
            }
        }

        // Take magnitudes — the LP's loss term is `Σ_i dloss[i] ×
        // P_inj[i]`, and the per-bus signs follow from slack choice.
        // For a warm-start we want positive penalties on gens and
        // (implicit) negative multipliers on loads; both fall out of
        // |raw|.
        for v in raw.iter_mut() {
            *v = v.abs();
        }

        let max_raw: f64 = raw.iter().copied().fold(0.0_f64, f64::max);
        if max_raw <= 1e-12 {
            // Degenerate (no in-service branches? load = 0?) —
            // fall back to uniform rate.
            for i in 0..n_bus {
                dloss_dp[t][i] = clamped_rate;
            }
        } else {
            // Scale so max dloss in the period hits `2 × rate`. This
            // matches the empirical pattern that the lossy-most gen
            // buses run around twice the system-average loss rate;
            // smaller-sensitivity buses scale down proportionally.
            let scale = (2.0 * clamped_rate) / max_raw;
            for i in 0..n_bus {
                dloss_dp[t][i] = (raw[i] * scale).clamp(0.0, 0.5);
            }
        }
        total_losses_mw[t] = clamped_rate * total_load;
    }

    LossFactorWarmStart {
        dloss_dp,
        total_losses_mw,
    }
}

/// DC-PF warm start (Option #3): run a DC power flow on each hourly
/// network using whatever generator setpoints the network carries,
/// then derive `dloss_dp` from the solved theta and `total_losses_mw`
/// from `compute_total_dc_losses × base_mva`.
///
/// The hourly networks carry the initial-condition dispatch
/// (load-proportional or from-spec setpoints per hour), so the DC PF
/// answers "if everyone were dispatched at their current setpoint,
/// what angle pattern + losses would result?" Accuracy: typically
/// within ~10% of the SCUC-converged loss pattern on stable networks;
/// worse on heavily constrained cases where the optimal dispatch
/// differs substantially from the initial one. Still a much better
/// starting point than `dloss = 0`.
///
/// Cost: one DC PF per period (single KLU solve on a pre-factorised
/// B', so ~1 ms on 617-bus). Dwarfs the [`build_load_pattern_*`]
/// variant in accuracy but costs 2–3 ms per period instead of sub-ms.
/// Negligible relative to the MIP wall it saves.
///
/// Falls back to `build_uniform_loss_warm_start(0.02)` on DC PF
/// failure (island / slack issues / singular B'), so the caller can
/// always treat this as "best-effort warm start".
/// Clone `network` and set each in-service generator's
/// `active_power_mw` to a load-proportional share of its rated
/// capacity, so that `Σ gen_active ≈ Σ load_active`. Returns the
/// balanced clone.
///
/// Used by [`build_dc_pf_loss_warm_start`] to seed a physically
/// meaningful injection vector before the DC PF solve. Without this,
/// the hourly network may carry the scenario's initial-condition
/// setpoints (often zero on SCUC pre-solve networks), leaving the DC
/// PF slack bus to absorb the full load as imbalance — angle swings
/// then scale with that slack injection and produce grossly inflated
/// DC-loss estimates (`flow² × r` terms blow up).
///
/// Uses pmax as the capacity proxy: each in-service generator is
/// dispatched at `pmax × (total_load / total_pmax_in_service)`. This
/// gives every unit a share of load proportional to its size, which
/// matches the steady-state pattern the network's transmission
/// topology is designed around. Generators with pmax = 0 or out of
/// service are left alone.
///
/// Pure clone + rewrite; no solver calls. O(n_gen + n_load).
fn balance_dispatch_to_load(network: &Network) -> Network {
    let total_load_mw: f64 = network
        .loads
        .iter()
        .filter(|l| l.in_service)
        .map(|l| l.active_power_demand_mw.max(0.0))
        .sum();
    let total_pmax_mw: f64 = network
        .generators
        .iter()
        .filter(|g| g.in_service)
        .map(|g| g.pmax.max(0.0))
        .sum();
    let mut out = network.clone();
    if total_load_mw > 1e-6 && total_pmax_mw > 1e-6 {
        let ratio = total_load_mw / total_pmax_mw;
        for g in out.generators.iter_mut() {
            if g.in_service {
                g.p = g.pmax.max(0.0) * ratio;
            }
        }
    }
    out
}

pub(crate) fn build_dc_pf_loss_warm_start(
    hourly_networks: &[Network],
    bus_map: &HashMap<u32, usize>,
    loss_ptdf_by_hour: &[surge_dc::PtdfRows],
    bus_load_mw_by_hour: &[Vec<f64>],
    total_load_by_hour: &[f64],
    n_bus: usize,
) -> LossFactorWarmStart {
    const FALLBACK_RATE: f64 = 0.02;
    let n_hours = hourly_networks.len();

    let mut dloss_dp = vec![vec![0.0_f64; n_bus]; n_hours];
    let mut total_losses_mw = vec![0.0_f64; n_hours];

    for (t, network_t) in hourly_networks.iter().enumerate() {
        // Balance the generator dispatch to match load BEFORE running
        // DC PF — otherwise the hourly network's arbitrary (often
        // zero) gen setpoints force the slack bus to absorb the full
        // load mismatch, producing wild angle swings and artificially
        // inflated DC-loss estimates (observed on 617-bus D2 #2:
        // ~1,612 MW losses per period = ~40% of load = clearly wrong).
        //
        // Approach: clone the network, set each in-service generator's
        // active_power_mw to a load-proportional share based on its
        // pmax. This gives a physically meaningful dispatch that a DC
        // PF can solve without large slack corrections, producing
        // angle patterns that track the network's natural flow
        // topology.
        let balanced = balance_dispatch_to_load(network_t);
        let solution = surge_dc::solve_dc(&balanced);
        match solution {
            Ok(pf) => {
                let dloss = surge_opf::advanced::compute_dc_loss_sensitivities(
                    &balanced,
                    &pf.theta,
                    bus_map,
                    &loss_ptdf_by_hour[t],
                );
                let losses_pu = surge_opf::compute_total_dc_losses(&balanced, &pf.theta, bus_map);
                let losses_mw = losses_pu.max(0.0) * balanced.base_mva;
                // Sanity cap: if the DC PF produces losses > 5% of
                // total load, something is off (commitment state only
                // has a subset of gens in-service, PST phase data is
                // degenerate, DC-grid injections are large, etc.) —
                // fall back to uniform rather than pass a wildly
                // over-estimated warm start to the MIP. Real DC losses
                // on transmission networks run 1-3% of load.
                let load_t = total_load_by_hour[t];
                let loss_ratio = if load_t > 1e-6 {
                    losses_mw / load_t
                } else {
                    0.0
                };
                if loss_ratio > 0.05 {
                    tracing::debug!(
                        period = t,
                        losses_mw,
                        total_load_mw = load_t,
                        loss_ratio,
                        "scuc loss-factor DC PF warm start: losses exceed 5% cap; falling back to uniform rate"
                    );
                    let rate = FALLBACK_RATE;
                    for i in 0..n_bus {
                        dloss_dp[t][i] = rate;
                    }
                    total_losses_mw[t] = rate * load_t;
                } else {
                    // Clamp each bus dloss into [0, 0.5] defensively —
                    // a degenerate DC PF can produce large magnitudes
                    // that would over-penalise generators in the LP
                    // warm start.
                    for i in 0..n_bus.min(dloss.len()) {
                        dloss_dp[t][i] = dloss[i].clamp(0.0, 0.5);
                    }
                    total_losses_mw[t] = losses_mw;
                }
            }
            Err(err) => {
                // DC PF failed — fall back to uniform. The security
                // loop still gets a non-empty warm start, just a
                // cruder one.
                tracing::debug!(
                    period = t,
                    error = %err,
                    "scuc loss-factor DC PF warm start failed; falling back to uniform rate"
                );
                let rate = FALLBACK_RATE;
                for i in 0..n_bus {
                    dloss_dp[t][i] = rate;
                }
                total_losses_mw[t] = rate * total_load_by_hour[t];
            }
        }
        // If the PF succeeded but produced zero losses (flat network,
        // no flow, load-free period), fall back to uniform to avoid
        // handing an all-zero warm start that wouldn't differ from the
        // lossless default.
        if total_losses_mw[t] <= 1e-6 && total_load_by_hour[t] > 0.0 {
            let rate = FALLBACK_RATE;
            for i in 0..n_bus {
                dloss_dp[t][i] = rate;
            }
            total_losses_mw[t] = rate * total_load_by_hour[t];
        }
    }

    // Suppress unused-variable warnings on auxiliary inputs that
    // downstream callers may want to pass unconditionally.
    let _ = (bus_load_mw_by_hour, total_load_by_hour);

    LossFactorWarmStart {
        dloss_dp,
        total_losses_mw,
    }
}
