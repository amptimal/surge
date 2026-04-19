// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Typed multi-stage market workflow helpers.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use surge_dispatch::{
    BranchDerateProfile, BranchRef, CommitmentPolicy, CommitmentSchedule, ConstraintKind,
    DispatchError, DispatchModel, DispatchRequest, DispatchSolution, DispatchSolveOptions,
    DispatchStageMetadata, ResourceCommitmentSchedule, ResourcePeriodDetail,
};
use tracing::info;

pub use surge_dispatch::DispatchStageRole as MarketStageRole;

/// Configuration for pinning generator dispatch bounds to a prior
/// stage's solved dispatch. Activated per stage via
/// [`MarketStage::pin_dispatch_from`].
#[derive(Clone, Debug)]
pub struct DispatchPinningConfig {
    /// Stage ID whose solved dispatch seeds the bounds.
    pub source_stage_id: String,
    /// Fractional band around the target dispatch, e.g. `0.05` = ±5%.
    pub band_fraction: f64,
    /// Minimum absolute band width in MW.
    pub band_floor_mw: f64,
    /// Maximum absolute band width in MW.
    pub band_cap_mw: f64,
    /// Anchor set — resources to skip during the canonical pre-pin so
    /// their profile bounds stay at the physical `[P_min, P_max]` range
    /// for the downstream retry runtime. Mirrors the same concept on
    /// `ProducerDispatchPinning`; populated by workflows that attach a
    /// last-ditch anchor retry rung and need the anchors to retain full
    /// flexibility.
    pub anchor_resource_ids: std::collections::HashSet<String>,
}

impl DispatchPinningConfig {
    pub fn new(source_stage_id: impl Into<String>) -> Self {
        Self {
            source_stage_id: source_stage_id.into(),
            band_fraction: 0.05,
            band_floor_mw: 1.0,
            band_cap_mw: 1.0e9,
            anchor_resource_ids: std::collections::HashSet::new(),
        }
    }
}

/// One executable stage in a market workflow.
#[derive(Clone)]
pub struct MarketStage {
    pub stage_id: String,
    pub role: MarketStageRole,
    pub model: DispatchModel,
    pub request: DispatchRequest,
    pub solve_options: DispatchSolveOptions,
    pub derived_from_stage_id: Option<String>,
    pub commitment_source_stage_id: Option<String>,
    pub dispatch_pinning: Option<DispatchPinningConfig>,
    /// Optional AC-SCED setup — applied after commitment handoff and
    /// canonical dispatch pinning, before the stage solves. Carries
    /// reactive-reserve filtering, commitment augmentation, bandable-
    /// subset producer pinning, AC warm start, and Q-bound overrides.
    /// See [`crate::AcScedSetup`].
    pub ac_sced_setup: Option<crate::AcScedSetup>,
    /// Optional retry / feedback / commitment-probe policy. When
    /// `Some`, [`solve_market_workflow`] dispatches this stage through
    /// [`crate::RefinementRuntime`] instead of calling the dispatch
    /// model's single-shot solve. `None` preserves the classical
    /// single-solve path.
    pub retry_policy: Option<crate::RetryPolicy>,
    /// When `Some`, before this stage solves, build per-period branch
    /// thermal derate factors > 1.0 (uprates) from the named source
    /// stage's `branch_thermal` constraint slacks. This relaxes the
    /// AC-SCED thermal limits to absorb the DC-SCUC's leftover overload,
    /// preventing the AC NLP from being asked to satisfy infeasible
    /// flows and cleaning up the SCED retry-grid failure mode on hard
    /// scenarios. The relaxation injects entries into
    /// `request.profiles.branch_derates` so it composes with any
    /// caller-supplied derates.
    pub branch_relax_from_dc_slack: Option<BranchRelaxFromDcSlack>,
}

/// Configuration for the per-period branch thermal relaxation hook on
/// [`MarketStage::branch_relax_from_dc_slack`].
#[derive(Debug, Clone)]
pub struct BranchRelaxFromDcSlack {
    /// `stage_id` of the source stage whose `branch_thermal` slacks drive
    /// the relaxation (typically the DC SCUC stage).
    pub source_stage_id: String,
    /// Extra MVA headroom added to the per-period relaxed limit so the
    /// AC NLP isn't right at the new edge. 0.0 is allowed.
    pub margin_mva: f64,
}

impl MarketStage {
    pub fn new(
        stage_id: impl Into<String>,
        role: MarketStageRole,
        model: DispatchModel,
        request: DispatchRequest,
    ) -> Self {
        Self {
            stage_id: stage_id.into(),
            role,
            model,
            request,
            solve_options: DispatchSolveOptions::default(),
            derived_from_stage_id: None,
            commitment_source_stage_id: None,
            dispatch_pinning: None,
            ac_sced_setup: None,
            retry_policy: None,
            branch_relax_from_dc_slack: None,
        }
    }

    pub fn with_options(mut self, solve_options: DispatchSolveOptions) -> Self {
        self.solve_options = solve_options;
        self
    }

    pub fn derived_from(mut self, stage_id: impl Into<String>) -> Self {
        self.derived_from_stage_id = Some(stage_id.into());
        self
    }

    pub fn commitment_from(mut self, stage_id: impl Into<String>) -> Self {
        self.commitment_source_stage_id = Some(stage_id.into());
        self
    }

    pub fn pin_dispatch_from(mut self, pinning: DispatchPinningConfig) -> Self {
        self.dispatch_pinning = Some(pinning);
        self
    }

    /// Attach an AC SCED setup config — applied after commitment
    /// handoff and canonical dispatch pinning, before the stage
    /// solves.
    pub fn with_ac_sced_setup(mut self, setup: crate::AcScedSetup) -> Self {
        self.ac_sced_setup = Some(setup);
        self
    }

    /// Attach a retry / feedback / commitment-probe policy. When set,
    /// the workflow executor dispatches this stage through
    /// [`crate::RefinementRuntime`] instead of the single-shot
    /// dispatch-model solve.
    pub fn with_retry_policy(mut self, policy: crate::RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    /// Attach a per-period branch thermal relaxation hook driven by an
    /// upstream stage's overload slacks. See
    /// [`MarketStage::branch_relax_from_dc_slack`].
    pub fn with_branch_relax_from_dc_slack(mut self, config: BranchRelaxFromDcSlack) -> Self {
        self.branch_relax_from_dc_slack = Some(config);
        self
    }
}

/// Ordered stage list for a higher-level market workflow.
#[derive(Clone, Default)]
pub struct MarketWorkflow {
    pub stages: Vec<MarketStage>,
}

impl MarketWorkflow {
    pub fn new(stages: Vec<MarketStage>) -> Self {
        Self { stages }
    }

    pub fn validate(&self) -> Result<(), DispatchError> {
        if self.stages.is_empty() {
            return Err(DispatchError::InvalidInput(
                "market workflow requires at least one stage".to_string(),
            ));
        }
        let mut seen = HashSet::new();
        for stage in &self.stages {
            if stage.stage_id.trim().is_empty() {
                return Err(DispatchError::InvalidInput(
                    "market workflow stage_id must be non-empty".to_string(),
                ));
            }
            if !seen.insert(stage.stage_id.clone()) {
                return Err(DispatchError::InvalidInput(format!(
                    "duplicate market workflow stage_id {}",
                    stage.stage_id
                )));
            }
            stage.model.validate_request(&stage.request)?;
        }
        Ok(())
    }
}

/// Per-phase wall-clock timings for one workflow stage (seconds).
///
/// ``total`` is the wall time for the stage (clone request → push
/// result). ``solve`` is the time spent in `stage.model.solve_with_options`
/// or `RefinementRuntime::solve`. The remaining phases detail the
/// pre-solve handoffs that mutate the request. ``solve`` minus the pure
/// optimizer time (`diagnostics.solve_time_secs`) gives the Rust-side
/// model-build + extract overhead.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageTimings {
    pub total_secs: f64,
    pub clone_request_secs: f64,
    pub commitment_handoff_secs: f64,
    pub branch_relax_secs: f64,
    pub dispatch_pinning_secs: f64,
    pub ac_sced_setup_secs: f64,
    pub solve_secs: f64,
    pub accumulate_secs: f64,
}

/// Result of a single workflow stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketStageResult {
    pub stage_id: String,
    pub role: MarketStageRole,
    pub solution: DispatchSolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<StageTimings>,
}

/// Information about a workflow stage that failed to solve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketStageError {
    pub stage_id: String,
    pub role: MarketStageRole,
    pub error: String,
}

/// Result of a full ordered market workflow.
///
/// When a stage fails, `stages` contains all *prior* successfully solved
/// stages and `error` describes the failed stage. Downstream stages that
/// depend on the failed stage are not attempted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketWorkflowResult {
    pub stages: Vec<MarketStageResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<MarketStageError>,
}

impl MarketWorkflowResult {
    pub fn final_stage(&self) -> Option<&MarketStageResult> {
        self.stages.last()
    }

    /// True when all stages solved successfully.
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// Solve a typed market workflow with each stage's embedded solve options.
///
/// When a stage has `commitment_source_stage_id = Some(prev_id)`, the
/// solved commitment from `prev_id` is extracted and substituted into
/// the stage's request as a `CommitmentPolicy::Fixed` schedule before
/// solving. This implements the standard "stage 1 commits, stage 2
/// dispatches against the fixed commitment" handoff.
pub fn solve_market_workflow(
    workflow: &MarketWorkflow,
) -> Result<MarketWorkflowResult, DispatchError> {
    solve_workflow_inner(workflow, None, None)
}

/// Solve a market workflow with one process-local options override applied to every stage.
pub fn solve_market_workflow_with_options(
    workflow: &MarketWorkflow,
    solve_options: &DispatchSolveOptions,
) -> Result<MarketWorkflowResult, DispatchError> {
    solve_workflow_inner(workflow, Some(solve_options), None)
}

/// Solve a market workflow, stopping after the named stage completes.
pub fn solve_market_workflow_until(
    workflow: &MarketWorkflow,
    stop_after_stage: &str,
    solve_options: Option<&DispatchSolveOptions>,
) -> Result<MarketWorkflowResult, DispatchError> {
    let known = workflow
        .stages
        .iter()
        .any(|s| s.stage_id == stop_after_stage);
    if !known {
        return Err(DispatchError::InvalidInput(format!(
            "stop_after_stage '{stop_after_stage}' not found in workflow stages: {:?}",
            workflow
                .stages
                .iter()
                .map(|s| &s.stage_id)
                .collect::<Vec<_>>()
        )));
    }
    solve_workflow_inner(workflow, solve_options, Some(stop_after_stage))
}

fn solve_workflow_inner(
    workflow: &MarketWorkflow,
    options_override: Option<&DispatchSolveOptions>,
    stop_after_stage: Option<&str>,
) -> Result<MarketWorkflowResult, DispatchError> {
    workflow.validate()?;
    let mut accumulated: Vec<MarketStageResult> = Vec::with_capacity(workflow.stages.len());

    for stage in &workflow.stages {
        let stage_start = Instant::now();
        let mut timings = StageTimings::default();

        let t = Instant::now();
        let mut request = stage.request.clone();
        timings.clone_request_secs = t.elapsed().as_secs_f64();

        // Commitment handoff: if this stage names a source stage,
        // extract its solved commitment and pin this stage to it.
        let t = Instant::now();
        if let Some(src_id) = &stage.commitment_source_stage_id {
            if let Some(src_result) = accumulated.iter().find(|r| r.stage_id == *src_id) {
                let resources = extract_commitment_schedule(&src_result.solution);
                request.set_commitment(CommitmentPolicy::Fixed(CommitmentSchedule { resources }));
            }
        }
        timings.commitment_handoff_secs = t.elapsed().as_secs_f64();

        // Branch thermal relaxation handoff: when the AC SCED stage is
        // about to inherit a DC SCUC commitment that pushes flows past
        // a branch's thermal limit, the AC NLP can't physically realise
        // those flows and exhausts its retry grid. Expand each
        // overloaded branch's per-period thermal limit to (rating +
        // dc_slack + margin) by injecting BranchDerateProfile entries
        // with derate_factor > 1.0 (uprate) before the stage solves.
        let t = Instant::now();
        if let Some(relax) = &stage.branch_relax_from_dc_slack {
            if let Some(src_result) = accumulated
                .iter()
                .find(|r| r.stage_id == relax.source_stage_id)
            {
                apply_branch_relax_from_dc_slack(
                    &mut request,
                    &src_result.solution,
                    &stage.model,
                    relax.margin_mva,
                );
            }
        }
        timings.branch_relax_secs = t.elapsed().as_secs_f64();

        // Dispatch pinning handoff: tighten generator P bounds around
        // the solved dispatch of a prior stage.
        // (When a retry policy is attached, the refinement runtime
        // re-applies per-attempt band configurations below; we still
        // apply the default pinning here so the base request's bounds
        // reflect the stage's canonical pin width.)
        // When an ac_sced_setup is attached, its
        // `ProducerDispatchPinning` supersedes this canonical pin; we
        // still run the canonical pin first so pinning defaults apply
        // to any generator the setup's subsets don't cover.
        let t = Instant::now();
        if let Some(pinning) = &stage.dispatch_pinning {
            if let Some(src_result) = accumulated
                .iter()
                .find(|r| r.stage_id == pinning.source_stage_id)
            {
                pin_generator_dispatch_bounds(&mut request, &src_result.solution, pinning);
            }
        }
        timings.dispatch_pinning_secs = t.elapsed().as_secs_f64();

        // AC SCED setup: reactive reserve filter, commitment
        // augmentation, bandable-subset pinning, warm start, Q-bound
        // overrides. Applied after canonical handoffs so the
        // ProducerDispatchPinning inside the setup can overwrite the
        // canonical pin's profile values.
        //
        // When a retry policy is attached, the refinement runtime
        // re-applies the producer dispatch pinning per attempt with
        // the attempt's band parameters — so skip it here to preserve
        // the original physical envelope (apply_producer_dispatch_pinning
        // reads the existing bounds as the physical envelope and would
        // otherwise narrow-against-narrowed on subsequent retries).
        let t = Instant::now();
        if let Some(setup) = &stage.ac_sced_setup {
            if let Some(src_result) = accumulated
                .iter()
                .find(|r| r.stage_id == setup.source_stage_id)
            {
                let apply_pinning = stage.retry_policy.is_none();
                crate::apply_ac_sced_setup(
                    setup,
                    &src_result.solution,
                    &mut request,
                    apply_pinning,
                );
            }
        }
        timings.ac_sced_setup_secs = t.elapsed().as_secs_f64();

        // Debug: dump the fully prepared request to disk so a failed
        // solve can be inspected post-mortem. Controlled by the
        // `SURGE_DUMP_STAGE_REQUEST` env var pointing at a directory;
        // each stage is written to `{dir}/{stage_id}.json`.
        if let Ok(dump_dir) = std::env::var("SURGE_DUMP_STAGE_REQUEST") {
            let path = std::path::Path::new(&dump_dir).join(format!("{}.json", stage.stage_id));
            if let Ok(json) = serde_json::to_string_pretty(&request) {
                let _ = std::fs::create_dir_all(&dump_dir);
                let _ = std::fs::write(&path, json);
            }
        }

        let opts = options_override.unwrap_or(&stage.solve_options);

        let solve_t = Instant::now();
        let solve_result = if let Some(policy) = &stage.retry_policy {
            let prior_stage_solution = stage
                .dispatch_pinning
                .as_ref()
                .and_then(|p| accumulated.iter().find(|r| r.stage_id == p.source_stage_id))
                .or_else(|| {
                    stage
                        .commitment_source_stage_id
                        .as_ref()
                        .and_then(|s| accumulated.iter().find(|r| r.stage_id == *s))
                })
                .map(|r| &r.solution);

            let producer_pinning = stage
                .ac_sced_setup
                .as_ref()
                .and_then(|s| s.dispatch_pinning.as_ref());
            let inputs = crate::RefinementInputs {
                model: &stage.model,
                base_request: &request,
                base_options: opts,
                policy,
                pinning: stage.dispatch_pinning.as_ref(),
                producer_pinning,
                prior_stage_solution,
                stage_id: &stage.stage_id,
            };
            crate::RefinementRuntime::solve(inputs).map(|(s, _report)| s)
        } else {
            stage.model.solve_with_options(&request, opts)
        };
        timings.solve_secs = solve_t.elapsed().as_secs_f64();

        match solve_result {
            Ok(solution) => {
                let acc_t = Instant::now();
                let solution = solution.with_stage_metadata(DispatchStageMetadata {
                    stage_id: stage.stage_id.clone(),
                    role: stage.role.clone(),
                    derived_from_stage_id: stage.derived_from_stage_id.clone(),
                    commitment_source_stage_id: stage.commitment_source_stage_id.clone(),
                });
                timings.accumulate_secs = acc_t.elapsed().as_secs_f64();
                timings.total_secs = stage_start.elapsed().as_secs_f64();
                info!(
                    stage = %stage.stage_id,
                    total_secs = timings.total_secs,
                    clone_secs = timings.clone_request_secs,
                    commitment_secs = timings.commitment_handoff_secs,
                    branch_relax_secs = timings.branch_relax_secs,
                    dispatch_pin_secs = timings.dispatch_pinning_secs,
                    ac_sced_setup_secs = timings.ac_sced_setup_secs,
                    solve_secs = timings.solve_secs,
                    accumulate_secs = timings.accumulate_secs,
                    "market workflow stage complete"
                );
                accumulated.push(MarketStageResult {
                    stage_id: stage.stage_id.clone(),
                    role: stage.role.clone(),
                    solution,
                    timings: Some(timings),
                });
                if stop_after_stage == Some(stage.stage_id.as_str()) {
                    break;
                }
            }
            Err(err) => {
                return Ok(MarketWorkflowResult {
                    stages: accumulated,
                    error: Some(MarketStageError {
                        stage_id: stage.stage_id.clone(),
                        role: stage.role.clone(),
                        error: err.to_string(),
                    }),
                });
            }
        }
    }
    Ok(MarketWorkflowResult {
        stages: accumulated,
        error: None,
    })
}

/// Pin the generator `p_min_mw` / `p_max_mw` profiles in `request` to
/// a narrow band around the MW dispatch solved in `source_solution`.
///
/// For each generator with a solved per-period MW, the band width is
/// `max(band_floor_mw, min(|target| * band_fraction, band_cap_mw))`;
/// new bounds are `[target - band, target + band]` clipped into the
/// existing envelope. Generators without a solved MW are left alone.
pub fn pin_generator_dispatch_bounds(
    request: &mut DispatchRequest,
    source_solution: &DispatchSolution,
    pinning: &DispatchPinningConfig,
) {
    let periods = source_solution.periods();
    // Build { resource_id: [mw per period] } from the source solution.
    let mut target_mw: HashMap<String, Vec<f64>> = HashMap::new();
    for (period_idx, period) in periods.iter().enumerate() {
        for r in period.resource_results() {
            if let ResourcePeriodDetail::Generator(_) = &r.detail {
                let slot = target_mw
                    .entry(r.resource_id.clone())
                    .or_insert_with(|| vec![0.0; periods.len()]);
                if period_idx < slot.len() {
                    slot[period_idx] = r.power_mw;
                }
            }
        }
    }
    let profiles = request.profiles_mut();
    for entry in profiles.generator_dispatch_bounds.profiles.iter_mut() {
        // Anchors skip pre-pin entirely so their profile bounds stay at
        // the physical envelope — the retry runtime's last-ditch anchor
        // rung relies on this.
        if pinning.anchor_resource_ids.contains(&entry.resource_id) {
            continue;
        }
        let Some(targets) = target_mw.get(&entry.resource_id) else {
            continue;
        };
        let n = entry
            .p_min_mw
            .len()
            .min(entry.p_max_mw.len())
            .min(targets.len());
        for (i, &target) in targets.iter().enumerate().take(n) {
            let band = (target.abs() * pinning.band_fraction)
                .max(pinning.band_floor_mw)
                .min(pinning.band_cap_mw);
            let new_min = (target - band).max(entry.p_min_mw[i]);
            let new_max = (target + band).min(entry.p_max_mw[i]);
            let (lo, hi) = if new_min <= new_max {
                (new_min, new_max)
            } else {
                let clipped = target.max(entry.p_min_mw[i]).min(entry.p_max_mw[i]);
                (clipped, clipped)
            };
            entry.p_min_mw[i] = lo;
            entry.p_max_mw[i] = hi;
        }
    }
}

/// Inject per-period branch thermal "uprate" derate factors into
/// `request` based on `branch_thermal` constraint slacks observed in
/// `source_solution`. For each (from_bus, to_bus, circuit) that
/// overloaded in the source stage, build a [`BranchDerateProfile`]
/// whose per-period factor equals
/// `(rating_a_mva + slack_mw + margin_mva) / rating_a_mva` (≥ 1.0)
/// and `1.0` in periods where the branch did not overload.
///
/// The downstream apply path multiplies `branch.rating_a_mva` by the
/// factor at each period (see `surge-dispatch/src/common/profiles.rs`),
/// so factors > 1.0 act as uprates — the AC SCED stage then sees a
/// thermal limit wide enough to absorb the DC stage's leftover overload
/// without forcing the AC NLP to find a power-flow point that doesn't
/// exist.
///
/// Constraint IDs are parsed as `branch:<from>:<to>:<circuit>:<dir>`
/// (the canonical form emitted by [`crate::result`]). Forward and reverse
/// direction slacks for the same physical circuit are folded into a
/// single per-period max — the thermal limit is on `|flow|`.
pub fn apply_branch_relax_from_dc_slack(
    request: &mut DispatchRequest,
    source_solution: &DispatchSolution,
    model: &DispatchModel,
    margin_mva: f64,
) {
    let periods = source_solution.periods();
    let n_periods = periods.len();
    if n_periods == 0 {
        return;
    }

    // (from_bus, to_bus, circuit) → per-period max slack across both
    // forward and reverse direction entries.
    let mut slack_by_circuit: HashMap<(u32, u32, String), Vec<f64>> = HashMap::new();
    for (period_idx, period) in periods.iter().enumerate() {
        for cr in period.constraint_results() {
            if cr.kind != ConstraintKind::BranchThermal {
                continue;
            }
            let Some(slack) = cr.slack_mw else {
                continue;
            };
            if slack <= 0.0 {
                continue;
            }
            let parts: Vec<&str> = cr.constraint_id.split(':').collect();
            if parts.len() < 4 || parts[0] != "branch" {
                continue;
            }
            let (Ok(from_bus), Ok(to_bus)) = (parts[1].parse::<u32>(), parts[2].parse::<u32>())
            else {
                continue;
            };
            let circuit = parts[3].to_string();
            let key = (from_bus, to_bus, circuit);
            let slot = slack_by_circuit
                .entry(key)
                .or_insert_with(|| vec![0.0; n_periods]);
            if period_idx < slot.len() && slack > slot[period_idx] {
                slot[period_idx] = slack;
            }
        }
    }

    if slack_by_circuit.is_empty() {
        return;
    }

    let network = model.network();
    let branch_map = network.branch_index_map();
    let profiles = request.profiles_mut();

    for ((from_bus, to_bus, circuit), slacks) in slack_by_circuit {
        // The constraint id may use either branch direction; the network
        // stores each branch in one canonical direction.
        let lookup = branch_map
            .get(&(from_bus, to_bus, circuit.clone()))
            .map(|&i| (i, from_bus, to_bus))
            .or_else(|| {
                branch_map
                    .get(&(to_bus, from_bus, circuit.clone()))
                    .map(|&i| (i, to_bus, from_bus))
            });
        let Some((idx, stored_from, stored_to)) = lookup else {
            continue;
        };
        let r_orig = network.branches[idx].rating_a_mva;
        if r_orig <= 0.0 {
            continue;
        }
        let factors: Vec<f64> = slacks
            .iter()
            .map(|&slack| {
                if slack > 0.0 {
                    (r_orig + slack + margin_mva) / r_orig
                } else {
                    1.0
                }
            })
            .collect();
        profiles.branch_derates.profiles.push(BranchDerateProfile {
            branch: BranchRef {
                from_bus: stored_from,
                to_bus: stored_to,
                circuit,
            },
            derate_factors: factors,
        });
    }
}

/// Extract the per-resource commitment schedule from a solved stage,
/// suitable for fixing a downstream stage's [`CommitmentPolicy`].
pub fn extract_commitment_schedule(solution: &DispatchSolution) -> Vec<ResourceCommitmentSchedule> {
    let periods = solution.periods();
    if periods.is_empty() {
        return Vec::new();
    }
    let mut by_resource: HashMap<String, Vec<bool>> = HashMap::new();
    for (period_idx, period) in periods.iter().enumerate() {
        for r in period.resource_results() {
            if let ResourcePeriodDetail::Generator(detail) = &r.detail {
                if let Some(committed) = detail.commitment {
                    let slot = by_resource
                        .entry(r.resource_id.clone())
                        .or_insert_with(|| vec![false; periods.len()]);
                    if period_idx < slot.len() {
                        slot[period_idx] = committed;
                    }
                }
            }
        }
    }
    let mut out: Vec<ResourceCommitmentSchedule> = by_resource
        .into_iter()
        .map(|(resource_id, periods)| ResourceCommitmentSchedule {
            initial: periods.first().copied().unwrap_or(false),
            periods: Some(periods),
            resource_id,
        })
        .collect();
    out.sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
    out
}
