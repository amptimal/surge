// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical AC OPF preset builders and retry-attempt templates.
//!
//! The AC SCED stage of a two-stage market workflow needs three things
//! baked into `runtime.ac_opf`:
//!
//! 1. Validator-aligned bus balance slack penalties derived from the
//!    problem's violation-cost primitives plus a safety multiplier (see
//!    [`validator_aligned_bus_balance_penalties`]).
//! 2. An [`AcOpfOptions`] preset shaped for AC SCED convergence —
//!    tap/phase-shifter optimisation off, switched shunts on with
//!    discrete rounding, long max-iterations budget, exact Hessian (see
//!    [`AcOpfSceduledBaseline`]).
//! 3. A retry ladder with progressively harder attempts (soft → strict
//!    bus balance → no thermal limits) so the runtime can recover from
//!    the NLP landing on a high-penalty solution on the first try (see
//!    [`standard_opf_retry_attempts`]).
//!
//! These patterns are not GO-C3-specific; any two-stage DC→AC market
//! formulation benefits from the same three ingredients. Adapters
//! supply the raw violation-cost numbers and a safety multiplier; the
//! canonical builders here do the rest.

use surge_opf::AcOpfOptions;
use surge_opf::ac::types::DiscreteMode;

use crate::OpfAttempt;

/// Raw violation-cost primitives plus the safety multiplier used to
/// derive the AC SCED stage's bus-balance slack penalties.
///
/// Adapters pass raw `$/pu-h` values straight from the problem's
/// violation-cost block; the canonical formula scales by `1 / base_mva`
/// and multiplies by `safety_multiplier` so the NLP strictly prefers
/// physical relief over absorbing imbalance into bus-balance slack.
///
/// The `safety_multiplier_baseline` field lets adapters express the
/// fallback penalties (used when the violation-cost block is missing)
/// in a scale-invariant way: the static fallbacks are multiplied by
/// `safety_multiplier / safety_multiplier_baseline` so lowering the
/// multiplier also loosens the fallback.
#[derive(Debug, Clone)]
pub struct BusBalancePenaltyInputs {
    /// Active-power bus balance violation cost in $/pu-h (typically
    /// `violation_cost.p_bus_vio_cost`). `None` treats the violation
    /// cost block as missing and returns the static fallback.
    pub p_bus_per_pu: Option<f64>,
    /// Reactive-power bus balance violation cost in $/pu-h. `None` or
    /// non-positive falls back to `p_bus_per_pu`.
    pub q_bus_per_pu: Option<f64>,
    /// Network base MVA. `<= 0` is treated as missing and triggers the
    /// static fallback.
    pub base_mva: f64,
    /// Applied multiplier on the per-pu penalty. Typical range 1–100.
    pub safety_multiplier: f64,
    /// Reference multiplier for scaling the static fallback. Static
    /// fallbacks are multiplied by `safety_multiplier /
    /// safety_multiplier_baseline`.
    pub safety_multiplier_baseline: f64,
    /// Static fallback for the active bus balance penalty in $/MW-h
    /// (scaled by the baseline ratio). Used when the violation-cost
    /// block is missing.
    pub fallback_p_mw: f64,
    /// Static fallback for the reactive bus balance penalty in
    /// $/MVAr-h (scaled by the baseline ratio).
    pub fallback_q_mvar: f64,
    /// Fallback for the raw per-pu active cost when the violation block
    /// is present but carries a non-positive `p_bus_per_pu`. Keeps the
    /// slack usefully expensive even when the upstream file lies about
    /// its own violation costs.
    pub p_per_pu_fallback: f64,
}

/// Per-scenario `(p $/MW, q $/MVAr)` bus-balance slack penalties.
///
/// Applies a safety-multiplier scaling to the adapter-supplied
/// per-pu violation costs (or to the fallback MW/MVAr values when
/// the per-pu inputs are missing or unusable).
pub fn validator_aligned_bus_balance_penalties(inputs: &BusBalancePenaltyInputs) -> (f64, f64) {
    let scale = if inputs.safety_multiplier_baseline > 0.0 {
        inputs.safety_multiplier / inputs.safety_multiplier_baseline
    } else {
        1.0
    };
    if inputs.base_mva <= 0.0 {
        return (inputs.fallback_p_mw * scale, inputs.fallback_q_mvar * scale);
    }
    let Some(raw_p) = inputs.p_bus_per_pu else {
        return (inputs.fallback_p_mw * scale, inputs.fallback_q_mvar * scale);
    };
    let p_per_pu = if raw_p > 0.0 {
        raw_p
    } else {
        inputs.p_per_pu_fallback
    };
    let q_per_pu = match inputs.q_bus_per_pu {
        Some(v) if v > 0.0 => v,
        _ => p_per_pu,
    };
    (
        p_per_pu / inputs.base_mva * inputs.safety_multiplier,
        q_per_pu / inputs.base_mva * inputs.safety_multiplier,
    )
}

/// Opinionated AC SCED baseline — tap/phase-shifter optimisation
/// off, switched shunts on with [`DiscreteMode::RoundAndCheck`],
/// long max-iterations budget, exact Hessian.
///
/// Construct via [`AcOpfSceduledBaseline::tap_locked_shunts_on`] to
/// pick up the canonical shape, then tweak fields before calling
/// [`into_options`][Self::into_options].
#[derive(Debug, Clone)]
pub struct AcOpfSceduledBaseline {
    pub bus_balance_penalties: (f64, f64),
    pub thermal_penalty_per_mva: f64,
    pub max_iterations: u32,
    pub exact_hessian: bool,
    pub enforce_regulated_bus_vm_targets: bool,
    pub optimize_taps: bool,
    pub optimize_phase_shifters: bool,
    pub optimize_switched_shunts: bool,
    pub discrete_mode: DiscreteMode,
    pub enforce_thermal_limits: bool,
    /// When `Some`, overrides [`AcOpfOptions::tolerance`] after the
    /// baseline is materialized.
    pub tolerance: Option<f64>,
}

impl AcOpfSceduledBaseline {
    /// Canonical AC SCED preset: taps + phase shifters locked at
    /// their initial values, switched shunts optimised with
    /// [`DiscreteMode::RoundAndCheck`], thermal slack active at 5
    /// $/MVA-h, 3000-iter NLP budget with exact Hessian.
    pub fn tap_locked_shunts_on(bus_balance_penalties: (f64, f64)) -> Self {
        Self {
            bus_balance_penalties,
            thermal_penalty_per_mva: 5.0,
            max_iterations: 3_000,
            exact_hessian: true,
            enforce_regulated_bus_vm_targets: false,
            optimize_taps: true,
            optimize_phase_shifters: true,
            optimize_switched_shunts: true,
            discrete_mode: DiscreteMode::RoundAndCheck,
            enforce_thermal_limits: true,
            tolerance: None,
        }
    }

    /// Materialize into a concrete [`AcOpfOptions`] value ready for
    /// `runtime.ac_opf`.
    pub fn into_options(self) -> AcOpfOptions {
        let mut opts = AcOpfOptions {
            thermal_limit_slack_penalty_per_mva: self.thermal_penalty_per_mva,
            bus_active_power_balance_slack_penalty_per_mw: self.bus_balance_penalties.0,
            bus_reactive_power_balance_slack_penalty_per_mvar: self.bus_balance_penalties.1,
            max_iterations: self.max_iterations,
            exact_hessian: self.exact_hessian,
            enforce_regulated_bus_vm_targets: self.enforce_regulated_bus_vm_targets,
            optimize_taps: self.optimize_taps,
            optimize_phase_shifters: self.optimize_phase_shifters,
            optimize_switched_shunts: self.optimize_switched_shunts,
            discrete_mode: self.discrete_mode,
            enforce_thermal_limits: self.enforce_thermal_limits,
            ..AcOpfOptions::default()
        };
        if let Some(tol) = self.tolerance {
            opts.tolerance = tol;
        }
        opts
    }
}

/// Standard three-rung OPF retry template.
///
/// * Rung 1 — `soft_label`: use `base` as-is. The soft bus-balance
///   slack lets the NLP absorb imbalance into slack variables with a
///   (hopefully large) penalty.
/// * Rung 2 — `"strict_bus_balance"`: zero the bus-balance penalty so
///   the NLP must satisfy bus balance exactly. Harder to converge but
///   eliminates any residual slack absorption.
/// * Rung 3 — `"no_thermal_limits"`: drop thermal limits entirely.
///   Last-resort rung for scenarios where even strict bus balance
///   fails and the overload is the blocker.
///
/// `soft_label` is the attempt's diagnostic tag (appears in solve
/// logs); adapters typically pass their canonical name for this
/// rung.
pub fn standard_opf_retry_attempts(base: &AcOpfOptions, soft_label: &str) -> Vec<OpfAttempt> {
    let soft = base.clone();

    let mut strict = base.clone();
    strict.bus_active_power_balance_slack_penalty_per_mw = 0.0;
    strict.bus_reactive_power_balance_slack_penalty_per_mvar = 0.0;

    let mut no_thermal = base.clone();
    no_thermal.enforce_thermal_limits = false;
    no_thermal.thermal_limit_slack_penalty_per_mva = 0.0;

    vec![
        OpfAttempt::new(soft_label, Some(soft)),
        OpfAttempt::new("strict_bus_balance", Some(strict)),
        OpfAttempt::new("no_thermal_limits", Some(no_thermal)),
    ]
}
