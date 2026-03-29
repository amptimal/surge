// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Q-limit enforcement for the Newton-Raphson solver.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::network::BusType;
use tracing::{debug, error, warn};

use super::nr_options::{AcPfError, AcPfOptions, QSharingMode, SlackAttributionMode};

/// Per-generator reactive power limit state.
#[derive(Clone, Debug)]
pub(crate) struct GenQState {
    pub(crate) qmin_pu: f64,    // Generator Qmin in per-unit
    pub(crate) qmax_pu: f64,    // Generator Qmax in per-unit
    pub(crate) mbase: f64,      // Machine base MVA (for Mbase sharing mode)
    pub(crate) is_fixed: bool,  // Has been clamped to a Q limit
    pub(crate) fixed_q_pu: f64, // Q injection when fixed (qmin_pu or qmax_pu)
}

/// Build per-bus Q limit enforcement data for in-service generators on PV buses.
///
/// Returns a map: `bus_index → (q_spec_at_qmin_pu, q_spec_at_qmax_pu)`.
///
/// - `q_spec_at_qmin_pu` = total_qmin - qd : bus-level Q injection when generator at min limit
/// - `q_spec_at_qmax_pu` = total_qmax - qd : bus-level Q injection when generator at max limit
///
/// These values match what `q_calc[i]` (total bus Q injection) should equal when the
/// generator hits its limit.  Comparison: `q_calc[i] > q_spec_at_qmax` → switch PV→PQ.
/// Reclassify PV buses with no in-service generators as PQ.
///
/// A PV bus with no live generator cannot regulate voltage — it has zero reactive
/// capability.  MATPOWER converts such buses to PQ before the first NR pass.
/// Without this, Surge holds the voltage at the case-file setpoint indefinitely,
/// producing incorrect results on cases with offline synchronous condensers.
///
/// Multiple generators at the same bus: limits are summed.
/// If any in-service generator at a voltage-regulated bus (PV or Slack) has
/// infinite Q bounds, the bus is effectively unconstrained and is excluded from
/// Q-limit enforcement (matching MATPOWER behaviour where the aggregate
/// Qmax/Qmin must be finite).
///
/// Includes both PV and Slack buses — MATPOWER enforces Q-limits on the slack
/// bus and can demote it to PQ, selecting a new slack from remaining PV buses.
///
/// Generators with Qmax == Qmin (degenerate range) are included: in MATPOWER,
/// these immediately trigger a violation because computed Qg almost never
/// exactly equals the fixed limit.
pub(crate) fn collect_q_limits(network: &Network) -> HashMap<usize, (f64, f64)> {
    let bus_map = network.bus_index_map();
    let base = network.base_mva;
    let bus_qd = network.bus_load_q_mvar();

    // First pass: identify regulated buses where ANY in-service generator has infinite Q bounds.
    let mut bus_has_infinite_q: HashMap<usize, bool> = HashMap::new();
    for g in &network.generators {
        if !g.in_service {
            continue;
        }
        if let Some(&idx) = bus_map.get(&g.bus) {
            let bt = network.buses[idx].bus_type;
            if bt != BusType::PV && bt != BusType::Slack {
                continue;
            }
            if !g.qmax.is_finite() || !g.qmin.is_finite() {
                bus_has_infinite_q.insert(idx, true);
            }
        }
    }

    // Second pass: build limits for regulated buses where all generators have finite bounds.
    let mut limits: HashMap<usize, (f64, f64)> = HashMap::new();
    for g in &network.generators {
        if !g.in_service {
            continue;
        }
        if let Some(&idx) = bus_map.get(&g.bus) {
            let bt = network.buses[idx].bus_type;
            if bt != BusType::PV && bt != BusType::Slack {
                continue;
            }
            // Skip entire bus if any generator has infinite Q limits
            if bus_has_infinite_q.get(&idx).copied().unwrap_or(false) {
                continue;
            }
            let qmax = g.qmax / base;
            let qmin = g.qmin / base;
            let qd_pu = bus_qd.get(idx).copied().unwrap_or(0.0) / base;
            let e = limits.entry(idx).or_insert((-qd_pu, -qd_pu));
            e.0 += qmin; // q_spec_at_qmin = sum(qmin) - qd
            e.1 += qmax; // q_spec_at_qmax = sum(qmax) - qd
        }
    }
    limits
}

/// Compute per-unit Q limits at a given real power output from a generator's D-curve.
///
/// When the generator has a non-empty `pq_curve`, performs piecewise-linear
/// interpolation at `p_mw` to obtain P-dependent reactive limits. When the curve
/// is absent or `p_mw` lies outside the domain, falls back to the flat `[qmin, qmax]`
/// bounds (clamped to the curve endpoints).
///
/// Returns `(qmin_pu, qmax_pu)` in per-unit on system base.
pub(crate) fn q_limits_at_p(
    g: &surge_network::network::Generator,
    p_mw: f64,
    base_mva: f64,
) -> (f64, f64) {
    let empty_pq: Vec<(f64, f64, f64)> = Vec::new();
    let curve = g
        .reactive_capability
        .as_ref()
        .map(|r| &r.pq_curve)
        .unwrap_or(&empty_pq);
    if curve.is_empty() {
        return (g.qmin / base_mva, g.qmax / base_mva);
    }
    let p_first = curve.first().expect("curve is non-empty").0;
    let p_last = curve.last().expect("curve is non-empty").0;
    let (p_lo, p_hi) = if p_first <= p_last {
        (p_first, p_last)
    } else {
        (p_last, p_first)
    };
    let p_pu = (p_mw / base_mva).clamp(p_lo, p_hi);

    let seg = curve.windows(2).find(|w| w[0].0 <= p_pu && p_pu <= w[1].0);
    let (qmin_pu, qmax_pu) = if let Some(seg) = seg {
        let (p1, qmax1, qmin1) = seg[0];
        let (p2, qmax2, qmin2) = seg[1];
        let dp: f64 = p2 - p1;
        if dp.abs() < 1e-12 {
            (qmin1, qmax1)
        } else {
            let t = (p_pu - p1) / dp;
            (qmin1 + t * (qmin2 - qmin1), qmax1 + t * (qmax2 - qmax1))
        }
    } else {
        // Single-point or out-of-range after clamp — use nearest endpoint.
        if p_pu <= curve.first().expect("curve is non-empty").0 {
            let (_, qmax, qmin) = curve.first().expect("curve is non-empty");
            (*qmin, *qmax)
        } else {
            let (_, qmax, qmin) = curve.last().expect("curve is non-empty");
            (*qmin, *qmax)
        }
    };
    // Clamp to nameplate rectangular limits as a safety net.
    let qmax_pu: f64 = qmax_pu.min(g.qmax / base_mva);
    let qmin_pu: f64 = qmin_pu.max(g.qmin / base_mva);
    (qmin_pu, qmax_pu)
}

/// Build per-generator Q-limit states and a bus→generator index map.
///
/// Returns:
/// - `gen_states`: Per-generator Q-limit tracking for all in-service generators
///   at PV/Slack buses with finite Q limits.
/// - `bus_gen_map`: Maps bus_idx → list of indices into `gen_states`.
/// - `buses_with_inf_q`: Set of bus indices that have at least one generator with
///   infinite Q limits (these buses can never be Q-limited).
pub(crate) fn build_gen_q_states(
    network: &Network,
    remote_reg_terminals: &HashSet<usize>,
) -> (
    Vec<GenQState>,
    HashMap<usize, Vec<usize>>,
    std::collections::HashSet<usize>,
) {
    let bus_map = network.bus_index_map();
    let base = network.base_mva;

    let mut buses_with_inf_q = std::collections::HashSet::new();
    let mut gen_states = Vec::new();
    let mut bus_gen_map: HashMap<usize, Vec<usize>> = HashMap::new();

    // First pass: identify buses with any infinite-Q generator.
    for g in &network.generators {
        if !g.in_service {
            continue;
        }
        if let Some(&idx) = bus_map.get(&g.bus) {
            let bt = network.buses[idx].bus_type;
            let is_eligible =
                bt == BusType::PV || bt == BusType::Slack || remote_reg_terminals.contains(&idx);
            if !is_eligible {
                continue;
            }
            if !g.qmax.is_finite() || !g.qmin.is_finite() {
                buses_with_inf_q.insert(idx);
            }
        }
    }

    // Second pass: build per-generator states for buses with all-finite limits.
    for g in &network.generators {
        if !g.in_service {
            continue;
        }
        if let Some(&idx) = bus_map.get(&g.bus) {
            let bt = network.buses[idx].bus_type;
            let is_eligible =
                bt == BusType::PV || bt == BusType::Slack || remote_reg_terminals.contains(&idx);
            if !is_eligible {
                continue;
            }
            if buses_with_inf_q.contains(&idx) {
                continue;
            }
            if !g.qmax.is_finite() || !g.qmin.is_finite() {
                continue;
            }
            let gen_state_idx = gen_states.len();
            let (qmin_pu, qmax_pu) = q_limits_at_p(g, g.p, base);
            gen_states.push(GenQState {
                qmin_pu,
                qmax_pu,
                mbase: g.machine_base_mva,
                is_fixed: false,
                fixed_q_pu: 0.0,
            });
            bus_gen_map.entry(idx).or_default().push(gen_state_idx);
        }
    }

    (gen_states, bus_gen_map, buses_with_inf_q)
}

/// Per-generator Q-limit enforcement with one-bus-at-a-time switching.
///
/// Two-phase approach:
///   **Phase 1 — Generator fixing** (cheap, no NR re-run needed):
///   For each regulated bus, distributes the total Q among generators
///   proportionally to capacity range and fixes violators at their limits.
///
///   **Phase 2 — Bus switching** (one at a time for stability):
///   Among buses where ALL generators are now fixed (bus must become PQ),
///   switches only the single worst violator.
///
/// Returns the number of **bus type changes** (0 or 1 per call), or
/// `AcPfError::NoSlackBus` if all PV buses were Q-limited and no slack remains.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_per_gen_q_limits(
    bus_types: &mut [BusType],
    q_calc: &[f64],
    gen_states: &mut [GenQState],
    bus_gen_map: &HashMap<usize, Vec<usize>>,
    q_spec: &mut [f64],
    switched_to_pq: &mut [bool],
    network: &Network,
    bus_qd: &[f64],
    remote_reg_terminals: &HashSet<usize>,
    skip_slack: bool,
    incremental: bool,
    q_sharing: QSharingMode,
) -> Result<u32, AcPfError> {
    let base = network.base_mva;

    // Phase 1: distribute Q and fix violating generators at all buses.
    for (&bus_idx, gen_indices) in bus_gen_map {
        if switched_to_pq[bus_idx] {
            continue;
        }
        // Skip the slack bus when requested (CPF mode).
        if skip_slack && bus_types[bus_idx] == BusType::Slack {
            continue;
        }
        // Include PV, Slack, and remote-regulating terminal buses (demoted to PQ).
        let is_eligible = matches!(bus_types[bus_idx], BusType::PV | BusType::Slack)
            || remote_reg_terminals.contains(&bus_idx);
        if !is_eligible {
            continue;
        }

        // Collect free (unfixed) generators at this bus.
        let mut free_indices: Vec<usize> = gen_indices
            .iter()
            .copied()
            .filter(|&gi| !gen_states[gi].is_fixed)
            .collect();

        if free_indices.is_empty() {
            continue;
        }

        // Total Q generation = q_calc (net injection) + Qload
        let qd_pu = bus_qd.get(bus_idx).copied().unwrap_or(0.0) / base;
        let total_qgen = q_calc[bus_idx] + qd_pu;

        // Q already consumed by fixed generators.
        let fixed_q: f64 = gen_indices
            .iter()
            .filter(|&&gi| gen_states[gi].is_fixed)
            .map(|&gi| gen_states[gi].fixed_q_pu)
            .sum();

        // Local fixpoint: distribute remaining Q, fix violators, repeat.
        let mut remaining_for_free = total_qgen - fixed_q;
        let mut newly_fixed_any = true;
        let mut local_iter = 0;

        while newly_fixed_any && !free_indices.is_empty() && local_iter < 50 {
            newly_fixed_any = false;
            local_iter += 1;

            // Compute per-generator weight based on sharing mode.
            let weights: Vec<f64> = free_indices
                .iter()
                .map(|&gi| match q_sharing {
                    QSharingMode::Capability => {
                        (gen_states[gi].qmax_pu - gen_states[gi].qmin_pu).max(1e-12)
                    }
                    QSharingMode::Mbase => gen_states[gi].mbase.max(1e-12),
                    QSharingMode::Equal => 1.0,
                })
                .collect();
            let total_weight: f64 = weights.iter().sum();
            let sum_qmin: f64 = free_indices.iter().map(|&gi| gen_states[gi].qmin_pu).sum();
            let excess = remaining_for_free - sum_qmin;

            // Match MATPOWER's Q-limit violation tolerance: opf.violation = 5e-6 MVA
            // converted to per-unit.
            let q_tol = 5e-6 / base;

            let mut new_free = Vec::new();
            for (fi, &gi) in free_indices.iter().enumerate() {
                let qmin = gen_states[gi].qmin_pu;
                let qmax = gen_states[gi].qmax_pu;
                let w = weights[fi];
                let gen_q = qmin + excess * (w / total_weight);

                if gen_q > qmax + q_tol {
                    gen_states[gi].is_fixed = true;
                    gen_states[gi].fixed_q_pu = qmax;
                    remaining_for_free -= qmax;
                    newly_fixed_any = true;
                } else if gen_q < qmin - q_tol {
                    gen_states[gi].is_fixed = true;
                    gen_states[gi].fixed_q_pu = qmin;
                    remaining_for_free -= qmin;
                    newly_fixed_any = true;
                } else {
                    new_free.push(gi);
                }
            }
            free_indices = new_free;
        }
    }

    // Phase 2: switch buses (PV and Slack) where ALL generators are now fixed.
    let mut candidates: Vec<(usize, f64)> = Vec::new(); // (bus_idx, |violation|)

    for (&bus_idx, gen_indices) in bus_gen_map {
        if switched_to_pq[bus_idx] {
            continue;
        }
        // Skip the slack bus when requested (CPF mode).
        if skip_slack && bus_types[bus_idx] == BusType::Slack {
            continue;
        }
        // Include PV, Slack, and remote-regulating terminal buses (already PQ).
        let is_eligible = matches!(bus_types[bus_idx], BusType::PV | BusType::Slack)
            || remote_reg_terminals.contains(&bus_idx);
        if !is_eligible {
            continue;
        }
        let all_fixed = gen_indices.iter().all(|&gi| gen_states[gi].is_fixed);
        if !all_fixed {
            continue;
        }

        // Compute Q violation magnitude for ranking.
        let qd_pu = bus_qd.get(bus_idx).copied().unwrap_or(0.0) / base;
        let total_qgen = q_calc[bus_idx] + qd_pu;
        let qmax_total: f64 = gen_indices.iter().map(|&gi| gen_states[gi].qmax_pu).sum();
        let qmin_total: f64 = gen_indices.iter().map(|&gi| gen_states[gi].qmin_pu).sum();
        let violation = if total_qgen > qmax_total {
            total_qgen - qmax_total
        } else if total_qgen < qmin_total {
            qmin_total - total_qgen
        } else {
            0.0
        };

        candidates.push((bus_idx, violation));
    }

    // In incremental mode, sort by violation descending and take only the worst.
    if incremental && candidates.len() > 1 {
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(1);
    }

    let mut n_bus_switches = 0u32;
    let mut slack_was_demoted = false;

    for &(bus_idx, _) in &candidates {
        let gen_indices = &bus_gen_map[&bus_idx];

        if bus_types[bus_idx] == BusType::Slack {
            slack_was_demoted = true;
        }

        // Switch bus to PQ (no-op for remote-reg terminals already PQ).
        bus_types[bus_idx] = BusType::PQ;
        switched_to_pq[bus_idx] = true;

        let qd_pu = bus_qd.get(bus_idx).copied().unwrap_or(0.0) / base;
        let total_fixed_q: f64 = gen_indices
            .iter()
            .map(|&gi| gen_states[gi].fixed_q_pu)
            .sum();
        q_spec[bus_idx] = total_fixed_q - qd_pu;

        n_bus_switches += 1;
    }

    // If the Slack bus was demoted to PQ, promote the first remaining PV bus to Slack
    // (matching MATPOWER's bustypes() fallback behaviour).
    if slack_was_demoted {
        warn!("Q-limit enforcement demoted slack bus to PQ, reassigning slack");
        let n = bus_types.len();
        let mut found_new_slack = false;
        for j in 0..n {
            if bus_types[j] == BusType::PV && !switched_to_pq[j] {
                bus_types[j] = BusType::Slack;
                debug!(new_slack_bus_idx = j, "reassigned slack bus");
                found_new_slack = true;
                break;
            }
        }
        if !found_new_slack {
            error!("all PV buses Q-limited and demoted — no slack bus remains");
            return Err(AcPfError::NoSlackBus);
        }
    }

    if n_bus_switches > 0 {
        let n_fixed_gens: usize = gen_states.iter().filter(|g| g.is_fixed).count();
        debug!(
            bus_switches = n_bus_switches,
            fixed_gens = n_fixed_gens,
            total_gens = gen_states.len(),
            "Q-limit enforcement: PV→PQ switching"
        );
    }

    Ok(n_bus_switches)
}

/// Metadata collected by the NR solver for enriching `PfSolution`.
#[derive(Default)]
pub(crate) struct NrMeta {
    pub(crate) island_ids: Vec<usize>,
    pub(crate) q_limited_buses: Vec<u32>,
    pub(crate) n_q_limit_switches: u32,
    pub(crate) gen_slack_contribution_mw: Vec<f64>,
}

/// Build `NrMeta` with Q-limit and distributed-slack metadata for a completed solve.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_nr_meta(
    network: &Network,
    bus_numbers: &[u32],
    switched_to_pq: &[bool],
    n_q_limit_switches: u32,
    participation: &Option<HashMap<usize, f64>>,
    lambda: f64,
    options: &AcPfOptions,
) -> NrMeta {
    // AC-02: collect external bus numbers of Q-limited buses.
    let q_limited_buses: Vec<u32> = switched_to_pq
        .iter()
        .enumerate()
        .filter_map(|(i, &sq)| {
            if sq {
                bus_numbers.get(i).copied()
            } else {
                None
            }
        })
        .collect();

    // AC-03: compute per-generator slack contribution in MW.
    let gen_slack_contribution_mw = if let Some(pmap) = participation {
        let base_mva = network.base_mva;
        let bus_map = network.bus_index_map();
        let mut generators_by_bus: HashMap<usize, Vec<usize>> = HashMap::new();
        for (gen_idx, generator) in network.generators.iter().enumerate() {
            if !generator.in_service {
                continue;
            }
            if let Some(&bus_idx) = bus_map.get(&generator.bus) {
                generators_by_bus.entry(bus_idx).or_default().push(gen_idx);
            }
        }

        let direction = if lambda >= 0.0 {
            SlackDirection::Upward
        } else {
            SlackDirection::Downward
        };
        let mut contributions = vec![0.0; network.generators.len()];

        for (&bus_idx, &alpha) in pmap {
            let bus_share_mw = alpha * lambda * base_mva;
            if bus_share_mw.abs() < 1e-12 {
                continue;
            }
            let Some(generator_indices) = generators_by_bus.get(&bus_idx) else {
                continue;
            };
            let weights =
                select_generator_slack_weights(network, generator_indices, direction, options);
            let total_weight: f64 = weights.iter().map(|(_, weight)| *weight).sum();
            if total_weight < 1e-12 {
                let equal_share = bus_share_mw / generator_indices.len() as f64;
                for &gen_idx in generator_indices {
                    contributions[gen_idx] = equal_share;
                }
                continue;
            }
            for (gen_idx, weight) in weights {
                contributions[gen_idx] = bus_share_mw * weight / total_weight;
            }
        }

        contributions
    } else {
        Vec::new()
    };

    NrMeta {
        island_ids: Vec::new(), // populated by caller (solve_ac_pf)
        q_limited_buses,
        n_q_limit_switches,
        gen_slack_contribution_mw,
    }
}

#[derive(Clone, Copy)]
enum SlackDirection {
    Upward,
    Downward,
}

fn select_generator_slack_weights(
    network: &Network,
    generator_indices: &[usize],
    direction: SlackDirection,
    options: &AcPfOptions,
) -> Vec<(usize, f64)> {
    if let Some(weights) =
        collect_generator_slack_weights(network, generator_indices, direction, options, true)
    {
        return weights;
    }

    let policy_weights =
        collect_generator_slack_weights(network, generator_indices, direction, options, false)
            .unwrap_or_default();
    if !policy_weights.is_empty() {
        return policy_weights;
    }

    generator_indices
        .iter()
        .map(|&gen_idx| (gen_idx, 1.0))
        .collect()
}

fn collect_generator_slack_weights(
    network: &Network,
    generator_indices: &[usize],
    direction: SlackDirection,
    options: &AcPfOptions,
    explicit_only: bool,
) -> Option<Vec<(usize, f64)>> {
    let try_explicit = |generator_indices: &[usize]| -> Vec<(usize, f64)> {
        options
            .generator_slack_participation
            .as_ref()
            .map(|weights| {
                generator_indices
                    .iter()
                    .filter_map(|&gen_idx| {
                        weights.get(&gen_idx).copied().and_then(|weight| {
                            (weight.is_finite() && weight > 0.0).then_some((gen_idx, weight))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    if explicit_only {
        let weights = try_explicit(generator_indices);
        return (!weights.is_empty()).then_some(weights);
    }

    let candidates: Vec<SlackAttributionMode> = match options.slack_attribution {
        SlackAttributionMode::Automatic => vec![
            SlackAttributionMode::AgcParticipation,
            SlackAttributionMode::RegulationRamp,
            SlackAttributionMode::DirectionalHeadroom,
            SlackAttributionMode::EqualShare,
        ],
        mode => vec![mode],
    };

    for mode in candidates {
        let weights: Vec<(usize, f64)> = match mode {
            SlackAttributionMode::Automatic => unreachable!(),
            SlackAttributionMode::AgcParticipation => generator_indices
                .iter()
                .filter_map(|&gen_idx| {
                    let generator = &network.generators[gen_idx];
                    generator
                        .agc_participation_factor
                        .filter(|weight| weight.is_finite() && *weight > 0.0)
                        .map(|weight| (gen_idx, weight))
                })
                .collect(),
            SlackAttributionMode::RegulationRamp => generator_indices
                .iter()
                .filter_map(|&gen_idx| {
                    regulation_ramp_weight(&network.generators[gen_idx], direction)
                        .filter(|weight| *weight > 0.0)
                        .map(|weight| (gen_idx, weight))
                })
                .collect(),
            SlackAttributionMode::DirectionalHeadroom => generator_indices
                .iter()
                .filter_map(|&gen_idx| {
                    let weight = directional_headroom_mw(&network.generators[gen_idx], direction);
                    (weight > 0.0).then_some((gen_idx, weight))
                })
                .collect(),
            SlackAttributionMode::EqualShare => generator_indices
                .iter()
                .map(|&gen_idx| (gen_idx, 1.0))
                .collect(),
        };
        if !weights.is_empty() {
            return Some(weights);
        }
    }

    None
}

fn directional_headroom_mw(
    generator: &surge_network::network::Generator,
    direction: SlackDirection,
) -> f64 {
    match direction {
        SlackDirection::Upward => (generator.pmax - generator.p).max(0.0),
        SlackDirection::Downward => (generator.p - generator.pmin).max(0.0),
    }
}

fn regulation_ramp_weight(
    generator: &surge_network::network::Generator,
    direction: SlackDirection,
) -> Option<f64> {
    let ramp = match direction {
        SlackDirection::Upward => generator
            .reg_ramp_up_at_mw(generator.p)
            .or_else(|| generator.ramp_up_at_mw(generator.p)),
        SlackDirection::Downward => generator
            .reg_ramp_down_at_mw(generator.p)
            .or_else(|| generator.ramp_down_at_mw(generator.p)),
    }?;
    Some(
        ramp.max(0.0)
            .min(directional_headroom_mw(generator, direction)),
    )
}
