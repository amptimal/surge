// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO C3 consumer-to-bus profile aggregation.
//!
//! Every GO C3 consumer has a `p_lb` (minimum consumption) time series
//! that is always served when the unit is online. The canonical market
//! treats that portion as fixed bus demand — it is not dispatchable —
//! so we aggregate it onto per-bus load profiles for the DC
//! formulation and onto `(p_mw, q_mvar)` AC bus-load profiles for the
//! AC formulation. Consumers also contribute an initial-status-derived
//! reactive injection that the adapter forwards via a fixed power
//! factor unless the unit has GO C3 `q_linear_cap=1` PQ linking, in
//! which case the reactive is computed downstream from the dispatchable
//! generator power.
//!
//! Output is a `(p_by_bus, q_by_bus)` pair keyed by Surge bus number.

use std::collections::HashMap;

use surge_io::go_c3::types::GoC3DeviceType;
use surge_io::go_c3::{GoC3Context, GoC3Device, GoC3Policy, GoC3Problem};

/// Aggregated per-bus fixed consumer profile in MW and MVAr.
#[allow(dead_code)]
pub(super) type BusProfiles = (HashMap<u32, Vec<f64>>, HashMap<u32, Vec<f64>>);

/// Pre-computed consumer fields used by the request builder and
/// reserve-zone-floor calculation.
pub(super) struct ConsumerAggregation {
    /// `p_by_bus_mw[bus_number]` — fixed consumption in MW per period.
    pub p_by_bus: HashMap<u32, Vec<f64>>,
    /// `q_by_bus_mvar[bus_number]` — fixed reactive injection in MVAr per period.
    pub q_by_bus: HashMap<u32, Vec<f64>>,
    /// Per-device fixed consumption in pu per period — used by the
    /// reserve-zone consumer-floor calculation.
    pub fixed_p_series_pu_by_uid: HashMap<String, Vec<f64>>,
}

/// Whether a consumer has GO C3 linear Q linking enabled (fields
/// `q_linear_cap` or `q_bound_cap` nonzero).
fn consumer_has_pq_linking(device: &GoC3Device) -> bool {
    (device.q_linear_cap as i32) == 1 || (device.q_bound_cap as i32) == 1
}

/// Return the pu time-series of "always consumed" bus load for a
/// consumer, honoring PQ linking when present.
fn fixed_bus_series_pu(
    device: &GoC3Device,
    device_ts: &surge_io::go_c3::GoC3DeviceTimeSeries,
    periods: usize,
) -> Vec<f64> {
    let mut p_lb: Vec<f64> = device_ts.p_lb.to_vec();
    if p_lb.len() < periods {
        p_lb.resize(periods, 0.0);
    }
    if !consumer_has_pq_linking(device) {
        return p_lb;
    }
    let base_floor = p_lb.iter().copied().fold(f64::INFINITY, f64::min);
    let base_floor = if base_floor.is_finite() {
        base_floor
    } else {
        0.0
    };
    p_lb.iter()
        .map(|&floor| (floor - base_floor).max(0.0))
        .collect()
}

/// Aggregate all consumers onto per-bus P and Q profiles.
pub(super) fn aggregate_consumer_bus_profiles(
    problem: &GoC3Problem,
    context: &GoC3Context,
    _policy: &GoC3Policy,
) -> ConsumerAggregation {
    let base_mva = problem.network.general.base_norm_mva;
    let periods = problem.time_series_input.general.time_periods;

    let device_ts_by_uid: HashMap<&str, &surge_io::go_c3::GoC3DeviceTimeSeries> = problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();

    let mut p_by_bus: HashMap<u32, Vec<f64>> = context
        .bus_number_to_uid
        .keys()
        .copied()
        .map(|bus_num| (bus_num, vec![0.0; periods]))
        .collect();
    let mut q_by_bus: HashMap<u32, Vec<f64>> = p_by_bus.clone();
    let mut fixed_p_series_pu_by_uid: HashMap<String, Vec<f64>> = HashMap::new();

    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Consumer {
            continue;
        }
        let Some(ts) = device_ts_by_uid.get(device.uid.as_str()) else {
            continue;
        };
        let Some(&bus_number) = context.bus_uid_to_number.get(&device.bus) else {
            continue;
        };
        let p_series_pu = fixed_bus_series_pu(device, ts, periods);
        let q_initial = device.initial_status.q;
        let p_initial = device.initial_status.p;
        // If PQ linking is active, the dispatchable generator handles
        // reactive; consumer's fixed MW contributes zero Q at the bus.
        let pf_ratio = if consumer_has_pq_linking(device) {
            0.0
        } else if p_initial.abs() > 1e-9 {
            q_initial / p_initial
        } else {
            0.0
        };
        let p_bus = p_by_bus
            .entry(bus_number)
            .or_insert_with(|| vec![0.0; periods]);
        let q_bus = q_by_bus
            .entry(bus_number)
            .or_insert_with(|| vec![0.0; periods]);
        for (i, &p_pu) in p_series_pu.iter().enumerate() {
            if i >= periods {
                break;
            }
            p_bus[i] += p_pu * base_mva;
            q_bus[i] += p_pu * pf_ratio * base_mva;
        }
        fixed_p_series_pu_by_uid.insert(device.uid.clone(), p_series_pu);
    }

    ConsumerAggregation {
        p_by_bus,
        q_by_bus,
        fixed_p_series_pu_by_uid,
    }
}

/// Field accessor on a `GoC3Device`: does this consumer carry PQ linking?
#[allow(dead_code)]
pub(super) fn consumer_has_go_pq_linking(device: &GoC3Device) -> bool {
    consumer_has_pq_linking(device)
}
