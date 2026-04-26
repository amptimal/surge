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
    /// Dispatchable-load served real power in pu (indexed by in-service load order).
    pub dispatchable_load_p: Vec<f64>,
    /// Dispatchable-load served reactive power in pu (indexed by in-service load order).
    pub dispatchable_load_q: Vec<f64>,
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
            dispatchable_load_p: Vec::new(),
            dispatchable_load_q: Vec::new(),
        }
    }

    /// Build a WarmStart from a [`PfSolution`] plus the solved network snapshot.
    ///
    /// In addition to voltage magnitudes and angles, this recovers approximate
    /// generator `Pg/Qg` seeds from the network operating point and the PF
    /// result. `Pg` is seeded from the network generator dispatch with any
    /// recorded distributed-slack contribution applied; `Qg` is reconstructed
    /// from the solved bus reactive injections.
    pub fn from_pf_with_network(network: &Network, solution: &PfSolution) -> Self {
        let base_mva = network.base_mva;
        let pg = network
            .generators
            .iter()
            .enumerate()
            .filter(|(_, generator)| generator.in_service)
            .map(|(idx, generator)| {
                let slack_mw = solution
                    .gen_slack_contribution_mw
                    .get(idx)
                    .copied()
                    .unwrap_or(0.0);
                (generator.p + slack_mw) / base_mva
            })
            .collect();
        let solved_qg_mvar = solution.generator_reactive_power_mvar(network);
        let qg = network
            .generators
            .iter()
            .enumerate()
            .filter(|(_, generator)| generator.in_service)
            .map(|(idx, _)| solved_qg_mvar.get(idx).copied().unwrap_or(0.0) / base_mva)
            .collect();
        let dispatchable_load_p = network
            .market_data
            .dispatchable_loads
            .iter()
            .filter(|load| load.in_service)
            .map(|load| load.p_sched_pu)
            .collect();
        let dispatchable_load_q = network
            .market_data
            .dispatchable_loads
            .iter()
            .filter(|load| load.in_service)
            .map(|load| load.q_sched_pu)
            .collect();

        Self {
            voltage_magnitude_pu: solution.voltage_magnitude_pu.clone(),
            voltage_angle_rad: solution.voltage_angle_rad.clone(),
            pg,
            qg,
            dispatchable_load_p,
            dispatchable_load_q,
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
            dispatchable_load_p: result
                .devices
                .dispatchable_load_served_mw
                .iter()
                .map(|&p| p / base_mva)
                .collect(),
            dispatchable_load_q: result
                .devices
                .dispatchable_load_served_q_mvar
                .iter()
                .map(|&q| q / base_mva)
                .collect(),
        }
    }

    /// Build a WarmStart from a prior [`OpfSolution`], but use the current
    /// network snapshot's real-power dispatch targets for generators and
    /// dispatchable loads.
    pub fn from_opf_with_network_targets(network: &Network, result: &OpfSolution) -> Self {
        let mut warm_start = Self::from_opf(result);
        let base_mva = result.base_mva;
        warm_start.pg = network
            .generators
            .iter()
            .filter(|generator| generator.in_service)
            .map(|generator| generator.p / base_mva)
            .collect();
        warm_start.dispatchable_load_p = network
            .market_data
            .dispatchable_loads
            .iter()
            .filter(|load| load.in_service)
            .map(|load| load.p_sched_pu)
            .collect();
        warm_start.dispatchable_load_q = network
            .market_data
            .dispatchable_loads
            .iter()
            .filter(|load| load.in_service)
            .map(|load| load.q_sched_pu)
            .collect();
        warm_start
    }
}

/// Runtime-only AC-OPF execution controls.
///
/// This type carries backend selection and transient solve state that should
/// not be mixed into the serializable/public AC-OPF problem specification.
#[derive(Debug, Clone, Default)]
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
    /// Optional additive quadratic penalties on active-power deviations from target schedules.
    #[doc(hidden)]
    pub objective_target_tracking: Option<AcObjectiveTargetTracking>,
    /// When `Some(path)` and the discrete polish runs, serialize the
    /// FIRST-pass `OpfSolution` to that path as pretty-printed JSON
    /// before the polish takes over. Captures the high-value debug
    /// surface — bus voltages, generator P/Q, continuous-vs-rounded
    /// discrete dispatch, bus balance slack values, discrete-feasibility
    /// flag — so a "bad" first-pass result can be inspected after the
    /// polish has rewritten the public solution. No-op when the polish
    /// doesn't run (continuous mode, no discrete devices, polish
    /// disabled). Caller is responsible for picking a unique path per
    /// AC-OPF call (e.g. include period index).
    pub pre_polish_dump_path: Option<std::path::PathBuf>,
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

    /// Attach additive target-tracking penalties to the AC-OPF objective.
    #[doc(hidden)]
    pub fn with_objective_target_tracking(mut self, tracking: AcObjectiveTargetTracking) -> Self {
        self.objective_target_tracking = if tracking.is_empty() {
            None
        } else {
            Some(tracking)
        };
        self
    }

    /// Tee the pre-polish OpfSolution to the given path as pretty JSON
    /// (builder pattern). See [`AcOpfRuntime::pre_polish_dump_path`].
    pub fn with_pre_polish_dump_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.pre_polish_dump_path = Some(path.into());
        self
    }
}

/// Per-direction quadratic penalty pair for tracking a single resource.
///
/// The AC OPF objective applies
/// `upward_per_mw2 * max(0, Pg - target)² + downward_per_mw2 * max(0, target - Pg)²`
/// so callers can encode asymmetric costs — e.g. free renewables get a
/// large `downward_per_mw2` (penalise curtailment) and a small or zero
/// `upward_per_mw2` (let AC physics use the full capacity if it can),
/// while thermals get symmetric coefficients.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AcTargetTrackingCoefficients {
    /// Penalty when `Pg - target > 0` (the resource is running harder
    /// than the DC target), `$/MW²-hr`.
    pub upward_per_mw2: f64,
    /// Penalty when `Pg - target < 0` (the resource is backed off
    /// relative to the DC target), `$/MW²-hr`.
    pub downward_per_mw2: f64,
}

impl AcTargetTrackingCoefficients {
    pub const ZERO: Self = Self {
        upward_per_mw2: 0.0,
        downward_per_mw2: 0.0,
    };

    /// Build a symmetric pair where both directions share the same
    /// coefficient. The default case that matches the legacy
    /// `generator_p_penalty_per_mw2` scalar.
    pub fn symmetric(penalty_per_mw2: f64) -> Self {
        let clamped = penalty_per_mw2.max(0.0);
        Self {
            upward_per_mw2: clamped,
            downward_per_mw2: clamped,
        }
    }

    /// True when neither direction is actively penalised — the resource
    /// is effectively free to move.
    pub fn is_zero(&self) -> bool {
        self.upward_per_mw2 <= 0.0 && self.downward_per_mw2 <= 0.0
    }

    /// Maximum coefficient across both directions. Used to decide
    /// whether the NLP objective needs to carry the one-sided quadratic
    /// contribution for this resource at all.
    pub fn max(&self) -> f64 {
        self.upward_per_mw2.max(self.downward_per_mw2)
    }
}

impl Default for AcTargetTrackingCoefficients {
    fn default() -> Self {
        Self::ZERO
    }
}

/// Additive quadratic penalties for tracking target active-power schedules.
///
/// The objective is constructed by iterating resources (generators and
/// dispatchable loads) with a target set. For each one, the coefficient
/// lookup order is:
///
///   1. If an entry exists in `generator_p_penalty_overrides_by_idx`
///      (resp. `dispatchable_load_p_penalty_overrides_by_idx`), use it.
///   2. Otherwise use `generator_p_coefficients_default` (resp.
///      `dispatchable_load_p_coefficients_default`).
///
/// Backward compatibility: when a caller sets only the legacy scalar
/// `generator_p_penalty_per_mw2`, the builder in
/// [`Self::with_generator_scalar_penalty`] fills
/// `generator_p_coefficients_default` with a symmetric pair. That means
/// the previous single-scalar API remains a no-op for this change.
#[derive(Debug, Clone, Default)]
pub struct AcObjectiveTargetTracking {
    /// **Legacy field** — symmetric penalty coefficient for generator
    /// real-power deviation. When nonzero, the builder threads it into
    /// `generator_p_coefficients_default` as a symmetric pair. New
    /// callers should prefer the per-direction fields instead.
    pub generator_p_penalty_per_mw2: f64,
    /// Default per-direction penalty coefficients for generators that
    /// do not have a per-index override. Set via
    /// [`Self::with_generator_default_coefficients`] (or populated from
    /// the legacy scalar). Applies to every generator in
    /// `generator_p_targets_mw` that is not overridden.
    pub generator_p_coefficients_default: AcTargetTrackingCoefficients,
    /// Per-generator penalty coefficient overrides keyed by global
    /// `network.generators` index. When an entry exists, it replaces
    /// the default for that generator.
    pub generator_p_coefficients_overrides: HashMap<usize, AcTargetTrackingCoefficients>,
    /// Generator active-power targets keyed by global `network.generators` index.
    pub generator_p_targets_mw: HashMap<usize, f64>,
    /// **Legacy field** — symmetric penalty coefficient for
    /// dispatchable-load served-power deviation.
    pub dispatchable_load_p_penalty_per_mw2: f64,
    /// Default per-direction penalty coefficients for dispatchable
    /// loads that do not have a per-index override.
    pub dispatchable_load_p_coefficients_default: AcTargetTrackingCoefficients,
    /// Per-load penalty coefficient overrides keyed by global
    /// dispatchable-load index.
    pub dispatchable_load_p_coefficients_overrides: HashMap<usize, AcTargetTrackingCoefficients>,
    /// Dispatchable-load served-power targets keyed by global load index.
    pub dispatchable_load_p_targets_mw: HashMap<usize, f64>,
}

impl AcObjectiveTargetTracking {
    pub fn is_empty(&self) -> bool {
        let has_generator_penalty = self.generator_p_penalty_per_mw2 > 0.0
            || !self.generator_p_coefficients_default.is_zero()
            || self
                .generator_p_coefficients_overrides
                .values()
                .any(|pair| !pair.is_zero());
        let has_load_penalty = self.dispatchable_load_p_penalty_per_mw2 > 0.0
            || !self.dispatchable_load_p_coefficients_default.is_zero()
            || self
                .dispatchable_load_p_coefficients_overrides
                .values()
                .any(|pair| !pair.is_zero());
        (!has_generator_penalty || self.generator_p_targets_mw.is_empty())
            && (!has_load_penalty || self.dispatchable_load_p_targets_mw.is_empty())
    }

    /// Look up the effective coefficient pair for a generator, falling
    /// back to the default (and to the legacy scalar when the default
    /// is zero).
    pub fn generator_coefficients_for(
        &self,
        global_gen_index: usize,
    ) -> AcTargetTrackingCoefficients {
        if let Some(pair) = self
            .generator_p_coefficients_overrides
            .get(&global_gen_index)
        {
            return *pair;
        }
        if !self.generator_p_coefficients_default.is_zero() {
            return self.generator_p_coefficients_default;
        }
        if self.generator_p_penalty_per_mw2 > 0.0 {
            return AcTargetTrackingCoefficients::symmetric(self.generator_p_penalty_per_mw2);
        }
        AcTargetTrackingCoefficients::ZERO
    }

    /// Look up the effective coefficient pair for a dispatchable load.
    pub fn dispatchable_load_coefficients_for(
        &self,
        global_load_index: usize,
    ) -> AcTargetTrackingCoefficients {
        if let Some(pair) = self
            .dispatchable_load_p_coefficients_overrides
            .get(&global_load_index)
        {
            return *pair;
        }
        if !self.dispatchable_load_p_coefficients_default.is_zero() {
            return self.dispatchable_load_p_coefficients_default;
        }
        if self.dispatchable_load_p_penalty_per_mw2 > 0.0 {
            return AcTargetTrackingCoefficients::symmetric(
                self.dispatchable_load_p_penalty_per_mw2,
            );
        }
        AcTargetTrackingCoefficients::ZERO
    }
}

#[derive(Debug, Clone, Default)]
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
    /// Penalty for explicit branch thermal-limit slack variables ($/MVA-h).
    ///
    /// When positive, each enforced branch-end thermal constraint gets a
    /// nonnegative overflow variable `sigma >= 0` and the apparent-power limit
    /// becomes `|S| <= rate_a + sigma`. The objective adds
    /// `thermal_limit_slack_penalty_per_mva * sigma_mva`, allowing AC-OPF to
    /// return the minimum-overflow solution instead of hard-failing.
    ///
    /// Default: `0.0` (disabled; historical hard-limit behavior).
    pub thermal_limit_slack_penalty_per_mva: f64,
    /// Penalty for per-bus active-power balance slack variables ($/MW-h).
    ///
    /// When positive, each bus balance equality gains nonnegative surplus and
    /// deficit slack variables so the AC-OPF can return a minimum-mismatch
    /// solution instead of hard-failing on exact nodal active balance.
    ///
    /// Default: `0.0` (disabled; historical exact-balance behavior).
    pub bus_active_power_balance_slack_penalty_per_mw: f64,
    /// Penalty for per-bus reactive-power balance slack variables ($/MVAr-h).
    ///
    /// When positive, each bus reactive balance equality gains nonnegative
    /// surplus and deficit slack variables so the AC-OPF can return a
    /// minimum-mismatch solution instead of hard-failing on exact nodal
    /// reactive balance.
    ///
    /// Default: `0.0` (disabled; historical exact-balance behavior).
    pub bus_reactive_power_balance_slack_penalty_per_mvar: f64,
    /// Penalty for per-bus voltage-magnitude slack variables ($/pu-h).
    ///
    /// When positive, each bus voltage magnitude variable gains nonnegative
    /// slack variables `σ_high` and `σ_low` so the AC-OPF can exceed the
    /// original `[vm_min, vm_max]` box bounds at a cost. The Vm bounds are
    /// widened and two inequality constraints per bus enforce
    /// `vm - σ_high ≤ vm_max` and `vm_min - vm ≤ σ_low`. The objective adds
    /// `voltage_magnitude_slack_penalty_per_pu * base_mva * (σ_high + σ_low)`.
    ///
    /// Default: `0.0` (disabled; historical hard-bound behavior).
    pub voltage_magnitude_slack_penalty_per_pu: f64,
    /// Penalty for per-branch angle-difference slack variables ($/rad-h).
    ///
    /// When positive AND `enforce_angle_limits` is true, each angle-constrained
    /// branch gains nonnegative slack variables so the NLP can return a
    /// minimum-violation solution instead of hard-failing. The angle constraint
    /// residual becomes `g = Va_from - Va_to - sigma_high + sigma_low` with the
    /// original `[angmin, angmax]` bounds, so positive `sigma_high` absorbs
    /// upper-limit violations and positive `sigma_low` absorbs lower-limit
    /// violations. The objective adds
    /// `angle_difference_slack_penalty_per_rad * base_mva * (sigma_high + sigma_low)`.
    ///
    /// Default: `0.0` (disabled; historical hard-limit behavior).
    pub angle_difference_slack_penalty_per_rad: f64,
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
    /// Ipopt `nlp_scaling_method` setting (exact-Hessian path only).
    ///
    /// Default: `"gradient-based"` — Ipopt scales each constraint row by the
    /// inverse of its max gradient magnitude. Helps convergence on poorly
    /// conditioned problems but means the `tolerance` (primal-feasibility
    /// `tol`) applies to **scaled** constraints. On networks with stiff
    /// branches (very low-impedance ties, parallel lines with large `b_sr`),
    /// the row-scaling factor at incident buses can be ~100×, which inflates
    /// the unscaled bus-balance residual to ~`tol × max_gradient` — visible
    /// as small but non-zero P/Q balance violations to any external pi-model
    /// reconstruction (e.g. the GO C3 validator).
    ///
    /// Set to `"none"` to disable scaling and have `tol` apply directly to
    /// unscaled residuals. Slower convergence on ill-conditioned problems
    /// but exact balance against external recomputations.
    ///
    /// Other Ipopt-supported values: `"user-scaling"` (caller provides per-
    /// constraint scale via `ipopt_user_scaling`).
    pub nlp_scaling_method: String,
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
    /// Per-generator SoC override (MWh) for storage generators co-optimized as native AC variables.
    ///
    /// Storage generators in the network with dispatch modes other than
    /// `StorageDispatchMode::SelfSchedule` are automatically co-optimized as
    /// native AC variables:
    ///   - `dis[s] ∈ [0, discharge_mw_max/base]`  (discharge power, pu)
    ///   - `ch[s]  ∈ [0, charge_mw_max/base]`     (charge power, pu)
    ///
    /// This map overrides the per-generator `soc_initial_mwh` field for SoC-derived bounds,
    /// keyed by global generator index. Generators not in the map use `soc_initial_mwh`.
    ///
    /// `SelfSchedule` units must be dispatched by the caller before passing the
    /// network to AC-OPF.
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
    /// Treat per-bus voltage setpoints from regulating generators as hard
    /// equality constraints (`Vm = Vset`) on the AC-OPF.
    ///
    /// When `true` (default), each bus that has at least one in-service
    /// voltage-regulating generator gets `lb_vm = ub_vm = Vset`, mirroring the
    /// classical PSS/E "PV bus" assumption used by Newton-Raphson power flow.
    /// This is appropriate for studies where the operator setpoints are part
    /// of the schedule and must be honored exactly.
    ///
    /// When `false`, regulated buses keep their normal `[Vmin, Vmax]` bounds
    /// and the setpoint becomes a *soft* target only — i.e. the AC-OPF is
    /// free to move `Vm` anywhere inside the bus voltage limits. Use this for
    /// market-style formulations where the optimizer should choose
    /// the operating voltage subject to bounds and generator reactive
    /// capability rather than honouring an exogenous setpoint, and
    /// the warm-start `Vm` from a prior solve should be respected
    /// instead of being overwritten by the regulator target.
    ///
    /// Default: `true` (preserve historical behavior).
    pub enforce_regulated_bus_vm_targets: bool,
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
    /// When `true` and `discrete_mode == RoundAndCheck`, run a final
    /// AC-OPF "polish" pass with all discrete devices (taps, phase
    /// shifters, switched shunts) PINNED at their rounded values. This
    /// lets generator Q dispatch and bus voltages re-balance against the
    /// quantized topology, eliminating the post-rounding residual that
    /// the validator otherwise scores as bus Q-balance violations. The
    /// polish reuses the first solve's primal as a warm start, so the
    /// extra cost is typically a few Ipopt iterations per period. No
    /// effect when there are no discrete devices to round.
    pub discrete_polish: bool,
}

impl Default for AcOpfOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-8,
            max_iterations: 0, // 0 = auto: max(500, n_buses / 20)
            print_level: 0,
            enforce_thermal_limits: true,
            thermal_limit_slack_penalty_per_mva: 0.0,
            bus_active_power_balance_slack_penalty_per_mw: 0.0,
            bus_reactive_power_balance_slack_penalty_per_mvar: 0.0,
            voltage_magnitude_slack_penalty_per_pu: 0.0,
            angle_difference_slack_penalty_per_rad: 0.0,
            min_rate_a: 1.0,
            enforce_angle_limits: false,
            exact_hessian: true,
            nlp_scaling_method: "gradient-based".to_string(),
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
            enforce_regulated_bus_vm_targets: true,
            discrete_mode: DiscreteMode::Continuous,
            discrete_polish: true,
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

impl BranchAdmittance {
    #[inline]
    pub(crate) fn s_max_pu(&self) -> f64 {
        self.s_max_sq.sqrt()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::market::DispatchableLoad;
    use surge_network::network::bus::{Bus, BusType};
    use surge_network::network::generator::Generator;
    use surge_network::network::load::Load;
    use surge_solution::{PfModel, SolveStatus};

    #[test]
    fn warm_start_from_pf_with_network_seeds_pg_and_qg() {
        let mut net = Network::new("warm-start");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        let mut gen_a = Generator::with_id("g1", 1, 50.0, 1.0);
        gen_a.qmin = -10.0;
        gen_a.qmax = 20.0;
        let mut gen_b = Generator::with_id("g2", 1, 25.0, 1.0);
        gen_b.qmin = -5.0;
        gen_b.qmax = 5.0;
        net.generators.push(gen_a);
        net.generators.push(gen_b);
        net.loads.push(Load::new(2, 60.0, 15.0));
        net.market_data
            .dispatchable_loads
            .push(DispatchableLoad::curtailable(
                2,
                30.0,
                7.5,
                10.0,
                100.0,
                net.base_mva,
            ));

        let pf_sol = PfSolution {
            pf_model: PfModel::Ac,
            status: SolveStatus::Converged,
            voltage_magnitude_pu: vec![1.0, 1.0],
            voltage_angle_rad: vec![0.02, -0.01],
            active_power_injection_pu: vec![0.0, 0.0],
            reactive_power_injection_pu: vec![0.40, -0.15],
            bus_numbers: vec![1, 2],
            gen_slack_contribution_mw: vec![5.0, -5.0],
            ..Default::default()
        };

        let warm_start = WarmStart::from_pf_with_network(&net, &pf_sol);
        assert_eq!(warm_start.voltage_magnitude_pu, vec![1.0, 1.0]);
        assert_eq!(warm_start.voltage_angle_rad, vec![0.02, -0.01]);
        assert_eq!(warm_start.pg, vec![0.55, 0.20]);
        assert_eq!(warm_start.qg, vec![0.30, 0.10]);
        assert_eq!(warm_start.dispatchable_load_p, vec![0.30]);
        assert_eq!(warm_start.dispatchable_load_q, vec![0.075]);
    }
}
