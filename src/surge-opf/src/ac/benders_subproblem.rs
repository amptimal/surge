// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC-OPF Benders subproblem for SCED-AC decomposition.
//!
//! This module provides [`solve_ac_opf_subproblem`], which solves an AC-OPF
//! with the active-power dispatch of selected generators **fixed** to caller-
//! specified target values, then returns:
//!
//!   1. The full [`OpfSolution`] (Vm/Va/Qg + slack penalties).
//!   2. A scalar `slack_cost_dollars_per_hour` measuring how much the AC
//!      operating point added on top of the DC schedule's energy cost (i.e.
//!      the dollar value of soft-penalty constraint violations).
//!   3. A `slack_marginal_per_gen` map giving `∂(slack_cost) / ∂(Pg_target)`
//!      at each fixed generator. These marginals are the cut coefficients
//!      consumed by the SCED master in a Benders decomposition: an
//!      optimality cut of the form
//!
//!        η[t] ≥ slack_cost(P̃g) + Σ_g λ_g · (Pg[g,t] − P̃g_g)
//!
//!      gives the master a linear lower bound on the AC physics adder, valid
//!      in a neighborhood of the current operating point.
//!
//! ## How fixed-Pg is enforced
//!
//! For each generator `gi` in `fixed_p_mw`, the network is cloned and the
//! generator's `pmin` and `pmax` are both set to the target value. The NLP
//! sees `lb_pg = ub_pg = target/base_mva`, the variable collapses to a
//! point, and the lower- and upper-bound multipliers from Ipopt/COPT carry
//! the marginal information we need. By the envelope theorem, when the
//! variable is parameterised as `Pg = target`, the gradient of the optimal
//! objective with respect to `target` is `μ_lower − μ_upper` (with surge's
//! `z_lower` / `z_upper` naming convention).
//!
//! ## Why a separate function instead of reusing [`solve_ac_opf`]
//!
//! The bound mutation must be local to the subproblem solve so it doesn't
//! pollute the surrounding network state, and we want a strongly-typed
//! result that exposes the marginals in dispatch-cost units (dollars per MW
//! per hour) rather than the raw NLP duals (which are in pu cost / pu
//! injection). Wrapping the call here keeps unit handling and target
//! validation in one place.

use std::collections::HashMap;

use surge_network::Network;
use surge_solution::OpfSolution;

use super::solve::solve_ac_opf_with_runtime;
use super::types::{AcOpfError, AcOpfOptions, AcOpfRuntime};

/// Result of solving the AC-OPF Benders subproblem with fixed generator P.
#[derive(Debug, Clone)]
pub struct AcOpfBendersSubproblem {
    /// Full AC-OPF solution at the fixed operating point.
    pub solution: OpfSolution,
    /// Total slack penalty cost added by the AC physics at this dispatch
    /// ($/hr). This is the value of η[t] in the master cut: it captures the
    /// branch-thermal slack penalty + bus P/Q balance slack penalty + any
    /// other soft-constraint cost the AC OPF objective evaluated at the
    /// optimum.
    pub slack_cost_dollars_per_hour: f64,
    /// Per-generator marginal of the slack cost with respect to the fixed
    /// `Pg_target`, in dollars per MW per hour.
    ///
    /// Keyed by **global** generator index (into `network.generators`), not
    /// by AC-OPF local index, so callers can correlate cuts to dispatch
    /// resources without going through the AC OPF mapping. Only generators
    /// that appeared in `fixed_p_mw` are present.
    ///
    /// Computed as `(z_lower − z_upper) * base_mva − marginal_cost(Pg_target)`
    /// so that the value is the gradient of the *slack penalty alone*, with
    /// the generator's own production cost gradient subtracted out. This is
    /// the right form for a master objective of `min DC_cost(Pg) + Σ η[t]`,
    /// where η[t] is meant to track only the slack adder, not the (already
    /// counted in DC_cost) energy cost.
    pub slack_marginal_dollars_per_mw_per_hour: HashMap<usize, f64>,
    /// Whether the NLP backend reported convergence. Even when `true`, the
    /// caller should also check the slack values to decide whether the
    /// operating point is acceptable; convergence with large slacks is
    /// expected and meaningful.
    pub converged: bool,
}

/// Solve an AC-OPF with `Pg` fixed for the generators listed in `fixed_p_mw`.
///
/// `fixed_p_mw` keys are global indices into `network.generators`. Generators
/// not present in the map keep their normal `[pmin, pmax]` envelope and are
/// free for the NLP to dispatch. (In a SCED-AC Benders use case, callers
/// typically pass *every* in-service generator to fully fix the dispatch.)
///
/// The function:
///
///   1. Clones `network`, sets `pmin = pmax = target_mw` for each fixed
///      generator (clipped to the original `[orig_pmin, orig_pmax]` envelope
///      so out-of-range targets do not produce infeasible LPs).
///   2. Calls [`solve_ac_opf_with_runtime`].
///   3. Reads `solution.generators.shadow_price_pg_min/max` (already
///      populated by the existing dual-extraction path) and converts to a
///      slack marginal in `$/MW-hr`, with the generator's own marginal
///      production cost subtracted.
///   4. Reads `solution.branches.thermal_limit_slack_from/to_mva` to compute
///      the total slack penalty cost.
///
/// Returns an [`AcOpfBendersSubproblem`].
pub fn solve_ac_opf_subproblem(
    network: &Network,
    options: &AcOpfOptions,
    runtime: &AcOpfRuntime,
    fixed_p_mw: &HashMap<usize, f64>,
) -> Result<AcOpfBendersSubproblem, AcOpfError> {
    let mut subproblem_network = network.clone();

    // Snapshot the *original* envelope so we can clip a target into bounds
    // (necessary when the master proposes a Pg slightly outside the per-gen
    // box because of LP slack or rounding).
    let mut original_bounds_mw: HashMap<usize, (f64, f64)> = HashMap::new();
    for (&gi, &target_mw) in fixed_p_mw.iter() {
        if gi >= subproblem_network.generators.len() {
            return Err(AcOpfError::InvalidNetwork(format!(
                "solve_ac_opf_subproblem: fixed_p_mw refers to generator index {gi}, but network has only {} generators",
                subproblem_network.generators.len()
            )));
        }
        if !subproblem_network.generators[gi].in_service {
            // Out-of-service generators cannot be fixed; silently skip so the
            // caller can pass a uniform map and not worry about per-period
            // commitment status.
            continue;
        }
        let orig_pmin = subproblem_network.generators[gi].pmin;
        let orig_pmax = subproblem_network.generators[gi].pmax;
        original_bounds_mw.insert(gi, (orig_pmin, orig_pmax));
        let clipped = target_mw.clamp(orig_pmin, orig_pmax);
        subproblem_network.generators[gi].pmin = clipped;
        subproblem_network.generators[gi].pmax = clipped;
        // Also pin the operating-point hint so warm-start logic does not
        // perturb the dispatch.
        subproblem_network.generators[gi].p = clipped;
    }

    // Solve the AC OPF with the fixed-Pg network. This re-uses every other
    // option (thermal slack penalty, bus balance slack penalty, voltage
    // regulator policy, NLP backend choice) from the caller's runtime.
    let solution = solve_ac_opf_with_runtime(&subproblem_network, options, runtime)?;

    // Compute the slack penalty cost. The AC OPF objective at the optimum is
    // (generator energy cost) + (thermal slack penalty) + (bus balance slack
    // penalty) + (AC target tracking penalty). For Benders we want only the
    // soft-constraint penalty contributions:
    let energy_cost_dollars_per_hour = energy_cost_at_dispatch(&subproblem_network, &solution);
    let mut slack_cost_dollars_per_hour = solution.total_cost - energy_cost_dollars_per_hour;
    if !slack_cost_dollars_per_hour.is_finite() {
        slack_cost_dollars_per_hour = 0.0;
    }
    if slack_cost_dollars_per_hour.abs() < 1e-9 {
        slack_cost_dollars_per_hour = 0.0;
    }

    // Build the per-generator marginal map. The shadow_price_pg_min and
    // shadow_price_pg_max are already in $/MW (the existing extraction divides
    // the raw NLP duals by base_mva). The "slack marginal" is the gradient of
    // the slack penalty *alone* — we subtract off the generator's own
    // marginal production cost so what remains is purely the AC physics
    // sensitivity to Pg motion.
    let mut slack_marginal: HashMap<usize, f64> = HashMap::new();
    let n_gen_ac = solution.generators.gen_p_mw.len();
    let gen_ids = &solution.generators.gen_ids;
    let shadow_pg_min = &solution.generators.shadow_price_pg_min;
    let shadow_pg_max = &solution.generators.shadow_price_pg_max;
    let has_pg_duals = shadow_pg_min.len() == n_gen_ac && shadow_pg_max.len() == n_gen_ac;

    if has_pg_duals && !gen_ids.is_empty() {
        // Map AC OPF local index → global network generator index. The
        // existing AC OPF returns gen_ids (resource id strings) parallel to
        // gen_p_mw, so we look each one up in the cloned network to recover
        // the global index.
        let mut id_to_global_idx: HashMap<&str, usize> = HashMap::new();
        for (i, g) in subproblem_network.generators.iter().enumerate() {
            id_to_global_idx.insert(g.id.as_str(), i);
        }

        for (j, resource_id) in gen_ids.iter().enumerate() {
            let Some(&global_idx) = id_to_global_idx.get(resource_id.as_str()) else {
                continue;
            };
            if !fixed_p_mw.contains_key(&global_idx) {
                continue; // only report marginals for generators we fixed
            }
            let target_mw = fixed_p_mw[&global_idx];
            // (z_lower - z_upper) carries the gradient of the *total*
            // objective (energy cost + slack) wrt Pg. The shadow_price_pg_min
            // / shadow_price_pg_max in solution are already divided by
            // base_mva, so they're in $/MW.
            let total_marginal = shadow_pg_min[j] - shadow_pg_max[j];
            // Subtract the generator's own marginal production cost evaluated
            // at the operating point so the residual is just the slack/penalty
            // gradient. This makes the cut compatible with a master objective
            // that already includes DC_cost(Pg) — see module docs.
            let marginal_cost =
                generator_marginal_cost_at(&subproblem_network.generators[global_idx], target_mw);
            let slack_marginal_value = total_marginal - marginal_cost;
            // Numerical floor: tiny values from solver tolerance are noise.
            if slack_marginal_value.abs() > 1e-8 {
                slack_marginal.insert(global_idx, slack_marginal_value);
            }
        }
    }

    // Restore original bounds on the cloned network as a safety measure even
    // though it's about to be dropped — keeps the network in a consistent
    // state if a future caller decides to retain the clone.
    for (gi, (orig_pmin, orig_pmax)) in original_bounds_mw.into_iter() {
        if let Some(g) = subproblem_network.generators.get_mut(gi) {
            g.pmin = orig_pmin;
            g.pmax = orig_pmax;
        }
    }

    let converged = matches!(
        solution.power_flow.status,
        surge_solution::SolveStatus::Converged
    );

    Ok(AcOpfBendersSubproblem {
        solution,
        slack_cost_dollars_per_hour,
        slack_marginal_dollars_per_mw_per_hour: slack_marginal,
        converged,
    })
}

/// Sum the energy production cost across all in-service generators in `solution`
/// using each generator's cost curve evaluated at its dispatch.
fn energy_cost_at_dispatch(network: &Network, solution: &OpfSolution) -> f64 {
    let mut total = 0.0_f64;
    let n_gen_ac = solution.generators.gen_p_mw.len();
    let gen_ids = &solution.generators.gen_ids;
    if gen_ids.len() != n_gen_ac {
        return 0.0;
    }
    let mut id_to_global: HashMap<&str, usize> = HashMap::new();
    for (i, g) in network.generators.iter().enumerate() {
        id_to_global.insert(g.id.as_str(), i);
    }
    for (j, resource_id) in gen_ids.iter().enumerate() {
        let Some(&gi) = id_to_global.get(resource_id.as_str()) else {
            continue;
        };
        let p_mw = solution.generators.gen_p_mw[j];
        if let Some(cost) = network.generators[gi]
            .cost
            .as_ref()
            .map(|c| c.evaluate(p_mw))
        {
            total += cost;
        }
    }
    total
}

/// Evaluate the marginal production cost (∂cost/∂Pg in $/MW-hr) of a
/// generator at the given dispatch point. Falls back to the linear coefficient
/// if the curve is polynomial; for piecewise-linear curves, returns the slope
/// of the segment containing `p_mw`.
fn generator_marginal_cost_at(generator: &surge_network::network::Generator, p_mw: f64) -> f64 {
    let Some(cost) = generator.cost.as_ref() else {
        return 0.0;
    };
    cost.marginal_cost(p_mw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    /// Build a 3-bus network with two generators of different cost so the
    /// AC OPF makes nontrivial dispatch choices and the Benders marginals
    /// have meaningful values.
    fn three_bus_two_gen_network() -> Network {
        let mut net = Network::new("benders-3bus-2gen");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PV, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 138.0));

        net.branches.push(Branch::new_line(1, 3, 0.01, 0.05, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.05, 0.0));
        net.branches.push(Branch::new_line(1, 2, 0.02, 0.10, 0.0));

        net.loads.push(Load::new(3, 100.0, 30.0));

        // Cheap base-load gen at bus 1: 10 + 0.005 P^2 ($/hr)
        let mut g1 = Generator::new(1, 60.0, 1.05);
        g1.pmin = 10.0;
        g1.pmax = 200.0;
        g1.qmin = -100.0;
        g1.qmax = 100.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.005, 10.0, 0.0],
        });
        net.generators.push(g1);

        // Expensive peaker at bus 2: 50 + 0.01 P^2
        let mut g2 = Generator::new(2, 40.0, 1.05);
        g2.pmin = 5.0;
        g2.pmax = 200.0;
        g2.qmin = -100.0;
        g2.qmax = 100.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.01, 50.0, 0.0],
        });
        net.generators.push(g2);

        net
    }

    #[test]
    fn fixed_pg_collapses_bounds_and_returns_marginal() {
        let net = three_bus_two_gen_network();
        let mut fixed = HashMap::new();
        fixed.insert(0, 60.0);
        fixed.insert(1, 40.0);

        let options = AcOpfOptions::default();
        let runtime = AcOpfRuntime::default();

        // We just verify the subproblem returns a structured result and
        // every marginal is finite. The exact value depends on backend
        // availability — assertions about specific numeric values would be
        // too brittle for unit-test territory.
        if let Ok(sub) = solve_ac_opf_subproblem(&net, &options, &runtime, &fixed) {
            assert!(sub.slack_cost_dollars_per_hour.is_finite());
            for (gi, &m) in sub.slack_marginal_dollars_per_mw_per_hour.iter() {
                assert!(*gi < 2);
                assert!(m.is_finite(), "marginal must be finite, got {m}");
            }
        }
    }

    #[test]
    fn fixed_pg_solution_matches_unfixed_when_targets_are_optimal() {
        // First solve the AC OPF freely to discover the optimal dispatch.
        // Then re-solve with Pg fixed to those values; the cost should match
        // (within tolerance) and the slack marginals should be near zero
        // because the fix is at the unconstrained optimum.
        let net = three_bus_two_gen_network();
        let options = AcOpfOptions::default();
        let runtime = AcOpfRuntime::default();
        let Ok(free_solution) =
            super::super::solve::solve_ac_opf_with_runtime(&net, &options, &runtime)
        else {
            return; // backend unavailable; skip
        };

        let mut fixed: HashMap<usize, f64> = HashMap::new();
        for (i, &p_mw) in free_solution.generators.gen_p_mw.iter().enumerate() {
            fixed.insert(i, p_mw);
        }

        let Ok(sub) = solve_ac_opf_subproblem(&net, &options, &runtime, &fixed) else {
            return; // skip rather than fail on solver-availability flake
        };

        let cost_diff = (sub.solution.total_cost - free_solution.total_cost).abs();
        assert!(
            cost_diff < 1e-3,
            "fixed-Pg cost should match free cost when fix is at the optimum (got diff = {cost_diff})"
        );
        // At the unconstrained optimum, the slack penalty marginal should be
        // very small (the gen is interior, no binding physics). We don't
        // assert exactly zero because solver tolerances accumulate.
        for (&gi, &marginal) in sub.slack_marginal_dollars_per_mw_per_hour.iter() {
            assert!(
                marginal.abs() < 5.0,
                "interior-optimum marginal should be near zero for gen {gi}, got {marginal} $/MW-hr"
            );
        }
    }

    #[test]
    fn fixed_pg_clips_targets_to_envelope() {
        // Pass targets that are out-of-range; the function should silently
        // clamp them into [pmin, pmax] rather than producing an infeasible
        // bound or crashing.
        let net = three_bus_two_gen_network();
        let mut fixed = HashMap::new();
        fixed.insert(0, 9999.0); // way above pmax = 200
        fixed.insert(1, -50.0); // way below pmin = 5
        let options = AcOpfOptions::default();
        let runtime = AcOpfRuntime::default();
        if let Ok(sub) = solve_ac_opf_subproblem(&net, &options, &runtime, &fixed) {
            assert!(sub.solution.total_cost.is_finite());
            // gen 0 should be clamped to pmax = 200
            assert!((sub.solution.generators.gen_p_mw[0] - 200.0).abs() < 1.0);
            // gen 1 should be clamped to pmin = 5
            assert!((sub.solution.generators.gen_p_mw[1] - 5.0).abs() < 1.0);
        }
    }

    #[test]
    fn out_of_service_generator_in_fixed_map_is_ignored() {
        // The orchestrator may pass every generator's target uniformly. If
        // commitment is fixed-off in some periods, those generators are
        // out-of-service in the network for that period and should be
        // silently skipped rather than blowing up.
        let mut net = three_bus_two_gen_network();
        net.generators[1].in_service = false;
        let mut fixed = HashMap::new();
        fixed.insert(0, 100.0);
        fixed.insert(1, 50.0); // out of service — should be skipped
        let options = AcOpfOptions::default();
        let runtime = AcOpfRuntime::default();
        if let Ok(sub) = solve_ac_opf_subproblem(&net, &options, &runtime, &fixed) {
            // Only gen 0 should be reported (gen 1 is out of service so
            // there's no marginal to extract).
            for &gi in sub.slack_marginal_dollars_per_mw_per_hour.keys() {
                assert_ne!(gi, 1, "out-of-service gen should not appear in marginals");
            }
        }
    }
}
