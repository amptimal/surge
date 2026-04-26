// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 adapter presets.
//!
//! Reads GO-C3-specific `GoC3Problem` / `GoC3Context` / `GoC3Policy`
//! field names and produces the canonical primitives the two-stage
//! workflow builder consumes. Everything in this file is "GO C3
//! chooses X for Y canonical slot" — the canonical shape lives in the
//! parent crate's top-level modules, not here.

use std::collections::{HashMap, HashSet};

use surge_dispatch::ResourceCommitmentSchedule;
use surge_io::go_c3::types::GoC3DeviceType;
use surge_io::go_c3::{GoC3Context, GoC3DeviceKind, GoC3DeviceTimeSeries, GoC3Problem};
use surge_network::market::PenaltyConfig;
use surge_opf::AcOpfOptions;

use crate::ac_opf_presets::{
    AcOpfSceduledBaseline, BusBalancePenaltyInputs, validator_aligned_bus_balance_penalties,
};
use crate::ac_reconcile::RampLimits;
use crate::ac_refinement::{BandAttempt, HvdcAttempt, OpfAttempt, RetryPolicy};
use crate::ac_sced_setup::{DispatchPinningBands, ReserveProductIdSets};
use crate::heuristics::{
    BandableCriteria, MarketDeviceKind, ReactiveSupportPinCriteria, ResourceClassification,
    WideQAnchorCriteria,
};
use crate::penalties::{PenaltyInputs, build_penalty_config, voltage_piecewise_curve};

// ── Reserve product IDs ────────────────────────────────────────────────────

/// GO C3 reserve product IDs partitioned into up-active / down-active /
/// reactive sets.
///
/// Offline-only products (`nsyn`, `ramp_up_off`) are excluded from the
/// up-active set because they gate on `(1 − u^on)` in the SCUC adapter
/// and never bind a committed generator's upper headroom.
pub fn goc3_reserve_product_ids() -> ReserveProductIdSets {
    ReserveProductIdSets {
        up_active: ["reg_up", "syn", "ramp_up_on"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        down_active: ["reg_down", "ramp_down_on"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        reactive: ["q_res_up", "q_res_down"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

// ── Classification ─────────────────────────────────────────────────────────

/// Translate [`GoC3Context`] into the canonical [`ResourceClassification`].
pub fn goc3_classification(context: &GoC3Context) -> ResourceClassification {
    let kind_by_resource_id = context
        .device_kind_by_uid
        .iter()
        .map(|(rid, kind)| {
            let market_kind = match kind {
                GoC3DeviceKind::Producer => MarketDeviceKind::Producer,
                GoC3DeviceKind::ProducerStatic => MarketDeviceKind::ProducerStatic,
                GoC3DeviceKind::Consumer => MarketDeviceKind::Consumer,
            };
            (rid.clone(), market_kind)
        })
        .collect();
    ResourceClassification {
        kind_by_resource_id,
        slack_bus_numbers: context.slack_bus_numbers.iter().copied().collect(),
        consumer_blocks_by_uid: context.consumer_dispatchable_resource_ids_by_uid.clone(),
    }
}

// ── Canonical-criteria presets (magic numbers live here) ──────────────────

/// Canonical GO-C3 bandable criteria: include slack producers, bounded
/// non-slack additions with `p_range > 1e-6 MW`, `q_range > 0 MVAr`.
pub fn goc3_bandable_criteria(max_additional: usize) -> BandableCriteria {
    BandableCriteria {
        include_slack_producers: true,
        max_additional,
        min_p_range_mw: 1.0e-6,
        min_q_range_mvar: 0.0,
    }
}

/// Canonical GO-C3 wide-Q anchor criteria: `p_range >= 1 MW`, `q_range
/// > 0 MVAr`, slack eligible.
pub fn goc3_wide_q_anchor_criteria(max_n: usize) -> WideQAnchorCriteria {
    WideQAnchorCriteria {
        max_n,
        min_p_range_mw: 1.0,
        exclude_slack: false,
    }
}

/// Canonical GO-C3 reactive-support pin criteria: `q_range >= 30
/// MVAr`, `p_range >= 50 MW`, 3-hop BFS load weighting.
pub fn goc3_reactive_support_pin_criteria(factor: f64) -> ReactiveSupportPinCriteria {
    ReactiveSupportPinCriteria {
        factor,
        min_q_range_mvar: 30.0,
        min_p_range_mw: 50.0,
        load_weight_hops: 3,
    }
}

/// `5` extra bandable producers for small cases (≤ 100 buses), `0`
/// otherwise. Mirrors Python's `_try_full_ac_redispatch::
/// default_max_extra_bandable` heuristic.
pub fn goc3_max_additional_bandable(bus_count: usize) -> usize {
    if bus_count <= 100 { 5 } else { 0 }
}

/// Canonical GO-C3 band/floor/cap for the AC SCED bandable-subset pin.
pub fn goc3_dispatch_pinning_bands() -> DispatchPinningBands {
    DispatchPinningBands::default_band_reserve_aware()
}

// ── Peak load (reactive support pin input) ────────────────────────────────

/// Compute `(peak_load_by_bus_mw, peak_system_load_mw)` from the GO C3
/// problem. Peak is max across time periods. Converts per-pu values to
/// MW using `base_norm_mva`.
pub fn goc3_peak_load(problem: &GoC3Problem, context: &GoC3Context) -> (HashMap<u32, f64>, f64) {
    let base = problem.network.general.base_norm_mva.max(1.0);
    let periods = problem
        .time_series_input
        .simple_dispatchable_device
        .first()
        .map(|ts| ts.p_ub.len())
        .unwrap_or(48);

    // Per-period system load (max across periods is the system peak).
    let mut per_period_load = vec![0.0f64; periods];
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Consumer {
            continue;
        }
        let Some(ts) = problem
            .time_series_input
            .simple_dispatchable_device
            .iter()
            .find(|ts| ts.uid == device.uid)
        else {
            continue;
        };
        for (t, &p) in ts.p_ub.iter().enumerate() {
            if t < periods {
                per_period_load[t] += p * base;
            }
        }
    }
    let peak_system_load = per_period_load.iter().copied().fold(0.0f64, f64::max);

    // Per-bus peak consumer load.
    let mut load_by_bus: HashMap<u32, f64> = HashMap::new();
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Consumer {
            continue;
        }
        let Some(&bus_number) = context.bus_uid_to_number.get(&device.bus) else {
            continue;
        };
        let Some(ts) = problem
            .time_series_input
            .simple_dispatchable_device
            .iter()
            .find(|ts| ts.uid == device.uid)
        else {
            continue;
        };
        let peak = ts.p_ub.iter().copied().fold(0.0f64, f64::max) * base;
        *load_by_bus.entry(bus_number).or_default() += peak;
    }
    (load_by_bus, peak_system_load)
}

// ── AC SCED primitives ────────────────────────────────────────────────────

/// Per-producer ramp envelope in MW/hr, converted from the GO C3
/// problem's per-pu/hr `p_ramp_up_ub` / `p_ramp_down_ub`.
pub fn goc3_producer_ramp_limits(
    problem: &GoC3Problem,
    context: &GoC3Context,
) -> HashMap<String, RampLimits> {
    let base_mva = problem.network.general.base_norm_mva.max(1.0);
    let mut out = HashMap::new();
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Producer {
            continue;
        }
        let resource_id = &device.uid;
        if !matches!(
            context.device_kind_by_uid.get(resource_id).copied(),
            Some(GoC3DeviceKind::Producer)
        ) {
            continue;
        }
        out.insert(
            resource_id.clone(),
            RampLimits {
                ramp_up_mw_per_hr: device.p_ramp_up_ub * base_mva,
                ramp_down_mw_per_hr: device.p_ramp_down_ub * base_mva,
            },
        );
    }
    out
}

/// Per-consumer Q/P ratio fallback, derived from the device's
/// `initial_status`. Used as a seed when the DC SCUC result carries no
/// per-block Q (which it typically doesn't — SCUC is DC).
pub fn goc3_consumer_q_to_p_ratios(problem: &GoC3Problem) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Consumer {
            continue;
        }
        let p_init = device.initial_status.p;
        let ratio = if p_init.abs() <= 1e-12 {
            0.0
        } else {
            device.initial_status.q / p_init
        };
        out.insert(device.uid.clone(), ratio);
    }
    out
}

/// Reactive-support commitment augmentation: voltage-support producers
/// that must stay online on the AC SCED stage plus the synthetic HVDC
/// terminal Q gens from the context.
///
/// Identifies producers that are:
/// 1. kind `Producer` or `ProducerStatic`,
/// 2. have zero (or near-zero) active-power lower bound across the
///    horizon,
/// 3. have a non-trivial reactive capability,
/// 4. have zero on-cost and zero startup-cost.
pub fn goc3_reactive_support_commitment_schedule(
    problem: &GoC3Problem,
    context: &GoC3Context,
    periods: usize,
) -> Vec<ResourceCommitmentSchedule> {
    let mut schedules: HashMap<String, Vec<bool>> = HashMap::new();

    for (resource_id, schedule) in &context.internal_support_commitment_schedule {
        schedules.insert(resource_id.clone(), schedule.to_vec());
    }

    let device_ts_by_uid: HashMap<&str, &GoC3DeviceTimeSeries> = problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();

    for device in &problem.network.simple_dispatchable_device {
        let resource_id = device.uid.as_str();
        if resource_id.is_empty() {
            continue;
        }
        let kind = context.device_kind_by_uid.get(resource_id).copied();
        if !matches!(
            kind,
            Some(GoC3DeviceKind::Producer) | Some(GoC3DeviceKind::ProducerStatic)
        ) {
            continue;
        }

        let ts = match device_ts_by_uid.get(resource_id).copied() {
            Some(ts) => ts,
            None => continue,
        };

        // Must be able to go to zero P.
        if !ts.p_lb.is_empty() {
            let min_p_lb = ts.p_lb.iter().copied().fold(f64::INFINITY, f64::min);
            if min_p_lb > 1e-9 {
                continue;
            }
        }

        // Must have some reactive capability.
        let reactive_capability = ts
            .q_lb
            .iter()
            .chain(ts.q_ub.iter())
            .map(|v| v.abs())
            .fold(0.0f64, f64::max);
        if reactive_capability <= 1e-9 {
            continue;
        }

        // Must be costless to commit.
        if device.on_cost.abs() > 1e-9 {
            continue;
        }
        if device.startup_cost.abs() > 1e-9 {
            continue;
        }

        // Allowed-on pattern from on_status_ub (or initial_status as fallback).
        let allowed_on: Vec<bool> = if ts.on_status_ub.is_empty() {
            let initial_on = device.initial_status.on_status != 0;
            vec![initial_on; periods]
        } else {
            let mut series: Vec<bool> = ts
                .on_status_ub
                .iter()
                .take(periods)
                .map(|v| v.abs() > 0.5)
                .collect();
            let last = series.last().copied().unwrap_or(false);
            while series.len() < periods {
                series.push(last);
            }
            series
        };

        schedules.insert(resource_id.to_string(), allowed_on);
    }

    let mut resource_ids: Vec<String> = schedules.keys().cloned().collect();
    resource_ids.sort();
    resource_ids
        .into_iter()
        .map(|rid| {
            let schedule = schedules.remove(&rid).unwrap_or_default();
            let initial = schedule.first().copied().unwrap_or(false);
            ResourceCommitmentSchedule {
                resource_id: rid,
                initial,
                periods: Some(schedule),
            }
        })
        .collect()
}

// ── Penalty config ────────────────────────────────────────────────────────

/// Max $/MWh bid cost observed anywhere in the problem — used to scale
/// the hard-ramp violation penalty.
fn max_bid_cost_mwh(problem: &GoC3Problem, base_mva: f64) -> f64 {
    let base = base_mva.max(1.0);
    problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .flat_map(|ts| ts.cost.iter())
        .flat_map(|blocks| blocks.iter())
        .filter_map(|block| {
            if block.len() >= 2 {
                Some(block[0] / base)
            } else {
                None
            }
        })
        .fold(0.0_f64, f64::max)
}

/// Build the canonical [`PenaltyConfig`] for a GO C3 scenario.
///
/// `thermal_multiplier` scales the SCUC-visible branch thermal slack
/// penalty only; SCED's AC-OPF thermal penalty is configured
/// separately on the AC OPF options.
pub fn goc3_penalty_config(problem: &GoC3Problem, thermal_multiplier: f64) -> PenaltyConfig {
    let base_mva = problem.network.general.base_norm_mva.max(1.0);
    let vio = problem.network.violation_cost.clone().unwrap_or_default();
    let max_bid = max_bid_cost_mwh(problem, base_mva);
    let p_balance_per_mw = vio.p_bus_vio_cost / base_mva;
    let q_balance_per_mvar = if vio.q_bus_vio_cost > 0.0 {
        vio.q_bus_vio_cost / base_mva
    } else {
        p_balance_per_mw
    };
    let ramp_per_mw = (max_bid * 10.0).max(1_000_000.0 / base_mva);

    build_penalty_config(&PenaltyInputs {
        thermal_per_mva: (vio.s_vio_cost / base_mva) * thermal_multiplier,
        p_balance_per_mw,
        q_balance_per_mvar,
        voltage_curve: voltage_piecewise_curve(0.01, 5_000.0 / base_mva, 50_000.0 / base_mva),
        angle_per_rad: 500.0 / base_mva,
        reserve_per_mw: 1_000.0 / base_mva,
        ramp_per_mw,
    })
}

// ── AC OPF preset ─────────────────────────────────────────────────────────

/// Canonical GO C3 safety multiplier baseline used by the fallback
/// scaling. The static fallback penalties scale as
/// `safety_multiplier / CANONICAL_BASELINE` to keep the fallback in
/// proportion to the active multiplier.
const GOC3_BUS_BALANCE_SAFETY_MULTIPLIER_BASELINE: f64 = 100.0;
const GOC3_STATIC_P_BUS_PENALTY_PER_MW: f64 = 50_000.0;
const GOC3_STATIC_Q_BUS_PENALTY_PER_MVAR: f64 = 50_000.0;
const GOC3_P_PER_PU_FALLBACK: f64 = 1_000_000.0;

/// Build the canonical GO C3 `AcOpfOptions` for the AC SCED stage.
///
/// `bus_balance_safety_multiplier` overrides the canonical safety
/// factor (100× on per-pu validator costs) the AC SCED stage uses to
/// make Ipopt prefer physical relief over slack absorption.
pub fn goc3_ac_opf_options(
    problem: &GoC3Problem,
    bus_balance_safety_multiplier: f64,
) -> AcOpfOptions {
    let vio = problem.network.violation_cost.as_ref();
    let bus_balance_penalties = validator_aligned_bus_balance_penalties(&BusBalancePenaltyInputs {
        p_bus_per_pu: vio.map(|v| v.p_bus_vio_cost),
        q_bus_per_pu: vio.map(|v| v.q_bus_vio_cost),
        base_mva: problem.network.general.base_norm_mva,
        safety_multiplier: bus_balance_safety_multiplier,
        safety_multiplier_baseline: GOC3_BUS_BALANCE_SAFETY_MULTIPLIER_BASELINE,
        fallback_p_mw: GOC3_STATIC_P_BUS_PENALTY_PER_MW,
        fallback_q_mvar: GOC3_STATIC_Q_BUS_PENALTY_PER_MVAR,
        p_per_pu_fallback: GOC3_P_PER_PU_FALLBACK,
    });
    let mut opts =
        AcOpfSceduledBaseline::tap_locked_shunts_on(bus_balance_penalties).into_options();
    // GO C3 scores bus P/Q balance using its own pi-model reconstruction
    // with no constraint scaling. Ipopt's `gradient-based` scaling
    // makes `tol` apply to scaled rows; on stiff-coupling buses (zero-r
    // ties, identical parallel lines) the scaled→unscaled blow-up at
    // termination shows up as ~1e-6 pu validator-visible Q residual.
    // Tightening `tolerance` to 1e-9 (combined with the constr_viol_tol
    // binding in backends/ipopt.rs) drops the unscaled residual to
    // ~1e-12 pu — well below any reporting threshold and effectively
    // machine precision against the validator's pi-model.
    opts.tolerance = 1e-9;
    opts
}

/// Two-rung OPF retry attempts for a GO C3 scenario:
///
/// 1. `"go_validator_costs"` — base options as configured (validator-
///    aligned slack penalties + thermal limits + thermal slacks).
/// 2. `"strict_bus_balance"` — same as #1 but with the bus P/Q balance
///    slack variables removed (penalty=0 disables them in the NLP),
///    forcing exact bus balance. Long-shot fallback for cases where the
///    soft slack landscape kept Ipopt from converging.
///
/// The third "no_thermal_limits" rung from `standard_opf_retry_attempts`
/// is intentionally NOT included. Dropping thermal limits hides real
/// network overloads (the thermal violation just becomes ex-post cost),
/// which masks a SCUC commitment quality issue rather than fixing it.
pub fn goc3_opf_retry_attempts(base: &AcOpfOptions) -> Vec<OpfAttempt> {
    let soft = base.clone();
    let mut strict = base.clone();
    strict.bus_active_power_balance_slack_penalty_per_mw = 0.0;
    strict.bus_reactive_power_balance_slack_penalty_per_mvar = 0.0;
    vec![
        OpfAttempt::new("go_validator_costs", Some(soft)),
        OpfAttempt::new("strict_bus_balance", Some(strict)),
    ]
}

// ── Retry policy ──────────────────────────────────────────────────────────

/// Canonical GO C3 retry policy: 2 OPF attempts × default band only.
///
/// `relax_pmin_sweep` is intentionally `[false]` — we do NOT want to
/// relax committed-Pmin to zero as a fallback. If a generator is
/// committed it must respect its physical Pmin; producing a solution
/// that fakes Pmin=0 hides a SCUC commitment quality issue and feeds a
/// non-physical operating point into validation.
///
/// `band_attempts` is `[default_band]` only — the wide-band retry
/// (which let many more generators move off their narrow band) was a
/// way to recover when the AC SCED couldn't balance Q with the default
/// trust region; with the discrete polish + Jacobian/Hessian fixes the
/// default band is sufficient and the wide band just masks issues.
///
/// Feedback providers are **not** attached here; callers layer them on
/// via [`RetryPolicy::with_feedback`].
pub fn goc3_retry_policy(base_ac_opf: &AcOpfOptions) -> RetryPolicy {
    RetryPolicy {
        relax_pmin_sweep: vec![false],
        opf_attempts: goc3_opf_retry_attempts(base_ac_opf),
        nlp_solver_candidates: vec![None],
        band_attempts: vec![BandAttempt::default_band()],
        wide_band_penalty_threshold_dollars: f64::INFINITY,
        // HVDC fallback for DC-LP-degeneracy scenarios (flat LMPs
        // across many zero-cost renewables): if the baseline HVDC
        // direction (anchored by DC bus voltages) traps AC in an
        // infeasibility basin, retry with the flipped direction,
        // then with HVDC=0. Flipped first because the discrete
        // direction jump is usually what's needed; neutral as a
        // last-ditch when flip overshoots. Threshold 5 MW applies
        // to both P and Q bus balance slack (see
        // max_bus_balance_slack_mw in ac_refinement).
        hvdc_attempts: vec![
            HvdcAttempt::default_attempt(),
            HvdcAttempt::flipped(),
            HvdcAttempt::neutral(),
        ],
        hvdc_retry_bus_slack_threshold_mw: 5.0,
        hard_fail_first_attempt: false,
        feedback_providers: Vec::new(),
        commitment_probes: Vec::new(),
        max_iterations: 0,
    }
}

// ── Prop-through helpers ──────────────────────────────────────────────────

/// Helper that applies all of [`GoC3Policy`]'s AC SCED overrides onto a
/// freshly-built [`AcOpfOptions`]. Kept here so the workflow builder
/// doesn't have to remember the five individual policy fields.
pub fn apply_goc3_policy_to_ac_opf(opts: &mut AcOpfOptions, policy: &surge_io::go_c3::GoC3Policy) {
    if policy.disable_sced_thermal_limits {
        opts.enforce_thermal_limits = false;
        opts.thermal_limit_slack_penalty_per_mva = 0.0;
    }
    if let Some(tol) = policy.sced_ac_opf_tolerance {
        opts.tolerance = tol;
    }
    if let Some(max_iter) = policy.sced_ac_opf_max_iterations {
        opts.max_iterations = max_iter;
    }
    if policy.sced_enforce_regulated_bus_vm_targets {
        opts.enforce_regulated_bus_vm_targets = true;
    }
}

/// Apply reactive-support-pin mutations to a [`DispatchRequest`]: set
/// each pinned generator's per-period P bounds to the midpoint of
/// their existing `[pmin, pmax]` range.
pub fn apply_reactive_support_pin_to_request(
    request: &mut surge_dispatch::DispatchRequest,
    pin_ids: &HashSet<String>,
) {
    if pin_ids.is_empty() {
        return;
    }
    let profiles = request.profiles_mut();
    for entry in profiles.generator_dispatch_bounds.profiles.iter_mut() {
        if !pin_ids.contains(&entry.resource_id) {
            continue;
        }
        let n = entry.p_min_mw.len().min(entry.p_max_mw.len());
        for i in 0..n {
            let mid = (entry.p_min_mw[i] + entry.p_max_mw[i]) / 2.0;
            entry.p_min_mw[i] = mid;
            entry.p_max_mw[i] = mid;
        }
    }
}

/// Merge reactive-support-pin IDs into a commitment augmentation list
/// as always-on schedules for every period.
pub fn merge_reactive_pin_must_runs(
    commitment_augmentation: &mut Vec<ResourceCommitmentSchedule>,
    pin_ids: &HashSet<String>,
    periods: usize,
) {
    for rid in pin_ids {
        if let Some(existing) = commitment_augmentation
            .iter_mut()
            .find(|s| s.resource_id == *rid)
        {
            existing.initial = true;
            existing.periods = Some(vec![true; periods]);
        } else {
            commitment_augmentation.push(ResourceCommitmentSchedule {
                resource_id: rid.clone(),
                initial: true,
                periods: Some(vec![true; periods]),
            });
        }
    }
}
