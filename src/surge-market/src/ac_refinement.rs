// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical AC refinement runtime — retry grid, feedback providers,
//! commitment probes.
//!
//! This module hosts the canonical orchestration layer that sits on top
//! of a single [`MarketStage`]'s solve. When a stage has a
//! [`RetryPolicy`] attached, [`solve_market_workflow`] dispatches the
//! stage through [`RefinementRuntime`] instead of calling
//! [`DispatchModel::solve_with_options`] directly.
//!
//! The runtime drives a nested retry grid — `relax_pmin × opf_attempts
//! × nlp_solver_candidates × band_attempts` — cloning the stage's
//! request per attempt so every iteration's input is preserved for
//! debugging and so multiple probes can compose without reasoning about
//! mutation order.
//!
//! ## Extension points
//!
//! * [`FeedbackProvider`] — pre-solve hooks that inspect the prior
//!   stage's solution and mutate the current stage's request (e.g.
//!   target-tracking overrides derived from DC reduced costs).
//! * [`CommitmentProbe`] — between-iteration hooks that may re-solve
//!   SCUC with mutated commitment constraints (e.g. the pmin=0 AC
//!   probe that drives decommit cuts).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use surge_dispatch::{
    DispatchError, DispatchModel, DispatchRequest, DispatchSolution, DispatchSolveOptions,
};
use surge_opf::AcOpfOptions;
use surge_opf::backends::{NlpSolver, ac_opf_nlp_solver_from_str};

use crate::ac_reconcile::{ProducerDispatchPinning, apply_producer_dispatch_pinning};
use crate::workflow::{DispatchPinningConfig, pin_generator_dispatch_bounds};

// ─── Configuration types ────────────────────────────────────────────

/// Named `AcOpfOptions` override pattern applied per retry attempt.
#[derive(Clone, Debug)]
pub struct OpfAttempt {
    pub name: String,
    pub overrides: Option<AcOpfOptions>,
}

impl OpfAttempt {
    pub fn new(name: impl Into<String>, overrides: Option<AcOpfOptions>) -> Self {
        Self {
            name: name.into(),
            overrides,
        }
    }
}

/// Dispatch-pinning band configuration for a retry attempt.
///
/// The default attempt uses a narrow band around stage-1's dispatch.
/// The optional wide-band fallback widens the band and lets more
/// generators move when the default AC redispatch lands on a high
/// penalty-cost solution.
#[derive(Clone, Debug)]
pub struct BandAttempt {
    pub name: String,
    pub band_fraction: f64,
    pub band_floor_mw: f64,
    pub band_cap_mw: f64,
    pub max_additional_bandable_producers: usize,
    /// Optional anchor set: resources in this list are skipped by the
    /// producer-dispatch-pinning narrowing step and keep their full
    /// physical `[P_min, P_max]` band for the attempt. Intended as a
    /// last-ditch retry rung — give a handful of high-Q-range generators
    /// full real-power flexibility so the AC NLP has enough degrees of
    /// freedom to balance bus P/Q when the default + wide-band attempts
    /// can't find a feasible solution. Empty = no anchors (default).
    pub anchor_resource_ids: Vec<String>,
}

impl BandAttempt {
    pub fn default_band() -> Self {
        Self {
            name: "default".to_string(),
            band_fraction: 0.05,
            band_floor_mw: 1.0,
            band_cap_mw: 1.0e9,
            max_additional_bandable_producers: 0,
            anchor_resource_ids: Vec::new(),
        }
    }

    pub fn wide_band_retry(fraction: f64, floor_mw: f64, cap_mw: f64, max_extra: usize) -> Self {
        Self {
            name: "wide_band_retry".to_string(),
            band_fraction: fraction,
            band_floor_mw: floor_mw,
            band_cap_mw: cap_mw,
            max_additional_bandable_producers: max_extra,
            anchor_resource_ids: Vec::new(),
        }
    }

    /// Anchor fallback: runs the wide-band config but also lets the
    /// named resources off pinning entirely (use their full
    /// `[P_min, P_max]`). The selection logic (which resource IDs to
    /// pass) lives next to each workflow.
    pub fn anchor_widest_q(
        fraction: f64,
        floor_mw: f64,
        cap_mw: f64,
        max_extra: usize,
        anchor_resource_ids: Vec<String>,
    ) -> Self {
        Self {
            name: "anchor_widest_q".to_string(),
            band_fraction: fraction,
            band_floor_mw: floor_mw,
            band_cap_mw: cap_mw,
            max_additional_bandable_producers: max_extra,
            anchor_resource_ids,
        }
    }
}

/// One NLP solver candidate — `None` means "default" (use whatever
/// the base `DispatchSolveOptions` carries).
pub type NlpSolverCandidate = Option<String>;

/// HVDC handoff strategy for a retry attempt.
///
/// The AC SCED's HVDC direction is not pinned by the canonical workflow
/// (AC NLP sees HVDC P as a free variable bounded by `[pdc_min, pdc_max]`),
/// but the DC warm-start bus voltages effectively anchor Ipopt near the
/// DC stage's HVDC choice. When the DC LP is degenerate (many zero-cost
/// renewables with no binding transmission → flat LMPs), the DC solver
/// picks an arbitrary HVDC direction that may land AC in an
/// infeasibility basin. This enum drives the fallback retry: override
/// `runtime.fixed_hvdc_dispatch` to force a different direction. In
/// practice, flipping the DC stage's HVDC direction (or pinning it to
/// zero) is often enough to break the NLP out of the bad basin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HvdcStrategy {
    /// No override — let AC NLP start from its natural equilibrium
    /// (anchored by the DC warm-start bus voltages).
    #[default]
    Default,
    /// Pin every HVDC link's per-period P to the **negation** of the
    /// prior stage's HVDC dispatch. Also negates Q (from-end and
    /// to-end). Use when the DC-anchored direction traps the NLP.
    Flipped,
    /// Pin every HVDC link's per-period P, Q_fr, Q_to to **zero**.
    /// Neutral — strips the DC anchor entirely and lets AC find its
    /// own P profile (well, with HVDC out of the picture).
    Neutral,
}

/// Named HVDC strategy applied per retry attempt. Used alongside
/// [`OpfAttempt`] / [`BandAttempt`] to form the retry grid.
#[derive(Clone, Debug)]
pub struct HvdcAttempt {
    pub name: String,
    pub strategy: HvdcStrategy,
}

impl HvdcAttempt {
    pub fn default_attempt() -> Self {
        Self {
            name: "default".to_string(),
            strategy: HvdcStrategy::Default,
        }
    }

    pub fn flipped() -> Self {
        Self {
            name: "hvdc_flipped".to_string(),
            strategy: HvdcStrategy::Flipped,
        }
    }

    pub fn neutral() -> Self {
        Self {
            name: "hvdc_neutral".to_string(),
            strategy: HvdcStrategy::Neutral,
        }
    }
}

/// Retry / feedback / probe configuration for a single market stage.
#[derive(Clone, Default)]
pub struct RetryPolicy {
    /// Outer sweep over `runtime.ac_relax_committed_pmin_to_zero`.
    /// Python default `[false, true]`.
    pub relax_pmin_sweep: Vec<bool>,

    /// Named AC-OPF options patches tried in order.
    /// Python default `[go_validator_costs, strict_bus_balance, no_thermal_limits]`.
    pub opf_attempts: Vec<OpfAttempt>,

    /// NLP solver names tried in order; `None` entry = default solver.
    pub nlp_solver_candidates: Vec<NlpSolverCandidate>,

    /// Dispatch-pinning band attempts. The first succeeds fast; if
    /// the default band's solution penalty exceeds
    /// `wide_band_penalty_threshold_dollars`, subsequent attempts run
    /// and the lowest-penalty solution is returned.
    pub band_attempts: Vec<BandAttempt>,

    /// Penalty threshold (dollars) above which the runtime tries the
    /// next band attempt instead of returning the default.
    pub wide_band_penalty_threshold_dollars: f64,

    /// HVDC-override fallback attempts. Tried as an inner loop under
    /// each `(relax_pmin, opf, nlp, band)` cell. The first attempt is
    /// typically [`HvdcAttempt::default_attempt`] (no override); if the
    /// resulting bus P or Q slack exceeds
    /// `hvdc_retry_bus_slack_threshold_mw` and more attempts remain,
    /// the runtime retries with the next attempt's strategy
    /// (typically `flipped` first, then `neutral`). Empty = single
    /// default attempt (no fallback). See [`HvdcStrategy`] for why
    /// this fallback exists.
    pub hvdc_attempts: Vec<HvdcAttempt>,

    /// Bus-balance slack threshold (MW / MVAr, applied to both
    /// `penalty_summary.power_balance_p_total_mw` and
    /// `power_balance_q_total_mvar`) above which the HVDC fallback
    /// triggers. Set to `f64::INFINITY` to disable the fallback while
    /// still evaluating the `default` HVDC attempt. Typical default:
    /// `5.0` MW/MVAr.
    pub hvdc_retry_bus_slack_threshold_mw: f64,

    /// Debug escape hatch: if true, the first attempt's exception
    /// propagates immediately rather than getting swallowed by the
    /// retry loop. Mirrors `SURGE_AC_HARD_FAIL_FIRST_ATTEMPT=1`.
    pub hard_fail_first_attempt: bool,

    /// Pre-solve mutators applied once per iteration *before* the
    /// retry grid runs. See [`FeedbackProvider`].
    pub feedback_providers: Vec<Arc<dyn FeedbackProvider>>,

    /// Between-iteration mutators that may rewrite the request after
    /// a successful solve (e.g. the pmin=0 decommit probe). See
    /// [`CommitmentProbe`].
    pub commitment_probes: Vec<Arc<dyn CommitmentProbe>>,

    /// Maximum number of outer refinement iterations. Iteration 0 is
    /// the base solve; iterations 1..=max_iterations are triggered
    /// by commitment probes. `0` disables probe-driven iteration.
    pub max_iterations: usize,
}

impl RetryPolicy {
    /// Empty policy — equivalent to a single solve with no retries.
    pub fn noop() -> Self {
        Self {
            relax_pmin_sweep: vec![false],
            opf_attempts: vec![OpfAttempt::new("default", None)],
            nlp_solver_candidates: vec![None],
            band_attempts: vec![BandAttempt::default_band()],
            wide_band_penalty_threshold_dollars: f64::INFINITY,
            hvdc_attempts: vec![HvdcAttempt::default_attempt()],
            hvdc_retry_bus_slack_threshold_mw: f64::INFINITY,
            hard_fail_first_attempt: false,
            feedback_providers: Vec::new(),
            commitment_probes: Vec::new(),
            max_iterations: 0,
        }
    }

    /// Default GO-style retry policy: relax-pmin sweep, three OPF
    /// attempts (soft / strict / no-thermal), default-then-wide band,
    /// HVDC fallback (default → flipped → neutral), no probes/feedback.
    /// Callers add their own feedback providers and probes via the
    /// builder methods on top of this.
    pub fn goc3_default() -> Self {
        Self {
            relax_pmin_sweep: vec![false, true],
            opf_attempts: vec![
                OpfAttempt::new("go_validator_costs", None),
                OpfAttempt::new("strict_bus_balance", None),
                OpfAttempt::new("no_thermal_limits", None),
            ],
            nlp_solver_candidates: vec![None],
            band_attempts: vec![BandAttempt::default_band()],
            wide_band_penalty_threshold_dollars: 1.0e6,
            hvdc_attempts: vec![
                HvdcAttempt::default_attempt(),
                HvdcAttempt::flipped(),
                HvdcAttempt::neutral(),
            ],
            hvdc_retry_bus_slack_threshold_mw: 5.0,
            hard_fail_first_attempt: false,
            feedback_providers: Vec::new(),
            commitment_probes: Vec::new(),
            max_iterations: 2,
        }
    }

    pub fn with_feedback(mut self, provider: Arc<dyn FeedbackProvider>) -> Self {
        self.feedback_providers.push(provider);
        self
    }

    pub fn with_commitment_probe(mut self, probe: Arc<dyn CommitmentProbe>) -> Self {
        self.commitment_probes.push(probe);
        self
    }
}

impl std::fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("relax_pmin_sweep", &self.relax_pmin_sweep)
            .field("opf_attempts", &self.opf_attempts.len())
            .field("nlp_solver_candidates", &self.nlp_solver_candidates)
            .field("band_attempts", &self.band_attempts.len())
            .field(
                "wide_band_penalty_threshold_dollars",
                &self.wide_band_penalty_threshold_dollars,
            )
            .field("hvdc_attempts", &self.hvdc_attempts.len())
            .field(
                "hvdc_retry_bus_slack_threshold_mw",
                &self.hvdc_retry_bus_slack_threshold_mw,
            )
            .field("hard_fail_first_attempt", &self.hard_fail_first_attempt)
            .field("feedback_providers", &self.feedback_providers.len())
            .field("commitment_probes", &self.commitment_probes.len())
            .field("max_iterations", &self.max_iterations)
            .finish()
    }
}

// ─── Extension traits ───────────────────────────────────────────────

/// Context handed to feedback providers when they mutate the current
/// stage's request.
pub struct FeedbackCtx<'a> {
    pub stage_id: &'a str,
    pub iteration: usize,
    pub prior_stage_solution: Option<&'a DispatchSolution>,
}

/// Pre-solve hook: inspects prior-stage results and mutates the
/// current stage's request. Target-tracking override builders
/// (`DcReducedCostTargetTracking`, `LmpMarginalCostTargetTracking`)
/// are the primary use case.
pub trait FeedbackProvider: Send + Sync {
    fn name(&self) -> &str;
    fn augment(
        &self,
        ctx: &FeedbackCtx,
        request: &mut DispatchRequest,
    ) -> Result<(), DispatchError>;
}

/// Context handed to commitment probes between iterations.
pub struct ProbeCtx<'a> {
    pub stage_id: &'a str,
    pub iteration: usize,
    pub current_request: &'a DispatchRequest,
    pub last_solution: &'a DispatchSolution,
}

/// Outcome of a commitment probe.
pub enum ProbeOutcome {
    /// Probe found nothing actionable — iteration loop terminates.
    NoChange,
    /// Probe returns a mutated request for the next iteration.
    /// Typically the caller will re-solve the SCUC source stage
    /// before the current stage re-runs against this request.
    MutatedRequest(Box<DispatchRequest>),
}

/// Between-iteration mutator: may rewrite the stage's request (and
/// implicitly trigger an SCUC re-solve on the source stage when the
/// caller wires that in). The pmin=0 decommit probe is the canonical
/// use case.
pub trait CommitmentProbe: Send + Sync {
    fn name(&self) -> &str;
    fn probe(&self, ctx: &ProbeCtx) -> Result<ProbeOutcome, DispatchError>;
}

// ─── Runtime ────────────────────────────────────────────────────────

/// Per-attempt report entry — one line per retry-grid cell the
/// runtime tried.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinementAttemptReport {
    pub iteration: usize,
    pub opf_attempt: String,
    pub nlp_solver: String,
    pub relax_pmin: bool,
    pub band_mode: String,
    pub max_additional_bandable_producers: usize,
    #[serde(default = "default_hvdc_attempt_name")]
    pub hvdc_attempt: String,
    pub dispatch_total_penalty_cost: f64,
    pub error: Option<String>,
}

fn default_hvdc_attempt_name() -> String {
    "default".to_string()
}

/// Summary of which attempt the runtime returned and why.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RefinementReport {
    pub attempts: Vec<RefinementAttemptReport>,
    pub selected_attempt: Option<String>,
    pub selected_iteration: usize,
    pub selected_after_failed_or_worse_wide_band_retry: bool,
    pub feedback_providers_applied: Vec<String>,
    pub commitment_probes_applied: Vec<String>,
}

/// Inputs the runtime needs beyond the stage's own `(model, request,
/// options)` triple. The source-stage solution seeds
/// [`DispatchPinningConfig`] + feedback providers.
pub struct RefinementInputs<'a> {
    pub model: &'a DispatchModel,
    pub base_request: &'a DispatchRequest,
    pub base_options: &'a DispatchSolveOptions,
    pub policy: &'a RetryPolicy,
    pub pinning: Option<&'a DispatchPinningConfig>,
    /// Optional format-aware producer-pinning config. When present,
    /// the retry runtime re-applies this pinning per attempt with the
    /// attempt's band parameters, overriding the canonical
    /// [`DispatchPinningConfig`] pin. Used to preserve
    /// `producer_static` zero-pins and reserve-headroom shrink across
    /// retry-grid attempts.
    pub producer_pinning: Option<&'a ProducerDispatchPinning>,
    pub prior_stage_solution: Option<&'a DispatchSolution>,
    pub stage_id: &'a str,
}

/// Driver for the retry grid. The runtime is stateless — every call
/// builds its own report and returns the selected solution.
pub struct RefinementRuntime;

impl RefinementRuntime {
    pub fn solve(
        inputs: RefinementInputs<'_>,
    ) -> Result<(DispatchSolution, RefinementReport), DispatchError> {
        let RefinementInputs {
            model,
            base_request,
            base_options,
            policy,
            pinning,
            producer_pinning,
            prior_stage_solution,
            stage_id,
        } = inputs;

        let mut report = RefinementReport::default();
        let max_iter = policy.max_iterations;
        // Clone once; probe-driven iterations overwrite this with a
        // fresh mutated request each pass.
        let mut working_request = base_request.clone();

        // Iteration 0 uses the base request as-is; probe iterations
        // 1..=max_iter may rewrite the request via CommitmentProbe.
        for iteration in 0..=max_iter {
            // Apply per-iteration feedback providers to a fresh clone
            // so the working_request is preserved for the next iter.
            let mut iter_request = working_request.clone();
            for provider in &policy.feedback_providers {
                let ctx = FeedbackCtx {
                    stage_id,
                    iteration,
                    prior_stage_solution,
                };
                provider.augment(&ctx, &mut iter_request)?;
                if !report
                    .feedback_providers_applied
                    .iter()
                    .any(|n| n == provider.name())
                {
                    report
                        .feedback_providers_applied
                        .push(provider.name().to_string());
                }
            }

            let grid_result = run_retry_grid(
                model,
                &iter_request,
                base_options,
                policy,
                pinning,
                producer_pinning,
                prior_stage_solution,
                iteration,
                &mut report,
            )?;

            let Some(grid_outcome) = grid_result else {
                // Retry grid exhausted without success. Surface the
                // per-attempt error trail so the caller can see *why*
                // each NLP attempt failed instead of staring at a bare
                // "exhausted retry grid" message.
                let trail: Vec<String> = report
                    .attempts
                    .iter()
                    .filter_map(|a| {
                        a.error.as_ref().map(|e| {
                            format!(
                                "[{}/{}/relax_pmin={}/band={}] {}",
                                a.opf_attempt,
                                a.nlp_solver.as_str(),
                                a.relax_pmin,
                                a.band_mode,
                                e
                            )
                        })
                    })
                    .collect();
                let trail_str = if trail.is_empty() {
                    "no per-attempt errors recorded".to_string()
                } else {
                    format!("attempts:\n  {}", trail.join("\n  "))
                };
                return Err(DispatchError::InvalidInput(format!(
                    "refinement runtime: stage '{stage_id}' exhausted retry grid without success — {trail_str}"
                )));
            };

            // Commitment probes run after a successful grid solve,
            // and may rewrite the working request for the next iter.
            if iteration >= max_iter || policy.commitment_probes.is_empty() {
                report.selected_attempt = Some(grid_outcome.attempt_name.clone());
                report.selected_iteration = iteration;
                report.selected_after_failed_or_worse_wide_band_retry =
                    grid_outcome.selected_after_failed_or_worse_wide_band_retry;
                return Ok((grid_outcome.solution, report));
            }

            let mut probe_fired = false;
            let mut probe_next_request: Option<DispatchRequest> = None;
            for probe in &policy.commitment_probes {
                let ctx = ProbeCtx {
                    stage_id,
                    iteration,
                    current_request: &working_request,
                    last_solution: &grid_outcome.solution,
                };
                match probe.probe(&ctx)? {
                    ProbeOutcome::NoChange => {}
                    ProbeOutcome::MutatedRequest(new_req) => {
                        probe_fired = true;
                        probe_next_request = Some(*new_req);
                        if !report
                            .commitment_probes_applied
                            .iter()
                            .any(|n| n == probe.name())
                        {
                            report
                                .commitment_probes_applied
                                .push(probe.name().to_string());
                        }
                        break;
                    }
                }
            }

            if !probe_fired {
                report.selected_attempt = Some(grid_outcome.attempt_name.clone());
                report.selected_iteration = iteration;
                report.selected_after_failed_or_worse_wide_band_retry =
                    grid_outcome.selected_after_failed_or_worse_wide_band_retry;
                return Ok((grid_outcome.solution, report));
            }
            working_request = probe_next_request.expect("probe_fired implies some request");
        }

        Err(DispatchError::InvalidInput(format!(
            "refinement runtime: stage '{stage_id}' exited loop without returning"
        )))
    }
}

struct GridOutcome {
    solution: DispatchSolution,
    attempt_name: String,
    selected_after_failed_or_worse_wide_band_retry: bool,
}

#[allow(clippy::too_many_arguments)]
fn run_retry_grid(
    model: &DispatchModel,
    iter_request: &DispatchRequest,
    base_options: &DispatchSolveOptions,
    policy: &RetryPolicy,
    pinning: Option<&DispatchPinningConfig>,
    producer_pinning: Option<&ProducerDispatchPinning>,
    prior_stage_solution: Option<&DispatchSolution>,
    iteration: usize,
    report: &mut RefinementReport,
) -> Result<Option<GridOutcome>, DispatchError> {
    let relax_sweep = if policy.relax_pmin_sweep.is_empty() {
        &[false][..]
    } else {
        policy.relax_pmin_sweep.as_slice()
    };
    let opf_attempts = if policy.opf_attempts.is_empty() {
        &[OpfAttempt {
            name: "default".into(),
            overrides: None,
        }][..]
    } else {
        policy.opf_attempts.as_slice()
    };
    let nlp_candidates = if policy.nlp_solver_candidates.is_empty() {
        &[None][..]
    } else {
        policy.nlp_solver_candidates.as_slice()
    };
    let band_attempts = if policy.band_attempts.is_empty() {
        &[BandAttempt::default_band()][..]
    } else {
        policy.band_attempts.as_slice()
    };
    let hvdc_attempts = if policy.hvdc_attempts.is_empty() {
        &[HvdcAttempt::default_attempt()][..]
    } else {
        policy.hvdc_attempts.as_slice()
    };

    for &relax_pmin in relax_sweep {
        for opf in opf_attempts {
            for nlp_candidate in nlp_candidates {
                // Per (relax, opf, nlp) cell, walk the band attempts
                // and apply the "default-else-wide-band" selection.
                let mut fallback: Option<(DispatchSolution, String, f64)> = None;

                let mut first_attempt_error: Option<DispatchError> = None;

                for (band_idx, band) in band_attempts.iter().enumerate() {
                    // HVDC fallback loop: tried within the (band) cell.
                    // The first HVDC attempt is the baseline (typically
                    // `default` / no override); if bus P or Q slack
                    // exceeds `hvdc_retry_bus_slack_threshold_mw` and
                    // more HVDC attempts remain, the runtime retries.
                    //
                    // Per-period independence: each period's AC SCED is
                    // independent. On retry, we only target periods
                    // still bad; for periods already good in an earlier
                    // attempt, we lock them to that attempt's HVDC
                    // value so the next retry's strategy change doesn't
                    // disrupt them. This carries forward per-period
                    // wins across strategies (e.g., period 10 might
                    // land good with flipped while period 30 needs
                    // neutral — both choices survive to the final
                    // attempt's solve).
                    //
                    // `committed_hvdc_by_link[link_id][t] = Some(mw)`
                    // means lock period t to mw; `None` means
                    // unpinned / use the current attempt's strategy.
                    let mut best_hvdc: Option<(DispatchSolution, String, f64, f64)> = None;
                    let hvdc_threshold = policy.hvdc_retry_bus_slack_threshold_mw;
                    let mut targeted_periods: Vec<usize> = Vec::new();
                    let mut committed_hvdc_by_link: std::collections::HashMap<
                        String,
                        Vec<Option<f64>>,
                    > = std::collections::HashMap::new();
                    // Per-period "what the LAST HVDC attempt produced
                    // for AC" — flipping is relative to THIS, not the
                    // DC stage's choice. When the AC NLP has already
                    // drifted away from the DC stage's direction into
                    // a basin trap, flipping relative to AC's own
                    // output is what pulls it back; flipping relative
                    // to the DC stage would just pin AC to the DC
                    // direction it has already rejected.
                    let mut last_attempt_ac_hvdc: std::collections::HashMap<String, Vec<f64>> =
                        std::collections::HashMap::new();

                    for hvdc in hvdc_attempts {
                        let attempt_name = if relax_pmin {
                            format!("{}_relax_pmin", opf.name)
                        } else {
                            opf.name.clone()
                        };
                        let full_attempt_name = if hvdc.strategy == HvdcStrategy::Default {
                            attempt_name.clone()
                        } else {
                            format!("{}:{}", attempt_name, hvdc.name)
                        };
                        let hvdc_debug =
                            std::env::var("SURGE_REFINEMENT_DEBUG").ok().as_deref() == Some("1");
                        let nlp_name = nlp_candidate
                            .clone()
                            .unwrap_or_else(|| "default".to_string());

                        // Build this attempt's request (fresh clone so
                        // per-attempt mutations don't bleed across cells).
                        let mut attempt_request = iter_request.clone();
                        apply_attempt_overrides(
                            &mut attempt_request,
                            pinning,
                            producer_pinning,
                            prior_stage_solution,
                            opf,
                            band,
                            relax_pmin,
                            hvdc,
                            &targeted_periods,
                            &committed_hvdc_by_link,
                            &last_attempt_ac_hvdc,
                        );

                        let mut attempt_options = base_options.clone();
                        if let Some(name) = nlp_candidate {
                            let solver = ac_opf_nlp_solver_from_str(name).map_err(|err| {
                                DispatchError::InvalidInput(format!(
                                    "refinement runtime: unknown NLP solver '{name}': {err}"
                                ))
                            })?;
                            attempt_options.nlp_solver = Some(solver as Arc<dyn NlpSolver>);
                        }

                        if hvdc_debug {
                            let pinned_detail = attempt_request
                                .runtime()
                                .fixed_hvdc_dispatch
                                .iter()
                                .map(|e| {
                                    let sampled: Vec<String> = [0usize, 29, 30, 31, 32]
                                        .iter()
                                        .filter_map(|&t| {
                                            e.p_mw.get(t).map(|v| format!("t{}={:+.1}", t, *v))
                                        })
                                        .collect();
                                    format!("{}[{}]", e.link_id, sampled.join(","))
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            eprintln!(
                                "[refinement] iter={} attempt='{}' band='{}' hvdc='{}' pinned_hvdc=[{}] targeted_periods={:?}",
                                iteration,
                                full_attempt_name,
                                band.name,
                                hvdc.name,
                                pinned_detail,
                                targeted_periods
                            );
                            // Optional: dump the full request for deep
                            // diff. Controlled by SURGE_DUMP_ATTEMPT_REQUEST.
                            if let Ok(dump_dir) = std::env::var("SURGE_DUMP_ATTEMPT_REQUEST") {
                                let path = std::path::Path::new(&dump_dir)
                                    .join(format!("{}.json", full_attempt_name));
                                if let Ok(json) = serde_json::to_string_pretty(&attempt_request) {
                                    let _ = std::fs::create_dir_all(&dump_dir);
                                    let _ = std::fs::write(&path, json);
                                }
                            }
                        }

                        match model.solve_with_options(&attempt_request, &attempt_options) {
                            Ok(solution) => {
                                let penalty = total_penalty_cost(&solution);
                                let bus_slack = max_bus_balance_slack_mw(&solution);
                                // Compute bad-period mask now (before
                                // `solution` moves into best_hvdc) so
                                // the next HVDC attempt can target only
                                // the periods that exceeded threshold.
                                let bad_periods_if_retry =
                                    bad_periods_by_bus_slack(&solution, hvdc_threshold);

                                // Commit per-period HVDC values ONLY
                                // when a non-default strategy succeeded
                                // for that period. On LP-degenerate
                                // scenarios, AC's "default"-attempt
                                // HVDC per period is unstable — AC
                                // can't always reproduce its own
                                // free-variable equilibrium when other
                                // periods become pinned. Using the
                                // prior-stage (DC) HVDC as the anchor
                                // for non-targeted periods is far more
                                // stable. So we commit only when we
                                // actively picked a strategy for a
                                // period (i.e., the period was in the
                                // prior attempt's targeted list).
                                if hvdc.strategy != HvdcStrategy::Default {
                                    commit_good_targeted_period_hvdc(
                                        &mut committed_hvdc_by_link,
                                        &solution,
                                        hvdc_threshold,
                                        &targeted_periods,
                                    );
                                }
                                // Record this attempt's AC HVDC per
                                // link/period so the NEXT attempt's
                                // flip/neutral reasons off of what AC
                                // actually did, not what DC asked for.
                                last_attempt_ac_hvdc.clear();
                                for (p_idx, period) in solution.periods().iter().enumerate() {
                                    for hr in period.hvdc_results() {
                                        let entry = last_attempt_ac_hvdc
                                            .entry(hr.link_id.clone())
                                            .or_insert_with(|| vec![0.0; solution.periods().len()]);
                                        if entry.len() < solution.periods().len() {
                                            entry.resize(solution.periods().len(), 0.0);
                                        }
                                        entry[p_idx] = hr.mw;
                                    }
                                }
                                if hvdc_debug {
                                    let s = solution.penalty_summary();
                                    eprintln!(
                                        "[refinement]   → ok  P_slack={:.2} MW  Q_slack={:.2} MVAr  max={:.2}  penalty=${:.2}",
                                        s.power_balance_p_total_mw,
                                        s.power_balance_q_total_mvar,
                                        bus_slack,
                                        penalty
                                    );
                                }
                                report.attempts.push(RefinementAttemptReport {
                                    iteration,
                                    opf_attempt: attempt_name.clone(),
                                    nlp_solver: nlp_name.clone(),
                                    relax_pmin,
                                    band_mode: band.name.clone(),
                                    max_additional_bandable_producers: band
                                        .max_additional_bandable_producers,
                                    hvdc_attempt: hvdc.name.clone(),
                                    dispatch_total_penalty_cost: penalty,
                                    error: None,
                                });

                                // Track the best (lowest bus_slack)
                                // successful HVDC attempt. If bus_slack
                                // is already within threshold on the
                                // first HVDC try, short-circuit.
                                let keep = best_hvdc
                                    .as_ref()
                                    .map(|(_, _, _, prev_slack)| bus_slack < *prev_slack - 1e-6)
                                    .unwrap_or(true);
                                if keep {
                                    best_hvdc = Some((
                                        solution,
                                        full_attempt_name.clone(),
                                        penalty,
                                        bus_slack,
                                    ));
                                }
                                if bus_slack <= hvdc_threshold {
                                    if hvdc_debug {
                                        eprintln!(
                                            "[refinement]   ← accept (slack {:.2} ≤ threshold {:.2})",
                                            bus_slack, hvdc_threshold
                                        );
                                    }
                                    // Accept early — no need to try
                                    // more HVDC fallback attempts.
                                    break;
                                }
                                // Per-period bad mask (computed above,
                                // before `solution` moved into best_hvdc).
                                targeted_periods = bad_periods_if_retry;
                                if hvdc_debug {
                                    eprintln!(
                                        "[refinement]   ↻ slack {:.2} > threshold {:.2}, bad periods: {:?}, trying next HVDC attempt",
                                        bus_slack, hvdc_threshold, targeted_periods
                                    );
                                }
                                // Otherwise continue to next HVDC
                                // attempt (unless we've exhausted).
                            }
                            Err(err) => {
                                if hvdc_debug {
                                    eprintln!("[refinement]   ✗ solve error: {}", err);
                                }
                                if policy.hard_fail_first_attempt
                                    && report.attempts.iter().all(|a| a.error.is_some())
                                    && first_attempt_error.is_none()
                                {
                                    return Err(err);
                                }
                                let msg = err.to_string();
                                report.attempts.push(RefinementAttemptReport {
                                    iteration,
                                    opf_attempt: attempt_name,
                                    nlp_solver: nlp_name,
                                    relax_pmin,
                                    band_mode: band.name.clone(),
                                    max_additional_bandable_producers: band
                                        .max_additional_bandable_producers,
                                    hvdc_attempt: hvdc.name.clone(),
                                    dispatch_total_penalty_cost: f64::NAN,
                                    error: Some(msg),
                                });
                                if first_attempt_error.is_none() {
                                    first_attempt_error = Some(err);
                                }
                            }
                        }
                    }

                    let Some((solution, full_attempt_name, penalty, _slack)) = best_hvdc else {
                        // All HVDC attempts failed for this band cell;
                        // continue to next band attempt.
                        continue;
                    };

                    if band_idx == 0
                        && band_attempts.len() > 1
                        && penalty > policy.wide_band_penalty_threshold_dollars
                    {
                        fallback = Some((solution, full_attempt_name, penalty));
                        continue;
                    }

                    if let Some((fb_sol, fb_name, fb_penalty)) = fallback.take() {
                        // We had a default solution under the
                        // threshold miss; the wide-band retry
                        // either matched or did worse. Pick
                        // the lower-penalty one.
                        if penalty + 1e-6 >= fb_penalty {
                            return Ok(Some(GridOutcome {
                                solution: fb_sol,
                                attempt_name: fb_name,
                                selected_after_failed_or_worse_wide_band_retry: true,
                            }));
                        }
                    }

                    return Ok(Some(GridOutcome {
                        solution,
                        attempt_name: full_attempt_name,
                        selected_after_failed_or_worse_wide_band_retry: false,
                    }));
                }

                // All band attempts exhausted for this (relax, opf,
                // nlp) cell. If we parked a fallback earlier, it's
                // still our best shot.
                if let Some((fb_sol, fb_name, _)) = fallback.take() {
                    return Ok(Some(GridOutcome {
                        solution: fb_sol,
                        attempt_name: fb_name,
                        selected_after_failed_or_worse_wide_band_retry: true,
                    }));
                }
            }
        }
    }
    Ok(None)
}

/// Maximum of a solution's total P-balance slack MW and total
/// Q-balance slack MVAr across all periods. Used by the HVDC fallback
/// to decide whether the baseline HVDC attempt cleared bus balance or
/// whether to retry with a different HVDC strategy.
///
/// Prefers summing `constraint_results[*].slack_mw` for the
/// `power_balance:bus:*:ac_pos|ac_neg` constraint family when they
/// are present: the top-level `penalty_summary` roll-up is not always
/// populated for AC SCED stage solutions even though
/// `constraint_results` carries the real per-bus slacks, so relying
/// on it alone hides the exact failures the HVDC fallback is meant
/// to catch. When no such constraints exist, falls back to the
/// `penalty_summary` roll-up.
fn max_bus_balance_slack_mw(solution: &DispatchSolution) -> f64 {
    use surge_dispatch::ConstraintKind;

    // Sum bus P/Q balance slacks across periods from constraint_results.
    let mut total_p_slack = 0.0_f64;
    let mut total_q_slack = 0.0_f64;
    let mut saw_bus_constraint = false;
    for period in solution.periods() {
        for cr in period.constraint_results() {
            // AC-side bus P/Q balance + reactive balance constraints.
            let is_p = matches!(cr.kind, ConstraintKind::PowerBalance);
            let is_q = matches!(cr.kind, ConstraintKind::ReactiveBalance);
            if !is_p && !is_q {
                continue;
            }
            // Canonical IDs: "power_balance:bus:<n>:ac_pos|ac_neg",
            //                "reactive_balance:bus:<n>:q_pos|q_neg".
            // Skip DC-stage-only constraints (they don't carry "ac_"
            // or "q_" and aren't meaningful here).
            let cid = cr.constraint_id.as_str();
            if !cid.contains(":ac_") && !cid.contains(":q_") {
                continue;
            }
            saw_bus_constraint = true;
            let slack = cr.slack_mw.unwrap_or(0.0).abs();
            if slack < 1e-9 {
                continue;
            }
            if is_p {
                total_p_slack += slack;
            } else {
                total_q_slack += slack;
            }
        }
    }
    if saw_bus_constraint {
        return total_p_slack.max(total_q_slack);
    }
    // Fallback: use the top-level penalty_summary roll-up when no
    // constraint-level bus balance rows are available.
    let s = solution.penalty_summary();
    s.power_balance_p_total_mw
        .max(s.power_balance_q_total_mvar.abs())
}

#[allow(clippy::too_many_arguments)]
fn apply_attempt_overrides(
    request: &mut DispatchRequest,
    pinning: Option<&DispatchPinningConfig>,
    producer_pinning: Option<&ProducerDispatchPinning>,
    prior_stage_solution: Option<&DispatchSolution>,
    opf: &OpfAttempt,
    band: &BandAttempt,
    relax_pmin: bool,
    hvdc: &HvdcAttempt,
    targeted_periods: &[usize],
    committed_hvdc_by_link: &std::collections::HashMap<String, Vec<Option<f64>>>,
    last_attempt_ac_hvdc: &std::collections::HashMap<String, Vec<f64>>,
) {
    let runtime = request.runtime_mut();
    runtime.ac_relax_committed_pmin_to_zero = relax_pmin;
    if let Some(overrides) = &opf.overrides {
        // Preserve user-set NLP knobs from the request so callers can
        // tune them via `request.runtime.ac_opf` and have the override
        // survive the retry loop's per-attempt resets. The preserved
        // fields are:
        //   * tolerance / max_iterations / print_level — NLP driver
        //     controls that shouldn't vary by retry intent;
        //   * bus_{active,reactive}_power_balance_slack_penalty — bus
        //     P/Q balance slack cost. Note this defeats the
        //     `strict_bus_balance` retry attempt's intent to zero
        //     these penalties; if the user set custom values the
        //     strict retry will use the user's values instead. For
        //     the first attempt (which is what matters when the
        //     problem converges), this is the expected behavior.
        let mut merged = overrides.clone();
        if let Some(existing) = &runtime.ac_opf {
            merged.tolerance = existing.tolerance;
            merged.max_iterations = existing.max_iterations;
            merged.print_level = existing.print_level;
            merged.bus_active_power_balance_slack_penalty_per_mw =
                existing.bus_active_power_balance_slack_penalty_per_mw;
            merged.bus_reactive_power_balance_slack_penalty_per_mvar =
                existing.bus_reactive_power_balance_slack_penalty_per_mvar;
        }
        runtime.ac_opf = Some(merged);
    }
    // Per-attempt re-pinning: when a producer pinning is supplied,
    // clone it with the attempt's band parameters + relax_pmin and
    // re-apply. Otherwise fall back to the canonical pin. The
    // producer pinning knows how to preserve producer_static zero-
    // pins and reserve shrink, which the canonical pin would destroy.
    //
    // HVDC fallback pairing: when the HVDC strategy is non-default,
    // the HVDC direction changes substantially (flip or zero).
    // Generators need to rebalance MW to match the new flow pattern
    // — flipping HVDC from +100 to -100, for example, requires a net
    // 200 MW shift somewhere. The default 5% producer band around
    // the prior DC dispatch is far too tight to allow that rebalance
    // and leaves AC stuck on the old flow. For non-default HVDC
    // strategies we open the band wide (full physical range) so the
    // AC NLP has the MW freedom to match the new HVDC direction.
    let effective_band_fraction = if hvdc.strategy != HvdcStrategy::Default {
        1.0e6
    } else {
        band.band_fraction
    };
    let effective_band_cap_mw = if hvdc.strategy != HvdcStrategy::Default {
        1.0e9
    } else {
        band.band_cap_mw
    };
    if let (Some(pp), Some(source)) = (producer_pinning, prior_stage_solution) {
        let mut pp = pp.clone();
        pp.band_fraction = effective_band_fraction;
        pp.band_floor_mw = band.band_floor_mw;
        pp.band_cap_mw = effective_band_cap_mw;
        pp.relax_pmin = relax_pmin;
        pp.anchor_resource_ids = band.anchor_resource_ids.iter().cloned().collect();
        apply_producer_dispatch_pinning(request, source, &pp);
    } else if let (Some(cfg), Some(source)) = (pinning, prior_stage_solution) {
        let band_cfg = DispatchPinningConfig {
            source_stage_id: cfg.source_stage_id.clone(),
            band_fraction: effective_band_fraction,
            band_floor_mw: band.band_floor_mw,
            band_cap_mw: effective_band_cap_mw,
            anchor_resource_ids: band.anchor_resource_ids.iter().cloned().collect(),
        };
        pin_generator_dispatch_bounds(request, source, &band_cfg);
    }

    // HVDC strategy override: set runtime.fixed_hvdc_dispatch based on
    // the attempt's strategy. Default = no change (any existing pin
    // left alone). Flipped/Neutral build a per-link per-period pin
    // from the prior stage's HVDC result, but only modify the
    // targeted_periods (bad periods from the prior HVDC attempt); other
    // periods keep the prior stage's HVDC value so good periods aren't
    // disrupted. Empty targeted_periods means all periods are flipped.
    match hvdc.strategy {
        HvdcStrategy::Default => {
            // Default strategy still applies committed per-period pins
            // from earlier attempts (e.g., period 10 locked to the
            // flipped value that cleared it), so a subsequent retry on
            // a different cell starts from the same committed state.
            if let Some(source) = prior_stage_solution {
                if !committed_hvdc_by_link.is_empty() {
                    request.runtime_mut().fixed_hvdc_dispatch = build_hvdc_override(
                        source,
                        HvdcStrategy::Default,
                        &[],
                        committed_hvdc_by_link,
                        last_attempt_ac_hvdc,
                    );
                }
            }
        }
        HvdcStrategy::Flipped | HvdcStrategy::Neutral => {
            if let Some(source) = prior_stage_solution {
                request.runtime_mut().fixed_hvdc_dispatch = build_hvdc_override(
                    source,
                    hvdc.strategy.clone(),
                    targeted_periods,
                    committed_hvdc_by_link,
                    last_attempt_ac_hvdc,
                );
            }
        }
    }
}

/// Build a per-link, per-period HVDC pin from the prior stage's solved
/// dispatch. The pin's shape matches the source's `hvdc_results`, one
/// entry per (link, period).
///
/// Only the periods listed in `targeted_periods` have the strategy
/// applied (flip or zero); all other periods inherit the prior stage's
/// HVDC value unchanged. An empty `targeted_periods` is treated as
/// "apply to all periods" — matches the caller's intent when no
/// per-period failure pattern is known yet.
///
/// Why per-period targeting: on LP-degenerate scenarios with tons
/// of zero-cost renewables, only a few periods end up in the AC
/// infeasibility basin. Flipping HVDC on the periods that were fine
/// creates NEW imbalances there, so flipping only the targeted
/// periods is what actually reduces total bus slack.
fn build_hvdc_override(
    source: &DispatchSolution,
    strategy: HvdcStrategy,
    targeted_periods: &[usize],
    committed_hvdc_by_link: &std::collections::HashMap<String, Vec<Option<f64>>>,
    last_attempt_ac_hvdc: &std::collections::HashMap<String, Vec<f64>>,
) -> Vec<surge_dispatch::request::HvdcPeriodPowerSeries> {
    use surge_dispatch::request::HvdcPeriodPowerSeries;

    let periods = source.periods();
    if periods.is_empty() {
        return Vec::new();
    }
    // Collect link IDs in stable order from the first period (every
    // period should have the same links; use period 0 as canonical).
    let link_ids: Vec<String> = periods[0]
        .hvdc_results()
        .iter()
        .map(|r| r.link_id.clone())
        .collect();

    let num_periods = periods.len();
    let targeted: std::collections::HashSet<usize> = if targeted_periods.is_empty() {
        (0..num_periods).collect()
    } else {
        targeted_periods.iter().copied().collect()
    };

    let mut per_link_p: std::collections::HashMap<String, Vec<f64>> = link_ids
        .iter()
        .map(|id| (id.clone(), vec![0.0_f64; num_periods]))
        .collect();
    // Q_fr / Q_to live on bus_results as net_reactive_injection at
    // the HVDC terminals, not on hvdc_results. For Flipped/Neutral we
    // only pin P (q_fr / q_to default to empty which preserves AC's
    // freedom on the reactive side — P direction is the source of the
    // basin jump anyway). Extending to Q would require reading the
    // HVDC's terminal Q from synthetic support generators.
    //
    // Resolution order per (link, period):
    //   1. committed_hvdc_by_link (locked value from an earlier
    //      successful attempt) — highest priority.
    //   2. If period is in `targeted`: apply strategy (flip source or
    //      zero) — used to RETRY a period that's still bad.
    //   3. Else: inherit the prior stage (source) value — noop-ish,
    //      preserves the flow pattern the AC NLP found naturally.
    for (p_idx, period) in periods.iter().enumerate() {
        for hr in period.hvdc_results() {
            let Some(slot) = per_link_p.get_mut(&hr.link_id) else {
                continue;
            };
            // 1. Committed from earlier attempt (e.g., period 10
            //    locked to flipped value that cleared it).
            if let Some(committed_series) = committed_hvdc_by_link.get(&hr.link_id) {
                if let Some(Some(v)) = committed_series.get(p_idx) {
                    slot[p_idx] = *v;
                    continue;
                }
            }
            slot[p_idx] = if !targeted.contains(&p_idx) {
                // 2. Period not targeted — use prior-stage (DC) value.
                // DC's HVDC is stable (DC LP doesn't have NLP basin
                // issues), so pinning non-targeted periods to DC's
                // value keeps them in the same operating point the AC
                // NLP would have found in attempt 1. Do NOT use AC's
                // attempt-1 value here: on degenerate problems the
                // AC NLP's free-variable HVDC is not stable when the
                // request later adds pins on other periods, and
                // locking it to AC's attempt-1 value can blow up the
                // slack elsewhere.
                hr.mw
            } else {
                // 3. Period still bad — apply the current strategy.
                // Flip is relative to the LAST HVDC attempt's AC
                // result (if any), so we're flipping the actual bad
                // direction AC picked, not DC's (possibly correct)
                // choice. Falls back to DC's value when no prior AC
                // attempt is recorded.
                let ac_reference = last_attempt_ac_hvdc
                    .get(&hr.link_id)
                    .and_then(|s| s.get(p_idx).copied())
                    .unwrap_or(hr.mw);
                match strategy {
                    HvdcStrategy::Default => hr.mw, // unused here
                    HvdcStrategy::Flipped => -ac_reference,
                    HvdcStrategy::Neutral => 0.0,
                }
            };
        }
    }

    let mut out = Vec::with_capacity(link_ids.len());
    for link_id in link_ids {
        let p_mw = per_link_p.remove(&link_id).unwrap_or_default();
        out.push(HvdcPeriodPowerSeries {
            link_id,
            p_mw,
            q_fr_mvar: Vec::new(),
            q_to_mvar: Vec::new(),
        });
    }
    out
}

/// Commit this attempt's HVDC per-period values — but ONLY for
/// periods that were targeted by this attempt's strategy AND are now
/// good (bus slack ≤ threshold). This is how "period 10 landed good
/// with flipped, period 30 needs neutral" gets encoded: each strategy
/// commits only its own wins. The default attempt does NOT commit
/// (its free-variable HVDC values are unstable under subsequent pins).
fn commit_good_targeted_period_hvdc(
    committed: &mut std::collections::HashMap<String, Vec<Option<f64>>>,
    solution: &DispatchSolution,
    threshold_mw: f64,
    targeted_periods: &[usize],
) {
    use surge_dispatch::ConstraintKind;
    let periods = solution.periods();
    let num_periods = periods.len();
    let targeted: std::collections::HashSet<usize> = targeted_periods.iter().copied().collect();
    if targeted.is_empty() {
        return;
    }
    // Per-period aggregate bus P/Q slack.
    let mut per_period_slack = vec![0.0_f64; num_periods];
    for (p_idx, period) in periods.iter().enumerate() {
        for cr in period.constraint_results() {
            let is_p = matches!(cr.kind, ConstraintKind::PowerBalance);
            let is_q = matches!(cr.kind, ConstraintKind::ReactiveBalance);
            if !is_p && !is_q {
                continue;
            }
            let cid = cr.constraint_id.as_str();
            if !cid.contains(":ac_") && !cid.contains(":q_") {
                continue;
            }
            per_period_slack[p_idx] += cr.slack_mw.unwrap_or(0.0).abs();
        }
    }
    for (p_idx, period) in periods.iter().enumerate() {
        if !targeted.contains(&p_idx) {
            continue;
        }
        if per_period_slack[p_idx] > threshold_mw {
            continue;
        }
        for hr in period.hvdc_results() {
            let series = committed
                .entry(hr.link_id.clone())
                .or_insert_with(|| vec![None; num_periods]);
            if series.len() < num_periods {
                series.resize(num_periods, None);
            }
            if series[p_idx].is_none() {
                series[p_idx] = Some(hr.mw);
            }
        }
    }
}

/// Identify periods whose bus P/Q balance slack exceeds the per-period
/// threshold. The threshold is the aggregate-over-periods threshold
/// from the retry policy; for per-period triggering we use the same
/// value (interpreted as "any period with slack > threshold is bad").
/// Scans `constraint_results` for the AC-side bus balance family;
/// returns a de-duplicated sorted vector of period indices.
fn bad_periods_by_bus_slack(solution: &DispatchSolution, threshold_mw: f64) -> Vec<usize> {
    use surge_dispatch::ConstraintKind;
    let mut per_period_slack: std::collections::HashMap<usize, f64> =
        std::collections::HashMap::new();
    for (p_idx, period) in solution.periods().iter().enumerate() {
        for cr in period.constraint_results() {
            let is_p = matches!(cr.kind, ConstraintKind::PowerBalance);
            let is_q = matches!(cr.kind, ConstraintKind::ReactiveBalance);
            if !is_p && !is_q {
                continue;
            }
            let cid = cr.constraint_id.as_str();
            if !cid.contains(":ac_") && !cid.contains(":q_") {
                continue;
            }
            let slack = cr.slack_mw.unwrap_or(0.0).abs();
            let entry = per_period_slack.entry(p_idx).or_insert(0.0);
            *entry += slack;
        }
    }
    let mut bad: Vec<usize> = per_period_slack
        .into_iter()
        .filter(|(_, slack)| *slack > threshold_mw)
        .map(|(p, _)| p)
        .collect();
    bad.sort_unstable();
    bad
}

fn total_penalty_cost(solution: &DispatchSolution) -> f64 {
    solution.summary().total_penalty_cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_noop_equivalent() {
        let noop = RetryPolicy::noop();
        assert_eq!(noop.relax_pmin_sweep, vec![false]);
        assert_eq!(noop.opf_attempts.len(), 1);
        assert_eq!(noop.band_attempts.len(), 1);
        assert_eq!(noop.max_iterations, 0);
    }

    #[test]
    fn goc3_default_policy_has_three_opf_attempts() {
        let p = RetryPolicy::goc3_default();
        assert_eq!(p.relax_pmin_sweep, vec![false, true]);
        assert_eq!(p.opf_attempts.len(), 3);
        assert_eq!(p.opf_attempts[0].name, "go_validator_costs");
        assert_eq!(p.opf_attempts[1].name, "strict_bus_balance");
        assert_eq!(p.opf_attempts[2].name, "no_thermal_limits");
    }

    #[test]
    fn goc3_default_policy_has_hvdc_fallback_flipped_before_neutral() {
        let p = RetryPolicy::goc3_default();
        assert_eq!(p.hvdc_attempts.len(), 3);
        assert_eq!(p.hvdc_attempts[0].strategy, HvdcStrategy::Default);
        // Order matters: flipped first (more aggressive discrete jump),
        // neutral second (last-ditch strip of DC anchor).
        assert_eq!(p.hvdc_attempts[1].strategy, HvdcStrategy::Flipped);
        assert_eq!(p.hvdc_attempts[2].strategy, HvdcStrategy::Neutral);
        assert!(p.hvdc_retry_bus_slack_threshold_mw.is_finite());
        assert!(p.hvdc_retry_bus_slack_threshold_mw > 0.0);
    }

    #[test]
    fn noop_policy_has_default_only_hvdc_fallback() {
        let p = RetryPolicy::noop();
        assert_eq!(p.hvdc_attempts.len(), 1);
        assert_eq!(p.hvdc_attempts[0].strategy, HvdcStrategy::Default);
        // Threshold = infinity means fallback never triggers even with
        // more attempts added later.
        assert!(p.hvdc_retry_bus_slack_threshold_mw.is_infinite());
    }
}
