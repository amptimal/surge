#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Optimal Reactive Power Dispatch (ORPD) / Volt-VAr Optimization (VVO).
//!
//! ORPD minimizes active power losses or voltage deviation while satisfying
//! reactive power balance, voltage limits, and equipment constraints. This is
//! distinct from economic dispatch (which minimizes generation cost).
//!
//! ## Formulation
//!
//! Variables: `x = [Va(non-slack) | Vm(all) | Pg(in-service) | Qg(in-service)]`
//!
//! When `fix_pg = true` (default), Pg variables are fixed at their current
//! dispatch by setting `pg_lb[i] = pg_ub[i] = pg_dispatch[i]`, so Ipopt
//! treats them as equality-constrained and optimizes only Vm/Va/Qg.
//!
//! ## Objectives
//!
//! - **MinimizeLosses**: `min Σ_k G_k(Vf² + Vt² - 2·Vf·Vt·cos(θ_ft))`
//! - **MinimizeVoltageDeviation**: `min Σ_i (Vi - V_ref)²`
//! - **MinimizeCombined**: `min α·P_loss + β·Σ(Vi - V_ref)²`
//!
//! ## Constraints
//!
//! - Full AC power flow (P and Q balance at all buses)
//! - Voltage magnitude bounds: `Vmin_i ≤ Vi ≤ Vmax_i`
//! - Generator reactive limits: `Qmin_j ≤ Qgj ≤ Qmax_j`
//! - Generator active power FIXED at current dispatch (when `fix_pg = true`)
//! - Branch thermal limits: `Pf²+Qf² ≤ (rate_a/base)²` (optional)

use std::collections::HashMap;
use std::time::Instant;

use surge_ac::matrix::mismatch::compute_power_injection;
use surge_ac::matrix::ybus::{YBus, build_ybus};
use surge_network::Network;

use crate::backends::{NlpSolver, try_default_nlp_solver};
use crate::nlp::{HessianMode, NlpOptions, NlpProblem};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// ORPD objective function selection.
#[derive(Debug, Clone, Default)]
pub enum OrpdObjective {
    /// Minimize total active power losses across all branches.
    ///
    /// Objective: `Σ_k G_k(Vf² + Vt² - 2·Vf·Vt·cos(θ_ft))`
    #[default]
    MinimizeLosses,

    /// Minimize RMS voltage deviation from a reference value.
    ///
    /// Objective: `Σ_i (Vi - v_ref)²`
    MinimizeVoltageDeviation {
        /// Reference voltage in per-unit. Default 1.0.
        v_ref: f64,
    },

    /// Minimize a weighted combination of losses and voltage deviation.
    ///
    /// Objective: `loss_weight * P_loss + vdev_weight * Σ_i (Vi - v_ref)²`
    MinimizeCombined {
        /// Weight on the loss term (dimensionless).
        loss_weight: f64,
        /// Weight on the voltage deviation term.
        vdev_weight: f64,
        /// Reference voltage in per-unit.
        v_ref: f64,
    },
}

impl OrpdObjective {
    /// Parse a canonical ORPD objective name into a fully specified objective.
    pub fn parse_named(
        name: &str,
        v_ref: f64,
        loss_weight: f64,
        voltage_weight: f64,
    ) -> Result<Self, String> {
        if !v_ref.is_finite() || v_ref <= 0.0 {
            return Err(format!(
                "ORPD voltage target must be a positive finite number, got {v_ref}"
            ));
        }
        if !loss_weight.is_finite() || loss_weight < 0.0 {
            return Err(format!(
                "ORPD loss weight must be a finite non-negative number, got {loss_weight}"
            ));
        }
        if !voltage_weight.is_finite() || voltage_weight < 0.0 {
            return Err(format!(
                "ORPD voltage weight must be a finite non-negative number, got {voltage_weight}"
            ));
        }
        match name {
            "loss" | "losses" => Ok(Self::MinimizeLosses),
            "voltage" | "voltage_deviation" => Ok(Self::MinimizeVoltageDeviation { v_ref }),
            "combined" => {
                if loss_weight == 0.0 && voltage_weight == 0.0 {
                    return Err(
                        "ORPD combined objective requires at least one positive weight".into(),
                    );
                }
                Ok(Self::MinimizeCombined {
                    loss_weight,
                    vdev_weight: voltage_weight,
                    v_ref,
                })
            }
            other => Err(format!(
                "unknown ORPD objective '{other}'. Valid choices: loss, voltage, combined"
            )),
        }
    }

    /// Canonical user-facing name for this objective family.
    pub fn canonical_name(&self) -> &'static str {
        match self {
            Self::MinimizeLosses => "loss",
            Self::MinimizeVoltageDeviation { .. } => "voltage",
            Self::MinimizeCombined { .. } => "combined",
        }
    }
}

/// Options for the ORPD solver.
#[derive(Debug, Clone)]
pub struct OrpdOptions {
    /// Objective function to minimize.
    pub objective: OrpdObjective,
    /// Fix generator active power at current dispatch (default: true).
    ///
    /// When `true`, `Pg[j]` bounds are collapsed to `pg_lb = pg_ub = pg_dispatch`,
    /// effectively removing active power as a degree of freedom. The solver
    /// then optimizes only reactive dispatch, voltage angles, and magnitudes.
    pub fix_pg: bool,
    /// Optimize generator reactive power within `[Qmin, Qmax]` (default: true).
    pub optimize_q: bool,
    /// Whether to enforce branch thermal limits (default: true).
    pub enforce_thermal_limits: bool,
    /// Minimum rate_a (MVA) for a branch to have a thermal limit.
    pub min_rate_a: f64,
    /// Convergence tolerance (default: 1e-6).
    pub tol: f64,
    /// Maximum NLP iterations (0 = auto: max(500, n_buses/20)).
    pub max_iter: u32,
    /// NLP solver print level (0 = silent, 5 = verbose).
    pub print_level: i32,
    /// Use exact analytical Hessian (true) or L-BFGS approximation (false).
    pub exact_hessian: bool,
    /// Override NLP solver backend. `None` = use the canonical default NLP policy.
    pub nlp_solver: Option<std::sync::Arc<dyn NlpSolver>>,
}

impl Default for OrpdOptions {
    fn default() -> Self {
        Self {
            objective: OrpdObjective::default(),
            fix_pg: true,
            optimize_q: true,
            enforce_thermal_limits: true,
            min_rate_a: 1.0,
            tol: 1e-6,
            max_iter: 0,
            print_level: 0,
            exact_hessian: true,
            nlp_solver: None,
        }
    }
}

/// Solution returned by `solve_orpd`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OrpdResult {
    /// Whether the NLP solver converged.
    pub converged: bool,
    /// Optimal objective value (losses in pu, or voltage deviation in pu², or combined).
    pub objective: f64,
    /// Total active power losses in MW.
    pub total_losses_mw: f64,
    /// RMS voltage deviation from 1.0 pu: `sqrt(Σ(Vi-1)²/n)`.
    pub voltage_deviation: f64,
    /// Bus voltage magnitudes and angles: `(Vm[pu], Va[rad])` per internal bus.
    pub voltages: Vec<(f64, f64)>,
    /// Generator reactive dispatch in pu per in-service generator.
    pub q_dispatch: Vec<f64>,
    /// Generator active dispatch in pu per in-service generator.
    pub p_dispatch: Vec<f64>,
    /// Number of NLP iterations, when the backend exposes it.
    pub iterations: Option<u32>,
    /// Solver wall time in milliseconds.
    pub solve_time_ms: f64,
}

// ---------------------------------------------------------------------------
// Internal branch admittance cache (mirrors ac_opf internal)
// ---------------------------------------------------------------------------

struct BranchAdm {
    from: usize,
    to: usize,
    // Loss objective needs only G_k (series conductance) and the mutual terms.
    // We store the full pi-circuit for accurate loss calculation and Hessian.
    g_ff: f64,
    b_ff: f64,
    g_ft: f64,
    b_ft: f64,
    g_tt: f64,
    b_tt: f64,
    g_tf: f64,
    b_tf: f64,
    /// Series conductance G_series for loss formula.
    g_series: f64,
    /// Flow limit squared `(rate_a / base_mva)²`.
    s_max_sq: f64,
}

fn build_branch_adm(network: &Network, bus_map: &HashMap<u32, usize>) -> Vec<BranchAdm> {
    let base = network.base_mva;
    network
        .branches
        .iter()
        .filter(|br| br.in_service)
        .map(|br| {
            let f = bus_map[&br.from_bus];
            let t = bus_map[&br.to_bus];
            let adm = br.pi_model(1e-40);
            let z_sq = br.r * br.r + br.x * br.x;
            let gs = if z_sq > 1e-40 { br.r / z_sq } else { 1e6_f64 };

            let s_max = br.rating_a_mva / base;
            BranchAdm {
                from: f,
                to: t,
                g_ff: adm.g_ff,
                b_ff: adm.b_ff,
                g_ft: adm.g_ft,
                b_ft: adm.b_ft,
                g_tt: adm.g_tt,
                b_tt: adm.b_tt,
                g_tf: adm.g_tf,
                b_tf: adm.b_tf,
                g_series: gs,
                s_max_sq: s_max * s_max,
            }
        })
        .collect()
}

/// Compute from-side branch flows (Pf, Qf).
#[inline]
fn flow_from(ba: &BranchAdm, vm: &[f64], va: &[f64]) -> (f64, f64) {
    let vi = vm[ba.from];
    let vj = vm[ba.to];
    let theta = va[ba.from] - va[ba.to];
    let (sin_t, cos_t) = theta.sin_cos();
    let pf = vi * vi * ba.g_ff + vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
    let qf = -vi * vi * ba.b_ff + vi * vj * (ba.g_ft * sin_t - ba.b_ft * cos_t);
    (pf, qf)
}

/// Compute to-side branch flows (Pt, Qt).
#[inline]
fn flow_to(ba: &BranchAdm, vm: &[f64], va: &[f64]) -> (f64, f64) {
    let vi = vm[ba.from];
    let vj = vm[ba.to];
    let theta_tf = va[ba.to] - va[ba.from];
    let (sin_t, cos_t) = theta_tf.sin_cos();
    let pt = vj * vj * ba.g_tt + vj * vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);
    let qt = -vj * vj * ba.b_tt + vj * vi * (ba.g_tf * sin_t - ba.b_tf * cos_t);
    (pt, qt)
}

// ---------------------------------------------------------------------------
// Variable mapping (mirrors AcOpfMapping in ac_opf.rs)
// ---------------------------------------------------------------------------

struct OrpdMapping {
    n_bus: usize,
    slack_idx: usize,
    /// In-service generator indices into network.generators[].
    gen_indices: Vec<usize>,
    n_gen: usize,
    /// Map from bus internal index to list of local gen indices at that bus.
    bus_gen_map: Vec<Vec<usize>>,
    n_var: usize,
    n_con: usize,
    va_offset: usize, // 0
    vm_offset: usize, // n_bus - 1
    pg_offset: usize, // n_bus - 1 + n_bus
    qg_offset: usize, // n_bus - 1 + n_bus + n_gen
}

impl OrpdMapping {
    fn new(network: &Network, constrained_branches: Vec<usize>) -> Result<Self, String> {
        let n_bus = network.n_buses();
        let slack_idx = network.slack_bus_index().ok_or("no slack bus in network")?;

        let gen_indices: Vec<usize> = network
            .generators
            .iter()
            .enumerate()
            .filter(|(_, g)| g.in_service)
            .map(|(i, _)| i)
            .collect();
        let n_gen = gen_indices.len();
        if n_gen == 0 {
            return Err("no in-service generators".into());
        }

        let bus_map = network.bus_index_map();
        let mut bus_gen_map: Vec<Vec<usize>> = vec![vec![]; n_bus];
        for (local_idx, &gi) in gen_indices.iter().enumerate() {
            let bus_idx = bus_map[&network.generators[gi].bus];
            bus_gen_map[bus_idx].push(local_idx);
        }

        let n_va = n_bus - 1;
        let n_vm = n_bus;
        let n_var = n_va + n_vm + 2 * n_gen;
        // Constraints: 2*n_bus (P+Q balance) + 2*constrained_branches (flow limits)
        let n_con = 2 * n_bus + 2 * constrained_branches.len();
        drop(constrained_branches);

        Ok(Self {
            n_bus,
            slack_idx,
            gen_indices,
            n_gen,
            bus_gen_map,
            n_var,
            n_con,
            va_offset: 0,
            vm_offset: n_va,
            pg_offset: n_va + n_vm,
            qg_offset: n_va + n_vm + n_gen,
        })
    }

    #[inline]
    fn va_var(&self, bus: usize) -> Option<usize> {
        if bus == self.slack_idx {
            None
        } else if bus < self.slack_idx {
            Some(self.va_offset + bus)
        } else {
            Some(self.va_offset + bus - 1)
        }
    }

    #[inline]
    fn vm_var(&self, bus: usize) -> usize {
        self.vm_offset + bus
    }

    #[inline]
    fn pg_var(&self, j: usize) -> usize {
        self.pg_offset + j
    }

    #[inline]
    fn qg_var(&self, j: usize) -> usize {
        self.qg_offset + j
    }

    /// Unpack x into (va[n_bus], vm[n_bus], pg[n_gen], qg[n_gen]).
    fn unpack<'a>(&self, x: &'a [f64]) -> (Vec<f64>, &'a [f64], &'a [f64], &'a [f64]) {
        let mut va = vec![0.0; self.n_bus];
        for i in 0..self.n_bus {
            if let Some(idx) = self.va_var(i) {
                va[i] = x[idx];
            }
        }
        let vm = &x[self.vm_offset..self.vm_offset + self.n_bus];
        let pg = &x[self.pg_offset..self.pg_offset + self.n_gen];
        let qg = &x[self.qg_offset..self.qg_offset + self.n_gen];
        (va, vm, pg, qg)
    }
}

// ---------------------------------------------------------------------------
// ORPD NLP problem
// ---------------------------------------------------------------------------

struct OrpdProblem<'a> {
    network: &'a Network,
    ybus: YBus,
    mapping: OrpdMapping,
    /// All in-service branches (indexed 0..n_inservice_branches).
    all_branches: Vec<BranchAdm>,
    /// Subset of all_branches with thermal limits (for constraint rows).
    constrained_admittances: Vec<BranchAdm>,
    base_mva: f64,
    options: &'a OrpdOptions,
    /// Fixed Pg values in pu (only used when `fix_pg = true`).
    pg_fixed_pu: Vec<f64>,
    /// Jacobian sparsity.
    jac_rows: Vec<i32>,
    jac_cols: Vec<i32>,
    /// Hessian sparsity (lower triangle).
    hess_rows: Vec<i32>,
    hess_cols: Vec<i32>,
    hess_map: HashMap<(usize, usize), usize>,
    /// Initial point.
    x0: Vec<f64>,
    /// Per-bus real power demand (MW), computed from loads and power injections.
    bus_pd_mw: Vec<f64>,
    /// Per-bus reactive power demand (MVAr), computed from loads and power injections.
    bus_qd_mvar: Vec<f64>,
}

impl<'a> OrpdProblem<'a> {
    fn new(network: &'a Network, options: &'a OrpdOptions) -> Result<Self, String> {
        let bus_map = network.bus_index_map();
        let base = network.base_mva;

        // Build all in-service branch admittances (for objective / constraint eval).
        let all_branches = build_branch_adm(network, &bus_map);

        // Identify constrained branch indices (index into all_branches vec).
        let constrained_branch_local_indices: Vec<usize> = if options.enforce_thermal_limits {
            all_branches
                .iter()
                .enumerate()
                .filter(|(_, ba)| {
                    ba.s_max_sq > 0.0
                        && ba.s_max_sq.is_finite()
                        && (ba.s_max_sq.sqrt() * base) >= options.min_rate_a
                })
                .map(|(i, _)| i)
                .collect()
        } else {
            vec![]
        };

        let constrained_admittances: Vec<BranchAdm> = constrained_branch_local_indices
            .iter()
            .map(|&i| {
                let ba = &all_branches[i];
                BranchAdm {
                    from: ba.from,
                    to: ba.to,
                    g_ff: ba.g_ff,
                    b_ff: ba.b_ff,
                    g_ft: ba.g_ft,
                    b_ft: ba.b_ft,
                    g_tt: ba.g_tt,
                    b_tt: ba.b_tt,
                    g_tf: ba.g_tf,
                    b_tf: ba.b_tf,
                    g_series: ba.g_series,
                    s_max_sq: ba.s_max_sq,
                }
            })
            .collect();

        let mapping = OrpdMapping::new(network, constrained_branch_local_indices)?;

        // Fixed Pg values: use generator's current pg, clamped to [pmin, pmax].
        let pg_fixed_pu: Vec<f64> = mapping
            .gen_indices
            .iter()
            .map(|&gi| {
                let g = &network.generators[gi];
                (g.p / base).clamp(g.pmin / base, g.pmax / base)
            })
            .collect();

        let ybus = build_ybus(network);

        // Build sparsity structures.
        let (jac_rows, jac_cols) =
            build_jac_sparsity(&mapping, &ybus, &constrained_admittances, network);
        let (hess_rows, hess_cols, hess_map) = build_hess_sparsity(
            &mapping,
            &ybus,
            &all_branches,
            &constrained_admittances,
            options,
        );

        // Build initial point.
        let x0 = build_x0(network, &mapping, &pg_fixed_pu, base);

        Ok(Self {
            network,
            ybus,
            mapping,
            all_branches,
            constrained_admittances,
            base_mva: base,
            options,
            pg_fixed_pu,
            jac_rows,
            jac_cols,
            hess_rows,
            hess_cols,
            hess_map,
            x0,
            bus_pd_mw: network.bus_load_p_mw(),
            bus_qd_mvar: network.bus_load_q_mvar(),
        })
    }

    /// Compute total active power losses (in pu) at the current voltage point.
    fn compute_losses_pu(&self, vm: &[f64], va: &[f64]) -> f64 {
        // P_loss = Σ_k (Pf_k + Pt_k) using the exact pi-circuit power flow formulae.
        // Pf and Pt include the shunt charging contribution; their sum gives real losses.
        let mut loss = 0.0_f64;
        for ba in &self.all_branches {
            let (pf, _qf) = flow_from(ba, vm, va);
            let (pt, _qt) = flow_to(ba, vm, va);
            loss += pf + pt;
        }
        loss
    }

    /// Helper to add a Hessian entry using the index map.
    fn add_hess(&self, values: &mut [f64], r: usize, c: usize, val: f64) {
        let (r, c) = if r >= c { (r, c) } else { (c, r) };
        if let Some(&pos) = self.hess_map.get(&(r, c)) {
            values[pos] += val;
        }
    }
}

impl NlpProblem for OrpdProblem<'_> {
    fn n_vars(&self) -> usize {
        self.mapping.n_var
    }

    fn n_constraints(&self) -> usize {
        self.mapping.n_con
    }

    fn var_bounds(&self) -> (Vec<f64>, Vec<f64>) {
        let m = &self.mapping;
        let base = self.base_mva;
        let mut lb = vec![0.0; m.n_var];
        let mut ub = vec![0.0; m.n_var];

        // Va bounds: [-π, π] for non-slack buses.
        for i in 0..m.n_bus {
            if let Some(idx) = m.va_var(i) {
                lb[idx] = -std::f64::consts::PI;
                ub[idx] = std::f64::consts::PI;
            }
        }

        // Vm bounds: [vmin, vmax] per bus.
        for i in 0..m.n_bus {
            let idx = m.vm_var(i);
            lb[idx] = self.network.buses[i].voltage_min_pu;
            ub[idx] = self.network.buses[i].voltage_max_pu;
        }

        // Pg bounds.
        for j in 0..m.n_gen {
            let gi = m.gen_indices[j];
            let g = &self.network.generators[gi];
            let idx = m.pg_var(j);
            if self.options.fix_pg {
                // Fix Pg at current dispatch — collapsed bounds.
                lb[idx] = self.pg_fixed_pu[j];
                ub[idx] = self.pg_fixed_pu[j];
            } else {
                lb[idx] = g.pmin / base;
                ub[idx] = g.pmax / base;
            }
        }

        // Qg bounds: [qmin/base, qmax/base].
        for j in 0..m.n_gen {
            let gi = m.gen_indices[j];
            let g = &self.network.generators[gi];
            let idx = m.qg_var(j);
            let qmin = if g.qmin.abs() > 1e10 { -9999.0 } else { g.qmin };
            let qmax = if g.qmax.abs() > 1e10 { 9999.0 } else { g.qmax };
            lb[idx] = qmin / base;
            ub[idx] = qmax / base;
        }

        (lb, ub)
    }

    fn constraint_bounds(&self) -> (Vec<f64>, Vec<f64>) {
        let m = &self.mapping;
        let mut gl = vec![0.0; m.n_con];
        let mut gu = vec![0.0; m.n_con];

        // P and Q balance: equality (0, 0) — already set.

        // Branch flow limits.
        let n_br = self.constrained_admittances.len();
        for (ci, ba) in self.constrained_admittances.iter().enumerate() {
            gl[2 * m.n_bus + ci] = f64::NEG_INFINITY;
            gu[2 * m.n_bus + ci] = ba.s_max_sq;
            gl[2 * m.n_bus + n_br + ci] = f64::NEG_INFINITY;
            gu[2 * m.n_bus + n_br + ci] = ba.s_max_sq;
        }

        (gl, gu)
    }

    fn initial_point(&self) -> Vec<f64> {
        self.x0.clone()
    }

    fn eval_objective(&self, x: &[f64]) -> f64 {
        let m = &self.mapping;
        let (va, vm, _pg, _qg) = m.unpack(x);
        match &self.options.objective {
            OrpdObjective::MinimizeLosses => {
                // P_loss = Σ_k (Pf_k + Pt_k) in pu.
                self.compute_losses_pu(vm, &va)
            }
            OrpdObjective::MinimizeVoltageDeviation { v_ref } => {
                // Σ_i (Vi - v_ref)²
                vm.iter().map(|&v| (v - v_ref) * (v - v_ref)).sum()
            }
            OrpdObjective::MinimizeCombined {
                loss_weight,
                vdev_weight,
                v_ref,
            } => {
                let losses = self.compute_losses_pu(vm, &va);
                let vdev: f64 = vm.iter().map(|&v| (v - v_ref) * (v - v_ref)).sum();
                loss_weight * losses + vdev_weight * vdev
            }
        }
    }

    fn eval_gradient(&self, x: &[f64], grad: &mut [f64]) {
        let m = &self.mapping;
        let (va, vm, _pg, _qg) = m.unpack(x);
        grad.fill(0.0);

        match &self.options.objective {
            OrpdObjective::MinimizeLosses => {
                // P_loss = Σ_k (Pf_k + Pt_k)
                // dPf/dVa_f = Vi*Vj*(-G_ft*sin + B_ft*cos) = Vi*Vj*a_val
                // dPf/dVa_t = Vi*Vj*(G_ft*sin - B_ft*cos) = Vi*Vj*d_val
                // dPf/dVm_f = 2*Vi*G_ff + Vj*(G_ft*cos + B_ft*sin)
                // dPf/dVm_t = Vi*(G_ft*cos + B_ft*sin)
                // Similarly for Pt.
                for ba in &self.all_branches {
                    let vf = vm[ba.from];
                    let vt = vm[ba.to];
                    let theta = va[ba.from] - va[ba.to];
                    let (sin_t, cos_t) = theta.sin_cos();
                    let theta_tf = -theta;
                    let (sin_tf, cos_tf) = theta_tf.sin_cos();

                    // From-side gradient contributions (dPf/d*)
                    let dpf_dvaf = vf * vt * (-ba.g_ft * sin_t + ba.b_ft * cos_t);
                    let dpf_dvat = vf * vt * (ba.g_ft * sin_t - ba.b_ft * cos_t);
                    let dpf_dvmf = 2.0 * vf * ba.g_ff + vt * (ba.g_ft * cos_t + ba.b_ft * sin_t);
                    let dpf_dvmt = vf * (ba.g_ft * cos_t + ba.b_ft * sin_t);

                    // To-side gradient contributions (dPt/d*)
                    // theta_tf = Va_t - Va_f, d(theta_tf)/dVa_f = -1
                    let dpt_dvaf = vt * vf * (ba.g_tf * sin_tf - ba.b_tf * cos_tf);
                    let dpt_dvat = vt * vf * (-ba.g_tf * sin_tf + ba.b_tf * cos_tf);
                    let dpt_dvmf = vt * (ba.g_tf * cos_tf + ba.b_tf * sin_tf);
                    let dpt_dvmt = 2.0 * vt * ba.g_tt + vf * (ba.g_tf * cos_tf + ba.b_tf * sin_tf);

                    let dloss_dvaf = dpf_dvaf + dpt_dvaf;
                    let dloss_dvat = dpf_dvat + dpt_dvat;
                    let dloss_dvmf = dpf_dvmf + dpt_dvmf;
                    let dloss_dvmt = dpf_dvmt + dpt_dvmt;

                    if let Some(va_f) = m.va_var(ba.from) {
                        grad[va_f] += dloss_dvaf;
                    }
                    if let Some(va_t) = m.va_var(ba.to) {
                        grad[va_t] += dloss_dvat;
                    }
                    grad[m.vm_var(ba.from)] += dloss_dvmf;
                    grad[m.vm_var(ba.to)] += dloss_dvmt;
                }
            }

            OrpdObjective::MinimizeVoltageDeviation { v_ref } => {
                // df/dVm_i = 2*(Vi - v_ref)
                for i in 0..m.n_bus {
                    grad[m.vm_var(i)] += 2.0 * (vm[i] - v_ref);
                }
            }

            OrpdObjective::MinimizeCombined {
                loss_weight,
                vdev_weight,
                v_ref,
            } => {
                // Loss gradient
                for ba in &self.all_branches {
                    let vf = vm[ba.from];
                    let vt = vm[ba.to];
                    let theta = va[ba.from] - va[ba.to];
                    let (sin_t, cos_t) = theta.sin_cos();
                    let theta_tf = -theta;
                    let (sin_tf, cos_tf) = theta_tf.sin_cos();

                    let dpf_dvaf = vf * vt * (-ba.g_ft * sin_t + ba.b_ft * cos_t);
                    let dpf_dvat = vf * vt * (ba.g_ft * sin_t - ba.b_ft * cos_t);
                    let dpf_dvmf = 2.0 * vf * ba.g_ff + vt * (ba.g_ft * cos_t + ba.b_ft * sin_t);
                    let dpf_dvmt = vf * (ba.g_ft * cos_t + ba.b_ft * sin_t);

                    let dpt_dvaf = vt * vf * (ba.g_tf * sin_tf - ba.b_tf * cos_tf);
                    let dpt_dvat = vt * vf * (-ba.g_tf * sin_tf + ba.b_tf * cos_tf);
                    let dpt_dvmf = vt * (ba.g_tf * cos_tf + ba.b_tf * sin_tf);
                    let dpt_dvmt = 2.0 * vt * ba.g_tt + vf * (ba.g_tf * cos_tf + ba.b_tf * sin_tf);

                    if let Some(va_f) = m.va_var(ba.from) {
                        grad[va_f] += loss_weight * (dpf_dvaf + dpt_dvaf);
                    }
                    if let Some(va_t) = m.va_var(ba.to) {
                        grad[va_t] += loss_weight * (dpf_dvat + dpt_dvat);
                    }
                    grad[m.vm_var(ba.from)] += loss_weight * (dpf_dvmf + dpt_dvmf);
                    grad[m.vm_var(ba.to)] += loss_weight * (dpf_dvmt + dpt_dvmt);
                }
                // Vdev gradient
                for i in 0..m.n_bus {
                    grad[m.vm_var(i)] += vdev_weight * 2.0 * (vm[i] - v_ref);
                }
            }
        }
        // Note: dObj/dPg = 0 (loss objective doesn't directly depend on Pg variable;
        // it appears only through the power balance constraints which are in the Lagrangian).
        // dObj/dQg = 0 for all objectives.
    }

    fn eval_constraints(&self, x: &[f64], g: &mut [f64]) {
        let m = &self.mapping;
        let (va, vm, pg, qg) = m.unpack(x);

        let (p_calc, q_calc) = compute_power_injection(&self.ybus, vm, &va);

        // P-balance: P_calc[i] - Σ Pg_at_bus_i + Pd_i/base = 0
        for i in 0..m.n_bus {
            let mut p_gen = 0.0;
            for &lj in &m.bus_gen_map[i] {
                p_gen += pg[lj];
            }
            g[i] = p_calc[i] - p_gen + self.bus_pd_mw[i] / self.base_mva;
        }

        // Q-balance: Q_calc[i] - Σ Qg_at_bus_i + Qd_i/base = 0
        for i in 0..m.n_bus {
            let mut q_gen = 0.0;
            for &lj in &m.bus_gen_map[i] {
                q_gen += qg[lj];
            }
            g[m.n_bus + i] = q_calc[i] - q_gen + self.bus_qd_mvar[i] / self.base_mva;
        }

        // Branch flow limits (from-side): Pf² + Qf² ≤ s_max²
        let n_br = self.constrained_admittances.len();
        for (ci, ba) in self.constrained_admittances.iter().enumerate() {
            let (pf, qf) = flow_from(ba, vm, &va);
            g[2 * m.n_bus + ci] = pf * pf + qf * qf;
        }

        // Branch flow limits (to-side): Pt² + Qt² ≤ s_max²
        for (ci, ba) in self.constrained_admittances.iter().enumerate() {
            let (pt, qt) = flow_to(ba, vm, &va);
            g[2 * m.n_bus + n_br + ci] = pt * pt + qt * qt;
        }
    }

    fn jacobian_structure(&self) -> (Vec<i32>, Vec<i32>) {
        (self.jac_rows.clone(), self.jac_cols.clone())
    }

    fn eval_jacobian(&self, x: &[f64], values: &mut [f64]) {
        let m = &self.mapping;
        let (va, vm, _pg, _qg) = m.unpack(x);
        values.fill(0.0);

        // Pre-compute P_i, Q_i injections.
        let mut p_inj = vec![0.0_f64; m.n_bus];
        let mut q_inj = vec![0.0_f64; m.n_bus];
        for i in 0..m.n_bus {
            let row = self.ybus.row(i);
            let vm_i = vm[i];
            for (k, &j) in row.col_idx.iter().enumerate() {
                let theta = va[i] - va[j];
                let (sin_t, cos_t) = theta.sin_cos();
                p_inj[i] += vm[j] * (row.g[k] * cos_t + row.b[k] * sin_t);
                q_inj[i] += vm[j] * (row.g[k] * sin_t - row.b[k] * cos_t);
            }
            p_inj[i] *= vm_i;
            q_inj[i] *= vm_i;
        }

        let mut idx = 0;

        // --- P-balance Jacobian (rows 0..n_bus) ---
        for i in 0..m.n_bus {
            let row_ybus = self.ybus.row(i);
            let vm_i = vm[i];

            // dP/dVa off-diagonal
            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue;
                }
                if m.va_var(j).is_some() {
                    let g_ij = row_ybus.g[k];
                    let b_ij = row_ybus.b[k];
                    let theta = va[i] - va[j];
                    let (sin_t, cos_t) = theta.sin_cos();
                    values[idx] = vm_i * vm[j] * (g_ij * sin_t - b_ij * cos_t);
                    idx += 1;
                }
            }
            // dP/dVa_i diagonal = -Q_i - B_ii*Vi²
            if m.va_var(i).is_some() {
                let b_ii = self.ybus.b(i, i);
                values[idx] = -q_inj[i] - b_ii * vm_i * vm_i;
                idx += 1;
            }
            // dP/dVm off-diagonal
            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue;
                }
                let g_ij = row_ybus.g[k];
                let b_ij = row_ybus.b[k];
                let theta = va[i] - va[j];
                let (sin_t, cos_t) = theta.sin_cos();
                values[idx] = vm_i * (g_ij * cos_t + b_ij * sin_t);
                idx += 1;
            }
            // dP/dVm_i diagonal = P_i/Vm_i + G_ii*Vi
            {
                let g_ii = self.ybus.g(i, i);
                values[idx] = p_inj[i] / vm_i + g_ii * vm_i;
                idx += 1;
            }
            // dP/dPg: -1 for each gen at bus i
            for &_lj in &m.bus_gen_map[i] {
                values[idx] = -1.0;
                idx += 1;
            }
        }

        // --- Q-balance Jacobian (rows n_bus..2*n_bus) ---
        for i in 0..m.n_bus {
            let row_ybus = self.ybus.row(i);
            let vm_i = vm[i];

            // dQ/dVa off-diagonal
            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue;
                }
                if m.va_var(j).is_some() {
                    let g_ij = row_ybus.g[k];
                    let b_ij = row_ybus.b[k];
                    let theta = va[i] - va[j];
                    let (sin_t, cos_t) = theta.sin_cos();
                    values[idx] = -vm_i * vm[j] * (g_ij * cos_t + b_ij * sin_t);
                    idx += 1;
                }
            }
            // dQ/dVa_i diagonal = P_i - G_ii*Vi²
            if m.va_var(i).is_some() {
                let g_ii = self.ybus.g(i, i);
                values[idx] = p_inj[i] - g_ii * vm_i * vm_i;
                idx += 1;
            }
            // dQ/dVm off-diagonal
            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue;
                }
                let g_ij = row_ybus.g[k];
                let b_ij = row_ybus.b[k];
                let theta = va[i] - va[j];
                let (sin_t, cos_t) = theta.sin_cos();
                values[idx] = vm_i * (g_ij * sin_t - b_ij * cos_t);
                idx += 1;
            }
            // dQ/dVm_i diagonal = Q_i/Vm_i - B_ii*Vi
            {
                let b_ii = self.ybus.b(i, i);
                values[idx] = q_inj[i] / vm_i - b_ii * vm_i;
                idx += 1;
            }
            // dQ/dQg: -1 for each gen at bus i
            for &_lj in &m.bus_gen_map[i] {
                values[idx] = -1.0;
                idx += 1;
            }
        }

        // --- Branch flow Jacobian (from-side) ---
        for ba in &self.constrained_admittances {
            let f = ba.from;
            let t = ba.to;
            let vi = vm[f];
            let vj = vm[t];
            let theta = va[f] - va[t];
            let (sin_t, cos_t) = theta.sin_cos();

            let pf = vi * vi * ba.g_ff + vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
            let qf = -vi * vi * ba.b_ff + vi * vj * (ba.g_ft * sin_t - ba.b_ft * cos_t);

            let dpf_dvaf = vi * vj * (-ba.g_ft * sin_t + ba.b_ft * cos_t);
            let dpf_dvat = vi * vj * (ba.g_ft * sin_t - ba.b_ft * cos_t);
            let dpf_dvmf = 2.0 * vi * ba.g_ff + vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
            let dpf_dvmt = vi * (ba.g_ft * cos_t + ba.b_ft * sin_t);

            let dqf_dvaf = vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
            let dqf_dvat = -vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
            let dqf_dvmf = -2.0 * vi * ba.b_ff + vj * (ba.g_ft * sin_t - ba.b_ft * cos_t);
            let dqf_dvmt = vi * (ba.g_ft * sin_t - ba.b_ft * cos_t);

            if m.va_var(f).is_some() {
                values[idx] = 2.0 * (pf * dpf_dvaf + qf * dqf_dvaf);
                idx += 1;
            }
            if m.va_var(t).is_some() {
                values[idx] = 2.0 * (pf * dpf_dvat + qf * dqf_dvat);
                idx += 1;
            }
            values[idx] = 2.0 * (pf * dpf_dvmf + qf * dqf_dvmf);
            idx += 1;
            values[idx] = 2.0 * (pf * dpf_dvmt + qf * dqf_dvmt);
            idx += 1;
        }

        // --- Branch flow Jacobian (to-side) ---
        for ba in &self.constrained_admittances {
            let f = ba.from;
            let t = ba.to;
            let vi = vm[f];
            let vj = vm[t];
            let theta_tf = va[t] - va[f];
            let (sin_t, cos_t) = theta_tf.sin_cos();

            let pt = vj * vj * ba.g_tt + vj * vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);
            let qt = -vj * vj * ba.b_tt + vj * vi * (ba.g_tf * sin_t - ba.b_tf * cos_t);

            let dpt_dvaf = vj * vi * (ba.g_tf * sin_t - ba.b_tf * cos_t);
            let dpt_dvat = vj * vi * (-ba.g_tf * sin_t + ba.b_tf * cos_t);
            let dpt_dvmf = vj * (ba.g_tf * cos_t + ba.b_tf * sin_t);
            let dpt_dvmt = 2.0 * vj * ba.g_tt + vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);

            let dqt_dvaf = -vj * vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);
            let dqt_dvat = vj * vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);
            let dqt_dvmf = vj * (ba.g_tf * sin_t - ba.b_tf * cos_t);
            let dqt_dvmt = -2.0 * vj * ba.b_tt + vi * (ba.g_tf * sin_t - ba.b_tf * cos_t);

            if m.va_var(f).is_some() {
                values[idx] = 2.0 * (pt * dpt_dvaf + qt * dqt_dvaf);
                idx += 1;
            }
            if m.va_var(t).is_some() {
                values[idx] = 2.0 * (pt * dpt_dvat + qt * dqt_dvat);
                idx += 1;
            }
            values[idx] = 2.0 * (pt * dpt_dvmf + qt * dqt_dvmf);
            idx += 1;
            values[idx] = 2.0 * (pt * dpt_dvmt + qt * dqt_dvmt);
            idx += 1;
        }

        debug_assert_eq!(
            idx,
            values.len(),
            "ORPD Jacobian fill mismatch: idx={idx}, expected={}",
            values.len()
        );
    }

    fn has_hessian(&self) -> bool {
        self.options.exact_hessian
    }

    fn hessian_structure(&self) -> (Vec<i32>, Vec<i32>) {
        (self.hess_rows.clone(), self.hess_cols.clone())
    }

    fn eval_hessian(&self, x: &[f64], obj_factor: f64, lambda: &[f64], values: &mut [f64]) {
        let m = &self.mapping;
        let (va, vm, _pg, _qg) = m.unpack(x);
        values.fill(0.0);

        // ----------------------------------------------------------------
        // Objective Hessian
        // ----------------------------------------------------------------
        match &self.options.objective {
            OrpdObjective::MinimizeLosses => {
                // ∇²P_loss[i][j] in (Va, Vm) variables.
                // For each branch k: P_loss_k = Pf_k + Pt_k
                // The second derivatives are analogous to the AC-OPF branch flow Hessian,
                // but for the objective (not a constraint).
                self.add_loss_hessian(vm, &va, obj_factor, values);
            }
            OrpdObjective::MinimizeVoltageDeviation { .. } => {
                // ∇²Σ(Vi-v_ref)² = 2*I in Vm variables (diagonal).
                for i in 0..m.n_bus {
                    let vmi = m.vm_var(i);
                    self.add_hess(values, vmi, vmi, obj_factor * 2.0);
                }
            }
            OrpdObjective::MinimizeCombined {
                loss_weight,
                vdev_weight,
                ..
            } => {
                // Loss Hessian
                self.add_loss_hessian(vm, &va, obj_factor * loss_weight, values);
                // Vdev Hessian: diagonal 2*vdev_weight in Vm variables
                for i in 0..m.n_bus {
                    let vmi = m.vm_var(i);
                    self.add_hess(values, vmi, vmi, obj_factor * vdev_weight * 2.0);
                }
            }
        }

        // ----------------------------------------------------------------
        // Power balance Hessian (identical to AC-OPF power balance block)
        // ----------------------------------------------------------------
        for i in 0..m.n_bus {
            let lp = lambda[i];
            let lq = lambda[m.n_bus + i];

            let row_ybus = self.ybus.row(i);
            let vi = vm[i];
            let g_ii = self.ybus.g(i, i);
            let b_ii = self.ybus.b(i, i);

            // Vm diagonal: d²P_i/dVi² = 2*Gii, d²Q_i/dVi² = -2*Bii
            let vmi = m.vm_var(i);
            self.add_hess(values, vmi, vmi, lp * 2.0 * g_ii + lq * (-2.0 * b_ii));

            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue;
                }
                let gij = row_ybus.g[k];
                let bij = row_ybus.b[k];
                let theta_ij = va[i] - va[j];
                let (sin_t, cos_t) = theta_ij.sin_cos();
                let aij = gij * cos_t + bij * sin_t;
                let dij = gij * sin_t - bij * cos_t;
                let vj = vm[j];
                let vivj = vi * vj;

                let vai_opt = m.va_var(i);
                let vaj_opt = m.va_var(j);
                let vmj = m.vm_var(j);

                // VaVa block
                if let Some(vaj) = vaj_opt {
                    self.add_hess(values, vaj, vaj, lp * (-vivj * aij) + lq * (-vivj * dij));
                }
                if let (Some(vai), Some(vaj)) = (vai_opt, vaj_opt) {
                    self.add_hess(values, vai, vaj, lp * (vivj * aij) + lq * (vivj * dij));
                }
                if let Some(vai) = vai_opt {
                    self.add_hess(values, vai, vai, lp * (-vivj * aij) + lq * (-vivj * dij));
                }
                // VmVa cross-block
                if let Some(vaj) = vaj_opt {
                    self.add_hess(values, vmi, vaj, lp * (vj * dij) + lq * (-vj * aij));
                }
                if let Some(vaj) = vaj_opt {
                    self.add_hess(values, vmj, vaj, lp * (vi * dij) + lq * (-vi * aij));
                }
                if let Some(vai) = vai_opt {
                    self.add_hess(values, vmj, vai, lp * (-vi * dij) + lq * (vi * aij));
                }
                if let Some(vai) = vai_opt {
                    self.add_hess(values, vmi, vai, lp * (-vj * dij) + lq * (vj * aij));
                }
                // VmVm block
                self.add_hess(values, vmi, vmj, lp * aij + lq * dij);
            }
        }

        // ----------------------------------------------------------------
        // Branch flow constraint Hessian (from-side and to-side)
        // ----------------------------------------------------------------
        let n_br = self.constrained_admittances.len();
        // From-side
        for (ci, ba) in self.constrained_admittances.iter().enumerate() {
            let mu = lambda[2 * m.n_bus + ci];
            if mu.abs() < 1e-30 {
                continue;
            }
            self.add_branch_flow_hess_from(ba, vm, &va, mu, values);
        }
        // To-side
        for (ci, ba) in self.constrained_admittances.iter().enumerate() {
            let mu = lambda[2 * m.n_bus + n_br + ci];
            if mu.abs() < 1e-30 {
                continue;
            }
            self.add_branch_flow_hess_to(ba, vm, &va, mu, values);
        }
    }
}

impl OrpdProblem<'_> {
    /// Add loss objective Hessian contributions with given weight.
    ///
    /// P_loss = Σ_k (Pf_k + Pt_k)
    /// The Hessian ∇²P_loss is the sum of ∇²Pf_k + ∇²Pt_k across all branches.
    fn add_loss_hessian(&self, vm: &[f64], va: &[f64], weight: f64, values: &mut [f64]) {
        let m = &self.mapping;
        for ba in &self.all_branches {
            let vf = vm[ba.from];
            let vt = vm[ba.to];
            let theta = va[ba.from] - va[ba.to];
            let (sin_t, cos_t) = theta.sin_cos();

            // Second derivatives of Pf = Vf²*Gff + Vf*Vt*(Gft*cos + Bft*sin):
            // ∂²Pf/∂θf² = -Vf*Vt*(Gft*cos + Bft*sin) = -Vf*Vt*c
            // ∂²Pf/∂θf∂θt = Vf*Vt*c
            // ∂²Pf/∂θt² = -Vf*Vt*c
            // ∂²Pf/∂θf∂Vmf = Vt*(-Gft*sin+Bft*cos) = Vt*a
            // ∂²Pf/∂θf∂Vmt = Vf*(-Gft*sin+Bft*cos) = Vf*a
            // ∂²Pf/∂θt∂Vmf = -Vt*a
            // ∂²Pf/∂θt∂Vmt = -Vf*a
            // ∂²Pf/∂Vmf² = 2*Gff
            // ∂²Pf/∂Vmf∂Vmt = Gft*cos + Bft*sin = c
            // ∂²Pf/∂Vmt² = 0

            let c_val = ba.g_ft * cos_t + ba.b_ft * sin_t; // = aij
            let a_val = -ba.g_ft * sin_t + ba.b_ft * cos_t; // = -dij (sign)

            let vai_opt = m.va_var(ba.from);
            let vaj_opt = m.va_var(ba.to);
            let vmi_idx = m.vm_var(ba.from);
            let vmj_idx = m.vm_var(ba.to);
            let vf_vt = vf * vt;

            // VaVa block for Pf
            if let Some(vai) = vai_opt {
                self.add_hess(values, vai, vai, weight * (-vf_vt * c_val));
            }
            if let (Some(vai), Some(vaj)) = (vai_opt, vaj_opt) {
                self.add_hess(values, vai, vaj, weight * (vf_vt * c_val));
            }
            if let Some(vaj) = vaj_opt {
                self.add_hess(values, vaj, vaj, weight * (-vf_vt * c_val));
            }
            // VmVa cross block for Pf
            if let Some(vai) = vai_opt {
                self.add_hess(values, vmi_idx, vai, weight * (vt * a_val));
                self.add_hess(values, vmj_idx, vai, weight * (vf * a_val));
            }
            if let Some(vaj) = vaj_opt {
                self.add_hess(values, vmi_idx, vaj, weight * (-vt * a_val));
                self.add_hess(values, vmj_idx, vaj, weight * (-vf * a_val));
            }
            // VmVm block for Pf
            self.add_hess(values, vmi_idx, vmi_idx, weight * 2.0 * ba.g_ff);
            self.add_hess(values, vmi_idx, vmj_idx, weight * c_val);
            // ∂²Pf/∂Vmt² = 0 — no contribution

            // Add contributions from Pt = Vt²*Gtt + Vt*Vf*(Gtf*cos_tf + Btf*sin_tf)
            // theta_tf = Va_t - Va_f. Pattern matches ac_opf.rs to-side Hessian block.
            // a_tf = -Gtf*sin_tf + Btf*cos_tf, c_tf = Gtf*cos_tf + Btf*sin_tf
            // d_val_to = -a_tf = Gtf*sin_tf - Btf*cos_tf
            let theta_tf = -theta;
            let (sin_tf, cos_tf) = theta_tf.sin_cos();
            let c_tf = ba.g_tf * cos_tf + ba.b_tf * sin_tf;
            let a_tf = -ba.g_tf * sin_tf + ba.b_tf * cos_tf;
            let d_val_to = -a_tf; // = Gtf*sin_tf - Btf*cos_tf

            // VaVa block for Pt
            if let Some(vai) = vai_opt {
                self.add_hess(values, vai, vai, weight * (-vf_vt * c_tf));
            }
            if let (Some(vai), Some(vaj)) = (vai_opt, vaj_opt) {
                self.add_hess(values, vai, vaj, weight * (vf_vt * c_tf));
            }
            if let Some(vaj) = vaj_opt {
                self.add_hess(values, vaj, vaj, weight * (-vf_vt * c_tf));
            }
            // VmVa cross block for Pt
            if let Some(vai) = vai_opt {
                self.add_hess(values, vmi_idx, vai, weight * (vt * d_val_to));
                self.add_hess(values, vmj_idx, vai, weight * (vf * d_val_to));
            }
            if let Some(vaj) = vaj_opt {
                self.add_hess(values, vmi_idx, vaj, weight * (-vt * d_val_to));
                self.add_hess(values, vmj_idx, vaj, weight * (-vf * d_val_to));
            }
            // VmVm block for Pt
            self.add_hess(values, vmi_idx, vmj_idx, weight * c_tf);
            self.add_hess(values, vmj_idx, vmj_idx, weight * 2.0 * ba.g_tt);
        }
    }

    /// Add from-side branch flow constraint Hessian: ∇²(Pf²+Qf²) * mu.
    fn add_branch_flow_hess_from(
        &self,
        ba: &BranchAdm,
        vm: &[f64],
        va: &[f64],
        mu: f64,
        values: &mut [f64],
    ) {
        let m = &self.mapping;
        let vf = vm[ba.from];
        let vt = vm[ba.to];
        let theta = va[ba.from] - va[ba.to];
        let (sin_t, cos_t) = theta.sin_cos();

        let a_val = -ba.g_ft * sin_t + ba.b_ft * cos_t;
        let c_val = ba.g_ft * cos_t + ba.b_ft * sin_t;
        let d_val = ba.g_ft * sin_t - ba.b_ft * cos_t;
        let vf_vt = vf * vt;

        let pf = vf * vf * ba.g_ff + vf_vt * c_val;
        let qf = -vf * vf * ba.b_ff + vf_vt * d_val;

        let dpf = [
            vf_vt * a_val,
            -vf_vt * a_val,
            2.0 * vf * ba.g_ff + vt * c_val,
            vf * c_val,
        ];
        let dqf = [
            vf_vt * c_val,
            -vf_vt * c_val,
            -2.0 * vf * ba.b_ff + vt * d_val,
            vf * d_val,
        ];

        let d2pf = [
            [-vf_vt * c_val, vf_vt * c_val, vt * a_val, vf * a_val],
            [vf_vt * c_val, -vf_vt * c_val, -vt * a_val, -vf * a_val],
            [vt * a_val, -vt * a_val, 2.0 * ba.g_ff, c_val],
            [vf * a_val, -vf * a_val, c_val, 0.0],
        ];
        let d2qf = [
            [vf_vt * a_val, -vf_vt * a_val, vt * c_val, vf * c_val],
            [-vf_vt * a_val, vf_vt * a_val, -vt * c_val, -vf * c_val],
            [vt * c_val, -vt * c_val, -2.0 * ba.b_ff, d_val],
            [vf * c_val, -vf * c_val, d_val, 0.0],
        ];

        let vars: [Option<usize>; 4] = [
            m.va_var(ba.from),
            m.va_var(ba.to),
            Some(m.vm_var(ba.from)),
            Some(m.vm_var(ba.to)),
        ];

        for a_idx in 0..4 {
            let Some(var_a) = vars[a_idx] else { continue };
            for b_idx in 0..=a_idx {
                let Some(var_b) = vars[b_idx] else { continue };
                let h_val = 2.0
                    * mu
                    * (dpf[a_idx] * dpf[b_idx]
                        + dqf[a_idx] * dqf[b_idx]
                        + pf * d2pf[a_idx][b_idx]
                        + qf * d2qf[a_idx][b_idx]);
                self.add_hess(values, var_a, var_b, h_val);
            }
        }
    }

    /// Add to-side branch flow constraint Hessian: ∇²(Pt²+Qt²) * mu.
    fn add_branch_flow_hess_to(
        &self,
        ba: &BranchAdm,
        vm: &[f64],
        va: &[f64],
        mu: f64,
        values: &mut [f64],
    ) {
        let m = &self.mapping;
        let vf = vm[ba.from];
        let vt = vm[ba.to];
        let theta_tf = va[ba.to] - va[ba.from];
        let (sin_t, cos_t) = theta_tf.sin_cos();

        let a_val = -ba.g_tf * sin_t + ba.b_tf * cos_t;
        let c_val = ba.g_tf * cos_t + ba.b_tf * sin_t;
        let d_val = ba.g_tf * sin_t - ba.b_tf * cos_t;
        let vf_vt = vf * vt;

        let pt = vt * vt * ba.g_tt + vf_vt * c_val;
        let qt = -vt * vt * ba.b_tt + vf_vt * d_val;

        let dpt = [
            vf_vt * d_val,
            vf_vt * a_val,
            vt * c_val,
            2.0 * vt * ba.g_tt + vf * c_val,
        ];
        let dqt = [
            -vf_vt * c_val,
            vf_vt * c_val,
            vt * d_val,
            -2.0 * vt * ba.b_tt + vf * d_val,
        ];

        // Second derivatives of Pt (from ac_opf.rs to-side block)
        let d2pt = [
            [-vf_vt * a_val, vf_vt * a_val, vt * d_val, vf * d_val],
            [vf_vt * a_val, -vf_vt * a_val, -vt * d_val, -vf * d_val],
            [vt * d_val, -vt * d_val, 0.0, c_val],
            [vf * d_val, -vf * d_val, c_val, 2.0 * ba.g_tt],
        ];
        let d2qt = [
            [vf_vt * c_val, -vf_vt * c_val, -vt * c_val, -vf * c_val],
            [-vf_vt * c_val, vf_vt * c_val, vt * c_val, vf * c_val],
            [-vt * c_val, vt * c_val, 0.0, d_val],
            [-vf * c_val, vf * c_val, d_val, -2.0 * ba.b_tt],
        ];

        let vars: [Option<usize>; 4] = [
            m.va_var(ba.from),
            m.va_var(ba.to),
            Some(m.vm_var(ba.from)),
            Some(m.vm_var(ba.to)),
        ];

        for a_idx in 0..4 {
            let Some(var_a) = vars[a_idx] else { continue };
            for b_idx in 0..=a_idx {
                let Some(var_b) = vars[b_idx] else { continue };
                let h_val = 2.0
                    * mu
                    * (dpt[a_idx] * dpt[b_idx]
                        + dqt[a_idx] * dqt[b_idx]
                        + pt * d2pt[a_idx][b_idx]
                        + qt * d2qt[a_idx][b_idx]);
                self.add_hess(values, var_a, var_b, h_val);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Jacobian sparsity
// ---------------------------------------------------------------------------

fn build_jac_sparsity(
    mapping: &OrpdMapping,
    ybus: &YBus,
    constrained_admittances: &[BranchAdm],
    network: &Network,
) -> (Vec<i32>, Vec<i32>) {
    let m = mapping;
    let mut rows = Vec::new();
    let mut cols = Vec::new();

    // P-balance rows (0..n_bus)
    for i in 0..m.n_bus {
        let row_ybus = ybus.row(i);
        let row = i as i32;

        for &j in row_ybus.col_idx {
            if j == i {
                continue;
            }
            if let Some(va_col) = m.va_var(j) {
                rows.push(row);
                cols.push(va_col as i32);
            }
        }
        if let Some(va_col) = m.va_var(i) {
            rows.push(row);
            cols.push(va_col as i32);
        }
        for &j in row_ybus.col_idx {
            if j == i {
                continue;
            }
            rows.push(row);
            cols.push(m.vm_var(j) as i32);
        }
        rows.push(row);
        cols.push(m.vm_var(i) as i32);
        for &lj in &m.bus_gen_map[i] {
            rows.push(row);
            cols.push(m.pg_var(lj) as i32);
        }
    }

    // Q-balance rows (n_bus..2*n_bus)
    for i in 0..m.n_bus {
        let row_ybus = ybus.row(i);
        let row = (m.n_bus + i) as i32;

        for &j in row_ybus.col_idx {
            if j == i {
                continue;
            }
            if let Some(va_col) = m.va_var(j) {
                rows.push(row);
                cols.push(va_col as i32);
            }
        }
        if let Some(va_col) = m.va_var(i) {
            rows.push(row);
            cols.push(va_col as i32);
        }
        for &j in row_ybus.col_idx {
            if j == i {
                continue;
            }
            rows.push(row);
            cols.push(m.vm_var(j) as i32);
        }
        rows.push(row);
        cols.push(m.vm_var(i) as i32);
        for &lj in &m.bus_gen_map[i] {
            rows.push(row);
            cols.push(m.qg_var(lj) as i32);
        }
    }

    // Branch flow from-side rows (2*n_bus..)
    let n_br = constrained_admittances.len();
    for (ci, ba) in constrained_admittances.iter().enumerate() {
        let row = (2 * m.n_bus + ci) as i32;
        if let Some(va_f) = m.va_var(ba.from) {
            rows.push(row);
            cols.push(va_f as i32);
        }
        if let Some(va_t) = m.va_var(ba.to) {
            rows.push(row);
            cols.push(va_t as i32);
        }
        rows.push(row);
        cols.push(m.vm_var(ba.from) as i32);
        rows.push(row);
        cols.push(m.vm_var(ba.to) as i32);
    }

    // Branch flow to-side rows (2*n_bus + n_br..)
    for (ci, ba) in constrained_admittances.iter().enumerate() {
        let row = (2 * m.n_bus + n_br + ci) as i32;
        if let Some(va_f) = m.va_var(ba.from) {
            rows.push(row);
            cols.push(va_f as i32);
        }
        if let Some(va_t) = m.va_var(ba.to) {
            rows.push(row);
            cols.push(va_t as i32);
        }
        rows.push(row);
        cols.push(m.vm_var(ba.from) as i32);
        rows.push(row);
        cols.push(m.vm_var(ba.to) as i32);
    }

    // Suppress unused-variable warning: network is kept for future use (e.g., angle limits)
    let _ = network;

    (rows, cols)
}

// ---------------------------------------------------------------------------
// Hessian sparsity
// ---------------------------------------------------------------------------

#[allow(clippy::type_complexity)]
fn build_hess_sparsity(
    mapping: &OrpdMapping,
    ybus: &YBus,
    all_branches: &[BranchAdm],
    constrained_admittances: &[BranchAdm],
    options: &OrpdOptions,
) -> (Vec<i32>, Vec<i32>, HashMap<(usize, usize), usize>) {
    let m = mapping;
    let cap = ybus.nnz * 4 + (all_branches.len() + constrained_admittances.len()) * 16 + m.n_gen;
    let mut rows = Vec::with_capacity(cap);
    let mut cols = Vec::with_capacity(cap);
    let mut index_map: HashMap<(usize, usize), usize> = HashMap::with_capacity(cap);

    let mut add_entry = |r: usize, c: usize| {
        let (r, c) = if r >= c { (r, c) } else { (c, r) };
        index_map.entry((r, c)).or_insert_with(|| {
            let idx = rows.len();
            rows.push(r as i32);
            cols.push(c as i32);
            idx
        });
    };

    // Power balance: Y-bus sparsity pattern in (Va, Vm) blocks.
    for i in 0..m.n_bus {
        let row = ybus.row(i);
        for &j in row.col_idx {
            if let (Some(vai), Some(vaj)) = (m.va_var(i), m.va_var(j)) {
                add_entry(vai, vaj);
            }
            if let Some(vaj) = m.va_var(j) {
                add_entry(m.vm_var(i), vaj);
            }
            if let Some(vai) = m.va_var(i) {
                add_entry(m.vm_var(j), vai);
            }
            add_entry(m.vm_var(i), m.vm_var(j));
        }
    }

    // Objective Hessian entries.
    match &options.objective {
        OrpdObjective::MinimizeLosses | OrpdObjective::MinimizeCombined { .. } => {
            // Loss Hessian: same sparsity as power balance (Va,Vm) block.
            for ba in all_branches {
                let va_vars: [Option<usize>; 2] = [m.va_var(ba.from), m.va_var(ba.to)];
                let vm_vars: [usize; 2] = [m.vm_var(ba.from), m.vm_var(ba.to)];
                for &vai in &va_vars {
                    for &vaj in &va_vars {
                        if let (Some(a), Some(b)) = (vai, vaj) {
                            add_entry(a, b);
                        }
                    }
                }
                for &vmi in &vm_vars {
                    for &vmj in &vm_vars {
                        add_entry(vmi, vmj);
                    }
                }
                for &vmi in &vm_vars {
                    for &vai in &va_vars {
                        if let Some(a) = vai {
                            add_entry(vmi, a);
                        }
                    }
                }
            }
            if matches!(&options.objective, OrpdObjective::MinimizeCombined { .. }) {
                // Also add Vm diagonal for vdev term
                for i in 0..m.n_bus {
                    add_entry(m.vm_var(i), m.vm_var(i));
                }
            }
        }
        OrpdObjective::MinimizeVoltageDeviation { .. } => {
            // Vm diagonal only.
            for i in 0..m.n_bus {
                add_entry(m.vm_var(i), m.vm_var(i));
            }
        }
    }

    // Branch flow constraint Hessian entries (for constrained branches).
    for ba in constrained_admittances {
        let va_vars: [Option<usize>; 2] = [m.va_var(ba.from), m.va_var(ba.to)];
        let vm_vars: [usize; 2] = [m.vm_var(ba.from), m.vm_var(ba.to)];
        for &vai in &va_vars {
            for &vaj in &va_vars {
                if let (Some(a), Some(b)) = (vai, vaj) {
                    add_entry(a, b);
                }
            }
        }
        for &vmi in &vm_vars {
            for &vmj in &vm_vars {
                add_entry(vmi, vmj);
            }
        }
        for &vmi in &vm_vars {
            for &vai in &va_vars {
                if let Some(a) = vai {
                    add_entry(vmi, a);
                }
            }
        }
    }

    (rows, cols, index_map)
}

// ---------------------------------------------------------------------------
// Initial point
// ---------------------------------------------------------------------------

fn build_x0(network: &Network, mapping: &OrpdMapping, pg_fixed_pu: &[f64], base: f64) -> Vec<f64> {
    let m = mapping;
    let mut x0 = vec![0.0; m.n_var];

    // Va: from DC power flow warm start.
    let dc_angles = match surge_dc::solve_dc(network) {
        Ok(r) => r.theta,
        Err(e) => {
            tracing::warn!(
                "DC power flow warm-start failed ({}); \
                 ORPD will start from flat (zero-angle) initial point",
                e
            );
            vec![0.0; m.n_bus]
        }
    };
    for (i, &angle) in dc_angles.iter().enumerate().take(m.n_bus) {
        if let Some(idx) = m.va_var(i) {
            x0[idx] = angle;
        }
    }

    // Vm: from bus data / generator setpoints.
    let bus_map = network.bus_index_map();
    let mut vm_init = vec![1.0; m.n_bus];
    for bus in &network.buses {
        let idx = bus_map[&bus.number];
        vm_init[idx] = bus.voltage_magnitude_pu;
    }
    for g in &network.generators {
        if g.in_service {
            let idx = bus_map[&g.bus];
            vm_init[idx] = g.voltage_setpoint_pu;
        }
    }
    for i in 0..m.n_bus {
        x0[m.vm_var(i)] = vm_init[i];
    }

    // Pg: use fixed values (or case data clamped to bounds).
    for (j, &gi) in m.gen_indices.iter().enumerate() {
        let g = &network.generators[gi];
        x0[m.pg_var(j)] = if !pg_fixed_pu.is_empty() {
            pg_fixed_pu[j]
        } else {
            (g.p / base).clamp(g.pmin / base, g.pmax / base)
        };
    }

    // Qg: from case data, clamped to bounds.
    for (j, &gi) in m.gen_indices.iter().enumerate() {
        let g = &network.generators[gi];
        let qmin = if g.qmin.abs() > 1e10 { -9999.0 } else { g.qmin };
        let qmax = if g.qmax.abs() > 1e10 { 9999.0 } else { g.qmax };
        x0[m.qg_var(j)] = (g.q / base).clamp(qmin / base, qmax / base);
    }

    x0
}

// ---------------------------------------------------------------------------
// Public solver interface
// ---------------------------------------------------------------------------

struct OrpdModelBuild<'a> {
    problem: OrpdProblem<'a>,
    nlp_options: NlpOptions,
}

struct OrpdExecution {
    solution: crate::nlp::NlpSolution,
    solve_time_ms: f64,
}

fn build_orpd_model<'a>(
    network: &'a Network,
    options: &'a OrpdOptions,
) -> Result<OrpdModelBuild<'a>, String> {
    let effective_max_iter = if options.max_iter == 0 {
        (network.n_buses() as u32 / 20).max(500)
    } else {
        options.max_iter
    };

    let problem = OrpdProblem::new(network, options)?;
    let nlp_options = NlpOptions {
        tolerance: options.tol,
        max_iterations: effective_max_iter,
        print_level: options.print_level,
        hessian_mode: if options.exact_hessian {
            HessianMode::Exact
        } else {
            HessianMode::LimitedMemory
        },
        warm_start: false,
    };

    Ok(OrpdModelBuild {
        problem,
        nlp_options,
    })
}

fn execute_orpd_model(
    options: &OrpdOptions,
    model: &OrpdModelBuild<'_>,
) -> Result<OrpdExecution, String> {
    let nlp = match options.nlp_solver.clone() {
        Some(s) => s,
        None => try_default_nlp_solver()?,
    };

    let start = Instant::now();
    let solution = crate::backends::run_nlp_solver_with_policy(nlp.as_ref(), || {
        nlp.solve(&model.problem, &model.nlp_options)
    })
    .map_err(|e| format!("ORPD NLP solver error: {e}"))?;

    Ok(OrpdExecution {
        solution,
        solve_time_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

fn decode_orpd_result(model: OrpdModelBuild<'_>, execution: OrpdExecution) -> OrpdResult {
    let OrpdModelBuild { problem, .. } = model;
    let OrpdExecution {
        solution: sol,
        solve_time_ms,
    } = execution;
    let m = &problem.mapping;
    let base = problem.base_mva;

    let (va, vm, pg_pu, qg_pu) = m.unpack(&sol.x);
    let voltages: Vec<(f64, f64)> = vm.iter().zip(va.iter()).map(|(&v, &a)| (v, a)).collect();
    let q_dispatch: Vec<f64> = qg_pu.to_vec();
    let p_dispatch: Vec<f64> = pg_pu.to_vec();
    let total_losses_mw = problem.compute_losses_pu(vm, &va) * base;
    let voltage_deviation = {
        let sum_sq: f64 = vm.iter().map(|&v| (v - 1.0) * (v - 1.0)).sum();
        (sum_sq / problem.network.n_buses() as f64).sqrt()
    };

    OrpdResult {
        converged: sol.converged,
        objective: sol.objective,
        total_losses_mw,
        voltage_deviation,
        voltages,
        q_dispatch,
        p_dispatch,
        iterations: sol.iterations,
        solve_time_ms,
    }
}

/// Solve the Optimal Reactive Power Dispatch (ORPD) / Volt-VAr Optimization problem.
///
/// Minimizes active power losses or voltage deviation while respecting AC power
/// balance, voltage limits, and reactive power limits. When `options.fix_pg = true`
/// (the default), active power setpoints are fixed at their current dispatch values
/// and the solver optimizes only reactive power, voltage magnitudes, and angles.
///
/// # Returns
///
/// An `OrpdResult` containing converged voltages, reactive dispatch, losses, and
/// voltage deviation. Returns an error string if the NLP fails to converge.
pub fn solve_orpd(network: &Network, options: &OrpdOptions) -> Result<OrpdResult, String> {
    let model = build_orpd_model(network, options)?;
    let execution = execute_orpd_model(options, &model)?;
    let result = decode_orpd_result(model, execution);
    if !result.converged {
        return Err("ORPD NLP did not converge".to_string());
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::test_util::case_path;

    use super::*;

    fn format_optional_iterations(iterations: Option<u32>) -> String {
        iterations
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Smoke test: ORPD loss minimization on case9 — runs in normal CI (no #[ignore]).
    ///
    /// Checks that `solve_orpd` returns `Ok` and total losses are in a plausible range.
    #[test]
    fn test_orpd_loss_minimization_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let opts = OrpdOptions {
            objective: OrpdObjective::MinimizeLosses,
            fix_pg: true,
            optimize_q: true,
            enforce_thermal_limits: false,
            tol: 1e-6,
            ..Default::default()
        };

        let result = solve_orpd(&net, &opts);
        assert!(result.is_ok(), "ORPD returned Err: {:?}", result.err());
        let result = result.unwrap();
        assert!(
            result.total_losses_mw > 0.0 && result.total_losses_mw < 50.0,
            "case9 ORPD losses {:.3} MW outside plausible range (0, 50) MW",
            result.total_losses_mw
        );
    }

    /// Full ORPD loss minimization test on case9 — slow, runs with opf_slow suite.
    ///
    /// Verifies that:
    /// 1. The solver converges.
    /// 2. Total losses are non-negative.
    /// 3. All bus voltages are within their bounds (0.9, 1.1 default).
    #[ignore = "slow: Ipopt NLP — run with cargo test --test opf_slow"]
    #[test]
    fn test_orpd_loss_minimization_case9_full() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let opts = OrpdOptions {
            objective: OrpdObjective::MinimizeLosses,
            fix_pg: true,
            optimize_q: true,
            enforce_thermal_limits: false, // relax for loss-min convergence
            tol: 1e-6,
            ..Default::default()
        };

        let result = solve_orpd(&net, &opts).expect("ORPD should not error");
        assert!(result.converged, "ORPD should converge on case9");
        assert!(
            result.total_losses_mw >= 0.0,
            "Total losses must be non-negative, got {}",
            result.total_losses_mw
        );
        // All voltages within [vmin, vmax]
        for (bus_idx, bus) in net.buses.iter().enumerate() {
            let (vm, _va) = result.voltages[bus_idx];
            assert!(
                vm >= bus.voltage_min_pu - 1e-4 && vm <= bus.voltage_max_pu + 1e-4,
                "Bus {} voltage {:.4} out of bounds [{:.4}, {:.4}]",
                bus.number,
                vm,
                bus.voltage_min_pu,
                bus.voltage_max_pu
            );
        }
        println!(
            "ORPD case9 losses: {:.3} MW, vdev: {:.4}, iters: {}, time: {:.2} ms",
            result.total_losses_mw,
            result.voltage_deviation,
            format_optional_iterations(result.iterations),
            result.solve_time_ms
        );
    }

    /// Test: ORPD voltage deviation minimization on case14.
    ///
    /// Verifies that:
    /// 1. The solver converges.
    /// 2. All bus voltages are within [0.95, 1.05] after optimization.
    #[ignore = "slow: Ipopt NLP — run with cargo test --test opf_slow"]
    #[test]
    fn test_orpd_vdev_minimization_case14() {
        let net = surge_io::load(case_path("case14")).unwrap();

        let opts = OrpdOptions {
            objective: OrpdObjective::MinimizeVoltageDeviation { v_ref: 1.0 },
            fix_pg: true,
            optimize_q: true,
            enforce_thermal_limits: false,
            tol: 1e-6,
            ..Default::default()
        };

        let result = solve_orpd(&net, &opts).expect("ORPD vdev should not error");
        assert!(result.converged, "ORPD vdev should converge");
        // Voltages within [0.95, 1.05] (IEEE standard band)
        for (bus_idx, bus) in net.buses.iter().enumerate() {
            let (vm, _va) = result.voltages[bus_idx];
            assert!(
                (0.95 - 1e-3..=1.05 + 1e-3).contains(&vm),
                "Bus {} voltage {:.4} outside [0.95, 1.05]",
                bus.number,
                vm
            );
            // Also check within explicit bounds
            assert!(
                vm >= bus.voltage_min_pu - 1e-4 && vm <= bus.voltage_max_pu + 1e-4,
                "Bus {} voltage {:.4} out of bounds [{:.4}, {:.4}]",
                bus.number,
                vm,
                bus.voltage_min_pu,
                bus.voltage_max_pu
            );
        }
        println!(
            "ORPD case14 vdev: {:.6}, losses: {:.3} MW, iters: {}, time: {:.2} ms",
            result.voltage_deviation,
            result.total_losses_mw,
            format_optional_iterations(result.iterations),
            result.solve_time_ms
        );
    }

    /// Test: ORPD achieves lower losses than base AC power flow on case9.
    ///
    /// Runs AC power flow (via base case losses), then ORPD, and asserts
    /// that ORPD losses are not greater than the base case losses.
    #[ignore = "slow: Ipopt NLP — run with cargo test --test opf_slow"]
    #[test]
    fn test_orpd_vs_acpf_losses() {
        use surge_ac::{AcPfOptions, solve_ac_pf};

        let net = surge_io::load(case_path("case9")).unwrap();

        // Step 1: Solve base AC power flow to get base case losses.
        let acpf_opts = AcPfOptions::default();
        let pf_sol = solve_ac_pf(&net, &acpf_opts).expect("AC power flow should converge");

        let base_mva = net.base_mva;
        // Base case total generation - total load = losses
        let base_gen_mw: f64 = net
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.p)
            .sum();
        let base_load_mw: f64 = net.total_load_mw();
        // Compute actual losses from power flow using branch flows
        let bus_map = net.bus_index_map();
        let mut base_losses_mw = 0.0_f64;
        for br in &net.branches {
            if !br.in_service {
                continue;
            }
            let fi = bus_map[&br.from_bus];
            let ti = bus_map[&br.to_bus];
            let vf = pf_sol.voltage_magnitude_pu[fi];
            let vt = pf_sol.voltage_magnitude_pu[ti];
            let theta = pf_sol.voltage_angle_rad[fi] - pf_sol.voltage_angle_rad[ti];
            let flows = br.power_flows_pu(vf, vt, theta, 1e-40);
            base_losses_mw += (flows.p_from_pu + flows.p_to_pu) * base_mva;
        }

        println!(
            "Base case losses: {:.4} MW (gen {:.2} MW, load {:.2} MW)",
            base_losses_mw, base_gen_mw, base_load_mw
        );

        // Step 2: Solve ORPD.
        let opts = OrpdOptions {
            objective: OrpdObjective::MinimizeLosses,
            fix_pg: true,
            optimize_q: true,
            enforce_thermal_limits: false,
            tol: 1e-6,
            ..Default::default()
        };

        let result = solve_orpd(&net, &opts).expect("ORPD should not error");
        assert!(result.converged, "ORPD should converge");

        println!(
            "ORPD losses: {:.4} MW, iters: {}, time: {:.2} ms",
            result.total_losses_mw,
            format_optional_iterations(result.iterations),
            result.solve_time_ms
        );

        // ORPD should achieve losses ≤ base case + small tolerance.
        // (It may match since Pg is fixed and Q degrees of freedom are limited.)
        assert!(
            result.total_losses_mw <= base_losses_mw + 0.5,
            "ORPD losses ({:.4} MW) should not exceed base case losses ({:.4} MW) by more than 0.5 MW",
            result.total_losses_mw,
            base_losses_mw
        );
    }
}
