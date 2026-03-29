#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC Security-Constrained Optimal Power Flow (AC-SCOPF) via Benders decomposition.
//!
//! Uses a master–subproblem Benders framework:
//!
//! 1. **Master**: Full AC-OPF (runtime-selected NLP backend) — optimizes
//!    dispatch and voltages.
//! 2. **Subproblems**: N-1 AC power flows (Newton-Raphson) for each contingency.
//! 3. **Cuts**: For each post-contingency thermal violation on branch m, compute
//!    the adjoint sensitivity `α[j] = ∂|S_m|/∂Pg_j` via `J^{k,T} λ = ∇S_m`,
//!    then add the linear cut `α^T Pg ≤ rating_pu − S_m_pu(Pg*) + α^T Pg*`
//!    to the master NLP.
//! 4. Repeat until no new violations or max iterations reached.
//!
//! Unlike the DC-SCOPF (`scopf_dc.rs`) which operates in θ-space, this formulation
//! handles full AC power flow including reactive power and voltage magnitudes.
//! The sensitivity is exact (not DC-approximated): it uses the actual NR Jacobian
//! transpose, not a DC PTDF matrix.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use surge_ac::AcPfOptions;
use surge_ac::solve_ac_pf_kernel;
use surge_network::Network;
use surge_network::network::generate_n1_branch_contingencies;
use surge_solution::OpfType;
use tracing::{debug, info, warn};

use super::types::*;
use crate::ac::sensitivity::{BendersCut, compute_ac_benders_cuts};
use crate::ac::solve::solve_ac_opf_with_context;
use crate::ac::types::AcOpfRunContext;
use crate::ac::{AcOpfRuntime, WarmStart};

/// Solve AC preventive SCOPF using Benders decomposition with AC-OPF master.
///
/// Iterates between:
/// - **Master** (AC-OPF): minimizes cost subject to power balance, thermal limits,
///   and any accumulated Benders cuts from prior iterations.
/// - **Subproblems** (AC NR): evaluates each N-1 contingency; for thermal violations,
///   computes the AC adjoint sensitivity cut and adds it to the master.
///
/// Convergence: no new thermal violations after a full contingency sweep.
pub(crate) fn solve_ac_preventive_with_context(
    network: &Network,
    options: &ScopfOptions,
    context: &ScopfRunContext,
) -> Result<ScopfResult, ScopfError> {
    let start = Instant::now();
    let base_mva = network.base_mva;
    let n_bus = network.n_buses();
    let bus_map = network.bus_index_map();

    info!(
        buses = n_bus,
        branches = network.branches.len(),
        "starting AC-SCOPF (Benders decomposition with AC adjoint cuts)"
    );

    // ── Generate N-1 contingency set ─────────────────────────────────────
    let mut contingencies = match &options.contingencies {
        Some(ctgs) => ctgs.clone(),
        None => generate_n1_branch_contingencies(network),
    };
    if options.max_contingencies > 0 && contingencies.len() > options.max_contingencies {
        contingencies.truncate(options.max_contingencies);
    }
    let n_contingencies = contingencies.len();
    info!(contingencies = n_contingencies, "contingency set generated");

    // ── In-service generator index list (fixed for the whole solve) ───────
    let in_service_gens: Vec<usize> = network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service)
        .map(|(i, _)| i)
        .collect();

    // ── Accumulated Benders cuts ──────────────────────────────────────────
    let mut accumulated_cuts: Vec<BendersCut> = Vec::new();
    let mut cut_keys: HashSet<(String, usize)> = HashSet::new();

    let mut binding_contingencies: HashSet<String> = HashSet::new();
    let mut last_opf: Option<surge_solution::OpfSolution> = None;
    let mut converged = false;
    let mut iter = 0u32;
    let mut remaining_violations = Vec::new();
    let mut failed_contingencies = Vec::new();

    // ── Benders loop ──────────────────────────────────────────────────────
    for benders_iter in 0..options.max_iterations {
        iter = benders_iter + 1;
        info!(
            iteration = iter,
            cuts = accumulated_cuts.len(),
            "AC-SCOPF Benders iteration"
        );

        // ── Step 1: Solve AC-OPF master with current cuts ─────────────────
        let mut opf_opts = options.ac.opf.clone();
        if options.enforce_flowgates
            && (!network.flowgates.is_empty() || !network.interfaces.is_empty())
        {
            opf_opts.enforce_flowgates = true;
        }
        let context = AcOpfRunContext {
            runtime: AcOpfRuntime {
                nlp_solver: context.runtime.nlp_solver.clone(),
                warm_start: last_opf.as_ref().map(WarmStart::from_opf),
                use_dc_opf_warm_start: None,
            },
            benders_cuts: accumulated_cuts.clone(),
        };

        let opf_result =
            solve_ac_opf_with_context(network, &opf_opts, &context).map_err(ScopfError::from)?;
        info!(
            cost = opf_result.total_cost,
            iterations = ?opf_result.iterations,
            "master AC-OPF solved"
        );

        // ── Step 2: Extract dispatch and voltage profile ──────────────────
        let vm_base = opf_result.power_flow.voltage_magnitude_pu.clone();
        let va_base = opf_result.power_flow.voltage_angle_rad.clone();
        let gen_p_mw = opf_result.generators.gen_p_mw.clone();
        let pg_pu: Vec<f64> = gen_p_mw.iter().map(|&p| p / base_mva).collect();

        let mut eval_net = network.clone();
        for (j, &gi) in in_service_gens.iter().enumerate() {
            eval_net.generators[gi].p = gen_p_mw[j];
            if j < opf_result.generators.gen_q_mvar.len() {
                eval_net.generators[gi].q = opf_result.generators.gen_q_mvar[j];
            }
        }
        for (idx, bus) in eval_net.buses.iter_mut().enumerate() {
            if idx < vm_base.len() {
                bus.voltage_magnitude_pu = vm_base[idx];
                bus.voltage_angle_rad = va_base[idx];
            }
        }

        // ── Step 3: Evaluate contingencies ────────────────────────────────
        let acpf_opts = AcPfOptions {
            max_iterations: options.ac.nr_max_iterations,
            tolerance: options.ac.nr_convergence_tolerance,
            flat_start: false,
            ..Default::default()
        };

        remaining_violations.clear();
        failed_contingencies.clear();
        let mut new_cuts_added = false;

        for ctg in &contingencies {
            // Skip empty contingencies (must have at least one branch or generator outage)
            if ctg.branch_indices.is_empty() && ctg.generator_indices.is_empty() {
                continue;
            }

            let mut ctg_net = eval_net.clone();

            // Trip all outaged branches
            let mut valid = true;
            for &br_idx in &ctg.branch_indices {
                if br_idx < ctg_net.branches.len() {
                    ctg_net.branches[br_idx].in_service = false;
                } else {
                    valid = false;
                    break;
                }
            }
            if !valid {
                continue;
            }

            // Trip all outaged generators and redistribute their power
            let mut lost_pg = 0.0;
            for &gen_idx in &ctg.generator_indices {
                if gen_idx < ctg_net.generators.len() {
                    lost_pg += ctg_net.generators[gen_idx].p;
                    ctg_net.generators[gen_idx].in_service = false;
                    ctg_net.generators[gen_idx].p = 0.0;
                    ctg_net.generators[gen_idx].q = 0.0;
                }
            }
            // Redistribute lost generation proportionally to remaining headroom
            if lost_pg.abs() > 1e-6 {
                let remaining: Vec<(usize, f64)> = ctg_net
                    .generators
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| g.in_service && g.pmax > g.p)
                    .map(|(i, g)| (i, g.pmax - g.p))
                    .collect();
                let total_headroom: f64 = remaining.iter().map(|(_, h)| h).sum();
                if total_headroom > 1e-6 {
                    for (gi, headroom) in &remaining {
                        let share = lost_pg * headroom / total_headroom;
                        ctg_net.generators[*gi].p += share;
                    }
                } else {
                    warn!(
                        contingency = %ctg.id,
                        lost_mw = lost_pg,
                        "no generator headroom for redistribution — contingency may be infeasible"
                    );
                }
            }

            let pf_result = match solve_ac_pf_kernel(&ctg_net, &acpf_opts) {
                Ok(sol) => {
                    if sol.status != surge_solution::SolveStatus::Converged {
                        debug!(
                            contingency = %ctg.id,
                            status = ?sol.status,
                            "NR did not converge for contingency"
                        );
                        failed_contingencies.push(FailedContingencyEvaluation {
                            contingency_id: ctg.id.clone(),
                            contingency_label: ctg.label.clone(),
                            outaged_branches: ctg.branch_indices.clone(),
                            outaged_generators: ctg.generator_indices.clone(),
                            reason: format!("power flow returned status {:?}", sol.status),
                        });
                        continue;
                    }
                    sol
                }
                Err(e) => {
                    debug!(
                        contingency = %ctg.id,
                        error = %e,
                        "NR failed for contingency"
                    );
                    failed_contingencies.push(FailedContingencyEvaluation {
                        contingency_id: ctg.id.clone(),
                        contingency_label: ctg.label.clone(),
                        outaged_branches: ctg.branch_indices.clone(),
                        outaged_generators: ctg.generator_indices.clone(),
                        reason: e.to_string(),
                    });
                    continue;
                }
            };

            // ── Check thermal violations ──────────────────────────────────
            let mut thermal_viols: Vec<(usize, f64, f64, f64)> = Vec::new();
            let flows_mva = compute_branch_flows_mva(
                &ctg_net.branches,
                base_mva,
                &bus_map,
                &pf_result.voltage_magnitude_pu,
                &pf_result.voltage_angle_rad,
            );

            let outaged_set: HashSet<usize> = ctg.branch_indices.iter().copied().collect();
            for (br_idx, branch) in ctg_net.branches.iter().enumerate() {
                if !branch.in_service || outaged_set.contains(&br_idx) {
                    continue;
                }
                let rating = options.contingency_rating.of(branch);
                if rating < options.min_rate_a {
                    continue;
                }
                let flow = flows_mva[br_idx];
                let overload = flow / rating;
                if overload > 1.0 {
                    thermal_viols.push((br_idx, flow, rating, overload));
                }
            }

            // ── Check flowgate violations (post-contingency) ─────────────
            if options.enforce_flowgates {
                // Compute active power flows (MW) for flowgate/interface evaluation
                let p_flows_mw = compute_branch_active_power_mw(
                    &ctg_net.branches,
                    base_mva,
                    &bus_map,
                    &pf_result.voltage_magnitude_pu,
                    &pf_result.voltage_angle_rad,
                );

                for fg in &network.flowgates {
                    if !fg.in_service {
                        continue;
                    }
                    let mut flow_mw = 0.0;
                    for member in &fg.monitored {
                        let coeff = member.coefficient;
                        let branch_ref = &member.branch;
                        if let Some(br_idx) = ctg_net
                            .branches
                            .iter()
                            .position(|br| br.in_service && branch_ref.matches_branch(br))
                        {
                            flow_mw += coeff * p_flows_mw[br_idx];
                        }
                    }
                    let fwd_limit = fg.limit_mw;
                    let rev_limit = fg.effective_reverse_or_forward(0);
                    let tol = options.violation_tolerance_pu * base_mva;
                    let violated = (fwd_limit > 0.0 && flow_mw > fwd_limit + tol)
                        || (rev_limit > 0.0 && flow_mw < -rev_limit - tol);
                    if violated
                        && let Some(br_idx) = fg.monitored.first().and_then(|member| {
                            ctg_net
                                .branches
                                .iter()
                                .position(|br| br.in_service && member.branch.matches_branch(br))
                        })
                    {
                        let limit = if flow_mw >= 0.0 { fwd_limit } else { rev_limit };
                        thermal_viols.push((br_idx, flow_mw.abs(), limit, flow_mw.abs() / limit));
                    }
                }

                for iface in &network.interfaces {
                    if !iface.in_service {
                        continue;
                    }
                    let mut flow_mw = 0.0;
                    for member in &iface.members {
                        let coeff = member.coefficient;
                        let branch_ref = &member.branch;
                        if let Some(br_idx) = ctg_net
                            .branches
                            .iter()
                            .position(|br| br.in_service && branch_ref.matches_branch(br))
                        {
                            flow_mw += coeff * p_flows_mw[br_idx];
                        }
                    }
                    let fwd_limit = iface.limit_forward_mw;
                    let rev_limit = iface.limit_reverse_mw;
                    let violated = (fwd_limit > 0.0
                        && flow_mw > fwd_limit + options.violation_tolerance_pu * base_mva)
                        || (rev_limit > 0.0
                            && flow_mw < -rev_limit - options.violation_tolerance_pu * base_mva);
                    if violated
                        && let Some(br_idx) = iface.members.first().and_then(|member| {
                            ctg_net
                                .branches
                                .iter()
                                .position(|br| br.in_service && member.branch.matches_branch(br))
                        })
                    {
                        let limit = if flow_mw >= 0.0 { fwd_limit } else { rev_limit };
                        thermal_viols.push((br_idx, flow_mw.abs(), limit, flow_mw.abs() / limit));
                    }
                }
            }

            // ── Check voltage violations ──────────────────────────────────
            let mut voltage_viols = Vec::new();
            for (bus_idx, bus) in ctg_net.buses.iter().enumerate() {
                if bus_idx >= pf_result.voltage_magnitude_pu.len() {
                    break;
                }
                let vm = pf_result.voltage_magnitude_pu[bus_idx];
                if vm < bus.voltage_min_pu - options.ac.voltage_threshold
                    || vm > bus.voltage_max_pu + options.ac.voltage_threshold
                {
                    voltage_viols.push((bus_idx, vm, bus.voltage_min_pu, bus.voltage_max_pu));
                }
            }

            if !thermal_viols.is_empty() || !voltage_viols.is_empty() {
                binding_contingencies.insert(ctg.id.clone());

                let viol = ContingencyViolation {
                    contingency_id: ctg.id.clone(),
                    contingency_label: ctg.label.clone(),
                    outaged_branches: ctg.branch_indices.clone(),
                    outaged_generators: ctg.generator_indices.clone(),
                    thermal_violations: thermal_viols.clone(),
                    voltage_violations: voltage_viols.clone(),
                };

                debug!(
                    contingency = %ctg.id,
                    thermal = thermal_viols.len(),
                    voltage = voltage_viols.len(),
                    "post-contingency violations found"
                );

                remaining_violations.push(viol);

                // ── Compute AC Benders cuts for thermal violations ────────
                if !thermal_viols.is_empty() {
                    let new_cuts = compute_ac_benders_cuts(
                        &ctg_net,
                        base_mva,
                        &in_service_gens,
                        &pg_pu,
                        &pf_result,
                        &thermal_viols,
                        &ctg.id,
                    );

                    for cut in new_cuts {
                        let key = (cut.contingency_id.clone(), cut.branch_idx);
                        if !cut_keys.contains(&key) {
                            cut_keys.insert(key);
                            accumulated_cuts.push(cut);
                            new_cuts_added = true;
                        }
                    }
                }

                // Voltage violations are tracked for reporting and convergence.
                // Voltage Benders cuts via ∂Vm/∂Pg are available in
                // `compute_ac_voltage_benders_cuts` but not enabled by default
                // because real-power-only sensitivities can produce infeasible
                // cuts (e.g., when increasing Pg worsens voltage at the
                // violated bus). A full voltage-secure AC-SCOPF would require
                // Qg/Vm as master variables, which is future work.
            }
        }

        info!(
            iteration = iter,
            violations = remaining_violations.len(),
            failed = failed_contingencies.len(),
            new_cuts = new_cuts_added,
            total_cuts = accumulated_cuts.len(),
            "contingency evaluation complete"
        );

        let has_thermal = remaining_violations
            .iter()
            .any(|v| !v.thermal_violations.is_empty());
        let has_voltage = remaining_violations
            .iter()
            .any(|v| !v.voltage_violations.is_empty());

        if failed_contingencies.is_empty() && !new_cuts_added && !has_thermal && !has_voltage {
            converged = true;
            last_opf = Some(opf_result);
            break;
        }

        if !new_cuts_added && !has_thermal && has_voltage {
            if options.ac.enforce_voltage_security {
                // Voltage violations remain but we couldn't generate new cuts —
                // stalled on voltage security. Mark as not converged.
                warn!(
                    iteration = iter,
                    voltage_violations = remaining_violations
                        .iter()
                        .map(|v| v.voltage_violations.len())
                        .sum::<usize>(),
                    "AC-SCOPF: voltage violations remain but no new voltage cuts could be generated — stalled"
                );
            } else {
                warn!(
                    iteration = iter,
                    voltage_violations = remaining_violations
                        .iter()
                        .map(|v| v.voltage_violations.len())
                        .sum::<usize>(),
                    "AC-SCOPF: voltage violations remain (enforce_voltage_security=false)"
                );
            }
            converged = false;
            last_opf = Some(opf_result);
            break;
        }

        if !new_cuts_added && !remaining_violations.is_empty() {
            warn!(
                iteration = iter,
                violations = remaining_violations.len(),
                "AC-SCOPF: violations found but no new cuts generated — stalled"
            );
            last_opf = Some(opf_result);
            break;
        }

        last_opf = Some(opf_result);
    }

    let total_time = start.elapsed().as_secs_f64();

    let mut result_opf = last_opf.ok_or_else(|| {
        ScopfError::SolverError("AC-SCOPF: no master OPF solution obtained".to_string())
    })?;
    result_opf.opf_type = OpfType::AcScopf;

    info!(
        converged,
        iterations = iter,
        binding = binding_contingencies.len(),
        violations = remaining_violations.len(),
        cuts = accumulated_cuts.len(),
        cost = result_opf.total_cost,
        time_secs = total_time,
        "AC-SCOPF complete"
    );

    // ── Extract binding contingencies from Benders cut duals ─────────────
    let binding_ctgs: Vec<BindingContingency> = if !accumulated_cuts.is_empty() {
        let duals = &result_opf.benders_cut_duals;
        accumulated_cuts
            .iter()
            .enumerate()
            .filter(|(i, _)| i < &duals.len() && duals[*i].abs() > 1e-6)
            .map(|(i, cut)| {
                BindingContingency {
                    contingency_label: cut.contingency_id.clone(),
                    cut_kind: ScopfCutKind::BranchThermal,
                    outaged_branch_indices: contingencies
                        .iter()
                        .find(|c| c.id == cut.contingency_id)
                        .map(|c| c.branch_indices.clone())
                        .unwrap_or_default(),
                    outaged_generator_indices: contingencies
                        .iter()
                        .find(|c| c.id == cut.contingency_id)
                        .map(|c| c.generator_indices.clone())
                        .unwrap_or_default(),
                    monitored_branch_idx: cut.branch_idx,
                    loading_pct: 0.0, // not available from cut structure
                    shadow_price: duals[i],
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(ScopfResult {
        base_opf: result_opf,
        formulation: ScopfFormulation::Ac,
        mode: ScopfMode::Preventive,
        iterations: iter,
        converged,
        total_contingencies_evaluated: n_contingencies,
        total_contingency_constraints: accumulated_cuts.len(),
        binding_contingencies: binding_ctgs,
        lmp_contingency_congestion: Vec::new(),
        remaining_violations,
        failed_contingencies,
        screening_stats: ScopfScreeningStats::default(),
        solve_time_secs: total_time,
    })
}

// ── Branch flow computation ──────────────────────────────────────────────

/// Compute from-end (Pf, Qf) in per-unit for each branch using the full pi-model.
fn compute_branch_pq_pu(
    branches: &[surge_network::network::Branch],
    bus_map: &HashMap<u32, usize>,
    vm: &[f64],
    va: &[f64],
) -> Vec<(f64, f64)> {
    let mut pq = vec![(0.0f64, 0.0f64); branches.len()];
    for (i, branch) in branches.iter().enumerate() {
        if !branch.in_service {
            continue;
        }
        let Some(&f) = bus_map.get(&branch.from_bus) else {
            continue;
        };
        let Some(&t) = bus_map.get(&branch.to_bus) else {
            continue;
        };

        let (g_ff, b_ff, g_ft, b_ft, _, _, _, _) = branch.pi_model_admittances(1e-40);

        let vi = vm[f];
        let vj = vm[t];
        let theta_ij = va[f] - va[t];
        let (sin_t, cos_t) = theta_ij.sin_cos();

        let p_ij = vi * vi * g_ff + vi * vj * (g_ft * cos_t + b_ft * sin_t);
        let q_ij = -vi * vi * b_ff + vi * vj * (g_ft * sin_t - b_ft * cos_t);
        pq[i] = (p_ij, q_ij);
    }
    pq
}

/// Compute max-end apparent power flow (MVA) for each branch.
fn compute_branch_flows_mva(
    branches: &[surge_network::network::Branch],
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    vm: &[f64],
    va: &[f64],
) -> Vec<f64> {
    branches
        .iter()
        .map(|branch| {
            if !branch.in_service {
                return 0.0;
            }
            let Some(&f) = bus_map.get(&branch.from_bus) else {
                return 0.0;
            };
            let Some(&t) = bus_map.get(&branch.to_bus) else {
                return 0.0;
            };
            branch
                .power_flows_pu(vm[f], vm[t], va[f] - va[t], 1e-40)
                .max_s_pu()
                * base_mva
        })
        .collect()
}

/// Compute from-end active power flow (MW) for each branch.
fn compute_branch_active_power_mw(
    branches: &[surge_network::network::Branch],
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    vm: &[f64],
    va: &[f64],
) -> Vec<f64> {
    compute_branch_pq_pu(branches, bus_map, vm, va)
        .iter()
        .map(|&(p, _)| p * base_mva)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::test_util::{data_available, test_data_dir};

    use super::*;
    use crate::ac::solve::solve_ac_opf;
    use crate::ac::types::AcOpfOptions;

    fn load_case(name: &str) -> Network {
        let path = test_data_dir().join(format!("{name}.m"));
        surge_io::matpower::load(&path).unwrap_or_else(|e| panic!("failed to parse {name}: {e}"))
    }

    #[test]
    fn test_ac_scopf_case9() {
        if !data_available() {
            eprintln!("SKIP: test data not present");
            return;
        }
        let net = load_case("case9");
        let opts = ScopfOptions {
            formulation: ScopfFormulation::Ac,
            mode: ScopfMode::Preventive,
            max_iterations: 5,
            ac: ScopfAcSettings {
                nr_max_iterations: 30,
                nr_convergence_tolerance: 1e-6,
                voltage_threshold: 0.02,
                ..Default::default()
            },
            ..Default::default()
        };
        let result = solve_ac_preventive_with_context(&net, &opts, &ScopfRunContext::default())
            .expect("AC-SCOPF should solve");
        assert!(result.base_opf.total_cost > 0.0, "cost should be positive");
        assert!(result.iterations >= 1, "should run at least 1 iteration");

        // AC-SCOPF cost >= unconstrained AC-OPF cost (security adds conservatism).
        let opf_opts = AcOpfOptions::default();
        let opf = solve_ac_opf(&net, &opf_opts).expect("AC-OPF should solve");
        assert!(
            result.base_opf.total_cost >= opf.total_cost - 1e-2,
            "AC-SCOPF cost ({}) should be >= AC-OPF cost ({})",
            result.base_opf.total_cost,
            opf.total_cost
        );
    }

    #[test]
    fn test_ac_scopf_case14() {
        if !data_available() {
            eprintln!("SKIP: test data not present");
            return;
        }
        let net = load_case("case14");
        let opts = ScopfOptions {
            formulation: ScopfFormulation::Ac,
            mode: ScopfMode::Preventive,
            max_iterations: 5,
            ac: ScopfAcSettings {
                nr_max_iterations: 30,
                nr_convergence_tolerance: 1e-6,
                ..Default::default()
            },
            ..Default::default()
        };
        let result = solve_ac_preventive_with_context(&net, &opts, &ScopfRunContext::default())
            .expect("AC-SCOPF should solve");
        assert!(result.base_opf.total_cost > 0.0, "cost should be positive");
        assert!(
            result.total_contingencies_evaluated > 0,
            "should evaluate some contingencies"
        );
    }

    #[test]
    fn test_ac_scopf_converges_no_violations() {
        if !data_available() {
            eprintln!("SKIP: test data not present");
            return;
        }
        let net = load_case("case9");
        let opts = ScopfOptions {
            formulation: ScopfFormulation::Ac,
            mode: ScopfMode::Preventive,
            max_iterations: 3,
            ac: ScopfAcSettings {
                voltage_threshold: 1.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let result = solve_ac_preventive_with_context(&net, &opts, &ScopfRunContext::default())
            .expect("AC-SCOPF should solve");
        assert!(result.converged, "should converge with loose thresholds");
        assert_eq!(result.iterations, 1, "should converge in 1 iteration");
    }
}
