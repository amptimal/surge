// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 solution export.
//!
//! Consumes a solved [`DispatchSolution`] and produces the typed
//! [`GoC3Solution`] document the validator scores. The heavy lifting
//! for trajectory derivation and online-status inference lives in the
//! canonical [`crate::trajectory`] module; this file is responsible
//! for reading the per-resource / per-bus / per-HVDC solution data,
//! applying GO-C3-specific reactive-sign conventions, and routing
//! consumer block-level outputs back through their parent UIDs.

use std::collections::{HashMap, HashSet};

use surge_dispatch::{DispatchSolution, ResourcePeriodDetail};
use surge_io::go_c3::types::{
    GoC3AcLineSolution, GoC3BusSolution, GoC3DcLineSolution, GoC3DeviceSolution, GoC3DeviceType,
    GoC3ShuntSolution, GoC3Solution, GoC3TimeSeriesOutput, GoC3TransformerSolution,
};
use surge_io::go_c3::{DcLineInitialState, GoC3Context, GoC3Problem};

use crate::trajectory::{
    derive_startup_shutdown_trajectory_power, infer_online_status_from_dispatch,
};

use super::GoC3DispatchError;

/// Convert a [`DispatchSolution`] into a GO C3 solution document.
///
/// Equivalent to [`export_go_c3_solution_with_reserve_source`] with
/// `dc_reserve_source = None` — active reserve awards are read from
/// `solution`. When the caller has both a DC SCUC and an AC SCED
/// solution available, prefer the two-argument form so active
/// reserve awards come from the DC stage (the AC stage filters
/// active reserves out of its market and won't carry awards).
pub fn export_go_c3_solution(
    problem: &GoC3Problem,
    context: &GoC3Context,
    solution: &DispatchSolution,
) -> Result<GoC3Solution, GoC3DispatchError> {
    export_go_c3_solution_with_reserve_source(problem, context, solution, None)
}

/// Optional knobs for [`export_go_c3_solution_with_options`].
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// When true, scale each consumer's exported active-reserve awards
    /// down so the validator's `viol_cs_t_p_on_min` /
    /// `viol_cs_t_p_on_max` constraints stay feasible after AC SCED
    /// curtails the consumer's `p_on`. Up-direction awards
    /// (regulation up, synchronous, ramp-up online) are capped at
    /// `max(0, p_on − p_lb)`; down-direction awards (regulation down,
    /// ramp-down online) at `max(0, p_ub − p_on)`. The shed amount is
    /// implicitly accepted as zonal reserve shortfall and scored by
    /// the validator's existing zonal-balance penalty — preferable
    /// to letting the validator stamp the whole solution `feas=0`
    /// over a bookkeeping mismatch between the SCUC reserve award
    /// and the AC-curtailed served power.
    ///
    /// Default `true`: the GO C3 path consistently produces this
    /// mismatch (AC SCED has no consumer-side reserve coupling rows),
    /// and shedding is the canonical resolution. Set `false` only for
    /// diagnostics that need to expose the raw SCUC awards.
    pub allow_consumer_reserve_shedding: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            allow_consumer_reserve_shedding: true,
        }
    }
}

/// Convert a [`DispatchSolution`] into a GO C3 solution document,
/// optionally taking active reserve awards from a separate "reserve
/// source" solution (typically the DC SCUC) while dispatch values
/// and reactive reserve awards come from `solution` (typically the
/// AC SCED).
///
/// Mirrors Python's `export_go3_solution(..., dc_dispatch_result=...)`
/// merge behavior. The active reserve product IDs to pull from
/// `dc_reserve_source` are the set of ReserveKind::Real products in
/// the problem's reserve catalog.
pub fn export_go_c3_solution_with_reserve_source(
    problem: &GoC3Problem,
    context: &GoC3Context,
    solution: &DispatchSolution,
    dc_reserve_source: Option<&DispatchSolution>,
) -> Result<GoC3Solution, GoC3DispatchError> {
    export_go_c3_solution_with_options(
        problem,
        context,
        solution,
        dc_reserve_source,
        &ExportOptions::default(),
    )
}

/// Like [`export_go_c3_solution_with_reserve_source`] but takes an
/// [`ExportOptions`] bag for opt-in behavior.
pub fn export_go_c3_solution_with_options(
    problem: &GoC3Problem,
    context: &GoC3Context,
    solution: &DispatchSolution,
    dc_reserve_source: Option<&DispatchSolution>,
    options: &ExportOptions,
) -> Result<GoC3Solution, GoC3DispatchError> {
    // The objective-ledger consistency check is advisory: LP
    // degeneracy / floating-point rounding can leave per-period
    // residuals of a few $K on long-horizon scenarios. Log and
    // continue rather than fail the export.
    if !solution.objective_ledger_is_consistent() {
        tracing::debug!(
            "dispatch solution objective ledger audit reported residuals; continuing export"
        );
    }

    let periods = problem.time_series_input.general.time_periods;
    let period_results = solution.periods();
    if period_results.len() != periods {
        return Err(GoC3DispatchError::Export(format!(
            "dispatch solution has {} periods, problem has {}",
            period_results.len(),
            periods
        )));
    }

    let base_mva = problem.network.general.base_norm_mva;

    // Bus solutions with voltage clamping to per-bus (vm_lb, vm_ub).
    let bus_solutions = export_bus_solutions(problem, context, solution);
    let (device_solutions, synthetic_ids) =
        export_device_solutions(problem, context, solution, dc_reserve_source, options);
    let ac_line_solutions = export_ac_line_solutions(problem, context, periods);
    let transformer_solutions = export_transformer_solutions(problem, context, solution, periods);
    let dc_line_solutions = export_dc_line_solutions(
        problem,
        context,
        solution,
        base_mva,
        periods,
        &synthetic_ids,
    );
    let shunt_solutions = export_shunt_solutions(problem, context, solution, periods);

    Ok(GoC3Solution {
        time_series_output: GoC3TimeSeriesOutput {
            bus: bus_solutions,
            simple_dispatchable_device: device_solutions,
            ac_line: ac_line_solutions,
            two_winding_transformer: transformer_solutions,
            dc_line: dc_line_solutions,
            shunt: shunt_solutions,
        },
    })
}

fn export_bus_solutions(
    problem: &GoC3Problem,
    context: &GoC3Context,
    solution: &DispatchSolution,
) -> Vec<GoC3BusSolution> {
    let periods = problem.time_series_input.general.time_periods;
    let period_results = solution.periods();
    let mut bus_solutions: Vec<GoC3BusSolution> = Vec::with_capacity(problem.network.bus.len());
    for bus in &problem.network.bus {
        let Some(&bus_number) = context.bus_uid_to_number.get(&bus.uid) else {
            continue;
        };
        let vm_lb = bus.vm_lb;
        let vm_ub = bus.vm_ub;
        let initial_vm = bus.initial_status.vm;
        let initial_va = bus.initial_status.va;
        let mut vm = vec![initial_vm; periods];
        let mut va = vec![initial_va; periods];
        for (i, period) in period_results.iter().enumerate() {
            if let Some(bus_result) = period.bus(bus_number) {
                vm[i] = bus_result
                    .voltage_pu
                    .unwrap_or(initial_vm)
                    .max(vm_lb)
                    .min(vm_ub);
                va[i] = bus_result.angle_rad.unwrap_or(initial_va);
            }
        }
        bus_solutions.push(GoC3BusSolution {
            uid: bus.uid.clone(),
            vm,
            va,
        });
    }
    bus_solutions
}

fn export_device_solutions(
    problem: &GoC3Problem,
    context: &GoC3Context,
    solution: &DispatchSolution,
    dc_reserve_source: Option<&DispatchSolution>,
    options: &ExportOptions,
) -> (Vec<GoC3DeviceSolution>, HashSet<String>) {
    let periods = problem.time_series_input.general.time_periods;
    let period_results = solution.periods();
    let base_mva = problem.network.general.base_norm_mva.max(1.0);

    let synthetic_ids: HashSet<String> = context
        .dc_line_reactive_support_resource_to_output
        .keys()
        .cloned()
        .collect();

    let device_ts_by_uid: HashMap<&str, &surge_io::go_c3::GoC3DeviceTimeSeries> = problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();

    // Index (resource_id, period) → ResourcePeriodResult lookups for O(1)
    // access across all devices.
    let mut resource_by_uid: HashMap<String, Vec<Option<&surge_dispatch::ResourcePeriodResult>>> =
        HashMap::new();
    for (period_idx, period) in period_results.iter().enumerate() {
        for r in period.resource_results() {
            let slot = resource_by_uid
                .entry(r.resource_id.clone())
                .or_insert_with(|| vec![None; periods]);
            if period_idx < slot.len() {
                slot[period_idx] = Some(r);
            }
        }
    }

    // Active (real-power) reserve awards come from the DC reserve
    // source when provided. Reactive awards stay on the AC solution.
    // This mirrors Python's `_merged_reserve_period_lookup` behavior.
    let mut dc_reserve_by_uid: HashMap<String, Vec<HashMap<String, f64>>> = HashMap::new();
    let active_reserve_product_ids: HashSet<String> = {
        use surge_network::market::ReserveKind;
        let mut ids = HashSet::new();
        // Derive from the AC-stage's request... but here we don't have
        // the request. Instead, infer "active" as anything NOT in the
        // reactive set (q_res_up / q_res_down). The reactive set is
        // small and stable.
        for r in period_results.iter().flat_map(|p| p.resource_results()) {
            for product_id in r.reserve_awards.keys() {
                if !matches!(product_id.as_str(), "q_res_up" | "q_res_down") {
                    ids.insert(product_id.clone());
                }
            }
        }
        // Also include product IDs that appear only in the DC source.
        if let Some(dc_sol) = dc_reserve_source {
            for r in dc_sol.periods().iter().flat_map(|p| p.resource_results()) {
                for product_id in r.reserve_awards.keys() {
                    if !matches!(product_id.as_str(), "q_res_up" | "q_res_down") {
                        ids.insert(product_id.clone());
                    }
                }
            }
        }
        let _ = ReserveKind::Real; // reserved for future typed refinement
        ids
    };
    if let Some(dc_sol) = dc_reserve_source {
        for (period_idx, period) in dc_sol.periods().iter().enumerate() {
            for r in period.resource_results() {
                let slot = dc_reserve_by_uid
                    .entry(r.resource_id.clone())
                    .or_insert_with(|| vec![HashMap::new(); periods]);
                if period_idx < slot.len() {
                    for (product, mw) in &r.reserve_awards {
                        if active_reserve_product_ids.contains(product) {
                            slot[period_idx].insert(product.clone(), *mw);
                        }
                    }
                }
            }
        }
    }

    let mut device_solutions: Vec<GoC3DeviceSolution> = Vec::new();
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type == GoC3DeviceType::Consumer {
            device_solutions.push(consumer_device_solution(
                device,
                device_ts_by_uid.get(device.uid.as_str()).copied(),
                periods,
                base_mva,
                &resource_by_uid,
                &dc_reserve_by_uid,
                dc_reserve_source.is_some(),
                context,
                options,
            ));
            continue;
        }

        // Producer path.
        let ts = device_ts_by_uid.get(device.uid.as_str()).copied();
        let resource_periods = resource_by_uid.get(&device.uid);
        let mut total_power_pu = vec![0.0_f64; periods];
        let mut q_pu = vec![0.0_f64; periods];
        let mut saw_solved_q = false;
        let mut commitment = vec![None; periods];
        let mut reserve_awards: Vec<HashMap<String, f64>> = vec![HashMap::new(); periods];
        if let Some(slots) = resource_periods {
            for (i, slot) in slots.iter().enumerate() {
                if let Some(r) = slot {
                    let p_mw = r.power_mw;
                    total_power_pu[i] = (p_mw / base_mva).max(0.0);
                    if let ResourcePeriodDetail::Generator(detail) = &r.detail {
                        commitment[i] = detail.commitment;
                        if let Some(q) = detail.q_mvar {
                            q_pu[i] = q / base_mva;
                            saw_solved_q = true;
                        }
                    }
                    for (product, mw) in &r.reserve_awards {
                        // Skip active reserves from the AC stage when
                        // a DC reserve source is provided — those
                        // awards are overridden by the DC values below
                        // (Python's `_merged_reserve_period_lookup`).
                        if dc_reserve_source.is_some()
                            && !matches!(product.as_str(), "q_res_up" | "q_res_down")
                        {
                            continue;
                        }
                        reserve_awards[i].insert(product.clone(), mw / base_mva);
                    }
                }
            }
        }
        // Overlay active reserve awards from the DC reserve source, if
        // provided. The AC stage's active-reserve awards were skipped
        // above; fill them in from the DC SCUC solution here.
        if let Some(dc_slots) = dc_reserve_by_uid.get(&device.uid) {
            for (i, dc_map) in dc_slots.iter().enumerate() {
                if i >= periods {
                    break;
                }
                for (product, mw) in dc_map {
                    reserve_awards[i].insert(product.clone(), mw / base_mva);
                }
            }
        }
        // When the solve path didn't populate a per-period reactive
        // trajectory (e.g. DC-only solve, zero-MW static producer),
        // fall back to the p-scaled q series: q[t] = p_on[t] *
        // (q_initial / p_initial), clamped into the per-period
        // `[q_lb, q_ub]` envelope. Matches the Python
        // `_scaled_q_series_pu` fallback. For zero-MW producers with
        // p_initial = 0, the ratio degenerates to 0 and clamping
        // raises q_pu into the lower bound (typically q_initial
        // itself), so the output matches Python's `_constant_q_series`
        // path as well.
        if !saw_solved_q {
            if let Some(ts) = ts {
                let q_initial = device.initial_status.q;
                let p_initial = device.initial_status.p;
                let static_producer = super::commitment::is_zero_mw_producer(ts);
                let ratio = if p_initial.abs() > 1e-9 {
                    q_initial / p_initial
                } else {
                    0.0
                };
                for i in 0..periods {
                    let lo = ts.q_lb.get(i).copied().unwrap_or(f64::NEG_INFINITY);
                    let hi = ts.q_ub.get(i).copied().unwrap_or(f64::INFINITY);
                    // For "static" zero-MW producers (p_ub = p_lb = 0
                    // across the horizon) Python uses the constant-q
                    // series clamped from `initial_status.q`; normal
                    // producers use the scaled-q series from
                    // `p_on * (q_initial / p_initial)`.
                    let base_q = if static_producer {
                        q_initial
                    } else {
                        total_power_pu[i] * ratio
                    };
                    q_pu[i] = base_q.max(lo).min(hi);
                }
            }
        } else if let Some(ts) = ts {
            // Solved Q comes back from the NLP potentially slightly
            // outside the `[q_lb, q_ub]` envelope due to Ipopt's
            // own convergence tolerance. Clamp into bounds so the GO
            // validator's strict physical-feasibility check doesn't
            // flag floating-point noise as a bound violation. Mirrors
            // Python's `min(max(q, q_lb), q_ub)` in `export_go3_solution`.
            for (i, q) in q_pu.iter_mut().enumerate().take(periods) {
                let lo = ts.q_lb.get(i).copied().unwrap_or(f64::NEG_INFINITY);
                let hi = ts.q_ub.get(i).copied().unwrap_or(f64::INFINITY);
                *q = q.max(lo).min(hi);
            }
        }

        // On-status inference.
        let on_status = infer_online_status_from_dispatch(&total_power_pu, &commitment);

        // Startup / shutdown edges.
        let mut startup_events = vec![false; periods];
        let mut shutdown_events = vec![false; periods];
        let mut prev_on = device.initial_status.on_status != 0;
        for t in 0..periods {
            let is_on = on_status[t] != 0;
            startup_events[t] = !prev_on && is_on;
            shutdown_events[t] = prev_on && !is_on;
            prev_on = is_on;
        }

        // Startup/shutdown trajectory (MW).
        let p_startup_ramp_mw = device.p_startup_ramp_ub * base_mva;
        let p_shutdown_ramp_mw = device.p_shutdown_ramp_ub * base_mva;
        let interval_hours = problem.time_series_input.general.interval_duration.clone();
        let on_bool: Vec<bool> = on_status.iter().map(|&v| v != 0).collect();
        let trajectory_mw = derive_startup_shutdown_trajectory_power(
            &on_bool,
            &vec![p_startup_ramp_mw; periods],
            &vec![p_shutdown_ramp_mw; periods],
            &interval_hours,
        );

        // p_on = total_power - trajectory, clamped to [0, p_ub].
        let mut p_on = vec![0.0_f64; periods];
        if let Some(ts) = ts {
            for i in 0..periods {
                let p_ub_pu = ts.p_ub.get(i).copied().unwrap_or(total_power_pu[i]);
                let trajectory_pu = trajectory_mw[i] / base_mva;
                let mut on_power = total_power_pu[i] - trajectory_pu;
                if on_status[i] == 0 {
                    on_power = 0.0;
                }
                p_on[i] = on_power.max(0.0).min(p_ub_pu);
            }
        } else {
            p_on[..periods].copy_from_slice(&total_power_pu[..periods]);
        }

        let (
            p_reg_res_up,
            p_reg_res_down,
            p_syn_res,
            p_nsyn_res,
            p_ramp_res_up_online,
            p_ramp_res_down_online,
            p_ramp_res_up_offline,
            p_ramp_res_down_offline,
        ) = extract_reserve_awards(&reserve_awards);

        device_solutions.push(GoC3DeviceSolution {
            uid: device.uid.clone(),
            on_status,
            p_on,
            q: q_pu,
            p_reg_res_up,
            p_reg_res_down,
            p_syn_res,
            p_nsyn_res,
            p_ramp_res_up_online,
            p_ramp_res_down_online,
            p_ramp_res_up_offline,
            p_ramp_res_down_offline,
            q_res_up: vec![0.0; periods],
            q_res_down: vec![0.0; periods],
        });
    }

    (device_solutions, synthetic_ids)
}

/// Per-period MW series for each of the eight GO C3 active-reserve
/// products, in the canonical order:
/// `(reg_up, reg_down, syn, nsyn, ramp_up_on, ramp_down_on,
/// ramp_up_off, ramp_down_off)`.
type ActiveReserveAwardSeries = (
    Vec<f64>,
    Vec<f64>,
    Vec<f64>,
    Vec<f64>,
    Vec<f64>,
    Vec<f64>,
    Vec<f64>,
    Vec<f64>,
);

/// Product IDs whose awards count against `p_on − p_lb` for a
/// consumer (validator's `viol_cs_t_p_on_min` formula).
const UP_DIRECTION_PRODUCTS: &[&str] = &["reg_up", "syn", "ramp_up_on"];

/// Product IDs whose awards count against `p_ub − p_on` for a
/// consumer (validator's `viol_cs_t_p_on_max` formula).
const DOWN_DIRECTION_PRODUCTS: &[&str] = &["reg_down", "ramp_down_on"];

fn extract_reserve_awards(reserve_awards: &[HashMap<String, f64>]) -> ActiveReserveAwardSeries {
    let take = |id: &str| -> Vec<f64> {
        reserve_awards
            .iter()
            .map(|m| m.get(id).copied().unwrap_or(0.0))
            .collect()
    };
    (
        take("reg_up"),
        take("reg_down"),
        take("syn"),
        take("nsyn"),
        take("ramp_up_on"),
        take("ramp_down_on"),
        take("ramp_up_off"),
        take("ramp_down_off"),
    )
}

fn consumer_device_solution(
    device: &surge_io::go_c3::GoC3Device,
    device_ts: Option<&surge_io::go_c3::GoC3DeviceTimeSeries>,
    periods: usize,
    base_mva: f64,
    resource_by_uid: &HashMap<String, Vec<Option<&surge_dispatch::ResourcePeriodResult>>>,
    dc_reserve_by_uid: &HashMap<String, Vec<HashMap<String, f64>>>,
    have_dc_reserve_source: bool,
    context: &GoC3Context,
    options: &ExportOptions,
) -> GoC3DeviceSolution {
    // Block IDs — populated by `build_dispatch_request`. When the
    // request builder hasn't been called on this handle, probe the
    // deterministic naming scheme (`{uid}::blk:NN`) against the
    // solution's per-resource lookup to recover them.
    let block_ids: Vec<String> = context
        .consumer_dispatchable_resource_ids_by_uid
        .get(&device.uid)
        .cloned()
        .unwrap_or_else(|| {
            let mut ids = Vec::new();
            for idx in 0..256 {
                let candidate = format!("{}::blk:{:02}", device.uid, idx);
                if resource_by_uid.contains_key(&candidate) {
                    ids.push(candidate);
                } else {
                    break;
                }
            }
            ids
        });

    let fixed_floor_pu = context
        .device_fixed_p_series_pu
        .get(&device.uid)
        .cloned()
        .unwrap_or_else(|| vec![0.0; periods]);

    let mut total_pu = fixed_floor_pu.clone();
    if total_pu.len() < periods {
        total_pu.resize(periods, 0.0);
    }
    let mut total_q_pu = vec![0.0_f64; periods];
    let mut saw_served_q = false;
    // Consumer reserve awards are summed across the consumer's bid
    // blocks. The LP clears reserves at the block level; the GO C3
    // validator evaluates zonal reserve balance over `sd` devices
    // (producers and consumers), so block awards must be folded back
    // onto the parent consumer uid.
    let mut reserve_awards: Vec<HashMap<String, f64>> = vec![HashMap::new(); periods];

    for block_id in &block_ids {
        if let Some(slots) = resource_by_uid.get(block_id) {
            for (i, slot) in slots.iter().enumerate() {
                if let Some(r) = slot {
                    if let ResourcePeriodDetail::DispatchableLoad(detail) = &r.detail {
                        total_pu[i] += detail.served_p_mw / base_mva;
                        if let Some(q) = detail.served_q_mvar {
                            total_q_pu[i] += q / base_mva;
                            saw_served_q = true;
                        }
                    } else {
                        // Generator-side result fallback: use sign flip.
                        total_pu[i] += (-r.power_mw / base_mva).max(0.0);
                    }
                    // Mirror the producer path: when a DC reserve source
                    // is provided, the AC stage's active-reserve awards
                    // are overridden below; keep only q_res_up / q_res_down
                    // from AC. Otherwise fold in everything from AC.
                    for (product, mw) in &r.reserve_awards {
                        if have_dc_reserve_source
                            && !matches!(product.as_str(), "q_res_up" | "q_res_down")
                        {
                            continue;
                        }
                        *reserve_awards[i].entry(product.clone()).or_insert(0.0) += mw / base_mva;
                    }
                }
            }
        }
        // Overlay DC SCUC reserve awards for this block, if any.
        if let Some(dc_slots) = dc_reserve_by_uid.get(block_id) {
            for (i, dc_map) in dc_slots.iter().enumerate().take(periods) {
                for (product, mw) in dc_map {
                    *reserve_awards[i].entry(product.clone()).or_insert(0.0) += mw / base_mva;
                }
            }
        }
    }

    let p_ub: Vec<f64> = device_ts
        .map(|ts| ts.p_ub.clone())
        .unwrap_or_else(|| vec![f64::INFINITY; periods]);

    let p_on: Vec<f64> = (0..periods)
        .map(|i| {
            let upper = p_ub.get(i).copied().unwrap_or(total_pu[i]);
            total_pu[i].max(0.0).min(upper)
        })
        .collect();

    let q: Vec<f64> = if saw_served_q {
        let q_lb = device_ts.map(|ts| ts.q_lb.clone()).unwrap_or_default();
        let q_ub = device_ts.map(|ts| ts.q_ub.clone()).unwrap_or_default();
        (0..periods)
            .map(|i| {
                let lo = q_lb.get(i).copied().unwrap_or(f64::NEG_INFINITY);
                let hi = q_ub.get(i).copied().unwrap_or(f64::INFINITY);
                total_q_pu[i].max(lo).min(hi)
            })
            .collect()
    } else {
        // Scaled q series by initial power factor.
        let q_lb = device_ts.map(|ts| ts.q_lb.clone()).unwrap_or_default();
        let q_ub = device_ts.map(|ts| ts.q_ub.clone()).unwrap_or_default();
        let q_initial = device.initial_status.q;
        let p_initial = device.initial_status.p;
        let ratio = if p_initial.abs() > 1e-9 {
            q_initial / p_initial
        } else {
            0.0
        };
        (0..periods)
            .map(|i| {
                let q_pu = p_on[i] * ratio;
                let lo = q_lb.get(i).copied().unwrap_or(f64::NEG_INFINITY);
                let hi = q_ub.get(i).copied().unwrap_or(f64::INFINITY);
                q_pu.max(lo).min(hi)
            })
            .collect()
    };

    let raw_on_status: Vec<i32> = p_on
        .iter()
        .zip(q.iter())
        .map(|(&p, &q)| {
            if p.abs() > 1e-9 || q.abs() > 1e-9 {
                1
            } else {
                0
            }
        })
        .collect();
    let on_status = stabilize_zero_floor_online_status(device, device_ts, raw_on_status);

    // Optional reserve-shedding pass: AC SCED can curtail consumer
    // `p_on` below the level needed to support the SCUC-awarded
    // reserves. The validator's
    //   `viol_cs_t_p_on_min`: p_on ≥ p_lb + p_rgu + p_scr + p_rru_on
    //   `viol_cs_t_p_off_max`: p_on ≤ p_ub − p_rgd − p_rrd_on
    // then stamps `feas=0` even when the AC dispatch is otherwise
    // valid. When `allow_consumer_reserve_shedding=true` we cap each
    // direction's awarded reserves so those constraints stay
    // feasible. The shed amount is implicitly accepted as zonal
    // reserve shortfall and scored by the validator's existing zonal-
    // balance penalty — preferable to letting the validator stamp
    // the whole solution `feas=0` over a bookkeeping mismatch
    // between SCUC awards and AC-curtailed dispatch.
    if options.allow_consumer_reserve_shedding {
        let p_lb_series: Vec<f64> = device_ts
            .map(|ts| ts.p_lb.clone())
            .unwrap_or_else(|| vec![0.0; periods]);
        for (i, awards) in reserve_awards.iter_mut().enumerate() {
            if i >= p_on.len() {
                break;
            }
            let p_lb_i = p_lb_series.get(i).copied().unwrap_or(0.0);
            let p_ub_i = p_ub.get(i).copied().unwrap_or(f64::INFINITY);

            // Up-direction: served power must cover `p_lb + Σ up_res`.
            let up_room = (p_on[i] - p_lb_i).max(0.0);
            let up_total: f64 = UP_DIRECTION_PRODUCTS
                .iter()
                .filter_map(|p| awards.get(*p).copied())
                .sum();
            if up_total > up_room + 1e-12 {
                let scale = if up_total > 0.0 {
                    (up_room / up_total).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                for product in UP_DIRECTION_PRODUCTS {
                    if let Some(v) = awards.get_mut(*product) {
                        *v *= scale;
                    }
                }
            }

            // Down-direction: served power plus down-reserves must
            // not exceed `p_ub`.
            let down_room = (p_ub_i - p_on[i]).max(0.0);
            let down_total: f64 = DOWN_DIRECTION_PRODUCTS
                .iter()
                .filter_map(|p| awards.get(*p).copied())
                .sum();
            if down_total > down_room + 1e-12 {
                let scale = if down_total > 0.0 {
                    (down_room / down_total).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                for product in DOWN_DIRECTION_PRODUCTS {
                    if let Some(v) = awards.get_mut(*product) {
                        *v *= scale;
                    }
                }
            }
        }
    }

    let (
        p_reg_res_up,
        p_reg_res_down,
        p_syn_res,
        p_nsyn_res,
        p_ramp_res_up_online,
        p_ramp_res_down_online,
        p_ramp_res_up_offline,
        p_ramp_res_down_offline,
    ) = extract_reserve_awards(&reserve_awards);

    GoC3DeviceSolution {
        uid: device.uid.clone(),
        on_status,
        p_on,
        q,
        p_reg_res_up,
        p_reg_res_down,
        p_syn_res,
        p_nsyn_res,
        p_ramp_res_up_online,
        p_ramp_res_down_online,
        p_ramp_res_up_offline,
        p_ramp_res_down_offline,
        q_res_up: vec![0.0; periods],
        q_res_down: vec![0.0; periods],
    }
}

fn export_ac_line_solutions(
    problem: &GoC3Problem,
    context: &GoC3Context,
    periods: usize,
) -> Vec<GoC3AcLineSolution> {
    problem
        .network
        .ac_line
        .iter()
        .map(|line| {
            let initial = context
                .ac_line_initial
                .get(&line.uid)
                .map(|s| s.on_status)
                .unwrap_or(1);
            GoC3AcLineSolution {
                uid: line.uid.clone(),
                on_status: vec![initial; periods],
            }
        })
        .collect()
}

fn export_transformer_solutions(
    problem: &GoC3Problem,
    context: &GoC3Context,
    solution: &DispatchSolution,
    periods: usize,
) -> Vec<GoC3TransformerSolution> {
    let period_results = solution.periods();
    problem
        .network
        .two_winding_transformer
        .iter()
        .map(|xfmr| {
            let initial = context.transformer_initial.get(&xfmr.uid);
            let on = initial.map(|s| s.on_status).unwrap_or(1);
            let init_tm = initial.map(|s| s.tm).unwrap_or(1.0);
            let init_ta = initial.map(|s| s.ta).unwrap_or(0.0);
            let mut tm = vec![init_tm; periods];
            let mut ta = vec![init_ta; periods];
            if let Some(&branch_idx) = context.branch_local_index_by_uid.get(&xfmr.uid) {
                for (t, period) in period_results.iter().enumerate() {
                    if t >= periods {
                        break;
                    }
                    if let Some((_, _t_cont, t_rounded)) = period
                        .tap_dispatch()
                        .iter()
                        .find(|(idx, _, _)| *idx == branch_idx)
                    {
                        tm[t] = *t_rounded;
                    }
                    if let Some((_, _p_cont, p_rounded)) = period
                        .phase_dispatch()
                        .iter()
                        .find(|(idx, _, _)| *idx == branch_idx)
                    {
                        ta[t] = *p_rounded;
                    }
                }
            }
            GoC3TransformerSolution {
                uid: xfmr.uid.clone(),
                on_status: vec![on; periods],
                tm,
                ta,
            }
        })
        .collect()
}

fn export_dc_line_solutions(
    problem: &GoC3Problem,
    context: &GoC3Context,
    solution: &DispatchSolution,
    base_mva: f64,
    periods: usize,
    _synthetic_ids: &HashSet<String>,
) -> Vec<GoC3DcLineSolution> {
    let period_results = solution.periods();
    let base = base_mva.max(1.0);
    problem
        .network
        .dc_line
        .iter()
        .map(|dc| {
            let initial =
                context
                    .dc_line_initial
                    .get(&dc.uid)
                    .cloned()
                    .unwrap_or(DcLineInitialState {
                        pdc_fr: 0.0,
                        qdc_fr: 0.0,
                        qdc_to: 0.0,
                    });
            let q_bounds = context.dc_line_q_bounds.get(&dc.uid).cloned();
            let mut pdc_fr = vec![initial.pdc_fr; periods];
            let mut qdc_fr = vec![initial.qdc_fr; periods];
            let mut qdc_to = vec![initial.qdc_to; periods];
            for (i, period) in period_results.iter().enumerate() {
                let mut transfer_mw = 0.0_f64;
                if let Some(hvdc) = period.hvdc(&dc.uid) {
                    pdc_fr[i] = hvdc.mw / base;
                    transfer_mw = hvdc.mw;
                }
                let mut rectifier_q_pu: Option<f64> = None;
                let mut inverter_q_pu: Option<f64> = None;
                if let Some(resources) = context.dc_line_reactive_support_resource_ids.get(&dc.uid)
                {
                    if let Some(res) = period
                        .resource_results()
                        .iter()
                        .find(|r| r.resource_id == resources.fr)
                    {
                        if let ResourcePeriodDetail::Generator(detail) = &res.detail {
                            if let Some(q) = detail.q_mvar {
                                rectifier_q_pu = Some(-q / base);
                            }
                        }
                    }
                    if let Some(res) = period
                        .resource_results()
                        .iter()
                        .find(|r| r.resource_id == resources.to)
                    {
                        if let ResourcePeriodDetail::Generator(detail) = &res.detail {
                            if let Some(q) = detail.q_mvar {
                                inverter_q_pu = Some(-q / base);
                            }
                        }
                    }
                }
                // LCC fixed-schedule fallback when the AC reconcile
                // hasn't populated the synthetic reactive-support
                // generators (e.g. a DC-only solve).
                let rectifier_q_pu = rectifier_q_pu
                    .unwrap_or_else(|| lcc_fixed_schedule_mvar(transfer_mw, 15.0) / base);
                let inverter_q_pu = inverter_q_pu
                    .unwrap_or_else(|| lcc_fixed_schedule_mvar(transfer_mw, 20.0) / base);
                let (qdc_fr_lb, qdc_fr_ub, qdc_to_lb, qdc_to_ub) = if let Some(q_bounds) = &q_bounds
                {
                    (
                        q_bounds.qdc_fr_lb,
                        q_bounds.qdc_fr_ub,
                        q_bounds.qdc_to_lb,
                        q_bounds.qdc_to_ub,
                    )
                } else {
                    (rectifier_q_pu, rectifier_q_pu, inverter_q_pu, inverter_q_pu)
                };
                qdc_fr[i] = clamp(rectifier_q_pu, qdc_fr_lb, qdc_fr_ub);
                qdc_to[i] = clamp(inverter_q_pu, qdc_to_lb, qdc_to_ub);
            }
            GoC3DcLineSolution {
                uid: dc.uid.clone(),
                pdc_fr,
                qdc_fr,
                qdc_to,
            }
        })
        .collect()
}

/// LCC converter fixed-schedule reactive draw: the magnitude of the
/// reactive component of a converter running at `|P|` MW with a
/// nominal firing angle of `typical_angle_deg` degrees.
fn lcc_fixed_schedule_mvar(p_dc_mw: f64, typical_angle_deg: f64) -> f64 {
    let power = p_dc_mw.abs();
    let typical_cos = typical_angle_deg.to_radians().cos();
    if typical_cos <= 1e-12 {
        return 0.0;
    }
    power * (1.0 - typical_cos * typical_cos).max(0.0).sqrt() / typical_cos
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    let (lo, hi) = if lo > hi { (hi, lo) } else { (lo, hi) };
    v.max(lo).min(hi)
}

/// Port of Python `_device_has_temporal_onoff_constraints`.
fn device_has_temporal_onoff_constraints(device: &surge_io::go_c3::GoC3Device) -> bool {
    device.in_service_time_lb > 0.0 || device.down_time_lb > 0.0 || !device.startups_ub.is_empty()
}

/// Port of Python `_stabilize_zero_floor_online_status`.
///
/// Used by the consumer online-status path: for devices whose `p_lb`
/// floor is zero across the horizon and which carry temporal on/off
/// constraints (min up/down time, startup windows), fill in any gaps
/// between the first and last "on" period so the exported schedule
/// is a contiguous block.
fn stabilize_zero_floor_online_status(
    device: &surge_io::go_c3::GoC3Device,
    device_ts: Option<&surge_io::go_c3::GoC3DeviceTimeSeries>,
    on_status: Vec<i32>,
) -> Vec<i32> {
    let periods = on_status.len();
    if periods == 0 {
        return on_status;
    }
    let Some(device_ts) = device_ts else {
        return on_status;
    };
    let p_lb = &device_ts.p_lb;
    if p_lb.iter().any(|v| *v > 1e-9) {
        return on_status;
    }
    if !device_has_temporal_onoff_constraints(device) {
        return on_status;
    }
    let required_on: Vec<i32> = (0..periods)
        .map(|i| device_ts.on_status_lb.get(i).copied().unwrap_or(0.0) as i32)
        .collect();
    let allowed_on: Vec<i32> = (0..periods)
        .map(|i| device_ts.on_status_ub.get(i).copied().unwrap_or(1.0) as i32)
        .collect();
    let positive_periods: Vec<usize> = on_status
        .iter()
        .enumerate()
        .filter_map(|(i, &v)| if v != 0 { Some(i) } else { None })
        .collect();
    if positive_periods.is_empty() {
        return (0..periods)
            .map(|i| {
                if allowed_on[i] != 0 {
                    required_on[i]
                } else {
                    0
                }
            })
            .collect();
    }
    let mut stabilized = on_status.clone();
    let first_on = *positive_periods.first().unwrap();
    let last_on = *positive_periods.last().unwrap();
    for idx in first_on..=last_on {
        if allowed_on[idx] != 0 {
            stabilized[idx] = 1;
        }
    }
    for idx in 0..periods {
        if allowed_on[idx] == 0 {
            stabilized[idx] = 0;
        } else if required_on[idx] != 0 {
            stabilized[idx] = 1;
        }
    }
    stabilized
}

fn export_shunt_solutions(
    problem: &GoC3Problem,
    context: &GoC3Context,
    solution: &DispatchSolution,
    periods: usize,
) -> Vec<GoC3ShuntSolution> {
    let period_results = solution.periods();

    // GO C3 problems often have "twin" shunts at the same bus with identical
    // per-step admittance bs. The continuous AC OPF treats them as separate
    // variables, but its objective is symmetric in the pair, so the NLP
    // typically picks b_1 = b_2 = total/2. Naive independent rounding then
    // sends BOTH twins to the same nearest step, doubling the realized
    // injection vs what the NLP planned. Group shunts by (bus, bs) and
    // round the AGGREGATE continuous susceptance, then split the resulting
    // integer step total across the twins (e.g., total = 3 across 2 twins
    // → split as (2, 1)). Saves ~35% of bus Q-balance penalty on
    // event4_617 D1 sw0 sc002 vs naive per-shunt rounding.
    let mut group_keys: HashMap<(String, i64), Vec<usize>> = HashMap::new();
    for (idx, shunt) in problem.network.shunt.iter().enumerate() {
        // Discretize bs to 1e-9 pu so floating-point noise doesn't split
        // physically-identical shunts into different groups.
        let bs_key = (shunt.bs * 1e9).round() as i64;
        group_keys
            .entry((shunt.bus.clone(), bs_key))
            .or_default()
            .push(idx);
    }

    let mut steps_per_shunt: Vec<Vec<i32>> = problem
        .network
        .shunt
        .iter()
        .map(|shunt| {
            let initial_step = context
                .shunt_initial_steps
                .get(&shunt.uid)
                .copied()
                .unwrap_or(shunt.initial_status.step);
            vec![initial_step; periods]
        })
        .collect();

    for indices in group_keys.values() {
        let lead = indices[0];
        let lead_shunt = &problem.network.shunt[lead];
        if lead_shunt.bs.abs() <= 1e-15 {
            continue;
        }
        let bs = lead_shunt.bs;
        let bounds: Vec<(i32, i32)> = indices
            .iter()
            .map(|&i| {
                let uid = &problem.network.shunt[i].uid;
                context
                    .shunt_step_bounds
                    .get(uid)
                    .copied()
                    .unwrap_or((0, 1))
            })
            .collect();
        let group_step_lb: i32 = bounds.iter().map(|(lb, _)| *lb).sum();
        let group_step_ub: i32 = bounds.iter().map(|(_, ub)| *ub).sum();

        for (t, period) in period_results.iter().enumerate() {
            if t >= periods {
                break;
            }
            // Sum continuous b across the group; if any member is missing
            // from the OPF dispatch, fall back to per-member naive rounding.
            let mut b_cont_sum = 0.0_f64;
            let mut all_found = true;
            for &i in indices {
                let uid = &problem.network.shunt[i].uid;
                if let Some((_, _, b_cont, _)) = period
                    .switched_shunt_dispatch()
                    .iter()
                    .find(|(id, _, _, _)| id == uid)
                {
                    b_cont_sum += *b_cont;
                } else {
                    all_found = false;
                    break;
                }
            }
            if !all_found {
                continue;
            }
            // Round the aggregate to the nearest discrete step total,
            // clamped to the group's combined step range.
            let mut total_steps = (b_cont_sum / bs).round() as i32;
            total_steps = total_steps.clamp(group_step_lb, group_step_ub);

            // Distribute across members in input order, respecting each
            // member's individual (step_lb, step_ub).
            let mut remaining = total_steps;
            for (k, &i) in indices.iter().enumerate() {
                let (lb, ub) = bounds[k];
                let assigned = remaining.clamp(lb, ub);
                steps_per_shunt[i][t] = assigned;
                remaining -= assigned;
            }
        }
    }

    problem
        .network
        .shunt
        .iter()
        .enumerate()
        .map(|(idx, shunt)| GoC3ShuntSolution {
            uid: shunt.uid.clone(),
            step: steps_per_shunt[idx].clone(),
        })
        .collect()
}
