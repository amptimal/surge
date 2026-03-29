// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared Newton-Raphson inner kernel.
//!
//! This module owns the allocation-heavy scratch buffers and the core
//! mismatch/Jacobian/step loop. Higher-level policies such as Q-limit
//! switching, FACTS outer loops, startup policy, and island decomposition stay
//! in `newton_raphson.rs`.

use std::collections::HashMap;

use surge_network::network::BusType;

use crate::matrix::fused::FusedPattern;
use crate::matrix::ybus::YBus;
use surge_sparse::KluSolver;

/// Configuration passed to the inner Newton-Raphson iteration kernel.
#[derive(Clone, Copy)]
pub struct NrKernelOptions {
    /// Convergence tolerance (max power mismatch in per-unit).
    pub tolerance: f64,
    /// Maximum number of NR iterations.
    pub max_iterations: u32,
    /// Abort early if mismatch fails to improve for this many consecutive iterations.
    pub stall_limit: u32,
    /// Lower voltage magnitude clamp (per-unit).
    pub vm_min: f64,
    /// Upper voltage magnitude clamp (per-unit).
    pub vm_max: f64,
    /// Enable backtracking line search on each NR step.
    pub line_search: bool,
    /// When `true`, return a partial (non-converged) result instead of failing.
    pub allow_partial_nonconverged: bool,
}

/// Per-bus ZIP load coefficients used by the NR kernel for voltage-dependent loads.
#[derive(Clone, Copy)]
pub struct ZipBusData {
    /// Internal (0-based) bus index.
    pub idx: usize,
    /// Base active power demand (p.u.).
    pub p_base: f64,
    /// Base reactive power demand (p.u.).
    pub q_base: f64,
    /// Constant-impedance fraction of P (scales with V^2).
    pub pz: f64,
    /// Constant-current fraction of P (scales with V).
    pub pi: f64,
    /// Constant-power fraction of P (voltage-independent).
    pub pp: f64,
    /// Constant-impedance fraction of Q (scales with V^2).
    pub qz: f64,
    /// Constant-current fraction of Q (scales with V).
    pub qi: f64,
    /// Constant-power fraction of Q (voltage-independent).
    pub qp: f64,
}

/// Read-only view of state-dependent power injection data for the NR kernel.
pub struct StateDependentSpecView<'a> {
    /// Base scheduled active power injection per bus (p.u.).
    pub p_spec_base: &'a [f64],
    /// Base scheduled reactive power injection per bus (p.u.).
    pub q_spec_base: &'a [f64],
    /// Distributed slack participation factors (bus index -> weight), if any.
    pub participation: Option<&'a HashMap<usize, f64>>,
    /// Current loading factor (1.0 at base case).
    pub lambda: f64,
    /// ZIP load data per bus (empty when all loads are constant-power).
    pub zip_bus_data: &'a [ZipBusData],
    /// Current voltage magnitudes per bus (p.u.).
    pub vm: &'a [f64],
}

/// Pre-allocated scratch buffers reused across NR iterations to avoid per-step allocation.
pub struct NrWorkspace {
    p_spec: Vec<f64>,
    q_spec: Vec<f64>,
    p_calc: Vec<f64>,
    q_calc: Vec<f64>,
    zip_dn: Vec<f64>,
    zip_dl: Vec<f64>,
    sin_va: Vec<f64>,
    cos_va: Vec<f64>,
    va_save: Vec<f64>,
    vm_save: Vec<f64>,
    trial_p_spec: Vec<f64>,
    trial_q_spec: Vec<f64>,
    csc_values: Vec<f64>,
    rhs: Vec<f64>,
    w_aug: Vec<f64>,
}

impl NrWorkspace {
    pub fn new(n: usize, has_zip: bool) -> Self {
        Self {
            p_spec: vec![0.0; n],
            q_spec: vec![0.0; n],
            p_calc: vec![0.0; n],
            q_calc: vec![0.0; n],
            zip_dn: if has_zip { vec![0.0; n] } else { Vec::new() },
            zip_dl: if has_zip { vec![0.0; n] } else { Vec::new() },
            sin_va: vec![0.0; n],
            cos_va: vec![0.0; n],
            va_save: vec![0.0; n],
            vm_save: vec![0.0; n],
            trial_p_spec: vec![0.0; n],
            trial_q_spec: vec![0.0; n],
            csc_values: Vec::new(),
            rhs: Vec::new(),
            w_aug: Vec::new(),
        }
    }

    /// Ensure CSC value, RHS, and augmented work buffers are sized for the solve.
    pub fn prepare_factor_buffers(&mut self, nnz: usize, dim: usize) {
        if self.csc_values.len() != nnz {
            self.csc_values.resize(nnz, 0.0);
        }
        if self.rhs.len() != dim {
            self.rhs.resize(dim, 0.0);
        }
        if self.w_aug.len() != dim {
            self.w_aug.resize(dim, 0.0);
        }
    }

    /// Computed active power injections from the most recent iteration.
    pub fn p_calc(&self) -> &[f64] {
        &self.p_calc
    }

    /// Computed reactive power injections from the most recent iteration.
    pub fn q_calc(&self) -> &[f64] {
        &self.q_calc
    }

    /// Scheduled active power injection targets used in the most recent iteration.
    pub fn p_spec(&self) -> &[f64] {
        &self.p_spec
    }
}

/// Pre-computed data for distributed-slack augmented Jacobian rows.
pub struct AugmentedSlackData {
    /// Internal index of the slack bus.
    pub slack_idx: usize,
    /// Participation factor of the slack bus itself.
    pub alpha_slack: f64,
    /// Position of each PVPQ bus in the Jacobian column ordering.
    pub pvpq_pos: Vec<usize>,
    /// Position of each PQ bus in the Jacobian column ordering.
    pub pq_pos: Vec<usize>,
    /// Participation weights for the augmented slack row.
    pub c: Vec<f64>,
}

/// Immutable references to the network model data needed by the NR inner kernel.
pub struct PreparedNrModel<'a> {
    /// Y-bus admittance matrix.
    pub ybus: &'a YBus,
    /// Pre-computed fused Jacobian sparsity pattern.
    pub fused_pattern: &'a FusedPattern,
    /// Base active power injection per bus (p.u.).
    pub p_spec_base: &'a [f64],
    /// Base reactive power injection per bus (p.u.).
    pub q_spec_base: &'a [f64],
    /// ZIP load coefficients (empty when all loads are constant-power).
    pub zip_bus_data: &'a [ZipBusData],
    /// Distributed slack participation factors, if any.
    pub participation: Option<&'a HashMap<usize, f64>>,
    /// Sorted indices of PV and PQ buses (theta unknowns).
    pub pvpq_indices: &'a [usize],
    /// Sorted indices of PQ buses only (Vm unknowns).
    pub pq_indices: &'a [usize],
    /// Augmented slack row data for distributed slack, if enabled.
    pub aug: Option<&'a AugmentedSlackData>,
    /// Inner kernel solver options.
    pub options: NrKernelOptions,
}

/// Mutable voltage state updated in-place by the NR inner kernel.
pub struct NrState<'a> {
    /// Voltage magnitudes per bus (p.u.), updated each iteration.
    pub vm: &'a mut [f64],
    /// Voltage angles per bus (radians), updated each iteration.
    pub va: &'a mut [f64],
    /// Loading factor (1.0 for standard PF).
    pub lambda: &'a mut f64,
}

/// Successful outcome of the NR inner kernel.
#[derive(Clone, Copy, Debug)]
pub struct InnerSolveResult {
    /// Whether the solve converged within tolerance.
    pub converged: bool,
    /// Number of NR iterations performed.
    pub iterations: u32,
    /// Final maximum absolute power mismatch (p.u.).
    pub max_mismatch: f64,
    /// Internal bus index with the largest mismatch on the final iteration.
    pub worst_internal_idx: usize,
}

/// Reason the NR inner kernel did not converge.
#[derive(Clone, Copy, Debug)]
pub enum InnerFailureReason {
    /// KLU factorization returned an error (singular Jacobian).
    FactorizationFailed,
    /// KLU reported an ill-conditioned factor (reciprocal condition below threshold).
    IllConditioned,
    /// Reached the maximum iteration count without converging.
    MaxIterations,
    /// Mismatch failed to improve for `stall_limit` consecutive iterations.
    Stalled,
    /// The triangular solve after factorization produced NaN/Inf.
    LinearSolveFailed,
}

/// Failure details from the NR inner kernel.
#[derive(Clone, Copy, Debug)]
pub struct InnerSolveFailure {
    /// Number of NR iterations completed before failure.
    pub iterations: u32,
    /// Maximum absolute power mismatch (p.u.) on the last iteration.
    pub max_mismatch: f64,
    /// Internal bus index with the largest mismatch, if available.
    pub worst_internal_idx: Option<usize>,
    /// Best (lowest) mismatch achieved during the solve attempt.
    pub best_mismatch: f64,
    /// Mismatch on the very first iteration (before any correction).
    pub first_mismatch: f64,
    /// Why the inner kernel gave up.
    pub reason: InnerFailureReason,
}

/// Build the augmented Jacobian row data for distributed slack.
///
/// Computes position mappings and participation weights so the NR kernel can
/// append an extra row/column to the Jacobian that distributes the slack
/// power mismatch across multiple buses.
pub fn build_augmented_slack_data(
    bus_types: &[BusType],
    participation: &HashMap<usize, f64>,
    pvpq_indices: &[usize],
    pq_indices: &[usize],
    dim: usize,
) -> AugmentedSlackData {
    let n = bus_types.len();
    let slack_idx = bus_types
        .iter()
        .position(|t| *t == BusType::Slack)
        .unwrap_or(0);
    let alpha_slack = participation.get(&slack_idx).copied().unwrap_or(0.0);

    let mut pvpq_pos = vec![usize::MAX; n];
    let mut pq_pos = vec![usize::MAX; n];
    for (pos, &bus) in pvpq_indices.iter().enumerate() {
        pvpq_pos[bus] = pos;
    }
    for (pos, &bus) in pq_indices.iter().enumerate() {
        pq_pos[bus] = pos;
    }

    let mut c = vec![0.0f64; dim];
    for (k, &bus) in pvpq_indices.iter().enumerate() {
        c[k] = participation.get(&bus).copied().unwrap_or(0.0);
    }

    AugmentedSlackData {
        slack_idx,
        alpha_slack,
        pvpq_pos,
        pq_pos,
        c,
    }
}

/// Populate scheduled P/Q injection arrays from base specs, ZIP loads, and slack participation.
///
/// Called at the start of each NR iteration when voltage-dependent loads or
/// distributed slack require recomputation of the injection targets.
pub fn populate_state_dependent_specs(
    p_spec: &mut [f64],
    q_spec: &mut [f64],
    view: StateDependentSpecView<'_>,
) {
    p_spec.copy_from_slice(view.p_spec_base);
    q_spec.copy_from_slice(view.q_spec_base);

    if let Some(pmap) = view.participation {
        for (&bus_idx, &alpha) in pmap {
            p_spec[bus_idx] += alpha * view.lambda;
        }
    }

    for zip in view.zip_bus_data {
        let v = view.vm[zip.idx];
        let v2 = v * v;
        let p_zip = zip.p_base * (zip.pz * v2 + zip.pi * v + zip.pp);
        let q_zip = zip.q_base * (zip.qz * v2 + zip.qi * v + zip.qp);
        p_spec[zip.idx] += zip.p_base - p_zip;
        q_spec[zip.idx] += zip.q_base - q_zip;
    }
}

/// Run the core Newton-Raphson iteration loop using KLU sparse factorization.
///
/// This is the innermost solve routine with no outer loops (no Q-limit switching,
/// no OLTC, no island detection). Caller is responsible for bus classification,
/// Y-bus construction, and post-processing.
pub fn run_nr_inner(
    model: PreparedNrModel<'_>,
    state: NrState<'_>,
    workspace: &mut NrWorkspace,
    klu: &mut KluSolver,
    convergence_history: Option<&mut Vec<(u32, f64)>>,
) -> Result<InnerSolveResult, InnerSolveFailure> {
    let n_pvpq = model.pvpq_indices.len();
    let mut iteration = 0u32;
    let mut max_mismatch: f64;
    let mut worst_internal_idx: usize;
    let mut first_iteration = true;
    let mut first_mismatch = f64::INFINITY;
    let mut best_inner_mismatch = f64::INFINITY;
    let mut inner_no_progress = 0u32;

    let mut convergence_history = convergence_history;
    let mut pq_fresh = false;

    loop {
        populate_state_dependent_specs(
            &mut workspace.p_spec,
            &mut workspace.q_spec,
            StateDependentSpecView {
                p_spec_base: model.p_spec_base,
                q_spec_base: model.q_spec_base,
                participation: model.participation,
                lambda: *state.lambda,
                zip_bus_data: model.zip_bus_data,
                vm: state.vm,
            },
        );
        populate_zip_jacobian_diagonals(
            &mut workspace.zip_dn,
            &mut workspace.zip_dl,
            model.zip_bus_data,
            state.vm,
        );

        if pq_fresh {
            model.fused_pattern.fill_jacobian_into_with_trig(
                model.ybus,
                state.vm,
                state.va,
                &workspace.p_calc,
                &workspace.q_calc,
                &mut workspace.csc_values,
                &workspace.zip_dn,
                &workspace.zip_dl,
                &mut workspace.sin_va,
                &mut workspace.cos_va,
            );
            pq_fresh = false;
        } else {
            model.fused_pattern.build_fused_into_with_trig(
                model.ybus,
                state.vm,
                state.va,
                &mut workspace.p_calc,
                &mut workspace.q_calc,
                &mut workspace.csc_values,
                &workspace.zip_dn,
                &workspace.zip_dl,
                &mut workspace.sin_va,
                &mut workspace.cos_va,
            );
        }

        max_mismatch = 0.0_f64;
        worst_internal_idx = 0;
        let mut rhs_idx = 0;

        for &i in model.pvpq_indices {
            let dp = workspace.p_spec[i] - workspace.p_calc[i];
            if dp.abs() > max_mismatch || dp.is_nan() {
                max_mismatch = dp.abs();
                worst_internal_idx = i;
            }
            workspace.rhs[rhs_idx] = dp;
            rhs_idx += 1;
        }
        for &i in model.pq_indices {
            let dq = workspace.q_spec[i] - workspace.q_calc[i];
            if dq.abs() > max_mismatch || dq.is_nan() {
                max_mismatch = dq.abs();
                worst_internal_idx = i;
            }
            workspace.rhs[rhs_idx] = dq;
            rhs_idx += 1;
        }

        if !max_mismatch.is_finite() {
            return Err(InnerSolveFailure {
                iterations: iteration,
                max_mismatch,
                worst_internal_idx: Some(worst_internal_idx),
                best_mismatch: best_inner_mismatch,
                first_mismatch,
                reason: InnerFailureReason::LinearSolveFailed,
            });
        }

        if iteration == 0 {
            first_mismatch = max_mismatch;
        }

        if first_iteration {
            if klu.factor(&workspace.csc_values).is_err() {
                return Err(InnerSolveFailure {
                    iterations: iteration,
                    max_mismatch: f64::INFINITY,
                    worst_internal_idx: None,
                    best_mismatch: best_inner_mismatch,
                    first_mismatch,
                    reason: InnerFailureReason::FactorizationFailed,
                });
            }
            first_iteration = false;
        } else if klu.refactor(&workspace.csc_values).is_err()
            && klu.factor(&workspace.csc_values).is_err()
        {
            return Err(InnerSolveFailure {
                iterations: iteration,
                max_mismatch,
                worst_internal_idx: Some(worst_internal_idx),
                best_mismatch: best_inner_mismatch.min(max_mismatch),
                first_mismatch,
                reason: InnerFailureReason::FactorizationFailed,
            });
        }

        let rcond = klu.rcond();
        if rcond > 0.0 && rcond < 1e-12 {
            return Err(InnerSolveFailure {
                iterations: iteration,
                max_mismatch,
                worst_internal_idx: Some(worst_internal_idx),
                best_mismatch: best_inner_mismatch.min(max_mismatch),
                first_mismatch,
                reason: InnerFailureReason::IllConditioned,
            });
        }

        if let Some(history) = convergence_history.as_deref_mut() {
            history.push((iteration, max_mismatch));
        }

        if max_mismatch < model.options.tolerance {
            return Ok(InnerSolveResult {
                converged: true,
                iterations: iteration,
                max_mismatch,
                worst_internal_idx,
            });
        }

        if iteration >= model.options.max_iterations {
            if !model.options.allow_partial_nonconverged {
                return Err(InnerSolveFailure {
                    iterations: iteration,
                    max_mismatch,
                    worst_internal_idx: Some(worst_internal_idx),
                    best_mismatch: best_inner_mismatch.min(max_mismatch),
                    first_mismatch,
                    reason: InnerFailureReason::MaxIterations,
                });
            }
            return Ok(InnerSolveResult {
                converged: false,
                iterations: iteration,
                max_mismatch,
                worst_internal_idx,
            });
        }

        if max_mismatch < best_inner_mismatch * 0.99 {
            best_inner_mismatch = max_mismatch;
            inner_no_progress = 0;
        } else {
            inner_no_progress += 1;
            if inner_no_progress >= model.options.stall_limit {
                if !model.options.allow_partial_nonconverged {
                    return Err(InnerSolveFailure {
                        iterations: iteration,
                        max_mismatch,
                        worst_internal_idx: Some(worst_internal_idx),
                        best_mismatch: best_inner_mismatch.min(max_mismatch),
                        first_mismatch,
                        reason: InnerFailureReason::Stalled,
                    });
                }
                return Ok(InnerSolveResult {
                    converged: false,
                    iterations: iteration,
                    max_mismatch,
                    worst_internal_idx,
                });
            }
        }

        if klu.solve(&mut workspace.rhs).is_err() {
            if model.options.allow_partial_nonconverged {
                return Ok(InnerSolveResult {
                    converged: false,
                    iterations: iteration,
                    max_mismatch,
                    worst_internal_idx,
                });
            }
            return Err(InnerSolveFailure {
                iterations: iteration,
                max_mismatch,
                worst_internal_idx: Some(worst_internal_idx),
                best_mismatch: best_inner_mismatch.min(max_mismatch),
                first_mismatch,
                reason: InnerFailureReason::LinearSolveFailed,
            });
        }

        let mut delta_lambda = 0.0f64;
        if let Some(aug) = model.aug {
            let r_slack = workspace.p_spec[aug.slack_idx] - workspace.p_calc[aug.slack_idx];
            let beta_dx0 = slack_row_dot(
                model.ybus,
                aug.slack_idx,
                state.vm,
                state.va,
                &aug.pvpq_pos,
                &aug.pq_pos,
                n_pvpq,
                &workspace.rhs,
            );

            workspace.w_aug.copy_from_slice(&aug.c);
            if klu.solve(&mut workspace.w_aug).is_ok() {
                let beta_w = slack_row_dot(
                    model.ybus,
                    aug.slack_idx,
                    state.vm,
                    state.va,
                    &aug.pvpq_pos,
                    &aug.pq_pos,
                    n_pvpq,
                    &workspace.w_aug,
                );
                let denom = beta_w - aug.alpha_slack;
                if denom.abs() > 1e-15 {
                    delta_lambda = (r_slack - beta_dx0) / denom;
                    for (i, &wi) in workspace.w_aug.iter().enumerate() {
                        workspace.rhs[i] += delta_lambda * wi;
                    }
                }
            }
        }

        damp_voltage_step(model.pq_indices, n_pvpq, &mut workspace.rhs);

        let lambda_base = *state.lambda;

        if model.options.line_search {
            apply_line_search(
                &model,
                state.vm,
                state.va,
                lambda_base,
                delta_lambda,
                state.lambda,
                workspace,
            );
            pq_fresh = true;
        } else {
            for (k, &i) in model.pvpq_indices.iter().enumerate() {
                state.va[i] += workspace.rhs[k];
            }
            for (k, &i) in model.pq_indices.iter().enumerate() {
                state.vm[i] += workspace.rhs[n_pvpq + k];
                state.vm[i] = state.vm[i].clamp(model.options.vm_min, model.options.vm_max);
            }
            *state.lambda = lambda_base + delta_lambda;
        }

        iteration += 1;
    }
}

fn populate_zip_jacobian_diagonals(
    zip_dn: &mut [f64],
    zip_dl: &mut [f64],
    zip_bus_data: &[ZipBusData],
    vm: &[f64],
) {
    if zip_bus_data.is_empty() {
        return;
    }

    zip_dn.fill(0.0);
    zip_dl.fill(0.0);
    for zip in zip_bus_data {
        let v = vm[zip.idx];
        zip_dn[zip.idx] = zip.p_base * (2.0 * zip.pz * v + zip.pi);
        zip_dl[zip.idx] = zip.q_base * (2.0 * zip.qz * v + zip.qi);
    }
}

fn compute_pq_ybus_with_trig(
    ybus: &YBus,
    vm: &[f64],
    va: &[f64],
    p_calc: &mut [f64],
    q_calc: &mut [f64],
    sin_va: &mut [f64],
    cos_va: &mut [f64],
) {
    let n = ybus.n;
    p_calc[..n].fill(0.0);
    q_calc[..n].fill(0.0);

    for i in 0..n {
        (sin_va[i], cos_va[i]) = va[i].sin_cos();
    }

    for i in 0..n {
        let vm_i = vm[i];
        let row = ybus.row(i);
        let mut p_i = 0.0;
        let mut q_i = 0.0;
        let si = sin_va[i];
        let ci = cos_va[i];

        for (k, &j) in row.col_idx.iter().enumerate() {
            let vm_j = vm[j];
            let g = row.g[k];
            let b = row.b[k];
            if j == i {
                p_i += vm_j * g;
                q_i -= vm_j * b;
            } else {
                let sin_t = si * cos_va[j] - ci * sin_va[j];
                let cos_t = ci * cos_va[j] + si * sin_va[j];
                p_i += vm_j * (g * cos_t + b * sin_t);
                q_i += vm_j * (g * sin_t - b * cos_t);
            }
        }

        p_calc[i] = vm_i * p_i;
        q_calc[i] = vm_i * q_i;
    }
}

fn mismatch_norm(
    p_spec: &[f64],
    q_spec: &[f64],
    p_calc: &[f64],
    q_calc: &[f64],
    pvpq_indices: &[usize],
    pq_indices: &[usize],
) -> f64 {
    let mut norm = 0.0_f64;
    for &i in pvpq_indices {
        norm = norm.max((p_spec[i] - p_calc[i]).abs());
    }
    for &i in pq_indices {
        norm = norm.max((q_spec[i] - q_calc[i]).abs());
    }
    norm
}

#[allow(clippy::too_many_arguments)]
fn slack_row_dot(
    ybus: &YBus,
    slack_idx: usize,
    vm: &[f64],
    va: &[f64],
    pvpq_pos: &[usize],
    pq_pos: &[usize],
    n_pvpq: usize,
    x: &[f64],
) -> f64 {
    let vm_s = vm[slack_idx];
    let va_s = va[slack_idx];
    let row = ybus.row(slack_idx);
    let mut dot = 0.0f64;

    for (k, &j) in row.col_idx.iter().enumerate() {
        if j == slack_idx {
            continue;
        }
        let theta_sj = va_s - va[j];
        let (sin_t, cos_t) = theta_sj.sin_cos();
        let g = row.g[k];
        let b = row.b[k];

        let pp = pvpq_pos[j];
        if pp != usize::MAX {
            dot += vm_s * vm[j] * (g * sin_t - b * cos_t) * x[pp];
        }

        let qp = pq_pos[j];
        if qp != usize::MAX {
            dot += vm_s * (g * cos_t + b * sin_t) * x[n_pvpq + qp];
        }
    }

    dot
}

fn damp_voltage_step(pq_indices: &[usize], n_pvpq: usize, rhs: &mut [f64]) {
    let max_dvm_limit = 0.3_f64;
    let max_dvm = pq_indices
        .iter()
        .enumerate()
        .map(|(k, _)| rhs[n_pvpq + k].abs())
        .fold(0.0_f64, f64::max);
    if max_dvm > max_dvm_limit {
        let scale = max_dvm_limit / max_dvm;
        for val in rhs.iter_mut() {
            *val *= scale;
        }
    }
}

fn apply_line_search(
    model: &PreparedNrModel<'_>,
    vm: &mut [f64],
    va: &mut [f64],
    lambda_base: f64,
    lambda_step: f64,
    lambda: &mut f64,
    workspace: &mut NrWorkspace,
) {
    let n_pvpq = model.pvpq_indices.len();
    workspace.va_save.copy_from_slice(va);
    workspace.vm_save.copy_from_slice(vm);
    let current_norm = mismatch_norm(
        &workspace.p_spec,
        &workspace.q_spec,
        &workspace.p_calc,
        &workspace.q_calc,
        model.pvpq_indices,
        model.pq_indices,
    );

    for (k, &i) in model.pvpq_indices.iter().enumerate() {
        va[i] = workspace.va_save[i] + workspace.rhs[k];
    }
    for (k, &i) in model.pq_indices.iter().enumerate() {
        vm[i] = (workspace.vm_save[i] + workspace.rhs[n_pvpq + k])
            .clamp(model.options.vm_min, model.options.vm_max);
    }

    let lambda_trial = lambda_base + lambda_step;
    compute_pq_ybus_with_trig(
        model.ybus,
        vm,
        va,
        &mut workspace.p_calc,
        &mut workspace.q_calc,
        &mut workspace.sin_va,
        &mut workspace.cos_va,
    );
    populate_state_dependent_specs(
        &mut workspace.trial_p_spec,
        &mut workspace.trial_q_spec,
        StateDependentSpecView {
            p_spec_base: model.p_spec_base,
            q_spec_base: model.q_spec_base,
            participation: model.participation,
            lambda: lambda_trial,
            zip_bus_data: model.zip_bus_data,
            vm,
        },
    );
    let trial_norm = mismatch_norm(
        &workspace.trial_p_spec,
        &workspace.trial_q_spec,
        &workspace.p_calc,
        &workspace.q_calc,
        model.pvpq_indices,
        model.pq_indices,
    );

    if trial_norm >= current_norm {
        let mut alpha = 0.5;
        let mut best_alpha = 0.5;
        let mut best_norm = trial_norm;

        for _ in 0..10 {
            for (k, &i) in model.pvpq_indices.iter().enumerate() {
                va[i] = workspace.va_save[i] + alpha * workspace.rhs[k];
            }
            for (k, &i) in model.pq_indices.iter().enumerate() {
                vm[i] = (workspace.vm_save[i] + alpha * workspace.rhs[n_pvpq + k])
                    .clamp(model.options.vm_min, model.options.vm_max);
            }
            compute_pq_ybus_with_trig(
                model.ybus,
                vm,
                va,
                &mut workspace.p_calc,
                &mut workspace.q_calc,
                &mut workspace.sin_va,
                &mut workspace.cos_va,
            );
            populate_state_dependent_specs(
                &mut workspace.trial_p_spec,
                &mut workspace.trial_q_spec,
                StateDependentSpecView {
                    p_spec_base: model.p_spec_base,
                    q_spec_base: model.q_spec_base,
                    participation: model.participation,
                    lambda: lambda_base + alpha * lambda_step,
                    zip_bus_data: model.zip_bus_data,
                    vm,
                },
            );
            let norm = mismatch_norm(
                &workspace.trial_p_spec,
                &workspace.trial_q_spec,
                &workspace.p_calc,
                &workspace.q_calc,
                model.pvpq_indices,
                model.pq_indices,
            );

            if norm < best_norm {
                best_norm = norm;
                best_alpha = alpha;
            }
            if norm < current_norm {
                break;
            }
            alpha *= 0.5;
        }

        for (k, &i) in model.pvpq_indices.iter().enumerate() {
            va[i] = workspace.va_save[i] + best_alpha * workspace.rhs[k];
        }
        for (k, &i) in model.pq_indices.iter().enumerate() {
            vm[i] = (workspace.vm_save[i] + best_alpha * workspace.rhs[n_pvpq + k])
                .clamp(model.options.vm_min, model.options.vm_max);
        }
        *lambda = lambda_base + best_alpha * lambda_step;
        compute_pq_ybus_with_trig(
            model.ybus,
            vm,
            va,
            &mut workspace.p_calc,
            &mut workspace.q_calc,
            &mut workspace.sin_va,
            &mut workspace.cos_va,
        );
    } else {
        *lambda = lambda_trial;
    }
}
