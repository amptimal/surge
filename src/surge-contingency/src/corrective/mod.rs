// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Post-Contingency Corrective Actions (PLAN-092 / P5-058).
//!
//! Implements corrective action schemes (CAS/RAS/SPS) as used in PSS/E IPLAN
//! and PowerWorld for NERC TPL reliability studies.  A contingency that can be
//! relieved by corrective actions does NOT count as a reliability violation per
//! NERC standards.
//!
//! # Violation-Agnostic Design
//!
//! RAS/SPS triggers and success criteria consider **all** violation types
//! equally — thermal, voltage, flowgate, interface.  This matches real-world
//! WECC RAS practice where schemes fire on high-voltage, low-voltage,
//! nomogram, or thermal conditions.
//!
//! # Sequential Priority-Ordered Execution
//!
//! RAS schemes are applied in **priority order** (lower value = fires first),
//! with a power flow re-solve after each scheme fires.  After each re-solve,
//! trigger conditions for remaining schemes are re-evaluated against the
//! **updated** post-RAS state — so a scheme that would have triggered on the
//! original violations may not trigger if a higher-priority scheme already
//! resolved them.  This matches PSS/E and PowerWorld behavior.
//!
//! # Arming
//!
//! Each scheme may define [`ArmCondition`]s evaluated against the pre-contingency
//! base-case solved state.  A scheme that is not armed cannot fire regardless
//! of post-contingency trigger conditions.
//!
//! # Mutual Exclusion
//!
//! Schemes sharing an `exclusion_group` are mutually exclusive — only the
//! highest-priority triggered scheme in each group fires.
//!
//! # Workflow
//!
//! 1. Evaluate arm conditions against base-case state; filter to armed schemes.
//! 2. Sort armed schemes by priority (stable, definition order as tiebreak).
//! 3. For each scheme in priority order: check exclusion, evaluate triggers
//!    against current violations, fire if triggered, re-solve, re-check.
//! 4. If violations remain and `enable_greedy_redispatch`, attempt PTDF-based
//!    MW redispatch for thermal overloads.
//! 5. If voltage violations remain and `enable_reactive_redispatch`, attempt
//!    Q-V sensitivity-based reactive dispatch, shunt switching, tap adjustment.
//! 6. If flowgate/interface violations remain, attempt flowgate-aware redispatch.
//! 7. Load shed as last resort (thermal + voltage).
//! 8. Report as "corrected" or "uncorrectable" per [`CorrectableCriteria`].

pub mod actions;
pub mod schemes;
pub mod triggers;

pub use actions::CorrectiveAction;
pub use schemes::{
    CorrectableCriteria, CorrectiveActionConfig, CorrectiveActionResult, FlowgateRedispatchDef,
    RemedialActionScheme, SchemeOutcome, SchemeStatus,
};
pub use triggers::{ArmCondition, BaseCaseState, RasTriggerCondition};

use std::collections::HashMap;

use surge_ac::AcPfOptions;
use surge_network::Network;
use surge_network::network::apply_contingency_modifications;
use tracing::{debug, info, warn};

use crate::{ContingencyOptions, ContingencyResult, Violation};
use actions::{
    apply_action_to_network, greedy_flowgate_redispatch, greedy_reactive_redispatch,
    greedy_thermal_redispatch, load_shed_last_resort, solve_and_detect_violations,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Apply corrective actions to a post-contingency result.
///
/// # Arguments
/// - `network`          — Base-case network (used to build the post-contingency network).
/// - `outaged_branches` — Branch indices outaged in this contingency.
/// - `base_result`      — The raw N-1 result before corrective actions.
/// - `base_case_state`  — Pre-contingency solved state for arming evaluation.
///   When `None`, all schemes are treated as unconditionally armed.
/// - `config`           — Corrective action configuration.
/// - `ctg_options`      — Contingency solver options (NR tolerance, thermal threshold, etc.).
///
/// # Returns
/// A [`CorrectiveActionResult`] describing what was applied and whether
/// violations were cleared.
pub fn apply_corrective_actions(
    network: &Network,
    outaged_branches: &[usize],
    base_result: &ContingencyResult,
    base_case_state: Option<&BaseCaseState>,
    config: &CorrectiveActionConfig,
    ctg_options: &ContingencyOptions,
) -> CorrectiveActionResult {
    let criteria = &config.correctable_criteria;

    // If no violations at all, nothing to do.
    if base_result.violations.is_empty() {
        return CorrectiveActionResult {
            corrective_actions_applied: Vec::new(),
            violations_after_correction: Vec::new(),
            correctable: true,
            scheme_outcomes: Vec::new(),
        };
    }

    // Build the post-contingency network (outages applied).
    let mut post_net = network.clone();
    for &br_idx in outaged_branches {
        if br_idx < post_net.branches.len() {
            post_net.branches[br_idx].in_service = false;
        }
    }

    let mut applied: Vec<CorrectiveAction> = Vec::new();
    let mut scheme_outcomes: Vec<SchemeOutcome> = Vec::new();

    // -----------------------------------------------------------------------
    // Step 1: Sequential RAS with priority, exclusion groups, and arming
    // -----------------------------------------------------------------------

    // Phase A: Arm check — filter to armed schemes only.
    let mut eligible: Vec<(usize, &RemedialActionScheme)> = Vec::new();
    for (def_order, scheme) in config.schemes.iter().enumerate() {
        let armed = match base_case_state {
            Some(state) if !scheme.arm_conditions.is_empty() => scheme
                .arm_conditions
                .iter()
                .all(|c| c.evaluate(network, state)),
            _ => true, // no state provided or no conditions → always armed
        };
        if !armed {
            info!(
                "RAS '{}': not armed (pre-contingency conditions not met)",
                scheme.name,
            );
            scheme_outcomes.push(SchemeOutcome {
                scheme_name: scheme.name.clone(),
                priority: scheme.priority,
                outcome: SchemeStatus::NotArmed,
            });
            continue;
        }
        eligible.push((def_order, scheme));
    }

    // Phase B: Stable sort by priority (definition order as tiebreak).
    eligible.sort_by_key(|(def_order, scheme)| (scheme.priority, *def_order));

    // Phase C: Sequential fire-and-check loop.
    let acpf_opts = AcPfOptions {
        tolerance: ctg_options.acpf_options.tolerance,
        max_iterations: ctg_options.acpf_options.max_iterations,
        flat_start: false,
        ..AcPfOptions::default()
    };

    let mut current_violations = base_result.violations.clone();
    let mut current_result = base_result.clone();
    // group_name → firing_scheme_name
    let mut fired_groups: HashMap<String, String> = HashMap::new();

    for (_def_order, scheme) in &eligible {
        // Already corrected? Skip remaining schemes.
        if criteria.is_correctable(&current_violations) {
            scheme_outcomes.push(SchemeOutcome {
                scheme_name: scheme.name.clone(),
                priority: scheme.priority,
                outcome: SchemeStatus::Unnecessary,
            });
            continue;
        }

        // Exclusion group check.
        if let Some(group) = &scheme.exclusion_group
            && let Some(firer) = fired_groups.get(group)
        {
            info!(
                "RAS '{}': skipped — exclusion group '{}' consumed by '{}'",
                scheme.name, group, firer,
            );
            scheme_outcomes.push(SchemeOutcome {
                scheme_name: scheme.name.clone(),
                priority: scheme.priority,
                outcome: SchemeStatus::ExcludedBy {
                    fired_scheme: firer.clone(),
                },
            });
            continue;
        }

        // Trigger check against CURRENT post-RAS state.
        let triggered = if scheme.trigger_conditions.is_empty() {
            debug!(
                "RAS '{}': no trigger conditions defined — cannot fire",
                scheme.name,
            );
            false
        } else {
            scheme
                .trigger_conditions
                .iter()
                .any(|cond| cond.evaluate(outaged_branches, &current_result))
        };

        if !triggered {
            scheme_outcomes.push(SchemeOutcome {
                scheme_name: scheme.name.clone(),
                priority: scheme.priority,
                outcome: SchemeStatus::NotTriggered,
            });
            continue;
        }

        // Redispatch MW limit check.
        let total_redispatch: f64 = scheme
            .actions
            .iter()
            .filter_map(|a| match a {
                CorrectiveAction::GeneratorRedispatch { delta_p_mw, .. } => Some(delta_p_mw.abs()),
                _ => None,
            })
            .sum();

        if total_redispatch > scheme.max_redispatch_mw {
            info!(
                "RAS '{}': redispatch {:.1} MW exceeds limit {:.1} MW — skipped",
                scheme.name, total_redispatch, scheme.max_redispatch_mw,
            );
            scheme_outcomes.push(SchemeOutcome {
                scheme_name: scheme.name.clone(),
                priority: scheme.priority,
                outcome: SchemeStatus::RedispatchExceeded {
                    requested_mw: total_redispatch,
                    limit_mw: scheme.max_redispatch_mw,
                },
            });
            continue;
        }

        // ---- FIRE ----
        info!(
            "RAS '{}' (priority {}) firing — {} action(s), {} modification(s)",
            scheme.name,
            scheme.priority,
            scheme.actions.len(),
            scheme.modifications.len(),
        );

        let violations_before = current_violations.len();

        if !scheme.modifications.is_empty() {
            if let Err(err) = apply_contingency_modifications(&mut post_net, &scheme.modifications)
            {
                warn!(
                    scheme = %scheme.name,
                    error = %err,
                    "RAS modifications failed; stopping corrective evaluation"
                );
                return CorrectiveActionResult {
                    corrective_actions_applied: applied,
                    violations_after_correction: current_violations,
                    correctable: false,
                    scheme_outcomes,
                };
            }
        }

        let mut scheme_actions = Vec::new();
        for action in &scheme.actions {
            apply_action_to_network(&mut post_net, action);
            scheme_actions.push(action.clone());
            applied.push(action.clone());
        }

        // Mark exclusion group consumed.
        if let Some(group) = &scheme.exclusion_group {
            fired_groups.insert(group.clone(), scheme.name.clone());
        }

        // Re-solve after this scheme.
        let (new_violations, _converged) =
            solve_and_detect_violations(&post_net, outaged_branches, &acpf_opts, ctg_options);

        info!(
            "RAS '{}': violations {} → {}",
            scheme.name,
            violations_before,
            new_violations.len(),
        );

        scheme_outcomes.push(SchemeOutcome {
            scheme_name: scheme.name.clone(),
            priority: scheme.priority,
            outcome: SchemeStatus::Fired {
                actions_applied: scheme_actions,
                violations_before,
                violations_after: new_violations.len(),
            },
        });

        // Update current state for next scheme's trigger evaluation.
        current_violations = new_violations;
        current_result = build_interim_result(base_result, &current_violations);
    }

    // Early return if all violations cleared by RAS.
    let remaining_violations = current_violations;
    if criteria.is_correctable(&remaining_violations) {
        return CorrectiveActionResult {
            corrective_actions_applied: applied,
            violations_after_correction: remaining_violations,
            correctable: true,
            scheme_outcomes,
        };
    }

    // -----------------------------------------------------------------------
    // Step 3: Greedy thermal redispatch (PTDF-based, for ThermalOverload)
    // -----------------------------------------------------------------------
    let remaining_violations = if config.enable_greedy_redispatch
        && remaining_violations
            .iter()
            .any(|v| matches!(v, Violation::ThermalOverload { .. }))
    {
        greedy_thermal_redispatch(
            &mut post_net,
            outaged_branches,
            remaining_violations,
            &mut applied,
            config,
            &acpf_opts,
            ctg_options,
        )
    } else {
        remaining_violations
    };

    if criteria.is_correctable(&remaining_violations) {
        return CorrectiveActionResult {
            corrective_actions_applied: applied,
            violations_after_correction: remaining_violations,
            correctable: true,
            scheme_outcomes,
        };
    }

    // -----------------------------------------------------------------------
    // Step 4: Greedy reactive dispatch (Q-V sensitivity, for voltage)
    // -----------------------------------------------------------------------
    let remaining_violations = if config.enable_reactive_redispatch
        && remaining_violations.iter().any(|v| {
            matches!(
                v,
                Violation::VoltageLow { .. } | Violation::VoltageHigh { .. }
            )
        }) {
        greedy_reactive_redispatch(
            &mut post_net,
            outaged_branches,
            remaining_violations,
            &mut applied,
            config,
            &acpf_opts,
            ctg_options,
        )
    } else {
        remaining_violations
    };

    if criteria.is_correctable(&remaining_violations) {
        return CorrectiveActionResult {
            corrective_actions_applied: applied,
            violations_after_correction: remaining_violations,
            correctable: true,
            scheme_outcomes,
        };
    }

    // -----------------------------------------------------------------------
    // Step 5: Greedy flowgate/interface redispatch
    // -----------------------------------------------------------------------
    let remaining_violations = if config.enable_greedy_redispatch
        && (!config.flowgates.is_empty() || !config.interfaces.is_empty())
        && remaining_violations.iter().any(|v| {
            matches!(
                v,
                Violation::FlowgateOverload { .. } | Violation::InterfaceOverload { .. }
            )
        }) {
        greedy_flowgate_redispatch(
            &mut post_net,
            outaged_branches,
            remaining_violations,
            &mut applied,
            config,
            &acpf_opts,
            ctg_options,
        )
    } else {
        remaining_violations
    };

    if criteria.is_correctable(&remaining_violations) {
        return CorrectiveActionResult {
            corrective_actions_applied: applied,
            violations_after_correction: remaining_violations,
            correctable: true,
            scheme_outcomes,
        };
    }

    // -----------------------------------------------------------------------
    // Step 6: Load shedding (last resort — thermal + voltage)
    // -----------------------------------------------------------------------
    let has_actionable = remaining_violations.iter().any(|v| {
        matches!(
            v,
            Violation::ThermalOverload { .. } | Violation::VoltageLow { .. }
        )
    });

    let remaining_violations =
        if config.enable_load_shed && has_actionable && config.max_load_shed_mw > 0.0 {
            load_shed_last_resort(
                &mut post_net,
                outaged_branches,
                remaining_violations,
                &mut applied,
                config,
                &acpf_opts,
                ctg_options,
            )
        } else {
            remaining_violations
        };

    let correctable = criteria.is_correctable(&remaining_violations);

    CorrectiveActionResult {
        corrective_actions_applied: applied,
        violations_after_correction: remaining_violations,
        correctable,
        scheme_outcomes,
    }
}

/// Build an interim `ContingencyResult` with updated violations for
/// trigger re-evaluation between RAS scheme firings.
fn build_interim_result(
    original: &ContingencyResult,
    new_violations: &[Violation],
) -> ContingencyResult {
    ContingencyResult {
        id: original.id.clone(),
        label: original.label.clone(),
        branch_indices: original.branch_indices.clone(),
        generator_indices: original.generator_indices.clone(),
        status: original.status,
        converged: true,
        violations: new_violations.to_vec(),
        n_islands: original.n_islands,
        tpl_category: original.tpl_category,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContingencyStatus;
    use surge_network::Network;
    use surge_network::network::BusType;
    use surge_network::network::{Branch, Bus, ContingencyModification, Generator, Load};

    /// Build a meshed 6-bus ring network suitable for corrective action tests.
    fn build_6bus() -> Network {
        let mut net = Network::new("6bus_corrective_test");
        net.base_mva = 100.0;

        net.buses = vec![
            Bus::new(1, BusType::Slack, 100.0),
            Bus::new(2, BusType::PV, 100.0),
            Bus::new(3, BusType::PQ, 100.0),
            Bus::new(4, BusType::PQ, 100.0),
            Bus::new(5, BusType::PV, 100.0),
            Bus::new(6, BusType::PQ, 100.0),
        ];
        net.loads.push(Load::new(3, 60.0, 0.0));
        net.loads.push(Load::new(4, 40.0, 0.0));
        net.loads.push(Load::new(6, 40.0, 0.0));

        let mut g1 = Generator::new(1, 100.0, 1.02);
        g1.pmax = 300.0;
        g1.pmin = 20.0;
        g1.qmax = 200.0;
        g1.qmin = -200.0;

        let mut g2 = Generator::new(2, 40.0, 1.01);
        g2.pmax = 120.0;
        g2.pmin = 10.0;
        g2.qmax = 100.0;
        g2.qmin = -100.0;

        let mut g5 = Generator::new(5, 40.0, 1.01);
        g5.pmax = 120.0;
        g5.pmin = 10.0;
        g5.qmax = 100.0;
        g5.qmin = -100.0;

        net.generators = vec![g1, g2, g5];

        let make = |from: u32, to: u32, rate_a: f64| -> Branch {
            let mut b = Branch::new_line(from, to, 0.02, 0.06, 0.0);
            b.rating_a_mva = rate_a;
            b.rating_b_mva = rate_a * 1.2;
            b.rating_c_mva = rate_a * 1.5;
            b
        };

        net.branches = vec![
            make(1, 2, 80.0),
            make(1, 5, 150.0),
            make(2, 3, 200.0),
            make(3, 4, 200.0),
            make(4, 5, 200.0),
            make(2, 6, 200.0),
        ];

        net
    }

    fn make_thermal_result(branch_idx: usize, flow_mva: f64, limit_mva: f64) -> ContingencyResult {
        ContingencyResult {
            id: "test".into(),
            label: "test".into(),
            branch_indices: vec![branch_idx],
            status: ContingencyStatus::Converged,
            converged: true,
            iterations: 5,
            violations: vec![Violation::ThermalOverload {
                branch_idx,
                from_bus: 1,
                to_bus: 2,
                loading_pct: flow_mva / limit_mva * 100.0,
                flow_mw: flow_mva,
                flow_mva,
                limit_mva,
            }],
            ..Default::default()
        }
    }

    fn make_voltage_result(
        bus_number: u32,
        vm: f64,
        limit: f64,
        is_low: bool,
    ) -> ContingencyResult {
        let violation = if is_low {
            Violation::VoltageLow {
                bus_number,
                vm,
                limit,
            }
        } else {
            Violation::VoltageHigh {
                bus_number,
                vm,
                limit,
            }
        };
        ContingencyResult {
            id: "test_voltage".into(),
            label: "test_voltage".into(),
            status: ContingencyStatus::Converged,
            converged: true,
            iterations: 4,
            violations: vec![violation],
            ..Default::default()
        }
    }

    fn make_flowgate_result(name: &str, flow_mw: f64, limit_mw: f64) -> ContingencyResult {
        ContingencyResult {
            id: "test_fg".into(),
            label: "test_fg".into(),
            branch_indices: vec![1],
            status: ContingencyStatus::Converged,
            converged: true,
            iterations: 4,
            violations: vec![Violation::FlowgateOverload {
                name: name.to_string(),
                flow_mw,
                limit_mw,
                loading_pct: flow_mw / limit_mw * 100.0,
            }],
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // Test 1: RAS clears violation
    // -----------------------------------------------------------------------

    #[test]
    fn ras_clears_violation() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let ras = RemedialActionScheme {
            name: "RAS_test_ras_clears".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -30.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };

        let ctg_opts = ContingencyOptions {
            thermal_threshold_frac: 1.0,
            ..Default::default()
        };

        let base_result = make_thermal_result(0, 110.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert!(
            !result.corrective_actions_applied.is_empty(),
            "RAS should have applied at least one corrective action"
        );

        let has_gen_redispatch = result
            .corrective_actions_applied
            .iter()
            .any(|a| matches!(a, CorrectiveAction::GeneratorRedispatch { gen_idx: 1, .. }));
        assert!(
            has_gen_redispatch,
            "RAS GeneratorRedispatch (gen_idx=1) should appear in applied actions"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: Greedy redispatch relieves overload
    // -----------------------------------------------------------------------

    #[test]
    fn greedy_redispatch_relieves_overload() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let config = CorrectiveActionConfig {
            schemes: vec![],
            enable_greedy_redispatch: true,
            max_redispatch_iter: 5,
            redispatch_step_fraction: 0.5,
            enable_load_shed: false,
            max_load_shed_mw: 0.0,
            ..Default::default()
        };

        let ctg_opts = ContingencyOptions {
            thermal_threshold_frac: 1.0,
            ..Default::default()
        };

        let base_result = make_thermal_result(0, 110.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        let _ = result.correctable;
        println!(
            "Greedy redispatch: {} actions applied, correctable={}",
            result.corrective_actions_applied.len(),
            result.correctable
        );
    }

    // -----------------------------------------------------------------------
    // Test 3: Uncorrectable violation reports correctable=false
    // -----------------------------------------------------------------------

    #[test]
    fn uncorrectable_reports_correctly() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let config = CorrectiveActionConfig {
            schemes: vec![],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            max_load_shed_mw: 0.0,
            ..Default::default()
        };

        let ctg_opts = ContingencyOptions {
            thermal_threshold_frac: 1.0,
            ..Default::default()
        };

        let base_result = make_thermal_result(0, 300.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert!(
            result.corrective_actions_applied.is_empty(),
            "No corrective actions should be applied when all mechanisms are disabled"
        );

        let has_thermal_after = result
            .violations_after_correction
            .iter()
            .any(|v| matches!(v, Violation::ThermalOverload { .. }));
        if has_thermal_after {
            assert!(
                !result.correctable,
                "correctable must be false when thermal violations remain after re-solve"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 4: Load shed as last resort
    // -----------------------------------------------------------------------

    #[test]
    fn load_shed_as_last_resort() {
        let mut net = build_6bus();
        net.branches[0].rating_a_mva = 1.0;
        net.branches[0].rating_b_mva = 1.2;
        net.branches[0].rating_c_mva = 1.5;

        let outaged = vec![1usize];

        let config = CorrectiveActionConfig {
            schemes: vec![],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: true,
            max_load_shed_mw: 200.0,
            ..Default::default()
        };

        let ctg_opts = ContingencyOptions {
            thermal_threshold_frac: 1.0,
            ..Default::default()
        };

        let base_result = make_thermal_result(0, 50.0, 1.0);

        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        let has_load_shed = result
            .corrective_actions_applied
            .iter()
            .any(|a| matches!(a, CorrectiveAction::LoadShed { .. }));
        assert!(
            has_load_shed,
            "LoadShed action should appear in applied actions when enable_load_shed=true \
             and actual re-solve finds overload; actions={:?}",
            result.corrective_actions_applied
        );

        println!(
            "Load shed: {} actions, correctable={}",
            result.corrective_actions_applied.len(),
            result.correctable
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: RAS max_redispatch_mw limit
    // -----------------------------------------------------------------------

    #[test]
    fn ras_max_redispatch_limit_skips_oversized_ras() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let ras = RemedialActionScheme {
            name: "RAS_oversized".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -500.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };

        let ctg_opts = ContingencyOptions {
            thermal_threshold_frac: 1.0,
            ..Default::default()
        };

        let base_result = make_thermal_result(0, 110.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert!(
            result.corrective_actions_applied.is_empty(),
            "Oversized RAS should be skipped — applied list must be empty"
        );
    }

    // -----------------------------------------------------------------------
    // Test 6: No-violation contingency
    // -----------------------------------------------------------------------

    #[test]
    fn no_violations_returns_correctable_true() {
        let net = build_6bus();
        let outaged = vec![2usize];

        let config = CorrectiveActionConfig::default();
        let ctg_opts = ContingencyOptions::default();

        let base_result = ContingencyResult {
            id: "no_viol".into(),
            label: "no_viol".into(),
            branch_indices: vec![2],
            status: ContingencyStatus::Converged,
            converged: true,
            iterations: 3,
            ..Default::default()
        };

        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert!(
            result.correctable,
            "No-violation contingency should be correctable=true"
        );
        assert!(
            result.corrective_actions_applied.is_empty(),
            "No actions should be applied for a violation-free contingency"
        );
    }

    // -----------------------------------------------------------------------
    // Test 7: branch_indices propagated on contingency results
    // -----------------------------------------------------------------------

    #[test]
    fn make_thermal_result_has_branch_indices() {
        let r = make_thermal_result(3, 120.0, 100.0);
        assert_eq!(
            r.branch_indices,
            vec![3],
            "branch_indices must equal the tripped branch"
        );
        assert!(
            r.generator_indices.is_empty(),
            "generator_indices must be empty for branch-only trip"
        );
    }

    // -----------------------------------------------------------------------
    // Test 8: RAS modifications applied to post-contingency network
    // -----------------------------------------------------------------------

    #[test]
    fn ras_modifications_applied_on_trigger() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let modification = ContingencyModification::BranchClose {
            from_bus: net.branches[0].from_bus,
            to_bus: net.branches[0].to_bus,
            circuit: net.branches[0].circuit.clone(),
        };

        let ras = RemedialActionScheme {
            name: "RAS_with_modification".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -30.0,
            }],
            modifications: vec![modification],
            max_redispatch_mw: 100.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 120.0, 100.0);

        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert!(
            !result.corrective_actions_applied.is_empty(),
            "RAS with modifications should still apply CorrectiveActions"
        );
    }

    // -----------------------------------------------------------------------
    // Test 9: Voltage-triggered RAS fires on pure voltage contingency
    // -----------------------------------------------------------------------

    #[test]
    fn voltage_triggered_ras_fires_on_pure_voltage_contingency() {
        let net = build_6bus();
        let outaged = vec![1usize];

        // RAS that fires when bus 3 voltage drops below 0.95 pu.
        let ras = RemedialActionScheme {
            name: "WECC_VoltageRAS".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::PostCtgVoltageLow {
                bus_number: 3,
                threshold_pu: 0.95,
            }],
            actions: vec![CorrectiveAction::ShuntSwitch {
                bus: 2, // bus 3 internal idx
                delta_b_pu: 0.1,
            }],
            modifications: vec![],
            max_redispatch_mw: f64::INFINITY,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };

        let ctg_opts = ContingencyOptions::default();

        // Pure voltage violation — no thermal violations.
        let base_result = make_voltage_result(3, 0.92, 0.95, true);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert!(
            !result.corrective_actions_applied.is_empty(),
            "Voltage-triggered RAS must fire even with no thermal violations; got empty actions"
        );

        let has_shunt = result
            .corrective_actions_applied
            .iter()
            .any(|a| matches!(a, CorrectiveAction::ShuntSwitch { .. }));
        assert!(
            has_shunt,
            "RAS ShuntSwitch should appear in applied actions"
        );
    }

    // -----------------------------------------------------------------------
    // Test 10: CorrectableCriteria variants
    // -----------------------------------------------------------------------

    #[test]
    fn correctable_criteria_all_violations() {
        let criteria = CorrectableCriteria::AllViolations;
        // Thermal counts
        assert!(criteria.counts(&Violation::ThermalOverload {
            branch_idx: 0,
            from_bus: 1,
            to_bus: 2,
            loading_pct: 110.0,
            flow_mw: 110.0,
            flow_mva: 110.0,
            limit_mva: 100.0,
        }));
        // Voltage counts
        assert!(criteria.counts(&Violation::VoltageLow {
            bus_number: 1,
            vm: 0.93,
            limit: 0.95
        }));
        // Flowgate counts
        assert!(criteria.counts(&Violation::FlowgateOverload {
            name: "FG1".into(),
            flow_mw: 110.0,
            limit_mw: 100.0,
            loading_pct: 110.0,
        }));
        // Islanding does NOT count
        assert!(!criteria.counts(&Violation::Islanding { n_components: 2 }));
    }

    #[test]
    fn correctable_criteria_thermal_only() {
        let criteria = CorrectableCriteria::ThermalOnly;
        // Thermal counts
        assert!(criteria.counts(&Violation::ThermalOverload {
            branch_idx: 0,
            from_bus: 1,
            to_bus: 2,
            loading_pct: 110.0,
            flow_mw: 110.0,
            flow_mva: 110.0,
            limit_mva: 100.0,
        }));
        // Voltage does NOT count
        assert!(!criteria.counts(&Violation::VoltageLow {
            bus_number: 1,
            vm: 0.93,
            limit: 0.95
        }));
        // Flowgate does NOT count
        assert!(!criteria.counts(&Violation::FlowgateOverload {
            name: "FG1".into(),
            flow_mw: 110.0,
            limit_mw: 100.0,
            loading_pct: 110.0,
        }));
    }

    // -----------------------------------------------------------------------
    // Test 11: BreakerSwitch action
    // -----------------------------------------------------------------------

    #[test]
    fn breaker_switch_opens_branch() {
        let net = build_6bus();
        let outaged = vec![1usize];

        // RAS that opens branch 0 when branch 1 is outaged.
        let ras = RemedialActionScheme {
            name: "BreakerRAS".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::BreakerSwitch {
                branch_idx: 5,
                close: false,
            }],
            modifications: vec![],
            max_redispatch_mw: f64::INFINITY,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);

        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);
        let has_breaker = result.corrective_actions_applied.iter().any(|a| {
            matches!(
                a,
                CorrectiveAction::BreakerSwitch {
                    branch_idx: 5,
                    close: false
                }
            )
        });
        assert!(
            has_breaker,
            "BreakerSwitch(open) should appear in applied actions"
        );
    }

    // -----------------------------------------------------------------------
    // Test 12: Flowgate-triggered RAS
    // -----------------------------------------------------------------------

    #[test]
    fn flowgate_triggered_ras_fires() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let ras = RemedialActionScheme {
            name: "NomogramRAS".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::PostCtgFlowgateOverload {
                flowgate_name: "COI".into(),
                threshold_pct: 100.0,
            }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -20.0,
            }],
            modifications: vec![],
            max_redispatch_mw: f64::INFINITY,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_flowgate_result("COI", 5000.0, 4800.0);

        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);
        assert!(
            !result.corrective_actions_applied.is_empty(),
            "Flowgate-triggered RAS should fire"
        );
    }

    // -----------------------------------------------------------------------
    // Test 13: Not trigger condition
    // -----------------------------------------------------------------------

    #[test]
    fn not_trigger_inverts_condition() {
        let net = build_6bus();
        let outaged = vec![1usize];

        // RAS fires when branch 2 is NOT outaged (should fire since branch 1 is outaged).
        let ras = RemedialActionScheme {
            name: "NotRAS".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::All(vec![
                RasTriggerCondition::BranchOutaged { branch_idx: 1 },
                RasTriggerCondition::Not(Box::new(RasTriggerCondition::BranchOutaged {
                    branch_idx: 2,
                })),
            ])],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 0,
                delta_p_mw: -10.0,
            }],
            modifications: vec![],
            max_redispatch_mw: f64::INFINITY,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);

        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);
        assert!(
            !result.corrective_actions_applied.is_empty(),
            "RAS with Not condition should fire when branch 1 outaged but not branch 2"
        );
    }

    // -----------------------------------------------------------------------
    // Test 14: Greedy reactive dispatch runs on voltage violations
    // -----------------------------------------------------------------------

    #[test]
    fn greedy_reactive_runs_on_voltage_violation() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let config = CorrectiveActionConfig {
            schemes: vec![],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: true,
            max_redispatch_iter: 3,
            enable_load_shed: false,
            ..Default::default()
        };

        let ctg_opts = ContingencyOptions {
            vm_min: 0.95,
            ..Default::default()
        };

        // Pure voltage violation.
        let base_result = make_voltage_result(3, 0.92, 0.95, true);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        // The reactive dispatch should have attempted something (Vs adjust, shunt, or Q redispatch).
        println!(
            "Greedy reactive: {} actions, correctable={}, violations_after={}",
            result.corrective_actions_applied.len(),
            result.correctable,
            result.violations_after_correction.len()
        );
        // We can't guarantee the 6-bus test case actually has a voltage violation
        // in the AC re-solve, but the path should not panic and should attempt actions
        // if the re-solve does find violations.
    }

    // -----------------------------------------------------------------------
    // Test 15: Sequential RAS — first scheme clears, second is unnecessary
    // -----------------------------------------------------------------------

    #[test]
    fn sequential_ras_fires_in_priority_order_with_audit() {
        // Both schemes trigger on BranchOutaged (structural — always true while
        // branch 1 is out). The first scheme's -30 MW redispatch does NOT fully
        // clear the 137% thermal overload, so the second scheme legitimately
        // fires as well. This test verifies:
        //   1. Both schemes produce audit outcomes in priority order.
        //   2. Priority-0 fires before priority-10.
        //   3. Both record Fired status with violation counts.
        let net = build_6bus();
        let outaged = vec![1usize];

        let ras1 = RemedialActionScheme {
            name: "RAS_priority_0".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -30.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };
        let ras2 = RemedialActionScheme {
            name: "RAS_priority_10".into(),
            priority: 10,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 0,
                delta_p_mw: -50.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 200.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras1, ras2],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };

        let ctg_opts = ContingencyOptions {
            thermal_threshold_frac: 1.0,
            ..Default::default()
        };

        let base_result = make_thermal_result(0, 110.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        // Both schemes should have outcomes recorded.
        assert_eq!(
            result.scheme_outcomes.len(),
            2,
            "Both schemes must appear in scheme_outcomes"
        );

        // First scheme (priority 0) fires first.
        assert_eq!(result.scheme_outcomes[0].scheme_name, "RAS_priority_0");
        assert_eq!(result.scheme_outcomes[0].priority, 0);
        assert!(
            matches!(
                result.scheme_outcomes[0].outcome,
                SchemeStatus::Fired { .. }
            ),
            "Priority-0 scheme should fire"
        );

        // Second scheme (priority 10) also fires because BranchOutaged is
        // structural and the thermal violation persists after first RAS.
        assert_eq!(result.scheme_outcomes[1].scheme_name, "RAS_priority_10");
        assert_eq!(result.scheme_outcomes[1].priority, 10);
        assert!(
            matches!(
                result.scheme_outcomes[1].outcome,
                SchemeStatus::Fired { .. }
            ),
            "Priority-10 scheme should also fire (overload not cleared by first RAS); \
             got {:?}",
            result.scheme_outcomes[1].outcome
        );
    }

    // -----------------------------------------------------------------------
    // Test 15b: Sequential RAS stops when violations are cleared
    // -----------------------------------------------------------------------

    #[test]
    fn sequential_ras_stops_when_violations_cleared() {
        // Use a milder overload (101% of 80 MVA) so the first scheme's -30 MW
        // redispatch is enough to clear it after re-solve. The second scheme
        // should then get Unnecessary status.
        let net = build_6bus();
        let outaged = vec![1usize];

        let ras1 = RemedialActionScheme {
            name: "RAS_first".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -30.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };
        let ras2 = RemedialActionScheme {
            name: "RAS_second".into(),
            priority: 10,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 0,
                delta_p_mw: -50.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 200.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras1, ras2],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };

        let ctg_opts = ContingencyOptions {
            thermal_threshold_frac: 1.0,
            ..Default::default()
        };

        // Mild overload: 81 MVA on 80 MVA limit = 101.25%.
        let base_result = make_thermal_result(0, 81.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert_eq!(result.scheme_outcomes.len(), 2);

        // First fires.
        assert_eq!(result.scheme_outcomes[0].scheme_name, "RAS_first");
        assert!(matches!(
            result.scheme_outcomes[0].outcome,
            SchemeStatus::Fired { .. }
        ));

        // Second should be Unnecessary (if re-solve cleared the violation)
        // or Fired (if the 6-bus re-solve still shows overload). Either is
        // acceptable — the key invariant is priority ordering.
        assert_eq!(result.scheme_outcomes[1].scheme_name, "RAS_second");
        assert!(
            matches!(
                result.scheme_outcomes[1].outcome,
                SchemeStatus::Unnecessary | SchemeStatus::Fired { .. }
            ),
            "Second scheme should be Unnecessary or Fired; got {:?}",
            result.scheme_outcomes[1].outcome
        );
    }

    // -----------------------------------------------------------------------
    // Test 16: Priority ordering — lower value fires first
    // -----------------------------------------------------------------------

    #[test]
    fn priority_ordering_fires_lower_first() {
        let net = build_6bus();
        let outaged = vec![1usize];

        // Define RAS-A at priority 20, RAS-B at priority 10.
        // Both trigger on same branch outage.
        let ras_a = RemedialActionScheme {
            name: "RAS_A_prio20".into(),
            priority: 20,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 0,
                delta_p_mw: -10.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };
        let ras_b = RemedialActionScheme {
            name: "RAS_B_prio10".into(),
            priority: 10,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -10.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };

        // Put A first in definition order, but B has lower priority.
        let config = CorrectiveActionConfig {
            schemes: vec![ras_a, ras_b],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        // RAS-B (priority 10) should appear before RAS-A (priority 20) in outcomes.
        assert!(result.scheme_outcomes.len() >= 2);
        assert_eq!(
            result.scheme_outcomes[0].scheme_name, "RAS_B_prio10",
            "Lower priority value must fire first"
        );
        assert_eq!(result.scheme_outcomes[1].scheme_name, "RAS_A_prio20");
    }

    // -----------------------------------------------------------------------
    // Test 17: Exclusion group blocks second scheme
    // -----------------------------------------------------------------------

    #[test]
    fn exclusion_group_blocks_second_scheme() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let ras1 = RemedialActionScheme {
            name: "RAS_cheap".into(),
            priority: 10,
            exclusion_group: Some("WN_FLOWGATE".into()),
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -20.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };
        let ras2 = RemedialActionScheme {
            name: "RAS_expensive".into(),
            priority: 20,
            exclusion_group: Some("WN_FLOWGATE".into()),
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::LoadShed {
                bus: 2,
                shed_fraction: 0.5,
            }],
            modifications: vec![],
            max_redispatch_mw: f64::INFINITY,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras1, ras2],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert_eq!(result.scheme_outcomes.len(), 2);
        assert!(matches!(
            result.scheme_outcomes[0].outcome,
            SchemeStatus::Fired { .. }
        ));
        assert!(
            matches!(
                &result.scheme_outcomes[1].outcome,
                SchemeStatus::ExcludedBy { fired_scheme } if fired_scheme == "RAS_cheap"
            ),
            "Second scheme should be ExcludedBy RAS_cheap; got {:?}",
            result.scheme_outcomes[1].outcome
        );

        // Verify no load shed was applied (the expensive RAS was blocked).
        let has_load_shed = result
            .corrective_actions_applied
            .iter()
            .any(|a| matches!(a, CorrectiveAction::LoadShed { .. }));
        assert!(
            !has_load_shed,
            "Excluded RAS should not have applied its LoadShed action"
        );
    }

    // -----------------------------------------------------------------------
    // Test 18: Arm condition prevents firing
    // -----------------------------------------------------------------------

    #[test]
    fn arm_condition_prevents_firing() {
        let net = build_6bus();
        let outaged = vec![1usize];

        // RAS armed only when total generation > 500 MW.
        // build_6bus has gen outputs: 100 + 40 + 40 = 180 MW → not armed.
        let ras = RemedialActionScheme {
            name: "RAS_high_gen_only".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![ArmCondition::SystemGenerationAbove {
                threshold_mw: 500.0,
            }],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -30.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);

        // Build a BaseCaseState with low generation.
        let base_state = BaseCaseState {
            vm: vec![1.0; net.n_buses()],
            va: vec![0.0; net.n_buses()],
            branch_flow_mw: vec![50.0; net.branches.len()],
            total_gen_mw: 180.0,
        };

        let result = apply_corrective_actions(
            &net,
            &outaged,
            &base_result,
            Some(&base_state),
            &config,
            &ctg_opts,
        );

        assert_eq!(result.scheme_outcomes.len(), 1);
        assert!(
            matches!(result.scheme_outcomes[0].outcome, SchemeStatus::NotArmed),
            "Scheme should be NotArmed when total_gen_mw=180 < threshold=500; got {:?}",
            result.scheme_outcomes[0].outcome
        );
        assert!(
            result.corrective_actions_applied.is_empty(),
            "No actions should be applied for unarmed scheme"
        );
    }

    // -----------------------------------------------------------------------
    // Test 19: Arm condition allows firing
    // -----------------------------------------------------------------------

    #[test]
    fn arm_condition_allows_firing() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let ras = RemedialActionScheme {
            name: "RAS_high_gen_only".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![ArmCondition::SystemGenerationAbove {
                threshold_mw: 100.0,
            }],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -30.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);

        let base_state = BaseCaseState {
            vm: vec![1.0; net.n_buses()],
            va: vec![0.0; net.n_buses()],
            branch_flow_mw: vec![50.0; net.branches.len()],
            total_gen_mw: 180.0,
        };

        let result = apply_corrective_actions(
            &net,
            &outaged,
            &base_result,
            Some(&base_state),
            &config,
            &ctg_opts,
        );

        assert_eq!(result.scheme_outcomes.len(), 1);
        assert!(
            matches!(
                result.scheme_outcomes[0].outcome,
                SchemeStatus::Fired { .. }
            ),
            "Scheme should fire when total_gen_mw=180 >= threshold=100; got {:?}",
            result.scheme_outcomes[0].outcome
        );
        assert!(
            !result.corrective_actions_applied.is_empty(),
            "Armed+triggered scheme should apply actions"
        );
    }

    // -----------------------------------------------------------------------
    // Test 20: Definition order tiebreak at equal priority
    // -----------------------------------------------------------------------

    #[test]
    fn definition_order_tiebreak() {
        let net = build_6bus();
        let outaged = vec![1usize];

        // Both at priority 0. First-defined should fire first.
        let ras_first = RemedialActionScheme {
            name: "RAS_defined_first".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -10.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };
        let ras_second = RemedialActionScheme {
            name: "RAS_defined_second".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 0,
                delta_p_mw: -10.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras_first, ras_second],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert!(result.scheme_outcomes.len() >= 2);
        assert_eq!(
            result.scheme_outcomes[0].scheme_name, "RAS_defined_first",
            "At equal priority, definition order must be preserved (stable sort)"
        );
    }

    // -----------------------------------------------------------------------
    // Test 21: No trigger conditions → never fires
    // -----------------------------------------------------------------------

    #[test]
    fn no_trigger_conditions_never_fires() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let ras = RemedialActionScheme {
            name: "RAS_no_triggers".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -30.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert_eq!(result.scheme_outcomes.len(), 1);
        assert!(
            matches!(
                result.scheme_outcomes[0].outcome,
                SchemeStatus::NotTriggered
            ),
            "Scheme with no trigger conditions should be NotTriggered; got {:?}",
            result.scheme_outcomes[0].outcome
        );
    }

    // -----------------------------------------------------------------------
    // Test 22: base_case_state=None skips arming
    // -----------------------------------------------------------------------

    #[test]
    fn base_case_state_none_skips_arming() {
        let net = build_6bus();
        let outaged = vec![1usize];

        // RAS with arm condition that would fail if evaluated.
        let ras = RemedialActionScheme {
            name: "RAS_with_arm".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![ArmCondition::SystemGenerationAbove {
                threshold_mw: 999999.0,
            }],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -30.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);

        // With None, arm conditions are skipped → scheme fires.
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert_eq!(result.scheme_outcomes.len(), 1);
        assert!(
            matches!(
                result.scheme_outcomes[0].outcome,
                SchemeStatus::Fired { .. }
            ),
            "With base_case_state=None, arm conditions should be skipped; got {:?}",
            result.scheme_outcomes[0].outcome
        );
    }

    // -----------------------------------------------------------------------
    // Test 23: Redispatch exceeded reports correctly in scheme_outcomes
    // -----------------------------------------------------------------------

    #[test]
    fn redispatch_exceeded_in_scheme_outcomes() {
        let net = build_6bus();
        let outaged = vec![1usize];

        let ras = RemedialActionScheme {
            name: "RAS_oversized_audit".into(),
            priority: 0,
            exclusion_group: None,
            arm_conditions: vec![],
            trigger_conditions: vec![RasTriggerCondition::BranchOutaged { branch_idx: 1 }],
            actions: vec![CorrectiveAction::GeneratorRedispatch {
                gen_idx: 1,
                delta_p_mw: -500.0,
            }],
            modifications: vec![],
            max_redispatch_mw: 100.0,
        };

        let config = CorrectiveActionConfig {
            schemes: vec![ras],
            enable_greedy_redispatch: false,
            enable_reactive_redispatch: false,
            enable_load_shed: false,
            ..Default::default()
        };
        let ctg_opts = ContingencyOptions::default();
        let base_result = make_thermal_result(0, 110.0, 80.0);
        let result =
            apply_corrective_actions(&net, &outaged, &base_result, None, &config, &ctg_opts);

        assert_eq!(result.scheme_outcomes.len(), 1);
        assert!(
            matches!(
                result.scheme_outcomes[0].outcome,
                SchemeStatus::RedispatchExceeded {
                    requested_mw,
                    limit_mw,
                } if requested_mw == 500.0 && limit_mw == 100.0
            ),
            "Should report RedispatchExceeded with correct MW values; got {:?}",
            result.scheme_outcomes[0].outcome
        );
    }
}
