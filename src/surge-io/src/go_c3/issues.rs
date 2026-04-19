// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Warning/error tracking for the GO C3 adapter pipeline.
//!
//! Mirrors Python's `markets/go_c3/adapter.py::AdapterIssue`. Each adapter
//! stage records the notable decisions it made (ambiguous inputs, unsupported
//! features, mappings that required heuristics) via [`GoC3Context::add_issue`].
//!
//! Consumers can surface these to users through the solve report, or
//! compare against a reference set to catch regressions in adapter behaviour.

use serde::{Deserialize, Serialize};

/// Severity of an adapter issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoC3IssueSeverity {
    /// Informational note about a decision the adapter made.
    Info,
    /// Non-fatal concern the user should know about (e.g. a heuristic was
    /// required because the input was ambiguous).
    Warning,
    /// Structural problem that prevented a clean mapping. The adapter may
    /// still produce a network/request but the downstream solve is likely
    /// to be wrong.
    Error,
}

/// A single adapter issue. Equivalent to Python `AdapterIssue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Issue {
    pub severity: GoC3IssueSeverity,
    /// Short machine-readable code (e.g. `"slack_bus_inferred"`).
    pub code: String,
    /// Free-form human-readable message with specifics.
    pub message: String,
}

impl GoC3Issue {
    pub fn info(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: GoC3IssueSeverity::Info,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: GoC3IssueSeverity::Warning,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: GoC3IssueSeverity::Error,
            code: code.into(),
            message: message.into(),
        }
    }
}
