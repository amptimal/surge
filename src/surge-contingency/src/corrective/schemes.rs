// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! RAS scheme types, configuration, and outcome recording.

use serde::{Deserialize, Serialize};
use surge_network::network::ContingencyModification;

use super::actions::CorrectiveAction;
use super::triggers::{ArmCondition, RasTriggerCondition};
use crate::Violation;

/// A Remedial Action Scheme (RAS/SPS) — triggered by specific contingencies and
/// applies a list of corrective actions when armed.
///
/// Schemes are applied in **priority order** (lower `priority` = fires first).
/// After each scheme fires, the power flow is re-solved and remaining schemes'
/// trigger conditions are re-evaluated against the updated state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemedialActionScheme {
    /// Human-readable name (e.g. "ERCOT RAS-001 Outage of Line 12-34").
    pub name: String,
    /// Firing priority.  Lower values fire first.  Schemes at equal priority
    /// fire in definition order (stable sort).
    #[serde(default)]
    pub priority: i32,
    /// Mutual exclusion group.  Within a group, only the highest-priority
    /// (lowest `priority` value) triggered scheme fires.  Schemes with `None`
    /// are never excluded by other schemes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_group: Option<String>,
    /// Pre-contingency arming conditions evaluated against the base-case solved
    /// state.  The scheme is only eligible to fire if **all** arm conditions
    /// evaluate to `true` (implicit AND).
    ///
    /// When empty, the scheme is always armed (unconditional).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arm_conditions: Vec<ArmCondition>,
    /// Post-contingency trigger conditions.  The scheme fires when **any**
    /// top-level condition evaluates to `true`.  Use
    /// [`RasTriggerCondition::All`] to express AND logic.
    ///
    /// Trigger conditions are re-evaluated after each higher-priority scheme
    /// fires, against the **updated** post-RAS violation state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trigger_conditions: Vec<RasTriggerCondition>,
    /// Corrective actions to apply when triggered.
    pub actions: Vec<CorrectiveAction>,
    /// Network modifications to apply to the post-contingency network when triggered.
    ///
    /// These reuse the same [`ContingencyModification`] vocabulary used for
    /// PSS/E `.con` file SET/CHANGE commands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifications: Vec<ContingencyModification>,
    /// Maximum total generation redispatch magnitude allowed (MW).
    /// The sum of |delta_p_mw| across all `GeneratorRedispatch` actions must
    /// not exceed this value.  Set to `f64::INFINITY` for no limit.
    pub max_redispatch_mw: f64,
}

/// Which violation types must be cleared for a contingency to be considered
/// "correctable" by the corrective action engine.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum CorrectableCriteria {
    /// All violations must be cleared (NERC TPL default).
    #[default]
    AllViolations,
    /// Only thermal + voltage must be cleared (common WECC practice for
    /// studies where flowgate/interface are handled separately).
    ThermalAndVoltage,
    /// Only thermal overloads must be cleared (legacy behavior).
    ThermalOnly,
    /// Custom: specify which violation types count.
    Custom {
        thermal: bool,
        voltage_low: bool,
        voltage_high: bool,
        flowgate: bool,
        interface: bool,
    },
}

impl CorrectableCriteria {
    /// Returns `true` if this violation counts toward the correctability check.
    pub fn counts(&self, v: &Violation) -> bool {
        match self {
            CorrectableCriteria::AllViolations => !matches!(v, Violation::Islanding { .. }),
            CorrectableCriteria::ThermalAndVoltage => matches!(
                v,
                Violation::ThermalOverload { .. }
                    | Violation::VoltageLow { .. }
                    | Violation::VoltageHigh { .. }
                    | Violation::NonConvergent { .. }
            ),
            CorrectableCriteria::ThermalOnly => matches!(
                v,
                Violation::ThermalOverload { .. } | Violation::NonConvergent { .. }
            ),
            CorrectableCriteria::Custom {
                thermal,
                voltage_low,
                voltage_high,
                flowgate,
                interface,
            } => match v {
                Violation::ThermalOverload { .. } => *thermal,
                Violation::VoltageLow { .. } => *voltage_low,
                Violation::VoltageHigh { .. } => *voltage_high,
                Violation::FlowgateOverload { .. } => *flowgate,
                Violation::InterfaceOverload { .. } => *interface,
                Violation::NonConvergent { .. } => true,
                Violation::Islanding { .. } => false,
            },
        }
    }

    pub(crate) fn is_correctable(&self, violations: &[Violation]) -> bool {
        !violations.iter().any(|v| self.counts(v))
    }
}

/// Configuration for post-contingency corrective action evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectiveActionConfig {
    /// Explicit RAS/SPS schemes to check after each N-1 contingency.
    pub schemes: Vec<RemedialActionScheme>,
    /// When `true`, attempt greedy generator redispatch to relieve remaining
    /// thermal violations after explicit RAS actions have been applied.
    pub enable_greedy_redispatch: bool,
    /// When `true`, attempt Q-V sensitivity-based reactive dispatch, shunt
    /// switching, and transformer tap adjustment to relieve voltage violations.
    pub enable_reactive_redispatch: bool,
    /// Maximum greedy redispatch outer iterations (shared by thermal, reactive,
    /// and flowgate greedy loops).
    pub max_redispatch_iter: usize,
    /// Fraction of violation relief to target per greedy iteration (0.0–1.0).
    /// A value of 0.5 means each iteration targets 50% of the remaining overload.
    pub redispatch_step_fraction: f64,
    /// When `true`, apply load shedding as a last resort if all redispatch
    /// options are exhausted and violations remain.
    pub enable_load_shed: bool,
    /// Maximum total load that may be shed per contingency (MW).
    pub max_load_shed_mw: f64,
    /// Which violation types must be cleared for the contingency to be
    /// considered "correctable". Default: [`CorrectableCriteria::AllViolations`].
    #[serde(default)]
    pub correctable_criteria: CorrectableCriteria,
    /// Flowgate definitions for flowgate-aware greedy redispatch.
    /// Each entry maps a flowgate to its component branches + direction coefficients.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flowgates: Vec<FlowgateRedispatchDef>,
    /// Interface definitions for interface-aware greedy redispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<FlowgateRedispatchDef>,
}

/// A flowgate or interface definition for greedy redispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowgateRedispatchDef {
    /// Flowgate/interface name (must match the name in [`Violation::FlowgateOverload`]
    /// or [`Violation::InterfaceOverload`]).
    pub name: String,
    /// Component branch indices + direction coefficients.
    /// The aggregated flowgate flow = Σ coeff_i × branch_flow_i.
    pub branch_coefficients: Vec<(usize, f64)>,
}

impl Default for CorrectiveActionConfig {
    fn default() -> Self {
        Self {
            schemes: Vec::new(),
            enable_greedy_redispatch: true,
            enable_reactive_redispatch: true,
            max_redispatch_iter: 10,
            redispatch_step_fraction: 0.5,
            enable_load_shed: false,
            max_load_shed_mw: 0.0,
            correctable_criteria: CorrectableCriteria::default(),
            flowgates: Vec::new(),
            interfaces: Vec::new(),
        }
    }
}

/// Per-scheme audit record: what happened to this RAS during evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemeOutcome {
    /// Name of the RAS scheme.
    pub scheme_name: String,
    /// Priority value of the scheme.
    pub priority: i32,
    /// What happened.
    pub outcome: SchemeStatus,
}

/// Disposition of a single RAS scheme during corrective action evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SchemeStatus {
    /// Scheme was not armed (pre-contingency arm conditions not met).
    NotArmed,
    /// Scheme was armed but trigger conditions did not match post-contingency state.
    NotTriggered,
    /// Scheme was armed and triggered but skipped — its exclusion group was
    /// already consumed by the named scheme.
    ExcludedBy { fired_scheme: String },
    /// Scheme was armed and triggered but total redispatch MW exceeded limit.
    RedispatchExceeded { requested_mw: f64, limit_mw: f64 },
    /// Scheme fired successfully.
    Fired {
        actions_applied: Vec<CorrectiveAction>,
        violations_before: usize,
        violations_after: usize,
    },
    /// Scheme was armed and triggered but skipped because all violations
    /// were already cleared by higher-priority schemes.
    Unnecessary,
}

/// Outcome of applying corrective actions to a post-contingency violation set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectiveActionResult {
    /// All corrective actions applied (from RAS + greedy redispatch + load shed).
    pub corrective_actions_applied: Vec<CorrectiveAction>,
    /// Violations remaining after all corrective actions were attempted.
    /// Empty means the contingency was fully corrected.
    pub violations_after_correction: Vec<Violation>,
    /// `true` if all violations matching [`CorrectableCriteria`] were cleared.
    pub correctable: bool,
    /// Per-scheme audit trail: which RAS fired, which were skipped, and why.
    pub scheme_outcomes: Vec<SchemeOutcome>,
}
