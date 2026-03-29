// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Newton-Raphson solver options and configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use surge_network::AngleReference;
use surge_network::network::SwitchedShunt;
use surge_network::network::discrete_control::{OltcControl, ParControl};
use surge_solution::PfSolution;

/// Lightweight warm-start state for AC Newton-Raphson.
///
/// Carries only the voltage magnitudes and angles needed to initialize the NR
/// state vector, avoiding the cost of cloning a full [`PfSolution`] (which
/// includes branch flows, bus numbers, and other metadata not used during
/// initialization).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmStart {
    /// Voltage magnitudes per bus (per-unit), indexed by internal bus order.
    pub vm: Vec<f64>,
    /// Voltage angles per bus (radians), indexed by internal bus order.
    pub va: Vec<f64>,
}

impl WarmStart {
    /// Extract a warm-start state from a prior power flow solution.
    pub fn from_solution(sol: &PfSolution) -> Self {
        Self {
            vm: sol.voltage_magnitude_pu.clone(),
            va: sol.voltage_angle_rad.clone(),
        }
    }
}

/// How reactive power is shared among generators regulating the same bus.
///
/// When multiple generators regulate the same bus (local or remote), the NR
/// solver must distribute total Q among them.  This enum controls the weight
/// function used for proportional allocation.  The sharing *mechanism* is
/// always the same — weighted proportional with per-generator limit checking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QSharingMode {
    /// Proportional to (Qmax − Qmin) capability range.  This is the default
    /// and matches the prior Surge behaviour.
    #[default]
    Capability,
    /// Proportional to machine base MVA (`Generator.machine_base_mva`).  Matches PSS/E
    /// default remote voltage regulation sharing.
    Mbase,
    /// Equal share among all free (unfixed) generators.
    Equal,
}

/// How a solved bus-level distributed-slack share is attributed to generators
/// connected to that bus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlackAttributionMode {
    /// Prefer explicit generator weights when provided, then fall back by bus to:
    /// AGC participation, regulation ramp, directional headroom, and equal share.
    #[default]
    Automatic,
    /// Weight by positive `agc_participation_factor` on generators at the bus.
    AgcParticipation,
    /// Weight by directional regulation-ramp capability, clamped by headroom.
    RegulationRamp,
    /// Weight by directional active-power headroom at the solved dispatch.
    DirectionalHeadroom,
    /// Split the bus-level slack share equally across all in-service generators
    /// at the bus.
    EqualShare,
}

/// How DC lines are modelled during an AC Newton-Raphson solve.
///
/// Choosing `FixedSchedule` (the default) adds zero overhead for networks
/// without DC lines and produces correct results when the DC operating point
/// is well-known and fixed. Choose `SequentialAcDc` when firing angles,
/// reactive absorption, or DC voltage profiles are required.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DcLineModel {
    /// Use `SETVL` (and `VSCHD` for LCC) as constant PQ injections.
    ///
    /// No AC/DC iteration: rectifier consumes `P_dc + losses` at its bus
    /// (load increase), inverter injects `P_dc` at its bus (generation).
    /// Fast; appropriate when the DC operating point is fixed by a market
    /// schedule and reactive absorption is handled separately.
    #[default]
    FixedSchedule,

    /// Full sequential AC/DC: iterate AC NR ↔ DC operating point until convergence.
    ///
    /// In each outer iteration:
    /// 1. Solve AC network with current DC P/Q injections.
    /// 2. Recompute DC operating point (Idc, Vd_r, Vd_i, α, γ) from new bus voltages.
    /// 3. Check DC power convergence; break if `|ΔP| < dc_tol_mw`.
    ///
    /// Required for accurate converter reactive absorption and firing angle
    /// calculations. Typical convergence in 3–6 outer iterations.
    SequentialAcDc,
}

/// Startup policy for a cold AC Newton-Raphson solve.
///
/// This controls solves that do not provide an explicit warm-start solution.
/// Surge can either run the requested initialization once, escalate through
/// sequential fallbacks on failure, or race warm/flat starts in parallel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupPolicy {
    /// Try the requested initialization once, then escalate on retryable failure.
    ///
    /// Escalation is sequential rather than parallel:
    /// - `flat_start=false`: retry with flat start + DC angles, then an FDPF warm start
    /// - `flat_start=true`: retry with flat start + DC angles (if not already used),
    ///   then an FDPF warm start
    ///
    /// This is the default because it preserves the fast common path while
    /// materially improving robustness on hard full-model cases.
    #[default]
    Adaptive,

    /// Run a single solve using exactly the requested initialization.
    Single,

    /// Race case-data initialization against a flat start with DC angles.
    ///
    /// This is more robust on some stressed networks, but it spends 2x CPU on
    /// the common path and is therefore opt-in.
    ParallelWarmAndFlat,
}

/// Newton-Raphson solver options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcPfOptions {
    /// Convergence tolerance (maximum power mismatch in per-unit).
    /// Default: 1e-8 (tight, matches MATPOWER default for iterative NR).
    pub tolerance: f64,
    /// Maximum number of iterations.
    pub max_iterations: u32,
    /// Use flat start (Vm=1.0, Va=0.0) instead of case data.
    pub flat_start: bool,
    /// Use DC power flow angles as initial guess for flat start.
    ///
    /// When `true` (default) and `flat_start` is also `true`, a quick DC-PF
    /// linear solve (B' * theta = P_inj) provides initial voltage angles
    /// instead of Va = 0.  This typically brings angles within 5-10 degrees
    /// of the AC solution and dramatically improves NR convergence on large
    /// networks where a true flat start diverges.  The DC solve adds ~1 ms
    /// even for 80k-bus cases.  Has no effect when `flat_start` is `false`
    /// or `warm_start` is `Some`.
    #[serde(default = "default_true")]
    pub dc_warm_start: bool,
    /// Enable backtracking line search to prevent NR divergence.
    ///
    /// When `true` (default), after computing the Newton correction, the solver
    /// checks whether the full step reduces the mismatch norm.  If not, the step
    /// size is halved up to 4 times (alpha = 0.5, 0.25, 0.125, 0.0625).  This
    /// prevents the solver from overshooting into a divergent region on
    /// ill-conditioned cases while preserving quadratic convergence near the
    /// solution (the full step is always tried first).
    #[serde(default = "default_true")]
    pub line_search: bool,
    /// Clamp voltage magnitudes to [vm_min, vm_max] after each update.
    ///
    /// MATPOWER applies no voltage clamp during NR iterations, allowing
    /// Newton's method to explore the full voltage space and recover from
    /// large first steps (which occur from flat start on stressed networks).
    /// Setting vm_min = f64::NEG_INFINITY disables the lower clamp entirely,
    /// matching MATPOWER behaviour.  The default is no clamping (NEG_INFINITY
    /// and POS_INFINITY) so NR can follow its natural quadratic convergence.
    #[serde(default = "default_vm_min", skip_serializing_if = "is_neg_infinity")]
    pub vm_min: f64,
    /// Upper clamp for voltage magnitudes during NR iteration (per-unit).
    ///
    /// Default: `f64::INFINITY` (no upper clamp). See `vm_min` for details.
    #[serde(default = "default_vm_max", skip_serializing_if = "is_pos_infinity")]
    pub vm_max: f64,
    /// Warm-start from a prior voltage state.
    ///
    /// When `Some`, the NR state vector is initialised from the provided `vm` and
    /// `va` vectors instead of the flat-start (Vm=1, Va=0) or case-data
    /// initialisation.  This typically halves iteration count for time-series
    /// or sequential power flow where the operating point changes slowly.
    ///
    /// Use [`WarmStart::from_solution`] to extract the state from a [`PfSolution`].
    #[serde(default)]
    pub warm_start: Option<WarmStart>,

    /// Startup policy when no explicit warm start is provided.
    ///
    /// Applies only when `flat_start == false` and `warm_start == None`.
    #[serde(default)]
    pub startup_policy: StartupPolicy,

    /// Enforce generator reactive power limits (PV→PQ bus switching).
    ///
    /// When `true` (default), after each NR iteration, all PV buses are
    /// checked against their generator qmin/qmax. Buses that violate their
    /// Q limits are switched to PQ (fixing |V|) and the Jacobian is rebuilt.
    /// A bus that was previously switched back to PV is only allowed to switch
    /// to PQ once more (one re-switch guard per bus per solve).
    #[serde(default = "default_true")]
    pub enforce_q_limits: bool,

    /// Skip the slack (REF) bus from Q-limit enforcement.
    ///
    /// When `true`, the slack bus is excluded from Q-limit checking even
    /// when `enforce_q_limits` is `true`. This matches MATPOWER's CPF
    /// callback (`cpf_qlim.m`) which explicitly excludes the REF bus from
    /// Q-limit switching to preserve the voltage reference during
    /// continuation power flow.
    ///
    /// Default: `false` (standard PF behaviour checks all buses).
    #[serde(default)]
    pub skip_slack_q_limits: bool,

    /// How reactive power is shared among generators at the same bus (or
    /// regulating the same remote bus).
    ///
    /// Controls the weight function used to distribute total Q proportionally.
    /// Default: `Capability` (proportional to Qmax − Qmin range).
    #[serde(default)]
    pub q_sharing: QSharingMode,

    /// Incremental Q-limit switching: switch only the worst violator per
    /// outer iteration instead of all violators at once.
    ///
    /// When `true`, Phase 2 of `apply_per_gen_q_limits` switches only the
    /// single bus with the largest Q violation magnitude. The NR outer loop
    /// reconverges after each switch, keeping the iterate in the convergence
    /// basin. This matches MATPOWER's `cpf_qlim` callback which switches
    /// one bus at a time within the corrector iteration.
    ///
    /// Default: `false` (batch switching — standard PF behaviour).
    #[serde(default)]
    pub incremental_q_limits: bool,

    /// Disable voltage regulation for generators with infeasible P setpoints.
    ///
    /// When `true` (default), before the NR solve begins, any PV bus whose
    /// **all** generators have `pg < pmin` (active power below minimum
    /// operating point) is downgraded to PQ.  This matches PowSyBl/OpenLoadFlow
    /// behavior: a machine that cannot feasibly operate at its scheduled
    /// output is treated as inactive for voltage regulation purposes.
    ///
    /// Synchronous condensers (pmin = pmax = 0, pg = 0) are unaffected because
    /// their operating point is feasible.
    #[serde(default = "default_true")]
    pub enforce_gen_p_limits: bool,

    /// Distributed slack: bus index → participation factor.
    ///
    /// When `Some`, the active-power mismatch is distributed across all buses
    /// in the map proportionally to their participation factors (which must sum
    /// to 1.0). This replaces the default single-slack assumption.
    ///
    /// When `None` and `distributed_slack` is `false` (default), the single
    /// slack bus absorbs all real-power mismatch.
    #[serde(default)]
    pub slack_participation: Option<HashMap<usize, f64>>,

    /// Distributed slack: generator index → participation factor.
    ///
    /// When present, these generator-level weights take precedence over
    /// `slack_participation` and `distributed_slack`. The solver aggregates the
    /// weights to buses for the numerical NR step, then reuses the generator
    /// weights when reporting `PfSolution::gen_slack_contribution_mw`.
    #[serde(default)]
    pub generator_slack_participation: Option<HashMap<usize, f64>>,

    /// Convenience flag: distribute slack equally among all in-service generators.
    ///
    /// When `true` and `slack_participation` is `None`, the solver automatically
    /// builds equal participation factors for all in-service generator buses
    /// (including the slack bus).
    #[serde(default)]
    pub distributed_slack: bool,

    /// How each bus-level distributed-slack share is split across generators on
    /// that bus when populating `PfSolution::gen_slack_contribution_mw`.
    #[serde(default)]
    pub slack_attribution: SlackAttributionMode,

    /// Detect islands (connected components) before solving.
    ///
    /// When `true` (default), the solver runs a BFS/DFS over in-service
    /// branches to find connected components. Each island is solved
    /// independently with its own slack bus. Results are merged into a
    /// single `PfSolution` and `island_ids` is populated.
    ///
    /// Isolated buses (degree 0 in the in-service graph) get their own
    /// island with V = 1.0 p.u. flat start.
    #[serde(default = "default_true")]
    pub detect_islands: bool,

    /// Enable OLTC (On-Load Tap Changer) post-NR tap adjustment outer loop.
    ///
    /// When `true`, after NR converges the solver steps OLTC transformer taps
    /// to bring regulated bus voltages within their dead-bands, then re-solves.
    /// Repeats up to `oltc_max_iter` times.  Has no effect when `oltc_controls`
    /// is empty.
    #[serde(default = "default_true")]
    pub oltc_enabled: bool,

    /// Maximum number of OLTC outer-loop iterations (tap-adjust + re-solve rounds).
    #[serde(default = "default_oltc_max_iter")]
    pub oltc_max_iter: usize,

    /// OLTC transformer tap controls.  Each entry describes one transformer
    /// and the bus it regulates.  Mutated in place during the solve.
    #[serde(default)]
    pub oltc_controls: Vec<OltcControl>,

    /// Enable Phase Angle Regulator (PAR) discrete phase-shift outer loop.
    ///
    /// When `true`, after NR converges the solver steps PAR phase angles to
    /// drive monitored-branch active power flow toward its target band, then
    /// re-solves.  Repeats up to `par_max_iter` times.  Has no effect when
    /// `par_controls` is empty.
    #[serde(default = "default_true")]
    pub par_enabled: bool,

    /// Maximum number of PAR outer-loop iterations.
    #[serde(default = "default_par_max_iter")]
    pub par_max_iter: usize,

    /// PAR phase-angle controls.  Each entry describes one PAR transformer and
    /// the branch whose active power flow it regulates.
    #[serde(default)]
    pub par_controls: Vec<ParControl>,

    /// Enable switched shunt discrete control outer loop.
    ///
    /// When `true`, after NR converges the solver steps capacitor/reactor banks
    /// to bring regulated bus voltages within their dead-bands, then re-solves.
    /// Repeats up to `shunt_max_iter` times.  Has no effect when
    /// `switched_shunts` is empty.
    #[serde(default = "default_true")]
    pub shunt_enabled: bool,

    /// Maximum number of switched shunt outer-loop iterations.
    #[serde(default = "default_shunt_max_iter")]
    pub shunt_max_iter: usize,

    /// Switched shunt bank controls.  Each entry describes one shunt bank set
    /// at a bus.  Mutated in place during the solve.
    #[serde(default)]
    pub switched_shunts: Vec<SwitchedShunt>,

    /// Reference angle convention for output angles.
    ///
    /// Controls how angles are reported after the solve. Does not affect
    /// convergence or power flow physics. Default is `PreserveInitial` which
    /// matches MATPOWER's runpf convention.
    #[serde(default)]
    pub angle_reference: AngleReference,

    /// How point-to-point HVDC links in `network.hvdc.links` are modelled.
    ///
    /// Default `FixedSchedule` adds zero overhead when no DC lines are present.
    #[serde(default)]
    pub dc_line_model: DcLineModel,

    /// Maximum number of outer AC/DC sequential iterations (only used with
    /// `dc_line_model = SequentialAcDc`).
    #[serde(default = "default_dc_max_iter")]
    pub dc_max_iter: u32,

    /// DC power convergence criterion in MW (only used with `SequentialAcDc`).
    ///
    /// The outer loop exits when the maximum change in DC power across all
    /// converters is below this threshold.
    #[serde(default = "default_dc_tol_mw")]
    pub dc_tol_mw: f64,

    /// Enforce area interchange targets from `network.area_schedules`.
    ///
    /// When true, a three-tier outer loop drives each area's actual net
    /// interchange (sum of tie-line flows) toward `p_desired_mw`:
    ///   1. **APF dispatch** — generators with `agc_participation_factor > 0` share the mismatch
    ///      proportionally, with iterative clamp-and-redistribute at Pmin/Pmax.
    ///   2. **Area slack bus fallback** — if no APF generators exist, the area's
    ///      slack bus absorbs the error (skipped for the system swing bus).
    ///   3. **System swing bus** — any residual unabsorbed by tiers 1–2 is
    ///      picked up by the NR power balance at the swing bus.
    ///
    /// `ScheduledAreaTransfer` entries are added to `p_desired_mw` targets.
    /// Default: false.
    #[serde(default)]
    pub enforce_interchange: bool,

    /// Maximum outer-loop iterations for area interchange enforcement.
    #[serde(default = "default_interchange_max_iter")]
    pub interchange_max_iter: usize,

    /// Record per-iteration convergence history in the solution.
    ///
    /// When `true`, `PfSolution::convergence_history` is populated with
    /// `(iteration, max_mismatch_pu)` after each NR iteration. Useful for
    /// diagnosing convergence behaviour. Disabled by default to avoid
    /// allocation overhead in hot paths such as N-1 contingency screening.
    #[serde(default)]
    pub record_convergence_history: bool,

    /// Automatically reduce node-breaker topology before solving.
    ///
    /// When `true` and `network.topology` is `Some`, calls
    /// `surge_topology::rebuild_topology()` to reduce the node-breaker model to a
    /// bus-branch network before the solve. No-op when `topology` is
    /// `None`. Default: `false` (callers must invoke rebuild_topology explicitly).
    #[serde(default)]
    pub auto_reduce_topology: bool,

    /// Automatically merge zero-impedance branches before solving.
    ///
    /// When `true` (default), any in-service branch with `r²+x² < 1e-12` is
    /// collapsed via Union-Find bus merging (`merge_zero_impedance`) before
    /// the solve, and the solution is expanded back to the original bus
    /// numbering afterwards. This avoids the ill-conditioned `1e6 − j1e6`
    /// large-admittance substitute in the Y-bus, which degrades Jacobian
    /// condition number by O(n × 1e6) when multiple zero-impedance ties are
    /// present (common in CGMES 3-winding star decompositions).
    ///
    /// Set to `false` if you are calling `merge_zero_impedance` manually or
    /// need the raw merged/expanded bus indices in the returned solution.
    #[serde(default = "default_true")]
    pub auto_merge_zero_impedance: bool,
}

fn default_true() -> bool {
    true
}

fn default_vm_min() -> f64 {
    f64::NEG_INFINITY
}

fn default_vm_max() -> f64 {
    f64::INFINITY
}

fn is_neg_infinity(v: &f64) -> bool {
    v.is_infinite() && v.is_sign_negative()
}

fn is_pos_infinity(v: &f64) -> bool {
    v.is_infinite() && v.is_sign_positive()
}

fn default_oltc_max_iter() -> usize {
    20
}

fn default_shunt_max_iter() -> usize {
    20
}

fn default_par_max_iter() -> usize {
    20
}

fn default_dc_max_iter() -> u32 {
    25
}

fn default_dc_tol_mw() -> f64 {
    1.0
}

fn default_interchange_max_iter() -> usize {
    10
}

impl AcPfOptions {
    /// Inner NR kernel stall limit used by shared prepared and inline solve paths.
    pub fn inner_stall_limit(&self) -> u32 {
        if self.startup_policy == StartupPolicy::Adaptive
            && self.flat_start
            && !self.dc_warm_start
            && self.warm_start.is_none()
        {
            4
        } else {
            10
        }
    }
}

impl Default for AcPfOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-8,
            max_iterations: 100,
            flat_start: false,
            dc_warm_start: true,
            line_search: true,
            // No Vm clamping by default — matches MATPOWER behaviour.
            // Clamping during Newton iterations prevents convergence on
            // stressed networks (e.g. case2848rte) where the first flat-start
            // step naturally overshoots before quadratic recovery.
            vm_min: f64::NEG_INFINITY,
            vm_max: f64::INFINITY,
            warm_start: None,
            startup_policy: StartupPolicy::Adaptive,
            enforce_q_limits: true,
            skip_slack_q_limits: false,
            enforce_gen_p_limits: true,
            slack_participation: None,
            generator_slack_participation: None,
            distributed_slack: false,
            slack_attribution: SlackAttributionMode::Automatic,
            detect_islands: true,
            oltc_enabled: true,
            oltc_max_iter: 20,
            oltc_controls: Vec::new(),
            par_enabled: true,
            par_max_iter: 20,
            par_controls: Vec::new(),
            shunt_enabled: true,
            shunt_max_iter: 20,
            switched_shunts: Vec::new(),
            angle_reference: AngleReference::PreserveInitial,
            dc_line_model: DcLineModel::FixedSchedule,
            dc_max_iter: 25,
            dc_tol_mw: 1.0,
            enforce_interchange: false,
            interchange_max_iter: 10,
            record_convergence_history: false,
            auto_reduce_topology: false,
            q_sharing: QSharingMode::Capability,
            incremental_q_limits: false,
            auto_merge_zero_impedance: true,
        }
    }
}

/// Errors returned by the AC power flow solver.
#[derive(Debug, thiserror::Error)]
pub enum AcPfError {
    /// The input network contains no buses.
    #[error("network has no buses")]
    EmptyNetwork,

    /// No bus is designated as the slack (reference) bus.
    #[error("network has no slack bus")]
    NoSlackBus,

    /// Newton-Raphson iterations did not converge within the allowed limit.
    #[error(
        "Newton-Raphson did not converge in {iterations} iterations (max mismatch: {max_mismatch:.2e})"
    )]
    NotConverged {
        /// Number of NR iterations performed before giving up.
        iterations: u32,
        /// Largest absolute power mismatch (p.u.) on the final iteration.
        max_mismatch: f64,
        /// External bus number with the largest mismatch on the final iteration.
        worst_bus: Option<u32>,
        /// Partial voltage magnitudes (p.u.) from the last NR iterate.
        /// `None` when the solver did not reach an iterate (e.g. setup failure).
        partial_vm: Option<Vec<f64>>,
        /// Partial voltage angles (rad) from the last NR iterate.
        /// `None` when the solver did not reach an iterate (e.g. setup failure).
        partial_va: Option<Vec<f64>>,
    },

    /// The network model is structurally invalid (e.g. dangling branch endpoints).
    #[error("invalid network: {0}")]
    InvalidNetwork(String),

    /// Solver options are inconsistent or out of range.
    #[error("invalid solver options: {0}")]
    InvalidOptions(String),

    /// LU factorization failed due to a singular or near-singular Jacobian.
    #[error("numerical failure in LU factorization: {0}")]
    NumericalFailure(String),
}
