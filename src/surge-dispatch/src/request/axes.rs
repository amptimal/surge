// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Study axes for dispatch requests.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Power balance formulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Formulation {
    /// DC linearized power balance (B-theta LP via HiGHS/Gurobi). Fast, approximate.
    #[default]
    Dc,
    /// Full AC polar power balance (NLP via Ipopt). Exact, slower.
    Ac,
}

/// How dispatch intervals are coupled across the study timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntervalCoupling {
    /// Solve intervals one at a time, threading state between solves.
    #[default]
    PeriodByPeriod,
    /// Solve all intervals in one time-coupled optimization.
    TimeCoupled,
}

/// Coarse commitment classification used in result metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentPolicyKind {
    AllCommitted,
    Fixed,
    Optimize,
    Additional,
}
