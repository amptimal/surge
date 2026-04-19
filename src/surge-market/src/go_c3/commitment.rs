// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 commitment metadata.
//!
//! This module wraps GO C3's per-device commitment descriptors (startup
//! frequency windows, multi-interval energy requirements, must-run
//! floors, and initial state) into the typed Rust structures that
//! [`surge_dispatch`] consumes. The canonical translators in
//! [`crate::commitment`] and [`crate::windows`] do the generic work;
//! this file is the GO-C3-specific field mapper.

use surge_dispatch::request::{
    CommitmentInitialCondition, ResourceEnergyWindowLimit, ResourcePeriodCommitment,
    ResourceStartupWindowLimit,
};
use surge_io::go_c3::{GoC3Device, GoC3DeviceTimeSeries, GoC3Problem};

use crate::commitment::initial_condition_from_accumulated_times;
use crate::windows::{period_range_by_interval_midpoint, period_range_by_interval_start};

/// Convert a GO C3 producer's `startups_ub` blocks to startup-window
/// limits over the solved timeline.
pub(super) fn startup_window_limits(
    problem: &GoC3Problem,
    device: &GoC3Device,
) -> Vec<ResourceStartupWindowLimit> {
    let intervals = &problem.time_series_input.general.interval_duration;
    let mut limits = Vec::new();
    for block in &device.startups_ub {
        if block.len() < 3 {
            continue;
        }
        let start_hour = block[0];
        let end_hour = block[1];
        let max_startups = block[2] as u32;
        let Some((start_idx, end_idx)) =
            period_range_by_interval_start(intervals, start_hour, end_hour)
        else {
            continue;
        };
        limits.push(ResourceStartupWindowLimit {
            resource_id: device.uid.clone(),
            start_period_idx: start_idx,
            end_period_idx: end_idx,
            max_startups,
        });
    }
    limits
}

/// Convert a GO C3 producer's `energy_req_lb` / `energy_req_ub`
/// blocks to energy-window limits over the solved timeline.
pub(super) fn energy_window_limits(
    problem: &GoC3Problem,
    device_ts: &GoC3DeviceTimeSeries,
    device: &GoC3Device,
) -> Vec<ResourceEnergyWindowLimit> {
    let base_mva = problem.network.general.base_norm_mva;
    let intervals = &problem.time_series_input.general.interval_duration;
    let mut limits_by_window: std::collections::BTreeMap<
        (usize, usize),
        ResourceEnergyWindowLimit,
    > = std::collections::BTreeMap::new();

    let _ = device_ts; // some future fields may move here; kept for signature stability

    let mut apply = |start_hour: f64,
                     end_hour: f64,
                     min_energy_mwh: Option<f64>,
                     max_energy_mwh: Option<f64>| {
        let Some((start_idx, end_idx)) =
            period_range_by_interval_midpoint(intervals, start_hour, end_hour)
        else {
            return;
        };
        let entry =
            limits_by_window
                .entry((start_idx, end_idx))
                .or_insert(ResourceEnergyWindowLimit {
                    resource_id: device.uid.clone(),
                    start_period_idx: start_idx,
                    end_period_idx: end_idx,
                    min_energy_mwh: None,
                    max_energy_mwh: None,
                });
        if let Some(v) = min_energy_mwh {
            entry.min_energy_mwh = Some(entry.min_energy_mwh.map_or(v, |current| current.max(v)));
        }
        if let Some(v) = max_energy_mwh {
            entry.max_energy_mwh = Some(entry.max_energy_mwh.map_or(v, |current| current.min(v)));
        }
    };

    for block in &device.energy_req_lb {
        if block.len() < 3 {
            continue;
        }
        let min_energy_pu = block[2];
        if min_energy_pu <= 1e-9 {
            continue;
        }
        apply(block[0], block[1], Some(min_energy_pu * base_mva), None);
    }
    for block in &device.energy_req_ub {
        if block.len() < 3 {
            continue;
        }
        apply(block[0], block[1], None, Some(block[2] * base_mva));
    }

    limits_by_window
        .into_values()
        .filter(|l| l.min_energy_mwh.is_some() || l.max_energy_mwh.is_some())
        .collect()
}

/// Return the `ResourcePeriodCommitment` must-run floor derived from
/// a device's `on_status_lb` series, or `None` when the series is
/// all zero.
pub(super) fn minimum_commitment(
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
) -> Option<ResourcePeriodCommitment> {
    let periods: Vec<bool> = device_ts.on_status_lb.iter().map(|v| *v != 0.0).collect();
    if !periods.iter().any(|b| *b) {
        return None;
    }
    Some(ResourcePeriodCommitment {
        resource_id: device.uid.clone(),
        periods,
    })
}

/// Determine whether a device is effectively committed at the start of
/// the horizon. A unit is "effectively committed" when either its
/// pre-horizon `on_status` flag is set or the very first period has a
/// must-run floor.
pub(super) fn effective_initial_commitment(
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
    periods: usize,
) -> bool {
    if device.initial_status.on_status != 0 {
        return true;
    }
    if periods == 0 || device_ts.on_status_lb.is_empty() {
        return false;
    }
    device_ts.on_status_lb[0] != 0.0
}

/// Build the `CommitmentInitialCondition` for one device, including
/// the horizon-boundary override used when a unit is declared offline
/// before the horizon but the scenario's first-period must-run floor
/// forces it online.
pub(super) fn initial_condition_for_device(
    problem: &GoC3Problem,
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
) -> CommitmentInitialCondition {
    let periods = problem.time_series_input.general.time_periods;
    let initial_committed = device.initial_status.on_status != 0;
    let effective_committed = effective_initial_commitment(device, device_ts, periods);

    let mut ic = initial_condition_from_accumulated_times(
        device.uid.clone(),
        effective_committed,
        device.initial_status.accu_up_time,
        device.initial_status.accu_down_time,
    );

    if effective_committed {
        let accu_up = device.initial_status.accu_up_time;
        // Only emit `hours_on` when the pre-horizon on_status was true
        // and the accumulated time is positive.
        if initial_committed && accu_up > 0.0 {
            ic.hours_on = Some(accu_up.max(0.0).floor() as i32);
        } else {
            // For horizon-boundary override units, leave hours_on unset.
            ic.hours_on = None;
        }
        if initial_committed && accu_up < 24.0 {
            ic.starts_24h = Some(1);
        }
        if initial_committed && accu_up < 168.0 {
            ic.starts_168h = Some(1);
        }
        if device.initial_status.p > 0.0 {
            ic.energy_mwh_24h = Some(device.initial_status.p);
        }
        ic.offline_hours = None;
    } else {
        ic.hours_on = None;
        let accu_down = device.initial_status.accu_down_time.max(0.0);
        ic.offline_hours = Some(accu_down);
        if ic.starts_24h.is_none() {
            ic.starts_24h = Some(0);
        }
        if ic.starts_168h.is_none() {
            ic.starts_168h = Some(0);
        }
        if ic.energy_mwh_24h.is_none() {
            ic.energy_mwh_24h = Some(0.0);
        }
    }

    ic
}

/// Detect whether the producer needs a period-zero offline startup
/// restriction: its pre-horizon `on_status = 0`, its `p_startup_ramp_ub`
/// is too low to reach `p_lb[0]` in the first interval, and there is
/// no must-run floor keeping it online.
#[allow(dead_code)]
pub(super) fn requires_period_zero_offline_start_restriction(
    problem: &GoC3Problem,
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
) -> bool {
    let periods = problem.time_series_input.general.time_periods;
    if periods == 0 {
        return false;
    }
    if effective_initial_commitment(device, device_ts, periods) {
        return false;
    }
    if device.initial_status.on_status != 0 {
        return false;
    }
    let Some(&p_lb0) = device_ts.p_lb.first() else {
        return false;
    };
    let first_interval = problem
        .time_series_input
        .general
        .interval_duration
        .first()
        .copied()
        .unwrap_or(1.0);
    let startup_cap_pu = device.p_startup_ramp_ub * first_interval;
    startup_cap_pu + 1e-9 < p_lb0
}

/// Per-period derate factor for a producer's on_status upper bound
/// and capacity headroom.
///
/// Returns a value in `[0, 1]`:
///
/// * `0.0` when the unit is forced offline for this period.
/// * a tiny positive number (`1e-6`) when the unit can be online but
///   has zero active-power headroom this period.
/// * `p_ub_pu / pmax_pu` otherwise.
pub(super) fn generator_derate_factor(p_ub_pu: f64, pmax_pu: f64, on_status_ub: i32) -> f64 {
    if on_status_ub == 0 {
        return 0.0;
    }
    if pmax_pu <= 0.0 {
        return 1.0;
    }
    if p_ub_pu <= 0.0 {
        return 1.0e-6;
    }
    p_ub_pu / pmax_pu
}

/// True when the producer has zero real-power schedule (p_ub and p_lb
/// both effectively zero across all periods).
pub(super) fn is_zero_mw_producer(device_ts: &GoC3DeviceTimeSeries) -> bool {
    device_ts.p_ub.iter().all(|v| v.abs() <= 1e-9) && device_ts.p_lb.iter().all(|v| v.abs() <= 1e-9)
}
