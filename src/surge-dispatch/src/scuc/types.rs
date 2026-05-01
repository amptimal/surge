// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Internal security-dispatch specification.

use crate::dispatch::CommitmentMode;
use crate::request::{DispatchInput, SecurityCutStrategy, SecurityPreseedMethod};

/// Internal normalized specification for iterative N-1 security-constrained dispatch.
///
/// Public callers reach this through [`crate::DispatchRequest`] with
/// [`crate::SecurityScreening`].
#[derive(Debug, Clone)]
pub(crate) struct SecurityDispatchSpec {
    /// Canonical dispatch input for the underlying SCUC solve.
    pub input: DispatchInput,
    /// Time-coupled commitment mode for the underlying SCUC solve.
    pub commitment: CommitmentMode,
    /// Maximum outer-loop iterations (default 10).
    pub max_iterations: usize,
    /// Post-contingency flow violation tolerance in p.u. on system base (default 0.01).
    pub violation_tolerance_pu: f64,
    /// Maximum number of new flowgate cuts added per iteration (default 50).
    pub max_cuts_per_iteration: usize,
    /// Branch indices to consider as contingencies.
    /// Empty = all in-service branches with rate_a > min_rate_a (full N-1).
    pub contingency_branches: Vec<usize>,
    /// HVDC link indices to consider as contingencies (indexes into `input.hvdc_links`).
    /// When an HVDC link trips, its power injection is lost at both converter buses,
    /// causing flow redistribution on the AC network via PTDF shifts.
    /// Empty = no HVDC contingencies.
    pub hvdc_contingency_indices: Vec<usize>,
    /// Pre-seed iter 0 with this many top-ranked (ctg, mon) cuts per period.
    /// `0` disables pre-seeding (default).
    pub preseed_count_per_period: usize,
    /// Ranking method used to pick the top-N pairs when pre-seeding.
    pub preseed_method: SecurityPreseedMethod,
    /// Strategy for selecting new cuts from each screening pass.
    pub cut_strategy: SecurityCutStrategy,
    /// Optional active-cut cap for the iterative flowgate pool.
    pub max_active_cuts: Option<usize>,
    /// Optional activity-aging threshold. Active cuts whose slack and shadow
    /// price remain near zero for this many solved rounds are retired even
    /// before `max_active_cuts` is reached.
    pub cut_retire_after_rounds: Option<usize>,
    /// Violation-count threshold for adaptive last-mile selection.
    pub targeted_cut_threshold: usize,
    /// Last-mile cap for adaptive selection.
    pub targeted_cut_cap: usize,
    /// Emit the final near-binding contingency diagnostic report.
    pub near_binding_report: bool,
}

impl Default for SecurityDispatchSpec {
    fn default() -> Self {
        Self {
            input: DispatchInput::default(),
            commitment: CommitmentMode::Optimize(Default::default()),
            max_iterations: 10,
            violation_tolerance_pu: 0.01,
            max_cuts_per_iteration: 50,
            contingency_branches: Vec::new(),
            hvdc_contingency_indices: Vec::new(),
            preseed_count_per_period: 0,
            preseed_method: SecurityPreseedMethod::None,
            cut_strategy: SecurityCutStrategy::Fixed,
            max_active_cuts: None,
            cut_retire_after_rounds: None,
            targeted_cut_threshold: 50_000,
            targeted_cut_cap: 50_000,
            near_binding_report: false,
        }
    }
}
