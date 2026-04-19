// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared time-series profile application for dispatch network snapshots.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::network::Load;

use crate::common::spec::DispatchProblemSpec;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BusLoadTarget {
    pub active_power_mw: f64,
    pub reactive_power_mvar: f64,
    pub active_overridden: bool,
    pub reactive_overridden: bool,
}

pub(crate) fn apply_dc_time_series_profiles(
    net: &mut Network,
    spec: &DispatchProblemSpec<'_>,
    period: usize,
) {
    apply_dc_load_profiles(net, spec, period);
    apply_nonload_profiles(net, spec, period);
}

pub(crate) fn apply_ac_time_series_profiles(
    net: &mut Network,
    spec: &DispatchProblemSpec<'_>,
    period: usize,
) {
    apply_ac_bus_load_profiles(net, spec, period);
    apply_nonload_profiles(net, spec, period);
}

fn apply_dc_load_profiles(net: &mut Network, spec: &DispatchProblemSpec<'_>, period: usize) {
    if spec.load_profiles.profiles.is_empty() {
        return;
    }

    // Replace loads matching each profile's bus with the profile value for this period.
    for profile in &spec.load_profiles.profiles {
        if period < profile.load_mw.len() {
            net.loads.retain(|l| l.bus != profile.bus);
            net.loads
                .push(Load::new(profile.bus, profile.load_mw[period], 0.0));
        }
    }
}

fn apply_ac_bus_load_profiles(net: &mut Network, spec: &DispatchProblemSpec<'_>, period: usize) {
    let targets = fixed_bus_load_targets(net, spec, period);
    if targets.is_empty() {
        return;
    }

    let mut buses: Vec<u32> = targets.keys().copied().collect();
    buses.sort_unstable();
    for bus in buses {
        if let Some(target) = targets.get(&bus).copied() {
            apply_ac_bus_load_target(net, bus, target);
        }
    }
}

pub(crate) fn fixed_bus_withdrawals_mw(
    net: &Network,
    spec: &DispatchProblemSpec<'_>,
    period: usize,
) -> HashMap<u32, f64> {
    let mut withdrawals: HashMap<u32, f64> = base_in_service_bus_withdrawals(net);
    for (bus, target) in fixed_bus_load_targets(net, spec, period) {
        withdrawals.insert(bus, target.active_power_mw);
    }
    withdrawals
}

pub(crate) fn fixed_bus_withdrawals_mvar(
    net: &Network,
    spec: &DispatchProblemSpec<'_>,
    period: usize,
) -> HashMap<u32, f64> {
    let mut withdrawals: HashMap<u32, f64> =
        net.loads
            .iter()
            .filter(|load| load.in_service)
            .fold(HashMap::new(), |mut acc, load| {
                *acc.entry(load.bus).or_insert(0.0) += load.reactive_power_demand_mvar;
                acc
            });
    for (bus, target) in fixed_bus_load_targets(net, spec, period) {
        withdrawals.insert(bus, target.reactive_power_mvar);
    }
    withdrawals
}

pub(crate) fn fixed_bus_load_targets(
    net: &Network,
    spec: &DispatchProblemSpec<'_>,
    period: usize,
) -> HashMap<u32, BusLoadTarget> {
    let active_profile_by_bus = active_load_profile_by_bus(spec, period);
    let ac_profile_by_bus = ac_bus_profile_by_bus(spec, period);

    if active_profile_by_bus.is_empty() && ac_profile_by_bus.is_empty() {
        return HashMap::new();
    }

    let affected_buses: HashSet<u32> = active_profile_by_bus
        .keys()
        .copied()
        .chain(ac_profile_by_bus.keys().copied())
        .collect();

    affected_buses
        .into_iter()
        .map(|bus| {
            let (current_p_mw, current_q_mvar) = current_bus_load_totals(net, bus);
            let (override_p_mw, override_q_mvar) =
                ac_profile_by_bus.get(&bus).copied().unwrap_or((None, None));
            let active_override =
                override_p_mw.or_else(|| active_profile_by_bus.get(&bus).copied());
            let reactive_override = override_q_mvar;
            let target_p_mw = active_override.unwrap_or(current_p_mw);
            let target_q_mvar = if let Some(q_mvar) = reactive_override {
                q_mvar
            } else if active_override.is_some() {
                preserve_bus_power_factor(current_p_mw, current_q_mvar, target_p_mw)
            } else {
                current_q_mvar
            };

            (
                bus,
                BusLoadTarget {
                    active_power_mw: target_p_mw,
                    reactive_power_mvar: target_q_mvar,
                    active_overridden: active_override.is_some(),
                    reactive_overridden: reactive_override.is_some(),
                },
            )
        })
        .collect()
}

fn preserve_bus_power_factor(current_p_mw: f64, current_q_mvar: f64, target_p_mw: f64) -> f64 {
    if current_p_mw.abs() > 1e-9 {
        current_q_mvar * (target_p_mw / current_p_mw)
    } else if current_q_mvar.abs() > 1e-9 {
        current_q_mvar
    } else {
        0.0
    }
}

fn base_in_service_bus_withdrawals(net: &Network) -> HashMap<u32, f64> {
    let mut withdrawals = HashMap::new();
    for load in &net.loads {
        if load.in_service {
            *withdrawals.entry(load.bus).or_insert(0.0) += load.active_power_demand_mw;
        }
    }
    withdrawals
}

fn current_bus_load_totals(net: &Network, bus: u32) -> (f64, f64) {
    let current_p_mw = net
        .loads
        .iter()
        .filter(|load| load.bus == bus && load.in_service)
        .map(|load| load.active_power_demand_mw)
        .sum();
    let current_q_mvar = net
        .loads
        .iter()
        .filter(|load| load.bus == bus && load.in_service)
        .map(|load| load.reactive_power_demand_mvar)
        .sum();
    (current_p_mw, current_q_mvar)
}

fn active_load_profile_by_bus(spec: &DispatchProblemSpec<'_>, period: usize) -> HashMap<u32, f64> {
    spec.load_profiles
        .profiles
        .iter()
        .filter_map(|profile| {
            profile
                .load_mw
                .get(period)
                .copied()
                .map(|mw| (profile.bus, mw))
        })
        .collect()
}

fn ac_bus_profile_by_bus(
    spec: &DispatchProblemSpec<'_>,
    period: usize,
) -> HashMap<u32, (Option<f64>, Option<f64>)> {
    spec.ac_bus_load_profiles
        .profiles
        .iter()
        .map(|profile| {
            let p_mw = profile
                .p_mw
                .as_ref()
                .and_then(|values| values.get(period))
                .copied();
            let q_mvar = profile
                .q_mvar
                .as_ref()
                .and_then(|values| values.get(period))
                .copied();
            (profile.bus_number, (p_mw, q_mvar))
        })
        .collect()
}

fn apply_ac_bus_load_target(net: &mut Network, bus: u32, target: BusLoadTarget) {
    let load_indices: Vec<usize> = net
        .loads
        .iter()
        .enumerate()
        .filter_map(|(idx, load)| (load.bus == bus && load.in_service).then_some(idx))
        .collect();

    if load_indices.is_empty() {
        if target.active_power_mw.abs() > 1e-12 || target.reactive_power_mvar.abs() > 1e-12 {
            net.loads.push(Load::new(
                bus,
                target.active_power_mw,
                target.reactive_power_mvar,
            ));
        }
        return;
    }

    let original_p: Vec<f64> = load_indices
        .iter()
        .map(|&idx| net.loads[idx].active_power_demand_mw)
        .collect();
    let original_q: Vec<f64> = load_indices
        .iter()
        .map(|&idx| net.loads[idx].reactive_power_demand_mvar)
        .collect();
    let current_p_mw: f64 = original_p.iter().sum();

    if target.active_overridden && !target.reactive_overridden && current_p_mw.abs() > 1e-9 {
        let scale = target.active_power_mw / current_p_mw;
        for (load_idx, (active_power, reactive_power)) in load_indices
            .iter()
            .zip(original_p.iter().zip(original_q.iter()))
        {
            net.loads[*load_idx].active_power_demand_mw = active_power * scale;
            net.loads[*load_idx].reactive_power_demand_mvar = reactive_power * scale;
        }
        return;
    }

    if target.active_overridden {
        let target_active =
            scaled_or_distributed_values(&original_p, &original_q, target.active_power_mw);
        for (load_idx, value) in load_indices.iter().zip(target_active) {
            net.loads[*load_idx].active_power_demand_mw = value;
        }
    }

    if target.reactive_overridden {
        let target_reactive =
            scaled_or_distributed_values(&original_q, &original_p, target.reactive_power_mvar);
        for (load_idx, value) in load_indices.iter().zip(target_reactive) {
            net.loads[*load_idx].reactive_power_demand_mvar = value;
        }
    }
}

fn scaled_or_distributed_values(original: &[f64], fallback: &[f64], target_total: f64) -> Vec<f64> {
    let current_total: f64 = original.iter().sum();
    if current_total.abs() > 1e-9 {
        let scale = target_total / current_total;
        return original.iter().map(|value| value * scale).collect();
    }

    let fallback_total: f64 = fallback.iter().map(|value| value.abs()).sum();
    if fallback_total > 1e-9 {
        return fallback
            .iter()
            .map(|value| target_total * (value.abs() / fallback_total))
            .collect();
    }

    if original.is_empty() {
        return Vec::new();
    }

    let equal_share = target_total / original.len() as f64;
    vec![equal_share; original.len()]
}

fn apply_nonload_profiles(net: &mut Network, spec: &DispatchProblemSpec<'_>, period: usize) {
    if !spec.gen_derate_profiles.profiles.is_empty() || !spec.renewable_profiles.profiles.is_empty()
    {
        let gen_index_by_id = net.gen_index_by_id();

        for profile in &spec.gen_derate_profiles.profiles {
            if period >= profile.derate_factors.len() {
                continue;
            }
            let idx = gen_index_by_id.get(&profile.generator_id).copied();
            if let Some(idx) = idx {
                let generator = &mut net.generators[idx];
                let derate = profile.derate_factors[period].clamp(0.0, 1.0);
                generator.pmax *= derate;
                generator.pmin = generator.pmin.min(generator.pmax);
            }
        }

        for profile in &spec.renewable_profiles.profiles {
            if period >= profile.capacity_factors.len() {
                continue;
            }
            let idx = gen_index_by_id.get(&profile.generator_id).copied();
            if let Some(idx) = idx {
                let generator = &mut net.generators[idx];
                let capacity_factor = profile.capacity_factors[period];
                generator.pmax = generator.pmax.max(generator.pmin) * capacity_factor;
                generator.pmin = generator.pmin.min(generator.pmax);
                generator.p = generator.pmax;
            }
        }
    }

    if !spec.generator_dispatch_bounds.profiles.is_empty() {
        let gen_index_by_id = net.gen_index_by_id();
        for profile in &spec.generator_dispatch_bounds.profiles {
            let Some(&idx) = gen_index_by_id.get(profile.resource_id.as_str()) else {
                continue;
            };
            // Capture the physical envelope BEFORE overlaying the profile.
            // The profile is meant to NARROW the per-period operating
            // window, not widen beyond physical capability. When a pinning
            // helper (typically a winner-roundtrip band) emits P or Q
            // bounds outside `[pmin_phys, pmax_phys]` or
            // `[qmin_phys, qmax_phys]`, we clip back to the physical
            // envelope. Without this clip, the NLP happily assigns values
            // outside physical capability (often via Ipopt's
            // `bound_relax_factor`) which the exporter then clamps to
            // `p_ub[t]`/`q_ub[t]` — leaving the validator with a bus
            // balance residual that matches the phantom amount.
            let phys_pmin = net.generators[idx].pmin;
            let phys_pmax = net.generators[idx].pmax;
            let phys_qmin = net.generators[idx].qmin;
            let phys_qmax = net.generators[idx].qmax;
            let p_profile_min = profile.p_min_mw.get(period).copied().unwrap_or(phys_pmin);
            let p_profile_max = profile.p_max_mw.get(period).copied().unwrap_or(phys_pmax);
            let p_phys_lo = phys_pmin.min(phys_pmax);
            let p_phys_hi = phys_pmax.max(phys_pmin);
            let p_min_mw = p_profile_min.clamp(p_phys_lo, p_phys_hi);
            let p_max_mw = p_profile_max.clamp(p_phys_lo, p_phys_hi);
            let generator = &mut net.generators[idx];
            let upper = p_max_mw.max(p_min_mw);
            generator.pmax = upper;
            generator.pmin = p_min_mw.min(upper);
            generator.p = generator.p.clamp(generator.pmin, generator.pmax);
            let q_profile_min = profile
                .q_min_mvar
                .as_ref()
                .and_then(|series| series.get(period))
                .copied();
            let q_profile_max = profile
                .q_max_mvar
                .as_ref()
                .and_then(|series| series.get(period))
                .copied();
            if q_profile_min.is_some() || q_profile_max.is_some() {
                let q_min_mvar = q_profile_min.unwrap_or(phys_qmin);
                let q_max_mvar = q_profile_max.unwrap_or(phys_qmax);
                // Clip the profile's Q window into the physical envelope.
                // Use min/max so we handle inverted inputs gracefully.
                let phys_lo = phys_qmin.min(phys_qmax);
                let phys_hi = phys_qmax.max(phys_qmin);
                let clipped_min = q_min_mvar.clamp(phys_lo, phys_hi);
                let clipped_max = q_max_mvar.clamp(phys_lo, phys_hi);
                let reactive_upper = clipped_max.max(clipped_min);
                generator.qmax = reactive_upper;
                generator.qmin = clipped_min.min(reactive_upper);
                generator.q = generator.q.clamp(generator.qmin, generator.qmax);
            }
        }
    }

    if !spec.branch_derate_profiles.profiles.is_empty() {
        let branch_map = net.branch_index_map();
        for profile in &spec.branch_derate_profiles.profiles {
            if period >= profile.derate_factors.len() {
                continue;
            }
            let key = (profile.from_bus, profile.to_bus, profile.circuit.clone());
            if let Some(&idx) = branch_map.get(&key) {
                // Factor < 0 is rejected at validation time. 0 takes the
                // branch out of service; factor > 1 acts as an uprate
                // (relaxing the thermal limit, e.g. to absorb DC-stage
                // slack before AC SCED).
                let factor = profile.derate_factors[period].max(0.0);
                let branch = &mut net.branches[idx];
                if factor == 0.0 {
                    branch.in_service = false;
                } else {
                    branch.rating_a_mva *= factor;
                }
            }
        }
    }

    if !spec.hvdc_derate_profiles.profiles.is_empty() {
        let dc_line_by_name: HashMap<String, usize> = net
            .hvdc
            .links
            .iter()
            .enumerate()
            .filter_map(|(idx, link)| link.as_lcc().map(|dc| (dc.name.clone(), idx)))
            .collect();
        for profile in &spec.hvdc_derate_profiles.profiles {
            if period >= profile.derate_factors.len() {
                continue;
            }
            if let Some(&idx) = dc_line_by_name.get(profile.name.as_str()) {
                let derate = profile.derate_factors[period].clamp(0.0, 1.0);
                if let Some(dc_line) = net.hvdc.links[idx].as_lcc_mut() {
                    if derate == 0.0 {
                        dc_line.rectifier.in_service = false;
                        dc_line.inverter.in_service = false;
                    } else {
                        dc_line.scheduled_setpoint *= derate;
                    }
                }
            }
        }
    }
}
