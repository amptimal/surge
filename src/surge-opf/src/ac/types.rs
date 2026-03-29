#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC-OPF types, options, errors, and branch admittance data.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use surge_network::Network;
use surge_solution::{OpfSolution, PfSolution};

use super::sensitivity::BendersCut;
use crate::backends::NlpSolver;

pub use super::solve::solve_ac_opf;

/// OPF-08: Warm-start point for AC-OPF, built from a prior power flow or OPF solution.
///
/// Carries the primal variable values used to initialise the NLP solver
/// (Ipopt) instead of the default DC power flow warm-start.  Sequentially
/// clearing market intervals — where the dispatch changes slowly between
/// intervals — typically halves solver iterations.
#[derive(Debug, Clone)]
pub struct WarmStart {
    /// Bus voltage magnitudes per-unit (indexed by internal bus index).
    pub voltage_magnitude_pu: Vec<f64>,
    /// Bus voltage angles in radians (indexed by internal bus index).
    pub voltage_angle_rad: Vec<f64>,
    /// Generator real power outputs in pu (indexed by generator order).
    pub pg: Vec<f64>,
    /// Generator reactive power outputs in pu (indexed by generator order).
    pub qg: Vec<f64>,
}

impl WarmStart {
    /// Build a WarmStart from a [`PfSolution`].
    ///
    /// Voltage magnitudes and angles are taken directly from the PF solution.
    /// Generator dispatch (`pg`) is left empty; the NLP uses the default
    /// initialisation for generator variables.
    pub fn from_pf(solution: &PfSolution) -> Self {
        Self {
            voltage_magnitude_pu: solution.voltage_magnitude_pu.clone(),
            voltage_angle_rad: solution.voltage_angle_rad.clone(),
            pg: Vec::new(),
            qg: Vec::new(),
        }
    }

    /// Build a WarmStart from a prior [`OpfSolution`].
    ///
    /// Voltage magnitudes and angles come from `solution.power_flow`; generator
    /// dispatch (pu) is computed from `gen_p_mw` divided by the system MVA base.
    pub fn from_opf(result: &OpfSolution) -> Self {
        let base_mva = result.base_mva;
        Self {
            voltage_magnitude_pu: result.power_flow.voltage_magnitude_pu.clone(),
            voltage_angle_rad: result.power_flow.voltage_angle_rad.clone(),
            pg: result
                .generators
                .gen_p_mw
                .iter()
                .map(|&p| p / base_mva)
                .collect(),
            qg: result
                .generators
                .gen_q_mvar
                .iter()
                .map(|&q| q / base_mva)
                .collect(),
        }
    }
}

/// Runtime-only AC-OPF execution controls.
///
/// This type carries backend selection and transient solve state that should
/// not be mixed into the serializable/public AC-OPF problem specification.
#[derive(Clone, Default)]
pub struct AcOpfRuntime {
    /// Override NLP solver backend. `None` = use the canonical default NLP policy.
    pub nlp_solver: Option<Arc<dyn NlpSolver>>,
    /// Warm-start the NLP solver from a prior AC operating point.
    pub warm_start: Option<WarmStart>,
    /// Seed the AC-OPF initial angles from a DC-OPF solution.
    ///
    /// `None` = auto (enabled when `n_buses > 2000` and no explicit warm start).
    /// `Some(true)` = force DC-OPF warm-start regardless of problem size.
    /// `Some(false)` = disable (use simple DC power flow for initial angles).
    pub use_dc_opf_warm_start: Option<bool>,
}

impl AcOpfRuntime {
    /// Set the NLP solver backend (builder pattern).
    pub fn with_nlp_solver(mut self, solver: Arc<dyn NlpSolver>) -> Self {
        self.nlp_solver = Some(solver);
        self
    }

    /// Set the warm-start point from a prior AC solution (builder pattern).
    pub fn with_warm_start(mut self, warm_start: WarmStart) -> Self {
        self.warm_start = Some(warm_start);
        self
    }

    /// Force-enable or disable DC-OPF warm-start for initial angles (builder pattern).
    pub fn with_dc_opf_warm_start(mut self, enabled: bool) -> Self {
        self.use_dc_opf_warm_start = Some(enabled);
        self
    }
}

#[derive(Clone, Default)]
pub(crate) struct AcOpfRunContext {
    pub(crate) runtime: AcOpfRuntime,
    pub(crate) benders_cuts: Vec<BendersCut>,
}

impl AcOpfRunContext {
    pub(crate) fn from_runtime(runtime: &AcOpfRuntime) -> Self {
        Self {
            runtime: runtime.clone(),
            benders_cuts: Vec::new(),
        }
    }
}

/// Controls how discrete transformer taps, phase shifters, and switched shunts
/// are handled in AC-OPF post-solve.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscreteMode {
    /// Continuous relaxation (default). NLP variables remain continuous.
    /// Tap/phase/shunt dispatch is reported but not rounded or verified.
    #[default]
    Continuous,
    /// Solve continuous NLP, round taps/phases/shunts to nearest discrete step,
    /// re-verify feasibility with an AC power flow. Reports violations.
    RoundAndCheck,
}

/// AC-OPF solver options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AcOpfOptions {
    /// Convergence tolerance.
    /// Default: 1e-6 (Ipopt NLP convergence — tighter values increase iteration
    /// count without meaningful accuracy gain).
    pub tolerance: f64,
    /// Maximum solver iterations.
    pub max_iterations: u32,
    /// NLP solver print level (0=silent, 5=verbose).
    pub print_level: i32,
    /// Whether to enforce branch thermal limits.
    pub enforce_thermal_limits: bool,
    /// Minimum rate_a (MVA) to consider a branch as having a thermal limit.
    pub min_rate_a: f64,
    /// Whether to enforce branch angle-difference limits (angmin/angmax).
    ///
    /// Default: `false` — matches MATPOWER's `opf.ignore_angle_lim = 1` default.
    ///
    /// Many case files (e.g., ACTIVSg series exported from PowerWorld) store
    /// `angmin = angmax = 0` as the *current operating angle* rather than as a
    /// binding limit. Enforcing those values makes the NLP immediately infeasible.
    /// Enable this only when the case file contains genuine operational angle limits.
    pub enforce_angle_limits: bool,
    /// Use exact analytical Hessian (true) or L-BFGS quasi-Newton approximation (false).
    ///
    /// # Default: `true` (exact Hessian)
    ///
    /// The exact analytical Hessian provides second-order curvature information
    /// to the NLP solver, typically reducing iterations from 100+ to 20-30 and
    /// improving convergence reliability near binding constraints.
    ///
    /// # When to use L-BFGS (`exact_hessian = false`)
    ///
    /// The L-BFGS quasi-Newton approximation builds an approximate Hessian from
    /// gradient history. It avoids the O(n^2) Hessian assembly cost and may be
    /// preferable for very large networks (>10,000 buses) where Hessian
    /// factorization dominates solve time, or when memory is constrained.
    /// However, L-BFGS typically requires 3-5x more NLP iterations and may
    /// fail to converge on tightly-constrained problems.
    ///
    /// # History
    ///
    /// Prior to the precision-defaults update, L-BFGS (`false`) was the default.
    /// It was changed to exact Hessian (`true`) because the exact Hessian path
    /// was already implemented and produces superior convergence for the vast
    /// majority of practical OPF problems.
    pub exact_hessian: bool,
    /// Co-optimize switched shunt susceptance banks as continuous NLP variables.
    ///
    /// When `true`, each entry in `network.controls.switched_shunts_opf` is relaxed to a
    /// continuous variable `b_sw[i] ∈ [b_min_pu, b_max_pu]`.
    ///
    /// Default: `false`.
    pub optimize_switched_shunts: bool,
    /// Optimize transformer tap ratios as continuous NLP variables.
    ///
    /// When `true`, each branch where `tap_mode == TapMode::Continuous` adds
    /// a variable `τ ∈ [tap_min, tap_max]`.
    ///
    /// Default: `false`.
    pub optimize_taps: bool,
    /// Optimize phase-shifting transformer angles as continuous NLP variables.
    ///
    /// When `true`, each branch where `phase_mode == PhaseMode::Continuous` adds
    /// a variable `θ_s ∈ [phase_min_rad, phase_max_rad]` (internally in radians).
    ///
    /// Default: `false`.
    pub optimize_phase_shifters: bool,
    /// Optimize SVC/STATCOM susceptance as continuous NLP variables.
    ///
    /// When `true`, each in-service shunt FACTS device (mode ShuntOnly or ShuntSeries)
    /// adds a variable `b_svc[i] ∈ [b_min_pu, b_max_pu]`.
    ///
    /// Default: `false`.
    pub optimize_svc: bool,
    /// Optimize TCSC compensating reactance as continuous NLP variables.
    ///
    /// When `true`, each in-service series FACTS device (mode SeriesOnly, ShuntSeries,
    /// SeriesPowerControl, ImpedanceModulation) adds a variable `x_comp[i]`.
    ///
    /// Default: `false`.
    pub optimize_tcsc: bool,
    /// Include HVDC converters in AC-OPF via sequential AC-DC iteration.
    ///
    /// - `None` = auto-detect: enabled when the network has point-to-point HVDC links
    ///   or explicit DC converters.
    /// - `Some(true)` = force HVDC inclusion.
    /// - `Some(false)` = disable HVDC (ignore any DC line data).
    pub include_hvdc: Option<bool>,
    /// Per-generator SoC override (MWh) for storage generators co-optimized as NLP variables.
    ///
    /// Storage generators in the network with `StorageDispatchMode::CostMinimization` are
    /// automatically co-optimized as native NLP variables:
    ///   - `dis[s] ∈ [0, discharge_mw_max/base]`  (discharge power, pu)
    ///   - `ch[s]  ∈ [0, charge_mw_max/base]`     (charge power, pu)
    ///
    /// This map overrides the per-generator `soc_initial_mwh` field for SoC-derived bounds,
    /// keyed by global generator index. Generators not in the map use `soc_initial_mwh`.
    ///
    /// `SelfSchedule` and `OfferCurve` units must be dispatched by the caller
    /// before passing the network to AC-OPF.
    ///
    /// Default: `None` (use `soc_initial_mwh` from each generator's `StorageParams`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_soc_override: Option<HashMap<usize, f64>>,
    /// Interval duration (hours) for SoC-derived discharge/charge bounds.
    ///
    /// Controls how much energy a storage unit may exchange in one interval:
    ///   `dis_ub = min(discharge_mw_max, (soc - soc_min) * sqrt(eta) / dt_hours)`
    ///   `ch_ub  = min(charge_mw_max,   (soc_max - soc) / (dt_hours * sqrt(eta)))`
    ///
    /// Must match the SCED interval length. Default: `1.0` (one hour).
    pub dt_hours: f64,
    /// Enforce flowgate and interface constraints from `network.flowgates` / `network.interfaces`.
    ///
    /// Default: `false` (matches historical behavior; AC-SCED sets this from DispatchOptions).
    pub enforce_flowgates: bool,
    /// Active constraint screening threshold for branch thermal limits.
    ///
    /// When `Some(t)`, the AC-OPF is solved with a reduced branch constraint
    /// set: only branches whose DC-OPF loading exceeds `t` (0–1 fraction of
    /// their thermal limit) are included initially. After each solve, all
    /// originally-constrained branches are checked; any violations are added
    /// and the problem is re-solved (up to 3 outer iterations).
    ///
    /// Typical values: `Some(0.80)` to `Some(0.90)`. Higher threshold → smaller
    /// initial problem but more likely to need outer iterations.
    ///
    /// `None` = disabled (all branches included, existing behaviour).
    /// Only active when `enforce_thermal_limits = true` and
    /// `n_buses >= constraint_screening_min_buses`.
    pub constraint_screening_threshold: Option<f64>,
    /// Minimum number of buses to activate constraint screening (default: 1000).
    pub constraint_screening_min_buses: usize,
    /// When `true`, run a violation-check fallback after the screened solve (default: false).
    ///
    /// If any violations are detected in excluded branches, re-solve with the full
    /// constraint set (correct result, but worst-case is 2 AC-OPF solves).
    /// When `false` (default), the screened solution is accepted as-is — faster,
    /// but excluded branches may slightly violate thermal limits.
    pub screening_fallback_enabled: bool,
    /// Enforce generator P-Q capability curves (D-curves) as NLP constraints.
    ///
    /// When `true` (default), generators with non-empty `pq_curve` data get
    /// piecewise-linear D-curve constraints that tighten Q bounds at high P output.
    /// Generators without `pq_curve` data keep flat `[Qmin, Qmax]` box bounds.
    ///
    /// When `false`, all generators use flat rectangular Q bounds regardless of
    /// whether `pq_curve` data is present — useful for screening studies where
    /// exact reactive capability doesn't matter.
    pub enforce_capability_curves: bool,
    /// Discrete control rounding mode for transformer taps, phase shifters, and
    /// switched shunts.
    ///
    /// - `Continuous` (default): NLP variables remain continuous. Dispatch values
    ///   are reported but not rounded or verified.
    /// - `RoundAndCheck`: After the continuous NLP solve, round all discrete
    ///   devices to their nearest realizable step, then run an AC power flow to
    ///   verify feasibility. Reports violations if rounding causes constraint
    ///   violations.
    pub discrete_mode: DiscreteMode,
}

impl Default for AcOpfOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-6,
            max_iterations: 0, // 0 = auto: max(500, n_buses / 20)
            print_level: 0,
            enforce_thermal_limits: true,
            min_rate_a: 1.0,
            enforce_angle_limits: false,
            exact_hessian: true,
            optimize_switched_shunts: false,
            optimize_taps: false,
            optimize_phase_shifters: false,
            optimize_svc: false,
            optimize_tcsc: false,
            include_hvdc: None,
            storage_soc_override: None,
            dt_hours: 1.0,
            enforce_flowgates: false,
            constraint_screening_threshold: None,
            constraint_screening_min_buses: 1000,
            screening_fallback_enabled: false,
            enforce_capability_curves: true,
            discrete_mode: DiscreteMode::Continuous,
        }
    }
}

crate::common::opf_common_errors!(AcOpfError {
    /// The NLP solver did not converge to a feasible optimum.
    #[error("solver did not converge")]
    NotConverged,
});

// ---------------------------------------------------------------------------
// Variable index mapping
// ---------------------------------------------------------------------------

/// SVC device data for NLP co-optimization.
#[derive(Debug, Clone)]
pub(crate) struct SvcOpfData {
    /// Internal 0-based bus index.
    pub(crate) bus_idx: usize,
    /// Minimum susceptance (pu, inductive, typically negative).
    pub(crate) b_min: f64,
    /// Maximum susceptance (pu, capacitive, typically positive).
    pub(crate) b_max: f64,
    /// Initial susceptance from q_des.
    pub(crate) b_init: f64,
}

/// TCSC device data for NLP co-optimization.
#[derive(Debug, Clone)]
pub(crate) struct TcscOpfData {
    /// Internal bus index of branch from-bus.
    pub(crate) from_idx: usize,
    /// Internal bus index of branch to-bus.
    pub(crate) to_idx: usize,
    /// Original branch reactance (pu).
    pub(crate) x_orig: f64,
    /// Branch resistance (pu).
    pub(crate) r: f64,
    /// Branch tap ratio.
    pub(crate) tap: f64,
    /// Branch phase shift (radians).
    pub(crate) shift_rad: f64,
    /// Minimum compensating reactance (pu).
    pub(crate) x_comp_min: f64,
    /// Maximum compensating reactance (pu).
    pub(crate) x_comp_max: f64,
    /// Initial compensation from linx.
    pub(crate) x_comp_init: f64,
}

// ---------------------------------------------------------------------------
// Branch admittance cache for flow computation
// ---------------------------------------------------------------------------

/// A single monitored branch within a flowgate or interface constraint.
pub(super) struct FgBranchEntry {
    /// Branch admittance (from-side self/mutual, to-side self/mutual).
    pub(super) adm: BranchAdmittance,
    /// Direction coefficient (typically +1.0 or -1.0).
    pub(super) coeff: f64,
}

/// Pre-computed data for a single flowgate or interface NLP constraint.
pub(super) struct FgConstraintData {
    /// Monitored branches with their admittance data and coefficients.
    pub(super) branches: Vec<FgBranchEntry>,
    /// Unique NLP variable columns that appear in the Jacobian for this constraint.
    pub(super) jac_cols: Vec<usize>,
}

/// Pre-computed branch admittance parameters for flow calculations.
pub(crate) struct BranchAdmittance {
    pub(crate) from: usize,
    pub(crate) to: usize,
    /// From-side self-admittance G_ff.
    pub(crate) g_ff: f64,
    /// From-side self-admittance B_ff.
    pub(crate) b_ff: f64,
    /// From-to mutual admittance G_ft.
    pub(crate) g_ft: f64,
    /// From-to mutual admittance B_ft.
    pub(crate) b_ft: f64,
    /// To-side self-admittance G_tt.
    pub(crate) g_tt: f64,
    /// To-side self-admittance B_tt.
    pub(crate) b_tt: f64,
    /// To-from mutual admittance G_tf.
    pub(crate) g_tf: f64,
    /// To-from mutual admittance B_tf.
    pub(crate) b_tf: f64,
    /// Flow limit squared (rate_a/base)^2.
    pub(crate) s_max_sq: f64,
}

/// Compute admittance parameters for a single branch.
///
/// `s_max_sq` is set to `(rating_a_mva / base_mva)^2`. Callers that don't
/// need a thermal limit (e.g. flowgate branches) can override it afterward.
pub(crate) fn compute_branch_admittance(
    br: &surge_network::network::Branch,
    from: usize,
    to: usize,
    base_mva: f64,
) -> BranchAdmittance {
    let adm = br.pi_model(1e-40);

    BranchAdmittance {
        from,
        to,
        g_ff: adm.g_ff,
        b_ff: adm.b_ff,
        g_ft: adm.g_ft,
        b_ft: adm.b_ft,
        g_tt: adm.g_tt,
        b_tt: adm.b_tt,
        g_tf: adm.g_tf,
        b_tf: adm.b_tf,
        s_max_sq: (br.rating_a_mva / base_mva).powi(2),
    }
}

pub(crate) fn build_branch_admittances(
    network: &Network,
    constrained_branches: &[usize],
    bus_map: &HashMap<u32, usize>,
) -> Vec<BranchAdmittance> {
    constrained_branches
        .iter()
        .map(|&l| {
            let br = &network.branches[l];
            let f = bus_map[&br.from_bus];
            let t = bus_map[&br.to_bus];
            compute_branch_admittance(br, f, t, network.base_mva)
        })
        .collect()
}

/// Compute from-side branch flow (Pf, Qf) for a branch.
#[inline]
pub(super) fn branch_flow_from(ba: &BranchAdmittance, vm: &[f64], va: &[f64]) -> (f64, f64) {
    let vi = vm[ba.from];
    let vj = vm[ba.to];
    let theta = va[ba.from] - va[ba.to];
    let (sin_t, cos_t) = theta.sin_cos();

    let pf = vi * vi * ba.g_ff + vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
    let qf = -vi * vi * ba.b_ff + vi * vj * (ba.g_ft * sin_t - ba.b_ft * cos_t);
    (pf, qf)
}

/// Compute to-side branch flow (Pt, Qt) for a branch.
#[inline]
pub(super) fn branch_flow_to(ba: &BranchAdmittance, vm: &[f64], va: &[f64]) -> (f64, f64) {
    let vi = vm[ba.from];
    let vj = vm[ba.to];
    let theta = va[ba.to] - va[ba.from]; // note: reversed
    let (sin_t, cos_t) = theta.sin_cos();

    let pt = vj * vj * ba.g_tt + vj * vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);
    let qt = -vj * vj * ba.b_tt + vj * vi * (ba.g_tf * sin_t - ba.b_tf * cos_t);
    (pt, qt)
}

// ---------------------------------------------------------------------------
// Hessian direct-index lookup (eliminates HashMap in hot eval_hessian paths)
// ---------------------------------------------------------------------------
