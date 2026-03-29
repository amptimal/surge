// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Area interchange enforcement helpers for the Newton-Raphson solver.

use std::collections::HashMap;

use surge_network::Network;
use surge_solution::PfSolution;
use tracing::{debug, warn};

use super::nr_options::{AcPfError, AcPfOptions, WarmStart};

/// Compute net scheduled interchange per area, incorporating bilateral transfers.
///
/// net_scheduled\[area\] = area.p_desired_mw
///     + sum(xfer.p_transfer_mw where xfer.from_area == area)
///     - sum(xfer.p_transfer_mw where xfer.to_area == area)
pub(crate) fn compute_net_scheduled(net: &Network) -> HashMap<u32, f64> {
    let mut scheduled: HashMap<u32, f64> = HashMap::new();
    for ai in &net.area_schedules {
        scheduled.insert(ai.number, ai.p_desired_mw);
    }
    for xfer in &net.metadata.scheduled_area_transfers {
        *scheduled.entry(xfer.from_area).or_insert(0.0) += xfer.p_transfer_mw;
        *scheduled.entry(xfer.to_area).or_insert(0.0) -= xfer.p_transfer_mw;
    }
    scheduled
}

/// Compute actual net interchange per area from solved tie-line flows.
pub(crate) fn compute_area_actual_interchange(
    sol: &PfSolution,
    net: &Network,
    bus_map: &HashMap<u32, usize>,
    bus_area: &[u32],
) -> HashMap<u32, f64> {
    let pq_flows = sol.branch_pq_flows();
    let mut area_actual_mw: HashMap<u32, f64> = HashMap::new();

    for (i, br) in net.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        let fi = match bus_map.get(&br.from_bus) {
            Some(&idx) => idx,
            None => continue,
        };
        let ti = match bus_map.get(&br.to_bus) {
            Some(&idx) => idx,
            None => continue,
        };
        let from_area = bus_area[fi];
        let to_area = bus_area[ti];
        if from_area != to_area {
            let (p_mw, _) = pq_flows[i];
            *area_actual_mw.entry(from_area).or_insert(0.0) += p_mw;
            *area_actual_mw.entry(to_area).or_insert(0.0) -= p_mw;
        }
    }

    area_actual_mw
}

/// Distribute an error (MW) across generators using APF weights.
///
/// Returns the total MW actually absorbed (may be less than `error` if
/// generators hit their limits).
pub(crate) fn distribute_error_apf(
    generators: &mut [surge_network::network::Generator],
    apf_gens: &[(usize, f64)],
    error: f64,
) -> f64 {
    // Iterative clamp-and-redistribute: when a unit hits a limit, redistribute
    // its unabsorbed share to remaining units proportionally.
    let mut remaining = error;
    let mut active: Vec<(usize, f64)> = apf_gens.to_vec();
    let mut total_absorbed = 0.0;

    for _round in 0..10 {
        if active.is_empty() || remaining.abs() < 1e-6 {
            break;
        }
        let sum_apf: f64 = active.iter().map(|(_, a)| a).sum();
        if sum_apf < 1e-12 {
            break;
        }

        let mut next_active = Vec::new();
        let mut round_absorbed = 0.0;

        for &(gi, apf) in &active {
            let share = remaining * (apf / sum_apf);
            let g = &mut generators[gi];
            let new_pg = (g.p + share).clamp(g.pmin, g.pmax);
            let delta = new_pg - g.p;
            g.p = new_pg;
            round_absorbed += delta;

            // If the unit didn't absorb its full share, it's at a limit.
            if (delta - share).abs() > 1e-6 {
                // Don't include in next round — it's clamped.
            } else {
                next_active.push((gi, apf));
            }
        }

        total_absorbed += round_absorbed;
        remaining -= round_absorbed;
        active = next_active;
    }

    total_absorbed
}

/// Solve AC power flow with area interchange enforcement outer loop.
///
/// This function contains the interchange outer loop extracted from `solve_ac_pf`.
pub(crate) fn solve_ac_pf_with_interchange(
    network: &Network,
    options: &AcPfOptions,
) -> Result<PfSolution, AcPfError> {
    use surge_solution::{AreaDispatchMethod, AreaInterchangeEntry, AreaInterchangeResult};

    // --- Interaction warnings ---
    if options.distributed_slack {
        warn!(
            "enforce_interchange and distributed_slack are both enabled; \
             distributed slack will redistribute generation globally after \
             each area interchange adjustment — results may not converge \
             to desired area targets"
        );
    }
    if options.slack_participation.is_some() {
        warn!(
            "enforce_interchange overrides slack_participation for area-level \
             generation adjustments; explicit participation factors are \
             applied only within the NR inner loop"
        );
    }

    let net_scheduled = compute_net_scheduled(network);
    let mut net = network.clone();
    let mut sol = crate::ac_dc::solve_ac_pf_with_dc_lines(&net, options)?;
    let mut final_iter = 0usize;
    let mut all_converged = false;

    // Identify the system swing bus.
    let system_swing_bus: Option<u32> = net
        .buses
        .iter()
        .find(|b| b.bus_type == surge_network::network::BusType::Slack)
        .map(|b| b.number);

    for outer in 0..options.interchange_max_iter {
        final_iter = outer;
        let bus_map = net.bus_index_map();
        let bus_area: Vec<u32> = net.buses.iter().map(|b| b.area).collect();

        // Compute actual net interchange per area from solved branch PQ flows.
        let area_actual_mw = compute_area_actual_interchange(&sol, &net, &bus_map, &bus_area);

        // Check convergence and compute adjustments.
        let mut converged = true;
        for ai in &net.area_schedules {
            let scheduled = net_scheduled.get(&ai.number).copied().unwrap_or(0.0);
            let actual = area_actual_mw.get(&ai.number).copied().unwrap_or(0.0);
            let error = scheduled - actual;
            let tol = if ai.p_tolerance_mw > 0.0 {
                ai.p_tolerance_mw
            } else {
                1.0 // default 1 MW tolerance
            };

            if error.abs() <= tol {
                continue;
            }
            converged = false;

            // Tier 1: APF-weighted regulating generators across the area.
            let apf_gens: Vec<(usize, f64)> = net
                .generators
                .iter()
                .enumerate()
                .filter(|(_, g)| {
                    g.in_service
                        && Some(g.bus) != system_swing_bus
                        && bus_area
                            .get(bus_map.get(&g.bus).copied().unwrap_or(usize::MAX))
                            .copied()
                            == Some(ai.number)
                        && g.agc_participation_factor.unwrap_or(0.0) > 0.0
                })
                .map(|(i, g)| (i, g.agc_participation_factor.unwrap_or(0.0)))
                .collect();

            let absorbed = if !apf_gens.is_empty() {
                let absorbed = distribute_error_apf(&mut net.generators, &apf_gens, error);
                debug!(
                    area = ai.number,
                    scheduled_mw = scheduled,
                    actual_mw = actual,
                    error_mw = error,
                    absorbed_mw = absorbed,
                    n_apf_gens = apf_gens.len(),
                    outer_iter = outer,
                    method = "apf",
                    "area interchange adjustment"
                );
                absorbed
            } else {
                0.0
            };

            let residual = error - absorbed;
            // Tier 2: area slack bus fallback.
            let tier2_eligible = residual.abs() > tol
                && (apf_gens.is_empty() || residual.abs() > 1e-3)
                && Some(ai.slack_bus) != system_swing_bus;

            if tier2_eligible {
                if let Some(&_slack_idx) = bus_map.get(&ai.slack_bus) {
                    let slack_gens: Vec<(usize, f64)> = net
                        .generators
                        .iter()
                        .enumerate()
                        .filter(|(_, g)| g.bus == ai.slack_bus && g.in_service)
                        .map(|(i, g)| {
                            let headroom = if residual > 0.0 {
                                g.pmax - g.p
                            } else {
                                g.p - g.pmin
                            };
                            (i, headroom.max(0.0))
                        })
                        .collect();

                    let total_headroom: f64 = slack_gens.iter().map(|(_, h)| h).sum();
                    if total_headroom > 1e-9 {
                        let mut slack_absorbed = 0.0;
                        for (gi, headroom) in &slack_gens {
                            let share = residual * (headroom / total_headroom);
                            let g = &mut net.generators[*gi];
                            let new_pg = (g.p + share).clamp(g.pmin, g.pmax);
                            slack_absorbed += new_pg - g.p;
                            g.p = new_pg;
                        }
                        debug!(
                            area = ai.number,
                            slack_bus = ai.slack_bus,
                            residual_mw = residual,
                            slack_absorbed_mw = slack_absorbed,
                            outer_iter = outer,
                            method = "slack_bus_fallback",
                            "area interchange: tier 2 slack bus fallback"
                        );
                    } else {
                        // Tier 3: all gens at limits — system swing absorbs remainder.
                        warn!(
                            area = ai.number,
                            slack_bus = ai.slack_bus,
                            residual_mw = residual,
                            "area interchange: all generators at limits, system swing bus \
                             will absorb the {:.1} MW residual",
                            residual
                        );
                    }
                } else {
                    warn!(
                        area = ai.number,
                        slack_bus = ai.slack_bus,
                        residual_mw = residual,
                        "area interchange: slack bus not found in network"
                    );
                }
            }
        }

        if converged {
            all_converged = true;
            debug!(outer_iter = outer, "area interchange converged");
            break;
        }

        // Re-solve with warm start.
        let mut inner_opts = options.clone();
        inner_opts.warm_start = Some(WarmStart::from_solution(&sol));
        inner_opts.enforce_interchange = false; // prevent recursion
        sol = crate::ac_dc::solve_ac_pf_with_dc_lines(&net, &inner_opts)?;
    }

    // Build final area interchange result from the last solution.
    let bus_map = net.bus_index_map();
    let bus_area: Vec<u32> = net.buses.iter().map(|b| b.area).collect();
    let area_actual_mw = compute_area_actual_interchange(&sol, &net, &bus_map, &bus_area);

    let mut entries = Vec::with_capacity(net.area_schedules.len());
    for ai in &net.area_schedules {
        let scheduled = net_scheduled.get(&ai.number).copied().unwrap_or(0.0);
        let actual = area_actual_mw.get(&ai.number).copied().unwrap_or(0.0);
        let error = scheduled - actual;
        let tol = if ai.p_tolerance_mw > 0.0 {
            ai.p_tolerance_mw
        } else {
            1.0
        };

        // Determine which dispatch method was used for this area.
        let has_apf_gens = net.generators.iter().any(|g| {
            g.in_service
                && bus_area
                    .get(bus_map.get(&g.bus).copied().unwrap_or(usize::MAX))
                    .copied()
                    == Some(ai.number)
                && g.agc_participation_factor.unwrap_or(0.0) > 0.0
        });
        let method = if error.abs() <= tol {
            AreaDispatchMethod::Converged
        } else if has_apf_gens {
            AreaDispatchMethod::Apf
        } else {
            AreaDispatchMethod::SlackBusFallback
        };

        entries.push(AreaInterchangeEntry {
            area: ai.number,
            scheduled_mw: scheduled,
            actual_mw: actual,
            error_mw: error,
            dispatch_method: method,
        });
    }

    sol.area_interchange = Some(AreaInterchangeResult {
        areas: entries,
        iterations: final_iter + 1,
        converged: all_converged,
    });

    Ok(sol)
}
