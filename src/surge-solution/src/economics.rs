// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared objective-ledger types used by OPF and dispatch reporting.

use serde::{Deserialize, Serialize};

/// High-level accounting bucket for an objective term.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveBucket {
    #[default]
    Other,
    Energy,
    Reserve,
    NoLoad,
    Startup,
    Shutdown,
    Penalty,
    Tracking,
    Adder,
}

/// Detailed classifier for one objective contribution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveTermKind {
    #[default]
    Other,
    GeneratorEnergy,
    GeneratorNoLoad,
    GeneratorStartup,
    GeneratorShutdown,
    StorageEnergy,
    StorageOfferEpigraph,
    DispatchableLoadEnergy,
    DispatchableLoadTargetTracking,
    GeneratorTargetTracking,
    ReserveProcurement,
    ReserveShortfall,
    ReactiveReserveProcurement,
    ReactiveReserveShortfall,
    VirtualBid,
    HvdcEnergy,
    CarbonAdder,
    PowerBalancePenalty,
    ReactiveBalancePenalty,
    ThermalLimitPenalty,
    FlowgatePenalty,
    InterfacePenalty,
    RampPenalty,
    VoltagePenalty,
    AngleDifferencePenalty,
    CommitmentCapacityPenalty,
    EnergyWindowPenalty,
    CombinedCycleNoLoad,
    CombinedCycleTransition,
    CombinedCycleDispatch,
    BranchSwitchingStartup,
    BranchSwitchingShutdown,
    ExplicitContingencyWorstCase,
    ExplicitContingencyAverageCase,
    BendersEta,
}

/// Public subject scope for an objective term.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveSubjectKind {
    #[default]
    Other,
    System,
    Resource,
    Bus,
    Branch,
    Flowgate,
    Interface,
    ReserveRequirement,
    HvdcLink,
    CombinedCyclePlant,
    VirtualBid,
}

/// Unit associated with an optional objective quantity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveQuantityUnit {
    #[default]
    Other,
    Mw,
    Mwh,
    Mvar,
    Mva,
    Pu,
    PuHour,
    Rad,
    Event,
}

/// Exact objective contribution for one reported component.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObjectiveTerm {
    /// Stable component label within the term's subject, such as `energy`,
    /// `startup`, `reserve:spin`, or `curtailment_segment_0`.
    pub component_id: String,
    /// High-level accounting bucket.
    pub bucket: ObjectiveBucket,
    /// Detailed classifier.
    pub kind: ObjectiveTermKind,
    /// Subject scope.
    pub subject_kind: ObjectiveSubjectKind,
    /// Stable subject id such as a resource id, reserve requirement id, or
    /// `system`.
    pub subject_id: String,
    /// Exact objective contribution in dollars for the solved interval or
    /// aggregate horizon view.
    pub dollars: f64,
    /// Optional physical quantity associated with the term.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<f64>,
    /// Unit for `quantity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_unit: Option<ObjectiveQuantityUnit>,
    /// Optional unit rate when the term has a meaningful single rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_rate: Option<f64>,
}

/// Scope classifier for an objective-ledger reconciliation check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveLedgerScopeKind {
    #[default]
    Other,
    OpfSolution,
    DispatchSolution,
    DispatchPeriod,
    ResourcePeriod,
    ResourceSummary,
    CombinedCyclePlant,
}

/// Exact-vs-reported mismatch surfaced by audit helpers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObjectiveLedgerMismatch {
    /// Where the mismatch was detected.
    pub scope_kind: ObjectiveLedgerScopeKind,
    /// Stable scope identifier such as `summary`, `period:3`, or `gen_1_2`.
    pub scope_id: String,
    /// Report field that failed reconciliation.
    pub field: String,
    /// Expected exact dollars from the objective ledger.
    pub expected_dollars: f64,
    /// Actual reported dollars carried on the public result.
    pub actual_dollars: f64,
    /// Signed difference `actual - expected`.
    pub difference: f64,
}

/// Version tag for the persisted solution-audit schema.
pub const SOLUTION_AUDIT_SCHEMA_VERSION: &str = "1";

/// Whether the objective-ledger audit should run on solution finalization.
///
/// Controlled by the `SURGE_OBJECTIVE_AUDIT` env var; off by default. The
/// audit is a ledger-sum consistency check — useful when debugging a new
/// penalty-term or cost-rollup wiring, not useful on every solve. Enable
/// with `SURGE_OBJECTIVE_AUDIT=1`.
///
/// When disabled, `refresh_audit()` is a no-op and the `audit` field on
/// the solution keeps its serde default (`audit_passed: false`,
/// `ledger_mismatches: []`). Callers that want to run the audit on
/// demand can still invoke `objective_ledger_mismatches()` directly —
/// this gate only affects the auto-populated `audit` block.
pub fn objective_audit_enabled() -> bool {
    match std::env::var("SURGE_OBJECTIVE_AUDIT") {
        Ok(value) => {
            let trimmed = value.trim();
            !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("0")
                && !trimmed.eq_ignore_ascii_case("false")
                && !trimmed.eq_ignore_ascii_case("off")
                && !trimmed.eq_ignore_ascii_case("no")
        }
        Err(_) => false,
    }
}

/// Serialized audit status carried alongside a persisted solution payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolutionAuditReport {
    /// Version of the audit-block schema.
    pub schema_version: String,
    /// Whether the solution passed all exact objective-ledger checks.
    pub audit_passed: bool,
    /// Whether any residual terms remain in the objective ledger.
    pub has_residual_terms: bool,
    /// Exact ledger mismatches surfaced during validation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ledger_mismatches: Vec<ObjectiveLedgerMismatch>,
}

impl Default for SolutionAuditReport {
    fn default() -> Self {
        Self {
            schema_version: SOLUTION_AUDIT_SCHEMA_VERSION.to_string(),
            audit_passed: false,
            has_residual_terms: false,
            ledger_mismatches: Vec::new(),
        }
    }
}

impl SolutionAuditReport {
    /// Build a persisted audit block from the exact objective-ledger mismatches.
    pub fn from_mismatches(mut ledger_mismatches: Vec<ObjectiveLedgerMismatch>) -> Self {
        ledger_mismatches.sort_by(|lhs, rhs| {
            (
                lhs.scope_kind as u8,
                lhs.scope_id.as_str(),
                lhs.field.as_str(),
            )
                .cmp(&(
                    rhs.scope_kind as u8,
                    rhs.scope_id.as_str(),
                    rhs.field.as_str(),
                ))
        });
        let has_residual_terms = ledger_mismatches
            .iter()
            .any(|mismatch| mismatch.field == "residual");
        Self {
            schema_version: SOLUTION_AUDIT_SCHEMA_VERSION.to_string(),
            audit_passed: ledger_mismatches.is_empty(),
            has_residual_terms,
            ledger_mismatches,
        }
    }
}

/// Shared hook for solution types that can compute an exact persisted audit block.
pub trait AuditableSolution {
    fn computed_solution_audit(&self) -> SolutionAuditReport;
}
