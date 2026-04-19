// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC dispatch warm-start builder.
//!
//! The AC SCED NLP needs a starting point close to the solution or
//! Ipopt runs out of iterations. This helper reads the source stage's
//! solution and produces a [`AcDispatchWarmStart`] consumable by the
//! dispatch runtime:
//!
//! * **Buses** — per-period voltage magnitude (pu) and angle (rad) read
//!   from `source_solution.periods[p].bus_results[*]`. When a bus is
//!   missing from the source, defaults to `(1.0 pu, 0 rad)`.
//! * **Generators** — per-period P/Q for every resource classified as
//!   `producer` or `producer_static` whose ID appears in the request's
//!   `generator_dispatch_bounds.profiles`.
//! * **Dispatchable loads** — per-period served P (and optional Q) for
//!   every consumer-block resource ID in the request's market.
//!
//! The adapter provides the [`AcWarmStartConfig`] mappings
//! (`bus_uid_to_number` is effectively the set of bus numbers to
//! seed; kept as a map for future sanity-check integrations).

use std::collections::{HashMap, HashSet};

use surge_dispatch::{
    DispatchRequest, DispatchSolution, ResourcePeriodDetail,
    request::{
        AcDispatchWarmStart, BusPeriodVoltageSeries, HvdcPeriodPowerSeries,
        ResourcePeriodPowerSeries,
    },
};

/// Adapter-provided data needed to build the AC warm start.
#[derive(Clone, Debug, Default)]
pub struct AcWarmStartConfig {
    /// Bus UIDs that should appear in the warm start, mapped to
    /// Surge bus numbers. The UIDs themselves aren't used inside the
    /// builder (buses are keyed by number); the map exists so the
    /// adapter can hand us the bus set in a single place.
    pub bus_uid_to_number: HashMap<String, u32>,

    /// Resource IDs classified as `producer` or `producer_static`.
    /// Only these generators' P/Q get warm-started.
    pub producer_resource_ids: HashSet<String>,

    /// Resource IDs classified as consumer-block (for formats that
    /// decompose physical consumers into per-block dispatchable
    /// loads). Only these dispatchable loads get warm-started.
    pub dispatchable_load_resource_ids: HashSet<String>,

    /// Per-consumer block decomposition — maps the consumer's UID to
    /// its block resource IDs in order. Used to locate the raw served
    /// MW in the source solution (which keys them by block ID).
    pub consumer_block_resource_ids_by_uid: HashMap<String, Vec<String>>,

    /// Per-consumer initial Q/P ratio, used to fill in Q when the
    /// source result doesn't carry it.
    pub consumer_q_to_p_ratio_by_uid: HashMap<String, f64>,
}

/// Build the AC dispatch warm start from a solved source stage.
///
/// The returned [`AcDispatchWarmStart`] is ready to assign to
/// `request.runtime_mut().ac_dispatch_warm_start`. HVDC warm-start
/// is not populated here — the canonical flow carries HVDC P through
/// `runtime.fixed_hvdc_dispatch` and terminal Q through synthetic
/// support generators, so no redundant warm-start is needed.
pub fn build_ac_dispatch_warm_start(
    request: &DispatchRequest,
    source_solution: &DispatchSolution,
    config: &AcWarmStartConfig,
) -> AcDispatchWarmStart {
    let periods_count = request.timeline().periods;
    let source_periods = source_solution.periods();

    // ── Buses ──────────────────────────────────────────────────────
    // Collect the bus numbers we want in the warm start, then read
    // vm/va from the source solution per period. Buses missing in a
    // given period default to (1.0 pu, 0 rad).
    let mut bus_numbers: Vec<u32> = config.bus_uid_to_number.values().copied().collect();
    bus_numbers.sort_unstable();
    bus_numbers.dedup();

    let per_period_bus_lookup: Vec<HashMap<u32, (f64, f64)>> = source_periods
        .iter()
        .map(|period| {
            period
                .bus_results()
                .iter()
                .map(|b| {
                    let vm = b.voltage_pu.unwrap_or(1.0);
                    let va = b.angle_rad.unwrap_or(0.0);
                    (b.bus_number, (vm, va))
                })
                .collect()
        })
        .collect();

    let mut bus_rows: Vec<BusPeriodVoltageSeries> = Vec::with_capacity(bus_numbers.len());
    for bus_number in bus_numbers {
        let mut vm_series = Vec::with_capacity(periods_count);
        let mut va_series = Vec::with_capacity(periods_count);
        for period_idx in 0..periods_count {
            let (vm, va) = per_period_bus_lookup
                .get(period_idx)
                .and_then(|m| m.get(&bus_number).copied())
                .unwrap_or((1.0, 0.0));
            vm_series.push(vm);
            va_series.push(va);
        }
        bus_rows.push(BusPeriodVoltageSeries {
            bus_number,
            vm_pu: vm_series,
            va_rad: va_series,
        });
    }

    // ── Generators ────────────────────────────────────────────────
    let per_period_resource_lookup: Vec<HashMap<&str, &surge_dispatch::ResourcePeriodResult>> =
        source_periods
            .iter()
            .map(|period| {
                period
                    .resource_results()
                    .iter()
                    .map(|r| (r.resource_id.as_str(), r))
                    .collect()
            })
            .collect();

    // Only emit warm-start rows for generators that appear in the
    // request's dispatch-bounds profiles. The dispatch runtime uses
    // the warm start to seed the NLP variables, and including rows
    // for gens not modeled in the request triggers a "no such
    // resource" mapping error.
    let request_generator_ids: HashSet<String> = request
        .profiles()
        .generator_dispatch_bounds
        .profiles
        .iter()
        .map(|p| p.resource_id.clone())
        .collect();

    let mut generator_rows: Vec<ResourcePeriodPowerSeries> = Vec::new();
    for resource_id in &config.producer_resource_ids {
        if !request_generator_ids.contains(resource_id) {
            continue;
        }
        let mut p_series = Vec::with_capacity(periods_count);
        let mut q_series = Vec::with_capacity(periods_count);
        for period_idx in 0..periods_count {
            let source_entry = per_period_resource_lookup
                .get(period_idx)
                .and_then(|m| m.get(resource_id.as_str()).copied());
            let p_mw = source_entry.map(|r| r.power_mw).unwrap_or(0.0);
            let q_mvar = source_entry
                .and_then(|r| match &r.detail {
                    ResourcePeriodDetail::Generator(g) => g.q_mvar,
                    ResourcePeriodDetail::Storage(s) => s.q_mvar,
                    _ => None,
                })
                .unwrap_or(0.0);
            p_series.push(p_mw);
            q_series.push(q_mvar);
        }
        // Only emit a q_mvar series if any entry is nonzero — keeps
        // the warm start compact for resources whose source stage
        // didn't clear Q (DC SCUC typically).
        let any_q = q_series.iter().any(|v| v.abs() > 1e-12);
        generator_rows.push(ResourcePeriodPowerSeries {
            resource_id: resource_id.clone(),
            p_mw: p_series,
            q_mvar: if any_q { q_series } else { Vec::new() },
        });
    }
    generator_rows.sort_by(|a, b| a.resource_id.cmp(&b.resource_id));

    // ── Dispatchable loads ────────────────────────────────────────
    let mut dispatchable_load_rows: Vec<ResourcePeriodPowerSeries> = Vec::new();
    for block_resource_ids in config.consumer_block_resource_ids_by_uid.values() {
        for block_resource_id in block_resource_ids {
            if !config
                .dispatchable_load_resource_ids
                .contains(block_resource_id)
            {
                continue;
            }
            let mut p_series = Vec::with_capacity(periods_count);
            let mut q_series = Vec::with_capacity(periods_count);
            for period_idx in 0..periods_count {
                let source_entry = per_period_resource_lookup
                    .get(period_idx)
                    .and_then(|m| m.get(block_resource_id.as_str()).copied());
                let p_mw = source_entry
                    .map(|r| match &r.detail {
                        ResourcePeriodDetail::DispatchableLoad(d) => d.served_p_mw,
                        _ => (-r.power_mw).max(0.0),
                    })
                    .unwrap_or(0.0);
                let q_mvar = source_entry
                    .and_then(|r| match &r.detail {
                        ResourcePeriodDetail::DispatchableLoad(d) => d.served_q_mvar,
                        _ => None,
                    })
                    .unwrap_or(0.0);
                p_series.push(p_mw);
                q_series.push(q_mvar);
            }
            let any_q = q_series.iter().any(|v| v.abs() > 1e-12);
            dispatchable_load_rows.push(ResourcePeriodPowerSeries {
                resource_id: block_resource_id.clone(),
                p_mw: p_series,
                q_mvar: if any_q { q_series } else { Vec::new() },
            });
        }
    }
    dispatchable_load_rows.sort_by(|a, b| a.resource_id.cmp(&b.resource_id));

    AcDispatchWarmStart {
        buses: bus_rows,
        generators: generator_rows,
        dispatchable_loads: dispatchable_load_rows,
        hvdc_links: Vec::<HvdcPeriodPowerSeries>::new(),
    }
}
