// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Commitment-related request types.

use schemars::JsonSchema;

use crate::request::CommitmentPolicyKind;

/// Initial commitment metadata for one resource.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct CommitmentInitialCondition {
    pub resource_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hours_on: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_24h: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_168h: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_mwh_24h: Option<f64>,
}

/// Public commitment optimization controls.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct CommitmentOptions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initial_conditions: Vec<CommitmentInitialCondition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warm_start_commitment: Vec<ResourcePeriodCommitment>,
    pub time_limit_secs: Option<f64>,
    /// Relative MIP optimality gap for the commitment optimizer (e.g. 0.01 = 1%).
    /// When set, the solver will continue improving the solution until this gap
    /// is reached or the time limit expires.
    pub mip_rel_gap: Option<f64>,
    /// Optional time-varying MIP gap schedule for the commitment optimizer.
    ///
    /// Each entry is `(time_secs, gap)`: at solve wall time `t`, the
    /// acceptable gap is the `gap` of the latest entry with `time_secs <= t`.
    /// The solver terminates as soon as the current incumbent's gap is
    /// within the target. When omitted, the caller's static `mip_rel_gap`
    /// is used unchanged. Backends without progress-callback support
    /// ignore this field and fall back to the static safety net.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mip_gap_schedule: Option<Vec<(f64, f64)>>,

    /// When `true`, skip the SCUC MIP warm-start pipeline entirely — no
    /// load-cover helper solve, no reduced-relaxed LP, no reduced-core
    /// MIP, no conservative fallback, no dense-primal-start construction.
    /// The SCUC MIP is handed to the solver cold. On cases the MIP can
    /// nail at the root this saves the full warm-start overhead
    /// (multiple seconds on 617-bus). On harder cases the warm start
    /// can pay for itself, so leave at default `false` for production.
    #[serde(default)]
    pub disable_warm_start: bool,
}

/// Fixed commitment schedule for one resource.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceCommitmentSchedule {
    pub resource_id: String,
    pub initial: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub periods: Option<Vec<bool>>,
}

/// Minimum commitment floor for one resource across the solved periods.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourcePeriodCommitment {
    pub resource_id: String,
    pub periods: Vec<bool>,
}

/// Fixed commitment schedule for dispatch-only studies.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct CommitmentSchedule {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceCommitmentSchedule>,
}

/// One linear commitment-cut term keyed by resource id.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommitmentTerm {
    pub resource_id: String,
    pub coeff: f64,
}

/// Public commitment constraint keyed by resource ids.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommitmentConstraint {
    pub name: String,
    pub period_idx: usize,
    pub terms: Vec<CommitmentTerm>,
    pub lower_bound: f64,
    pub penalty_cost: Option<f64>,
}

/// Public commitment policy for dispatch studies.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentPolicy {
    /// All in-service generators are committed.
    #[default]
    AllCommitted,
    /// Commitment is provided externally.
    Fixed(CommitmentSchedule),
    /// Optimize commitment endogenously.
    Optimize(CommitmentOptions),
    /// Lock day-ahead commitments on and optimize only additional units.
    Additional {
        /// Per-resource minimum commitment floor.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        minimum_commitment: Vec<ResourcePeriodCommitment>,
        /// Commitment optimization settings.
        options: CommitmentOptions,
    },
}

impl CommitmentPolicy {
    pub fn kind(&self) -> CommitmentPolicyKind {
        match self {
            Self::AllCommitted => CommitmentPolicyKind::AllCommitted,
            Self::Fixed(_) => CommitmentPolicyKind::Fixed,
            Self::Optimize(_) => CommitmentPolicyKind::Optimize,
            Self::Additional { .. } => CommitmentPolicyKind::Additional,
        }
    }
}
