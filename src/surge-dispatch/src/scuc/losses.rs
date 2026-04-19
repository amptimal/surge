// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared loss-factor iteration for horizon SCUC solves and pricing LPs.

use std::collections::HashMap;

use surge_network::Network;
use surge_opf::backends::{LpPrimalStart, LpResult, LpSolveStatus, LpSolver, SparseProblem};
use tracing::{debug, warn};

use super::layout::ScucLayout;
use crate::common::dc::solve_sparse_problem_with_start;
use crate::common::spec::DispatchProblemSpec;
use crate::error::ScedError;

pub(super) struct ScucLossIterationInput<'a> {
    pub solver: &'a dyn LpSolver,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub hourly_networks: &'a [Network],
    pub bus_map: &'a HashMap<u32, usize>,
    pub layout: &'a ScucLayout,
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
/// Captured once at setup time so the loss-factor iteration can keep
/// scaling the original coefficient by the per-period bus penalty
/// factor without losing track of the unscaled value.
#[derive(Clone, Copy, Debug)]
struct BusInjectionCoeff {
    /// Index into `SparseProblem::a_value` of the entry to scale.
    pos: usize,
    /// Bus the row corresponds to (used to look up `dloss_dp_out[t][bus_idx]`).
    bus_idx: usize,
    /// Coefficient as it was when the matrix was first built (before
    /// any loss-factor scaling). Multiplied by `(1 - dloss[bus_idx])`
    /// at every loss-factor iteration.
    original: f64,
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

pub(super) fn iterate_loss_factors(
    input: ScucLossIterationInput<'_>,
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

    // For each period, capture every (a_value position, bus_idx,
    // original coefficient) for every column that physically injects
    // or withdraws power at a bus. The loss factor iteration then
    // multiplies each entry by `(1 - dloss[bus_idx])` at every step,
    // which keeps the LP in sync with the AC penalty factors at all
    // injection points -- generators *and* HVDC links *and*
    // dispatchable loads *and* virtual bids *and* storage charge /
    // discharge. The previous implementation only updated generator
    // coefficients, which made HVDC and DL look artificially loss-
    // free relative to local generation and pushed the LP toward
    // pathological HVDC choices (e.g. always at the lower bound on
    // event4_73 D2 911, regardless of local LMP differentials).
    //
    // The "extra terms" used by SCUC for power-balance penalty slacks
    // (`pb_curtailment_bus`, `pb_excess_bus`) and for DR rebound
    // injections are intentionally NOT loss-factored: the slacks must
    // continue to absorb a full pu of imbalance per pu of slack so
    // they actually clear the bus equation, and the rebound terms are
    // a small effect that the existing code does not pin to a single
    // bus_idx.
    let mut injections_by_hour: Vec<Vec<BusInjectionCoeff>> = Vec::with_capacity(n_hours);
    let mut orig_rhs_by_hour: Vec<Vec<f64>> = Vec::with_capacity(n_hours);
    let mut bus_load_mw_by_hour: Vec<Vec<f64>> = Vec::with_capacity(n_hours);
    let mut total_load_by_hour: Vec<f64> = Vec::with_capacity(n_hours);
    let mut loss_ptdf_by_hour = Vec::with_capacity(n_hours);

    let dispatch = &input.layout.dispatch;
    let n_gen = input.gen_bus_idx.len();
    let n_storage = dispatch.sto_dis - dispatch.sto_ch;
    let n_hvdc_vars = dispatch.e_g - dispatch.hvdc;
    let n_dl = dispatch.vbid - dispatch.dl;
    let n_vbid = dispatch.block - dispatch.vbid;

    for (t, network_t) in input.hourly_networks.iter().enumerate() {
        let pb_start = input.hour_row_bases[t] + input.n_flow;
        orig_rhs_by_hour.push(
            (0..input.n_bus)
                .map(|i| input.problem.row_lower[pb_start + i])
                .collect(),
        );

        let mut bus_load_mw = vec![0.0_f64; input.n_bus];
        for load in &network_t.loads {
            if load.in_service
                && let Some(&bus_idx) = input.bus_map.get(&load.bus)
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
                input.problem,
                &mut hour_injections,
                input.layout.pg_col(t, j),
                pb_start,
                input.n_bus,
            );
        }

        // Storage charge/discharge each touch the storage bus.
        for s in 0..n_storage {
            collect_bus_injection_coeffs(
                input.problem,
                &mut hour_injections,
                input.layout.storage_charge_col(t, s),
                pb_start,
                input.n_bus,
            );
            collect_bus_injection_coeffs(
                input.problem,
                &mut hour_injections,
                input.layout.storage_discharge_col(t, s),
                pb_start,
                input.n_bus,
            );
        }

        // HVDC link variables touch BOTH the from-bus and the to-bus
        // (banded layouts have one column per band; the helper picks
        // up every entry the column writes into the bus-balance row
        // range, regardless of how many terminals it touches).
        for k in 0..n_hvdc_vars {
            collect_bus_injection_coeffs(
                input.problem,
                &mut hour_injections,
                input.layout.col(t, dispatch.hvdc + k),
                pb_start,
                input.n_bus,
            );
        }

        // Dispatchable loads (DR) consume at one bus.
        for k in 0..n_dl {
            collect_bus_injection_coeffs(
                input.problem,
                &mut hour_injections,
                input.layout.col(t, dispatch.dl + k),
                pb_start,
                input.n_bus,
            );
        }

        // Virtual bids (INC/DEC) inject or withdraw at one bus.
        for k in 0..n_vbid {
            collect_bus_injection_coeffs(
                input.problem,
                &mut hour_injections,
                input.layout.col(t, dispatch.vbid + k),
                pb_start,
                input.n_bus,
            );
        }

        injections_by_hour.push(hour_injections);
    }

    let mut prev_dloss = vec![vec![0.0_f64; input.n_bus]; n_hours];
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
                &loss_ptdf_by_hour[t],
            );
            max_delta = max_delta.max(
                dloss
                    .iter()
                    .zip(prev_dloss[t].iter())
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0_f64, f64::max),
            );
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
        if loss_iter > 0 && max_delta < input.spec.loss_factor_tol {
            debug!(iter = loss_iter, max_delta, "SCUC loss factors converged");
            converged = true;
        }
        prev_dloss.clone_from(&dloss_dp_out);

        for t in 0..n_hours {
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
            for term in &injections_by_hour[t] {
                if term.original >= 0.0 {
                    continue;
                }
                // Clamp to [0.5, 1.0]: a generator physically cannot deliver
                // more than its rated MW, so pf must not exceed 1.0. The
                // previous upper clamp of 1.5 allowed dloss < 0 buses to
                // credit generators with 150% delivery, causing sum(gen) <
                // sum(load) in the dispatch result.
                let pf = (1.0 - dloss_dp_out[t][term.bus_idx]).clamp(0.5, 1.0);
                input.problem.a_value[term.pos] = term.original * pf;
            }

            let pb_start = input.hour_row_bases[t] + input.n_flow;
            let total_load = total_load_by_hour[t];
            for (i, &orig_rhs) in orig_rhs_by_hour[t].iter().enumerate() {
                let loss_share = if total_load > 1e-6 {
                    total_losses_by_hour[t] * (bus_load_mw_by_hour[t][i].max(0.0) / total_load)
                } else {
                    total_losses_by_hour[t] / input.n_bus as f64
                };
                let rhs = orig_rhs - loss_share;
                input.problem.row_lower[pb_start + i] = rhs;
                input.problem.row_upper[pb_start + i] = rhs;
            }
        }

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
        tracing::info!(
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
        let total_load = total_load_by_hour[t];
        for i in 0..input.n_bus {
            loss_allocation_mw[t][i] = if total_load > 1e-6 {
                total_losses_mw * (bus_load_mw_by_hour[t][i].max(0.0) / total_load)
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
