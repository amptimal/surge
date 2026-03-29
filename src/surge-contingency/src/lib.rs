// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge Contingency — Parallel N-1 contingency analysis with LODF screening.
//!
//! Two-stage approach:
//! 1. **LODF screening** (optional): Uses DC power transfer distribution factors
//!    to quickly identify contingencies that *might* cause thermal overloads.
//!    For branch outages, eliminates ~90% of contingencies that clearly won't
//!    cause violations. Generator, breaker, HVDC, and other non-branch
//!    contingencies bypass branch-only LODF screening and proceed to AC.
//!    For large networks, uses sparse column-wise LODF via factored B' matrix
//!    to avoid O(n_br²) memory.
//! 2. **Parallel AC solve**: Solves remaining contingencies using Newton-Raphson
//!    with KLU, parallelized across CPU cores via rayon.
//!
//! Detects thermal overloads (branch MVA > rating) and voltage violations.

// ---------------------------------------------------------------------------
// Module structure
// ---------------------------------------------------------------------------
mod engine;
pub mod prepared;
pub mod ranking;
pub mod scrd;
mod screening;
pub(crate) mod types;
mod violations;

#[cfg(test)]
pub(crate) mod test_util;

pub mod advanced;
pub mod corrective;
pub mod generation {
    pub use crate::engine::{
        generate_n1_branch_contingencies, generate_n1_generator_contingencies,
        generate_n2_branch_contingencies,
    };
}
pub mod probabilistic;
pub mod tpl_report;
pub mod voltage;

// ---------------------------------------------------------------------------
// Re-exports: types (public API surface)
// ---------------------------------------------------------------------------
pub use types::{
    AnalysisSummary, BusVoltageStress, ContingencyAnalysis, ContingencyError, ContingencyMetric,
    ContingencyOptions, ContingencyResult, ContingencyStatus, ProgressCallback, ScreeningMode,
    ThermalRating, Violation, VoltageStressMode, VoltageStressOptions, VoltageStressResult,
    VsmCategory, get_rating,
};

// ---------------------------------------------------------------------------
// Re-exports: engine (public API functions)
// ---------------------------------------------------------------------------
pub use engine::{
    analyze_contingencies, analyze_n1_branch, analyze_n1_generator, analyze_n2_branch,
};

// ---------------------------------------------------------------------------
// Crate-internal re-exports (for use by sibling modules and tests)
// ---------------------------------------------------------------------------
#[allow(unused_imports)]
pub(crate) use engine::islands::find_connected_components;
#[allow(unused_imports)]
pub(crate) use ranking::contingency_severity_score;
#[allow(unused_imports)]
pub(crate) use screening::screen_with_lodf;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
