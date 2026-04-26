// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Policy knobs for the GO C3 adapter pipeline.
//!
//! Mirrors Python's `markets/go_c3/adapter.py::AdapterPolicy`, narrowed to
//! the fields that affect *network construction*. Solver selection, pricing
//! passes, Benders orchestration, log level, and AC target-tracking overrides
//! all live in the dispatch-request builder (phase 3) and will be layered
//! onto an extended policy type there.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Network formulation: DC linearized or full AC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoC3Formulation {
    /// DC power flow — linear, no reactive variables.
    #[default]
    Dc,
    /// AC power flow — full nonlinear, with voltage and reactive power.
    Ac,
}

/// AC reconciliation strategy after a DC solve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoC3AcReconcileMode {
    /// Run an AC OPF redispatch pass using the DC solution as a seed.
    #[default]
    AcDispatch,
    /// Skip AC reconciliation entirely; keep the DC solve as the final answer.
    None,
}

/// How consumers (loads) are modelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoC3ConsumerMode {
    /// Curtailable dispatchable load tranches above the per-period `p_lb`
    /// floor. The baseline can shed load to respect reserve/thermal limits.
    #[default]
    Dispatchable,
    /// Fixed bus load profile pinned to the per-period `p_ub` (prize-mode
    /// behaviour for commitment-less scenarios).
    Fixed,
}

/// How unit commitment is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoC3CommitmentMode {
    /// SCUC optimizes startup/shutdown decisions.
    #[default]
    Optimize,
    /// Commitment is pinned to each device's `initial_status.on_status`.
    FixedInitial,
    /// All committable devices are forced on for the entire horizon.
    AllCommitted,
}

/// Slack bus selection strategy when the GO C3 input has no explicit Slack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoC3SlackInferenceMode {
    /// Honour explicit Slack labels only; otherwise leave the `network.rs`
    /// fallback (first PV / first bus) in place.
    Explicit,
    /// Select the bus that hosts the single largest reactive-capable
    /// producer, scored by `(peak_p_mw, q_range_mvar)`. Mirrors Python
    /// `build_surge_network` lines 2667-2706.
    #[default]
    ReactiveCapability,
}

/// Adapter policy — configures how the GO C3 problem is mapped into Surge.
///
/// Fields default to the baseline configuration the Python adapter uses for
/// prize-mode solves. See each field for what it affects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Policy {
    pub formulation: GoC3Formulation,
    pub ac_reconcile_mode: GoC3AcReconcileMode,
    pub consumer_mode: GoC3ConsumerMode,
    pub commitment_mode: GoC3CommitmentMode,
    pub slack_mode: GoC3SlackInferenceMode,

    /// GO C3 §7 `AllowSwitching`: when false, branch on/off binaries are
    /// pinned to `in_service`. When true, branches may be switched on/off
    /// as MIP decision variables.
    pub allow_branch_switching: bool,
    /// When `allow_branch_switching` is true, restrict switchability to this
    /// subset of branch UIDs. `None` means all branches are switchable when
    /// switching is allowed.
    pub switchable_branch_uids: Option<BTreeSet<String>>,

    /// Multiplier on the branch thermal slack penalty ($/MVA) used by the
    /// SCUC (DC MIP) stage. Applied on top of the GO C3 input's
    /// `violation_cost.s_vio_cost`. Default `10.0` — empirically the
    /// sweet spot across event4 73-bus scenarios: SCUC is stiff enough
    /// on thermal compliance to avoid committing units that force
    /// downstream SCED thermal slack, while SCED stays at GO C3's
    /// native penalty so the AC NLP can converge without penalty bleed.
    pub scuc_thermal_penalty_multiplier: f64,
    /// Multiplier on the branch thermal slack penalty ($/MVA) used by the
    /// SCED (AC NLP) stage — i.e. `AcOpfOptions.thermal_limit_slack_penalty_per_mva`.
    /// Default `1.0` preserves GO C3 prize-mode penalty scaling.
    pub sced_thermal_penalty_multiplier: f64,

    /// Multiplier on every reserve-shortfall cost ($/pu-h or $/MVAr-h) fed
    /// into the SCUC (DC MIP). Applied to both the system-level
    /// `PenaltyCurve` and every zonal `shortfall_cost_per_unit`, so a
    /// single knob stiffens the LP against taking reserve shortfall in
    /// preference to committing more units. Default `1.0` preserves GO
    /// C3 prize-mode penalty scaling; larger values push the LP toward
    /// a solution whose validator-scored reserve shortfall is smaller.
    pub scuc_reserve_penalty_multiplier: f64,

    /// When `true`, before AC SCED solves, expand each branch's per-period
    /// thermal limit by the leftover overload slack in the DC SCUC solution.
    /// Prevents the AC NLP from being asked to find a power-flow point that
    /// satisfies impossible flows (a common cause of SCED retry-grid
    /// exhaustion on stressed scenarios). Off by default.
    pub relax_sced_branch_limits_to_dc_slack: bool,
    /// Extra MVA headroom added on top of `(rating + dc_slack)` so the AC
    /// NLP isn't right at the new edge. Only used when
    /// `relax_sced_branch_limits_to_dc_slack` is `true`. Default `0.5`.
    pub sced_branch_relax_margin_mva: f64,

    /// When `true`, completely drop branch thermal limits from the AC SCED
    /// stage (sets `AcOpfOptions.enforce_thermal_limits = false`). Useful
    /// for diagnosing whether the AC NLP's failure to converge stems from
    /// the SCUC handing it commitments whose flows can't fit in the
    /// network's thermal envelope. Off by default.
    pub disable_sced_thermal_limits: bool,
    /// Multiplier applied to GO C3's per-pu bus P/Q balance penalty before
    /// it lands on `AcOpfOptions.bus_*_power_balance_slack_penalty_per_*`.
    /// The canonical default `100.0` makes Ipopt strictly prefer physical
    /// relief over slack absorption (see `ac_opf.rs::BUS_BALANCE_SAFETY_MULTIPLIER`).
    /// Lower values (e.g. `1.0`) loosen the penalty so the AC NLP can
    /// converge with non-zero residual bus balance — useful when a
    /// scenario is on the edge of NLP feasibility.
    pub sced_bus_balance_safety_multiplier: f64,
    /// When `true`, set `runtime.ac_relax_committed_pmin_to_zero = true` on
    /// the AC SCED stage. Inside the AC NLP this drives every committed
    /// non-storage generator's `pmin` to 0, giving the NLP freedom to
    /// dispatch committed units down to zero MW. Off by default.
    pub ac_relax_committed_pmin_to_zero: bool,
    /// Override `AcOpfOptions.tolerance` on the AC SCED stage. `None`
    /// keeps Ipopt's default (typically `1e-8`); higher values (e.g.
    /// `1e-4`) loosen the convergence criterion so Ipopt accepts a
    /// less-precise solution rather than running out of attempts.
    pub sced_ac_opf_tolerance: Option<f64>,
    /// Override `AcOpfOptions.max_iterations` on the AC SCED stage. `None`
    /// keeps the GO C3 default of 3000. Increase when Ipopt is making
    /// slow but steady progress and "maximum iterations exceeded" is the
    /// failure mode.
    pub sced_ac_opf_max_iterations: Option<u32>,
    /// When `true`, set `AcOpfOptions.enforce_regulated_bus_vm_targets = true`
    /// on the AC SCED stage. Pins V at PV buses to the generator setpoint,
    /// removing voltage as a free variable at those buses. Useful when
    /// Ipopt converges to a non-winner V basin even with a winner V/θ
    /// warm-start — pinning V forces the angle pattern to match.
    pub sced_enforce_regulated_bus_vm_targets: bool,

    /// When > 0, select the top-N Q-capable producers (by
    /// `q_range × load_within_3_hops²`) whose cumulative Q range ≥
    /// `factor × peak_system_load_mw`, force them must-run at all
    /// periods, and pin their SCED Pg to the midpoint of `[p_lb, p_ub]`.
    /// Resolves Ipopt convergence-basin issues on stressed AC scenarios
    /// by tightening Pg bounds on structurally important Q-capable
    /// generators. 0.0 = disabled (default). Typical value: 0.2.
    pub reactive_support_pin_factor: f64,

    /// When `true`, the SCUC stage re-solves the MIP as an LP with
    /// commitment binaries fixed to recover LMP duals. Adds ~15-25s per
    /// 617-bus SCUC (scales with constraint count). Disabled by default
    /// because GO C3 scoring doesn't consume LMPs — they're only used
    /// for diagnostics. Leave off unless pricing data is actively needed.
    pub run_pricing: bool,

    /// Pre-seed iter 0 of the SCUC iterative-screening security loop with
    /// this many top-ranked (contingency, monitored) cuts per period. `0`
    /// disables pre-seeding (default). The ranking is dispatch-free
    /// (topology + emergency ratings only) so the cost is negligible
    /// compared with one SCUC re-solve. Targets reducing outer iteration
    /// count on contingency-heavy scenarios. Typical values: 20–50 per
    /// period on 73-bus; tune per network.
    pub scuc_security_preseed_count_per_period: usize,

    /// Maximum outer-loop iterations for the iterative SCUC N-1 security
    /// screening (preseed → solve → check violations → add cuts →
    /// repeat). `1` runs a single SCUC solve with only the preseeded
    /// cuts; larger values let the loop absorb post-solve violations.
    /// Matches `surge_dispatch::SecurityDispatchSpec::max_iterations`.
    pub scuc_security_max_iterations: usize,

    /// Cap on the number of new flowgate cuts added per outer iteration
    /// of the iterative SCUC security loop. Only active when
    /// `scuc_security_max_iterations > 1`; with a single iteration the
    /// cap is irrelevant since no second solve consumes the added cuts.
    pub scuc_security_max_cuts_per_iteration: usize,

    /// Cold-start loss-factor warm-start mode used on the first SCUC
    /// security iteration. Subsequent iterations always warm-start
    /// from the prior iteration's converged `dloss_dp` regardless of
    /// this setting. Modes:
    ///
    /// * `None` — no cold-start; first MIP is lossless, refinement LP
    ///   corrects after.
    /// * `Some(("uniform", rate))` — every bus seeded with
    ///   `dloss = rate`, `total_losses = rate × total_load`. Crude but
    ///   free.
    /// * `Some(("load_pattern", rate))` (default `("load_pattern",
    ///   0.02)`) — per-bus `dloss` from loss-PTDF applied to the load
    ///   vector; calibrated to `rate × total_load`. Captures topology
    ///   asymmetry without running DC PF.
    /// * `Some(("dc_pf", 0.0))` — DC power flow on initial gen
    ///   setpoints; most accurate cold-start, costs ~ms per period.
    ///   Falls back to uniform if the resulting loss estimate exceeds
    ///   5% of load (guards against degenerate PF with only a partial
    ///   commitment available).
    ///
    /// Serialized as a 2-tuple `(mode_str, rate)` for convenient
    /// Python-side construction; the rate is ignored for `dc_pf`.
    pub scuc_loss_factor_warm_start: Option<(String, f64)>,

    /// Override for `DispatchRequest::network::loss_factors::max_iterations`.
    /// Default `Some(0)` — trust the warm-start entirely, skip the
    /// refinement LP. `None` preserves the historical 1-iteration
    /// behaviour; higher values run additional refinement rounds.
    pub scuc_loss_factor_max_iterations: Option<usize>,

    /// Per-period AC SCED concurrency.
    ///
    /// * `None` (default) — sequential per-period AC SCED, each period
    ///   warm-starting from the prior period's `OpfSolution`.
    /// * `Some(n)` (n ≥ 2) — run AC SCED periods on a rayon thread pool
    ///   of size `n`. AC→AC warm-start is dropped; each period falls
    ///   back to its own per-period AC power-flow warm-start. The
    ///   `prev_dispatch_mw` anchor used for ramp constraints comes from
    ///   the per-period `generator_dispatch_bounds` midpoint —
    ///   equivalent to the DC SCUC dispatch when (as in the standard
    ///   GO C3 reconcile pipeline) bounds are pinned around the source-
    ///   stage dispatch. Networks with in-service storage devices fall
    ///   back to sequential automatically (storage SoC continuity needs
    ///   sequential threading).
    /// * `Some(0)` or `Some(1)` are normalized to sequential.
    pub ac_sced_period_concurrency: Option<usize>,

    /// Static relative MIP optimality gap for the SCUC commitment solve
    /// (e.g. `0.0001` = 0.01 %). `None` uses the solver default. When a
    /// `commitment_mip_gap_schedule` is also provided, the backend treats
    /// this value as the terminal safety-net gap; otherwise it's the
    /// only termination criterion.
    pub commitment_mip_rel_gap: Option<f64>,

    /// Wall-clock time limit for the SCUC commitment solve (seconds).
    /// `None` disables the limit.
    pub commitment_time_limit_secs: Option<f64>,

    /// Time-varying MIP gap schedule for the SCUC commitment solve:
    /// piecewise-constant breakpoints `(t_secs, gap)` sorted by `t_secs`.
    /// At wall time `t` the solver terminates once the current incumbent's
    /// gap is within the `gap` of the latest breakpoint with `t_secs <= t`.
    /// When set, the static `commitment_mip_rel_gap` acts as a terminal
    /// safety net. Backends without progress-callback support ignore this
    /// field and fall back to the static value.
    pub commitment_mip_gap_schedule: Option<Vec<(f64, f64)>>,

    /// When `true`, drop flowgate enforcement entirely on the SCUC LP —
    /// normal flowgates *and* the explicit N-1 contingency flowgates are
    /// disabled. Diagnostic-only: production solves need this `false` for
    /// GO C3 security compliance. Useful to measure the MIP solve cost
    /// without the security overhead (and to validate that the progress
    /// callback / gap schedule is firing on a tractable problem).
    pub disable_flowgates: bool,

    /// When `true`, skip the SCUC MIP warm-start pipeline entirely —
    /// `try_build_mip_primal_start` returns immediately with no primal
    /// start, saving the six helper LP/MIP pre-solves (load-cover,
    /// reduced-relaxed, reduced-core-MIP, conservative, plus cold-dense
    /// refinements).
    ///
    /// Defaulted `true` for the GO C3 adapter: the helpers cost ~9 s on
    /// 617-bus and the auto short-circuit only fires after the first
    /// 1.8 s helper has already run. On the cases we've measured,
    /// Gurobi solves the SCUC cold within the caller's time budget, so
    /// paying the warm-start tax by default isn't worth it. Set `false`
    /// explicitly when a scenario needs the warm start to converge.
    pub disable_scuc_warm_start: bool,

    /// Diagnostic knob: when `true`, pin every per-bus power-balance
    /// slack column in SCUC to `col_upper = 0`, turning the bus-balance
    /// rows into firm constraints. Used to measure the LP weight of the
    /// soft-balance slack family. Off by default — production solves
    /// need the slacks so infeasible inputs still produce a solve.
    pub scuc_firm_bus_balance_slacks: bool,

    /// Diagnostic knob: when `true`, pin every branch thermal slack
    /// column in SCUC to `col_upper = 0`, turning the thermal rows into
    /// firm constraints. Different from `enforce_thermal_limits = false`
    /// on the DispatchRequest (which skips the rows entirely); this
    /// preserves the rows but removes the slack escape hatch. Off by
    /// default.
    pub scuc_firm_branch_thermal_slacks: bool,

    /// When `true`, drop SCUC branch thermal enforcement entirely —
    /// sets `DispatchRequest.network.thermal_limits.enforce = false` for
    /// the DC SCUC stage. Diagnostic knob to measure the MIP cost
    /// contributed by the thermal-limit row family (and its slacks).
    /// Off by default; production solves need this `false`.
    pub disable_scuc_thermal_limits: bool,

    /// Diagnostic: zero out the `power_balance_penalty` on the SCUC
    /// request so per-bus balance slack is free. Combined with a loose
    /// thermal cap this effectively decouples the network — the MIP
    /// solver sees only commitment + capacity + reserves. Useful to
    /// probe whether the UC part is solvable independently of DC-PF.
    /// Off by default.
    pub scuc_copperplate: bool,

    /// Scaling multiplier applied to both segments of the SCUC
    /// `power_balance_penalty` (curtailment and excess). The default
    /// `1.0` preserves the `$1e7/MW curtailment, $1e5/MW excess`
    /// ship-penalty. On stressed networks the LP relaxation exploits
    /// the large gap between fractional-commitment (cheap) and
    /// integer-commitment (forces bus slack at $1e7/MW) to produce a
    /// dual bound Gurobi can't close inside heuristics — 6049-bus D1
    /// observed this pattern, with Gurobi returning obj=1e14 dummy
    /// incumbents and zero simplex iterations. Lowering the
    /// multiplier makes bus slack cheaper so integer-commitment
    /// solutions don't blow up. Typical values:
    ///   * `0.01` → $1e5 / $1e3 — strong reduction, often enough to
    ///     unstick large SCUCs.
    ///   * `0.1` → $1e6 / $1e4 — moderate.
    ///   * `0.0` → effectively [`scuc_copperplate`].
    ///
    /// Ignored when `scuc_copperplate` is `true`.
    pub scuc_power_balance_penalty_multiplier: f64,

    /// When `true`, drop the SCUC per-bus power-balance row family
    /// and its `pb_*` slack column blocks from the LP entirely,
    /// replacing them with a single system-balance row per period.
    /// The `theta` / thermal row blocks remain allocated but become
    /// vestigial (no KCL couples them to `pg`), so the MIP sees a
    /// copperplate commitment problem. DC branch losses are accounted
    /// for at the system level via the `loss_factor_warm_start_mode`
    /// rate times total period load.
    ///
    /// Use when per-bus balance is blocking Gurobi convergence on
    /// large networks (observed on 6049-bus D1 where default pb
    /// penalties produced obj=1e14 dummy incumbents with zero simplex
    /// iterations). AC SCED still enforces full nodal physics when
    /// it runs downstream. Off by default.
    pub scuc_disable_bus_power_balance: bool,

    /// How losses are represented inside SCUC across security
    /// iterations when running in `scuc_disable_bus_power_balance`
    /// (system-row) mode. Three modes:
    ///
    /// * [`GoC3ScucLossTreatment::Static`] (default) — single scalar
    ///   `rate × total_load` per period, baked into the system-row
    ///   RHS. Same value every security iteration. Cheapest, but
    ///   ignores realized dispatch.
    /// * [`GoC3ScucLossTreatment::ScalarFeedback`] — after each
    ///   security iteration's repaired DC PF, compute realized total
    ///   losses per period and feed back as next iteration's RHS.
    ///   Damped with asymmetric upward bias (under-commitment costs
    ///   more than over because AC SCED can't commit new units).
    /// * [`GoC3ScucLossTreatment::PenaltyFactors`] — full marginal
    ///   loss factors `(1 − LF_g)` on every injection coefficient,
    ///   with linearization-point RHS correction. Distributed-load
    ///   slack reference. Recovers locational signal — a renewable
    ///   at a high-loss bus contributes effective MW < raw MW, so
    ///   SCUC commits more thermal up front.
    ///
    /// In per-bus-balance mode (`scuc_disable_bus_power_balance =
    /// false`) this knob is ignored — the existing per-bus
    /// `iterate_loss_factors` machinery handles loss representation
    /// directly.
    pub scuc_loss_treatment: GoC3ScucLossTreatment,
}

/// SCUC system-row loss treatment selector — public mirror of
/// [`surge_dispatch::request::network::ScucLossTreatment`] for the GO C3
/// policy. Serialized as snake_case strings (`"static"`,
/// `"scalar_feedback"`, `"penalty_factors"`).
///
/// Default: `ScalarFeedback`. Validated 2026-04-25 on a 617-bus event4
/// cut (6 scenarios across D1/D2/D3, SW0): scalar feedback beat
/// `Static` on every scenario, +$1.81 M aggregate validator surplus
/// (+0.094 %) at near-static wall cost. `PenaltyFactors` came in
/// second on every scenario; available as an opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoC3ScucLossTreatment {
    Static,
    #[default]
    ScalarFeedback,
    PenaltyFactors,
}

impl Default for GoC3Policy {
    fn default() -> Self {
        Self {
            formulation: GoC3Formulation::default(),
            ac_reconcile_mode: GoC3AcReconcileMode::default(),
            consumer_mode: GoC3ConsumerMode::default(),
            commitment_mode: GoC3CommitmentMode::default(),
            slack_mode: GoC3SlackInferenceMode::default(),
            allow_branch_switching: false,
            switchable_branch_uids: None,
            scuc_thermal_penalty_multiplier: 10.0,
            sced_thermal_penalty_multiplier: 1.0,
            scuc_reserve_penalty_multiplier: 1.0,
            relax_sced_branch_limits_to_dc_slack: false,
            sced_branch_relax_margin_mva: 0.5,
            disable_sced_thermal_limits: false,
            sced_bus_balance_safety_multiplier: 100.0,
            ac_relax_committed_pmin_to_zero: false,
            sced_ac_opf_tolerance: None,
            sced_ac_opf_max_iterations: None,
            sced_enforce_regulated_bus_vm_targets: false,
            reactive_support_pin_factor: 0.0,
            run_pricing: false,
            scuc_security_preseed_count_per_period: 0,
            scuc_security_max_iterations: 5,
            scuc_security_max_cuts_per_iteration: 2_500,
            scuc_loss_factor_warm_start: Some(("load_pattern".to_string(), 0.02)),
            scuc_loss_factor_max_iterations: Some(0),
            ac_sced_period_concurrency: None,
            commitment_mip_rel_gap: None,
            commitment_time_limit_secs: None,
            commitment_mip_gap_schedule: None,
            disable_flowgates: false,
            disable_scuc_warm_start: true,
            scuc_firm_bus_balance_slacks: false,
            scuc_firm_branch_thermal_slacks: false,
            disable_scuc_thermal_limits: false,
            scuc_copperplate: false,
            scuc_power_balance_penalty_multiplier: 1.0,
            scuc_disable_bus_power_balance: true,
            scuc_loss_treatment: GoC3ScucLossTreatment::default(),
        }
    }
}

impl GoC3Policy {
    /// True when AC voltage controls (generator voltage setpoints, reactive
    /// support qualification, slack fallback) should be preserved on the
    /// network. Mirrors Python `_preserve_ac_voltage_controls`.
    pub fn preserve_ac_voltage_controls(&self) -> bool {
        self.formulation == GoC3Formulation::Ac
            || self.ac_reconcile_mode != GoC3AcReconcileMode::None
    }

    /// True when a given branch UID is eligible for on/off switching under
    /// this policy. When `allow_branch_switching` is false, nothing is
    /// switchable. When it is true and no subset is provided, everything is
    /// switchable. Otherwise only UIDs in the subset qualify.
    pub fn is_branch_switchable(&self, uid: &str) -> bool {
        if !self.allow_branch_switching {
            return false;
        }
        match &self.switchable_branch_uids {
            None => true,
            Some(set) => set.contains(uid),
        }
    }
}
