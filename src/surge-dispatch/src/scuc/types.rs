// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Internal security-dispatch specification.

use crate::dispatch::CommitmentMode;
use crate::request::{DispatchInput, SecurityPreseedMethod};

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
        }
    }
}
